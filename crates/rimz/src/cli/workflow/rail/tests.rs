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
