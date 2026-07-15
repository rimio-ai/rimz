use super::*;

fn workspace(root_class: RootClass) -> ResolvedWorkspace {
    let project_root = PathBuf::from("/code/query-engine");
    ResolvedWorkspace {
        workspace_id: crate::ids::WorkspaceId::from_project_root(&project_root),
        project_root: project_root.clone(),
        root_class,
        worktree_root: project_root,
        worktree_branch: None,
        session_name: "rimz-query-engine".to_owned(),
        mux_hint: None,
    }
}

#[test]
fn launch_checkout_without_flags_keeps_current_worktree() {
    let workspace = workspace(RootClass::Directory);

    let checkout = resolve_launch_checkout(&workspace, &WorktreeConfig::default(), None, None)
        .expect("current checkout");

    assert_eq!(checkout.cwd, workspace.worktree_root);
    assert_eq!(checkout.worktree_name, None);
    assert_eq!(checkout.generated_name(), None);
}

#[test]
fn launch_checkout_reports_non_repository_flags_exactly() {
    let workspace = workspace(RootClass::Directory);
    let config = WorktreeConfig::default();

    let worktree = resolve_launch_checkout(&workspace, &config, Some("demo"), None)
        .expect_err("worktree needs repo");
    assert_eq!(
        worktree.to_string(),
        "--worktree requires a git repository-backed room"
    );

    let pr = PrTarget {
        number: 42,
        forge: None,
    };
    let from_pr =
        resolve_launch_checkout(&workspace, &config, None, Some(&pr)).expect_err("PR needs repo");
    assert_eq!(
        from_pr.to_string(),
        "--from-pr requires a git repository-backed room"
    );
}

#[test]
fn generated_launch_name_is_exposed_only_for_bare_checkout() {
    let generated = LaunchCheckout {
        cwd: PathBuf::from("/code/query-engine-worktrees/swift-orbit"),
        worktree_name: Some("swift-orbit".to_owned()),
        generated_name: true,
    };
    let named = LaunchCheckout {
        generated_name: false,
        ..generated.clone()
    };

    assert_eq!(generated.generated_name(), Some("swift-orbit"));
    assert_eq!(named.generated_name(), None);
}

#[test]
fn template_expands_relative_to_repo_root() {
    let config = WorktreeConfig::default();
    assert_eq!(
        worktree_parent(Path::new("/code/query-engine"), &config).expect("parent"),
        PathBuf::from("/code/query-engine/../query-engine-worktrees")
    );
}

#[test]
fn auto_name_is_deterministic_and_retries_with_suffix() {
    let seed = Uuid::parse_str("01890f3c-0000-7000-8000-000000000001").expect("uuid");
    assert_eq!(auto_name_from_uuid(seed, 0), auto_name_from_uuid(seed, 0));
    assert!(auto_name_from_uuid(seed, 1).ends_with("-1"));
}

#[test]
fn requested_name_maps_branch_style_spelling_to_dashed_worktree_name() {
    assert_eq!(
        parse_requested_name("feat/great").unwrap(),
        RequestedName {
            name: "feat-great".to_owned(),
            branch: Some("feat/great".to_owned()),
        }
    );
    assert_eq!(
        parse_requested_name("feat-great").unwrap(),
        RequestedName {
            name: "feat-great".to_owned(),
            branch: None,
        }
    );
}

#[test]
fn requested_name_rejects_empty_segments_and_bad_chars() {
    for raw in ["", "feat/", "/feat", "feat//great", "feat/great.work"] {
        assert!(
            matches!(
                parse_requested_name(raw),
                Err(WorktreeErr::InvalidName(name)) if name == raw
            ),
            "{raw} should be invalid"
        );
    }
}

#[test]
fn explicit_branch_wins_over_branch_style_spelling() {
    assert_eq!(
        resolve_branch(Some("other"), Some("feat/great"), "feat-great").unwrap(),
        "other"
    );
    assert_eq!(
        resolve_branch(None, Some("feat/great"), "feat-great").unwrap(),
        "feat/great"
    );
    assert_eq!(
        resolve_branch(None, None, "feat-great").unwrap(),
        "feat-great"
    );
}

#[test]
fn landed_verdict_and_status_constructors() {
    assert!(LandedVerdict::Landed.is_landed());
    assert!(!LandedVerdict::Pending.is_landed());
    assert!(!LandedVerdict::Unknown.is_landed());

    assert_eq!(
        WorktreeStatus::default(),
        WorktreeStatus {
            dirty: false,
            landed: LandedVerdict::Landed,
        }
    );
    assert_eq!(
        WorktreeStatus::unknown(),
        WorktreeStatus {
            dirty: false,
            landed: LandedVerdict::Unknown,
        }
    );
}

#[test]
fn protection_set_normalizes_containment_and_excludes_own_and_sidebar_panes() {
    let own = PaneId::from_parts(crate::ids::MuxName::Zellij, "terminal_own");
    let panes = vec![
        PaneProtectionFact {
            pane_id: own.clone(),
            cwd: Some(PathBuf::from("/repo-worktrees/demo")),
            sidebar: false,
        },
        PaneProtectionFact {
            pane_id: PaneId::from_parts(crate::ids::MuxName::Zellij, "terminal_sidebar"),
            cwd: Some(PathBuf::from("/repo-worktrees/demo")),
            sidebar: true,
        },
        PaneProtectionFact {
            pane_id: PaneId::from_parts(crate::ids::MuxName::Zellij, "terminal_user"),
            cwd: Some(PathBuf::from("/repo/../repo-worktrees/demo/src")),
            sidebar: false,
        },
    ];
    let protections = ProtectionSet::from_facts(&panes, &[], Some(&own));

    assert!(protections.protects(Path::new("/repo-worktrees/demo")));
    assert!(!protections.protects(Path::new("/repo-worktrees/other")));
    assert!(
        !ProtectionSet::from_facts(&panes[..2], &[], Some(&own))
            .protects(Path::new("/repo-worktrees/demo"))
    );
}

#[test]
fn protection_set_applies_agent_liveness_rules() {
    let stored = Some(PathBuf::from("/repo-worktrees/demo"));
    let process = Some(PathBuf::from("/repo-worktrees/demo/src"));
    for (liveness, stored_path, process_cwd, expected) in [
        (AgentLiveness::Dead, stored.clone(), process.clone(), false),
        (AgentLiveness::Unknown, stored.clone(), None, true),
        (
            AgentLiveness::Live { pid: 7 },
            Some(PathBuf::from("/repo-worktrees/other")),
            process,
            true,
        ),
    ] {
        let protections = ProtectionSet::from_facts(
            &[],
            &[AgentProtectionFact {
                pane_id: None,
                liveness,
                stored_path,
                process_cwd,
            }],
            None,
        );
        assert_eq!(
            protections.protects(Path::new("/repo-worktrees/demo")),
            expected
        );
    }
}

#[test]
fn removal_assessment_uses_in_use_dirty_landing_precedence() {
    let protections = ProtectionSet::from_facts(
        &[PaneProtectionFact {
            pane_id: PaneId::from_parts(crate::ids::MuxName::Tmux, "%1"),
            cwd: Some(PathBuf::from("/repo-worktrees/in-use/src")),
            sidebar: false,
        }],
        &[],
        None,
    );
    let cases = [
        (
            "/repo-worktrees/in-use",
            WorktreeStatus {
                dirty: true,
                landed: LandedVerdict::Pending,
            },
            RemovalAssessment::InUse,
        ),
        (
            "/repo-worktrees/dirty",
            WorktreeStatus {
                dirty: true,
                landed: LandedVerdict::Pending,
            },
            RemovalAssessment::Dirty,
        ),
        (
            "/repo-worktrees/pending",
            WorktreeStatus {
                dirty: false,
                landed: LandedVerdict::Pending,
            },
            RemovalAssessment::NotLanded,
        ),
        (
            "/repo-worktrees/unknown",
            WorktreeStatus::unknown(),
            RemovalAssessment::NotLanded,
        ),
        (
            "/repo-worktrees/clean",
            WorktreeStatus::default(),
            RemovalAssessment::Removable,
        ),
    ];
    for (path, status, expected) in cases {
        assert_eq!(protections.assess(Path::new(path), status), expected);
    }
}

#[test]
fn parses_git_worktree_porcelain() {
    let raw = "\
worktree /code/query-engine
HEAD abc
branch refs/heads/main

worktree /code/query-engine-worktrees/swift-otter
HEAD def
branch refs/heads/swift-otter

";
    assert_eq!(
        parse_worktree_list(raw),
        vec![
            WorktreeRow {
                path: PathBuf::from("/code/query-engine"),
                branch: Some("main".to_owned())
            },
            WorktreeRow {
                path: PathBuf::from("/code/query-engine-worktrees/swift-otter"),
                branch: Some("swift-otter".to_owned())
            }
        ]
    );
}

#[test]
fn marker_v2_json_parses_without_base_branch() {
    let raw = r#"{
        "version": 2,
        "name": "demo",
        "branch": "demo",
        "base_ref": "0123456789abcdef0123456789abcdef01234567",
        "repo_root": "/repo",
        "worktree_path": "/repo-worktrees/demo",
        "created_at": "2026-06-10T00:00:00Z"
    }"#;

    let marker: WorktreeMarker = serde_json::from_str(raw).expect("marker");

    assert_eq!(marker.version, 2);
    assert_eq!(marker.base_branch, None);
    assert_eq!(marker.from_pr, None);
}

#[test]
fn marker_v3_json_parses_without_pr_provenance() {
    let raw = r#"{
        "version": 3,
        "name": "demo",
        "branch": "demo",
        "base_branch": "main",
        "base_ref": "0123456789abcdef0123456789abcdef01234567",
        "repo_root": "/repo",
        "worktree_path": "/repo-worktrees/demo",
        "created_at": "2026-06-10T00:00:00Z"
    }"#;

    let marker: WorktreeMarker = serde_json::from_str(raw).expect("marker");

    assert_eq!(marker.version, 3);
    assert_eq!(marker.from_pr, None);
}

#[test]
fn checkout_metadata_marker_reader_follows_relative_gitdir_file() {
    let dir = tempfile::tempdir().unwrap();
    let worktree = dir.path().join("wt");
    let admin = dir.path().join("admin/wt");
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::create_dir_all(&admin).unwrap();
    std::fs::write(worktree.join(".git"), "gitdir: ../admin/wt\n").unwrap();
    let marker = WorktreeMarker {
        version: 1,
        name: "feature".to_owned(),
        branch: "feature".to_owned(),
        base_branch: Some("main".to_owned()),
        from_pr: None,
        base_ref: "HEAD".to_owned(),
        repo_root: dir.path().to_path_buf(),
        worktree_path: worktree.clone(),
        created_at: jiff::Timestamp::now(),
    };
    crate::store::atomic::write_temp_then_rename(&admin.join(MARKER_FILE), &marker).unwrap();

    assert_eq!(
        read_marker_from_checkout_metadata(&worktree)
            .unwrap()
            .map(|marker| marker.name),
        Some("feature".to_owned())
    );
}

#[test]
fn discovery_returns_owned_identity_before_explicit_inspection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = init_test_repo(dir.path());
    let config = test_worktree_config(dir.path());
    let created = create(&repo, &config, Some("demo"), None, None, false).expect("create");
    std::fs::write(
        created.marker.worktree_path.join("feature.txt"),
        "feature\n",
    )
    .expect("feature file");
    git_run(&created.marker.worktree_path, ["add", "feature.txt"]).expect("add feature");
    git_run(&created.marker.worktree_path, ["commit", "-m", "feature"]).expect("feature commit");
    std::fs::write(created.marker.worktree_path.join("dirty.txt"), "dirty\n").expect("dirty file");

    let discovered = discover_owned(&repo).expect("discover");

    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].marker, created.marker);
    assert_eq!(discovered[0].branch.as_deref(), Some("demo"));
    let inspected = status(&discovered[0].path, &discovered[0].marker).expect("inspect");
    assert!(inspected.dirty);
    assert_eq!(inspected.landed, LandedVerdict::Pending);
}

#[test]
fn reused_worktree_keeps_seeded_and_linked_destinations_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = init_test_repo(dir.path());
    let config = test_worktree_config(dir.path());
    std::fs::write(repo.join(".env.local"), "first\n").expect("seed source");
    std::fs::write(repo.join(".worktreeinclude"), ".env.local\n").expect("include config");
    std::fs::create_dir_all(repo.join("node_modules/pkg")).expect("link source");
    std::fs::write(
        repo.join("node_modules/pkg/index.js"),
        "module.exports = 1\n",
    )
    .expect("link file");
    std::fs::write(repo.join(".worktreelink"), "node_modules\n").expect("link config");

    let first = create(&repo, &config, Some("demo"), None, None, false).expect("fresh create");
    assert_eq!((first.included, first.linked), (1, 1));
    let seeded = first.marker.worktree_path.join(".env.local");
    let linked = first.marker.worktree_path.join("node_modules");
    std::fs::write(&seeded, "keep\n").expect("change seeded destination");

    let reused = create(&repo, &config, Some("demo"), None, None, true).expect("reuse");

    assert_eq!((reused.included, reused.linked), (0, 0));
    assert_eq!(std::fs::read_to_string(seeded).unwrap(), "keep\n");
    assert!(
        std::fs::symlink_metadata(linked)
            .expect("linked destination")
            .is_symlink()
    );
}

fn init_test_repo(parent: &Path) -> PathBuf {
    let repo = parent.join("repo");
    std::fs::create_dir_all(&repo).expect("repo dir");
    git_run(&repo, ["init", "-b", "main"]).expect("git init");
    git_run(&repo, ["config", "user.email", "rimz@example.test"]).expect("git email");
    git_run(&repo, ["config", "user.name", "Rimz Test"]).expect("git name");
    std::fs::write(repo.join("README.md"), "base\n").expect("base file");
    git_run(&repo, ["add", "README.md"]).expect("git add");
    git_run(&repo, ["commit", "-m", "base"]).expect("base commit");
    repo
}

fn test_worktree_config(parent: &Path) -> WorktreeConfig {
    WorktreeConfig {
        dir: parent.join("worktrees").display().to_string(),
        ..WorktreeConfig::default()
    }
}

#[test]
fn protection_facts_filter_sidebar_own_and_count_user_panes() {
    let worktree = Path::new("/repo-worktrees/demo");
    let own = PaneId::from_parts(crate::ids::MuxName::Zellij, "terminal_own");
    let panes = vec![
        pane("terminal_side", Some("rimz-sidebar"), Some(worktree)),
        pane("terminal_outside", Some("zsh"), Some(Path::new("/repo"))),
        pane("terminal_own", Some("codex"), Some(worktree)),
    ];

    assert!(!protection_set_from_runtime(&panes, &[], Some(&own)).protects(worktree));

    let shell_dir = worktree.join("src");
    let agent = vec![pane("terminal_agent", Some("codex"), Some(worktree))];
    let shell = vec![pane("terminal_shell", Some("zsh"), Some(&shell_dir))];

    assert!(protection_set_from_runtime(&agent, &[], Some(&own)).protects(worktree));
    assert!(protection_set_from_runtime(&shell, &[], Some(&own)).protects(worktree));
}

#[test]
fn protection_facts_apply_agent_liveness_and_own_pane() {
    let worktree = Path::new("/repo-worktrees/demo");
    let own = PaneId::from_parts(crate::ids::MuxName::Zellij, "terminal_own");
    let now = jiff::Timestamp::from_second(1_700_000_000).unwrap();

    assert!(
        protection_set_from_runtime(
            &[],
            &[agent(
                "inflight",
                Some("/repo/../repo-worktrees/demo"),
                None,
                now,
            )],
            Some(&own),
        )
        .protects(worktree)
    );
    assert!(
        protection_set_from_runtime(
            &[],
            &[agent(
                "other-pane",
                Some("/repo-worktrees/demo/src"),
                Some("terminal_other"),
                now,
            )],
            Some(&own),
        )
        .protects(worktree)
    );
    assert!(
        !protection_set_from_runtime(
            &[],
            &[agent(
                "own",
                Some("/repo-worktrees/demo"),
                Some("terminal_own"),
                now,
            )],
            Some(&own),
        )
        .protects(worktree)
    );
    assert!(
        protection_set_from_runtime(
            &[],
            &[agent(
                "idle-live-unknown",
                Some("/repo-worktrees/demo"),
                None,
                now - std::time::Duration::from_secs(30),
            )],
            Some(&own),
        )
        .protects(worktree)
    );
    #[cfg(target_os = "linux")]
    {
        let mut dead = agent("dead", Some("/repo-worktrees/demo"), None, now);
        dead.runtime_owner = Some(crate::RuntimeOwner::new(
            crate::RuntimeOwnerKind::Agent,
            "dead",
            u32::MAX,
            None,
        ));
        assert!(!protection_set_from_runtime(&[], &[dead], Some(&own)).protects(worktree));
    }
    let mut live = agent("live", Some("/repo-worktrees/demo"), None, now);
    live.runtime_owner = Some(crate::store::runtime::current_process_owner(
        crate::RuntimeOwnerKind::Agent,
        "live",
    ));
    assert!(protection_set_from_runtime(&[], &[live], Some(&own)).protects(worktree));
    assert!(
        !protection_set_from_runtime(
            &[],
            &[agent(
                "other-worktree",
                Some("/repo-worktrees/other"),
                None,
                now,
            )],
            Some(&own),
        )
        .protects(worktree)
    );
}

#[cfg(target_os = "linux")]
#[test]
fn runtime_protection_includes_realtime_process_cwd() {
    let now = jiff::Timestamp::from_second(1_700_000_000).unwrap();
    let mut live = agent("live", Some("/repo-worktrees/other"), None, now);
    live.runtime_owner = Some(crate::store::runtime::current_process_owner(
        crate::RuntimeOwnerKind::Agent,
        "live",
    ));

    let current = normalize_path_lexical(&std::env::current_dir().expect("current dir"));

    assert!(protection_set_from_runtime(&[], &[live], None).protects(&current));
}

fn pane(raw: &str, command: Option<&str>, cwd: Option<&Path>) -> PaneRef {
    PaneRef {
        command: command.map(ToOwned::to_owned),
        cwd: cwd.map(|path| path.display().to_string()),
        ..PaneRef::from_id(PaneId::from_parts(crate::ids::MuxName::Zellij, raw))
    }
}

fn agent(
    id: &str,
    worktree_path: Option<&str>,
    raw_pane: Option<&str>,
    last_seen: jiff::Timestamp,
) -> AgentState {
    AgentState {
        name: Some(id.to_owned()),
        pane: raw_pane.map(|raw| pane(raw, Some("codex"), None)),
        worktree_path: worktree_path.map(ToOwned::to_owned),
        ..crate::testkit::agent_state("codex", id, last_seen)
    }
}
