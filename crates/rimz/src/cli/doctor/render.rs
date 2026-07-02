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
use rimz::diag::record::DiagSeverity;
use rimz::trust::TrustState;

use super::model::{
    AgentCounts, AgentRollup, Capabilities, Diagnostics, DoctorReport, HookStatus, Host, LoopTasks,
    MessageProblemRow, Messages, Mux, Presence, Probe, RemoteControl, SessionHealth, Storage,
    Terminal, Trust, Version, Workspace,
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
    match health {
        Health::Ok => ("✓", palette::GOOD),
        Health::Warn => ("⚠", palette::WARN),
        Health::Alarm => ("✗", palette::ALARM),
        Health::Info => ("●", palette::COOL),
        Health::Neutral => ("·", palette::FAINT),
    }
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

fn style_of(health: Health) -> anstyle::Style {
    parts(health).1
}

/// Open a titled section: a blank line then the heading in the accent tone.
fn section(w: &mut impl Write, title: &str) -> io::Result<()> {
    writeln!(w)?;
    writeln!(w, "{}", paint(palette::ACCENT.bold(), title))
}

/// A hanging note under a section: an indented `glyph text` line.
fn note(tally: &mut Tally, w: &mut impl Write, health: Health, text: &str) -> io::Result<()> {
    tally.record(health);
    let (glyph, style) = parts(health);
    writeln!(w, "    {}", paint(style, &format!("{glyph} {text}")))
}

pub(super) fn render_human(report: &DoctorReport, w: &mut impl Write) -> io::Result<()> {
    let mut tally = Tally::default();
    render_identity(w, report.version, &report.host)?;
    render_workspace(w, &report.workspace, &mut tally)?;
    render_mux(w, &report.mux, &mut tally)?;
    render_terminal(w, &report.terminal, &mut tally)?;
    render_hooks(w, report, &mut tally)?;
    render_loop(w, &report.loop_tasks, &mut tally)?;
    render_remote_control(w, &report.remote_control, &mut tally)?;
    render_storage(w, &report.storage, &mut tally)?;

    if let Some(protocols) = &report.protocols {
        section(w, "PROTOCOLS")?;
        let mut kv = KeyVals::new().indent(2);
        kv.push("event", cell(protocols.event));
        kv.push("sidebar", cell(protocols.sidebar));
        kv.push("resolver", cell(protocols.resolver));
        kv.render(w)?;
        for warning in &protocols.warnings {
            note(&mut tally, w, Health::Warn, warning)?;
        }
    }

    if let Some(trust) = &report.trust {
        render_trust(w, trust, &mut tally)?;
    }
    render_resolver_heartbeats(w, report, &mut tally)?;
    render_agents(w, report, &mut tally)?;
    render_messages(w, report, &mut tally)?;
    render_diagnostics(w, report, &mut tally)?;
    render_tally(w, &tally)?;
    Ok(())
}

fn render_identity(w: &mut impl Write, version: &str, host: &Host) -> io::Result<()> {
    writeln!(w, "{}", paint(palette::ACCENT.bold(), "Rimz doctor"))?;
    let mut kv = KeyVals::new().indent(2);
    kv.push("version", cell(version));
    let user = match &host.user {
        Some(name) => format!("{name} (uid {})", host.uid),
        None => format!("uid {}", host.uid),
    };
    kv.push("user", cell(user));
    kv.push(
        "binary",
        cell(host.binary.as_deref().unwrap_or("unknown")).fg(palette::BODY),
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
        .fg(palette::FAINT),
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
            kv.push("id", cell(ws.workspace_id.as_str()).fg(palette::ACCENT));
            kv.push(
                "project root",
                cell(ws.project_root.as_str()).fg(palette::BODY),
            );
            kv.push("root class", cell(ws.root_class.label()));
            kv.push(
                "worktree root",
                cell(ws.worktree_root.as_str()).fg(palette::BODY),
            );
            kv.push(
                "worktree branch",
                cell(ws.worktree_branch.as_deref().unwrap_or("<detached>")),
            );
            kv.push("session", cell(ws.session_name.as_str()));
            match &ws.sock_headroom {
                Probe::Unavailable { error } => kv.push(
                    "sock headroom",
                    verdict(tally, Health::Alarm, format!("unavailable ({error})")),
                ),
                Probe::Ready(budget) => {
                    let health = if budget.fits {
                        Health::Ok
                    } else {
                        Health::Alarm
                    };
                    let label = if budget.fits { "OK" } else { "TOO LONG" };
                    kv.push(
                        "sock headroom",
                        verdict(
                            tally,
                            health,
                            format!(
                                "{label} ({}/{} bytes for {})",
                                budget.used,
                                budget.limit,
                                budget.dir.as_str()
                            ),
                        ),
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
            kv.push(
                "multiplexer",
                verdict(tally, Health::Alarm, format!("unavailable ({error})")),
            );
            return kv.render(w);
        }
        Probe::Ready(mux) => mux,
    };

    let mut kv = KeyVals::new().indent(2);
    kv.push("backend", cell(mux.name.to_string()).fg(palette::ACCENT));
    match &mux.version {
        Version::Reported { version } => kv.push("version", cell(version.as_str())),
        Version::Unknown => kv.push("version", cell("unknown").fg(palette::FAINT)),
        Version::Unavailable { error } => kv.push(
            "version",
            verdict(tally, Health::Warn, format!("unavailable ({error})")),
        ),
    }
    match &mux.capabilities {
        Capabilities::Zellij(Probe::Ready(caps)) => {
            kv.push(
                "zellij floor",
                floor_cell(tally, caps.meets_min_version, caps.min_version),
            );
        }
        Capabilities::Zellij(Probe::Unavailable { error }) => kv.push(
            "zellij floor",
            verdict(tally, Health::Warn, format!("unavailable ({error})")),
        ),
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
        Capabilities::Tmux(Probe::Unavailable { error }) => kv.push(
            "tmux floor",
            verdict(tally, Health::Warn, format!("unavailable ({error})")),
        ),
    }
    if let Some(socket) = &mux.socket {
        kv.push("socket", cell(socket.as_str()).fg(palette::BODY));
    }
    if let Some(socket) = &mux.zellij_socket {
        let health = if socket.fits {
            Health::Ok
        } else {
            Health::Alarm
        };
        let label = if socket.fits { "OK" } else { "TOO LONG" };
        kv.push(
            "zellij socket",
            verdict(
                tally,
                health,
                format!(
                    "{label} ({}/{} bytes for {})",
                    socket.len, socket.limit, socket.path
                ),
            ),
        );
        if let Some(fix) = &socket.fix {
            kv.push("fix", verdict(tally, Health::Warn, fix));
        }
    }
    if let Some(health) = &mux.session_health {
        let value = match health {
            Probe::Unavailable { error } => {
                verdict(tally, Health::Warn, format!("unavailable ({error})"))
            }
            Probe::Ready(SessionHealth::Ok) => verdict(tally, Health::Ok, "ok"),
            Probe::Ready(SessionHealth::Stuck { fix }) => verdict(
                tally,
                Health::Alarm,
                format!("stuck (resurrected/suspended panes) — {fix}"),
            ),
        };
        kv.push("session health", value);
    }
    if let Some(presence) = &mux.presence {
        let value = match presence {
            Presence::Event { poked_secs } => verdict(
                tally,
                Health::Ok,
                format!("event mode (poked {poked_secs}s ago)"),
            ),
            Presence::Poll { reason } => {
                verdict(tally, Health::Warn, format!("poll mode — {reason}"))
            }
            Presence::Unavailable { error } => {
                verdict(tally, Health::Warn, format!("unavailable ({error})"))
            }
        };
        kv.push("presence", value);
    }
    kv.render(w)?;

    if let Some(Probe::Ready(dup)) = &mux.duplicate_sessions {
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
                writeln!(
                    w,
                    "      {}",
                    paint(
                        palette::BODY,
                        &format!(
                            "{here}{}: {} sidebars ({panes})",
                            group.session_name, group.sidebar_count
                        )
                    )
                )?;
            }
            if let Some(advice) = &dup.advice {
                note(tally, w, Health::Warn, advice)?;
            }
        }
    } else if let Some(Probe::Unavailable { error }) = &mux.duplicate_sessions {
        note(
            tally,
            w,
            Health::Warn,
            &format!("duplicate sessions: unavailable ({error})"),
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
    let mut table = Table::new(["", "AGENT", "STATUS", "FIX"]);
    for row in &report.hooks {
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
            HookStatus::Unsupported { reason } => (Health::Neutral, reason.clone()),
        };
        let fix = if fix.is_empty() { "-".to_owned() } else { fix };
        table.row([
            badge(tally, health),
            cell(row.kind.as_str()).fg(palette::ACCENT),
            cell(row.status.label()).fg(style_of(health)),
            cell(fix).dash(),
        ]);
    }
    table.render(w)
}

fn render_loop(w: &mut impl Write, loop_tasks: &LoopTasks, tally: &mut Tally) -> io::Result<()> {
    section(w, "LOOP TASKS")?;
    if loop_tasks.tasks.is_empty() {
        return writeln!(w, "  {}", paint(palette::FAINT, "none configured"));
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
            cell(row.name.as_str()).fg(palette::ACCENT),
            cell(row.spec.as_str()),
            cell(row.when.as_str()).fg(style_of(health)),
            cell(home_relative(&row.root)).fg(palette::BODY),
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
            kv.push("remote control", cell("off").fg(palette::FAINT));
            kv.render(w)
        }
        RemoteControl::On {
            agents,
            refusals,
            skipped,
        } => {
            let mut kv = KeyVals::new().indent(2);
            for agent in agents {
                let (name, rest) = agent
                    .label
                    .split_once(' ')
                    .unwrap_or((agent.label.as_str(), ""));
                let health = if agent.ready {
                    Health::Ok
                } else {
                    Health::Warn
                };
                kv.push(name.to_owned(), verdict(tally, health, rest));
            }
            kv.render(w)?;
            for refusal in refusals {
                note(tally, w, Health::Alarm, "`rimz start` refuses:")?;
                for line in refusal.lines() {
                    writeln!(w, "      {}", paint(palette::MUTED, line))?;
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
                    writeln!(w, "      {}", paint(palette::MUTED, line))?;
                }
            }
            Ok(())
        }
    }
}

fn render_storage(w: &mut impl Write, storage: &Storage, tally: &mut Tally) -> io::Result<()> {
    section(w, "STORAGE")?;
    writeln!(
        w,
        "  {}",
        paint(
            palette::MUTED,
            &format!("rimz on disk: {}", fmt_bytes(storage.total_bytes))
        )
    )?;
    let mut table = Table::new(["", "AREA", "SIZE", "PATH"]).right(&[2]);
    for root in &storage.roots {
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
                palette::BODY
            } else {
                palette::FAINT
            }),
        ]);
    }
    table.render(w)
}

fn render_trust(w: &mut impl Write, trust: &Probe<Trust>, tally: &mut Tally) -> io::Result<()> {
    section(w, "TRUST")?;
    let mut kv = KeyVals::new().indent(2);
    let value = match trust {
        Probe::Unavailable { error } => {
            verdict(tally, Health::Alarm, format!("unavailable ({error})"))
        }
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

fn render_resolver_heartbeats(
    w: &mut impl Write,
    report: &DoctorReport,
    tally: &mut Tally,
) -> io::Result<()> {
    let Some(probe) = &report.resolver_heartbeats else {
        return Ok(());
    };
    match probe {
        Probe::Unavailable { error } => {
            section(w, "RESOLVER HEARTBEATS")?;
            note(tally, w, Health::Warn, &format!("unavailable ({error})"))
        }
        Probe::Ready(ids) if !ids.is_empty() => {
            section(w, "RESOLVER HEARTBEATS")?;
            for id in ids {
                note(
                    tally,
                    w,
                    Health::Warn,
                    &format!("unauthorized resolver heartbeat seen ({id})"),
                )?;
            }
            Ok(())
        }
        // No unauthorized heartbeats is the quiet, healthy case.
        Probe::Ready(_) => Ok(()),
    }
}

fn render_agents(w: &mut impl Write, report: &DoctorReport, tally: &mut Tally) -> io::Result<()> {
    let Some(rollup) = &report.agents else {
        return Ok(());
    };
    section(w, "AGENTS")?;
    match rollup {
        AgentRollup::Unavailable { error } => {
            note(tally, w, Health::Alarm, &format!("unavailable ({error})"))
        }
        AgentRollup::None => writeln!(w, "  {}", paint(palette::FAINT, "none observed")),
        AgentRollup::Observed { counts, rows } => {
            writeln!(w, "  {}", paint(palette::MUTED, &agent_counts_line(counts)))?;
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
                    cell(agent.kind.as_str()),
                    cell(agent.agent_id.as_str()).fg(palette::ACCENT),
                    cell(agent.branch.as_deref().unwrap_or("-")).dash(),
                    cell(status_label(agent.status)).fg(style),
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
            return note(tally, w, Health::Alarm, &format!("unavailable ({error})"));
        }
        Probe::Ready(messages) => messages,
    };
    if messages.open.total() == 0
        && messages.stuck.is_empty()
        && messages.recent_failures.is_empty()
    {
        return writeln!(w, "  {}", paint(palette::FAINT, "no open messages"));
    }
    if messages.open.total() == 0 {
        writeln!(
            w,
            "  {}",
            paint(palette::MUTED, "0 open — `rimz message list`")
        )?;
    } else {
        writeln!(
            w,
            "  {}",
            paint(
                palette::MUTED,
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
        cell(row.message_id.as_str()).fg(palette::ACCENT),
        cell(row.status.as_str()).fg(style_of(health)),
        cell(row.target.as_str()).fg(palette::META),
        cell(age_short(now, row.at)),
        cell(row.problem.as_str()).fg(palette::BODY),
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
    match diagnostics {
        Diagnostics::Unavailable => writeln!(w, "  {}", paint(palette::FAINT, "unavailable")),
        Diagnostics::Ready { path, records } if records.is_empty() => writeln!(
            w,
            "  {}",
            paint(palette::FAINT, &format!("no recent records ({path})"))
        ),
        Diagnostics::Ready { path, records } => {
            writeln!(
                w,
                "  {}",
                paint(
                    palette::MUTED,
                    &format!("{} recent records ({path})", records.len())
                )
            )?;
            let now_ms = rimz::sidebar::timing::unix_now_ms();
            let mut table = Table::new(["", "SEVERITY", "KIND", "SEEN", "SUMMARY"]).right(&[3]);
            for record in records {
                let health = severity_health(record.severity);
                table.row([
                    badge(tally, health),
                    cell(severity_label(record.severity)).fg(style_of(health)),
                    cell(record.kind.as_str()),
                    cell(age_ms_short(now_ms, record.at_ms)),
                    cell(record.summary.as_str()).fg(palette::BODY),
                ]);
            }
            table.render(w)
        }
    }
}

fn render_tally(w: &mut impl Write, tally: &Tally) -> io::Result<()> {
    writeln!(w)?;
    if tally.alarms > 0 {
        writeln!(
            w,
            "{}",
            paint(
                palette::ALARM,
                &format!(
                    "✗ {}, ⚠ {}",
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
                palette::WARN,
                &format!("⚠ {}", plural(tally.warns, "warning"))
            )
        )
    } else {
        writeln!(w, "{}", paint(palette::GOOD, "✓ no problems found"))
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

fn status_label(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Running => "running",
        AgentStatus::Waiting => "waiting",
        AgentStatus::Idle => "idle",
        AgentStatus::Success => "success",
        AgentStatus::Failed => "failed",
        AgentStatus::Paused => "paused",
    }
}

fn severity_health(severity: DiagSeverity) -> Health {
    match severity {
        DiagSeverity::Info => Health::Info,
        DiagSeverity::Warn => Health::Warn,
        DiagSeverity::Error => Health::Alarm,
    }
}

fn severity_label(severity: DiagSeverity) -> &'static str {
    match severity {
        DiagSeverity::Info => "info",
        DiagSeverity::Warn => "warn",
        DiagSeverity::Error => "error",
    }
}

/// Relative age of `then` from `now`, rendered compactly (`5s ago`, `2m ago`).
fn age_short(now: Timestamp, then: Timestamp) -> String {
    let span = now.duration_since(then);
    if span.is_negative() {
        return "now".to_owned();
    }
    let secs = span.as_secs().max(0) as u64;
    age_label(secs)
}

fn age_ms_short(now_ms: u64, then_ms: u64) -> String {
    age_label(now_ms.saturating_sub(then_ms) / 1_000)
}

fn age_label(secs: u64) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::doctor::model::{
        HookRow, Host, LoopTaskRow, MessageProblemRow, OpenCounts, RemoteAgent, StorageRootView,
        TmuxCaps,
    };
    use rimz::ids::MuxName;

    fn strip(
        render_one: impl FnOnce(&mut anstream::StripStream<Vec<u8>>) -> io::Result<()>,
    ) -> String {
        let mut stream = anstream::StripStream::new(Vec::new());
        render_one(&mut stream).expect("render to in-memory buffer");
        String::from_utf8(stream.into_inner()).expect("utf-8")
    }

    fn terminal_fixture() -> Terminal {
        Terminal {
            theme_mode: rimz::config::ThemeMode::Auto,
            truecolor_advertised: true,
            resolved_depth: "truecolor",
            colorterm: None,
            term: Some("xterm-ghostty".to_owned()),
            terminfo_truecolor: true,
            fix: None,
        }
    }

    fn storage_fixture() -> Storage {
        Storage {
            total_bytes: 0,
            roots: Vec::new(),
        }
    }

    fn report_fixture() -> DoctorReport {
        DoctorReport {
            version: crate::cli::version::VERSION,
            host: Host {
                user: None,
                uid: 0,
                binary: None,
            },
            workspace: Probe::Unavailable {
                error: "test".to_owned(),
            },
            mux: Probe::Unavailable {
                error: "test".to_owned(),
            },
            terminal: terminal_fixture(),
            hooks: Vec::new(),
            loop_tasks: LoopTasks { tasks: Vec::new() },
            remote_control: RemoteControl::Off,
            storage: storage_fixture(),
            protocols: None,
            trust: None,
            resolver_heartbeats: None,
            agents: None,
            messages: None,
            diagnostics: None,
        }
    }

    #[test]
    fn render_identity_shows_user_and_binary() {
        let host = Host {
            user: Some("eddie".to_owned()),
            uid: 1001,
            binary: Some("/home/eddie/.cargo/bin/rimz".to_owned()),
        };
        let out = strip(|w| render_identity(w, "0.1.0", &host));
        assert!(out.contains("Rimz doctor"), "{out}");
        assert!(out.contains("0.1.0"), "{out}");
        assert!(out.contains("eddie (uid 1001)"), "{out}");
        assert!(out.contains("/home/eddie/.cargo/bin/rimz"), "{out}");
    }

    #[test]
    fn terminal_section_renders_depth_signals_and_fix() {
        let terminal = Terminal {
            truecolor_advertised: false,
            resolved_depth: "256",
            term: Some("xterm-256color".to_owned()),
            terminfo_truecolor: false,
            fix: Some("set `[theme] mode = \"truecolor\"` to force RGB".to_owned()),
            ..terminal_fixture()
        };
        let out = strip(|w| {
            let mut tally = Tally::default();
            render_terminal(w, &terminal, &mut tally)
        });
        assert!(out.contains("TERMINAL"), "section title:\n{out}");
        assert!(out.contains("256 (mode auto)"), "resolved depth:\n{out}");
        assert!(out.contains("truecolor-advertised=false"), "{out}");
        assert!(out.contains("COLORTERM=unset"), "{out}");
        assert!(out.contains("TERM=xterm-256color"), "{out}");
        assert!(out.contains("terminfo-truecolor=false"), "{out}");
        assert!(
            out.contains("mode = \"truecolor\""),
            "fix command is present:\n{out}"
        );
    }

    #[test]
    fn hooks_section_renders_glyph_status_and_fix() {
        let report = DoctorReport {
            hooks: vec![
                HookRow {
                    kind: "claude".to_owned(),
                    status: HookStatus::Installed,
                },
                HookRow {
                    kind: "codex".to_owned(),
                    status: HookStatus::NotInstalled {
                        fix: "run `rimz hooks install codex` to wire codex agents".to_owned(),
                    },
                },
            ],
            ..report_fixture()
        };
        let out = strip(|w| {
            let mut tally = Tally::default();
            render_hooks(w, &report, &mut tally)
        });
        assert!(out.contains("HOOKS"), "section title:\n{out}");
        assert!(out.contains("✓"), "installed carries a check:\n{out}");
        assert!(out.contains("✗"), "missing carries a cross:\n{out}");
        assert!(out.contains("installed"), "{out}");
        assert!(out.contains("not installed"), "{out}");
        assert!(
            out.contains("rimz hooks install codex"),
            "fix command in the table:\n{out}"
        );
    }

    #[test]
    fn loop_section_lists_tasks_and_flags_invalid_ones() {
        let loop_tasks = LoopTasks {
            tasks: vec![
                LoopTaskRow {
                    name: "morning".to_owned(),
                    spec: "claude".to_owned(),
                    when: "07:00 on weekdays".to_owned(),
                    root: "/home/you/code/app".to_owned(),
                    valid: true,
                },
                LoopTaskRow {
                    name: "broken".to_owned(),
                    spec: "codex".to_owned(),
                    when: "invalid: bad time".to_owned(),
                    root: "/home/you/code/other".to_owned(),
                    valid: false,
                },
            ],
        };
        let out = strip(|w| {
            let mut tally = Tally::default();
            render_loop(w, &loop_tasks, &mut tally)
        });
        assert!(out.contains("LOOP TASKS"), "section title:\n{out}");
        assert!(
            out.contains("morning") && out.contains("07:00 on weekdays"),
            "{out}"
        );
        assert!(
            out.contains("broken") && out.contains("invalid: bad time"),
            "{out}"
        );
        assert!(
            out.contains('✗'),
            "an invalid schedule carries a cross:\n{out}"
        );
        assert!(
            out.contains("rimz loop list"),
            "the installed-state hint is present:\n{out}"
        );
    }

    #[test]
    fn mux_section_shows_backend_socket() {
        let mux = Mux {
            name: MuxName::Tmux,
            version: Version::Reported {
                version: "tmux 3.5".to_owned(),
            },
            capabilities: Capabilities::Tmux(Probe::Ready(TmuxCaps {
                meets_min_version: true,
                min_version: (3, 5, 0),
                popup_supported: true,
            })),
            zellij_socket: None,
            socket: Some("/tmp/tmux-1001/default".to_owned()),
            session_health: None,
            duplicate_sessions: None,
            presence: None,
        };
        let out = strip(|w| {
            let mut tally = Tally::default();
            render_mux(w, &Probe::Ready(mux), &mut tally)
        });
        assert!(out.contains("MULTIPLEXER"), "{out}");
        assert!(out.contains("/tmp/tmux-1001/default"), "{out}");
    }

    #[test]
    fn loop_section_reads_empty_when_unconfigured() {
        let out = strip(|w| {
            let mut tally = Tally::default();
            render_loop(w, &LoopTasks { tasks: Vec::new() }, &mut tally)
        });
        assert!(out.contains("LOOP TASKS"), "{out}");
        assert!(out.contains("none configured"), "{out}");
    }

    #[test]
    fn storage_section_renders_total_and_roots() {
        let storage = Storage {
            total_bytes: 13_018,
            roots: vec![
                StorageRootView {
                    label: "state",
                    path: "/home/you/.local/state/rimz".to_owned(),
                    bytes: 13_018,
                    present: true,
                },
                StorageRootView {
                    label: "runtime",
                    path: "/run/user/1000/rimz".to_owned(),
                    bytes: 0,
                    present: false,
                },
            ],
        };
        let out = strip(|w| {
            let mut tally = Tally::default();
            render_storage(w, &storage, &mut tally)
        });
        assert!(out.contains("STORAGE"), "section title:\n{out}");
        assert!(out.contains("rimz on disk: 13 KB"), "total:\n{out}");
        assert!(
            out.contains("state") && out.contains("13 KB") && out.contains(".local/state/rimz"),
            "present root row:\n{out}"
        );
        assert!(
            out.contains("runtime") && out.contains("-") && out.contains("/run/user/1000/rimz"),
            "absent root row:\n{out}"
        );
    }

    #[test]
    fn messages_section_renders_stuck_and_failure_rows() {
        let mut report = report_fixture();
        report.messages = Some(Probe::Ready(Messages {
            open: OpenCounts {
                queued: 2,
                claimed: 0,
                sent: 1,
            },
            stuck: vec![MessageProblemRow {
                message_id: "msg_stuck".to_owned(),
                status: "queued".to_owned(),
                target: "@coder".to_owned(),
                at: Timestamp::UNIX_EPOCH,
                problem: "attempts 3, pane rejected".to_owned(),
            }],
            recent_failures: vec![MessageProblemRow {
                message_id: "msg_failed".to_owned(),
                status: "errored".to_owned(),
                target: "codex:sess-1".to_owned(),
                at: Timestamp::UNIX_EPOCH,
                problem: "pane rejected input".to_owned(),
            }],
        }));
        let out = strip(|w| {
            let mut tally = Tally::default();
            render_messages(w, &report, &mut tally)?;
            render_tally(w, &tally)
        });
        assert!(out.contains("MESSAGES"), "{out}");
        assert!(out.contains("3 open: 2 queued, 1 sent"), "{out}");
        assert!(out.contains("msg_stuck") && out.contains("@coder"), "{out}");
        assert!(
            out.contains("msg_failed") && out.contains("pane rejected input"),
            "{out}"
        );
        assert!(
            out.contains("✗ 1 problem, ⚠ 1 warning"),
            "mixed verdict counts message rows:\n{out}"
        );
    }

    #[test]
    fn messages_section_renders_empty_state() {
        let mut report = report_fixture();
        report.messages = Some(Probe::Ready(Messages {
            open: OpenCounts::default(),
            stuck: Vec::new(),
            recent_failures: Vec::new(),
        }));
        let out = strip(|w| {
            let mut tally = Tally::default();
            render_messages(w, &report, &mut tally)
        });
        assert!(out.contains("MESSAGES"), "{out}");
        assert!(out.contains("no open messages"), "{out}");
    }

    #[test]
    fn tally_renders_clean_verdict() {
        let out = strip(|w| render_tally(w, &Tally::default()));
        assert!(out.contains("✓ no problems found"), "{out}");
    }

    #[test]
    fn remote_agent_label_splits_into_key_and_verdict() {
        let _ = RemoteAgent {
            label: "claude ready".to_owned(),
            ready: true,
        };
        let out = strip(|w| {
            let mut tally = Tally::default();
            render_remote_control(
                w,
                &RemoteControl::On {
                    agents: vec![RemoteAgent {
                        label: "claude enabled, blocked".to_owned(),
                        ready: false,
                    }],
                    refusals: vec!["disableRemoteControl: true".to_owned()],
                    skipped: vec!["managed standalone Codex install is missing".to_owned()],
                },
                &mut tally,
            )
        });
        assert!(out.contains("claude"), "{out}");
        assert!(out.contains("enabled, blocked"), "{out}");
        assert!(out.contains("`rimz start` refuses"), "{out}");
        assert!(out.contains("disableRemoteControl: true"), "{out}");
        assert!(
            out.contains("enabled but not installed")
                && out.contains("skipped (the room still starts)"),
            "{out}"
        );
        assert!(
            out.contains("managed standalone Codex install is missing"),
            "{out}"
        );
    }
}
