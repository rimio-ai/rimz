use super::*;
use crate::ids::AgentKind;
use tempfile::tempdir;

fn project_with(text: &str) -> tempfile::TempDir {
    let dir = tempdir().expect("tempdir");
    let config_dir = dir.path().join(".rimz");
    std::fs::create_dir_all(&config_dir).expect("mkdir .rimz");
    std::fs::write(config_dir.join("config.toml"), text).expect("write config");
    dir
}

fn birth_prompt_due(project_root: &Path, config_root: &Path) -> bool {
    birth_prompt_with_roots(project_root, config_root)
        .expect("birth prompt")
        .is_some()
}

#[test]
fn blocked_fix_distinguishes_stale_from_untrusted() {
    assert!(blocked_fix(TrustState::Stale).contains("since your last grant\n"));
    assert!(blocked_fix(TrustState::Untrusted).starts_with("review the project config"));
}

#[test]
fn empty_project_reports_no_config() {
    let dir = tempdir().expect("tempdir");
    let config = tempdir().expect("config root");
    let report = status_with_roots(dir.path(), config.path()).expect("status");
    assert_eq!(report.state, TrustState::NoConfig);
    assert!(report.current_hash.is_none());
    assert!(report.granted_hash.is_none());
    assert!(!birth_prompt_due(dir.path(), config.path()));
}

#[test]
fn fresh_config_reports_untrusted() {
    let dir = project_with("[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n");
    let config = tempdir().expect("config root");
    let report = status_with_roots(dir.path(), config.path()).expect("status");
    assert_eq!(report.state, TrustState::Untrusted);
    assert!(report.current_hash.is_some());
    assert!(report.granted_hash.is_none());
}

#[test]
fn birth_prompt_offers_current_hash_and_summary_for_untrusted_config() {
    let dir = project_with(
        "[tasks.sync]\nagent = \"codex\"\nprompt = \"sync the repo\"\n\n[profiles.planner]\nagent = \"claude\"\n\n[subagents.profiles.reviewer]\nagent = \"codex\"\n\n[agents.teams.review]\nlayout = \"planner\"\n\n[[agents.teams.review.roles]]\nrole = \"planner\"\nprofile = \"planner\"\n\n[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n",
    );
    let config = tempdir().expect("config root");
    let offer = birth_prompt_with_roots(dir.path(), config.path())
        .expect("birth prompt")
        .expect("offer");
    let project_config = read_project_config(&dir.path().join(CONFIG_REL))
        .expect("read config")
        .expect("config present");

    assert_eq!(offer.current_hash, executable_surface_hash(&project_config));
    assert_eq!(offer.summary.task_names, vec!["sync".to_owned()]);
    assert_eq!(offer.summary.profiles, vec!["planner".to_owned()]);
    assert_eq!(offer.summary.teams, vec!["review".to_owned()]);
    assert_eq!(offer.summary.hooks, 1);
}

#[test]
fn birth_prompt_dismissal_suppresses_until_surface_changes_and_grant_cleans_it() {
    let dir = project_with("[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n");
    let config = tempdir().expect("config root");
    let dismissal_path =
        birth_prompt_path(config.path(), &WorkspaceId::from_project_root(dir.path()));

    let offer = birth_prompt_with_roots(dir.path(), config.path())
        .expect("birth prompt")
        .expect("offer");
    dismiss_birth_prompt_offer_with_roots(dir.path(), config.path(), &offer)
        .expect("dismiss prompt");
    assert!(dismissal_path.exists());
    assert!(!birth_prompt_due(dir.path(), config.path()));

    std::fs::write(
        dir.path().join(CONFIG_REL),
        "[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks codex\"\n",
    )
    .expect("rewrite");
    assert!(birth_prompt_due(dir.path(), config.path()));

    grant_with_roots(dir.path(), config.path()).expect("grant");
    assert!(!dismissal_path.exists());
}

#[test]
fn grant_pins_hash_and_returns_trusted() {
    let dir = project_with("[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n");
    let config = tempdir().expect("config root");
    let granted = grant_with_roots(dir.path(), config.path()).expect("grant");
    assert_eq!(granted.state, TrustState::Trusted);
    let now = status_with_roots(dir.path(), config.path()).expect("status");
    assert_eq!(now.state, TrustState::Trusted);
    assert_eq!(now.current_hash, granted.current_hash);
    assert_eq!(now.granted_hash, granted.current_hash);
    assert!(!birth_prompt_due(dir.path(), config.path()));
}

#[test]
fn editing_command_field_demotes_to_stale() {
    let dir = project_with("[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n");
    let config = tempdir().expect("config root");
    grant_with_roots(dir.path(), config.path()).expect("grant");

    std::fs::write(
        dir.path().join(".rimz/config.toml"),
        "[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks codex\"\n",
    )
    .expect("rewrite");

    let report = status_with_roots(dir.path(), config.path()).expect("status");
    assert_eq!(report.state, TrustState::Stale);
    assert_ne!(report.current_hash, report.granted_hash);
    assert!(!birth_prompt_due(dir.path(), config.path()));
    let entries = report
        .surface_diff
        .expect("stale status should explain changed fields");
    assert!(entries.iter().any(|entry| {
        entry.kind == SurfaceDiffKind::Changed
            && entry.path == vec!["hooks".to_owned(), "[0]".to_owned(), "command".to_owned()]
            && entry.granted == Some(serde_json::json!("rimz hooks claude"))
            && entry.current == Some(serde_json::json!("rimz hooks codex"))
    }));
}

#[test]
fn editing_profile_field_demotes_to_stale() {
    let dir = project_with("[profiles.planner]\nagent = \"claude\"\nargs = \"--safe\"\n");
    let config = tempdir().expect("config root");
    grant_with_roots(dir.path(), config.path()).expect("grant");

    std::fs::write(
        dir.path().join(".rimz/config.toml"),
        "[profiles.planner]\nagent = \"codex\"\nargs = \"--safe\"\n",
    )
    .expect("rewrite");

    let report = status_with_roots(dir.path(), config.path()).expect("status");
    assert_eq!(report.state, TrustState::Stale);
    assert_ne!(report.current_hash, report.granted_hash);
}

#[test]
fn unknown_non_command_field_does_not_change_hash() {
    let base = project_with(
        "display_name = \"Query Engine\"\n\n[profiles.x]\nagent = \"claude\"\n\n[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n",
    );
    let extra = project_with(
        "display_name = \"Query Engine dev\"\nsidebar = true\n\n[profiles.x]\nagent = \"claude\"\nsubagents = [\"explorer\"]\n\n[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n",
    );
    let a = read_project_config(&base.path().join(CONFIG_REL))
        .expect("read base")
        .expect("config present");
    let b = read_project_config(&extra.path().join(CONFIG_REL))
        .expect("read extra")
        .expect("config present");
    assert_eq!(executable_surface_hash(&a), executable_surface_hash(&b));
}

#[test]
fn project_notifications_do_not_enter_trust_hash() {
    let base = project_with("[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n");
    let extra = project_with(
        "[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n\n[notifications]\ntitle = \"{{task}}\"\n[[notifications.handler]]\ncommand = \"ntfy publish rimz {{body}}\"\n",
    );
    let a = read_project_config(&base.path().join(CONFIG_REL))
        .expect("read base")
        .expect("config present");
    let b = read_project_config(&extra.path().join(CONFIG_REL))
        .expect("read extra")
        .expect("config present");
    assert_eq!(executable_surface_hash(&a), executable_surface_hash(&b));
}

#[test]
fn revoke_drops_record_and_returns_untrusted() {
    let dir = project_with("[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks\"\n");
    let config = tempdir().expect("config root");
    grant_with_roots(dir.path(), config.path()).expect("grant");
    let revoked = revoke_with_roots(dir.path(), config.path()).expect("revoke");
    assert_eq!(revoked.state, TrustState::Untrusted);
    assert!(!revoked.record_path.exists());
}

#[test]
fn project_layout_table_fails_with_per_machine_fix() {
    let dir = project_with("[[layout.initial_panes]]\nname = \"shell\"\ncommand = \"$SHELL\"\n");
    let config = tempdir().expect("config root");
    let err = status_with_roots(dir.path(), config.path()).expect_err("layout must fail");
    let rendered = err.to_string();
    assert!(rendered.contains("[layout]"), "{rendered}");
    assert!(rendered.contains("per-machine"), "{rendered}");
}

#[test]
fn grant_record_stores_canonical_surface_json() {
    let dir = project_with("[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n");
    let config = tempdir().expect("config root");
    let granted = grant_with_roots(dir.path(), config.path()).expect("grant");
    let record = read_trust_record(&granted.record_path)
        .expect("read trust record")
        .expect("record present");
    let parsed: Value = serde_json::from_str(&record.surface_json).expect("surface json");
    assert_eq!(parsed["hooks"][0]["command"], "rimz hooks claude");
}

#[test]
fn trust_record_without_surface_json_fails_to_parse() {
    let dir = project_with("[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n");
    let config = tempdir().expect("config root");
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let record_path = trust_record_path(config.path(), &workspace_id);
    let original_config = read_project_config(&dir.path().join(CONFIG_REL))
        .expect("read config")
        .expect("config present");
    #[derive(Serialize)]
    struct LegacyTrustRecord<'a> {
        project_root: &'a Path,
        surface_hash: String,
        granted_at: Timestamp,
    }
    let record = LegacyTrustRecord {
        project_root: dir.path(),
        surface_hash: executable_surface_hash(&original_config),
        granted_at: Timestamp::now(),
    };
    let text = toml::to_string_pretty(&record).expect("serialize legacy record");
    write_bytes_atomically(&record_path, text.as_bytes()).expect("write record");

    let err = status_with_roots(dir.path(), config.path()).expect_err("status must fail");
    match err {
        TrustErr::RecordParse { path, diagnosis } => {
            assert_eq!(path, record_path);
            let rendered = diagnosis.to_string();
            assert!(
                rendered.contains("missing field `surface_json`"),
                "{rendered}"
            );
        }
        other => panic!("expected record parse error, got {other:?}"),
    }
}

#[test]
fn corrupt_stored_surface_json_returns_structured_error() {
    let dir = project_with("[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n");
    let config = tempdir().expect("config root");
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let record_path = trust_record_path(config.path(), &workspace_id);
    let original_config = read_project_config(&dir.path().join(CONFIG_REL))
        .expect("read config")
        .expect("config present");
    let record = TrustRecord {
        project_root: dir.path().to_path_buf(),
        surface_hash: executable_surface_hash(&original_config),
        surface_json: "not-json".to_owned(),
        granted_at: Timestamp::now(),
    };
    let text = toml::to_string_pretty(&record).expect("serialize record");
    write_bytes_atomically(&record_path, text.as_bytes()).expect("write record");

    std::fs::write(
        dir.path().join(CONFIG_REL),
        "[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks codex\"\n",
    )
    .expect("rewrite");

    let err = status_with_roots(dir.path(), config.path()).expect_err("status must fail");
    match err {
        TrustErr::RecordSurfaceJson { path, source } => {
            assert_eq!(path, record_path);
            assert!(source.is_syntax(), "{source}");
        }
        other => panic!("expected surface json error, got {other:?}"),
    }
}

#[test]
fn grant_returns_diff_before_repinning_stale_surface() {
    let dir = project_with("[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n");
    let config = tempdir().expect("config root");
    grant_with_roots(dir.path(), config.path()).expect("grant");
    std::fs::write(
        dir.path().join(CONFIG_REL),
        "[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks codex\"\n",
    )
    .expect("rewrite");

    let granted = grant_with_roots(dir.path(), config.path()).expect("regrant");
    assert_eq!(granted.state, TrustState::Trusted);
    assert!(matches!(
        granted.surface_diff,
        Some(ref entries) if !entries.is_empty()
    ));
    let now = status_with_roots(dir.path(), config.path()).expect("status");
    assert_eq!(now.state, TrustState::Trusted);
    assert!(now.surface_diff.is_none());
}

#[test]
fn agent_env_is_unconfigured_without_a_matching_entry() {
    let config = tempdir().expect("config root");

    let empty = tempdir().expect("tempdir");
    assert_eq!(
        agent_env_with_roots(empty.path(), config.path(), "claude").expect("agent env"),
        AgentEnv::Unconfigured,
    );

    let other_kind = project_with("[[agents]]\nname = \"claude\"\nenv = { FOO = \"1\" }\n");
    grant_with_roots(other_kind.path(), config.path()).expect("grant");
    assert_eq!(
        agent_env_with_roots(other_kind.path(), config.path(), "codex").expect("agent env"),
        AgentEnv::Unconfigured,
    );

    let no_env = project_with("[[agents]]\nname = \"claude\"\nlaunch_command = \"claude\"\n");
    grant_with_roots(no_env.path(), config.path()).expect("grant");
    assert_eq!(
        agent_env_with_roots(no_env.path(), config.path(), "claude").expect("agent env"),
        AgentEnv::Unconfigured,
    );
}

#[test]
fn agent_env_applies_merged_entries_when_trusted() {
    let dir = project_with(
        "[[agents]]\nname = \"claude\"\nenv = { A = \"1\", B = \"1\" }\n\n[[agents]]\nname = \"claude\"\nenv = { B = \"2\" }\n",
    );
    let config = tempdir().expect("config root");
    grant_with_roots(dir.path(), config.path()).expect("grant");

    let env = match agent_env_with_roots(dir.path(), config.path(), "claude") {
        Ok(AgentEnv::Apply(env)) => env,
        other => panic!("expected Apply, got {other:?}"),
    };
    assert_eq!(
        env,
        BTreeMap::from([
            ("A".to_owned(), "1".to_owned()),
            ("B".to_owned(), "2".to_owned())
        ]),
    );
}

#[test]
fn agent_env_blocks_untrusted_and_stale_workspaces() {
    let dir = project_with("[[agents]]\nname = \"claude\"\nenv = { FOO = \"1\" }\n");
    let config = tempdir().expect("config root");
    assert_eq!(
        agent_env_with_roots(dir.path(), config.path(), "claude").expect("agent env"),
        AgentEnv::Blocked(TrustState::Untrusted),
    );

    grant_with_roots(dir.path(), config.path()).expect("grant");
    std::fs::write(
        dir.path().join(CONFIG_REL),
        "[[agents]]\nname = \"claude\"\nenv = { FOO = \"2\" }\n",
    )
    .expect("rewrite");
    assert_eq!(
        agent_env_with_roots(dir.path(), config.path(), "claude").expect("agent env"),
        AgentEnv::Blocked(TrustState::Stale),
    );
}

#[test]
fn hash_covers_every_documented_surface_field() {
    // One config per documented executable-surface field. Any two must
    // hash to distinct values; if a future refactor drops a field from
    // `ExecutableSurface`, two cases collide and this test fires.
    let cases = [
        "[[agents]]\nname = \"claude\"\nlaunch_command = \"claude code\"\n",
        "[[agents]]\nname = \"claude\"\nenv = { PATH = \"/opt/llms/bin\" }\n",
        "[profiles.x]\nagent = \"claude\"\n",
        "[profiles.x]\nagent = \"codex\"\n",
        "[profiles.x]\nagent = \"claude\"\nskills = []\n",
        "[profiles.x]\nagent = \"claude\"\nskills = [\"merge\"]\n",
        "[subagents.profiles.x]\nagent = \"claude\"\nskills = []\n",
        "[subagents.profiles.x]\nagent = \"claude\"\nskills = [\"merge\"]\n",
        "[profiles.x]\nagent = \"claude\"\nmode = \"ask\"\n",
        "[profiles.x]\nagent = \"claude\"\nmodel = \"opus\"\n",
        "[profiles.x]\nagent = \"claude\"\neffort = \"low\"\n",
        "[profiles.x]\nagent = \"claude\"\nsystem-prompt-file = \"prompts/x.md\"\n",
        "[profiles.x]\nagent = \"claude\"\nappend-system-prompt-files = [\"prompts/a.md\"]\n",
        "[profiles.x]\nagent = \"claude\"\nargs = \"--profile x\"\n",
        "[profiles.y]\nagent = \"claude\"\n",
        "[agents.teams.review]\nlayout = \"planner,coder\"\n\n[[agents.teams.review.roles]]\nrole = \"planner\"\nprofile = \"x\"\n[[agents.teams.review.roles]]\nrole = \"coder\"\nprofile = \"x\"\n",
        "[[agents.teams.review.roles]]\nrole = \"planner\"\nprofile = \"x\"\n",
        "[[agents.teams.review.roles]]\nrole = \"coder\"\nprofile = \"x\"\n",
        "[[agents.teams.review.roles]]\nrole = \"planner\"\nprofile = \"y\"\n",
        "[[agents.teams.review.roles]]\nrole = \"planner\"\nprofile = \"x\"\nmode = \"ask\"\n",
        "[[agents.teams.review.roles]]\nrole = \"planner\"\nprofile = \"x\"\nmodel = \"opus\"\n",
        "[[agents.teams.review.roles]]\nrole = \"planner\"\nprofile = \"x\"\neffort = \"low\"\n",
        "[[agents.teams.review.roles]]\nrole = \"planner\"\nprofile = \"x\"\nsystem-prompt-file = \"prompts/planner.md\"\n",
        "[[agents.teams.review.roles]]\nrole = \"planner\"\nprofile = \"x\"\nappend-system-prompt-files = [\"prompts/a.md\"]\n",
        "[[agents.teams.review.roles]]\nrole = \"planner\"\nprofile = \"x\"\nargs = \"--role planner\"\n",
        "[tasks.x]\nagent = \"codex\"\n",
        "[tasks.y]\nagent = \"codex\"\n",
        "[tasks.x]\nprompt = \"repair CI\"\n",
        "[tasks.x]\nprompt-file = \"prompts/ci.md\"\n",
        "[tasks.x]\ncheck = \"cargo test\"\n",
        "[tasks.x]\nverify = \"cargo test\"\n",
        "[tasks.x]\nmax-attempts = 4\n",
        "[tasks.x]\non = \"success\"\n",
        "[tasks.x]\nworktree = \"sync\"\n",
        "[tasks.x]\nmode = \"yolo\"\n",
        "[tasks.x]\neffort = \"low\"\n",
        "[tasks.x]\nsystem-prompt-file = \"prompts/system.md\"\n",
        "[tasks.x]\ntimeout = \"2h\"\n",
        "[tasks.x]\nat = \"08:00\"\n",
        "[tasks.x]\nevery = \"15m\"\n",
        "[tasks.x]\ncron = \"0 8 * * 1\"\n",
        "[tasks.x]\nsignal = \"ci.failed\"\n",
        "[tasks.x]\nmatch = { branch = \"feature\" }\n",
        "[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n",
        "[env]\nPATH_PREPEND = \"/opt/rimz/bin\"\n",
        "[accounts]\nclaude = \"work\"\n",
        "[accounts]\nclaude = \"personal\"\n",
        "[accounts]\ncodex = \"work\"\n",
    ];
    let mut hashes = std::collections::HashSet::new();
    for text in cases {
        let config: ProjectConfig =
            toml::from_str(text).unwrap_or_else(|err| panic!("parse `{text}`: {err}"));
        assert!(
            hashes.insert(executable_surface_hash(&config)),
            "case `{text}` collided with another surface case",
        );
    }
    let role = "[[agents.teams.review.roles]]\nrole = \"coder\"\nprofile = \"codex\"\n";
    let binding = "{ signal = \"ci.failed\" }";
    let second = "{ signal = \"pr.merged\" }";
    for text in [
        format!("{role}signals = [{binding}]"),
        format!(
            "{role}signals = [{}]",
            binding.replace("ci.failed", "ci.passed")
        ),
        format!("{}signals = [{binding}]", role.replace("coder", "planner")),
        format!(
            "{role}signals = [{{ signal = \"ci.failed\", match = {{ branch = \"feature\" }} }}]"
        ),
        format!("{role}signals = [{{ signal = \"ci.failed\", match = {{ branch = \"main\" }} }}]"),
        format!("{role}signals = [{{ signal = \"ci.failed\", match = {{ path = \"feature\" }} }}]"),
        format!("{role}signals = [{{ signal = \"ci.failed\", prompt = \"Repair CI\" }}]"),
        format!("{role}signals = [{{ signal = \"ci.failed\", prompt = \"Inspect CI\" }}]"),
        format!("{role}signals = [{binding}, {second}]"),
        format!("{role}signals = [{second}, {binding}]"),
        format!(
            "{role}signals = [{binding}]\n{}",
            role.replace("coder", "planner")
        ),
        format!(
            "{role}{}signals = [{binding}]",
            role.replace("coder", "planner")
        ),
    ] {
        let config: ProjectConfig = toml::from_str(&text).expect("parse team signals");
        assert!(
            hashes.insert(executable_surface_hash(&config)),
            "case `{text}` collided with another surface case",
        );
    }
}

#[test]
fn empty_team_signals_preserve_executable_surface_hash() {
    let legacy = "[agents.teams.review]\nlayout = \"coder\"\n[[agents.teams.review.roles]]\nrole = \"coder\"\nprofile = \"codex\"\n";
    let config: ProjectConfig = toml::from_str(legacy).expect("legacy team");
    let explicit: ProjectConfig =
        toml::from_str(&format!("{legacy}signals = []\n")).expect("empty signals");
    let snapshot = surface_snapshot(&config);
    assert_eq!(snapshot.hash, executable_surface_hash(&explicit));
    assert_eq!(
        serde_json::to_string(&ExecutableSurface::from(&config).teams).expect("team surface"),
        r#"[{"name":"review","layout":"coder","roles":[{"role":"coder","profile":"codex","mode":null,"model":null,"effort":null,"system_prompt_file":null,"append_system_prompt_file":null,"args":null}]}]"#,
    );
}

#[test]
fn project_accounts_apply_only_under_trust_and_leave_old_hashes_alone() {
    let config = tempdir().expect("config root");
    let dir = project_with("[accounts]\nclaude = \"work\"\n");
    assert_eq!(
        project_logins_with_roots(dir.path(), config.path()).expect("project logins"),
        ProjectLogins::Blocked(TrustState::Untrusted),
    );
    grant_with_roots(dir.path(), config.path()).expect("grant");
    assert_eq!(
        project_logins_with_roots(dir.path(), config.path()).expect("project logins"),
        ProjectLogins::Apply(RoomLogins::from([(
            AgentKind::new_unchecked("claude"),
            "work".parse().expect("login name"),
        )])),
    );

    let invalid = project_with("[accounts]\nclaude = \"Work Laptop\"\n");
    assert!(project_logins_with_roots(invalid.path(), config.path()).is_err());

    let config: ProjectConfig = toml::from_str("[env]\nA = \"1\"\n").expect("parse");
    assert!(!surface_snapshot(&config).json.contains("accounts"));
}

#[test]
fn surface_diff_reports_added_leaf() {
    let diff = executable_surface_diff(
        &serde_json::json!({"env": {}}),
        &serde_json::json!({"env": {"FOO": "1"}}),
    );
    assert_eq!(
        diff,
        vec![SurfaceDiffEntry {
            kind: SurfaceDiffKind::Added,
            path: vec!["env".to_owned(), "FOO".to_owned()],
            granted: None,
            current: Some(serde_json::json!("1")),
        }]
    );
}

#[test]
fn surface_diff_reports_removed_leaf() {
    let diff = executable_surface_diff(
        &serde_json::json!({"env": {"FOO": "1"}}),
        &serde_json::json!({"env": {}}),
    );
    assert_eq!(
        diff,
        vec![SurfaceDiffEntry {
            kind: SurfaceDiffKind::Removed,
            path: vec!["env".to_owned(), "FOO".to_owned()],
            granted: Some(serde_json::json!("1")),
            current: None,
        }]
    );
}

#[test]
fn surface_diff_reports_changed_leaf() {
    let diff = executable_surface_diff(
        &serde_json::json!({"hooks": [{"command": "a"}]}),
        &serde_json::json!({"hooks": [{"command": "b"}]}),
    );
    assert_eq!(
        diff,
        vec![SurfaceDiffEntry {
            kind: SurfaceDiffKind::Changed,
            path: vec!["hooks".to_owned(), "[0]".to_owned(), "command".to_owned()],
            granted: Some(serde_json::json!("a")),
            current: Some(serde_json::json!("b")),
        }]
    );
}

#[test]
fn surface_diff_noops_on_equal_values() {
    let value = serde_json::json!({"hooks": [{"command": "a"}]});
    assert!(executable_surface_diff(&value, &value).is_empty());
}
