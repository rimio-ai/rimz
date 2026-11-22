use serde_json::json;

use super::*;
use crate::mcp::wording::Wording;

fn tool(name: &str) -> &'static Tool {
    TOOLS
        .iter()
        .find(|tool| tool.name == name)
        .unwrap_or_else(|| panic!("no tool {name}"))
}

fn call(name: &str, arguments: Value) -> Call {
    tool(name).call(arguments).expect("arguments fit")
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

/// Arguments reach the runner as typed values, so a value that looks like a
/// flag or shell text stays data.
#[test]
fn calls_carry_their_arguments_as_data() {
    let Call::Lsp(lsp) = call(
        "lsp",
        json!({"verb": "refs", "targets": ["--help", "Store"], "no_tests": true, "limit": 5}),
    ) else {
        panic!("an lsp call");
    };
    assert_eq!(lsp.verb, LspVerb::Refs);
    assert_eq!(lsp.targets, ["--help", "Store"]);
    assert!(lsp.no_tests && !lsp.all);
    assert_eq!(lsp.limit, Some(5));
    let Call::Wait(wait) = call(
        "wait",
        json!({"trigger": "run", "value": "--version; echo $(id)", "on": "fail", "check_in": "5m"}),
    ) else {
        panic!("a wait call");
    };
    assert_eq!(wait.trigger, WaitTrigger::Run);
    assert_eq!(wait.value, "--version; echo $(id)");
    assert_eq!(wait.check_in.as_deref(), Some("5m"));
    let Call::WaitCancel(cancel) = call("wait_cancel", json!({"name": "--all"})) else {
        panic!("a wait_cancel call");
    };
    assert_eq!((cancel.name.as_deref(), cancel.all), (Some("--all"), false));
    let Call::SelfCompact(compact) = call(
        "self_compact",
        json!({"instruction": "-keep the plan", "no_continue": true}),
    ) else {
        panic!("a self_compact call");
    };
    assert_eq!(compact.instruction.as_deref(), Some("-keep the plan"));
    assert!(compact.no_continue);
    let Call::NotesAdd(note) = call(
        "notes_add",
        json!({
            "kind": "handoff",
            "title": "--force",
            "body": "say \"hi\"\n$(rm -rf /)\n",
            "meta": {"ticket": "42"},
        }),
    ) else {
        panic!("a notes_add call");
    };
    assert_eq!(note.title, "--force");
    assert_eq!(note.body.as_deref(), Some("say \"hi\"\n$(rm -rf /)\n"));
    assert_eq!(note.meta["ticket"], "42");
    let Call::NotesList(list) = call("notes_list", json!({"filter": ["kind=handoff", "age<2d"]}))
    else {
        panic!("a notes_list call");
    };
    assert_eq!(list.filter, ["kind=handoff", "age<2d"]);
}

/// Every tool but `lsp` and `guide` answers data and lists the object
/// schema it fits; those two answer text and list none.
#[test]
fn data_tools_list_an_object_output_schema() {
    for tool in &TOOLS {
        let listed = tool.listing(&Wording::built_in());
        match tool.name {
            "lsp" | GUIDE => assert!(listed.get("outputSchema").is_none(), "{}", tool.name),
            _ => {
                assert_eq!(listed["outputSchema"]["type"], "object", "{}", tool.name);
                assert_eq!(Some(&listed["outputSchema"]), tool.output_schema().as_ref());
            }
        }
    }
    assert_eq!(
        tool("wait").output_schema().unwrap()["required"],
        json!(["id", "status", "stop", "report"])
    );
}

/// `self_compact` answers every receipt status its schema lists, a join included.
#[test]
fn self_compact_schema_admits_every_receipt_status() {
    use crate::message::compact::{CompactReceipt, ReceiptOutcome};

    let schema = tool("self_compact").output_schema().unwrap();
    let statuses: Vec<Value> = [
        ReceiptOutcome::Sent,
        ReceiptOutcome::Queued { continues: true },
        ReceiptOutcome::Joined { continues: true },
    ]
    .into_iter()
    .map(|outcome| {
        serde_json::to_value(CompactReceipt {
            message_id: crate::ids::MessageId::new(),
            outcome,
        })
        .unwrap()["status"]
            .clone()
    })
    .collect();
    assert_eq!(
        statuses,
        json!(["sent", "queued", "joined"])
            .as_array()
            .unwrap()
            .clone()
    );
    assert_eq!(schema["properties"]["status"]["enum"], json!(statuses));
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
        ("notes_list", json!("kind=handoff")),
        ("wait", json!({"trigger": "run", "value": "x", "on": "any"})),
        (
            "notes_add",
            json!({"kind": "k", "title": "t", "cwd": "/src"}),
        ),
        ("notes_list", json!({"limit": 5})),
        ("lsp", json!({"verb": "refs", "targets": []})),
        (
            "lsp",
            json!({"verb": "check", "targets": ["a.md"], "hints": true}),
        ),
        (
            "lsp",
            json!({"verb": "refs", "targets": ["X"], "all": true, "limit": 1}),
        ),
        ("lsp", json!({"verb": "refs", "targets": ["X"], "limit": 0})),
        ("lsp", json!({"verb": "def", "targets": ["X"], "limit": 5})),
        (
            "lsp",
            json!({"verb": "hover", "targets": ["X"], "all": true}),
        ),
        (
            "lsp",
            json!({"verb": "symbols", "targets": ["a.rs"], "external": true}),
        ),
        (
            "lsp",
            json!({"verb": "find", "targets": ["X"], "no_tests": true}),
        ),
        (
            "lsp",
            json!({"verb": "refs", "targets": ["X"], "full": true}),
        ),
        (
            "lsp",
            json!({"verb": "show", "targets": ["X"], "fix": true}),
        ),
    ] {
        assert!(
            tool(name).call(arguments.clone()).is_err(),
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
    assert_eq!(lsp["inputSchema"]["properties"]["targets"]["minItems"], 1);
    assert_eq!(lsp["inputSchema"]["properties"]["limit"]["minimum"], 1);
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
        json!(["fail", "success"])
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
            json!({"every": "1m", "grep": "x", "on": "fail", "check_in": "5m"}),
            "trigger",
        ),
        (json!({"when": ["pr=merged"]}), "when"),
    ] {
        let err = tool("wait").call(arguments.clone()).expect_err("refused");
        assert!(err.to_string().contains(missing), "{arguments}: {err}");
    }
    for trigger in ["run", "check", "file", "pid", "when", "in", "at"] {
        let Call::Wait(wait) = call("wait", json!({"trigger": trigger, "value": "v"})) else {
            panic!("a wait call");
        };
        let parsed: WaitTrigger = serde_json::from_value(json!(trigger)).unwrap();
        assert_eq!(wait.trigger, parsed);
        assert_eq!(wait.value, "v");
    }
}

#[test]
fn lsp_passes_a_server_except_to_check() {
    let server = |verb: &str| match call(
        "lsp",
        json!({"verb": verb, "targets": ["Store"], "server": "ra"}),
    ) {
        Call::Lsp(lsp) => lsp.server().map(str::to_owned),
        other => panic!("an lsp call: {other:?}"),
    };
    assert_eq!(server("refs").as_deref(), Some("ra"));
    assert_eq!(server("check"), None);
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
            tool(name).call(arguments.clone()).is_err(),
            "{name} took {arguments}"
        );
    }
    let Call::SelfTune(tune) = call("self_tune", json!({"setting": "keep-warm"})) else {
        panic!("a self_tune call");
    };
    assert_eq!(
        (tune.setting, tune.value),
        (Some(TuneSetting::KeepWarm), None)
    );
    for arguments in [json!({}), json!({"name": "w", "all": true})] {
        assert!(
            tool("wait_cancel").call(arguments.clone()).is_err(),
            "{arguments}"
        );
    }
}
