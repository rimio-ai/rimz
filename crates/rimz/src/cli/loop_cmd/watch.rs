//! Full-screen dashboard for live loop task state.

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use rimz::tui::{MouseCapture, Screen, TerminalModeGuard};
use unicode_width::UnicodeWidthStr;

use super::list::{Attention, TaskRow, TaskState};
use super::render::{room_label, room_style};
use super::*;

const WATCH_NARROW: usize = 44;
const WATCH_WIDE: usize = 68;

pub(super) fn watch(args: WatchArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone()).ok();
    let _input = TerminalModeGuard::enable(MouseCapture::Off, Screen::Alternate)?;
    loop {
        repaint_watch(workspace.as_ref(), args.hold)?;
        match event::poll(Duration::from_secs(1)) {
            Ok(true) => match event::read() {
                Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Char('q') if !args.hold => return Ok(()),
                    KeyCode::Char('c') | KeyCode::Char('C')
                        if key.modifiers.contains(KeyModifiers::CONTROL) && !args.hold =>
                    {
                        return Ok(());
                    }
                    _ => {}
                },
                Ok(_) => {}
                Err(err) => return Err(err.into()),
            },
            Ok(false) => {}
            Err(err) => return Err(err.into()),
        }
    }
}

fn repaint_watch(workspace: Option<&rimz::ResolvedWorkspace>, hold: bool) -> Result<()> {
    let mut frame = Vec::new();
    render_watch_frame(&mut frame, workspace, hold)?;
    if frame.last() == Some(&b'\n') {
        frame.pop();
        if frame.last() == Some(&b'\r') {
            frame.pop();
        }
    }
    let mut stdout = std::io::stdout();
    rimz::tui::replace_frame(&mut stdout, &frame)?;
    Ok(())
}

fn render_watch_frame(
    out: &mut impl Write,
    workspace: Option<&rimz::ResolvedWorkspace>,
    hold: bool,
) -> Result<()> {
    let model = list::load(workspace, list::LoadScope::CallerRoom)?;
    let groups = model
        .rooms
        .iter()
        .map(|room| WatchGroup {
            root: room.root.clone(),
            room_is_open: room.open,
            rows: room
                .tasks
                .iter()
                .map(|task| watch_row_model(task, model.now))
                .collect(),
        })
        .collect::<Vec<_>>();
    let (cols, rows) = rimz::mux::detect_terminal_size().unwrap_or((80, 24));
    render_dashboard(
        out,
        &groups,
        usize::from(cols),
        usize::from(rows),
        model.now,
        hold,
    )?;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowState {
    Running,
    Due,
    Held,
    Blocked,
    Listening,
    Watching,
    Condition,
    Upcoming(Timestamp),
    NeverRun,
}

#[derive(Clone)]
struct WatchRow {
    name: String,
    glyph: &'static str,
    glyph_style: anstyle::Style,
    state: RowState,
    failed: bool,
    next_ts: Option<Timestamp>,
    next_text: String,
    last_text: String,
    status_text: String,
}

#[derive(Clone, Copy)]
enum WatchBand {
    Running,
    Failed,
    Held,
    Ok,
}

impl WatchRow {
    fn band(&self) -> WatchBand {
        if self.state == RowState::Running {
            WatchBand::Running
        } else if self.failed {
            WatchBand::Failed
        } else if matches!(self.state, RowState::Held | RowState::Blocked) {
            WatchBand::Held
        } else {
            WatchBand::Ok
        }
    }

    fn rank_key(&self) -> (u8, Option<Timestamp>, &str) {
        let rank = match self.band() {
            WatchBand::Running => 0,
            WatchBand::Failed => 1,
            WatchBand::Ok => match self.state {
                RowState::Due => 2,
                RowState::Upcoming(_)
                | RowState::Listening
                | RowState::Watching
                | RowState::Condition => 3,
                RowState::NeverRun => 5,
                RowState::Running | RowState::Held | RowState::Blocked => 0,
            },
            WatchBand::Held => 4,
        };
        let next = match self.state {
            RowState::Upcoming(next) => Some(next),
            _ => None,
        };
        (rank, next, &self.name)
    }

    fn eligible_next(&self) -> Option<Timestamp> {
        (!matches!(self.state, RowState::Held | RowState::Blocked))
            .then_some(self.next_ts)
            .flatten()
    }
}

struct WatchGroup {
    root: PathBuf,
    room_is_open: bool,
    rows: Vec<WatchRow>,
}

#[derive(Default)]
struct WatchSummary<'a> {
    total: usize,
    running: usize,
    failed: usize,
    held: usize,
    ok: usize,
    next: Option<(&'a WatchRow, Timestamp)>,
}

impl<'a> WatchSummary<'a> {
    fn from_groups(groups: &'a [WatchGroup]) -> Self {
        let mut summary = Self::default();
        for row in groups.iter().flat_map(|group| &group.rows) {
            summary.total += 1;
            match row.band() {
                WatchBand::Running => summary.running += 1,
                WatchBand::Failed => summary.failed += 1,
                WatchBand::Held => summary.held += 1,
                WatchBand::Ok => summary.ok += 1,
            }
            if let Some(next) = row.eligible_next()
                && summary.next.is_none_or(|(_, earliest)| next < earliest)
            {
                summary.next = Some((row, next));
            }
        }
        summary
    }
}

fn watch_row_model(task: &TaskRow, now: Timestamp) -> WatchRow {
    let state = if task.state == TaskState::Running {
        RowState::Running
    } else if matches!(
        task.attention,
        Some(Attention::CheckoutGone | Attention::Blocked | Attention::Invalid)
    ) {
        RowState::Blocked
    } else {
        task.timing.as_ref().map_or(RowState::NeverRun, |timing| {
            row_state_for_timing(timing, task.state)
        })
    };
    let next_ts = task
        .timing
        .as_ref()
        .and_then(|timing| watch_next_timestamp(timing, task.state == TaskState::Running));
    let (glyph, glyph_style, failed, last_text, status_text) = match &task.last {
        Some(last) => {
            let status = render::run_status(&last.record);
            (
                status.glyph,
                status.style,
                !last.ok,
                ui::rel_age(last.at, now),
                status.label,
            )
        }
        None => (
            "○",
            ui::palette::faint(),
            false,
            task.heard
                .as_ref()
                .map_or_else(|| "—".into(), |heard| ui::rel_age(heard.at, now)),
            task.heard.as_ref().map_or_else(
                || task.last_text(now),
                |heard| format!("heard {}", heard.signal),
            ),
        ),
    };

    WatchRow {
        name: task.name.clone(),
        glyph,
        glyph_style,
        state,
        failed,
        next_ts,
        next_text: task.state_text(now),
        last_text,
        status_text,
    }
}

fn watch_next_timestamp(timing: &schedule::TaskTiming, running: bool) -> Option<Timestamp> {
    if running {
        timing.scheduled_next_timestamp()
    } else {
        timing.next_timestamp()
    }
}

fn row_state_for_timing(timing: &schedule::TaskTiming, state: TaskState) -> RowState {
    match timing.state() {
        schedule::TaskTimingState::Blocked(_) => RowState::Blocked,
        _ if matches!(
            state,
            TaskState::Off | TaskState::NotEnabled | TaskState::Paused | TaskState::WaitsForRoom
        ) =>
        {
            RowState::Held
        }
        schedule::TaskTimingState::Disabled(_) | schedule::TaskTimingState::Paused(_) => {
            RowState::Held
        }
        schedule::TaskTimingState::Due(_) => RowState::Due,
        schedule::TaskTimingState::Upcoming(next) => RowState::Upcoming(next),
        schedule::TaskTimingState::Listening { .. } => RowState::Listening,
        schedule::TaskTimingState::Watching { .. } => RowState::Watching,
        schedule::TaskTimingState::Waiting { .. }
        | schedule::TaskTimingState::Holding { .. }
        | schedule::TaskTimingState::Fired => RowState::Condition,
        schedule::TaskTimingState::Invalid
        | schedule::TaskTimingState::Unarmed
        | schedule::TaskTimingState::NoOccurrence => RowState::NeverRun,
    }
}

fn render_dashboard(
    out: &mut impl Write,
    groups: &[WatchGroup],
    cols: usize,
    rows: usize,
    now: Timestamp,
    hold: bool,
) -> std::io::Result<()> {
    write_watch_band(out, groups, cols, now, hold)?;
    if rows <= 2 {
        return Ok(());
    }
    writeln!(out)?;
    let mut remaining_rows = rows - 2;
    let mut remaining_tasks = groups.iter().map(|group| group.rows.len()).sum::<usize>();

    if remaining_tasks == 0 {
        writeln!(
            out,
            "{}",
            ui::clip_to_width("no loop tasks; add one with `rimz loop add …`", cols)
        )?;
        return Ok(());
    }

    for group in groups {
        if group.rows.is_empty() {
            continue;
        }
        let mut ranked = group.rows.iter().collect::<Vec<_>>();
        ranked.sort_by_key(|row| row.rank_key());
        let full_section_rows = ranked.len() + 2;
        let more_rows = usize::from(remaining_tasks > ranked.len());
        if full_section_rows + more_rows <= remaining_rows {
            write_dashboard_heading(out, &group.root, group.room_is_open, cols)?;
            render_watch_rows(out, &ranked, cols)?;
            remaining_rows -= full_section_rows;
            remaining_tasks -= ranked.len();
            continue;
        }
        if remaining_rows < 4 {
            write_more(out, remaining_tasks, cols)?;
            break;
        }
        write_dashboard_heading(out, &group.root, group.room_is_open, cols)?;
        let visible = remaining_rows - 3;
        render_watch_rows(out, &ranked[..visible], cols)?;
        remaining_tasks -= visible;
        write_more(out, remaining_tasks, cols)?;
        break;
    }
    Ok(())
}

fn render_watch_rows(out: &mut impl Write, rows: &[&WatchRow], cols: usize) -> std::io::Result<()> {
    let headers = if cols < WATCH_NARROW {
        vec!["", "task", "next"]
    } else if cols < WATCH_WIDE {
        vec!["", "task", "next", "last run"]
    } else {
        vec!["", "task", "next", "last run", "status"]
    };
    let mut table = ui::Table::new(headers).indent(2).max_width(cols);
    for row in rows {
        let next_style = match row.state {
            RowState::Running => ui::palette::cool(),
            RowState::Held | RowState::Blocked => ui::palette::muted(),
            RowState::NeverRun => ui::palette::faint(),
            RowState::Due => ui::palette::warn(),
            RowState::Listening | RowState::Watching | RowState::Condition => ui::palette::cool(),
            RowState::Upcoming(_) => anstyle::Style::new(),
        };
        let name = ui::clip_to_width(&row.name, (cols / 4).max(1));
        let next = ui::clip_to_width(&row.next_text, (cols / 4).max(1));
        let mut cells = vec![
            ui::cell(row.glyph).fg(row.glyph_style),
            ui::cell(name).fg(ui::palette::body()),
            ui::cell(next).fg(next_style),
        ];
        if cols >= WATCH_NARROW {
            cells.push(ui::cell(ui::clip_to_width(&row.last_text, 12)));
        }
        if cols >= WATCH_WIDE {
            cells.push(ui::cell(&row.status_text).fg(row.glyph_style));
        }
        table.row(cells);
    }
    table.render(out)
}

fn write_watch_band(
    out: &mut impl Write,
    groups: &[WatchGroup],
    cols: usize,
    now: Timestamp,
    hold: bool,
) -> std::io::Result<()> {
    let summary = WatchSummary::from_groups(groups);

    let prefix = format!("loop · {} tasks", summary.total);
    if prefix.width() > cols {
        writeln!(
            out,
            "{}",
            ui::paint(ui::palette::header(), &ui::clip_to_width(&prefix, cols),)
        )?;
        return Ok(());
    }
    let mut segments = vec![(prefix, ui::palette::header())];
    let mut candidates = Vec::new();
    for (count, glyph, noun, style) in [
        (summary.running, "▸", "running", ui::palette::cool()),
        (summary.failed, "✗", "failed", ui::palette::alarm()),
        (summary.held, "○", "held", ui::palette::muted()),
        (summary.ok, "●", "ok", ui::palette::good()),
    ] {
        if count > 0 {
            candidates.push((format!("{glyph} {count} {noun}"), style));
        }
    }
    if let Some((row, next)) = summary.next {
        candidates.push((
            format!("next: {} {}", row.name, ui::rel_until(next, now)),
            ui::palette::muted(),
        ));
    }
    if !hold {
        candidates.push(("q quit".to_owned(), ui::palette::faint()));
    }
    for (text, style) in candidates {
        if !push_band_segment(&mut segments, text, style, cols) {
            break;
        }
    }

    for (index, (text, style)) in segments.iter().enumerate() {
        if index > 0 {
            write!(out, "{}", ui::paint(ui::palette::faint(), " · "))?;
        }
        write!(out, "{}", ui::paint(*style, text))?;
    }
    writeln!(out)
}

fn push_band_segment(
    segments: &mut Vec<(String, anstyle::Style)>,
    text: String,
    style: anstyle::Style,
    cols: usize,
) -> bool {
    let width = segments.iter().map(|(text, _)| text.width()).sum::<usize>()
        + 3 * segments.len()
        + text.width();
    if width <= cols {
        segments.push((text, style));
        true
    } else {
        false
    }
}

fn write_dashboard_heading(
    out: &mut impl Write,
    root: &Path,
    room_is_open: bool,
    cols: usize,
) -> std::io::Result<()> {
    let room = room_label(room_is_open);
    let suffix = format!(" · {room}");
    let root = ui::home_relative(root.to_string_lossy().as_ref());
    let root_budget = cols.saturating_sub(suffix.width());
    write!(
        out,
        "{}",
        ui::paint(
            ui::palette::header(),
            &ui::clip_to_width(&root, root_budget)
        )
    )?;
    if root_budget > 0 && suffix.width() <= cols {
        write!(out, " · {}", ui::paint(room_style(room_is_open), room))?;
    }
    writeln!(out)
}

fn write_more(out: &mut impl Write, count: usize, cols: usize) -> std::io::Result<()> {
    let text = ui::clip_to_width(&format!("+{count} more"), cols);
    writeln!(out, "{}", ui::paint(ui::palette::faint(), &text))
}

#[cfg(test)]
mod tests;
