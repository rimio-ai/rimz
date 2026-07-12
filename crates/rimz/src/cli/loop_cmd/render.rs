//! List and show loop tasks plus recorded run details.

use super::*;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const NOTE_MAX: usize = 60;
const WATCH_NARROW: usize = 44;
const WATCH_WIDE: usize = 68;

struct ListRowContext<'a> {
    pauses: &'a BTreeMap<String, PauseEntry>,
    stats: &'a BTreeMap<String, run_log::LoopRunStats>,
    stamps: &'a BTreeMap<String, Timestamp>,
    now_zoned: &'a jiff::Zoned,
    now: Timestamp,
}

// ---- list -------------------------------------------------------------------

pub(super) fn list(globals: &GlobalFlags) -> Result<()> {
    let tasks = load_all_tasks(globals)?;
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
    let mut groups: BTreeMap<PathBuf, Vec<(&String, &TaskEntry, TaskSource)>> = BTreeMap::new();
    for (name, (entry, source)) in &tasks {
        let root = entry.resolved_root();
        groups.entry(root).or_default().push((name, entry, *source));
    }
    for (idx, (root, entries)) in groups.into_iter().enumerate() {
        let runtime = runtime_for_root(&root);
        let room_is_open = runtime.as_ref().is_some_and(fresh_sidebar_present);
        let stamps = runtime
            .as_ref()
            .map(rimz::harness::schedule::last_stamps)
            .unwrap_or_default();
        if idx > 0 {
            writeln!(out)?;
        }
        write_root_heading(&mut out, &root, room_is_open)?;
        let mut table = ui::Table::new([
            "NAME", "TASK", "SOURCE", "SCHEDULE", "LAST", "STATUS", "COST", "NEXT",
        ])
        .right(&[6])
        .indent(2);
        let context = ListRowContext {
            pauses: &pause_entries,
            stats: &stats,
            stamps: &stamps,
            now_zoned: &now_zoned,
            now,
        };
        for (name, entry, source) in entries {
            let blocked_state = blocked_project_state(source);
            blocked_count += usize::from(blocked_state.is_some());
            table.row(task_row(name, entry, source, blocked_state, &context));
        }
        table.render(&mut out)?;
    }
    if blocked_count > 0 {
        writeln!(out)?;
        write_blocked_footer(&mut out, blocked_count)?;
    }
    Ok(())
}

fn task_row(
    name: &str,
    entry: &TaskEntry,
    source: TaskSource,
    blocked_state: Option<TrustState>,
    context: &ListRowContext<'_>,
) -> Vec<ui::Cell> {
    let parsed = schedule::parse_schedule(name, entry);
    let when = match &parsed {
        Ok(schedule) => schedule.describe(),
        Err(err) => format!("invalid: {err}"),
    };
    let next = next_cell(
        name,
        entry,
        blocked_state,
        &parsed,
        context.pauses.get(name),
        context,
    );
    let (last, status) = context
        .stats
        .get(name)
        .map(|stats| last_run_cells(stats, context.now))
        .unwrap_or_else(|| (ui::cell("-").dash(), ui::cell("-").dash()));
    let cost = list_cost_label(
        entry,
        context
            .stats
            .get(name)
            .map_or(0.0, |stats| stats.spend_today_usd),
    )
    .map(ui::cell)
    .unwrap_or_else(|| ui::cell("-").dash());
    vec![
        ui::cell(name).fg(ui::palette::ACCENT),
        ui::cell(task_subject(entry)),
        source_cell(source),
        ui::cell(when),
        last,
        status,
        cost,
        next,
    ]
}

fn next_cell(
    name: &str,
    entry: &TaskEntry,
    blocked_state: Option<TrustState>,
    parsed: &std::result::Result<schedule::ParsedSchedule, schedule::ScheduleErr>,
    pause: Option<&PauseEntry>,
    context: &ListRowContext<'_>,
) -> ui::Cell {
    if let Some(state) = blocked_state {
        return blocked_next_cell(state);
    }
    match pause.filter(|entry| pauses::is_active(entry, context.now)) {
        Some(PauseEntry {
            until: None,
            strikes: Some(strikes),
        }) => ui::cell(format!("paused · {strikes} strikes")).fg(ui::palette::MUTED),
        Some(PauseEntry {
            until: None,
            strikes: None,
        }) => ui::cell("paused").fg(ui::palette::MUTED),
        Some(PauseEntry {
            until: Some(until), ..
        }) => ui::cell(format!("paused · {}", ui::rel_until(*until, context.now)))
            .fg(ui::palette::MUTED),
        None => parsed
            .as_ref()
            .ok()
            .and_then(|parsed| {
                next_fire_text(
                    name,
                    &parsed.schedule,
                    context.stamps,
                    pause,
                    context.now_zoned,
                    context.now,
                    window_reset_for(entry),
                )
            })
            .map(ui::cell)
            .unwrap_or_else(|| ui::cell("-").dash()),
    }
}

fn blocked_project_state(source: TaskSource) -> Option<TrustState> {
    match source {
        TaskSource::Project { state } if state != TrustState::Trusted => Some(state),
        _ => None,
    }
}

fn blocked_next_cell(state: TrustState) -> ui::Cell {
    ui::cell("blocked · trust").fg(ui::status::trust(state))
}

fn source_cell(source: TaskSource) -> ui::Cell {
    let cell = ui::cell(source_label(source));
    match blocked_project_state(source) {
        Some(state) => cell.fg(ui::status::trust(state)),
        None => cell,
    }
}

fn write_blocked_footer(out: &mut impl Write, count: usize) -> std::io::Result<()> {
    writeln!(
        out,
        "{}",
        ui::paint(
            ui::palette::WARN,
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
    let _input = TerminalModeGuard::enable(MouseCapture::Off)?;
    loop {
        repaint_watch(globals)?;
        match event::poll(Duration::from_secs(1)) {
            Ok(true) => match event::read() {
                Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Char('q') => return Ok(()),
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

fn repaint_watch(globals: &GlobalFlags) -> Result<()> {
    use ratatui::crossterm::{
        cursor::MoveTo,
        execute,
        terminal::{Clear, ClearType},
    };

    let mut frame = Vec::new();
    render_watch_frame(&mut frame, globals)?;
    let mut stdout = std::io::stdout();
    execute!(stdout, MoveTo(0, 0))?;
    rimz::tui::write_crlf(&mut stdout, &frame)?;
    execute!(stdout, Clear(ClearType::FromCursorDown))?;
    stdout.flush()?;
    Ok(())
}

fn render_watch_frame(out: &mut impl Write, globals: &GlobalFlags) -> Result<()> {
    let tasks = load_all_tasks(globals)?;
    let now = Timestamp::now();
    let pause_entries = pauses::load();
    let now_zoned = now.to_zoned(MachineConfig::load_lenient().time_zone());
    let stats = run_log::stats(&state_home(), &now_zoned);
    let mut entries_by_root: BTreeMap<PathBuf, Vec<(&String, &TaskEntry, TaskSource)>> =
        BTreeMap::new();
    for (name, (entry, source)) in &tasks {
        entries_by_root
            .entry(entry.resolved_root())
            .or_default()
            .push((name, entry, *source));
    }

    let mut groups = Vec::new();
    for (root, entries) in entries_by_root {
        let runtime = runtime_for_root(&root);
        let room_is_open = runtime.as_ref().is_some_and(fresh_sidebar_present);
        let stamps = runtime
            .as_ref()
            .map(rimz::harness::schedule::last_stamps)
            .unwrap_or_default();
        let context = ListRowContext {
            pauses: &pause_entries,
            stats: &stats,
            stamps: &stamps,
            now_zoned: &now_zoned,
            now,
        };
        let mut rows = Vec::new();
        for (name, entry, source) in entries {
            rows.push(watch_row_model(name, entry, source, &context));
        }
        groups.push(WatchGroup {
            root,
            room_is_open,
            rows,
        });
    }
    let (cols, rows) = rimz::mux::detect_terminal_size().unwrap_or((80, 24));
    render_dashboard(out, &groups, usize::from(cols), usize::from(rows), now)?;
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

struct WatchGroup {
    root: PathBuf,
    room_is_open: bool,
    rows: Vec<WatchRow>,
}

fn watch_row_model(
    name: &str,
    entry: &TaskEntry,
    source: TaskSource,
    context: &ListRowContext<'_>,
) -> WatchRow {
    let parsed = schedule::parse_schedule(name, entry);
    let pause = context
        .pauses
        .get(name)
        .filter(|pause| pauses::is_active(pause, context.now));
    let blocked = blocked_project_state(source).is_some();
    let next_ts = parsed.as_ref().ok().and_then(|parsed| {
        next_fire_timestamp(
            name,
            &parsed.schedule,
            context.stamps,
            context.pauses.get(name),
            context.now_zoned,
            context.now,
            window_reset_for(entry),
        )
    });
    let running = matches!(acquire_run_lock(name, entry), Ok(None));
    let state = if running {
        RowState::Running
    } else if blocked {
        RowState::Blocked
    } else if pause.is_some() {
        RowState::Paused
    } else if next_ts.is_some_and(|next| next <= context.now) {
        RowState::Due
    } else if let Some(next) = next_ts {
        RowState::Upcoming(next)
    } else {
        RowState::NeverRun
    };
    let (glyph, glyph_style, failed, last_text, status_text) = context.stats.get(name).map_or(
        (
            "○",
            ui::palette::FAINT,
            false,
            "—".to_owned(),
            "never run".to_owned(),
        ),
        |stats| {
            let status = run_status(&stats.last);
            (
                status.glyph,
                status.style,
                status.style == ui::palette::ALARM,
                ui::rel_age(stats.last.at, context.now),
                status.label,
            )
        },
    );
    let next_text = match state {
        RowState::Running => "running now".to_owned(),
        RowState::Paused => pause.map_or_else(
            || "paused".to_owned(),
            |pause| match pause.until {
                Some(until) => format!("paused · {}", ui::rel_until(until, context.now)),
                None => pause.strikes.map_or_else(
                    || "paused".to_owned(),
                    |strikes| format!("paused · {strikes} strikes"),
                ),
            },
        ),
        RowState::Blocked => "blocked · trust".to_owned(),
        RowState::Due => "due".to_owned(),
        RowState::Upcoming(next) => ui::rel_until(next, context.now),
        RowState::NeverRun => "—".to_owned(),
    };
    WatchRow {
        name: name.to_owned(),
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

fn next_fire_timestamp(
    name: &str,
    schedule: &schedule::Schedule,
    stamps: &BTreeMap<String, Timestamp>,
    pause: Option<&PauseEntry>,
    now_zoned: &jiff::Zoned,
    now: Timestamp,
    window_reset: Option<Timestamp>,
) -> Option<Timestamp> {
    let stamp = pauses::effective_last_fire(*stamps.get(name)?, pause, now);
    schedule.next_after(stamp, now_zoned, window_reset)
}

fn render_dashboard(
    out: &mut impl Write,
    groups: &[WatchGroup],
    cols: usize,
    rows: usize,
    now: Timestamp,
) -> std::io::Result<()> {
    write_watch_band(out, groups, cols, now)?;
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
            clip_watch_text("no loop tasks; add one with `rimz loop add …`", cols)
        )?;
        return Ok(());
    }

    for (group_index, group) in groups.iter().enumerate() {
        if group.rows.is_empty() {
            continue;
        }
        if remaining_rows == 1 {
            write_more(out, remaining_tasks, cols)?;
            break;
        }
        write_dashboard_heading(out, &group.root, group.room_is_open, cols)?;
        remaining_rows -= 1;

        let mut ranked = group.rows.iter().collect::<Vec<_>>();
        ranked.sort_by_key(|row| watch_rank(row));
        let later_lines = groups[group_index + 1..]
            .iter()
            .filter(|group| !group.rows.is_empty())
            .map(|group| group.rows.len() + 1)
            .sum::<usize>();
        let all_fit = ranked.len() + later_lines <= remaining_rows;
        let visible = if all_fit {
            ranked.len()
        } else {
            ranked.len().min(remaining_rows.saturating_sub(1))
        };
        if visible > 0 {
            render_watch_rows(out, &ranked[..visible], cols)?;
            remaining_rows -= visible;
            remaining_tasks -= visible;
        }
        if !all_fit && visible < ranked.len() {
            write_more(out, remaining_tasks, cols)?;
            break;
        }
    }
    Ok(())
}

fn watch_rank(row: &WatchRow) -> (u8, Option<Timestamp>, &str) {
    let rank = if row.state == RowState::Running {
        0
    } else if row.failed {
        1
    } else {
        match row.state {
            RowState::Due => 2,
            RowState::Upcoming(_) => 3,
            RowState::Paused | RowState::Blocked => 4,
            RowState::NeverRun => 5,
            RowState::Running => 0,
        }
    };
    let next = match row.state {
        RowState::Upcoming(next) => Some(next),
        _ => None,
    };
    (rank, next, &row.name)
}

fn render_watch_rows(out: &mut impl Write, rows: &[&WatchRow], cols: usize) -> std::io::Result<()> {
    let headers = if cols < WATCH_NARROW {
        vec!["", "", ""]
    } else if cols < WATCH_WIDE {
        vec!["", "", "", ""]
    } else {
        vec!["", "", "", "", ""]
    };
    let mut table = ui::Table::new(headers)
        .headerless()
        .indent(2)
        .clip_last(cols);
    for row in rows {
        let next_style = match row.state {
            RowState::Running => ui::palette::ACCENT,
            RowState::Paused | RowState::Blocked => ui::palette::MUTED,
            RowState::NeverRun => ui::palette::FAINT,
            RowState::Due => ui::palette::WARN,
            RowState::Upcoming(_) => anstyle::Style::new(),
        };
        let name = clip_watch_text(&row.name, (cols / 4).max(1));
        let next = clip_watch_text(&row.next_text, (cols / 4).max(1));
        let mut cells = vec![
            ui::cell(row.glyph).fg(row.glyph_style),
            ui::cell(name).fg(ui::palette::ACCENT),
            ui::cell(next).fg(next_style),
        ];
        if cols >= WATCH_NARROW {
            cells.push(ui::cell(clip_watch_text(&row.last_text, 12)));
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
) -> std::io::Result<()> {
    let all = groups
        .iter()
        .flat_map(|group| &group.rows)
        .collect::<Vec<_>>();
    let total = all.len();
    let running = all
        .iter()
        .filter(|row| row.state == RowState::Running)
        .count();
    let failed = all
        .iter()
        .filter(|row| row.state != RowState::Running && row.failed)
        .count();
    let paused = all
        .iter()
        .filter(|row| !row.failed && matches!(row.state, RowState::Paused | RowState::Blocked))
        .count();
    let ok = total.saturating_sub(running + failed + paused);
    let next = all
        .iter()
        .filter(|row| !matches!(row.state, RowState::Paused | RowState::Blocked))
        .filter_map(|row| row.next_ts.map(|next| (*row, next)))
        .min_by_key(|(_, next)| *next);

    let prefix = format!("loop · {total} tasks");
    if prefix.width() > cols {
        writeln!(
            out,
            "{}",
            ui::paint(ui::palette::ACCENT.bold(), &clip_watch_text(&prefix, cols))
        )?;
        return Ok(());
    }
    let mut segments = vec![(prefix, ui::palette::ACCENT.bold())];
    let mut candidates = Vec::new();
    for (count, label, style) in [
        (running, "▸", ui::palette::ACCENT),
        (failed, "✗", ui::palette::ALARM),
        (paused, "○", ui::palette::MUTED),
        (ok, "●", ui::palette::GOOD),
    ] {
        if count > 0 {
            let noun = match label {
                "▸" => "running",
                "✗" => "failed",
                "○" => "paused",
                _ => "ok",
            };
            candidates.push((format!("{label} {count} {noun}"), style));
        }
    }
    if let Some((row, next)) = next {
        candidates.push((
            format!("next: {} {}", row.name, ui::rel_until(next, now)),
            ui::palette::MUTED,
        ));
    }
    candidates.push(("q quit".to_owned(), ui::palette::FAINT));
    for (text, style) in candidates {
        if !push_band_segment(&mut segments, text, style, cols) {
            break;
        }
    }

    for (index, (text, style)) in segments.iter().enumerate() {
        if index > 0 {
            write!(out, "{}", ui::paint(ui::palette::FAINT, " · "))?;
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
            ui::palette::ACCENT.bold(),
            &clip_watch_text(&root, root_budget)
        )
    )?;
    if root_budget > 0 && suffix.width() <= cols {
        write!(out, " · {}", ui::paint(room_style(room_is_open), room))?;
    }
    writeln!(out)
}

fn write_more(out: &mut impl Write, count: usize, cols: usize) -> std::io::Result<()> {
    let text = clip_watch_text(&format!("+{count} more"), cols);
    writeln!(out, "{}", ui::paint(ui::palette::FAINT, &text))
}

fn clip_watch_text(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return "…".to_owned();
    }
    let mut clipped = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let char_width = ch.width().unwrap_or(0);
        if used + char_width > width - 1 {
            break;
        }
        clipped.push(ch);
        used += char_width;
    }
    clipped.push('…');
    clipped
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
            ui::palette::ACCENT.bold(),
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
        ui::palette::GOOD
    } else {
        ui::palette::MUTED
    }
}

fn schedule_style<T, E>(parsed: std::result::Result<&T, &E>) -> anstyle::Style {
    if parsed.is_ok() {
        anstyle::Style::new()
    } else {
        ui::palette::ALARM
    }
}

fn check_summary(entry: &TaskEntry) -> Option<String> {
    let check = entry.check.as_ref()?;
    if entry.agent.is_some() || entry.wake.is_some() {
        Some(format!(
            "{check} ({} {} on {})",
            action_third_person_verb(entry),
            task_subject(entry),
            check_on_label(entry.on.unwrap_or_default())
        ))
    } else {
        Some(check.clone())
    }
}

pub(super) fn task_run_rule(entry: &TaskEntry) -> String {
    let action = if entry.agent.is_some() || entry.wake.is_some() {
        Some(format!(
            "{} {}",
            action_base_verb(entry),
            task_subject(entry)
        ))
    } else {
        None
    };
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

pub(super) fn action_progressive_verb(entry: &TaskEntry) -> &'static str {
    if entry.agent.is_some() {
        "starting"
    } else {
        "waking"
    }
}

pub(super) fn check_skip_decision(entry: &TaskEntry) -> String {
    let subject = task_subject(entry);
    let untouched = if entry.agent.is_some() {
        "not started"
    } else {
        "not woken"
    };
    let condition = match entry.on.unwrap_or_default() {
        CheckOn::Fail => "fails",
        CheckOn::Success => "passes",
    };
    format!("{subject} {untouched}; fires when the check {condition}")
}

fn action_base_verb(entry: &TaskEntry) -> &'static str {
    if entry.agent.is_some() {
        "start"
    } else {
        "wake"
    }
}

fn action_third_person_verb(entry: &TaskEntry) -> &'static str {
    if entry.agent.is_some() {
        "starts"
    } else {
        "wakes"
    }
}

fn check_on_label(on: CheckOn) -> &'static str {
    match on {
        CheckOn::Fail => "fail",
        CheckOn::Success => "success",
    }
}

fn source_label(source: TaskSource) -> String {
    match source {
        TaskSource::Project { state } if state != TrustState::Trusted => {
            format!("project · {}", state.as_str())
        }
        _ => source.label().to_owned(),
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

fn spend_label(entry: &TaskEntry, records: &[LoopRunRecord], now: &jiff::Zoned) -> Option<String> {
    let spend_today_usd = run_log::spend_on_local_day(records, now);
    let summary = run_log::cost_summary(records);
    let mut segments = Vec::new();
    if entry.budget_per_day.is_some() || run_log::has_cost_on_local_day(records, now) {
        let mut today = format!("${spend_today_usd:.2} today");
        if let Some(raw) = entry.budget_per_day.as_deref() {
            today.push_str(" of ");
            today.push_str(&budget_amount(raw));
        }
        segments.push(today);
    }
    if let Some(last_usd) = summary.last_usd {
        segments.push(format!("${last_usd:.2} last"));
    }
    if summary.costed_runs >= 2
        && let Some(avg_usd) = summary.avg_usd
    {
        segments.push(format!("ø ${avg_usd:.2} over {} runs", summary.costed_runs));
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
    match source {
        TaskSource::Config => MachineConfig::loop_path(),
        TaskSource::Project { .. } => project_config_path(&entry.root),
        TaskSource::Instance => instances::path(&state_home()),
    }
}

fn display_path(path: &Path) -> String {
    ui::home_relative(path.to_string_lossy().as_ref())
}

fn runtime_for_root(root: &Path) -> Option<RuntimePaths> {
    RuntimePaths::for_workspace(WorkspaceId::from_project_root(root)).ok()
}

fn next_fire_text(
    name: &str,
    schedule: &schedule::Schedule,
    stamps: &BTreeMap<String, Timestamp>,
    pause: Option<&PauseEntry>,
    now_zoned: &jiff::Zoned,
    now: Timestamp,
    window_reset: Option<Timestamp>,
) -> Option<String> {
    let next = next_fire_timestamp(name, schedule, stamps, pause, now_zoned, now, window_reset)?;
    Some(ui::rel_until(next, now))
}

pub(super) fn task_next_fire_text(
    name: &str,
    entry: &TaskEntry,
    pause: Option<&PauseEntry>,
    now: Timestamp,
) -> Option<String> {
    let root = entry.resolved_root();
    let runtime = runtime_for_root(&root)?;
    let stamps = rimz::harness::schedule::last_stamps(&runtime);
    let parsed = schedule::parse_schedule(name, entry).ok()?;
    let now_zoned = now.to_zoned(MachineConfig::load_lenient().time_zone());
    next_fire_text(
        name,
        &parsed.schedule,
        &stamps,
        pause,
        &now_zoned,
        now,
        window_reset_for(entry),
    )
}

fn window_reset_for(entry: &TaskEntry) -> Option<Timestamp> {
    if entry.every.as_deref() != Some("reset") {
        return None;
    }
    let kind = entry
        .agent
        .as_deref()
        .and_then(rimz::harness::spec::ping_kind)?;
    window_reset_at(entry, kind).ok().flatten()
}

pub(super) fn show(args: ShowArgs, globals: &GlobalFlags) -> Result<()> {
    let (entry, source) = load_task(&args.name, globals)?.ok_or_else(|| {
        anyhow::anyhow!("no loop task named `{}`; see `rimz loop list`", args.name)
    })?;
    let root = entry.resolved_root();
    let runtime = runtime_for_root(&root);
    let stamps = runtime
        .as_ref()
        .map(rimz::harness::schedule::last_stamps)
        .unwrap_or_default();
    let blocked_state = blocked_project_state(source);
    let parsed = schedule::parse_schedule(&args.name, &entry);
    let now = Timestamp::now();
    let pause = pauses::load().remove(&args.name);
    let active_pause = pause.as_ref().filter(|entry| pauses::is_active(entry, now));
    let now_zoned = now.to_zoned(MachineConfig::load_lenient().time_zone());
    let window_reset = window_reset_for(&entry);
    let next = if blocked_state.is_none() {
        parsed.as_ref().ok().and_then(|parsed| {
            next_fire_text(
                &args.name,
                &parsed.schedule,
                &stamps,
                pause.as_ref(),
                &now_zoned,
                now,
                window_reset,
            )
        })
    } else {
        None
    };
    let records = run_log::task_records(&state_home(), &args.name);

    let mut out = ui::out();
    write_show_headline(
        &mut out,
        &args.name,
        &parsed,
        blocked_state,
        active_pause,
        next.as_deref(),
        now,
    )?;
    write_show_facts(
        &mut out,
        &args.name,
        &entry,
        source,
        &records,
        &now_zoned,
        active_pause.is_some(),
    )?;

    if records.is_empty() {
        writeln!(out)?;
        writeln!(out, "no runs recorded; try `rimz loop fire {}`", args.name)?;
        return Ok(());
    }

    write_runs_table(&mut out, &records, args.runs, now)?;
    let (detail_idx, failure_idx) = detail_indices(&records);
    if let Some(detail) = detail_idx.and_then(|idx| records.get(idx)) {
        writeln!(out)?;
        render_record_detail(&mut out, &entry, detail, "LAST RUN", now)?;
    }
    if let Some(failure) = failure_idx.and_then(|idx| records.get(idx)) {
        writeln!(out)?;
        render_record_detail(&mut out, &entry, failure, "LAST FAILURE", now)?;
    }
    Ok(())
}

fn write_show_headline(
    out: &mut impl Write,
    name: &str,
    parsed: &std::result::Result<schedule::ParsedSchedule, schedule::ScheduleErr>,
    blocked_state: Option<TrustState>,
    active_pause: Option<&PauseEntry>,
    next: Option<&str>,
    now: Timestamp,
) -> std::io::Result<()> {
    let schedule_text = match parsed {
        Ok(parsed) => parsed.describe(),
        Err(err) => format!("invalid: {err}"),
    };
    write!(
        out,
        "{} — {}",
        ui::paint(ui::palette::ACCENT.bold(), name),
        ui::paint(schedule_style(parsed.as_ref()), &schedule_text)
    )?;
    match blocked_state {
        Some(state) => write!(
            out,
            " · next {}",
            ui::paint(ui::status::trust(state), "blocked · trust")
        )?,
        None => match active_pause {
            Some(PauseEntry {
                until: None,
                strikes: Some(strikes),
            }) => write!(
                out,
                " · paused after {strikes} strikes — resume with `rimz loop resume {}`",
                name
            )?,
            Some(PauseEntry {
                until: None,
                strikes: None,
            }) => write!(out, " · paused")?,
            Some(PauseEntry {
                until: Some(until), ..
            }) => write!(out, " · paused, resumes {}", pause_until_text(*until, now))?,
            None => {
                if let Some(next) = next {
                    write!(out, " · next {next}")?;
                }
            }
        },
    }
    writeln!(out)?;
    if matches!(
        active_pause,
        Some(PauseEntry {
            until: None,
            strikes: None,
        })
    ) {
        writeln!(out, "  resume with `rimz loop resume {name}`")?;
    }
    Ok(())
}

fn write_show_facts(
    out: &mut impl Write,
    name: &str,
    entry: &TaskEntry,
    source: TaskSource,
    records: &[LoopRunRecord],
    now_zoned: &jiff::Zoned,
    is_paused: bool,
) -> std::io::Result<()> {
    let root = entry.resolved_root();
    let room_is_open = room_open(&root);
    let blocked_state = blocked_project_state(source);
    let strike_count = strikes::load().get(name).copied().unwrap_or(0);
    let mut kv = ui::KeyVals::new().indent(2);
    kv.push("task", ui::cell(task_subject(entry)));
    if let Some(check) = check_summary(entry) {
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
    if let Some(state) = blocked_state {
        kv.push(
            "will not fire",
            ui::cell(blocked_notice(state)).fg(ui::status::trust(state)),
        );
    }
    if let Some(budget) = budget_label(entry) {
        kv.push("budget", ui::cell(budget));
    }
    if let Some(spend) = spend_label(entry, records, now_zoned) {
        kv.push("spend", ui::cell(spend));
    }
    if !is_paused
        && strike_count > 0
        && let Some(max) = strikes::threshold(entry)
    {
        kv.push(
            "strikes",
            ui::cell(format!("{strike_count}/{max}")).fg(ui::palette::MUTED),
        );
    }
    kv.render(out)
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
            ui::palette::HEADER,
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
            ui::cell(
                record
                    .cost_usd
                    .filter(|cost| cost.is_finite() && *cost >= 0.0)
                    .map(|cost| format!("${cost:.2}"))
                    .unwrap_or_else(|| "-".to_owned()),
            )
            .dash(),
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
        LoopRunResult::Completed => "completed".to_owned(),
        LoopRunResult::Delivered => "delivered".to_owned(),
        LoopRunResult::Failed => {
            let mut label = "failed".to_owned();
            if let Some(exit) = failure_exit_label(record) {
                label.push_str(" (");
                label.push_str(&exit);
                label.push(')');
            }
            label
        }
        LoopRunResult::VerifyFailed => "verify failed".to_owned(),
        LoopRunResult::TimedOut => "timed out".to_owned(),
        LoopRunResult::BudgetExceeded => "budget exceeded".to_owned(),
        LoopRunResult::BudgetSkipped => "budget skipped".to_owned(),
        LoopRunResult::Errored => "error".to_owned(),
        LoopRunResult::SkippedWindow => "skipped".to_owned(),
        LoopRunResult::Expired => "expired".to_owned(),
        LoopRunResult::Canceled => "canceled".to_owned(),
        LoopRunResult::TargetGone => "target gone".to_owned(),
        LoopRunResult::Overlapped => "overlapped".to_owned(),
        LoopRunResult::CheckSkipped => check_skipped_label(record).to_owned(),
    };
    let (glyph, style) = match record.result {
        LoopRunResult::CheckSkipped => check_skip_display(record.check.as_ref()),
        result => (loop_result_glyph(result), loop_result_style(result)),
    };
    RunStatusDisplay {
        glyph,
        label,
        style,
    }
}

pub(super) fn check_skip_display(check: Option<&CheckRecord>) -> (&'static str, anstyle::Style) {
    match check {
        Some(check) if check.timed_out => ("○", ui::palette::WARN),
        Some(check) if check.code == Some(0) => ("✓", ui::palette::GOOD),
        _ => ("○", ui::palette::MUTED),
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
            | LoopRunResult::Errored
    )
}

pub(super) fn loop_result_style(result: LoopRunResult) -> anstyle::Style {
    match result {
        LoopRunResult::Completed | LoopRunResult::Delivered => ui::palette::GOOD,
        LoopRunResult::Failed
        | LoopRunResult::VerifyFailed
        | LoopRunResult::TimedOut
        | LoopRunResult::BudgetExceeded
        | LoopRunResult::Errored => ui::palette::ALARM,
        LoopRunResult::Expired
        | LoopRunResult::Canceled
        | LoopRunResult::TargetGone
        | LoopRunResult::Overlapped
        | LoopRunResult::BudgetSkipped => ui::palette::WARN,
        LoopRunResult::SkippedWindow | LoopRunResult::CheckSkipped => ui::palette::MUTED,
    }
}

pub(super) fn loop_result_glyph(result: LoopRunResult) -> &'static str {
    match result {
        LoopRunResult::Completed | LoopRunResult::Delivered => "✓",
        LoopRunResult::Failed
        | LoopRunResult::VerifyFailed
        | LoopRunResult::TimedOut
        | LoopRunResult::BudgetExceeded
        | LoopRunResult::Errored => "✗",
        LoopRunResult::Expired
        | LoopRunResult::Canceled
        | LoopRunResult::TargetGone
        | LoopRunResult::Overlapped
        | LoopRunResult::SkippedWindow
        | LoopRunResult::BudgetSkipped
        | LoopRunResult::CheckSkipped => "○",
    }
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
    let note = record
        .error
        .as_deref()
        .map(first_line)
        .or_else(|| check_failure_line(record))
        .or_else(|| record.last_message.as_deref().map(first_line))
        .or_else(|| record.target.as_deref().map(first_line))?;
    Some(truncate_note(note, NOTE_MAX))
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
            ui::paint(ui::palette::MUTED, &format!("  cost: {spend}"))
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
            Some(ui::palette::ALARM)
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
                ui::palette::MUTED,
                &format!(
                    "  verify `{}` exited {status} (attempt {})",
                    verify.cmd, verify.attempts
                )
            )
        )?;
        write_gutter_block(out, Some(ui::palette::ALARM), &verify.output)?;
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
            ui::paint(ui::palette::MUTED, &format!("  run: {run_id}"))
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
                ui::paint(ui::palette::MUTED, &format!("  transcript: {transcript}"))
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
        ui::paint(ui::palette::MUTED, &format!("  {label}:"))
    )
}

pub(super) fn write_gutter_block(
    out: &mut impl Write,
    first_style: Option<anstyle::Style>,
    body: &str,
) -> std::io::Result<()> {
    let body = body.trim_end();
    if body.trim().is_empty() {
        return write_gutter_line(out, Some(ui::palette::FAINT), "-");
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
    write!(out, "  {}", ui::paint(ui::palette::FAINT, "│ "))?;
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
mod tests {
    use super::*;

    fn dashboard_row(name: &str, state: RowState, failed: bool) -> WatchRow {
        WatchRow {
            name: name.to_owned(),
            glyph: if failed { "✗" } else { "✓" },
            glyph_style: if failed {
                ui::palette::ALARM
            } else {
                ui::palette::GOOD
            },
            state,
            failed,
            next_ts: match state {
                RowState::Due => Some(Timestamp::from_second(90).unwrap()),
                RowState::Upcoming(next) => Some(next),
                _ => None,
            },
            next_text: name.repeat(8),
            last_text: "LAST-COLUMN".to_owned(),
            status_text: "STATUS-COLUMN".to_owned(),
        }
    }

    fn dashboard(group: &WatchGroup, cols: usize, rows: usize) -> String {
        let mut out = anstream::StripStream::new(Vec::new());
        render_dashboard(
            &mut out,
            std::slice::from_ref(group),
            cols,
            rows,
            Timestamp::from_second(100).unwrap(),
        )
        .unwrap();
        String::from_utf8(out.into_inner()).unwrap()
    }

    fn record(second: i64, result: LoopRunResult) -> LoopRunRecord {
        LoopRunRecord {
            task: "wake".to_owned(),
            at: Timestamp::from_second(second).expect("timestamp"),
            result,
            mode: None,
            duration_ms: None,
            error: None,
            check: None,
            run_id: None,
            transcript_path: None,
            last_message: None,
            target: None,
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
        }
    }

    #[test]
    fn watch_dashboard_adapts_band_columns_rank_height_and_width() {
        let group = WatchGroup {
            root: PathBuf::from("/a/very/long/project/root/that/must/be/clipped"),
            room_is_open: true,
            rows: vec![
                dashboard_row("never", RowState::NeverRun, false),
                dashboard_row("paused", RowState::Paused, false),
                dashboard_row(
                    "later",
                    RowState::Upcoming(Timestamp::from_second(300).unwrap()),
                    false,
                ),
                dashboard_row(
                    "sooner",
                    RowState::Upcoming(Timestamp::from_second(200).unwrap()),
                    false,
                ),
                dashboard_row("due", RowState::Due, false),
                dashboard_row(
                    "failed",
                    RowState::Upcoming(Timestamp::from_second(150).unwrap()),
                    true,
                ),
                dashboard_row("running", RowState::Running, false),
            ],
        };
        let wide = dashboard(&group, 100, 20);
        assert!(
            wide.starts_with("loop · 7 tasks · ▸ 1 running · ✗ 1 failed"),
            "{wide}"
        );
        let body = wide.lines().skip(3).collect::<Vec<_>>().join("\n");
        let positions = [
            "running", "failed", "due", "sooner", "later", "paused", "never",
        ]
        .map(|name| body.find(name).unwrap());
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{wide}");
        assert!(!dashboard(&group, WATCH_NARROW - 1, 20).contains("LAST-COLUMN"));
        assert!(dashboard(&group, WATCH_NARROW, 20).contains("LAST-COLUMN"));
        assert!(dashboard(&group, WATCH_WIDE, 20).contains("STATUS-COLUMN"));
        let short = dashboard(&group, 30, 6);
        assert_eq!(short.lines().count(), 6, "{short}");
        assert!(short.contains("+5 more"), "{short}");
        assert!(short.lines().all(|line| line.width() <= 30), "{short}");
        assert_eq!(dashboard(&group, 14, 2).lines().count(), 1);
    }

    #[test]
    fn task_rules_and_check_rows_use_action_specific_verbs() {
        let spawn = TaskEntry {
            agent: Some("codex".to_owned()),
            check: Some("cargo test".to_owned()),
            on: Some(CheckOn::Fail),
            ..TaskEntry::default()
        };
        assert_eq!(task_run_rule(&spawn), "check, then start codex on fail");
        assert_eq!(
            check_summary(&spawn).as_deref(),
            Some("cargo test (starts codex on fail)")
        );

        let wake = TaskEntry {
            wake: Some(TaskTarget {
                kind: "claude".to_owned(),
                session: "sess-planner".to_owned(),
                handle: "@planner".to_owned(),
            }),
            check: Some("cargo test".to_owned()),
            on: Some(CheckOn::Success),
            ..TaskEntry::default()
        };
        assert_eq!(task_run_rule(&wake), "check, then wake @planner on success");
        assert_eq!(
            check_summary(&wake).as_deref(),
            Some("cargo test (wakes @planner on success)")
        );

        let check = TaskEntry {
            check: Some("cargo test".to_owned()),
            ..TaskEntry::default()
        };
        assert_eq!(task_run_rule(&check), "check");

        let spawn = TaskEntry {
            agent: Some("claude".to_owned()),
            verify: Some("cargo xtask gate".to_owned()),
            max_attempts: Some(4),
            ..TaskEntry::default()
        };
        assert_eq!(
            task_run_rule(&spawn),
            "start claude, verify `cargo xtask gate` (up to 4 attempts)"
        );

        let mut wake_only = wake;
        wake_only.check = None;
        assert_eq!(task_run_rule(&wake_only), "wake @planner");
    }

    #[test]
    fn budget_and_cost_labels_cover_capped_plain_and_empty_spend() {
        let entry = TaskEntry {
            budget: Some("$5.00".to_owned()),
            budget_per_day: Some("$20.00".to_owned()),
            ..TaskEntry::default()
        };
        assert_eq!(
            budget_label(&entry).as_deref(),
            Some("$5 per run · $20 per day")
        );
        assert_eq!(list_cost_label(&entry, 3.2).as_deref(), Some("$3.20/$20"));

        let uncapped = TaskEntry::default();
        assert_eq!(list_cost_label(&uncapped, 0.85).as_deref(), Some("$0.85"));
        assert_eq!(list_cost_label(&uncapped, 0.0), None);
    }

    #[test]
    fn spend_label_renders_today_last_and_cost_window() {
        let now = "2026-06-02T12:00:00Z[UTC]".parse::<jiff::Zoned>().unwrap();
        let entry = TaskEntry {
            budget_per_day: Some("20".to_owned()),
            ..TaskEntry::default()
        };
        let mut records = Vec::new();
        for (second, cost) in [(1, 0.28), (2, 0.42)] {
            let mut run = record(second, LoopRunResult::Completed);
            run.at = now.timestamp() + jiff::SignedDuration::from_secs(second);
            run.cost_usd = Some(cost);
            records.push(run);
        }

        assert_eq!(
            spend_label(&entry, &records, &now).as_deref(),
            Some("$0.70 today of $20 · $0.42 last · ø $0.35 over 2 runs")
        );
        assert_eq!(spend_label(&TaskEntry::default(), &[], &now), None);
    }

    #[test]
    fn check_failure_line_uses_last_non_empty_failed_check_line() {
        let mut failed = record(10, LoopRunResult::Failed);
        failed.check = Some(CheckRecord {
            code: Some(127),
            timed_out: false,
            output: "ignored\n\nmissing command\n".to_owned(),
        });
        assert_eq!(check_failure_line(&failed), Some("missing command"));

        let mut passed = record(11, LoopRunResult::Completed);
        passed.check = Some(CheckRecord {
            code: Some(0),
            timed_out: false,
            output: "ok".to_owned(),
        });
        assert_eq!(check_failure_line(&passed), None);
    }

    #[test]
    fn record_note_prefers_error_then_failed_check_output() {
        let mut failed = record(10, LoopRunResult::Failed);
        failed.check = Some(CheckRecord {
            code: Some(1),
            timed_out: false,
            output: "first\ncheck failed".to_owned(),
        });
        failed.last_message = Some("last message".to_owned());
        assert_eq!(record_note(&failed), Some("check failed".to_owned()));

        failed.error = Some("outer error\nignored detail".to_owned());
        assert_eq!(record_note(&failed), Some("outer error".to_owned()));
    }

    #[test]
    fn run_status_names_check_skipped_outcomes() {
        let mut skipped = record(10, LoopRunResult::CheckSkipped);
        skipped.check = Some(CheckRecord {
            code: Some(0),
            timed_out: false,
            output: "ok".to_owned(),
        });
        let status = run_status(&skipped);
        assert_eq!(status.glyph, "✓");
        assert_eq!(status.label, "check passed");
        assert_eq!(status.style, ui::palette::GOOD);

        skipped.check = Some(CheckRecord {
            code: Some(1),
            timed_out: false,
            output: "not yet".to_owned(),
        });
        let status = run_status(&skipped);
        assert_eq!(status.glyph, "○");
        assert_eq!(status.label, "check failed");
        assert_eq!(status.style, ui::palette::MUTED);

        skipped.check = Some(CheckRecord {
            code: None,
            timed_out: true,
            output: "too slow".to_owned(),
        });
        let status = run_status(&skipped);
        assert_eq!(status.glyph, "○");
        assert_eq!(status.label, "check timed out");
        assert_eq!(status.style, ui::palette::WARN);

        assert_eq!(
            loop_result_style(LoopRunResult::SkippedWindow),
            ui::palette::MUTED
        );
    }

    #[test]
    fn source_detail_names_definition_path() {
        let entry = TaskEntry {
            root: PathBuf::from("/repo"),
            ..TaskEntry::default()
        };

        assert_eq!(
            source_detail(TaskSource::Config, &entry),
            format!(
                "machine — {}",
                ui::home_relative(MachineConfig::loop_path().to_string_lossy().as_ref())
            )
        );
        assert_eq!(
            source_detail(
                TaskSource::Project {
                    state: TrustState::Untrusted
                },
                &entry,
            ),
            "project · untrusted — /repo/.rimz/config.toml"
        );
        assert_eq!(
            source_detail(TaskSource::Instance, &entry),
            format!(
                "state — {}",
                ui::home_relative(instances::path(&state_home()).to_string_lossy().as_ref())
            )
        );
    }

    #[test]
    fn blocked_project_rendering_names_the_gate_and_fix() {
        let mut table = ui::Table::new(["NEXT"]);
        table.row([blocked_next_cell(TrustState::Stale)]);
        let mut out = Vec::new();
        table.render(&mut out).unwrap();
        write_blocked_footer(&mut out, 2).unwrap();

        let out = anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string();
        assert!(out.contains("blocked · trust"), "{out}");
        assert!(
            out.contains(
                "2 task(s) blocked by project trust — review with `rimz trust`, approve with `rimz trust grant`"
            ),
            "{out}"
        );
        assert_eq!(
            blocked_notice(TrustState::Untrusted),
            "project trust is untrusted — review with `rimz trust`, approve with `rimz trust grant`"
        );
    }

    #[test]
    fn run_status_merges_failed_check_exit() {
        let mut failed = record(10, LoopRunResult::Failed);
        failed.check = Some(CheckRecord {
            code: Some(127),
            timed_out: false,
            output: "missing".to_owned(),
        });

        let status = run_status(&failed);

        assert_eq!(status.glyph, "✗");
        assert_eq!(status.label, "failed (exit 127)");
    }

    #[test]
    fn runs_table_shows_tokens_only_when_present() {
        let now = Timestamp::from_second(30).expect("timestamp");
        let without_tokens = record(10, LoopRunResult::Completed);
        let mut out = Vec::new();
        write_runs_table(&mut out, &[without_tokens], 10, now).unwrap();
        let out = anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string();
        assert!(!out.contains("TOKENS"), "{out}");

        let mut with_tokens = record(20, LoopRunResult::Completed);
        with_tokens.input_tokens = Some(14_000);
        with_tokens.output_tokens = Some(269);
        let without_tokens = record(10, LoopRunResult::Failed);
        let mut out = Vec::new();
        write_runs_table(&mut out, &[without_tokens, with_tokens], 10, now).unwrap();
        let out = anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string();
        assert!(out.contains("TOKENS"), "{out}");
        assert!(out.contains("↘ 14k ↗ 269"), "{out}");
        assert!(
            out.lines()
                .any(|line| line.contains("✗ failed") && line.ends_with('-')),
            "{out}"
        );
    }

    #[test]
    fn collapsed_run_rows_merge_adjacent_matching_render_columns() {
        let mut first = record(10, LoopRunResult::Failed);
        first.mode = Some(LoopRunMode::Scheduled);
        first.duration_ms = Some(10);
        first.check = Some(CheckRecord {
            code: Some(1),
            timed_out: false,
            output: "boom".to_owned(),
        });
        let mut second = first.clone();
        second.at = Timestamp::from_second(20).expect("timestamp");
        second.duration_ms = Some(20);
        let mut third = second.clone();
        third.at = Timestamp::from_second(30).expect("timestamp");
        third.check = Some(CheckRecord {
            code: Some(1),
            timed_out: false,
            output: "different".to_owned(),
        });
        let records = vec![first, second, third];

        let rows = collapsed_run_rows(&records);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].count, 2);
        assert_eq!(
            rows[0].latest.at,
            Timestamp::from_second(20).expect("timestamp")
        );
        assert_eq!(rows[0].latest.duration_ms, Some(20));
        assert_eq!(rows[0].key.note.as_deref(), Some("boom"));
        assert_eq!(rows[1].count, 1);
        assert_eq!(rows[1].key.note.as_deref(), Some("different"));
    }

    #[test]
    fn detail_indices_include_prior_failure_when_latest_detail_shadows_it() {
        let mut error = record(10, LoopRunResult::Errored);
        error.error = Some("reading prompt-file\nmissing".to_owned());
        let mut failed = record(20, LoopRunResult::Failed);
        failed.run_id = Some("run_0123456789abcdef01234567".to_owned());
        let records = vec![error, failed];

        assert_eq!(detail_indices(&records), (Some(1), Some(0)));
    }

    #[test]
    fn render_record_detail_titles_status_age_and_mode() {
        let mut detail = record(20, LoopRunResult::Errored);
        detail.mode = Some(LoopRunMode::Manual);
        detail.error = Some("outer error\ninner detail".to_owned());
        detail.cost_usd = Some(0.42);
        detail.input_tokens = Some(12_000);
        detail.output_tokens = Some(3_400);
        let entry = TaskEntry {
            root: PathBuf::from("/tmp/rimz-run"),
            ..TaskEntry::default()
        };
        let mut out = Vec::new();

        render_record_detail(
            &mut out,
            &entry,
            &detail,
            "LAST FAILURE",
            Timestamp::from_second(30).expect("timestamp"),
        )
        .unwrap();

        let raw = String::from_utf8(out).unwrap();
        assert!(raw.contains(&ui::paint(ui::palette::MUTED, "  error:")));
        let out = anstream::adapter::strip_str(&raw).to_string();
        assert!(out.contains("LAST FAILURE — ✗ error · "));
        assert!(out.contains(" · manual"));
        assert!(out.contains("  error:\n  │ outer error\n  │ inner detail"));
        assert!(out.contains("  cost: $0.42 · ↘ 12k ↗ 3k"));
    }

    #[test]
    fn render_record_detail_marks_failed_check_output() {
        let mut detail = record(20, LoopRunResult::Failed);
        detail.check = Some(CheckRecord {
            code: Some(2),
            timed_out: false,
            output: "first line\nsecond line".to_owned(),
        });
        let entry = TaskEntry {
            root: PathBuf::from("/tmp/rimz-run"),
            ..TaskEntry::default()
        };
        let mut out = Vec::new();

        render_record_detail(
            &mut out,
            &entry,
            &detail,
            "LAST FAILURE",
            Timestamp::from_second(30).expect("timestamp"),
        )
        .unwrap();

        let raw = String::from_utf8(out).unwrap();
        assert!(raw.contains(&ui::paint(ui::palette::ALARM, "first line")));
        let out = anstream::adapter::strip_str(&raw).to_string();
        assert!(out.contains("LAST FAILURE — ✗ failed (exit 2)"));
        assert!(out.contains("  │ first line\n  │ second line"));
    }
}
