use super::*;
use crate::sidebar::refresh::pr::PrLink;

fn cache(ci: Option<WorktreePrCi>, state: WorktreePrState) -> PrStateCache {
    let mut cache = PrStateCache::default();
    cache.states.insert(
        "/repo/feat-x".to_owned(),
        PrLink {
            branch: Some("feat-x".to_owned()),
            incarnation: None,
            state,
            number: Some(91),
            url: None,
            ci,
            merge_sha: None,
        },
    );
    cache
}

fn resolve_cache(selector: &str, matches: &[(&str, &str)], cache: &PrStateCache) -> WaitStatus {
    from_cache(
        &selector.parse().unwrap(),
        &matches
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect(),
        cache,
    )
}

#[test]
fn deadline_distinguishes_sibling_answers_from_open_and_matching_verdicts() {
    for (selector, ci, pr, answer, expected) in [
        (
            "ci.failed",
            Some(WorktreePrCi::Passing),
            WorktreePrState::Open,
            Some("ci.passed"),
            "ci passing",
        ),
        (
            "ci.failed",
            Some(WorktreePrCi::Pending),
            WorktreePrState::Open,
            None,
            "ci pending",
        ),
        (
            "ci.failed",
            Some(WorktreePrCi::Failing),
            WorktreePrState::Open,
            None,
            "ci failing",
        ),
        (
            "ci.*",
            Some(WorktreePrCi::Passing),
            WorktreePrState::Open,
            None,
            "ci passing",
        ),
        ("ci.failed", None, WorktreePrState::Open, None, "no CI seen"),
        (
            "pr.merged",
            None,
            WorktreePrState::Closed,
            Some("pr.closed"),
            "pr closed",
        ),
        ("pr.merged", None, WorktreePrState::Open, None, "pr open"),
        (
            "pr.merged",
            None,
            WorktreePrState::Merged,
            None,
            "pr merged",
        ),
    ] {
        let status = resolve_cache(selector, &[("path", "/repo/feat-x")], &cache(ci, pr));
        match (status, answer) {
            (WaitStatus::Answered { label, signal }, Some(answer)) => {
                assert_eq!(label, format!("{expected} on feat-x (PR #91)"));
                assert_eq!(signal.name.as_str(), answer);
                assert_eq!(
                    signal.payload,
                    serde_json::json!({"path":"/repo/feat-x", "branch":"feat-x", "number":91})
                        .as_object()
                        .unwrap()
                        .clone()
                );
            }
            (WaitStatus::Open(Some(view)), None) => {
                assert_eq!(view.headline, "feat-x (PR #91)");
                assert!(
                    view.label
                        .starts_with(&format!("{expected} on feat-x (PR #91)"))
                );
                let matching_terminal = selector == "ci.*"
                    || ci == Some(WorktreePrCi::Failing)
                    || pr == WorktreePrState::Merged;
                assert_eq!(
                    view.label.contains("no matching transition received"),
                    matching_terminal
                );
            }
            _ => panic!("wrong deadline outcome for {selector} {ci:?} {pr:?}"),
        }
    }
}

#[test]
fn deadline_scope_requires_unambiguous_fully_understood_matches() {
    let mut cache = cache(Some(WorktreePrCi::Passing), WorktreePrState::Open);
    assert!(matches!(
        resolve_cache("ci.failed", &[("branch", "feat-x")], &cache),
        WaitStatus::Answered { .. }
    ));
    for matches in [
        vec![("path", "/repo/feat-x"), ("branch", "other")],
        vec![("path", "/repo/feat-x"), ("head", "abc")],
        vec![("path", "/repo/feat-x"), ("number", "92")],
        vec![],
    ] {
        assert!(matches!(
            resolve_cache("ci.failed", &matches, &cache),
            WaitStatus::Open(None)
        ));
    }
    cache.states.insert(
        "/other/feat-x".to_owned(),
        cache.states["/repo/feat-x"].clone(),
    );
    assert!(matches!(
        resolve_cache("ci.failed", &[("branch", "feat-x")], &cache),
        WaitStatus::Open(None)
    ));
}

#[test]
fn branch_ci_and_missing_cache_keep_their_actual_scope() {
    let mut cache = PrStateCache::default();
    for matches in [[("path", "/repo/feat-x")], [("branch", "feat-x")]] {
        let WaitStatus::Open(Some(view)) = resolve_cache("ci.failed", &matches, &cache) else {
            panic!("unknown stays open");
        };
        assert_eq!(view.label, format!("no PR or CI seen on {}", matches[0].1));
    }
    cache
        .branch_ci
        .insert("/repo/feat-x".to_owned(), WorktreePrCi::Passing);
    let WaitStatus::Answered { signal, label } =
        resolve_cache("ci.failed", &[("path", "/repo/feat-x")], &cache)
    else {
        panic!("branch CI answers");
    };
    assert_eq!(label, "ci passing on /repo/feat-x");
    assert_eq!(
        signal.payload,
        serde_json::json!({"path":"/repo/feat-x"})
            .as_object()
            .unwrap()
            .clone()
    );
    assert!(matches!(
        resolve_cache("ci.failed", &[("branch", "feat-x")], &cache),
        WaitStatus::Open(Some(_))
    ));
}

#[test]
fn rearm_preserves_arguments_without_shell_expansion_or_changing_file_scope() {
    let entry = TaskEntry {
        root: "/repo root".into(),
        signal: Some("ci.*".to_owned()),
        matches: Some(BTreeMap::from([
            ("path".to_owned(), "/repo/it\u{27}s $(false)".to_owned()),
            ("branch".to_owned(), "feat;false".to_owned()),
        ])),
        timeout: Some("10m".to_owned()),
        prompt_file: Some("notes/wake.md".into()),
        prompt: Some("not copied".to_owned()),
        ..TaskEntry::default()
    };
    let command = rearm_command(&entry).unwrap();
    assert_eq!(
        shlex::split(&command).unwrap(),
        [
            "rimz",
            "wake",
            "--signal",
            "ci.*",
            "--match",
            "branch=feat;false",
            "--match",
            "path=/repo/it\u{27}s $(false)",
            "--timeout",
            "10m",
            "--prompt-file",
            "notes/wake.md"
        ]
    );
    let entry = TaskEntry {
        signal: Some("ci.failed".to_owned()),
        timeout: Some("59m".to_owned()),
        ..TaskEntry::default()
    };
    assert_eq!(
        rearm_command(&entry).unwrap(),
        "rimz wake --signal ci.failed"
    );
}
