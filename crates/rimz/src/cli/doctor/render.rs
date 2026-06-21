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
use rimz::schema::diag::DiagSeverity;
use rimz::trust::TrustState;

use super::model::{
    AgentRollup, Capabilities, Diagnostics, DoctorReport, HookStatus, LoopTasks, Mux, Presence,
    Probe, RemoteControl, Rooms, SessionHealth, Storage, Terminal, Trust, Version, Workspace,
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

/// A glyph-only cell for a table's status column.
fn badge(health: Health) -> Cell {
    let (glyph, style) = parts(health);
    cell(glyph).fg(style)
}

/// A `glyph text` value cell, both painted in the verdict's tone.
fn verdict(health: Health, text: impl Into<String>) -> Cell {
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
fn note(w: &mut impl Write, health: Health, text: &str) -> io::Result<()> {
    let (glyph, style) = parts(health);
    writeln!(w, "    {}", paint(style, &format!("{glyph} {text}")))
}

pub(super) fn render_human(report: &DoctorReport, w: &mut impl Write) -> io::Result<()> {
    writeln!(w, "{}", paint(palette::ACCENT.bold(), "Rimz doctor"))?;
    let mut kv = KeyVals::new().indent(2);
    kv.push("version", cell(report.version));
    kv.render(w)?;
    render_workspace(w, &report.workspace)?;
    render_mux(w, &report.mux)?;

    section(w, "SIDEBAR")?;
    let mut kv = KeyVals::new().indent(2);
    kv.push("renderer", cell(report.sidebar_renderer));
    kv.render(w)?;

    render_terminal(w, &report.terminal)?;
    render_hooks(w, report)?;
    render_loop(w, &report.loop_tasks)?;
    render_remote_control(w, &report.remote_control)?;
    render_rooms(w, &report.rooms)?;
    render_storage(w, &report.storage)?;

    if let Some(protocols) = &report.protocols {
        section(w, "PROTOCOLS")?;
        let mut kv = KeyVals::new().indent(2);
        kv.push("event", cell(protocols.event));
        kv.push("sidebar", cell(protocols.sidebar));
        kv.push("resolver", cell(protocols.resolver));
        kv.render(w)?;
        for warning in &protocols.warnings {
            note(w, Health::Warn, warning)?;
        }
    }

    if let Some(trust) = &report.trust {
        render_trust(w, trust)?;
    }
    render_resolver_heartbeats(w, report)?;
    render_agents(w, report)?;
    render_diagnostics(w, report)?;
    Ok(())
}

fn render_terminal(w: &mut impl Write, terminal: &Terminal) -> io::Result<()> {
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
        kv.push("fix", verdict(Health::Warn, fix));
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

fn render_workspace(w: &mut impl Write, workspace: &Probe<Workspace>) -> io::Result<()> {
    section(w, "WORKSPACE")?;
    let mut kv = KeyVals::new().indent(2);
    match workspace {
        Probe::Unavailable { error } => {
            kv.push(
                "workspace",
                verdict(Health::Alarm, format!("could not resolve ({error})")),
            );
            kv.render(w)
        }
        Probe::Ready(ws) => {
            kv.push("id", cell(ws.workspace_id.as_str()).fg(palette::ACCENT));
            kv.push(
                "project root",
                cell(home_relative(&ws.project_root)).fg(palette::BODY),
            );
            kv.push("root class", cell(ws.root_class.label()));
            kv.push(
                "worktree root",
                cell(home_relative(&ws.worktree_root)).fg(palette::BODY),
            );
            kv.push(
                "worktree branch",
                cell(ws.worktree_branch.as_deref().unwrap_or("<detached>")),
            );
            kv.push("session", cell(ws.session_name.as_str()));
            match &ws.sock_headroom {
                Probe::Unavailable { error } => kv.push(
                    "sock headroom",
                    verdict(Health::Alarm, format!("unavailable ({error})")),
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
                            health,
                            format!(
                                "{label} ({}/{} bytes for {})",
                                budget.used,
                                budget.limit,
                                home_relative(&budget.dir)
                            ),
                        ),
                    );
                    if let Some(remedy) = &budget.remedy {
                        kv.push("remedy", verdict(Health::Warn, remedy));
                    }
                }
            }
            kv.render(w)
        }
    }
}

fn render_mux(w: &mut impl Write, mux: &Probe<Mux>) -> io::Result<()> {
    section(w, "MULTIPLEXER")?;
    let mux = match mux {
        Probe::Unavailable { error } => {
            let mut kv = KeyVals::new().indent(2);
            kv.push(
                "multiplexer",
                verdict(Health::Alarm, format!("unavailable ({error})")),
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
            verdict(Health::Warn, format!("unavailable ({error})")),
        ),
    }
    match &mux.capabilities {
        Capabilities::Zellij(Probe::Ready(caps)) => {
            kv.push(
                "zellij floor",
                floor_cell(caps.meets_min_version, caps.min_version),
            );
        }
        Capabilities::Zellij(Probe::Unavailable { error }) => kv.push(
            "zellij floor",
            verdict(Health::Warn, format!("unavailable ({error})")),
        ),
        Capabilities::Tmux(Probe::Ready(caps)) => {
            kv.push(
                "tmux floor",
                floor_cell(caps.meets_min_version, caps.min_version),
            );
            let (maj, min, patch) = caps.min_version;
            let popup = if caps.popup_supported {
                verdict(Health::Ok, "supported")
            } else {
                verdict(
                    Health::Warn,
                    format!("unavailable (requires tmux >= {maj}.{min}.{patch})"),
                )
            };
            kv.push("tmux popup", popup);
        }
        Capabilities::Tmux(Probe::Unavailable { error }) => kv.push(
            "tmux floor",
            verdict(Health::Warn, format!("unavailable ({error})")),
        ),
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
                health,
                format!(
                    "{label} ({}/{} bytes for {})",
                    socket.len, socket.limit, socket.path
                ),
            ),
        );
        if let Some(fix) = &socket.fix {
            kv.push("fix", verdict(Health::Warn, fix));
        }
    }
    if let Some(health) = &mux.session_health {
        let value = match health {
            Probe::Unavailable { error } => verdict(Health::Warn, format!("unavailable ({error})")),
            Probe::Ready(SessionHealth::Ok) => verdict(Health::Ok, "ok"),
            Probe::Ready(SessionHealth::Stuck { fix }) => verdict(
                Health::Alarm,
                format!("stuck (resurrected/suspended panes) — {fix}"),
            ),
        };
        kv.push("session health", value);
    }
    if let Some(presence) = &mux.presence {
        let value = match presence {
            Presence::Event { poked_secs } => {
                verdict(Health::Ok, format!("event mode (poked {poked_secs}s ago)"))
            }
            Presence::Poll { reason } => verdict(Health::Warn, format!("poll mode — {reason}")),
            Presence::Unavailable { error } => {
                verdict(Health::Warn, format!("unavailable ({error})"))
            }
        };
        kv.push("presence", value);
    }
    kv.render(w)?;

    if let Some(Probe::Ready(dup)) = &mux.duplicate_sessions {
        if dup.groups.is_empty() {
            note(w, Health::Ok, "duplicate sessions: none")?;
        } else {
            note(
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
                note(w, Health::Warn, advice)?;
            }
        }
    } else if let Some(Probe::Unavailable { error }) = &mux.duplicate_sessions {
        note(
            w,
            Health::Warn,
            &format!("duplicate sessions: unavailable ({error})"),
        )?;
    }
    Ok(())
}

fn floor_cell(meets: bool, min: (u32, u32, u32)) -> Cell {
    let (maj, min_v, patch) = min;
    let (health, label) = if meets {
        (Health::Ok, "OK")
    } else {
        (Health::Alarm, "TOO OLD")
    };
    verdict(
        health,
        format!("{label} (>= {maj}.{min_v}.{patch} required)"),
    )
}

fn render_hooks(w: &mut impl Write, report: &DoctorReport) -> io::Result<()> {
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
            badge(health),
            cell(row.kind.as_str()).fg(palette::ACCENT),
            cell(row.status.label()).fg(style_of(health)),
            cell(fix).dash(),
        ]);
    }
    table.render(w)
}

fn render_loop(w: &mut impl Write, loop_tasks: &LoopTasks) -> io::Result<()> {
    section(w, "LOOP TASKS")?;
    if loop_tasks.tasks.is_empty() {
        return writeln!(w, "  {}", paint(palette::FAINT, "none configured"));
    }
    let mut table = Table::new(["", "NAME", "SPEC", "WHEN", "ROOT"]);
    for row in &loop_tasks.tasks {
        let health = if row.valid {
            Health::Info
        } else {
            Health::Alarm
        };
        table.row([
            badge(health),
            cell(row.name.as_str()).fg(palette::ACCENT),
            cell(row.spec.as_str()),
            cell(row.when.as_str()).fg(style_of(health)),
            cell(home_relative(&row.root)).fg(palette::BODY),
        ]);
    }
    table.render(w)?;
    note(w, Health::Neutral, "`rimz loop list` shows installed state")
}

fn render_remote_control(w: &mut impl Write, remote: &RemoteControl) -> io::Result<()> {
    section(w, "REMOTE CONTROL")?;
    match remote {
        RemoteControl::Unavailable { error } => {
            let mut kv = KeyVals::new().indent(2);
            kv.push(
                "remote control",
                verdict(Health::Alarm, format!("config unavailable ({error})")),
            );
            kv.render(w)
        }
        RemoteControl::Off => {
            let mut kv = KeyVals::new().indent(2);
            kv.push("remote control", cell("off").fg(palette::FAINT));
            kv.render(w)
        }
        RemoteControl::On { agents, refusals } => {
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
                kv.push(name.to_owned(), verdict(health, rest));
            }
            kv.render(w)?;
            for refusal in refusals {
                writeln!(
                    w,
                    "    {}",
                    paint(palette::ALARM, "✗ `rimz start` refuses:")
                )?;
                for line in refusal.lines() {
                    writeln!(w, "      {}", paint(palette::MUTED, line))?;
                }
            }
            Ok(())
        }
    }
}

fn render_rooms(w: &mut impl Write, rooms: &Probe<Rooms>) -> io::Result<()> {
    section(w, "ROOMS")?;
    let rooms = match rooms {
        Probe::Unavailable { error } => {
            return note(w, Health::Alarm, &format!("unavailable ({error})"));
        }
        Probe::Ready(rooms) => rooms,
    };
    if rooms.recorded == 0 {
        return writeln!(w, "  {}", paint(palette::FAINT, "none recorded"));
    }
    writeln!(
        w,
        "  {}",
        paint(
            palette::MUTED,
            &format!("{} recorded, {} live", rooms.recorded, rooms.live)
        )
    )?;
    let mut table = Table::new(["", "SESSION", "ROOT", "CLASS", "STATE"]);
    for room in &rooms.rooms {
        let here = if room.is_current { "* " } else { "" };
        let (health, state) = if room.live {
            (Health::Ok, "live")
        } else {
            (Health::Neutral, "idle")
        };
        table.row([
            badge(health),
            cell(format!("{here}{}", room.session_name)),
            cell(home_relative(&room.project_root)).fg(palette::BODY),
            cell(room.root_class.label()),
            cell(state).fg(style_of(health)),
        ]);
    }
    table.render(w)?;
    for overlap in &rooms.overlaps {
        note(
            w,
            Health::Warn,
            &format!(
                "`{}` and `{}` nest; an agent belongs to the room its pane lives in",
                overlap.a, overlap.b
            ),
        )?;
    }
    Ok(())
}

fn render_storage(w: &mut impl Write, storage: &Storage) -> io::Result<()> {
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
            badge(Health::Neutral),
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

fn render_trust(w: &mut impl Write, trust: &Probe<Trust>) -> io::Result<()> {
    section(w, "TRUST")?;
    let mut kv = KeyVals::new().indent(2);
    let value = match trust {
        Probe::Unavailable { error } => verdict(Health::Alarm, format!("unavailable ({error})")),
        Probe::Ready(Trust { state, granted_at }) => match state {
            TrustState::Trusted => verdict(
                Health::Ok,
                format!(
                    "trusted (granted {})",
                    granted_at.as_deref().unwrap_or("<unknown>")
                ),
            ),
            TrustState::Stale => verdict(
                Health::Alarm,
                "stale (executable surface drifted; run `rimz trust grant` to refresh)",
            ),
            TrustState::Untrusted => verdict(
                Health::Warn,
                "untrusted (run `rimz trust grant` to enable command paths)",
            ),
            TrustState::NoConfig => verdict(Health::Neutral, "no project config"),
        },
    };
    kv.push("trust", value);
    kv.render(w)
}

fn render_resolver_heartbeats(w: &mut impl Write, report: &DoctorReport) -> io::Result<()> {
    let Some(probe) = &report.resolver_heartbeats else {
        return Ok(());
    };
    match probe {
        Probe::Unavailable { error } => {
            section(w, "RESOLVER HEARTBEATS")?;
            note(w, Health::Warn, &format!("unavailable ({error})"))
        }
        Probe::Ready(ids) if !ids.is_empty() => {
            section(w, "RESOLVER HEARTBEATS")?;
            for id in ids {
                note(
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

fn render_agents(w: &mut impl Write, report: &DoctorReport) -> io::Result<()> {
    let Some(rollup) = &report.agents else {
        return Ok(());
    };
    section(w, "AGENTS OBSERVED")?;
    match rollup {
        AgentRollup::Unavailable { error } => {
            note(w, Health::Alarm, &format!("unavailable ({error})"))
        }
        AgentRollup::None => writeln!(w, "  {}", paint(palette::FAINT, "none observed")),
        AgentRollup::Observed { groups } => {
            let now = Timestamp::now();
            let mut table = Table::new(["", "KIND", "ID", "BRANCH", "STATUS", "SEEN"]);
            for group in groups {
                for agent in &group.agents {
                    let health = status_health(agent.status);
                    let style = status::agent(agent.status, agent.phase);
                    table.row([
                        badge(health),
                        cell(group.kind.as_str()),
                        cell(agent.agent_id.as_str()).fg(palette::ACCENT),
                        cell(agent.branch.as_deref().unwrap_or("-")).dash(),
                        cell(status_label(agent.status)).fg(style),
                        cell(age_short(now, agent.last_seen)),
                    ]);
                }
            }
            table.render(w)
        }
    }
}

fn render_diagnostics(w: &mut impl Write, report: &DoctorReport) -> io::Result<()> {
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
            let now_ms = rimz::sidebar::cache::unix_now_ms();
            let mut table = Table::new(["", "SEVERITY", "KIND", "SEEN", "SUMMARY"]).right(&[3]);
            for record in records {
                let health = severity_health(record.severity);
                table.row([
                    badge(health),
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
    use crate::cli::doctor::model::{HookRow, LoopTaskRow, RemoteAgent, StorageRootView};

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
        let out = strip(|w| render_terminal(w, &terminal));
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
            version: crate::cli::version::VERSION,
            workspace: Probe::Unavailable {
                error: "test".to_owned(),
            },
            mux: Probe::Unavailable {
                error: "test".to_owned(),
            },
            sidebar_renderer: "built into rimz",
            terminal: terminal_fixture(),
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
            loop_tasks: LoopTasks { tasks: Vec::new() },
            remote_control: RemoteControl::Off,
            rooms: Probe::Ready(Rooms {
                recorded: 0,
                live: 0,
                rooms: Vec::new(),
                overlaps: Vec::new(),
            }),
            storage: storage_fixture(),
            protocols: None,
            trust: None,
            resolver_heartbeats: None,
            agents: None,
            diagnostics: None,
        };
        let out = strip(|w| render_hooks(w, &report));
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
        let out = strip(|w| render_loop(w, &loop_tasks));
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
    fn loop_section_reads_empty_when_unconfigured() {
        let out = strip(|w| render_loop(w, &LoopTasks { tasks: Vec::new() }));
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
        let out = strip(|w| render_storage(w, &storage));
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
    fn remote_agent_label_splits_into_key_and_verdict() {
        let _ = RemoteAgent {
            label: "claude ready".to_owned(),
            ready: true,
        };
        let out = strip(|w| {
            render_remote_control(
                w,
                &RemoteControl::On {
                    agents: vec![RemoteAgent {
                        label: "claude enabled, blocked".to_owned(),
                        ready: false,
                    }],
                    refusals: vec!["disableRemoteControl: true".to_owned()],
                },
            )
        });
        assert!(out.contains("claude"), "{out}");
        assert!(out.contains("enabled, blocked"), "{out}");
        assert!(out.contains("`rimz start` refuses"), "{out}");
        assert!(out.contains("disableRemoteControl: true"), "{out}");
    }
}
