use super::*;
use crate::disk::lock::WorkspaceLock;
use crate::disk::paths::StatePaths;
use crate::store::workflow::{self as records, JournalEvent, Key, RunRecord, RunState};
use jiff::Timestamp;
use serde_json::{Value, json};

const SLEEP: &str = "[workflow]\non-break='@failed'\nscope='worktree'\nstart='cooldown'\n[checks.cooldown]\nsleep='30m'\nthen='@done'\n";

fn update(state: &StatePaths, record: &RunRecord) {
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    records::write(&state.workflow(&record.id), record, &guard).unwrap();
}

fn advance_sleep(state: &StatePaths, record: &RunRecord) -> RunRecord {
    let result = advance(state, &record.id, &FakeHost::new());
    assert!(
        result.is_ok(),
        "a sleep must evaluate in-process: {result:?}"
    );
    records::load(&state.workflow(&record.id)).unwrap()
}

fn evaluations(state: &StatePaths, record: &RunRecord) -> Vec<Value> {
    records::journal(&state.workflow(&record.id))
        .unwrap()
        .into_iter()
        .filter_map(|row| match row.event {
            JournalEvent::Evaluation { object, .. } => Some(object),
            _ => None,
        })
        .collect()
}

#[test]
fn plain_sleep_waits_until_its_entry_based_end_and_preserves_input_and_visit_bucket() {
    let text = SLEEP.replace("then='@done'", "then='build'\n[steps.build]\nagent='claude'\nlifecycle='turn'\nprompt-file='prompt.md'\nthen='cooldown'\nmax-visits=3\nmax-visits-per-outcome=2");
    let (_dir, state, mut record) = attempts::setup(&text);
    record.input = Some(json!({"outcome":"repair", "key":"original", "context":42}));
    record.key = Some(Key::String("original".into()));
    let input = record.input.clone();
    update(&state, &record);
    let waiting = advance_sleep(&state, &record);
    assert_eq!(
        waiting.wait.as_ref().map(|wait| wait.outcome.as_str()),
        Some("sleeping")
    );
    assert_eq!(
        waiting.recheck_at,
        Some(record.entered_at + std::time::Duration::from_secs(1800))
    );
    assert_eq!((&waiting.input, &waiting.key), (&input, &record.key));
    advance_sleep(&state, &record);
    assert_eq!(evaluations(&state, &record).len(), 1);
    let mut due = waiting;
    due.entered_at = Timestamp::now() - std::time::Duration::from_secs(1801);
    due.recheck_at = Some(due.entered_at + std::time::Duration::from_secs(1800));
    update(&state, &due);
    let next = advance_sleep(&state, &record);
    assert_eq!(next.node, "build");
    assert_eq!((&next.input, &next.key), (&input, &record.key));
    assert_eq!(
        next.visits["build"].outcomes[0].outcome.as_deref(),
        Some("repair")
    );
    assert_eq!(
        evaluations(&state, &record),
        vec![
            json!({
                "outcome":"sleeping", "key":"original",
                "recheck_at":record.entered_at + std::time::Duration::from_secs(1800)
            }),
            json!({"outcome":"then", "key":"original", "reason":"slept 30m"}),
        ]
    );
    let spec = load(&state.workflow(&record.id).home).unwrap();
    assert!(spec.checks["cooldown"].subscriptions().unwrap().is_empty());
    let journal = records::journal(&state.workflow(&record.id)).unwrap();
    let routes: Vec<_> = hops(&spec, &journal, next.held.as_ref())
        .into_iter()
        .filter(|hop| hop.node == "cooldown" && hop.kind == HopKind::Route)
        .map(|hop| (hop.outcome.unwrap(), hop.target.unwrap()))
        .collect();
    assert_eq!(
        routes,
        [
            ("sleeping".into(), "@wait".into()),
            ("then".into(), "build".into())
        ]
    );
    let brief = super::super::brief::step(&state.workflow(&record.id), &spec, &next);
    assert!(
        brief.contains("slept at") && !brief.contains("answered `then`"),
        "{brief}"
    );
    let text = text.replace("start='cooldown'", "start='cooldown'\non.deadline='build'");
    let (_dir, state, record) = attempts::setup(&text);
    let spec = load(&state.workflow(&record.id).home).unwrap();
    let mut waiting = advance_sleep(&state, &record);
    waiting.deadline = Some(Timestamp::UNIX_EPOCH);
    waiting.recheck_at = None;
    update(&state, &waiting);
    let next = advance_sleep(&state, &record);
    assert_eq!(next.node, "build");
    let brief = super::super::brief::step(&state.workflow(&record.id), &spec, &next);
    assert!(
        brief.contains("The `deadline` route from `cooldown` sent the run here.")
            && !brief.contains("slept at"),
        "{brief}"
    );
}

#[test]
fn sleep_deadline_clamps_the_wake_and_fails_without_replacing_input() {
    let (_dir, state, mut record) = attempts::setup(SLEEP);
    record.input = Some(json!({"outcome":"repair", "key":7}));
    record.key = Some(Key::Integer(7));
    record.deadline = Some(Timestamp::now() + std::time::Duration::from_secs(60));
    update(&state, &record);
    let mut waiting = advance_sleep(&state, &record);
    assert_eq!(waiting.recheck_at, record.deadline);
    waiting.deadline = Some(Timestamp::now() - std::time::Duration::from_secs(1));
    update(&state, &waiting);
    let failed = advance_sleep(&state, &record);
    assert_eq!(failed.state, RunState::Failed);
    assert_eq!((&failed.input, &failed.key), (&record.input, &record.key));
}

#[test]
fn sleep_conditions_share_freshness_unread_and_hold_semantics_with_expiry_first() {
    for (term, expire) in [
        ("pr=merged", false),
        ("pr=merged", true),
        ("!pr.queue=queued", true),
    ] {
        let text = format!("{SLEEP}when=['{term}']\nfor='2m'\n");
        let (dir, state, record) = attempts::setup(&text);
        let mut waiting = advance_sleep(&state, &record);
        assert_eq!(
            waiting.wait.as_ref().map(|wait| wait.outcome.as_str()),
            Some("unknown")
        );
        assert_eq!(waiting.recheck_at, None);
        let runtime = crate::RuntimePaths::for_state(&state).unwrap();
        let path = runtime.lane_path("pr-state.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let forge =
            |at: Timestamp, pr: &str| {
                fs::write(&path, serde_json::to_vec(&json!({
                "branch_ci":{}, "path_repos":{(dir.path().to_string_lossy().as_ref()):"repo"},
                "repos":{"repo":{"refreshed_at_ms":at.as_millisecond(),"ok":true}},
                "states":{(dir.path().to_string_lossy().as_ref()):{"state":pr}}
            })).unwrap()).unwrap();
            };
        forge(
            record.entered_at - std::time::Duration::from_secs(1),
            "merged",
        );
        waiting = advance_sleep(&state, &record);
        assert_eq!(waiting.wait.as_ref().unwrap().outcome, "unknown");
        assert!(
            evaluations(&state, &record).last().unwrap()["reason"]
                .as_str()
                .unwrap()
                .contains("probed before")
        );
        forge(Timestamp::now(), "open");
        waiting = advance_sleep(&state, &record);
        if term == "pr=merged" {
            assert_eq!(waiting.wait.as_ref().unwrap().outcome, "sleeping");
            forge(Timestamp::now(), "merged");
            waiting = advance_sleep(&state, &record);
        }
        assert_eq!(waiting.wait.as_ref().unwrap().outcome, "hold");
        assert_eq!(
            waiting.wait.as_ref().unwrap().detail.as_deref(),
            Some("then")
        );
        assert!(waiting.input.is_none());
        if expire {
            waiting.entered_at = Timestamp::now() - std::time::Duration::from_secs(1801);
        } else {
            waiting.wait.as_mut().unwrap().first =
                Timestamp::now() - std::time::Duration::from_secs(121);
        }
        waiting.recheck_at = None;
        update(&state, &waiting);
        let done = advance_sleep(&state, &record);
        assert_eq!(done.state, RunState::Done);
        let expected = if expire {
            json!({"outcome":"then", "reason":"slept 30m"})
        } else {
            json!({"outcome":"then", "case":0, "readings":{"pr":"merged"}})
        };
        assert_eq!(evaluations(&state, &record).last().unwrap(), &expected);
        assert!(done.input.is_none());
    }
}

#[test]
fn a_sleep_hold_survives_unknown_and_ends_on_false() {
    let text = format!("{SLEEP}when=['pr=merged']\nfor='2m'\n");
    let (dir, state, record) = attempts::setup(&text);
    let runtime = crate::RuntimePaths::for_state(&state).unwrap();
    let path = runtime.lane_path("pr-state.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let forge = |fresh: bool, pr: &str| {
        let at = if fresh {
            Timestamp::now()
        } else {
            record.entered_at - std::time::Duration::from_secs(1)
        };
        fs::write(
            &path,
            serde_json::to_vec(&json!({
                "branch_ci":{}, "path_repos":{(dir.path().to_string_lossy().as_ref()):"repo"},
                "repos":{"repo":{"refreshed_at_ms":at.as_millisecond(),"ok":true}},
                "states":{(dir.path().to_string_lossy().as_ref()):{"state":pr}}
            }))
            .unwrap(),
        )
        .unwrap();
    };
    let advance_due = || {
        let mut due = records::load(&state.workflow(&record.id)).unwrap();
        due.recheck_at = None;
        update(&state, &due);
        advance_sleep(&state, &record).wait.unwrap()
    };
    forge(true, "merged");
    let first = advance_due().first;
    forge(false, "merged");
    let wait = advance_due();
    assert_eq!(
        (wait.outcome.as_str(), wait.first, wait.count),
        ("hold", first, 2)
    );
    forge(true, "merged");
    let wait = advance_due();
    assert_eq!((wait.outcome.as_str(), wait.first), ("hold", first));
    forge(true, "open");
    assert_eq!(advance_due().outcome, "sleeping");
}

#[test]
fn invalid_sleep_fails_without_replacing_the_incoming_input_or_key() {
    let (_dir, state, mut record) = attempts::setup(SLEEP);
    record.input = Some(json!({"outcome":"repair", "key":"kept"}));
    record.key = Some(Key::String("kept".into()));
    update(&state, &record);
    let waiting = advance_sleep(&state, &record);
    let paths = state.workflow(&record.id);
    fs::write(&paths.spec, SLEEP.replace("sleep='30m'", "sleep='soon'")).unwrap();
    let mut waiting = waiting;
    waiting.recheck_at = None;
    update(&state, &waiting);
    let failed = advance_sleep(&state, &record);
    assert_eq!(failed.state, RunState::Failed);
    assert!(failed.park.is_none());
    assert_eq!((&failed.input, &failed.key), (&record.input, &record.key));
}

#[test]
fn sleep_validation_accepts_plain_conditional_and_polling_cycles_with_explicit_then() {
    let (_home, dir) = fixture();
    let actual: Vec<_> = [
        SLEEP.to_owned(),
        format!("{SLEEP}when=['ci=passed']\nfor='2m'\n"),
        format!("{SLEEP}when=['!pr.queue=queued']\n"),
        SLEEP.replace(
            "then='@done'",
            "then='poll'\n[checks.poll]\nrun='true'\non.done='@done'\non.pending='cooldown'",
        ),
    ]
    .iter()
    .map(|text| {
        fs::write(dir.join("workflow.toml"), text).unwrap();
        let report = validate(&dir, &LoadedDefinitions::default(), None);
        (
            report
                .errors
                .iter()
                .map(|issue| issue.message.clone())
                .collect::<Vec<_>>(),
            report
                .warnings
                .iter()
                .map(|issue| issue.message.clone())
                .collect::<Vec<_>>(),
        )
    })
    .collect();
    assert_eq!(actual, vec![(vec![], vec![]); 4]);
    let spec = load(&dir).unwrap();
    let edges = spec.edges("cooldown");
    assert_eq!(
        edges
            .iter()
            .map(|edge| (edge.outcome.as_str(), edge.target.as_str(), edge.source))
            .collect::<Vec<_>>(),
        [
            ("then", "poll", EdgeSource::Explicit),
            ("sleeping", "@wait", EdgeSource::Default),
            ("invalid", "@failed", EdgeSource::OnBreak),
            ("deadline", "@failed", EdgeSource::OnBreak),
        ]
    );
}

#[test]
fn sleep_validation_refuses_required_fields_bad_terms_and_until_reentry() {
    let (_home, dir) = fixture();
    let cases = [
        (
            SLEEP.replace("sleep='30m'", "sleep='soon'"),
            "checks.cooldown.sleep",
        ),
        (
            SLEEP.replace("sleep='30m'", "sleep='0s'"),
            "checks.cooldown.sleep",
        ),
        (SLEEP.replace("then='@done'", ""), "checks.cooldown.then"),
        (
            SLEEP.replace("then='@done'", "then='@wait'"),
            "checks.cooldown.then",
        ),
        (
            SLEEP.replace("then='@done'", "then='missing'"),
            "checks.cooldown.then",
        ),
        (format!("{SLEEP}when=7\n"), "checks.cooldown.when"),
        (format!("{SLEEP}when=[]\n"), "checks.cooldown.when"),
        (
            format!("{SLEEP}when=['bad syntax']\n"),
            "checks.cooldown.when",
        ),
        (
            format!("{SLEEP}when=['pr=unknown']\n"),
            "checks.cooldown.when",
        ),
        (
            format!("{SLEEP}when=['window.5h.left>=40']\n"),
            "checks.cooldown.provider",
        ),
        (
            format!("{SLEEP}when=['team.stage=MissingStage']\n"),
            "checks.cooldown.when",
        ),
        (format!("{SLEEP}run='true'\n"), "checks.cooldown"),
        (
            format!(
                "{SLEEP}[steps.work]\nagent='claude'\nprompt-file='prompts/build.md'\nlifecycle='until'\nthen='cooldown'\n"
            ),
            "steps.work.then",
        ),
    ];
    let actual: Vec<_> = cases
        .iter()
        .map(|(text, path)| {
            fs::write(dir.join("workflow.toml"), text).unwrap();
            let report = validate(&dir, &LoadedDefinitions::default(), Some(dir.as_path()));
            (path, report.errors.iter().any(|issue| issue.path == *path))
        })
        .collect();
    assert_eq!(
        actual,
        cases
            .iter()
            .map(|(_, path)| (path, true))
            .collect::<Vec<_>>()
    );
}

#[test]
fn sleep_derives_the_same_subscriptions_and_forge_demand_as_a_condition() {
    let actual = [
        "ci=passed",
        "!pr.queue=queued",
        "pr[main]=merged",
        "team.stage=Done",
        "window.5h.left>=40",
    ]
    .map(|term| {
        let sleep: WorkflowSpec = toml::from_str(&format!("{SLEEP}when=['{term}']\n")).unwrap();
        let condition: WorkflowSpec = toml::from_str(&format!(
            "[workflow]\non-break='@failed'\nscope='worktree'\nstart='cooldown'\n\
             [checks.cooldown]\ncases=[{{when=['{term}'],outcome='then'}}]\nelse='sleeping'\n"
        ))
        .unwrap();
        [sleep, condition].map(|spec| {
            let selectors = spec.checks["cooldown"]
                .subscriptions()
                .unwrap()
                .iter()
                .map(|(selector, _)| selector.to_string())
                .collect::<Vec<_>>();
            (selectors, spec.forge_branches().unwrap())
        })
    });
    assert!(
        actual.iter().all(|[sleep, condition]| sleep == condition),
        "{actual:?}"
    );
}
