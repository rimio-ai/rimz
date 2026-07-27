use super::*;

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

    assert!(pr_url_matches_origin(
        &target,
        "git@github.com:org/repo.git"
    ));
    assert!(!pr_url_matches_origin(
        &target,
        "ssh://git@github.com/other/repo.git"
    ));
    assert!(!pr_url_matches_origin(
        &target,
        "git@gitlab.com:org/repo.git"
    ));
    assert!(pr_url_matches_origin(
        &parse("7").unwrap(),
        "git@gitlab.com:other/repo.git"
    ));
    assert!(pr_url_matches_origin(
        &parse("https://GITHUB.com/Org/Team/Repo/pull/7").unwrap(),
        "git@github.com:org/team/repo.git"
    ));
}

#[test]
fn maps_remote_hosts_to_forge() {
    for remote in [
        "https://github.com/org/repo.git",
        "git@github.com:org/repo.git",
        "https://gitea.example.test/org/repo.git",
        "git@gitea.example.test:org/repo.git",
    ] {
        assert_eq!(forge_for_remote(remote), Forge::GitHubStyle, "{remote}");
    }
    for remote in [
        "https://gitlab.com/org/repo.git",
        "git@gitlab.com:org/repo.git",
        "ssh://git@gitlab.example.test/org/repo.git",
        "ssh://git@gitlab.example.test:2222/org/repo.git",
    ] {
        assert_eq!(forge_for_remote(remote), Forge::GitLab, "{remote}");
    }
}

#[test]
fn maps_remote_hosts_to_forge_cli() {
    for remote in [
        "https://github.com/org/repo.git",
        "git@github.com:org/repo.git",
    ] {
        assert_eq!(forge_cli_for_remote(remote), Some(ForgeCli::Gh), "{remote}");
    }
    for remote in [
        "https://gitea.example.test/org/repo.git",
        "git@forgejo.example.test:org/repo.git",
        "https://codeberg.org/org/repo.git",
    ] {
        assert_eq!(
            forge_cli_for_remote(remote),
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
        assert_eq!(forge_cli_for_remote(remote), None, "{remote}");
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
        assert_eq!(remote_repo_slug(remote), Some(slug.to_owned()), "{remote}");
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
        assert_eq!(remote_repo_slug(remote), None, "{remote}");
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
                "head":{"label":"alice:feature","repo":{"full_name":"alice/fork"}},
                "base":{"repo":{"full_name":"org/repo"}}
            }"#
        )
        .unwrap(),
        PrHead {
            branch: "feature".to_owned(),
            owner: Some("alice".to_owned()),
            repo_full_name: Some("alice/fork".to_owned()),
            is_cross_repository: None,
        }
    );
    assert_eq!(
        parse_tea_pr_head_json(r#"{"head":{"ref":"feature","repo":{"full_name":"org/repo"}}}"#)
            .unwrap(),
        PrHead {
            branch: "feature".to_owned(),
            owner: Some("org".to_owned()),
            repo_full_name: Some("org/repo".to_owned()),
            is_cross_repository: None,
        }
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
            sibling_repo_url(origin, "alice/fork").as_deref(),
            Some(expected),
            "{origin}"
        );
    }
    assert_eq!(
        sibling_repo_url("https://host/org/repo.git", " /alice/fork/ ").as_deref(),
        Some("https://host/alice/fork.git")
    );
    assert_eq!(sibling_repo_url("/tmp/origin.git", "alice/fork"), None);
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
fn builds_github_bulk_query_with_ordered_escaped_aliases() {
    let query = github_bulk_query(
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
    assert_eq!(query.matches("pullRequests(").count(), 2);
    assert_eq!(query.matches(": object(").count(), 2);
}

#[test]
fn parses_github_bulk_prs_and_commits_by_alias() {
    let response = parse_github_bulk_response(
        r#"{
            "data": {
                "repository": {
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
                number: 12,
                state: WorktreePrState::Open,
                created_at: Some("2026-07-18T01:25:14Z".parse().unwrap()),
                head_ci: Some(WorktreePrCi::Pending),
                merge_sha: None,
                merge_ci: None,
            }),
            Some(GhBulkPr {
                number: 20,
                state: WorktreePrState::Merged,
                created_at: Some("2026-07-18T01:26:14Z".parse().unwrap()),
                head_ci: Some(WorktreePrCi::Failing),
                merge_sha: Some("merge-sha".to_owned()),
                merge_ci: Some(WorktreePrCi::Passing),
            }),
            Some(GhBulkPr {
                number: 30,
                state: WorktreePrState::Closed,
                created_at: None,
                head_ci: Some(WorktreePrCi::Failing),
                merge_sha: None,
                merge_ci: None,
            }),
            None,
            Some(GhBulkPr {
                number: 40,
                state: WorktreePrState::Merged,
                created_at: None,
                head_ci: Some(WorktreePrCi::Pending),
                merge_sha: None,
                merge_ci: None,
            }),
        ]
    );
    assert_eq!(
        response.commits,
        vec![
            Some(WorktreePrCi::Passing),
            Some(WorktreePrCi::Failing),
            None,
            None,
        ]
    );
}

#[test]
fn github_rollup_state_mapping_is_aggregate_only() {
    for (state, expected) in [
        ("SUCCESS", Some(WorktreePrCi::Passing)),
        ("failure", Some(WorktreePrCi::Failing)),
        ("ERROR", Some(WorktreePrCi::Failing)),
        ("PENDING", Some(WorktreePrCi::Pending)),
        ("expected", Some(WorktreePrCi::Pending)),
        ("NEUTRAL", None),
        ("", None),
    ] {
        assert_eq!(ci_from_gh_rollup_state(state), expected, "{state}");
    }
}

#[test]
fn rejects_incomplete_or_error_github_bulk_responses() {
    assert!(parse_github_bulk_response("{", 0, 0).is_err());
    assert!(
        parse_github_bulk_response(
            r#"{"errors":[{"message":"rate limited"}],"data":{"repository":{"pr0":{"nodes":[]}}}}"#,
            1,
            0,
        )
        .is_err()
    );
    assert!(parse_github_bulk_response(r#"{"data":{"repository":null}}"#, 0, 0).is_err());
    assert!(
        parse_github_bulk_response(r#"{"data":{"repository":{"pr0":{"nodes":[]}}}}"#, 2, 0)
            .is_err()
    );
    assert!(parse_github_bulk_response(r#"{"data":{"repository":{"sha0":null}}}"#, 0, 2).is_err());
    assert!(
        parse_github_bulk_response(r#"{"data":{"repository":{"sha0":null}}}"#, 0, 1)
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
        "pr 42 --output json --repo org/repo"
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
            .decode_pr_head(r#"{"head":{"label":"org:feature"}}"#)
            .unwrap()
            .branch,
        "feature"
    );
}

#[test]
fn parses_tea_combined_commit_status() {
    for (state, expected) in [
        ("success", WorktreePrCi::Passing),
        ("pending", WorktreePrCi::Pending),
        ("failure", WorktreePrCi::Failing),
        ("error", WorktreePrCi::Failing),
        ("warning", WorktreePrCi::Failing),
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
