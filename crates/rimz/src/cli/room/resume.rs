//! Room recovery presentation and live-session health checks.

use std::io::Write;

use rimz::mux::{MuxBackend, SessionHealth};

pub(crate) fn session_is_healthy_live(backend: &dyn MuxBackend, session_name: &str) -> bool {
    let exists = backend
        .list_sessions()
        .map(|sessions| sessions.iter().any(|name| name == session_name))
        .unwrap_or(false);
    exists
        && matches!(
            backend.probe_session_health(session_name),
            Ok(SessionHealth::Healthy)
        )
}

pub(super) fn report_previous_session_death(death: &rimz::store::event::LastDeathMarker) {
    let _ = writeln!(std::io::stderr().lock(), "{}", death_notice(death));
}

fn death_notice(death: &rimz::store::event::LastDeathMarker) -> String {
    let at = death.at.strftime("%Y-%m-%d %H:%M");
    match death.cause {
        rimz::store::event::SessionDeathCause::Reboot => {
            format!("rimz: machine rebooted since this room was last open ({at})")
        }
        rimz::store::event::SessionDeathCause::Crash => {
            format!("rimz: this room's previous session ended with agents still running ({at})")
        }
    }
}

/// Report recovered and skipped prior agents to stderr so attach stdout stays clean.
pub(super) fn report_resume(plan: &rimz::harness::resume::ResumePlan) {
    if !plan.tabs.is_empty() {
        let agents = plan
            .tabs
            .iter()
            .map(rimz::mux::ResumeTab::pane_count)
            .sum::<usize>();
        let labels = plan
            .tabs
            .iter()
            .map(|tab| tab.label.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        if agents == 0 {
            let _ = writeln!(std::io::stderr(), "rimz: restored channel tab(s): {labels}");
        } else {
            let _ = writeln!(
                std::io::stderr(),
                "rimz: resumed {} agent{}: {labels}",
                agents,
                if agents == 1 { "" } else { "s" },
            );
        }
    }
    if !plan.skipped.is_empty() {
        let detail = plan
            .skipped
            .iter()
            .map(|skip| format!("{} ({})", skip.label, skip.reason.label()))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(std::io::stderr(), "rimz: not resumed: {detail}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::ids::AgentKind;
    use rimz::store::event::{LastDeathMarker, SessionDeathAgent, SessionDeathCause};

    #[test]
    fn death_notice_matches_session_death_cause() {
        for (cause, expected) in [
            (
                SessionDeathCause::Crash,
                "rimz: this room's previous session ended with agents still running (1970-01-01 00:00)",
            ),
            (
                SessionDeathCause::Reboot,
                "rimz: machine rebooted since this room was last open (1970-01-01 00:00)",
            ),
        ] {
            let death = LastDeathMarker {
                cause,
                lost_agents: vec![SessionDeathAgent {
                    kind: AgentKind::new_unchecked("claude"),
                    agent_id: "sess".into(),
                    name: None,
                }],
                at: jiff::Timestamp::UNIX_EPOCH,
                recovered: None,
            };
            assert_eq!(death_notice(&death), expected);
        }
    }
}
