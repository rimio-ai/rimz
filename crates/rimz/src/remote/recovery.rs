//! Pure recovery-panel timing and checkpoint state.

use std::time::Duration;

use super::{env_ms, reachability::DialPlan};

pub const RECOVERY_GRACE: Duration = Duration::from_millis(500);
pub const RECOVERY_MIN_DISPLAY: Duration = Duration::from_millis(1_500);
pub const INTERNET_PROBE_TIMEOUT: Duration = Duration::from_secs(1);

const INTERNET_PROBE: &str = "1.1.1.1:443";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageStatus {
    Waiting,
    Checking,
    Ok,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryStage {
    Internet,
    Server,
    Session,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageFrame {
    pub stage: RecoveryStage,
    pub status: StageStatus,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryFrame {
    pub host: String,
    pub rows: Vec<StageFrame>,
}

/// Checkpoint state for one inter-attempt wait.
pub struct RecoveryPanel {
    host: String,
    grace: Duration,
    min_display: Duration,
    first_wait: bool,
    shown_at: Option<Duration>,
    internet: Option<(String, StageStatus)>,
    server: Option<(String, StageStatus)>,
    session: StageStatus,
}

impl RecoveryPanel {
    pub fn new(
        host: impl Into<String>,
        internet: Option<&DialPlan>,
        server: Option<&DialPlan>,
        first_wait: bool,
    ) -> Self {
        Self::with_timing(
            host,
            internet,
            server,
            first_wait,
            recovery_grace_from_env(),
            recovery_min_display_from_env(),
        )
    }

    fn with_timing(
        host: impl Into<String>,
        internet: Option<&DialPlan>,
        server: Option<&DialPlan>,
        first_wait: bool,
        grace: Duration,
        min_display: Duration,
    ) -> Self {
        Self {
            host: host.into(),
            grace,
            min_display,
            first_wait,
            shown_at: None,
            internet: internet.map(|plan| (endpoint_label(plan), StageStatus::Checking)),
            server: server.map(|plan| (endpoint_label(plan), StageStatus::Checking)),
            session: StageStatus::Waiting,
        }
    }

    pub fn checking_internet(&mut self) {
        if let Some((_, status)) = &mut self.internet {
            *status = StageStatus::Checking;
        }
    }

    pub fn note_internet(&mut self, reachable: bool) {
        if let Some((_, status)) = &mut self.internet {
            *status = checkpoint_status(reachable);
        }
    }

    pub fn checking_server(&mut self) {
        if let Some((_, status)) = &mut self.server {
            *status = StageStatus::Checking;
        }
    }

    pub fn note_server(&mut self, reachable: bool) {
        if let Some((_, status)) = &mut self.server {
            *status = checkpoint_status(reachable);
        }
    }

    pub fn session_starting(&mut self) {
        self.session = StageStatus::Checking;
    }

    /// Whether the recovery canvas owns the terminal at this elapsed age.
    pub fn visible(&mut self, elapsed: Duration) -> bool {
        let visible = !self.first_wait || elapsed >= self.grace;
        if visible && self.shown_at.is_none() {
            self.shown_at = Some(elapsed);
        }
        visible
    }

    /// Earliest release time after the checkpoints say the next attach may start.
    pub fn release_at(&self, ready_elapsed: Duration) -> Duration {
        self.shown_at.map_or(ready_elapsed, |shown_at| {
            ready_elapsed.max(shown_at.saturating_add(self.min_display))
        })
    }

    pub fn frame(&self) -> RecoveryFrame {
        let mut rows = Vec::with_capacity(3);
        if let Some((endpoint, status)) = &self.internet {
            rows.push(StageFrame {
                stage: RecoveryStage::Internet,
                status: *status,
                label: format!("Internet  {endpoint}"),
            });
        }
        if let Some((endpoint, status)) = &self.server {
            rows.push(StageFrame {
                stage: RecoveryStage::Server,
                status: *status,
                label: format!("Server    {endpoint}"),
            });
        }
        rows.push(StageFrame {
            stage: RecoveryStage::Session,
            status: self.session,
            label: match self.session {
                StageStatus::Checking => "SSH session  starting…".to_owned(),
                _ => "SSH session  waiting".to_owned(),
            },
        });
        RecoveryFrame {
            host: self.host.clone(),
            rows,
        }
    }
}

pub fn recovery_grace_from_env() -> Duration {
    env_ms("RIMZ_REMOTE_GRACE_MS").unwrap_or(RECOVERY_GRACE)
}

pub fn recovery_min_display_from_env() -> Duration {
    env_ms("RIMZ_REMOTE_MIN_DISPLAY_MS").unwrap_or(RECOVERY_MIN_DISPLAY)
}

/// Internet checkpoint endpoint. Empty, `0`, or an invalid `host:port`
/// disables the checkpoint.
pub fn internet_probe_from_env() -> Option<DialPlan> {
    match std::env::var("RIMZ_REMOTE_INTERNET_PROBE") {
        Ok(value) => parse_endpoint(&value),
        Err(_) => parse_endpoint(INTERNET_PROBE),
    }
}

fn parse_endpoint(value: &str) -> Option<DialPlan> {
    if value.is_empty() || value == "0" {
        return None;
    }
    let (host, port) = value.rsplit_once(':')?;
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    if host.is_empty() {
        return None;
    }
    Some(DialPlan {
        host: host.to_owned(),
        port: port.parse().ok()?,
    })
}

fn endpoint_label(plan: &DialPlan) -> String {
    if plan.host.contains(':') {
        format!("[{}]:{}", plan.host, plan.port)
    } else {
        format!("{}:{}", plan.host, plan.port)
    }
}

fn checkpoint_status(reachable: bool) -> StageStatus {
    if reachable {
        StageStatus::Ok
    } else {
        StageStatus::Down
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(host: &str) -> DialPlan {
        DialPlan {
            host: host.to_owned(),
            port: 22,
        }
    }

    fn panel(first_wait: bool, server: Option<&DialPlan>) -> RecoveryPanel {
        RecoveryPanel::with_timing(
            "dev-box",
            Some(&plan("1.1.1.1")),
            server,
            first_wait,
            Duration::from_millis(500),
            Duration::from_millis(1_500),
        )
    }

    #[test]
    fn sub_grace_recovery_never_shows_or_holds() {
        let mut panel = panel(true, None);

        assert!(!panel.visible(Duration::from_millis(499)));
        assert_eq!(
            panel.release_at(Duration::from_millis(499)),
            Duration::from_millis(499)
        );
    }

    #[test]
    fn shown_panel_holds_for_the_minimum_display_time() {
        let mut panel = panel(true, None);

        assert!(panel.visible(Duration::from_millis(500)));
        assert_eq!(
            panel.release_at(Duration::from_millis(700)),
            Duration::from_millis(2_000)
        );
        assert_eq!(
            panel.release_at(Duration::from_millis(2_500)),
            Duration::from_millis(2_500)
        );
    }

    #[test]
    fn later_wait_has_no_grace() {
        let mut panel = panel(false, None);

        assert!(panel.visible(Duration::ZERO));
        assert_eq!(
            panel.release_at(Duration::from_millis(100)),
            Duration::from_millis(1_500)
        );
    }

    #[test]
    fn checkpoint_status_can_regress() {
        let server = plan("dev-box");
        let mut panel = panel(true, Some(&server));

        panel.note_internet(true);
        panel.note_server(true);
        panel.note_internet(false);
        panel.note_server(false);

        let frame = panel.frame();
        assert_eq!(frame.rows[0].status, StageStatus::Down);
        assert_eq!(frame.rows[1].status, StageStatus::Down);
    }

    #[test]
    fn proxy_target_omits_server_row() {
        let panel = panel(true, None);

        assert_eq!(
            panel
                .frame()
                .rows
                .iter()
                .map(|row| row.stage)
                .collect::<Vec<_>>(),
            vec![RecoveryStage::Internet, RecoveryStage::Session]
        );
    }

    #[test]
    fn endpoint_parser_accepts_hostnames_and_bracketed_ipv6() {
        assert_eq!(
            parse_endpoint("dev:2202"),
            Some(DialPlan {
                host: "dev".to_owned(),
                port: 2202
            })
        );
        assert_eq!(
            parse_endpoint("[::1]:443"),
            Some(DialPlan {
                host: "::1".to_owned(),
                port: 443
            })
        );
        assert_eq!(parse_endpoint("0"), None);
        assert_eq!(parse_endpoint("bad"), None);
    }
}
