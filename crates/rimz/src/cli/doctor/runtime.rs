use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::SystemTime;

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

#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
pub(super) fn report_zellij_socket_headroom(ws: &rimz::ResolvedWorkspace) {
    let headroom = zellij_mod::socket_headroom(&ws.session_name);
    let status = if headroom.len < headroom.limit {
        "OK"
    } else {
        "TOO LONG"
    };
    println!(
        "  zellij socket : {status} ({}/{} bytes for {})",
        headroom.len,
        headroom.limit,
        headroom.path.display(),
    );
    if headroom.len >= headroom.limit {
        println!("  zellij socket : export ZELLIJ_SOCKET_DIR=/tmp/zellij and rerun rimz");
    }
}

/// Report live sidebar sessions that share this workspace. Producer election
/// is workspace-wide, so an old room for the same workspace can keep producing
/// the shared pane cache for its session and make the current room's renderer
/// hold frameless updates.
#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
pub(super) fn report_duplicate_sidebar_sessions(ws: &rimz::ResolvedWorkspace) {
    let runtime = match RuntimePaths::for_workspace(ws.workspace_id.clone()) {
        Ok(runtime) => runtime,
        Err(err) => {
            println!("  duplicate sessions: unavailable ({err})");
            return;
        }
    };
    let heartbeats = match fresh_sidebar_heartbeats_for_doctor(&runtime) {
        Ok(heartbeats) => heartbeats,
        Err(err) => {
            println!("  duplicate sessions: unavailable ({err})");
            return;
        }
    };
    let groups = duplicate_sidebar_session_groups(&heartbeats);
    if groups.is_empty() {
        println!("  duplicate sessions: none");
        return;
    }

    println!(
        "  duplicate sessions: WARN ({} live sessions share this workspace; pane updates can be held)",
        groups.len(),
    );
    for group in groups {
        let here = if group.session_name == ws.session_name {
            "* "
        } else {
            "  "
        };
        let panes = if group.pane_ids.is_empty() {
            "unlocated".to_owned()
        } else {
            group.pane_ids.join(", ")
        };
        println!(
            "    {here}{session}: {count} sidebars ({panes})",
            session = group.session_name,
            count = group.sidebar_count,
        );
    }
    println!("  duplicate sessions: close stale sidebars or retire stale sessions when safe");
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SidebarSessionGroup {
    session_name: String,
    sidebar_count: usize,
    pane_ids: Vec<String>,
}

fn duplicate_sidebar_session_groups(
    heartbeats: &[rimz::schema::heartbeat::SidebarHeartbeat],
) -> Vec<SidebarSessionGroup> {
    let mut by_session: BTreeMap<String, Vec<&rimz::schema::heartbeat::SidebarHeartbeat>> =
        BTreeMap::new();
    for heartbeat in heartbeats {
        by_session
            .entry(heartbeat.session_name.clone())
            .or_default()
            .push(heartbeat);
    }
    if by_session.len() < 2 {
        return Vec::new();
    }
    by_session
        .into_iter()
        .map(|(session_name, mut heartbeats)| {
            heartbeats
                .sort_by(|left, right| left.instance_id.as_str().cmp(right.instance_id.as_str()));
            let sidebar_count = heartbeats.len();
            let mut pane_ids = heartbeats
                .into_iter()
                .filter_map(|heartbeat| heartbeat.pane_id.as_ref().map(ToString::to_string))
                .collect::<Vec<_>>();
            pane_ids.sort();
            pane_ids.dedup();
            SidebarSessionGroup {
                session_name,
                sidebar_count,
                pane_ids,
            }
        })
        .collect()
}

fn fresh_sidebar_heartbeats_for_doctor(
    runtime: &RuntimePaths,
) -> std::io::Result<Vec<rimz::schema::heartbeat::SidebarHeartbeat>> {
    let entries = match fs::read_dir(&runtime.heartbeat_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut heartbeats = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if !rimz::schema::heartbeat::SidebarHeartbeat::is_heartbeat_file(&path)
            || !heartbeat_mtime_is_fresh(&path)
        {
            continue;
        }
        let heartbeat = match rimz::schema::heartbeat::SidebarHeartbeat::read_from(&path) {
            Ok(heartbeat) => heartbeat,
            Err(_) => continue,
        };
        if heartbeat.protocol_version != rimz::schema::SIDEBAR_PROTOCOL_VERSION
            || heartbeat.workspace_id != runtime.workspace_id
        {
            continue;
        }
        heartbeats.push(heartbeat);
    }
    Ok(heartbeats)
}

fn heartbeat_mtime_is_fresh(path: &Path) -> bool {
    let modified = match fs::metadata(path).and_then(|meta| meta.modified()) {
        Ok(modified) => modified,
        Err(_) => return false,
    };
    match SystemTime::now().duration_since(modified) {
        Ok(age) => age <= rimz::sidebar::timing::SIDEBAR_HEARTBEAT_TTL,
        Err(_) => true,
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

/// Report the per-machine remote-control auto-launch posture. Configured hosts
/// have hard preconditions that `rimz start` enforces fail-fast, so doctor
/// surfaces the same gaps and fixes ahead of time.
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

    let claude_preflight = config
        .claude
        .then(|| rimz::remote_control::preflight_claude(&config));
    let codex_preflight = config
        .codex
        .then(|| rimz::remote_control::preflight_codex(&config));
    let mut parts = Vec::new();
    if config.claude {
        let claude_present = which::which("claude").is_ok();
        parts.push(if !claude_present {
            "claude enabled, not on PATH".to_owned()
        } else if claude_preflight.as_ref().is_some_and(Result::is_ok) {
            "claude ready".to_owned()
        } else {
            "claude enabled, blocked".to_owned()
        });
    }
    if config.codex {
        parts.push(if codex_preflight.as_ref().is_some_and(Result::is_err) {
            "codex enabled, standalone install missing".to_owned()
        } else {
            "codex ready".to_owned()
        });
    }
    println!("  remote control: {}", parts.join("; "));

    for err in [codex_preflight.as_ref(), claude_preflight.as_ref()]
        .into_iter()
        .flatten()
        .filter_map(|result| result.as_ref().err())
    {
        println!("  remote control: `rimz start` refuses:\n{err}");
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
        DiagEvent::PaneCarryForward {
            carried,
            prior,
            fresh,
            cli_confirmed,
            frames_ref,
            ..
        } => format!(
            "carried {} panes over source shrink {prior}->{fresh}; cli_confirmed={cli_confirmed}{}",
            carried.len(),
            frames_ref
                .as_ref()
                .map(|name| format!("; frames {name}"))
                .unwrap_or_default()
        ),
        DiagEvent::PaneCarryRefuted {
            carried,
            prior,
            fresh,
            verified,
            frames_ref,
            ..
        } => format!(
            "refuted {} carried panes after source re-pull {prior}->{fresh}->{verified}{}",
            carried.len(),
            frames_ref
                .as_ref()
                .map(|name| format!("; frames {name}"))
                .unwrap_or_default()
        ),
        DiagEvent::CarryForwardExpired {
            pane_id,
            pid,
            carried_ms,
        } => match pid {
            Some(pid) => format!("expired carried {pane_id} pid {pid} after {carried_ms}ms"),
            None => format!("expired carried {pane_id} after {carried_ms}ms"),
        },
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
        DiagEvent::LinkAlert {
            tier,
            rtt_ms,
            miss_pct,
            recovered_after_ms,
            ..
        } => {
            let rtt = rtt_ms
                .map(|ms| format!("{ms}ms"))
                .unwrap_or_else(|| "?".to_owned());
            match recovered_after_ms {
                Some(ms) => format!("link recovered after {ms}ms; rtt {rtt}; loss {miss_pct}%"),
                None => format!("link {tier:?}; rtt {rtt}; loss {miss_pct}%"),
            }
        }
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
        DiagEvent::FocusContested {
            view_id,
            candidates,
            resolved,
        } => format!("focus contested in {view_id}: {candidates:?}; resolved {resolved}"),
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
        DiagEvent::MixedBuildWriters {
            prior_build,
            own_build,
        } => format!("prior frame from build {prior_build}; this producer is {own_build}"),
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

    fn heartbeat(
        session_name: &str,
        instance_id: &str,
        pane: Option<&str>,
    ) -> rimz::schema::heartbeat::SidebarHeartbeat {
        rimz::schema::heartbeat::SidebarHeartbeat::new(
            rimz::WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
            sidebar(instance_id),
            rimz::MuxName::Zellij,
            session_name,
            "/tmp/sidebar.sock".into(),
            pane.map(|pane| rimz::PaneId::parse(pane).unwrap()),
        )
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

    #[test]
    fn duplicate_sidebar_sessions_require_multiple_session_names() {
        let same_session = vec![
            heartbeat(
                "rimz-current",
                "sb_019eb7da41f478b2a84079743e472a87",
                Some("zellij:terminal_1"),
            ),
            heartbeat(
                "rimz-current",
                "sb_019eb7da43787c6081a474afb02c2067",
                Some("zellij:terminal_2"),
            ),
        ];
        assert!(
            duplicate_sidebar_session_groups(&same_session).is_empty(),
            "multiple sidebars in one session are normal"
        );

        let duplicate_sessions = vec![
            heartbeat(
                "rimz-current",
                "sb_019eb7da41f478b2a84079743e472a87",
                Some("zellij:terminal_1"),
            ),
            heartbeat(
                "rimz-old",
                "sb_019eb7da2dda7992b4286dee69d33358",
                Some("zellij:terminal_7"),
            ),
            heartbeat("rimz-old", "sb_019eb7da2de17752994de2401b433b70", None),
        ];
        let groups = duplicate_sidebar_session_groups(&duplicate_sessions);
        assert_eq!(
            groups,
            vec![
                SidebarSessionGroup {
                    session_name: "rimz-current".to_owned(),
                    sidebar_count: 1,
                    pane_ids: vec!["zellij:terminal_1".to_owned()],
                },
                SidebarSessionGroup {
                    session_name: "rimz-old".to_owned(),
                    sidebar_count: 2,
                    pane_ids: vec!["zellij:terminal_7".to_owned()],
                },
            ]
        );
    }
}
