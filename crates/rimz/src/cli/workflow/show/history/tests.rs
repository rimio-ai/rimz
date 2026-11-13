use std::collections::BTreeMap;

use super::*;
use jiff::Timestamp;
use records::{EvaluationOrigin, JournalEntry, JournalEvent};
use rimz::store::workflow::Children;
use serde_json::json;

fn at(minute: i64) -> Timestamp {
    "2026-10-30T19:00:00Z"
        .parse::<Timestamp>()
        .unwrap()
        .checked_add(jiff::SignedDuration::from_secs(minute * 60))
        .unwrap()
}

fn evaluation(seq: u64, minute: i64, outcome: &str, case: Option<u32>) -> JournalEntry {
    let mut object = json!({"outcome":outcome, "reason":format!("answer {seq}")});
    if let Some(case) = case {
        object["case"] = json!(case);
    }
    JournalEntry {
        seq: Some(seq),
        at: at(minute),
        node: "pr".into(),
        event: JournalEvent::Evaluation {
            origin: EvaluationOrigin::Check,
            outcome: outcome.into(),
            object,
            digests: BTreeMap::new(),
            started_at: at(minute),
            ended_at: at(minute),
            stderr_tail: None,
            cost_usd: None,
            retry: None,
        },
    }
}

fn wake(minute: i64, signal: &str) -> JournalEntry {
    JournalEntry {
        seq: None,
        at: at(minute),
        node: "pr".into(),
        event: JournalEvent::Wake {
            signal: signal.into(),
        },
    }
}

fn wait(minute: i64, outcome: &str, first: i64, last: i64, count: u64) -> JournalEntry {
    JournalEntry {
        seq: None,
        at: at(minute),
        node: "pr".into(),
        event: JournalEvent::Wait {
            outcome: outcome.into(),
            detail: None,
            hold: None,
            first: at(first),
            last: at(last),
            count,
            recheck_at: None,
        },
    }
}

fn history(journal: &[JournalEntry]) -> String {
    history_of(
        "[workflow]\nscope='worktree'\nstart='pr'\n\
         [script.pr]\nrun='sh checks/pr.sh'\non.pending='@wait'\non.done='@done'\n\
         [agent.build]\nagent='codex'\nprompt-file='prompt.md'\nthen='pr'\n",
        journal,
    )
}

fn history_of(spec: &str, journal: &[JournalEntry]) -> String {
    let spec = toml::from_str(spec).unwrap();
    let record = waiting_record(0, 0, 1);
    let paths = paths::StatePaths::for_project_root(&record.project_root)
        .unwrap()
        .workflow(&record.id);
    let globals = crate::cli::GlobalFlags {
        mux: None,
        zellij: false,
        tmux: false,
        root: None,
        color: crate::cli::ColorWhen::Never,
    };
    let view = View {
        record: &record,
        paths: &paths,
        spec: &spec,
        journal,
        now: at(20),
        commands: resolve::RunCommands::new(&record, &globals),
    };
    let mut out = Vec::new();
    view.history(&mut out).unwrap();
    String::from_utf8(out).unwrap()
}

fn clock(minute: i64) -> String {
    render::workflow_clock(at(minute), at(20))
}

mod memory;

#[test]
fn cancellation_history_prints_the_recorded_reason() {
    let dir = tempfile::tempdir().unwrap();
    let state = paths::StatePaths::for_project_root_under(dir.path(), dir.path()).unwrap();
    let globals = crate::cli::GlobalFlags {
        mux: None,
        zellij: false,
        tmux: false,
        root: None,
        color: crate::cli::ColorWhen::Never,
    };
    let spec = b"[workflow]\nscope='worktree'\nstart='pr'\n\
        [test.pr]\nshell='true'\non={pass='@done',fail='@failed'}\n";
    let mut actual = Vec::new();
    for reason in [None, Some("worktree removed")] {
        let mut record = waiting_record(0, 0, 1);
        record.id = WorkflowRunId::new();
        let guard = rimz::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
        records::initialize(&state, &record, spec, &guard).unwrap();
        drop(guard);
        workflow::cancel(
            &state,
            &record.id,
            &Host::new(&state, &globals, None),
            reason,
        )
        .unwrap();
        let journal = records::journal(&state.workflow(&record.id)).unwrap();
        let text = history(&journal);
        actual.push(
            text.lines()
                .find(|line| line.contains("cancel canceled"))
                .unwrap()
                .split_once("cancel canceled")
                .unwrap()
                .1
                .to_owned(),
        );
    }
    assert_eq!(actual, ["", " · worktree removed"]);
}

#[test]
fn evaluations_fold_with_newest_answer_and_suppress_the_closed_wait() {
    let mut journal = (1..=5)
        .map(|seq| evaluation(seq, seq as i64 - 1, "pending", None))
        .collect::<Vec<_>>();
    for entry in &mut journal[1..] {
        if let JournalEvent::Evaluation { cost_usd, .. } = &mut entry.event {
            *cost_usd = Some(0.03);
        }
    }
    journal.push(evaluation(6, 5, "done", None));
    journal.push(wait(5, "pending", 0, 4, 5));
    let text = history(&journal);
    assert_eq!(collapsed_history(&journal).len(), 2, "{text}");
    assert!(
        text.contains(&format!(
            "{} #1–5 pr said pending → @wait ×5 over 4m · $0.12\n",
            clock(4)
        )),
        "{text}"
    );
    assert!(text.contains("reason: answer 5"), "{text}");
    assert!(!text.contains("answer 1"), "{text}");
    assert!(
        text.contains(&format!("{} #6 pr said done → @done\n", clock(5))),
        "{text}"
    );
    assert!(!text.contains("pr wait"), "{text}");
}

#[test]
fn a_closed_hold_searches_past_a_newer_case_without_breaking_its_run() {
    let journal = [
        evaluation(1, 0, "hold", Some(0)),
        evaluation(2, 1, "hold", Some(0)),
        evaluation(3, 2, "hold", Some(1)),
        wait(2, "hold", 0, 1, 2),
        evaluation(4, 3, "hold", Some(1)),
        evaluation(5, 4, "hold", Some(1)),
    ];
    let text = history(&journal);
    assert_eq!(collapsed_history(&journal).len(), 2, "{text}");
    assert!(
        text.contains("#1–2 pr said hold → @wait ×2 over 1m"),
        "{text}"
    );
    assert!(
        text.contains("#3–5 pr said hold → @wait ×3 over 2m"),
        "{text}"
    );
    assert!(!text.contains("pr wait"), "{text}");
}

#[test]
fn wakes_break_evaluation_runs_and_only_fold_with_the_same_signal() {
    let mut journal = vec![
        evaluation(1, 0, "pending", None),
        wake(1, "review.ready"),
        evaluation(2, 2, "pending", None),
    ];
    assert_eq!(collapsed_history(&journal).len(), 3);
    journal.extend([
        wake(3, "review.ready"),
        wake(12, "review.ready"),
        wake(13, "review.changed"),
    ]);
    let text = history(&journal);
    assert_eq!(collapsed_history(&journal).len(), 5, "{text}");
    assert!(
        text.contains(&format!(
            "{} #— pr wake review.ready ×2 over 9m\n",
            clock(12)
        )),
        "{text}"
    );
    assert!(text.contains("pr wake review.changed\n"), "{text}");
}

#[test]
fn subsecond_folds_keep_the_count_without_a_span() {
    let journal = [
        evaluation(1, 0, "pending", None),
        evaluation(2, 0, "pending", None),
        wake(0, "review.ready"),
        wake(0, "review.ready"),
        wait(0, "window", 0, 0, 2),
    ];
    let text = history(&journal);
    assert_eq!(text.matches("×2").count(), 3, "{text}");
    assert!(!text.contains("over"), "{text}");
}

#[test]
fn unrepresented_waits_show_their_span_and_detail_but_no_singleton_count() {
    let mut window = wait(3, "window", 0, 2, 3);
    window.node = "build".into();
    if let JournalEvent::Wait { detail, .. } = &mut window.event {
        *detail = Some("5h.left=12<42".into());
    }
    let text = history(&[window, wait(4, "pending", 4, 4, 1)]);
    assert_eq!(
        text,
        format!(
            "  {} #— build wait window ×3 over 2m · 5h.left=12<42\n  {} #— pr wait pending\n",
            clock(3),
            clock(4),
        )
    );
}

#[test]
fn attempts_keep_individual_rows_and_their_detail_lines() {
    let journal = [1, 2].map(|seq| JournalEntry {
        seq: Some(seq),
        at: at(seq as i64),
        node: "build".into(),
        event: JournalEvent::Attempt {
            outcome: records::AttemptOutcome::Ended,
            visit: seq,
            started_past_limit: None,
            key: None,
            routed_by: None,
            digests: BTreeMap::from([("prompt.md".into(), format!("digest-{seq}"))]),
            started_at: at(seq as i64),
            ended_at: at(seq as i64),
            handle: Some(format!("@builder-{seq}")),
            run_id: None,
            head_start: Some("0123456789".into()),
            head_end: Some("abcdef0123".into()),
            code: None,
            log: Some(PathBuf::from(format!("work/{seq}.step.log"))),
            cost_usd: Some(0.25),
            children: None,
            limited: None,
            asked: None,
            reason: None,
            message: None,
        },
    });
    let text = history(&journal);
    assert_eq!(collapsed_history(&journal).len(), 2, "{text}");
    for seq in [1, 2] {
        assert!(
            text.contains(&format!("#{seq} build attempt ended · visit {seq} · 0s\n")),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                concat!(
                    "    @builder-{seq} HEAD 01234567 → abcdef01 cost $0.25\n",
                    "    log: work/{seq}.step.log\n    prompt.md: digest-{seq}\n",
                ),
                seq = seq,
            )),
            "{text}"
        );
    }
}

#[test]
fn parks_keep_text_not_printed_by_the_latest_answer_at_their_node() {
    for (node, reason) in [("build", "answer 2"), ("pr", "answer 1")] {
        let journal = [
            evaluation(1, 0, "pending", None),
            evaluation(2, 1, "done", None),
            JournalEntry {
                seq: None,
                at: at(2),
                node: node.into(),
                event: JournalEvent::Park {
                    reason: records::ParkReason::User,
                    text: Some(reason.into()),
                },
            },
        ];
        let text = history(&journal);
        assert!(
            text.ends_with(&format!("{node} park user\n    {reason}\n")),
            "{text}"
        );
    }
}

#[test]
fn evaluation_keys_keep_origins_nodes_and_outcomes_separate() {
    let check = evaluation(1, 0, "pending", None);
    let mut engine = evaluation(2, 1, "pending", None);
    if let JournalEvent::Evaluation { origin, .. } = &mut engine.event {
        *origin = EvaluationOrigin::Engine;
    }
    let mut other_node = engine.clone();
    other_node.node = "ci".into();
    let mut other_outcome = other_node.clone();
    if let JournalEvent::Evaluation {
        outcome, object, ..
    } = &mut other_outcome.event
    {
        *outcome = "done".into();
        object["outcome"] = json!("done");
    }
    let journal = [check, engine, other_node, other_outcome];
    assert_eq!(collapsed_history(&journal).len(), 4);
}

#[test]
fn wait_rows_match_the_nodes_outcome_and_inclusive_span() {
    let journal = [evaluation(1, 1, "pending", None)];
    let rows = collapsed_history(&journal);
    assert!(wait_row_counts(&rows, "pr", "pending", at(1), at(1)).eq([1]));
    assert!(wait_row_counts(&rows, "pr", "pending", at(0), at(2)).eq([1]));
    for (node, outcome, first, last) in [
        ("pr", "window", 0, 2),
        ("ci", "pending", 0, 2),
        ("pr", "pending", 2, 3),
        ("pr", "pending", 0, 0),
    ] {
        assert!(
            wait_row_counts(&rows, node, outcome, at(first), at(last))
                .next()
                .is_none()
        );
    }
}

fn waiting_record(first: i64, last: i64, count: u64) -> RunRecord {
    serde_json::from_value(json!({
        "version":rimz::store::workflow::VERSION, "id":WorkflowRunId::new(), "workflow":"review",
        "spec_dir":"/spec", "spec_digest":"digest",
        "scope":{"kind":"worktree", "path":"/checkout"}, "project_root":"/checkout",
        "created_at":at(0), "state":"active", "node":"build", "entered_at":at(0),
        "visits":{}, "visit_grant":0, "deadline_fired":false, "next_seq":4,
        "wait":{"node":"pr", "outcome":"pending", "first":at(first), "last":at(last),
                "count":count}
    }))
    .unwrap()
}

fn kept_turn(minute: i64) -> JournalEntry {
    JournalEntry {
        seq: Some(1),
        at: at(minute),
        node: "build".into(),
        event: JournalEvent::TurnEnd {
            action: records::TurnEndAction::Kept,
            turn: records::TurnEnd {
                agent_id: "leader".into(),
                started_at: at(minute - 1),
                ended_at: at(minute),
            },
            check: "pr".into(),
            outcome: Some("pending".into()),
            target: Some("@wait".into()),
            reason: None,
            visit: None,
            message: None,
        },
    }
}

#[test]
fn one_kept_turn_states_the_open_wait_and_suppresses_its_closing_row() {
    let mut journal = vec![
        evaluation(2, 0, "pending", None),
        kept_turn(1),
        evaluation(3, 2, "done", None),
    ];
    let record = waiting_record(0, 0, 1);
    let line = workflow_view::wait(&record, None, at(20)).unwrap();
    assert!(!line.contains('×'), "{line}");
    assert!(
        !workflow_view::wait(&record, None, Timestamp::now())
            .unwrap()
            .contains('×')
    );
    journal.push(wait(2, "pending", 0, 0, 1));
    assert_eq!(collapsed_history(&journal).len(), 3);
    assert!(!history(&journal).contains("pr wait"));
}

#[test]
fn several_kept_turns_omit_the_header_total_and_closing_summary() {
    let mut journal = vec![
        evaluation(2, 0, "pending", None),
        kept_turn(1),
        evaluation(3, 2, "pending", None),
        kept_turn(3),
    ];
    let record = waiting_record(0, 2, 2);
    let line = workflow_view::wait(&record, None, at(20)).unwrap();
    assert!(!line.contains('×'), "{line}");
    journal.push(wait(4, "pending", 0, 2, 2));
    assert_eq!(collapsed_history(&journal).len(), 4);
    assert!(!history(&journal).contains("pr wait"));
}

#[test]
fn wakes_do_not_add_a_waiting_line_count() {
    let mut journal = vec![
        evaluation(1, 0, "pending", None),
        evaluation(2, 1, "pending", None),
        wake(2, "review.ready"),
        evaluation(3, 3, "pending", None),
    ];
    let record = waiting_record(0, 3, 3);
    let line = workflow_view::wait(&record, None, at(20)).unwrap();
    assert!(!line.contains('×'), "{line}");
    journal.push(wait(4, "pending", 0, 3, 3));
    assert_eq!(collapsed_history(&journal).len(), 3);
    assert!(!history(&journal).contains("pr wait"));
}

#[test]
fn early_advances_do_not_add_a_waiting_line_count() {
    let mut journal = vec![evaluation(1, 0, "pending", None)];
    let record = waiting_record(0, 2, 3);
    let line = workflow_view::wait(&record, None, at(20)).unwrap();
    assert!(!line.contains('×'), "{line}");
    journal.push(wait(3, "pending", 0, 2, 3));
    assert_eq!(collapsed_history(&journal).len(), 1);
    assert!(!history(&journal).contains("pr wait"));
}

/// Journals `harness::workflow::tests::message` runs wrote for `TELL`, delivered and undelivered.
const DELIVERED: &str = r#"{"seq":1,"at":"2026-11-08T22:46:30.825083175Z","node":"tell","event":"attempt","outcome":"ended","visit":1,"key":null,"routed_by":null,"digests":{"prompt":"84893a778be73583c7aef377b6ed232cccefb47cb89ea9052a636c9532fbbf7f","prompt.md":"5e383207c65159ba379ca0ca2a83d23f2d145626b4d9527af53b1a5441151549"},"started_at":"2026-11-08T22:46:30.818860072Z","ended_at":"2026-11-08T22:46:30.825083175Z","handle":"@planner","run_id":null,"head_start":null,"head_end":null,"code":null,"log":null,"cost_usd":null,"children":null,"limited":null,"asked":null,"reason":null,"message":"msg_06grqls0k849ljsn"}
{"seq":null,"at":"2026-11-08T22:46:30.825083175Z","node":"tell","event":"terminal","state":"done"}"#;
const UNDELIVERED: &str = r#"{"seq":1,"at":"2026-11-08T22:46:30.841509403Z","node":"tell","event":"attempt","outcome":"died","visit":1,"key":null,"routed_by":null,"digests":{"prompt":"84893a778be73583c7aef377b6ed232cccefb47cb89ea9052a636c9532fbbf7f","prompt.md":"5e383207c65159ba379ca0ca2a83d23f2d145626b4d9527af53b1a5441151549"},"started_at":"2026-11-08T22:46:30.83551457Z","ended_at":"2026-11-08T22:46:30.841509403Z","handle":null,"run_id":null,"head_start":null,"head_end":null,"code":null,"log":null,"cost_usd":null,"children":null,"limited":null,"asked":null,"reason":"no agent matches target `@planner`; run `rimz agents list` to see live agents","message":"msg_06grqls0mf1fmajg"}
{"seq":null,"at":"2026-11-08T22:46:30.841509403Z","node":"tell","event":"route","outcome":"undelivered","input":{"outcome":"undelivered","reason":"no agent matches target `@planner`; run `rimz agents list` to see live agents"}}
{"seq":null,"at":"2026-11-08T22:46:30.841509403Z","node":"tell","event":"terminal","state":"failed"}"#;

#[test]
fn a_message_attempt_names_its_target_or_why_none_took_it() {
    let spec = "[workflow]\non-break='@failed'\nscope='worktree'\nstart='tell'\n\
         [message.tell]\nmessage='@planner'\nprompt-file='prompt.md'\nthen='@done'\n";
    for (journal, detail) in [
        (DELIVERED, "    to @planner · ended"),
        (
            UNDELIVERED,
            "    to — · no agent matches target `@planner`; run `rimz agents list` to see live agents",
        ),
    ] {
        let journal = journal
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect::<Vec<JournalEntry>>();
        let text = history_of(spec, &journal);
        let lines = text.lines().collect::<Vec<_>>();
        let attempt = lines
            .iter()
            .position(|line| line.contains("#1 tell attempt"))
            .unwrap_or_else(|| panic!("{text}"));
        assert_eq!(lines[attempt + 1], detail, "{text}");
        assert!(
            !text.contains("exit ") && !text.contains("HEAD") && !text.contains("reason:"),
            "{text}"
        );
    }
}
