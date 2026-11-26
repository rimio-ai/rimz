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
fn a_schedule_takes_a_flat_trigger_and_carries_a_launch_tool() {
    let schedule = tool("schedule");
    assert_eq!(schedule.commands(), Vec::<String>::new());
    assert!(Capability::Loop.starts_things());

    let schema = &schedule.listing(&Wording::built_in())["inputSchema"];
    assert_eq!(
        schema["required"],
        json!(["name", "trigger", "value", "tool", "arguments"])
    );
    assert_eq!(
        schema["properties"]["trigger"]["enum"],
        json!(["at", "in", "every", "cron", "signal", "when"])
    );
    assert_eq!(
        schema["properties"]["tool"]["enum"],
        json!(["agent", "team", "workflow"])
    );
    assert_eq!(schema["properties"]["arguments"]["type"], json!("object"));
    assert!(schema["properties"].get("worktree").is_some(), "{schema}");
    assert!(schema["properties"].get("run").is_none(), "{schema}");

    let agent_args = json!({"profile": "worker", "prompt": "go"});
    let team_args = json!({"team": "forge", "prompt": "go"});
    let workflow_args = json!({"name": "held"});

    // Valid flat calls across triggers and all three targets.
    for valid in [
        json!({
            "name": "n",
            "trigger": "signal",
            "value": "ci.failed",
            "match": {"branch": "x"},
            "repeat": true,
            "tool": "agent",
            "arguments": agent_args,
        }),
        json!({
            "name": "n",
            "trigger": "when",
            "value": "ci=passed",
            "for": "5m",
            "repeat": true,
            "overlap": "replace",
            "worktree": "fix",
            "tool": "team",
            "arguments": team_args,
        }),
        json!({
            "name": "n",
            "trigger": "in",
            "value": "30m",
            "tool": "team",
            "arguments": {"team": "forge", "prompt": "go", "from_pr": "42"},
        }),
        json!({
            "name": "n",
            "trigger": "every",
            "value": "1h",
            "tool": "agent",
            "arguments": {"profile": "worker", "prompt": "go", "from_pr": "42"},
        }),
        json!({
            "name": "n",
            "trigger": "every",
            "value": "1h",
            "worktree": "fix",
            "tool": "agent",
            "arguments": {"profile": "worker", "prompt": "go", "fresh": true},
        }),
        json!({
            "name": "n",
            "trigger": "cron",
            "value": "0 9 * * mon",
            "tool": "workflow",
            "arguments": {
                "name": "held",
                "prompt": "--check",
                "deadline": "8h",
                "give_up": "2h",
                "skip_capacity": true,
            },
        }),
    ] {
        assert!(schedule.call(valid.clone()).is_ok(), "{valid}");
    }

    // Trigger-option refusals: each option given to a trigger that cannot take it.
    let mut trigger_failures = Vec::new();
    for (label, arguments, expected) in [
        (
            "match on at",
            json!({"name": "n", "trigger": "at", "value": "09:00", "match": {"branch": "x"}, "tool": "agent", "arguments": agent_args}),
            "`match` takes a `signal` trigger",
        ),
        (
            "match on in",
            json!({"name": "n", "trigger": "in", "value": "5m", "match": {"branch": "x"}, "tool": "agent", "arguments": agent_args}),
            "`match` takes a `signal` trigger",
        ),
        (
            "match on every",
            json!({"name": "n", "trigger": "every", "value": "1h", "match": {"branch": "x"}, "tool": "agent", "arguments": agent_args}),
            "`match` takes a `signal` trigger",
        ),
        (
            "match on cron",
            json!({"name": "n", "trigger": "cron", "value": "@daily", "match": {"branch": "x"}, "tool": "agent", "arguments": agent_args}),
            "`match` takes a `signal` trigger",
        ),
        (
            "match on when",
            json!({"name": "n", "trigger": "when", "value": "ci=passed", "match": {"branch": "x"}, "tool": "agent", "arguments": agent_args}),
            "`match` takes a `signal` trigger",
        ),
        (
            "empty match key",
            json!({"name": "n", "trigger": "signal", "value": "ci.failed", "match": {"": "x"}, "tool": "agent", "arguments": agent_args}),
            "a `match` key is empty",
        ),
        (
            "for on at",
            json!({"name": "n", "trigger": "at", "value": "09:00", "for": "1m", "tool": "agent", "arguments": agent_args}),
            "`for` takes a `when` trigger",
        ),
        (
            "for on in",
            json!({"name": "n", "trigger": "in", "value": "5m", "for": "1m", "tool": "agent", "arguments": agent_args}),
            "`for` takes a `when` trigger",
        ),
        (
            "for on every",
            json!({"name": "n", "trigger": "every", "value": "1h", "for": "1m", "tool": "agent", "arguments": agent_args}),
            "`for` takes a `when` trigger",
        ),
        (
            "for on cron",
            json!({"name": "n", "trigger": "cron", "value": "@daily", "for": "1m", "tool": "agent", "arguments": agent_args}),
            "`for` takes a `when` trigger",
        ),
        (
            "for on signal",
            json!({"name": "n", "trigger": "signal", "value": "ci.failed", "for": "1m", "tool": "agent", "arguments": agent_args}),
            "`for` takes a `when` trigger",
        ),
        (
            "repeat on at",
            json!({"name": "n", "trigger": "at", "value": "09:00", "repeat": true, "tool": "agent", "arguments": agent_args}),
            "`repeat` takes a `signal` or `when` trigger",
        ),
        (
            "repeat on in",
            json!({"name": "n", "trigger": "in", "value": "5m", "repeat": true, "tool": "agent", "arguments": agent_args}),
            "`repeat` takes a `signal` or `when` trigger",
        ),
        (
            "repeat on every",
            json!({"name": "n", "trigger": "every", "value": "1h", "repeat": true, "tool": "agent", "arguments": agent_args}),
            "`repeat` takes a `signal` or `when` trigger",
        ),
        (
            "repeat on cron",
            json!({"name": "n", "trigger": "cron", "value": "@daily", "repeat": true, "tool": "agent", "arguments": agent_args}),
            "`repeat` takes a `signal` or `when` trigger",
        ),
        (
            "overlap on at",
            json!({"name": "n", "trigger": "at", "value": "09:00", "overlap": "skip", "tool": "agent", "arguments": agent_args}),
            "`overlap` takes a repeating trigger",
        ),
        (
            "overlap on in",
            json!({"name": "n", "trigger": "in", "value": "5m", "overlap": "skip", "tool": "agent", "arguments": agent_args}),
            "`overlap` takes a repeating trigger",
        ),
        (
            "overlap on one-shot signal",
            json!({"name": "n", "trigger": "signal", "value": "ci.failed", "overlap": "skip", "tool": "agent", "arguments": agent_args}),
            "`overlap` takes a repeating trigger",
        ),
        (
            "overlap on one-shot when",
            json!({"name": "n", "trigger": "when", "value": "ci=passed", "overlap": "skip", "tool": "agent", "arguments": agent_args}),
            "`overlap` takes a repeating trigger",
        ),
    ] {
        match schedule.call(arguments) {
            Ok(_) => trigger_failures.push(format!("{label}: accepted")),
            Err(err) if !err.to_string().contains(expected) => {
                trigger_failures.push(format!("{label}: got {err:?}, wanted {expected:?}"));
            }
            Err(_) => {}
        }
    }
    assert_eq!(trigger_failures, Vec::<String>::new());

    // Target, worktree, and fire-only refusals.
    let mut target_failures = Vec::new();
    for (label, arguments, expected) in [
        (
            "arguments.worktree on agent",
            json!({"name": "n", "trigger": "every", "value": "1h", "tool": "agent", "arguments": {"profile": "worker", "prompt": "go", "worktree": "feat"}}),
            "arguments.worktree",
        ),
        (
            "arguments.worktree on team",
            json!({"name": "n", "trigger": "every", "value": "1h", "tool": "team", "arguments": {"team": "forge", "prompt": "go", "worktree": "feat"}}),
            "arguments.worktree",
        ),
        (
            "arguments.worktree on workflow",
            json!({"name": "n", "trigger": "every", "value": "1h", "tool": "workflow", "arguments": {"name": "held", "worktree": "feat"}}),
            "arguments.worktree",
        ),
        (
            "team with no place",
            json!({"name": "n", "trigger": "in", "value": "5m", "tool": "team", "arguments": team_args}),
            "`arguments` (with top-level `worktree`): give exactly one of `worktree` and `from_pr`",
        ),
        (
            "team with worktree and from_pr",
            json!({"name": "n", "trigger": "in", "value": "5m", "worktree": "fix", "tool": "team", "arguments": {"team": "forge", "prompt": "go", "from_pr": "42"}}),
            "`arguments` (with top-level `worktree`): give exactly one of `worktree` and `from_pr`",
        ),
        (
            "team with goal instead of prompt",
            json!({"name": "n", "trigger": "every", "value": "1h", "worktree": "fix", "tool": "team", "arguments": {"team": "forge", "goal": "go"}}),
            "`arguments`: unknown field `goal`",
        ),
        (
            "workflow missing required name",
            json!({"name": "n", "trigger": "every", "value": "1h", "tool": "workflow", "arguments": {}}),
            "`arguments`: missing field `name`",
        ),
        (
            "workflow path name",
            json!({"name": "n", "trigger": "every", "value": "1h", "tool": "workflow", "arguments": {"name": "a/b"}}),
            "`arguments`: `name` is a deployed workflow's name, not a path",
        ),
        (
            "agent blank profile",
            json!({"name": "n", "trigger": "every", "value": "1h", "tool": "agent", "arguments": {"profile": " ", "prompt": "go"}}),
            "`arguments`: `profile` is blank",
        ),
        (
            "repeating team every with from_pr",
            json!({"name": "n", "trigger": "every", "value": "1h", "tool": "team", "arguments": {"team": "forge", "prompt": "go", "from_pr": "42"}}),
            "arguments.from_pr",
        ),
        (
            "repeating team signal with from_pr",
            json!({"name": "n", "trigger": "signal", "value": "ci.failed", "repeat": true, "tool": "team", "arguments": {"team": "forge", "prompt": "go", "from_pr": "42"}}),
            "arguments.from_pr",
        ),
        (
            "team with #lane",
            json!({"name": "n", "trigger": "every", "value": "1h", "worktree": "fix", "tool": "team", "arguments": {"team": "forge#fix", "prompt": "go"}}),
            "arguments.team",
        ),
        (
            "agent fresh without worktree",
            json!({"name": "n", "trigger": "every", "value": "1h", "tool": "agent", "arguments": {"profile": "worker", "prompt": "go", "fresh": true}}),
            "`arguments` (with top-level `worktree`): `fresh` needs `worktree`",
        ),
        (
            "agent worktree with cwd",
            json!({"name": "n", "trigger": "every", "value": "1h", "worktree": "fix", "tool": "agent", "arguments": {"profile": "worker", "prompt": "go", "cwd": "."}}),
            "`arguments` (with top-level `worktree`): give at most one of `worktree`, `from_pr`, and `cwd`",
        ),
        (
            "agent unknown field",
            json!({"name": "n", "trigger": "every", "value": "1h", "tool": "agent", "arguments": {"profile": "worker", "prompt": "go", "model": "opus"}}),
            "`arguments`: unknown field `model`",
        ),
        (
            "workflow input",
            json!({"name": "n", "trigger": "every", "value": "1h", "tool": "workflow", "arguments": {"name": "held", "input": {"outcome": "go"}}}),
            "arguments.input",
        ),
        (
            "workflow detach",
            json!({"name": "n", "trigger": "every", "value": "1h", "tool": "workflow", "arguments": {"name": "held", "detach": true}}),
            "arguments.detach",
        ),
        (
            "agent verb profile trimmed",
            json!({"name": "n", "trigger": "every", "value": "1h", "tool": "agent", "arguments": {"profile": " logs ", "prompt": "go"}}),
            "arguments.profile",
        ),
        (
            "agent launch verb profile",
            json!({"name": "n", "trigger": "every", "value": "1h", "tool": "agent", "arguments": {"profile": "launch", "prompt": "go"}}),
            "arguments.profile",
        ),
        (
            "agent address profile",
            json!({"name": "n", "trigger": "every", "value": "1h", "tool": "agent", "arguments": {"profile": "@coder", "prompt": "go"}}),
            "arguments.profile",
        ),
        (
            "agent flag profile",
            json!({"name": "n", "trigger": "every", "value": "1h", "tool": "agent", "arguments": {"profile": "-w", "prompt": "go"}}),
            "arguments.profile",
        ),
        (
            "agent flag prompt",
            json!({"name": "n", "trigger": "every", "value": "1h", "tool": "agent", "arguments": {"profile": "worker", "prompt": "-w now"}}),
            "arguments.prompt",
        ),
        (
            "team flag name",
            json!({"name": "n", "trigger": "every", "value": "1h", "worktree": "fix", "tool": "team", "arguments": {"team": "-nightly", "prompt": "go"}}),
            "arguments.team",
        ),
        (
            "workflow flag name",
            json!({"name": "n", "trigger": "every", "value": "1h", "tool": "workflow", "arguments": {"name": "-nightly"}}),
            "arguments.name",
        ),
        (
            "top-level worktree flag",
            json!({"name": "n", "trigger": "every", "value": "1h", "worktree": "-probe", "tool": "workflow", "arguments": workflow_args}),
            "`worktree` starts with `-`",
        ),
        (
            "NUL in agent description",
            json!({"name": "n", "trigger": "every", "value": "1h", "tool": "agent", "arguments": {"profile": "worker", "prompt": "go", "description": "a\0b"}}),
            "arguments.description",
        ),
        (
            "NUL in agent name",
            json!({"name": "n", "trigger": "every", "value": "1h", "tool": "agent", "arguments": {"profile": "worker", "prompt": "go", "name": "a\0b"}}),
            "arguments.name",
        ),
        (
            "NUL in agent override",
            json!({"name": "n", "trigger": "every", "value": "1h", "tool": "agent", "arguments": {"profile": "worker", "prompt": "go", "agent": "a\0b"}}),
            "arguments.agent",
        ),
        (
            "NUL in agent cwd",
            json!({"name": "n", "trigger": "every", "value": "1h", "tool": "agent", "arguments": {"profile": "worker", "prompt": "go", "cwd": "a\0b"}}),
            "arguments.cwd",
        ),
        (
            "NUL in workflow deadline",
            json!({"name": "n", "trigger": "every", "value": "1h", "tool": "workflow", "arguments": {"name": "held", "deadline": "8h\0"}}),
            "arguments.deadline",
        ),
        (
            "NUL in top-level worktree",
            json!({"name": "n", "trigger": "every", "value": "1h", "worktree": "a\0b", "tool": "workflow", "arguments": workflow_args}),
            "`worktree` holds a NUL",
        ),
    ] {
        match schedule.call(arguments) {
            Ok(_) => target_failures.push(format!("{label}: accepted")),
            Err(err) if !err.to_string().contains(expected) => {
                target_failures.push(format!("{label}: got {err:?}, wanted {expected:?}"));
            }
            Err(_) => {}
        }
    }
    assert_eq!(target_failures, Vec::<String>::new());

    // An unknown field inside `arguments` never lists `worktree` among expected keys.
    for unknown in [
        json!({"name": "n", "trigger": "every", "value": "1h", "worktree": "fix", "tool": "team", "arguments": {"team": "forge", "goal": "go"}}),
        json!({"name": "n", "trigger": "every", "value": "1h", "tool": "agent", "arguments": {"profile": "worker", "prompt": "go", "model": "opus"}}),
        json!({"name": "n", "trigger": "every", "value": "1h", "tool": "workflow", "arguments": {"name": "held", "once": true}}),
    ] {
        let err = schedule
            .call(unknown.clone())
            .expect_err("refused")
            .to_string();
        assert!(
            err.starts_with("`arguments`: ") && !err.contains("`worktree`"),
            "{unknown}: {err}"
        );
    }
}

#[test]
fn argument_text_len_counts_nested_descriptions() {
    let nested = Tool {
        name: "nested",
        capability: None,
        purpose: Purpose::Guides,
        read_only: true,
        command: "",
        declared: &[],
        verbs: &[],
        schema: || {
            json!({
                "type": "object",
                "description": "root",
                "properties": {
                    "description": { "type": "string", "description": "label" },
                    "top": {
                        "type": "object",
                        "description": "outer",
                        "properties": {
                            "leaf": { "type": "string", "description": "inner" }
                        }
                    },
                    "list": {
                        "type": "array",
                        "description": "items",
                        "items": { "type": "string", "description": "one" }
                    }
                }
            })
        },
        output: None,
        parse: |arguments| parse::<GuideArgs>(arguments).map(Call::Guide),
    };
    assert_eq!(nested.argument_text_len(), 27);
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
        names(&[Capability::Blackboard]),
        ["blackboard_flip", "blackboard_record"]
    );
    assert!(!Capability::Blackboard.starts_things() && !Capability::Blackboard.launches_agents());
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
    let Call::BlackboardFlip(flip) = call(
        "blackboard_flip",
        json!({"stage": "--help", "note": "done; $(id)"}),
    ) else {
        panic!("a blackboard_flip call");
    };
    assert_eq!(
        (flip.stage.as_str(), flip.note.as_str()),
        ("--help", "done; $(id)")
    );
    let Call::BlackboardRecord(record) = call(
        "blackboard_record",
        json!({"section": "decisions", "text": "-x `y`"}),
    ) else {
        panic!("a blackboard_record call");
    };
    assert_eq!(
        (record.section, record.text.as_str()),
        (RecordSection::Decisions, "-x `y`")
    );
}

/// Every tool but `lsp`, `blackboard_record`, and `guide` answers data and lists the object
/// schema it fits; those three answer text and list none.
#[test]
fn data_tools_list_an_object_output_schema() {
    for tool in &TOOLS {
        let listed = tool.listing(&Wording::built_in());
        match tool.name {
            "lsp" | "blackboard_record" | GUIDE => {
                assert!(listed.get("outputSchema").is_none(), "{}", tool.name)
            }
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

/// The blackboard tools list the approved definitions as written.
#[test]
fn the_blackboard_tools_list_their_approved_definitions() {
    let wording = Wording::built_in();
    assert_eq!(
        tool("blackboard_flip").listing(&wording),
        json!({
            "name": "blackboard_flip",
            "description": "Hand your finished stage to the next one, or close the run with `Done`. \
                Make it your turn's last call, after every file write has returned. A flip to a \
                stage you own wakes nobody: carry on in this turn.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "stage": {
                        "type": "string",
                        "description": "The next stage by its exact name, or `Done`.",
                    },
                    "note": {
                        "type": "string",
                        "description": "What is done; the reasoning stays in your stage file.",
                    },
                },
                "required": ["stage", "note"],
            },
            "outputSchema": {
                "type": "object",
                "properties": {"stage": {"type": "string"}, "owner": {"type": "string"}},
                "required": ["stage"],
            },
        })
    );
    assert_eq!(
        tool("blackboard_record").listing(&wording),
        json!({
            "name": "blackboard_record",
            "description": "Append an entry to your team's board. Keep it short and point to the \
                stage file that holds the substance.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "section": {"type": "string", "enum": ["goal", "decisions", "result"]},
                    "text": {"type": "string", "description": "The entry."},
                },
                "required": ["section", "text"],
            },
        })
    );
    assert_eq!(
        (
            tool("blackboard_flip").commands(),
            tool("blackboard_record").commands()
        ),
        (
            vec!["rimz teams flip".to_owned()],
            vec!["rimz teams record".to_owned()]
        )
    );
}

/// `self_compact` answers every receipt status its schema lists, a join included.
#[test]
fn self_compact_schema_admits_every_receipt_status() {
    use crate::message::compact::{CompactReceipt, ReceiptOutcome};

    let schema = tool("self_compact").output_schema().unwrap();
    let statuses: Vec<Value> = [
        ReceiptOutcome::Sent { continues: false },
        ReceiptOutcome::Queued { continues: true },
        ReceiptOutcome::Joined { continues: true },
    ]
    .into_iter()
    .map(|outcome| {
        serde_json::to_value(CompactReceipt {
            message_id: crate::ids::MessageId::new(),
            outcome,
            soft_because: None,
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
        ("blackboard_flip", json!({"stage": "Review"})),
        (
            "blackboard_flip",
            json!({"stage": "Review", "note": "n", "team": "forge"}),
        ),
        (
            "blackboard_record",
            json!({"section": "progress", "text": "t"}),
        ),
        ("blackboard_record", json!({"section": "Goal", "text": "t"})),
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
        assert!(
            schema.get("description").is_none(),
            "{}: root inputSchema carries internal doc {:?}",
            tool.name,
            schema.get("description")
        );
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
        |tool| match tool.capability {
            Some(Capability::Lsp) => "always",
            _ => "default",
        },
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
        ("blackboard_flip", json!({"stage": " ", "note": "n"})),
        ("blackboard_flip", json!({"stage": "Review", "note": "\n"})),
        ("blackboard_record", json!({"section": "goal", "text": " "})),
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
