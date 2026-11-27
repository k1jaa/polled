// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Pure-Win32 zero-OpenGL background tray daemon.
//!
//! Owns the system tray icon, `ProfileRuntimeHandle` (foreground focus watcher, multi-mouse,
//! keyboards, audio endpoints/ducking, display overrides, USB hot-plug), Win32 `RegisterHotKey`
//! manager, pure-Win32 OSD switch pill, single-instance lock, and local Named Pipe IPC server.
//!
//! Because this host process never initializes `eframe` / `winit` / OpenGL, GPU drivers
//! (`nvoglv64.dll` / AMD ICD) are never loaded while Polled runs in the tray. When the user
//! opens the GUI, the daemon spawns `polled-gui.exe --ui --ipc-pid <pid>` on demand; closing
//! `[X]` exits the UI child process and returns 100% of GPU memory back to Windows immediately.

#[cfg(windows)]
pub fn run_daemon(start_minimized: bool, wait_for_previous_exit: bool) -> Result<(), String> {
    win32_daemon::run(start_minimized, wait_for_previous_exit)
}

#[cfg(not(windows))]
pub fn run_daemon(_start_minimized: bool, _wait_for_previous_exit: bool) -> Result<(), String> {
    Err("Pure-Win32 background daemon is only supported on Windows.".to_string())
}

#[cfg(windows)]
pub(crate) fn trim_process_working_set() {
    use windows::Win32::System::Memory::{
        SetProcessWorkingSetSizeEx, SETPROCESSWORKINGSETSIZEEX_FLAGS,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;

    // SAFETY: Passing `usize::MAX` for both min and max tells Windows to trim unused
    // startup working-set pages immediately (equivalent to `EmptyWorkingSet`).
    unsafe {
        let _ = SetProcessWorkingSetSizeEx(
            GetCurrentProcess(),
            usize::MAX,
            usize::MAX,
            SETPROCESSWORKINGSETSIZEEX_FLAGS(0),
        );
    }
}

#[cfg(windows)]
mod win32_daemon {
    use std::cell::RefCell;
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::process::{Child, Command};
    use std::sync::atomic::{AtomicIsize, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use polled_core::win::WindowsEnv;
    use polled_core::{
        check_latest_github_release, AppPaths, AppSettings, HealthEngine, ProfileStore,
        ScanSummary, UpdateCheckResult,
    };
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
        GetMessageW, PostMessageW, PostQuitMessage, RegisterClassW, TranslateMessage,
        UnregisterClassW, HWND_MESSAGE, MSG, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE,
        WM_DESTROY, WM_ENDSESSION, WM_QUERYENDSESSION, WNDCLASSW,
    };

    use crate::app::WorkerMsg;
    use crate::hotkeys::{HotkeyAction, HotkeyManager};
    use crate::ipc::{DaemonToUiMsg, IpcServer, IpcServerEvent, UiToDaemonMsg};
    use crate::notifications::{
        notify_if_regressed, notify_update_available, spawn_windows_build_watcher,
    };
    use crate::osd::OsdController;
    use crate::profile_runtime::{ProfileRuntimeCmd, ProfileRuntimeHandle, ProfileRuntimeSnapshot};
    use crate::single_instance::{InstanceLock, SingleInstanceGuard};
    use crate::theme::ThemeMode;
    use crate::tray::{TrayAction, TrayController};

    const WM_DAEMON_WAKE: u32 = WM_APP + 10;

    enum DaemonSignal {
        ShowRequested,
    }

    struct DaemonState {
        hwnd: HWND,
        own_pid: u32,
        settings: AppSettings,
        dark_mode: bool,
        auto_switch_enabled: bool,
        profile_locked: bool,
        latest_snapshot: Option<ProfileRuntimeSnapshot>,
        latest_scan: Option<ScanSummary>,
        latest_update: Option<UpdateCheckResult>,
        pending_open_bench: bool,
        pending_focus_on_connect: bool,
        trimmed_after_boot: bool,
        shutting_down: bool,

        ui_child: Option<Child>,
        worker_tx: Sender<WorkerMsg>,
        worker_rx: Receiver<WorkerMsg>,
        tray_rx: Receiver<TrayAction>,
        ipc_msg_rx: Receiver<UiToDaemonMsg>,
        ipc_evt_rx: Receiver<IpcServerEvent>,
        signal_rx: Receiver<DaemonSignal>,
        wake: Arc<dyn Fn() + Send + Sync>,

        ipc_server: IpcServer,
        hotkey_manager: HotkeyManager,
        osd: OsdController,
        tray: TrayController,
        profile_runtime: ProfileRuntimeHandle,
        _instance_guard: SingleInstanceGuard,
    }

    thread_local! {
        static DAEMON: RefCell<Option<DaemonState>> = const { RefCell::new(None) };
    }

    impl DaemonState {
        fn process_all_events(&mut self) {
            if self.shutting_down {
                return;
            }

            // 1. Single-instance wake signals (user launched `polled-gui.exe` again).
            while let Ok(sig) = self.signal_rx.try_recv() {
                match sig {
                    DaemonSignal::ShowRequested => self.spawn_or_focus_ui(false),
                }
            }

            // 2. IPC server lifecycle (UI child connected or closed [X]).
            while let Ok(evt) = self.ipc_evt_rx.try_recv() {
                match evt {
                    IpcServerEvent::ClientConnected => {
                        if let Some(snap) = &self.latest_snapshot {
                            let _ = self
                                .ipc_server
                                .send(DaemonToUiMsg::RuntimeSnapshot(Box::new(snap.clone())));
                        }
                        if let Some(scan) = &self.latest_scan {
                            let _ = self
                                .ipc_server
                                .send(DaemonToUiMsg::CachedScanSummary(scan.clone()));
                        }
                        if let Some(update) = &self.latest_update {
                            let _ = self
                                .ipc_server
                                .send(DaemonToUiMsg::CachedUpdateCheck(update.clone()));
                        }
                        if self.pending_focus_on_connect {
                            self.pending_focus_on_connect = false;
                            let _ = self.ipc_server.send(DaemonToUiMsg::FocusWindow);
                        }
                        if self.pending_open_bench {
                            self.pending_open_bench = false;
                            let _ = self.ipc_server.send(DaemonToUiMsg::ToggleInputLab);
                        }
                    }
                    IpcServerEvent::ClientDisconnected => {
                        self.reap_ui_child_if_exited();
                        super::trim_process_working_set();
                    }
                }
            }

            // 3. Background worker messages (profile runtime, hotkeys, tray/build scans).
            while let Ok(msg) = self.worker_rx.try_recv() {
                match msg {
                    WorkerMsg::ProfileRuntimeUpdated(snap) => {
                        self.auto_switch_enabled = snap.auto_switch_enabled;
                        self.profile_locked = snap.profile_locked;
                        if self.settings.auto_switch_on_startup != snap.auto_switch_enabled {
                            self.settings.auto_switch_on_startup = snap.auto_switch_enabled;
                            let _ = self
                                .settings
                                .save_atomic(&AppPaths::default_user_dir().settings_path());
                        }
                        self.tray.sync_from_snapshot(
                            &snap,
                            self.settings.alerts.battery_tray_icon,
                            self.settings.alerts.clamped_threshold(),
                        );
                        self.latest_snapshot = Some((*snap).clone());
                        let _ = self.ipc_server.send(DaemonToUiMsg::RuntimeSnapshot(snap));
                        if !self.trimmed_after_boot {
                            self.trimmed_after_boot = true;
                            super::trim_process_working_set();
                        }
                    }
                    WorkerMsg::ShowOsdPill { title, subtitle } => {
                        self.osd.show(title, subtitle, self.dark_mode);
                    }
                    WorkerMsg::HotkeyTriggered(action) => match action {
                        HotkeyAction::CycleProfile => {
                            self.profile_runtime
                                .send(ProfileRuntimeCmd::CycleNextProfile);
                        }
                        HotkeyAction::ToggleProfileLock => {
                            let next = !self.profile_locked;
                            self.profile_locked = next;
                            self.profile_runtime
                                .send(ProfileRuntimeCmd::SetProfileLock(next));
                        }
                        HotkeyAction::ToggleInputLab => {
                            if self.is_ui_alive() && self.ipc_server.has_client() {
                                let _ = self.ipc_server.send(DaemonToUiMsg::ToggleInputLab);
                            } else {
                                self.spawn_or_focus_ui(true);
                            }
                        }
                        HotkeyAction::CycleAudioOutput => {
                            self.profile_runtime
                                .send(ProfileRuntimeCmd::CycleAudioOutput);
                        }
                        HotkeyAction::CycleWootingProfile => {
                            self.profile_runtime
                                .send(ProfileRuntimeCmd::CycleWootingProfile);
                        }
                        HotkeyAction::ToggleMicMute => {
                            self.profile_runtime.send(ProfileRuntimeCmd::ToggleMicMute);
                        }
                    },
                    WorkerMsg::ReadFromMouseFinished {
                        device_name,
                        settings,
                        error,
                    } => {
                        let _ = self.ipc_server.send(DaemonToUiMsg::ReadFromMouseFinished {
                            device_name,
                            settings,
                            error,
                        });
                    }
                    WorkerMsg::DisplayTestStarted {
                        device_name,
                        applied_label,
                        previous_label,
                        error,
                    } => {
                        let _ = self.ipc_server.send(DaemonToUiMsg::DisplayTestStarted {
                            device_name,
                            applied_label,
                            previous_label,
                            error,
                        });
                    }
                    WorkerMsg::DisplayTestReverted(msg) => {
                        let _ = self
                            .ipc_server
                            .send(DaemonToUiMsg::DisplayTestReverted(msg));
                    }
                    WorkerMsg::WootingActionResult { ok, message } => {
                        let _ = self
                            .ipc_server
                            .send(DaemonToUiMsg::WootingActionResult { ok, message });
                    }
                    WorkerMsg::ScanFinished {
                        result,
                        notify_regressions,
                    } => {
                        if let Ok(ref summary) = result {
                            if notify_regressions {
                                notify_if_regressed(summary);
                            }
                            self.latest_scan = Some(summary.clone());
                        }
                        let _ = self.ipc_server.send(DaemonToUiMsg::ScanFinished {
                            result,
                            notify_regressions: false,
                        });
                    }
                    WorkerMsg::TraySyncAutoSwitch(enabled) => {
                        self.auto_switch_enabled = enabled;
                        self.settings.auto_switch_on_startup = enabled;
                        let _ = self
                            .ipc_server
                            .send(DaemonToUiMsg::TraySyncAutoSwitch(enabled));
                    }
                    WorkerMsg::UpdateCheckFinished(Ok(info)) => {
                        if info.update_available {
                            notify_update_available(&info);
                        }
                        self.latest_update = Some(info.clone());
                        let _ = self.ipc_server.send(DaemonToUiMsg::CachedUpdateCheck(info));
                    }
                    WorkerMsg::ActionFinished { .. }
                    | WorkerMsg::UpdateCheckFinished(Err(_))
                    | WorkerMsg::UpdateInstallProgress(_)
                    | WorkerMsg::UpdateInstallFinished(_)
                    | WorkerMsg::TrayAction(_)
                    | WorkerMsg::DaemonFocusWindow
                    | WorkerMsg::DaemonShutdownUi
                    | WorkerMsg::DaemonCachedScan(_)
                    | WorkerMsg::DaemonCachedUpdate(_) => {}
                }
            }

            // 4. Native tray icon & context menu actions.
            while let Ok(action) = self.tray_rx.try_recv() {
                match action {
                    TrayAction::Open => self.spawn_or_focus_ui(false),
                    TrayAction::ScanNow => {
                        let tx_scan = self.worker_tx.clone();
                        let wake_scan = Arc::clone(&self.wake);
                        std::thread::spawn(move || {
                            let paths = AppPaths::default_user_dir();
                            let engine = HealthEngine::with_default_and_custom_checks(&paths);
                            let result = engine
                                .scan_all(&WindowsEnv, &paths, true)
                                .map_err(|e| e.to_string());
                            if let Ok(summary) = &result {
                                notify_if_regressed(summary);
                            }
                            let _ = tx_scan.send(WorkerMsg::ScanFinished {
                                result,
                                notify_regressions: false,
                            });
                            wake_scan();
                        });
                    }
                    TrayAction::ApplyProfile(id) => {
                        self.profile_runtime
                            .send(ProfileRuntimeCmd::ApplyProfileNow(id));
                    }
                    TrayAction::QuickSetMousePolling(hz) => {
                        self.profile_runtime
                            .send(ProfileRuntimeCmd::QuickSetMousePolling(hz));
                    }
                    TrayAction::QuickSetMouseDpi(dpi) => {
                        self.profile_runtime
                            .send(ProfileRuntimeCmd::QuickSetMouseDpi(dpi));
                    }
                    TrayAction::RefreshMouseAndBattery => {
                        self.profile_runtime
                            .send(ProfileRuntimeCmd::RefreshMouseAndBattery);
                    }
                    TrayAction::SwitchKeyboardProfile(slot) => {
                        self.profile_runtime
                            .send(ProfileRuntimeCmd::SwitchWootingProfile {
                                device_id: None,
                                slot,
                            });
                    }
                    TrayAction::RefreshKeyboards => {
                        self.profile_runtime.send(ProfileRuntimeCmd::RefreshWooting);
                    }
                    TrayAction::SwitchAudioOutput(audio_id) => {
                        self.profile_runtime
                            .send(ProfileRuntimeCmd::SwitchAudioOutput(audio_id));
                    }
                    TrayAction::ToggleMicMute => {
                        self.profile_runtime.send(ProfileRuntimeCmd::ToggleMicMute);
                    }
                    TrayAction::RestoreNativeDisplay => {
                        self.profile_runtime
                            .send(ProfileRuntimeCmd::RestoreOriginalDisplay);
                    }
                    TrayAction::ToggleProfileLock => {
                        let next = !self.profile_locked;
                        self.profile_locked = next;
                        self.profile_runtime
                            .send(ProfileRuntimeCmd::SetProfileLock(next));
                    }
                    TrayAction::ToggleAutoSwitch => {
                        let next = !self.auto_switch_enabled;
                        self.auto_switch_enabled = next;
                        self.settings.auto_switch_on_startup = next;
                        let paths = AppPaths::default_user_dir();
                        let mut saved = AppSettings::load_or_default(&paths.settings_path());
                        saved.auto_switch_on_startup = next;
                        let _ = saved.save_atomic(&paths.settings_path());
                        self.profile_runtime
                            .send(ProfileRuntimeCmd::SetAutoSwitch(next));
                        let _ = self
                            .ipc_server
                            .send(DaemonToUiMsg::TraySyncAutoSwitch(next));
                    }
                    TrayAction::Quit => {
                        self.begin_shutdown();
                        return;
                    }
                }
            }

            // 5. Messages sent from the attached `--ui` window.
            while let Ok(msg) = self.ipc_msg_rx.try_recv() {
                match msg {
                    UiToDaemonMsg::RuntimeCmd(cmd) => {
                        if let ProfileRuntimeCmd::UpdateAlertConfig(ref alerts) = cmd {
                            self.settings.alerts = alerts.clone();
                            if let Some(snap) = &self.latest_snapshot {
                                self.tray.sync_from_snapshot(
                                    snap,
                                    self.settings.alerts.battery_tray_icon,
                                    self.settings.alerts.clamped_threshold(),
                                );
                            }
                        }
                        self.profile_runtime.send(cmd);
                    }
                    UiToDaemonMsg::UpdateHotkeys(cfg) => {
                        self.settings.hotkeys = cfg.clone();
                        self.hotkey_manager.update_config(cfg);
                    }
                    UiToDaemonMsg::UpdateThemeDark(dark) => {
                        self.dark_mode = dark;
                    }
                    UiToDaemonMsg::ShowOsdPill { title, subtitle } => {
                        self.osd.show(title, subtitle, self.dark_mode);
                    }
                    UiToDaemonMsg::SyncScanSummary(summary) => {
                        self.latest_scan = Some(summary);
                    }
                    UiToDaemonMsg::SyncUpdateCheck(info) => {
                        self.latest_update = Some(info);
                    }
                    UiToDaemonMsg::RequestQuitDaemon => {
                        self.begin_shutdown();
                        return;
                    }
                }
            }
        }

        fn is_ui_alive(&mut self) -> bool {
            if let Some(child) = self.ui_child.as_mut() {
                match child.try_wait() {
                    Ok(None) => return true,
                    Ok(Some(_)) | Err(_) => {
                        self.ui_child = None;
                    }
                }
            }
            false
        }

        fn reap_ui_child_if_exited(&mut self) {
            let _ = self.is_ui_alive();
        }

        fn spawn_or_focus_ui(&mut self, open_bench: bool) {
            if self.is_ui_alive() {
                if let Some(child) = &self.ui_child {
                    // SAFETY: Grants foreground focus permission specifically to our own UI child PID.
                    unsafe {
                        let _ = AllowSetForegroundWindow(child.id());
                    }
                }
                if self.ipc_server.has_client() {
                    let _ = self.ipc_server.send(DaemonToUiMsg::FocusWindow);
                    if open_bench {
                        let _ = self.ipc_server.send(DaemonToUiMsg::ToggleInputLab);
                    }
                } else {
                    self.pending_focus_on_connect = true;
                    self.pending_open_bench |= open_bench;
                }
                return;
            }

            let Ok(exe) = std::env::current_exe() else {
                return;
            };
            let mut cmd = Command::new(exe);
            cmd.arg("--ui")
                .arg("--ipc-pid")
                .arg(self.own_pid.to_string());
            if open_bench {
                cmd.arg("--open-bench");
            }
            if let Ok(child) = cmd.spawn() {
                // SAFETY: Allows the freshly spawned UI window to come to the foreground cleanly.
                unsafe {
                    let _ = AllowSetForegroundWindow(child.id());
                }
                self.ui_child = Some(child);
            }
        }

        fn begin_shutdown(&mut self) {
            if self.shutting_down {
                return;
            }
            self.shutting_down = true;

            // Ask any attached GUI child to exit gracefully first.
            let _ = self.ipc_server.send(DaemonToUiMsg::ShutdownUi);
            if let Some(mut child) = self.ui_child.take() {
                let deadline = Instant::now() + Duration::from_millis(1_500);
                while Instant::now() < deadline {
                    if !matches!(child.try_wait(), Ok(None)) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
            }

            // SAFETY: Posting `WM_CLOSE` to our own daemon message window cleanly ends `GetMessageW`.
            unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
    }

    unsafe extern "system" fn daemon_wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_DAEMON_WAKE => {
                let mut repost = false;
                DAEMON.with(|cell| match cell.try_borrow_mut() {
                    Ok(mut opt) => {
                        if let Some(state) = opt.as_mut() {
                            state.process_all_events();
                        }
                    }
                    Err(_) => {
                        // A nested Win32 modal loop (e.g. tray popup menu) re-entered while
                        // `DAEMON` was borrowed; repost so events drain immediately afterwards.
                        repost = true;
                    }
                });
                if repost {
                    unsafe {
                        let _ = PostMessageW(Some(hwnd), WM_DAEMON_WAKE, WPARAM(0), LPARAM(0));
                    }
                }
                LRESULT(0)
            }
            WM_QUERYENDSESSION => LRESULT(1),
            WM_ENDSESSION => {
                if wparam.0 != 0 {
                    DAEMON.with(|cell| {
                        if let Ok(mut opt) = cell.try_borrow_mut() {
                            if let Some(state) = opt.as_mut() {
                                state.begin_shutdown();
                            }
                        }
                    });
                    unsafe {
                        let _ = DestroyWindow(hwnd);
                    }
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                unsafe {
                    PostQuitMessage(0);
                }
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    pub fn run(start_minimized: bool, wait_for_previous_exit: bool) -> Result<(), String> {
        let mut instance_guard =
            match crate::acquire_instance_guard(!start_minimized, wait_for_previous_exit) {
                InstanceLock::Primary(guard) => guard,
                InstanceLock::AlreadyRunning => return Ok(()),
            };
        crate::spawn_stale_backup_cleanup(wait_for_previous_exit);

        let class_str = format!("PolledDaemonHost_{}", std::process::id());
        let class_w: Vec<u16> = OsStr::new(&class_str)
            .encode_wide()
            .chain(Some(0))
            .collect();

        let hmodule = unsafe { GetModuleHandleW(PCWSTR::null()) }.unwrap_or_default();
        let hinstance = HINSTANCE(hmodule.0);

        let wc = WNDCLASSW {
            lpfnWndProc: Some(daemon_wnd_proc),
            hInstance: hinstance,
            lpszClassName: PCWSTR(class_w.as_ptr()),
            ..Default::default()
        };
        if unsafe { RegisterClassW(&wc) } == 0 {
            return Err("RegisterClassW failed for Polled daemon host".to_string());
        }

        let hwnd = match unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR(class_w.as_ptr()),
                PCWSTR::null(),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(hinstance),
                None,
            )
        } {
            Ok(h) if !h.is_invalid() => h,
            _ => {
                unsafe {
                    let _ = UnregisterClassW(PCWSTR(class_w.as_ptr()), Some(hinstance));
                }
                return Err("CreateWindowExW failed for Polled daemon host".to_string());
            }
        };

        let hwnd_slot = Arc::new(AtomicIsize::new(hwnd.0 as isize));
        let wake_slot = Arc::clone(&hwnd_slot);
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let raw = wake_slot.load(Ordering::SeqCst);
            if raw != 0 {
                let h = HWND(raw as *mut core::ffi::c_void);
                // SAFETY: Posting custom `WM_DAEMON_WAKE` to our own daemon message window.
                unsafe {
                    let _ = PostMessageW(Some(h), WM_DAEMON_WAKE, WPARAM(0), LPARAM(0));
                }
            }
        });

        let paths = AppPaths::default_user_dir();
        let settings = AppSettings::load_or_default(&paths.settings_path());
        let dark_mode = matches!(ThemeMode::from(settings.theme), ThemeMode::Dark);
        let store = ProfileStore::load_or_default(&paths.profiles_path()).unwrap_or_default();

        let (worker_tx, worker_rx) = mpsc::channel();
        let (tray_tx, tray_rx) = mpsc::channel();
        let (ipc_msg_tx, ipc_msg_rx) = mpsc::channel();
        let (ipc_evt_tx, ipc_evt_rx) = mpsc::channel();
        let (signal_tx, signal_rx) = mpsc::channel();

        let own_pid = std::process::id();
        let ipc_server = IpcServer::start(own_pid, ipc_msg_tx, ipc_evt_tx, Arc::clone(&wake))?;

        let profile_runtime = ProfileRuntimeHandle::start(
            store,
            settings.auto_switch_on_startup,
            settings.alerts.clone(),
            settings.audio_cycle_device_ids.clone(),
            worker_tx.clone(),
            Arc::clone(&wake),
        );
        let hotkey_manager = HotkeyManager::start(
            settings.hotkeys.clone(),
            worker_tx.clone(),
            Arc::clone(&wake),
        );
        let osd = OsdController::start();

        let wake_tray = Arc::clone(&wake);
        let tray = TrayController::new(
            settings.auto_switch_on_startup,
            Arc::new(move |action| {
                let _ = tray_tx.send(action);
                wake_tray();
            }),
        );

        let wake_show = Arc::clone(&wake);
        instance_guard.start_show_listener(Arc::new(move || {
            let _ = signal_tx.send(DaemonSignal::ShowRequested);
            wake_show();
        }));

        spawn_windows_build_watcher(worker_tx.clone(), Arc::clone(&wake));

        if settings.check_updates_on_startup {
            let tx_upd = worker_tx.clone();
            let wake_upd = Arc::clone(&wake);
            let delay = if start_minimized {
                Duration::from_secs(4)
            } else {
                Duration::ZERO
            };
            std::thread::spawn(move || {
                if !delay.is_zero() {
                    std::thread::sleep(delay);
                }
                if let Ok(info) = check_latest_github_release() {
                    let _ = tx_upd.send(WorkerMsg::UpdateCheckFinished(Ok(info)));
                    wake_upd();
                }
            });
        }

        let auto_switch_enabled = settings.auto_switch_on_startup;
        let mut state = DaemonState {
            hwnd,
            own_pid,
            settings,
            dark_mode,
            auto_switch_enabled,
            profile_locked: false,
            latest_snapshot: None,
            latest_scan: None,
            latest_update: None,
            pending_open_bench: false,
            pending_focus_on_connect: false,
            trimmed_after_boot: false,
            shutting_down: false,
            ui_child: None,
            worker_tx,
            worker_rx,
            tray_rx,
            ipc_msg_rx,
            ipc_evt_rx,
            signal_rx,
            wake,
            ipc_server,
            hotkey_manager,
            osd,
            tray,
            profile_runtime,
            _instance_guard: instance_guard,
        };

        if !start_minimized {
            state.spawn_or_focus_ui(false);
        }

        DAEMON.with(|cell| {
            *cell.borrow_mut() = Some(state);
        });

        // Drain any events produced during initialization before entering the idle Win32 message loop.
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_DAEMON_WAKE, WPARAM(0), LPARAM(0));
        }
        super::trim_process_working_set();

        let mut msg = MSG::default();
        while unsafe { GetMessageW(&mut msg, None, 0, 0).0 > 0 } {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        // Prevent late background callbacks from posting to the destroyed HWND during teardown.
        hwnd_slot.store(0, Ordering::SeqCst);

        // Deterministic teardown: drops IPC server, hotkeys, OSD, tray icon, and joins
        // `profile_runtime` (restoring temporary display overrides, ducked audio, and toasts).
        DAEMON.with(|cell| {
            cell.borrow_mut().take();
        });

        unsafe {
            let _ = UnregisterClassW(PCWSTR(class_w.as_ptr()), Some(hinstance));
        }

        Ok(())
    }
}
