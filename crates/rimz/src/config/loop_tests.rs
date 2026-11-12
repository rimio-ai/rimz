use super::*;

#[test]
fn task_notes_toggle_round_trips_without_changing_the_default() {
    let actual: Vec<_> = ["", "notes = false", "notes = true"]
        .into_iter()
        .map(|text| {
            let task: TaskEntry = toml::from_str(text).unwrap();
            serde_json::to_value(task).unwrap()["notes"].clone()
        })
        .collect();
    assert_eq!(actual, [serde_json::Value::Null, false.into(), true.into()]);
}

#[test]
fn task_capacity_wait_accepts_positive_durations_or_forever() {
    let actual: Vec<_> = ["1s", "2m", "3h", "4d", "forever", "0s", "1ms", "2", "never"]
        .into_iter()
        .map(|raw| {
            toml::from_str::<TaskEntry>(&format!("capacity-wait = '{raw}'"))
                .ok()
                .and_then(|entry| serde_json::to_value(entry).ok())
                .and_then(|value| value["capacity-wait"].as_str().map(str::to_owned))
        })
        .collect();
    assert_eq!(
        actual,
        [
            Some("1s".into()),
            Some("2m".into()),
            Some("3h".into()),
            Some("4d".into()),
            Some("forever".into()),
            None,
            None,
            None,
            None
        ]
    );
}

#[test]
fn narrow_loop_matches_full_lenient_config() {
    for contents in [
        Some("default-timeout = '5m'\n[tasks.nightly]\ncheck = 'cargo test'\nevery = '1h'\n"),
        Some("[tasks.nightly\n"),
        Some("[tasks.nightly]\ncheck = 'cargo test'\nbudget = 'bad'\n"),
        None,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let core_path = dir.path().join("config.toml");
        let loop_path = dir.path().join("loop.toml");
        if let Some(contents) = contents {
            std::fs::write(&loop_path, contents).unwrap();
        }
        let full = crate::config::MachineConfig::load_lenient_from(
            &core_path,
            &dir.path().join("agents"),
            &BTreeMap::new(),
        );
        assert_eq!(
            LoopConfig::load_lenient_from(&loop_path),
            full.r#loop,
            "loop contents: {contents:?}"
        );
    }
}

#[test]
fn a_judge_table_under_check_refuses_with_the_loop_fix() {
    let refused = |error: String| {
        assert!(
            error.contains("judge table under `check`")
                && error.contains("older RimZ")
                && error.contains("re-add it with `rimz loop add`"),
            "{error}"
        );
    };
    let row = "check = { agent = 'haiku', prompt = 'q' }";
    refused(toml::from_str::<TaskEntry>(row).map_or_else(|e| e.to_string(), |_| String::new()));
    let json: serde_json::Value = toml::from_str(row).unwrap();
    refused(
        serde_json::from_value::<TaskEntry>(json).map_or_else(|e| e.to_string(), |_| String::new()),
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("loop.toml");
    let shell = "[tasks.ok]\nroot = '/repo'\ncheck = 'true'\nevery = '1h'\n";
    let text = format!(
        "{shell}[tasks.old]\nroot = '/repo'\nevery = '1h'\n[tasks.old.check]\nagent = 'haiku'\nprompt = 'q'\n"
    );
    let error = super::super::parse_loop_text(&path, &text).unwrap_err();
    assert!(error.to_string().contains("loop.toml"), "{error}");
    refused(std::error::Error::source(&error).unwrap().to_string());
    std::fs::write(&path, &text).unwrap();
    assert!(LoopConfig::load_lenient_from(&path).tasks.0.is_empty());
    let loaded = super::super::parse_loop_text(&path, shell).unwrap();
    assert_eq!(
        loaded.tasks.0["ok"].check,
        Some(TaskCheck::from("true")),
        "a string check beside no table loads as before"
    );
}

#[test]
fn shell_watch_previews_preserve_trailing_redirects() {
    let short = "cargo test > /tmp/gate.log 2>&1";
    assert_eq!(WatchSpec::Command(short.into()).subject(), short);
    let path = format!("/long/{}/gate.log", "directory/".repeat(7));
    for operator in [">", ">>", "2>", "&>"] {
        let clause = format!("{operator} {path} 2>&1");
        let command = format!("cargo xtask test {} {clause}", "argument ".repeat(7));
        for spec in [
            WatchSpec::Command(command.clone()),
            WatchSpec::Check {
                check: command.clone(),
                every: "1s".into(),
                on: CheckOn::Success,
            },
        ] {
            let preview = spec.subject();
            assert!(preview.starts_with("cargo xtask test "), "{preview}");
            assert!(preview.ends_with(&clause), "{preview}");
            assert!(preview.contains('…'), "{preview}");
            assert!(preview.ends_with(&format!("… {clause}")), "{preview}");
            assert!(spec.headline().contains(&clause), "{}", spec.headline());
        }
    }
    for command in [
        format!("echo '{}'", "x".repeat(150)),
        format!("echo {} '>' {path}", "x".repeat(90)),
        format!("echo {} \">\" {path}", "x".repeat(90)),
        format!("echo {} \\> {path}", "x".repeat(90)),
        format!("echo {} > file && echo done", "x".repeat(150)),
    ] {
        assert_eq!(
            WatchSpec::Command(command.clone()).subject(),
            crate::theme::fmt::command_preview(&command)
        );
    }
}

#[test]
fn guard_reads_shell_then_judge_table_or_array_in_order() {
    for source in [
        "check = 'cargo test'\ncheck-timeout = '10m'\njudge = { agent = 'haiku', prompt = 'q' }",
        "check = 'cargo test'\ncheck-timeout = '10m'\njudge = [{ agent = 'haiku', prompt = 'q' }, { agent = 'sonnet', prompt = 'review' }]",
    ] {
        let entry: TaskEntry = toml::from_str(source).unwrap();
        let guard = entry.guard();
        assert!(!guard.judges.is_empty(), "{source}");
        assert_eq!(guard.judges[0].agent, "haiku", "{source}");
        assert_eq!(guard.shell, Some("cargo test"));
        assert_eq!(guard.shell_timeout, Some("10m"));
        if source.contains("sonnet") {
            assert_eq!(guard.judges[1].agent, "sonnet");
        }
        assert_eq!(
            serde_json::from_str::<TaskEntry>(&serde_json::to_string(&entry).unwrap()).unwrap(),
            entry
        );
        assert_eq!(
            toml::from_str::<TaskEntry>(&toml::to_string(&entry).unwrap()).unwrap(),
            entry
        );
    }
}

#[test]
fn shell_check_timeout_rejects_zero_and_invalid_durations() {
    for value in ["0", "0s", "bad"] {
        let source = format!("check = 'true'\ncheck-timeout = '{value}'");
        assert!(toml::from_str::<TaskEntry>(&source).is_err(), "{source}");
    }
}

#[test]
fn judges_round_trip_toml_and_json() {
    for prompt in ["prompt = 'Is work needed?'", "prompt-file = 'check.md'"] {
        let source =
            format!("[judge]\nagent = 'haiku'\n{prompt}\nrecheck = '6h'\ntimeout = '5m'\n");
        let decoded = toml::from_str::<TaskEntry>(&source);
        assert!(decoded.is_ok(), "judge table must be accepted: {decoded:?}");
        let entry = decoded.unwrap();
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["judge"][0]["agent"], "haiku");
        assert_eq!(json["judge"][0]["recheck"], "6h");
        assert_eq!(json["judge"][0]["timeout"], "5m");
        assert_eq!(serde_json::from_value::<TaskEntry>(json).unwrap(), entry);
        assert_eq!(
            toml::from_str::<TaskEntry>(&toml::to_string(&entry).unwrap()).unwrap(),
            entry
        );
    }
}

#[test]
fn judges_require_exactly_one_prompt() {
    for fields in ["", "prompt = 'question'\nprompt-file = 'check.md'"] {
        let source = format!("[judge]\nagent = 'haiku'\n{fields}\n");
        assert!(toml::from_str::<TaskEntry>(&source).is_err(), "{source}");
        let mut json = serde_json::json!({"agent": "haiku"});
        if !fields.is_empty() {
            json["prompt"] = "question".into();
            json["prompt-file"] = "check.md".into();
        }
        assert!(
            serde_json::from_value::<TaskEntry>(serde_json::json!({"judge": json, "root": ""}))
                .is_err()
        );
    }
}

#[test]
fn judges_reject_unknown_fields() {
    let source = "[judge]\nagent = 'haiku'\nprompt = 'Is work needed?'\nrechek = '6h'\n";
    let error = toml::from_str::<TaskEntry>(source).unwrap_err();
    assert!(
        error.to_string().contains("unknown field `rechek`"),
        "{error}"
    );
    assert!(
        serde_json::from_value::<TaskEntry>(serde_json::json!({
            "root": "",
            "judge": {"agent": "haiku", "prompt": "Is work needed?", "rechek": "6h"}
        }))
        .is_err(),
        "JSON judge tables must also reject unknown keys"
    );
}

#[test]
fn malformed_judges_preserve_the_cause() {
    for (source, cause) in [
        (
            "judge = { agent = 'haiku' }",
            "exactly one of prompt and prompt-file",
        ),
        (
            "judge = { agent = 'haiku', prompt = 'q', rechek = '6h' }",
            "unknown field `rechek`",
        ),
    ] {
        let error = toml::from_str::<TaskEntry>(source).unwrap_err();
        assert!(error.to_string().contains(cause), "{error}");
        let json: serde_json::Value = toml::from_str(source).unwrap();
        let error = serde_json::from_value::<TaskEntry>(json).unwrap_err();
        assert!(error.to_string().contains(cause), "{error}");
    }
}

#[test]
fn watch_descriptions_preserve_prefixes_and_expose_subjects() {
    let cases = [
        (
            WatchSpec::Command("echo hello".into()),
            "watch",
            "echo hello",
            "watch: echo hello",
        ),
        (WatchSpec::Pid { pid: 123 }, "pid", "123", "pid 123"),
        (
            WatchSpec::Check {
                check: "false".into(),
                every: "1s".into(),
                on: CheckOn::Success,
            },
            "check",
            "false",
            "check: false",
        ),
        (
            WatchSpec::Check {
                check: "false".into(),
                every: "2s".into(),
                on: CheckOn::Fail,
            },
            "check",
            "false every 2s on fail",
            "check: false every 2s on fail",
        ),
        (
            WatchSpec::File {
                file: "app.log".into(),
                grep: None,
                mark: None,
            },
            "file",
            "app.log",
            "file: app.log",
        ),
        (
            WatchSpec::File {
                file: "app.log".into(),
                grep: Some("ready".into()),
                mark: None,
            },
            "file",
            "app.log grep: ready",
            "file: app.log grep: ready",
        ),
    ];
    for (spec, kind, subject, description) in cases {
        assert_eq!(spec.describe(), description);
        assert_eq!(spec.kind(), kind);
        assert_eq!(spec.subject(), subject);
    }
}

#[test]
fn resolve_root_expands_tilde_prefix() {
    let home = PathBuf::from("/home/dev");
    assert_eq!(
        resolve_root_with(Path::new("~/workspace/app"), home.clone()),
        home.join("workspace/app")
    );
    assert_eq!(resolve_root_with(Path::new("~"), home.clone()), home);
    assert_eq!(
        resolve_root_with(Path::new("~other/app"), PathBuf::from("/home/dev")),
        PathBuf::from("~other/app")
    );
}

#[test]
fn resolve_root_canonicalizes_existing_absolute_paths() {
    let dir = tempfile::tempdir().expect("tempdir");
    let nested = dir.path().join("nested");
    std::fs::create_dir(&nested).expect("mkdir nested");
    let dotted = nested.join(".");

    assert_eq!(
        resolve_root_with(&dotted, PathBuf::from("/home/dev")),
        nested.canonicalize().expect("canonical nested")
    );
}

#[test]
fn file_mark_tracks_absence_size_mtime_and_identity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.log");
    assert_eq!(FileMark::read(&path).unwrap(), None);
    std::fs::write(&path, "one").unwrap();
    let first = FileMark::read(&path).unwrap().unwrap();
    assert_eq!(first.size, 3);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(std::time::SystemTime::UNIX_EPOCH)
        .unwrap();
    let rewritten = FileMark::read(&path).unwrap().unwrap();
    assert_eq!(rewritten.size, first.size);
    assert_ne!(rewritten, first);

    let replacement = dir.path().join("app.log.new");
    std::fs::write(&replacement, "one").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&replacement)
        .unwrap()
        .set_modified(std::time::SystemTime::UNIX_EPOCH)
        .unwrap();
    std::fs::rename(&replacement, &path).unwrap();
    let replaced = FileMark::read(&path).unwrap().unwrap();
    assert_eq!(
        (replaced.size, replaced.modified),
        (rewritten.size, rewritten.modified)
    );
    assert_ne!(replaced, rewritten);
}

#[test]
fn task_run_dir_defaults_to_root_and_resolves_explicit_directory() {
    let mut entry = TaskEntry {
        root: PathBuf::from("~/repo"),
        ..TaskEntry::default()
    };
    assert_eq!(entry.run_dir(), entry.resolved_root());
    assert!(serde_json::to_value(&entry).unwrap().get("dir").is_none());
    assert!(toml::Value::try_from(&entry).unwrap().get("dir").is_none());

    entry.dir = Some(PathBuf::from("~/linked"));
    assert_eq!(
        entry.run_dir(),
        resolve_root_with(Path::new("~/linked"), home_dir())
    );
}

#[test]
fn task_entry_check_fields_round_trip_toml_and_json() {
    let labelled: TaskEntry = toml::from_str("label = \"gate docs\"").unwrap();
    let encoded = toml::to_string(&labelled).unwrap();
    let decoded: toml::Value = toml::from_str(&encoded).unwrap();
    assert_eq!(
        decoded.get("label").and_then(toml::Value::as_str),
        Some("gate docs")
    );
    let json = serde_json::to_value(&labelled).unwrap();
    assert_eq!(json["label"], "gate docs");
    assert_eq!(serde_json::from_value::<TaskEntry>(json).unwrap(), labelled);
    assert!(
        serde_json::to_value(TaskEntry::default())
            .unwrap()
            .get("label")
            .is_none()
    );
    let deadline = Timestamp::from_second(1_783_000_000).expect("deadline");
    let entry = TaskEntry {
        wait: Some(TaskTarget {
            kind: AgentKind::new_unchecked("claude"),
            session: "sess-1".into(),
            handle: "@claude".to_owned(),
        }),
        wait_meta: Some(WaitMeta {
            armed_at: deadline,
            delay: Some("30m".to_owned()),
            reader: Some("coder".to_owned()),
        }),
        watch: Some(WatchSpec::Pid { pid: 16776 }),
        prompt: Some("wait".to_owned()),
        check: Some("cargo test".into()),
        verify: Some("cargo xtask gate".to_owned()),
        max_attempts: Some(4),
        max_strikes: Some(5),
        on: Some(CheckOn::Success),
        root: PathBuf::from("/repo"),
        dir: Some(PathBuf::from("/linked")),
        fresh_worktree: Some("nightly".to_owned()),
        every: Some("weekday".to_owned()),
        at: Some("07:00".to_owned()),
        budget: Some("$5.00".to_owned()),
        budget_per_day: Some("$20.00".to_owned()),
        surplus: Some("1.5x".to_owned()),
        surplus_after: Some("3d".to_owned()),
        signal: Some("ci.failed".to_owned()),
        matches: Some(BTreeMap::from([(
            "branch".to_owned(),
            "feature".to_owned(),
        )])),
        once: Some(true),
        deadline: Some(deadline),
        account: Some("work".parse().expect("login name")),
        ..TaskEntry::default()
    };
    let tasks = Tasks(BTreeMap::from([("ci".to_owned(), entry.clone())]));
    let loop_config = LoopConfig {
        tasks,
        ..LoopConfig::default()
    };

    let toml = toml::to_string(&loop_config).expect("toml");
    assert!(toml.contains("fresh-worktree = \"nightly\""), "{toml}");
    assert_eq!(
        serde_json::to_value(&entry).unwrap()["fresh-worktree"],
        "nightly"
    );
    let toml_round: LoopConfig = toml::from_str(&toml).expect("toml round trip");
    assert_eq!(toml_round.tasks.0["ci"], entry);
    assert_eq!(toml_round.tasks.0["ci"].wait_meta, entry.wait_meta);
    assert_eq!(
        toml_round
            .tasks
            .0
            .get("ci")
            .and_then(|entry| entry.check.as_ref()),
        Some(&TaskCheck("cargo test".into()))
    );
    assert_eq!(
        toml_round.tasks.0.get("ci").and_then(|entry| entry.on),
        Some(CheckOn::Success)
    );
    assert_eq!(
        toml_round
            .tasks
            .0
            .get("ci")
            .and_then(|entry| entry.verify.as_deref()),
        Some("cargo xtask gate")
    );
    assert_eq!(
        toml_round
            .tasks
            .0
            .get("ci")
            .and_then(|entry| entry.max_attempts),
        Some(4)
    );
    assert_eq!(
        toml_round
            .tasks
            .0
            .get("ci")
            .and_then(|entry| entry.deadline),
        Some(deadline)
    );
    assert_eq!(
        toml_round
            .tasks
            .0
            .get("ci")
            .and_then(|entry| entry.every.as_deref()),
        Some("weekday")
    );
    assert!(
        toml.contains("every = \"weekday\""),
        "weekday cadence should round-trip through TOML: {toml}"
    );
    assert!(toml.contains("budget = \"$5.00\""), "{toml}");
    assert!(toml.contains("budget-per-day = \"$20.00\""), "{toml}");
    assert!(toml.contains("surplus = \"1.5x\""), "{toml}");
    assert!(toml.contains("surplus-after = \"3d\""), "{toml}");
    assert!(toml.contains("max-attempts = 4"), "{toml}");
    assert!(toml.contains("signal = \"ci.failed\""), "{toml}");
    assert!(toml.contains("[tasks.ci.match]"), "{toml}");
    assert!(toml.contains("branch = \"feature\""), "{toml}");
    assert!(toml.contains("once = true"), "{toml}");
    assert!(toml.contains("account = \"work\""), "{toml}");
    assert!(
        !toml::to_string(&TaskEntry::default())
            .expect("unpinned toml")
            .contains("account")
    );

    let json = serde_json::to_string(&loop_config.tasks).expect("json");
    let json_round: Tasks = serde_json::from_str(&json).expect("json round trip");
    assert_eq!(json_round.0.get("ci"), Some(&entry));
    let mut value = serde_json::to_value(&loop_config.tasks).expect("json value");
    assert_eq!(
        value["ci"]["wait"],
        serde_json::json!({"kind": "claude", "session": "sess-1", "handle": "@claude"})
    );
    assert!(value["ci"]["wait-meta"].get("armed_by").is_none());
    assert_eq!(value["ci"]["watch"], serde_json::json!({"pid": 16776}));
    for spec in [
        WatchSpec::Check {
            check: "nc -z localhost 3000".to_owned(),
            every: "30s".to_owned(),
            on: CheckOn::Fail,
        },
        WatchSpec::File {
            file: PathBuf::from("/repo/app.log"),
            grep: Some("ready".to_owned()),
            mark: Some(FileMark {
                size: 12,
                modified: deadline,
                dev: 2049,
                ino: 131_074,
            }),
        },
        WatchSpec::File {
            file: PathBuf::from("/repo/app.log"),
            grep: None,
            mark: None,
        },
    ] {
        let json = serde_json::to_string(&spec).unwrap();
        assert_eq!(serde_json::from_str::<WatchSpec>(&json).unwrap(), spec);
        let toml = toml::to_string(&TaskEntry {
            watch: Some(spec.clone()),
            ..TaskEntry::default()
        })
        .unwrap();
        assert_eq!(
            toml::from_str::<TaskEntry>(&toml).unwrap().watch,
            Some(spec),
            "{toml}"
        );
    }
    value["ci"]["watch"] = serde_json::json!("true");
    let decoded: Tasks = serde_json::from_value(value).expect("string watch json");
    assert_eq!(
        decoded.0["ci"].watch,
        Some(WatchSpec::Command("true".to_owned()))
    );
}

#[test]
fn default_timeout_accepts_task_duration_units_and_rejects_invalid_values() {
    let config: LoopConfig =
        toml::from_str("default-timeout = \"2h\"\n").expect("valid default timeout");
    assert_eq!(config.default_timeout.as_deref(), Some("2h"));

    let err = toml::from_str::<LoopConfig>("default-timeout = \"forever\"\n")
        .expect_err("invalid default timeout");
    assert!(err.to_string().contains("duration"), "{err}");
}

#[test]
fn default_timeout_rejects_a_zero_duration() {
    for text in [
        "default-timeout = \"0s\"\n",
        "default-timeout = \"0m\"\n",
        "default-timeout = \"0d\"\n",
    ] {
        let err = toml::from_str::<LoopConfig>(text).expect_err("zero default timeout");
        assert!(
            err.to_string().contains("must be greater than zero"),
            "{err}"
        );
    }
}

#[test]
fn task_throttle_switch_round_trips_and_is_absent_by_default() {
    let tasks = toml::from_str::<Tasks>(
        "[exempt]\nroot = \"/repo\"\nthrottle = \"off\"\n[plain]\nroot = \"/repo\"\n",
    )
    .expect("parse");
    assert_eq!(tasks.0["exempt"].throttle, Some(ThrottleSwitch::Off));
    assert_eq!(tasks.0["plain"].throttle, None);
    let text = toml::to_string(&tasks).expect("serialize");
    assert!(text.contains("throttle = \"off\""), "{text}");
    assert_eq!(text.matches("throttle").count(), 1, "{text}");
    toml::from_str::<Tasks>("[bad]\nroot = \"/repo\"\nthrottle = \"maybe\"\n")
        .expect_err("only on and off");
}

#[test]
fn unknown_task_keys_are_ignored() {
    let tasks =
        toml::from_str::<Tasks>("[old]\nspec = \"claude\"\nroot = \"/repo\"\n").expect("parse");
    assert_eq!(tasks.0["old"].root, PathBuf::from("/repo"));
    assert!(tasks.0["old"].agent.is_none());
}

#[test]
fn task_budgets_validate_as_one_config_unit() {
    let invalid = TaskEntry {
        budget_per_day: Some("$20.00".to_owned()),
        ..TaskEntry::default()
    };
    assert!(matches!(
        invalid.validate_budget("nightly"),
        Err(TaskBudgetError::MissingRunBudget { task }) if task == "nightly"
    ));

    let malformed = TaskEntry {
        budget: Some("many dollars".to_owned()),
        ..TaskEntry::default()
    };
    assert!(matches!(
        malformed.validate_budget("nightly"),
        Err(TaskBudgetError::Invalid {
            field: "budget",
            ..
        })
    ));

    for (field, entry) in [
        (
            "surplus",
            TaskEntry {
                surplus: Some("many".to_owned()),
                ..TaskEntry::default()
            },
        ),
        (
            "surplus-after",
            TaskEntry {
                surplus_after: Some("soon".to_owned()),
                ..TaskEntry::default()
            },
        ),
    ] {
        assert!(matches!(
            entry.validate_budget("nightly"),
            Err(TaskBudgetError::InvalidSurplus { field: actual, .. }) if actual == field
        ));
    }

    assert!(matches!(
        TaskEntry {
            surplus: Some("1.5x".to_owned()),
            ..TaskEntry::default()
        }
        .validate_budget("nightly"),
        Err(TaskBudgetError::SurplusNeedsAgent { task }) if task == "nightly"
    ));
    assert!(
        TaskEntry {
            surplus: Some("1.5x".to_owned()),
            budget_per_day: Some("$5".to_owned()),
            workflow: Some("review".to_owned()),
            ..TaskEntry::default()
        }
        .validate_budget("nightly")
        .is_ok(),
        "a workflow task's daily cap and surplus gate its runs"
    );
}
