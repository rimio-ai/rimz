use super::*;

#[test]
fn judgment_retains_skipped_loop_task_warning() {
    let root = tempfile::tempdir().unwrap();
    let workspace = crate::WorkspaceResolver::resolve(root.path(), None).unwrap();
    let mut machine = MachineConfig::default();
    let message = "loop task `one` has no `root`; the task does not run".to_owned();
    machine
        .notices
        .skipped_loop_tasks
        .insert("one".into(), message.clone());
    let admission = crate::harness::hibernate::admit(&machine.agents.hibernate).unwrap();
    let judgment = judge(&machine, &workspace, MuxName::Tmux, &admission, false, None).unwrap();
    let warnings = judgment
        .skipped
        .iter()
        .filter(|entry| entry.setting == "loop.tasks.one")
        .collect::<Vec<_>>();
    assert_eq!(
        warnings,
        vec![&SkippedSetting {
            setting: "loop.tasks.one".into(),
            message
        }]
    );
}

#[test]
fn account_label_warnings_do_not_refuse_the_room_or_drop_accounts() {
    let root = tempfile::tempdir().unwrap();
    let workspace = crate::WorkspaceResolver::resolve(root.path(), None).unwrap();
    for (label, count) in [("", 1), ("work", 0)] {
        let mut machine = MachineConfig::default();
        machine.accounts.codex.insert(
            "team-1".parse().unwrap(),
            crate::config::NamedAccount {
                label: Some(label.to_owned()),
                home: Some(root.path().join("team-1")),
                ..Default::default()
            },
        );
        let admission = crate::harness::hibernate::admit(&machine.agents.hibernate).unwrap();
        let judgment = judge(&machine, &workspace, MuxName::Tmux, &admission, false, None).unwrap();
        let warnings: Vec<_> = judgment
            .skipped
            .iter()
            .filter(|entry| entry.setting == "accounts.codex.team-1.label")
            .collect();
        assert_eq!(warnings.len(), count, "{warnings:?}");
        if count > 0 {
            let message = &warnings[0].message;
            for text in [
                "accounts.codex.team-1.label",
                "empty",
                "name instead",
                "rimz accounts add codex team-1 --label",
                "--label ''",
            ] {
                assert!(message.contains(text), "{message}");
            }
        }
        assert!(
            crate::agents::LoginCatalog::from_config(&machine.accounts)
                .unwrap()
                .select(
                    &AgentKind::new_unchecked("codex"),
                    &"team-1".parse().unwrap()
                )
                .is_ok()
        );
    }
}

#[test]
fn failed_account_never_prepares_remote_control() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("missing-account");
    let workspace = crate::WorkspaceResolver::resolve(root.path(), None).unwrap();
    let machine = MachineConfig {
        accounts: toml::from_str(&format!(
            "[claude.work]\nhome = '{}'\n[use]\nclaude = 'work'\n",
            home.display()
        ))
        .unwrap(),
        remote_control: toml::from_str("claude = true\n").unwrap(),
        ..Default::default()
    };
    let admission = crate::harness::hibernate::admit(&machine.agents.hibernate).unwrap();
    let judgment = judge(&machine, &workspace, MuxName::Tmux, &admission, false, None).unwrap();
    assert!(
        !home.exists(),
        "judgment must not create a failed account's home"
    );
    let message = |setting: &str| {
        judgment
            .skipped
            .iter()
            .find(|entry| entry.setting == setting)
            .map(|entry| entry.message.as_str())
            .unwrap_or_else(|| panic!("missing {setting}"))
    };
    let account = message("accounts.claude.work");
    let waiver = message("remote_control.claude");
    assert!(waiver.contains("see accounts.claude.work"), "{waiver}");
    assert!(
        waiver.contains("rimz config set remote_control.claude false"),
        "{waiver}"
    );
    assert!(!waiver.contains(account), "{waiver}");
    assert!(
        judgment
            .remote_control_off
            .contains(&AgentKind::new_unchecked("claude"))
    );
}

#[test]
fn judgment_warns_about_out_of_range_tmux_options() {
    let root = tempfile::tempdir().unwrap();
    let workspace = crate::WorkspaceResolver::resolve(root.path(), None).unwrap();
    let machine = MachineConfig {
        tmux: toml::from_str("history_limit = 4294967295\nescape_time_ms = 3000000000\n").unwrap(),
        ..Default::default()
    };
    let admission = crate::harness::hibernate::admit(&machine.agents.hibernate).unwrap();
    let judgment = judge(&machine, &workspace, MuxName::Tmux, &admission, false, None).unwrap();
    for key in ["tmux.history_limit", "tmux.escape_time_ms"] {
        assert!(
            judgment.skipped.iter().any(|entry| entry.setting == key),
            "missing {key}"
        );
    }
}

#[test]
fn judgment_warns_about_an_uninstalled_mux_default() {
    if std::env::var_os("RIMZ_TEST_JUDGMENT_NO_MUX").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "room::judgment::tests::judgment_warns_about_an_uninstalled_mux_default",
                "--nocapture",
            ])
            .env("PATH", "/no-rimz-test-binaries")
            .env("RIMZ_TEST_JUDGMENT_NO_MUX", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let workspace = crate::WorkspaceResolver::resolve(root.path(), None).unwrap();
    let mut machine = MachineConfig::default();
    machine.mux.default = Some(MuxName::Zellij);
    let admission = crate::harness::hibernate::admit(&machine.agents.hibernate).unwrap();
    let judgment = judge(&machine, &workspace, MuxName::Tmux, &admission, false, None).unwrap();
    assert!(judgment.skipped.iter().any(
        |entry| entry.setting == "mux.default" && entry.message.contains("this room uses tmux")
    ));
}

#[test]
fn judgment_warns_for_shared_account_budget_and_subscription_month_cap() {
    if std::env::var_os("RIMZ_TEST_JUDGMENT_BUDGET_HOME").is_none() {
        let home = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "room::judgment::tests::judgment_warns_for_shared_account_budget_and_subscription_month_cap",
                "--nocapture",
            ])
            .env("RIMZ_UNIT_TEST_RIMZ_HOME", home.path())
            .env("RIMZ_TEST_JUDGMENT_BUDGET_HOME", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let workspace = crate::WorkspaceResolver::resolve(root.path(), None).unwrap();
    let machine = MachineConfig {
        accounts: toml::from_str(
            "[budget]\nclaude = '10/month'\n[claude.work]\nbudget = '5/day'\n",
        )
        .unwrap(),
        ..Default::default()
    };
    let state = crate::StatePaths::for_project_root(&workspace.project_root).unwrap();
    let runtime = crate::RuntimePaths::for_state(&state).unwrap();
    let record = crate::agents::account::ProviderRecord {
        consecutive_failures: 0,
        probed_at_ms: 0,
        version_probed_at_ms: None,
        ok: true,
        inputs_stamp: None,
        account: Some(crate::agents::AgentAccount {
            metered: Some(true),
            ..Default::default()
        }),
        login: None,
        probe_error: None,
        profile: None,
    };
    crate::disk::atomic::write_temp_then_rename_cache(
        &runtime.shared_accounts_path(),
        &crate::agents::account::AccountsCache {
            logins: [("claude@default".parse().unwrap(), record)].into(),
        },
    )
    .unwrap();
    let admission = crate::harness::hibernate::admit(&machine.agents.hibernate).unwrap();
    let judgment = judge(&machine, &workspace, MuxName::Tmux, &admission, false, None).unwrap();
    let warnings: BTreeMap<_, _> = judgment
        .skipped
        .iter()
        .map(|entry| (entry.setting.as_str(), entry.message.as_str()))
        .collect();
    assert!(
        warnings
            .get("accounts.claude.work.budget")
            .is_some_and(|text| text.contains("history = \"standalone\"")
                && text.contains("claude@default")
                && text.contains("[accounts.budget].claude")),
        "{warnings:?}"
    );
    assert!(
        warnings
            .get("accounts.budget.claude")
            .is_some_and(|text| text.contains("claude@default")
                && text.contains("subscription")
                && text.contains("API-key billing only")
                && text.contains("will never park")
                && text.contains("extra-usage limit")),
        "{warnings:?}"
    );
}

#[test]
fn judgment_names_a_bare_shell_missing_from_path() {
    let root = tempfile::tempdir().unwrap();
    let workspace = crate::WorkspaceResolver::resolve(root.path(), None).unwrap();
    let mut machine = MachineConfig::default();
    machine.agents.shell = Some("rimz-test-shell-not-installed".into());
    let admission = crate::harness::hibernate::admit(&machine.agents.hibernate).unwrap();
    let judgment = judge(&machine, &workspace, MuxName::Tmux, &admission, false, None).unwrap();
    let warning = judgment
        .skipped
        .iter()
        .find(|entry| entry.setting == "agents.shell")
        .unwrap();
    assert!(
        warning.message.contains("is not on PATH"),
        "{}",
        warning.message
    );
    assert!(
        warning
            .message
            .contains("rimz config set agents.shell bash"),
        "{}",
        warning.message
    );
}

#[test]
fn judgment_names_independent_shell_budget_and_schedule_problems() {
    let root = tempfile::tempdir().unwrap();
    let workspace = crate::WorkspaceResolver::resolve(root.path(), None).unwrap();
    let mut machine = MachineConfig::default();
    machine.agents.shell = Some(root.path().join("missing/bash"));
    machine.accounts = toml::from_str("[budget]\ncopilot = '10/day'\n").unwrap();
    machine.agents.hibernate.after = "bad".to_owned();
    let admission = crate::harness::hibernate::admit(&machine.agents.hibernate).unwrap();
    let judgment = judge(&machine, &workspace, MuxName::Tmux, &admission, false, None).unwrap();
    let settings: BTreeSet<_> = judgment
        .skipped
        .iter()
        .map(|entry| entry.setting.as_str())
        .collect();
    for setting in [
        "agents.shell",
        "accounts.budget.copilot",
        "agents.hibernate",
    ] {
        assert!(
            settings.contains(setting),
            "missing {setting}: {settings:?}"
        );
    }
}
use std::path::PathBuf;
#[test]
fn hibernate_notice_only_for_explicit_unavailable_capability() {
    let mut config = crate::config::HibernateConfig::default();
    let mut admission = crate::harness::hibernate::Admission {
        available: false,
        reason: Some("no swap".into()),
    };
    assert_eq!(hibernate_notice(&config, &admission), None);
    config.enabled = Some(false);
    assert_eq!(hibernate_notice(&config, &admission), None);
    config.enabled = Some(true);
    assert_eq!(
        hibernate_notice(&config, &admission).as_deref(),
        Some("agents.hibernate.enabled is set but hibernation is skipped: no swap")
    );
    admission.available = true;
    admission.reason = None;
    assert_eq!(hibernate_notice(&config, &admission), None);
}

#[test]
fn broken_config_notice_is_one_line_and_names_the_fallback() {
    let err = crate::config::MachineConfig::parse_text(
        std::path::Path::new("/tmp/theme.toml"),
        "[theme.display]\nmax_cols = 64\nmax_cols = 72\n",
        std::path::Path::new("/tmp/missing-agents-home"),
    )
    .expect_err("duplicate key fails");

    let notice = broken_config_notice(&err);

    assert_eq!(
        notice.lines().count(),
        1,
        "notice stays on one line: {notice}"
    );
    assert!(
        notice.contains("/tmp/theme.toml is unparseable"),
        "{notice}"
    );
    assert!(notice.contains("line 3"), "{notice}");
    assert!(
        notice.contains("`max_cols` is defined more than once"),
        "{notice}"
    );
    assert!(notice.contains("built-in defaults apply"), "{notice}");
}

#[test]
fn theme_notice_preserves_the_effective_palette_without_dropping_other_settings() {
    let message = "unknown sidebar theme scheme `missing`; the sidebar keeps the inline `[colors]` palette until it is fixed";
    let err = crate::config::ConfigErr::Theme {
        path: "/tmp/theme.toml".into(),
        message: message.to_owned(),
    };
    assert_eq!(
        broken_config_notice(&err),
        format!("/tmp/theme.toml: {message}")
    );
}

#[test]
fn definition_summary_groups_causes_and_deduplicates_team_roles() {
    let home = PathBuf::from("/home/tester");
    let agents_home = home.join(".rimz");
    let missing = DefinitionCause::MissingSkill {
        skill: "rimz-lsp".parse().unwrap(),
        roots: vec![home.join(".claude/skills"), agents_home.join("skills")],
    };
    let failed = DefinitionCause::DependsOnFailed {
        name: "finder".to_owned(),
    };
    let names = [
        "agents/coder",
        "agents/planner",
        "subagents/finder",
        "subagents/newcomer",
        "subagents/surveyor",
    ];
    let mut errors: Vec<_> = names
        .iter()
        .map(|name| DefinitionErr {
            path: agents_home.join(format!("{name}.md")),
            message: "detail".to_owned(),
            cause: missing.clone(),
        })
        .collect();
    for name in ["agents/astra", "agents/reviewer", "teams/x", "teams/x"] {
        errors.push(DefinitionErr {
            path: agents_home.join(format!("{name}.md")),
            message: "detail".to_owned(),
            cause: failed.clone(),
        });
    }
    let lines = definition_summary(
        &errors.iter().collect::<Vec<_>>(),
        &agents_home,
        Some("/home/tester"),
    );
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(lines[0].starts_with("5 definitions"));
    assert!(lines[0].contains("'rimz-lsp'"));
    assert!(lines[0].contains("~/.claude/skills or ~/.rimz/skills"));
    for name in names {
        assert!(lines[0].contains(name));
    }
    assert!(lines[1].starts_with("3 definitions"));
    assert_eq!(lines[1].matches("teams/x").count(), 1);
    assert!(lines[2].contains("rimz agents validate"));
    assert!(
        lines
            .iter()
            .all(|line| line.len() <= 210 && !line.contains(&home.display().to_string())),
        "{lines:?}"
    );
}

#[test]
fn definition_summary_caps_names_and_handles_other_errors() {
    let home = PathBuf::from("/definitions");
    let mut errors: Vec<_> = (0..9)
        .rev()
        .map(|n| DefinitionErr {
            path: home.join(format!("agents/a{n}.md")),
            message: "detail".to_owned(),
            cause: DefinitionCause::MissingSkill {
                skill: "one".parse().unwrap(),
                roots: vec![home.join("skills")],
            },
        })
        .collect();
    errors.push(DefinitionErr {
        path: PathBuf::from("/outside/bad.md"),
        message: "detail".to_owned(),
        cause: DefinitionCause::Invalid,
    });
    let lines = definition_summary(&errors.iter().collect::<Vec<_>>(), &home, None);
    assert_eq!(lines.len(), 3);
    assert!(lines[0].starts_with("9 definitions"));
    assert!(
        lines[0]
            .ends_with("agents/a0, agents/a1, agents/a2, agents/a3, agents/a4, agents/a5, +3 more")
    );
    assert!(lines[1].starts_with("1 definition"));
    assert!(lines[1].ends_with("/outside/bad.md"));
    assert!(definition_summary(&[], &home, None).is_empty());
}

#[test]
fn definition_summary_unions_roots_per_skill() {
    let home = PathBuf::from("/definitions");
    let errors: Vec<_> = [("one", "/a"), ("one", "/b"), ("two", "/a")]
        .into_iter()
        .map(|(skill, root)| DefinitionErr {
            path: home.join("agents/same.md"),
            message: "detail".to_owned(),
            cause: DefinitionCause::MissingSkill {
                skill: skill.parse().unwrap(),
                roots: vec![PathBuf::from(root)],
            },
        })
        .collect();
    let lines = definition_summary(&errors.iter().collect::<Vec<_>>(), &home, None);
    assert_eq!(lines.len(), 3);
    assert!(lines[0].starts_with("1 definition"));
    assert!(lines[0].contains("'one'") && lines[0].contains("/a or /b"));
    assert!(lines[1].contains("'two'"));
}
