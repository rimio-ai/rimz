use super::*;

fn open_pr(number: u64, head: &str, base: &str) -> OpenPr {
    OpenPr {
        number,
        head: head.to_owned(),
        base: base.to_owned(),
    }
}

#[test]
fn stack_walks_both_directions_and_stops_at_forks() {
    let mut open = vec![open_pr(522, "lower", "main"), open_pr(530, "own", "lower")];
    let stack = pr_stack(530, &open, |b| b == "main", 8);
    assert_eq!(
        stack.below.iter().map(|p| p.number).collect::<Vec<_>>(),
        [522]
    );
    assert!(stack.above.is_empty());
    open.push(open_pr(540, "upper", "own"));
    let stack = pr_stack(530, &open, |b| b == "main", 8);
    assert_eq!(stack.below[0].number, 522);
    assert_eq!(stack.above[0][0].number, 540);
    open.extend([open_pr(550, "beyond", "upper"), open_pr(541, "fork", "own")]);
    open.reverse();
    let stack = pr_stack(530, &open, |b| b == "main", 8);
    assert_eq!(stack.above.len(), 1);
    assert_eq!(
        stack.above[0].iter().map(|p| p.number).collect::<Vec<_>>(),
        [540, 541]
    );
}

#[test]
fn stack_stops_at_trunk_missing_anchor_cycles_and_depth() {
    let open = vec![
        open_pr(1, "develop", "main"),
        open_pr(2, "own", "develop"),
        open_pr(3, "top", "own"),
    ];
    assert!(pr_stack(99, &open, |_| false, 8).is_empty());
    let stack = pr_stack(2, &open, |b| b == "develop", 8);
    assert!(stack.below.is_empty());
    assert_eq!(stack.above[0][0].number, 3);
    assert!(pr_stack(2, &open, |_| false, 0).is_empty());
    assert_eq!(
        pr_stack(3, &open, |_| false, 1)
            .below
            .iter()
            .map(|p| p.number)
            .collect::<Vec<_>>(),
        [2]
    );
    assert_eq!(pr_stack(1, &open, |_| false, 1).above.len(), 1);
    let cycle = vec![open_pr(1, "a", "b"), open_pr(2, "b", "a")];
    let stack = pr_stack(1, &cycle, |_| false, 8);
    assert_eq!(
        stack.below.iter().map(|p| p.number).collect::<Vec<_>>(),
        [2]
    );
    assert!(stack.above.is_empty());
}

#[test]
fn stack_chooses_highest_duplicate_head_and_orders_bottom_first() {
    let open = vec![
        open_pr(1, "bottom", "main"),
        open_pr(2, "lower", "bottom"),
        open_pr(4, "lower", "bottom"),
        open_pr(5, "own", "lower"),
    ];
    assert_eq!(
        pr_stack(5, &open, |b| b == "main", 8)
            .below
            .iter()
            .map(|p| p.number)
            .collect::<Vec<_>>(),
        [1, 4]
    );
}

#[test]
fn github_stack_open_alias_is_required_and_excludes_forks() {
    let query = github_bulk_query(true, "org/repo", &[], &[]);
    assert!(query.contains("open: pullRequests(first: 100"));
    assert!(query.contains("headRefName baseRefName isCrossRepository"));
    let raw = r#"{"data":{"repository":{"open":{"nodes":[{"number":1,"headRefName":"a","baseRefName":"main","isCrossRepository":false},{"number":2,"headRefName":"b","baseRefName":"a","isCrossRepository":true}]}}}}"#;
    assert_eq!(
        parse_github_bulk_response(true, raw, 0, 0).unwrap().open,
        Some(vec![open_pr(1, "a", "main")])
    );
    assert!(parse_github_bulk_response(true, r#"{"data":{"repository":{}}}"#, 0, 0).is_err());
}

#[test]
fn tea_stack_open_set_tolerates_base_objects_and_excludes_forks() {
    let raw = r#"[{"index":"1","state":"open","head":"a","base":"main"},{"index":"2","state":"open","head":"b","base":{"ref":"a"}},{"index":"3","state":"open","head":"me:feature","base":"b"}]"#;
    assert_eq!(
        parse_tea_open_prs(raw).unwrap(),
        [open_pr(1, "a", "main"), open_pr(2, "b", "a")]
    );
    assert_eq!(parse_tea_pr_list_links(raw).unwrap()["feature"].number, 3);
    assert!(tea_pr_list_args("open", None).contains(&"index,state,head,base,created"));
}

#[test]
fn parses_bare_number_without_forge() {
    assert_eq!(
        parse(" 42 ").expect("parse PR number"),
        PrTarget {
            number: 42,
            forge: None,
            host: None,
            repo: None,
        }
    );
}

#[test]
fn parses_github_style_urls() {
    assert_eq!(
        parse("https://github.com/org/repo/pull/123").expect("github URL"),
        PrTarget {
            number: 123,
            forge: Some(Forge::GitHubStyle),
            host: Some("github.com".to_owned()),
            repo: Some("org/repo".to_owned()),
        }
    );
    assert_eq!(
        parse("https://gitea.example.test/org/repo/pulls/7").expect("gitea URL"),
        PrTarget {
            number: 7,
            forge: Some(Forge::GitHubStyle),
            host: Some("gitea.example.test".to_owned()),
            repo: Some("org/repo".to_owned()),
        }
    );
}

#[test]
fn parses_gitlab_urls() {
    assert_eq!(
        parse("https://GitLab.com/org/team/repo.git/-/merge_requests/9").expect("gitlab URL"),
        PrTarget {
            number: 9,
            forge: Some(Forge::GitLab),
            host: Some("gitlab.com".to_owned()),
            repo: Some("org/team/repo".to_owned()),
        }
    );
}

#[test]
fn compares_pr_url_identity_with_origin() {
    let target = parse("https://github.com/Org/Repo/pull/7").unwrap();

    let origin = RemoteRepo::parse("git@github.com:org/repo.git").unwrap();
    assert!(origin.matches_target(&target));
    let origin = RemoteRepo::parse("ssh://git@github.com/other/repo.git").unwrap();
    assert!(!origin.matches_target(&target));
    let origin = RemoteRepo::parse("git@gitlab.com:org/repo.git").unwrap();
    assert!(!origin.matches_target(&target));
    let origin = RemoteRepo::parse("git@gitlab.com:other/repo.git").unwrap();
    assert!(origin.matches_target(&parse("7").unwrap()));
    let origin = RemoteRepo::parse("git@github.com:org/team/repo.git").unwrap();
    assert!(origin.matches_target(&parse("https://GITHUB.com/Org/Team/Repo/pull/7").unwrap()));
}

#[test]
fn maps_remote_hosts_to_forge() {
    for remote in [
        "https://github.com/org/repo.git",
        "git@github.com:org/repo.git",
        "https://gitea.example.test/org/repo.git",
        "git@gitea.example.test:org/repo.git",
    ] {
        assert_eq!(
            RemoteRepo::parse(remote).unwrap().forge(),
            Forge::GitHubStyle,
            "{remote}"
        );
    }
    for remote in [
        "https://gitlab.com/org/repo.git",
        "git@gitlab.com:org/repo.git",
        "ssh://git@gitlab.example.test/org/repo.git",
        "ssh://git@gitlab.example.test:2222/org/repo.git",
    ] {
        assert_eq!(
            RemoteRepo::parse(remote).unwrap().forge(),
            Forge::GitLab,
            "{remote}"
        );
    }
}

#[test]
fn maps_remote_hosts_to_forge_cli() {
    for remote in [
        "https://github.com/org/repo.git",
        "git@github.com:org/repo.git",
    ] {
        assert_eq!(
            RemoteRepo::parse(remote).and_then(|repo| repo.forge_cli()),
            Some(ForgeCli::Gh),
            "{remote}"
        );
    }
    for remote in [
        "https://gitea.example.test/org/repo.git",
        "git@forgejo.example.test:org/repo.git",
        "https://codeberg.org/org/repo.git",
    ] {
        assert_eq!(
            RemoteRepo::parse(remote).and_then(|repo| repo.forge_cli()),
            Some(ForgeCli::Tea),
            "{remote}"
        );
    }
    for remote in [
        "https://gitlab.com/org/repo.git",
        "https://example.test/org/repo.git",
        "/tmp/gitea.example.test/org/repo.git",
        "https:///gitea.example.test/org/repo.git",
        "not-a-remote",
    ] {
        assert_eq!(
            RemoteRepo::parse(remote).and_then(|repo| repo.forge_cli()),
            None,
            "{remote}"
        );
    }
}

#[test]
fn extracts_remote_repo_slug() {
    for (remote, slug) in [
        ("git@gitea-ssh.example.test:owner/repo.git", "owner/repo"),
        ("https://gitea.example.test/owner/repo.git", "owner/repo"),
        ("ssh://git@host:2222/owner/repo.git", "owner/repo"),
        ("git@host:owner/repo", "owner/repo"),
        ("https://host/owner/repo/", "owner/repo"),
        ("git@host:owner/team/repo.git", "owner/team/repo"),
        (
            "ssh://build@host:2222/owner/team/repo.git",
            "owner/team/repo",
        ),
    ] {
        assert_eq!(
            RemoteRepo::parse(remote).unwrap().repo_slug(),
            Some(slug),
            "{remote}"
        );
    }
}

#[test]
fn rejects_remote_repo_slug_without_owner_repo_path() {
    for remote in [
        "",
        "git@gitea-ssh.example.test",
        "not-a-remote",
        "/tmp/repo",
        "https://host/repo.git",
        "https:///owner/repo.git",
    ] {
        assert_eq!(
            RemoteRepo::parse(remote).and_then(|repo| repo.repo_slug),
            None,
            "{remote}"
        );
    }
}

#[test]
fn parses_gh_pr_heads() {
    assert_eq!(
        parse_gh_pr_view_json(
            r#"{
                "headRefName":"feature",
                "headRepository":{"name":"repo"},
                "headRepositoryOwner":{"login":"org"},
                "isCrossRepository":false
            }"#
        )
        .unwrap(),
        PrHead {
            branch: "feature".to_owned(),
            owner: Some("org".to_owned()),
            repo_full_name: Some("org/repo".to_owned()),
            is_cross_repository: Some(false),
        }
    );
    assert_eq!(
        parse_gh_pr_view_json(
            r#"{
                "headRefName":"fork-work",
                "headRepository":{"name":"fork"},
                "headRepositoryOwner":{"login":"alice"}
            }"#
        )
        .unwrap()
        .repo_full_name
        .as_deref(),
        Some("alice/fork")
    );
}

#[test]
fn parses_tea_pr_heads() {
    assert_eq!(
        parse_tea_pr_head_json(
            r#"{
                "head":{"label":"feature","ref":"refs/pull/1/head","sha":"abc123","repo":{"full_name":"org/repo","owner":{"login":"org"}}},
                "base":{"label":"main","ref":"main","repo":{"full_name":"ORG/Repo"}}
            }"#
        )
        .unwrap(),
        PrHead {
            branch: "feature".to_owned(),
            owner: Some("org".to_owned()),
            repo_full_name: Some("org/repo".to_owned()),
            is_cross_repository: Some(false),
        }
    );
    assert_eq!(
        parse_tea_pr_head_json(
            r#"{
                "head":{"label":"feature","ref":"refs/pull/1/head","sha":"abc123","repo":{"full_name":"alice/fork","owner":{"login":"alice"}}},
                "base":{"label":"main","ref":"main","repo":{"full_name":"org/repo"}}
            }"#
        )
        .unwrap(),
        PrHead {
            branch: "feature".to_owned(),
            owner: Some("alice".to_owned()),
            repo_full_name: Some("alice/fork".to_owned()),
            is_cross_repository: Some(true),
        }
    );
    assert_eq!(
        parse_tea_pr_head_json(
            r#"{
                "head":{"label":"feature","ref":"refs/pull/1/head","repo":null},
                "base":{"label":"main","ref":"main","repo":{"full_name":"org/repo"}}
            }"#
        )
        .unwrap(),
        PrHead {
            branch: "feature".to_owned(),
            owner: None,
            repo_full_name: None,
            is_cross_repository: None,
        }
    );
    assert_eq!(
        parse_tea_pr_head_json(r#"{"head":{"label":"feature","repo":{"full_name":"org/repo"}}}"#)
            .unwrap(),
        PrHead {
            branch: "feature".to_owned(),
            owner: Some("org".to_owned()),
            repo_full_name: Some("org/repo".to_owned()),
            is_cross_repository: None,
        }
    );
    assert!(
        parse_tea_pr_head_json(
            r#"{"head":"feat/native-boros-adapter","headSha":"9c94455afb3cd7a58a6403e8e5cb423e6cdaa52d"}"#
        )
        .unwrap_err()
        .contains("tea PR payload")
    );
    assert_eq!(
        parse_tea_pr_head_json(r#"{"head":{"label":"","ref":"refs/pull/1/head"}}"#).unwrap_err(),
        "tea PR head branch is empty"
    );
}

#[test]
fn builds_sibling_repo_urls() {
    for (origin, expected) in [
        (
            "https://github.com/org/repo.git",
            "https://github.com/alice/fork.git",
        ),
        (
            "ssh://build@host:2222/org/team/repo.git",
            "ssh://build@host:2222/alice/fork.git",
        ),
        ("git@host:org/team/repo.git", "git@host:alice/fork.git"),
        ("host/org/repo", "host/alice/fork"),
    ] {
        assert_eq!(
            RemoteRepo::parse(origin)
                .unwrap()
                .sibling_url("alice/fork")
                .as_deref(),
            Some(expected),
            "{origin}"
        );
    }
    assert_eq!(
        RemoteRepo::parse("https://host/org/repo.git")
            .unwrap()
            .sibling_url(" /alice/fork/ ")
            .as_deref(),
        Some("https://host/alice/fork.git")
    );
    assert!(RemoteRepo::parse("/tmp/origin.git").is_none());
}

#[test]
fn builds_pull_request_web_urls() {
    for (remote, expected) in [
        (
            "https://github.com/org/repo.git",
            "https://github.com/org/repo/pull/91",
        ),
        (
            "git@github.com:org/repo.git",
            "https://github.com/org/repo/pull/91",
        ),
        (
            "git@gitea.example.test:org/repo.git",
            "https://gitea.example.test/org/repo/pulls/91",
        ),
        (
            "git@gitlab.com:org/repo.git",
            "https://gitlab.com/org/repo/-/merge_requests/91",
        ),
    ] {
        assert_eq!(
            RemoteRepo::parse(remote)
                .and_then(|repo| repo.pr_web_url(91))
                .as_deref(),
            Some(expected),
            "{remote}"
        );
    }
    assert_eq!(
        RemoteRepo::parse("https://github.com/repo.git").and_then(|repo| repo.pr_web_url(91)),
        None
    );
}

#[test]
fn builds_checks_web_urls() {
    for (remote, expected) in [
        (
            "https://github.com/org/repo.git",
            "https://github.com/org/repo/commit/abc123/checks",
        ),
        (
            "git@github.com:org/repo.git",
            "https://github.com/org/repo/commit/abc123/checks",
        ),
        (
            "git@gitea.example.test:org/repo.git",
            "https://gitea.example.test/org/repo/commit/abc123",
        ),
        (
            "https://codeberg.org/org/repo.git",
            "https://codeberg.org/org/repo/commit/abc123",
        ),
    ] {
        assert_eq!(
            RemoteRepo::parse(remote)
                .and_then(|repo| repo.checks_web_url("abc123"))
                .as_deref(),
            Some(expected),
            "{remote}"
        );
    }
    for remote in ["https://github.com/repo.git", "git@gitlab.com:org/repo.git"] {
        assert_eq!(
            RemoteRepo::parse(remote).and_then(|repo| repo.checks_web_url("abc123")),
            None
        );
    }
}

#[test]
fn builds_github_bulk_query_with_ordered_escaped_aliases() {
    let query = github_bulk_query(
        false,
        "org/repo",
        &["feature", "quote\"branch"],
        &["head-one", "head\"two"],
    );

    assert!(query.starts_with(r#"query { repository(owner: "org", name: "repo") {"#));
    assert!(query.contains(
        r#"pr0: pullRequests(first: 10, headRefName: "feature", states: [OPEN, MERGED, CLOSED]"#
    ));
    assert!(query.contains("nodes { number state createdAt statusCheckRollup"));
    assert!(query.contains(r#"pr1: pullRequests(first: 10, headRefName: "quote\"branch""#));
    assert!(query.contains(r#"sha0: object(oid: "head-one")"#));
    assert!(query.contains(r#"sha1: object(oid: "head\"two")"#));
    assert!(
        query.contains(r#"facts0: pullRequests(first: 1, headRefName: "feature", states: [OPEN]"#)
    );
    assert!(query.contains(r#"baseRef { name compare(headRef: "quote\"branch") { behindBy } }"#));
    assert_eq!(
        query
            .matches("headRefOid mergeable isCrossRepository")
            .count(),
        2
    );
    assert_eq!(query.matches("pullRequests(").count(), 4);
    assert_eq!(query.matches(": object(").count(), 2);
    let queue_fields = "mergeQueueEntry { enqueuedAt } timelineItems(last: 1, itemTypes: [REMOVED_FROM_MERGE_QUEUE_EVENT]) { nodes { ... on RemovedFromMergeQueueEvent { createdAt reason beforeCommit { oid } } } }";
    let facts = query.split("facts").skip(1).map(|alias| {
        alias
            .split(" pr1:")
            .next()
            .unwrap()
            .split(" sha0:")
            .next()
            .unwrap()
    });
    assert_eq!(facts.clone().count(), 2);
    assert!(facts.clone().all(|alias| alias.contains(queue_fields)));
    assert_eq!(query.matches(queue_fields).count(), 2);
    assert_eq!(query.matches("mergeQueueEntry").count(), 2);
    assert_eq!(query.matches("timelineItems").count(), 2);
}

#[test]
fn parses_github_merge_queue_facts() {
    use pr_state::PrQueueFact;
    use serde_json::json;

    let removal = |reason: Value, commit: Value| json!({"nodes": [null, {"createdAt": "2026-10-03T12:46:03Z", "reason": reason, "beforeCommit": commit}]});
    let dequeued = |reason: Option<&str>, commit: Option<&str>| {
        Some(PrQueueFact::Dequeued {
            at: "2026-10-03T12:46:03Z".into(),
            reason: reason.map(str::to_owned),
            commit: commit.map(str::to_owned),
        })
    };
    let queued = Some(PrQueueFact::Queued {
        at: "2026-10-03T12:28:46Z".into(),
    });
    let entry = json!({"enqueuedAt": "2026-10-03T12:28:46Z"});
    for (label, queue_entry, timeline, expected) in [
        ("absent fields", None, None, None),
        (
            "never queued",
            Some(json!(null)),
            Some(json!({"nodes": []})),
            None,
        ),
        (
            "null nodes",
            Some(json!(null)),
            Some(json!({"nodes": null})),
            None,
        ),
        (
            "queued",
            Some(entry.clone()),
            Some(json!({"nodes": []})),
            queued.clone(),
        ),
        (
            "re-queued after a removal",
            Some(entry),
            Some(removal(json!("failed_checks"), json!({"oid": "queue-a"}))),
            queued,
        ),
        (
            "failed checks",
            Some(json!(null)),
            Some(removal(json!("failed_checks"), json!({"oid": "queue-a"}))),
            dequeued(Some("failed_checks"), Some("queue-a")),
        ),
        (
            "manual",
            Some(json!(null)),
            Some(removal(json!("manual"), json!(null))),
            dequeued(Some("manual"), None),
        ),
        (
            "a reason this build has never seen",
            Some(json!(null)),
            Some(removal(json!("queue_cleared"), json!(null))),
            dequeued(Some("queue_cleared"), None),
        ),
        (
            "no reason",
            Some(json!(null)),
            Some(removal(json!(null), json!(null))),
            dequeued(None, None),
        ),
        (
            "merged",
            Some(json!(null)),
            Some(removal(json!("merged"), json!({"oid": "queue-a"}))),
            None,
        ),
    ] {
        let mut facts = json!({"number": 42, "headRefOid": "head-a", "mergeable": "MERGEABLE",
            "isCrossRepository": false, "baseRef": null});
        if let Some(queue_entry) = queue_entry {
            facts["mergeQueueEntry"] = queue_entry;
        }
        if let Some(timeline) = timeline {
            facts["timelineItems"] = timeline;
        }
        let raw = json!({"data": {"repository": {
            "pr0": {"nodes": [{"number": 42, "state": "OPEN"}]},
            "facts0": {"nodes": [facts]}
        }}});
        let response = parse_github_bulk_response(false, &raw.to_string(), 1, 0).unwrap();
        let open = response.prs[0].as_ref().unwrap().open.as_ref().unwrap();
        assert_eq!(open.queue, expected, "{label}");
    }
}

#[test]
fn parses_github_open_pr_facts_and_unknown_bases() {
    use pr_state::{OpenPrFacts, SettledMergeability};
    use serde_json::json;

    for (mergeable, expected) in [
        (
            "CONFLICTING",
            Some(SettledMergeability::Conflicting("head-a".into())),
        ),
        (
            "MERGEABLE",
            Some(SettledMergeability::Mergeable("head-a".into())),
        ),
        ("UNKNOWN", None),
    ] {
        for (base_ref, base, behind_by) in [
            (
                json!({"name": "main", "compare": {"behindBy": 3}}),
                Some("main".into()),
                Some(3),
            ),
            (
                json!({"name": "main", "compare": null}),
                Some("main".into()),
                None,
            ),
            (json!(null), None, None),
        ] {
            let raw = json!({"data": {"repository": {
                "pr0": {"nodes": [{"number": 42, "state": "OPEN"}]},
                "facts0": {"nodes": [{"number": 42, "headRefOid": "head-a", "mergeable": mergeable, "isCrossRepository": false, "baseRef": base_ref}]}
            }}});
            let response = parse_github_bulk_response(false, &raw.to_string(), 1, 0).unwrap();
            assert_eq!(
                response.prs[0].as_ref().unwrap().open,
                Some(OpenPrFacts {
                    head: "head-a".into(),
                    base,
                    behind_by,
                    mergeability: expected.clone(),
                    queue: None,
                })
            );
        }
    }
}

#[test]
fn github_facts_errors_are_scoped() {
    use serde_json::json;

    let mut raw = json!({"data": {"repository": {
        "pr0": {"nodes": [{"number": 14519, "state": "OPEN"}]},
        "facts0": {"nodes": [{"number": 14519, "headRefOid": "head-a",
            "mergeable": "CONFLICTING", "isCrossRepository": false,
            "baseRef": {"name": "trunk", "compare": null}}]}
    }}, "errors": [{"type": "NOT_FOUND",
        "path": ["repository", "facts0", "nodes", 0, "baseRef", "compare"],
        "message": "Could not resolve head ref 'document-search-operator-support'."}]});
    let response = parse_github_bulk_response(false, &raw.to_string(), 1, 0).unwrap();
    let pr = response.prs[0].as_ref().unwrap();
    assert_eq!(pr.number, 14519);
    let facts = pr.open.as_ref().unwrap();
    assert_eq!(facts.behind_by, None);
    assert_eq!(
        facts.mergeability,
        Some(pr_state::SettledMergeability::Conflicting("head-a".into()))
    );
    raw["data"]["repository"]["facts0"]["nodes"][0]["baseRef"]["compare"] = json!({"behindBy": 7});
    assert_eq!(
        parse_github_bulk_response(false, &raw.to_string(), 1, 0)
            .unwrap()
            .prs[0]
            .as_ref()
            .unwrap()
            .open
            .as_ref()
            .unwrap()
            .behind_by,
        None
    );
    for path in [
        json!(["repository", "pr0"]),
        json!(["repository", "sha0"]),
        json!(["repository", "facts9"]),
        json!(null),
    ] {
        raw["errors"][0]["path"] = path;
        assert!(parse_github_bulk_response(false, &raw.to_string(), 1, 0).is_err());
    }
}

#[test]
fn github_queue_field_errors_fail_the_probe() {
    use serde_json::json;

    // A queued PR whose last removal is older: a nulled entry would read as dequeued.
    let mut raw = json!({"data": {"repository": {
        "pr0": {"nodes": [{"number": 42, "state": "OPEN"}]},
        "facts0": {"nodes": [{"number": 42, "headRefOid": "head-a",
            "mergeable": "MERGEABLE", "isCrossRepository": false,
            "baseRef": {"name": "main", "compare": null},
            "mergeQueueEntry": {"enqueuedAt": "2026-10-03T13:00:00Z"},
            "timelineItems": {"nodes": [{"createdAt": "2026-10-03T12:46:03Z",
                "reason": "failed_checks", "beforeCommit": {"oid": "queue-a"}}]}}]}
    }}, "errors": [{"type": "NOT_FOUND",
        "path": ["repository", "facts0", "nodes", 0, "baseRef", "compare"],
        "message": "Could not resolve head ref 'feature'."}]});
    let response = parse_github_bulk_response(false, &raw.to_string(), 1, 0).unwrap();
    assert_eq!(
        response.prs[0]
            .as_ref()
            .unwrap()
            .open
            .as_ref()
            .unwrap()
            .queue,
        Some(pr_state::PrQueueFact::Queued {
            at: "2026-10-03T13:00:00Z".into()
        }),
        "a compare error keeps the healthy queue fact"
    );

    raw["data"]["repository"]["facts0"]["nodes"][0]["mergeQueueEntry"] = json!(null);
    for path in [
        json!(["repository", "facts0", "nodes", 0, "mergeQueueEntry"]),
        json!([
            "repository",
            "facts0",
            "nodes",
            0,
            "mergeQueueEntry",
            "enqueuedAt"
        ]),
        json!(["repository", "facts0", "nodes", 0, "timelineItems"]),
        json!([
            "repository",
            "facts0",
            "nodes",
            0,
            "timelineItems",
            "nodes",
            0,
            "beforeCommit"
        ]),
    ] {
        raw["errors"][0]["path"] = path.clone();
        assert!(
            parse_github_bulk_response(false, &raw.to_string(), 1, 0).is_err(),
            "{path}"
        );
    }
}

#[test]
fn github_fork_facts_have_unknown_distance() {
    let raw = serde_json::json!({"data": {"repository": {
        "pr0": {"nodes": [{"number": 42, "state": "OPEN"}]},
        "facts0": {"nodes": [{"number": 42, "headRefOid": "head-a",
            "mergeable": "CONFLICTING", "isCrossRepository": true,
            "baseRef": {"name": "main", "compare": {"behindBy": 7}}}]}
    }}});
    let response = parse_github_bulk_response(false, &raw.to_string(), 1, 0).unwrap();
    let facts = response.prs[0].as_ref().unwrap().open.as_ref().unwrap();
    assert_eq!(facts.behind_by, None);
    assert!(facts.mergeability.is_some());
}

#[test]
fn parses_github_bulk_prs_and_commits_by_alias() {
    let response = parse_github_bulk_response(false,
        r#"{
            "data": {
                "repository": {
                    "facts0": {"nodes": []},
                    "facts1": {"nodes": []},
                    "facts2": {"nodes": []},
                    "facts3": {"nodes": []},
                    "facts4": {"nodes": []},
                    "pr0": {"nodes": [
                        {"number": 10, "state": "CLOSED", "statusCheckRollup": null, "mergeCommit": null},
                        {"number": 11, "state": "MERGED", "statusCheckRollup": {"state": "FAILURE"}, "mergeCommit": {"oid": "old-merge", "statusCheckRollup": {"state": "SUCCESS"}}},
                        {"number": 12, "state": "OPEN", "createdAt": "2026-07-18T01:25:14Z", "statusCheckRollup": {"state": "PENDING"}, "mergeCommit": null},
                        {"number": 13, "state": "OPEN", "statusCheckRollup": {"state": "FAILURE"}, "mergeCommit": null}
                    ]},
                    "pr1": {"nodes": [
                        {"number": 20, "state": "MERGED", "createdAt": "2026-07-18T01:26:14Z", "statusCheckRollup": {"state": "FAILURE"}, "mergeCommit": {"oid": "merge-sha", "statusCheckRollup": {"state": "SUCCESS"}}}
                    ]},
                    "pr2": {"nodes": [
                        {"number": 30, "state": "CLOSED", "createdAt": "not-a-timestamp", "statusCheckRollup": {"state": "ERROR"}, "mergeCommit": null}
                    ]},
                    "pr3": {"nodes": []},
                    "pr4": {"nodes": [
                        {"number": 40, "state": "MERGED", "statusCheckRollup": {"state": "EXPECTED"}, "mergeCommit": {"oid": "", "statusCheckRollup": null}}
                    ]},
                    "sha0": {"oid": "head-a", "statusCheckRollup": {"state": "SUCCESS"}},
                    "sha1": {"oid": "head-b", "statusCheckRollup": {"state": "ERROR"}},
                    "sha2": {"oid": "head-c", "statusCheckRollup": {"state": "UNKNOWN"}},
                    "sha3": null
                }
            }
        }"#,
        5,
        4,
    )
    .unwrap();

    assert_eq!(
        response.prs,
        vec![
            Some(GhBulkPr {
                open: None,
                number: 12,
                state: WorktreePrState::Open,
                created_at: Some("2026-07-18T01:25:14Z".parse().unwrap()),
                head_ci: Some(WorktreeCi::Pending),
                merge_sha: None,
                merge_ci: None,
            }),
            Some(GhBulkPr {
                open: None,
                number: 20,
                state: WorktreePrState::Merged,
                created_at: Some("2026-07-18T01:26:14Z".parse().unwrap()),
                head_ci: Some(WorktreeCi::Failing),
                merge_sha: Some("merge-sha".to_owned()),
                merge_ci: Some(WorktreeCi::Passing),
            }),
            Some(GhBulkPr {
                open: None,
                number: 30,
                state: WorktreePrState::Closed,
                created_at: None,
                head_ci: Some(WorktreeCi::Failing),
                merge_sha: None,
                merge_ci: None,
            }),
            None,
            Some(GhBulkPr {
                open: None,
                number: 40,
                state: WorktreePrState::Merged,
                created_at: None,
                head_ci: Some(WorktreeCi::Pending),
                merge_sha: None,
                merge_ci: None,
            }),
        ]
    );
    assert_eq!(
        response.commits,
        vec![
            Some(WorktreeCi::Passing),
            Some(WorktreeCi::Failing),
            None,
            None,
        ]
    );
}

#[test]
fn github_rollup_state_mapping_is_aggregate_only() {
    for (state, expected) in [
        ("SUCCESS", Some(WorktreeCi::Passing)),
        ("failure", Some(WorktreeCi::Failing)),
        ("ERROR", Some(WorktreeCi::Failing)),
        ("PENDING", Some(WorktreeCi::Pending)),
        ("expected", Some(WorktreeCi::Pending)),
        ("NEUTRAL", None),
        ("", None),
    ] {
        assert_eq!(ci_from_gh_rollup_state(state), expected, "{state}");
    }
}

#[test]
fn rejects_incomplete_or_error_github_bulk_responses() {
    assert!(parse_github_bulk_response(false, "{", 0, 0).is_err());
    assert!(
        parse_github_bulk_response(
            false,
            r#"{"errors":[{"message":"rate limited"}],"data":{"repository":{"pr0":{"nodes":[]}}}}"#,
            1,
            0,
        )
        .is_err()
    );
    assert!(parse_github_bulk_response(false, r#"{"data":{"repository":null}}"#, 0, 0).is_err());
    assert!(
        parse_github_bulk_response(
            false,
            r#"{"data":{"repository":{"pr0":{"nodes":[]}}}}"#,
            2,
            0
        )
        .is_err()
    );
    assert!(
        parse_github_bulk_response(false, r#"{"data":{"repository":{"sha0":null}}}"#, 0, 2)
            .is_err()
    );
    assert!(
        parse_github_bulk_response(false, r#"{"data":{"repository":{"sha0":null}}}"#, 0, 1)
            .unwrap()
            .commits[0]
            .is_none()
    );
}

#[test]
fn parses_tea_pr_list_and_detail_json() {
    let list =
        r#"[{"index":"916","state":"merged","head":"mill-cli","created":"2026-07-18T01:25:14Z"}]"#;
    assert_eq!(
        parse_tea_pr_list_json(list, "mill-cli").unwrap(),
        Some(PrCandidate {
            number: 916,
            state: WorktreePrState::Merged,
            created_at: Some("2026-07-18T01:25:14Z".parse().unwrap()),
        })
    );
    assert_eq!(
        parse_tea_pr_detail_json(
            r#"{
                "state":"closed",
                "merged":true,
                "created_at":"2026-07-18T01:20:14Z",
                "merged_at":"2026-07-18T03:25:14+02:00",
                "merge_commit_sha":"ed3c062267135fa5195374b7b561c458ac399a98",
                "head":{"sha":"4c915ee98590bad6897a7a864cd635b7cdfe937d"}
            }"#
        )
        .unwrap(),
        TeaPrDetail {
            state: Some(WorktreePrState::Merged),
            created_at: Some("2026-07-18T01:20:14Z".parse().unwrap()),
            merged_sha: Some("ed3c062267135fa5195374b7b561c458ac399a98".to_owned()),
            head_sha: Some("4c915ee98590bad6897a7a864cd635b7cdfe937d".to_owned()),
        }
    );
    assert_eq!(
        parse_tea_pr_detail_json(
            r#"{
                "state":"closed",
                "merged":false,
                "merged_at":null,
                "head":{"sha":"closed-head-sha"}
            }"#
        )
        .unwrap(),
        TeaPrDetail {
            state: Some(WorktreePrState::Closed),
            created_at: None,
            merged_sha: None,
            head_sha: Some("closed-head-sha".to_owned()),
        }
    );
    assert_eq!(parse_tea_pr_list_json("[]", "mill-cli").unwrap(), None);
    assert!(parse_tea_pr_list_json("{}", "mill-cli").is_err());
}

#[test]
fn parses_tea_pr_list_links_by_head_branch() {
    let payload = r#"[
        {"number": "7", "head": "me:feature", "state": "closed"},
        {"index": 8, "head": {"branch": "feature"}, "state": "open"},
        {"id": "9", "head": {"label": "owner:feature"}, "state": "closed", "merged": true},
        {"index": 10, "source_branch": "other", "state": "open"}
    ]"#;
    let links = parse_tea_pr_list_links(payload).unwrap();

    assert_eq!(
        links.get("feature"),
        Some(&PrCandidate {
            number: 9,
            state: WorktreePrState::Merged,
            created_at: None,
        })
    );
    assert_eq!(
        parse_tea_pr_list_json(payload, "feature").unwrap(),
        links.get("feature").cloned()
    );
    assert_eq!(
        links.get("other"),
        Some(&PrCandidate {
            number: 10,
            state: WorktreePrState::Open,
            created_at: None,
        })
    );
    assert!(parse_tea_pr_list_links("{}").is_err());
    assert!(parse_tea_pr_list_json("{}", "feature").is_err());
}

#[test]
fn tea_pr_list_args_thread_limit_state_and_repo() {
    let args = tea_pr_list_args("all", Some("org/repo"));
    assert!(args.windows(2).any(|window| window == ["--state", "all"]));
    assert!(args.windows(2).any(|window| window == ["--limit", "500"]));
    assert!(
        args.windows(2)
            .any(|window| window == ["--repo", "org/repo"])
    );

    let bare = tea_pr_list_args("open", None);
    assert!(bare.windows(2).any(|window| window == ["--limit", "500"]));
    assert!(!bare.contains(&"--repo"));
}

#[test]
fn forge_cli_builds_and_decodes_head_commands() {
    assert_eq!(
        ForgeCli::Gh.pr_head_args(42, None).unwrap().join(" "),
        "pr view 42 --json headRefName,headRepository,headRepositoryOwner,isCrossRepository"
    );
    assert_eq!(
        ForgeCli::Tea
            .pr_head_args(42, Some("org/repo"))
            .unwrap()
            .join(" "),
        "api repos/org/repo/pulls/42 --repo org/repo"
    );
    assert_eq!(
        ForgeCli::Tea.pr_head_args(42, None).unwrap_err(),
        "could not derive the origin repository for tea"
    );
    assert_eq!(
        ForgeCli::Gh
            .decode_pr_head(
                r#"{"headRefName":"feature","headRepository":{"name":"repo"},"headRepositoryOwner":{"login":"org"}}"#,
            )
            .unwrap()
            .branch,
        "feature"
    );
    assert_eq!(
        ForgeCli::Tea
            .decode_pr_head(r#"{"head":{"label":"feature"}}"#)
            .unwrap()
            .branch,
        "feature"
    );
}

#[test]
fn parses_tea_combined_commit_status() {
    for (state, expected) in [
        ("success", WorktreeCi::Passing),
        ("pending", WorktreeCi::Pending),
        ("failure", WorktreeCi::Failing),
        ("error", WorktreeCi::Failing),
        ("warning", WorktreeCi::Failing),
    ] {
        let raw = format!(r#"{{"state":"{state}"}}"#);
        assert_eq!(parse_tea_combined_status(&raw).unwrap(), Some(expected));
    }

    for raw in [
        r#"{"state":""}"#,
        r#"{"state":"unknown"}"#,
        r#"{}"#,
        r#"{"message":"not found"}"#,
    ] {
        assert_eq!(parse_tea_combined_status(raw).unwrap(), None);
    }
    assert!(parse_tea_combined_status("[]").is_err());
}

#[test]
fn tea_commit_status_endpoint_carries_repo_and_branch() {
    assert_eq!(
        tea_commit_status_endpoint("org/repo", "feature/topic"),
        "repos/org/repo/commits/feature/topic/status"
    );
}

#[test]
fn renders_forge_refspecs() {
    assert_eq!(
        Forge::GitHubStyle.pr_refspec(5),
        "refs/pull/5/head".to_owned()
    );
    assert_eq!(
        Forge::GitLab.pr_refspec(5),
        "refs/merge-requests/5/head".to_owned()
    );
}

#[test]
fn rejects_unusable_input() {
    assert!(parse("not-a-number").is_err());
    assert!(parse("https://github.com/org/repo/pull/nope").is_err());
    assert!(parse("https://example.test/org/repo/issues/1").is_err());
}
