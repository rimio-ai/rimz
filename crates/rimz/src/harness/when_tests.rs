use super::*;

#[test]
fn pr_number_reads_only_the_scopes_cached_link() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::StatePaths::for_project_root_under(dir.path(), dir.path()).unwrap();
    let runtime = crate::RuntimePaths::for_state_under(&state, dir.path());
    let source = CiSource(
        serde_json::from_value(serde_json::json!({
            "states": {"/scope": {"state":"open", "number":1517}},
        }))
        .unwrap(),
    );
    assert_eq!(
        [
            source.pr_number(Path::new("/scope")),
            source.pr_number(Path::new("/other")),
            CiSource::read(&runtime).pr_number(Path::new("/scope"))
        ],
        [Some(1517), None, None]
    );
}

#[test]
fn ci_commits_only_reads_an_open_pr_with_a_known_head() {
    for (state, head, ci_sha, expected) in [
        ("open", Some("pr"), Some("ci"), Some(("pr", Some("ci")))),
        ("open", Some("pr"), None, Some(("pr", None))),
        ("open", Some("pr"), Some(""), Some(("pr", None))),
        ("open", Some(""), Some("ci"), None),
        ("open", None, Some("ci"), None),
        ("merged", Some("pr"), Some("ci"), None),
        ("closed", Some("pr"), Some("ci"), None),
    ] {
        let source = CiSource(
            serde_json::from_value(serde_json::json!({
                "branch_ci": {"/branch": "passing", "/scope": "passing"},
                "states": {"/scope": {
                    "state": state, "ci": "passing", "ci_sha": ci_sha,
                    "open": head.map(|head| serde_json::json!({"head": head}))
                }}
            }))
            .unwrap(),
        );
        assert_eq!(
            source.ci_commits(Path::new("/scope")),
            expected,
            "{state}, {head:?}, {ci_sha:?}"
        );
        assert_eq!(source.ci_commits(Path::new("/branch")), None);
        let expression = WhenExpr::parse(&["ci=passed".into()]).unwrap();
        for scope in ["/scope", "/branch"] {
            assert!(
                evaluate(
                    &expression,
                    Path::new(scope),
                    Some(&source),
                    None,
                    None,
                    &no_windows()
                )
                .truth
                    == Some(true),
                "loop readings stay unchanged: {state}, {scope}"
            );
        }
    }
}

#[test]
fn workflow_truth_uses_kleene_logic_without_turning_unread_into_false() {
    let and = WhenExpr::parse(&["pr=merged && ci=passed".into()]).unwrap();
    let or = WhenExpr::parse(&["pr=merged || ci=passed".into()]).unwrap();
    let not = WhenExpr::parse(&["!pr=merged".into()]).unwrap();
    for (left, right, conjunction, disjunction) in [
        (Some(true), Some(true), Some(true), Some(true)),
        (Some(true), Some(false), Some(false), Some(true)),
        (Some(true), None, None, Some(true)),
        (Some(false), Some(true), Some(false), Some(true)),
        (Some(false), Some(false), Some(false), Some(false)),
        (Some(false), None, Some(false), None),
        (None, Some(true), None, Some(true)),
        (None, Some(false), Some(false), None),
        (None, None, None, None),
    ] {
        let readings = BTreeMap::from([
            (
                "pr".into(),
                left.map(|yes| if yes { "merged" } else { "open" }.into()),
            ),
            (
                "ci".into(),
                right.map(|yes| if yes { "passed" } else { "failed" }.into()),
            ),
        ]);
        assert_eq!(and.truth(&readings), conjunction, "AND {left:?}, {right:?}");
        assert_eq!(or.truth(&readings), disjunction, "OR {left:?}, {right:?}");
        assert_eq!(not.truth(&readings), left.map(|yes| !yes));
    }
}

#[test]
fn scope_checkout_keys_include_nested_terms_but_not_named_branches_or_windows() {
    for (input, key) in [
        ("pr=merged", Some("pr")),
        ("!ci=failed", Some("ci")),
        (
            "pr[topic]=merged && (window.5h.left<50 || pr.queue=queued)",
            Some("pr.queue"),
        ),
        ("!(pr[topic]=open || team.stage=Done)", Some("team.stage")),
        ("team.stage[topic]=Done && ci[topic]=passed", None),
        (
            "ci[topic]=passed && pr[topic]=merged && pr.queue[topic]=none",
            None,
        ),
        ("window.5h.left<50", None),
        ("pr.mergeable=conflicting", Some("pr.mergeable")),
        (
            "!(pr.review=required || ci[topic]=passed)",
            Some("pr.review"),
        ),
        (
            "pr.mergeable[topic]=mergeable && pr.review[topic]=approved",
            None,
        ),
    ] {
        let stages = BTreeSet::from(["Done".to_owned()]);
        let expr = WhenExpr::parse_with_stages(&[input.to_owned()], Some(&stages)).unwrap();
        assert_eq!(expr.scope_checkout_key(), key, "{input}");
    }
}

#[test]
fn unread_forge_reasons_follow_probe_status_before_empty_rows() {
    use crate::forge::pr_state::{BranchRow, RepoProbe};
    let scope = Path::new("/checkout");
    let mut source = CiSource(PrStateCache::default());
    assert_eq!(
        source.unread_reason("pr[main]", scope),
        Some(UnreadReason::NotChecked)
    );
    source
        .0
        .path_repos
        .insert("/checkout".into(), "<unsupported>".into());
    assert_eq!(
        source.unread_reason("pr", scope),
        Some(UnreadReason::NoForge)
    );
    source.0.repos.insert(
        "<unsupported>".into(),
        RepoProbe {
            refreshed_at_ms: 1_000,
            ok: true,
            consecutive_failures: 0,
        },
    );
    assert_eq!(source.checked_at(scope), None);
    source
        .0
        .path_repos
        .insert("/checkout".into(), String::new());
    assert_eq!(
        source.unread_reason("pr", scope),
        Some(UnreadReason::NoForge)
    );
    source
        .0
        .path_repos
        .insert("/checkout".into(), "repo".into());
    source.0.repos.insert(
        "repo".into(),
        RepoProbe {
            refreshed_at_ms: 1_000,
            ok: false,
            consecutive_failures: 1,
        },
    );
    source.0.branches.insert(
        "repo".into(),
        BTreeMap::from([("main".into(), BranchRow::default())]),
    );
    assert_eq!(
        source.unread_reason("pr[main]", scope),
        Some(UnreadReason::LookupFailing { last_tried: 1_000 })
    );
    assert_eq!(source.checked_at(scope), None);
    source.0.repos.get_mut("repo").unwrap().ok = true;
    assert_eq!(
        source.checked_at(scope),
        Some(Timestamp::from_millisecond(1_000).unwrap())
    );
    for key in ["pr[main]", "pr.queue[main]", "ci[main]"] {
        assert_eq!(
            source.probe_unread_reason(key, scope),
            Some(UnreadReason::NoPr {
                branch: Some("main".into())
            })
        );
    }
    assert_eq!(
        source.probe_unread_reason("pr", scope),
        Some(UnreadReason::NoPr { branch: None })
    );
    // A successful probe that found nothing is the reading `none`, so nothing is unread.
    assert_eq!(source.unread_reason("pr", scope), None);
    assert_eq!(
        source.probe_unread_reason("ci", scope),
        Some(UnreadReason::NoCi { branch: None })
    );
    source.0.states.insert(
        "/checkout".into(),
        serde_json::from_value(serde_json::json!({"state": "open"})).unwrap(),
    );
    source
        .0
        .branches
        .get_mut("repo")
        .unwrap()
        .get_mut("main")
        .unwrap()
        .pr = source.0.states.get("/checkout").cloned();
    assert_eq!(
        source.probe_unread_reason("ci", scope),
        Some(UnreadReason::NoCi { branch: None })
    );
    assert_eq!(
        source.probe_unread_reason("ci[main]", scope),
        Some(UnreadReason::NoCi {
            branch: Some("main".into())
        })
    );
    source
        .0
        .branch_ci
        .insert("/checkout".into(), WorktreeCi::Passing);
    assert_eq!(source.unread_reason("ci", scope), None);
    assert_eq!(source.unread_reason("team.stage", scope), None);
}

#[test]
fn unread_pr_facts_name_the_open_pr_not_ci() {
    use serde_json::json;

    let scope = Path::new("/checkout");
    let mut reasons = Vec::new();
    let mut expected = Vec::new();
    for state in ["open", "merged", "closed"] {
        let source = CiSource(
            serde_json::from_value(json!({
                "branch_ci": {}, "path_repos": {"/checkout": "repo"},
                "repos": {"repo": {"ok": true, "refreshed_at_ms": 1_000}},
                "states": {"/checkout": {"state": state}},
                "branches": {"repo": {"main": {"pr": {"state": state}}}}
            }))
            .unwrap(),
        );
        for key in ["pr.mergeable", "pr.review", "pr.queue"] {
            for branch in [false, true] {
                let key = if branch {
                    format!("{key}[main]")
                } else {
                    key.into()
                };
                let reason = super::condition::unread_reason(
                    source.probe_unread_reason(&key, scope),
                    Some(Timestamp::from_millisecond(1_000).unwrap()),
                );
                reasons.push((key.clone(), state, reason));
                let reason = match (state, branch) {
                    ("open", false) => "not reported for this checkout's open PR",
                    ("open", true) => "not reported for branch main's open PR",
                    (_, false) => "no open PR for this checkout",
                    (_, true) => "no open PR for branch main",
                };
                expected.push((key, state, reason.to_owned()));
            }
        }
        assert_eq!(
            source.probe_unread_reason("ci", scope),
            Some(UnreadReason::NoCi { branch: None })
        );
    }
    assert_eq!(reasons, expected);
}

fn no_windows() -> WindowReadings<'static> {
    WindowReadings::fixed(Timestamp::now(), BTreeMap::new())
}

#[test]
fn unread_terms_are_unknown_under_negation_and_inequality() {
    let scope = tempfile::tempdir().unwrap();
    for (input, unread, read) in [
        ("ci!=failed", None, Some(true)),
        ("!ci=failed", None, Some(true)),
        ("ci=passed,pending", None, Some(true)),
        ("team.stage!=Done", None, Some(false)),
        ("!team.stage=Done", None, Some(false)),
    ] {
        let stages = BTreeSet::from(["Done".to_owned()]);
        let expr = WhenExpr::parse_with_stages(&[input.to_owned()], Some(&stages)).unwrap();
        assert_eq!(
            evaluate(&expr, scope.path(), None, None, None, &no_windows()).truth,
            unread,
            "{input}"
        );
        std::fs::create_dir_all(scope.path().join(".rimz/team")).unwrap();
        std::fs::write(
            scope.path().join(".rimz/team/blackboard.md"),
            "Stage: Done\n",
        )
        .unwrap();
        let source = CiSource(PrStateCache {
            branch_ci: BTreeMap::from([(scope.path().display().to_string(), WorktreeCi::Passing)]),
            ..Default::default()
        });
        assert_eq!(
            evaluate(
                &expr,
                scope.path(),
                Some(&source),
                None,
                None,
                &no_windows()
            )
            .truth,
            read,
            "{input}"
        );
        std::fs::remove_file(scope.path().join(".rimz/team/blackboard.md")).unwrap();
    }
}

#[test]
fn unknown_is_not_a_value_any_term_can_match() {
    let stages = BTreeSet::from(["Done".to_owned()]);
    for input in [
        "ci=unknown",
        "ci!=unknown",
        "pr=open,unknown",
        "pr.queue[b]=unknown",
        "pr.mergeable=unknown",
        "pr.review!=unknown",
        "team.stage=unknown",
        "team.stage[b]=Done,unknown",
    ] {
        for stages in [None, Some(&stages)] {
            let error = WhenExpr::parse_with_stages(&[input.to_owned()], stages)
                .unwrap_err()
                .to_string();
            assert!(
                error.ends_with(
                    "unknown is not a value: a term over an unread key is unknown by itself; \
                     match a value the key reads"
                ),
                "{input}: {error}"
            );
        }
    }
    for key in ["ci", "pr", "pr.queue", "pr.mergeable", "pr.review"] {
        let error = WhenExpr::parse(&[format!("{key}=invalid")])
            .unwrap_err()
            .to_string();
        assert!(
            !error.contains("unknown,") && !error.ends_with("unknown"),
            "{error}"
        );
        assert!(error.contains(", none"), "{error}");
    }
}

#[test]
fn inequality_display_and_evidence() {
    let parsed = WhenExpr::parse(&["ci != passed , pending".to_owned()]);
    assert!(parsed.is_ok(), "inequality must parse: {parsed:?}");
    let expr = parsed.unwrap();
    assert_eq!(expr.to_string(), "ci!=passed,pending");
    assert_eq!(WhenExpr::parse(&[expr.to_string()]).unwrap(), expr);
    let expr = WhenExpr::parse(&["ci!=failed".to_owned()]).unwrap();
    let term = expr.terms().next().unwrap();
    assert_eq!(term.comparison_evidence("failed"), "ci=failed=failed");
    assert_eq!(term.comparison_evidence("passed"), "ci=passed!=failed");
}

#[test]
fn pr_reads_link_state_and_requires_a_scoped_link() {
    let scope = Path::new("/checkout");
    for state in ["open", "merged", "closed"] {
        let parsed = WhenExpr::parse(&[format!("pr={state}")]);
        assert!(parsed.is_ok(), "pr state must parse: {parsed:?}");
        let expr = parsed.unwrap();
        let cache = CiSource(PrStateCache {
            states: BTreeMap::from([(
                scope.display().to_string(),
                serde_json::from_value(serde_json::json!({"state": state})).unwrap(),
            )]),
            ..PrStateCache::default()
        });
        let verdict = evaluate(&expr, scope, Some(&cache), None, None, &no_windows());
        assert_eq!(verdict.truth, Some(true));
        assert_eq!(verdict.readings["pr"].as_deref(), Some(state));
        for source in [None, Some(&cache)] {
            let verdict = evaluate(
                &expr,
                Path::new("/missing"),
                source,
                None,
                None,
                &no_windows(),
            );
            assert_eq!(verdict.truth, None);
            assert_eq!(verdict.readings["pr"], None);
        }
    }
    let error = WhenExpr::parse(&["pr=pending".to_owned()]).unwrap_err();
    assert!(error.to_string().contains("values: open, merged, closed"));
    let error = WhenExpr::parse(&["unknown=value".to_owned()]).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("team.stage, ci, pr, pr.queue, pr.mergeable, pr.review,")
    );
}

#[test]
fn pr_mergeable_requires_a_settled_answer_at_the_current_open_head() {
    use serde_json::json;

    let scope = Path::new("/checkout");
    let mut readings = Vec::new();
    let mut expected = Vec::new();
    for (state, head, settled, reading) in [
        (
            "open",
            "head-a",
            Some(("mergeable", "head-a")),
            Some("mergeable"),
        ),
        (
            "open",
            "head-a",
            Some(("conflicting", "head-a")),
            Some("conflicting"),
        ),
        ("open", "head-b", Some(("mergeable", "head-a")), None),
        ("open", "head-b", Some(("conflicting", "head-a")), None),
        ("open", "head-a", None, None),
        ("open", "", Some(("conflicting", "")), None),
        ("merged", "head-a", Some(("conflicting", "head-a")), None),
        ("closed", "head-a", Some(("mergeable", "head-a")), None),
    ] {
        let mergeability = settled.map(|(state, head)| json!({"state": state, "head": head}));
        let open = json!({"head": head, "mergeability": mergeability});
        let link = json!({"state": state, "open": open});
        let mut source = CiSource(
            serde_json::from_value(json!({
                "branch_ci": {}, "path_repos": {"/checkout": "repo"},
                "states": {"/checkout": link},
                "branches": {"repo": {"topic": {"pr": link}}}
            }))
            .unwrap(),
        );
        for key in ["pr.mergeable", "pr.mergeable[topic]"] {
            let result = WhenExpr::parse(&[format!("{key}=conflicting")])
                .map(|expr| {
                    evaluate(&expr, scope, Some(&source), None, None, &no_windows()).readings[key]
                        .clone()
                })
                .map_err(|error| error.to_string());
            readings.push(result);
            expected.push(Ok(reading.map(str::to_owned)));
            source.0.states.clear();
        }
    }
    for link in [json!({"state": "open"}), json!(null)] {
        let source = CiSource(
            serde_json::from_value(json!({"branch_ci": {},
                "states": if link.is_null() { json!({}) } else { json!({"/checkout": link}) }
            }))
            .unwrap(),
        );
        for source in [None, Some(&source)] {
            let reading = WhenExpr::parse(&["pr.mergeable=mergeable".into()])
                .map(|expr| {
                    let verdict = evaluate(&expr, scope, source, None, None, &no_windows());
                    verdict.readings["pr.mergeable"].clone()
                })
                .map_err(|error| error.to_string());
            readings.push(reading);
            expected.push(Ok(None));
        }
    }
    assert_eq!(readings, expected);
}

#[test]
fn pr_review_keeps_never_fetched_distinct_from_none() {
    use serde_json::json;

    let scope = Path::new("/checkout");
    let mut readings = Vec::new();
    let mut expected = Vec::new();
    for state in ["open", "merged", "closed"] {
        for review in [
            Some("draft"),
            Some("changes_requested"),
            Some("required"),
            Some("approved"),
            Some("none"),
            None,
        ] {
            let link = json!({"state": state, "open": {"head": "head-a", "review": review}});
            let mut source = CiSource(
                serde_json::from_value(json!({
                    "branch_ci": {}, "path_repos": {"/checkout": "repo"},
                    "states": {"/checkout": link},
                    "branches": {"repo": {"topic": {"pr": link}}}
                }))
                .unwrap(),
            );
            for key in ["pr.review", "pr.review[topic]"] {
                let value = review.unwrap_or("none");
                readings.push(
                    WhenExpr::parse(&[format!("{key}={value}")])
                        .map(|expr| {
                            let verdict =
                                evaluate(&expr, scope, Some(&source), None, None, &no_windows());
                            (verdict.readings[key].clone(), verdict.truth)
                        })
                        .map_err(|error| error.to_string()),
                );
                let reading = review.filter(|_| state == "open");
                expected.push(Ok((reading.map(str::to_owned), reading.map(|_| true))));
                source.0.states.clear();
            }
        }
    }
    let expr = WhenExpr::parse(&["pr.review=approved".into()]);
    let factless = CiSource(
        serde_json::from_value(json!({"branch_ci": {},
            "states": {"/checkout": {"state": "open"}}
        }))
        .unwrap(),
    );
    for source in [
        None,
        Some(&CiSource(PrStateCache::default())),
        Some(&factless),
    ] {
        readings.push(
            expr.as_ref()
                .map(|expr| {
                    let verdict = evaluate(expr, scope, source, None, None, &no_windows());
                    (verdict.readings["pr.review"].clone(), verdict.truth)
                })
                .map_err(|error| error.to_string()),
        );
        expected.push(Ok((None, None)));
    }
    assert_eq!(readings, expected);
}

#[test]
fn pr_fact_value_refusals_list_the_readings() {
    let cases = [
        ("pr.mergeable", "mergeable, conflicting, none"),
        (
            "pr.review",
            "draft, changes_requested, required, approved, none",
        ),
    ];
    let errors = cases.map(|(key, _)| {
        WhenExpr::parse(&[format!("{key}=invalid")])
            .unwrap_err()
            .to_string()
    });
    assert_eq!(
        errors
            .each_ref()
            .map(|error| error.split_once("unknown ").unwrap().1),
        cases.map(|(key, values)| format!("{key} value; values: {values}"))
    );
}

#[test]
fn pr_queue_reads_the_open_link_fact_and_leaves_pr_open() {
    use serde_json::json;
    let scope = Path::new("/checkout");
    let queued = json!({"state": "queued", "at": "2026-10-03T12:28:46Z"});
    let dequeued = json!({"state": "dequeued", "at": "2026-10-03T12:46:03Z", "reason": "manual"});
    for (link, pr, queue) in [
        (
            json!({"state": "open", "open": {"head": "a", "queue": queued}}),
            "open",
            "queued",
        ),
        (
            json!({"state": "open", "open": {"head": "a", "queue": dequeued}}),
            "open",
            "dequeued",
        ),
        (
            json!({"state": "open", "open": {"head": "a"}}),
            "open",
            "none",
        ),
        (json!({"state": "open"}), "open", "none"),
        // An auto-merge request reads armed only below a queue fact.
        (
            json!({"state": "open", "open": {"head": "a", "auto_merge": true}}),
            "open",
            "armed",
        ),
        (
            json!({"state": "open", "open": {"head": "a", "auto_merge": true, "queue": queued}}),
            "open",
            "queued",
        ),
        (
            json!({"state": "open", "open": {"head": "a", "auto_merge": true, "queue": dequeued}}),
            "open",
            "dequeued",
        ),
        // A fact left on a link that is no longer open is not a reading.
        (
            json!({"state": "merged", "open": {"head": "a", "queue": queued}}),
            "merged",
            "none",
        ),
        (
            json!({"state": "closed", "open": {"head": "a", "auto_merge": true}}),
            "closed",
            "none",
        ),
        (json!({"state": "closed"}), "closed", "none"),
    ] {
        let source = CiSource(PrStateCache {
            states: BTreeMap::from([(
                "/checkout".to_owned(),
                serde_json::from_value(link).unwrap(),
            )]),
            ..PrStateCache::default()
        });
        let expr = WhenExpr::parse(&[format!("pr={pr} && pr.queue={queue}")]).unwrap();
        let verdict = evaluate(&expr, scope, Some(&source), None, None, &no_windows());
        assert_eq!(verdict.truth, Some(true), "{:?}", verdict.readings);
        assert_eq!(verdict.readings["pr.queue"].as_deref(), Some(queue));
        for (scope, source) in [(scope, None), (Path::new("/missing"), Some(&source))] {
            let verdict = evaluate(&expr, scope, source, None, None, &no_windows());
            assert_eq!(
                (verdict.truth, &verdict.readings["pr.queue"]),
                (None, &None)
            );
        }
    }
    let armed = CiSource(
        serde_json::from_value(json!({
            "branch_ci": {}, "path_repos": {"/checkout": "repo"},
            "branches": {"repo": {
                "topic": {"pr": {"state": "open", "open": {"head": "a", "auto_merge": true}}}
            }}
        }))
        .unwrap(),
    );
    let expr = WhenExpr::parse(&["pr.queue[topic]=armed".to_owned()]).unwrap();
    assert_eq!(
        evaluate(&expr, scope, Some(&armed), None, None, &no_windows()).truth,
        Some(true)
    );
    let reads_forge = |when: &str| WhenExpr::parse(&[when.to_owned()]).unwrap().reads_forge();
    assert!(reads_forge("pr.queue=queued,dequeued") && reads_forge("ci=passed || team.stage=Done"));
    assert!(!reads_forge("team.stage=Done && window.5h.left>=40"));
    let error = WhenExpr::parse(&["pr.queue=failed".to_owned()]).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unknown pr.queue value; values: queued, dequeued, armed, none")
    );
}

#[test]
fn whole_expression_node_limit_includes_flat_chains_and_clauses() {
    for separator in [" && ", " || "] {
        assert!(WhenExpr::parse(&[vec!["ci=passed"; 64].join(separator)]).is_ok());
        let error = WhenExpr::parse(&[vec!["ci=passed"; 65].join(separator)]).unwrap_err();
        assert!(error.to_string().contains("simplify the condition"));
    }
    assert!(WhenExpr::parse(&vec!["ci=passed".to_owned(); 64]).is_ok());
    assert!(WhenExpr::parse(&vec!["ci=passed".to_owned(); 65]).is_err());
    assert!(WhenExpr::parse(&[vec!["!ci=passed"; 43].join(" && ")]).is_ok());
    assert!(WhenExpr::parse(&[vec!["!ci=passed"; 44].join(" && ")]).is_err());
}

#[test]
fn ci_link_fallback_mapping_and_run_dir_key() {
    use crate::config::TaskEntry;
    use crate::forge::pr_state::PrLink;
    use crate::store::snapshot::{WorktreeCi, WorktreePrState};

    let root = tempfile::tempdir().unwrap();
    let scope = tempfile::tempdir().unwrap();
    let runtime = crate::RuntimePaths::under(
        crate::ids::WorkspaceId::from_project_root(root.path()),
        root.path(),
    )
    .unwrap();
    runtime.ensure_dirs().unwrap();
    let entry = TaskEntry {
        root: root.path().to_owned(),
        dir: Some(scope.path().to_owned()),
        ..TaskEntry::default()
    };
    // The producer copies the worktree's absolute path into Target::path unchanged.
    let key = scope.path().to_string_lossy().into_owned();
    for (ci, label) in [
        (WorktreeCi::Passing, "passed"),
        (WorktreeCi::Failing, "failed"),
        (WorktreeCi::Pending, "pending"),
    ] {
        for state in [
            WorktreePrState::Open,
            WorktreePrState::Merged,
            WorktreePrState::Closed,
        ] {
            let link: PrLink =
                serde_json::from_value(serde_json::json!({"state": state, "ci": ci})).unwrap();
            let cache = CiSource(PrStateCache {
                states: BTreeMap::from([(key.clone(), link)]),
                branch_ci: BTreeMap::from([(key.clone(), WorktreeCi::Pending)]),
                ..PrStateCache::default()
            });
            crate::disk::atomic::write_temp_then_rename_cache(&runtime.pr_state_path(), &cache.0)
                .unwrap();
            let cache = CiSource::read(&runtime);
            let expected = if state == WorktreePrState::Closed {
                "pending"
            } else {
                label
            };
            let expr = WhenExpr::parse(&[format!("ci={expected}")]).unwrap();
            let verdict = evaluate(
                &expr,
                &entry.run_dir(),
                Some(&cache),
                None,
                None,
                &no_windows(),
            );
            assert_eq!(verdict.truth, Some(true), "{state:?} {ci:?}");
            assert_eq!(verdict.readings["ci"].as_deref(), Some(expected));
            let run_dir = entry.run_dir();
            for (scope, cache) in [(root.path(), Some(&cache)), (run_dir.as_path(), None)] {
                let verdict = evaluate(&expr, scope, cache, None, None, &no_windows());
                assert_eq!(verdict.truth, None, "{state:?} {ci:?}");
            }
        }
    }
    let expr = WhenExpr::parse(&["ci=passed".to_owned()]).unwrap();
    assert_eq!(
        evaluate(
            &expr,
            &entry.run_dir(),
            Some(&CiSource(PrStateCache::default())),
            None,
            None,
            &no_windows(),
        )
        .readings["ci"],
        None
    );
}

#[test]
fn readings_and_kleene_truth_keep_unread_negation_unknown() {
    let scope = tempfile::tempdir().unwrap();
    for input in ["ci=failed", "!ci=failed", "team.stage=Done"] {
        let expr = WhenExpr::parse(&[input.to_owned()]).unwrap();
        assert_eq!(
            evaluate(&expr, scope.path(), None, None, None, &no_windows()).truth,
            None,
            "{input}"
        );
    }
    std::fs::create_dir_all(scope.path().join(".rimz/team")).unwrap();
    std::fs::write(
        scope.path().join(".rimz/team/blackboard.md"),
        "Stage: Done\n",
    )
    .unwrap();
    let cache = CiSource(PrStateCache {
        branch_ci: BTreeMap::from([(
            scope.path().to_string_lossy().into_owned(),
            crate::store::snapshot::WorktreeCi::Pending,
        )]),
        ..PrStateCache::default()
    });
    for (input, expected) in [
        ("ci=passed,pending", true),
        ("team.stage=Done && ci=passed", false),
        ("team.stage=Done || ci=passed", true),
        ("!team.stage=Done && ci=pending", false),
        ("ci=passed && ci=failed || team.stage=Done", true),
        ("ci=passed && (ci=failed || team.stage=Done)", false),
    ] {
        let expr = WhenExpr::parse(&[input.to_owned()]).unwrap();
        assert_eq!(
            evaluate(&expr, scope.path(), Some(&cache), None, None, &no_windows()).truth,
            Some(expected),
            "{input}"
        );
    }
    let expr = WhenExpr::parse(&["team.stage=Done && ci=pending".to_owned()]).unwrap();
    assert_eq!(
        evaluate(&expr, scope.path(), None, None, None, &no_windows()).readings,
        BTreeMap::from([
            ("team.stage".to_owned(), Some("Done".to_owned())),
            ("ci".to_owned(), None),
        ])
    );
}

#[test]
fn grammar_and_canonical_display() {
    for (clauses, expected) in [
        (vec!["ci=passed"], "ci=passed"),
        (vec![" ci = passed , pending "], "ci=passed,pending"),
        (
            vec!["team.stage=Done && ci=passed || ci=pending"],
            "team.stage=Done && ci=passed || ci=pending",
        ),
        (
            vec!["!ci=failed && team.stage=Done"],
            "!ci=failed && team.stage=Done",
        ),
        (
            vec!["!(ci=failed || ci=pending)"],
            "!(ci=failed || ci=pending)",
        ),
        (vec!["((!!ci=passed))"], "!!ci=passed"),
        (
            vec!["ci=passed || ci=pending", "team.stage=Done"],
            "(ci=passed || ci=pending) && team.stage=Done",
        ),
        (vec![" window.5h.left >= 40 "], "window.5h.left>=40"),
        (
            vec!["team.stage[feat/x] = Done,Review"],
            "team.stage[feat/x]=Done,Review",
        ),
        (
            vec!["window.7d.left<10 || !window.5h.left>90"],
            "window.7d.left<10 || !window.5h.left>90",
        ),
        (
            vec!["window.5h.left<=0 && ci=passed", "window.7d.left>100"],
            "window.5h.left<=0 && ci=passed && window.7d.left>100",
        ),
    ] {
        let clauses = clauses.into_iter().map(str::to_owned).collect::<Vec<_>>();
        let expr = WhenExpr::parse(&clauses).unwrap();
        assert_eq!(expr.to_string(), expected);
        assert_eq!(
            WhenExpr::parse(&[expected.to_owned()]).unwrap().to_string(),
            expected
        );
    }
}

#[test]
fn refusals_name_clause_position_and_fix() {
    for (input, hint) in [
        (
            "agent=idle",
            "team.stage, ci, pr, pr.queue, pr.mergeable, pr.review, window.5h.left, window.7d.left",
        ),
        (
            "window.1h.left>=40",
            "team.stage, ci, pr, pr.queue, pr.mergeable, pr.review, window.5h.left, window.7d.left",
        ),
        ("ci=green", "passed, failed, pending"),
        ("", "empty term"),
        ("ci=", "empty term"),
        ("ci=passed,", "empty term"),
        ("(ci=passed", "parenthes"),
        ("ci=passed)", "parenthes"),
        ("ci=passed &&", "dangling operator"),
        ("ci=passed ||", "dangling operator"),
        ("!", "dangling operator"),
    ] {
        let error = WhenExpr::parse(&["ci=passed".to_owned(), input.to_owned()])
            .unwrap_err()
            .to_string();
        assert!(error.contains("clause 2"), "{error}");
        assert!(error.contains("position"), "{error}");
        assert!(error.contains(hint), "{input}: {error}");
    }
    let error = WhenExpr::parse(&["agent=idle".to_owned()])
        .unwrap_err()
        .to_string();
    assert!(
        error
            .contains("team.stage, ci, pr, pr.queue, pr.mergeable, pr.review also accept [branch]"),
        "{error}"
    );
    assert!(WhenExpr::parse(&[]).is_err());
}

#[test]
fn window_refusals_suggest_a_form_that_parses() {
    for (input, hint) in [
        (
            "window.5h.left=40",
            "window.5h.left>=40 && window.5h.left<=40",
        ),
        (
            "window.5h.left!=40",
            "window.5h.left<40 || window.5h.left>40",
        ),
        ("window.7d.left>=101", "window.7d.left>=100"),
        ("window.7d.left<-5", "window.7d.left<0"),
        ("window.5h.left>=lots", "window.5h.left>=40"),
        ("window.5h.left", "window.5h.left>=40"),
        ("ci>=passed", "ci=passed"),
        ("team.stage<Done", "team.stage=Done"),
    ] {
        let error = WhenExpr::parse(&["ci=passed".to_owned(), input.to_owned()])
            .unwrap_err()
            .to_string();
        assert!(error.contains("clause 2, position"), "{input}: {error}");
        assert!(error.ends_with(&format!("use {hint}")), "{input}: {error}");
        assert!(WhenExpr::parse(&[hint.to_owned()]).is_ok(), "{hint}");
    }
    let error = WhenExpr::parse(&["window.5h.left!=40".to_owned()])
        .unwrap_err()
        .to_string();
    assert!(!error.contains("!window"), "{error}");
}

#[test]
fn window_terms_read_percent_left_at_the_boundaries() {
    let scope = tempfile::tempdir().unwrap();
    let now = Timestamp::now();
    let window = |used, resets_in: i64, span: WindowSpan| RateLimitWindow {
        used_percentage: used,
        resets_at: Some(now + jiff::SignedDuration::from_secs(resets_in)),
        duration_mins: Some(span.minutes()),
        ..RateLimitWindow::default()
    };
    let claude = AgentKind::new_unchecked("claude");
    let codex = AgentKind::new_unchecked("codex");
    let windows = WindowReadings::fixed(
        now,
        BTreeMap::from([
            (
                LoginKey::default_for(claude.clone()),
                ProviderCapacity::from_windows(vec![
                    window(Some(60), 3_600, WindowSpan::FiveHour),
                    window(None, 86_400, WindowSpan::SevenDay),
                ]),
            ),
            (
                LoginKey::new(claude.clone(), "work".parse().unwrap()),
                ProviderCapacity::from_windows(vec![window(Some(10), 3_600, WindowSpan::FiveHour)]),
            ),
            (
                LoginKey::default_for(codex.clone()),
                ProviderCapacity::from_windows(vec![
                    RateLimitWindow {
                        lifted: true,
                        resets_at: None,
                        ..window(None, 0, WindowSpan::FiveHour)
                    },
                    window(Some(70), -60, WindowSpan::SevenDay),
                ]),
            ),
        ]),
    );
    let read = |input: &str, provider: Option<&AgentKind>, account: Option<&str>| {
        let expr = WhenExpr::parse(&[input.to_owned()]).unwrap();
        let account = account.map(|name| name.parse().unwrap());
        evaluate(
            &expr,
            scope.path(),
            None,
            provider,
            account.as_ref(),
            &windows,
        )
    };
    let check = |input: &str, provider: Option<&AgentKind>| read(input, provider, None);
    for (input, expected) in [
        ("window.5h.left>=40", Some(true)),
        ("window.5h.left>40", Some(false)),
        ("window.5h.left<=40", Some(true)),
        ("window.5h.left<40", Some(false)),
        ("window.5h.left>=41", Some(false)),
        ("window.7d.left>=0", None),
        ("!window.7d.left>=0", None),
        ("window.7d.left<=100 || window.5h.left>39", Some(true)),
        ("window.7d.left<=100 && window.5h.left>40", Some(false)),
    ] {
        assert_eq!(
            check(input, Some(&claude)).truth,
            expected,
            "claude {input}"
        );
    }
    let verdict = check("window.5h.left>=40 && window.7d.left<10", Some(&claude));
    assert_eq!(
        verdict.readings,
        BTreeMap::from([
            ("window.5h.left".to_owned(), Some("40".to_owned())),
            ("window.7d.left".to_owned(), None),
        ])
    );
    let verdict = check("window.5h.left>=100 && window.7d.left>=100", Some(&codex));
    assert_eq!(
        verdict.truth,
        Some(true),
        "a lifted window and a rolled window are full"
    );
    assert_eq!(check("window.5h.left>=0", None).truth, None);
    assert_eq!(
        check("window.5h.left>=0", Some(&AgentKind::new_unchecked("pi"))).truth,
        None
    );
    // A row's account reads its own stored window, never the room's.
    for (pin, left) in [
        (Some("default"), Some("40")),
        (Some("work"), Some("90")),
        (Some("idle"), None),
    ] {
        assert_eq!(
            read("window.5h.left>=0", Some(&claude), pin).readings["window.5h.left"].as_deref(),
            left,
            "{pin:?}"
        );
    }
}

#[test]
fn window_readings_read_nothing_until_a_window_term_asks() {
    let root = tempfile::tempdir().unwrap();
    let runtime = crate::RuntimePaths::under(
        crate::ids::WorkspaceId::from_project_root(root.path()),
        root.path(),
    )
    .unwrap();
    let windows = WindowReadings::new(Some(&runtime), Timestamp::now());
    let claude = AgentKind::new_unchecked("claude");
    let expr = WhenExpr::parse(&["ci=passed".to_owned()]).unwrap();
    evaluate(&expr, root.path(), None, Some(&claude), None, &windows);
    assert!(windows.capacities.get().is_none());
    let expr = WhenExpr::parse(&["window.5h.left>=40".to_owned()]).unwrap();
    let verdict = evaluate(&expr, root.path(), None, Some(&claude), None, &windows);
    assert_eq!(verdict.readings["window.5h.left"], None);
    assert!(windows.capacities.get().is_some());
}

#[test]
fn branch_selectors_read_only_the_scopes_repository() {
    let source = CiSource(serde_json::from_value(serde_json::json!({
        "branch_ci": {},
        "path_repos": {"/scope": "repo", "/unsupported": "<unsupported>"},
        "branches": {"repo": {
            "feat/a": {"pr": {"state": "merged", "ci": "passing"}, "ci": "failing"},
            "b": {"pr": {"state": "open", "open": {"head": "sha", "queue": {"state": "queued", "at": "now"}}}, "ci": "pending"},
            "closed": {"pr": {"state": "closed", "ci": "passing"}, "ci": "failing"}
        }}
    })).unwrap());
    let input = "pr[feat/a]=merged && ci[feat/a]=passed && pr.queue[b]=queued && ci[b]=pending && ci[closed]=failed";
    let parsed = WhenExpr::parse(&[input.to_owned()]);
    assert!(parsed.is_ok(), "branch selectors must parse: {parsed:?}");
    let expr = parsed.unwrap();
    assert_eq!(expr.to_string(), input);
    assert_eq!(WhenExpr::parse(&[expr.to_string()]).unwrap(), expr);
    assert!(expr.reads_forge());
    assert_eq!(
        evaluate(
            &expr,
            Path::new("/scope"),
            Some(&source),
            None,
            None,
            &no_windows()
        )
        .truth,
        Some(true)
    );
    for scope in ["/unclassified", "/unsupported"] {
        let verdict = evaluate(
            &expr,
            Path::new(scope),
            Some(&source),
            None,
            None,
            &no_windows(),
        );
        assert!(verdict.readings.values().all(Option::is_none), "{scope}");
    }
    // No probe has succeeded for the repository, so an absent branch is unread, not `none`.
    let expr = WhenExpr::parse(&[
        "!pr[absent]=open || ci[absent]=none || pr.queue[absent]=none".to_owned(),
    ])
    .unwrap();
    let verdict = evaluate(
        &expr,
        Path::new("/scope"),
        Some(&source),
        None,
        None,
        &no_windows(),
    );
    assert_eq!(verdict.truth, None);
    assert!(verdict.readings.values().all(Option::is_none));
    let expr =
        WhenExpr::parse(&["pr[feat/a]=merged && pr[b]=open && pr[feat/a]!=closed".to_owned()])
            .unwrap();
    let verdict = evaluate(
        &expr,
        Path::new("/scope"),
        Some(&source),
        None,
        None,
        &no_windows(),
    );
    assert_eq!(verdict.truth, Some(true));
    assert_eq!(verdict.readings.len(), 2);
    assert_eq!(
        expr.readings(&verdict).collect::<Vec<_>>(),
        [("pr[feat/a]", Some("merged")), ("pr[b]", Some("open"))]
    );
}

#[test]
fn branch_selector_refusals_suggest_parseable_fixes() {
    for input in [
        "pr[]=merged",
        "ci[feat/x=passed",
        "pr[feat x]=merged",
        "pr[a[b]]=merged",
        "team.stage[]=Done",
        "window.5h.left[b]>=40",
    ] {
        let error = WhenExpr::parse(&["ci=passed".to_owned(), input.to_owned()])
            .unwrap_err()
            .to_string();
        assert!(error.contains("clause 2, position"), "{input}: {error}");
        let fix = error
            .split_once("; use ")
            .unwrap_or_else(|| panic!("{error}"))
            .1;
        assert!(
            WhenExpr::parse(&[fix.to_owned()]).is_ok(),
            "{input}: {error}"
        );
        if input.starts_with("window") {
            assert!(
                error.contains("only team.stage, ci, pr, pr.queue, pr.mergeable, pr.review"),
                "{error}"
            );
        }
    }
}

#[test]
fn refusals_suggest_nearby_keys_values_and_stages() {
    let stages = BTreeSet::from(["Review".to_owned()]);
    for (input, expected) in [
        ("pr.que=queued", Some("pr.queue")),
        ("pr.que[b]=queued", Some("pr.queue")),
        ("ci=pased", Some("passed")),
        ("ci[b]=pased", Some("passed")),
        ("ci=noen", Some("none")),
        ("team.stage=Reveiw", Some("Review")),
        ("unrecognizable=passed", None),
        ("ci=unrecognizable", None),
        ("team.stage=unrecognizable", None),
    ] {
        let error = WhenExpr::parse_with_stages(&[input.to_owned()], Some(&stages))
            .unwrap_err()
            .to_string();
        assert!(error.starts_with("clause 1, position"), "{error}");
        match expected {
            Some(name) => assert!(error.contains(&format!("did you mean {name}?")), "{error}"),
            None => assert!(!error.contains("did you mean"), "{error}"),
        }
    }
}

#[test]
fn loop_freshness_holds_forge_keys_probed_before_the_fire_stamp_or_without_a_source() {
    let scope = Path::new("/checkout");
    let fired = Timestamp::from_millisecond(10_000).unwrap();
    let source = |ok: bool, at: i64| {
        CiSource(
            serde_json::from_value(serde_json::json!({
                "branch_ci": {},
                "path_repos": {"/checkout": "repo"},
                "repos": {"repo": {"ok": ok, "refreshed_at_ms": at}},
                "states": {"/checkout": {"state": "open", "ci": "passing"}},
            }))
            .unwrap(),
        )
    };
    let unsupported = CiSource(
        serde_json::from_value(serde_json::json!({
            "branch_ci": {},
            "path_repos": {"/checkout": UNSUPPORTED_REPO_KEY},
            "repos": {},
            "states": {},
        }))
        .unwrap(),
    );
    let expr = WhenExpr::parse(&["!ci=failed && pr=open".into()]).unwrap();
    for (source, fired_at, reason) in [
        (None, Some(fired), Some("no sidebar source")),
        (
            Some(unsupported),
            None,
            Some("no supported forge for this checkout"),
        ),
        (
            Some(source(true, 9_999)),
            Some(fired),
            Some("probed before the last fire or first sight"),
        ),
        (
            Some(source(false, 10_001)),
            Some(fired),
            Some("forge lookup failing"),
        ),
        (Some(source(true, 10_000)), Some(fired), None),
        (Some(source(true, 10_001)), Some(fired), None),
        (Some(source(true, 1)), None, None),
    ] {
        let verdict = evaluate(&expr, scope, source.as_ref(), None, None, &no_windows()).freshen(
            &expr,
            scope,
            source.as_ref(),
            fired_at,
            None,
            true,
        );
        let label = format!("{fired_at:?}, {reason:?}");
        match reason {
            Some(reason) => {
                assert_eq!(verdict.truth, None, "{label}");
                assert_eq!(
                    verdict.unread,
                    BTreeMap::from([("ci".into(), reason.into()), ("pr".into(), reason.into())]),
                    "{label}"
                );
                assert_eq!(verdict.readings["ci"], None, "{label}");
                assert_eq!(verdict.readings["pr"], None, "{label}");
            }
            None => {
                assert_eq!(verdict.truth, Some(true), "{label}");
                assert!(verdict.unread.is_empty(), "{label}");
                assert_eq!(verdict.readings["ci"].as_deref(), Some("passed"), "{label}");
            }
        }
    }
    let stage = WhenExpr::parse(&["!team.stage=Done".into()]).unwrap();
    let verdict = evaluate(&stage, scope, None, None, None, &no_windows());
    assert_eq!(
        verdict
            .clone()
            .freshen(&stage, scope, None, Some(fired), None, true),
        verdict
    );
}

#[test]
fn forge_keys_read_none_only_when_a_successful_probe_found_nothing() {
    use serde_json::json;

    let scope = Path::new("/checkout");
    let probed = |ok: bool, link: serde_json::Value| {
        CiSource(
            serde_json::from_value(json!({
                "branch_ci": {}, "path_repos": {"/checkout": "repo"},
                "repos": {"repo": {"ok": ok, "refreshed_at_ms": 10_000}},
                "states": if link.is_null() { json!({}) } else { json!({"/checkout": link}) },
                "branches": {"repo": {"topic": {}}},
            }))
            .unwrap(),
        )
    };
    let unsupported = CiSource(
        serde_json::from_value(json!({
            "branch_ci": {}, "path_repos": {"/checkout": UNSUPPORTED_REPO_KEY},
        }))
        .unwrap(),
    );
    let open = json!({"state": "open", "open": {"head": "a"}});
    for (label, source, key, reason, reading) in [
        (
            "no PR",
            probed(true, json!(null)),
            "pr",
            UnreadReason::NoPr { branch: None },
            Some("none"),
        ),
        (
            "no PR on a branch",
            probed(true, json!(null)),
            "pr.queue[topic]",
            UnreadReason::NoPr {
                branch: Some("topic".into()),
            },
            Some("none"),
        ),
        (
            "a branch the probe has no row for",
            probed(true, json!(null)),
            "pr[other]",
            UnreadReason::NotChecked,
            None,
        ),
        (
            "CI on a branch the probe has no row for",
            probed(true, json!(null)),
            "ci[other]",
            UnreadReason::NotChecked,
            None,
        ),
        (
            "no CI",
            probed(true, json!(null)),
            "ci",
            UnreadReason::NoCi { branch: None },
            Some("none"),
        ),
        (
            "no open PR",
            probed(true, json!({"state": "merged"})),
            "pr.mergeable",
            UnreadReason::NoOpenPr { branch: None },
            Some("none"),
        ),
        (
            "open PR without the fact",
            probed(true, open.clone()),
            "pr.review",
            UnreadReason::PrFactsUnavailable { branch: None },
            None,
        ),
        (
            "lookup failing",
            probed(false, json!(null)),
            "pr",
            UnreadReason::LookupFailing { last_tried: 10_000 },
            None,
        ),
        ("no forge", unsupported, "pr", UnreadReason::NoForge, None),
        (
            "not checked",
            CiSource(PrStateCache::default()),
            "pr",
            UnreadReason::NotChecked,
            None,
        ),
    ] {
        assert_eq!(
            source.probe_unread_reason(key, scope),
            Some(reason),
            "{label}"
        );
        let expr = WhenExpr::parse(&[format!("{key}=none")]).unwrap();
        let verdict = evaluate(&expr, scope, Some(&source), None, None, &no_windows());
        assert_eq!(verdict.readings[key].as_deref(), reading, "{label}");
        assert_eq!(verdict.truth, reading.map(|_| true), "{label}");
    }
    // After a fresh look with no PR, `!pr=open` is true on a loop; behind the floor it is unknown.
    let expr = WhenExpr::parse(&["!pr=open".into()]).unwrap();
    let source = probed(true, json!(null));
    for (fired, truth) in [(9_999, Some(true)), (10_001, None)] {
        let fired = Timestamp::from_millisecond(fired).unwrap();
        let verdict = evaluate(&expr, scope, Some(&source), None, None, &no_windows()).freshen(
            &expr,
            scope,
            Some(&source),
            Some(fired),
            None,
            true,
        );
        assert_eq!(verdict.truth, truth, "{fired}");
    }
}

#[test]
fn loop_truth_is_kleene_with_one_key_unread_and_no_veto() {
    let scope = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(scope.path().join(".rimz/team")).unwrap();
    std::fs::write(
        scope.path().join(".rimz/team/blackboard.md"),
        "Stage: Done\n",
    )
    .unwrap();
    for (input, truth) in [
        ("team.stage=Done || ci=passed", Some(true)),
        ("team.stage=Plan && ci=passed", Some(false)),
        ("team.stage=Done && ci=passed", None),
        ("!(team.stage=Done || ci=failed)", Some(false)),
    ] {
        let expr = WhenExpr::parse(&[input.to_owned()]).unwrap();
        // Without a sidebar source, every forge key is held unread.
        let verdict = evaluate(&expr, scope.path(), None, None, None, &no_windows()).freshen(
            &expr,
            scope.path(),
            None,
            None,
            None,
            true,
        );
        assert_eq!(
            verdict.unread,
            BTreeMap::from([("ci".into(), "no sidebar source".into())]),
            "{input}"
        );
        assert_eq!(verdict.truth, truth, "{input}");
    }
}

#[test]
fn team_stage_branch_reads_the_board_of_the_checkout_on_that_branch() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    let linked = dir.path().join("linked");
    let git = |cwd: &Path, args: &[&str]| {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args([
                "-c",
                "user.email=rimz@example.test",
                "-c",
                "user.name=RimZ Test",
            ])
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["commit", "--allow-empty", "-m", "base"]);
    git(
        &repo,
        &["worktree", "add", "-b", "feat/x", linked.to_str().unwrap()],
    );
    for (checkout, stage) in [(&repo, "Plan"), (&linked, "Review")] {
        std::fs::create_dir_all(checkout.join(".rimz/team")).unwrap();
        std::fs::write(
            checkout.join(".rimz/team/blackboard.md"),
            format!("Stage: {stage}\n"),
        )
        .unwrap();
    }
    let read = |input: &str| {
        let expr = WhenExpr::parse(&[input.to_owned()]).unwrap();
        let verdict = evaluate(&expr, &repo, None, None, None, &no_windows());
        (
            verdict.truth,
            verdict.readings.into_values().next().flatten(),
        )
    };
    assert_eq!(
        read("team.stage[feat/x]=Review"),
        (Some(true), Some("Review".to_owned()))
    );
    assert_eq!(
        read("team.stage[main]=Plan"),
        (Some(true), Some("Plan".to_owned()))
    );
    assert_eq!(
        read("team.stage=Plan"),
        (Some(true), Some("Plan".to_owned()))
    );
    assert_eq!(read("!team.stage[gone]=Review"), (None, None));
    let expr = WhenExpr::parse(&["team.stage[feat/x]=Review".to_owned()]).unwrap();
    let (selector, matches) = expr.terms().next().unwrap().wake_subscription().unwrap();
    assert_eq!(
        (selector.to_string(), matches),
        (
            "team.stage".to_owned(),
            BTreeMap::from([("board".to_owned(), "*".to_owned())])
        ),
        "a branch stage wakes on team.stage signals from any checkout"
    );
    let bare = WhenExpr::parse(&["team.stage=Review".to_owned()]).unwrap();
    assert_eq!(
        bare.terms().next().unwrap().wake_subscription().unwrap().1,
        BTreeMap::new(),
        "the bare key keeps the scope's default board"
    );
}

#[test]
fn loop_commit_fence_holds_bare_ci_only_on_a_fresh_true_verdict() {
    // No git checkout exists here, so local HEAD is unknown and only the CI SHA can fence.
    let scope = Path::new("/rimz-no-such-checkout");
    let fired = Timestamp::from_millisecond(10_000).unwrap();
    let source = |ci_sha: Option<&str>| {
        CiSource(
            serde_json::from_value(serde_json::json!({
                "branch_ci": {},
                "path_repos": {"/rimz-no-such-checkout": "repo"},
                "repos": {"repo": {"ok": true, "refreshed_at_ms": 10_000}},
                "states": {"/rimz-no-such-checkout": {
                    "state": "open", "ci": "failing", "ci_sha": ci_sha,
                    "open": {"head": "bbbbbbb2"},
                }},
            }))
            .unwrap(),
        )
    };
    for (clause, ci_sha, fenced) in [
        (
            "ci=failed",
            Some("aaaaaaa1"),
            Some("read at aaaaaaa, PR head bbbbbbb"),
        ),
        (
            "!ci=passed",
            Some("aaaaaaa1"),
            Some("read at aaaaaaa, PR head bbbbbbb"),
        ),
        ("ci=failed", Some("bbbbbbb2"), None),
        ("ci=failed", None, None),
        ("ci=passed", Some("aaaaaaa1"), None),
        ("pr=open", Some("aaaaaaa1"), None),
    ] {
        let source = source(ci_sha);
        let expr = WhenExpr::parse(&[clause.into()]).unwrap();
        let raw = evaluate(&expr, scope, Some(&source), None, None, &no_windows());
        let verdict = raw
            .clone()
            .freshen(&expr, scope, Some(&source), Some(fired), None, true);
        let label = format!("{clause}, {ci_sha:?}");
        match fenced {
            Some(reason) => {
                assert_eq!(verdict.truth, None, "{label}");
                assert_eq!(
                    verdict.unread,
                    BTreeMap::from([("ci".into(), reason.into())]),
                    "{label}"
                );
                assert_eq!(verdict.readings["ci"], None, "{label}");
            }
            None => assert_eq!(verdict, raw, "{label}"),
        }
    }
}

#[test]
fn team_stage_branch_values_take_the_stage_name_check() {
    let stages = BTreeSet::from(["Review".to_owned()]);
    assert!(
        WhenExpr::parse_with_stages(&["team.stage[b]=Review".to_owned()], Some(&stages)).is_ok()
    );
    let error = WhenExpr::parse_with_stages(&["team.stage[b]=Reveiw".to_owned()], Some(&stages))
        .unwrap_err()
        .to_string();
    assert!(error.contains("did you mean Review?"), "{error}");
}

#[test]
fn closed_pr_term_needs_a_merge_the_rest_cannot_replace() {
    let pick = |expr: &str, readings: &[(&str, Option<&str>)]| {
        let expr = WhenExpr::parse(&[expr.to_owned()]).unwrap();
        let readings = readings
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.map(ToOwned::to_owned)))
            .collect::<Vec<_>>();
        expr.closed_pr_term(&readings).map(|term| term.key.clone())
    };
    let closed = [("pr[x]", Some("closed")), ("pr[y]", Some("open"))];
    for (expr, readings, expected) in [
        ("pr[x]=merged", &closed[..1], Some("pr[x]")),
        ("pr[y]=open && pr[x]=merged", &closed[..], Some("pr[x]")),
        ("pr[y]=merged && pr[x]=merged", &closed[..], Some("pr[x]")),
        ("pr[x]=merged || pr[y]=merged", &closed[..], None),
        ("!(pr[x]=merged) && pr[y]=open", &closed[..], None),
        (
            "!(pr[y]=open || !(pr[x]=merged))",
            &closed[..],
            Some("pr[x]"),
        ),
        ("pr[x]=merged,closed", &closed[..1], None),
        ("pr[x]=open", &closed[..1], None),
        (
            "pr.queue[x]=queued",
            &[("pr.queue[x]", Some("closed"))][..],
            None,
        ),
        ("pr[x]=merged", &[("pr[x]", Some("open"))][..], None),
        ("pr[x]=merged", &[("pr[x]", None)][..], None),
        ("pr=merged", &[("pr", Some("closed"))][..], Some("pr")),
    ] {
        assert_eq!(pick(expr, readings).as_deref(), expected, "{expr}");
    }
}
