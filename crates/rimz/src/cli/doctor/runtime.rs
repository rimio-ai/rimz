use rimz::bridge::AF_UNIX_PATH_LIMIT;
use rimz::config::MachineConfig;
use rimz::mux::{
    MuxBackend, SessionHealth,
    tmux::{self as tmux_mod, MIN_TMUX_VERSION},
    zellij::{self as zellij_mod, MIN_ZELLIJ_VERSION},
};
use rimz::{RuntimePaths, StatePaths};

use super::LONGEST_SOCKET_TAIL_LEN;

#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
pub(super) fn report_session_health(backend: &dyn MuxBackend, session_name: &str) {
    match backend.probe_session_health(session_name) {
        // `probe_session_health` never returns `Reborn` (it does not mutate), so
        // the live verdict is just clean-or-stuck.
        Ok(SessionHealth::Healthy | SessionHealth::Reborn) => println!("  session health: ok"),
        Ok(SessionHealth::Stuck) => println!(
            "  session health: stuck (resurrected/suspended panes) — run `rimz reset` to rebuild",
        ),
        Err(err) => println!("  session health: unavailable ({err})"),
    }
}

#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
pub(super) fn report_zellij_capabilities() {
    match zellij_mod::capabilities() {
        Ok(caps) => {
            let floor_status = if caps.meets_min_version {
                "OK"
            } else {
                "TOO OLD"
            };
            let (maj, min, patch) = MIN_ZELLIJ_VERSION;
            println!("  zellij floor  : {floor_status} (>= {maj}.{min}.{patch} required)");
        }
        Err(err) => println!("  zellij floor  : unavailable ({err})"),
    }
}

/// One presence row for the workspace: the pane-discovery mode its producer
/// is actually in — the verdict comes from the same stamp helpers the
/// producer reads, so doctor and producer always agree — and, when degraded
/// to the poll, the first failing precondition with its fix.
#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
pub(super) fn report_presence_channel(ws: &rimz::ResolvedWorkspace) {
    use rimz::sidebar::cache::{presence_event_mode, presence_stamp_age_ms};

    let runtime = match RuntimePaths::for_workspace(ws.workspace_id.clone()) {
        Ok(runtime) => runtime,
        Err(err) => {
            println!("  presence      : unavailable ({err})");
            return;
        }
    };
    let age = presence_stamp_age_ms(&runtime);
    if presence_event_mode(age) {
        let secs = age.unwrap_or(0) / 1000;
        println!("  presence      : event mode (plugin poked {secs}s ago)");
        return;
    }
    // Poll mode: name the first failing precondition in fix order.
    if zellij_mod::presence_plugin_path().is_none() {
        println!(
            "  presence      : poll mode — embedded plugin unavailable or could not \
             materialize (reinstall rimz)",
        );
        return;
    }
    let meets_floor = zellij_mod::capabilities().is_ok_and(|caps| {
        caps.parsed_version
            .is_some_and(|v| v >= zellij_mod::PRESENCE_PLUGIN_MIN_ZELLIJ)
    });
    if !meets_floor {
        let (maj, min, patch) = zellij_mod::PRESENCE_PLUGIN_MIN_ZELLIJ;
        println!(
            "  presence      : poll mode — zellij below the plugin floor \
             (>= {maj}.{min}.{patch} required)",
        );
        return;
    }
    match age {
        Some(age) => println!(
            "  presence      : poll mode — last plugin poke {}s ago (plugin gone or \
             `rimz` not runnable from Zellij; reattach or run `rimz reload`)",
            age / 1000,
        ),
        None => println!(
            "  presence      : poll mode — no plugin poke yet (approve the one-time \
             permission prompt in the Zellij session)",
        ),
    }
}

/// Report the per-machine remote-control auto-launch posture. Codex's host has a
/// hard precondition — the managed standalone install — that `rimz start`
/// enforces fail-fast, so doctor surfaces the same gap and the same fix ahead of
/// time. Claude's host is best-effort (gated on PATH), so it only warns.
#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
pub(super) fn report_remote_control() {
    let config = match MachineConfig::load() {
        Ok(config) => config.remote_control,
        Err(err) => {
            println!("  remote control: config unavailable ({err})");
            return;
        }
    };
    if !config.claude && !config.codex {
        println!("  remote control: off");
        return;
    }

    let codex_standalone_missing =
        config.codex && rimz::remote_control::codex_standalone_bin().is_none();
    let mut parts = Vec::new();
    if config.claude {
        parts.push(if which::which("claude").is_ok() {
            "claude ready".to_owned()
        } else {
            "claude enabled, not on PATH".to_owned()
        });
    }
    if config.codex {
        parts.push(if codex_standalone_missing {
            "codex enabled, standalone install missing".to_owned()
        } else {
            "codex ready".to_owned()
        });
    }
    println!("  remote control: {}", parts.join("; "));

    if codex_standalone_missing {
        println!(
            "  remote control: `rimz start` refuses until the managed standalone Codex install exists — {}",
            rimz::remote_control::CODEX_INSTALL_COMMAND,
        );
    }
}

#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
pub(super) fn report_sidebar_pane() {
    println!("  sidebar renderer: built into rimz");
}

#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
pub(super) fn report_tmux_capabilities() {
    match tmux_mod::capabilities() {
        Ok(caps) => {
            let floor_status = if caps.meets_min_version {
                "OK"
            } else {
                "TOO OLD"
            };
            let (maj, min, patch) = MIN_TMUX_VERSION;
            println!("  tmux floor    : {floor_status} (>= {maj}.{min}.{patch} required)");
            // Popup landed in 3.2; the floor gate covers it.
            let popup_status = if caps.popup_supported {
                "supported".to_owned()
            } else {
                format!("unavailable (requires tmux >= {maj}.{min}.{patch})")
            };
            println!("  tmux popup    : {popup_status}");
        }
        Err(err) => println!("  tmux floor    : unavailable ({err})"),
    }
}

#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
pub(super) fn report_socket_headroom(ws: &rimz::ResolvedWorkspace) {
    if StatePaths::for_workspace(ws.workspace_id.clone()).is_err() {
        return;
    }
    let runtime = match RuntimePaths::for_workspace(ws.workspace_id.clone()) {
        Ok(r) => r,
        Err(err) => {
            println!("  sock headroom : unavailable ({err})");
            return;
        }
    };
    let dir_len = runtime.sock_dir.as_os_str().len();
    let total = dir_len + LONGEST_SOCKET_TAIL_LEN;
    let status = if total < AF_UNIX_PATH_LIMIT {
        "OK"
    } else {
        "TIGHT"
    };
    println!(
        "  sock headroom : {status} ({total}/{AF_UNIX_PATH_LIMIT} bytes for {})",
        runtime.sock_dir.display(),
    );
}

#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
pub(super) fn report_recent_diagnostics(ws: &rimz::ResolvedWorkspace) {
    let Some((path, records)) = rimz::diag::recent_records(ws.workspace_id.clone(), 12) else {
        println!("  diagnostics   : unavailable");
        return;
    };
    if records.is_empty() {
        println!("  diagnostics   : no recent records ({})", path.display());
        return;
    }
    println!(
        "  diagnostics   : {} recent records ({})",
        records.len(),
        path.display()
    );
    let now_ms = rimz::sidebar::cache::unix_now_ms();
    for record in records {
        println!(
            "    {:<5} {:<24} {:>8}  {}",
            severity_label(record.severity),
            record.event.kind_name(),
            age_ms_short(now_ms, record.at_ms),
            diagnostic_summary(&record.event),
        );
    }
}

fn severity_label(severity: rimz::schema::diag::DiagSeverity) -> &'static str {
    match severity {
        rimz::schema::diag::DiagSeverity::Info => "info",
        rimz::schema::diag::DiagSeverity::Warn => "warn",
        rimz::schema::diag::DiagSeverity::Error => "error",
    }
}

fn age_ms_short(now_ms: u64, then_ms: u64) -> String {
    let secs = now_ms.saturating_sub(then_ms) / 1_000;
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3_600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3_600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

fn diagnostic_summary(event: &rimz::schema::diag::DiagEvent) -> String {
    use rimz::schema::diag::DiagEvent;
    match event {
        DiagEvent::FrameRejected {
            reason,
            prior_pane_count,
            fresh_pane_count,
            frames_ref,
        } => format!(
            "rejected {reason:?}; panes {prior_pane_count}->{fresh_pane_count}{}",
            frames_ref
                .as_ref()
                .map(|name| format!("; frames {name}"))
                .unwrap_or_default()
        ),
        DiagEvent::FrameRejectEscape { held_ms } => {
            format!("published after holding {held_ms}ms")
        }
        DiagEvent::FrameShrinkVerified { prior, fresh } => {
            format!("verified shrink {prior}->{fresh}")
        }
        DiagEvent::PaneCountDrop {
            prior,
            new,
            frames_ref,
            ..
        } => format!(
            "pane count {prior}->{new}{}",
            frames_ref
                .as_ref()
                .map(|name| format!("; frames {name}"))
                .unwrap_or_default()
        ),
        DiagEvent::GateHold {
            rule,
            reject_streak,
            ..
        } => format!("held {rule:?}; streak {reject_streak}"),
        DiagEvent::GateRelease {
            rule,
            held_ms,
            via_escape_hatch,
        } => format!("released {rule:?} after {held_ms}ms; escape={via_escape_hatch}"),
        DiagEvent::FetchFailure {
            reason,
            failure_streak,
        } => format!("{reason}; streak {failure_streak}"),
        DiagEvent::HealthAlert {
            reason,
            recovered_after_ms,
            ..
        } => match recovered_after_ms {
            Some(ms) => format!("recovered after {ms}ms: {reason}"),
            None => reason.clone(),
        },
        DiagEvent::ProducerElected { prior_elder } => {
            format!("this renderer became producer after {prior_elder} aged out")
        }
        DiagEvent::ProducerDemoted { new_elder } => {
            format!("this renderer stopped producing; elder {new_elder}")
        }
        DiagEvent::RowConflict {
            agent_kind,
            agent_session_id,
            bound_pane,
            conflicting_pane,
        } => format!(
            "{agent_kind}/{agent_session_id} already on {bound_pane}; suppressed {conflicting_pane}"
        ),
        DiagEvent::DuplicatePaneId { pane_id } => format!("duplicate {pane_id} suppressed"),
        DiagEvent::ForeignSessionPane { pane_id, session } => {
            format!("dropped {pane_id} from session {session}")
        }
        DiagEvent::GroupMigration {
            pane_id, from, to, ..
        } => format!(
            "{pane_id} moved {}:{} -> {}:{}",
            from.kind, from.key, to.kind, to.key
        ),
        DiagEvent::NewbornQuarantined { pane_id } => {
            format!("held newborn {pane_id} until cwd resolves")
        }
        DiagEvent::RendererPanic { message, .. } => message.clone(),
        DiagEvent::FrameAnomaly {
            anomaly,
            suppressed_since_last,
            ..
        } => {
            let subject = anomaly
                .subject()
                .map(|subject| format!(" on {subject}"))
                .unwrap_or_default();
            let suppressed = if *suppressed_since_last > 0 {
                format!("; {suppressed_since_last} suppressed")
            } else {
                String::new()
            };
            format!("observed {}{subject}{suppressed}", anomaly.key())
        }
    }
}

/// The machine's room tree: every recorded workspace with its root, root
/// class, and liveness, the current directory's room starred. Live rooms
/// whose roots nest earn an overlap line — legal by design (an agent belongs
/// to the room its pane lives in), surfaced so the human always sees it.
#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
pub(super) fn report_room_tree(current: Option<&rimz::ResolvedWorkspace>) {
    let known = match rimz::workspace::known_workspaces() {
        Ok(known) => known,
        Err(err) => {
            println!("  rooms         : unavailable ({err})");
            return;
        }
    };
    if known.is_empty() {
        println!("  rooms         : none recorded");
        return;
    }
    let live = super::super::live_session_names();
    let live_count = known
        .iter()
        .filter(|ws| live.contains(&ws.session_name))
        .count();
    println!(
        "  rooms         : {} recorded, {live_count} live",
        known.len()
    );
    let mut rooms: Vec<_> = known.iter().collect();
    rooms.sort_by(|a, b| a.project_root.cmp(&b.project_root));
    for ws in &rooms {
        let liveness = if live.contains(&ws.session_name) {
            "live"
        } else {
            "idle"
        };
        let here = if current.is_some_and(|cur| cur.workspace_id == ws.workspace_id) {
            "* "
        } else {
            "  "
        };
        println!(
            "    {here}{session}  {root} ({class}) · {liveness}",
            session = ws.session_name,
            root = ws.project_root.display(),
            class = ws.root_class.label(),
        );
    }
    for (i, a) in rooms.iter().enumerate() {
        for b in rooms.iter().skip(i + 1) {
            if !(live.contains(&a.session_name) && live.contains(&b.session_name)) {
                continue;
            }
            if rimz::workspace::root_contains(&a.project_root, &b.project_root)
                || rimz::workspace::root_contains(&b.project_root, &a.project_root)
            {
                println!(
                    "  rooms overlap : `{}` and `{}` nest; an agent belongs to the room its pane lives in",
                    a.session_name, b.session_name,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::schema::diag::{DiagEvent, FrameRejectReason};

    fn sidebar(raw: &str) -> rimz::SidebarInstanceId {
        rimz::SidebarInstanceId::parse(raw).expect("valid sidebar id")
    }

    #[test]
    fn diagnostic_summary_includes_frame_ref_and_producer_peer_ids() {
        let rejected = diagnostic_summary(&DiagEvent::FrameRejected {
            reason: FrameRejectReason::MissingOwnPane,
            prior_pane_count: 3,
            fresh_pane_count: 2,
            frames_ref: Some("frame.42.0.frame_rejected.json".to_owned()),
        });
        assert!(rejected.contains("frame.42.0.frame_rejected.json"));

        let elder = sidebar("sb_019e8c565bbd708097fce9514f79da04");
        assert!(
            diagnostic_summary(&DiagEvent::ProducerElected {
                prior_elder: elder.clone(),
            })
            .contains(elder.as_str())
        );
        assert!(
            diagnostic_summary(&DiagEvent::ProducerDemoted {
                new_elder: elder.clone(),
            })
            .contains(elder.as_str())
        );
    }
}
