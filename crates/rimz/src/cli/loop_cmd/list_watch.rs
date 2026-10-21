//! Compact room loop panel, projected from the shared list model.

use super::*;
use unicode_width::UnicodeWidthStr;

pub(super) fn write(
    out: &mut impl Write,
    room: &Room,
    now: Timestamp,
    cols: usize,
    rows: usize,
    hold: bool,
) -> std::io::Result<()> {
    let mut tasks = room
        .tasks
        .iter()
        .filter(|row| {
            row.running.is_some()
                || (row.section != Section::Done
                    && (row.kind != TaskKind::WakeUp || row.section == Section::NeedsYou))
        })
        .collect::<Vec<_>>();
    tasks.sort_by_key(|row| rank_key(row));
    write_band(out, &tasks, now, cols, hold)?;
    if rows <= 2 {
        return Ok(());
    }
    writeln!(out)?;

    let folds = fold_lines(room, now);
    if tasks.is_empty() {
        if folds.is_empty() {
            return write_line(
                out,
                &[(
                    0,
                    "no loop tasks; add one with `rimz loop add …`",
                    ui::palette::body(),
                )],
                cols,
            );
        }
        for fold in folds.iter().take(rows - 2) {
            write_line(out, &[(2, fold, ui::palette::muted())], cols)?;
        }
        return Ok(());
    }

    let fold_height = folds.len() + usize::from(!folds.is_empty());
    let mut visible = tasks.len();
    let (mut columns, mut cells) = layout(&tasks, now, cols);
    if 3 + cells.iter().map(Row::height).sum::<usize>() + fold_height > rows {
        visible = visible.min(rows.saturating_sub(4 + fold_height));
        loop {
            (columns, cells) = layout(&tasks[..visible], now, cols);
            if 4 + cells.iter().map(Row::height).sum::<usize>() + fold_height <= rows
                || visible == 0
            {
                break;
            }
            visible -= 1;
        }
    }
    let header = ui::palette::header();
    write_line(
        out,
        &[
            (2, &ui::clip_to_width("task", columns.name), header),
            (columns.next_start(), "next", header),
            (columns.last_start(), "last run", header),
        ],
        cols,
    )?;
    let mut written = 3;
    for row in cells {
        write_line(
            out,
            &[
                (
                    2,
                    &ui::clip_to_width(&row.task.name, columns.name),
                    ui::palette::body(),
                ),
                (columns.next_start(), &row.next[0], next_style(row.task)),
                (columns.last_start(), &row.last, row.last_style),
            ],
            cols,
        )?;
        for next in row.next.iter().skip(1) {
            write_line(
                out,
                &[(columns.next_start(), next, next_style(row.task))],
                cols,
            )?;
        }
        for reason in &row.reason {
            write_line(
                out,
                &[(
                    columns.next_start(),
                    reason,
                    text::reason_style(row.task.attention),
                )],
                cols,
            )?;
        }
        written += row.height();
    }
    if visible < tasks.len() && written < rows {
        write_line(
            out,
            &[(
                0,
                &format!("+{} more", tasks.len() - visible),
                ui::palette::faint(),
            )],
            cols,
        )?;
        written += 1;
    }
    if !folds.is_empty() && written + 1 < rows {
        writeln!(out)?;
        written += 1;
        for fold in folds.iter().take(rows - written) {
            write_line(out, &[(2, fold, ui::palette::muted())], cols)?;
        }
    }
    Ok(())
}

fn held(row: &TaskRow) -> bool {
    matches!(
        row.state,
        TaskState::Off | TaskState::NotEnabled | TaskState::Paused | TaskState::WaitsForRoom
    ) || row.timing.as_ref().is_some_and(|timing| {
        matches!(
            timing.state(),
            schedule::TaskTimingState::Disabled(_) | schedule::TaskTimingState::Paused(_)
        )
    })
}

fn blocked(row: &TaskRow) -> bool {
    matches!(
        row.attention,
        Some(Attention::CheckoutGone | Attention::Blocked | Attention::Invalid)
    ) || row
        .timing
        .as_ref()
        .is_some_and(|timing| matches!(timing.state(), schedule::TaskTimingState::Blocked(_)))
}

fn rank_key(row: &TaskRow) -> (u8, Option<Timestamp>, &str) {
    let (rank, next) = if row.section == Section::NeedsYou {
        (0, None)
    } else if row.running.is_some() {
        (1, None)
    } else if held(row) {
        (5, None)
    } else {
        match row.timing.as_ref().map(schedule::TaskTiming::state) {
            Some(schedule::TaskTimingState::Due(at)) => (2, Some(at)),
            Some(schedule::TaskTimingState::Upcoming(at)) => (3, Some(at)),
            _ => (4, None),
        }
    };
    (rank, next, &row.name)
}

fn next_timestamp(row: &TaskRow) -> Option<Timestamp> {
    let timing = row.timing.as_ref()?;
    if row.running.is_some() {
        return timing.scheduled_next_timestamp();
    }
    if held(row) || blocked(row) {
        return None;
    }
    timing.next_timestamp()
}

fn next_style(row: &TaskRow) -> anstyle::Style {
    if row.running.is_some() {
        return ui::palette::cool();
    }
    if held(row) || blocked(row) {
        return ui::palette::muted();
    }
    match row.timing.as_ref().map(schedule::TaskTiming::state) {
        Some(schedule::TaskTimingState::Due(_)) => ui::palette::warn(),
        Some(
            schedule::TaskTimingState::Listening { .. }
            | schedule::TaskTimingState::Watching { .. }
            | schedule::TaskTimingState::Waiting { .. }
            | schedule::TaskTimingState::Holding { .. }
            | schedule::TaskTimingState::Fired,
        ) => ui::palette::cool(),
        Some(schedule::TaskTimingState::Upcoming(_)) => anstyle::Style::new(),
        _ => ui::palette::faint(),
    }
}

fn write_band(
    out: &mut impl Write,
    tasks: &[&TaskRow],
    now: Timestamp,
    cols: usize,
    hold: bool,
) -> std::io::Result<()> {
    let needs_you = tasks
        .iter()
        .filter(|row| row.section == Section::NeedsYou)
        .count();
    let running = tasks.iter().filter(|row| rank_key(row).0 == 1).count();
    let held = tasks.iter().filter(|row| rank_key(row).0 == 5).count();
    let mut candidates = Vec::new();
    if needs_you > 0 {
        let verb = if needs_you == 1 { "needs" } else { "need" };
        candidates.push((format!("✗ {needs_you} {verb} you"), ui::palette::alarm()));
    }
    if running > 0 {
        candidates.push((format!("▸ {running} running"), ui::palette::cool()));
    }
    if let Some((row, next)) = tasks
        .iter()
        .filter_map(|row| next_timestamp(row).map(|at| (row, at)))
        .min_by_key(|(row, at)| (*at, &row.name))
    {
        candidates.push((
            format!("next: {} {}", row.name, ui::rel_until(next, now)),
            ui::palette::muted(),
        ));
    }
    if held > 0 {
        candidates.push((format!("○ {held} held"), ui::palette::muted()));
    }
    if !hold {
        candidates.push(("q quit".into(), ui::palette::faint()));
    }
    write!(
        out,
        "{}",
        ui::paint(ui::palette::header(), &ui::clip_to_width("loop", cols))
    )?;
    let mut width = 4;
    for (text, style) in candidates {
        width += 3 + text.width();
        if width > cols {
            break;
        }
        write!(
            out,
            "{}{}",
            ui::paint(ui::palette::faint(), " · "),
            ui::paint(style, &text)
        )?;
    }
    writeln!(out)
}

fn fold_lines(room: &Room, now: Timestamp) -> Vec<String> {
    let done = done_entries(room)
        .into_iter()
        .filter(|entry| !entry.running())
        .collect::<Vec<_>>();
    let mut folds = Vec::new();
    if let Some(newest) = done.first() {
        let plural = if done.len() == 1 { "" } else { "s" };
        folds.push(format!(
            "done      {} one-off{plural} · newest {} {}",
            done.len(),
            newest.name(),
            ui::rel_age(newest.at(), now)
        ));
    }
    let wakes = room
        .tasks
        .iter()
        .filter(|row| {
            row.kind == TaskKind::WakeUp
                && row.section != Section::NeedsYou
                && row.running.is_none()
        })
        .count();
    if wakes > 0 {
        folds.push(format!(
            "wake-ups  {wakes} wake agents · all healthy · rimz loop list --all"
        ));
    }
    folds
}

struct Columns {
    name: usize,
    next: usize,
}

impl Columns {
    fn next_start(&self) -> usize {
        2 + self.name + 2
    }
    fn last_start(&self) -> usize {
        self.next_start() + self.next + 2
    }
}

struct Row<'a> {
    task: &'a TaskRow,
    next: Vec<String>,
    reason: Vec<String>,
    last: String,
    last_style: anstyle::Style,
}

impl Row<'_> {
    fn height(&self) -> usize {
        self.next.len() + self.reason.len()
    }
}

fn layout<'a>(tasks: &[&'a TaskRow], now: Timestamp, cols: usize) -> (Columns, Vec<Row<'a>>) {
    let mut rows = tasks
        .iter()
        .map(|task| {
            let (last, last_style) = if let Some(last) = &task.last {
                (
                    acting_run_text(last, task.exit.as_deref(), now),
                    render::run_status(&last.record).style,
                )
            } else if let Some(heard) = &task.heard {
                (
                    format!("heard {} {}", heard.signal, ui::rel_age(heard.at, now)),
                    ui::palette::faint(),
                )
            } else {
                (
                    if task.running.is_some() {
                        "—"
                    } else {
                        "never fired"
                    }
                    .into(),
                    ui::palette::faint(),
                )
            };
            Row {
                task,
                next: vec![ui::one_line(&task.state_text(now))],
                reason: Vec::new(),
                last: ui::one_line(&last),
                last_style,
            }
        })
        .collect::<Vec<_>>();
    let name = tasks
        .iter()
        .map(|row| row.name.width())
        .max()
        .unwrap_or(0)
        .max(4);
    let last = rows
        .iter()
        .map(|row| row.last.width())
        .max()
        .unwrap_or(0)
        .max(8);
    let next_floor = rows
        .iter()
        .flat_map(|row| row.next[0].split_whitespace())
        .map(UnicodeWidthStr::width)
        .max()
        .unwrap_or(0)
        .max(4);
    let mut columns = Columns {
        name: name.min(
            cols.saturating_sub(2 + next_floor + last + 4)
                .max(name.min(cols / 4).max(1)),
        ),
        next: 4,
    };
    let next_budget = cols
        .saturating_sub(2 + columns.name + last + 4)
        .max(next_floor);
    for row in &mut rows {
        row.next = text::wrap_trigger(&row.next[0], next_budget);
        if matches!(
            row.task.attention,
            Some(
                Attention::CheckoutGone
                    | Attention::Strikes
                    | Attention::Blocked
                    | Attention::Invalid
                    | Attention::Parked
                    | Attention::Lost
            )
        ) {
            row.reason = text::wrap_trigger(
                &format!("✗ {}", ui::one_line(&row.task.reason)),
                cols.saturating_sub(columns.next_start()).max(1),
            );
        }
    }
    columns.next = rows
        .iter()
        .flat_map(|row| &row.next)
        .map(|line| line.width())
        .max()
        .unwrap_or(0)
        .max(4);
    (columns, rows)
}

fn write_line(
    out: &mut impl Write,
    cells: &[(usize, &str, anstyle::Style)],
    cols: usize,
) -> std::io::Result<()> {
    let mut position = 0;
    for (offset, text, style) in cells {
        if *offset >= cols {
            break;
        }
        if text.is_empty() {
            continue;
        }
        write!(out, "{}", " ".repeat(offset.saturating_sub(position)))?;
        let text = ui::clip_to_width(text, cols - offset);
        write!(out, "{}", ui::paint(*style, &text))?;
        position = offset + text.width();
    }
    writeln!(out)
}

#[cfg(test)]
#[path = "list_watch_tests.rs"]
mod tests;
