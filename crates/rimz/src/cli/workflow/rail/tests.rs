use super::*;

const LAND: &str = r#"
[workflow]
scope = 'worktree'
start = 'begin'
[checks.begin]
shell = 'true'
on.pass = 'sweep'
on.fail = '@user'
[steps.sweep]
shell = 'echo sweep'
timeout = '1h'
then = 'watch'
[checks.watch]
cases = [{ when = ['ci=passed'], outcome = 'green' }]
else = 'red'
on.green = 'notes'
on.red = 'fix'
timeout = '3d'
[steps.fix]
shell = 'echo fix'
timeout = '1h'
then = 'watch'
max-visits = 4
[checks.notes]
shell = 'true'
on.pass = 'triage'
on.fail = '@done'
[steps.triage]
shell = 'echo triage'
timeout = '1h'
then = 'ruled'
max-visits = 2
[checks.ruled]
shell = 'true'
on.pass = 'triage'
on.fail = '@done'
"#;

#[test]
fn turn_end_footer_only_describes_turn_agent_steps() {
    for (runner, turn_end) in [
        ("shell='true'", false),
        ("agent='claude'\nlifecycle='stay'", false),
        ("agent='claude'\nlifecycle='until'", false),
        ("agent='claude'", true),
    ] {
        let spec: WorkflowSpec = toml::from_str(&format!(
            "[workflow]\nscope='worktree'\nstart='gate'\n\
             [checks.gate]\nshell='true'\non.pass='work'\n\
             [steps.work]\nthen='gate'\n{runner}\n"
        ))
        .unwrap();
        assert_eq!(
            defaults(&spec).contains(&"A step goes on to its next check however its turn ends."),
            turn_end,
            "{runner}"
        );
    }
}

fn record(node: &str) -> RunRecord {
    serde_json::from_value(serde_json::json!({
        "version":1, "id":rimz::ids::WorkflowRunId::new(), "workflow":"land",
        "spec_dir":"/spec", "spec_digest":"digest", "project_root":"/repo",
        "scope":{"kind":"worktree", "path":"/repo/lane"},
        "created_at":jiff::Timestamp::UNIX_EPOCH, "entered_at":jiff::Timestamp::UNIX_EPOCH,
        "state":"active", "node":node, "visits":{}, "visit_grant":0,
        "deadline_fired":false, "next_seq":1,
    }))
    .unwrap()
}

fn route(node: &str, outcome: &str, target: &str) -> Hop {
    Hop {
        at: jiff::Timestamp::UNIX_EPOCH,
        node: node.into(),
        outcome: Some(outcome.into()),
        target: Some(target.into()),
        count: 1,
        kind: HopKind::Route,
    }
}

fn land_hops() -> Vec<Hop> {
    vec![
        route("begin", "pass", "sweep"),
        route("sweep", "ended", "watch"),
        route("watch", "green", "notes"),
        route("notes", "pass", "triage"),
    ]
}

fn horizontal_text(record: &RunRecord, hops: &[Hop], width: Option<usize>) -> String {
    let spec: WorkflowSpec = toml::from_str(LAND).unwrap();
    let mut out = anstream::StripStream::new(Vec::new());
    horizontal(&mut out, &spec, record, hops, &[], width, 6).unwrap();
    String::from_utf8(out.into_inner()).unwrap()
}

#[test]
fn reached_counts_entries_not_repeated_wait_answers() {
    let mut wait = route("watch", "pending", "@wait");
    wait.count = 3;
    let hops = [
        route("begin", "pass", "sweep"),
        route("sweep", "ended", "watch"),
        wait,
        route("watch", "green", "clean"),
    ];
    assert_eq!(
        reached(&hops, &[]),
        BTreeMap::from([("begin", 1), ("sweep", 1), ("watch", 1), ("clean", 1),])
    );
}

#[test]
fn horizontal_walk_distinguishes_kinds_and_states_without_color() {
    let hops = land_hops();
    let text = horizontal_text(&record("triage"), &hops, None);
    assert_eq!(
        text,
        "      ◆ begin ─ ● sweep ─ ◆ watch ─ ◆ notes ─ ⢿ triage ─ ◇ ruled\n"
    );
    assert_eq!(text.matches("triage").count(), 1, "{text}");
    let current_check = horizontal_text(&record("watch"), &hops[..2], None);
    assert!(
        current_check.contains("◆ begin ─ ● sweep ─ ⢿ watch ─ ◇ notes ─ ○ triage ─ ◇ ruled"),
        "{current_check}"
    );
    assert!(
        !text.contains("fix") && !current_check.contains("fix"),
        "{text}\n{current_check}"
    );
    let mut held = record("sweep");
    held.wait = Some(
        serde_json::from_value(serde_json::json!({
            "node":"sweep", "outcome":"capacity", "hold":"queue",
            "first":jiff::Timestamp::UNIX_EPOCH, "last":jiff::Timestamp::UNIX_EPOCH,
            "count":1,
        }))
        .unwrap(),
    );
    let text = horizontal_text(&held, &hops[..1], None);
    assert!(text.contains("◆ begin ─ ◉ sweep ─ ◇ watch"), "{text}");
    for set in ["unicode", "nerd_font"] {
        let mut theme = rimz::config::ThemeConfig::default();
        theme.glyphs.set = Some(set.into());
        let glyph = rimz::theme::theme_glyphs(&theme);
        let current = glyph(super::super::workflow_view::glyph_role(&held));
        for future in [GlyphRole::PipelineFuture, GlyphRole::PipelineCheckFuture] {
            assert_ne!(current, glyph(future), "{set}");
        }
    }
}

#[test]
fn live_current_node_roles_differ_from_every_node_mark_in_each_set() {
    for set in ["unicode", "nerd_font"] {
        let mut theme = rimz::config::ThemeConfig::default();
        theme.glyphs.set = Some(set.into());
        let glyph = rimz::theme::theme_glyphs(&theme);
        for current in [
            GlyphRole::StatusWorking,
            GlyphRole::StatusWaiting,
            GlyphRole::StatusSleeping,
            GlyphRole::PipelineCurrent,
        ] {
            for mark in [
                GlyphRole::PipelineFuture,
                GlyphRole::PipelinePassed,
                GlyphRole::PipelineCheckFuture,
                GlyphRole::PipelineCheckPassed,
            ] {
                assert_ne!(
                    glyph(current),
                    glyph(mark),
                    "{set}: {current:?} vs {mark:?}"
                );
            }
        }
    }
}

#[test]
fn horizontal_detours_hang_under_their_parent_and_follow_width_elision() {
    let mut record = record("triage");
    record.visits.insert(
        "fix".into(),
        rimz::store::workflow::StepVisits {
            total: 2,
            ..Default::default()
        },
    );
    let mut hops = land_hops();
    hops.splice(
        2..2,
        [
            route("watch", "red", "fix"),
            route("fix", "ended", "watch"),
            route("watch", "red", "fix"),
            route("fix", "ended", "watch"),
        ],
    );
    let text = horizontal_text(&record, &hops, None);
    let lines = text.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 2, "{text}");
    assert_eq!(
        lines[1].find('╰'),
        lines[0]
            .find("◆ watch")
            .map(|at| unicode_width::UnicodeWidthStr::width(&lines[0][..at])),
        "{text}"
    );
    assert!(lines[1].contains("╰─ ● fix ×2 ↑"), "{text}");
    record.node = "fix".into();
    let current = horizontal_text(&record, &hops[..5], None);
    assert!(current.contains("╰─ ⢿ fix ×2 ↑"), "{current}");
    assert_eq!(current.matches("fix").count(), 1, "{current}");
    record.node = "triage".into();
    for width in [40, 60] {
        let text = horizontal_text(&record, &hops, Some(width));
        assert!(
            text.starts_with("      … ") && text.contains("◆ notes ─ ⢿ triage"),
            "{width}: {text}"
        );
        assert!(
            text.lines()
                .all(|line| unicode_width::UnicodeWidthStr::width(line) <= width),
            "{width}: {text}"
        );
        assert_eq!(text.contains("╰─"), text.contains("◆ watch"), "{text}");
    }
}

#[test]
fn horizontal_elides_unreached_left_and_reached_right_branches() {
    let spec: WorkflowSpec = toml::from_str(
        "[workflow]\nscope='worktree'\nstart='choose'\n\
         [checks.choose]\nshell='true'\non.a='unit_tests'\n\
         on.b='integration_tests'\non.c='used'\n\
         [steps.unit_tests]\nshell='true'\nthen='used'\n\
         [steps.integration_tests]\nshell='true'\nthen='used'\n\
         [checks.used]\nshell='true'\non.pass='final'\n\
         [checks.final]\nshell='true'\non.pass='@done'\non.retry='choose'\n",
    )
    .unwrap();
    let mut hops = vec![route("choose", "c", "used"), route("used", "pass", "final")];
    let mut rendered = Vec::new();
    for (node, width, anchors) in [
        ("final", 60, "◆ used ─ ⢿ final"),
        ("choose", 40, "⢿ choose"),
    ] {
        if node == "choose" {
            hops.push(route("final", "retry", "choose"));
        }
        let mut out = anstream::StripStream::new(Vec::new());
        horizontal(&mut out, &spec, &record(node), &hops, &[], Some(width), 6).unwrap();
        let text = String::from_utf8(out.into_inner()).unwrap();
        assert!(text.contains(anchors), "{text}");
        rendered.push((width, text));
    }
    assert!(
        rendered
            .iter()
            .all(|(width, text)| text.lines().all(|line| line.width() <= *width)),
        "{rendered:#?}"
    );
}

#[test]
fn horizontal_keeps_a_terminal_detour_and_its_parent_even_without_visits() {
    let spec: WorkflowSpec = toml::from_str(LAND).unwrap();
    let mut record = record("fix");
    record.state = RunState::Failed;
    let mut hops = land_hops()[..2].to_vec();
    hops.extend([
        route("watch", "red", "fix"),
        route("fix", "capacity", "@failed"),
        Hop {
            kind: HopKind::Terminal {
                state: record.state,
            },
            ..route("fix", "failed", "@failed")
        },
    ]);
    let mut rendered = Vec::new();
    for visits in [0, 1] {
        record.visits.insert(
            "fix".into(),
            rimz::store::workflow::StepVisits {
                total: visits,
                ..Default::default()
            },
        );
        for width in [None, Some(40)] {
            let mut out = Vec::new();
            horizontal(&mut out, &spec, &record, &hops, &[], width, 6).unwrap();
            rendered.push((width, String::from_utf8(out).unwrap()));
        }
    }
    assert!(
        rendered.iter().all(|(_, text)| text.contains("watch")
            && text.contains(&render::paint(render::palette::alarm(), "fix"))
            && text.contains(&render::paint(render::palette::alarm(), "●"))),
        "{rendered:#?}"
    );
    for (width, text) in rendered {
        let text = anstream::adapter::strip_str(&text).to_string();
        assert_eq!(text.matches("fix").count(), 1, "{text}");
        assert!(
            width.is_none_or(|width| text.lines().all(|line| line.width() <= width)),
            "{text}"
        );
    }
}

#[test]
fn horizontal_appends_resumed_and_reached_unreachable_nodes() {
    let spec: WorkflowSpec = toml::from_str(&format!(
        "{LAND}\n[steps.resumed]\nshell='true'\nthen='watch'\n\
         [steps.never_used]\nshell='true'\nthen='watch'\n"
    ))
    .unwrap();
    let machine = super::super::super::machine_config();
    let glyph = rimz::theme::theme_glyphs(&machine.theme);
    let mut rendered = Vec::new();
    for (state, node) in [
        (RunState::Active, "resumed"),
        (RunState::Parked, "resumed"),
        (RunState::Active, "watch"),
    ] {
        let mut record = record(node);
        record.state = state;
        let mut hops = vec![Hop {
            kind: HopKind::Resume,
            ..route("resumed", "resume", "resumed")
        }];
        if node == "watch" {
            hops.push(route("resumed", "ended", "watch"));
        }
        let role = if node == "resumed" {
            super::super::workflow_view::glyph_role(&record)
        } else {
            GlyphRole::PipelinePassed
        };
        let mut out = anstream::StripStream::new(Vec::new());
        horizontal(&mut out, &spec, &record, &hops, &[], Some(40), 6).unwrap();
        rendered.push((
            format!("      {} resumed", glyph(role)),
            String::from_utf8(out.into_inner()).unwrap(),
        ));
    }
    assert!(
        rendered
            .iter()
            .all(|(last, text)| text.lines().last() == Some(last.as_str())
                && text.matches("resumed").count() == 1
                && !text.contains("never_used")
                && text.lines().all(|line| line.width() <= 40)),
        "{rendered:#?}"
    );
}

#[test]
fn horizontal_terminal_marks_only_the_failed_ending_node_alarm() {
    let spec: WorkflowSpec = toml::from_str(LAND).unwrap();
    let actual = [RunState::Done, RunState::Failed].map(|state| {
        let mut record = record("watch");
        record.state = state;
        let mut hops = land_hops()[..2].to_vec();
        hops.push(route(
            "watch",
            state.as_str(),
            if state == RunState::Done {
                "@done"
            } else {
                "@failed"
            },
        ));
        hops.push(Hop {
            kind: HopKind::Terminal { state },
            target: None,
            ..hops.last().unwrap().clone()
        });
        let mut out = Vec::new();
        horizontal(&mut out, &spec, &record, &hops, &[], None, 0).unwrap();
        let text = String::from_utf8(out).unwrap();
        (
            text.matches(&render::paint(render::palette::good(), "◆"))
                .count(),
            text.matches(&render::paint(render::palette::alarm(), "◆"))
                .count(),
        )
    });
    assert_eq!(actual, [(2, 0), (1, 1)]);
}

#[test]
fn limit_routes_use_the_same_direction_marks_as_other_edges() {
    let spec: WorkflowSpec = toml::from_str(
        &LAND
            .replace("timeout = '3d'", "timeout = '3d'\non.timeout = 'fix'")
            .replace("max-visits = 4", "max-visits = 4\non.cap = 'notes'"),
    )
    .unwrap();
    let rail = Rail::definition(&spec);
    let watch = rail.rows.iter().find(|row| row.name == "watch").unwrap();
    let fix = rail.rows.iter().find(|row| row.name == "fix").unwrap();
    assert!(
        watch.heading.contains("waits ≤3d, then → fix"),
        "{}",
        watch.heading
    );
    assert!(
        fix.heading.contains("4 visits, then → notes"),
        "{}",
        fix.heading
    );
}

#[test]
fn wrapped_rows_budget_by_the_wider_prefix() {
    let cases = [
        (
            "├─猫 ",
            "├─猫 abcdefghijklmno\n│   pqrstuvwxyz1234\n│   5\n",
        ),
        ("◆ ", "◆ abcdefghijklmnop\n│   qrstuvwxyz12345\n"),
    ];
    let rendered = cases.map(|(prefix, _)| {
        let first = render::paint(render::palette::good(), prefix);
        let mut out = anstream::StripStream::new(Vec::new());
        write_wrapped(
            &mut out,
            &first,
            "│   ",
            "abcdefghijklmnopqrstuvwxyz12345",
            20,
            render::palette::body(),
        )
        .unwrap();
        String::from_utf8(out.into_inner()).unwrap()
    });
    assert_eq!(rendered, cases.map(|(_, expected)| expected.to_owned()));
}

#[test]
fn multiline_commands_stay_on_one_rail_line() {
    let headings = ["run", "shell"].map(|runner| {
        let spec: WorkflowSpec = toml::from_str(&format!(
            "[workflow]\nscope='worktree'\nstart='gate'\n\
             [checks.gate]\n{runner}=\"\"\"echo first\necho second\"\"\"\non.pass='work'\n\
             [steps.work]\n{runner}=\"\"\"echo first\necho second\"\"\"\nthen='gate'\n"
        ))
        .unwrap();
        Rail::definition(&spec)
            .rows
            .into_iter()
            .map(|row| row.heading)
            .collect::<Vec<_>>()
    });
    assert!(
        headings
            .iter()
            .flatten()
            .all(|heading| heading.contains("echo first; echo second") && !heading.contains('\n')),
        "{headings:?}"
    );
}

#[test]
fn land_rail_agrees_with_validate_and_arrows_follow_the_walk() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("land");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("workflow.toml"), LAND).unwrap();
    let spec = rimz::harness::workflow::load(&dir).unwrap();
    let report = rimz::harness::workflow::validate(&dir, &Default::default(), None);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let mut out = anstream::StripStream::new(Vec::new());
    let rail = Rail::definition(&spec);
    rail.write(&mut out, 140).unwrap();
    let text = String::from_utf8(out.into_inner()).unwrap();
    assert!(text.contains("├─○ fix"), "{text}");
    assert!(!text.contains("├─○ sweep"), "{text}");
    assert!(!text.contains("├─○ triage"), "{text}");
    let footer = defaults(&spec).join("\n");
    let walk = spec.walk();
    for (index, row) in report.rows.iter().enumerate() {
        let mut out = anstream::StripStream::new(Vec::new());
        rail.rows
            .iter()
            .find(|rail| rail.name == row.node)
            .unwrap()
            .write(&mut out, 140)
            .unwrap();
        let text = String::from_utf8(out.into_inner()).unwrap();
        let glyph = if spec.checks.contains_key(&row.node) {
            "◇"
        } else {
            "○"
        };
        assert!(text.contains(&format!("{glyph} {}", row.node)), "{text}");
        for edge in &row.edges {
            match edge.outcome.as_str() {
                "deadline" => assert!(footer.contains("deadline")),
                "invalid" => assert!(footer.contains("cannot run")),
                "unknown" => assert!(text.contains("an unread term above it waits")),
                "timeout" if edge.fires != Fireability::Never => {
                    assert!(text.contains(&format!("waits ≤3d, then {}", edge.target)));
                }
                "cap" if edge.fires != Fireability::Never => {
                    assert!(text.contains(&format!("visits, then {}", edge.target)));
                }
                _ if edge.fires == Fireability::Never => {
                    assert!(!text.contains(&format!("{} → {}", edge.outcome, edge.target)));
                }
                _ => {
                    let position = walk.nodes.iter().position(|node| node.node == edge.target);
                    let target = match position {
                        Some(position) if position <= index => format!("↑ {}", edge.target),
                        Some(_) => format!("→ {}", edge.target),
                        None => edge.target.clone(),
                    };
                    assert!(text.contains(&target), "missing {target}:\n{text}");
                }
            }
        }
    }
    for width in [80, 140] {
        let mut out = anstream::StripStream::new(Vec::new());
        Rail::definition(&spec).write(&mut out, width).unwrap();
        let text = String::from_utf8(out.into_inner()).unwrap();
        assert!(
            text.lines()
                .all(|line| unicode_width::UnicodeWidthStr::width(line) <= width)
        );
    }
}

mod horizontal_sweep {
    use std::collections::{BTreeSet, VecDeque};

    use super::*;

    fn shapes() -> [(&'static str, String); 7] {
        [
            ("land", LAND.into()),
            (
                "branch",
                "[workflow]\nscope='worktree'\nstart='choose'\n\
                 [checks.choose]\nshell='true'\non.a='unit_tests'\n\
                 on.b='integration_tests'\non.c='used'\n\
                 [steps.unit_tests]\nshell='true'\nthen='used'\n\
                 [steps.integration_tests]\nshell='true'\nthen='used'\n\
                 [checks.used]\nshell='true'\non.pass='final'\n\
                 [checks.final]\nshell='true'\non.pass='@done'\non.retry='choose'\n"
                    .into(),
            ),
            (
                "detours",
                "[workflow]\nscope='worktree'\nstart='build'\n\
                 [steps.build]\nshell='true'\nthen='review'\n\
                 [checks.review]\nshell='true'\non.pass='merge'\n\
                 on.fail='address_review_comments'\non.flaky='rerun_flaky_suite'\n\
                 [steps.address_review_comments]\nshell='true'\nthen='review'\nmax-visits=9\n\
                 [steps.rerun_flaky_suite]\nshell='true'\nthen='review'\nmax-visits=9\n\
                 [checks.merge]\nshell='true'\non.pass='@done'\non.fail='@failed'\n"
                    .into(),
            ),
            (
                "unreachable",
                format!(
                    "{LAND}\n[steps.resumed]\nshell='true'\nthen='watch'\n\
                     [steps.never_used]\nshell='true'\nthen='watch'\n"
                ),
            ),
            (
                "deadline",
                LAND.replace(
                    "start = 'begin'",
                    "start = 'begin'\ndeadline = '1d'\non.deadline = 'cleanup'",
                ) + "\n[steps.cleanup]\nshell='true'\nthen='@done'\n",
            ),
            (
                "until",
                "[workflow]\nscope='worktree'\nstart='plan'\n\
                 [steps.plan]\nagent='claude'\nthen='planned'\n\
                 [checks.planned]\nshell='true'\non.pass='work'\n\
                 [steps.work]\nagent='claude'\nlifecycle='until'\nthen='gate'\n\
                 max-visits=3\non.cap='escalate_to_release_manager'\n\
                 [steps.escalate_to_release_manager]\nshell='true'\nthen='gate'\n\
                 [checks.gate]\nshell='true'\non.pass='@done'\non.fail='work'\n"
                    .into(),
            ),
            (
                "long",
                "[workflow]\nscope='worktree'\nstart='collect_release_candidates'\n\
                 [checks.collect_release_candidates]\nshell='true'\n\
                 on.pass='build_signed_release_artifacts'\non.fail='repair_release_candidate_set'\n\
                 [steps.repair_release_candidate_set]\nshell='true'\n\
                 then='collect_release_candidates'\nmax-visits=3\n\
                 [steps.build_signed_release_artifacts]\nshell='true'\n\
                 then='verify_signed_artifacts_match'\n\
                 [checks.verify_signed_artifacts_match]\nshell='true'\n\
                 on.pass='publish_release_to_registry'\non.fail='@failed'\n\
                 [steps.publish_release_to_registry]\nshell='true'\nthen='@done'\n"
                    .into(),
            ),
        ]
    }

    fn shape(name: &str) -> WorkflowSpec {
        let (_, definition) = shapes()
            .into_iter()
            .find(|(shape, _)| *shape == name)
            .unwrap();
        toml::from_str(&definition).unwrap()
    }

    fn path_to(spec: &WorkflowSpec, goal: &str) -> Vec<Hop> {
        let mut previous = BTreeMap::new();
        let mut queue = VecDeque::from([spec.workflow.start.clone()]);
        let mut seen = BTreeSet::from([spec.workflow.start.clone()]);
        while let Some(node) = queue.pop_front() {
            if node == goal {
                break;
            }
            for edge in spec.edges(&node) {
                if edge.fires == Fireability::Never || edge.target.starts_with('@') {
                    continue;
                }
                if seen.insert(edge.target.clone()) {
                    previous.insert(edge.target.clone(), (node.clone(), edge.outcome));
                    queue.push_back(edge.target);
                }
            }
        }
        if !seen.contains(goal) {
            return vec![Hop {
                kind: HopKind::Resume,
                ..route(goal, "resume", goal)
            }];
        }
        let mut hops = Vec::new();
        let mut node = goal;
        while let Some((from, outcome)) = previous.get(node) {
            hops.push(route(from, outcome, node));
            node = from;
        }
        hops.reverse();
        hops
    }

    fn runs(
        spec: &WorkflowSpec,
        node: &str,
        repeated_detours: bool,
    ) -> Vec<(&'static str, RunRecord, Vec<Hop>)> {
        let hops = path_to(spec, node);
        let mut base = record(node);
        for name in std::iter::once(node).chain(
            hops.iter()
                .flat_map(|hop| std::iter::once(hop.node.as_str()).chain(hop.target.as_deref())),
        ) {
            if spec.steps.contains_key(name) {
                base.visits.entry(name.into()).or_default().total = 1;
            }
        }
        if repeated_detours {
            for row in spec.walk().nodes {
                if row.hangs_under.is_some() {
                    base.visits.entry(row.node).or_default().total = 12;
                }
            }
        }
        let mut runs = Vec::new();
        for state in [
            RunState::Active,
            RunState::Parked,
            RunState::Done,
            RunState::Failed,
        ] {
            let mut record = base.clone();
            record.state = state;
            let mut hops = hops.clone();
            if state.is_terminal() {
                let target = if state == RunState::Done {
                    "@done"
                } else {
                    "@failed"
                };
                hops.push(route(node, state.as_str(), target));
                hops.push(Hop {
                    kind: HopKind::Terminal { state },
                    target: None,
                    ..route(node, state.as_str(), target)
                });
            }
            runs.push((state.as_str(), record, hops));
        }
        if let Some(step) = spec.steps.get(node)
            && step.lifecycle == Some(StepLifecycle::Until)
        {
            base.node = step.then.clone().unwrap();
            base.held = Some(
                serde_json::from_value(serde_json::json!({
                    "attempt": {
                        "kind":"agent", "lifecycle":"until", "seq":1, "node":node,
                        "started_at":jiff::Timestamp::UNIX_EPOCH, "host":{"pid":1, "start":null},
                    },
                    "entered_at":jiff::Timestamp::UNIX_EPOCH,
                }))
                .unwrap(),
            );
            runs.push(("held", base, hops));
        }
        runs
    }

    fn position_marks(spec: &WorkflowSpec, record: &RunRecord) -> Vec<(String, String)> {
        let machine = super::super::super::super::machine_config();
        let glyph = rimz::theme::theme_glyphs(&machine.theme);
        std::iter::once(record.node.as_str())
            .chain(record.held.as_ref().map(|held| held.attempt.node.as_str()))
            .map(|name| {
                let role = if record.state.is_terminal() {
                    if spec.checks.contains_key(name) {
                        GlyphRole::PipelineCheckPassed
                    } else {
                        GlyphRole::PipelinePassed
                    }
                } else {
                    super::super::super::workflow_view::glyph_role(record)
                };
                (name.into(), format!("{} {name}", glyph(role)))
            })
            .collect()
    }

    fn position_floor(
        spec: &WorkflowSpec,
        record: &RunRecord,
        hops: &[Hop],
        marks: &[(String, String)],
    ) -> Vec<String> {
        let walk = spec.walk();
        let anchors = walk
            .nodes
            .iter()
            .filter(|row| marks.iter().any(|(name, _)| *name == row.node))
            .map(|row| row.hangs_under.as_ref().unwrap_or(&row.node))
            .collect::<BTreeSet<_>>();
        let main = walk
            .nodes
            .iter()
            .filter(|row| row.hangs_under.is_none())
            .collect::<Vec<_>>();
        let counts = reached(hops, &[]);
        let machine = super::super::super::super::machine_config();
        let glyph = rimz::theme::theme_glyphs(&machine.theme);
        let mut floor = Vec::new();
        let mut line = "      ".to_owned();
        let mut previous = None;
        for (index, row) in main
            .iter()
            .enumerate()
            .filter(|(_, row)| anchors.contains(&row.node))
        {
            if previous.is_none() && index > 0 {
                line.push_str("… ");
            }
            if let Some(previous) = previous {
                line.push_str(if index > previous + 1 {
                    " ─ … ─ "
                } else {
                    " ─ "
                });
            }
            let padding = line.width();
            if let Some((_, mark)) = marks.iter().find(|(name, _)| *name == row.node) {
                line.push_str(mark);
            } else {
                let role = if counts.contains_key(row.node.as_str()) {
                    GlyphRole::PipelineCheckPassed
                } else {
                    GlyphRole::PipelineCheckFuture
                };
                line.push_str(&format!("{} {}", glyph(role), row.node));
            }
            for (name, mark) in marks {
                if !walk
                    .nodes
                    .iter()
                    .any(|hang| hang.node == *name && hang.hangs_under.as_ref() == Some(&row.node))
                {
                    continue;
                }
                let visits = record.visits.get(name).map_or(0, |visits| visits.total);
                let repeat = if visits > 1 {
                    format!(" ×{visits}")
                } else {
                    String::new()
                };
                floor.push(format!("{:padding$}╰─ {mark}{repeat} ↑", ""));
            }
            previous = Some(index);
        }
        if !anchors.is_empty() {
            floor.push(line);
        }
        for (name, mark) in marks {
            if walk.unreachable.contains(name) {
                floor.push(format!("      {mark}"));
            }
        }
        floor
    }

    fn rendered(spec: &WorkflowSpec, record: &RunRecord, hops: &[Hop], width: usize) -> String {
        let mut out = Vec::new();
        horizontal(&mut out, spec, record, hops, &[], Some(width), 6).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn check_case(
        spec: &WorkflowSpec,
        record: &RunRecord,
        hops: &[Hop],
        width: usize,
    ) -> (bool, Vec<String>) {
        let colored = rendered(spec, record, hops, width);
        let text = anstream::adapter::strip_str(&colored).to_string();
        let marks = position_marks(spec, record);
        let floor = position_floor(spec, record, hops, &marks);
        let mut failures = Vec::new();
        let mut at_floor = false;
        for line in text.lines().filter(|line| line.width() > width) {
            if floor.iter().any(|minimum| minimum == line) {
                at_floor = true;
            } else {
                failures.push(format!("width {} > {width}: {line:?}", line.width()));
            }
        }
        for (name, mark) in marks {
            let count = text.matches(&mark).count();
            if count != 1 {
                failures.push(format!(
                    "position mark {mark:?}: {count} occurrences\n{text}"
                ));
            }
            if record.state.is_terminal() {
                let style = if record.state == RunState::Done {
                    render::palette::good()
                } else {
                    render::palette::alarm()
                };
                let (glyph, _) = mark.split_once(' ').unwrap();
                let painted = format!(
                    "{} {}",
                    render::paint(style, glyph),
                    render::paint(style, &name)
                );
                let count = colored.matches(&painted).count();
                if count != 1 {
                    failures.push(format!(
                        "ending mark {painted:?}: {count} occurrences\n{text}"
                    ));
                }
            }
        }
        (at_floor, failures)
    }

    #[test]
    fn held_run_elides_interior_nodes_between_current_anchors() {
        let spec = shape("until");
        let (_, record, hops) = runs(&spec, "work", false)
            .into_iter()
            .find(|(state, _, _)| *state == "held")
            .unwrap();
        let walk = spec
            .walk()
            .nodes
            .into_iter()
            .map(|row| row.node)
            .collect::<Vec<_>>();
        assert_eq!(
            anstream::adapter::strip_str(&rendered(&spec, &record, &hops, 40)).to_string(),
            "      … ◆ planned ─ ⢿ work ─ … ─ ⢿ gate\n",
            "until walk: {walk:?}"
        );
    }

    #[test]
    fn detour_elision_restores_fitting_main_track_context() {
        let spec = shape("detours");
        let (_, record, hops) = runs(&spec, "review", true)
            .into_iter()
            .find(|(state, _, _)| *state == "active")
            .unwrap();
        assert_eq!(
            anstream::adapter::strip_str(&rendered(&spec, &record, &hops, 40)).to_string(),
            "      ● build ─ ⢿ review ─ ◇ merge\n"
        );
    }

    #[test]
    fn fits_width_and_keeps_position_across_shapes() {
        let mut failures = Vec::new();
        let mut cases = 0;
        let mut long_floor_cases = 0;
        for (shape, definition) in shapes() {
            let spec: WorkflowSpec = toml::from_str(&definition).unwrap();
            for node in spec.checks.keys().chain(spec.steps.keys()) {
                for repeated_detours in [false, true] {
                    for (state, record, hops) in runs(&spec, node, repeated_detours) {
                        for width in [40, 60, 80] {
                            cases += 1;
                            let (at_floor, errors) = check_case(&spec, &record, &hops, width);
                            if shape == "long" && width == 40 && at_floor {
                                long_floor_cases += 1;
                            }
                            for error in errors {
                                failures.push(format!(
                                    "{shape}/{node}/{state}/repeated={repeated_detours}/w{width}: \
                                     {error}"
                                ));
                            }
                        }
                    }
                }
            }
        }
        if long_floor_cases == 0 {
            failures.push("long names at width 40 never exercised the position floor".into());
        }
        assert!(
            failures.is_empty(),
            "{cases} cases, {} failures, {long_floor_cases} long-name floor cases:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}
