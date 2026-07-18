//! The human `rimz doctor` report: each [`DoctorReport`](super::model::DoctorReport)
//! section as a titled block in the room's palette, with a status glyph carrying
//! the verdict. Built on the shared [`crate::cli::render`] table and key/value
//! primitives, so the report reads like every other `rimz` command and strips to
//! clean text when color is off.

use std::io::{self, Write};

use jiff::Timestamp;

use crate::cli::render::{
    Cell, KeyVals, Table, cell, fmt_bytes, home_relative, paint, palette, status,
};
use rimz::agents::AgentStatus;
use rimz::trust::TrustState;

use super::model::{
    AgentCounts, AgentRollup, Capabilities, Diagnostics, DoctorImpact, DoctorReport, DoctorState,
    DuplicateSessions, HookStatus, Host, LogScope, LoopTasks, MachineConfigHealth,
    MessageProblemRow, Messages, Mux, MuxBinaryRow, MuxLog, PluginRow, Presence,
    PresencePluginStatus, PresencePlugins, Probe, Protocols, RemoteAgent, RemoteControl, Room,
    RoomState, SessionHealth, Storage, Terminal, TopologyWriterHealth, Trust, Version, Workspace,
};

/// A section verdict: the glyph and palette tone it renders with.
#[derive(Clone, Copy)]
enum Health {
    Ok,
    Warn,
    Alarm,
    Info,
    Neutral,
}

fn parts(health: Health) -> (&'static str, anstyle::Style) {
    let role = match health {
        Health::Ok => status::StateRole::Success,
        Health::Warn => status::StateRole::Waiting,
        Health::Alarm => status::StateRole::Failed,
        Health::Info => status::StateRole::Working,
        Health::Neutral => status::StateRole::Neutral,
    };
    crate::cli::render::verdict(role)
}

#[derive(Default)]
struct Tally {
    warns: usize,
    alarms: usize,
}

impl Tally {
    fn record(&mut self, health: Health) {
        match health {
            Health::Warn => self.warns += 1,
            Health::Alarm => self.alarms += 1,
            Health::Ok | Health::Info | Health::Neutral => {}
        }
    }
}

/// A glyph-only cell for a table's status column.
fn badge(tally: &mut Tally, health: Health) -> Cell {
    tally.record(health);
    let (glyph, style) = parts(health);
    cell(glyph).fg(style)
}

/// A `glyph text` value cell, both painted in the verdict's tone.
fn verdict(tally: &mut Tally, health: Health, text: impl Into<String>) -> Cell {
    tally.record(health);
    let (glyph, style) = parts(health);
    cell(format!("{glyph} {}", text.into())).fg(style)
}

fn unavailable_text(error: &str) -> String {
    format!("unavailable ({error})")
}

fn unavailable(tally: &mut Tally, health: Health, error: &str) -> Cell {
    verdict(tally, health, unavailable_text(error))
}

fn fit_cell(tally: &mut Tally, fits: bool, used: usize, limit: usize, path: &str) -> Cell {
    let (health, label) = if fits {
        (Health::Ok, "OK")
    } else {
        (Health::Alarm, "TOO LONG")
    };
    verdict(
        tally,
        health,
        format!("{label} ({used}/{limit} bytes for {path})"),
    )
}

fn style_of(health: Health) -> anstyle::Style {
    parts(health).1
}

/// Open a titled section: a blank line then the heading in the accent tone.
fn section(w: &mut impl Write, title: &str) -> io::Result<()> {
    writeln!(w)?;
    writeln!(w, "{}", paint(palette::header(), title))
}

/// A hanging note under a section: an indented `glyph text` line.
fn note(tally: &mut Tally, w: &mut impl Write, health: Health, text: &str) -> io::Result<()> {
    tally.record(health);
    let (glyph, style) = parts(health);
    writeln!(w, "    {}", paint(style, &format!("{glyph} {text}")))
}

fn detail(w: &mut impl Write, style: anstyle::Style, text: &str) -> io::Result<()> {
    writeln!(w, "      {}", paint(style, text))
}

pub(super) fn render_human(report: &DoctorReport, w: &mut impl Write) -> io::Result<()> {
    let mut tally = Tally::default();
    render_identity(w, report.version, &report.host)?;
    render_workspace(w, &report.workspace, &mut tally)?;
    render_mux(w, &report.mux, &mut tally)?;
    render_terminal(w, &report.terminal, &mut tally)?;
    render_machine_config(w, &report.machine_config, &mut tally)?;
    render_hooks(w, report, &mut tally)?;
    render_plugins(w, report, &mut tally)?;
    render_loop(w, &report.loop_tasks, &mut tally)?;
    render_remote_control(w, &report.remote_control, &mut tally)?;
    render_storage(w, &report.disk_usage, &mut tally)?;

    if let Some(protocols) = &report.protocols {
        render_protocols(w, protocols, &mut tally)?;
    }

    if let Some(trust) = &report.trust {
        render_trust(w, trust, &mut tally)?;
    }
    render_agents(w, report, &mut tally)?;
    render_messages(w, report, &mut tally)?;
    render_diagnostics(w, report, &mut tally)?;
    render_last_incident(w, report, &mut tally)?;
    render_tally(w, &tally)?;
    Ok(())
}

fn render_machine_config(
    w: &mut impl Write,
    config: &MachineConfigHealth,
    tally: &mut Tally,
) -> io::Result<()> {
    section(w, "MACHINE CONFIG")?;
    if config.broken_files.is_empty() {
        let mut kv = KeyVals::new().indent(2);
        kv.push(
            "config files",
            verdict(tally, Health::Ok, "all present files parse"),
        );
        return kv.render(w);
    }
    for problem in &config.broken_files {
        note(
            tally,
            w,
            Health::Warn,
            &format!(
                "{} is unparseable: {}; settings in this file use built-in defaults",
                home_relative(&problem.path),
                problem.error,
            ),
        )?;
    }
    Ok(())
}

fn render_identity(w: &mut impl Write, version: &str, host: &Host) -> io::Result<()> {
    writeln!(w, "{}", paint(palette::header(), "RimZ doctor"))?;
    let mut kv = KeyVals::new().indent(2);
    kv.push("version", cell(version));
    let user = match &host.user {
        Some(name) => format!("{name} (uid {})", host.uid),
        None => format!("uid {}", host.uid),
    };
    kv.push("user", cell(user));
    kv.push(
        "binary",
        cell(host.binary.as_deref().unwrap_or("unknown")).fg(palette::body()),
    );
    kv.render(w)
}

fn render_terminal(w: &mut impl Write, terminal: &Terminal, tally: &mut Tally) -> io::Result<()> {
    section(w, "TERMINAL")?;
    let mut kv = KeyVals::new().indent(2);
    let depth_health = if terminal.resolved_depth == "truecolor" {
        Health::Ok
    } else {
        Health::Neutral
    };
    kv.push(
        "depth",
        verdict(
            tally,
            depth_health,
            format!(
                "{} (mode {})",
                terminal.resolved_depth,
                terminal_mode_label(terminal.theme_mode)
            ),
        ),
    );
    kv.push(
        "signals",
        cell(format!(
            "truecolor-advertised={} COLORTERM={} TERM={} terminfo-truecolor={}",
            terminal.truecolor_advertised,
            terminal.colorterm.as_deref().unwrap_or("unset"),
            terminal.term.as_deref().unwrap_or("unset"),
            terminal.terminfo_truecolor,
        ))
        .fg(palette::faint()),
    );
    if let Some(fix) = &terminal.fix {
        kv.push("fix", verdict(tally, Health::Warn, fix));
    }
    kv.render(w)
}

fn terminal_mode_label(mode: rimz::config::ThemeMode) -> &'static str {
    match mode {
        rimz::config::ThemeMode::Auto => "auto",
        rimz::config::ThemeMode::Truecolor => "truecolor",
        rimz::config::ThemeMode::Indexed => "256",
    }
}

fn render_workspace(
    w: &mut impl Write,
    workspace: &Probe<Workspace>,
    tally: &mut Tally,
) -> io::Result<()> {
    section(w, "WORKSPACE")?;
    let mut kv = KeyVals::new().indent(2);
    match workspace {
        Probe::Unavailable { error } => {
            kv.push(
                "workspace",
                verdict(tally, Health::Alarm, format!("could not resolve ({error})")),
            );
            kv.render(w)
        }
        Probe::Ready(ws) => {
            kv.push("id", cell(ws.workspace_id.as_str()).fg(palette::accent()));
            kv.push(
                "project root",
                cell(ws.project_root.as_str()).fg(palette::body()),
            );
            kv.push("root class", cell(ws.root_class.label()));
            kv.push(
                "worktree root",
                cell(ws.worktree_root.as_str()).fg(palette::body()),
            );
            kv.push(
                "worktree branch",
                cell(ws.worktree_branch.as_deref().unwrap_or("<detached>")),
            );
            kv.push("session", cell(ws.session_name.as_str()));
            match &ws.sock_headroom {
                Probe::Unavailable { error } => {
                    kv.push("sock headroom", unavailable(tally, Health::Alarm, error))
                }
                Probe::Ready(budget) => {
                    kv.push(
                        "sock headroom",
                        fit_cell(tally, budget.fits, budget.used, budget.limit, &budget.dir),
                    );
                    if let Some(remedy) = &budget.remedy {
                        kv.push("remedy", verdict(tally, Health::Warn, remedy));
                    }
                }
            }
            kv.render(w)
        }
    }
}

fn render_mux(w: &mut impl Write, mux: &Probe<Mux>, tally: &mut Tally) -> io::Result<()> {
    section(w, "MULTIPLEXER")?;
    let mux = match mux {
        Probe::Unavailable { error } => {
            let mut kv = KeyVals::new().indent(2);
            kv.push("multiplexer", unavailable(tally, Health::Alarm, error));
            return kv.render(w);
        }
        Probe::Ready(mux) => mux,
    };

    let mut kv = KeyVals::new().indent(2);
    kv.push("backend", cell(mux.name.to_string()).fg(palette::accent()));
    kv.push("version", mux_version_cell(tally, &mux.version));
    push_capabilities(&mut kv, tally, &mux.capabilities);
    match &mux.binaries.active {
        Some(active) => kv.push("binary", cell(binary_label(active)).fg(palette::body())),
        None => kv.push("binary", verdict(tally, Health::Warn, "not found on PATH")),
    }
    kv.push("log", mux_log_cell(tally, &mux.log));
    if let Some(socket) = &mux.socket {
        kv.push("socket", cell(socket.as_str()).fg(palette::body()));
    }
    if let Some(socket) = &mux.zellij_socket {
        kv.push(
            "zellij socket",
            fit_cell(tally, socket.fits, socket.len, socket.limit, &socket.path),
        );
        if let Some(fix) = &socket.fix {
            kv.push("fix", verdict(tally, Health::Warn, fix));
        }
    }
    if let Some(room) = &mux.room {
        kv.push("room", room_cell(tally, mux.name, room));
    }
    if let Some(health) = &mux.session_health {
        kv.push("session health", session_health_cell(tally, health));
    }
    if let Some(presence) = &mux.presence {
        kv.push("presence", presence_cell(tally, presence));
    }
    if let Some(writer) = &mux.topology_writer {
        push_topology_writer(&mut kv, tally, writer);
    }
    if let Some(plugins) = &mux.presence_plugins {
        push_presence_plugins(&mut kv, tally, plugins);
    }
    if let Some(ttyd) = &mux.ttyd {
        match ttyd {
            Probe::Ready(ttyd) => kv.push(
                "ttyd web",
                verdict(
                    tally,
                    Health::Ok,
                    format!("{} ({})", ttyd.version, ttyd.path),
                ),
            ),
            Probe::Unavailable { error } => kv.push(
                "ttyd web",
                verdict(
                    tally,
                    Health::Warn,
                    format!("missing — rimz web needs it; {error}"),
                ),
            ),
        }
    }
    kv.render(w)?;

    render_mux_binary_notes(w, mux, tally)?;
    render_mux_log_notes(w, &mux.log, tally)?;
    render_duplicate_session_notes(w, &mux.duplicate_sessions, tally)
}

fn room_cell(tally: &mut Tally, selected: rimz::MuxName, room: &Room) -> Cell {
    let here = room_state_label(&room.selected_state);
    let rival = selected.other();
    let rival_state = match rival {
        rimz::MuxName::Zellij => &room.zellij,
        rimz::MuxName::Tmux => &room.tmux,
    };
    let elsewhere = if matches!(rival_state, RoomState::Live) {
        format!("live on {rival}")
    } else {
        format!("{rival} {}", room_state_label(rival_state))
    };
    let label = format!("{here} here; {elsewhere}");
    if room.conflict {
        verdict(
            tally,
            Health::Alarm,
            format!("{label} (room ownership conflict)"),
        )
    } else if matches!(room.selected_state, RoomState::Unavailable { .. }) {
        verdict(tally, Health::Warn, label)
    } else if matches!(room.selected_state, RoomState::Live) {
        verdict(tally, Health::Ok, label)
    } else {
        cell(label).fg(palette::faint())
    }
}

fn room_state_label(state: &RoomState) -> String {
    match state {
        RoomState::Live => "live".to_owned(),
        RoomState::Exited => "exited".to_owned(),
        RoomState::Absent => "absent".to_owned(),
        RoomState::Unavailable { error } => format!("unavailable ({error})"),
    }
}

fn mux_version_cell(tally: &mut Tally, version: &Version) -> Cell {
    match version {
        Version::Reported { version } => cell(version.as_str()),
        Version::Unknown => cell("unknown").fg(palette::faint()),
        Version::Unavailable { error } => unavailable(tally, Health::Warn, error),
    }
}

fn push_capabilities(kv: &mut KeyVals, tally: &mut Tally, capabilities: &Capabilities) {
    match capabilities {
        Capabilities::Zellij(Probe::Ready(caps)) => kv.push(
            "zellij floor",
            floor_cell(tally, caps.meets_min_version, caps.min_version),
        ),
        Capabilities::Zellij(Probe::Unavailable { error }) => {
            kv.push("zellij floor", unavailable(tally, Health::Warn, error));
        }
        Capabilities::Tmux(Probe::Ready(caps)) => {
            kv.push(
                "tmux floor",
                floor_cell(tally, caps.meets_min_version, caps.min_version),
            );
            let (maj, min, patch) = caps.min_version;
            let popup = if caps.popup_supported {
                verdict(tally, Health::Ok, "supported")
            } else {
                verdict(
                    tally,
                    Health::Warn,
                    format!("unavailable (requires tmux >= {maj}.{min}.{patch})"),
                )
            };
            kv.push("tmux popup", popup);
        }
        Capabilities::Tmux(Probe::Unavailable { error }) => {
            kv.push("tmux floor", unavailable(tally, Health::Warn, error));
        }
    }
}

fn mux_log_cell(tally: &mut Tally, log: &MuxLog) -> Cell {
    match log {
        MuxLog::Ready {
            path, size_bytes, ..
        } => cell(format!("{path} ({})", fmt_bytes(*size_bytes))).fg(palette::body()),
        MuxLog::Missing { path } => cell(format!("none yet ({path})")).fg(palette::faint()),
        MuxLog::Disabled { hint } => cell(hint.as_str()).fg(palette::faint()),
        MuxLog::Unavailable { error } => unavailable(tally, Health::Warn, error),
    }
}

fn session_health_cell(tally: &mut Tally, health: &Probe<SessionHealth>) -> Cell {
    match health {
        Probe::Unavailable { error } => unavailable(tally, Health::Warn, error),
        Probe::Ready(SessionHealth::Ok) => verdict(tally, Health::Ok, "ok"),
        Probe::Ready(SessionHealth::Stuck { fix }) => verdict(
            tally,
            Health::Alarm,
            format!("stuck (resurrected/suspended panes) — {fix}"),
        ),
    }
}

fn presence_cell(tally: &mut Tally, presence: &Presence) -> Cell {
    match presence {
        Presence::Event { poked_secs } => verdict(
            tally,
            Health::Ok,
            format!("event mode (poked {poked_secs}s ago)"),
        ),
        Presence::Poll { reason, expected } => {
            let health = if *expected { Health::Ok } else { Health::Warn };
            verdict(tally, health, format!("polling — {reason}"))
        }
        Presence::NotApplicable { reason } => {
            cell(format!("not applicable — {reason}")).fg(palette::faint())
        }
        Presence::Unavailable { error } => unavailable(tally, Health::Alarm, error),
    }
}

fn push_topology_writer(kv: &mut KeyVals, tally: &mut Tally, writer: &TopologyWriterHealth) {
    if let Some(bin) = &writer.recorded_bin {
        let health = if bin.exists { Health::Ok } else { Health::Warn };
        let label = if bin.exists {
            bin.path.clone()
        } else {
            format!("missing ({})", bin.path)
        };
        kv.push("room binary", verdict(tally, health, label));
        if let Some(fix) = &bin.fix {
            kv.push("room binary fix", verdict(tally, Health::Warn, fix));
        }
    }
    if let Some(conflict) = &writer.conflict {
        let stale = conflict
            .stale
            .as_ref()
            .map(topology_writer_label)
            .unwrap_or_else(|| "legacy".to_owned());
        let accepted = conflict
            .accepted
            .as_ref()
            .map(topology_writer_label)
            .unwrap_or_else(|| "legacy".to_owned());
        kv.push(
            "topology writers",
            verdict(
                tally,
                Health::Info,
                format!(
                    "contained stale writer: rejected {stale}, accepted {accepted}; {} rejects, {}s ago — {}",
                    conflict.rejected_count, conflict.age_secs, conflict.fix
                ),
            ),
        );
    }
}

fn push_presence_plugins(kv: &mut KeyVals, tally: &mut Tally, plugins: &PresencePlugins) {
    let non_inactive = plugins
        .rows
        .iter()
        .filter(|row| row.status != PresencePluginStatus::Inactive)
        .count();
    let desired = plugins
        .desired_build
        .as_deref()
        .map(short_build)
        .unwrap_or_else(|| "unknown".to_owned());
    let header = format!(
        "desired {desired} · {} recent generations",
        plugins.rows.len()
    );
    kv.push(
        "presence plugins",
        verdict(
            tally,
            if non_inactive > 1 {
                Health::Warn
            } else {
                Health::Info
            },
            if non_inactive > 1 {
                format!("{header} · {non_inactive} active/rejected — run `rimz reload`")
            } else {
                header
            },
        ),
    );

    for row in &plugins.rows {
        let version = row.zellij_version.as_deref().unwrap_or("unknown");
        let build = row
            .build
            .as_deref()
            .map(short_build)
            .unwrap_or_else(|| "unknown".to_owned());
        let status = match row.status {
            PresencePluginStatus::Active => "active".to_owned(),
            PresencePluginStatus::Rejected => format!(
                "rejected ×{} — run `rimz reload`",
                row.rejected_count.unwrap_or_default()
            ),
            PresencePluginStatus::Inactive => "inactive".to_owned(),
        };
        let outdated = if row.outdated { " · outdated" } else { "" };
        let topology_failures = row.topology_failures_delta.unwrap_or_default();
        let other_failures = row.other_failures_delta.unwrap_or_default();
        let recent_failures = row.last_seen_age_secs
            <= rimz::sidebar::timing::PRESENCE_STAMP_FRESH.as_secs()
            && (topology_failures > 0 || other_failures > 0);
        let telemetry = if row.sample_count == 0 {
            format!("no telemetry · seen {}s ago", row.last_seen_age_secs)
        } else {
            let succeeded = row
                .commands_succeeded_delta
                .map(|delta| format!("/{delta} succeeded"))
                .unwrap_or_default();
            let rejects = row
                .stale_writer_rejections_delta
                .filter(|delta| *delta > 0)
                .map(|delta| format!(" · stale rejects +{delta}"))
                .unwrap_or_default();
            let failures = (topology_failures > 0 || other_failures > 0)
                .then(|| format!(" · failures {topology_failures}/{other_failures}"))
                .unwrap_or_default();
            format!(
                "{} samples · seen {}s ago · pages {:+} · bytes {:+} · commands +{}{}{rejects}{failures}",
                row.sample_count,
                row.last_seen_age_secs,
                row.page_growth,
                row.byte_growth,
                row.commands_completed_delta,
                succeeded,
            )
        };
        kv.push(
            format!("plugin {}", row.plugin_id),
            verdict(
                tally,
                if recent_failures {
                    Health::Warn
                } else if row.status == PresencePluginStatus::Active && !row.outdated {
                    Health::Ok
                } else {
                    Health::Info
                },
                format!(
                    "loaded {} · build {build} · zellij {version} · {status}{outdated} · {telemetry}",
                    plugin_loaded_time(row.loaded_at_ms),
                ),
            ),
        );
    }
}

fn plugin_loaded_time(loaded_at_ms: u64) -> String {
    i64::try_from(loaded_at_ms)
        .ok()
        .and_then(|millis| Timestamp::from_millisecond(millis).ok())
        .map(|timestamp| timestamp.strftime("%H:%M:%S").to_string())
        .unwrap_or_else(|| loaded_at_ms.to_string())
}

fn short_build(build: &str) -> String {
    build.chars().take(8).collect()
}

fn render_duplicate_session_notes(
    w: &mut impl Write,
    duplicate_sessions: &Option<Probe<DuplicateSessions>>,
    tally: &mut Tally,
) -> io::Result<()> {
    if let Some(Probe::Ready(dup)) = duplicate_sessions {
        if dup.groups.is_empty() {
            note(tally, w, Health::Ok, "duplicate sessions: none")?;
        } else {
            note(
                tally,
                w,
                Health::Warn,
                &format!(
                    "duplicate sessions: {} live sessions share this workspace; pane updates can be held",
                    dup.groups.len()
                ),
            )?;
            for group in &dup.groups {
                let here = if group.is_current { "* " } else { "  " };
                let panes = if group.pane_ids.is_empty() {
                    "unlocated".to_owned()
                } else {
                    group.pane_ids.join(", ")
                };
                detail(
                    w,
                    palette::body(),
                    &format!(
                        "{here}{}/{}: {} sidebars ({panes})",
                        group.mux, group.session_name, group.sidebar_count
                    ),
                )?;
            }
            if let Some(advice) = &dup.advice {
                note(tally, w, Health::Warn, advice)?;
            }
        }
    } else if let Some(Probe::Unavailable { error }) = duplicate_sessions {
        note(
            tally,
            w,
            Health::Warn,
            &format!("duplicate sessions: {}", unavailable_text(error)),
        )?;
    }
    Ok(())
}

fn topology_writer_label(writer: &super::model::TopologyWriterId) -> String {
    format!("{}:{}", writer.loaded_at_ms, writer.plugin_id)
}

fn binary_label(row: &MuxBinaryRow) -> String {
    match &row.version {
        Some(version) => format!("{} — {version}", row.path),
        None => row.path.clone(),
    }
}

fn render_mux_binary_notes(w: &mut impl Write, mux: &Mux, tally: &mut Tally) -> io::Result<()> {
    if !mux.binaries.duplicates.is_empty() {
        note(
            tally,
            w,
            Health::Warn,
            &format!(
                "multiple {} binaries on PATH — clients and servers can mismatch; keep one, remove or shadow the rest",
                mux.name
            ),
        )?;
        if let Some(active) = &mux.binaries.active {
            detail(w, palette::body(), &format!("* {}", binary_label(active)))?;
        }
        for install in &mux.binaries.duplicates {
            detail(w, palette::body(), &format!("  {}", binary_label(install)))?;
        }
    }
    if mux.binaries.active.is_none() {
        return Ok(());
    }
    for server in &mux.binaries.server_mismatches {
        let deleted = if server.deleted { " (deleted)" } else { "" };
        note(
            tally,
            w,
            Health::Warn,
            &format!(
                "running {} server (pid {}) uses {}{} — restart its sessions on the PATH binary",
                mux.name, server.pid, server.exe, deleted
            ),
        )?;
    }
    Ok(())
}

fn render_mux_log_notes(w: &mut impl Write, log: &MuxLog, tally: &mut Tally) -> io::Result<()> {
    let MuxLog::Ready {
        scope,
        problem_records,
        scanned_bytes,
        omitted_issue_groups,
        issues,
        ..
    } = log
    else {
        return Ok(());
    };
    match scope {
        LogScope::HostUser { uid } => note(
            tally,
            w,
            Health::Info,
            &format!("log scope: host user uid {uid}; records may come from other Zellij sessions"),
        )?,
        LogScope::Server => note(tally, w, Health::Info, "log scope: active tmux server")?,
    }
    if *problem_records == 0 {
        return note(tally, w, Health::Ok, "log: no recent warnings or errors");
    }
    for issue in issues {
        let health = doctor_health(issue.state, issue.impact);
        let range = match (&issue.first_occurrence, &issue.last_occurrence) {
            (Some(first), Some(last)) if first != last => format!(" · {first} to {last}"),
            (Some(at), _) | (_, Some(at)) => format!(" · at {at}"),
            (None, None) => String::new(),
        };
        note(
            tally,
            w,
            health,
            &format!(
                "{} {:?}/{:?} · {} occurrences{} in {} · {}",
                issue.source_severity,
                issue.state,
                issue.impact,
                issue.occurrences,
                range,
                fmt_bytes(*scanned_bytes),
                issue.summary
            ),
        )?;
        for sample in &issue.samples {
            detail(w, style_of(health), sample)?;
        }
        if issue.evidence_truncated {
            detail(w, palette::muted(), "evidence truncated at 8 KiB")?;
        }
    }
    if *omitted_issue_groups > 0 {
        detail(
            w,
            palette::muted(),
            &format!("{omitted_issue_groups} older issue groups omitted"),
        )?;
    }
    Ok(())
}

fn floor_cell(tally: &mut Tally, meets: bool, min: (u32, u32, u32)) -> Cell {
    let (maj, min_v, patch) = min;
    let (health, label) = if meets {
        (Health::Ok, "OK")
    } else {
        (Health::Alarm, "TOO OLD")
    };
    verdict(
        tally,
        health,
        format!("{label} (>= {maj}.{min_v}.{patch} required)"),
    )
}

fn render_hooks(w: &mut impl Write, report: &DoctorReport, tally: &mut Tally) -> io::Result<()> {
    section(w, "HOOKS")?;
    let (mut not_detected, table_rows): (Vec<_>, Vec<_>) = report.hooks.iter().partition(|row| {
        !row.detected
            && matches!(
                &row.status,
                HookStatus::NotDetected | HookStatus::Unsupported { .. }
            )
    });
    let has_table_rows = !table_rows.is_empty();
    let mut table = Table::new(["", "AGENT", "STATUS", "FIX"]);
    for row in table_rows {
        let (health, fix) = match &row.status {
            HookStatus::Installed => (Health::Ok, String::new()),
            HookStatus::InstalledUntrusted { events, fix } => (
                Health::Warn,
                format!(
                    "silently skips untrusted hooks ({}) — {fix}",
                    events.join(", ")
                ),
            ),
            HookStatus::NotInstalled { fix } => (Health::Alarm, fix.clone()),
            HookStatus::NotDetected => (Health::Neutral, String::new()),
            HookStatus::Unsupported { reason } => (Health::Neutral, reason.clone()),
        };
        let fix = if fix.is_empty() { "-".to_owned() } else { fix };
        table.row([
            badge(tally, health),
            cell(row.kind.as_str()).fg(palette::identity(&row.kind)),
            cell(row.status.label()).fg(style_of(health)),
            cell(fix).dash(),
        ]);
    }
    if has_table_rows {
        table.render(w)?;
    }
    if !not_detected.is_empty() {
        not_detected.sort_by(|left, right| left.kind.cmp(&right.kind));
        let kinds = not_detected
            .iter()
            .map(|row| row.kind.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        note(
            tally,
            w,
            Health::Neutral,
            &format!("not detected on this machine: {kinds}"),
        )?;
        detail(
            w,
            palette::faint(),
            "hooks are offered automatically once an agent is installed",
        )?;
    }
    Ok(())
}

fn render_plugins(w: &mut impl Write, report: &DoctorReport, tally: &mut Tally) -> io::Result<()> {
    if report.plugins.is_empty() {
        return Ok(());
    }
    section(w, "AGENT PLUGINS")?;
    let mut table = Table::new(["", "AGENT", "STATUS", "MANIFEST", "DETAIL"]);
    for plugin in &report.plugins {
        let (health, status, detail) = plugin_verdict(plugin);
        table.row([
            badge(tally, health),
            cell(plugin.kind.as_str()).fg(palette::identity(&plugin.kind)),
            cell(status).fg(style_of(health)),
            cell(home_relative(&plugin.manifest)).fg(palette::body()),
            cell(detail).dash(),
        ]);
        push_probe_rows(&mut table, tally, plugin);
    }
    table.render(w)
}

fn plugin_verdict(plugin: &PluginRow) -> (Health, &'static str, String) {
    if !plugin.valid {
        return (
            Health::Alarm,
            "invalid",
            plugin
                .error
                .clone()
                .unwrap_or_else(|| "manifest validation failed".to_owned()),
        );
    }
    let bad_probes = plugin
        .probes
        .iter()
        .filter(|probe| !probe.present || !probe.executable)
        .map(|probe| probe.name)
        .collect::<Vec<_>>();
    if bad_probes.is_empty() {
        (
            Health::Ok,
            "valid",
            plugin
                .setup_doc
                .clone()
                .unwrap_or_else(|| "self-managed hooks".to_owned()),
        )
    } else {
        (
            Health::Warn,
            "valid; probe unavailable",
            format!("check {}", bad_probes.join(", ")),
        )
    }
}

fn push_probe_rows(table: &mut Table, tally: &mut Tally, plugin: &PluginRow) {
    for probe in &plugin.probes {
        let probe_health = if probe.present && probe.executable {
            Health::Ok
        } else {
            Health::Warn
        };
        let status = match (probe.present, probe.executable) {
            (true, true) => "executable",
            (true, false) => "not executable",
            (false, _) => "missing",
        };
        table.row([
            badge(tally, probe_health),
            cell(format!("  {} probe", probe.name)).fg(palette::meta()),
            cell(status).fg(style_of(probe_health)),
            cell("-"),
            cell(probe.command.as_str()).fg(palette::body()),
        ]);
    }
}

fn render_loop(w: &mut impl Write, loop_tasks: &LoopTasks, tally: &mut Tally) -> io::Result<()> {
    section(w, "LOOP TASKS")?;
    if loop_tasks.tasks.is_empty() {
        return writeln!(w, "  {}", paint(palette::faint(), "none configured"));
    }
    let mut table = Table::new(["", "NAME", "TARGET", "WHEN", "ROOT"]);
    for row in &loop_tasks.tasks {
        let health = if row.valid {
            Health::Info
        } else {
            Health::Alarm
        };
        table.row([
            badge(tally, health),
            cell(row.name.as_str()).fg(palette::accent()),
            cell(row.spec.as_str()),
            cell(row.when.as_str()).fg(style_of(health)),
            cell(home_relative(&row.root)).fg(palette::body()),
        ]);
    }
    table.render(w)?;
    note(
        tally,
        w,
        Health::Neutral,
        "`rimz loop list` shows room-open state",
    )
}

fn render_remote_control(
    w: &mut impl Write,
    remote: &RemoteControl,
    tally: &mut Tally,
) -> io::Result<()> {
    section(w, "REMOTE CONTROL")?;
    match remote {
        RemoteControl::Unavailable { error } => {
            let mut kv = KeyVals::new().indent(2);
            kv.push(
                "remote control",
                verdict(
                    tally,
                    Health::Alarm,
                    format!("config unavailable ({error})"),
                ),
            );
            kv.render(w)
        }
        RemoteControl::Off => {
            let mut kv = KeyVals::new().indent(2);
            kv.push("remote control", cell("off").fg(palette::faint()));
            kv.render(w)
        }
        RemoteControl::On {
            agents,
            refusals,
            skipped,
            advisories,
        } => render_remote_on(w, agents, refusals, skipped, advisories, tally),
    }
}

fn render_remote_on(
    w: &mut impl Write,
    agents: &[RemoteAgent],
    refusals: &[String],
    skipped: &[String],
    advisories: &[String],
    tally: &mut Tally,
) -> io::Result<()> {
    let mut kv = KeyVals::new().indent(2);
    for agent in agents {
        let health = if agent.ready {
            Health::Ok
        } else {
            Health::Warn
        };
        kv.push(agent.kind, verdict(tally, health, &agent.detail));
    }
    kv.render(w)?;
    for refusal in refusals {
        note(tally, w, Health::Alarm, "`rimz start` refuses:")?;
        for line in refusal.lines() {
            detail(w, palette::muted(), line)?;
        }
    }
    for skip in skipped {
        note(
            tally,
            w,
            Health::Warn,
            "enabled but not installed — skipped (the room still starts):",
        )?;
        for line in skip.lines() {
            detail(w, palette::muted(), line)?;
        }
    }
    for advisory in advisories {
        note(
            tally,
            w,
            Health::Warn,
            "provider daemon advisory (no start impact):",
        )?;
        for line in advisory.lines() {
            detail(w, palette::muted(), line)?;
        }
    }
    Ok(())
}

fn render_storage(w: &mut impl Write, disk_usage: &Storage, tally: &mut Tally) -> io::Result<()> {
    section(w, "STORAGE")?;
    writeln!(
        w,
        "  {}",
        paint(
            palette::muted(),
            &format!("rimz on disk: {}", fmt_bytes(disk_usage.total_bytes))
        )
    )?;
    let mut table = Table::new(["", "AREA", "SIZE", "PATH"]).right(&[2]);
    for root in &disk_usage.roots {
        let size = if root.present {
            fmt_bytes(root.bytes)
        } else {
            "-".to_owned()
        };
        table.row([
            badge(tally, Health::Neutral),
            cell(root.label),
            cell(size).dash(),
            cell(home_relative(&root.path)).fg(if root.present {
                palette::body()
            } else {
                palette::faint()
            }),
        ]);
    }
    table.render(w)
}

fn render_protocols(
    w: &mut impl Write,
    protocols: &Protocols,
    tally: &mut Tally,
) -> io::Result<()> {
    section(w, "PROTOCOLS")?;
    let mut kv = KeyVals::new().indent(2);
    kv.push("event", cell(protocols.event));
    kv.push("sidebar", cell(protocols.sidebar));
    kv.render(w)?;
    for warning in &protocols.warnings {
        note(tally, w, Health::Warn, warning)?;
    }
    if let Some(drift) = &protocols.build_drift {
        note(
            tally,
            w,
            Health::Warn,
            &format!(
                "mixed rimz builds writing this workspace: {} distinct builds; run `rimz reload` to converge",
                drift.writers.len(),
            ),
        )?;
        for writer in &drift.writers {
            let tag = if writer.is_running {
                " (this binary)"
            } else {
                ""
            };
            let location = if writer.sidebar_count == 0 {
                "no live sidebar".to_owned()
            } else if writer.pane_ids.is_empty() {
                format!("{} sidebars: unlocated", writer.sidebar_count)
            } else {
                format!(
                    "{} sidebars: {}",
                    writer.sidebar_count,
                    writer.pane_ids.join(", ")
                )
            };
            detail(
                w,
                palette::body(),
                &format!("{}{tag}: {location}", writer.build),
            )?;
        }
    }
    Ok(())
}

fn render_trust(w: &mut impl Write, trust: &Probe<Trust>, tally: &mut Tally) -> io::Result<()> {
    section(w, "TRUST")?;
    let mut kv = KeyVals::new().indent(2);
    let value = match trust {
        Probe::Unavailable { error } => unavailable(tally, Health::Alarm, error),
        Probe::Ready(Trust { state, granted_at }) => match state {
            TrustState::Trusted => verdict(
                tally,
                Health::Ok,
                format!(
                    "trusted (granted {})",
                    granted_at.as_deref().unwrap_or("<unknown>")
                ),
            ),
            TrustState::Stale => verdict(
                tally,
                Health::Alarm,
                "stale (executable surface drifted; run `rimz trust grant` to refresh)",
            ),
            TrustState::Untrusted => verdict(
                tally,
                Health::Warn,
                "untrusted (run `rimz trust grant` to enable command paths)",
            ),
            TrustState::NoConfig => verdict(tally, Health::Neutral, "no project config"),
        },
    };
    kv.push("trust", value);
    kv.render(w)
}

fn render_agents(w: &mut impl Write, report: &DoctorReport, tally: &mut Tally) -> io::Result<()> {
    let Some(rollup) = &report.agents else {
        return Ok(());
    };
    section(w, "AGENTS")?;
    match rollup {
        AgentRollup::Unavailable { error } => {
            note(tally, w, Health::Alarm, &unavailable_text(error))
        }
        AgentRollup::None => writeln!(w, "  {}", paint(palette::faint(), "none observed")),
        AgentRollup::Observed { counts, rows } => {
            writeln!(
                w,
                "  {}",
                paint(palette::muted(), &agent_counts_line(counts))
            )?;
            if rows.is_empty() {
                return Ok(());
            }
            let now = Timestamp::now();
            let mut table = Table::new(["", "KIND", "ID", "BRANCH", "STATUS", "SEEN"]);
            for agent in rows {
                let health = status_health(agent.status);
                let style = status::agent(agent.status, agent.phase);
                table.row([
                    badge(tally, health),
                    cell(agent.kind.as_str()).fg(palette::identity(&agent.kind)),
                    cell(agent.agent_id.as_str()).fg(palette::accent()),
                    cell(agent.branch.as_deref().unwrap_or("-")).dash(),
                    cell(agent.status.as_str()).fg(style),
                    cell(age_short(now, agent.last_seen)),
                ]);
            }
            table.render(w)
        }
    }
}

fn agent_counts_line(counts: &AgentCounts) -> String {
    let mut parts = Vec::new();
    push_count(&mut parts, counts.running, "running");
    push_count(&mut parts, counts.waiting, "waiting");
    push_count(&mut parts, counts.idle, "idle");
    push_count(&mut parts, counts.success, "success");
    push_count(&mut parts, counts.failed, "failed");
    push_count(&mut parts, counts.paused, "paused");
    if parts.is_empty() {
        "0 live".to_owned()
    } else {
        format!("{} live: {}", counts.total(), parts.join(", "))
    }
}

fn push_count(parts: &mut Vec<String>, count: usize, label: &str) {
    if count > 0 {
        parts.push(format!("{count} {label}"));
    }
}

fn render_messages(w: &mut impl Write, report: &DoctorReport, tally: &mut Tally) -> io::Result<()> {
    let Some(messages) = &report.messages else {
        return Ok(());
    };
    section(w, "MESSAGES")?;
    let messages = match messages {
        Probe::Unavailable { error } => {
            return note(tally, w, Health::Alarm, &unavailable_text(error));
        }
        Probe::Ready(messages) => messages,
    };
    if messages.open.total() == 0
        && messages.stuck.is_empty()
        && messages.recent_failures.is_empty()
    {
        return writeln!(w, "  {}", paint(palette::faint(), "no open messages"));
    }
    if messages.open.total() == 0 {
        writeln!(
            w,
            "  {}",
            paint(palette::muted(), "0 open — `rimz message list`")
        )?;
    } else {
        writeln!(
            w,
            "  {}",
            paint(
                palette::muted(),
                &format!(
                    "{} open: {} — `rimz message list`",
                    messages.open.total(),
                    open_counts_line(messages)
                )
            )
        )?;
    }
    render_message_rows(w, messages, tally)
}

fn open_counts_line(messages: &Messages) -> String {
    let mut parts = Vec::new();
    push_count(&mut parts, messages.open.queued, "queued");
    push_count(&mut parts, messages.open.claimed, "claimed");
    push_count(&mut parts, messages.open.sent, "sent");
    parts.join(", ")
}

fn render_message_rows(
    w: &mut impl Write,
    messages: &Messages,
    tally: &mut Tally,
) -> io::Result<()> {
    if messages.stuck.is_empty() && messages.recent_failures.is_empty() {
        return Ok(());
    }
    let now = Timestamp::now();
    let mut table = Table::new(["", "ID", "STATUS", "TARGET", "AGE", "PROBLEM"]);
    for row in &messages.stuck {
        render_message_row(&mut table, tally, row, Health::Warn, now);
    }
    for row in &messages.recent_failures {
        render_message_row(&mut table, tally, row, Health::Alarm, now);
    }
    table.render(w)
}

fn render_message_row(
    table: &mut Table,
    tally: &mut Tally,
    row: &MessageProblemRow,
    health: Health,
    now: Timestamp,
) {
    table.row([
        badge(tally, health),
        cell(row.message_id.as_str()).fg(palette::accent()),
        cell(row.status.as_str()).fg(style_of(health)),
        cell(row.target.as_str()).fg(palette::meta()),
        cell(age_short(now, row.at)),
        cell(row.problem.as_str()).fg(palette::body()),
    ]);
}

fn render_diagnostics(
    w: &mut impl Write,
    report: &DoctorReport,
    tally: &mut Tally,
) -> io::Result<()> {
    let Some(diagnostics) = &report.diagnostics else {
        return Ok(());
    };
    section(w, "DIAGNOSTICS")?;
    if let Some(cleared_at) = report.history_cleared_at {
        writeln!(
            w,
            "  {}",
            paint(
                palette::muted(),
                &format!(
                    "history cleared {}",
                    age_short(Timestamp::now(), cleared_at)
                )
            )
        )?;
    }
    match diagnostics {
        Diagnostics::Unavailable => writeln!(w, "  {}", paint(palette::faint(), "unavailable")),
        Diagnostics::Ready { path, incidents } if incidents.is_empty() => writeln!(
            w,
            "  {}",
            paint(palette::faint(), &format!("no recent records ({path})"))
        ),
        Diagnostics::Ready { path, incidents } => {
            writeln!(
                w,
                "  {}",
                paint(
                    palette::muted(),
                    &format!("{} recent incidents ({path})", incidents.len())
                )
            )?;
            let now_ms = rimz::sidebar::timing::unix_now_ms();
            let mut table =
                Table::new(["", "STATE", "IMPACT", "KIND", "SEEN", "SUMMARY"]).right(&[4]);
            for incident in incidents {
                let health = doctor_health(incident.state, incident.impact);
                let seen = if incident.first_at_ms == incident.last_at_ms {
                    age_ms_short(now_ms, incident.last_at_ms)
                } else {
                    format!(
                        "{} to {}",
                        age_ms_short(now_ms, incident.first_at_ms),
                        age_ms_short(now_ms, incident.last_at_ms)
                    )
                };
                let mut summary = format!(
                    "{} · {} observers · {} records",
                    incident.summary, incident.distinct_observer_count, incident.record_count
                );
                if incident.stale_build
                    && let Some(build) = &incident.build
                {
                    // ponytail: SUMMARY is the unpadded final column; add styled Cell spans
                    // before giving this table a max width.
                    summary.push_str(&paint(palette::muted(), &format!(" · old build {build}")));
                }
                table.row([
                    badge(tally, health),
                    cell(format!("{:?}", incident.state).to_ascii_lowercase()).fg(style_of(health)),
                    cell(format!("{:?}", incident.impact).to_ascii_lowercase())
                        .fg(style_of(health)),
                    cell(incident.kind.as_str()),
                    cell(seen),
                    cell(summary).fg(palette::body()),
                ]);
            }
            table.render(w)
        }
    }
}

fn doctor_health(state: DoctorState, impact: DoctorImpact) -> Health {
    if state != DoctorState::Investigate {
        return Health::Info;
    }
    match impact {
        DoctorImpact::Alarm => Health::Alarm,
        DoctorImpact::Warn => Health::Warn,
        DoctorImpact::Info => Health::Info,
    }
}

fn render_last_incident(
    w: &mut impl Write,
    report: &DoctorReport,
    tally: &mut Tally,
) -> io::Result<()> {
    let Some(incident) = &report.last_incident else {
        return Ok(());
    };
    section(w, "LAST INCIDENT")?;
    let now = Timestamp::now();
    note(
        tally,
        w,
        Health::Info,
        &format!(
            "{} · {} ({})",
            incident.cause,
            incident.at.strftime("%Y-%m-%d %H:%M"),
            age_short(now, incident.at),
        ),
    )?;
    if !incident.lost_agents.is_empty() {
        let names = incident
            .lost_agents
            .iter()
            .map(|agent| match &agent.name {
                Some(name) => format!("{name} ({})", agent.kind),
                None => agent.kind.clone(),
            })
            .collect::<Vec<_>>()
            .join(", ");
        detail(w, palette::body(), &format!("lost: {names}"))?;
    }
    if let Some(recovered) = incident.recovered {
        detail(
            w,
            palette::body(),
            &format!("recovered: {recovered} of {}", incident.lost_agents.len()),
        )?;
    }
    if let Some(forensics) = &incident.forensics {
        detail(
            w,
            palette::muted(),
            &format!("forensics: {}", home_relative(forensics)),
        )?;
    }
    Ok(())
}

fn render_tally(w: &mut impl Write, tally: &Tally) -> io::Result<()> {
    writeln!(w)?;
    if tally.alarms > 0 {
        writeln!(
            w,
            "{}",
            paint(
                palette::alarm(),
                &format!(
                    "✗ {}, ! {}",
                    plural(tally.alarms, "problem"),
                    plural(tally.warns, "warning")
                )
            )
        )
    } else if tally.warns > 0 {
        writeln!(
            w,
            "{}",
            paint(
                palette::warn(),
                &format!("! {}", plural(tally.warns, "warning"))
            )
        )
    } else {
        writeln!(w, "{}", paint(palette::good(), "✓ no problems found"))
    }
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

fn status_health(status: AgentStatus) -> Health {
    match status {
        AgentStatus::Running | AgentStatus::Success => Health::Ok,
        AgentStatus::Waiting | AgentStatus::Paused => Health::Warn,
        AgentStatus::Failed => Health::Alarm,
        AgentStatus::Idle => Health::Neutral,
    }
}

/// Relative age of `then` from `now`, rendered compactly (`5s ago`, `2m ago`).
fn age_short(now: Timestamp, then: Timestamp) -> String {
    if now.duration_since(then).is_negative() {
        return "now".to_owned();
    }
    format!("{} ago", crate::cli::render::age_short(then, now))
}

fn age_ms_short(now_ms: u64, then_ms: u64) -> String {
    format!(
        "{} ago",
        crate::cli::render::age_label(now_ms.saturating_sub(then_ms) / 1_000)
    )
}

#[cfg(test)]
mod tests;
