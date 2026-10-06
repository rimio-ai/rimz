use super::*;

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
        check: Some("cargo test".to_owned()),
        verify: Some("cargo xtask gate".to_owned()),
        max_attempts: Some(4),
        max_strikes: Some(5),
        on: Some(CheckOn::Success),
        root: PathBuf::from("/repo"),
        dir: Some(PathBuf::from("/linked")),
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
    let toml_round: LoopConfig = toml::from_str(&toml).expect("toml round trip");
    assert_eq!(toml_round.tasks.0["ci"], entry);
    let mut legacy_toml: toml::Value = toml::from_str(&toml).expect("toml value");
    legacy_toml["tasks"]["ci"]["wait-meta"]
        .as_table_mut()
        .unwrap()
        .insert("pid".to_owned(), toml::Value::Integer(16776));
    legacy_toml["tasks"]["ci"]["watch"] =
        toml::Value::String("while kill -0 16776 2>/dev/null; do sleep 1; done".to_owned());
    let legacy_toml: LoopConfig = legacy_toml.try_into().expect("legacy toml");
    assert_eq!(
        legacy_toml.tasks.0["ci"].watch,
        Some(WatchSpec::Command(
            "while kill -0 16776 2>/dev/null; do sleep 1; done".to_owned()
        ))
    );
    assert_eq!(legacy_toml.tasks.0["ci"].wait_meta, entry.wait_meta);
    assert_eq!(toml_round.tasks.0["ci"].wait_meta, entry.wait_meta);
    assert_eq!(
        toml_round
            .tasks
            .0
            .get("ci")
            .and_then(|entry| entry.check.as_deref()),
        Some("cargo test")
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
    let mut legacy = serde_json::to_value(&loop_config.tasks).expect("json value");
    assert_eq!(
        legacy["ci"]["wait"],
        serde_json::json!({"kind": "claude", "session": "sess-1", "handle": "@claude"})
    );
    assert!(legacy["ci"]["wait-meta"].get("armed_by").is_none());
    for armed_by in [
        serde_json::json!({"kind": "human"}),
        serde_json::json!({"kind": "agent", "handle": "@planner"}),
    ] {
        legacy["ci"]["wait-meta"]["armed_by"] = armed_by;
        let decoded: Tasks = serde_json::from_value(legacy.clone()).expect("legacy json");
        assert_eq!(decoded.0.get("ci"), Some(&entry));
    }
    assert_eq!(legacy["ci"]["watch"], serde_json::json!({"pid": 16776}));
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
    legacy["ci"]["wait-meta"]["pid"] = serde_json::json!(16776);
    legacy["ci"]["watch"] = serde_json::json!("true");
    let decoded: Tasks = serde_json::from_value(legacy).expect("legacy json with pid");
    assert_eq!(
        decoded.0["ci"].watch,
        Some(WatchSpec::Command("true".to_owned()))
    );
    assert!(
        serde_json::to_value(&decoded).unwrap()["ci"]["wait-meta"]
            .get("pid")
            .is_none()
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
fn throttle_table_parses_with_defaults_and_typed_values() {
    let defaults = LoopConfig::default().throttle;
    assert!(defaults.is_empty());
    assert_eq!(defaults.pace(), Duration::from_secs(10));
    assert_eq!(defaults.max_wait(), Duration::from_secs(30 * 60));
    assert_eq!(defaults.min_memory_bytes(), None);

    let config: LoopConfig = toml::from_str(
        "[throttle]\npace = \"0s\"\nmax-wait = \"2h\"\nmax-active = 12\n\
         max-active-per-task = 4\ncpu-pressure = 60\nio-pressure = 40\n\
         memory-pressure = 100\nmin-memory = \"8GB\"\nmin-disk = \"20GB\"\n",
    )
    .expect("valid throttle table");
    let throttle = &config.throttle;
    assert!(!config.is_empty(), "a throttle table alone is content");
    assert_eq!(throttle.pace(), Duration::ZERO);
    assert_eq!(throttle.max_wait(), Duration::from_secs(2 * 60 * 60));
    assert_eq!(throttle.max_active, Some(12));
    assert_eq!(throttle.max_active_per_task, Some(4));
    assert_eq!(
        (
            throttle.cpu_pressure,
            throttle.io_pressure,
            throttle.memory_pressure
        ),
        (Some(60), Some(40), Some(100))
    );
    assert_eq!(throttle.min_memory_bytes(), Some(8_000_000_000));
    assert_eq!(throttle.min_disk_bytes(), Some(20_000_000_000));
    let round_trip: LoopConfig =
        toml::from_str(&toml::to_string(&config).expect("serialize")).expect("reparse");
    assert_eq!(round_trip, config);
}

#[test]
fn throttle_table_rejects_values_that_could_never_admit() {
    for (text, expected) in [
        ("max-wait = \"0s\"", "must be greater than zero"),
        ("max-wait = \"1d\"", "unit"),
        ("pace = \"soon\"", "duration"),
        ("max-active = 0", "must be greater than zero"),
        ("max-active-per-task = 0", "must be greater than zero"),
        ("cpu-pressure = 0", "between 1 and 100"),
        ("io-pressure = 101", "between 1 and 100"),
        ("min-memory = \"lots\"", "size"),
        ("min-disk = \"0\"", "must be greater than zero"),
    ] {
        let err = toml::from_str::<LoopConfig>(&format!("[throttle]\n{text}\n")).expect_err(text);
        assert!(err.to_string().contains(expected), "{text}: {err}");
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
}
