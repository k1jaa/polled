// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Local-only Win32 Named Pipe IPC bridge between the pure-Win32 tray daemon and
//! on-demand `eframe` UI child windows (`polled-gui.exe --ui --ipc-pid <pid>`).
//!
//! Uses two unidirectional kernel named pipes per daemon PID (`PolledIpcC2S_<pid>` and
//! `PolledIpcS2C_<pid>`) with `PIPE_REJECT_REMOTE_CLIENTS`:
//! - Zero network sockets, zero firewall prompts, zero anti-cheat concerns.
//! - Zero half-duplex read/write deadlocks (dedicated reader and writer pipes).
//! - Clean reconnect across repeated UI open/close cycles.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use polled_core::{HotkeyConfig, MouseSettings, ScanSummary};
use serde::{Deserialize, Serialize};

use crate::profile_runtime::{ProfileRuntimeCmd, ProfileRuntimeSnapshot};

/// Commands sent from the on-demand GUI window (`--ui`) to the pure-Win32 background daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum UiToDaemonMsg {
    RuntimeCmd(ProfileRuntimeCmd),
    UpdateHotkeys(HotkeyConfig),
    UpdateThemeDark(bool),
    ShowOsdPill { title: String, subtitle: String },
    SyncScanSummary(ScanSummary),
    RequestQuitDaemon,
}

/// Live state and events streamed from the background daemon to an attached GUI window (`--ui`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DaemonToUiMsg {
    RuntimeSnapshot(Box<ProfileRuntimeSnapshot>),
    CachedScanSummary(ScanSummary),
    TraySyncAutoSwitch(bool),
    ReadFromMouseFinished {
        device_name: Option<String>,
        settings: Option<MouseSettings>,
        error: Option<String>,
    },
    DisplayTestStarted {
        device_name: String,
        applied_label: String,
        previous_label: String,
        error: Option<String>,
    },
    DisplayTestReverted(String),
    WootingActionResult {
        ok: bool,
        message: String,
    },
    ScanFinished {
        result: Result<ScanSummary, String>,
        notify_regressions: bool,
    },
    FocusWindow,
    ToggleInputLab,
    ShutdownUi,
}

/// Lifecycle notifications emitted by `IpcServer` to the daemon main loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcServerEvent {
    ClientConnected,
    ClientDisconnected,
}

pub fn c2s_pipe_name(daemon_id: u32) -> String {
    format!(r"\\.\pipe\PolledIpcC2S_{daemon_id}")
}

pub fn s2c_pipe_name(daemon_id: u32) -> String {
    format!(r"\\.\pipe\PolledIpcS2C_{daemon_id}")
}

fn write_json_line<T: Serialize, W: Write>(writer: &mut W, msg: &T) -> std::io::Result<()> {
    let json = serde_json::to_string(msg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    writer.write_all(json.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}

/// Daemon-side local IPC server accepting one on-demand `--ui` window at a time and
/// automatically re-listening after the user closes the window.
#[derive(Default)]
struct ServerStopState {
    shutdown: bool,
    pipes_created: bool,
}

pub struct IpcServer {
    daemon_id: u32,
    shutdown: Arc<AtomicBool>,
    stop_state: Arc<Mutex<ServerStopState>>,
    active_client: Arc<Mutex<Option<Sender<DaemonToUiMsg>>>>,
    accept_handle: Option<JoinHandle<()>>,
}

impl IpcServer {
    #[cfg(windows)]
    pub fn start(
        daemon_id: u32,
        ui_msg_tx: Sender<UiToDaemonMsg>,
        event_tx: Sender<IpcServerEvent>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, String> {
        let shutdown = Arc::new(AtomicBool::new(false));
        let stop_state = Arc::new(Mutex::new(ServerStopState::default()));
        let active_client: Arc<Mutex<Option<Sender<DaemonToUiMsg>>>> = Arc::new(Mutex::new(None));
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();

        let shutdown_bg = Arc::clone(&shutdown);
        let stop_bg = Arc::clone(&stop_state);
        let client_bg = Arc::clone(&active_client);

        let accept_handle = thread::Builder::new()
            .name("polled-ipc-server".to_string())
            .spawn(move || {
                run_windows_pipe_server(
                    daemon_id,
                    shutdown_bg,
                    stop_bg,
                    client_bg,
                    ui_msg_tx,
                    event_tx,
                    wake,
                    ready_tx,
                );
            })
            .map_err(|e| format!("failed to spawn IPC server thread: {e}"))?;

        match ready_rx.recv_timeout(Duration::from_secs(3)) {
            Ok(Ok(())) => Ok(Self {
                daemon_id,
                shutdown,
                stop_state,
                active_client,
                accept_handle: Some(accept_handle),
            }),
            Ok(Err(err)) => {
                shutdown.store(true, Ordering::SeqCst);
                let _ = accept_handle.join();
                Err(err)
            }
            Err(_) => {
                shutdown.store(true, Ordering::SeqCst);
                let _ = accept_handle.join();
                Err("timed out waiting for local IPC named pipes to initialize".to_string())
            }
        }
    }

    #[cfg(not(windows))]
    pub fn start(
        daemon_id: u32,
        _ui_msg_tx: Sender<UiToDaemonMsg>,
        _event_tx: Sender<IpcServerEvent>,
        _wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, String> {
        Ok(Self {
            daemon_id,
            shutdown: Arc::new(AtomicBool::new(false)),
            stop_state: Arc::new(Mutex::new(ServerStopState::default())),
            active_client: Arc::new(Mutex::new(None)),
            accept_handle: None,
        })
    }

    /// Returns `true` when an on-demand GUI child window is currently connected.
    pub fn has_client(&self) -> bool {
        self.active_client
            .lock()
            .map(|g| g.is_some())
            .unwrap_or(false)
    }

    /// Non-blocking send to the currently connected GUI window (no-op when closed).
    pub fn send(&self, msg: DaemonToUiMsg) -> bool {
        if let Ok(guard) = self.active_client.lock() {
            if let Some(tx) = guard.as_ref() {
                return tx.send(msg).is_ok();
            }
        }
        false
    }

    /// Signals shutdown, disconnects any attached client, and unblocks `ConnectNamedPipe`.
    pub fn stop(&mut self) {
        if self.shutdown.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = self.send(DaemonToUiMsg::ShutdownUi);
        if let Ok(mut guard) = self.active_client.lock() {
            guard.take();
        }

        // Synchronize with `run_windows_pipe_server` so we never race between `CreateNamedPipeW`
        // and `ConnectNamedPipe`: if pipes are already created, wake `ConnectNamedPipe` by opening
        // them briefly; if not yet created, the server thread will see `shutdown == true` under the
        // same lock right after `CreateNamedPipeW` and exit without calling `ConnectNamedPipe`.
        let should_wake_pipes = self
            .stop_state
            .lock()
            .map(|mut st| {
                st.shutdown = true;
                st.pipes_created
            })
            .unwrap_or(true);

        if should_wake_pipes {
            let _ = OpenOptions::new()
                .write(true)
                .open(c2s_pipe_name(self.daemon_id));
            let _ = OpenOptions::new()
                .read(true)
                .open(s2c_pipe_name(self.daemon_id));
        }

        if let Some(handle) = self.accept_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Client handle held by an on-demand `--ui` process to communicate with the daemon.
pub struct IpcClient {
    tx: Option<Sender<UiToDaemonMsg>>,
    writer_handle: Option<JoinHandle<()>>,
    _reader_handle: Option<JoinHandle<()>>,
}

impl IpcClient {
    /// Connects to the daemon's C2S and S2C named pipes with a short startup retry window.
    pub fn connect(
        daemon_id: u32,
        on_daemon_msg: impl Fn(DaemonToUiMsg) + Send + 'static,
        on_disconnected: impl FnOnce() + Send + 'static,
    ) -> Result<Self, String> {
        let c2s_path = c2s_pipe_name(daemon_id);
        let s2c_path = s2c_pipe_name(daemon_id);
        let deadline = Instant::now() + Duration::from_secs(4);

        let c2s_file = loop {
            match OpenOptions::new().write(true).open(&c2s_path) {
                Ok(f) => break f,
                Err(err) => {
                    if Instant::now() >= deadline {
                        return Err(format!(
                            "could not connect to Polled daemon pipe {c2s_path}: {err}"
                        ));
                    }
                    thread::sleep(Duration::from_millis(25));
                }
            }
        };

        let s2c_file = loop {
            match OpenOptions::new().read(true).open(&s2c_path) {
                Ok(f) => break f,
                Err(err) => {
                    if Instant::now() >= deadline {
                        return Err(format!(
                            "could not connect to Polled daemon stream {s2c_path}: {err}"
                        ));
                    }
                    thread::sleep(Duration::from_millis(25));
                }
            }
        };

        let (tx, rx) = mpsc::channel::<UiToDaemonMsg>();

        let writer_handle = thread::Builder::new()
            .name("polled-ui-ipc-tx".to_string())
            .spawn(move || {
                let mut writer = BufWriter::new(c2s_file);
                while let Ok(msg) = rx.recv() {
                    if write_json_line(&mut writer, &msg).is_err() {
                        break;
                    }
                }
            })
            .ok();

        let reader_handle = thread::Builder::new()
            .name("polled-ui-ipc-rx".to_string())
            .spawn(move || {
                let mut reader = BufReader::new(s2c_file);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            let trimmed = line.trim();
                            if trimmed.is_empty() {
                                continue;
                            }
                            if let Ok(msg) = serde_json::from_str::<DaemonToUiMsg>(trimmed) {
                                on_daemon_msg(msg);
                            }
                        }
                    }
                }
                on_disconnected();
            })
            .ok();

        Ok(Self {
            tx: Some(tx),
            writer_handle,
            _reader_handle: reader_handle,
        })
    }

    pub fn sender(&self) -> Option<Sender<UiToDaemonMsg>> {
        self.tx.clone()
    }

    pub fn send(&self, msg: UiToDaemonMsg) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(msg);
        }
    }
}

impl Drop for IpcClient {
    fn drop(&mut self) {
        // Closing `tx` terminates the writer thread and closes the C2S pipe handle so the
        // daemon observes clean EOF immediately.
        self.tx.take();
        if let Some(handle) = self.writer_handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn run_windows_pipe_server(
    daemon_id: u32,
    shutdown: Arc<AtomicBool>,
    stop_state: Arc<Mutex<ServerStopState>>,
    active_client: Arc<Mutex<Option<Sender<DaemonToUiMsg>>>>,
    ui_msg_tx: Sender<UiToDaemonMsg>,
    event_tx: Sender<IpcServerEvent>,
    wake: Arc<dyn Fn() + Send + Sync>,
    ready_tx: Sender<Result<(), String>>,
) {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, HANDLE};
    use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_WAIT,
    };

    const PIPE_ACCESS_INBOUND_RAW: u32 = 0x0000_0001;
    const PIPE_ACCESS_OUTBOUND_RAW: u32 = 0x0000_0002;
    const PIPE_BUF_BYTES: u32 = 65_536;
    const MAX_INSTANCES: u32 = 4;

    fn to_wide_null(s: &str) -> Vec<u16> {
        OsStr::new(s).encode_wide().chain(Some(0)).collect()
    }

    fn create_pipe_pair(daemon_id: u32) -> Result<(HANDLE, HANDLE), String> {
        let c2s_w = to_wide_null(&c2s_pipe_name(daemon_id));
        let s2c_w = to_wide_null(&s2c_pipe_name(daemon_id));
        let mode = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;

        // SAFETY: Null-terminated UTF-16 pipe names with valid flags and default security descriptor.
        let c2s = unsafe {
            CreateNamedPipeW(
                PCWSTR(c2s_w.as_ptr()),
                FILE_FLAGS_AND_ATTRIBUTES(PIPE_ACCESS_INBOUND_RAW),
                mode,
                MAX_INSTANCES,
                PIPE_BUF_BYTES,
                PIPE_BUF_BYTES,
                0,
                None,
            )
        };
        if c2s.is_invalid() {
            return Err("CreateNamedPipeW failed for C2S pipe".to_string());
        }

        // SAFETY: Same as above for outbound S2C pipe.
        let s2c = unsafe {
            CreateNamedPipeW(
                PCWSTR(s2c_w.as_ptr()),
                FILE_FLAGS_AND_ATTRIBUTES(PIPE_ACCESS_OUTBOUND_RAW),
                mode,
                MAX_INSTANCES,
                PIPE_BUF_BYTES,
                PIPE_BUF_BYTES,
                0,
                None,
            )
        };
        if s2c.is_invalid() {
            // SAFETY: `c2s` is valid and owned by us on error path.
            unsafe {
                let _ = CloseHandle(c2s);
            }
            return Err("CreateNamedPipeW failed for S2C pipe".to_string());
        }

        Ok((c2s, s2c))
    }

    fn connect_pipe(handle: HANDLE) -> bool {
        // SAFETY: `handle` is a valid named-pipe server handle created by `CreateNamedPipeW`.
        match unsafe { ConnectNamedPipe(handle, None) } {
            Ok(()) => true,
            Err(err) => err.code() == ERROR_PIPE_CONNECTED.to_hresult(),
        }
    }

    let mut ready_signal = Some(ready_tx);

    while !shutdown.load(Ordering::SeqCst) {
        let (c2s, s2c) = match create_pipe_pair(daemon_id) {
            Ok(pair) => {
                if let Some(tx) = ready_signal.take() {
                    let _ = tx.send(Ok(()));
                }
                pair
            }
            Err(err) => {
                if let Some(tx) = ready_signal.take() {
                    let _ = tx.send(Err(err));
                    return;
                }
                thread::sleep(Duration::from_millis(100));
                continue;
            }
        };

        let should_stop = stop_state
            .lock()
            .map(|mut st| {
                if st.shutdown {
                    true
                } else {
                    st.pipes_created = true;
                    false
                }
            })
            .unwrap_or(true);
        if should_stop {
            unsafe {
                let _ = CloseHandle(c2s);
                let _ = CloseHandle(s2c);
            }
            break;
        }

        let c2s_ok = connect_pipe(c2s);
        let s2c_ok = if !shutdown.load(Ordering::SeqCst) && c2s_ok {
            connect_pipe(s2c)
        } else {
            false
        };

        if let Ok(mut st) = stop_state.lock() {
            st.pipes_created = false;
        }

        if shutdown.load(Ordering::SeqCst) || !c2s_ok || !s2c_ok {
            // SAFETY: Both handles are still owned by us and not wrapped in `File`.
            unsafe {
                let _ = CloseHandle(c2s);
                let _ = CloseHandle(s2c);
            }
            if shutdown.load(Ordering::SeqCst) {
                break;
            }
            continue;
        }

        // SAFETY: Both handles are connected Win32 pipe handles owned exclusively by these `File`s.
        let c2s_file = unsafe { File::from_raw_handle(c2s.0) };
        let s2c_file = unsafe { File::from_raw_handle(s2c.0) };

        let (s2c_tx, s2c_rx) = mpsc::channel::<DaemonToUiMsg>();
        if let Ok(mut guard) = active_client.lock() {
            *guard = Some(s2c_tx);
        }

        let writer_handle = thread::Builder::new()
            .name("polled-ipc-writer".to_string())
            .spawn(move || {
                let mut writer = BufWriter::new(s2c_file);
                while let Ok(msg) = s2c_rx.recv() {
                    if write_json_line(&mut writer, &msg).is_err() {
                        break;
                    }
                }
            })
            .ok();

        let _ = event_tx.send(IpcServerEvent::ClientConnected);
        wake();

        let mut reader = BufReader::new(c2s_file);
        let mut line = String::new();
        while !shutdown.load(Ordering::SeqCst) {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if let Ok(msg) = serde_json::from_str::<UiToDaemonMsg>(trimmed) {
                        let _ = ui_msg_tx.send(msg);
                        wake();
                    }
                }
            }
        }

        // Dropping the sender closes `s2c_rx`, letting `polled-ipc-writer` exit and close `s2c`.
        if let Ok(mut guard) = active_client.lock() {
            guard.take();
        }
        drop(reader);
        if let Some(wh) = writer_handle {
            let _ = wh.join();
        }

        let _ = event_tx.send(IpcServerEvent::ClientDisconnected);
        wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile_runtime::BackendSelection;
    use polled_core::WootingAnalogBasePatch;

    #[test]
    fn roundtrip_ui_to_daemon_messages() {
        let msgs = vec![
            UiToDaemonMsg::RuntimeCmd(ProfileRuntimeCmd::SetAutoSwitch(true)),
            UiToDaemonMsg::RuntimeCmd(ProfileRuntimeCmd::QuickSetMousePolling(4000)),
            UiToDaemonMsg::RuntimeCmd(ProfileRuntimeCmd::SelectBackend(
                BackendSelection::AutoConnected,
            )),
            UiToDaemonMsg::RuntimeCmd(ProfileRuntimeCmd::WriteWootingAnalogBase {
                device_id: Some("wooting_1".to_string()),
                patch: WootingAnalogBasePatch {
                    actuation_mm: Some(0.4),
                    rapid_trigger_enabled: Some(true),
                    rapid_trigger_sensitivity_mm: Some(0.15),
                },
            }),
            UiToDaemonMsg::UpdateHotkeys(HotkeyConfig::default()),
            UiToDaemonMsg::UpdateThemeDark(true),
            UiToDaemonMsg::ShowOsdPill {
                title: "Polled".to_string(),
                subtitle: "CS2 · 1600 DPI".to_string(),
            },
            UiToDaemonMsg::RequestQuitDaemon,
        ];

        for msg in msgs {
            let encoded = serde_json::to_string(&msg).expect("serialize UiToDaemonMsg");
            let decoded: UiToDaemonMsg =
                serde_json::from_str(&encoded).expect("deserialize UiToDaemonMsg");
            assert_eq!(
                serde_json::to_string(&decoded).unwrap(),
                encoded,
                "round-trip JSON mismatch"
            );
        }
    }

    #[test]
    fn roundtrip_daemon_to_ui_messages() {
        let msgs = vec![
            DaemonToUiMsg::TraySyncAutoSwitch(false),
            DaemonToUiMsg::ReadFromMouseFinished {
                device_name: Some("Superlight 2".to_string()),
                settings: Some(MouseSettings {
                    dpi: Some(1600),
                    polling_rate_hz: Some(4000),
                    ..Default::default()
                }),
                error: None,
            },
            DaemonToUiMsg::DisplayTestStarted {
                device_name: r"\\.\DISPLAY1".to_string(),
                applied_label: "1280×960 @ 360 Hz".to_string(),
                previous_label: "1920×1080 @ 360 Hz".to_string(),
                error: None,
            },
            DaemonToUiMsg::DisplayTestReverted("Reverted".to_string()),
            DaemonToUiMsg::WootingActionResult {
                ok: true,
                message: "Switched to P2".to_string(),
            },
            DaemonToUiMsg::FocusWindow,
            DaemonToUiMsg::ToggleInputLab,
            DaemonToUiMsg::ShutdownUi,
        ];

        for msg in msgs {
            let encoded = serde_json::to_string(&msg).expect("serialize DaemonToUiMsg");
            let decoded: DaemonToUiMsg =
                serde_json::from_str(&encoded).expect("deserialize DaemonToUiMsg");
            assert_eq!(
                serde_json::to_string(&decoded).unwrap(),
                encoded,
                "round-trip JSON mismatch"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn named_pipe_live_roundtrip_and_reconnect() {
        let test_id = 0x7100_0000 | (std::process::id() & 0xFFFF);
        let (ui_msg_tx, ui_msg_rx) = mpsc::channel();
        let (evt_tx, evt_rx) = mpsc::channel();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});

        let mut server = IpcServer::start(test_id, ui_msg_tx, evt_tx, wake)
            .expect("start named pipe IPC server");

        // Session 1: connect, exchange messages in both directions, then close client (simulating [X]).
        {
            let (d2u_tx, d2u_rx) = mpsc::channel();
            let client = IpcClient::connect(
                test_id,
                move |msg| {
                    let _ = d2u_tx.send(msg);
                },
                || {},
            )
            .expect("connect first IPC client");

            assert_eq!(
                evt_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
                IpcServerEvent::ClientConnected
            );
            assert!(server.has_client());

            client.send(UiToDaemonMsg::UpdateThemeDark(false));
            let received = ui_msg_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(matches!(received, UiToDaemonMsg::UpdateThemeDark(false)));

            assert!(server.send(DaemonToUiMsg::FocusWindow));
            let reply = d2u_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(matches!(reply, DaemonToUiMsg::FocusWindow));
        }

        assert_eq!(
            evt_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            IpcServerEvent::ClientDisconnected
        );

        // Session 2: reopen UI client and confirm clean reconnect.
        {
            let (d2u_tx, d2u_rx) = mpsc::channel();
            let client = IpcClient::connect(
                test_id,
                move |msg| {
                    let _ = d2u_tx.send(msg);
                },
                || {},
            )
            .expect("reconnect second IPC client");

            assert_eq!(
                evt_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
                IpcServerEvent::ClientConnected
            );

            assert!(server.send(DaemonToUiMsg::ToggleInputLab));
            let reply = d2u_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(matches!(reply, DaemonToUiMsg::ToggleInputLab));
            drop(client);
        }

        assert_eq!(
            evt_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            IpcServerEvent::ClientDisconnected
        );
        server.stop();
    }
}
