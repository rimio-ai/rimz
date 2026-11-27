use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::*;

/// The refusal's head: the text before its first `: `, which names the field a grammar refused.
fn head(refusal: Option<String>) -> Option<String> {
    refusal.map(|refusal| match refusal.split_once(": ") {
        Some((head, _)) => head.to_owned(),
        None => refusal,
    })
}

fn refusal<B: Block>(object: Value, caller: Caller) -> Option<String> {
    head(parse::<B>(object, caller).err())
}

fn by_name(name: BlockName, object: Value, caller: Caller) -> Option<String> {
    match name {
        BlockName::Agent => refusal::<Agent>(object, caller),
        BlockName::Team => refusal::<Team>(object, caller),
        BlockName::Shell => refusal::<Shell>(object, caller),
        BlockName::Watch => refusal::<Watch>(object, caller),
        BlockName::Workflow => refusal::<Workflow>(object, caller),
        BlockName::Schedule => refusal::<Schedule>(object, caller),
        BlockName::Now => refusal::<Now>(object, caller),
    }
}

/// One object per block that parses for either caller, with the fields each requires.
fn minimal() -> Vec<(BlockName, Value, &'static [&'static str])> {
    vec![
        (
            BlockName::Agent,
            json!({ "profile": "judge", "prompt": "Review." }),
            &["profile", "prompt"],
        ),
        (
            BlockName::Team,
            json!({ "team": "feature", "prompt": "Build.", "worktree": "fix-tokens" }),
            &["team", "prompt"],
        ),
        (
            BlockName::Shell,
            json!({ "command": "cargo xtask check" }),
            &["command"],
        ),
        (BlockName::Watch, json!({ "when": "pr=merged" }), &["when"]),
        (
            BlockName::Workflow,
            json!({ "source": "name", "value": "land" }),
            &["source", "value"],
        ),
        (
            BlockName::Schedule,
            json!({ "name": "triage", "trigger": "cron", "value": "0 9 * * mon-fri",
                    "tool": "agent", "arguments": { "profile": "triager", "prompt": "Rule." } }),
            &["name", "trigger", "value", "tool", "arguments"],
        ),
        (BlockName::Now, json!({}), &[]),
    ]
}

fn with(mut object: Value, field: &str, value: Value) -> Value {
    object
        .as_object_mut()
        .expect("every fixture object is a JSON object")
        .insert(field.to_owned(), value);
    object
}

#[test]
fn each_block_parses_its_minimal_object_for_both_callers() {
    let mut refused = Vec::new();
    for (name, object, _) in minimal() {
        for caller in [Caller::Tool, Caller::Script] {
            refused.push((name, caller, by_name(name, object.clone(), caller)));
        }
    }
    refused.retain(|(_, _, refusal)| refusal.is_some());
    assert_eq!(refused, Vec::new());
}

#[test]
fn every_required_field_is_refused_when_missing() {
    let mut got = Vec::new();
    let mut want = Vec::new();
    for (name, object, required) in minimal() {
        for field in required {
            let mut object = object.clone();
            object
                .as_object_mut()
                .expect("every fixture object is a JSON object")
                .remove(*field);
            got.push((name, by_name(name, object, Caller::Tool)));
            want.push((name, Some(format!("missing field `{field}`"))));
        }
    }
    assert_eq!(got, want);
}

#[test]
fn an_unknown_field_is_refused_on_every_block() {
    let got: Vec<_> = minimal()
        .into_iter()
        .map(|(name, object, _)| {
            let refusal = by_name(name, with(object, "bogus", json!(1)), Caller::Tool);
            (
                name,
                refusal.map(|refusal| refusal.split(',').next().map(str::to_owned)),
            )
        })
        .collect();
    let want: Vec<_> = minimal()
        .into_iter()
        .map(|(name, ..)| (name, Some(Some("unknown field `bogus`".to_owned()))))
        .collect();
    assert_eq!(got, want);
}

#[test]
fn a_script_cannot_pass_the_fields_that_wake_a_calling_agent_or_own_its_run() {
    let cases = [
        (
            BlockName::Shell,
            json!({ "command": "make", "on": "fail" }),
            "on",
        ),
        (
            BlockName::Shell,
            json!({ "command": "make", "check_in": "10m" }),
            "check_in",
        ),
        (
            BlockName::Watch,
            json!({ "when": "pr=merged", "check_in": "10m" }),
            "check_in",
        ),
        (
            BlockName::Workflow,
            json!({ "source": "name", "value": "land", "detach": true }),
            "detach",
        ),
        (
            BlockName::Workflow,
            json!({ "source": "name", "value": "land", "worktree": "pr-1" }),
            "worktree",
        ),
        (
            BlockName::Workflow,
            json!({ "source": "name", "value": "land", "max_launches": 4 }),
            "max_launches",
        ),
    ];
    let got: Vec<_> = cases
        .iter()
        .map(|(name, object, _)| {
            (
                by_name(*name, object.clone(), Caller::Script),
                by_name(*name, object.clone(), Caller::Tool),
            )
        })
        .collect();
    let want: Vec<_> = cases
        .iter()
        .map(|(name, _, field)| {
            (
                Some(format!(
                    "a script awaits its calls and owns its run, so its `{}` does not take `{field}`",
                    name.as_str()
                )),
                None,
            )
        })
        .collect();
    assert_eq!(got, want);
}

#[test]
fn a_script_cannot_run_an_ended_run_again() {
    let object = json!({ "source": "run", "value": "wf-123" });
    assert_eq!(
        (
            refusal::<Workflow>(object.clone(), Caller::Script),
            refusal::<Workflow>(object, Caller::Tool),
        ),
        (Some("a script runs no ended run again".to_owned()), None)
    );
}

#[test]
fn every_duration_field_is_parsed() {
    let bad = json!("soon");
    let cases: Vec<(BlockName, Value, &str)> = vec![
        (
            BlockName::Agent,
            with(
                json!({ "profile": "p", "prompt": "x" }),
                "timeout",
                bad.clone(),
            ),
            "`timeout`",
        ),
        (
            BlockName::Team,
            with(
                json!({ "team": "t", "prompt": "x", "worktree": "w" }),
                "timeout",
                bad.clone(),
            ),
            "`timeout`",
        ),
        (
            BlockName::Shell,
            with(json!({ "command": "make" }), "timeout", bad.clone()),
            "`timeout`",
        ),
        (
            BlockName::Shell,
            with(json!({ "command": "make" }), "check_in", bad.clone()),
            "`check_in`",
        ),
        (
            BlockName::Watch,
            json!({ "when": "pr=merged", "timeout": "soon" }),
            "`timeout`",
        ),
        (
            BlockName::Watch,
            json!({ "when": "pr=merged", "check_in": "soon" }),
            "`check_in`",
        ),
        (
            BlockName::Watch,
            json!({ "when": "pr=merged", "check_in": "24h" }),
            "`check_in`",
        ),
        (
            BlockName::Workflow,
            json!({ "source": "name", "value": "land", "timeout": "soon" }),
            "`timeout`",
        ),
        (
            BlockName::Schedule,
            json!({ "name": "n", "trigger": "in", "value": "soon", "tool": "agent",
                    "arguments": { "profile": "p", "prompt": "x" } }),
            "`value`",
        ),
        (
            BlockName::Schedule,
            json!({ "name": "n", "trigger": "every", "value": "0m", "tool": "agent",
                    "arguments": { "profile": "p", "prompt": "x" } }),
            "`value`",
        ),
        (
            BlockName::Schedule,
            json!({ "name": "n", "trigger": "when", "value": "pr=merged", "for": "soon",
                    "tool": "agent", "arguments": { "profile": "p", "prompt": "x" } }),
            "`for`",
        ),
    ];
    let got: Vec<_> = cases
        .iter()
        .map(|(name, object, _)| by_name(*name, object.clone(), Caller::Tool))
        .collect();
    let want: Vec<_> = cases
        .iter()
        .map(|(_, _, field)| Some((*field).to_owned()))
        .collect();
    assert_eq!(got, want);
}

#[test]
fn conditions_and_clocks_are_parsed() {
    let arguments = json!({ "profile": "p", "prompt": "x" });
    let cases: Vec<(BlockName, Value, Option<&str>)> = vec![
        (BlockName::Watch, json!({ "when": "pr=merged" }), None),
        (BlockName::Watch, json!({ "when": "pr=" }), Some("`when`")),
        (BlockName::Watch, json!({ "when": "&&" }), Some("`when`")),
        (
            BlockName::Schedule,
            json!({ "name": "n", "trigger": "when", "value": "pr=", "tool": "agent", "arguments": arguments }),
            Some("`value`"),
        ),
        (
            BlockName::Schedule,
            json!({ "name": "n", "trigger": "at", "value": "teatime", "tool": "agent", "arguments": arguments }),
            Some("`value`"),
        ),
        (
            BlockName::Schedule,
            json!({ "name": "n", "trigger": "at", "value": "09:30", "tool": "agent", "arguments": arguments }),
            None,
        ),
    ];
    let got: Vec<_> = cases
        .iter()
        .map(|(name, object, _)| by_name(*name, object.clone(), Caller::Tool))
        .collect();
    let want: Vec<_> = cases
        .iter()
        .map(|(_, _, refusal)| refusal.map(str::to_owned))
        .collect();
    assert_eq!(got, want);
}

#[test]
fn a_watch_takes_the_watch_tool_fields_and_a_timeout() {
    let cases = [
        json!({ "when": "pr=merged || pr=closed", "label": "land it", "cwd": "/src",
                "timeout": "3d" }),
        json!({ "when": "pr=merged", "cases": [{ "when": "pr=merged", "outcome": "m" }] }),
        json!({ "when": "pr=merged", "for": "5m" }),
        json!({ "trigger": "in", "value": "15m" }),
        json!({ "label": "land it" }),
    ];
    let got: Vec<_> = cases
        .into_iter()
        .map(|object| refusal::<Watch>(object, Caller::Tool))
        .map(|refusal| refusal.map(|refusal| refusal.split(',').next().map(str::to_owned)))
        .collect();
    let want: Vec<Option<Option<String>>> = [
        None,
        Some("unknown field `cases`"),
        Some("unknown field `for`"),
        Some("unknown field `trigger`"),
        Some("missing field `when`"),
    ]
    .into_iter()
    .map(|refusal| refusal.map(|refusal| Some(refusal.to_owned())))
    .collect();
    assert_eq!(got, want);
}

#[test]
fn an_answer_schema_uses_only_the_named_keywords() {
    let schema = |schema: Value| json!({ "profile": "judge", "prompt": "x", "schema": schema });
    let cases = [
        schema(json!({ "type": "object", "required": ["outcome"],
                       "properties": { "outcome": { "enum": ["done", "blocked"] },
                                       "question": { "type": "string" },
                                       "list": { "type": "array", "items": { "type": "string" } } } })),
        schema(json!({ "type": "object", "additionalProperties": false })),
        schema(json!({ "properties": { "outcome": { "type": "string", "minLength": 1 } } })),
        schema(json!({ "type": "array", "items": { "oneOf": [] } })),
        schema(json!({ "properties": { "outcome": true } })),
        schema(json!({ "items": [] })),
    ];
    let got: Vec<_> = cases
        .into_iter()
        .map(|object| refusal::<Agent>(object, Caller::Script))
        .map(|refusal| refusal.map(|refusal| refusal.split(';').next().map(str::to_owned)))
        .collect();
    let want: Vec<Option<Option<String>>> = [
        None,
        Some("`schema` uses `additionalProperties`"),
        Some("`schema.properties.outcome` uses `minLength`"),
        Some("`schema.items` uses `oneOf`"),
        Some("`schema.properties.outcome` is not a schema object"),
        Some("`schema.items` is not an object"),
    ]
    .into_iter()
    .map(|refusal| refusal.map(|refusal| Some(refusal.to_owned())))
    .collect();
    assert_eq!(got, want);
}

#[test]
fn the_tool_refusals_hold_for_both_callers() {
    let agent = json!({ "profile": "p", "prompt": "x" });
    let booking = |extra: Value| {
        let mut object = json!({ "name": "n", "trigger": "cron", "value": "0 9 * * *", "tool": "agent",
                                 "arguments": { "profile": "p", "prompt": "x" } });
        for (field, value) in extra.as_object().expect("an object of extra fields") {
            object = with(object, field, value.clone());
        }
        object
    };
    let cases: Vec<(BlockName, Value, &str)> = vec![
        (
            BlockName::Agent,
            json!({ "profile": " ", "prompt": "x" }),
            "`profile` is blank",
        ),
        (
            BlockName::Agent,
            with(
                with(agent.clone(), "worktree", json!("w")),
                "cwd",
                json!("/tmp"),
            ),
            "give at most one of `worktree`, `from_pr`, and `cwd`",
        ),
        (
            BlockName::Agent,
            with(agent.clone(), "fresh", json!(true)),
            "`fresh` needs `worktree`",
        ),
        (
            BlockName::Agent,
            with(agent, "from_pr", json!(" ")),
            "`from_pr` is blank",
        ),
        (
            BlockName::Team,
            json!({ "team": "t", "prompt": "x" }),
            "give exactly one of `worktree` and `from_pr`",
        ),
        (
            BlockName::Shell,
            json!({ "command": " " }),
            "`command` is blank",
        ),
        (
            BlockName::Shell,
            json!({ "command": "x", "input": "text" }),
            "invalid type",
        ),
        (
            BlockName::Workflow,
            json!({ "source": "name", "value": "a/b" }),
            "`value` is an installed workflow's name, not a path; give `source` `path`",
        ),
        (
            BlockName::Workflow,
            json!({ "source": "script", "value": " " }),
            "`value` is blank",
        ),
        (
            BlockName::Schedule,
            booking(json!({ "match": { "k": "v" } })),
            "`match` takes a `signal` trigger",
        ),
        (
            BlockName::Schedule,
            booking(json!({ "for": "5m" })),
            "`for` takes a `when` trigger",
        ),
        (
            BlockName::Schedule,
            booking(json!({ "repeat": true })),
            "`repeat` takes a `signal` or `when` trigger",
        ),
        (
            BlockName::Schedule,
            json!({ "name": "n", "trigger": "in", "value": "5m", "tool": "agent",
                                      "arguments": { "profile": "p", "prompt": "x" }, "overlap": "skip" }),
            "`overlap` takes a repeating trigger",
        ),
        (
            BlockName::Schedule,
            booking(json!({ "worktree": "-x" })),
            "`worktree` starts with `-`, which a fire's launch reads as a flag",
        ),
        (
            BlockName::Schedule,
            booking(json!({ "arguments": { "profile": "p", "prompt": "x", "worktree": "w" } })),
            "`arguments.worktree`",
        ),
        (
            BlockName::Schedule,
            booking(json!({ "arguments": { "profile": "p" } })),
            "`arguments`",
        ),
        (
            BlockName::Schedule,
            json!({ "name": "n", "trigger": "signal", "value": " ", "tool": "agent",
                                      "arguments": { "profile": "p", "prompt": "x" } }),
            "`value` is blank",
        ),
    ];
    let got: Vec<_> = cases
        .iter()
        .map(|(name, object, _)| {
            (
                by_name(*name, object.clone(), Caller::Tool),
                by_name(*name, object.clone(), Caller::Script),
            )
        })
        .collect();
    let want: Vec<_> = cases
        .iter()
        .map(|(_, _, refusal)| (Some((*refusal).to_owned()), Some((*refusal).to_owned())))
        .collect();
    assert_eq!(got, want);
}

#[test]
fn a_booking_parses_its_carried_block_as_a_fresh_start() {
    let object = json!({ "name": "n", "trigger": "cron", "value": "0 9 * * *", "tool": "workflow",
                         "worktree": "pr-1", "arguments": { "source": "name", "value": "land", "detach": true } });
    assert_eq!(refusal::<Schedule>(object, Caller::Script), None);
}

#[test]
fn the_digest_hashes_the_block_name_and_the_object_with_sorted_keys() {
    let object = json!({ "prompt": "Review.", "profile": "judge", "schema": { "type": "object", "required": ["b", "a"] } });
    let canonical = r#"agent
{"profile":"judge","prompt":"Review.","schema":{"required":["b","a"],"type":"object"}}"#;
    let want = hex::encode(Sha256::digest(canonical.as_bytes()));
    let got = digest(BlockName::Agent, &object);
    assert_eq!(
        (got.clone(), got == digest(BlockName::Team, &object)),
        (want, false)
    );
}

#[test]
fn the_types_print_every_field_a_script_may_pass() {
    fn missing<B: Block>(text: &str) -> Vec<String> {
        let schema = schema::<B>();
        schema
            .get("properties")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|properties| properties.keys())
            .filter(|field| !B::SCRIPT_REFUSED.contains(&field.as_str()))
            .filter(|field| {
                !text.contains(&format!("  {field}:")) && !text.contains(&format!("  {field}?:"))
            })
            .map(|field| format!("{}.{field}", B::NAME.as_str()))
            .collect()
    }
    let text = typescript();
    let mut missed = missing::<Agent>(&text);
    missed.extend(missing::<Team>(&text));
    missed.extend(missing::<Shell>(&text));
    missed.extend(missing::<Watch>(&text));
    missed.extend(missing::<Workflow>(&text));
    missed.extend(missing::<Schedule>(&text));
    missed.extend(missing::<Now>(&text));
    let declared = [
        BlockName::Agent,
        BlockName::Team,
        BlockName::Shell,
        BlockName::Watch,
        BlockName::Workflow,
        BlockName::Schedule,
        BlockName::Now,
    ]
    .iter()
    .filter(|name| !text.contains(&format!("declare function {}(", name.as_str())))
    .count();
    assert_eq!(
        (
            missed,
            declared,
            text.contains("interface Failure extends Error")
        ),
        (Vec::<String>::new(), 0, true)
    );
}

#[test]
fn the_types_hide_the_fields_a_script_may_not_pass() {
    let text = typescript();
    let shell = text
        .split("interface Shell ")
        .nth(1)
        .and_then(|rest| rest.split("\n}").next())
        .unwrap_or_default();
    assert_eq!(
        (
            shell.contains("check_in"),
            shell.contains("  on?:"),
            shell.contains("command:")
        ),
        (false, false, true),
        "{shell}"
    );
}

#[test]
fn the_types_are_stable() {
    insta::assert_snapshot!(typescript());
}

#[test]
fn an_answer_template_follows_the_result_its_call_gives() {
    let answer = json!({ "type": "object", "required": ["outcome"],
                         "properties": { "outcome": { "enum": ["none", "rule"] },
                                         "notes": { "type": "array" } } });
    assert_eq!(
        [
            template(BlockName::Shell, &json!({ "command": "true" })),
            template(
                BlockName::Agent,
                &json!({ "profile": "judge", "prompt": "Rule." })
            ),
            template(
                BlockName::Agent,
                &json!({ "profile": "judge", "prompt": "Rule.", "schema": answer })
            ),
            template(
                BlockName::Team,
                &json!({ "team": "feature", "prompt": "Build." })
            ),
            template(
                BlockName::Watch,
                &json!({ "when": "pr=merged || ci[main]=failed" })
            ),
            template(BlockName::Schedule, &json!({ "name": "triage" })),
            template(BlockName::Now, &json!({})),
        ],
        [
            json!({ "code": 0, "stdout": "", "output": "" }),
            json!({ "report": "", "handle": "" }),
            json!({ "outcome": "none", "notes": [] }),
            json!({ "report": "" }),
            json!({ "readings": { "ci[main]": null, "pr": null } }),
            json!({ "id": "" }),
            json!(0),
        ]
    );
}

#[test]
fn a_block_that_is_not_an_object_is_refused_before_its_fields_are_read() {
    let positional = json!(["true", null, null, "fail", null, null, null]);
    assert_eq!(
        [
            parse::<Shell>(positional.clone(), Caller::Script).err(),
            parse::<Shell>(positional, Caller::Tool).err(),
            parse::<Watch>(json!(["pr=merged"]), Caller::Script).err(),
            parse::<Agent>(json!("judge"), Caller::Script).err(),
        ],
        [
            Some("a `shell` block is one object of named fields".to_owned()),
            Some("a `shell` block is one object of named fields".to_owned()),
            Some("a `watch` block is one object of named fields".to_owned()),
            Some("a `agent` block is one object of named fields".to_owned()),
        ]
    );
}

#[test]
fn a_blank_name_cron_or_condition_is_refused() {
    let (_, booking, _) = minimal().remove(5);
    assert_eq!(
        [
            refusal::<Schedule>(with(booking.clone(), "name", json!(" ")), Caller::Script),
            refusal::<Schedule>(with(booking, "value", json!(" ")), Caller::Tool),
            refusal::<Watch>(json!({ "when": " " }), Caller::Script),
        ],
        [
            Some("`name` is blank".to_owned()),
            Some("`value` is blank".to_owned()),
            Some("`when` is blank".to_owned()),
        ]
    );
}

#[test]
fn the_types_print_no_value_a_script_is_always_refused() {
    let text = typescript();
    let workflow = text
        .split("interface Workflow ")
        .nth(1)
        .and_then(|rest| rest.split("\n}").next())
        .unwrap_or_default();
    assert!(
        workflow.contains("source: \"name\" | \"path\" | \"script\";"),
        "{workflow}"
    );
}
