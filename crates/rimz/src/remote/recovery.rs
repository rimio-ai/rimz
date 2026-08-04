//! Pure recovery-panel timing and checkpoint state.

use std::time::Duration;

use super::{
    env_ms,
    reachability::{DialPlan, FooterPhase},
};

const RECOVERY_GRACE: Duration = Duration::from_millis(500);
const RECOVERY_MIN_DISPLAY: Duration = Duration::from_millis(1_500);
pub const INTERNET_PROBE_TIMEOUT: Duration = Duration::from_secs(1);

const INTERNET_PROBE: &str = "http://cp.cloudflare.com/generate_204";

/// End-to-end HTTP checkpoint used by the recovery panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InternetProbe {
    url: String,
    host: String,
}

impl InternetProbe {
    pub fn url(&self) -> &str {
        &self.url
    }

    fn host(&self) -> &str {
        &self.host
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageStatus {
    Waiting,
    Checking,
    Ok,
    Down,
    Suspect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryStage {
    Internet,
    Server,
    Session,
    Multiplexer,
}

/// Which connection transition the panel presents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectStage {
    Initial,
    Recovery,
}

/// The action that takes over after the SSH master is ready.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandoffStage {
    Multiplexer,
    WebTunnel,
}

impl HandoffStage {
    fn label(self) -> &'static str {
        match self {
            Self::Multiplexer => "Multiplexer",
            Self::WebTunnel => "Web tunnel",
        }
    }

    fn opening_detail(self) -> &'static str {
        match self {
            Self::Multiplexer => "attaching…",
            Self::WebTunnel => "opening…",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageFrame {
    pub stage: RecoveryStage,
    pub status: StageStatus,
    pub label: String,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryFrame {
    pub connect_stage: ConnectStage,
    pub host: String,
    pub outage_for: Duration,
    pub attempt: u32,
    pub phase: FooterPhase,
    pub last_error: Option<String>,
    pub attaching: bool,
    pub rows: Vec<StageFrame>,
}

#[derive(Clone)]
struct Checkpoint {
    endpoint_label: String,
    result: Option<StageStatus>,
    tun: Option<String>,
}

/// Checkpoint state for one transport outage.
pub struct RecoveryPanel {
    connect_stage: ConnectStage,
    handoff_stage: HandoffStage,
    host: String,
    grace: Duration,
    min_display: Duration,
    wait_started: bool,
    grace_this_wait: bool,
    shown_at: Option<Duration>,
    internet: Option<Checkpoint>,
    server: Option<Checkpoint>,
    session: StageStatus,
    master_ready: bool,
    attempt: u32,
    last_error: Option<String>,
}

impl RecoveryPanel {
    pub fn new(
        connect_stage: ConnectStage,
        handoff_stage: HandoffStage,
        host: impl Into<String>,
        internet: Option<&InternetProbe>,
        server: Option<&DialPlan>,
    ) -> Self {
        Self::with_timing(
            connect_stage,
            handoff_stage,
            host,
            internet,
            server,
            env_ms("RIMZ_REMOTE_GRACE_MS").unwrap_or(RECOVERY_GRACE),
            env_ms("RIMZ_REMOTE_MIN_DISPLAY_MS").unwrap_or(RECOVERY_MIN_DISPLAY),
        )
    }

    fn with_timing(
        connect_stage: ConnectStage,
        handoff_stage: HandoffStage,
        host: impl Into<String>,
        internet: Option<&InternetProbe>,
        server: Option<&DialPlan>,
        grace: Duration,
        min_display: Duration,
    ) -> Self {
        Self {
            connect_stage,
            handoff_stage,
            host: host.into(),
            grace,
            min_display,
            wait_started: false,
            grace_this_wait: true,
            shown_at: None,
            internet: internet.map(|probe| Checkpoint {
                endpoint_label: probe.host().to_owned(),
                result: None,
                tun: None,
            }),
            server: server.map(checkpoint),
            session: StageStatus::Waiting,
            master_ready: false,
            attempt: 0,
            last_error: None,
        }
    }

    /// Reset per-wait presentation while preserving outage checkpoint results.
    pub fn begin_wait(&mut self) {
        self.grace_this_wait = !self.wait_started;
        self.wait_started = true;
        self.shown_at = None;
        self.session = StageStatus::Waiting;
        self.master_ready = false;
    }

    pub fn note_attempt(&mut self, consecutive_failures: u32) {
        self.attempt = consecutive_failures;
    }

    pub fn note_internet(&mut self, reachable: bool) {
        if let Some(checkpoint) = &mut self.internet {
            checkpoint.result = Some(checkpoint_status(reachable));
        }
    }

    pub fn note_server(&mut self, reachable: bool) {
        if let Some(checkpoint) = &mut self.server {
            checkpoint.result = Some(checkpoint_status(reachable));
            checkpoint.tun = None;
        }
    }

    pub fn note_server_tun(&mut self, ifname: &str) {
        if let Some(checkpoint) = &mut self.server {
            checkpoint.result = Some(StageStatus::Ok);
            checkpoint.tun = Some(ifname.to_owned());
        }
    }

    pub fn session_starting(&mut self) {
        self.session = StageStatus::Checking;
    }

    pub fn note_master_ready(&mut self) {
        self.master_ready = true;
        self.session = StageStatus::Ok;
    }

    pub fn note_ssh_error(&mut self, error: Option<String>) {
        self.master_ready = false;
        self.session = if error.is_some() {
            StageStatus::Down
        } else {
            StageStatus::Waiting
        };
        self.last_error = error;
    }

    /// Whether the recovery canvas owns the terminal at this elapsed age.
    pub fn visible(&self, elapsed: Duration) -> bool {
        !self.grace_this_wait || elapsed >= self.grace
    }

    /// Record the instant at which the renderer successfully opened the canvas.
    pub fn note_shown(&mut self, elapsed: Duration) {
        self.shown_at.get_or_insert(elapsed);
    }

    /// Earliest release time after the checkpoints say the next attach may start.
    pub fn release_at(&self, ready_elapsed: Duration) -> Duration {
        self.shown_at.map_or(ready_elapsed, |shown_at| {
            ready_elapsed.max(shown_at.saturating_add(self.min_display))
        })
    }

    pub fn frame(&self, outage_for: Duration, phase: FooterPhase) -> RecoveryFrame {
        let mut rows = Vec::with_capacity(4);
        if let Some(checkpoint) = &self.internet {
            rows.push(StageFrame {
                stage: RecoveryStage::Internet,
                status: checkpoint.result.unwrap_or(StageStatus::Checking),
                label: "Internet".to_owned(),
                detail: checkpoint.endpoint_label.clone(),
            });
        }
        if let Some(checkpoint) = &self.server {
            let mut status = checkpoint.result.unwrap_or(StageStatus::Checking);
            let mut detail = checkpoint.tun.as_ref().map_or_else(
                || checkpoint.endpoint_label.clone(),
                |tun| {
                    format!(
                        "{} · via TUN {tun} · TCP check skipped",
                        checkpoint.endpoint_label
                    )
                },
            );
            if status == StageStatus::Ok && self.attempt >= 2 {
                status = StageStatus::Suspect;
                if let Some(tun) = &checkpoint.tun {
                    detail = format!(
                        "{} · via TUN {tun} · SSH failing",
                        checkpoint.endpoint_label
                    );
                } else {
                    detail.push_str(" · answers TCP · SSH failing");
                }
            }
            rows.push(StageFrame {
                stage: RecoveryStage::Server,
                status,
                label: "Server".to_owned(),
                detail,
            });
        }
        rows.push(StageFrame {
            stage: RecoveryStage::Session,
            status: self.session,
            label: "SSH session".to_owned(),
            detail: match self.session {
                StageStatus::Checking => "starting…".to_owned(),
                StageStatus::Down => self
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "failed".to_owned()),
                StageStatus::Ok => "connected".to_owned(),
                _ => "waiting".to_owned(),
            },
        });
        rows.push(StageFrame {
            stage: RecoveryStage::Multiplexer,
            status: if self.master_ready {
                StageStatus::Checking
            } else {
                StageStatus::Waiting
            },
            label: self.handoff_stage.label().to_owned(),
            detail: if self.master_ready {
                self.handoff_stage.opening_detail().to_owned()
            } else {
                "waiting".to_owned()
            },
        });
        RecoveryFrame {
            connect_stage: self.connect_stage,
            host: self.host.clone(),
            outage_for,
            attempt: self.attempt,
            phase,
            last_error: self.last_error.clone(),
            attaching: self.master_ready,
            rows,
        }
    }
}

/// Internet checkpoint URL. Empty, `0`, or an invalid HTTP URL disables the
/// checkpoint.
pub fn internet_probe_from_env() -> Option<InternetProbe> {
    match std::env::var("RIMZ_REMOTE_INTERNET_PROBE") {
        Ok(value) => parse_endpoint(&value),
        Err(_) => parse_endpoint(INTERNET_PROBE),
    }
}

fn parse_endpoint(value: &str) -> Option<InternetProbe> {
    if value.is_empty() || value == "0" {
        return None;
    }
    let uri = value.parse::<ureq::http::Uri>().ok()?;
    if !matches!(uri.scheme_str(), Some("http" | "https")) {
        return None;
    }
    Some(InternetProbe {
        url: value.to_owned(),
        host: uri.host()?.to_owned(),
    })
}

fn endpoint_label(plan: &DialPlan) -> String {
    if plan.host.contains(':') {
        format!("[{}]:{}", plan.host, plan.port)
    } else {
        format!("{}:{}", plan.host, plan.port)
    }
}

fn checkpoint(plan: &DialPlan) -> Checkpoint {
    Checkpoint {
        endpoint_label: endpoint_label(plan),
        result: None,
        tun: None,
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

    fn internet() -> InternetProbe {
        parse_endpoint(INTERNET_PROBE).expect("default internet probe")
    }

    fn panel(server: Option<&DialPlan>) -> RecoveryPanel {
        RecoveryPanel::with_timing(
            ConnectStage::Recovery,
            HandoffStage::Multiplexer,
            "dev-box",
            Some(&internet()),
            server,
            Duration::from_millis(500),
            Duration::from_millis(1_500),
        )
    }

    fn row(frame: &RecoveryFrame, stage: RecoveryStage) -> &StageFrame {
        frame
            .rows
            .iter()
            .find(|row| row.stage == stage)
            .expect("stage row")
    }

    #[test]
    fn initial_frame_carries_its_connection_stage() {
        let panel = RecoveryPanel::with_timing(
            ConnectStage::Initial,
            HandoffStage::Multiplexer,
            "dev-box",
            None,
            None,
            Duration::ZERO,
            Duration::ZERO,
        );

        assert_eq!(
            panel
                .frame(Duration::ZERO, FooterPhase::Connecting)
                .connect_stage,
            ConnectStage::Initial
        );
    }

    #[test]
    fn sub_grace_recovery_never_shows_or_holds() {
        let mut panel = panel(None);
        panel.begin_wait();

        assert!(!panel.visible(Duration::from_millis(499)));
        assert_eq!(
            panel.release_at(Duration::from_millis(499)),
            Duration::from_millis(499)
        );
    }

    #[test]
    fn shown_panel_holds_for_the_minimum_display_time() {
        let mut panel = panel(None);
        panel.begin_wait();

        assert!(panel.visible(Duration::from_millis(500)));
        panel.note_shown(Duration::from_millis(500));
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
        let mut panel = panel(None);
        panel.begin_wait();
        assert!(!panel.visible(Duration::ZERO));

        panel.begin_wait();

        assert!(panel.visible(Duration::ZERO));
        assert_eq!(
            panel.release_at(Duration::from_millis(100)),
            Duration::from_millis(100),
            "eligibility alone does not count as a displayed panel"
        );
        panel.note_shown(Duration::ZERO);
        assert_eq!(
            panel.release_at(Duration::from_millis(100)),
            Duration::from_millis(1_500)
        );
    }

    #[test]
    fn checkpoint_status_can_regress() {
        let server = plan("dev-box");
        let mut panel = panel(Some(&server));

        panel.note_internet(true);
        panel.note_server(true);
        panel.note_internet(false);
        panel.note_server(false);

        let frame = panel.frame(Duration::ZERO, FooterPhase::Connecting);
        assert_eq!(frame.rows[0].status, StageStatus::Down);
        assert_eq!(frame.rows[1].status, StageStatus::Down);
    }

    #[test]
    fn proxy_target_omits_server_row() {
        let panel = panel(None);

        assert_eq!(
            panel
                .frame(Duration::ZERO, FooterPhase::Connecting)
                .rows
                .iter()
                .map(|row| row.stage)
                .collect::<Vec<_>>(),
            vec![
                RecoveryStage::Internet,
                RecoveryStage::Session,
                RecoveryStage::Multiplexer,
            ]
        );
    }

    #[test]
    fn settled_checkpoints_survive_later_waits_without_spinner_flicker() {
        let server = plan("dev-box");
        let mut panel = panel(Some(&server));
        assert_eq!(
            panel.frame(Duration::ZERO, FooterPhase::Connecting).rows[0].status,
            StageStatus::Checking
        );

        panel.note_internet(true);
        panel.note_server(true);
        panel.begin_wait();
        panel.begin_wait();

        let frame = panel.frame(Duration::from_secs(2), FooterPhase::Connecting);
        assert_eq!(frame.rows[0].status, StageStatus::Ok);
        assert_eq!(frame.rows[1].status, StageStatus::Ok);
    }

    #[test]
    fn reachable_server_becomes_suspect_after_repeated_ssh_failures() {
        let server = plan("dev-box");
        let mut panel = panel(Some(&server));
        panel.note_server(true);

        panel.note_attempt(1);
        assert_eq!(
            panel.frame(Duration::ZERO, FooterPhase::Connecting).rows[1].status,
            StageStatus::Ok
        );
        panel.note_attempt(2);
        let suspect = panel.frame(Duration::ZERO, FooterPhase::Connecting);
        assert_eq!(suspect.rows[1].status, StageStatus::Suspect);
        assert!(
            suspect.rows[1]
                .detail
                .ends_with("answers TCP · SSH failing")
        );

        panel.note_attempt(0);
        assert_eq!(
            panel.frame(Duration::ZERO, FooterPhase::Connecting).rows[1].status,
            StageStatus::Ok
        );
    }

    #[test]
    fn tun_server_skips_tcp_wording_and_becomes_suspect() {
        let server = plan("dev-box");
        let mut panel = panel(Some(&server));
        panel.note_server_tun("utun3");

        panel.note_attempt(1);
        let reachable = panel.frame(Duration::ZERO, FooterPhase::Connecting);
        assert_eq!(reachable.rows[1].status, StageStatus::Ok);
        assert_eq!(
            reachable.rows[1].detail,
            "dev-box:22 · via TUN utun3 · TCP check skipped"
        );

        panel.note_attempt(2);
        let suspect = panel.frame(Duration::ZERO, FooterPhase::Connecting);
        assert_eq!(suspect.rows[1].status, StageStatus::Suspect);
        assert_eq!(
            suspect.rows[1].detail,
            "dev-box:22 · via TUN utun3 · SSH failing"
        );
    }

    #[test]
    fn frame_carries_footer_phase_and_persistent_ssh_error() {
        let mut panel = panel(None);
        panel.note_attempt(7);
        panel.note_ssh_error(Some("Permission denied (publickey).".to_owned()));

        let frame = panel.frame(
            Duration::from_secs(133),
            FooterPhase::NextAttemptIn(Duration::from_secs(12)),
        );
        assert_eq!(frame.outage_for, Duration::from_secs(133));
        assert_eq!(frame.attempt, 7);
        assert_eq!(
            frame.phase,
            FooterPhase::NextAttemptIn(Duration::from_secs(12))
        );
        assert_eq!(
            frame.last_error.as_deref(),
            Some("Permission denied (publickey).")
        );
        assert_eq!(
            row(&frame, RecoveryStage::Session).status,
            StageStatus::Down
        );

        panel.session_starting();
        let connecting = panel.frame(Duration::from_secs(133), FooterPhase::Connecting);
        assert_eq!(
            row(&connecting, RecoveryStage::Session).status,
            StageStatus::Checking
        );
        assert_eq!(
            connecting.last_error.as_deref(),
            Some("Permission denied (publickey).")
        );
    }

    #[test]
    fn multiplexer_stage_is_always_present_and_last() {
        for server in [None, Some(plan("dev-box"))] {
            let panel = panel(server.as_ref());
            let frame = panel.frame(Duration::ZERO, FooterPhase::Connecting);
            let multiplexer = frame.rows.last().expect("multiplexer row");

            assert_eq!(multiplexer.stage, RecoveryStage::Multiplexer);
            assert_eq!(multiplexer.status, StageStatus::Waiting);
            assert_eq!(multiplexer.detail, "waiting");
        }
    }

    #[test]
    fn confirmed_master_moves_the_panel_to_attaching() {
        let mut panel = panel(None);

        panel.note_master_ready();

        let frame = panel.frame(Duration::ZERO, FooterPhase::Connecting);
        assert!(frame.attaching);
        assert_eq!(row(&frame, RecoveryStage::Session).status, StageStatus::Ok);
        assert_eq!(row(&frame, RecoveryStage::Session).detail, "connected");
        assert_eq!(
            row(&frame, RecoveryStage::Multiplexer).status,
            StageStatus::Checking
        );
        assert_eq!(row(&frame, RecoveryStage::Multiplexer).detail, "attaching…");
    }

    #[test]
    fn web_handoff_names_the_tunnel_opening_stage() {
        let mut panel = RecoveryPanel::with_timing(
            ConnectStage::Initial,
            HandoffStage::WebTunnel,
            "dev-box",
            None,
            None,
            Duration::ZERO,
            Duration::ZERO,
        );
        panel.note_master_ready();

        let frame = panel.frame(Duration::ZERO, FooterPhase::Connecting);
        let handoff = row(&frame, RecoveryStage::Multiplexer);

        assert_eq!(handoff.label, "Web tunnel");
        assert_eq!(handoff.detail, "opening…");
    }

    #[test]
    fn beginning_a_wait_clears_the_attaching_phase() {
        let mut panel = panel(None);
        panel.note_master_ready();

        panel.begin_wait();

        let frame = panel.frame(Duration::ZERO, FooterPhase::Connecting);
        assert!(!frame.attaching);
        assert_eq!(
            row(&frame, RecoveryStage::Multiplexer).status,
            StageStatus::Waiting
        );
    }

    #[test]
    fn master_failure_clears_the_attaching_phase() {
        let mut panel = panel(None);
        panel.note_master_ready();

        panel.note_ssh_error(Some("control socket closed".to_owned()));

        let frame = panel.frame(Duration::ZERO, FooterPhase::Connecting);
        assert!(!frame.attaching);
        assert_eq!(
            row(&frame, RecoveryStage::Session).status,
            StageStatus::Down
        );
        assert_eq!(
            row(&frame, RecoveryStage::Multiplexer).status,
            StageStatus::Waiting
        );
    }

    #[test]
    fn endpoint_parser_accepts_http_urls_and_extracts_the_host() {
        let probe = parse_endpoint("http://dev:2202/generate_204").expect("probe");
        assert_eq!(probe.url(), "http://dev:2202/generate_204");
        assert_eq!(probe.host(), "dev");
        let v6 = parse_endpoint("https://[::1]:443/check").expect("IPv6 probe");
        assert_eq!(v6.host(), "[::1]");
        assert_eq!(parse_endpoint("0"), None);
        assert_eq!(parse_endpoint("dev:443"), None);
    }
}
