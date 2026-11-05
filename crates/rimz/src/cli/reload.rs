//! Per-room reload report, pending room settings, and targeted fixes.
//!
//! The library owns upgrade, optional repair and room judgments. This handler
//! presents those results plus the machine-wide web restart, then fails only
//! when the report's closing block names something that needs the user.

use std::io::Write;
use std::path::Path;

use anyhow::Result;
use clap::Args;
use unicode_width::UnicodeWidthStr;

use super::GlobalFlags;
use crate::cli::render::{self, palette, prose::StyledFragment, status};
use rimz::mux::room_settings::{AppliesWhen, PendingSetting, PendingSettings};
use rimz::reload::{
    MachineProblem, ReloadReport, RoomProblem, RoomReload, RoomState, reload_user_sidebars,
};

#[derive(Debug, Args)]
pub struct ReloadArgs {
    /// Repair sidebar panes and reopen closed rimzd tabs before reporting.
    #[arg(long)]
    repair: bool,
}

pub fn run(args: ReloadArgs, _globals: &GlobalFlags) -> Result<()> {
    let mut result = reload_user_sidebars(args.repair)?;
    match rimz::web::restart_if_online(&super::machine_config()) {
        Ok(web) => {
            if let Some(writable) = &web.writable {
                render::web_warnings(&writable.warnings);
            }
            if let Some(share) = &web.share {
                render::web_warnings(&share.warnings);
            }
            result.machine.web_restarted = web.writable.is_some() || web.share.is_some();
        }
        Err(error) => result.machine.web_error = Some(error.to_string()),
    }
    let mut out = render::out();
    let needs_you = report(&mut out, &result, render::terminal_columns(80))?;
    out.flush()?;
    if needs_you > 0 {
        return Err(reload_error(needs_you));
    }
    Ok(())
}

fn report(out: &mut impl Write, result: &ReloadReport, width: usize) -> Result<usize> {
    let mut rooms: Vec<_> = result
        .rooms
        .iter()
        .map(|room| (room, render::home_relative_path(&room.project_root)))
        .collect();
    rooms.sort_by(|(a, a_path), (b, b_path)| (a.state(), a_path).cmp(&(b.state(), b_path)));
    let needs_you = result.needs_attention();
    if rooms.is_empty() {
        writeln!(out, "No running sidebars to reload.")?;
        wrapped(
            out,
            vec![StyledFragment::plain(
                "Launch one with `rimz start` or `rimz attach`.",
            )],
            width,
            "",
            "",
        )?;
    } else {
        headline(out, result, width)?;
        writeln!(out)?;
        let columns = Columns::for_rooms(&rooms, width);
        for (room, path) in &rooms {
            room_line(out, room, path, &columns, width)?;
        }
    }
    extras(out, result, width)?;
    pending(out, &rooms, width)?;
    if needs_you > 0 {
        writeln!(out)?;
        wrapped(
            out,
            vec![StyledFragment::styled(
                format!("! {}", attention(needs_you)),
                palette::warn().bold(),
            )],
            width,
            "",
            "",
        )?;
        for (room, _) in &rooms {
            for problem in room.problems() {
                let (phrase, fix) = room_problem(&problem, room)?;
                problem_entry(out, &room.session_name, &phrase, &fix, width)?;
            }
        }
        for problem in result.machine.problems() {
            let (phrase, fix) = machine_problem(problem);
            problem_entry(out, "machine", &phrase, fix, width)?;
        }
    }
    Ok(needs_you)
}

fn headline(out: &mut impl Write, result: &ReloadReport, width: usize) -> Result<()> {
    let healthy = result
        .rooms
        .iter()
        .filter(|r| r.state() == RoomState::Healthy)
        .count();
    let settling = result
        .rooms
        .iter()
        .filter(|r| r.state() == RoomState::Settling)
        .count();
    let needs_you = result.needs_attention();
    let state = if needs_you > 0 {
        RoomState::NeedsYou
    } else if settling > 0 {
        RoomState::Settling
    } else {
        RoomState::Healthy
    };
    let (glyph, tone) = state_style(state);
    let tally = if state == RoomState::Healthy {
        count(result.rooms.len(), "room")
    } else {
        format!("{healthy} of {}", count(result.rooms.len(), "room"))
    };
    let mut fragments = vec![
        StyledFragment::styled(format!("{glyph} {tally} on rimz"), tone.bold()),
        StyledFragment::styled(rimz::build_id::VERSION, palette::muted()),
    ];
    if settling > 0 {
        fragments.push(StyledFragment::styled(
            format!("· {settling} settling"),
            tone.bold(),
        ));
    }
    if needs_you > 0 {
        fragments.push(StyledFragment::styled(
            format!("· {}", attention(needs_you)),
            tone.bold(),
        ));
    }
    wrapped(out, fragments, width, "", "")
}

struct Columns {
    path: usize,
    mux: usize,
    session: Option<usize>,
    counts: bool,
}

impl Columns {
    fn for_rooms(rooms: &[(&RoomReload, String)], width: usize) -> Self {
        let mux = rooms
            .iter()
            .map(|(r, _)| r.mux.as_str().len())
            .max()
            .unwrap_or(0);
        let mut session = (width >= 80).then(|| {
            rooms
                .iter()
                .map(|(r, _)| UnicodeWidthStr::width(r.session_name.as_str()))
                .max()
                .unwrap_or(0)
        });
        let count_width = rooms
            .iter()
            .map(|(r, _)| UnicodeWidthStr::width(sidebar_label(r).as_str()))
            .max()
            .unwrap_or(0)
            + 2;
        let min_path = rooms
            .iter()
            .map(|(_, path)| {
                UnicodeWidthStr::width(path.as_str())
                    .min(UnicodeWidthStr::width(path_tail(path).as_str()))
            })
            .max()
            .unwrap_or(0);
        let fixed = 4 + 2 + mux;
        if fixed + session.map_or(0, |n| n + 2) + count_width + min_path > width {
            session = None;
        }
        let fixed = fixed + session.map_or(0, |n| n + 2);
        let counts = width >= 40 && fixed + count_width + min_path <= width;
        let fixed = fixed + if counts { count_width } else { 0 };
        let path = rooms
            .iter()
            .map(|(_, p)| UnicodeWidthStr::width(p.as_str()))
            .max()
            .unwrap_or(0)
            .min(width.saturating_sub(fixed));
        Self {
            path,
            mux,
            session,
            counts,
        }
    }
}

fn path_tail(path: &str) -> String {
    let path = Path::new(path);
    let tail = path
        .ancestors()
        .nth(2)
        .and_then(|root| path.strip_prefix(root).ok())
        .unwrap_or(path);
    format!("…/{}", tail.display())
}

fn room_line(
    out: &mut impl Write,
    room: &RoomReload,
    path: &str,
    columns: &Columns,
    width: usize,
) -> Result<()> {
    let (glyph, tone) = state_style(room.state());
    let path = if UnicodeWidthStr::width(path) <= columns.path {
        path.to_owned()
    } else {
        render::truncate_left_width(&path_tail(path), columns.path)
    };
    let path_pad = columns
        .path
        .saturating_sub(UnicodeWidthStr::width(path.as_str()));
    let mux = format!("{:width$}", room.mux.as_str(), width = columns.mux);
    write!(
        out,
        "  {} {}{}  {}",
        render::paint(tone, glyph),
        render::paint(palette::body().bold(), &path),
        " ".repeat(path_pad),
        render::paint(palette::muted(), &mux)
    )?;
    let mut used = 4 + columns.path + 2 + columns.mux;
    if let Some(size) = columns.session {
        let pad = size.saturating_sub(UnicodeWidthStr::width(room.session_name.as_str()));
        write!(
            out,
            "  {}{}",
            render::paint(palette::muted(), &room.session_name),
            " ".repeat(pad)
        )?;
        used += size + 2;
    }
    if columns.counts {
        let label = sidebar_label(room);
        write!(out, "  {}", render::paint(palette::accent(), &label))?;
        used += UnicodeWidthStr::width(label.as_str()) + 2;
    }
    let mut phrases: Vec<_> = room
        .problems()
        .iter()
        .map(|problem| room_problem(problem, room).map(|(phrase, _)| phrase))
        .collect::<Result<_>>()?;
    if room.state() == RoomState::Settling {
        phrases.push(format!("{} settling, retries on its own", room.settling()));
    }
    let suffix = phrases.join(" · ");
    if suffix.is_empty() {
        writeln!(out)?;
    } else if used + 3 + UnicodeWidthStr::width(suffix.as_str()) <= width {
        writeln!(out, " {} {suffix}", render::paint(palette::faint(), "·"))?;
    } else {
        writeln!(out)?;
        wrapped(
            out,
            vec![StyledFragment::plain(format!("· {suffix}"))],
            width,
            "      ",
            "        ",
        )?;
    }
    Ok(())
}

fn sidebar_label(room: &RoomReload) -> String {
    let (current, total) = room.sidebar_counts();
    if room.state() == RoomState::Settling {
        format!("{current} of {}", count(total, "sidebar"))
    } else {
        count(total, "sidebar")
    }
}

fn extras(out: &mut impl Write, result: &ReloadReport, width: usize) -> Result<()> {
    let machine = &result.machine;
    let mut items = Vec::new();
    for (n, noun, verb) in [
        (machine.stats_reloaded, "stats dashboard", "reloaded"),
        (machine.brokers_retired, "language-server broker", "retired"),
    ] {
        if n > 0 {
            items.push(format!("{} {verb}", count(n, noun)));
        }
    }
    if machine.brokers_marked > 0 {
        let verb = if machine.brokers_marked == 1 {
            "retires"
        } else {
            "retire"
        };
        items.push(format!(
            "{} {verb} when {} server stops",
            count(machine.brokers_marked, "broker"),
            if machine.brokers_marked == 1 {
                "its"
            } else {
                "their"
            }
        ));
    }
    let reaped = result
        .rooms
        .iter()
        .map(|room| room.outcome.reaped + room.repair.map_or(0, |repair| repair.reaped))
        .sum();
    if reaped > 0 {
        items.push(format!(
            "{} reaped",
            count(reaped, "orphaned sidebar process")
        ));
    }
    if machine.dead_swept > 0 {
        items.push(format!(
            "{} swept from stopped rooms",
            count(machine.dead_swept, "leftover process")
        ));
    }
    if machine.web_restarted {
        items.push("web daemon restarted".into());
    }
    if !items.is_empty() {
        writeln!(out)?;
        wrapped(
            out,
            vec![StyledFragment::plain(format!(
                "Also: {}",
                items.join(" · ")
            ))],
            width,
            "  ",
            "    ",
        )?;
    }
    let hosts: Vec<_> = result
        .rooms
        .iter()
        .filter(|room| room.claude_restarts)
        .map(|room| room.session_name.as_str())
        .collect();
    if !hosts.is_empty() {
        writeln!(out)?;
        wrapped(
            out,
            vec![StyledFragment::plain(format!(
                "Claude remote control restarts in {}; an open phone session drops.",
                hosts.join(", ")
            ))],
            width,
            "  ",
            "  ",
        )?;
    }
    Ok(())
}

fn pending(out: &mut impl Write, rooms: &[(&RoomReload, String)], width: usize) -> Result<()> {
    for (when, heading) in [
        (AppliesWhen::RoomStart, "Applies when the room next starts:"),
        (AppliesWhen::Attach, "Applies when you next attach:"),
    ] {
        let mut headed = false;
        for (room, _) in rooms {
            let PendingSettings::Tracked(settings) = &room.pending else {
                continue;
            };
            let settings: Vec<_> = settings.iter().filter(|s| s.when == when).collect();
            if settings.is_empty() {
                continue;
            }
            if !headed {
                writeln!(out)?;
                wrapped(
                    out,
                    vec![StyledFragment::styled(heading, palette::muted())],
                    width,
                    "  ",
                    "  ",
                )?;
                headed = true;
            }
            wrapped(
                out,
                vec![StyledFragment::styled(
                    &room.session_name,
                    palette::body().bold(),
                )],
                width,
                "    ",
                "    ",
            )?;
            for setting in settings {
                let (section, key) = setting.key.split_once('.').unwrap_or(("", &setting.key));
                wrapped(
                    out,
                    vec![
                        StyledFragment::styled(
                            format!("[{section}] {key}"),
                            palette::body().bold(),
                        ),
                        StyledFragment::styled(pending_value(setting), palette::muted()),
                    ],
                    width,
                    "      ",
                    "        ",
                )?;
            }
            let (verb, instruction, aside) = if when == AppliesWhen::RoomStart {
                ("reset", "", "(closes this room's agents)")
            } else {
                ("attach", "detach, then", "")
            };
            command_line(
                out,
                ("to apply now:", palette::muted()),
                instruction,
                &root_command(verb, room)?,
                aside,
                width,
            )?;
        }
    }
    let older = rooms
        .iter()
        .filter(|(room, _)| room.pending == PendingSettings::BeforeTracking)
        .count();
    if older > 0 {
        writeln!(out)?;
        wrapped(
            out,
            vec![StyledFragment::styled(
                format!(
                    concat!(
                        "{} started before RimZ tracked room settings; ",
                        "changes there show after {} next start."
                    ),
                    count(older, "room"),
                    if older == 1 { "its" } else { "their" }
                ),
                palette::muted(),
            )],
            width,
            "  ",
            "  ",
        )?;
    }
    Ok(())
}

fn reload_error(needs_you: usize) -> anyhow::Error {
    anyhow::anyhow!(
        "reload left {} that {} you; see the fixes above",
        count(needs_you, "item"),
        if needs_you == 1 { "needs" } else { "need" }
    )
}

fn pending_value(setting: &PendingSetting) -> String {
    let applied = if setting.when == AppliesWhen::Attach {
        "your last attach used"
    } else {
        "the room started with"
    };
    match (&setting.configured, &setting.recorded) {
        (Some(config), Some(recorded)) => {
            format!("is {config} in your config; {applied} {recorded}")
        }
        (None, Some(recorded)) => format!("is unset in your config; the room keeps {recorded}"),
        (config, None) => format!(
            "is {} in your config; {applied} your config.kdl value",
            config
                .as_ref()
                .map_or_else(|| "unset".into(), ToString::to_string)
        ),
    }
}

fn room_problem(problem: &RoomProblem, room: &RoomReload) -> Result<(String, String)> {
    let (phrase, fix) = match problem {
        RoomProblem::MissingSidebar => (
            "no sidebar reporting in".into(),
            "rimz reload --repair".into(),
        ),
        RoomProblem::Drainer => (
            "hook drainer still on the old build".into(),
            "rimz reload once its agents are idle".into(),
        ),
        RoomProblem::Record(error) => (
            format!("new build not recorded: {error}"),
            root_command("doctor", room)?,
        ),
        RoomProblem::Options(error) => (
            format!("tmux room options not applied: {error}"),
            "rimz reload".into(),
        ),
        RoomProblem::Plugin(error) => (
            format!("presence plugin not upgraded: {error}"),
            root_command("doctor", room)?,
        ),
        RoomProblem::RepairFailed => (
            "sidebar could not be repaired".into(),
            format!(
                "{}, then rimz reload --repair",
                root_command("attach", room)?
            ),
        ),
        RoomProblem::RepairDeferred => (
            "repair waits for an attached client".into(),
            format!(
                "{}, then rimz reload --repair",
                root_command("attach", room)?
            ),
        ),
        RoomProblem::Misdocked => (
            "sidebar working but not docked".into(),
            "rimz reload --repair".into(),
        ),
        RoomProblem::RepairBusy => (
            "another repair holds the room".into(),
            "rimz reload --repair once it finishes".into(),
        ),
        RoomProblem::DaemonReopen => (
            "rimzd tab could not be reopened".into(),
            "rimz reload --repair".into(),
        ),
        RoomProblem::PresenceSilent => (
            "presence plugin silent".into(),
            root_command("attach", room)?,
        ),
    };
    Ok((phrase, fix))
}

fn machine_problem(problem: MachineProblem) -> (String, &'static str) {
    match problem {
        MachineProblem::KeptBrokers(n) => (
            format!("{} too old to retire", count(n, "language-server broker")),
            "rimz reload once idle",
        ),
        MachineProblem::UncheckedBrokers => (
            "no readable rimz on PATH; brokers not checked".into(),
            "put rimz on PATH, then rimz reload",
        ),
        MachineProblem::Web(error) => (
            format!("web daemon restart failed: {error}"),
            "rimz web restart",
        ),
    }
}

fn problem_entry(
    out: &mut impl Write,
    label: &str,
    phrase: &str,
    fix: &str,
    width: usize,
) -> Result<()> {
    wrapped(
        out,
        vec![
            StyledFragment::styled("!", palette::warn()),
            StyledFragment::styled(format!("{label}:"), palette::body()),
            StyledFragment::plain(phrase),
        ],
        width,
        "  ",
        "    ",
    )?;
    command_line(out, ("fix:", palette::warn()), "", fix, "", width)
}

fn command_line(
    out: &mut impl Write,
    label: (&str, anstyle::Style),
    instruction: &str,
    command: &str,
    aside: &str,
    width: usize,
) -> Result<()> {
    let prefix_width = 6
        + UnicodeWidthStr::width(label.0)
        + if instruction.is_empty() {
            0
        } else {
            1 + UnicodeWidthStr::width(instruction)
        };
    let command_width = UnicodeWidthStr::width(command);
    let aside_width = if aside.is_empty() {
        0
    } else {
        3 + UnicodeWidthStr::width(aside)
    };
    let mut prefix = vec![StyledFragment::styled(label.0, label.1)];
    if !instruction.is_empty() {
        prefix.push(StyledFragment::styled(instruction, palette::muted()));
    }
    let command_column = if prefix_width + 1 + command_width + aside_width <= width {
        write!(out, "      ")?;
        for fragment in prefix {
            write!(out, "{} ", fragment.paint())?;
        }
        prefix_width + 1
    } else {
        wrapped(out, prefix, width, "      ", "        ")?;
        write!(out, "        ")?;
        8
    };
    write!(out, "{}", render::paint(palette::accent(), command))?;
    if !aside.is_empty() {
        if command_column + command_width + aside_width > width {
            writeln!(out)?;
            return wrapped(
                out,
                vec![StyledFragment::styled(aside, palette::muted())],
                width,
                "        ",
                "        ",
            );
        }
        write!(out, "   {}", render::paint(palette::muted(), aside))?;
    }
    writeln!(out)?;
    Ok(())
}

fn root_command(verb: &str, room: &RoomReload) -> Result<String> {
    Ok(format!(
        "rimz {verb} --root {}",
        shlex::try_quote(&room.project_root.to_string_lossy())?
    ))
}

fn state_style(state: RoomState) -> (&'static str, anstyle::Style) {
    match state {
        RoomState::NeedsYou => ("!", palette::warn()),
        RoomState::Settling => ("…", status::role(status::StateRole::Working)),
        RoomState::Healthy => ("✓", status::role(status::StateRole::Success)),
    }
}

fn wrapped(
    out: &mut impl Write,
    fragments: Vec<StyledFragment>,
    width: usize,
    indent: &str,
    hang: &str,
) -> Result<()> {
    for line in render::prose::wrap_fragments(fragments, width, indent, hang) {
        for fragment in line {
            write!(out, "{}", fragment.paint())?;
        }
        writeln!(out)?;
    }
    Ok(())
}

fn attention(n: usize) -> String {
    format!("{n} {} you", if n == 1 { "needs" } else { "need" })
}

fn count(n: usize, noun: &str) -> String {
    let plural = if n == 1 {
        ""
    } else if noun.ends_with("process") {
        "es"
    } else {
        "s"
    };
    format!("{n} {noun}{plural}")
}

#[cfg(test)]
mod tests;
