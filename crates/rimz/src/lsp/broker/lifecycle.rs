//! Lease graces and progress-driven query readiness.

use crate::lsp::registry::{Lease, State, StopReason};
use serde_json::Value;

pub(super) fn idle_expired(
    now: u64,
    ready_at: Option<u64>,
    last_request: Option<u64>,
    in_flight: usize,
    timeout: u64,
) -> bool {
    in_flight == 0
        && ready_at.is_some_and(|ready| {
            now.saturating_sub(ready.max(last_request.unwrap_or(ready))) >= timeout
        })
}

#[derive(Default)]
pub(super) struct Lifecycle {
    pub(super) leases: Vec<Lease>,
    last_release: Option<u64>,
}

impl Lifecycle {
    pub(super) fn register(&mut self, lease: Lease) {
        self.leases
            .retain(|old| old.launch_id != lease.launch_id || old.pid != lease.pid);
        self.leases.push(lease);
        self.last_release = None;
    }
    pub(super) fn retain(&mut self, now: u64, keep: impl FnMut(&Lease) -> bool) {
        let had_leases = !self.leases.is_empty();
        self.leases.retain(keep);
        if had_leases && self.leases.is_empty() {
            self.last_release = Some(now);
        }
    }
    pub(super) fn expired(&self, now: u64) -> Option<StopReason> {
        if !self.leases.is_empty() {
            return None;
        }
        match self.last_release {
            Some(released) if now.saturating_sub(released) >= 60_000 => Some(StopReason::Released),
            None if now >= 300_000 => Some(StopReason::NeverLeased),
            _ => None,
        }
    }
}

#[derive(Default)]
pub(super) struct Readiness {
    initialized_at: Option<u64>,
    open: Vec<Value>,
    ended: bool,
    last_progress_at: Option<u64>,
}

impl Readiness {
    pub(super) fn initialized(&mut self, now: u64) {
        self.initialized_at = Some(now);
    }
    pub(super) fn progress(&mut self, params: &Value, now: u64) {
        self.last_progress_at = Some(now);
        let token = &params["token"];
        match params["value"]["kind"].as_str() {
            Some("begin") if !self.open.contains(token) => self.open.push(token.clone()),
            Some("end") => {
                self.open.retain(|open| open != token);
                self.ended = true;
            }
            _ => {}
        }
    }
    pub(super) fn state(&self, now: u64) -> State {
        let Some(initialized) = self.initialized_at else {
            return State::Starting;
        };
        let settled = self.last_progress_at.map_or_else(
            || now.saturating_sub(initialized) >= 10_000,
            |last| self.ended && now.saturating_sub(last) >= 2_000,
        );
        if self.open.is_empty() && settled {
            State::Ready
        } else {
            State::Indexing
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn idle_expiry_waits_for_readiness_and_in_flight_queries() {
        assert!(!idle_expired(20_000, None, None, 0, 2_000));
        assert!(!idle_expired(20_000, Some(10_000), None, 1, 2_000));
        assert!(!idle_expired(11_999, Some(10_000), Some(5_000), 0, 2_000));
        assert!(idle_expired(12_000, Some(10_000), Some(5_000), 0, 2_000));
        assert!(!idle_expired(16_999, Some(10_000), Some(15_000), 0, 2_000));
        assert!(idle_expired(17_000, Some(10_000), Some(15_000), 0, 2_000));
    }

    fn lease(pid: u32) -> Lease {
        Lease {
            launch_id: Some("launch".to_owned().into()),
            pid,
            start_token: "token".into(),
            since_ms: 0,
        }
    }

    #[test]
    fn anonymous_lease_keeps_broker_alive_until_release() {
        let lease = serde_json::from_value::<Lease>(
            json!({"pid": 1, "start_token": "token", "since_ms": 0}),
        );
        assert!(lease.is_ok(), "a launch id is an optional label");
        let mut state = Lifecycle::default();
        state.register(lease.unwrap());
        assert_eq!(state.expired(600_000), None);
        state.retain(600_000, |_| false);
        assert_eq!(state.expired(660_000), Some(StopReason::Released));
    }

    #[test]
    fn leases_reap_release_and_cancel_restart_grace() {
        let mut state = Lifecycle::default();
        assert_eq!(state.expired(299_999), None);
        assert_eq!(state.expired(300_000), Some(StopReason::NeverLeased));
        state.register(lease(1));
        state.register(lease(1));
        state.register(lease(2));
        assert_eq!(state.leases.len(), 2);
        state.retain(400_000, |lease| lease.pid != 1);
        assert_eq!(state.leases.len(), 1);
        assert_eq!(state.expired(500_000), None);
        state.retain(500_000, |_| false);
        assert_eq!(state.expired(559_999), None);
        state.register(lease(3));
        assert_eq!(state.expired(560_000), None);
        state.retain(600_000, |_| false);
        state.retain(610_000, |_| false);
        assert_eq!(state.expired(660_000), Some(StopReason::Released));
    }

    #[test]
    fn readiness_requires_initialized_and_all_progress_ended() {
        let mut state = Readiness::default();
        assert_eq!(state.state(50_000), State::Starting);
        state.initialized(100);
        assert_eq!(state.state(10_099), State::Indexing);
        assert_eq!(state.state(10_100), State::Ready);
        state.progress(&json!({"token": "a", "value": {"kind": "begin"}}), 200);
        state.progress(&json!({"token": "a", "value": {"kind": "end"}}), 300);
        assert_eq!(state.state(301), State::Indexing);
        state.progress(&json!({"token": 2, "value": {"kind": "begin"}}), 400);
        assert_eq!(state.state(30_000), State::Indexing);
        state.progress(&json!({"token": 2, "value": {"kind": "end"}}), 30_000);
        assert_eq!(state.state(31_999), State::Indexing);
        assert_eq!(state.state(32_000), State::Ready);
    }
}
