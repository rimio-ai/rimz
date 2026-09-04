use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;

use serde_json::{Map, Value, json};

use super::super::*;
use crate::harness::schedule::signal::SignalSource;

const PATH: &str = "/repo/worktree";
const REPO: &str = "gh:github.com:org/repo";

#[test]
fn open_pr_emits_only_its_terminal_transition() {
    for (state, expected_name) in [
        (WorktreePrState::Merged, "pr.merged"),
        (WorktreePrState::Closed, "pr.closed"),
    ] {
        let prior = pr_cache(WorktreePrState::Open, Some(WorktreePrCi::Pending));
        let next = pr_cache(state, Some(WorktreePrCi::Pending));

        let signals = production_transitions(&prior, &next);

        assert_eq!(signal_names(&signals), vec![expected_name]);
        assert_eq!(signals[0].source, SignalSource::Forge);
        assert_eq!(
            Value::Object(signals[0].payload.clone()),
            json!({
                "path": PATH,
                "branch": "feature",
                "repo": REPO,
                "number": 42,
                "url": "https://github.com/org/repo/pull/42",
                "head": "head-2",
                "state": if state == WorktreePrState::Merged { "merged" } else { "closed" },
            })
        );
    }
}

#[test]
fn pending_ci_emits_success_and_failure_conclusions() {
    for (ci, conclusion) in [
        (WorktreePrCi::Passing, "success"),
        (WorktreePrCi::Failing, "failure"),
    ] {
        let prior = pr_cache(WorktreePrState::Open, Some(WorktreePrCi::Pending));
        let next = pr_cache(WorktreePrState::Open, Some(ci));

        let signals = production_transitions(&prior, &next);

        assert_eq!(signal_names(&signals), vec!["ci.finished"]);
        assert_eq!(signals[0].payload["conclusion"], conclusion);
        assert_eq!(signals[0].payload["head"], "head-2");
    }
}

#[test]
fn changed_final_ci_on_a_new_head_emits_again() {
    let mut prior = pr_cache(WorktreePrState::Open, Some(WorktreePrCi::Passing));
    prior.head_seen.insert(PATH.to_owned(), "head-1".to_owned());
    let next = pr_cache(WorktreePrState::Open, Some(WorktreePrCi::Failing));

    let signals = production_transitions(&prior, &next);

    assert_eq!(signal_names(&signals), vec!["ci.finished"]);
    assert_eq!(signals[0].payload["conclusion"], "failure");
    assert_eq!(signals[0].payload["head"], "head-2");
}

#[test]
fn terminal_pr_and_finished_ci_emit_both_transitions() {
    let prior = pr_cache(WorktreePrState::Open, Some(WorktreePrCi::Pending));
    let next = pr_cache(WorktreePrState::Merged, Some(WorktreePrCi::Passing));

    let signals = production_transitions(&prior, &next);

    assert_eq!(signal_names(&signals), vec!["pr.merged", "ci.finished"]);
}

#[test]
fn first_sight_reset_and_failed_probe_carry_forward_emit_nothing() {
    let next = pr_cache(WorktreePrState::Merged, Some(WorktreePrCi::Passing));
    assert!(production_transitions(&PrStateCache::default(), &next).is_empty());

    let mut reset = pr_cache(WorktreePrState::Open, Some(WorktreePrCi::Pending));
    reset.target_seen.clear();
    assert!(production_transitions(&reset, &next).is_empty());

    let prior = pr_cache(WorktreePrState::Open, Some(WorktreePrCi::Pending));
    let mut carried = prior.clone();
    carried.repos.get_mut(REPO).unwrap().ok = false;
    assert!(production_transitions(&prior, &carried).is_empty());

    let mut failed = next;
    failed.repos.get_mut(REPO).unwrap().ok = false;
    assert!(production_transitions(&prior, &failed).is_empty());
}

#[test]
fn transitions_require_branch_and_incarnation_continuity() {
    let prior = pr_cache(WorktreePrState::Open, Some(WorktreePrCi::Pending));

    let mut changed_branch = pr_cache(WorktreePrState::Merged, Some(WorktreePrCi::Passing));
    changed_branch.target_seen.get_mut(PATH).unwrap().branch = "other".to_owned();
    changed_branch.states.get_mut(PATH).unwrap().branch = Some("other".to_owned());
    assert!(
        production_transitions(&prior, &changed_branch).is_empty(),
        "branch reuse must not replay the old worktree's terminal state"
    );

    let mut changed_incarnation = pr_cache(WorktreePrState::Merged, Some(WorktreePrCi::Passing));
    let incarnation = jiff::Timestamp::from_second(2_000).unwrap();
    changed_incarnation
        .target_seen
        .get_mut(PATH)
        .unwrap()
        .incarnation = Some(incarnation);
    changed_incarnation
        .states
        .get_mut(PATH)
        .unwrap()
        .incarnation = Some(incarnation);
    assert!(
        production_transitions(&prior, &changed_incarnation).is_empty(),
        "path reuse must not replay the prior incarnation's terminal state"
    );
}

#[test]
fn branch_only_ci_uses_cached_target_continuity() {
    let mut prior = base_cache();
    prior
        .branch_ci
        .insert(PATH.to_owned(), WorktreePrCi::Pending);
    let mut next = base_cache();
    next.branch_ci
        .insert(PATH.to_owned(), WorktreePrCi::Passing);

    let signals = production_transitions(&prior, &next);

    assert_eq!(signal_names(&signals), vec!["ci.finished"]);
    assert_eq!(
        Value::Object(signals[0].payload.clone()),
        json!({
            "path": PATH,
            "branch": "feature",
            "repo": REPO,
            "head": "head-2",
            "conclusion": "success",
        })
    );

    next.target_seen.get_mut(PATH).unwrap().branch = "other".to_owned();
    assert!(production_transitions(&prior, &next).is_empty());
}

#[test]
fn tracked_none_to_final_ci_emits_but_identical_final_ci_does_not() {
    let prior = base_cache();
    let mut next = base_cache();
    next.branch_ci
        .insert(PATH.to_owned(), WorktreePrCi::Failing);
    assert_eq!(
        signal_names(&production_transitions(&prior, &next)),
        vec!["ci.finished"]
    );

    assert!(production_transitions(&next, &next).is_empty());
}

#[test]
fn target_stamps_round_trip_with_the_cache() {
    let cache = base_cache();
    let encoded = serde_json::to_vec(&cache).unwrap();
    let decoded: PrStateCache = serde_json::from_slice(&encoded).unwrap();

    assert_eq!(decoded.target_seen, cache.target_seen);
}

#[test]
fn forge_signal_argv_keeps_root_name_and_payload_as_distinct_values() {
    let prior = pr_cache(WorktreePrState::Open, Some(WorktreePrCi::Pending));
    let next = pr_cache(WorktreePrState::Closed, Some(WorktreePrCi::Pending));
    let signal = production_transitions(&prior, &next).remove(0);

    let args = transition_argv(Path::new("/project with spaces"), &signal);

    assert_eq!(
        &args[..8],
        &[
            OsString::from("--root"),
            OsString::from("/project with spaces"),
            OsString::from("events"),
            OsString::from("emit"),
            OsString::from("pr.closed"),
            OsString::from("--source"),
            OsString::from("forge"),
            OsString::from("--json"),
        ]
    );
    let payload: Map<String, Value> =
        serde_json::from_str(args[8].to_str().unwrap()).expect("JSON payload argument");
    assert_eq!(payload, signal.payload);
}

fn production_transitions(
    prior: &PrStateCache,
    next: &PrStateCache,
) -> Vec<crate::harness::schedule::signal::Signal> {
    super::super::transitions::transitions(prior, next)
}

fn signal_names(signals: &[crate::harness::schedule::signal::Signal]) -> Vec<&str> {
    signals.iter().map(|signal| signal.name.as_str()).collect()
}

fn pr_cache(state: WorktreePrState, ci: Option<WorktreePrCi>) -> PrStateCache {
    let mut cache = base_cache();
    cache.states.insert(
        PATH.to_owned(),
        PrLink {
            branch: Some("feature".to_owned()),
            incarnation: None,
            state,
            number: Some(42),
            url: Some("https://github.com/org/repo/pull/42".to_owned()),
            ci,
            merge_sha: (state == WorktreePrState::Merged).then(|| "merge-42".to_owned()),
        },
    );
    cache
}

fn base_cache() -> PrStateCache {
    PrStateCache {
        repos: BTreeMap::from([(
            REPO.to_owned(),
            RepoProbe {
                refreshed_at_ms: 2_000,
                ok: true,
                consecutive_failures: 0,
            },
        )]),
        head_seen: BTreeMap::from([(PATH.to_owned(), "head-2".to_owned())]),
        path_repos: BTreeMap::from([(PATH.to_owned(), REPO.to_owned())]),
        target_seen: BTreeMap::from([(
            PATH.to_owned(),
            TargetStamp {
                branch: "feature".to_owned(),
                incarnation: None,
            },
        )]),
        ..PrStateCache::default()
    }
}
