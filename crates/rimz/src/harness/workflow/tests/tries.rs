//! Runs started by hand at a node (`--at`), with an input (`--input`), or as a try (`--once`).

use super::*;
use crate::disk::paths::StatePaths;
use crate::store::workflow::{
    self as records, JournalEntry, JournalEvent, Key, RunEnding, RunRecord, RunState,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const WATCH: &str = "[workflow]\nscope='worktree'\nstart='build'\n\
    [agent.build]\nagent='claude'\nlifecycle='turn'\nadmission.throttle='off'\nprompt-file='prompt.md'\nthen='watch'\nmax-visits=6\n\
    [script.watch]\nrun='cat answer.json'\ntimeout='1h'\non={red='build',bad='@failed',green='@done',queued='@wait'}\n";

/// A room whose checkout and definition folder are one directory, `try`, holding `WATCH`.
fn room() -> (tempfile::TempDir, StatePaths, PathBuf) {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("try");
    fs::create_dir(&root).unwrap();
    let state = StatePaths::for_project_root_under(home.path(), &root).unwrap();
    fs::create_dir_all(state.workspace_record.parent().unwrap()).unwrap();
    fs::write(
        &state.workspace_record,
        serde_json::to_vec(&json!({
            "layout":crate::disk::paths::WORKSPACE_LAYOUT,
            "workspace_id":crate::ids::WorkspaceId::from_project_root(&root),
            "project_root":root, "session_name":"test", "updated_at":jiff::Timestamp::now()
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(crate::disk::paths::workflow_spec_file(&root), WATCH).unwrap();
    fs::write(root.join("prompt.md"), "Build it").unwrap();
    (home, state, root)
}

fn start(state: &StatePaths, home: &Path, args: StartArgs) -> Result<RunRecord, WorkflowErr> {
    create(
        state,
        DefinitionSource::Deployed {
            loop_fire: None,
            dir: home,
            task: None,
            fire: None,
            fire_facts: None,
        },
        ScopeRoots {
            checkout: home.into(),
            project: home.into(),
        },
        || Ok(None),
        false,
        args,
    )
    .map(|(record, _)| record)
}

fn placed(at: Option<&str>, input: Option<Value>, once: bool) -> StartArgs {
    StartArgs {
        at: at.map(str::to_owned),
        input: input.map(|input| input.as_object().unwrap().clone()),
        once,
        ..Default::default()
    }
}

fn journal(state: &StatePaths, record: &RunRecord) -> Vec<JournalEntry> {
    records::journal(&state.workflow(&record.id)).unwrap()
}

fn load(state: &StatePaths, record: &RunRecord) -> RunRecord {
    records::load(&state.workflow(&record.id)).unwrap()
}

#[test]
fn a_try_at_a_check_ends_tried_at_its_answer_whatever_the_target() {
    for (answer, target) in [("red", "build"), ("bad", "@failed"), ("green", "@done")] {
        let (_home, state, root) = room();
        fs::write(
            root.join("answer.json"),
            json!({"outcome":answer}).to_string(),
        )
        .unwrap();
        let given = json!({"outcome":"pass", "key":"lint"});
        let record = start(
            &state,
            &root,
            placed(Some("watch"), Some(given.clone()), true),
        )
        .unwrap();
        assert_eq!(
            (
                record.node.as_str(),
                record.input.as_ref(),
                record.key.as_ref(),
                record.once
            ),
            (
                "watch",
                Some(&given),
                Some(&Key::String("lint".into())),
                true
            )
        );
        let host = FakeHost::new();
        advance(&state, &record.id, &host).unwrap();
        let tried = load(&state, &record);
        assert_eq!(
            (
                tried.state,
                tried.node.as_str(),
                tried.outcome.map(|outcome| outcome.ending),
                tried.input.as_ref(),
            ),
            (
                RunState::Done,
                "watch",
                Some(RunEnding::Tried),
                Some(&given)
            ),
            "{answer}"
        );
        assert!(
            host.launched.borrow().is_empty(),
            "{answer}: nothing entered"
        );
        let journal = journal(&state, &record);
        assert_eq!(
            journal.last().map(|entry| &entry.event),
            Some(&JournalEvent::Terminal {
                state: RunState::Done,
                tried: Some(target.into())
            }),
            "{answer}"
        );
    }
}

#[test]
fn only_a_hand_placed_start_journals_a_start_entry_and_it_comes_first() {
    let input = json!({"outcome":"red", "key":7});
    for (at, given, expected) in [
        (None, None, None),
        (Some("watch"), None, Some(("watch", None))),
        (
            None,
            Some(input.clone()),
            Some(("build", Some(input.clone()))),
        ),
    ] {
        let (_home, state, root) = room();
        let record = start(&state, &root, placed(at, given.clone(), false)).unwrap();
        let journal = journal(&state, &record);
        let starts = journal
            .iter()
            .filter(|entry| matches!(entry.event, JournalEvent::Start { .. }))
            .count();
        let first = journal.first().and_then(|entry| match &entry.event {
            JournalEvent::Start { input } => Some((entry.node.as_str(), input.clone())),
            _ => None,
        });
        assert_eq!(
            (starts, first),
            (usize::from(expected.is_some()), expected),
            "{at:?} {given:?}"
        );
        if given.is_some() {
            assert_eq!(record.key, Some(Key::Integer(7)));
        }
    }
}

#[test]
fn an_unknown_start_node_refuses_and_publishes_nothing() {
    let (_home, state, root) = room();
    let error = start(&state, &root, placed(Some("nowhere"), None, true)).unwrap_err();
    assert!(
        matches!(&error, WorkflowErr::Refused(text) if text == "unknown workflow node nowhere"),
        "{error:?}"
    );
    assert!(state.workflow_runs().map_or(true, |runs| runs.is_empty()));
}

#[test]
fn a_try_never_refuses_or_is_refused_but_a_mid_graph_start_is() {
    let (_home, state, root) = room();
    let tried = start(&state, &root, placed(Some("watch"), None, true)).unwrap();
    let plain =
        start(&state, &root, StartArgs::default()).expect("a live try does not hold the checkout");
    start(&state, &root, placed(Some("watch"), None, true))
        .expect("a try starts beside a live run and another try");
    let refused = |args| match start(&state, &root, args) {
        Err(WorkflowErr::Exists { run, .. }) => run,
        other => panic!("a live run refuses a start that is not a try: {other:?}"),
    };
    assert_eq!(refused(placed(Some("watch"), None, false)), plain.id);
    assert_eq!(
        refused(placed(None, Some(json!({"outcome":"red"})), false)),
        plain.id
    );
    assert_ne!(tried.id, plain.id);
}

#[test]
fn resume_at_refuses_a_try_before_it_changes_anything_and_extend_still_works() {
    let (_home, state, root) = room();
    let record = start(&state, &root, placed(Some("build"), None, true)).unwrap();
    let before = (load(&state, &record), journal(&state, &record));
    let host = FakeHost::new();
    let error = resume(&state, &record.id, &host, Some("watch"), &[]).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "workflow run {} is a try, which ends at its node and cannot be moved; cancel it \
             with `rimz workflow cancel {}`",
            record.id, record.id
        )
    );
    assert!(matches!(error, WorkflowErr::Refused(_)), "{error:?}");
    assert_eq!((load(&state, &record), journal(&state, &record)), before);
    let extended = resume(&state, &record.id, &host, None, &["visits=+1".into()]).unwrap();
    assert_eq!((extended.visit_grant, extended.once), (1, true));
}

#[test]
fn canceling_a_try_ends_it_canceled_not_tried() {
    let (_home, state, root) = room();
    let record = start(&state, &root, placed(Some("watch"), None, true)).unwrap();
    assert!(cancel(&state, &record.id, &FakeHost::new(), None).unwrap());
    let canceled = load(&state, &record);
    assert_eq!(
        (
            canceled.state,
            canceled.outcome.map(|outcome| outcome.ending)
        ),
        (RunState::Canceled, Some(RunEnding::Canceled))
    );
}

#[test]
fn a_try_at_an_occupied_step_keeps_waiting() {
    let (_home, state, root) = room();
    let record = start(&state, &root, placed(Some("build"), None, true)).unwrap();
    let host = FakeHost::new();
    *host.occupancy.borrow_mut() = Occupancy::Occupied(vec!["builder".into()]);
    advance(&state, &record.id, &host).unwrap();
    let waiting = load(&state, &record);
    assert_eq!(waiting.state, RunState::Active);
    assert!(waiting.wait.is_some(), "{waiting:?}");
    assert!(
        !journal(&state, &record)
            .iter()
            .any(|entry| matches!(entry.event, JournalEvent::Terminal { .. }))
    );
}

#[test]
fn the_brief_of_a_hand_placed_step_says_so_and_names_a_given_input() {
    for input in [None, Some(json!({"outcome":"red", "detail":"lint"}))] {
        let (_home, state, root) = room();
        let record = start(&state, &root, placed(Some("build"), input.clone(), false)).unwrap();
        let host = FakeHost::new();
        advance(&state, &record.id, &host).unwrap();
        let note = host.launched.borrow().last().unwrap().workflow_note.clone();
        assert!(
            note.contains("The user started the run at this step by hand."),
            "{note}"
        );
        assert!(!note.contains("first step"), "{note}");
        assert_eq!(
            note.contains(
                "An input was given with the start; it is `input` in `context.json` in \
                 RIMZ_WORKFLOW_DIR."
            ),
            input.is_some(),
            "{note}"
        );
    }
}

/// `WATCH` with its `watch` check renamed `look`, a definition a run at `watch` no longer fits.
fn watch_renamed() -> String {
    WATCH
        .replace("then='watch'", "then='look'")
        .replace("[script.watch]", "[script.look]")
}

/// A run of `text` started at `at`, as a try and then without `once`, through a definition that
/// stops fitting while `misfit` drives it: each run's record, the terminal entries of its
/// journal, and the targets its route assists name.
fn misfit_both(
    text: &str,
    edited: &str,
    at: &str,
    misfit: impl Fn(&StatePaths, &RunRecord, &FakeHost),
) -> Vec<(RunRecord, Vec<JournalEvent>, Vec<String>)> {
    [true, false]
        .into_iter()
        .map(|once| {
            let (_home, state, root) = room();
            fs::write(crate::disk::paths::workflow_spec_file(&root), text).unwrap();
            fs::write(root.join("edited.toml"), edited).unwrap();
            fs::write(
                root.join("answer.json"),
                json!({"outcome":"red"}).to_string(),
            )
            .unwrap();
            let record = start(&state, &root, placed(Some(at), None, once)).unwrap();
            misfit(&state, &record, &FakeHost::new());
            let terminals = journal(&state, &record)
                .into_iter()
                .filter(|entry| matches!(entry.event, JournalEvent::Terminal { .. }))
                .map(|entry| entry.event)
                .collect();
            let routes = crate::harness::assist_log::recent(&crate::disk::paths::logs_dir(), None)
                .into_iter()
                .filter_map(|assist| match assist.assist {
                    crate::harness::assist_log::Assist::WorkflowRoute { run, to, .. }
                        if run == record.id =>
                    {
                        Some(to)
                    }
                    _ => None,
                })
                .collect();
            (load(&state, &record), terminals, routes)
        })
        .collect()
}

/// A try whose definition stops fitting ends failed as the same run without `once` does: its
/// node never answered, so there is no route to show.
fn assert_misfit_fails(runs: &[(RunRecord, Vec<JournalEvent>, Vec<String>)], what: &str) {
    let [(tried, terminals, routes), (plain, ..)] = runs else {
        panic!("two runs: {runs:#?}");
    };
    assert_eq!(
        (
            tried.state,
            tried.once,
            tried.outcome.as_ref().map(|outcome| &outcome.ending),
            tried.outcome.as_ref().map(|outcome| &outcome.reason),
        ),
        (
            RunState::Failed,
            true,
            plain.outcome.as_ref().map(|outcome| &outcome.ending),
            plain.outcome.as_ref().map(|outcome| &outcome.reason),
        ),
        "{tried:#?}"
    );
    assert_eq!(plain.state, RunState::Failed, "{plain:#?}");
    assert!(
        tried
            .outcome
            .as_ref()
            .and_then(|outcome| outcome.reason.as_deref())
            .is_some_and(|reason| reason.contains(what)),
        "{tried:#?}"
    );
    assert_eq!(
        terminals,
        &[JournalEvent::Terminal {
            state: RunState::Failed,
            tried: None
        }]
    );
    assert_eq!(
        routes.last().map(String::as_str),
        Some("@failed"),
        "{routes:?}"
    );
}

#[test]
fn a_try_whose_check_stops_fitting_ends_failed_not_tried() {
    let text = WATCH.replace(
        "run='cat answer.json'",
        "run=\"sh -c 'cp edited.toml workflow.toml && cat answer.json'\"",
    );
    let runs = misfit_both(&text, &watch_renamed(), "watch", |state, record, host| {
        advance(state, &record.id, host).unwrap();
    });
    assert_misfit_fails(
        &runs,
        "watch: the workflow definition changed under the run",
    );
}

#[test]
fn a_try_whose_command_step_stops_fitting_ends_failed_not_tried() {
    let text = format!(
        "{WATCH}[command.fix]\nshell='cp edited.toml workflow.toml'\ntimeout='1h'\nthen='watch'\n"
    );
    let runs = misfit_both(&text, WATCH, "fix", |state, record, host| {
        advance(state, &record.id, host).unwrap();
    });
    assert_misfit_fails(&runs, "fix: the workflow definition changed under the run");
}

#[test]
fn a_try_whose_agent_step_stops_fitting_ends_failed_not_tried() {
    let edited = WATCH.replace(
        "[agent.build]\nagent='claude'\nlifecycle='turn'\nadmission.throttle='off'\nprompt-file='prompt.md'\nthen='watch'\nmax-visits=6\n",
        "[command.build]\nshell='true'\ntimeout='1h'\nthen='watch'\nmax-visits=6\n",
    );
    let runs = misfit_both(WATCH, &edited, "build", |state, record, host| {
        advance(state, &record.id, host).unwrap();
        assert!(load(state, record).attempt.is_some(), "the agent launched");
        fs::copy(
            record.scope.path.join("edited.toml"),
            crate::disk::paths::workflow_spec_file(&record.scope.path),
        )
        .unwrap();
        advance(state, &record.id, host).unwrap();
        assert_eq!(host.stops.get(), 1);
    });
    assert_misfit_fails(
        &runs,
        "build: the workflow definition changed under the run",
    );
}

#[test]
fn a_try_whose_judge_stops_fitting_ends_failed_not_tried() {
    let text = format!(
        "{WATCH}[judge.review]\nagent='claude'\nsummary='Reviews the change'\n\
         prompt-file='prompt.md'\ntimeout='1h'\non={{pass='@done',fail='build'}}\n"
    );
    let runs = misfit_both(&text, WATCH, "review", |state, record, host| {
        advance(state, &record.id, host).unwrap();
        assert!(load(state, record).attempt.is_some(), "the judge launched");
        fs::copy(
            record.scope.path.join("edited.toml"),
            crate::disk::paths::workflow_spec_file(&record.scope.path),
        )
        .unwrap();
        advance(state, &record.id, host).unwrap();
        assert_eq!(host.stops.get(), 1);
    });
    assert_misfit_fails(
        &runs,
        "review: the workflow definition changed under the run",
    );
}
