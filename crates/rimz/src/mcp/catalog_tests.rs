use serde_json::json;

use super::*;

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
        .filter(|line| line.starts_with("| `rimz-"))
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
        .map(|tool| (tool.capability.name().to_owned(), tool.name.to_owned()))
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
    let names: Vec<_> = tools_for(&[Capability::Wait])
        .map(|tool| tool.name)
        .collect();
    assert_eq!(names, ["wait", "wait_list", "wait_cancel"]);
    assert_eq!(tools_for(&[]).count(), TOOLS.len());
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
            json!({"run": "--version; echo $(id)", "when": ["pr=merged", "ci=green"], "in": "5m"}),
        ),
        [
            "wait",
            "--run=--version; echo $(id)",
            "--when=pr=merged",
            "--when=ci=green",
            "--in=5m",
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
        ("wait", json!({"pid": "one"})),
        ("wait", json!({"timeout": "5m"})),
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
        let listing = tool.listing();
        let schema = &listing["inputSchema"];
        assert_eq!(schema["type"], "object", "{}", tool.name);
        let text = schema.to_string();
        for absent in ["null", "$ref", "$schema", "definitions", "anyOf"] {
            assert!(!text.contains(absent), "{}: {text}", tool.name);
        }
    }
    let lsp = tool("lsp").listing();
    assert_eq!(lsp["inputSchema"]["required"], json!(["verb", "targets"]));
    assert_eq!(
        lsp["inputSchema"]["properties"]["verb"]["enum"],
        json!([
            "def", "hover", "refs", "callers", "callees", "impl", "symbols", "find", "show",
            "check"
        ])
    );
    assert_eq!(
        tool("wait").listing()["inputSchema"]["properties"]["on"]["enum"],
        json!(["fail", "success", "any"])
    );
}

/// Without a trigger, `rimz wait` lists the waits and exits 0, which would
/// read as a wake armed; modifiers alone are no trigger.
#[test]
fn a_wait_without_a_trigger_is_refused() {
    for arguments in [
        json!({}),
        json!({"when": []}),
        json!({"label": "x"}),
        json!({"every": "1m", "grep": "x", "on": "any", "check_in": "5m"}),
    ] {
        let err = tool("wait")
            .invocation(arguments.clone())
            .expect_err("no trigger");
        assert!(err.to_string().contains("trigger"), "{arguments}: {err}");
    }
    assert_eq!(
        args("wait", json!({"when": ["pr=merged"]})),
        ["wait", "--when=pr=merged"]
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
    assert!(tool("lsp").description.contains("`check` ignores"));
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
    let cwd = tool("notes_add").listing()["inputSchema"]["properties"]["cwd"]["description"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(cwd.contains("room"), "{cwd}");
}
