// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::time::{Duration, Instant};

use crate::profiles::matcher::normalize_exe_name;

/// Debounces foreground window changes so alt-tab spam never triggers redundant profile/HID writes.
///
/// A new foreground executable must hold focus continuously for `delay` (typically 1–2 s) before
/// `poll_ready()` commits and emits the transition.
#[derive(Debug, Clone)]
pub struct FocusDebouncer {
    delay: Duration,
    committed: Option<String>,
    has_committed_once: bool,
    pending: Option<(Option<String>, Instant)>,
}

impl FocusDebouncer {
    pub fn new(delay: Duration) -> Self {
        Self {
            delay,
            committed: None,
            has_committed_once: false,
            pending: None,
        }
    }

    pub fn delay(&self) -> Duration {
        self.delay
    }

    pub fn set_delay(&mut self, delay: Duration) {
        self.delay = delay;
    }

    pub fn committed_exe(&self) -> Option<&str> {
        self.committed.as_deref()
    }

    pub fn pending_exe(&self) -> Option<Option<&str>> {
        self.pending.as_ref().map(|(exe, _)| exe.as_deref())
    }

    /// Records a new foreground window focus observation at timestamp `now`.
    pub fn observe(&mut self, exe: Option<&str>, now: Instant) {
        let normalized = exe.map(normalize_exe_name).filter(|name| !name.is_empty());

        // If focus returned to the already-committed exe before the debounce elapsed, cancel
        // the pending switch completely (zero writes on quick alt-tabs!).
        if self.has_committed_once && normalized == self.committed {
            self.pending = None;
            return;
        }

        // Ignore duplicate observations of the exact same pending target so we don't keep
        // pushing back the commit timer when a single game opens a child dialog.
        if let Some((pending_exe, _)) = &self.pending {
            if *pending_exe == normalized {
                return;
            }
        }

        self.pending = Some((normalized, now));
    }

    /// Returns `Some(new_exe)` once a pending focus target has held focus for at least `delay`.
    pub fn poll_ready(&mut self, now: Instant) -> Option<Option<String>> {
        let (target, since) = self.pending.as_ref()?;
        if now.saturating_duration_since(*since) < self.delay {
            return None;
        }

        let committed_target = target.clone();
        self.pending = None;
        self.committed = committed_target.clone();
        self.has_committed_once = true;
        Some(committed_target)
    }

    /// Remaining duration until the current pending focus change commits, if any.
    pub fn time_until_ready(&self, now: Instant) -> Option<Duration> {
        let (_, since) = self.pending.as_ref()?;
        let elapsed = now.saturating_duration_since(*since);
        Some(self.delay.saturating_sub(elapsed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alt_tab_spam_triggers_zero_switches() {
        let mut debouncer = FocusDebouncer::new(Duration::from_millis(1500));
        let t0 = Instant::now();

        // Commit initial game focus.
        debouncer.observe(Some("cs2.exe"), t0);
        assert_eq!(
            debouncer.poll_ready(t0 + Duration::from_millis(1500)),
            Some(Some("cs2.exe".to_string()))
        );

        // Rapid alt-tab spam: cs2 -> discord -> chrome -> cs2 within 1.2s.
        let t1 = t0 + Duration::from_secs(5);
        debouncer.observe(Some("discord.exe"), t1);
        assert_eq!(debouncer.poll_ready(t1 + Duration::from_millis(400)), None);

        debouncer.observe(Some("chrome.exe"), t1 + Duration::from_millis(500));
        assert_eq!(debouncer.poll_ready(t1 + Duration::from_millis(900)), None);

        // Returning to cs2 cancels the pending switch completely.
        debouncer.observe(Some("CS2.EXE"), t1 + Duration::from_millis(1100));
        assert_eq!(debouncer.poll_ready(t1 + Duration::from_secs(5)), None);
        assert_eq!(debouncer.committed_exe(), Some("cs2.exe"));
    }

    #[test]
    fn commits_once_after_sustained_focus() {
        let mut debouncer = FocusDebouncer::new(Duration::from_millis(1500));
        let t0 = Instant::now();

        debouncer.observe(Some("valorant-win64-shipping.exe"), t0);
        assert_eq!(debouncer.poll_ready(t0 + Duration::from_millis(1499)), None);
        assert_eq!(
            debouncer.poll_ready(t0 + Duration::from_millis(1500)),
            Some(Some("valorant-win64-shipping.exe".to_string()))
        );
        // Must not fire twice.
        assert_eq!(debouncer.poll_ready(t0 + Duration::from_secs(10)), None);
    }
}
