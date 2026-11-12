use std::collections::BTreeMap;
use std::io::IsTerminal;

use jiff::Timestamp;
use rimz::store::workflow::{RunState, Wait};
use unicode_width::UnicodeWidthStr;

use super::*;

struct Row<'a> {
    record: &'a RunRecord,
    spec: Option<&'a workflow::WorkflowSpec>,
    journal: Vec<records::JournalEntry>,
    ignored: &'a [workflow::Issue],
    address: String,
    glyph: String,
    state: RunState,
    asking: bool,
    detail: String,
    age: String,
    since: Timestamp,
}

impl Row<'_> {
    fn needs_you(&self) -> bool {
        self.state == RunState::Parked
            || self.asking
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

pub(super) fn run(json: bool, globals: &super::super::GlobalFlags) -> Result<()> {
    for refusal in resolve::refused()? {
        writeln!(
            render::err(),
            "{} {refusal}",
            render::paint(render::palette::warn().bold(), "warning:")
        )?;
    }
    render(&resolve::all()?, None, json, globals)
}

pub(super) fn render(
    runs: &[resolve::Located],
    workflow: Option<&str>,
    json: bool,
    globals: &super::super::GlobalFlags,
) -> Result<()> {
    let now = Timestamp::now();
    let live = runs
        .iter()
        .filter(|run| !run.record.state.is_terminal())
        .collect::<Vec<_>>();
    let specs = live
        .iter()
        .map(|run| workflow::load(&run.paths.home).ok())
        .collect::<Vec<_>>();
    if json {
        let rows = live
            .iter()
            .zip(&specs)
            .map(|(run, spec)| {
                let detail = workflow_view::detail(&run.record, spec.as_ref(), now, None);
                let mut row = json_row(&run.record, &detail);
                if let Some(spec) = spec
                    && !spec.ignored.is_empty()
                {
                    row["warnings"] = serde_json::json!(spec.ignored);
                }
                row
            })
            .collect::<Vec<_>>();
        return render::json_pretty(&rows);
    }
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
    for (run, spec) in live.into_iter().zip(&specs) {
        let record = &run.record;
        let view = views
            .entry(&record.project_root)
            .or_insert_with(|| RoomView::new(&record.project_root));
        let asking = workflow_view::asking(record).is_some();
        let capacity = capacity_wait(record).filter(|_| !asking);
        let role = workflow_view::glyph_role(record);
        let since = capacity.map_or_else(|| workflow_view::since(record), |wait| wait.first);
        let agent = (workflow_view::with_you(record) && !asking)
            .then(|| view.agent(&run.state, record, false))
            .flatten();
        let mut detail = capacity.map_or_else(
            || workflow_view::detail(record, spec.as_ref(), now, agent.as_deref()),
            |wait| {
                format!(
                    "held · {}",
                    render::one_line(wait.detail.as_deref().unwrap_or("waiting for its turn"))
                )
            },
        );
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
                ignored: spec.as_ref().map_or(&[], |spec| spec.ignored.as_slice()),
                address: record.address().to_string(),
                glyph: glyph(role),
                state: record.state,
                asking,
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
            let mut mark = if row.ignored.is_empty() {
                String::new()
            } else {
                format!(" · {} ignored", row.ignored.len())
            };
            if budget.is_some_and(|width| mark.width() > width) {
                mark.clear();
            }
            if show_origin {
                mark.push_str(&format!(" · by {}", run_origin(row.record)));
            }
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

    fn record(lane: &str, task: Option<&str>, manual: bool, held: bool) -> RunRecord {
        serde_json::from_value(serde_json::json!({
            "version":rimz::store::workflow::VERSION, "id":WorkflowRunId::new(), "workflow":"review",
            "spec_dir":"/spec", "spec_digest":"digest", "project_root":"/repo",
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
            ignored: &[],
            address: record.address().to_string(),
            glyph: if capacity_wait(record).is_some() {
                "○"
            } else {
                "⢿"
            }
            .into(),
            state: record.state,
            asking: false,
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
                "kind":"agent", "lifecycle":"stay", "seq":7, "node":"gate",
                "started_at":Timestamp::UNIX_EPOCH, "host":{"pid":1, "start":null},
                "run":{"run_id":rimz::ids::RunId::new(), "handle":"@pet"},
            }))
            .unwrap(),
        );
        let mut agent = rimz::testkit::agent_state("codex", "leader", Timestamp::UNIX_EPOCH);
        agent.profile = Some("yagni".into());
        agent.worktree_path = Some("/repo/lane".into());
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
                workflow_agent(&record, &snapshot, false),
                workflow_agent(&record, &snapshot, true),
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
                "kind":"agent", "lifecycle":"stay", "seq":7, "node":"gate",
                "started_at":Timestamp::UNIX_EPOCH, "host":{"pid":1, "start":null},
                "run":{"run_id":rimz::ids::RunId::new(), "handle":"@pet"},
            }))
            .unwrap(),
        );
        let mut leader = rimz::testkit::agent_state("codex", "leader", Timestamp::UNIX_EPOCH);
        leader.name = Some("pet".into());
        leader.profile = Some("yagni".into());
        leader.worktree_path = Some("/repo/lane".into());
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
                workflow_agent(&record, &snapshot, false),
                workflow_agent(&record, &snapshot, true),
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
            "spec_dir":"/spec", "spec_digest":"digest", "project_root":"/repo",
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
            "spec_dir":"/spec", "spec_digest":"digest", "project_root":"/repo",
            "scope":{"kind":"worktree", "path":"/repo/feature"},
            "created_at":Timestamp::UNIX_EPOCH, "entered_at":Timestamp::UNIX_EPOCH,
            "state":"parked", "node":"decision", "visits":{}, "visit_grant":0,
            "deadline_fired":false, "next_seq":1,
        }))
        .unwrap();
        let spec: workflow::WorkflowSpec = toml::from_str("[workflow]\nscope='worktree'\nstart='decision'\n[checks.decision]\nshell='true'\non.pass='work'\non.fail='@user'\n[steps.work]\nshell='true'\ntimeout='1h'\nthen='decision'\n").unwrap();
        let mut rows = [Row {
            record: &record,
            spec: Some(&spec),
            journal: Vec::new(),
            ignored: &[],
            address: "review#feature".into(),
            glyph: rimz::theme::theme_glyphs(&rimz::config::ThemeConfig::default())(
                workflow_view::glyph_role(&record),
            ),
            state: RunState::Parked,
            asking: false,
            detail: "user: This decision needs a long explanation".repeat(3),
            age: "47m".into(),
            since: Timestamp::UNIX_EPOCH,
        }];
        let ignored = [workflow::Issue {
            path: "workflow.future".into(),
            message: "ignored field: unknown field".into(),
        }];
        for ignored in [ignored.as_slice(), &[]] {
            rows[0].ignored = ignored;
            for width in [40, 60] {
                let mut out = anstream::StripStream::new(Vec::new());
                write_rows(&mut out, &[&rows[0]], Some(width), false, false).unwrap();
                let text = String::from_utf8(out.into_inner()).unwrap();
                assert!(
                    text.lines().all(|line| line.width() <= width),
                    "{width}: {text}"
                );
                assert!(
                    text.contains('…') && text.lines().next().unwrap().ends_with("47m"),
                    "{text}"
                );
                assert_eq!(text.contains("· 1 ignored"), !ignored.is_empty(), "{text}");
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
}
