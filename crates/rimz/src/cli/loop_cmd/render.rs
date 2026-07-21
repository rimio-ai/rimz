//! List and show loop tasks plus recorded run details.

use super::*;
use unicode_width::UnicodeWidthStr;

const NOTE_MAX: usize = 60;
const WATCH_NARROW: usize = 44;
const WATCH_WIDE: usize = 68;

struct ListRowContext<'a> {
    stats: &'a BTreeMap<String, run_log::LoopRunStats>,
    now: Timestamp,
}

struct ObservedTask<'a> {
    name: &'a str,
    task: &'a LoadedTask,
    timing: schedule::TaskTiming,
}

struct ObservedTaskGroup<'a> {
    root: PathBuf,
    room_is_open: bool,
    tasks: Vec<ObservedTask<'a>>,
}

fn grouped_tasks<'a>(
    tasks: &'a BTreeMap<String, LoadedTask>,
    pauses: &BTreeMap<String, PauseEntry>,
    now_zoned: &jiff::Zoned,
) -> Vec<ObservedTaskGroup<'a>> {
    let mut entries_by_root: BTreeMap<PathBuf, Vec<(&str, &LoadedTask)>> = BTreeMap::new();
    for (name, task) in tasks {
        entries_by_root
            .entry(task.entry().resolved_root())
            .or_default()
            .push((name, task));
    }
    entries_by_root
        .into_iter()
        .map(|(root, entries)| {
            let runtime = runtime_for_root(&root);
            let room_is_open = runtime.as_ref().is_some_and(fresh_sidebar_present);
            let stamps = runtime
                .as_ref()
                .map(schedule::last_stamps)
                .unwrap_or_default();
            let tasks = entries
                .into_iter()
                .map(|(name, task)| ObservedTask {
                    name,
                    task,
                    timing: observe_task_timing(
                        name,
                        task,
                        task.source().blocked_state(),
                        &stamps,
                        pauses.get(name),
                        now_zoned,
                    ),
                })
                .collect();
            ObservedTaskGroup {
                root,
                room_is_open,
                tasks,
            }
        })
        .collect()
}

// ---- list -------------------------------------------------------------------

pub(super) fn list(globals: &GlobalFlags) -> Result<()> {
    let catalog = task_catalog(globals)?;
    let tasks = catalog.visible();
    let pause_entries = pauses::load();
    let mut out = ui::out();
    if tasks.is_empty() {
        writeln!(out, "no loop tasks; add one with `rimz loop add`")?;
        return Ok(());
    }
    let now = Timestamp::now();
    let now_zoned = now.to_zoned(MachineConfig::load_lenient().time_zone());
    let stats = run_log::stats(&state_home(), &now_zoned);
    let mut blocked_count = 0;
    let groups = grouped_tasks(tasks, &pause_entries, &now_zoned);
    for (idx, group) in groups.into_iter().enumerate() {
        if idx > 0 {
            writeln!(out)?;
        }
        write_root_heading(&mut out, &group.root, group.room_is_open)?;
        let mut table = ui::Table::new([
            "NAME", "TASK", "SOURCE", "SCHEDULE", "LAST", "STATUS", "COST", "NEXT",
        ])
        .right(&[6])
        .indent(2);
        let context = ListRowContext { stats: &stats, now };
        for task in group.tasks {
            blocked_count += usize::from(task.task.source().blocked_state().is_some());
            table.row(task_row(&task, &context));
        }
        table.render(&mut out)?;
    }
    if blocked_count > 0 {
        writeln!(out)?;
        write_blocked_footer(&mut out, blocked_count)?;
    }
    Ok(())
}

fn task_row(task: &ObservedTask<'_>, context: &ListRowContext<'_>) -> Vec<ui::Cell> {
    let entry = task.task.entry();
    let when = match task.timing.parsed() {
        Ok(schedule) => schedule.describe(),
        Err(err) => format!("invalid: {err}"),
    };
    let next = next_cell(&task.timing, context.now);
    let (last, status) = context
        .stats
        .get(task.name)
        .map(|stats| last_run_cells(stats, context.now))
        .unwrap_or_else(|| (ui::cell("-").dash(), ui::cell("-").dash()));
    let cost = list_cost_label(
        entry,
        context
            .stats
            .get(task.name)
            .map_or(0.0, |stats| stats.spend_today_usd),
    )
    .map(ui::cell)
    .unwrap_or_else(|| ui::cell("-").dash());
    vec![
        ui::cell(task.name).fg(ui::palette::body()),
        ui::cell(task_subject(task.task)),
        source_cell(task.task.source()),
        ui::cell(when),
        last,
        status,
        cost,
        next,
    ]
}

fn next_cell(timing: &schedule::TaskTiming, now: Timestamp) -> ui::Cell {
    match timing.state() {
        schedule::TaskTimingState::Blocked(state) => blocked_next_cell(state),
        schedule::TaskTimingState::Paused(PauseEntry {
            until: None,
            strikes: Some(strikes),
        }) => ui::cell(format!("paused · {strikes} strikes")).fg(ui::palette::muted()),
        schedule::TaskTimingState::Paused(PauseEntry {
            until: None,
            strikes: None,
        }) => ui::cell("paused").fg(ui::palette::muted()),
        schedule::TaskTimingState::Paused(PauseEntry {
            until: Some(until), ..
        }) => ui::cell(format!("paused · {}", ui::rel_until(until, now))).fg(ui::palette::muted()),
        schedule::TaskTimingState::Upcoming(next) | schedule::TaskTimingState::Due(next) => {
            ui::cell(ui::rel_until(next, now))
        }
        schedule::TaskTimingState::Invalid
        | schedule::TaskTimingState::Unarmed
        | schedule::TaskTimingState::NoOccurrence => ui::cell("-").dash(),
    }
}

fn blocked_next_cell(state: TrustState) -> ui::Cell {
    ui::cell("blocked · trust").fg(ui::status::trust(state))
}

fn source_cell(source: TaskSource) -> ui::Cell {
    let cell = ui::cell(source_label(source));
    match source.blocked_state() {
        Some(state) => cell.fg(ui::status::trust(state)),
        None => cell,
    }
}

fn write_blocked_footer(out: &mut impl Write, count: usize) -> std::io::Result<()> {
    writeln!(
        out,
        "{}",
        ui::paint(
            ui::palette::warn(),
            &format!(
                "{count} task(s) blocked by project trust — review with `rimz trust`, approve with `rimz trust grant`"
            )
        )
    )
}

pub(super) fn room_open(root: &Path) -> bool {
    runtime_for_root(root)
        .as_ref()
        .is_some_and(fresh_sidebar_present)
}

pub(super) fn watch(args: WatchArgs, globals: &GlobalFlags) -> Result<()> {
    let project_root = project_root_for_globals(globals);
    let _input = TerminalModeGuard::enable(MouseCapture::Off, Screen::Alternate)?;
    loop {
        repaint_watch(project_root.as_deref(), args.hold)?;
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

fn repaint_watch(project_root: Option<&Path>, hold: bool) -> Result<()> {
    let mut frame = Vec::new();
    render_watch_frame(&mut frame, project_root, hold)?;
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

fn render_watch_frame(out: &mut impl Write, project_root: Option<&Path>, hold: bool) -> Result<()> {
    let catalog = TaskCatalog::load(project_root)?;
    let now = Timestamp::now();
    let pause_entries = pauses::load();
    let now_zoned = now.to_zoned(MachineConfig::load_lenient().time_zone());
    let stats = run_log::stats(&state_home(), &now_zoned);
    let context = ListRowContext { stats: &stats, now };
    let groups = grouped_tasks(catalog.visible(), &pause_entries, &now_zoned)
        .into_iter()
        .map(|group| WatchGroup {
            root: group.root,
            room_is_open: group.room_is_open,
            rows: group
                .tasks
                .into_iter()
                .map(|task| watch_row_model(&task, &context))
                .collect(),
        })
        .collect::<Vec<_>>();
    let (cols, rows) = rimz::mux::detect_terminal_size().unwrap_or((80, 24));
    render_dashboard(
        out,
        &groups,
        usize::from(cols),
        usize::from(rows),
        now,
        hold,
    )?;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowState {
    Running,
    Due,
    Paused,
    Blocked,
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
    Paused,
    Ok,
}

impl WatchRow {
    fn band(&self) -> WatchBand {
        if self.state == RowState::Running {
            WatchBand::Running
        } else if self.failed {
            WatchBand::Failed
        } else if matches!(self.state, RowState::Paused | RowState::Blocked) {
            WatchBand::Paused
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
                RowState::Upcoming(_) => 3,
                RowState::NeverRun => 5,
                RowState::Running | RowState::Paused | RowState::Blocked => 0,
            },
            WatchBand::Paused => 4,
        };
        let next = match self.state {
            RowState::Upcoming(next) => Some(next),
            _ => None,
        };
        (rank, next, &self.name)
    }

    fn eligible_next(&self) -> Option<Timestamp> {
        (!matches!(self.state, RowState::Paused | RowState::Blocked))
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
    paused: usize,
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
                WatchBand::Paused => summary.paused += 1,
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

fn watch_row_model(task: &ObservedTask<'_>, context: &ListRowContext<'_>) -> WatchRow {
    let running = matches!(
        probe_run_lock(task.name, task.task.entry()),
        Ok(RunLockState::Held(_))
    );
    let next_ts = watch_next_timestamp(&task.timing, running);
    let state = if running {
        RowState::Running
    } else {
        row_state_for_timing(&task.timing)
    };
    let (glyph, glyph_style, failed, last_text, status_text) = context.stats.get(task.name).map_or(
        (
            "○",
            ui::palette::faint(),
            false,
            "—".to_owned(),
            "never run".to_owned(),
        ),
        |stats| {
            let status = run_status(&stats.last);
            (
                status.glyph,
                status.style,
                status.style == ui::palette::alarm(),
                ui::rel_age(stats.last.at, context.now),
                status.label,
            )
        },
    );
    let next_text = match state {
        RowState::Running => "running now".to_owned(),
        RowState::Paused => timing_next_text(&task.timing, context.now),
        RowState::Blocked => "blocked · trust".to_owned(),
        RowState::Due => "due".to_owned(),
        RowState::Upcoming(next) => ui::until_label(next, context.now),
        RowState::NeverRun => "—".to_owned(),
    };
    WatchRow {
        name: task.name.to_owned(),
        glyph,
        glyph_style,
        state,
        failed,
        next_ts,
        next_text,
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

fn row_state_for_timing(timing: &schedule::TaskTiming) -> RowState {
    match timing.state() {
        schedule::TaskTimingState::Blocked(_) => RowState::Blocked,
        schedule::TaskTimingState::Paused(_) => RowState::Paused,
        schedule::TaskTimingState::Due(_) => RowState::Due,
        schedule::TaskTimingState::Upcoming(next) => RowState::Upcoming(next),
        schedule::TaskTimingState::Invalid
        | schedule::TaskTimingState::Unarmed
        | schedule::TaskTimingState::NoOccurrence => RowState::NeverRun,
    }
}

fn timing_next_text(timing: &schedule::TaskTiming, now: Timestamp) -> String {
    match timing.state() {
        schedule::TaskTimingState::Blocked(_) => "blocked · trust".to_owned(),
        schedule::TaskTimingState::Paused(PauseEntry {
            until: Some(until), ..
        }) => format!("paused · {}", ui::rel_until(until, now)),
        schedule::TaskTimingState::Paused(PauseEntry {
            until: None,
            strikes: Some(strikes),
        }) => format!("paused · {strikes} strikes"),
        schedule::TaskTimingState::Paused(PauseEntry {
            until: None,
            strikes: None,
        }) => "paused".to_owned(),
        schedule::TaskTimingState::Due(_) => "due".to_owned(),
        schedule::TaskTimingState::Upcoming(next) => ui::until_label(next, now),
        schedule::TaskTimingState::Invalid
        | schedule::TaskTimingState::Unarmed
        | schedule::TaskTimingState::NoOccurrence => "—".to_owned(),
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
            RowState::Paused | RowState::Blocked => ui::palette::muted(),
            RowState::NeverRun => ui::palette::faint(),
            RowState::Due => ui::palette::warn(),
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
        (summary.paused, "○", "paused", ui::palette::muted()),
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

fn write_root_heading(
    out: &mut impl Write,
    root: &Path,
    room_is_open: bool,
) -> std::io::Result<()> {
    writeln!(
        out,
        "{} · {}",
        ui::paint(
            ui::palette::header(),
            &ui::home_relative(root.to_string_lossy().as_ref())
        ),
        ui::paint(room_style(room_is_open), room_label(room_is_open))
    )
}

fn root_with_room(root: &Path, room_is_open: bool) -> String {
    format!(
        "{} · {}",
        ui::home_relative(root.to_string_lossy().as_ref()),
        room_label(room_is_open)
    )
}

fn room_label(room_is_open: bool) -> &'static str {
    if room_is_open { "room open" } else { "no room" }
}

fn room_style(room_is_open: bool) -> anstyle::Style {
    if room_is_open {
        ui::palette::good()
    } else {
        ui::palette::muted()
    }
}

fn schedule_style<T, E>(parsed: std::result::Result<&T, &E>) -> anstyle::Style {
    if parsed.is_ok() {
        anstyle::Style::new()
    } else {
        ui::palette::alarm()
    }
}

struct ActionWords {
    subject: String,
    base: &'static str,
    third_person: &'static str,
    progressive: &'static str,
    untouched: &'static str,
}

fn action_words(action: &TaskAction) -> Option<ActionWords> {
    match action {
        TaskAction::Spawn(subject) => Some(ActionWords {
            subject: subject.clone(),
            base: "start",
            third_person: "starts",
            progressive: "starting",
            untouched: "not started",
        }),
        TaskAction::Deliver(target) => Some(ActionWords {
            subject: target.handle.clone(),
            base: "wake",
            third_person: "wakes",
            progressive: "waking",
            untouched: "not woken",
        }),
        TaskAction::CheckOnly => None,
    }
}

fn check_summary(entry: &TaskEntry, action: Option<&TaskAction>) -> Option<String> {
    let check = entry.check.as_ref()?;
    if let Some(action) = action.and_then(action_words) {
        Some(format!(
            "{check} ({} {} on {})",
            action.third_person,
            action.subject,
            check_on_label(entry.on.unwrap_or_default())
        ))
    } else {
        Some(check.clone())
    }
}

pub(super) fn task_run_rule(entry: &TaskEntry, task_action: &TaskAction) -> String {
    let action = action_words(task_action).map(|words| format!("{} {}", words.base, words.subject));
    let mut rule = match (entry.check.is_some(), action) {
        (true, Some(action)) => format!(
            "check, then {action} on {}",
            check_on_label(entry.on.unwrap_or_default())
        ),
        (true, None) => "check".to_owned(),
        (false, Some(action)) => action,
        (false, None) => "run".to_owned(),
    };
    if let Some(cmd) = entry.verify.as_deref() {
        let attempts = entry.max_attempts.unwrap_or(3);
        rule.push_str(&format!(", verify `{cmd}` (up to {attempts} attempts)"));
    }
    rule
}

pub(super) fn action_progressive_phrase(action: &TaskAction) -> String {
    action_words(action)
        .map(|words| format!("{} {}", words.progressive, words.subject))
        .unwrap_or_else(|| "running <invalid>".to_owned())
}

pub(super) fn check_skip_decision(entry: &TaskEntry, task_action: &TaskAction) -> String {
    let Some(action) = action_words(task_action) else {
        return "<invalid> unchanged".to_owned();
    };
    let condition = match entry.on.unwrap_or_default() {
        CheckOn::Fail => "fails",
        CheckOn::Success => "passes",
    };
    format!(
        "{} {}; fires when the check {condition}",
        action.subject, action.untouched
    )
}

fn check_on_label(on: CheckOn) -> &'static str {
    match on {
        CheckOn::Fail => "fail",
        CheckOn::Success => "success",
    }
}

fn source_label(source: TaskSource) -> String {
    if let Some(state) = source.blocked_state() {
        format!("project · {}", state.as_str())
    } else {
        source.label().to_owned()
    }
}

fn budget_amount(raw: &str) -> String {
    raw.parse::<rimz::harness::budget::BudgetSpec>()
        .map(|spec| format_budget_cap(spec.cap_usd))
        .unwrap_or_else(|_| raw.to_owned())
}

fn format_budget_cap(cap_usd: f64) -> String {
    if cap_usd.fract() == 0.0 {
        format!("${cap_usd:.0}")
    } else {
        format!("${cap_usd:.2}")
    }
}

fn budget_label(entry: &TaskEntry) -> Option<String> {
    let mut segments = Vec::new();
    if let Some(raw) = entry.budget.as_deref() {
        segments.push(format!("{} per run", budget_amount(raw)));
    }
    if let Some(raw) = entry.budget_per_day.as_deref() {
        segments.push(format!("{} per day", budget_amount(raw)));
    }
    (!segments.is_empty()).then(|| segments.join(" · "))
}

fn surplus_label(entry: &TaskEntry) -> Option<String> {
    let mut segments = Vec::new();
    if entry.surplus.is_some() || entry.surplus_after.is_some() {
        let threshold = entry
            .surplus
            .as_deref()
            .and_then(|raw| schedule::parse_surplus(raw).ok())
            .unwrap_or(1.0);
        segments.push(format!("surplus ≥ {threshold:.1}x"));
    }
    if let Some(after) = entry.surplus_after.as_deref() {
        segments.push(format!("after {after} of window"));
    }
    (!segments.is_empty()).then(|| segments.join(" · "))
}

fn list_cost_label(entry: &TaskEntry, spend_today_usd: f64) -> Option<String> {
    if let Some(cap) = entry
        .budget_per_day
        .as_deref()
        .and_then(|raw| raw.parse::<rimz::harness::budget::BudgetSpec>().ok())
    {
        return Some(format!(
            "${spend_today_usd:.2}/{}",
            format_budget_cap(cap.cap_usd)
        ));
    }
    (spend_today_usd > 0.0).then(|| format!("${spend_today_usd:.2}"))
}

fn spend_label(
    entry: &TaskEntry,
    records: &[LoopRunRecord],
    now: &jiff::Zoned,
    full: bool,
) -> Option<String> {
    let spend_today_usd = run_log::spend_on_local_day(records, now);
    let mut segments = Vec::new();
    if entry.budget_per_day.is_some() || run_log::has_cost_on_local_day(records, now) {
        let mut today = format!("${spend_today_usd:.2} today");
        if let Some(raw) = entry.budget_per_day.as_deref() {
            today.push_str(" of ");
            today.push_str(&budget_amount(raw));
        }
        segments.push(today);
    }
    if full {
        let summary = run_log::cost_summary(records);
        if let Some(last_usd) = summary.last_usd {
            segments.push(format!("${last_usd:.2} last"));
        }
        if summary.costed_runs >= 2
            && let Some(avg_usd) = summary.avg_usd
        {
            segments.push(format!("ø ${avg_usd:.2} over {} runs", summary.costed_runs));
        }
    }
    (!segments.is_empty()).then(|| segments.join(" · "))
}

fn source_detail(source: TaskSource, entry: &TaskEntry) -> String {
    format!(
        "{} — {}",
        source_label(source),
        display_path(&source_path(source, entry))
    )
}

fn source_path(source: TaskSource, entry: &TaskEntry) -> PathBuf {
    source.path(entry)
}

fn display_path(path: &Path) -> String {
    ui::home_relative(path.to_string_lossy().as_ref())
}

fn has_agent_runs_section(task: &LoadedTask) -> bool {
    task.entry().check.is_some() && task.action().ok().and_then(action_words).is_some()
}

pub(super) fn show(args: ShowArgs, globals: &GlobalFlags) -> Result<()> {
    let task = load_task(&args.name, globals)?.ok_or_else(|| {
        anyhow::anyhow!("no loop task named `{}`; see `rimz loop list`", args.name)
    })?;
    let entry = task.entry();
    let source = task.source();
    let root = entry.resolved_root();
    let runtime = runtime_for_root(&root);
    let stamps = runtime
        .as_ref()
        .map(rimz::harness::schedule::last_stamps)
        .unwrap_or_default();
    let now = Timestamp::now();
    let pause = pauses::load().remove(&args.name);
    let now_zoned = now.to_zoned(MachineConfig::load_lenient().time_zone());
    let timing = observe_task_timing(
        &args.name,
        &task,
        source.blocked_state(),
        &stamps,
        pause.as_ref(),
        &now_zoned,
    );
    let records = run_log::task_records(&state_home(), &args.name);
    let show_agent_runs = has_agent_runs_section(&task);
    let lock_state = probe_run_lock(&args.name, entry).ok();
    let active_run = if matches!(lock_state.as_ref(), Some(RunLockState::Held(_))) {
        newest_active_run_for_entry(&args.name, entry)?
    } else {
        None
    };

    let mut out = ui::out();
    write_show_headline(&mut out, &args.name, &timing, now)?;
    if let Some((verdict, style)) = verdict_line(&records, now) {
        writeln!(out, "  {}", ui::paint(style, &verdict))?;
    }
    write_show_facts(
        &mut out,
        &args.name,
        &task,
        &records,
        ShowFactsContext {
            now_zoned: &now_zoned,
            is_paused: timing.active_pause().is_some(),
            lock_state: lock_state.as_ref(),
            active_run: active_run.as_ref(),
            full_spend: !show_agent_runs,
        },
    )?;

    if show_agent_runs {
        write_agent_runs(&mut out, &records, now)?;
    }

    if records.is_empty() {
        writeln!(out)?;
        writeln!(out, "no runs recorded; try `rimz loop fire {}`", args.name)?;
        return Ok(());
    }

    write_runs_table(&mut out, &records, args.runs, now)?;
    let (detail_idx, failure_idx) = detail_indices(&records);
    if let Some(detail) = detail_idx.and_then(|idx| records.get(idx)) {
        writeln!(out)?;
        render_record_detail(&mut out, entry, detail, "LAST RUN", now)?;
    }
    if let Some(failure) = failure_idx.and_then(|idx| records.get(idx)) {
        write_failure_pointer(&mut out, &args.name, failure, now)?;
    }
    Ok(())
}

pub(super) fn logs(args: LogsArgs, globals: &GlobalFlags) -> Result<()> {
    let task = load_task(&args.name, globals)?.ok_or_else(|| {
        anyhow::anyhow!("no loop task named `{}`; see `rimz loop list`", args.name)
    })?;
    let entry = task.entry();
    let records = run_log::task_records(&state_home(), &args.name);
    let visible = records
        .iter()
        .filter(|record| !args.failed || record_is_failure(record))
        .rev()
        .take(args.runs)
        .collect::<Vec<_>>();
    let mut out = ui::out();
    if visible.is_empty() {
        if args.failed {
            writeln!(out, "no failed runs recorded")?;
        } else {
            writeln!(out, "no runs recorded; try `rimz loop fire {}`", args.name)?;
        }
        return Ok(());
    }
    let now = Timestamp::now();
    for (idx, record) in visible.iter().rev().enumerate() {
        if idx > 0 {
            writeln!(out)?;
        }
        let status = run_status(record);
        write!(
            out,
            "{}",
            ui::paint(status.style, &format!("{} {}", status.glyph, status.label))
        )?;
        write!(
            out,
            " · {} · {}",
            ui::rel_age(record.at, now),
            record.mode.map_or("legacy", LoopRunMode::label)
        )?;
        if let Some(exit) = detail_exit_segment(record) {
            write!(out, " · {exit}")?;
        }
        writeln!(out)?;
        write_record_forensics(&mut out, entry, record)?;
    }
    Ok(())
}

fn write_show_headline(
    out: &mut impl Write,
    name: &str,
    timing: &schedule::TaskTiming,
    now: Timestamp,
) -> std::io::Result<()> {
    let schedule_text = match timing.parsed() {
        Ok(parsed) => parsed.describe(),
        Err(err) => format!("invalid: {err}"),
    };
    write!(
        out,
        "{} — {}",
        ui::paint(ui::palette::header(), name),
        ui::paint(schedule_style(timing.parsed()), &schedule_text)
    )?;
    match timing.state() {
        schedule::TaskTimingState::Blocked(state) => write!(
            out,
            " · next {}",
            ui::paint(ui::status::trust(state), "blocked · trust")
        )?,
        schedule::TaskTimingState::Paused(PauseEntry {
            until: None,
            strikes: Some(strikes),
        }) => write!(
            out,
            " · paused after {strikes} strikes — resume with `rimz loop resume {}`",
            name
        )?,
        schedule::TaskTimingState::Paused(PauseEntry {
            until: None,
            strikes: None,
        }) => write!(out, " · paused")?,
        schedule::TaskTimingState::Paused(PauseEntry {
            until: Some(until), ..
        }) => write!(out, " · paused, resumes {}", pause_until_text(until, now))?,
        schedule::TaskTimingState::Upcoming(next) | schedule::TaskTimingState::Due(next) => {
            write!(out, " · next {}", ui::rel_until(next, now))?;
        }
        schedule::TaskTimingState::Invalid
        | schedule::TaskTimingState::Unarmed
        | schedule::TaskTimingState::NoOccurrence => {}
    }
    writeln!(out)?;
    if matches!(
        timing.active_pause(),
        Some(PauseEntry {
            until: None,
            strikes: None,
        })
    ) {
        writeln!(out, "  resume with `rimz loop resume {name}`")?;
    }
    Ok(())
}

struct ShowFactsContext<'a> {
    now_zoned: &'a jiff::Zoned,
    is_paused: bool,
    lock_state: Option<&'a RunLockState>,
    active_run: Option<&'a RunRecord>,
    full_spend: bool,
}

fn write_show_facts(
    out: &mut impl Write,
    name: &str,
    task: &LoadedTask,
    records: &[LoopRunRecord],
    context: ShowFactsContext<'_>,
) -> std::io::Result<()> {
    let entry = task.entry();
    let source = task.source();
    let root = entry.resolved_root();
    let room_is_open = room_open(&root);
    let blocked_state = source.blocked_state();
    let strike_count = strikes::load().get(name).copied().unwrap_or(0);
    let run_id = context.active_run.map(|record| record.run_id.as_str());
    let active = match context.lock_state {
        Some(RunLockState::Held(Some(info))) => Some(format!(
            "run in progress{} · pid {} · started {}",
            run_id
                .map(|run_id| format!(" · run {run_id}"))
                .unwrap_or_default(),
            info.pid,
            ui::rel_age(info.started_at, Timestamp::now())
        )),
        Some(RunLockState::Held(None)) => Some(
            run_id
                .map(|run_id| format!("run in progress · run {run_id}"))
                .unwrap_or_else(|| "run in progress".to_owned()),
        ),
        Some(RunLockState::Available) | None => None,
    };
    let has_active_run = active.is_some();
    let timeout = entry.timeout.clone().or_else(|| {
        entry.agent.as_ref().map(|_| {
            format!(
                "{} (default)",
                MachineConfig::load_lenient()
                    .r#loop
                    .default_timeout
                    .clone()
                    .unwrap_or_else(|| { SCHEDULED_RUN_DEFAULT_TIMEOUT_LABEL.to_owned() })
            )
        })
    });
    let mut kv = ui::KeyVals::new().indent(2);
    kv.push("task", ui::cell(task_subject(task)));
    if let Some(check) = check_summary(entry, task.action().ok()) {
        kv.push("check", ui::cell(check));
    }
    if let Some(verify) = entry.verify.as_deref() {
        kv.push(
            "verify",
            ui::cell(format!(
                "{verify} (up to {} attempts)",
                entry.max_attempts.unwrap_or(3)
            )),
        );
    }
    kv.push("root", ui::cell(root_with_room(&root, room_is_open)));
    kv.push("source", ui::cell(source_detail(source, entry)));
    if let Some(active) = active {
        kv.push("active", ui::cell(active));
    }
    if let Some(state) = blocked_state {
        kv.push(
            "will not fire",
            ui::cell(blocked_notice(state)).fg(ui::status::trust(state)),
        );
    }
    if let Some(budget) = budget_label(entry) {
        kv.push("budget", ui::cell(budget));
    }
    if let Some(surplus) = surplus_label(entry) {
        kv.push("surplus", ui::cell(surplus));
    }
    if let Some(spend) = spend_label(entry, records, context.now_zoned, context.full_spend) {
        kv.push("spend", ui::cell(spend));
    }
    if !context.is_paused
        && strike_count > 0
        && let Some(max) = strikes::threshold(entry)
    {
        kv.push(
            "strikes",
            ui::cell(format!("{strike_count}/{max}")).fg(ui::palette::muted()),
        );
    }
    kv.render(out)?;
    if let Some(timeout) = timeout {
        writeln!(out, "  timeout: {timeout}")?;
    }
    if has_active_run {
        writeln!(out, "  stop with `rimz loop stop {name}`")?;
    }
    Ok(())
}

fn write_agent_runs(
    out: &mut impl Write,
    records: &[LoopRunRecord],
    now: Timestamp,
) -> std::io::Result<()> {
    let agent_runs = records
        .iter()
        .filter(|record| is_agent_run(record))
        .collect::<Vec<_>>();
    writeln!(out)?;
    let heading = if agent_runs.is_empty() {
        format!("AGENT RUNS — none in {} runs", records.len())
    } else {
        let costs = agent_runs
            .iter()
            .filter_map(|record| valid_cost(record))
            .collect::<Vec<_>>();
        let mut heading = format!(
            "AGENT RUNS — {} of {} runs",
            agent_runs.len(),
            records.len()
        );
        if !costs.is_empty() {
            let total = costs.iter().sum::<f64>();
            let average = total / costs.len() as f64;
            heading.push_str(&format!(" · ${total:.2} total · ø ${average:.2}"));
        }
        heading
    };
    writeln!(out, "{}", ui::paint(ui::palette::header(), &heading))?;
    if agent_runs.is_empty() {
        return Ok(());
    }

    let start = agent_runs.len().saturating_sub(5);
    let visible = &agent_runs[start..];
    let show_note = visible.iter().any(|record| record_note(record).is_some());
    let mut headers = vec!["WHEN", "STATUS", "TOOK", "COST"];
    if show_note {
        headers.push("NOTE");
    }
    let mut table = ui::Table::new(headers).right(&[2, 3]).indent(2);
    for record in visible {
        let mut cells = vec![
            ui::cell(ui::rel_age(record.at, now)),
            run_status_cell(record, 1),
            ui::cell(
                record
                    .duration_ms
                    .map(format_duration_ms)
                    .unwrap_or_else(|| "-".to_owned()),
            )
            .dash(),
            cost_cell(record),
        ];
        if show_note {
            cells.push(ui::cell(record_note(record).as_deref().unwrap_or("-")).dash());
        }
        table.row(cells);
    }
    table.render(out)
}

fn write_runs_table(
    out: &mut impl Write,
    records: &[LoopRunRecord],
    limit: usize,
    now: Timestamp,
) -> std::io::Result<()> {
    writeln!(out)?;
    writeln!(
        out,
        "{}",
        ui::paint(
            ui::palette::header(),
            &format!("RECENT RUNS ({} recorded)", records.len())
        )
    )?;
    let rows = collapsed_run_rows(records);
    let start = rows.len().saturating_sub(limit);
    let visible_rows = &rows[start..];
    let show_note = visible_rows.iter().any(|row| row.key.note.is_some());
    let show_tokens = visible_rows
        .iter()
        .any(|row| row.latest.input_tokens.is_some() || row.latest.output_tokens.is_some());
    let mut headers = vec!["WHEN", "MODE", "STATUS", "TOOK", "COST"];
    if show_tokens {
        headers.push("TOKENS");
    }
    if show_note {
        headers.push("NOTE");
    }
    let mut table = ui::Table::new(headers).right(&[4]).indent(2);
    for row in visible_rows {
        let record = row.latest;
        let mut cells = vec![
            ui::cell(ui::rel_age(record.at, now)),
            ui::cell(row.key.mode.map_or("-", LoopRunMode::label)).dash(),
            run_status_cell(record, row.count),
            ui::cell(
                record
                    .duration_ms
                    .map(format_duration_ms)
                    .unwrap_or_else(|| "-".to_owned()),
            )
            .dash(),
            cost_cell(record),
        ];
        if show_tokens {
            cells.push(
                ui::cell(
                    token_segments(record.input_tokens, record.output_tokens)
                        .unwrap_or_else(|| "-".to_owned()),
                )
                .dash(),
            );
        }
        if show_note {
            cells.push(ui::cell(row.key.note.as_deref().unwrap_or("-")).dash());
        }
        table.row(cells);
    }
    table.render(out)
}

fn blocked_notice(state: TrustState) -> String {
    format!(
        "project trust is {} — review with `rimz trust`, approve with `rimz trust grant`",
        state.as_str()
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RunRowKey {
    mode: Option<LoopRunMode>,
    result: LoopRunResult,
    exit: Option<String>,
    note: Option<String>,
}

impl RunRowKey {
    fn new(record: &LoopRunRecord) -> Self {
        Self {
            mode: record.mode,
            result: record.result,
            exit: record_exit(record),
            note: record_note(record),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct CollapsedRunRow<'a> {
    key: RunRowKey,
    latest: &'a LoopRunRecord,
    count: usize,
}

fn collapsed_run_rows(records: &[LoopRunRecord]) -> Vec<CollapsedRunRow<'_>> {
    let mut rows = Vec::<CollapsedRunRow<'_>>::new();
    for record in records {
        let key = RunRowKey::new(record);
        if let Some(row) = rows.last_mut().filter(|row| row.key == key) {
            row.count += 1;
            if record.at >= row.latest.at {
                row.latest = record;
            }
        } else {
            rows.push(CollapsedRunRow {
                key,
                latest: record,
                count: 1,
            });
        }
    }
    rows
}

fn last_run_cells(stats: &run_log::LoopRunStats, now: Timestamp) -> (ui::Cell, ui::Cell) {
    let status = run_status(&stats.last);
    let mut label = format!("{} {}", status.glyph, status.label);
    if stats.streak > 1 {
        label.push_str(&format!(" ×{}", stats.streak));
    }
    if failure_note_visible(stats.last.result)
        && let Some(note) = record_note(&stats.last)
    {
        label.push_str(" · ");
        label.push_str(&note);
    }
    (
        ui::cell(ui::rel_age(stats.last.at, now)),
        ui::cell(label).fg(status.style),
    )
}

fn run_status_cell(record: &LoopRunRecord, count: usize) -> ui::Cell {
    let status = run_status(record);
    let mut label = format!("{} {}", status.glyph, status.label);
    if count > 1 {
        label.push_str(&format!(" ×{count}"));
    }
    ui::cell(label).fg(status.style)
}

struct RunStatusDisplay {
    glyph: &'static str,
    label: String,
    style: anstyle::Style,
}

fn run_status(record: &LoopRunRecord) -> RunStatusDisplay {
    let label = match record.result {
        LoopRunResult::Failed => {
            let mut label = record.result.label().to_owned();
            if let Some(exit) = failure_exit_label(record) {
                label.push_str(" (");
                label.push_str(&exit);
                label.push(')');
            }
            label
        }
        LoopRunResult::CheckSkipped => check_skipped_label(record).to_owned(),
        result => result.label().to_owned(),
    };
    let mark = match record.result {
        LoopRunResult::CheckSkipped => {
            let (glyph, style) = check_skip_display(record.check.as_ref());
            ResultMark { glyph, style }
        }
        result => loop_result_mark(result),
    };
    RunStatusDisplay {
        glyph: mark.glyph,
        label,
        style: mark.style,
    }
}

fn record_is_good(record: &LoopRunRecord) -> bool {
    matches!(
        record.result,
        LoopRunResult::Completed | LoopRunResult::Delivered
    ) || matches!(record.result, LoopRunResult::CheckSkipped)
        && record
            .check
            .as_ref()
            .is_some_and(|check| check.code == Some(0))
}

fn verdict_line(records: &[LoopRunRecord], now: Timestamp) -> Option<(String, anstyle::Style)> {
    let (decisive_idx, healthy) = records.iter().enumerate().rev().find_map(|(idx, record)| {
        if record_is_failure(record) {
            Some((idx, false))
        } else if record_is_good(record) {
            Some((idx, true))
        } else {
            None
        }
    })?;
    let boundary = records[..decisive_idx]
        .iter()
        .rposition(|record| {
            if healthy {
                record_is_failure(record)
            } else {
                record_is_good(record)
            }
        })
        .map_or(0, |idx| idx + 1);
    let matching = |record: &&LoopRunRecord| {
        if healthy {
            record_is_good(record)
        } else {
            record_is_failure(record)
        }
    };
    let mut streak = records[boundary..=decisive_idx].iter().filter(matching);
    let oldest = streak.next()?;
    let count = 1 + streak.count();
    let status = run_status(&records[decisive_idx]);
    let state = if healthy { "healthy" } else { "failing" };
    Some((
        format!(
            "{} {state} · {} ×{count} since {}",
            status.glyph,
            status.label,
            ui::rel_age(oldest.at, now)
        ),
        if healthy {
            ui::palette::good()
        } else {
            ui::palette::alarm()
        },
    ))
}

pub(super) fn check_skip_display(check: Option<&CheckRecord>) -> (&'static str, anstyle::Style) {
    match check {
        Some(check) if check.timed_out => ("○", ui::palette::warn()),
        Some(check) if check.code == Some(0) => ("✓", ui::palette::good()),
        _ => ("○", ui::palette::muted()),
    }
}

fn check_skipped_label(record: &LoopRunRecord) -> &'static str {
    match record.check.as_ref() {
        Some(check) if check.timed_out => "check timed out",
        Some(check) if check.code == Some(0) => "check passed",
        Some(_) => "check failed",
        None => "check failed",
    }
}

fn failure_exit_label(record: &LoopRunRecord) -> Option<String> {
    record_exit(record).map(|exit| match exit.as_str() {
        "timeout" | "signal" => exit,
        code => format!("exit {code}"),
    })
}

fn failure_note_visible(result: LoopRunResult) -> bool {
    matches!(
        result,
        LoopRunResult::Failed
            | LoopRunResult::VerifyFailed
            | LoopRunResult::TimedOut
            | LoopRunResult::BudgetExceeded
            | LoopRunResult::BudgetSkipped
            | LoopRunResult::SurplusSkipped
            | LoopRunResult::Errored
    )
}

#[derive(Clone, Copy)]
pub(super) struct ResultMark {
    pub(super) glyph: &'static str,
    pub(super) style: anstyle::Style,
}

pub(super) fn loop_result_mark(result: LoopRunResult) -> ResultMark {
    let (glyph, style) = match result {
        LoopRunResult::Completed | LoopRunResult::Delivered => ("✓", ui::palette::good()),
        LoopRunResult::Failed
        | LoopRunResult::VerifyFailed
        | LoopRunResult::TimedOut
        | LoopRunResult::BudgetExceeded
        | LoopRunResult::Errored => ("✗", ui::palette::alarm()),
        LoopRunResult::Expired
        | LoopRunResult::Canceled
        | LoopRunResult::TargetGone
        | LoopRunResult::Overlapped
        | LoopRunResult::BudgetSkipped => ("○", ui::palette::warn()),
        LoopRunResult::CheckSkipped | LoopRunResult::SurplusSkipped => ("○", ui::palette::muted()),
    };
    ResultMark { glyph, style }
}

pub(super) fn format_duration_ms(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms}ms")
    } else if ms < 10_000 {
        format!("{:.1}s", ms as f64 / 1_000.0)
    } else if ms < 60_000 {
        format!("{}s", ms / 1_000)
    } else {
        format!("{}m", ms / 60_000)
    }
}

fn valid_cost(record: &LoopRunRecord) -> Option<f64> {
    record
        .cost_usd
        .filter(|cost| cost.is_finite() && *cost >= 0.0)
}

fn cost_cell(record: &LoopRunRecord) -> ui::Cell {
    ui::cell(
        valid_cost(record)
            .map(|cost| format!("${cost:.2}"))
            .unwrap_or_else(|| "-".to_owned()),
    )
    .dash()
}

fn record_exit(record: &LoopRunRecord) -> Option<String> {
    if let Some(check) = &record.check {
        if check.timed_out {
            return Some("timeout".to_owned());
        }
        return Some(
            check
                .code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".to_owned()),
        );
    }
    record
        .run_id
        .as_ref()
        .and_then(|_| spawn_exit_code(record.result))
        .map(|code| code.to_string())
}

fn spawn_exit_code(result: LoopRunResult) -> Option<i32> {
    match result {
        LoopRunResult::Completed => Some(0),
        LoopRunResult::Failed => Some(1),
        LoopRunResult::VerifyFailed => Some(123),
        LoopRunResult::TimedOut => Some(124),
        LoopRunResult::BudgetExceeded => Some(125),
        LoopRunResult::Canceled => Some(130),
        _ => None,
    }
}

fn record_note(record: &LoopRunRecord) -> Option<String> {
    record
        .error
        .as_deref()
        .map(first_line)
        .or_else(|| check_failure_line(record))
        .or_else(|| record.last_message.as_deref().map(first_line))
        .or_else(|| record.target.as_deref().map(first_line))
        .map(|note| truncate_note(note, NOTE_MAX))
}

fn check_failure_line(record: &LoopRunRecord) -> Option<&str> {
    let check = record.check.as_ref()?;
    if !check.timed_out && check.code == Some(0) {
        return None;
    }
    check
        .output
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

fn truncate_note(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let clipped: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() && max >= 3 {
        format!("{}...", clipped.chars().take(max - 3).collect::<String>())
    } else {
        clipped
    }
}

fn record_has_detail(record: &LoopRunRecord) -> bool {
    record.check.is_some()
        || record.error.is_some()
        || record.last_message.is_some()
        || record.run_id.is_some()
        || record
            .cost_usd
            .is_some_and(|cost| cost.is_finite() && cost >= 0.0)
        || record.input_tokens.is_some()
        || record.output_tokens.is_some()
}

fn record_is_failure(record: &LoopRunRecord) -> bool {
    matches!(
        record.result,
        LoopRunResult::Errored
            | LoopRunResult::Failed
            | LoopRunResult::VerifyFailed
            | LoopRunResult::TimedOut
            | LoopRunResult::BudgetExceeded
    )
}

fn is_agent_run(record: &LoopRunRecord) -> bool {
    record.run_id.is_some()
        || matches!(
            record.result,
            LoopRunResult::Delivered | LoopRunResult::TargetGone
        )
}

fn detail_indices(records: &[LoopRunRecord]) -> (Option<usize>, Option<usize>) {
    let detail_idx = records.iter().rposition(record_has_detail);
    let failure_idx = records
        .iter()
        .enumerate()
        .rev()
        .find(|(idx, record)| {
            Some(*idx) != detail_idx && record_is_failure(record) && record_has_detail(record)
        })
        .map(|(idx, _record)| idx);
    (detail_idx, failure_idx)
}

fn render_record_detail(
    out: &mut impl Write,
    entry: &TaskEntry,
    record: &LoopRunRecord,
    title: &str,
    now: Timestamp,
) -> std::io::Result<()> {
    write!(out, "{} — ", ui::paint(anstyle::Style::new().bold(), title))?;
    let status = run_status(record);
    write!(
        out,
        "{}",
        ui::paint(status.style, &format!("{} {}", status.glyph, status.label))
    )?;
    write!(
        out,
        " · {} · {}",
        ui::rel_age(record.at, now),
        record.mode.map_or("legacy", LoopRunMode::label)
    )?;
    if let Some(exit) = detail_exit_segment(record) {
        write!(out, " · {exit}")?;
    }
    writeln!(out)?;
    write_record_forensics(out, entry, record)
}

fn write_failure_pointer(
    out: &mut impl Write,
    name: &str,
    record: &LoopRunRecord,
    now: Timestamp,
) -> std::io::Result<()> {
    write!(
        out,
        "{}",
        ui::paint(ui::palette::muted(), "  last failure — ")
    )?;
    let status = run_status(record);
    write!(
        out,
        "{}",
        ui::paint(status.style, &format!("{} {}", status.glyph, status.label))
    )?;
    writeln!(
        out,
        "{}",
        ui::paint(
            ui::palette::muted(),
            &format!(
                " · {} · {} · dig in: rimz loop logs {name} --failed",
                ui::rel_age(record.at, now),
                record.mode.map_or("legacy", LoopRunMode::label)
            )
        )
    )
}

fn write_record_forensics(
    out: &mut impl Write,
    entry: &TaskEntry,
    record: &LoopRunRecord,
) -> std::io::Result<()> {
    let run_record = record
        .run_id
        .as_deref()
        .and_then(|run_id| run_record_for(entry, run_id));
    write_check_section(out, record, run_record.as_ref())?;
    write_verify_section(out, run_record.as_ref())?;
    if let Some(spend) = record_spend_label(record) {
        writeln!(
            out,
            "{}",
            ui::paint(ui::palette::muted(), &format!("  cost: {spend}"))
        )?;
    }
    write_run_links(out, record, run_record.as_ref())
}

fn write_check_section(
    out: &mut impl Write,
    record: &LoopRunRecord,
    run_record: Option<&rimz::harness::run::RunRecord>,
) -> std::io::Result<()> {
    if let Some(check) = &record.check {
        let first_style = if check.timed_out || check.code != Some(0) {
            Some(ui::palette::alarm())
        } else {
            None
        };
        write_gutter_block(out, first_style, &check.output)?;
    }
    if let Some(error) = &record.error {
        write_detail_label(out, "error")?;
        write_gutter_block(out, None, error)?;
    }
    if let Some(last_message) = record
        .last_message
        .as_ref()
        .or_else(|| run_record.and_then(|record| record.last_message.as_ref()))
    {
        write_detail_label(out, "last message")?;
        write_gutter_block(out, None, last_message)?;
    }
    Ok(())
}

fn write_verify_section(
    out: &mut impl Write,
    run_record: Option<&rimz::harness::run::RunRecord>,
) -> std::io::Result<()> {
    if let Some(verify) = run_record
        .and_then(|record| record.verify.as_ref())
        .filter(|verify| !verify.passed)
    {
        let status = if verify.timed_out {
            "timeout".to_owned()
        } else {
            verify
                .code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".to_owned())
        };
        writeln!(
            out,
            "{}",
            ui::paint(
                ui::palette::muted(),
                &format!(
                    "  verify `{}` exited {status} (attempt {})",
                    verify.cmd, verify.attempts
                )
            )
        )?;
        write_gutter_block(out, Some(ui::palette::alarm()), &verify.output)?;
    }
    Ok(())
}

fn write_run_links(
    out: &mut impl Write,
    record: &LoopRunRecord,
    run_record: Option<&rimz::harness::run::RunRecord>,
) -> std::io::Result<()> {
    if let Some(run_id) = &record.run_id {
        writeln!(
            out,
            "{}",
            ui::paint(ui::palette::muted(), &format!("  run: {run_id}"))
        )?;
        if let Some(tail) = run_record
            .and_then(|record| record.failure_tail.as_deref())
            .filter(|tail| !tail.trim().is_empty())
        {
            write_detail_label(out, "output tail")?;
            write_gutter_block(out, None, tail)?;
        }
        if let Some(transcript) = run_record.and_then(|record| record.transcript_path.as_deref()) {
            writeln!(
                out,
                "{}",
                ui::paint(ui::palette::muted(), &format!("  transcript: {transcript}"))
            )?;
        }
    }
    Ok(())
}

fn record_spend_label(record: &LoopRunRecord) -> Option<String> {
    spend_segments(
        record
            .cost_usd
            .filter(|cost| cost.is_finite() && *cost >= 0.0),
        record.input_tokens,
        record.output_tokens,
    )
}

pub(super) fn spend_segments(
    cost_usd: Option<f64>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
) -> Option<String> {
    let mut segments = cost_usd
        .map(|cost| format!("${cost:.2}"))
        .into_iter()
        .collect::<Vec<_>>();
    if let Some(tokens) = token_segments(input_tokens, output_tokens) {
        segments.push(tokens);
    }
    (!segments.is_empty()).then(|| segments.join(" · "))
}

fn token_segments(input_tokens: Option<u64>, output_tokens: Option<u64>) -> Option<String> {
    let mut tokens = Vec::new();
    if let Some(input) = input_tokens {
        tokens.push(format!("↘ {}", ui::compact_count(input)));
    }
    if let Some(output) = output_tokens {
        tokens.push(format!("↗ {}", ui::compact_count(output)));
    }
    (!tokens.is_empty()).then(|| tokens.join(" "))
}

fn detail_exit_segment(record: &LoopRunRecord) -> Option<String> {
    if matches!(
        record.result,
        LoopRunResult::Failed
            | LoopRunResult::VerifyFailed
            | LoopRunResult::TimedOut
            | LoopRunResult::BudgetExceeded
            | LoopRunResult::Errored
    ) {
        return None;
    }
    let check = record.check.as_ref()?;
    if check.timed_out {
        return Some("timeout".to_owned());
    }
    Some(
        check
            .code
            .map(|code| format!("exit {code}"))
            .unwrap_or_else(|| "signal".to_owned()),
    )
}

fn write_detail_label(out: &mut impl Write, label: &str) -> std::io::Result<()> {
    writeln!(
        out,
        "{}",
        ui::paint(ui::palette::muted(), &format!("  {label}:"))
    )
}

pub(super) fn write_gutter_block(
    out: &mut impl Write,
    first_style: Option<anstyle::Style>,
    body: &str,
) -> std::io::Result<()> {
    let body = body.trim_end();
    if body.trim().is_empty() {
        return write_gutter_line(out, Some(ui::palette::faint()), "-");
    }
    for (idx, line) in body.lines().enumerate() {
        let style = if idx == 0 { first_style } else { None };
        write_gutter_line(out, style, line)?;
    }
    Ok(())
}

fn write_gutter_line(
    out: &mut impl Write,
    style: Option<anstyle::Style>,
    line: &str,
) -> std::io::Result<()> {
    write!(out, "  {}", ui::paint(ui::palette::faint(), "│ "))?;
    if let Some(style) = style {
        write!(out, "{}", ui::paint(style, line))?;
    } else {
        write!(out, "{line}")?;
    }
    writeln!(out)
}

fn run_record_for(entry: &TaskEntry, run_id: &str) -> Option<rimz::harness::run::RunRecord> {
    let run_id = rimz::RunId::parse(run_id).ok()?;
    let paths = StatePaths::under(
        WorkspaceId::from_project_root(&entry.resolved_root()),
        &state_home(),
    )
    .ok()?;
    rimz::harness::run::load(&paths, &run_id).ok()
}

#[cfg(test)]
mod tests;
