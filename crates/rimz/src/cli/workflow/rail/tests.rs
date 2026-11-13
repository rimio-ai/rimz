use super::*;
use rimz::harness::workflow::HopKind;
use std::collections::BTreeMap;

const LAND: &str = r#"
[workflow]
on-break = '@failed'
scope = 'worktree'
start = 'begin'
[checks.begin]
shell = 'true'
on.pass = 'sweep'
on.fail = '@failed'
[steps.sweep]
shell = 'echo sweep'
timeout = '1h'
then = 'watch'
[checks.watch]
cases = [{ when = ['ci=passed'], outcome = 'green' }]
else = 'red'
on.green = 'notes'
on.red = 'fix'
on.unknown = '@wait'
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
        ("agent='claude'\nlifecycle='turn'", true),
        ("agent='claude'", false),
    ] {
        let spec: Definition = toml::from_str(&format!(
            "[workflow]\nscope='worktree'\nstart='gate'\n\
             [checks.gate]\nshell='true'\non.pass='work'\n\
             [steps.work]\nthen='gate'\n{runner}\n"
        ))
        .unwrap();
        assert_eq!(
            defaults(&spec).contains(&"A turn step goes on to its next check when its turn ends cleanly or its provider exits."),
            turn_end,
            "{runner}"
        );
    }
}

fn record(node: &str) -> RunRecord {
    serde_json::from_value(serde_json::json!({
        "version":rimz::store::workflow::VERSION, "id":rimz::ids::WorkflowRunId::new(), "workflow":"land",
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
    let spec: Definition = toml::from_str(LAND).unwrap();
    let mut out = anstream::StripStream::new(Vec::new());
    horizontal(&mut out, &spec, record, hops, &[], width, 6).unwrap();
    String::from_utf8(out.into_inner()).unwrap()
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
            GlyphRole::WorkflowHeld,
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
        for attention in [GlyphRole::StatusWaiting, GlyphRole::StatusPaused] {
            assert_ne!(glyph(GlyphRole::WorkflowHeld), glyph(attention), "{set}");
        }
    }
}

#[test]
fn parks_and_needs_fix_holds_use_the_held_mark_and_warning_color() {
    let spec: Definition = toml::from_str(LAND).unwrap();
    let machine = super::super::super::machine_config();
    let glyph = rimz::theme::theme_glyphs(&machine.theme)(GlyphRole::WorkflowHeld);
    for hold in [None, Some("check_failed"), Some("disk"), Some("store")] {
        let mut record = record("sweep");
        if let Some(hold) = hold {
            record.wait = Some(
                serde_json::from_value(serde_json::json!({
                    "node":"sweep", "outcome":"capacity", "hold":hold,
                    "first":jiff::Timestamp::UNIX_EPOCH,
                    "last":jiff::Timestamp::UNIX_EPOCH, "count":1,
                }))
                .unwrap(),
            );
        } else {
            record.state = RunState::Parked;
        }
        let mut out = Vec::new();
        horizontal(&mut out, &spec, &record, &land_hops()[..1], &[], None, 0).unwrap();
        let text = String::from_utf8(out).unwrap();
        let style = render::status::workflow(record.state, true).bold();
        assert!(
            text.contains(&format!(
                "{} {}",
                render::paint(style, &glyph),
                render::paint(style, "sweep")
            )),
            "{hold:?}: {text}"
        );
    }
}

#[test]
fn horizontal_detours_hang_under_their_parent_and_follow_width_elision() {
    let mut record = record("triage");
    record.visits.insert("fix".into(), 2);
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
    let spec: Definition = toml::from_str(
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
    let spec: Definition = toml::from_str(LAND).unwrap();
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
        record.visits.insert("fix".into(), visits);
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
    let spec: Definition = toml::from_str(&format!(
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
    let spec: Definition = toml::from_str(LAND).unwrap();
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

fn canceled_horizontal(runner: &str, stopping: &str, journal: &str) -> String {
    let spec: Definition = toml::from_str(&format!(
        "[workflow]\nscope='worktree'\nstart='begin'\n\
         [checks.begin]\nshell='true'\non.pass='work'\non.fail='@failed'\n\
         {runner}[checks.after]\nshell='touch \"$RIMZ_WORKFLOW_DIR/ready\"; sleep 30'\n\
         on.pass='@done'\non.fail='@failed'\n"
    ))
    .unwrap();
    let journal: Vec<JournalEntry> = serde_json::from_str(journal).unwrap();
    let hops = rimz::harness::workflow::hops(&spec, &journal, None);
    let mut record = record(stopping);
    record.state = RunState::Canceled;
    let mut out = Vec::new();
    horizontal(&mut out, &spec, &record, &hops, &journal, None, 0).unwrap();
    let text = String::from_utf8(out).unwrap();
    let good = render::palette::good();
    assert_eq!(
        text.matches(&render::paint(good, stopping)).count(),
        1,
        "{text}"
    );
    assert_eq!(
        spec.checks()
            .chain(spec.steps())
            .map(|(name, _)| text.matches(&render::paint(good, name)).count())
            .sum::<usize>(),
        1,
        "{text}"
    );
    if stopping != "begin" {
        assert_eq!(
            text.matches(&render::paint(render::palette::body(), "begin"))
                .count(),
            1,
            "{text}"
        );
    }
    text
}

// Engine journals captured with validated specs via workflow advance/cancel.

#[test]
fn horizontal_canceled_check_origin_wait_route_keeps_the_stopping_node_ending() {
    // Pin the Check-origin @wait route, whose source already marked the ending before the fix.
    canceled_horizontal(
        r#"[checks.work]
shell='false'
timeout='1h'
on.pass='@done'
on.fail='@wait'
"#,
        "work",
        r#"[
  {
    "at": "2026-11-04T14:21:58.815763075Z",
    "cost_usd": null,
    "digests": {},
    "ended_at": "2026-11-04T14:21:58.815763075Z",
    "event": "evaluation",
    "node": "begin",
    "object": {
      "code": 0,
      "log": "/tmp/rimz-sandbox-LAxPrN/tmp/.tmp6TI2EX/ws/tmp6TI2EX-ba2d/owned/workflows/wf_01a1a6f0268f71c3a6b283068bf37273/work/1.check.log",
      "outcome": "pass"
    },
    "origin": "check",
    "outcome": "pass",
    "retry": null,
    "seq": 1,
    "started_at": "2026-11-04T14:21:58.811620071Z"
  },
  {
    "at": "2026-11-04T14:21:58.823464743Z",
    "cost_usd": null,
    "digests": {},
    "ended_at": "2026-11-04T14:21:58.823464743Z",
    "event": "evaluation",
    "node": "work",
    "object": {
      "code": 1,
      "log": "/tmp/rimz-sandbox-LAxPrN/tmp/.tmp6TI2EX/ws/tmp6TI2EX-ba2d/owned/workflows/wf_01a1a6f0268f71c3a6b283068bf37273/work/2.check.log",
      "outcome": "fail",
      "reason": "exited 1"
    },
    "origin": "check",
    "outcome": "fail",
    "retry": null,
    "seq": 2,
    "started_at": "2026-11-04T14:21:58.818628513Z"
  },
  {
    "at": "2026-11-04T14:21:58.825484448Z",
    "count": 1,
    "detail": null,
    "event": "wait",
    "first": "2026-11-04T14:21:58.823464743Z",
    "last": "2026-11-04T14:21:58.823464743Z",
    "node": "work",
    "outcome": "fail",
    "recheck_at": null,
    "seq": null
  },
  {
    "at": "2026-11-04T14:21:58.825484448Z",
    "event": "cancel",
    "node": "work",
    "seq": null
  },
  {
    "at": "2026-11-04T14:21:58.825484448Z",
    "event": "terminal",
    "node": "work",
    "seq": null,
    "state": "canceled"
  }
]"#,
    );
}

#[test]
fn horizontal_canceled_limited_judge_wait_marks_the_stopping_node() {
    // Cancel a limited judge after its Engine-origin evaluation, before the scheduled recheck.
    canceled_horizontal(
        r#"[checks.work]
agent='claude'
summary='Review the work'
prompt-file='prompt.md'
timeout='1h'
on.pass='@done'
on.fail='@failed'
"#,
        "work",
        r#"[
  {
    "at": "2026-11-04T15:13:38.534197849Z",
    "cost_usd": null,
    "digests": {},
    "ended_at": "2026-11-04T15:13:38.534197849Z",
    "event": "evaluation",
    "node": "begin",
    "object": {
      "code": 0,
      "log": "/tmp/rimz-sandbox-Ml9p1z/tmp/rimz-test-home-oMUQOu/rimz-home/ws/project-09a3/owned/workflows/wf_01a1a71f72bd77248052b2b09f6ddd41/work/1.check.log",
      "outcome": "pass"
    },
    "origin": "check",
    "outcome": "pass",
    "retry": null,
    "seq": 1,
    "started_at": "2026-11-04T15:13:38.529570803Z"
  },
  {
    "at": "2026-11-04T15:13:38.548114847Z",
    "cost_usd": null,
    "digests": {
      "prompt.md": "5e383207c65159ba379ca0ca2a83d23f2d145626b4d9527af53b1a5441151549"
    },
    "ended_at": "2026-11-04T15:13:38.548114847Z",
    "event": "evaluation",
    "node": "work",
    "object": {
      "outcome": "limited",
      "reason": "Claude check failed: quota exhausted"
    },
    "origin": "engine",
    "outcome": "limited",
    "retry": null,
    "seq": 2,
    "started_at": "2026-11-04T15:13:38.537922564Z",
    "stderr_tail": ""
  },
  {
    "at": "2026-11-04T15:13:38.572010164Z",
    "event": "cancel",
    "node": "work",
    "seq": null
  },
  {
    "at": "2026-11-04T15:13:38.572010164Z",
    "event": "terminal",
    "node": "work",
    "seq": null,
    "state": "canceled"
  }
]"#,
    );
}

#[test]
fn horizontal_canceled_live_check_marks_the_stopping_node() {
    // Cancel a shell check while its command is still running.
    canceled_horizontal(
        r#"[checks.work]
shell='touch "$RIMZ_WORKFLOW_DIR/ready"; sleep 30'
on.pass='@done'
on.fail='@failed'
"#,
        "work",
        r#"[
  {
    "at": "2026-11-04T14:21:58.836089083Z",
    "cost_usd": null,
    "digests": {},
    "ended_at": "2026-11-04T14:21:58.836089083Z",
    "event": "evaluation",
    "node": "begin",
    "object": {
      "code": 0,
      "log": "/tmp/rimz-sandbox-LAxPrN/tmp/.tmpBYyJ2n/ws/tmpBYyJ2n-79e7/owned/workflows/wf_01a1a6f026ac767c94bd8957b5f30a4d/work/1.check.log",
      "outcome": "pass"
    },
    "origin": "check",
    "outcome": "pass",
    "retry": null,
    "seq": 1,
    "started_at": "2026-11-04T14:21:58.832228802Z"
  },
  {
    "at": "2026-11-04T14:21:58.852502271Z",
    "cost_usd": null,
    "digests": {},
    "ended_at": "2026-11-04T14:21:58.852502271Z",
    "event": "evaluation",
    "node": "work",
    "object": null,
    "origin": "engine",
    "outcome": "canceled",
    "retry": null,
    "seq": 2,
    "started_at": "2026-11-04T14:21:58.838869411Z"
  },
  {
    "at": "2026-11-04T14:21:58.852502271Z",
    "event": "cancel",
    "node": "work",
    "seq": null
  },
  {
    "at": "2026-11-04T14:21:58.852502271Z",
    "event": "terminal",
    "node": "work",
    "seq": null,
    "state": "canceled"
  }
]"#,
    );
}

#[test]
fn horizontal_canceled_command_step_marks_the_stopping_node() {
    // Cancel a command step while its shell command is still running.
    canceled_horizontal(
        r#"[steps.work]
shell='touch "$RIMZ_WORKFLOW_DIR/ready"; sleep 30'
timeout='1h'
then='after'
"#,
        "work",
        r#"[
  {
    "at": "2026-11-04T14:21:58.872297509Z",
    "cost_usd": null,
    "digests": {},
    "ended_at": "2026-11-04T14:21:58.872297509Z",
    "event": "evaluation",
    "node": "begin",
    "object": {
      "code": 0,
      "log": "/tmp/rimz-sandbox-LAxPrN/tmp/.tmpYYLr3X/ws/tmpYYLr3X-3956/owned/workflows/wf_01a1a6f026d074c1b9ddd53a7cae67b2/work/1.check.log",
      "outcome": "pass"
    },
    "origin": "check",
    "outcome": "pass",
    "retry": null,
    "seq": 1,
    "started_at": "2026-11-04T14:21:58.868196865Z"
  },
  {
    "asked": null,
    "at": "2026-11-04T14:21:58.888306566Z",
    "children": null,
    "code": null,
    "cost_usd": null,
    "digests": {},
    "ended_at": "2026-11-04T14:21:58.888306566Z",
    "event": "attempt",
    "handle": null,
    "head_end": null,
    "head_start": null,
    "key": null,
    "limited": null,
    "log": "/tmp/rimz-sandbox-LAxPrN/tmp/.tmpYYLr3X/ws/tmpYYLr3X-3956/owned/workflows/wf_01a1a6f026d074c1b9ddd53a7cae67b2/work/2.step.log",
    "node": "work",
    "outcome": "canceled",
    "reason": null,
    "routed_by": "pass",
    "run_id": null,
    "seq": 2,
    "started_at": "2026-11-04T14:21:58.875708234Z",
    "visit": 1
  },
  {
    "at": "2026-11-04T14:21:58.888306566Z",
    "event": "cancel",
    "node": "work",
    "seq": null
  },
  {
    "at": "2026-11-04T14:21:58.888306566Z",
    "event": "terminal",
    "node": "work",
    "seq": null,
    "state": "canceled"
  }
]"#,
    );
}

#[test]
fn horizontal_canceled_agent_turn_leaves_its_then_check_unreached() {
    // Cancel an agent turn before it reaches its then check.
    let text = canceled_horizontal(
        r#"[steps.work]
agent='claude'
prompt-file='prompt.md'
lifecycle='turn'
then='after'
"#,
        "work",
        r#"[
  {
    "at": "2026-11-04T14:21:58.91100852Z",
    "cost_usd": null,
    "digests": {},
    "ended_at": "2026-11-04T14:21:58.91100852Z",
    "event": "evaluation",
    "node": "begin",
    "object": {
      "code": 0,
      "log": "/tmp/rimz-sandbox-LAxPrN/tmp/.tmpC3D9TY/ws/tmpC3D9TY-b2dc/owned/workflows/wf_01a1a6f026f6745599bb3c21211484cc/work/1.check.log",
      "outcome": "pass"
    },
    "origin": "check",
    "outcome": "pass",
    "retry": null,
    "seq": 1,
    "started_at": "2026-11-04T14:21:58.906727826Z"
  },
  {
    "asked": null,
    "at": "2026-11-04T14:21:58.921845904Z",
    "children": {
      "live": [],
      "stopped": []
    },
    "code": null,
    "cost_usd": 0.25,
    "digests": {
      "prompt": "84893a778be73583c7aef377b6ed232cccefb47cb89ea9052a636c9532fbbf7f",
      "prompt.md": "5e383207c65159ba379ca0ca2a83d23f2d145626b4d9527af53b1a5441151549"
    },
    "ended_at": "2026-11-04T14:21:58.921845904Z",
    "event": "attempt",
    "handle": "@builder",
    "head_end": null,
    "head_start": null,
    "key": null,
    "limited": null,
    "log": null,
    "node": "work",
    "outcome": "canceled",
    "reason": null,
    "routed_by": "pass",
    "run_id": "run_01a1a6f0270572cf930fde00db983047",
    "seq": 2,
    "started_at": "2026-11-04T14:21:58.914426097Z",
    "visit": 1
  },
  {
    "at": "2026-11-04T14:21:58.921845904Z",
    "event": "cancel",
    "node": "work",
    "seq": null
  },
  {
    "at": "2026-11-04T14:21:58.921845904Z",
    "event": "terminal",
    "node": "work",
    "seq": null,
    "state": "canceled"
  }
]"#,
    );
    assert!(
        text.contains(&format!(
            "{} {}",
            render::paint(render::palette::muted(), "◇"),
            render::paint(render::palette::muted(), "after")
        )),
        "{text}"
    );
}

#[test]
fn horizontal_canceled_held_session_marks_the_step_not_the_check() {
    // Cancel an until session while its held shell check is still running.
    let text = canceled_horizontal(
        r#"[steps.work]
agent='claude'
prompt-file='prompt.md'
lifecycle='until'
then='after'
"#,
        "work",
        r#"[
  {
    "at": "2026-11-04T14:21:58.934775359Z",
    "cost_usd": null,
    "digests": {},
    "ended_at": "2026-11-04T14:21:58.934775359Z",
    "event": "evaluation",
    "node": "begin",
    "object": {
      "code": 0,
      "log": "/tmp/rimz-sandbox-LAxPrN/tmp/.tmpeNny6t/ws/tmpeNny6t-81ee/owned/workflows/wf_01a1a6f0270d73cf894522b647026452/work/1.check.log",
      "outcome": "pass"
    },
    "origin": "check",
    "outcome": "pass",
    "retry": null,
    "seq": 1,
    "started_at": "2026-11-04T14:21:58.930000785Z"
  },
  {
    "at": "2026-11-04T14:21:58.952775065Z",
    "cost_usd": null,
    "digests": {},
    "ended_at": "2026-11-04T14:21:58.952775065Z",
    "event": "evaluation",
    "node": "after",
    "object": null,
    "origin": "engine",
    "outcome": "canceled",
    "retry": null,
    "seq": 3,
    "started_at": "2026-11-04T14:21:58.94404983Z"
  },
  {
    "action": "interrupted",
    "at": "2026-11-04T14:21:58.952775065Z",
    "check": "after",
    "event": "turn_end",
    "node": "work",
    "reason": "cancel",
    "seq": 2,
    "turn": {
      "agent_id": "leader",
      "ended_at": "1970-01-01T00:00:01Z",
      "started_at": "1970-01-01T00:00:00Z"
    }
  },
  {
    "asked": null,
    "at": "2026-11-04T14:21:58.955931461Z",
    "children": {
      "live": [],
      "stopped": []
    },
    "code": null,
    "cost_usd": 0.25,
    "digests": {
      "prompt": "84893a778be73583c7aef377b6ed232cccefb47cb89ea9052a636c9532fbbf7f",
      "prompt.md": "5e383207c65159ba379ca0ca2a83d23f2d145626b4d9527af53b1a5441151549"
    },
    "ended_at": "2026-11-04T14:21:58.955931461Z",
    "event": "attempt",
    "handle": "@builder",
    "head_end": null,
    "head_start": null,
    "key": null,
    "limited": null,
    "log": null,
    "node": "work",
    "outcome": "canceled",
    "reason": null,
    "routed_by": "pass",
    "run_id": "run_01a1a6f0271d716ab6de92cce312c1d1",
    "seq": 2,
    "started_at": "2026-11-04T14:21:58.938355343Z",
    "visit": 1
  },
  {
    "at": "2026-11-04T14:21:58.955931461Z",
    "event": "cancel",
    "node": "work",
    "seq": null
  },
  {
    "at": "2026-11-04T14:21:58.955931461Z",
    "event": "terminal",
    "node": "work",
    "seq": null,
    "state": "canceled"
  }
]"#,
    );
    assert_eq!(
        text.matches(&render::paint(render::palette::body(), "after"))
            .count(),
        1,
        "{text}"
    );
}

#[test]
fn horizontal_canceled_start_marks_an_ending_without_a_route() {
    // Cancel the start node before the engine takes any route.
    let text = canceled_horizontal(
        r#"[checks.work]
shell='false'
timeout='1h'
on.pass='@done'
on.fail='@wait'
"#,
        "begin",
        r#"[
  {
    "at": "2026-11-04T14:21:58.973153462Z",
    "event": "cancel",
    "node": "begin",
    "seq": null
  },
  {
    "at": "2026-11-04T14:21:58.973153462Z",
    "event": "terminal",
    "node": "begin",
    "seq": null,
    "state": "canceled"
  }
]"#,
    );
    assert_eq!(
        text.matches(&render::paint(render::palette::muted(), "work"))
            .count(),
        1,
        "{text}"
    );
}

#[test]
fn horizontal_canceled_parked_run_keeps_the_stopping_node_ending() {
    // Cancel a run already parked by a check routing to @user.
    canceled_horizontal(
        r#"[checks.work]
shell='true'
on.pass='@user'
on.fail='@failed'
"#,
        "work",
        r#"[
  {
    "at": "2026-11-04T14:21:58.983609839Z",
    "cost_usd": null,
    "digests": {},
    "ended_at": "2026-11-04T14:21:58.983609839Z",
    "event": "evaluation",
    "node": "begin",
    "object": {
      "code": 0,
      "log": "/tmp/rimz-sandbox-LAxPrN/tmp/.tmpFoUgj5/ws/tmpFoUgj5-2000/owned/workflows/wf_01a1a6f02740702d8743c7aa8bc87c21/work/1.check.log",
      "outcome": "pass"
    },
    "origin": "check",
    "outcome": "pass",
    "retry": null,
    "seq": 1,
    "started_at": "2026-11-04T14:21:58.979762993Z"
  },
  {
    "at": "2026-11-04T14:21:58.989624128Z",
    "cost_usd": null,
    "digests": {},
    "ended_at": "2026-11-04T14:21:58.989624128Z",
    "event": "evaluation",
    "node": "work",
    "object": {
      "code": 0,
      "log": "/tmp/rimz-sandbox-LAxPrN/tmp/.tmpFoUgj5/ws/tmpFoUgj5-2000/owned/workflows/wf_01a1a6f02740702d8743c7aa8bc87c21/work/2.check.log",
      "outcome": "pass"
    },
    "origin": "check",
    "outcome": "pass",
    "retry": null,
    "seq": 2,
    "started_at": "2026-11-04T14:21:58.985839259Z"
  },
  {
    "at": "2026-11-04T14:21:58.989624128Z",
    "event": "park",
    "node": "work",
    "reason": "user",
    "seq": null,
    "text": null
  },
  {
    "at": "2026-11-04T14:21:58.992042223Z",
    "event": "cancel",
    "node": "work",
    "seq": null
  },
  {
    "at": "2026-11-04T14:21:58.992042223Z",
    "event": "terminal",
    "node": "work",
    "seq": null,
    "state": "canceled"
  }
]"#,
    );
}

#[test]
fn limit_routes_use_the_same_direction_marks_as_other_edges() {
    let spec: Definition = toml::from_str(
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
        let spec: Definition = toml::from_str(&format!(
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
    let told = LAND.replace("on.pass = 'triage'\non.fail = '@done'\n[steps.triage]", "on.pass = 'tell'\non.fail = 'stand'\n[steps.tell]\nmessage = '@planner'\nthen = 'triage'\n[steps.stand]\nagent = 'claude'\nlifecycle = 'stay'\nprompt-file = 'prompt.md'\nsubscribe = ['ci.failed']\nthen = 'ruled'\n[steps.triage]");
    assert_ne!(told, LAND);
    for text in [LAND, &told] {
        land_rail_agrees(text);
    }
}

fn land_rail_agrees(text: &str) {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("land");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("workflow.toml"), text).unwrap();
    std::fs::write(dir.join("prompt.md"), "Stand by").unwrap();
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
        let glyph = if spec.check(&row.node).is_some() {
            "◇"
        } else {
            "○"
        };
        assert!(text.contains(&format!("{glyph} {}", row.node)), "{text}");
        if let Some(target) = &row.message {
            assert!(text.contains(&format!("step  {target}")), "{text}");
        }
        for edge in &row.edges {
            match edge.outcome.as_str() {
                "deadline" => assert_eq!(edge.source, EdgeSource::OnBreak),
                "invalid" => assert!(text.contains("invalid @failed"), "{text}"),
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
                    let position = walk
                        .nodes
                        .iter()
                        .position(|node| edge.target == node.node.as_str());
                    let target = match position {
                        Some(position) if position <= index => format!("↑ {}", edge.target),
                        Some(_) => format!("→ {}", edge.target),
                        None => edge.target.to_string(),
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
                 [steps.plan]\nagent='claude'\nlifecycle='turn'\nthen='planned'\n\
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

    fn shape(name: &str) -> Definition {
        let (_, definition) = shapes()
            .into_iter()
            .find(|(shape, _)| *shape == name)
            .unwrap();
        toml::from_str(&definition).unwrap()
    }

    fn path_to(spec: &Definition, goal: &str) -> Vec<Hop> {
        let mut previous = BTreeMap::new();
        let mut queue = VecDeque::from([spec.start().to_owned()]);
        let mut seen = BTreeSet::from([spec.start().to_owned()]);
        while let Some(node) = queue.pop_front() {
            if node == goal {
                break;
            }
            for edge in spec.edges(&node) {
                let target = edge.target.to_string();
                if edge.fires == Fireability::Never || target.starts_with('@') {
                    continue;
                }
                if seen.insert(target.clone()) {
                    previous.insert(target.clone(), (node.clone(), edge.outcome));
                    queue.push_back(target);
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
        spec: &Definition,
        node: &str,
        repeated_detours: bool,
    ) -> Vec<(&'static str, RunRecord, Vec<Hop>)> {
        let hops = path_to(spec, node);
        let mut base = record(node);
        for name in std::iter::once(node).chain(hops.iter().flat_map(|hop| {
            std::iter::once(hop.node.as_str())
                .chain(hop.target.as_ref().map(|target| target.as_str()))
        })) {
            if spec.step(name).is_some() {
                base.visits.insert(name.into(), 1);
            }
        }
        if repeated_detours {
            for row in spec.walk().nodes {
                if row.hangs_under.is_some() {
                    *base.visits.entry(row.node).or_default() = 12;
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
        if let Some(step) = spec.step(node)
            && step.lifecycle() == StepLifecycle::Until
        {
            base.node = step.then().first().unwrap().as_str().to_owned();
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

    fn position_marks(spec: &Definition, record: &RunRecord) -> Vec<(String, String)> {
        let machine = super::super::super::super::machine_config();
        let glyph = rimz::theme::theme_glyphs(&machine.theme);
        std::iter::once(record.node.as_str())
            .chain(record.held.as_ref().map(|held| held.attempt.node.as_str()))
            .map(|name| {
                let role = if record.state.is_terminal() {
                    if spec.check(name).is_some() {
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
        spec: &Definition,
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
                let visits = record.visits.get(name).map_or(0, |visits| *visits);
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

    fn rendered(spec: &Definition, record: &RunRecord, hops: &[Hop], width: usize) -> String {
        let mut out = Vec::new();
        horizontal(&mut out, spec, record, hops, &[], Some(width), 6).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn check_case(
        spec: &Definition,
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
            let spec: Definition = toml::from_str(&definition).unwrap();
            for node in spec.checks().chain(spec.steps()).map(|(name, _)| name) {
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

#[test]
fn a_sleep_rail_prints_its_spelling_and_points_then_along_the_walk() {
    let spec: Definition = toml::from_str(
        "[workflow]\nscope='worktree'\nstart='poll'\n\
         [checks.poll]\nshell='true'\non={pass='@done',fail='cooldown'}\n\
         [checks.cooldown]\nsleep='30m'\nwhen=['ci=passed']\nfor='2m'\nthen='poll'\n",
    )
    .unwrap();
    let mut out = anstream::StripStream::new(Vec::new());
    Rail::definition(&spec).write(&mut out, 140).unwrap();
    let text = String::from_utf8(out.into_inner()).unwrap();
    for line in [
        "cooldown  check  sleep 30m",
        "when ci=passed",
        "for 2m",
        "then ↑ poll",
    ] {
        assert_eq!(text.matches(line).count(), 1, "{line}: {text}");
    }
    assert!(
        !text.contains("sleeping") && !text.contains("unknown"),
        "{text}"
    );
}
