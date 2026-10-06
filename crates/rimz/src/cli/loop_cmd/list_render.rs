//! Grouped loop list text, with trigger-only wrapping and subscription collapse.

use super::*;
use unicode_width::UnicodeWidthStr;

pub(super) fn write(
    out: &mut impl Write,
    model: &ListModel,
    rooms: &[&Room],
    all: bool,
    width: Option<usize>,
) -> Result<()> {
    let total = model
        .rooms
        .iter()
        .map(|room| room.tasks.len())
        .sum::<usize>();
    if total == 0 {
        writeln!(out, "no loop tasks; add one with `rimz loop add`")?;
        return Ok(());
    }
    let timer = timer::active();
    for (index, room) in rooms.iter().enumerate() {
        if index > 0 {
            writeln!(out)?;
        }
        if room.tasks.is_empty() {
            writeln!(
                out,
                "no loop tasks in {} · {total} in other rooms: rimz loop list --all",
                ui::home_relative(&room.root.to_string_lossy())
            )?;
            continue;
        }
        heading(out, room, model.now, timer)?;
        needs_you(out, room, width)?;
        room_rows(out, room, model.now, width)?;
        worktree_rows(out, room, model.now, width)?;
    }
    if !all {
        let elsewhere = model
            .rooms
            .iter()
            .filter(|room| !room.here)
            .flat_map(|room| {
                room.tasks
                    .iter()
                    .filter(|row| row.attention.is_some())
                    .map(|row| {
                        format!(
                            "{} ({}) {}",
                            row.name,
                            ui::home_relative(&room.root.to_string_lossy()),
                            row.reason
                        )
                    })
            })
            .collect::<Vec<_>>();
        if !elsewhere.is_empty() {
            writeln!(
                out,
                "\nelsewhere: {} · rimz loop list --all",
                elsewhere.join(" · ")
            )?;
        }
    }
    Ok(())
}

fn heading(out: &mut impl Write, room: &Room, now: Timestamp, timer: bool) -> Result<()> {
    let mut parts = vec![
        ui::home_relative(&room.root.to_string_lossy()),
        if room.open {
            "room open".into()
        } else {
            "no room open".into()
        },
        format!(
            "{} task{}",
            room.tasks.len(),
            if room.tasks.len() == 1 { "" } else { "s" }
        ),
    ];
    if room.open
        && let Some(row) = room
            .tasks
            .iter()
            .filter(|row| row.running.is_none())
            .filter(|row| row.next_at.is_some())
            .min_by_key(|row| row.next_at)
    {
        // The filtered row has a next timestamp.
        parts.push(format!(
            "next: {} {}",
            row.name,
            ui::rel_until(row.next_at.unwrap(), now)
        ));
    }
    if room.spend_today_usd > 0.0 {
        parts.push(format!("${:.2} spent today", room.spend_today_usd));
    }
    if !room.open {
        parts.push(if timer { "clocks use the active timer; CI/PR signals wait until `rimz start`".into() } else { "clocks and CI/PR signals wait until `rimz start` (or `rimz loop timer install` for clocks)".into() });
    }
    writeln!(
        out,
        "{}",
        ui::paint(ui::palette::header(), &parts.join(" · "))
    )?;
    Ok(())
}

fn needs_you(out: &mut impl Write, room: &Room, width: Option<usize>) -> Result<()> {
    let rows = room
        .tasks
        .iter()
        .filter(|row| row.section == Section::NeedsYou)
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return Ok(());
    }
    writeln!(out, "\n{}", ui::paint(ui::palette::header(), "NEEDS YOU"))?;
    let cells = rows
        .iter()
        .map(|row| {
            (
                vec![row.name.clone(), row.head.clone(), row.reason.clone()],
                row.continuation.clone(),
            )
        })
        .collect::<Vec<_>>();
    write_table(out, &["", "", ""], &cells, width)?;
    let mut hints: BTreeMap<Attention, Vec<&str>> = BTreeMap::new();
    for row in rows {
        if let Some(attention) = row.attention {
            hints.entry(attention).or_default().push(&row.name);
        }
    }
    for (attention, names) in hints {
        let command = match attention {
            Attention::CheckoutGone => format!("rimz loop remove {}", names.join(" ")),
            Attention::Strikes | Attention::Failing | Attention::Invalid => names
                .iter()
                .map(|name| format!("rimz loop show {name}"))
                .collect::<Vec<_>>()
                .join(" · "),
            Attention::Blocked => "rimz trust grant".into(),
            Attention::Held => continue,
        };
        writeln!(out, "  → {command}")?;
    }
    Ok(())
}

fn room_rows(
    out: &mut impl Write,
    room: &Room,
    now: Timestamp,
    width: Option<usize>,
) -> Result<()> {
    let mut rows = room
        .tasks
        .iter()
        .filter(|row| row.section == Section::Room)
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return Ok(());
    }
    rows.sort_by(|a, b| {
        b.running
            .is_some()
            .cmp(&a.running.is_some())
            .then(a.name.cmp(&b.name))
    });
    writeln!(out, "\n{}", ui::paint(ui::palette::header(), "ROOM"))?;
    let cells = rows
        .iter()
        .map(|row| {
            (
                vec![
                    row.name.clone(),
                    row.head.clone(),
                    action(row),
                    row.last_text(now),
                ],
                row.continuation.clone(),
            )
        })
        .collect::<Vec<_>>();
    write_table(out, &["NAME", "TRIGGER", "ACTION", "LAST"], &cells, width)
}

fn action(row: &TaskRow) -> String {
    let Some(spec) = &row.action else {
        return String::new();
    };
    let mut action = match spec.kind {
        "wake" => spec
            .subject
            .split('#')
            .next()
            .unwrap_or(&spec.subject)
            .to_owned(),
        "start"
            if row
                .task
                .as_ref()
                .is_some_and(|task| task.entry().check.is_some()) =>
        {
            format!("check, then start {}", spec.subject)
        }
        "start" => format!("start {}", spec.subject),
        _ => "run check".into(),
    };
    if row.you {
        action.push_str(" (you)");
    }
    if let Some(account) = row
        .task
        .as_ref()
        .and_then(|task| task.entry().account.as_ref())
    {
        action.push_str(&format!(" · account {account}"));
    }
    action
}

fn worktree_rows(
    out: &mut impl Write,
    room: &Room,
    now: Timestamp,
    width: Option<usize>,
) -> Result<()> {
    let mut rows = room
        .tasks
        .iter()
        .filter(|row| row.section == Section::Worktrees)
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return Ok(());
    }
    rows.sort_by(|a, b| {
        b.worktree_here
            .cmp(&a.worktree_here)
            .then(a.worktree.cmp(&b.worktree))
            .then(a.dir.cmp(&b.dir))
            .then(a.name.cmp(&b.name))
    });
    writeln!(out, "\n{}", ui::paint(ui::palette::header(), "WORKTREES"))?;
    let mut cells = Vec::new();
    let mut start = 0;
    while start < rows.len() {
        let first = rows[start];
        let end = start
            + rows[start..]
                .iter()
                .take_while(|row| row.worktree == first.worktree && row.dir == first.dir)
                .count();
        for (index, members) in collapsed(&rows[start..end]).into_iter().enumerate() {
            let row = members
                .iter()
                .copied()
                .max_by_key(|row| {
                    (
                        row.last.is_some(),
                        row.last.as_ref().map(|last| last.at),
                        row.heard.as_ref().map(|heard| heard.at),
                    )
                })
                .unwrap_or(first);
            let group = if index == 0 {
                format!(
                    "{}{}",
                    first.worktree.as_deref().unwrap_or_default(),
                    if first.worktree_here { " (here)" } else { "" }
                )
            } else {
                String::new()
            };
            let name = match &row.owner {
                Some(owner) if owner.kind == "team" => {
                    let name = owner
                        .name
                        .split_once('#')
                        .filter(|(_, channel)| Some(*channel) == row.worktree.as_deref())
                        .map_or(owner.name.as_str(), |(name, _)| name);
                    format!("↳ team {name}")
                }
                Some(owner) => format!("↳ {}", owner.name),
                None => row.name.clone(),
            };
            let head = if members.len() > 1 {
                let signals = members
                    .iter()
                    .filter_map(|row| row.task.as_ref()?.entry().signal.clone())
                    .collect::<Vec<_>>();
                let matches = row
                    .head
                    .split_once(" · ")
                    .map(|(_, matches)| format!(" · {matches}"))
                    .unwrap_or_default();
                format!("on {}{matches}", signal_names(signals))
            } else {
                row.head.clone()
            };
            cells.push((
                vec![group, head, action(row), row.last_text(now), name],
                row.continuation.clone(),
            ));
        }
        start = end;
    }
    write_table(
        out,
        &["WORKTREE", "TRIGGER", "WAKES", "LAST", "NAME"],
        &cells,
        width,
    )
}

fn collapsed<'a>(rows: &[&'a TaskRow]) -> Vec<Vec<&'a TaskRow>> {
    let mut groups: Vec<Vec<&TaskRow>> = Vec::new();
    for row in rows {
        if row.owner.is_none()
            || row
                .task
                .as_ref()
                .is_none_or(|task| task.entry().signal.is_none())
        {
            groups.push(vec![row]);
            continue;
        }
        let filters = |row: &TaskRow| {
            row.task
                .as_ref()
                .and_then(|task| task.entry().matches.as_ref())
                .into_iter()
                .flatten()
                .filter(|(key, _)| key.as_str() != "path")
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<BTreeMap<_, _>>()
        };
        let group = groups.iter_mut().find(|group| {
            let first = group[0];
            first.owner == row.owner
                && first.action == row.action
                && first
                    .task
                    .as_ref()
                    .is_some_and(|task| task.entry().signal.is_some())
                && filters(first) == filters(row)
        });
        match group {
            Some(group) => group.push(row),
            None => groups.push(vec![row]),
        }
    }
    groups
}

fn signal_names(mut signals: Vec<String>) -> String {
    signals.sort();
    signals.dedup();
    let mut families: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for signal in signals {
        let (family, member) = signal.split_once('.').unwrap_or((&signal, ""));
        families
            .entry(family.to_owned())
            .or_default()
            .push(member.to_owned());
    }
    families
        .into_iter()
        .map(|(family, members)| {
            if members.len() > 1 {
                format!("{family}.{{{}}}", members.join(","))
            } else if members[0].is_empty() {
                family
            } else {
                format!("{family}.{}", members[0])
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn write_table(
    out: &mut impl Write,
    headers: &[&str],
    rows: &[(Vec<String>, String)],
    width: Option<usize>,
) -> Result<()> {
    let trigger_width = width.map(|width| {
        let other = headers
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != 1)
            .map(|(index, header)| {
                rows.iter()
                    .map(|(cells, _)| cells[index].width())
                    .max()
                    .unwrap_or(0)
                    .max(header.width())
            })
            .sum::<usize>();
        width
            .saturating_sub(other + 2 + (headers.len() - 1) * 2)
            .max(24)
    });
    let mut table = ui::Table::new(headers.iter().copied()).indent(2);
    for (cells, continuation) in rows {
        let lines = trigger_width.map_or_else(
            || vec![cells[1].clone()],
            |width| wrap_trigger(&cells[1], width),
        );
        for (index, line) in lines.iter().enumerate() {
            let mut cells = if index == 0 {
                cells.clone()
            } else {
                vec![String::new(); cells.len()]
            };
            cells[1] = line.clone();
            table.row(cells.into_iter().map(styled));
        }
        if !continuation.is_empty() {
            let lines = trigger_width.map_or_else(
                || vec![continuation.clone()],
                |width| wrap_trigger(continuation, width),
            );
            for line in lines {
                let mut cells = vec![String::new(); cells.len()];
                cells[1] = line;
                table.row(cells.into_iter().map(styled));
            }
        }
    }
    if headers.iter().all(|header| header.is_empty()) {
        let mut rendered = Vec::new();
        table.render(&mut rendered)?;
        out.write_all(
            rendered
                .splitn(2, |byte| *byte == b'\n')
                .nth(1)
                .unwrap_or_default(),
        )?;
    } else {
        table.render(out)?;
    }
    Ok(())
}

fn styled(text: String) -> ui::Cell {
    use ui::status::{StateRole, role};

    let style = if text.starts_with('✓') {
        role(StateRole::Success)
    } else if text.starts_with('✗') || text.starts_with("invalid:") {
        role(StateRole::Failed)
    } else if text.starts_with('▸') {
        role(StateRole::Working)
    } else if text.starts_with("off") || text == "lost" {
        role(StateRole::Neutral)
    } else {
        ui::palette::body()
    };
    ui::cell(text).fg(style)
}

pub(super) fn wrap_trigger(text: &str, width: usize) -> Vec<String> {
    fn wrap(text: &str, width: usize, separators: &[&str]) -> Vec<String> {
        if text.width() <= width {
            return vec![text.to_owned()];
        }
        let Some((separator, rest)) = separators.split_first() else {
            return ui::wrap_words(text, width);
        };
        let mut lines = Vec::new();
        let mut current = String::new();
        for (index, part) in text.split(separator).enumerate() {
            let part = if index == 0 {
                part.to_owned()
            } else {
                format!("{} {part}", separator.trim())
            };
            if !current.is_empty() && current.width() + 1 + part.width() <= width {
                current.push(' ');
                current.push_str(&part);
                continue;
            }
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            let mut wrapped = wrap(&part, width, rest);
            current = wrapped.pop().unwrap_or_default();
            lines.extend(wrapped);
        }
        if !current.is_empty() {
            lines.push(current);
        }
        lines
    }
    wrap(text, width, &[" · ", " && "])
}
