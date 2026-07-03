use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::SystemTime;

use rimz::RuntimePaths;
use rimz::config::{ColorDepth, MachineConfig, ThemeMode};
use rimz::ids::MuxName;
use rimz::mux::{
    MuxBackend, SessionHealth, binaries, logtail,
    tmux::{self as tmux_mod, MIN_TMUX_VERSION},
    zellij::{self as zellij_mod, MIN_ZELLIJ_VERSION},
};

use super::model;

const MUX_LOG_WINDOW_BYTES: u64 = 256 * 1024;
const MUX_LOG_ENTRY_CAP: usize = 10;

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
    let binaries = collect_mux_binaries(mux);
    let log = collect_mux_log(mux);
    let mut report = model::Mux {
        name: mux,
        version,
        capabilities,
        binaries,
        log,
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
                Ok(true) => scan_mux_log(path, zellij_mod::classify_log_line),
                Ok(false) => model::MuxLog::Missing {
                    path: path.display().to_string(),
                },
                Err(err) => model::MuxLog::Unavailable {
                    error: format!("{}: {err}", path.display()),
                },
            }
        }
        MuxName::Tmux => match tmux_mod::server_log_file() {
            Some(path) => scan_mux_log(path, tmux_mod::classify_log_line),
            None => model::MuxLog::Disabled {
                hint: "server logging off (start tmux with `-v` to enable)".to_owned(),
            },
        },
    }
}

fn scan_mux_log(
    path: std::path::PathBuf,
    classify: fn(&str) -> Option<logtail::LogSeverity>,
) -> model::MuxLog {
    match logtail::scan_tail(&path, MUX_LOG_WINDOW_BYTES, MUX_LOG_ENTRY_CAP, classify) {
        Ok(scan) => model::MuxLog::Ready {
            path: path.display().to_string(),
            size_bytes: scan.size_bytes,
            scanned_bytes: scan.scanned_bytes,
            matched: scan.matched,
            entries: scan
                .entries
                .into_iter()
                .map(|entry| model::MuxLogEntry {
                    severity: severity_label(entry.severity).to_owned(),
                    line: entry.line,
                })
                .collect(),
        },
        Err(err) => model::MuxLog::Unavailable {
            error: format!("{}: {err}", path.display()),
        },
    }
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
    heartbeats: &[rimz::sidebar::heartbeat::SidebarHeartbeat],
) -> Vec<SidebarSessionGroup> {
    let mut by_session: BTreeMap<String, Vec<&rimz::sidebar::heartbeat::SidebarHeartbeat>> =
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

pub(super) fn collect_diagnostics(ws: &rimz::ResolvedWorkspace) -> model::Diagnostics {
    const RECENT_DIAG_ROWS: usize = 12;
    let Some((path, records)) = rimz::diag::recent_records(ws.workspace_id.clone(), usize::MAX)
    else {
        return model::Diagnostics::Unavailable;
    };
    let records = diagnostic_rows(records, RECENT_DIAG_ROWS);
    model::Diagnostics::Ready {
        path: path.display().to_string(),
        records,
    }
}

fn diagnostic_rows(
    records: Vec<rimz::diag::record::DiagEnvelope>,
    limit: usize,
) -> Vec<model::DiagRow> {
    let mut groups: Vec<(String, rimz::diag::record::DiagEnvelope, usize)> = Vec::new();
    for record in records {
        let key = record.event.identity_key();
        match groups.last_mut() {
            Some((last_key, latest, count)) if last_key == &key => {
                *latest = record;
                *count = count.saturating_add(1);
            }
            _ => groups.push((key, record, 1)),
        }
    }
    if groups.len() > limit {
        groups.drain(..groups.len() - limit);
    }
    groups
        .into_iter()
        .map(|(_, record, count)| diagnostic_row(record, count))
        .collect()
}

fn diagnostic_row(record: rimz::diag::record::DiagEnvelope, count: usize) -> model::DiagRow {
    let summary = summary_with_record_count(
        summary_with_suppressed(
            diagnostic_summary(&record.event),
            record.suppressed_since_last,
        ),
        count,
    );
    model::DiagRow {
        severity: record.severity,
        kind: record.event.kind_name().to_owned(),
        at_ms: record.at_ms,
        summary,
    }
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

fn summary_with_record_count(summary: String, count: usize) -> String {
    if count <= 1 {
        summary
    } else {
        format!("{summary}; {count} records")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::diag::record::{DiagEnvelope, DiagEvent, FrameRejectReason, TickLoop};

    fn sidebar(raw: &str) -> rimz::SidebarInstanceId {
        rimz::SidebarInstanceId::parse(raw).expect("valid sidebar id")
    }

    fn heartbeat(
        session_name: &str,
        instance_id: &str,
        pane: Option<&str>,
    ) -> rimz::sidebar::heartbeat::SidebarHeartbeat {
        rimz::sidebar::heartbeat::SidebarHeartbeat::new(
            rimz::WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
            sidebar(instance_id),
            rimz::MuxName::Zellij,
            session_name,
            "/tmp/sidebar.sock".into(),
            pane.map(|pane| rimz::PaneId::parse(pane).unwrap()),
        )
    }

    fn diag_record(at_ms: u64, event: DiagEvent) -> DiagEnvelope {
        DiagEnvelope::new(
            rimz::WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
            "rimz-test".to_owned(),
            None,
            at_ms,
            event,
        )
    }

    fn tick_breach(since_ms: u64, recovered_after_ms: Option<u64>, over_ticks: u32) -> DiagEvent {
        DiagEvent::TickBudgetBreach {
            tick_loop: TickLoop::Fetch,
            over_ticks,
            last_wall_ms: 1_100,
            last_mux_wait_ms: 0,
            last_fold_bytes: 0,
            last_spawns: 0,
            wall_ms: 1_500,
            mux_wait_ms: 0,
            fold_bytes: 0,
            spawns: 0,
            budget_wall_ms: 1_000,
            budget_mux_wait_ms: 5_000,
            budget_fold_bytes: 262_144,
            budget_spawns: 32,
            since_ms,
            recovered_after_ms,
        }
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

        let tick = diagnostic_summary(&DiagEvent::TickBudgetBreach {
            tick_loop: TickLoop::Fetch,
            over_ticks: 5,
            last_wall_ms: 900,
            last_mux_wait_ms: 250,
            last_fold_bytes: 1_024,
            last_spawns: 1,
            wall_ms: 1_500,
            mux_wait_ms: 900,
            fold_bytes: 300_000,
            spawns: 40,
            budget_wall_ms: 1_000,
            budget_mux_wait_ms: 5_000,
            budget_fold_bytes: 262_144,
            budget_spawns: 32,
            since_ms: 10,
            recovered_after_ms: None,
        });
        assert!(tick.contains("last 900ms (250ms mux)/1024B/1 spawns"));
        assert!(tick.contains("worst 1500ms (900ms mux)/300000B/40 spawns"));
    }

    #[test]
    fn diagnostic_rows_collapse_consecutive_same_identity_records() {
        let rows = diagnostic_rows(
            vec![
                diag_record(1, tick_breach(10, None, 5)),
                diag_record(2, tick_breach(10, None, 6)),
            ],
            12,
        );

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].at_ms, 2);
        assert!(rows[0].summary.contains("over budget for 6 ticks"));
        assert!(rows[0].summary.ends_with("; 2 records"));
    }

    #[test]
    fn diagnostic_rows_group_before_recent_cap_and_keep_distinct_identities() {
        let mut records = Vec::new();
        for at_ms in 1..=13 {
            records.push(diag_record(at_ms, tick_breach(10, None, at_ms as u32)));
        }
        records.push(diag_record(20, tick_breach(10, Some(10), 13)));
        records.push(diag_record(21, tick_breach(30, None, 5)));

        let rows = diagnostic_rows(records, 12);

        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].at_ms, 13);
        assert!(rows[0].summary.ends_with("; 13 records"));
        assert!(rows[1].summary.contains("recovered after 10ms"));
        assert!(!rows[1].summary.contains("records"));
        assert!(rows[2].summary.contains("over budget for 5 ticks"));
        assert!(!rows[2].summary.contains("records"));
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
