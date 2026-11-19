use std::collections::BTreeMap;
use std::io::IsTerminal;

use jiff::Timestamp;
use rimz::store::workflow::{RunState, Wait};
use unicode_width::UnicodeWidthStr;

use super::*;

struct Row<'a> {
    record: &'a RunRecord,
    spec: Option<&'a workflow::Definition>,
    journal: Vec<records::JournalEntry>,
    address: String,
    glyph: String,
    state: RunState,
    calls_you: bool,
    detail: String,
    age: String,
    since: Timestamp,
}

impl Row<'_> {
    fn needs_you(&self) -> bool {
        self.state == RunState::Parked
            || self.calls_you
            || capacity_wait(self.record)
                .is_some_and(|wait| wait.hold.is_some_and(rimz::ids::HoldKind::needs_fix))
    }

    fn held(&self) -> bool {
        !self.needs_you() && capacity_wait(self.record).is_some()
    }
}

fn capacity_wait(record: &RunRecord) -> Option<&Wait> {
    record
        .wait
        .as_ref()
        .filter(|wait| record.state == RunState::Active && wait.outcome == "capacity")
}

pub(super) fn run(
    json: bool,
    all: Option<usize>,
    globals: &super::super::GlobalFlags,
) -> Result<()> {
    let live = resolve::all()?;
    let Some(limit) = all else {
        return render(&live, None, json, globals);
    };
    let now = Timestamp::now();
    let mut finished = finished()?;
    drop_live(
        &mut finished,
        &live.iter().map(|run| &run.record).collect::<Vec<_>>(),
    );
    if json {
        let mut rows = live_json(&live, now);
        rows.extend(finished.iter().take(limit).map(Finished::json_row));
        return render::json_pretty(&rows);
    }
    render(&live, None, false, globals)?;
    let caller = resolve::caller(globals).map(|workspace| workspace.project_root);
    write_finished(&mut render::out(), &finished, caller.as_deref(), limit, now)?;
    Ok(())
}

type FoldKey<'a> = (
    Option<&'a std::path::Path>,
    Option<&'a str>,
    Option<&'a records::ScopeKind>,
    Option<&'a std::path::Path>,
    Option<&'a WorkflowRunId>,
    RunState,
    bool,
    Option<std::mem::Discriminant<records::RunEnding>>,
);

/// A run that ended, read from its record's header so that any version's record is listed.
struct Finished {
    id: WorkflowRunId,
    header: records::RunHeader,
    ended: Timestamp,
}

impl Finished {
    fn address(&self) -> String {
        self.header
            .address()
            .map_or_else(|| self.id.to_string(), |address| address.to_string())
    }

    fn state(&self) -> RunState {
        // `finished` keeps only headers whose state ends a run.
        self.header.state.unwrap_or(RunState::Failed)
    }

    /// How the run ended, a try marked as the live list marks one.
    fn ending(&self) -> String {
        let word = workflow_view::state_word(self.state(), self.header.ending.as_ref());
        // A try that ended done ended `tried`, which already says it was a try.
        if self.header.once && self.header.ending != Some(records::RunEnding::Tried) {
            format!("once · {word}")
        } else {
            word.into()
        }
    }

    /// What two adjacent rows must agree on to fold, in typed fields: the run's room, workflow,
    /// scope and how it ended. A record naming no address keeps its own row, which prints its id.
    fn key(&self) -> FoldKey<'_> {
        let header = &self.header;
        (
            header.project_root.as_deref(),
            header.workflow.as_deref(),
            header.scope_kind.as_ref(),
            header.scope_path.as_deref(),
            header.address().is_none().then_some(&self.id),
            self.state(),
            header.once,
            header.ending.as_ref().map(std::mem::discriminant),
        )
    }

    fn json_row(&self) -> serde_json::Value {
        let header = &self.header;
        let mut row = serde_json::json!({
            "id": self.id,
            "address": self.address(),
            "workflow": header.workflow,
            "scope": header.scope_path,
            "state": self.state(),
            "node": header.node,
            "since": header.entered_at,
            "detail": "",
            "ended": self.ended,
        });
        if header.once {
            row["once"] = serde_json::json!(true);
        }
        row
    }
}

/// Every run that ended of a workflow started by name, in every recorded room, newest ending
/// first: finished homes and terminal records still in the live directory, whatever version
/// wrote them. A home whose record cannot be read is skipped.
fn finished() -> Result<Vec<Finished>> {
    let mut runs = Vec::new();
    for state in rimz::disk::paths::recorded_rooms_under(&rimz::disk::paths::rimz_home())? {
        let live = state.workflow_runs().unwrap_or_default();
        let finished = state.finished_workflow_runs().unwrap_or_default();
        for home in live.into_iter().chain(finished) {
            let Ok(class) = records::classify(&home) else {
                continue;
            };
            let Some(header) = class.header() else {
                continue;
            };
            let Some(id) = home
                .home
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| WorkflowRunId::parse(name).ok())
            else {
                continue;
            };
            let Some(ended) = header.terminal_at.or(header.created_at) else {
                continue;
            };
            if !header.is_terminal() || header.origin != records::RunOrigin::Deployed {
                continue;
            }
            runs.push(Finished {
                id,
                header: header.into_owned(),
                ended,
            });
        }
    }
    runs.sort_by_key(|run| std::cmp::Reverse(run.ended));
    Ok(runs)
}

/// Drops the finished runs a live row lists: a run that ended between the two reads is listed
/// once, where the first read found it.
fn drop_live(runs: &mut Vec<Finished>, live: &[&RunRecord]) {
    let listed = live
        .iter()
        .filter(|record| !record.state.is_terminal())
        .map(|record| &record.id)
        .collect::<std::collections::HashSet<_>>();
    runs.retain(|run| !listed.contains(&run.id));
}

/// Adjacent runs that agree on every printed fact but the id and time fold into one row: the
/// newest member, the count, and the oldest member's ending.
fn fold<'a>(runs: impl IntoIterator<Item = &'a Finished>) -> Vec<(&'a Finished, usize, Timestamp)> {
    let mut rows: Vec<(&Finished, usize, Timestamp)> = Vec::new();
    for run in runs {
        if let Some((newest, count, oldest)) = rows.last_mut()
            && newest.key() == run.key()
        {
            *count += 1;
            *oldest = run.ended;
            continue;
        }
        rows.push((run, 1, run.ended));
    }
    rows
}

/// The finished section: at most `limit` rows, then the runs the limit hid and how to see them.
/// Runs fold within their room, and each room is headed as the live list heads it when several
/// rooms hold finished runs, the caller's first.
fn write_finished(
    out: &mut impl Write,
    runs: &[Finished],
    caller: Option<&std::path::Path>,
    limit: usize,
    now: Timestamp,
) -> std::io::Result<()> {
    let mut rooms = BTreeMap::<Option<&std::path::Path>, Vec<&Finished>>::new();
    for run in runs {
        rooms
            .entry(run.header.project_root.as_deref())
            .or_default()
            .push(run);
    }
    let mut rooms = rooms.into_iter().collect::<Vec<_>>();
    rooms.sort_by_key(|(root, _)| (root.is_none() || *root != caller, *root));
    let several = rooms.len() > 1;
    let rows = rooms
        .into_iter()
        .flat_map(|(root, runs)| fold(runs).into_iter().map(move |row| (root, row)))
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return Ok(());
    }
    let shown = &rows[..rows.len().min(limit)];
    let machine = super::super::machine_config();
    let glyph = rimz::theme::theme_glyphs(&machine.theme);
    let cells = shown
        .iter()
        .map(|(_, (run, count, oldest))| {
            let mut ending = run.ending();
            if *count > 1 {
                ending.push_str(&format!(" ×{count}"));
                if let Some(span) = render::folded_span(*oldest, run.ended) {
                    ending.push_str(&format!(" {span}"));
                }
            }
            let role = workflow_view::ended_glyph_role(run.state());
            (glyph(role), run.address(), ending)
        })
        .collect::<Vec<_>>();
    let name = cells
        .iter()
        .map(|(glyph, address, _)| glyph.width() + 1 + address.width())
        .max()
        .unwrap_or(0);
    let ending = cells
        .iter()
        .map(|(.., ending)| ending.width())
        .max()
        .unwrap_or(0);
    let id = shown
        .iter()
        .map(|(_, (run, ..))| run.id.as_str().width())
        .max()
        .unwrap_or(0);
    writeln!(out)?;
    writeln!(
        out,
        "{}",
        render::paint(render::palette::header(), "FINISHED")
    )?;
    let mut room = None;
    for ((root, (run, ..)), (glyph, address, ended)) in shown.iter().zip(&cells) {
        if several && room != Some(*root) {
            if room.is_some() {
                writeln!(out)?;
            }
            room = Some(*root);
            if let Some(root) = root {
                writeln!(
                    out,
                    "{}",
                    render::paint(render::palette::header(), &render::home_relative_path(root))
                )?;
            }
        }
        writeln!(
            out,
            "  {} {}{:name_pad$}  {}{:ending_pad$}  {}{:id_pad$}  {}",
            render::paint(render::status::workflow(run.state(), false), glyph),
            render::paint(render::palette::body(), address),
            "",
            ended,
            "",
            run.id,
            "",
            render::paint(
                render::palette::muted(),
                &render::workflow_clock(run.ended, now)
            ),
            name_pad = name.saturating_sub(glyph.width() + 1 + address.width()),
            ending_pad = ending.saturating_sub(ended.width()),
            id_pad = id.saturating_sub(run.id.as_str().width()),
        )?;
    }
    let hidden = rows[shown.len()..]
        .iter()
        .map(|(_, (_, count, _))| count)
        .sum::<usize>();
    if hidden > 0 {
        writeln!(
            out,
            "  {hidden} more finished {}; rimz workflow list --all -n {} shows {}",
            if hidden == 1 { "run" } else { "runs" },
            rows.len(),
            if hidden == 1 { "it" } else { "them" }
        )?;
    }
    Ok(())
}

pub(super) fn render(
    runs: &[resolve::Located],
    workflow: Option<&str>,
    json: bool,
    globals: &super::super::GlobalFlags,
) -> Result<()> {
    let now = Timestamp::now();
    if json {
        return render::json_pretty(&live_json(runs, now));
    }
    let live = runs
        .iter()
        .filter(|run| !run.record.state.is_terminal())
        .collect::<Vec<_>>();
    let specs = live
        .iter()
        .map(|run| workflow::load(&run.record.definition_dir(&run.paths)).ok())
        .collect::<Vec<_>>();
    let mut out = render::out();
    if live.is_empty() {
        if let Some(name) = workflow {
            writeln!(out, "no active or parked runs of {name}")?;
        } else {
            writeln!(
                out,
                "no active or parked runs; start one with rimz workflow run <name>"
            )?;
        }
        return Ok(());
    }
    let caller = resolve::caller(globals).map(|workspace| workspace.project_root);
    let mut rooms = BTreeMap::new();
    let mut views = BTreeMap::new();
    let machine = super::super::machine_config();
    let glyph = rimz::theme::theme_glyphs(&machine.theme);
    let loaded = workflow_view::definitions();
    for (run, spec) in live.into_iter().zip(&specs) {
        let record = &run.record;
        let view = views
            .entry(&record.project_root)
            .or_insert_with(|| RoomView::new(&record.project_root));
        let calls_you = workflow_view::calls_you(record);
        let capacity = capacity_wait(record).filter(|_| !calls_you);
        let role = workflow_view::glyph_role(record);
        let since = capacity.map_or_else(|| workflow_view::since(record), |wait| wait.first);
        let agent = (workflow_view::with_you(record) && !calls_you)
            .then(|| view.agent(&run.state, record, false))
            .flatten();
        let refusal =
            workflow_view::definition_refusal(record, &record.definition_dir(&run.paths), &loaded);
        // The run cannot leave a refused definition until the file is fixed, so that cause
        // comes before a capacity wait still recorded.
        let mut detail = match (&refusal, capacity) {
            (Some(error), _) => format!("waits on its definition: {error}"),
            (None, Some(wait)) => format!(
                "held · {}",
                render::one_line(wait.detail.as_deref().unwrap_or("waiting for its turn"))
            ),
            (None, None) => workflow_view::detail(record, spec.as_ref(), now, agent.as_deref()),
        };
        if record.once {
            detail.insert_str(0, "once · ");
        }
        if let Some(number) = view.pr_number(&record.scope.path) {
            detail.push_str(&format!(" · PR #{number}"));
        }
        rooms
            .entry(&record.project_root)
            .or_insert_with(Vec::new)
            .push(Row {
                record,
                spec: spec.as_ref(),
                journal: records::journal(&run.paths).unwrap_or_default(),
                address: record.address().to_string(),
                glyph: glyph(role),
                state: record.state,
                calls_you,
                detail,
                age: render::age_short(since, now),
                since,
            });
    }
    let mut rooms = rooms.into_iter().collect::<Vec<_>>();
    rooms.sort_by_key(|(root, _)| (Some(*root) != caller.as_ref(), *root));
    let several = rooms.len() > 1;
    let width = std::io::stdout()
        .is_terminal()
        .then(|| render::terminal_columns(100));
    for (index, (root, rows)) in rooms.iter_mut().enumerate() {
        if index > 0 {
            writeln!(out)?;
        }
        if several {
            writeln!(
                out,
                "{}",
                render::paint(render::palette::header(), &render::home_relative_path(root))
            )?;
        }
        rows.sort_by(|a, b| {
            let a_needs = a.needs_you();
            let b_needs = b.needs_you();
            b_needs
                .cmp(&a_needs)
                .then_with(|| {
                    if a_needs {
                        a.since.cmp(&b.since)
                    } else {
                        a.address.cmp(&b.address)
                    }
                })
                .then(a.address.cmp(&b.address))
        });
        let needs = rows.partition_point(Row::needs_you);
        if needs > 0 {
            writeln!(
                out,
                "{}",
                render::paint(render::palette::header(), "NEEDS YOU")
            )?;
            let section = rows[..needs].iter().collect::<Vec<_>>();
            write_rows(&mut out, &section, width, false, false)?;
            writeln!(
                out,
                "  → {}",
                resolve::run_command(section[0].record, "show", globals, None, &[], &[])
            )?;
            if needs < rows.len() {
                writeln!(out)?;
            }
        }
        write_groups(&mut out, &rows[needs..], width)?;
    }
    Ok(())
}

fn live_json(runs: &[resolve::Located], now: Timestamp) -> Vec<serde_json::Value> {
    runs.iter()
        .filter(|run| !run.record.state.is_terminal())
        .map(|run| {
            let spec = workflow::load(&run.record.definition_dir(&run.paths)).ok();
            let detail = workflow_view::detail(&run.record, spec.as_ref(), now, None);
            json_row(&run.record, &detail)
        })
        .collect()
}

fn json_row(record: &RunRecord, detail: &str) -> serde_json::Value {
    let mut row = serde_json::json!({
        "id": record.id,
        "address": record.address().to_string(),
        "workflow": record.workflow,
        "scope": record.scope.path,
        "state": record.state,
        "node": record.node,
        "since": record.entered_at,
        "detail": detail,
    });
    if let Some(hold) = capacity_wait(record).and_then(|wait| wait.hold) {
        row["hold"] = serde_json::json!(hold);
    }
    if record.once {
        row["once"] = serde_json::json!(true);
    }
    row
}

fn write_groups(out: &mut impl Write, rows: &[Row], width: Option<usize>) -> std::io::Result<()> {
    let mut groups = BTreeMap::<&str, Vec<&Row>>::new();
    for row in rows {
        groups.entry(&row.record.workflow).or_default().push(row);
    }
    for (index, (workflow, mut group)) in groups.into_iter().enumerate() {
        if index > 0 {
            writeln!(out)?;
        }
        group.sort_by(|a, b| a.held().cmp(&b.held()).then(a.address.cmp(&b.address)));
        let held = group.iter().filter(|row| row.held()).count();
        let mut heading = format!("{workflow} · {} active", group.len() - held);
        if held > 0 {
            heading.push_str(&format!(" · {held} held"));
        }
        let origin = (group[0].record.task.as_deref(), group[0].record.manual);
        let shared = group
            .iter()
            .all(|row| (row.record.task.as_deref(), row.record.manual) == origin);
        if shared {
            heading.push_str(&format!(" · started by {}", run_origin(group[0].record)));
        }
        if let Some(width) = width {
            heading = render::clip_to_width(&heading, width);
        }
        writeln!(
            out,
            "{}",
            render::paint(render::palette::header(), &heading)
        )?;
        write_rows(out, &group, width, true, !shared)?;
    }
    Ok(())
}

fn write_rows(
    out: &mut impl Write,
    rows: &[&Row],
    width: Option<usize>,
    grouped: bool,
    show_origin: bool,
) -> std::io::Result<()> {
    let labels = rows
        .iter()
        .map(|row| {
            if grouped {
                format!("#{}", row.record.address().lane)
            } else {
                row.address.clone()
            }
        })
        .collect::<Vec<_>>();
    let name = rows
        .iter()
        .zip(&labels)
        .map(|(row, label)| row.glyph.width() + 1 + label.width())
        .max()
        .unwrap_or(0);
    let age = rows.iter().map(|row| row.age.width()).max().unwrap_or(0);
    let budget = width.map(|width| width.saturating_sub(2 + name + age + 4));
    let details = rows
        .iter()
        .map(|row| {
            let mark = if show_origin {
                format!(" · by {}", run_origin(row.record))
            } else {
                String::new()
            };
            let detail = budget.map_or_else(
                || row.detail.clone(),
                |width| render::clip_to_width(&row.detail, width.saturating_sub(mark.width())),
            );
            let detail = format!("{detail}{mark}");
            budget.map_or_else(
                || detail.clone(),
                |width| render::clip_to_width(&detail, width),
            )
        })
        .collect::<Vec<_>>();
    let detail_width = budget.unwrap_or_else(|| {
        details
            .iter()
            .map(|detail| detail.width())
            .max()
            .unwrap_or(0)
    });
    for ((row, label), detail) in rows.iter().zip(labels).zip(details) {
        let style = if row.held() {
            render::status::role(render::status::StateRole::Neutral)
        } else {
            render::status::workflow(row.state, row.needs_you())
        };
        writeln!(
            out,
            "  {} {}{:name_pad$}  {}{:detail_pad$}  {:age_pad$}{}",
            render::paint(style, &row.glyph),
            render::paint(render::palette::body(), &label),
            "",
            detail,
            "",
            "",
            render::paint(render::palette::muted(), &row.age),
            name_pad = name.saturating_sub(row.glyph.width() + 1 + label.width()),
            detail_pad = detail_width.saturating_sub(detail.width()),
            age_pad = age.saturating_sub(row.age.width()),
        )?;
        if let Some(spec) = row.spec {
            let hops = workflow::hops(spec, &row.journal, row.record.held.as_ref());
            rail::horizontal(out, spec, row.record, &hops, &row.journal, width, 6)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ended(workflow: &str, lane: &str, state: &str, once: bool, at: i64) -> Finished {
        let header = records::RunHeader {
            workflow: Some(workflow.into()),
            project_root: Some("/repo".into()),
            scope_kind: Some(records::ScopeKind::Worktree),
            scope_path: Some(format!("/repo/{lane}").into()),
            state: Some(serde_json::from_value(serde_json::json!(state)).unwrap()),
            once,
            ..Default::default()
        };
        Finished {
            id: WorkflowRunId::new(),
            header,
            ended: Timestamp::from_second(at).unwrap(),
        }
    }

    fn ending(mut run: Finished, ending: records::RunEnding) -> Finished {
        run.header.ending = Some(ending);
        run
    }

    fn finished_text(runs: &[Finished], caller: &str, limit: usize) -> String {
        let mut out = Vec::new();
        let caller = std::path::Path::new(caller);
        write_finished(
            &mut out,
            runs,
            Some(caller),
            limit,
            Timestamp::from_second(6000).unwrap(),
        )
        .unwrap();
        anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string()
    }

    #[test]
    fn finished_rows_fold_adjacent_agreeing_runs_and_name_the_runs_the_limit_hid() {
        let gave_up = || records::RunEnding::CapacityGaveUp {
            waited_ms: 60_000,
            reason: None,
        };
        let runs = [
            ended("land", "feat", "done", false, 5000),
            ended("land", "feat", "done", false, 4000),
            // Only the last fact of the fold key differs: a try keeps its own row.
            ended("land", "feat", "done", true, 3000),
            // Agrees with the first row, but a different row sits between them.
            ended("land", "feat", "done", false, 2000),
            ending(
                ended("land", "main", "failed", false, 1000),
                records::RunEnding::Failed,
            ),
            // Prints as the row above, but gave up waiting for capacity.
            ending(ended("land", "main", "failed", false, 900), gave_up()),
            // Prints as the row above, but ran in another checkout of the same name.
            ending(ended("land", "x/main", "failed", false, 800), gave_up()),
            ending(ended("land", "x/main", "failed", false, 700), gave_up()),
            // A try that ended done says so once.
            ending(
                ended("land", "feat", "done", true, 600),
                records::RunEnding::Tried,
            ),
        ];
        let rows = fold(&runs);
        assert_eq!(
            rows.iter()
                .map(|(run, count, _)| (run.id.clone(), *count))
                .collect::<Vec<_>>(),
            [(0, 2), (2, 1), (3, 1), (4, 1), (5, 1), (6, 2), (8, 1)]
                .map(|(index, count)| (runs[index].id.clone(), count))
                .to_vec()
        );
        let text = finished_text(&runs, "/repo", 2);
        let lines = text.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 5, "{text}");
        assert_eq!(lines[0], "", "{text}");
        assert_eq!(lines[1], "FINISHED", "{text}");
        assert!(
            lines[2].contains("land#feat") && lines[2].contains("done ×2 over 16m"),
            "{text}"
        );
        assert_eq!(text.matches(runs[0].id.as_str()).count(), 1, "{text}");
        assert!(!text.contains(runs[1].id.as_str()), "{text}");
        assert!(lines[3].contains("once · done") && lines[3].contains(runs[2].id.as_str()));
        assert_eq!(
            lines[4],
            "  6 more finished runs; rimz workflow list --all -n 7 shows them"
        );
        assert!(
            finished_text(&runs, "/repo", 5)
                .ends_with("  3 more finished runs; rimz workflow list --all -n 7 shows them\n"),
            "a hidden folded row counts each of its runs"
        );
        assert!(
            finished_text(&runs, "/repo", 6)
                .ends_with("  1 more finished run; rimz workflow list --all -n 7 shows it\n"),
            "one hidden run reads in the singular"
        );
        let text = finished_text(&runs, "/repo", 7);
        assert!(!text.contains("more finished"), "every row fits: {text}");
        for run in &runs[4..7] {
            assert_eq!(text.matches(run.id.as_str()).count(), 1, "{text}");
        }
        assert!(!text.contains(runs[7].id.as_str()), "{text}");
        let tried = text
            .lines()
            .find(|line| line.contains(runs[8].id.as_str()))
            .unwrap();
        assert!(
            tried.contains(" tried ") && !tried.contains("once"),
            "{text}"
        );
        assert!(
            finished_text(&[], "/repo", 20).is_empty(),
            "no finished runs print no section"
        );
    }

    #[test]
    fn finished_rows_head_each_room_when_several_hold_runs_the_callers_first() {
        let mut other = ended("land", "feat", "done", false, 2000);
        other.header.project_root = Some("/other".into());
        other.header.scope_path = Some("/other/feat".into());
        let runs = [ended("land", "feat", "done", false, 3000), other];
        for (caller, first, second) in [("/repo", 0, 1), ("/other", 1, 0)] {
            let text = finished_text(&runs, caller, 20);
            let lines = text.lines().collect::<Vec<_>>();
            let room = |index: usize| {
                runs[index]
                    .header
                    .project_root
                    .as_ref()
                    .unwrap()
                    .display()
                    .to_string()
            };
            assert_eq!(lines.len(), 7, "{text}");
            assert_eq!(lines[1], "FINISHED", "{text}");
            assert_eq!(lines[2], room(first), "{text}");
            assert!(
                lines[3].contains("land#feat") && lines[3].contains(runs[first].id.as_str()),
                "{text}"
            );
            assert_eq!(lines[4], "", "{text}");
            assert_eq!(lines[5], room(second), "{text}");
            assert!(lines[6].contains(runs[second].id.as_str()), "{text}");
        }
    }

    #[test]
    fn a_run_the_live_rows_hold_is_no_finished_row() {
        let active = record("feat", None, false, false);
        let mut owed = record("owed", None, false, false);
        owed.state = RunState::Done;
        let mut runs = vec![
            ended("review", "feat", "done", false, 3000),
            ended("review", "owed", "done", false, 2000),
            ended("review", "gone", "done", false, 1000),
        ];
        runs[0].id = active.id.clone();
        runs[1].id = owed.id.clone();
        let kept = [runs[1].id.clone(), runs[2].id.clone()];
        drop_live(&mut runs, &[&active, &owed]);
        assert_eq!(
            runs.iter().map(|run| run.id.clone()).collect::<Vec<_>>(),
            kept,
            "an active run is listed live; a terminal one still in the live directory is not"
        );
    }

    fn record(lane: &str, task: Option<&str>, manual: bool, held: bool) -> RunRecord {
        serde_json::from_value(serde_json::json!({
            "version":rimz::store::workflow::VERSION, "id":WorkflowRunId::new(), "workflow":"review",
            "spec_dir":"/spec", "project_root":"/repo",
            "scope":{"kind":"worktree", "path":format!("/repo/{lane}")},
            "created_at":Timestamp::UNIX_EPOCH, "entered_at":Timestamp::UNIX_EPOCH,
            "state":"active", "node":"gate", "visits":{}, "visit_grant":0,
            "deadline_fired":false, "next_seq":1, "task":task, "manual":manual,
            "wait":held.then(|| serde_json::json!({
                "node":"gate", "outcome":"capacity", "hold":"queue", "detail":"queued",
                "first":Timestamp::UNIX_EPOCH, "last":Timestamp::UNIX_EPOCH, "count":3,
            })),
        }))
        .unwrap()
    }

    fn row(record: &RunRecord) -> Row<'_> {
        Row {
            record,
            spec: None,
            journal: Vec::new(),
            address: record.address().to_string(),
            glyph: if capacity_wait(record).is_some() {
                "○"
            } else {
                "⢿"
            }
            .into(),
            state: record.state,
            calls_you: false,
            detail: if capacity_wait(record).is_some() {
                "held · queued"
            } else {
                "running"
            }
            .into(),
            age: "1m".into(),
            since: Timestamp::UNIX_EPOCH,
        }
    }

    #[test]
    fn groups_name_the_origin_only_when_every_run_shares_it() {
        let cases = [
            (Some("land"), false, Some("land"), false, true, 0),
            (Some("land"), false, Some("land"), true, false, 2),
            (None, false, Some("land"), false, false, 2),
        ];
        let actual = cases.map(|(first, first_manual, second, second_manual, _, _)| {
            let records = [
                record("one", first, first_manual, false),
                record("two", second, second_manual, false),
            ];
            let rows = records.iter().map(row).collect::<Vec<_>>();
            let mut out = anstream::StripStream::new(Vec::new());
            write_groups(&mut out, &rows, None).unwrap();
            let text = String::from_utf8(out.into_inner()).unwrap();
            (
                text.lines()
                    .next()
                    .unwrap()
                    .contains("started by loop task land"),
                text.matches(" · by ").count(),
            )
        });
        assert_eq!(
            actual,
            cases.map(|(_, _, _, _, heading, count)| (heading, count))
        );
    }

    #[test]
    fn groups_sort_held_rows_last_and_print_each_pr_once() {
        let records = [
            record("a-held", Some("land"), false, true),
            record("z-run", Some("land"), false, false),
        ];
        let mut rows = records.iter().map(row).collect::<Vec<_>>();
        for (row, number) in rows.iter_mut().zip([1517, 1518]) {
            row.detail.push_str(&format!(" · PR #{number}"));
        }
        let mut out = anstream::StripStream::new(Vec::new());
        write_groups(&mut out, &rows, None).unwrap();
        let text = String::from_utf8(out.into_inner()).unwrap();
        assert!(
            text.starts_with("review · 1 active · 1 held · started by loop task land\n"),
            "{text}"
        );
        assert!(
            text.find("#z-run").unwrap() < text.find("#a-held").unwrap(),
            "{text}"
        );
        assert!(
            text.contains("held · queued") && !text.contains('×'),
            "{text}"
        );
        assert_eq!(text.matches("PR #").count(), 2, "{text}");
        assert!(!text.contains("HELD") && !text.contains("ACTIVE"), "{text}");
    }

    #[test]
    fn agent_names_join_on_the_workflow_pin_and_use_card_handles() {
        use rimz::agents::{WorkflowAttemptRef, WorkflowAttemptRole};
        let mut record = record("lane", None, false, false);
        record.attempt = Some(
            serde_json::from_value(serde_json::json!({
                "kind":"agent", "lifecycle":"session", "seq":7, "node":"gate",
                "started_at":Timestamp::UNIX_EPOCH, "host":{"pid":1, "start":null},
                "run":{"run_id":rimz::ids::RunId::new(), "handle":"@pet"},
            }))
            .unwrap(),
        );
        let mut agent = rimz::testkit::agent_state("codex", "leader", Timestamp::UNIX_EPOCH);
        agent.profile = Some("yagni".into());
        agent.worktree_path = Some("/repo/lane".into());
        rimz::testkit::set_own_lane(&mut agent);
        let pin = WorkflowAttemptRef {
            run: record.id.clone(),
            seq: 7,
            role: WorkflowAttemptRole::Step,
        };
        let pins = [
            pin.clone(),
            WorkflowAttemptRef {
                seq: 8,
                ..pin.clone()
            },
            WorkflowAttemptRef {
                run: WorkflowRunId::new(),
                ..pin.clone()
            },
            WorkflowAttemptRef {
                role: WorkflowAttemptRole::Judge,
                ..pin
            },
        ];
        let actual = pins.map(|pin| {
            agent.workflow = Some(pin);
            let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
                rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/repo")),
                vec![agent.clone()],
                Timestamp::UNIX_EPOCH,
            );
            (
                workflow_agent(&record, &snapshot, false, None),
                workflow_agent(&record, &snapshot, true, None),
            )
        });
        assert_eq!(
            actual,
            [
                (Some("@yagni".into()), Some("@yagni#lane".into())),
                (None, None),
                (None, None),
                (None, None)
            ]
        );
    }

    #[test]
    fn agent_names_choose_the_attempt_leader_among_pinned_layout_cells() {
        use rimz::agents::{WorkflowAttemptRef, WorkflowAttemptRole};
        let mut record = record("lane", None, false, false);
        record.attempt = Some(
            serde_json::from_value(serde_json::json!({
                "kind":"agent", "lifecycle":"session", "seq":7, "node":"gate",
                "started_at":Timestamp::UNIX_EPOCH, "host":{"pid":1, "start":null},
                "run":{"run_id":rimz::ids::RunId::new(), "handle":"@pet"},
            }))
            .unwrap(),
        );
        let mut leader = rimz::testkit::agent_state("codex", "leader", Timestamp::UNIX_EPOCH);
        leader.name = Some("pet".into());
        leader.profile = Some("yagni".into());
        leader.worktree_path = Some("/repo/lane".into());
        rimz::testkit::set_own_lane(&mut leader);
        leader.workflow = Some(WorkflowAttemptRef {
            run: record.id.clone(),
            seq: 7,
            role: WorkflowAttemptRole::Step,
        });
        let mut companion = leader.clone();
        companion.agent_id = "companion".into();
        companion.name = Some("companion".into());
        companion.profile = Some("helper".into());
        let actual = [
            vec![companion.clone(), leader.clone()],
            vec![leader, companion],
        ]
        .map(|agents| {
            let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
                rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/repo")),
                agents,
                Timestamp::UNIX_EPOCH,
            );
            (
                workflow_agent(&record, &snapshot, false, None),
                workflow_agent(&record, &snapshot, true, None),
            )
        });
        assert_eq!(
            actual,
            std::array::from_fn(|_| (Some("@yagni".into()), Some("@yagni#lane".into())))
        );
    }

    #[test]
    fn json_adds_only_known_capacity_causes_and_keeps_the_detail() {
        let mut record: RunRecord = serde_json::from_value(serde_json::json!({
            "version":rimz::store::workflow::VERSION, "id":WorkflowRunId::new(), "workflow":"review",
            "spec_dir":"/spec", "project_root":"/repo",
            "scope":{"kind":"worktree", "path":"/repo/feature"},
            "created_at":Timestamp::UNIX_EPOCH, "entered_at":Timestamp::UNIX_EPOCH,
            "state":"active", "node":"build", "visits":{}, "visit_grant":0,
            "deadline_fired":false, "next_seq":1,
        }))
        .unwrap();
        for (outcome, hold) in [
            ("capacity", Some("check_failed")),
            ("capacity", None),
            ("window", None),
        ] {
            record.wait = Some(
                serde_json::from_value(serde_json::json!({
                    "node":"build", "outcome":outcome, "hold":hold,
                    "first":Timestamp::UNIX_EPOCH, "last":Timestamp::UNIX_EPOCH, "count":2,
                }))
                .unwrap(),
            );
            let row = json_row(&record, "recorded detail");
            assert_eq!(row.get("hold"), hold.map(serde_json::Value::from).as_ref());
            assert_eq!(row["detail"], "recorded detail");
            assert_eq!(row["since"], serde_json::json!(record.entered_at));
            assert_eq!(
                row.as_object().unwrap().len(),
                8 + usize::from(hold.is_some())
            );
        }
    }

    #[test]
    fn narrow_rows_clip_detail_before_age() {
        let record = serde_json::from_value(serde_json::json!({
            "version":rimz::store::workflow::VERSION, "id":WorkflowRunId::new(), "workflow":"review",
            "spec_dir":"/spec", "project_root":"/repo",
            "scope":{"kind":"worktree", "path":"/repo/feature"},
            "created_at":Timestamp::UNIX_EPOCH, "entered_at":Timestamp::UNIX_EPOCH,
            "state":"parked", "node":"decision", "visits":{}, "visit_grant":0,
            "deadline_fired":false, "next_seq":1,
        }))
        .unwrap();
        let spec: workflow::Definition = toml::from_str("[workflow]\nscope='worktree'\nstart='decision'\n[test.decision]\ntimeout='365d'\nshell='true'\non.pass='work'\non.fail='@user'\n[command.work]\nshell='true'\ntimeout='1h'\nthen='decision'\n").unwrap();
        let row = Row {
            record: &record,
            spec: Some(&spec),
            journal: Vec::new(),
            address: "review#feature".into(),
            glyph: rimz::theme::theme_glyphs(&rimz::config::ThemeConfig::default())(
                workflow_view::glyph_role(&record),
            ),
            state: RunState::Parked,
            calls_you: false,
            detail: "user: This decision needs a long explanation".repeat(3),
            age: "47m".into(),
            since: Timestamp::UNIX_EPOCH,
        };
        for width in [40, 60] {
            let mut out = anstream::StripStream::new(Vec::new());
            write_rows(&mut out, &[&row], Some(width), false, false).unwrap();
            let text = String::from_utf8(out.into_inner()).unwrap();
            assert!(
                text.lines().all(|line| line.width() <= width),
                "{width}: {text}"
            );
            assert!(
                text.contains('…') && text.lines().next().unwrap().ends_with("47m"),
                "{text}"
            );
            assert!(
                text.lines()
                    .nth(1)
                    .is_some_and(|line| line.starts_with("      ")),
                "{text}"
            );
            let rail = text.split_once('\n').unwrap().1;
            assert_eq!(rail.matches("decision").count(), 1, "{text}");
        }
    }
}
