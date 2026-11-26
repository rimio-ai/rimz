use super::*;

#[test]
fn narrow_loop_matches_full_lenient_config() {
    for contents in [
        Some(
            "default-timeout = '5m'\n[tasks.nightly]\nargs = ['cargo', 'test']\ntimeout = '1h'\nevery = '1h'\n",
        ),
        Some("[tasks.nightly\n"),
        Some("[tasks.nightly]\nargs = ['cargo', 'test']\nagent = 'codex'\n"),
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
fn shell_watch_previews_preserve_trailing_redirects() {
    let short = "cargo test > /tmp/gate.log 2>&1";
    assert_eq!(WatchSpec::Command(short.into()).subject(), short);
    let path = format!("/long/{}/gate.log", "directory/".repeat(7));
    for operator in [">", ">>", "2>", "&>"] {
        let clause = format!("{operator} {path} 2>&1");
        let command = format!("cargo xtask test {} {clause}", "argument ".repeat(7));
        let spec = WatchSpec::Command(command.clone());
        let preview = spec.subject();
        assert!(preview.starts_with("cargo xtask test "), "{preview}");
        assert!(preview.ends_with(&clause), "{preview}");
        assert!(preview.contains('…'), "{preview}");
        assert!(preview.ends_with(&format!("… {clause}")), "{preview}");
        assert!(spec.headline().contains(&clause), "{}", spec.headline());
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
fn a_shell_spec_describes_itself_as_a_shell() {
    let spec = WatchSpec::Command("echo hello".into());
    assert_eq!(spec.describe(), "shell: echo hello");
    assert_eq!(spec.kind(), "shell");
    assert_eq!(spec.subject(), "echo hello");
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
fn task_entry_row_fields_round_trip_toml_and_json() {
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
            joined_until: None,
        }),
        watch: Some(WatchSpec::Command("cargo test".to_owned())),
        prompt: Some("wait".to_owned()),
        max_strikes: Some(5),
        on: Some(CheckOn::Success),
        root: PathBuf::from("/repo"),
        dir: Some(PathBuf::from("/linked")),
        every: Some("1d".to_owned()),
        timeout: Some("45m".to_owned()),
        signal: Some("ci.failed".to_owned()),
        matches: Some(BTreeMap::from([(
            "branch".to_owned(),
            "feature".to_owned(),
        )])),
        repeat: true,
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
    assert_eq!(toml_round.tasks.0["ci"].wait_meta, entry.wait_meta);
    assert_eq!(
        toml_round.tasks.0.get("ci").and_then(|entry| entry.on),
        Some(CheckOn::Success)
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
        Some("1d")
    );
    assert!(
        toml.contains("every = \"1d\""),
        "an interval cadence should round-trip through TOML: {toml}"
    );
    assert!(toml.contains("timeout = \"45m\""), "{toml}");
    assert!(toml.contains("signal = \"ci.failed\""), "{toml}");
    assert!(toml.contains("[tasks.ci.match]"), "{toml}");
    assert!(toml.contains("branch = \"feature\""), "{toml}");
    assert!(toml.contains("repeat = true"), "{toml}");
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
    assert_eq!(value["ci"]["watch"], serde_json::json!("cargo test"));
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
fn a_row_with_a_key_outside_the_row_refuses_alone_and_keeps_its_values() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("loop.toml");
    let text = "[tasks.ok]\nroot = '/repo'\nargs = ['true']\ntimeout = '1m'\nevery = '1h'\n\
                [tasks.old]\nroot = '/repo'\nagent = 'codex'\nprompt = 'fix'\nevery = '1h'\n";
    let loaded = super::super::parse_loop_text(&path, text).expect("the file still loads");
    let tasks = &loaded.tasks.0;
    assert_eq!(tasks["ok"].refused, None);
    assert_eq!(tasks["ok"].args, ["true"]);
    let refused = tasks["old"].refused.as_ref().expect("the old row refuses");
    assert!(
        refused.detail.contains("unknown field `agent`"),
        "{refused:?}"
    );
    assert_eq!(tasks["old"].root, PathBuf::from("/repo"));
    let rewritten = toml::to_string(&loaded).unwrap();
    let reread = super::super::parse_loop_text(&path, &rewritten).unwrap();
    assert_eq!(
        reread.tasks.0["old"].refused.as_ref().map(|row| &row.raw),
        Some(&refused.raw),
        "a rewrite keeps the refused row as stored: {rewritten}"
    );
    assert_eq!(reread.tasks.0["ok"], tasks["ok"]);
}
