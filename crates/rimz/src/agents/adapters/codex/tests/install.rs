use super::super::install::{
    has_rimz_hook_command, hooks_installed_at, install_into, snake_event_token, uninstall_from,
    untrusted_hook_events_at, untrusted_preflight_hook_events_at,
};
use super::*;

const EXPECTED_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "SubagentStart",
    "SubagentStop",
    "Stop",
    "Interrupt",
    "PermissionRequest",
    "PreToolUse",
    "PostToolUse",
    "PreCompact",
    "PostCompact",
];

#[test]
fn install_into_empty_dir_creates_documented_inline_hooks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let report = install_into(&path).unwrap();
    assert!(!report.files[0].existed);
    assert_eq!(report.agent, "codex");
    assert_eq!(report.installed_events, EXPECTED_EVENTS);
    assert!(hooks_installed_at(&path));

    // Every command is identical (no `--event`; the helper reads the event
    // from the stdin payload's `hook_event_name`).
    let text = std::fs::read_to_string(&path).unwrap();
    insta::assert_snapshot!(text, @r###"
        [[hooks.Interrupt]]

        [[hooks.Interrupt.hooks]]
        command = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex"
        statusMessage = "Routing Interrupt through RimZ"
        timeout = 1
        type = "command"

        [[hooks.PermissionRequest]]
        matcher = ".*"

        [[hooks.PermissionRequest.hooks]]
        command = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex"
        statusMessage = "Routing PermissionRequest through RimZ"
        timeout = 2
        type = "command"

        [[hooks.PostCompact]]
        matcher = ".*"

        [[hooks.PostCompact.hooks]]
        command = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex"
        statusMessage = "Routing PostCompact through RimZ"
        timeout = 2
        type = "command"

        [[hooks.PostToolUse]]
        matcher = ".*"

        [[hooks.PostToolUse.hooks]]
        command = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex"
        statusMessage = "Routing PostToolUse through RimZ"
        timeout = 2
        type = "command"

        [[hooks.PreCompact]]
        matcher = ".*"

        [[hooks.PreCompact.hooks]]
        command = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex"
        statusMessage = "Routing PreCompact through RimZ"
        timeout = 2
        type = "command"

        [[hooks.PreToolUse]]
        matcher = ".*"

        [[hooks.PreToolUse.hooks]]
        command = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex"
        statusMessage = "Routing PreToolUse through RimZ"
        timeout = 2
        type = "command"

        [[hooks.SessionStart]]
        matcher = "startup|resume|clear|compact"

        [[hooks.SessionStart.hooks]]
        command = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex"
        statusMessage = "Routing SessionStart through RimZ"
        timeout = 2
        type = "command"

        [[hooks.Stop]]

        [[hooks.Stop.hooks]]
        command = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex"
        statusMessage = "Routing Stop through RimZ"
        timeout = 2
        type = "command"

        [[hooks.SubagentStart]]
        matcher = ".*"

        [[hooks.SubagentStart.hooks]]
        command = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex"
        statusMessage = "Routing SubagentStart through RimZ"
        timeout = 2
        type = "command"

        [[hooks.SubagentStop]]
        matcher = ".*"

        [[hooks.SubagentStop.hooks]]
        command = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex"
        statusMessage = "Routing SubagentStop through RimZ"
        timeout = 2
        type = "command"

        [[hooks.UserPromptSubmit]]

        [[hooks.UserPromptSubmit.hooks]]
        command = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex"
        statusMessage = "Routing UserPromptSubmit through RimZ"
        timeout = 2
        type = "command"
        "###);
}

#[test]
fn install_preserves_user_hooks_and_wires_per_tool() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"model = "gpt-5.5"

[[hooks.PreToolUse]]
matcher = "^Bash$"

[[hooks.PreToolUse.hooks]]
type = "command"
command = "echo user"
"#,
    )
    .unwrap();

    let report = install_into(&path).unwrap();
    assert!(report.files[0].existed);
    for per_tool_event in ["PreToolUse", "PostToolUse"] {
        assert!(report.installed_events.iter().any(|e| e == per_tool_event));
    }

    let parsed: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        parsed.get("model").and_then(toml::Value::as_str),
        Some("gpt-5.5")
    );
    let pre_tool = parsed
        .get("hooks")
        .and_then(toml::Value::as_table)
        .and_then(|hooks| hooks.get("PreToolUse"))
        .and_then(toml::Value::as_array)
        .unwrap();
    assert!(
        pre_tool.iter().any(|group| {
            group
                .as_table()
                .and_then(|table| table.get("hooks"))
                .and_then(toml::Value::as_array)
                .is_some_and(|handlers| {
                    handlers.iter().any(|handler| {
                        handler
                            .as_table()
                            .and_then(|table| table.get("command"))
                            .and_then(toml::Value::as_str)
                            == Some("echo user")
                    })
                })
        }),
        "user hook must survive install"
    );
    assert!(
        has_rimz_hook_command(&parsed, "PreToolUse"),
        "install wires the broad PreToolUse hook"
    );
}

#[test]
fn uninstall_removes_legacy_block_and_rimz_commands_but_preserves_user_config() {
    let dir = tempfile::tempdir().unwrap();

    // A path that was never written is a no-op, not an error.
    let missing = dir.path().join("missing-config.toml");
    let missing_report = uninstall_from(&missing).unwrap();
    assert!(!missing_report.files[0].existed);
    assert!(missing_report.removed_events.is_empty());

    // The legacy `[hooks.rimz]` block is removed and its declared events
    // reported, while user keys and a user `[hooks.*]` entry survive.
    let legacy = dir.path().join("legacy.toml");
    std::fs::write(
            &legacy,
            "model = \"o4-mini\"\n[hooks.user_custom]\ncommand = [\"echo\", \"hi\"]\n[hooks.rimz]\nevents = [\"SessionStart\", \"PermissionRequest\"]\nmanaged_by = \"rimz\"\n",
        )
        .unwrap();
    let report = uninstall_from(&legacy).unwrap();
    assert!(report.files[0].existed);
    assert_eq!(
        report.removed_events,
        vec!["PermissionRequest".to_owned(), "SessionStart".to_owned()]
    );
    let parsed: toml::Table = toml::from_str(&std::fs::read_to_string(&legacy).unwrap()).unwrap();
    assert_eq!(
        parsed.get("model").and_then(toml::Value::as_str),
        Some("o4-mini")
    );
    let hooks = parsed.get("hooks").and_then(toml::Value::as_table).unwrap();
    assert!(hooks.contains_key("user_custom"));
    assert!(!hooks.contains_key(RIMZ_BLOCK));

    // A real install plus a user Stop hook: uninstall strips every rimz command
    // and leaves the user hook intact.
    let path = dir.path().join("config.toml");
    install_into(&path).unwrap();
    std::fs::write(
            &path,
            format!(
                "{}\n[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"echo user stop\"\n",
                std::fs::read_to_string(&path).unwrap()
            ),
        )
        .unwrap();
    let report = uninstall_from(&path).unwrap();
    assert!(report.files[0].existed);
    assert!(report.removed_events.contains(&"SessionStart".to_owned()));
    assert!(
        report
            .removed_events
            .contains(&"PermissionRequest".to_owned())
    );
    assert!(!hooks_installed_at(&path));
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("echo user stop"));
    assert!(!text.contains("rimz hooks feed --source codex"));
}

#[test]
fn hooks_installed_rejects_legacy_unwrapped_commands() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"[[hooks.SessionStart]]
[[hooks.SessionStart.hooks]]
type = "command"
command = "rimz hooks feed --source codex --event SessionStart"

[[hooks.Stop]]
[[hooks.Stop.hooks]]
type = "command"
command = "rimz hooks feed --source codex --event Stop"

[[hooks.PermissionRequest]]
[[hooks.PermissionRequest.hooks]]
type = "command"
command = "rimz hooks feed --source codex --event PermissionRequest"
"#,
    )
    .unwrap();
    assert!(
        !hooks_installed_at(&path),
        "legacy commands lack the PID wrapper and must be reinstalled"
    );
    install_into(&path).unwrap();
    assert!(hooks_installed_at(&path));
}

#[test]
fn install_reclaims_legacy_event_tables() {
    // Version drift: an older build wrote the exec form *with* `--event`,
    // and a duplicate stacked up. Reinstall must reclaim every old rimz
    // table — regardless of `--event` — and leave exactly one current
    // handler per event, with the user hook untouched.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"[[hooks.SessionStart]]
matcher = "startup|resume|clear|compact"
[[hooks.SessionStart.hooks]]
type = "command"
command = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex --event SessionStart"

[[hooks.SessionStart]]
matcher = "startup|resume|clear|compact"
[[hooks.SessionStart.hooks]]
type = "command"
command = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex --event SessionStart"

[[hooks.PreToolUse]]
matcher = "^Bash$"
[[hooks.PreToolUse.hooks]]
type = "command"
command = "echo user"
"#,
    )
    .unwrap();
    install_into(&path).unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        !text.contains("--event"),
        "every legacy `--event` table must be reclaimed: {text}"
    );
    assert!(text.contains("echo user"), "user hook must survive install");

    let parsed: toml::Table = toml::from_str(&text).unwrap();
    let group_count = |event: &str| {
        parsed
            .get("hooks")
            .and_then(toml::Value::as_table)
            .and_then(|hooks| hooks.get(event))
            .and_then(toml::Value::as_array)
            .map_or(0, Vec::len)
    };
    // Two stacked legacy SessionStart tables collapse to one.
    assert_eq!(group_count("SessionStart"), 1);
    // PreToolUse keeps the user group and gains exactly one rimz group.
    assert_eq!(group_count("PreToolUse"), 2);
    assert!(hooks_installed_at(&path));
}

/// Append a `[hooks.state]` trust entry for `token`, key-shaped exactly as
/// Codex writes it: `"<config-path>:<event_token>:<i>:<j>"`.
fn trust_event(path: &std::path::Path, token: &str) {
    let entry = format!(
        "\n[hooks.state.\"{}:{token}:0:0\"]\ntrusted_hash = \"sha256:deadbeef\"\n",
        path.display(),
    );
    let mut text = std::fs::read_to_string(path).unwrap();
    text.push_str(&entry);
    std::fs::write(path, text).unwrap();
}

#[test]
fn untrusted_hooks_report_by_trust_state() {
    #[derive(Clone, Copy)]
    enum Case {
        StateAbsent,
        EveryEventTrusted,
        OtherHomeTrusted,
        CanonicalHomeTrusted,
        MissingStopAndPermission,
        RimzHooksNotInstalled,
    }

    for (case, label) in [
        (
            Case::StateAbsent,
            "state absent reports every installed event",
        ),
        (Case::EveryEventTrusted, "every event trusted reports none"),
        (
            Case::OtherHomeTrusted,
            "another home's trust does not count",
        ),
        (Case::CanonicalHomeTrusted, "canonical home trust counts"),
        (
            Case::MissingStopAndPermission,
            "partial trust reports the exact missing subset",
        ),
        (Case::RimzHooksNotInstalled, "user-only config reports none"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut path = dir.path().join("config.toml");
        let expected = match case {
            Case::StateAbsent => {
                install_into(&path).unwrap();
                EXPECTED_EVENTS
                    .iter()
                    .map(|event| (*event).to_owned())
                    .collect::<Vec<_>>()
            }
            Case::EveryEventTrusted => {
                install_into(&path).unwrap();
                for event in EXPECTED_EVENTS {
                    trust_event(&path, &snake_event_token(event));
                }
                Vec::new()
            }
            Case::OtherHomeTrusted => {
                let other = dir.path().join("other.toml");
                install_into(&other).unwrap();
                for event in EXPECTED_EVENTS {
                    trust_event(&other, &snake_event_token(event));
                }
                std::fs::copy(other, &path).unwrap();
                EXPECTED_EVENTS
                    .iter()
                    .map(|event| (*event).to_owned())
                    .collect()
            }
            Case::CanonicalHomeTrusted => {
                install_into(&path).unwrap();
                for event in EXPECTED_EVENTS {
                    trust_event(&path, &snake_event_token(event));
                }
                let alias = dir.path().join("alias");
                std::os::unix::fs::symlink(dir.path(), &alias).unwrap();
                path = alias.join("config.toml");
                Vec::new()
            }
            Case::MissingStopAndPermission => {
                install_into(&path).unwrap();
                for event in EXPECTED_EVENTS {
                    // `subagent_stop` trusted while `stop` is not also proves
                    // the token match is colon-delimited, not substring-loose.
                    if *event != "Stop" && *event != "PermissionRequest" {
                        trust_event(&path, &snake_event_token(event));
                    }
                }
                vec!["Stop".to_owned(), "PermissionRequest".to_owned()]
            }
            Case::RimzHooksNotInstalled => {
                std::fs::write(&path, "model = \"gpt-5.5\"\n").unwrap();
                Vec::new()
            }
        };

        assert_eq!(untrusted_hook_events_at(&path), expected, "{label}");
    }
}

#[test]
fn adopting_config_carries_only_trust_and_preserves_target_keys_and_links() {
    let temp = tempfile::tempdir().unwrap();
    let named = temp.path().join("named");
    let native = temp.path().join("native");
    std::fs::create_dir(&named).unwrap();
    std::fs::create_dir(&native).unwrap();
    let existing = named.join("config.toml");
    let target = native.join("config.toml");
    let shared = temp.path().join("shared.toml");
    let original = "model = 'private'\n[hooks.state.named]\ntrusted_hash = 'named'\n[hooks.state.duplicate]\ntrusted_hash = 'old'\n";
    std::fs::write(&existing, original).unwrap();
    std::fs::write(&shared, "model = 'shared'\n[hooks.state.native]\ntrusted_hash = 'native'\n[hooks.state.duplicate]\ntrusted_hash = 'current'\n").unwrap();
    std::os::unix::fs::symlink(&shared, &target).unwrap();
    let login = crate::agents::ProviderLogin::named(
        crate::ids::AgentKind::new_unchecked("codex"),
        "work".parse().unwrap(),
        named.clone(),
    )
    .unwrap();
    let ambient = BTreeMap::from([("CODEX_HOME".to_owned(), native.display().to_string())]);
    let report = crate::agents::account_links::reconcile(&login, &ambient, &|| Some(0))
        .unwrap()
        .unwrap();
    assert!(
        report.to_string().contains(&format!(
            "carried 1 hook trust entry from {} into {}",
            existing.display(),
            target.display()
        )),
        "{report}"
    );
    let table: toml::Value = toml::from_str(&std::fs::read_to_string(&shared).unwrap()).unwrap();
    assert_eq!(table["model"].as_str(), Some("shared"));
    assert_eq!(
        table["hooks"]["state"]["named"]["trusted_hash"].as_str(),
        Some("named")
    );
    assert_eq!(
        table["hooks"]["state"]["native"]["trusted_hash"].as_str(),
        Some("native")
    );
    assert_eq!(
        table["hooks"]["state"]["duplicate"]["trusted_hash"].as_str(),
        Some("current")
    );
    let aside = std::fs::read_dir(named.join(".rimz-aside"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(
        std::fs::read_to_string(aside.join("config.toml")).unwrap(),
        original
    );
    assert_eq!(std::fs::read_link(&existing).unwrap(), target);
    assert_eq!(std::fs::read_link(&target).unwrap(), shared);
    install_into(&existing).unwrap();
    assert!(hooks_installed_at(&target));
    uninstall_from(&existing).unwrap();
    assert!(!hooks_installed_at(&target));
    assert_eq!(std::fs::read_link(&existing).unwrap(), target);
    assert_eq!(std::fs::read_link(&target).unwrap(), shared);
}

#[test]
fn interrupt_trust_is_advisory_for_preflight() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    install_into(&path).unwrap();
    for event in EXPECTED_EVENTS {
        if *event != "Interrupt" {
            trust_event(&path, &snake_event_token(event));
        }
    }

    assert_eq!(untrusted_hook_events_at(&path), ["Interrupt"]);
    assert!(untrusted_preflight_hook_events_at(&path).is_empty());
}
