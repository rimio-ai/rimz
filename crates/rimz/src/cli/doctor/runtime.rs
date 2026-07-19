use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rimz::config::{ColorDepth, MachineConfig, ThemeMode};
use rimz::ids::MuxName;
use rimz::mux::{
    MuxBackend, SessionHealth, binaries, logtail,
    tmux::{self as tmux_mod, MIN_TMUX_VERSION},
    zellij::{self as zellij_mod, MIN_ZELLIJ_VERSION},
};
use rimz::store::event::SessionDeathCause;
use rimz::{RuntimePaths, StatePaths};

use super::model;

const MUX_LOG_WINDOW_BYTES: u64 = 256 * 1024;
const MUX_LOG_ENTRY_CAP: usize = 10;
const TOPOLOGY_CONFLICT_FRESH_MS: u64 = 10 * 60 * 1000;

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
    let disk_usage = rimz::disk_usage::measure();
    model::Storage {
        total_bytes: disk_usage.total_bytes(),
        roots: disk_usage
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

pub(super) fn collect_last_incident(
    ws: &rimz::ResolvedWorkspace,
    cleared_at: Option<jiff::Timestamp>,
) -> Option<model::LastIncident> {
    let paths = StatePaths::for_workspace(ws.workspace_id.clone()).ok()?;
    let marker: rimz::store::event::LastDeathMarker =
        serde_json::from_slice(&fs::read(&paths.last_death_marker).ok()?).ok()?;
    if cleared_at.is_some_and(|cleared_at| marker.at <= cleared_at) {
        return None;
    }
    let forensics = (marker.cause == SessionDeathCause::Crash)
        .then(|| newest_crash_archive(&paths.crashes_dir))
        .flatten()
        .map(|path| path.display().to_string());
    let lost_agents = marker
        .lost_agents
        .iter()
        .map(|agent| model::IncidentAgent {
            kind: agent.kind.as_str().to_owned(),
            name: agent.name.clone(),
            agent_id: agent.agent_id.as_str().to_owned(),
        })
        .collect();
    Some(model::LastIncident {
        cause: marker.cause.as_str(),
        at: marker.at,
        lost_agents,
        recovered: marker.recovered,
        forensics,
    })
}

fn newest_crash_archive(crashes_dir: &Path) -> Option<PathBuf> {
    fs::read_dir(crashes_dir)
        .ok()?
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .max_by_key(|path| path.file_name().map(std::ffi::OsStr::to_owned))
}

/// The multiplexer section: which backend RimZ detected, its version, floor, and
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
    let binaries = collect_mux_binaries(mux);
    let log = collect_mux_log(mux);
    let mut report = model::Mux {
        name: mux,
        version,
        capabilities,
        binaries,
        log,
        room: None,
        presence_plugins: None,
        zellij_socket: None,
        socket: None,
        session_health: None,
        duplicate_sessions: None,
        presence: None,
        topology_writer: None,
        ttyd: (mux == MuxName::Tmux).then(collect_ttyd),
    };
    if mux == MuxName::Tmux {
        report.socket = Some(tmux_mod::default_server_socket_path().display().to_string());
    }
    if let Some(ws) = ws {
        let ownership = rimz::room::session::probe_room_ownership(mux, &ws.session_name);
        report.room = Some(room_view(&ws.session_name, &ownership));
        if mux == MuxName::Zellij {
            report.zellij_socket = Some(collect_zellij_socket_headroom(ws));
        }
        if matches!(
            ownership.selected_state(),
            rimz::room::session::BackendRoomState::Live
                | rimz::room::session::BackendRoomState::Exited
        ) {
            report.session_health =
                Some(collect_session_health(backend.as_ref(), &ws.session_name));
        }
        report.duplicate_sessions = Some(collect_duplicate_sessions(ws, mux));
        report.presence = Some(collect_presence(ws, mux, &ownership));
        if mux == MuxName::Zellij {
            report.topology_writer = collect_topology_writer(ws);
            if matches!(
                ownership.selected_state(),
                rimz::room::session::BackendRoomState::Live
            ) {
                report.presence_plugins = collect_plugin_presence(ws);
            }
        }
    }
    model::Probe::Ready(report)
}

fn room_view(session_name: &str, ownership: &rimz::room::session::RoomOwnership) -> model::Room {
    model::Room {
        session_name: session_name.to_owned(),
        selected_state: room_state_view(ownership.selected_state()),
        live_on: ownership.live_on(),
        conflict: ownership.conflict(),
        zellij: room_state_view(&ownership.zellij),
        tmux: room_state_view(&ownership.tmux),
    }
}

fn room_state_view(state: &rimz::room::session::BackendRoomState) -> model::RoomState {
    match state {
        rimz::room::session::BackendRoomState::Live => model::RoomState::Live,
        rimz::room::session::BackendRoomState::Exited => model::RoomState::Exited,
        rimz::room::session::BackendRoomState::Absent => model::RoomState::Absent,
        rimz::room::session::BackendRoomState::Unavailable { error } => {
            model::RoomState::Unavailable {
                error: error.clone(),
            }
        }
    }
}

fn collect_ttyd() -> model::Probe<model::TtydWeb> {
    match rimz::web::ttyd_diagnostic() {
        Ok(diagnostic) => model::Probe::Ready(model::TtydWeb {
            path: diagnostic.path.display().to_string(),
            version: diagnostic.version,
        }),
        Err(err) => model::Probe::Unavailable {
            error: err.to_string(),
        },
    }
}

fn collect_mux_binaries(mux: MuxName) -> model::MuxBinaries {
    let scan = binaries::scan(mux);
    let mut installs = scan.installs.into_iter();
    let active = installs.next().map(binary_row);
    let duplicates = installs.map(binary_row).collect();
    let server_mismatches = scan
        .servers
        .into_iter()
        .filter(|server| !server.matches_active)
        .map(|server| model::ServerMismatchRow {
            pid: server.pid,
            exe: server.exe.display().to_string(),
            deleted: server.deleted,
        })
        .collect();
    model::MuxBinaries {
        active,
        duplicates,
        server_mismatches,
    }
}

fn binary_row(install: binaries::BinaryInstall) -> model::MuxBinaryRow {
    model::MuxBinaryRow {
        path: install.path.display().to_string(),
        version: install.version,
    }
}

fn collect_mux_log(mux: MuxName) -> model::MuxLog {
    match mux {
        MuxName::Zellij => {
            let path = zellij_mod::log_file();
            match path.try_exists() {
                Ok(true) => scan_mux_log(
                    path,
                    model::LogScope::HostUser {
                        uid: nix::unistd::Uid::current().as_raw(),
                    },
                    zellij_mod::parse_log_line,
                    zellij_mod::diagnose_log_record,
                ),
                Ok(false) => model::MuxLog::Missing {
                    path: path.display().to_string(),
                },
                Err(err) => model::MuxLog::Unavailable {
                    error: format!("{}: {err}", path.display()),
                },
            }
        }
        MuxName::Tmux => match tmux_mod::server_log_file() {
            Some(path) => scan_mux_log(
                path,
                model::LogScope::Server,
                tmux_mod::parse_log_line,
                tmux_mod::diagnose_log_record,
            ),
            None => model::MuxLog::Disabled {
                hint: "server logging off (start tmux with `-v` to enable)".to_owned(),
            },
        },
    }
}

fn scan_mux_log(
    path: std::path::PathBuf,
    scope: model::LogScope,
    parse_line: fn(&str) -> logtail::RecordLine,
    diagnose: fn(
        Option<&logtail::LogicalRecord>,
        &logtail::LogicalRecord,
        Option<&logtail::LogicalRecord>,
    ) -> Option<logtail::LogDiagnosis>,
) -> model::MuxLog {
    match logtail::scan_tail(
        &path,
        MUX_LOG_WINDOW_BYTES,
        MUX_LOG_ENTRY_CAP,
        parse_line,
        diagnose,
    ) {
        Ok(scan) => model::MuxLog::Ready {
            path: path.display().to_string(),
            scope,
            size_bytes: scan.size_bytes,
            scanned_bytes: scan.scanned_bytes,
            logical_records: scan.logical_records,
            problem_records: scan.problem_records,
            omitted_issue_groups: scan.omitted_issue_groups,
            issues: scan
                .issues
                .into_iter()
                .map(|issue| model::MuxLogIssue {
                    source_severity: severity_label(issue.severity).to_owned(),
                    state: match issue.state {
                        logtail::LogState::Investigate => model::DoctorState::Investigate,
                        logtail::LogState::Expected => model::DoctorState::Expected,
                    },
                    impact: match issue.impact {
                        logtail::LogImpact::Alarm => model::DoctorImpact::Alarm,
                        logtail::LogImpact::Warn => model::DoctorImpact::Warn,
                        logtail::LogImpact::Info => model::DoctorImpact::Info,
                    },
                    summary: issue.summary,
                    occurrences: issue.occurrences,
                    first_occurrence: issue.first_occurrence,
                    last_occurrence: issue.last_occurrence,
                    samples: issue.samples,
                    evidence_truncated: issue.evidence_truncated,
                })
                .collect(),
        },
        Err(err) => model::MuxLog::Unavailable {
            error: format!("{}: {err}", path.display()),
        },
    }
}

fn collect_plugin_presence(
    ws: &rimz::ResolvedWorkspace,
) -> Option<model::Probe<model::PresencePlugins>> {
    let runtime = RuntimePaths::for_workspace(ws.workspace_id.clone()).ok()?;
    let state = StatePaths::for_workspace(ws.workspace_id.clone()).ok()?;
    let now_ms = rimz::sidebar::timing::unix_now_ms();
    let cache = rimz::sidebar::cache::read_pane_topology_cache(&runtime, &ws.session_name);
    let cache_writer = cache.as_ref().and_then(|cache| cache.writer.as_ref());
    let conflict = fresh_topology_writer_conflict(&runtime, cache_writer, now_ms);
    let desired = rimz::sidebar::cache::read_presence_desired(&runtime);
    presence_plugins_view(
        zellij_mod::live_presence_plugin_ids(&ws.session_name).map_err(|err| err.to_string()),
        rimz::diag::plugin_presence::recent_generations(&state.root, &ws.session_name),
        cache.as_ref(),
        conflict.as_ref(),
        desired.as_ref(),
        rimz::diag::plugin_presence::history_paths(&state.root)
            .into_iter()
            .map(|path| path.display().to_string())
            .collect(),
        now_ms,
    )
}

fn presence_plugins_view(
    live: Result<Vec<u32>, String>,
    spans: Vec<rimz::diag::plugin_presence::PluginPresenceSpan>,
    cache: Option<&rimz::mux::zellij::pane_topology::PaneTopologyCache>,
    conflict: Option<&rimz::sidebar::presence::TopologyWriterConflict>,
    desired: Option<&rimz::sidebar::cache::PresenceDesired>,
    history: Vec<String>,
    now_ms: u64,
) -> Option<model::Probe<model::PresencePlugins>> {
    let live_ids = match live {
        Ok(ids) => ids,
        Err(error) => return Some(model::Probe::Unavailable { error }),
    };
    let active_writer = cache
        .filter(|cache| rimz::sidebar::cache::pane_topology_cache_is_fresh(cache, now_ms, None))
        .and_then(|cache| cache.writer.as_ref());
    let rejected_writer = conflict.and_then(|conflict| conflict.stale_writer.as_ref());
    let mut spans_by_id = HashMap::<u32, Vec<_>>::new();
    for span in spans {
        spans_by_id.entry(span.plugin_id).or_default().push(span);
    }

    let mut rows = live_ids
        .into_iter()
        .map(|plugin_id| {
            let active = active_writer.filter(|writer| writer.plugin_id == plugin_id);
            let rejected = rejected_writer.filter(|writer| writer.plugin_id == plugin_id);
            let span = if let Some(active) = active {
                spans_by_id.get(&plugin_id).and_then(|spans| {
                    spans
                        .iter()
                        .find(|span| span.loaded_at_ms == active.loaded_at_ms)
                })
            } else {
                spans_by_id
                    .get(&plugin_id)
                    .and_then(|spans| spans.iter().max_by_key(|span| span.loaded_at_ms))
            };
            let writer = active.or_else(|| {
                rejected.filter(|writer| {
                    span.is_none_or(|span| span.loaded_at_ms == writer.loaded_at_ms)
                })
            });
            let loaded_at_ms = span
                .map(|span| span.loaded_at_ms)
                .or_else(|| writer.map(|writer| writer.loaded_at_ms));
            let build = span
                .and_then(|span| span.build.clone())
                .or_else(|| writer.and_then(|writer| writer.build.clone()));
            let (status, rejected_count) = if active.is_some() {
                (model::PresencePluginStatus::Active, None)
            } else if rejected.is_some() {
                (
                    model::PresencePluginStatus::Rejected,
                    conflict.map(|conflict| conflict.rejected_count),
                )
            } else {
                (model::PresencePluginStatus::Inactive, None)
            };
            let outdated =
                desired.is_some_and(|desired| build.as_deref() != Some(desired.build.as_str()));
            let telemetry = span.map(|span| model::PresencePluginTelemetry {
                sample_count: span.sample_count,
                first_at_ms: span.first_at_ms,
                last_at_ms: span.last_at_ms,
                last_seen_age_secs: now_ms.saturating_sub(span.last_at_ms) / 1000,
                zellij_version: span.zellij_version.clone(),
                page_growth: span.page_growth,
                byte_growth: span.byte_growth,
                commands_completed_delta: span.commands_completed_delta,
                commands_succeeded_delta: span.commands_succeeded_delta,
                stale_writer_rejections_delta: span.stale_writer_rejections_delta,
                topology_failures_delta: span.topology_failures_delta,
                other_failures_delta: span.other_failures_delta,
            });
            model::PresencePluginRow {
                plugin_id,
                loaded_at_ms,
                build,
                status,
                rejected_count,
                outdated,
                telemetry,
            }
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .loaded_at_ms
            .cmp(&left.loaded_at_ms)
            .then_with(|| right.plugin_id.cmp(&left.plugin_id))
    });
    if rows.is_empty() && desired.is_none() {
        return None;
    }
    Some(model::Probe::Ready(model::PresencePlugins {
        desired_build: desired.map(|desired| desired.build.clone()),
        rows,
        history,
    }))
}

fn severity_label(severity: logtail::LogSeverity) -> &'static str {
    match severity {
        logtail::LogSeverity::Warn => "warn",
        logtail::LogSeverity::Error => "error",
        logtail::LogSeverity::Panic => "panic",
    }
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
    selected: MuxName,
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
            is_current: group.mux == selected && group.session_name == ws.session_name,
            mux: group.mux,
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
    mux: MuxName,
    session_name: String,
    sidebar_count: usize,
    pane_ids: Vec<String>,
}

fn duplicate_sidebar_session_groups(
    heartbeats: &[rimz::sidebar::heartbeat::SidebarHeartbeat],
) -> Vec<SidebarSessionGroup> {
    let mut by_session: BTreeMap<
        (MuxName, String),
        Vec<&rimz::sidebar::heartbeat::SidebarHeartbeat>,
    > = BTreeMap::new();
    for heartbeat in heartbeats {
        by_session
            .entry((heartbeat.mux, heartbeat.session_name.clone()))
            .or_default()
            .push(heartbeat);
    }
    if by_session.len() < 2 {
        return Vec::new();
    }
    by_session
        .into_iter()
        .map(|((mux, session_name), mut heartbeats)| {
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
                mux,
                session_name,
                sidebar_count,
                pane_ids,
            }
        })
        .collect()
}

pub(super) fn fresh_sidebar_heartbeats_for_doctor(
    runtime: &RuntimePaths,
) -> std::io::Result<Vec<rimz::sidebar::heartbeat::SidebarHeartbeat>> {
    let current = rimz::sidebar::heartbeat::read_current_heartbeats(&runtime.heartbeat_dir)?;
    let mut heartbeats = Vec::new();
    for (path, heartbeat) in current {
        if !heartbeat_mtime_is_fresh(&path) {
            continue;
        }
        if heartbeat.workspace_id != runtime.workspace_id {
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
/// backend's presence channel pokes. Zellij treats a missing plugin as a
/// failed precondition; tmux names whether polling is expected for the current
/// sidebar state.
fn collect_presence(
    ws: &rimz::ResolvedWorkspace,
    mux: MuxName,
    ownership: &rimz::room::session::RoomOwnership,
) -> model::Presence {
    use rimz::sidebar::cache::read_presence_stamp;

    if !matches!(
        ownership.selected_state(),
        rimz::room::session::BackendRoomState::Live
    ) {
        return model::Presence::NotApplicable {
            reason: presence_not_applicable_reason(mux, ownership),
        };
    }

    let runtime = match RuntimePaths::for_workspace(ws.workspace_id.clone()) {
        Ok(runtime) => runtime,
        Err(err) => {
            return model::Presence::Unavailable {
                error: err.to_string(),
            };
        }
    };
    let stamp = read_presence_stamp(&runtime);
    let age = stamp
        .as_ref()
        .map(|stamp| rimz::sidebar::timing::unix_now_ms().saturating_sub(stamp.written_at_ms));
    if presence_stamp_is_event(stamp.as_ref(), age, mux, &ws.session_name, ownership) {
        return model::Presence::Event {
            poked_secs: age.unwrap_or(0) / 1000,
        };
    }
    if mux == MuxName::Tmux {
        let sidebar_running = fresh_sidebar_heartbeats_for_doctor(&runtime)
            .map(|heartbeats| !heartbeats.is_empty())
            .unwrap_or(true);
        let watch_attached = tmux_watch_client_attached(&ws.session_name);
        return tmux_poll_presence(age, sidebar_running, watch_attached);
    }
    if zellij_mod::presence_plugin_path().is_none() {
        return model::Presence::Unavailable {
            error: "embedded plugin unavailable or could not materialize; reinstall rimz or use the tmux backend"
                .to_owned(),
        };
    }
    let meets_floor = zellij_mod::capabilities().is_ok_and(|caps| caps.meets_min_version);
    if !meets_floor {
        let (maj, min, patch) = zellij_mod::MIN_ZELLIJ_VERSION;
        return model::Presence::Unavailable {
            error: format!(
                "zellij below the RimZ floor; upgrade to >= {maj}.{min}.{patch} or use the tmux backend"
            ),
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
    model::Presence::Unavailable { error: reason }
}

fn presence_not_applicable_reason(
    selected: MuxName,
    ownership: &rimz::room::session::RoomOwnership,
) -> String {
    if let Some(owner) = ownership
        .live_on()
        .into_iter()
        .find(|owner| *owner != selected)
    {
        return format!("this workspace room is live on {owner}, not {selected}");
    }
    match ownership.selected_state() {
        rimz::room::session::BackendRoomState::Exited => {
            format!("this workspace room is exited on {selected}")
        }
        rimz::room::session::BackendRoomState::Absent => {
            format!("this workspace room is absent on {selected}")
        }
        rimz::room::session::BackendRoomState::Unavailable { error } => {
            format!("{selected} room probe unavailable: {error}")
        }
        rimz::room::session::BackendRoomState::Live => {
            unreachable!("caller handles live selected rooms")
        }
    }
}

fn presence_stamp_is_event(
    stamp: Option<&rimz::sidebar::cache::PresenceStamp>,
    age_ms: Option<u64>,
    selected: MuxName,
    session_name: &str,
    ownership: &rimz::room::session::RoomOwnership,
) -> bool {
    if !rimz::sidebar::cache::presence_event_mode(age_ms) {
        return false;
    }
    let Some(stamp) = stamp else {
        return false;
    };
    match stamp.mux {
        Some(mux) => {
            mux == selected
                && stamp
                    .session_name
                    .as_deref()
                    .is_none_or(|stamp_session| stamp_session == session_name)
        }
        None => ownership.legacy_stamp_acceptable(),
    }
}

fn tmux_watch_client_attached(session: &str) -> bool {
    let output = rimz::mux::CommandSpec::new("tmux")
        .args(["list-clients", "-t", session, "-F", "#{client_flags}"])
        .run()
        .ok();
    let Some(output) = output else {
        return false;
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| line.split(',').any(|flag| flag.trim() == "ignore-size"))
}

/// The tmux polling verdict once the presence stamp is not fresh: expected
/// while no sidebar producer runs (the watch starts with the sidebar) or the
/// watch is attached but idle (no keepalive; events re-arm on activity), a
/// warning only while a sidebar runs without a watch client.
fn tmux_poll_presence(
    stamp_age_ms: Option<u64>,
    sidebar_running: bool,
    watch_attached: bool,
) -> model::Presence {
    if watch_attached {
        let age = stamp_age_ms
            .map(|age| format!(", last poke {}s ago", age / 1000))
            .unwrap_or_default();
        return model::Presence::Poll {
            reason: format!(
                "live tmux watch attached, session idle{age}; events resume on activity"
            ),
            expected: true,
        };
    }

    if !sidebar_running {
        return model::Presence::Poll {
            reason: "no sidebar running in this workspace; the live pane watch starts with the sidebar (`rimz start`)"
                .to_owned(),
            expected: true,
        };
    }

    model::Presence::Poll {
        reason:
            "sidebar running but the live tmux watch is not attached; reattach or run `rimz reload`"
                .to_owned(),
        expected: false,
    }
}

fn collect_topology_writer(ws: &rimz::ResolvedWorkspace) -> Option<model::TopologyWriterHealth> {
    let recorded_bin = StatePaths::for_workspace(ws.workspace_id.clone())
        .ok()
        .and_then(|state| rimz::store::workspace_record::read(&state.workspace_record).ok())
        .and_then(|record| record.rimz_bin)
        .map(|path| {
            let exists = path.is_file();
            model::RecordedRoomBin {
                path: path.display().to_string(),
                exists,
                fix: (!exists).then(|| "run `rimz reload`".to_owned()),
            }
        });
    let conflict = RuntimePaths::for_workspace(ws.workspace_id.clone())
        .ok()
        .and_then(|runtime| {
            let now_ms = rimz::sidebar::timing::unix_now_ms();
            let cache_writer =
                rimz::sidebar::cache::read_pane_topology_cache(&runtime, &ws.session_name)
                    .and_then(|cache| cache.writer);
            fresh_topology_writer_conflict(&runtime, cache_writer.as_ref(), now_ms).map(
                |conflict| model::TopologyWriterConflict {
                    stale: conflict.stale_writer.map(topology_writer_id),
                    accepted: conflict
                        .accepted_writer
                        .or(cache_writer)
                        .map(topology_writer_id),
                    rejected_count: conflict.rejected_count,
                    age_secs: now_ms.saturating_sub(conflict.last_ms) / 1000,
                    fix: "run `rimz reload`".to_owned(),
                },
            )
        });
    (recorded_bin.is_some() || conflict.is_some()).then_some(model::TopologyWriterHealth {
        recorded_bin,
        conflict,
    })
}

fn fresh_topology_writer_conflict(
    runtime: &RuntimePaths,
    cache_writer: Option<&rimz::mux::zellij::pane_topology::TopologyWriter>,
    now_ms: u64,
) -> Option<rimz::sidebar::presence::TopologyWriterConflict> {
    let conflict = rimz::sidebar::presence::read_topology_writer_conflict(runtime)?;
    if topology_conflict_superseded(cache_writer, conflict.accepted_writer.as_ref())
        || now_ms.saturating_sub(conflict.last_ms) > TOPOLOGY_CONFLICT_FRESH_MS
    {
        return None;
    }
    Some(conflict)
}

fn topology_conflict_superseded(
    cache_writer: Option<&rimz::mux::zellij::pane_topology::TopologyWriter>,
    accepted_writer: Option<&rimz::mux::zellij::pane_topology::TopologyWriter>,
) -> bool {
    let generation = |writer: Option<&rimz::mux::zellij::pane_topology::TopologyWriter>| {
        writer.map_or((0, 0), |writer| writer.generation())
    };
    generation(cache_writer) > generation(accepted_writer)
}

fn topology_writer_id(
    writer: rimz::mux::zellij::pane_topology::TopologyWriter,
) -> model::TopologyWriterId {
    model::TopologyWriterId {
        plugin_id: writer.plugin_id,
        loaded_at_ms: writer.loaded_at_ms,
    }
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
    if !config.enabled_for("claude") && !config.enabled_for("codex") {
        return model::RemoteControl::Off;
    }

    let readiness = rimz::remote_control::ReadinessSnapshot::probe(&config);
    let advisories = rimz::remote_control::advisories(&config);
    let mut agents = Vec::new();
    if config.enabled_for("claude") {
        let (detail, ready) = match readiness
            .for_host(rimz::remote_control::RemoteControlHost::Claude)
        {
            rimz::remote_control::HostState::Ready => ("ready".to_owned(), true),
            rimz::remote_control::HostState::Uninstalled(_) => {
                ("enabled, not on PATH".to_owned(), false)
            }
            rimz::remote_control::HostState::Blocked(_) => ("enabled, blocked".to_owned(), false),
            rimz::remote_control::HostState::Disabled => ("ready".to_owned(), true),
        };
        agents.push(model::RemoteAgent {
            kind: "claude",
            detail,
            ready,
        });
    }
    if config.enabled_for("codex") {
        let (detail, ready) =
            match readiness.for_host(rimz::remote_control::RemoteControlHost::Codex) {
                rimz::remote_control::HostState::Uninstalled(_) => {
                    ("enabled, standalone install missing".to_owned(), false)
                }
                rimz::remote_control::HostState::Ready
                | rimz::remote_control::HostState::Disabled => ("ready".to_owned(), true),
                rimz::remote_control::HostState::Blocked(_) => {
                    ("enabled, standalone install missing".to_owned(), false)
                }
            };
        agents.push(model::RemoteAgent {
            kind: "codex",
            detail,
            ready,
        });
    }

    let skipped = match readiness.for_host(rimz::remote_control::RemoteControlHost::Codex) {
        rimz::remote_control::HostState::Uninstalled(issue) => vec![issue.to_string()],
        _ => Vec::new(),
    };
    let refusals = match readiness.for_host(rimz::remote_control::RemoteControlHost::Claude) {
        rimz::remote_control::HostState::Blocked(issue) => vec![issue.to_string()],
        _ => Vec::new(),
    };
    model::RemoteControl::On {
        agents,
        refusals,
        skipped,
        advisories,
    }
}

pub(super) fn collect_socket_headroom(
    ws: &rimz::ResolvedWorkspace,
) -> model::Probe<model::SockBudget> {
    let runtime =
        match RuntimePaths::under(ws.workspace_id.clone(), &rimz::store::paths::runtime_home()) {
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

pub(super) fn collect_diagnostics(
    ws: &rimz::ResolvedWorkspace,
    cleared_at: Option<jiff::Timestamp>,
) -> model::Diagnostics {
    const RECENT_DIAG_INCIDENTS: usize = 12;
    let Some((path, records)) = rimz::diag::recent_records(ws.workspace_id.clone(), usize::MAX)
    else {
        return model::Diagnostics::Unavailable;
    };
    let cleared_at_ms = cleared_at.and_then(|at| u64::try_from(at.as_millisecond()).ok());
    let incidents = diagnostic_incidents(records, RECENT_DIAG_INCIDENTS, cleared_at_ms);
    model::Diagnostics::Ready {
        path: path.display().to_string(),
        incidents,
    }
}

fn diagnostic_incidents(
    mut records: Vec<rimz::diag::record::DiagEnvelope>,
    limit: usize,
    cleared_at_ms: Option<u64>,
) -> Vec<model::DiagIncident> {
    const EPISODE_GAP_MS: u64 = 60_000;
    records.retain(|record| cleared_at_ms.is_none_or(|cleared_at| record.at_ms > cleared_at));
    records.sort_by_key(|record| record.at_ms);

    let mut builders = Vec::<IncidentBuilder>::new();
    let mut latest_by_key = HashMap::<String, usize>::new();
    for record in records {
        let (key, exact_frame) = incident_key(&record);
        let existing = latest_by_key.get(&key).copied().filter(|index| {
            exact_frame
                || record.at_ms.saturating_sub(builders[*index].last_at_ms) <= EPISODE_GAP_MS
        });
        match existing {
            Some(index) => builders[index].merge(record),
            None => {
                let index = builders.len();
                builders.push(IncidentBuilder::new(record));
                latest_by_key.insert(key, index);
            }
        }
    }
    builders.sort_by_key(|incident| incident.last_at_ms);
    if builders.len() > limit {
        builders.drain(..builders.len() - limit);
    }
    builders.into_iter().map(IncidentBuilder::finish).collect()
}

struct IncidentBuilder {
    kind: String,
    source_severity: rimz::diag::record::DiagSeverity,
    state: model::DoctorState,
    impact: model::DoctorImpact,
    first_at_ms: u64,
    last_at_ms: u64,
    record_count: usize,
    observer_ids: BTreeSet<String>,
    sink_suppressed: u64,
    observer_suppressed: u64,
    dropped_messages: u64,
    summary: String,
    build: Option<String>,
    evidence_refs: BTreeSet<String>,
}

impl IncidentBuilder {
    fn new(record: rimz::diag::record::DiagEnvelope) -> Self {
        let at_ms = record.at_ms;
        let mut incident = Self {
            kind: incident_kind(&record.event).to_owned(),
            source_severity: record.severity,
            state: model::DoctorState::Investigate,
            impact: model::DoctorImpact::Warn,
            first_at_ms: at_ms,
            last_at_ms: at_ms,
            record_count: 0,
            observer_ids: BTreeSet::new(),
            sink_suppressed: 0,
            observer_suppressed: 0,
            dropped_messages: 0,
            summary: String::new(),
            build: record.build.clone(),
            evidence_refs: BTreeSet::new(),
        };
        incident.merge(record);
        incident
    }

    fn merge(&mut self, record: rimz::diag::record::DiagEnvelope) {
        self.source_severity = max_severity(self.source_severity, record.severity);
        (self.state, self.impact) = classify_diagnostic(&record.event, self.source_severity);
        self.last_at_ms = self.last_at_ms.max(record.at_ms);
        self.record_count = self.record_count.saturating_add(1);
        if let Some(instance_id) = &record.instance_id {
            self.observer_ids.insert(instance_id.as_str().to_owned());
        }
        self.sink_suppressed = self
            .sink_suppressed
            .saturating_add(u64::from(record.suppressed_since_last));
        if let rimz::diag::record::DiagEvent::FrameAnomaly {
            suppressed_since_last,
            dropped_msgs,
            ..
        } = &record.event
        {
            self.observer_suppressed = self
                .observer_suppressed
                .saturating_add(u64::from(*suppressed_since_last));
            self.dropped_messages = self
                .dropped_messages
                .saturating_add(u64::from(*dropped_msgs));
        }
        self.summary = diagnostic_summary(&record.event);
        self.evidence_refs
            .extend(diagnostic_evidence_refs(&record.event));
    }

    fn finish(self) -> model::DiagIncident {
        let stale_build = stale_build(self.build.as_deref(), rimz::build_id::current());
        let observer_ids = self.observer_ids.into_iter().collect::<Vec<_>>();
        model::DiagIncident {
            kind: self.kind,
            source_severity: self.source_severity,
            state: self.state,
            impact: self.impact,
            first_at_ms: self.first_at_ms,
            last_at_ms: self.last_at_ms,
            record_count: self.record_count,
            distinct_observer_count: observer_ids.len(),
            observer_ids,
            sink_suppressed: self.sink_suppressed,
            observer_suppressed: self.observer_suppressed,
            dropped_messages: self.dropped_messages,
            summary: self.summary,
            build: self.build,
            stale_build,
            evidence_refs: self.evidence_refs.into_iter().collect(),
        }
    }
}

fn incident_key(record: &rimz::diag::record::DiagEnvelope) -> (String, bool) {
    let build = record.build.as_deref().unwrap_or("unknown");
    if let rimz::diag::record::DiagEvent::FrameAnomaly {
        frame:
            rimz::diag::record::FrameStamp {
                produced_at_ms: Some(produced_at_ms),
                ..
            },
        ..
    } = &record.event
    {
        return (
            format!(
                "{}:{build}:{}:{produced_at_ms}",
                record.session_name,
                record.event.identity_key(),
            ),
            true,
        );
    }
    (
        format!(
            "{}:{build}:{}",
            record.session_name,
            normalized_incident_identity(&record.event)
        ),
        false,
    )
}

fn normalized_incident_identity(event: &rimz::diag::record::DiagEvent) -> String {
    use rimz::diag::record::DiagEvent;
    match event {
        DiagEvent::GateHold { rule, .. } | DiagEvent::GateRelease { rule, .. } => {
            format!("gate:{rule:?}")
        }
        DiagEvent::PaneCarryForward { carried, .. }
        | DiagEvent::PaneCarryRefuted { carried, .. } => format!("pane_carry:{carried:?}"),
        DiagEvent::FrameRejected { .. } | DiagEvent::FrameShrinkVerified { .. } => {
            "frame_shrink".to_owned()
        }
        DiagEvent::HealthAlert {
            reason, since_ms, ..
        } => format!("health:{reason}:{since_ms}"),
        DiagEvent::LinkAlert { since_ms, .. } => format!("link:{since_ms}"),
        DiagEvent::TickBudgetBreach {
            tick_loop,
            since_ms,
            ..
        } => format!("tick:{tick_loop:?}:{since_ms}"),
        DiagEvent::TopologyWriteRejected {
            accepted_plugin_id,
            accepted_loaded_at_ms,
            ..
        }
        | DiagEvent::TopologyWriterChanged {
            plugin_id: accepted_plugin_id,
            loaded_at_ms: accepted_loaded_at_ms,
            ..
        } => format!("topology_writer:{accepted_loaded_at_ms}:{accepted_plugin_id}"),
        _ => event.identity_key(),
    }
}

fn incident_kind(event: &rimz::diag::record::DiagEvent) -> &'static str {
    use rimz::diag::record::DiagEvent;
    match event {
        DiagEvent::GateHold { .. } | DiagEvent::GateRelease { .. } => "gate",
        DiagEvent::PaneCarryForward { .. } | DiagEvent::PaneCarryRefuted { .. } => "pane_carry",
        DiagEvent::FrameRejected { .. } | DiagEvent::FrameShrinkVerified { .. } => "frame_shrink",
        _ => event.kind_name(),
    }
}

fn classify_diagnostic(
    event: &rimz::diag::record::DiagEvent,
    severity: rimz::diag::record::DiagSeverity,
) -> (model::DoctorState, model::DoctorImpact) {
    use rimz::diag::record::{DiagEvent, HostedCarryDropReason, RendererExitCause};
    let state = match event {
        DiagEvent::FrameRejected { .. }
        | DiagEvent::PaneCarryForward { .. }
        | DiagEvent::GateHold { .. }
        | DiagEvent::TopologyWriteRejected { .. }
        | DiagEvent::NewbornQuarantined { .. }
        | DiagEvent::ClientReaped { settled: true, .. } => model::DoctorState::Contained,
        DiagEvent::FrameShrinkVerified { .. }
        | DiagEvent::PaneCarryRefuted { .. }
        | DiagEvent::GateRelease { .. }
        | DiagEvent::TopologyWriterChanged { .. }
        | DiagEvent::HealthAlert {
            recovered_after_ms: Some(_),
            ..
        }
        | DiagEvent::LinkAlert {
            recovered_after_ms: Some(_),
            ..
        }
        | DiagEvent::TickBudgetBreach {
            recovered_after_ms: Some(_),
            ..
        } => model::DoctorState::Recovered,
        DiagEvent::PaneCountDrop {
            evidence: Some(evidence),
            ..
        } if pane_drop_is_expected(evidence) => model::DoctorState::Expected,
        DiagEvent::RendererExit {
            cause: RendererExitCause::SelfCloseEmptyTab,
        }
        | DiagEvent::HostedCarryDropped {
            reason: HostedCarryDropReason::ProbeReportsAbsent | HostedCarryDropReason::CarryExpired,
            ..
        }
        | DiagEvent::SidebarWidthIntent { .. }
        | DiagEvent::SidebarWidthNudge { .. }
        | DiagEvent::SidebarWidthSettle { .. }
        | DiagEvent::FetchFoldStats { .. }
        | DiagEvent::ProducerElected { .. }
        | DiagEvent::ProducerDemoted { .. }
        | DiagEvent::GroupMigration { .. } => model::DoctorState::Expected,
        DiagEvent::ResolutionFallback { .. }
        | DiagEvent::PaneCountDrop { .. }
        | DiagEvent::CarryForwardExpired { .. }
        | DiagEvent::HostedCarryDropped {
            reason:
                HostedCarryDropReason::StartRegressed | HostedCarryDropReason::ForegroundKindMismatch,
            ..
        }
        | DiagEvent::FetchFailure { .. }
        | DiagEvent::HealthAlert {
            recovered_after_ms: None,
            ..
        }
        | DiagEvent::LinkAlert {
            recovered_after_ms: None,
            ..
        }
        | DiagEvent::ClientReaped { settled: false, .. }
        | DiagEvent::TickBudgetBreach {
            recovered_after_ms: None,
            ..
        }
        | DiagEvent::RowConflict { .. }
        | DiagEvent::DuplicatePaneId { .. }
        | DiagEvent::ForeignSessionPane { .. }
        | DiagEvent::MixedBuildWriters { .. }
        | DiagEvent::RendererPanic { .. }
        | DiagEvent::RendererSignalDeath { .. }
        | DiagEvent::RendererOrphanReaped { .. }
        | DiagEvent::SidebarOrphanReaped { .. }
        | DiagEvent::PaneCacheDivergence { .. }
        | DiagEvent::SupervisorConvergence { .. }
        | DiagEvent::SupervisorPreflightRejected { .. }
        | DiagEvent::SelfCloseRejected { .. }
        | DiagEvent::RendererExit {
            cause: RendererExitCause::DegradedGaveUp,
        }
        | DiagEvent::FrameAnomaly { .. } => model::DoctorState::Investigate,
    };
    let impact = if state == model::DoctorState::Investigate {
        match severity {
            rimz::diag::record::DiagSeverity::Error => model::DoctorImpact::Alarm,
            rimz::diag::record::DiagSeverity::Warn | rimz::diag::record::DiagSeverity::Info => {
                model::DoctorImpact::Warn
            }
        }
    } else {
        model::DoctorImpact::Info
    };
    (state, impact)
}

fn pane_drop_is_expected(evidence: &rimz::diag::record::PaneDropEvidence) -> bool {
    !evidence.mass_shrink
        && evidence.affected_views.len() == 1
        && evidence.affected_views[0].removed_completely()
        && evidence.affected_views[0].managed_panes.is_empty()
}

fn diagnostic_evidence_refs(event: &rimz::diag::record::DiagEvent) -> Vec<String> {
    use rimz::diag::record::DiagEvent;
    match event {
        DiagEvent::FrameRejected { frames_ref, .. }
        | DiagEvent::PaneCountDrop { frames_ref, .. }
        | DiagEvent::PaneCarryForward { frames_ref, .. }
        | DiagEvent::PaneCarryRefuted { frames_ref, .. } => frames_ref.iter().cloned().collect(),
        _ => Vec::new(),
    }
}

fn max_severity(
    left: rimz::diag::record::DiagSeverity,
    right: rimz::diag::record::DiagSeverity,
) -> rimz::diag::record::DiagSeverity {
    use rimz::diag::record::DiagSeverity;
    match (left, right) {
        (DiagSeverity::Error, _) | (_, DiagSeverity::Error) => DiagSeverity::Error,
        (DiagSeverity::Warn, _) | (_, DiagSeverity::Warn) => DiagSeverity::Warn,
        _ => DiagSeverity::Info,
    }
}

fn stale_build(record_build: Option<&str>, current_build: Option<&str>) -> bool {
    matches!((record_build, current_build), (Some(record), Some(current)) if record != current)
}

fn diagnostic_summary(event: &rimz::diag::record::DiagEvent) -> String {
    use rimz::diag::record::DiagEvent;
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
        DiagEvent::ResolutionFallback { reason } => {
            format!("resolution snapshot fell back to rollup: {reason}")
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
        DiagEvent::HostedCarryDropped {
            pane_id,
            agent_kind,
            reason,
        } => format!(
            "dropped hosted {agent_kind} carry for {pane_id}: {}",
            reason.as_str()
        ),
        DiagEvent::TopologyWriterChanged {
            prior_plugin_id,
            prior_loaded_at_ms,
            plugin_id,
            loaded_at_ms,
        } => format!(
            "topology writer changed {prior_loaded_at_ms}:{prior_plugin_id}->{loaded_at_ms}:{plugin_id}"
        ),
        DiagEvent::TopologyWriteRejected {
            plugin_id,
            loaded_at_ms,
            accepted_plugin_id,
            accepted_loaded_at_ms,
            rejected_count,
        } => format!(
            "rejected topology writer {loaded_at_ms}:{plugin_id}; accepted {accepted_loaded_at_ms}:{accepted_plugin_id}; count {rejected_count}"
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
        DiagEvent::ClientReaped {
            killed_pids,
            pre_clients,
            post_clients,
            settled,
            timed_out,
            errors,
        } => format!(
            "remote Zellij client reap pids {killed_pids:?}; clients {pre_clients:?}->{post_clients:?}; settled={settled}; timed_out={timed_out}{}",
            if errors.is_empty() {
                String::new()
            } else {
                format!("; {}", errors.join("; "))
            }
        ),
        DiagEvent::SidebarWidthIntent {
            trigger,
            own_cols,
            base_cols,
            step_cols,
            step_exact,
            target_cols,
            verdict,
        } => format!(
            "sidebar width {}: own {own_cols}, base {base_cols}, step {step_cols:?} (exact={step_exact}), target {target_cols:?}; {}",
            trigger.as_str(),
            verdict.as_str(),
        ),
        DiagEvent::SidebarWidthNudge {
            trigger,
            from_cols,
            target_cols,
        } => format!(
            "sidebar width nudge ({}) {from_cols}->{target_cols}",
            trigger.as_str()
        ),
        DiagEvent::SidebarWidthSettle {
            settled_cols,
            learned_step,
            outcome,
        } => format!(
            "sidebar width settled at {settled_cols}; learned step {learned_step:?}; {}",
            outcome.as_str()
        ),
        DiagEvent::TickBudgetBreach {
            tick_loop,
            over_ticks,
            last_wall_ms,
            last_mux_wait_ms,
            last_fold_bytes,
            last_spawns,
            wall_ms,
            mux_wait_ms,
            fold_bytes,
            spawns,
            budget_wall_ms,
            budget_mux_wait_ms,
            budget_fold_bytes,
            budget_spawns,
            recovered_after_ms,
            ..
        } => {
            let last = format!(
                "last {last_wall_ms}ms ({last_mux_wait_ms}ms mux)/{last_fold_bytes}B/{last_spawns} spawns"
            );
            let worst =
                format!("worst {wall_ms}ms ({mux_wait_ms}ms mux)/{fold_bytes}B/{spawns} spawns");
            let budget = format!(
                "budget {budget_wall_ms}ms in-process/{budget_mux_wait_ms}ms mux/{budget_fold_bytes}B/{budget_spawns} spawns"
            );
            match recovered_after_ms {
                Some(ms) => {
                    format!(
                        "{tick_loop:?} tick recovered after {ms}ms; {over_ticks} over ticks; {last}; {worst}; {budget}"
                    )
                }
                None => {
                    format!(
                        "{tick_loop:?} tick over budget for {over_ticks} ticks; {last}; {worst}; {budget}"
                    )
                }
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
        DiagEvent::RendererSignalDeath {
            signal,
            exit_code,
            stderr_excerpt,
        } => {
            let reason = match (signal, exit_code) {
                (Some(signal), _) => format!("signal {signal}"),
                (None, Some(code)) => format!("exit {code}"),
                (None, None) => "unknown termination".to_owned(),
            };
            let excerpt = stderr_excerpt.lines().last().unwrap_or(stderr_excerpt);
            format!("render worker died by {reason}: {excerpt}")
        }
        DiagEvent::RendererOrphanReaped {
            pane_id,
            worker_pid,
        } => format!("reaped orphaned renderer {worker_pid} after pane {pane_id} disappeared"),
        DiagEvent::SidebarOrphanReaped {
            pane_id,
            pid,
            first_confirmed_at_ms,
            second_confirmed_at_ms,
            sigkilled,
        } => format!(
            "reaped orphaned sidebar {pid} after pane {pane_id} was absent at {first_confirmed_at_ms} and {second_confirmed_at_ms}; sigkill={sigkilled}"
        ),
        DiagEvent::PaneCacheDivergence {
            pane_id,
            pid,
            cache_observed_at_ms,
            authoritative_observed_at_ms,
        } => format!(
            "pane cache at {cache_observed_at_ms:?} omitted live sidebar {pid} in {pane_id}; authoritative roster observed it at {authoritative_observed_at_ms}"
        ),
        DiagEvent::SupervisorConvergence { target_build } => {
            format!("supervisor converging onto build {target_build}")
        }
        DiagEvent::SupervisorPreflightRejected {
            target_build,
            reason,
        } => format!("supervisor rejected build {target_build}: {reason}"),
        DiagEvent::SelfCloseRejected { siblings, reason } => {
            format!("self-close rejected ({siblings} siblings): {reason}")
        }
        DiagEvent::RendererExit { cause } => format!("renderer exited: {}", cause.as_str()),
        DiagEvent::FetchFoldStats {
            interval_ms,
            causes,
        } => format!(
            "fetch fold totals over {interval_ms}ms across {} causes",
            causes.len()
        ),
        DiagEvent::FrameAnomaly {
            anomaly:
                rimz::diag::record::AnomalyKind::RowPresenceFlap {
                    row_id,
                    gone_at_ms,
                    back_at_ms,
                    gap_evidence: Some(evidence),
                    ..
                },
            suppressed_since_last,
            ..
        } => {
            let pulled_pane = evidence
                .pulled_pane_present
                .map(|present| present.to_string())
                .unwrap_or_else(|| "unknown".to_owned());
            let suppressed = if *suppressed_since_last > 0 {
                format!("; {suppressed_since_last} suppressed")
            } else {
                String::new()
            };
            format!(
                "observed row_presence_flap on {row_id}; gap {}ms; pulled row present={}; pulled pane present={pulled_pane}{suppressed}",
                back_at_ms.saturating_sub(*gone_at_ms),
                evidence.pulled_row_present,
            )
        }
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

#[cfg(test)]
mod tests;
