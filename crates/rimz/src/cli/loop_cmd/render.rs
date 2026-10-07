//! List, show, and log loop task state.

use super::*;

const NOTE_MAX: usize = 60;

pub(super) fn room_open(root: &Path) -> bool {
    runtime_for_root(root)
        .as_ref()
        .is_some_and(fresh_sidebar_present)
}

fn root_with_room(root: &Path, room_is_open: bool) -> String {
    format!(
        "{} · {}",
        ui::home_relative(root.to_string_lossy().as_ref()),
        room_label(room_is_open)
    )
}

pub(super) fn room_label(room_is_open: bool) -> &'static str {
    if room_is_open { "room open" } else { "no room" }
}

pub(super) fn room_style(room_is_open: bool) -> anstyle::Style {
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
        let attempts = entry
            .max_attempts
            .unwrap_or(rimz::harness::run::VERIFY_MAX_ATTEMPTS_DEFAULT);
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
        CheckOn::Any => "finishes",
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
        CheckOn::Any => "any outcome",
    }
}

fn source_description(source: TaskSource, entry: &TaskEntry) -> String {
    if source == TaskSource::Instance
        && let Some(team) = &entry.team
    {
        return format!("team {team}");
    }
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

pub(super) fn format_budget_cap(cap_usd: f64) -> String {
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
        source_description(source, entry),
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

struct ShowView {
    name: String,
    entry: TaskEntry,
    source: TaskSource,
    timing: schedule::TaskTiming,
    now_zoned: jiff::Zoned,
    records: Vec<LoopRunRecord>,
    launches: BTreeMap<PathBuf, schedule::launch_ledger::LaunchRecord>,
    in_flight: Option<InFlightRun>,
    condition: String,
    subscriptions: Vec<SubscriptionView>,
    room_is_open: bool,
    strike_count: u32,
    live_leader: bool,
    you: bool,
    config: std::sync::Arc<MachineConfig>,
    throttle: ShowThrottle,
    show_agent_runs: bool,
}

struct SubscriptionView {
    name: String,
    checkout: PathBuf,
    target: Option<TaskTarget>,
    signal: Option<String>,
    last: String,
    last_style: anstyle::Style,
}

enum ShowThrottle {
    Off,
    Compact(Option<String>),
    Full(Vec<(&'static str, String)>),
}

fn render_show(out: &mut impl Write, view: &ShowView, runs: usize) -> Result<()> {
    let now = view.now_zoned.timestamp();
    write_show_headline(
        out,
        &view.name,
        &view.timing,
        view.in_flight.as_ref(),
        now,
        view.entry.each_worktree,
    )?;
    write_verdict(
        out,
        &view.name,
        &view.records,
        view.in_flight.is_some(),
        now,
    )?;
    if !view.launches.is_empty() {
        writeln!(out, "\nLAUNCHES")?;
        let mut launches = view.launches.iter().collect::<Vec<_>>();
        launches.sort_by(|a, b| b.1.at.cmp(&a.1.at).then(a.0.cmp(b.0)));
        for (checkout, launch) in launches {
            writeln!(
                out,
                "  {}  @{}  {}",
                checkout.file_name().unwrap_or_default().to_string_lossy(),
                launch.leader,
                ui::rel_age(launch.at, now)
            )?;
        }
    }
    if view.entry.each_worktree {
        writeln!(
            out,
            "condition: evaluated per owned worktree · {} launched",
            view.launches.len()
        )?;
    } else {
        write!(out, "{}", view.condition)?;
    }
    if view.records.is_empty() {
        writeln!(
            out,
            "\nno runs recorded; try `rimz loop fire {}`",
            view.name
        )?;
    } else {
        write_last_run(
            out,
            &view.name,
            &view.entry,
            &view.records,
            now,
            ui::prose::Prose::for_stdout(),
        )?;
        if view.show_agent_runs {
            write_agent_runs(out, &view.records, now)?;
        }
        write_runs_table(out, &view.records, runs, now)?;
    }
    if !view.entry.each_worktree {
        write_subscriptions(out, &view.subscriptions)?;
    }
    writeln!(out)?;
    write_show_facts(out, view)?;
    if let ShowThrottle::Full(readings) = &view.throttle {
        write_throttle_readings(out, readings)?;
    }
    Ok(())
}

pub(super) fn show(args: ShowArgs, globals: &GlobalFlags) -> Result<()> {
    let Some(task) = load_task(&args.name, globals)? else {
        if args.json {
            bail!("no loop task named {}; see rimz loop list", args.name);
        }
        // A failed lookup falls through to `logs`, whose own lookup warns once.
        if let Ok(Some((root, in_flight))) = in_flight_without_row(&args.name, globals) {
            return show_in_flight(&args, &root, &in_flight);
        }
        return logs(
            LogsArgs {
                name: args.name,
                runs: args.runs,
                failed: false,
            },
            globals,
        );
    };
    let entry = task.entry();
    let root = entry.resolved_root();
    let runtime = runtime_for_root(&root);
    let stamps = runtime
        .as_ref()
        .map(rimz::harness::schedule::last_stamps)
        .unwrap_or_default();
    let now = Timestamp::now();
    let key = task.key(&args.name);
    let arming = arming::load().remove(&key);
    let now_zoned = now.to_zoned(MachineConfig::load_lenient().time_zone());
    let timing = if entry.each_worktree {
        schedule::TaskTiming::evaluate(
            task.trigger(),
            task.source(),
            stamps.get(&args.name).copied(),
            arming.as_ref(),
            &now_zoned,
        )
    } else {
        observe_task_timing(&args.name, &task, &stamps, arming.as_ref(), &now_zoned)
    };
    let mut records =
        run_log::task_records(&rimz::disk::paths::logs_dir(), &args.name, Some(&root));
    if let Some(meta) = &entry.wait_meta {
        records.retain(|record| record.at >= meta.armed_at);
    }
    let launches = if entry.stay {
        rimz::harness::schedule::launch_ledger::load(&StatePaths::for_project_root(&root)?)?
            .remove(&args.name)
            .unwrap_or_default()
    } else {
        BTreeMap::new()
    };
    let in_flight = displayed_in_flight(&args.name, in_flight_run(&args.name, &root));
    if args.json {
        let running = in_flight.as_ref().map(|in_flight| {
            serde_json::json!({
                "pid": in_flight.holder.map(|info| info.pid),
                "started_at": in_flight.holder.map(|info| info.started_at),
                "run_id": in_flight.run.as_ref().map(|run| &run.run_id),
            })
        });
        let (held, waiting) = in_flight.as_ref().map_or((None, Vec::new()), |in_flight| {
            let (own, others) = split_held(in_flight.holder, &in_flight.held);
            (own.map(held_json), others.map(held_json).collect())
        });
        writeln!(
            ui::out(),
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({ "task": args.name, "entry": entry, "launches": launches, "runs": records, "running": running, "held": held, "waiting": waiting })
            )?
        )?;
        return Ok(());
    }
    let mut condition = Vec::new();
    if !entry.each_worktree {
        condition::write_show(&mut condition, entry, &timing)?;
    }
    let subscriptions = load_subscriptions(&args.name, &root, &now_zoned)?;
    let store = if !launches.is_empty() || entry.wait.is_some() {
        let workspace = WorkspaceResolver::resolve(&root, Some(root.clone()))?;
        super::super::open_existing_store(&workspace)?
    } else {
        None
    };
    let snapshot = store
        .as_ref()
        .map(|store| store.snapshot_cached())
        .transpose()?;
    let live_leader = snapshot.as_ref().is_some_and(|snapshot| {
        launches.values().any(|launch| {
            snapshot
                .agents
                .iter()
                .any(|agent| agent.name.as_deref() == Some(launch.leader.as_str()))
        })
    });
    let you = store
        .as_ref()
        .and_then(|store| {
            let caller = super::super::send::resolve_caller(store).ok().flatten()?;
            let projection = store.runtime_projection(rimz::RuntimeScope::Audit).ok()?;
            rimz::harness::ancestry::resolve_launch_caller(&projection.agents, &caller)
                .ok()
                .filter(|agent| agent.ended_at.is_none())
                .map(|agent| (agent.kind.clone(), agent.agent_id.clone()))
        })
        .zip(entry.wait.as_ref())
        .is_some_and(|((kind, session), target)| kind == target.kind && session == target.session);
    let config = MachineConfig::load_lenient();
    let throttle = if entry.throttle == Some(rimz::config::ThrottleSwitch::Off) {
        ShowThrottle::Off
    } else if config.r#loop.throttle.has_limits()
        || in_flight.as_ref().is_some_and(|run| !run.held.is_empty())
    {
        ShowThrottle::Full(schedule::throttle::readings(&config, &args.name, entry))
    } else {
        ShowThrottle::Compact(schedule::throttle::compact_load())
    };
    let view = ShowView {
        strike_count: strikes::load().get(&key).copied().unwrap_or(0),
        room_is_open: room_open(&root),
        show_agent_runs: has_agent_runs_section(&task),
        name: args.name,
        entry: entry.clone(),
        source: task.source(),
        timing,
        now_zoned,
        records,
        launches,
        in_flight,
        condition: String::from_utf8(condition)?,
        subscriptions,
        live_leader,
        you,
        config,
        throttle,
    };
    render_show(&mut ui::out(), &view, args.runs)
}

fn load_subscriptions(name: &str, root: &Path, now: &jiff::Zoned) -> Result<Vec<SubscriptionView>> {
    let catalog = TaskCatalog::load_room(root)?;
    let subscriptions = catalog
        .visible()
        .iter()
        .filter(|(_, task)| task.entry().loop_task.as_deref() == Some(name))
        .collect::<Vec<_>>();
    if subscriptions.is_empty() {
        return Ok(Vec::new());
    }
    let stats = run_log::stats(&rimz::disk::paths::logs_dir(), now, Some(root));
    Ok(subscriptions
        .into_iter()
        .map(|(name, task)| {
            let entry = task.entry();
            let history = stats.get(name);
            SubscriptionView {
                name: name.clone(),
                checkout: entry.run_dir(),
                target: entry.wait.clone(),
                signal: entry.signal.clone(),
                last: list::TaskRow::subscription_last(task, history, now),
                last_style: history
                    .and_then(|stats| stats.acting.as_ref())
                    .map_or_else(ui::palette::muted, |acting| {
                        run_status(&acting.record).style
                    }),
            }
        })
        .collect())
}

fn write_subscriptions(out: &mut impl Write, subscriptions: &[SubscriptionView]) -> Result<()> {
    if subscriptions.is_empty() {
        return Ok(());
    }
    writeln!(out, "\nSUBSCRIPTIONS")?;
    let mut table = ui::Table::new(["NAME", "CHECKOUT", "TARGET", "SIGNAL", "LAST"]).indent(2);
    for subscription in subscriptions {
        let target = subscription.target.as_ref().map_or_else(
            || ui::cell("-").dash(),
            |target| ui::cell(&target.handle).fg(ui::palette::identity(target.kind.as_str())),
        );
        table.row([
            ui::cell(&subscription.name),
            ui::cell(display_path(&subscription.checkout)),
            target,
            ui::cell(subscription.signal.as_deref().unwrap_or("-")).dash(),
            ui::cell(&subscription.last).fg(subscription.last_style),
        ]);
    }
    table.render(out)?;
    Ok(())
}

/// What the start throttle reads on this machine now, beside any limit set.
fn write_throttle_readings(
    out: &mut impl Write,
    readings: &[(&'static str, String)],
) -> std::io::Result<()> {
    if readings.is_empty() {
        return Ok(());
    }
    writeln!(out, "\nTHROTTLE")?;
    let mut kv = ui::KeyVals::new().indent(2);
    for (label, text) in readings {
        kv.push(*label, ui::cell(text));
    }
    kv.render(out)
}

/// The answer under the headline: the verdict, any fires nothing has answered, and how to stop a running task.
fn write_verdict(
    out: &mut impl Write,
    name: &str,
    records: &[LoopRunRecord],
    is_running: bool,
    now: Timestamp,
) -> std::io::Result<()> {
    let verdict = verdict_line(records, now).map(|(verdict, style)| ui::paint(style, &verdict));
    let stale =
        stale_clause(records, is_running).map(|clause| ui::paint(ui::palette::warn(), &clause));
    match (verdict, stale) {
        (Some(verdict), Some(stale)) => writeln!(out, "  {verdict} · {stale}")?,
        (Some(line), None) | (None, Some(line)) => writeln!(out, "  {line}")?,
        (None, None) => {}
    }
    if is_running {
        writeln!(out, "  stop with `rimz loop stop {name}`")?;
    }
    Ok(())
}

fn is_overlap(record: &LoopRunRecord) -> bool {
    record.result == LoopRunResult::Overlapped
}

fn fires(count: usize) -> String {
    if count == 1 {
        "1 fire".to_owned()
    } else {
        format!("{count} fires")
    }
}

/// A fire a gate refused before anything ran: no run answers it, and it is no run itself.
fn is_refused_fire(record: &LoopRunRecord) -> bool {
    matches!(
        record.result,
        LoopRunResult::Overlapped
            | LoopRunResult::BudgetSkipped
            | LoopRunResult::AccountSkipped
            | LoopRunResult::ThrottleSkipped
            | LoopRunResult::SurplusSkipped
            | LoopRunResult::SignalSkipped
    )
}

/// A fire refused at the run lock is spent, and the run that refused it started
/// before it. So fires refused during or after the newest run, with no run
/// since, are fires nothing has answered. A run in flight with no refusal
/// after the newest run started after every one of them, and answers them.
/// Fires refused at the other gates ran nothing, so they neither bound the
/// newest run nor end a count of overlaps.
fn stale_clause(records: &[LoopRunRecord], has_active_run: bool) -> Option<String> {
    let newest_run = records.iter().rposition(|record| !is_refused_fire(record));
    let tail = &records[newest_run.map_or(0, |idx| idx + 1)..];
    let after = tail.iter().filter(|record| is_overlap(record)).count();
    if after > 0 && has_active_run {
        return Some(format!(
            "{} skipped while the active run holds the lock",
            fires(after)
        ));
    }
    if after > 0 {
        // The counted overlaps are "the last fires" only when no fire refused
        // at another gate sits among or after them.
        let trailing = tail
            .iter()
            .rev()
            .take_while(|record| is_overlap(record))
            .count();
        let skipped = match (trailing == after, after) {
            (true, 1) => "last fire".to_owned(),
            (true, _) => format!("last {after} fires"),
            (false, _) => fires(after),
        };
        return Some(format!("{skipped} skipped, nothing has run since"));
    }
    if has_active_run {
        return None;
    }
    let during = records[..newest_run?]
        .iter()
        .rev()
        .take_while(|record| is_refused_fire(record))
        .filter(|record| is_overlap(record))
        .count();
    (during > 0).then(|| {
        format!(
            "{} skipped during the last run, nothing has run since",
            fires(during)
        )
    })
}

fn write_last_run(
    out: &mut impl Write,
    name: &str,
    entry: &TaskEntry,
    records: &[LoopRunRecord],
    now: Timestamp,
    prose: ui::prose::Prose,
) -> std::io::Result<()> {
    let (detail_idx, failure_idx) = detail_indices(records);
    if let Some(idx) = detail_idx {
        writeln!(out)?;
        run_report::render_record_detail(
            out,
            entry,
            &records[idx],
            "LAST RUN",
            &format!("rimz loop logs {name} -n {}", records.len() - idx),
            now,
            prose,
        )?;
    }
    if let Some(failure) = failure_idx.and_then(|idx| records.get(idx)) {
        run_report::write_failure_pointer(out, name, failure, now)?;
    }
    Ok(())
}

/// `show` for a fired one-shot whose run consumed its row: the run is all there is.
fn show_in_flight(args: &ShowArgs, root: &Path, in_flight: &InFlightRun) -> Result<()> {
    let mut out = ui::out();
    writeln!(
        out,
        "{} — one-shot fired · {}",
        ui::paint(ui::palette::header(), &args.name),
        ui::paint(
            ui::palette::cool(),
            &running_text_full(in_flight, Timestamp::now())
        )
    )?;
    if let Some(run) = &in_flight.run {
        let agent = match &run.agent_name {
            Some(name) => format!("{} {name}", run.kind.as_str()),
            None => run.kind.as_str().to_owned(),
        };
        let mut kv = ui::KeyVals::new().indent(2);
        kv.push("agent", ui::cell(agent));
        kv.render(&mut out)?;
    }
    writeln!(out, "  stop with `rimz loop stop {}`", args.name)?;
    let records = run_log::task_records(&rimz::disk::paths::logs_dir(), &args.name, Some(root));
    let visible = records.iter().rev().take(args.runs).collect::<Vec<_>>();
    if !visible.is_empty() {
        writeln!(out)?;
        write_log_records(&mut out, None, &visible)?;
    }
    Ok(())
}

pub(super) fn logs(args: LogsArgs, globals: &GlobalFlags) -> Result<()> {
    let task = load_task(&args.name, globals)?;
    let entry = task.as_ref().map(|task| task.entry());
    let records = run_log::task_records(
        &rimz::disk::paths::logs_dir(),
        &args.name,
        project_root_for_globals(globals).as_deref(),
    );
    let lookup = match entry {
        Some(entry) => in_flight_run(&args.name, &entry.resolved_root()),
        None => in_flight_without_row(&args.name, globals)
            .map(|found| found.map(|(_, in_flight)| in_flight)),
    };
    let in_flight = displayed_in_flight(&args.name, lookup);
    if entry.is_none() && records.is_empty() && in_flight.is_none() {
        anyhow::bail!("no loop task named `{}`; see `rimz loop list`", args.name);
    }
    let in_flight = in_flight.filter(|_| !args.failed);
    let visible = records
        .iter()
        .filter(|record| !args.failed || record_is_failure(record))
        .rev()
        .take(args.runs)
        .collect::<Vec<_>>();
    let mut out = ui::out();
    if visible.is_empty() && in_flight.is_none() {
        if args.failed {
            writeln!(out, "no failed runs recorded")?;
        } else {
            writeln!(out, "no runs recorded; try `rimz loop fire {}`", args.name)?;
        }
        return Ok(());
    }
    write_log_records(&mut out, entry, &visible)?;
    if let Some(in_flight) = &in_flight {
        if !visible.is_empty() {
            writeln!(out)?;
        }
        writeln!(
            out,
            "{}",
            ui::paint(
                ui::palette::cool(),
                &running_text_full(in_flight, Timestamp::now())
            )
        )?;
    }
    Ok(())
}

/// History rows oldest first, from `visible` collected newest first.
fn write_log_records(
    out: &mut impl Write,
    entry: Option<&TaskEntry>,
    visible: &[&LoopRunRecord],
) -> Result<()> {
    let now = Timestamp::now();
    let prose = ui::prose::Prose::for_stdout();
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
        if let Some(exit) = run_report::detail_exit_segment(record) {
            write!(out, " · {exit}")?;
        }
        writeln!(out)?;
        run_report::write_record_forensics(out, entry, record, prose, run_report::Forensics::Full)?;
    }
    Ok(())
}

/// The in-flight lookup as `show` and `logs` use it: the active run is
/// enrichment there, so a failed lookup warns and costs only the running state.
fn displayed_in_flight<T>(name: &str, lookup: Result<Option<T>>) -> Option<T> {
    lookup.unwrap_or_else(|error| {
        warn_run_lock(name, &error);
        None
    })
}

fn warn_run_lock(name: &str, error: &anyhow::Error) {
    let _ = writeln!(
        ui::err(),
        "{} cannot read the loop run lock of `{name}`, so no active run is shown: {error:#}",
        ui::paint(ui::palette::warn().bold(), "warning:")
    );
}

/// The one wording of a held run lock, as `list` and `watch` print it. The
/// elapsed time needs the holder, which a held lock may lack.
pub(super) fn running_text(holder: Option<RunLockInfo>, now: Timestamp) -> String {
    match holder {
        Some(info) => {
            let elapsed = now.duration_since(info.started_at).as_secs().max(0) as u64;
            format!("▸ running {}", ui::age_label(elapsed))
        }
        None => "▸ running".to_owned(),
    }
}

/// A run waiting for its turn in the start throttle, in place of running.
fn held_run_text(held: &Held, now: Timestamp) -> String {
    let since = i64::try_from(held.since_ms).unwrap_or(i64::MAX);
    let elapsed = (now.as_millisecond().saturating_sub(since) / 1_000).max(0) as u64;
    format!(
        "held: {}, {}",
        held.reason.as_deref().unwrap_or("waiting for its turn"),
        ui::age_label(elapsed)
    )
}

/// The holder's own waiting ticket, matched by pid, and the task's other
/// waiting runs. A holder the lock does not name owns none.
pub(super) fn split_held(
    holder: Option<RunLockInfo>,
    held: &[Held],
) -> (Option<&Held>, impl Iterator<Item = &Held>) {
    let own = holder.and_then(|info| held.iter().find(|held| held.pid == info.pid));
    let others = held
        .iter()
        .filter(move |held| own.is_none_or(|own| !std::ptr::eq(own, *held)));
    (own, others)
}

/// The task's waiting runs beside the one in flight: their count and the
/// first one's reason.
pub(super) fn others_held_text<'a>(mut others: impl Iterator<Item = &'a Held>) -> Option<String> {
    let first = others.next()?;
    Some(format!(
        "{} held: {}",
        1 + others.count(),
        first.reason.as_deref().unwrap_or("waiting for its turn")
    ))
}

/// A run in flight as `list` and `watch` print it: held in place of running
/// only when the run is itself waiting for its turn, with the task's other
/// waiting runs beside it.
pub(super) fn in_flight_text(holder: Option<RunLockInfo>, held: &[Held], now: Timestamp) -> String {
    let (own, others) = split_held(holder, held);
    let state = match own {
        Some(own) => held_run_text(own, now),
        None => running_text(holder, now),
    };
    match others_held_text(others) {
        Some(others) => format!("{state} · {others}"),
        None => state,
    }
}

fn held_json(held: &Held) -> serde_json::Value {
    serde_json::json!({
        "pid": held.pid,
        "checkout": held.checkout,
        "since": Timestamp::from_millisecond(i64::try_from(held.since_ms).unwrap_or(i64::MAX)).ok(),
        "reason": held.reason,
        "position": held.position,
    })
}

/// The running state with the holder's pid and the run's id, for `show` and `logs`.
fn running_text_full(in_flight: &InFlightRun, now: Timestamp) -> String {
    let pid = in_flight
        .holder
        .map(|info| format!(" · pid {}", info.pid))
        .unwrap_or_default();
    let run = in_flight
        .run
        .as_ref()
        .map(|run| format!(" · run {}", run.run_id))
        .unwrap_or_default();
    let (own, others) = split_held(in_flight.holder, &in_flight.held);
    let state = match own {
        Some(own) => held_run_text(own, now),
        None => running_text(in_flight.holder, now),
    };
    let others = others_held_text(others)
        .map(|others| format!(" · {others}"))
        .unwrap_or_default();
    format!("{state}{pid}{run}{others}")
}

fn write_show_headline(
    out: &mut impl Write,
    name: &str,
    timing: &schedule::TaskTiming,
    in_flight: Option<&InFlightRun>,
    now: Timestamp,
    each_worktree: bool,
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
    if each_worktree {
        write!(out, " · each worktree")?;
    }
    if let Some(in_flight) = in_flight {
        return writeln!(
            out,
            " · {}",
            ui::paint(ui::palette::cool(), &running_text_full(in_flight, now))
        );
    }
    match timing.state() {
        schedule::TaskTimingState::Blocked(state) => write!(
            out,
            " · next {}",
            ui::paint(ui::status::trust(state), "blocked · trust")
        )?,
        schedule::TaskTimingState::Disabled(DisabledReason::NotEnabledHere) => write!(
            out,
            " · disabled — enable here with `rimz loop enable {}`",
            name
        )?,
        schedule::TaskTimingState::Disabled(DisabledReason::Manual) => {
            write!(out, " · disabled — enable with `rimz loop enable {name}`")?
        }
        schedule::TaskTimingState::Disabled(DisabledReason::Strikes(strikes)) => write!(
            out,
            " · disabled after {strikes} strikes — enable with `rimz loop enable {}`",
            name
        )?,
        schedule::TaskTimingState::Paused(until) => {
            write!(out, " · paused, resumes {}", pause_until_text(until, now))?
        }
        schedule::TaskTimingState::Upcoming(next) | schedule::TaskTimingState::Due(next) => {
            write!(out, " · next {}", ui::rel_until(next, now))?;
        }
        schedule::TaskTimingState::Listening { .. } => write!(out, " · listening")?,
        schedule::TaskTimingState::Watching { .. } => write!(out, " · watching")?,
        state @ (schedule::TaskTimingState::Waiting { .. }
        | schedule::TaskTimingState::Holding { .. }
        | schedule::TaskTimingState::Fired)
            if !each_worktree =>
        {
            write!(out, " · {}", state.condition_label().unwrap_or_default())?
        }
        schedule::TaskTimingState::Invalid
        | schedule::TaskTimingState::Unarmed
        | schedule::TaskTimingState::NoOccurrence
        | schedule::TaskTimingState::Waiting { .. }
        | schedule::TaskTimingState::Holding { .. }
        | schedule::TaskTimingState::Fired => {}
    }
    writeln!(out)?;
    Ok(())
}

fn write_show_facts(out: &mut impl Write, view: &ShowView) -> std::io::Result<()> {
    let entry = &view.entry;
    let source = view.source;
    let root = entry.resolved_root();
    let blocked_state = source.blocked_state();
    let timeout = entry.timeout.clone().or_else(|| {
        entry.agent.as_ref().map(|_| {
            format!(
                "{} (default)",
                view.config
                    .r#loop
                    .default_timeout
                    .clone()
                    .unwrap_or_else(|| { SCHEDULED_RUN_DEFAULT_TIMEOUT_LABEL.to_owned() })
            )
        })
    });
    let mut kv = ui::KeyVals::new().indent(2);
    let shape = schedule::TaskShape::compile(&view.name, entry);
    let action = shape.action().ok();
    let mut action_text = action.map_or_else(
        || "<invalid>".to_owned(),
        |action| list::action(action.kind(), action.subject(), Some(entry), view.you),
    );
    let check = check_summary(entry, action);
    if matches!(action, Some(TaskAction::CheckOnly))
        && let Some(check) = &check
    {
        action_text.push_str(&format!(" · {check}"));
    }
    if entry.stay {
        action_text.push_str(" · stays");
    }
    if entry.takeover {
        action_text.push_str(" · takes over the checkout");
    }
    kv.push("action", ui::cell(action_text));
    if !matches!(action, Some(TaskAction::CheckOnly))
        && let Some(check) = check
    {
        kv.push("check", ui::cell(check));
    }
    if !entry.subscribe.is_empty() {
        let signals = entry
            .subscribe
            .iter()
            .map(|binding| binding.signal.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let armed = view.subscriptions.len();
        let no_leader = if armed == 0 && !view.live_leader {
            ", no live leader"
        } else {
            ""
        };
        kv.push(
            "wakes",
            ui::cell(format!("leader on {signals} · {armed} armed{no_leader}")),
        );
    }
    if let Some(label) = entry.label.as_deref() {
        kv.push("label", ui::cell(label));
    }
    if let Some(verify) = entry.verify.as_deref() {
        kv.push(
            "verify",
            ui::cell(format!(
                "{verify} (up to {} attempts)",
                entry
                    .max_attempts
                    .unwrap_or(rimz::harness::run::VERIFY_MAX_ATTEMPTS_DEFAULT)
            )),
        );
    }
    kv.push("root", ui::cell(root_with_room(&root, view.room_is_open)));
    if let Some(dir) = entry.dir.as_deref() {
        kv.push("dir", ui::cell(display_path(dir)));
    }
    kv.push("source", ui::cell(source_detail(source, entry)));
    if !entry.stay
        && let Some(timeout) = timeout
    {
        kv.push("timeout", ui::cell(timeout));
    }
    if let Some(state) = blocked_state {
        kv.push(
            "will not fire",
            ui::cell(blocked_notice(state)).fg(ui::status::trust(state)),
        );
    }
    match &view.throttle {
        ShowThrottle::Off => kv.push("throttle", ui::cell("off (starts without taking a turn)")),
        ShowThrottle::Compact(load) => {
            kv.push("throttle", ui::cell("no limits set"));
            if let Some(load) = load {
                kv.push("load", ui::cell(load));
            }
        }
        ShowThrottle::Full(_) if entry.throttle == Some(rimz::config::ThrottleSwitch::On) => {
            kv.push("throttle", ui::cell("on"))
        }
        ShowThrottle::Full(_) => {}
    }
    if let Some(budget) = budget_label(entry) {
        kv.push("budget", ui::cell(budget));
    }
    if let Some(surplus) = surplus_label(entry) {
        kv.push("surplus", ui::cell(surplus));
    }
    if let Some(spend) = spend_label(entry, &view.records, &view.now_zoned, !view.show_agent_runs) {
        kv.push("spend", ui::cell(spend));
    }
    if view.timing.arm_state() == ArmState::Live
        && view.strike_count > 0
        && let Some(max) = strikes::threshold(entry)
    {
        kv.push(
            "strikes",
            ui::cell(format!("{}/{max}", view.strike_count)).fg(ui::palette::muted()),
        );
    }
    kv.render(out)
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

    let visible = agent_runs.iter().rev().take(5).collect::<Vec<_>>();
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
                    .watch
                    .as_ref()
                    .map(|verdict| verdict.elapsed_ms())
                    .or(record.duration_ms)
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
    let rows = collapsed_run_rows(records);
    let visible_rows = rows.iter().rev().take(limit).collect::<Vec<_>>();
    let shown = visible_rows
        .iter()
        .map(|row| row.count + row.skipped)
        .sum::<usize>();
    writeln!(out)?;
    writeln!(
        out,
        "{}",
        ui::paint(
            ui::palette::header(),
            &format!("RECENT RUNS (newest first · {shown} of {})", records.len())
        )
    )?;
    let show_mode = visible_rows
        .iter()
        .any(|row| row.key.mode != visible_rows[0].key.mode);
    let show_cost = visible_rows
        .iter()
        .any(|row| valid_cost(row.latest).is_some());
    let show_tokens = visible_rows
        .iter()
        .any(|row| row.latest.input_tokens.is_some() || row.latest.output_tokens.is_some());
    let show_note = visible_rows.iter().any(|row| row.note().is_some());
    let mut headers = vec!["WHEN"];
    if show_mode {
        headers.push("MODE");
    }
    headers.extend(["STATUS", "TOOK"]);
    let cost_column = headers.len();
    if show_cost {
        headers.push("COST");
    }
    if show_tokens {
        headers.push("TOKENS");
    }
    if show_note {
        headers.push("NOTE");
    }
    let mut table = ui::Table::new(headers).indent(2);
    if show_cost {
        table = table.right(&[cost_column]);
    }
    for row in visible_rows {
        let record = row.latest;
        let mut cells = vec![ui::cell(ui::rel_age(record.at, now))];
        if show_mode {
            cells.push(ui::cell(row.key.mode.map_or("-", LoopRunMode::label)).dash());
        }
        cells.push(run_status_cell(record, row.count));
        cells.push(
            ui::cell(
                record
                    .watch
                    .as_ref()
                    .map(|verdict| verdict.elapsed_ms())
                    .or(record.duration_ms)
                    .map(format_duration_ms)
                    .unwrap_or_else(|| "-".to_owned()),
            )
            .dash(),
        );
        if show_cost {
            cells.push(cost_cell(record));
        }
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
            cells.push(ui::cell(row.note().as_deref().unwrap_or("-")).dash());
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
    /// Overlapped fires folded into this row: refused while its runs held the lock.
    skipped: usize,
}

impl CollapsedRunRow<'_> {
    fn note(&self) -> Option<String> {
        let skipped = (self.skipped > 0)
            .then(|| format!("{} skipped, run already active", fires(self.skipped)));
        match (&self.key.note, skipped) {
            (Some(note), Some(skipped)) => Some(format!("{note} · {skipped}")),
            (note, skipped) => note.clone().or(skipped),
        }
    }
}

/// Table rows in record order. An overlap takes no row once a later record
/// exists: its row precedes the row of the run that refused it, so it folds
/// into the next record as a count, and neighbours it used to split collapse.
/// Overlaps with no later record are a run still in flight, or one that
/// crashed, and keep one row.
fn collapsed_run_rows(records: &[LoopRunRecord]) -> Vec<CollapsedRunRow<'_>> {
    let mut rows = Vec::<CollapsedRunRow<'_>>::new();
    let mut overlaps = 0;
    for record in records {
        if is_overlap(record) {
            overlaps += 1;
            continue;
        }
        let key = RunRowKey::new(record);
        if let Some(row) = rows.last_mut().filter(|row| row.key == key) {
            row.count += 1;
            row.skipped += overlaps;
            if record.at >= row.latest.at {
                row.latest = record;
            }
        } else {
            rows.push(CollapsedRunRow {
                key,
                latest: record,
                count: 1,
                skipped: overlaps,
            });
        }
        overlaps = 0;
    }
    if let Some(latest) = records.last().filter(|_| overlaps > 0) {
        rows.push(CollapsedRunRow {
            key: RunRowKey::new(latest),
            latest,
            count: overlaps,
            skipped: 0,
        });
    }
    rows
}

fn run_status_cell(record: &LoopRunRecord, count: usize) -> ui::Cell {
    let status = run_status(record);
    let mut label = format!("{} {}", status.glyph, status.label);
    if count > 1 {
        label.push_str(&format!(" ×{count}"));
    }
    ui::cell(label).fg(status.style)
}

pub(super) struct RunStatusDisplay {
    pub(super) glyph: &'static str,
    pub(super) label: String,
    pub(super) style: anstyle::Style,
}

pub(super) fn run_status(record: &LoopRunRecord) -> RunStatusDisplay {
    let label = match (&record.watch, record.result) {
        (
            Some(verdict),
            LoopRunResult::Failed
            | LoopRunResult::VerifyFailed
            | LoopRunResult::TimedOut
            | LoopRunResult::BudgetExceeded
            | LoopRunResult::Errored
            | LoopRunResult::StartFailed,
        ) => {
            format!("{} ({})", record.result.label(), verdict.label())
        }
        (Some(_), LoopRunResult::CheckSkipped) => record.result.label().to_owned(),
        (_, result) => match result {
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
        },
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
    record.polarity() == Some(run_log::RunPolarity::Good)
}

fn verdict_line(records: &[LoopRunRecord], now: Timestamp) -> Option<(String, anstyle::Style)> {
    let acting = run_log::acting_run(records)?;
    let healthy = acting.polarity == run_log::RunPolarity::Good;
    let newest = &acting.record;
    let status = run_status(newest);
    let mut line = format!(
        "{} {} · last run {}, {}",
        status.glyph,
        if healthy { "healthy" } else { "failing" },
        ui::rel_age(newest.at, now),
        status.label
    );
    if let Some(took) = run_duration_label(newest) {
        line.push_str(&format!(" in {took}"));
    }
    let since = acting.since.map(|at| {
        format!(
            " since a {} {}",
            if healthy { "failure" } else { "good run" },
            ui::rel_age(at, now)
        )
    });
    match (acting.streak, since) {
        (1, None) => {}
        (1, Some(since)) => line.push_str(&format!(" · first{since}")),
        (count, since) => {
            line.push_str(&format!(" · {count} in a row{}", since.unwrap_or_default()))
        }
    }
    Some((
        line,
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

#[derive(Clone, Copy)]
pub(super) struct ResultMark {
    pub(super) glyph: &'static str,
    pub(super) style: anstyle::Style,
}

pub(super) fn loop_result_mark(result: LoopRunResult) -> ResultMark {
    let (glyph, style) = match result {
        LoopRunResult::Completed | LoopRunResult::Delivered | LoopRunResult::Launched => {
            ("✓", ui::palette::good())
        }
        LoopRunResult::Failed
        | LoopRunResult::VerifyFailed
        | LoopRunResult::TimedOut
        | LoopRunResult::BudgetExceeded
        | LoopRunResult::Errored
        | LoopRunResult::StartFailed => ("✗", ui::palette::alarm()),
        LoopRunResult::Expired
        | LoopRunResult::Canceled
        | LoopRunResult::TargetGone
        | LoopRunResult::Overlapped
        | LoopRunResult::TakeoverBlocked
        | LoopRunResult::BudgetSkipped
        | LoopRunResult::AccountSkipped
        | LoopRunResult::ThrottleSkipped => ("○", ui::palette::warn()),
        LoopRunResult::CheckSkipped
        | LoopRunResult::SignalSkipped
        | LoopRunResult::SurplusSkipped => ("○", ui::palette::muted()),
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

/// A run's own duration. A watched command's verdict label already states its
/// elapsed time, so such a record has none to add.
pub(super) fn run_duration_label(record: &LoopRunRecord) -> Option<String> {
    record
        .duration_ms
        .filter(|_| record.watch.is_none())
        .map(format_duration_ms)
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

pub(super) fn record_exit(record: &LoopRunRecord) -> Option<String> {
    if let Some(verdict) = &record.watch {
        return Some(verdict.label());
    }
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
        .and_then(|_| record.result.spawn_exit_code())
        .map(|code| code.to_string())
}

fn record_note(record: &LoopRunRecord) -> Option<String> {
    let note = record
        .error
        .as_deref()
        .map(first_line)
        .or_else(|| check_failure_line(record))
        .or_else(|| record.last_message.as_deref().map(first_line))
        .or_else(|| record.target.as_deref().map(first_line))
        .map(|note| truncate_note(note, NOTE_MAX))
        .filter(|note| !note.is_empty());
    let held = record
        .throttle_wait_ms
        .map(|waited| format!("held {}", format_duration_ms(waited)));
    let note = match (held, note) {
        (Some(held), Some(note)) => Some(format!("{held} · {note}")),
        (held, note) => held.or(note),
    };
    match (&record.checkout, note) {
        (Some(checkout), Some(note)) if !note.is_empty() => {
            Some(format!("{} {note}", checkout.display()))
        }
        (Some(checkout), _) => Some(checkout.display().to_string()),
        (None, note) => note,
    }
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
        || record.signal.is_some()
        || record.message_id.is_some()
        || record.run_id.is_some()
        || record
            .cost_usd
            .is_some_and(|cost| cost.is_finite() && cost >= 0.0)
        || record.input_tokens.is_some()
        || record.output_tokens.is_some()
}

pub(super) fn record_is_failure(record: &LoopRunRecord) -> bool {
    record.polarity() == Some(run_log::RunPolarity::Failure)
}

fn is_agent_run(record: &LoopRunRecord) -> bool {
    record.run_id.is_some()
        || matches!(
            record.result,
            LoopRunResult::Delivered | LoopRunResult::TargetGone
        )
}

/// The record LAST RUN details, and the failure to point at when the loop is
/// failing and LAST RUN is some other record. An overlap is never the detail:
/// it is a refused fire, and the run before it is the last thing that ran.
fn detail_indices(records: &[LoopRunRecord]) -> (Option<usize>, Option<usize>) {
    if records
        .iter()
        .rev()
        .find(|record| !is_overlap(record))
        .is_some_and(|record| record.result == LoopRunResult::Launched)
    {
        return (None, None);
    }
    let detail_idx = records
        .iter()
        .rposition(|record| !is_overlap(record) && record_has_detail(record));
    let failure_idx = records
        .iter()
        .rposition(|record| record_is_failure(record) || record_is_good(record))
        .filter(|&idx| record_is_failure(&records[idx]) && Some(idx) != detail_idx);
    (detail_idx, failure_idx)
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

#[cfg(test)]
mod tests;
