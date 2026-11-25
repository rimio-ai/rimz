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
fn a_schedule_takes_exactly_one_trigger_and_declares_no_command() {
    let schedule = TOOLS.iter().find(|tool| tool.name == "schedule").unwrap();
    assert_eq!(schedule.commands(), Vec::<String>::new());
    assert!(Capability::Loop.starts_things());
    let run = serde_json::json!({"agent": {"profile": "worker", "prompt": "go"}});
    for trigger in [
        serde_json::json!({"at": "09:00", "every": "1h"}),
        serde_json::json!({"every": "1h", "match": {"branch": "x"}}),
        serde_json::json!({"cron": "@daily", "for": "1m"}),
        serde_json::json!({}),
    ] {
        let arguments = serde_json::json!({"name": "n", "trigger": trigger, "run": run});
        assert!(schedule.call(arguments).is_err(), "{trigger}");
    }
    let arguments = serde_json::json!({
        "name": "n",
        "trigger": {"signal": "ci.failed", "match": {"branch": "x"}},
        "repeat": true,
        "run": run,
    });
    assert!(schedule.call(arguments).is_ok());
    let two_targets = serde_json::json!({
        "name": "n",
        "trigger": {"every": "1h"},
        "run": {"agent": {"profile": "worker", "prompt": "go"}, "team": {"team": "t", "goal": "g"}},
    });
    assert!(schedule.call(two_targets).is_err());
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
        ["wait", "tasks", "join", "stop", "guide"]
    );
    assert_eq!(
        names(&[Capability::Subagents]),
        ["subagent", "tasks", "join", "stop", "guide"]
    );
    assert!(Capability::Subagents.starts_things());
    assert_eq!(names(&[Capability::Message]), ["message"]);
    assert!(!Capability::Message.starts_things());
    assert_eq!(
        names(&[Capability::Agents]),
        ["agent", "tasks", "join", "stop", "guide"]
    );
    assert_eq!(
        names(&[Capability::Teams]),
        ["team", "tasks", "join", "stop", "guide"]
    );
    assert_eq!(
        names(&[Capability::Workflow]),
        ["workflow", "tasks", "join", "stop", "guide"]
    );
    assert!(Capability::Workflow.starts_things());
    assert_eq!(
        names(&[Capability::Lsp, Capability::Notes]),
        ["lsp", "notes"]
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
    let Call::Stop(stop) = call("stop", json!({"id": "--all"})) else {
        panic!("a stop call");
    };
    assert_eq!(stop.id, "--all");
    let Call::Join(join) = call("join", json!({"id": "wait:$(id)", "timeout": "-1s"})) else {
        panic!("a join call");
    };
    assert_eq!(
        (join.id.as_str(), join.timeout.as_deref()),
        ("wait:$(id)", Some("-1s"))
    );
    let Call::Subagent(subagent) = call(
        "subagent",
        json!({"profile": "--help", "prompt": "-p $(id)", "cwd": "../other", "timeout": "90m"}),
    ) else {
        panic!("a subagent call");
    };
    assert_eq!(
        (subagent.profile.as_str(), subagent.prompt.as_str()),
        ("--help", "-p $(id)")
    );
    assert_eq!(
        subagent.cwd.as_deref(),
        Some(std::path::Path::new("../other"))
    );
    assert_eq!(subagent.timeout.as_deref(), Some("90m"));
    let Call::Message(message) = call("message", json!({"to": "@coder", "text": "--steer $(id)"}))
    else {
        panic!("a message call");
    };
    assert_eq!(
        (message.to.as_str(), message.text.as_str(), message.mode),
        ("@coder", "--steer $(id)", MessageMode::Park)
    );
    assert!(!message.all && !message.reply_wanted);
    let Call::Message(message) = call(
        "message",
        json!({"to": "@coder", "text": "t", "mode": "interrupt", "all": true, "reply_wanted": true}),
    ) else {
        panic!("a message call");
    };
    assert_eq!(message.mode, MessageMode::Interrupt);
    assert!(message.all && message.reply_wanted);
    let Call::Tasks(tasks) = call("tasks", json!({"kind": "schedule"})) else {
        panic!("a tasks call");
    };
    assert_eq!(tasks.kind, Some(StartedKind::Schedule));
    let Call::SelfCompact(compact) = call(
        "self_compact",
        json!({"instruction": "-keep the plan", "no_continue": true}),
    ) else {
        panic!("a self_compact call");
    };
    assert_eq!(compact.instruction.as_deref(), Some("-keep the plan"));
    assert!(compact.no_continue);
    let Call::Notes(note) = call(
        "notes",
        json!({
            "kind": "handoff",
            "title": "--force",
            "body": "say \"hi\"\n$(rm -rf /)\n",
            "meta": {"ticket": "42"},
        }),
    ) else {
        panic!("a notes call");
    };
    assert_eq!(note.title, "--force");
    assert_eq!(note.body.as_deref(), Some("say \"hi\"\n$(rm -rf /)\n"));
    assert_eq!(note.meta["ticket"], "42");
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
    assert_eq!(
        tool("subagent").output_schema(),
        tool("wait").output_schema()
    );
    let subagent = tool("subagent").listing(&Wording::built_in());
    assert_eq!(
        subagent["inputSchema"]["required"],
        json!(["profile", "prompt"])
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
        ("tasks", json!({"kind": "native"})),
        ("tasks", json!({"all": true})),
        ("join", json!({"timeout": "4m"})),
        ("stop", json!({"id": "wait:a", "all": true})),
        ("self_tune", json!({"setting": "budget"})),
        ("self_molt", json!({"agent": "codex"})),
        ("notes", json!({"kind": "log", "title": 7})),
        ("wait", json!({"trigger": "run", "value": "x", "on": "any"})),
        ("notes", json!({"kind": "k", "title": "t", "cwd": "/src"})),
        ("agent", json!({"profile": " ", "prompt": "p"})),
        ("agent", json!({"profile": "coder", "prompt": "\n"})),
        (
            "agent",
            json!({"profile": "coder", "prompt": "p", "fresh": true}),
        ),
        (
            "agent",
            json!({"profile": "coder", "prompt": "p", "worktree": "a", "cwd": "/b"}),
        ),
        (
            "agent",
            json!({"profile": "coder", "prompt": "p", "worktree": "a", "from_pr": "1"}),
        ),
        (
            "agent",
            json!({"profile": "coder", "prompt": "p", "from_pr": "1", "cwd": "/b"}),
        ),
        (
            "agent",
            json!({"profile": "coder", "prompt": "p", "worktree": " "}),
        ),
        (
            "agent",
            json!({"profile": "coder", "prompt": "p", "model": "opus"}),
        ),
        ("team", json!({"team": " ", "prompt": "p", "worktree": "a"})),
        (
            "team",
            json!({"team": "forge", "prompt": " ", "worktree": "a"}),
        ),
        ("team", json!({"team": "forge", "prompt": "p"})),
        (
            "team",
            json!({"team": "forge", "prompt": "p", "worktree": "a", "from_pr": "1"}),
        ),
        (
            "team",
            json!({"team": "forge", "prompt": "p", "from_pr": " "}),
        ),
        ("workflow", json!({"prompt": "p"})),
        ("workflow", json!({"name": ""})),
        ("workflow", json!({"name": "./ship"})),
        ("workflow", json!({"name": "../ship"})),
        ("workflow", json!({"name": "/srv/ship"})),
        ("workflow", json!({"name": "a/b"})),
        ("workflow", json!({"name": "ship", "worktree": " "})),
        ("workflow", json!({"name": "ship", "once": true})),
        ("workflow", json!({"name": "ship", "input": "{}"})),
        ("message", json!({"to": "@coder"})),
        ("message", json!({"to": " ", "text": "t"})),
        ("message", json!({"to": "@coder", "text": "\n"})),
        (
            "message",
            json!({"to": "@coder", "text": "t", "mode": "later"}),
        ),
        (
            "message",
            json!({"to": "@coder", "text": "t", "schedule": "5m"}),
        ),
        (
            "message",
            json!({"to": "@coder", "text": "t", "force": true}),
        ),
        ("subagent", json!({"prompt": "p"})),
        ("subagent", json!({"profile": "general"})),
        ("subagent", json!({"profile": " ", "prompt": "p"})),
        ("subagent", json!({"profile": "general", "prompt": "\n\t"})),
        (
            "subagent",
            json!({"profile": "general", "prompt": "p", "prompt_file": "/b.md"}),
        ),
        (
            "subagent",
            json!({"profile": "general", "prompt": "p", "wait": "5m"}),
        ),
        (
            "subagent",
            json!({"profile": "general", "prompt": "p", "yolo": true}),
        ),
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
    for purpose in TOOLS.iter().map(|tool| tool.purpose) {
        let phrase = purpose.phrase();
        assert!(first.contains(phrase), "{phrase} missing from {first:?}");
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
    for tool in &TOOLS {
        let schema = tool.listing(&wording)["inputSchema"].to_string();
        assert!(
            schema.len() < 5000,
            "{} inputSchema is {} bytes, over the 5,000 Codex keeps",
            tool.name,
            schema.len(),
        );
    }
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
    assert_eq!(
        first,
        "RimZ tools to read code, manage yourself, and load usage guides."
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
fn ambiguous_or_empty_arguments_are_refused() {
    for (name, arguments) in [
        ("self_tune", json!({"value": "budget"})),
        ("self_molt", json!({"prompt": ""})),
        ("self_molt", json!({"prompt": " \n"})),
        (
            "notes",
            json!({"kind": "k", "title": "t", "body": "b", "file": "/f"}),
        ),
        (
            "notes",
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
    for arguments in [json!({}), json!({"id": "wait:w", "timeout": 5})] {
        assert!(tool("join").call(arguments.clone()).is_err(), "{arguments}");
    }
}
