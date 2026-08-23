//! The human `rimz doctor` report: each [`DoctorReport`](super::model::DoctorReport)
//! section as a titled block in the room's palette, with a status glyph carrying
//! the verdict. Built on the shared [`crate::cli::render`] table and key/value
//! primitives, so the report reads like every other `rimz` command and strips to
//! clean text when color is off.

use std::io::{self, Write};

use jiff::Timestamp;

use crate::cli::render::{
    Cell, KeyVals, Table, age_label, cell, fmt_bytes, home_relative, paint, palette, status,
};
use rimz::agents::AgentStatus;
use rimz::trust::TrustState;

use super::model::{
    AgentCounts, AgentRollup, Capabilities, Diagnostics, DoctorImpact, DoctorReport, DoctorState,
    DuplicateSessions, HookStatus, Host, LogScope, LoopTasks, MachineConfigHealth,
    MachineConfigProblemKind, MessageProblemRow, Messages, Mux, MuxBinaryRow, MuxLog, PluginRow,
    Presence, PresencePluginRow, PresencePluginStatus, PresencePluginTelemetry, PresencePlugins,
    Probe, Protocols, RemoteAgent, RemoteControl, Room, RoomState, SessionHealth, Storage,
    Terminal, TopologyWriterHealth, Trust, Version, Workspace, ZellijKittyGraphics,
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

/// Running count of what the report found, kept per section so the closing
/// line can point at where to look rather than just how much there is.
#[derive(Default)]
struct Tally {
    section: &'static str,
    warns: Vec<&'static str>,
    alarms: Vec<&'static str>,
}

impl Tally {
    fn enter(&mut self, section: &'static str) {
        self.section = section;
    }

    fn record(&mut self, health: Health) {
        let bucket = match health {
            Health::Warn => &mut self.warns,
            Health::Alarm => &mut self.alarms,
            Health::Ok | Health::Info | Health::Neutral => return,
        };
        bucket.push(self.section);
    }
}

/// The sections a bucket touched, in report order and named once each.
fn sections(hits: &[&'static str]) -> String {
    let mut seen = Vec::new();
    for hit in hits {
        if !seen.contains(hit) {
            seen.push(*hit);
        }
    }
    seen.join(", ")
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
/// Entering also aims the tally, so every finding below is attributed here.
fn section(w: &mut impl Write, tally: &mut Tally, title: &'static str) -> io::Result<()> {
    tally.enter(title);
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
    section(w, tally, "MACHINE CONFIG")?;
    if config.broken_files.is_empty() {
        let mut kv = KeyVals::new().indent(2);
        kv.push(
            "config files",
            verdict(tally, Health::Ok, "all present files parse"),
        );
        return kv.render(w);
    }
    for problem in &config.broken_files {
        let detail = match problem.kind {
            MachineConfigProblemKind::Fragment => format!(
                "{} cannot be used: {}; `rimz agents` and `rimz teams` refuse launches until this fragment is fixed",
                home_relative(&problem.path),
                problem.error,
            ),
            MachineConfigProblemKind::Parse => format!(
                "{} is unparseable: {}; settings in this file use built-in defaults",
                home_relative(&problem.path),
                problem.error,
            ),
            MachineConfigProblemKind::Semantic => format!(
                "{} is invalid: {}; settings in this file use built-in defaults",
                home_relative(&problem.path),
                problem.error,
            ),
        };
        note(tally, w, Health::Warn, &detail)?;
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
    section(w, tally, "TERMINAL")?;
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
    section(w, tally, "WORKSPACE")?;
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
    section(w, tally, "MULTIPLEXER")?;
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
    push_mux_log(&mut kv, tally, &mux.log);
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
    if let Some(legacy) = &mux.legacy_session {
        kv.push(
            "legacy session",
            verdict(
                tally,
                Health::Warn,
                format!("{} on {}", legacy.session, legacy.socket),
            ),
        );
        kv.push("fix", verdict(tally, Health::Warn, &legacy.fix));
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
        match plugins {
            Probe::Ready(plugins) => {
                push_presence_plugins(&mut kv, tally, plugins, server_version(&mux.version))
            }
            Probe::Unavailable { error } => {
                kv.push("presence plugin", unavailable(tally, Health::Warn, error))
            }
        }
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
    let label = if matches!(rival_state, RoomState::Live) {
        format!("{here} here; live on {rival}")
    } else {
        here
    };
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
        Capabilities::Zellij(Probe::Ready(caps)) => {
            kv.push(
                "zellij floor",
                floor_cell(tally, caps.meets_min_version, caps.min_version),
            );
            let graphics = match caps.kitty_graphics {
                ZellijKittyGraphics::Supported => verdict(tally, Health::Ok, "supported"),
                ZellijKittyGraphics::Unsupported => {
                    verdict(tally, Health::Info, "unsupported by host terminal")
                }
                ZellijKittyGraphics::NoReply => verdict(
                    tally,
                    Health::Info,
                    "no reply (graphics disabled or session predates Zellij 0.45)",
                ),
                ZellijKittyGraphics::BelowMinimum => verdict(
                    tally,
                    Health::Info,
                    "unavailable (requires Zellij >= 0.45.0)",
                ),
                ZellijKittyGraphics::NotProbed => {
                    cell("not probed (outside Zellij or no controlling tty)").fg(palette::faint())
                }
            };
            kv.push("zellij kitty graphics", graphics);
        }
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

/// Where the log verdict below comes from: the file, how far back the scan
/// reached, and which sessions can write into it. Provenance rather than a
/// verdict, so the issue lines under the section carry the health alone.
fn push_mux_log(kv: &mut KeyVals, tally: &mut Tally, log: &MuxLog) {
    let MuxLog::Ready {
        path,
        scope,
        size_bytes,
        scanned_bytes,
        records_before_cutoff,
        since,
        ..
    } = log
    else {
        let value = match log {
            MuxLog::Missing { path } => cell(format!("none yet ({path})")).fg(palette::faint()),
            MuxLog::Disabled { hint } => cell(hint.as_str()).fg(palette::faint()),
            MuxLog::Unavailable { error } => unavailable(tally, Health::Warn, error),
            MuxLog::Ready { .. } => unreachable!("matched above"),
        };
        kv.push("log", value);
        return;
    };
    let mut reach = format!(
        "last {} of {}",
        fmt_bytes(*scanned_bytes),
        fmt_bytes(*size_bytes)
    );
    if let Some(since) = since {
        reach.push_str(&format!(
            ", since you cleared {}",
            age_short(Timestamp::now(), *since)
        ));
        if *records_before_cutoff > 0 {
            reach.push_str(&format!(
                " ({records_before_cutoff} older records dismissed)"
            ));
        }
    }
    let scope = match scope {
        LogScope::HostUser { uid } => {
            format!("written by every zellij server running as uid {uid}")
        }
        LogScope::Server => "written by this room's tmux server".to_owned(),
    };
    kv.push_lines(
        "log",
        vec![
            vec![cell(home_relative(path)).fg(palette::body())],
            vec![cell(format!("read {reach} · {scope}")).fg(palette::faint())],
        ],
    );
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
            .unwrap_or_else(|| "a legacy plugin".to_owned());
        let accepted = conflict
            .accepted
            .as_ref()
            .map(topology_writer_label)
            .unwrap_or_else(|| "a legacy plugin".to_owned());
        kv.push(
            "topology writers",
            verdict(
                tally,
                Health::Info,
                format!(
                    "contained a stale writer: {stale} lost to {accepted}; {} writes rejected, last {} ago — {}",
                    conflict.rejected_count,
                    age_label(conflict.age_secs),
                    conflict.fix
                ),
            ),
        );
    }
}

/// The presence-plugin block: a verdict line per loaded plugin, then the
/// identity and traffic that explain it.
///
/// The plugin is the sidebar's eyes on Zellij, so the reader's question is
/// "does the sidebar still see my panes, and if not what do I run". Each row
/// leads with that answer and its remedy; build hash, load time, and the
/// telemetry window follow as subordinate evidence. Counters the report can
/// derive stay off the human surface — `--json` keeps every raw field.
fn push_presence_plugins(
    kv: &mut KeyVals,
    tally: &mut Tally,
    plugins: &PresencePlugins,
    server_version: Option<&str>,
) {
    let desired = plugins.desired_build.as_deref().map(short_build);
    if plugins.rows.is_empty() {
        let want = desired
            .as_deref()
            .map(|build| format!(" (want build {build})"))
            .unwrap_or_default();
        kv.push(
            "presence plugin",
            verdict(
                tally,
                Health::Warn,
                format!("none loaded{want} — the sidebar cannot see panes; run `rimz reload`"),
            ),
        );
        push_presence_telemetry_log(kv, plugins);
        return;
    }

    let many = plugins.rows.len() > 1;
    if many {
        kv.push(
            "presence plugins",
            verdict(
                tally,
                Health::Warn,
                format!(
                    "{} loaded — only one may write pane topology; run `rimz reload`",
                    plugins.rows.len()
                ),
            ),
        );
    }
    for row in &plugins.rows {
        let key = if many {
            format!("  plugin #{}", row.plugin_id)
        } else {
            "presence plugin".to_owned()
        };
        let mut lines = vec![
            vec![presence_plugin_verdict(tally, row)],
            vec![
                cell(presence_plugin_identity(
                    row,
                    desired.as_deref(),
                    server_version,
                    many,
                ))
                .fg(palette::body()),
            ],
        ];
        if let Some(traffic) = presence_plugin_traffic(row) {
            lines.push(vec![cell(traffic).fg(palette::faint())]);
        }
        kv.push_lines(key, lines);
    }
    push_presence_telemetry_log(kv, plugins);
}

/// What this plugin is doing for the sidebar right now, and the fix when that
/// answer is unwelcome.
fn presence_plugin_verdict(tally: &mut Tally, row: &PresencePluginRow) -> Cell {
    if row.status == PresencePluginStatus::Rejected {
        return verdict(
            tally,
            Health::Warn,
            format!(
                "a newer plugin took over — {} of its topology writes were ignored; run `rimz reload`",
                row.rejected_count.unwrap_or_default()
            ),
        );
    }
    if row.status == PresencePluginStatus::Inactive {
        return verdict(tally, Health::Info, "loaded; not writing pane topology");
    }
    let telemetry = row.telemetry.as_ref();
    if telemetry.is_some_and(failing_recently) {
        // The traffic line below owns every count; this line owns the cause.
        let cause = telemetry
            .and_then(|telemetry| telemetry.last_failure.as_ref())
            .map_or_else(
                || "pane discovery lags; run `rimz reload`".to_owned(),
                failure_cause,
            );
        return verdict(
            tally,
            Health::Warn,
            format!("writing pane topology; wakes are failing — {cause}"),
        );
    }
    if row.outdated {
        return verdict(
            tally,
            Health::Info,
            "writing pane topology on an outdated build; run `rimz reload`",
        );
    }
    verdict(tally, Health::Ok, "writing pane topology")
}

/// Whether the plugin reported command failures while its telemetry was still
/// fresh. Stale telemetry describes a plugin that is no longer reporting, and
/// the identity line already says so.
fn failing_recently(telemetry: &PresencePluginTelemetry) -> bool {
    let topology = telemetry.topology_failures_delta.unwrap_or_default();
    let other = telemetry.other_failures_delta.unwrap_or_default();
    let fresh =
        telemetry.last_seen_age_secs <= rimz::sidebar::timing::PRESENCE_STAMP_FRESH.as_secs();
    fresh && (topology > 0 || other > 0)
}

/// The host's own account of the failure, falling back to the exit status when
/// it died without saying anything.
fn failure_cause(failure: &super::model::PresenceCommandFailure) -> String {
    if !failure.detail.is_empty() {
        return failure.detail.clone();
    }
    match failure.exit_code {
        Some(code) => format!("the wake exited {code} without reporting a cause"),
        None => "the wake was killed before it could report".to_owned(),
    }
}

/// Which build is loaded, when it loaded, and how recently it reported in.
fn presence_plugin_identity(
    row: &PresencePluginRow,
    desired: Option<&str>,
    server_version: Option<&str>,
    keyed_by_id: bool,
) -> String {
    let mut parts = Vec::new();
    parts.push(match (row.build.as_deref().map(short_build), desired) {
        (Some(build), Some(desired)) if row.outdated => {
            format!("build {build}, outdated (want {desired})")
        }
        (Some(build), Some(_)) => format!("build {build}, current"),
        (Some(build), None) => format!("build {build}"),
        (None, _) => "build unknown".to_owned(),
    });
    if let Some(loaded_at_ms) = row.loaded_at_ms {
        parts.push(format!("loaded {}", plugin_loaded_time(loaded_at_ms)));
    }
    match &row.telemetry {
        Some(telemetry) => parts.push(format!(
            "last report {} ago",
            age_label(telemetry.last_seen_age_secs)
        )),
        None => parts.push("no telemetry yet".to_owned()),
    }
    // The server's own version already heads this section; repeat it only when
    // the plugin loaded under a different one, which a restart resolves.
    if let Some(version) = row
        .telemetry
        .as_ref()
        .and_then(|telemetry| telemetry.zellij_version.as_deref())
        && server_version.is_none_or(|server| !server.contains(version))
    {
        parts.push(format!("loaded under zellij {version}"));
    }
    if !keyed_by_id {
        parts.push(format!("plugin #{}", row.plugin_id));
    }
    parts.join(" · ")
}

/// The work and memory the plugin logged across its telemetry window. Deltas
/// only mean something against the span that produced them, so the window
/// leads.
fn presence_plugin_traffic(row: &PresencePluginRow) -> Option<String> {
    let telemetry = row.telemetry.as_ref()?;
    if telemetry.sample_count < 2 {
        return Some("one telemetry sample so far; no trend yet".to_owned());
    }
    let mut parts = Vec::new();
    let window_secs = telemetry.last_at_ms.saturating_sub(telemetry.first_at_ms) / 1_000;
    if window_secs > 0 {
        parts.push(format!("last {}", age_label(window_secs)));
    }
    parts.push(format!("{} commands", telemetry.commands_completed_delta));
    let topology = telemetry.topology_failures_delta.unwrap_or_default();
    let other = telemetry.other_failures_delta.unwrap_or_default();
    if topology > 0 {
        parts.push(format!("{topology} failed to apply topology"));
    }
    if other > 0 {
        parts.push(format!("{other} other failures"));
    }
    if topology == 0 && other == 0 {
        parts.push("all applied".to_owned());
    }
    if let Some(rejected) = telemetry
        .stale_writer_rejections_delta
        .filter(|delta| *delta > 0)
    {
        parts.push(format!("{rejected} writes rejected as stale"));
    }
    // `bytes` is `pages * 64 KiB`; one of the two is enough, and bytes are the
    // ones a reader can judge.
    parts.push(match telemetry.byte_growth {
        0 => "memory steady".to_owned(),
        growth if growth > 0 => format!("memory +{}", fmt_bytes(growth.unsigned_abs())),
        growth => format!("memory -{}", fmt_bytes(growth.unsigned_abs())),
    });
    Some(parts.join(" · "))
}

fn push_presence_telemetry_log(kv: &mut KeyVals, plugins: &PresencePlugins) {
    if let Some(path) = plugins.history.first() {
        let rotated = if plugins.history.len() > 1 {
            " (+ rotated .1)"
        } else {
            ""
        };
        kv.push(
            "telemetry log",
            cell(format!("{path}{rotated}")).fg(palette::faint()),
        );
    }
}

fn server_version(version: &Version) -> Option<&str> {
    match version {
        Version::Reported { version } => Some(version.as_str()),
        Version::Unknown | Version::Unavailable { .. } => None,
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

/// Name a writer generation the way the presence-plugin rows do, so the two
/// rows describe the same plugin in the same words.
fn topology_writer_label(writer: &super::model::TopologyWriterId) -> String {
    format!(
        "plugin #{} loaded {}",
        writer.plugin_id,
        plugin_loaded_time(writer.loaded_at_ms)
    )
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

/// The log's verdict: one line per issue worth a human's time, then a single
/// line accounting for the lifecycle records the room provokes by running.
///
/// Expected records outnumber real ones by orders of magnitude in a busy room,
/// so they earn a count and a naming, never a line each. An issue under
/// investigation leads with what it means, and only an alarm carries its raw
/// record, because that is the only case where the reader needs the forensics.
fn render_mux_log_notes(w: &mut impl Write, log: &MuxLog, tally: &mut Tally) -> io::Result<()> {
    let MuxLog::Ready {
        problem_records,
        omitted_issue_groups,
        issues,
        ..
    } = log
    else {
        return Ok(());
    };
    if *problem_records == 0 {
        return note(tally, w, Health::Ok, "log: nothing to report");
    }
    let (expected, investigate): (Vec<_>, Vec<_>) = issues
        .iter()
        .partition(|issue| issue.state != DoctorState::Investigate);

    if investigate.is_empty() {
        note(tally, w, Health::Ok, "log: nothing needing attention")?;
    }
    for issue in investigate {
        let health = doctor_health(issue.state, issue.impact);
        note(
            tally,
            w,
            health,
            &format!("{} ({})", issue.summary, issue_span(issue)),
        )?;
        if issue.impact == DoctorImpact::Alarm {
            for sample in &issue.samples {
                detail(w, palette::muted(), sample.trim_end())?;
            }
            if issue.evidence_truncated {
                detail(w, palette::muted(), "evidence truncated at 8 KiB")?;
            }
        }
    }
    if !expected.is_empty() {
        let occurrences: usize = expected.iter().map(|issue| issue.occurrences).sum();
        note(
            tally,
            w,
            Health::Info,
            &format!(
                "{occurrences} records are routine room lifecycle: {}",
                naming(expected.iter().map(|issue| issue.summary.as_str()))
            ),
        )?;
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

/// How often an issue fired and how recently, in the reader's own terms.
fn issue_span(issue: &super::model::MuxLogIssue) -> String {
    let count = match issue.occurrences {
        1 => "once".to_owned(),
        count => format!("{count}×"),
    };
    let now = Timestamp::now();
    // A burst inside one second has no span worth naming; reporting it as
    // "over 0s" reads as a broken clock rather than a tight cluster.
    let span_secs = match (issue.first_occurrence, issue.last_occurrence) {
        (Some(first), Some(last)) => u64::try_from(last.duration_since(first).as_secs())
            .ok()
            .filter(|secs| *secs > 0),
        _ => None,
    };
    match (span_secs, issue.first_occurrence, issue.last_occurrence) {
        (Some(secs), _, Some(last)) => format!(
            "{count} over {}, last {}",
            age_label(secs),
            age_short(now, last)
        ),
        (_, Some(at), None) | (_, _, Some(at)) => format!("{count}, {}", age_short(now, at)),
        (_, None, None) => count,
    }
}

/// Name the first few of a set and count the rest, so a fold still says what it
/// swallowed.
fn naming<'a>(summaries: impl Iterator<Item = &'a str>) -> String {
    const NAMED: usize = 3;
    let all: Vec<_> = summaries.collect();
    let named = all
        .iter()
        .take(NAMED)
        .copied()
        .collect::<Vec<_>>()
        .join(" · ");
    match all.len().saturating_sub(NAMED) {
        0 => named,
        rest => format!("{named} · and {rest} more"),
    }
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

/// Hook wiring, sorted by what the reader can act on.
///
/// A working agent is worth one word, so the installed set collapses to a
/// single roll-up and the table holds only the agents asking for a command.
/// Agents absent from the machine stay in a closing aside: they are the
/// section's largest group and its least interesting one. Names render plain
/// here — provider color marks agents at work, and a wiring audit is not that.
fn render_hooks(w: &mut impl Write, report: &DoctorReport, tally: &mut Tally) -> io::Result<()> {
    section(w, tally, "HOOKS")?;
    let mut installed = Vec::new();
    let mut needs_action = Vec::new();
    let mut unsupported = Vec::new();
    let mut absent = Vec::new();
    for row in &report.hooks {
        match &row.status {
            HookStatus::Installed => installed.push(row.kind.as_str()),
            HookStatus::InstalledUntrusted { .. } | HookStatus::NotInstalled { .. } => {
                needs_action.push(row);
            }
            HookStatus::Unsupported { reason } if row.detected => {
                unsupported.push((row.kind.as_str(), reason.as_str()));
            }
            HookStatus::NotDetected | HookStatus::Unsupported { .. } => {
                absent.push(row.kind.as_str());
            }
        }
    }

    if installed.is_empty() {
        writeln!(
            w,
            "  {}",
            paint(palette::faint(), "no agent reports to RimZ yet")
        )?;
    } else {
        installed.sort_unstable();
        writeln!(
            w,
            "  {}",
            paint(
                palette::muted(),
                &format!(
                    "{} reporting to RimZ: {}",
                    installed.len(),
                    installed.join(", ")
                )
            )
        )?;
    }

    if !needs_action.is_empty() {
        let mut table = Table::new(["", "AGENT", "STATUS", "FIX"]);
        for row in needs_action {
            let (health, fix) = match &row.status {
                HookStatus::InstalledUntrusted { events, fix } => (
                    Health::Warn,
                    format!(
                        "silently skips untrusted hooks ({}) — {fix}",
                        events.join(", ")
                    ),
                ),
                HookStatus::NotInstalled { fix } => (Health::Alarm, fix.clone()),
                _ => unreachable!("only actionable statuses reach the table"),
            };
            table.row([
                badge(tally, health),
                cell(row.kind.as_str()),
                cell(row.status.label()).fg(style_of(health)),
                cell(fix).dash(),
            ]);
        }
        table.render(w)?;
    }

    for (kind, reason) in unsupported {
        note(
            tally,
            w,
            Health::Neutral,
            &format!("{kind} is installed but cannot carry RimZ hooks"),
        )?;
        detail(w, palette::faint(), reason)?;
    }

    if !absent.is_empty() {
        absent.sort_unstable();
        note(
            tally,
            w,
            Health::Neutral,
            &format!("not found on this machine: {}", absent.join(", ")),
        )?;
        detail(
            w,
            palette::faint(),
            "RimZ offers their hooks as soon as one appears",
        )?;
    }
    Ok(())
}

fn render_plugins(w: &mut impl Write, report: &DoctorReport, tally: &mut Tally) -> io::Result<()> {
    if report.plugins.is_empty() {
        return Ok(());
    }
    section(w, tally, "AGENT PLUGINS")?;
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
    section(w, tally, "LOOP TASKS")?;
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
    section(w, tally, "REMOTE CONTROL")?;
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
    section(w, tally, "STORAGE")?;
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
    section(w, tally, "PROTOCOLS")?;
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
    section(w, tally, "TRUST")?;
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
    section(w, tally, "AGENTS")?;
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
    section(w, tally, "MESSAGES")?;
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
    section(w, tally, "DIAGNOSTICS")?;
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
        // An incident RimZ already understood — expected, contained, or
        // recovered — is evidence that the machinery worked. Those earn a
        // counted line by kind; the table is for what is still open.
        Diagnostics::Ready { path, incidents } => {
            let (settled, open): (Vec<_>, Vec<_>) = incidents
                .iter()
                .partition(|incident| incident.state != DoctorState::Investigate);
            writeln!(
                w,
                "  {}",
                paint(
                    palette::muted(),
                    &format!("{} recent incidents ({path})", incidents.len())
                )
            )?;
            let now_ms = rimz::sidebar::timing::unix_now_ms();
            let mut table = Table::new(["", "KIND", "SEEN", "SUMMARY"]).right(&[2]);
            for incident in &open {
                let health = doctor_health(incident.state, incident.impact);
                table.row([
                    badge(tally, health),
                    cell(incident.kind.as_str()).fg(style_of(health)),
                    cell(incident_seen(now_ms, incident)),
                    cell(incident_summary(incident)).fg(palette::body()),
                ]);
            }
            if !open.is_empty() {
                table.render(w)?;
            }
            if !settled.is_empty() {
                let mut kinds: Vec<_> = settled
                    .iter()
                    .map(|incident| incident.kind.as_str())
                    .collect();
                kinds.sort_unstable();
                kinds.dedup();
                note(
                    tally,
                    w,
                    Health::Info,
                    &format!(
                        "{} handled and closed themselves: {}",
                        settled.len(),
                        naming(kinds.into_iter())
                    ),
                )?;
            }
            Ok(())
        }
    }
}

fn incident_seen(now_ms: u64, incident: &super::model::DiagIncident) -> String {
    if incident.first_at_ms == incident.last_at_ms {
        return age_ms_short(now_ms, incident.last_at_ms);
    }
    format!(
        "{} to {}",
        age_ms_short(now_ms, incident.first_at_ms),
        age_ms_short(now_ms, incident.last_at_ms)
    )
}

fn incident_summary(incident: &super::model::DiagIncident) -> String {
    let mut summary = incident.summary.clone();
    if incident.record_count > 1 {
        summary.push_str(&format!(" · {} records", incident.record_count));
    }
    if incident.sink_suppressed > 0 {
        summary.push_str(&format!(" · {} suppressed", incident.sink_suppressed));
    }
    if incident.stale_build
        && let Some(build) = &incident.build
    {
        // ponytail: SUMMARY is the unpadded final column; add styled Cell spans
        // before giving this table a max width.
        summary.push_str(&paint(palette::muted(), &format!(" · old build {build}")));
    }
    summary
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
    section(w, tally, "LAST INCIDENT")?;
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

/// The closing line: how many findings, which sections hold them, and what the
/// two glyphs mean, so the count is a place to go rather than a number.
fn render_tally(w: &mut impl Write, tally: &Tally) -> io::Result<()> {
    writeln!(w)?;
    if tally.alarms.is_empty() && tally.warns.is_empty() {
        return writeln!(
            w,
            "{}",
            paint(palette::good(), "✓ everything checked is healthy")
        );
    }
    let mut parts = Vec::new();
    if !tally.alarms.is_empty() {
        parts.push(format!(
            "✗ {} in {}",
            plural(tally.alarms.len(), "problem"),
            sections(&tally.alarms)
        ));
    }
    if !tally.warns.is_empty() {
        parts.push(format!(
            "! {} in {}",
            plural(tally.warns.len(), "warning"),
            sections(&tally.warns)
        ));
    }
    let headline = if tally.alarms.is_empty() {
        palette::warn()
    } else {
        palette::alarm()
    };
    writeln!(w, "{}", paint(headline, &parts.join("  ·  ")))?;
    writeln!(
        w,
        "{}",
        paint(
            palette::muted(),
            "  ✗ marks something broken, with the command that fixes it beside it; ! marks something degraded that still works",
        )
    )
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
