//! Lifecycle loop sections, with trigger-only wrapping and folded wake-ups.

use super::*;
use unicode_width::UnicodeWidthStr;

pub(super) fn write(
    out: &mut impl Write,
    model: &ListModel,
    rooms: &[&Room],
    all: bool,
    width: Option<usize>,
    current_root: Option<&Path>,
    mux: Option<rimz::ids::MuxName>,
) -> Result<()> {
    let total = model
        .rooms
        .iter()
        .map(|room| room.tasks.len() + room.retired.len())
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
        if room.tasks.is_empty() && room.retired.is_empty() {
            writeln!(
                out,
                "no loop tasks in {} · {total} in other rooms: rimz loop list --all",
                ui::home_relative(&room.root.to_string_lossy())
            )?;
            continue;
        }
        heading(out, room, model.now, timer)?;
        needs_you(out, room, model.now, width)?;
        routine_rows(out, room, model.now, width)?;
        waiting_rows(out, room, model.now, width)?;
        done_rows(out, room, model.now, all, width)?;
        wake_ups(out, room, model.now, all, current_root, mux)?;
    }
    if !all && model.caller_root.is_some() {
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
        && let Some((row, next)) = room
            .tasks
            .iter()
            .filter(|row| row.running.is_none())
            .filter_map(|row| row.next_at.map(|next| (row, next)))
            .min_by_key(|(_, next)| *next)
    {
        parts.push(format!("next: {} {}", row.name, ui::rel_until(next, now)));
    }
    if room.spend_today_usd > 0.0 {
        parts.push(format!("${:.2} spent today", room.spend_today_usd));
    }
    if !room.open {
        let waiting = if timer {
            "clocks use the active timer; CI/PR signals wait until `rimz start`"
        } else {
            "clocks and CI/PR signals wait until `rimz start` (or `rimz loop timer install` for clocks)"
        };
        parts.push(waiting.into());
    }
    writeln!(
        out,
        "{}",
        ui::paint(ui::palette::header(), &parts.join(" · "))
    )?;
    Ok(())
}

fn needs_you(
    out: &mut impl Write,
    room: &Room,
    now: Timestamp,
    width: Option<usize>,
) -> Result<()> {
    let mut rows = room
        .tasks
        .iter()
        .filter(|row| row.section == Section::NeedsYou)
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return Ok(());
    }
    rows.sort_by_key(|row| &row.name);
    let worktree = rows.iter().any(|row| row.worktree.is_some());
    writeln!(out, "\n{}", ui::paint(ui::palette::header(), "NEEDS YOU"))?;
    let cells = rows
        .iter()
        .map(|row| {
            let mut cells = name_cells(row, worktree);
            cells.extend([row.head.clone(), row.reason.clone()]);
            let continuation = if row.capacity_wait.is_some() {
                [row.continuation.as_str(), &row.state_text(now)]
                    .into_iter()
                    .filter(|text| !text.is_empty())
                    .collect::<Vec<_>>()
                    .join(" · ")
            } else {
                row.continuation.clone()
            };
            (cells, continuation, reason_style(row.attention))
        })
        .collect::<Vec<_>>();
    write_table(
        out,
        &vec![""; 3 + usize::from(worktree)],
        &cells,
        1 + usize::from(worktree),
        2 + usize::from(worktree),
        width,
    )?;
    for row in &rows {
        let parked: Vec<_> = row
            .workflow_runs
            .iter()
            .filter(|run| run.reason.is_some())
            .collect();
        // The first parked run is already the row's reason.
        for run in parked
            .into_iter()
            .skip(usize::from(row.attention == Some(Attention::Parked)))
        {
            writeln!(out, "  {}", run.summary(now))?;
        }
    }
    let mut hints: BTreeMap<Attention, Vec<&str>> = BTreeMap::new();
    for row in rows {
        if let Some(attention) = row.attention {
            hints.entry(attention).or_default().push(&row.name);
        }
    }
    let rimz = if room.here {
        "rimz".into()
    } else {
        shlex::try_join(["rimz", "--root", room.root.to_string_lossy().as_ref()])?
    };
    for (attention, names) in hints {
        let command = match attention {
            Attention::CheckoutGone | Attention::Lost => {
                format!("{rimz} loop remove {}", names.join(" "))
            }
            Attention::Strikes | Attention::Failing | Attention::Invalid | Attention::Parked => {
                names
                    .iter()
                    .map(|name| format!("{rimz} loop show {name}"))
                    .collect::<Vec<_>>()
                    .join(" · ")
            }
            Attention::Blocked => format!("{rimz} trust grant"),
            Attention::Held => continue,
        };
        writeln!(out, "  → {command}")?;
    }
    Ok(())
}

fn section_heading(
    out: &mut impl Write,
    title: &str,
    note: &str,
    width: Option<usize>,
) -> Result<()> {
    let gap = width.map_or(2, |width| {
        width.saturating_sub(title.width() + note.width()).max(2)
    });
    writeln!(
        out,
        "\n{}{:gap$}{}",
        ui::paint(ui::palette::header(), title),
        "",
        ui::paint(ui::palette::body(), note)
    )?;
    Ok(())
}

fn columns(tail: &[&'static str], worktree: bool) -> Vec<&'static str> {
    let mut columns = vec!["NAME"];
    if worktree {
        columns.push("WORKTREE");
    }
    columns.extend_from_slice(tail);
    columns
}

fn worktree_text(row: &TaskRow) -> String {
    let Some(worktree) = &row.worktree else {
        return String::new();
    };
    format!(
        "{worktree}{}",
        if row.worktree_here { " (here)" } else { "" }
    )
}

fn name_cells(row: &TaskRow, worktree: bool) -> Vec<String> {
    let mut cells = vec![row.name.clone()];
    if worktree {
        cells.push(worktree_text(row));
    }
    cells
}

fn routine_rows(
    out: &mut impl Write,
    room: &Room,
    now: Timestamp,
    width: Option<usize>,
) -> Result<()> {
    let mut rows = room
        .tasks
        .iter()
        .filter(|row| row.section == Section::Routine)
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return Ok(());
    }
    rows.sort_by_key(|row| &row.name);
    let worktree = rows.iter().any(|row| row.worktree.is_some());
    writeln!(out, "\n{}", ui::paint(ui::palette::header(), "ROUTINE"))?;
    let cells = rows
        .iter()
        .map(|row| {
            let mut cells = name_cells(row, worktree);
            cells.extend([
                row.head.clone(),
                if row.capacity_wait.is_some() {
                    row.state_text(now)
                } else {
                    row.next_at
                        .map(|at| ui::rel_until(at, now))
                        .unwrap_or_else(|| "—".into())
                },
                action(row),
                row.last_text(now),
            ]);
            (cells, row.continuation.clone(), last_style(row))
        })
        .collect::<Vec<_>>();
    let headers = columns(&["RUNS", "NEXT", "ACTION", "LAST"], worktree);
    write_table(
        out,
        &headers,
        &cells,
        1 + usize::from(worktree),
        headers.len() - 1,
        width,
    )
}

fn waiting_rows(
    out: &mut impl Write,
    room: &Room,
    now: Timestamp,
    width: Option<usize>,
) -> Result<()> {
    let mut rows = room
        .tasks
        .iter()
        .filter(|row| row.section == Section::Waiting)
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return Ok(());
    }
    rows.sort_by_key(|row| &row.name);
    let worktree = rows.iter().any(|row| row.worktree.is_some());
    section_heading(out, "WAITING", "one-offs, fire once", width)?;
    let cells = rows
        .iter()
        .map(|row| {
            let mut trigger = row.head.clone();
            if matches!(
                row.state,
                TaskState::Off | TaskState::NotEnabled | TaskState::Running
            ) {
                trigger.push_str(&format!(" · {}", row.last_text(now)));
            }
            if let Some(count) = row.condition {
                trigger.push_str(&format!(" · {} of {} met", count.met, count.total));
                if count.not_read > 0 {
                    trigger.push_str(&format!(" · {} not read", count.not_read));
                }
            }
            if row.capacity_wait.is_some() {
                trigger.push_str(&format!(" · {}", row.state_text(now)));
            }
            let mut cells = name_cells(row, worktree);
            cells.extend([trigger, action(row)]);
            (cells, row.continuation.clone(), ui::palette::body())
        })
        .collect::<Vec<_>>();
    let headers = columns(&["WAITING FOR", "THEN"], worktree);
    write_table(
        out,
        &headers,
        &cells,
        1 + usize::from(worktree),
        headers.len() - 1,
        width,
    )
}

fn action(row: &TaskRow) -> String {
    let Some(spec) = &row.action else {
        return String::new();
    };
    let kind = match spec.kind {
        ActionKind::Wake => TaskActionKind::Deliver,
        ActionKind::Start => TaskActionKind::Spawn,
        ActionKind::Check => TaskActionKind::CheckOnly,
        ActionKind::Workflow => TaskActionKind::Workflow,
    };
    crate::cli::wake_view::action_text(
        kind,
        &spec.subject,
        row.task.as_ref().map(LoadedTask::entry),
        row.you,
    )
}

fn done_rows(
    out: &mut impl Write,
    room: &Room,
    now: Timestamp,
    all: bool,
    width: Option<usize>,
) -> Result<()> {
    let mut rows = done_entries(room)
        .into_iter()
        .map(|entry| match entry {
            DoneEntry::Task(row) => {
                let fired = if row.running.is_some() {
                    row.state_text(now)
                } else if let Some(launch) = &row.launch {
                    format!(
                        "{} · launched {} @{}",
                        ui::rel_age(launch.at, now),
                        row.action
                            .as_ref()
                            .map(|action| action.subject.as_str())
                            .unwrap_or_default(),
                        launch.leader.trim_start_matches('@')
                    )
                } else {
                    row.last_text(now)
                };
                (
                    row.name.as_str(),
                    row.worktree.as_ref().map(|_| worktree_text(row)),
                    fired,
                    last_style(row),
                    row.running.is_some(),
                )
            }
            DoneEntry::Retired(row) => (
                row.name.as_str(),
                row.checkout.as_ref().map(|path| {
                    format!(
                        "{}{}",
                        checkout_name(path),
                        if row.worktree_here { " (here)" } else { "" }
                    )
                }),
                format!("{} · {}", ui::rel_age(row.at, now), row.result.label()),
                render::loop_result_mark(row.result).style,
                false,
            ),
        })
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return Ok(());
    }
    let older = if all {
        0
    } else {
        rows.iter().filter(|row| !row.4).count().saturating_sub(5)
    };
    rows.truncate(rows.len() - older);
    let worktree = rows.iter().any(|row| row.1.is_some());
    section_heading(out, "DONE", "recent one-offs", width)?;
    let cells = rows
        .into_iter()
        .map(|(name, checkout, fired, style, _)| {
            let mut cells = vec![name.to_owned()];
            if worktree {
                cells.push(checkout.unwrap_or_default());
            }
            cells.push(fired);
            (cells, String::new(), style)
        })
        .collect::<Vec<_>>();
    let headers = columns(&["FIRED"], worktree);
    write_table(
        out,
        &headers,
        &cells,
        headers.len() - 1,
        headers.len() - 1,
        None,
    )?;
    if older > 0 {
        writeln!(out, "  (and {older} older · --all)")?;
    }
    Ok(())
}

fn wake_ups(
    out: &mut impl Write,
    room: &Room,
    now: Timestamp,
    all: bool,
    current_root: Option<&Path>,
    mux: Option<rimz::ids::MuxName>,
) -> Result<()> {
    use crate::cli::wake_view::{self, WakeRow};

    let rows = room
        .tasks
        .iter()
        .filter(|row| row.kind == TaskKind::WakeUp)
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return Ok(());
    }
    let counts = wake_view::counts(rows.iter().map(|row| row.owner_class()));
    let need_you = rows.iter().filter(|row| row.attention.is_some()).count();
    let health = if need_you == 0 {
        "all healthy".into()
    } else {
        format!("{need_you} need you")
    };
    let hint = if all { "" } else { " · rimz loop list --all" };
    writeln!(
        out,
        "\n{}  {counts} · {health}{hint}",
        ui::paint(ui::palette::header(), "WAKE-UPS")
    )?;
    if !all {
        return Ok(());
    }
    let snapshot = if rows
        .iter()
        .any(|row| row.owner_class() == schedule::DeliveryOwner::SelfWait)
    {
        StatePaths::for_project_root(&room.root)
            .ok()
            .and_then(|paths| crate::cli::ctx::Ctx::for_workspace(paths.workspace_id, mux).ok())
            .and_then(|ctx| ctx.resolution_snapshot().ok())
    } else {
        None
    };
    let wakes = rows
        .iter()
        .filter_map(|row| {
            let task = row.task.as_ref()?;
            Some(WakeRow::new(
                &row.name,
                task.entry(),
                row.timing
                    .as_ref()
                    .map_or(ArmState::Live, |timing| timing.arm_state()),
                row.watcher_pid,
                snapshot.as_ref(),
                now,
            ))
        })
        .collect::<Vec<_>>();
    let caller = rows
        .iter()
        .find(|row| row.you)
        .and_then(|row| row.task.as_ref()?.entry().wait.as_ref())
        .map(|target| (target.kind.clone(), target.session.clone()));
    wake_view::write_room(
        out,
        &wakes,
        snapshot.as_ref(),
        caller.as_ref(),
        current_root.unwrap_or(&room.root),
        now,
    )
}

pub(super) fn write_table(
    out: &mut impl Write,
    headers: &[&str],
    rows: &[(Vec<String>, String, anstyle::Style)],
    trigger_column: usize,
    state_column: usize,
    width: Option<usize>,
) -> Result<()> {
    let trigger_width = width.map(|width| {
        let other = headers
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != trigger_column)
            .map(|(index, header)| {
                rows.iter()
                    .map(|(cells, _, _)| cells[index].width())
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
    for (cells, continuation, style) in rows {
        let lines = trigger_width.map_or_else(
            || vec![cells[trigger_column].clone()],
            |width| wrap_trigger(&cells[trigger_column], width),
        );
        for (index, line) in lines.iter().enumerate() {
            let mut cells = if index == 0 {
                cells.clone()
            } else {
                vec![String::new(); cells.len()]
            };
            cells[trigger_column] = line.clone();
            table.row(cells.into_iter().enumerate().map(|(column, text)| {
                ui::cell(text).fg(if column == state_column {
                    *style
                } else {
                    ui::palette::body()
                })
            }));
        }
        if !continuation.is_empty() {
            let lines = trigger_width.map_or_else(
                || vec![continuation.clone()],
                |width| wrap_trigger(continuation, width),
            );
            for line in lines {
                let mut cells = vec![String::new(); cells.len()];
                cells[trigger_column] = line;
                table.row(
                    cells
                        .into_iter()
                        .map(|text| ui::cell(text).fg(ui::palette::body())),
                );
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

fn last_style(row: &TaskRow) -> anstyle::Style {
    use ui::status::{StateRole, role};

    match (row.state, row.last.as_ref()) {
        (TaskState::Running, _) => role(StateRole::Working),
        (TaskState::Off | TaskState::NotEnabled, _) => role(StateRole::Neutral),
        (_, Some(last)) => role(if last.ok {
            StateRole::Success
        } else {
            StateRole::Failed
        }),
        (_, None)
            if row.heard.is_none()
                && row.watcher_pid.is_none()
                && row
                    .task
                    .as_ref()
                    .is_some_and(|task| task.entry().watch.is_some()) =>
        {
            role(StateRole::Neutral)
        }
        _ => ui::palette::body(),
    }
}

pub(super) fn reason_style(attention: Option<Attention>) -> anstyle::Style {
    use ui::status::{StateRole, role};

    match attention {
        Some(
            Attention::CheckoutGone
            | Attention::Strikes
            | Attention::Failing
            | Attention::Invalid
            | Attention::Lost,
        ) => role(StateRole::Failed),
        Some(Attention::Held | Attention::Blocked | Attention::Parked) => role(StateRole::Waiting),
        None => ui::palette::body(),
    }
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
