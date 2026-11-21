use serde_json::json;

use super::*;
use crate::mcp::wording::Wording;

fn tool(name: &str) -> &'static Tool {
    TOOLS
        .iter()
        .find(|tool| tool.name == name)
        .unwrap_or_else(|| panic!("no tool {name}"))
}

fn args(name: &str, arguments: Value) -> Vec<String> {
    tool(name)
        .invocation(arguments)
        .expect("arguments fit")
        .args
}

#[test]
fn the_reference_table_lists_every_tool_under_its_capability() {
    let text = include_str!("../../../../docs/reference/cli/mcp.md");
    let published: Vec<(String, String)> = text
        .lines()
        .filter(|line| line.starts_with("| `rimz-") || line.starts_with("| - |"))
        .map(|line| {
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            (
                cells[1].trim_matches('`').to_owned(),
                cells[2].trim_matches('`').to_owned(),
            )
        })
        .collect();
    let catalog: Vec<(String, String)> = TOOLS
        .iter()
        .map(|tool| (tool.group_name().to_owned(), tool.name.to_owned()))
        .collect();
    assert_eq!(published, catalog);
}

#[test]
fn capability_names_round_trip_and_become_only_arguments() {
    for capability in Capability::ALL {
        assert_eq!(Capability::from_name(capability.name()), Some(capability));
    }
    assert_eq!(Capability::from_name("rimz"), None);
    assert_eq!(serve_args(&[]), ["mcp", "serve"]);
    assert_eq!(
        serve_args(&[Capability::Lsp, Capability::Notes]),
        ["mcp", "serve", "--only", "rimz-lsp", "--only", "rimz-notes"]
    );
    let names = |only: &[Capability]| tools_for(only).map(|tool| tool.name).collect::<Vec<_>>();
    assert_eq!(
        names(&[Capability::Wait]),
        ["wait", "wait_list", "wait_cancel", "guide"]
    );
    assert_eq!(
        names(&[Capability::Lsp, Capability::Notes]),
        ["lsp", "notes_add", "notes_list"]
    );
    assert_eq!(tools_for(&[]).count(), TOOLS.len());
    assert_eq!(served_groups(&[]), Capability::ALL);
    assert_eq!(
        served_groups(&[Capability::Notes, Capability::Lsp]),
        [Capability::Lsp, Capability::Notes]
    );
}

#[test]
fn values_join_their_flags_and_positionals_follow_the_terminator() {
    assert_eq!(
        args(
            "lsp",
            json!({"verb": "refs", "targets": ["--help", "Store"], "no_tests": true, "limit": 5}),
        ),
        [
            "lsp",
            "refs",
            "--no-tests",
            "--limit=5",
            "--",
            "--help",
            "Store"
        ]
    );
    assert_eq!(
        args(
            "wait",
            json!({"trigger": "run", "value": "--version; echo $(id)", "on": "fail", "check_in": "5m"}),
        ),
        [
            "wait",
            "--run=--version; echo $(id)",
            "--on=fail",
            "--check-in=5m",
        ]
    );
    assert_eq!(
        args("wait_cancel", json!({"name": "--all"})),
        ["wait", "cancel", "--", "--all"]
    );
    assert_eq!(
        args("wait_cancel", json!({"all": true})),
        ["wait", "cancel", "--all"]
    );
    assert_eq!(args("wait_list", json!({})), ["wait", "list"]);
    assert_eq!(
        args("self_stop", json!({})),
        ["agents", "stop", "--when-idle", "--", "@me"]
    );
    assert_eq!(
        args("self_stop", json!({"when_idle": "off"})),
        ["agents", "stop", "--when-idle=off", "--", "@me"]
    );
    assert_eq!(
        args("self_compact", json!({"instruction": "-keep the plan"})),
        ["agents", "compact", "--", "@me", "-keep the plan"]
    );
    assert_eq!(
        args(
            "self_compact",
            json!({"instruction": "-keep the plan", "no_continue": true})
        ),
        [
            "agents",
            "compact",
            "--no-continue",
            "--",
            "@me",
            "-keep the plan"
        ]
    );
    assert_eq!(
        args(
            "self_tune",
            json!({"setting": "idle-compact", "value": "120k"})
        ),
        ["agents", "tune", "--", "@me", "idle-compact", "120k"]
    );
    assert_eq!(
        args(
            "notes_list",
            json!({"filter": ["kind=handoff"], "limit": 0})
        ),
        ["notes", "list", "--filter=kind=handoff", "--limit=0"]
    );
}

#[test]
fn a_notes_body_travels_on_stdin_and_cwd_is_kept() {
    let invocation = tool("notes_add")
        .invocation(json!({
            "kind": "handoff",
            "title": "--force",
            "body": "say \"hi\"\n$(rm -rf /)\n",
            "meta": {"ticket": "42"},
            "cwd": "/src/repo",
        }))
        .expect("arguments fit");
    assert_eq!(
        invocation,
        Invocation {
            args: [
                "notes",
                "add",
                "--meta=ticket=42",
                "--",
                "handoff",
                "--force"
            ]
            .map(String::from)
            .to_vec(),
            cwd: Some(PathBuf::from("/src/repo")),
            stdin: Some(b"say \"hi\"\n$(rm -rf /)\n".to_vec()),
        }
    );
}

#[test]
fn arguments_outside_the_schema_shape_are_refused() {
    for (name, arguments) in [
        ("lsp", json!({"verb": "rename", "targets": []})),
        ("lsp", json!({"verb": "def"})),
        ("wait", json!({"trigger": "sleep", "value": "5m"})),
        ("wait", json!({"trigger": "pid", "value": 42})),
        (
            "wait",
            json!({"trigger": "in", "value": "5m", "timeout": "5m"}),
        ),
        ("wait_list", json!({"all": true})),
        ("self_tune", json!({"setting": "budget"})),
        ("notes_add", json!({"kind": "log", "title": 7})),
        ("notes_list", json!([])),
    ] {
        assert!(
            tool(name).invocation(arguments.clone()).is_err(),
            "{name} took {arguments}"
        );
    }
}

#[test]
fn schemas_are_inline_objects_without_null_types() {
    for tool in &TOOLS {
        let listing = tool.listing(&Wording::built_in());
        let schema = &listing["inputSchema"];
        assert_eq!(schema["type"], "object", "{}", tool.name);
        let text = schema.to_string();
        for absent in ["null", "$ref", "$schema", "definitions", "anyOf", "oneOf"] {
            assert!(!text.contains(absent), "{}: {text}", tool.name);
        }
    }
    let lsp = tool("lsp").listing(&Wording::built_in());
    assert_eq!(lsp["inputSchema"]["required"], json!(["verb", "targets"]));
    assert_eq!(
        lsp["inputSchema"]["properties"]["verb"]["enum"],
        json!([
            "def", "hover", "refs", "callers", "callees", "impl", "symbols", "find", "show",
            "check"
        ])
    );
    let wait = tool("wait").listing(&Wording::built_in());
    assert_eq!(
        wait["inputSchema"]["properties"]["on"]["enum"],
        json!(["fail", "success", "any"])
    );
    assert_eq!(
        wait["inputSchema"]["properties"]["trigger"]["enum"],
        json!(["run", "check", "file", "pid", "when", "in", "at"])
    );
    assert_eq!(wait["inputSchema"]["required"], json!(["trigger", "value"]));
    let guide = tool("guide").listing(&Wording::built_in());
    assert_eq!(guide["inputSchema"]["required"], json!(["name"]));
}

/// Codex compacts an input schema over 5,000 bytes; the text limits are
/// `Wording::breaches`'.
#[test]
fn the_text_fits_what_providers_keep() {
    let wording = Wording::built_in();
    assert_eq!(wording.breaches(), Vec::<String>::new());
    let instructions = wording.instructions(&Capability::ALL);
    let first = instructions.lines().next().unwrap_or_default();
    for group in ["`lsp`", "`wait`", "`notes_", "`self_*` when the user asks"] {
        assert!(first.contains(group), "{group} missing from {first:?}");
    }
    for tool in TOOLS
        .iter()
        .filter(|tool| tool.capability == Some(Capability::Control))
    {
        let description = wording.description(tool).text();
        assert!(
            description.starts_with("When the user"),
            "{} acts on the agent unasked: {description}",
            tool.name,
        );
    }
    let wait = tool("wait").listing(&wording)["inputSchema"].to_string();
    assert!(wait.len() < 5000, "{}", wait.len());
}

#[test]
fn the_full_listing_is_the_served_text() {
    let wording = Wording::built_in();
    let groups = [Capability::Lsp, Capability::Control];
    let tools: Vec<&Tool> = tools_for(&groups).collect();
    let text = render_listing(
        &tools,
        &groups,
        &[(Capability::Lsp, LoadMode::Always)],
        None,
        &wording,
    );
    let instructions = wording.instructions(&groups);
    assert!(text.contains(&instructions), "{text}");
    let first = instructions.lines().next().unwrap();
    assert!(
        first.contains("`lsp`") && first.contains("`self_*`"),
        "{first}"
    );
    assert!(
        !first.contains("`wait`") && !first.contains("`notes_"),
        "{first}"
    );
    assert!(!text.contains("only some of these tools"), "{text}");
    assert!(!text.contains(" · text "), "{text}");
    let served = tools_list(&tools, &[], &wording);
    for (tool, entry) in tools.iter().zip(served["tools"].as_array().unwrap()) {
        let schema = serde_json::to_string_pretty(&entry["inputSchema"]).unwrap();
        assert!(text.contains(&format!("\n## {}\n", tool.name)), "{text}");
        assert!(
            text.contains(entry["description"].as_str().unwrap()),
            "{text}"
        );
        assert!(text.contains(&format!("```json\n{schema}\n```")), "{text}");
    }
    let lsp = format!(
        "## lsp\n~{} tokens · rimz-lsp · load always\n",
        tool("lsp").tokens(&wording)
    );
    assert!(text.contains(&lsp), "{text}");
    assert!(text.contains("· rimz-control · load default\n"), "{text}");
    assert!(!text.contains("## wait"), "{text}");
}

/// Without a trigger, `rimz wait` lists the waits and exits 0, which would
/// read as a wake armed; modifiers alone are no trigger.
#[test]
fn a_wait_needs_one_trigger_and_its_value() {
    for (arguments, missing) in [
        (json!({}), "trigger"),
        (json!({"value": "5m"}), "trigger"),
        (json!({"trigger": "in"}), "value"),
        (json!({"label": "x"}), "trigger"),
        (
            json!({"every": "1m", "grep": "x", "on": "any", "check_in": "5m"}),
            "trigger",
        ),
        (json!({"when": ["pr=merged"]}), "when"),
    ] {
        let err = tool("wait")
            .invocation(arguments.clone())
            .expect_err("refused");
        assert!(err.to_string().contains(missing), "{arguments}: {err}");
    }
    for trigger in ["run", "check", "file", "pid", "when", "in", "at"] {
        assert_eq!(
            args("wait", json!({"trigger": trigger, "value": "v"})),
            ["wait".to_owned(), format!("--{trigger}=v")]
        );
    }
    assert_eq!(
        args(
            "wait",
            json!({"trigger": "when", "value": "pr=merged && !ci=failed", "label": "x"})
        ),
        ["wait", "--when=pr=merged && !ci=failed", "--label=x"]
    );
}

#[test]
fn lsp_passes_a_server_except_to_check() {
    assert_eq!(
        args(
            "lsp",
            json!({"verb": "refs", "targets": ["Store"], "server": "ra"})
        ),
        ["lsp", "refs", "--server=ra", "--", "Store"]
    );
    assert_eq!(
        args(
            "lsp",
            json!({"verb": "check", "targets": ["a.md"], "server": "ra"})
        ),
        ["lsp", "check", "--", "a.md"]
    );
    let description = Wording::built_in()
        .description(tool("lsp"))
        .text()
        .to_owned();
    assert!(description.contains("`check` ignores"), "{description}");
}

#[test]
fn ambiguous_tune_and_note_arguments_are_refused() {
    for (name, arguments) in [
        ("self_tune", json!({"value": "budget"})),
        (
            "notes_add",
            json!({"kind": "k", "title": "t", "body": "b", "file": "/f"}),
        ),
        (
            "notes_add",
            json!({"kind": "k", "title": "t", "meta": {"a=b": "c"}}),
        ),
    ] {
        assert!(
            tool(name).invocation(arguments.clone()).is_err(),
            "{name} took {arguments}"
        );
    }
    assert_eq!(
        args("self_tune", json!({"setting": "keep-warm"})),
        ["agents", "tune", "--", "@me", "keep-warm"]
    );
    let cwd = tool("notes_add").listing(&Wording::built_in())["inputSchema"]["properties"]["cwd"]["description"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(cwd.contains("room"), "{cwd}");
}
