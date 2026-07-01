use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::SystemTime;

use rimz::RuntimePaths;
use rimz::config::{ColorDepth, MachineConfig, ThemeMode};
use rimz::ids::MuxName;
use rimz::mux::{
    MuxBackend, SessionHealth,
    tmux::{self as tmux_mod, MIN_TMUX_VERSION},
    zellij::{self as zellij_mod, MIN_ZELLIJ_VERSION},
};

use super::model;

pub(super) fn collect_terminal() -> model::Terminal {
    let theme_mode = MachineConfig::load()
        .map(|config| config.theme.effective_theme_mode())
        .unwrap_or_default();
    let signals = rimz::tui::TruecolorSignals::detect();
    let truecolor_advertised = signals.truecolor();
    let depth = theme_mode.depth(truecolor_advertised);
    let resolved_depth = match depth {
        ColorDepth::Truecolor => "truecolor",
        ColorDepth::Indexed => "256",
    };
    let fix = (theme_mode == ThemeMode::Auto && !truecolor_advertised)
        .then(|| "set `[theme] mode = \"truecolor\"` to force RGB".to_owned());
    model::Terminal {
        theme_mode,
        truecolor_advertised,
        resolved_depth,
        colorterm: signals.colorterm,
        term: signals.term,
        terminfo_truecolor: signals.terminfo,
        fix,
    }
}

pub(super) fn collect_host() -> model::Host {
    let uid = nix::unistd::Uid::current().as_raw();
    model::Host {
        user: rimz::proc::user_name(uid),
        uid,
        binary: std::env::current_exe()
            .ok()
            .map(|path| path.display().to_string()),
    }
}

pub(super) fn collect_storage() -> model::Storage {
    let storage = rimz::storage::measure();
    model::Storage {
        total_bytes: storage.total_bytes(),
        roots: storage
            .roots
            .into_iter()
            .map(|root| model::StorageRootView {
                label: root.kind.label(),
                path: root.path.display().to_string(),
                bytes: root.bytes,
                present: root.present,
            })
            .collect(),
    }
}

/// The multiplexer section: which backend Rimz detected, its version, floor, and
/// server socket; once a workspace resolves, its session, duplicate-session, and
/// presence health.
pub(super) fn collect_mux(
    mux_hint: Option<MuxName>,
    ws: Option<&rimz::ResolvedWorkspace>,
) -> model::Probe<model::Mux> {
    let mux = match rimz::mux::auto_detect_backend(mux_hint) {
        Ok(mux) => mux,
        Err(err) => {
            return model::Probe::Unavailable {
                error: err.to_string(),
            };
        }
    };
    let backend = rimz::mux::backend_for(mux);
    let version = match backend.version() {
        Ok(version) if !version.is_empty() => model::Version::Reported { version },
        Ok(_) => model::Version::Unknown,
        Err(err) => model::Version::Unavailable {
            error: err.to_string(),
        },
    };
    let capabilities = match mux {
        MuxName::Zellij => model::Capabilities::Zellij(collect_zellij_capabilities()),
        MuxName::Tmux => model::Capabilities::Tmux(collect_tmux_capabilities()),
    };
    let mut report = model::Mux {
        name: mux,
        version,
        capabilities,
        zellij_socket: None,
        socket: None,
        session_health: None,
        duplicate_sessions: None,
        presence: None,
    };
    if mux == MuxName::Tmux {
        report.socket = Some(tmux_mod::default_server_socket_path().display().to_string());
    }
    if let Some(ws) = ws {
        if mux == MuxName::Zellij {
            report.zellij_socket = Some(collect_zellij_socket_headroom(ws));
        }
        report.session_health = Some(collect_session_health(backend.as_ref(), &ws.session_name));
        report.duplicate_sessions = Some(collect_duplicate_sessions(ws));
        report.presence = Some(collect_presence(ws, mux));
    }
    model::Probe::Ready(report)
}

fn collect_session_health(
    backend: &dyn MuxBackend,
    session_name: &str,
) -> model::Probe<model::SessionHealth> {
    match backend.probe_session_health(session_name) {
        // `probe_session_health` never returns `Reborn` (it does not mutate), so
        // the live verdict is just clean-or-stuck.
        Ok(SessionHealth::Healthy | SessionHealth::Reborn) => {
            model::Probe::Ready(model::SessionHealth::Ok)
        }
        Ok(SessionHealth::Stuck) => model::Probe::Ready(model::SessionHealth::Stuck {
            fix: "run `rimz reset` to rebuild".to_owned(),
        }),
        Err(err) => model::Probe::Unavailable {
            error: err.to_string(),
        },
    }
}

fn collect_zellij_capabilities() -> model::Probe<model::ZellijCaps> {
    match zellij_mod::capabilities() {
        Ok(caps) => model::Probe::Ready(model::ZellijCaps {
            meets_min_version: caps.meets_min_version,
            min_version: MIN_ZELLIJ_VERSION,
        }),
        Err(err) => model::Probe::Unavailable {
            error: err.to_string(),
        },
    }
}

fn collect_tmux_capabilities() -> model::Probe<model::TmuxCaps> {
    match tmux_mod::capabilities() {
        Ok(caps) => model::Probe::Ready(model::TmuxCaps {
            meets_min_version: caps.meets_min_version,
            min_version: MIN_TMUX_VERSION,
            // Popup landed in 3.2; the floor gate covers it.
            popup_supported: caps.popup_supported,
        }),
        Err(err) => model::Probe::Unavailable {
            error: err.to_string(),
        },
    }
}

fn collect_zellij_socket_headroom(ws: &rimz::ResolvedWorkspace) -> model::ZellijSocket {
    let headroom = zellij_mod::socket_headroom(&ws.session_name);
    let fits = headroom.len < headroom.limit;
    model::ZellijSocket {
        fits,
        len: headroom.len,
        limit: headroom.limit,
        path: headroom.path.display().to_string(),
        fix: (!fits).then(|| "export ZELLIJ_SOCKET_DIR=/tmp/zellij and rerun rimz".to_owned()),
    }
}

/// Live sidebar sessions that share this workspace. Producer election is
/// workspace-wide, so an old room for the same workspace can keep producing the
/// shared pane cache and make the current room's renderer hold updates.
fn collect_duplicate_sessions(
    ws: &rimz::ResolvedWorkspace,
) -> model::Probe<model::DuplicateSessions> {
    let runtime = match RuntimePaths::for_workspace(ws.workspace_id.clone()) {
        Ok(runtime) => runtime,
        Err(err) => {
            return model::Probe::Unavailable {
                error: err.to_string(),
            };
        }
    };
    let heartbeats = match fresh_sidebar_heartbeats_for_doctor(&runtime) {
        Ok(heartbeats) => heartbeats,
        Err(err) => {
            return model::Probe::Unavailable {
                error: err.to_string(),
            };
        }
    };
    let groups: Vec<model::SidebarGroup> = duplicate_sidebar_session_groups(&heartbeats)
        .into_iter()
        .map(|group| model::SidebarGroup {
            is_current: group.session_name == ws.session_name,
            session_name: group.session_name,
            sidebar_count: group.sidebar_count,
            pane_ids: group.pane_ids,
        })
        .collect();
    let advice = (!groups.is_empty())
        .then(|| "close stale sidebars or retire stale sessions when safe".to_owned());
    model::Probe::Ready(model::DuplicateSessions { groups, advice })
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

/// The producer's pane-discovery mode for this workspace — event when the
/// backend's presence channel pokes, otherwise poll with the first failing
/// precondition and its fix.
fn collect_presence(ws: &rimz::ResolvedWorkspace, mux: MuxName) -> model::Presence {
    use rimz::sidebar::cache::{presence_event_mode, presence_stamp_age_ms};

    let runtime = match RuntimePaths::for_workspace(ws.workspace_id.clone()) {
        Ok(runtime) => runtime,
        Err(err) => {
            return model::Presence::Unavailable {
                error: err.to_string(),
            };
        }
    };
    let age = presence_stamp_age_ms(&runtime);
    if presence_event_mode(age) {
        return model::Presence::Event {
            poked_secs: age.unwrap_or(0) / 1000,
        };
    }
    if mux == MuxName::Tmux {
        let reason = match age {
            Some(age) => format!(
                "last control-mode watch poke {}s ago (watch idle, detached, or producer not elected)",
                age / 1000,
            ),
            None => {
                "control-mode presence watch not attached (old tmux, or producer not yet elected)"
                    .to_owned()
            }
        };
        return model::Presence::Poll { reason };
    }
    // Poll mode: name the first failing precondition in fix order.
    if zellij_mod::presence_plugin_path().is_none() {
        return model::Presence::Poll {
            reason: "embedded plugin unavailable or could not materialize (reinstall rimz)"
                .to_owned(),
        };
    }
    let meets_floor = zellij_mod::capabilities().is_ok_and(|caps| {
        caps.parsed_version
            .is_some_and(|v| v >= zellij_mod::PRESENCE_PLUGIN_MIN_ZELLIJ)
    });
    if !meets_floor {
        let (maj, min, patch) = zellij_mod::PRESENCE_PLUGIN_MIN_ZELLIJ;
        return model::Presence::Poll {
            reason: format!("zellij below the plugin floor (>= {maj}.{min}.{patch} required)"),
        };
    }
    let reason = match age {
        Some(age) => format!(
            "last plugin poke {}s ago (plugin gone or `rimz` not runnable from Zellij; \
             reattach or run `rimz reload`)",
            age / 1000,
        ),
        None => "no plugin poke yet (approve the one-time permission prompt in the Zellij session)"
            .to_owned(),
    };
    model::Presence::Poll { reason }
}

/// Per-machine remote-control auto-launch posture. Doctor separates hard
/// `rimz start` refusals for installed-agent misconfiguration from enabled
/// hosts whose agent is not installed; start skips those inert toggles.
pub(super) fn collect_remote_control() -> model::RemoteControl {
    let config = match MachineConfig::load() {
        Ok(config) => config.remote_control,
        Err(err) => {
            return model::RemoteControl::Unavailable {
                error: err.to_string(),
            };
        }
    };
    if !config.claude && !config.codex {
        return model::RemoteControl::Off;
    }

    let claude_preflight = config
        .claude
        .then(|| rimz::remote_control::preflight_claude(&config));
    let codex_preflight = config
        .codex
        .then(|| rimz::remote_control::preflight_codex(&config));
    let mut agents = Vec::new();
    if config.claude {
        let claude_present = which::which("claude").is_ok();
        let (label, ready) = if !claude_present {
            ("claude enabled, not on PATH".to_owned(), false)
        } else if claude_preflight.as_ref().is_some_and(Result::is_ok) {
            ("claude ready".to_owned(), true)
        } else {
            ("claude enabled, blocked".to_owned(), false)
        };
        agents.push(model::RemoteAgent { label, ready });
    }
    if config.codex {
        let (label, ready) = if codex_preflight.as_ref().is_some_and(Result::is_err) {
            (
                "codex enabled, standalone install missing".to_owned(),
                false,
            )
        } else {
            ("codex ready".to_owned(), true)
        };
        agents.push(model::RemoteAgent { label, ready });
    }

    let (skipped, refusals): (Vec<_>, Vec<_>) =
        [codex_preflight.as_ref(), claude_preflight.as_ref()]
            .into_iter()
            .flatten()
            .filter_map(|result| result.as_ref().err())
            .partition(|err| err.is_uninstalled_host());
    let skipped = skipped.into_iter().map(ToString::to_string).collect();
    let refusals = refusals.into_iter().map(ToString::to_string).collect();
    model::RemoteControl::On {
        agents,
        refusals,
        skipped,
    }
}

pub(super) fn collect_socket_headroom(
    ws: &rimz::ResolvedWorkspace,
) -> model::Probe<model::SockBudget> {
    let runtime = match RuntimePaths::under(
        ws.workspace_id.clone(),
        &rimz::ledger::paths::runtime_home(),
    ) {
        Ok(runtime) => runtime,
        Err(err) => {
            return model::Probe::Unavailable {
                error: err.to_string(),
            };
        }
    };
    let budget = rimz::sock::SockBudget::for_sock_dir(&runtime.sock_dir);
    let fits = budget.fits();
    model::Probe::Ready(model::SockBudget {
        fits,
        used: budget.used,
        limit: budget.limit,
        dir: budget.sock_dir.display().to_string(),
        remedy: (!fits).then(|| format!("{} and rerun rimz", rimz::sock::XDG_REMEDY)),
    })
}

/// The machine's room tree: every recorded workspace with its root, class, and
/// liveness. Live rooms whose roots nest earn an overlap entry — legal by design
/// (an agent belongs to the room its pane lives in), surfaced so it stays seen.
pub(super) fn collect_rooms(
    current: Option<&rimz::ResolvedWorkspace>,
) -> model::Probe<model::Rooms> {
    let known = match rimz::workspace::known_workspaces() {
        Ok(known) => known,
        Err(err) => {
            return model::Probe::Unavailable {
                error: err.to_string(),
            };
        }
    };
    let live = super::super::live_session_names();
    let live_count = known
        .iter()
        .filter(|ws| live.contains(&ws.session_name))
        .count();
    let mut sorted: Vec<_> = known.iter().collect();
    sorted.sort_by(|a, b| a.project_root.cmp(&b.project_root));
    let rooms = sorted
        .iter()
        .map(|ws| model::Room {
            session_name: ws.session_name.clone(),
            project_root: ws.project_root.display().to_string(),
            root_class: ws.root_class,
            live: live.contains(&ws.session_name),
            is_current: current.is_some_and(|cur| cur.workspace_id == ws.workspace_id),
        })
        .collect();
    let mut overlaps = Vec::new();
    for (i, a) in sorted.iter().enumerate() {
        for b in sorted.iter().skip(i + 1) {
            if !(live.contains(&a.session_name) && live.contains(&b.session_name)) {
                continue;
            }
            if rimz::workspace::root_contains(&a.project_root, &b.project_root)
                || rimz::workspace::root_contains(&b.project_root, &a.project_root)
            {
                overlaps.push(model::RoomOverlap {
                    a: a.session_name.clone(),
                    b: b.session_name.clone(),
                });
            }
        }
    }
    model::Probe::Ready(model::Rooms {
        recorded: known.len(),
        live: live_count,
        rooms,
        overlaps,
    })
}

pub(super) fn collect_diagnostics(ws: &rimz::ResolvedWorkspace) -> model::Diagnostics {
    let Some((path, records)) = rimz::diag::recent_records(ws.workspace_id.clone(), 12) else {
        return model::Diagnostics::Unavailable;
    };
    let records = records
        .into_iter()
        .map(|record| model::DiagRow {
            severity: record.severity,
            kind: record.event.kind_name().to_owned(),
            at_ms: record.at_ms,
            summary: summary_with_suppressed(
                diagnostic_summary(&record.event),
                record.suppressed_since_last,
            ),
        })
        .collect();
    model::Diagnostics::Ready {
        path: path.display().to_string(),
        records,
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

fn summary_with_suppressed(summary: String, suppressed_since_last: u32) -> String {
    if suppressed_since_last == 0 {
        summary
    } else {
        format!("{summary}; {suppressed_since_last} suppressed")
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
    fn summary_with_suppressed_appends_nonzero_count() {
        assert_eq!(
            summary_with_suppressed("refuted panes".to_owned(), 0),
            "refuted panes"
        );
        assert_eq!(
            summary_with_suppressed("refuted panes".to_owned(), 3),
            "refuted panes; 3 suppressed"
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
