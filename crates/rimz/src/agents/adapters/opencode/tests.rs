use super::*;
use crate::agents::ToolEffect;

use crate::agents::testkit::{hook_lifecycle, hook_observation, hook_output};
use crate::transcript::{AskOption, AskQuestion};
use serde_json::json;

#[test]
fn deadline_context_reply_rides_the_plugin_tool_after_feed() {
    let (post, other) = crate::agents::testkit::deadline_context_replies("opencode", "tool_after");
    insta::assert_json_snapshot!(post, @r#"
    {
      "additional_context": "deadline context"
    }
    "#);
    insta::assert_json_snapshot!(other, @"null");
}

#[test]
fn mini_frontend_takes_only_a_leading_subcommand_and_exact_permission_flags() {
    let cases = [
        (&["mini"][..], Some("mini"), &[][..], false),
        (
            &["--auto", "mini", "--agent", "x"],
            Some("mini"),
            &["--agent", "x"],
            true,
        ),
        (&["mini", "--auto"], Some("mini"), &[], true),
        (&["--yolo", "mini"], Some("mini"), &[], true),
        (
            &["mini", "--dangerously-skip-permissions"],
            Some("mini"),
            &[],
            true,
        ),
        (&["--agent", "mini"], None, &["--agent", "mini"], false),
        (&["x", "mini"], None, &["x", "mini"], false),
        (&[], None, &[], false),
        (&["--auto"], None, &["--auto"], false),
        (
            &["mini", "--auto=true"],
            Some("mini"),
            &["--auto=true"],
            false,
        ),
    ];
    let actual: Vec<_> = cases
        .iter()
        .map(|(args, _, _, _)| {
            let mut args = args.iter().map(|arg| (*arg).to_owned()).collect();
            let frontend = OpencodeAdapter.take_frontend(&mut args);
            (frontend.subcommand, args, frontend.extension_auto_approve)
        })
        .collect();
    let expected: Vec<_> = cases
        .iter()
        .map(|(_, subcommand, args, auto)| {
            (
                *subcommand,
                args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
                *auto,
            )
        })
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn models_render_without_argv_and_restore_from_the_session() {
    let launch = OpencodeAdapter.spec().launch;
    assert_eq!(
        OpencodeAdapter
            .spec()
            .render_preset(&crate::agents::LaunchPreset {
                model: Some("opencode-go/deepseek-v4.1-flash".into()),
                ..Default::default()
            }),
        Ok(Vec::new())
    );
    assert!(launch.restores_session_model);
    assert!(
        launch
            .preset_arg_matcher(crate::agents::PresetField::Model)
            .is_none()
    );
}

#[test]
fn opencode_activity_filter_and_launch_commands_build() {
    for event in [
        "tool_after",
        "session_idle",
        "session_error",
        "SubagentStart",
    ] {
        assert!(
            hook_output(&OpencodeAdapter, event, &json!({ "session_id": "ses_1" }))
                .records_progress(),
            "{event} records progress"
        );
    }
    for event in ["permission_ask", "question_ask", "session_compacting"] {
        assert!(
            !hook_output(&OpencodeAdapter, event, &json!({ "session_id": "ses_1" }))
                .records_progress(),
            "{event} does not record progress"
        );
    }

    assert_eq!(
        OpencodeAdapter.resume_command("ses_123", Path::new("/tmp")),
        Some(vec![
            "opencode".to_owned(),
            "--session".to_owned(),
            "ses_123".to_owned(),
            "--standalone".to_owned(),
        ])
    );
    assert_eq!(OpencodeAdapter.spec().launch.fork_command("ses_123"), None);
    assert_eq!(
        OpencodeAdapter.launch_command(&[], None),
        Some(vec!["opencode".to_owned(), "--standalone".to_owned()])
    );
    assert_eq!(
        OpencodeAdapter.launch_command(&["--pure".to_owned()], Some("review this")),
        Some(vec![
            "opencode".to_owned(),
            "--standalone".to_owned(),
            "--pure".to_owned(),
            "--prompt".to_owned(),
            "review this".to_owned(),
        ])
    );
    assert_eq!(
        OpencodeAdapter
            .spec()
            .launch
            .permission_args(PermissionMode::Ask),
        Vec::<String>::new()
    );
    assert_eq!(
        OpencodeAdapter
            .spec()
            .launch
            .permission_args(PermissionMode::Auto),
        Vec::<String>::new()
    );
    assert_eq!(
        OpencodeAdapter
            .spec()
            .launch
            .permission_args(PermissionMode::Plan),
        Vec::<String>::new()
    );
    assert_eq!(
        OpencodeAdapter
            .spec()
            .launch
            .permission_args(PermissionMode::Yolo),
        vec!["--auto"]
    );
}

/// One OpenCode launch's final environment edit over `inherited`, serving no MCP server.
fn finalize(
    inherited: &BTreeMap<String, String>,
    env: &mut BTreeMap<String, String>,
    disable_native_subagents: bool,
    system_prompt_file: Option<&Path>,
    code_mode: Option<bool>,
    release_code_mode: bool,
) -> Vec<String> {
    OpencodeAdapter.finalize_launch_env(
        inherited,
        env,
        &mut std::collections::BTreeSet::new(),
        &LaunchEnvEdit {
            disable_native_subagents,
            system_prompt_file,
            code_mode,
            release_code_mode,
            mcp: None,
        },
    )
}

#[test]
fn composed_system_prompt_preserves_config_with_or_without_delegation() {
    let raw = r#"{/* JSONC */"theme":"dark","plugins":"untouched",
        "agents":{"review":{"system":"review"},"build":{"model":"custom"}}}"#;
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for disable_delegation in [false, true] {
        let raw = if disable_delegation {
            raw.replace("\"untouched\"", "[\"custom\"]")
        } else {
            raw.to_owned()
        };
        let inherited = BTreeMap::from([("OPENCODE_CONFIG_CONTENT".into(), raw)]);
        let mut env = BTreeMap::new();
        let warnings = finalize(
            &inherited,
            &mut env,
            disable_delegation,
            Some(Path::new("/runtime/prompt.md")),
            None,
            false,
        );
        actual.push((
            env.get("OPENCODE_CONFIG_CONTENT")
                .map(|raw| serde_json::from_str::<Value>(raw).unwrap()),
            warnings,
        ));
        let plugins = if disable_delegation {
            json!(["custom", "-opencode.tool.subagent"])
        } else {
            json!("untouched")
        };
        expected.push((
            Some(json!({"theme":"dark", "plugins":plugins,
            "agents":{"review":{"system":"review"},"build":{
                "model":"custom","system":"{file:/runtime/prompt.md}"
            }}})),
            Vec::<String>::new(),
        ));
    }
    assert_eq!(actual, expected);
}

#[test]
fn composed_system_prompt_replaces_inherited_system_with_one_warning() {
    let mut actual = Vec::new();
    for system in [
        json!("previous"),
        json!(""),
        json!(null),
        json!({"file":"old"}),
    ] {
        let inherited = BTreeMap::from([(
            "OPENCODE_CONFIG_CONTENT".into(),
            json!({"agents":{"build":{"system":system}}}).to_string(),
        )]);
        let mut env = BTreeMap::new();
        let warnings = finalize(
            &inherited,
            &mut env,
            false,
            Some(Path::new("/runtime/prompt.md")),
            None,
            false,
        );
        actual.push((
            env.get("OPENCODE_CONFIG_CONTENT")
                .map(|raw| serde_json::from_str::<Value>(raw).unwrap()),
            warnings,
        ));
    }
    let warning = "OPENCODE_CONFIG_CONTENT set agents.build.system; RimZ's composed system prompt \
                   replaced it for this launch; move that prompt into agents/opencode.md or a \
                   profile's system-prompt-file, or remove it from the variable";
    assert_eq!(
        actual,
        vec![
            (
                Some(json!({"agents":{"build":{"system":"{file:/runtime/prompt.md}"}}})),
                vec![warning.to_owned()],
            );
            4
        ]
    );
}

#[test]
fn malformed_prompt_and_delegation_config_warns_once_and_keeps_both_edits() {
    let mut actual = Vec::new();
    for (raw, member, shape, problem) in [
        ("not JSON", "plugins", "an array", "is not valid JSONC"),
        ("[]", "plugins", "an array", "is not a JSON object"),
        (
            r#"{"plugins":false}"#,
            "plugins",
            "an array",
            "sets plugins to a value that is not an array",
        ),
        (
            r#"{"agents":[]}"#,
            "agents",
            "an object",
            "sets agents to a value that is not an object",
        ),
        (
            r#"{"agents":{"build":null}}"#,
            "agents.build",
            "an object",
            "sets agents.build to a value that is not an object",
        ),
    ] {
        let inherited = BTreeMap::from([("OPENCODE_CONFIG_CONTENT".into(), raw.into())]);
        let mut env = BTreeMap::new();
        let warnings = finalize(
            &inherited,
            &mut env,
            true,
            Some(Path::new("/runtime/prompt.md")),
            None,
            false,
        );
        let expected_warning = format!(
            "OPENCODE_CONFIG_CONTENT {problem}; this launch ignored the configured value; \
             set it to a JSON object whose {member} is {shape}, or unset it"
        );
        actual.push((
            env.get("OPENCODE_CONFIG_CONTENT")
                .map(|raw| serde_json::from_str::<Value>(raw).unwrap()),
            warnings == [expected_warning],
        ));
    }
    assert_eq!(
        actual,
        vec![
            (
                Some(json!({"plugins":["-opencode.tool.subagent"],
            "agents":{"build":{"system":"{file:/runtime/prompt.md}"}}})),
                true
            );
            5
        ]
    );
}

#[test]
fn unmanaged_core_config_is_untouched_without_prompt_or_delegation() {
    let mut actual = Vec::new();
    for raw in [
        None,
        Some("not JSON"),
        Some(r#"{"agents":{"build":{"system":"keep"}}}"#),
    ] {
        let inherited = raw
            .map(|raw| BTreeMap::from([("OPENCODE_CONFIG_CONTENT".into(), raw.into())]))
            .unwrap_or_default();
        let mut env = BTreeMap::new();
        let warnings = finalize(&inherited, &mut env, false, None, None, false);
        actual.push((env.contains_key("OPENCODE_CONFIG_CONTENT"), warnings));
    }
    assert_eq!(actual, vec![(false, Vec::<String>::new()); 3]);
}

/// The composed core config and warnings of one OpenCode launch over an inherited value.
fn code_mode_config(
    raw: Option<&str>,
    code_mode: Option<bool>,
    release_code_mode: bool,
) -> (Option<Value>, Vec<String>) {
    let inherited = raw
        .map(|raw| BTreeMap::from([("OPENCODE_CONFIG_CONTENT".into(), raw.into())]))
        .unwrap_or_default();
    let mut env = BTreeMap::new();
    let warnings = finalize(
        &inherited,
        &mut env,
        false,
        None,
        code_mode,
        release_code_mode,
    );
    (
        env.get("OPENCODE_CONFIG_CONTENT")
            .map(|raw| serde_json::from_str(raw).unwrap()),
        warnings,
    )
}

#[test]
fn code_mode_on_turns_full_code_mode_on_keeping_inherited_config() {
    let actual = [
        None,
        Some(r#"{/* JSONC */"theme":"dark","experimental":{"other":1,},}"#),
        Some(r#"{"experimental":{"full_codemode":true}}"#),
    ]
    .map(|raw| code_mode_config(raw, Some(true), false));
    assert_eq!(
        actual,
        [
            (Some(json!({"experimental":{"full_codemode":true}})), vec![]),
            (
                Some(json!({"theme":"dark","experimental":{"other":1,"full_codemode":true}})),
                vec![]
            ),
            (Some(json!({"experimental":{"full_codemode":true}})), vec![]),
        ]
    );
}

#[test]
fn code_mode_on_warns_when_it_replaces_or_ignores_inherited_config() {
    let actual = [
        r#"{"theme":"dark","experimental":{"full_codemode":false}}"#,
        r#"{"theme":"dark","experimental":true}"#,
    ]
    .map(|raw| code_mode_config(Some(raw), Some(true), false));
    assert_eq!(
        actual,
        [
            (
                Some(json!({"theme":"dark","experimental":{"full_codemode":true}})),
                vec![
                    "OPENCODE_CONFIG_CONTENT set experimental.full_codemode to false; the \
                     profile's code-mode on replaced it with true for this launch; remove it \
                     from the variable, or turn code-mode off in the profile"
                        .to_owned()
                ]
            ),
            (
                Some(json!({"experimental":{"full_codemode":true}})),
                vec![
                    "OPENCODE_CONFIG_CONTENT sets experimental to a value that is not an \
                     object; this launch ignored the configured value; set it to a JSON object \
                     whose experimental is an object, or unset it"
                        .to_owned()
                ]
            ),
        ]
    );
}

#[test]
fn code_mode_off_forces_full_code_mode_off_over_inherited_config() {
    let actual = [
        None,
        Some(r#"{"theme":"dark","experimental":{"other":1}}"#),
        Some(r#"{"theme":"dark","experimental":{"full_codemode":true}}"#),
    ]
    .map(|raw| code_mode_config(raw, Some(false), false));
    assert_eq!(
        actual,
        [
            (
                Some(json!({"experimental":{"full_codemode":false}})),
                vec![]
            ),
            (
                Some(json!({"theme":"dark","experimental":{"other":1,"full_codemode":false}})),
                vec![]
            ),
            (
                Some(json!({"theme":"dark","experimental":{"full_codemode":false}})),
                vec![
                    "OPENCODE_CONFIG_CONTENT set experimental.full_codemode to true; the \
                     profile's code-mode off replaced it with false for this launch; remove it \
                     from the variable, or turn code-mode on in the profile"
                        .to_owned()
                ]
            ),
        ]
    );
}

#[test]
fn code_mode_unset_takes_out_only_the_member_an_outer_launch_wrote() {
    let on = r#"{"theme":"dark","experimental":{"other":1,"full_codemode":true}}"#;
    let actual = [
        (Some(on), true),
        // Without RimZ's marker the value is the user's own and passes through.
        (Some(on), false),
        // An outer launch's forced off is released as its on is.
        (Some(r#"{"experimental":{"full_codemode":false}}"#), true),
        (Some(r#"{"experimental":{"full_codemode":"yes"}}"#), true),
        (None, true),
    ]
    .map(|(raw, release)| code_mode_config(raw, None, release));
    assert_eq!(
        actual,
        [
            (
                Some(json!({"theme":"dark","experimental":{"other":1}})),
                vec![]
            ),
            (None, vec![]),
            (Some(json!({"experimental":{}})), vec![]),
            (None, vec![]),
            (None, vec![]),
        ]
    );

    // A value the launch env sets itself (trusted `[[agents]] env`) is the user's too.
    let inherited = BTreeMap::from([("OPENCODE_CONFIG_CONTENT".into(), on.into())]);
    let mut env = inherited.clone();
    let warnings = finalize(&inherited, &mut env, false, None, None, true);
    assert_eq!(
        (
            env.get("OPENCODE_CONFIG_CONTENT").map(String::as_str),
            warnings
        ),
        (Some(on), Vec::new())
    );
}

/// What one OpenCode launch that takes RimZ's MCP server writes over `inherited`: the composed
/// inline config, the RimZ plugin's per-tool loads, the variables it unsets, its warnings, and
/// each mark it reports unapplied.
#[derive(Debug, PartialEq)]
struct Served {
    config: Option<Value>,
    loads: Option<Value>,
    marker: Option<Value>,
    unset: std::collections::BTreeSet<String>,
    warnings: Vec<String>,
    unapplied: Vec<String>,
}

/// One OpenCode launch serving `served` (`None`: RimZ's server is off) with the `mcps` list,
/// its routine grant, and code mode, over `inherited` and the launch env `env`.
fn serve(
    inherited: &BTreeMap<String, String>,
    mut env: BTreeMap<String, String>,
    served: Option<&[crate::mcp::catalog::Capability]>,
    mcps: Option<&[&str]>,
    grant: bool,
    code_mode: Option<bool>,
) -> Served {
    let mcps: Option<Vec<crate::config::McpName>> =
        mcps.map(|names| names.iter().map(|name| name.parse().unwrap()).collect());
    let launch = McpLaunch {
        server: served.map(|only| McpServer {
            command: PathBuf::from("/room/rimz"),
            args: crate::mcp::catalog::serve_args(only),
            grant,
            env_names: Default::default(),
        }),
        mcps: mcps.as_deref(),
        granted: false,
        login_env: inherited,
        rendered_tools: None,
        code_mode,
        served: &[],
        always: None,
    };
    let mut unset = std::collections::BTreeSet::new();
    let warnings = OpencodeAdapter.finalize_launch_env(
        inherited,
        &mut env,
        &mut unset,
        &LaunchEnvEdit {
            disable_native_subagents: false,
            system_prompt_file: None,
            code_mode,
            release_code_mode: false,
            mcp: Some(&launch),
        },
    );
    let (mcp_warnings, unapplied) = OpencodeAdapter
        .mcp_server_args(
            &launch,
            (Path::new("/work"), Path::new("/work")),
            &mut Vec::new(),
            &mut None,
        )
        .unwrap();
    assert_eq!(mcp_warnings, Vec::<String>::new());
    let json = |key: &str| env.get(key).map(|raw| serde_json::from_str(raw).unwrap());
    Served {
        config: json("OPENCODE_CONFIG_CONTENT"),
        loads: json("RIMZ_LAUNCH_MCP_TOOLS"),
        marker: json("RIMZ_OPENCODE_MCP_CONFIG"),
        unset,
        warnings,
        unapplied: unapplied.iter().map(ToString::to_string).collect(),
    }
}

fn config_env(raw: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("OPENCODE_CONFIG_CONTENT".into(), raw.into())])
}

const LSP_WATCH: &[crate::mcp::catalog::Capability] = &[
    crate::mcp::catalog::Capability::Lsp,
    crate::mcp::catalog::Capability::Watch,
];

fn rimz_record(codemode: Option<bool>) -> Value {
    let mut record = json!({
        "type": "local",
        "command": ["/room/rimz", "mcp", "serve", "--only", "rimz-lsp", "--only", "rimz-watch"],
    });
    if let Some(codemode) = codemode {
        record["codemode"] = codemode.into();
    }
    record
}

#[test]
fn opencode_takes_rimz_server_through_its_inline_config() {
    assert!(OpencodeAdapter.takes_mcp_servers());
    assert!(!OpencodeAdapter.serves_mcp_from_launch_env());
}

/// The `rimz` record and the routine grant join the composed inline config beside every
/// member the user set, their own servers, policies, and permission rules included.
#[test]
fn rimz_record_and_grant_join_the_inline_config_keeping_the_users_members() {
    let user = json!({
        "theme": "dark",
        "mcp": {"servers": {"github": {"type": "local", "command": ["gh"]}}},
        "policies": [{"action": "provider.use", "resource": "x", "effect": "deny"}],
        "permissions": [{"action": "bash", "resource": "*", "effect": "ask"}],
    });
    // One rule per served tool, so no rule matches a user server's tool (`rimz_docs_x`).
    let grant: Vec<Value> = ["lsp", "watch", "tasks", "join", "stop", "guide"]
        .map(|tool| json!({"action": format!("rimz_{tool}"), "resource": "*", "effect": "allow"}))
        .into();
    let served = serve(
        &config_env(&user.to_string()),
        BTreeMap::new(),
        Some(LSP_WATCH),
        None,
        true,
        None,
    );
    let mut expected = user.clone();
    expected["mcp"]["servers"]["rimz"] = rimz_record(None);
    expected["permissions"]
        .as_array_mut()
        .unwrap()
        .extend(grant.clone());
    assert_eq!(served.config, Some(expected));
    assert_eq!(served.warnings, Vec::<String>::new());
    assert_eq!(served.loads, None);
    assert!(served.unset.contains("RIMZ_LAUNCH_MCP_TOOLS"));
    assert_eq!(
        served.marker,
        Some(json!({"server": rimz_record(None), "permissions": grant.clone()}))
    );

    // Without the grant RimZ writes no permission rule.
    let served = serve(
        &BTreeMap::new(),
        BTreeMap::new(),
        Some(LSP_WATCH),
        None,
        false,
        None,
    );
    assert_eq!(
        served.config,
        Some(json!({"mcp": {"servers": {"rimz": rimz_record(None)}}}))
    );

    // A malformed inherited value warns once and the launch still serves.
    let mut actual = Vec::new();
    for (raw, problem, member, shape) in [
        ("not JSON", "is not valid JSONC", "mcp", "an object"),
        ("[]", "is not a JSON object", "mcp", "an object"),
        (
            r#"{"mcp":[]}"#,
            "sets mcp to a value that is not an object",
            "mcp",
            "an object",
        ),
        (
            r#"{"mcp":{"servers":1}}"#,
            "sets mcp.servers to a value that is not an object",
            "mcp.servers",
            "an object",
        ),
        (
            r#"{"permissions":{}}"#,
            "sets permissions to a value that is not an array",
            "permissions",
            "an array",
        ),
    ] {
        let served = serve(
            &config_env(raw),
            BTreeMap::new(),
            Some(LSP_WATCH),
            None,
            true,
            None,
        );
        actual.push((
            served.config,
            served.warnings
                == [format!(
                    "OPENCODE_CONFIG_CONTENT {problem}; this launch ignored the configured \
                     value; set it to a JSON object whose {member} is {shape}, or unset it"
                )],
        ));
    }
    assert_eq!(
        actual,
        vec![
            (
                Some(
                    json!({"mcp": {"servers": {"rimz": rimz_record(None)}}, "permissions": grant})
                ),
                true
            );
            5
        ]
    );
}

/// A launch that serves nothing and writes no list leaves the inherited value, valid or not,
/// untouched, and drops an inherited plugin variable.
#[test]
fn a_launch_that_serves_nothing_leaves_the_inline_config_untouched() {
    for raw in [None, Some("not JSON"), Some(r#"{"theme":"dark"}"#)] {
        let mut inherited = raw.map(config_env).unwrap_or_default();
        inherited.insert("RIMZ_LAUNCH_MCP_TOOLS".into(), r#"{"lsp":"native"}"#.into());
        let served = serve(&inherited, BTreeMap::new(), None, None, true, None);
        assert_eq!(served.config, None, "{raw:?}");
        assert_eq!(served.loads, None, "{raw:?}");
        assert!(served.unset.contains("RIMZ_LAUNCH_MCP_TOOLS"), "{raw:?}");
        assert_eq!(served.warnings, Vec::<String>::new(), "{raw:?}");
        let served = serve(
            &inherited,
            BTreeMap::new(),
            None,
            Some(&["*", "github"]),
            true,
            None,
        );
        assert_eq!(served.config, None, "{raw:?}");
    }
}

/// Each cell of the mark mapping: the record's `codemode`, the plugin's per-tool loads, and the
/// marks reported unapplied, for each code mode.
#[test]
fn marks_map_to_the_record_and_the_plugins_tool_loads_per_code_mode() {
    let tools: Vec<&str> = crate::mcp::catalog::tools_for(LSP_WATCH)
        .map(|tool| tool.name)
        .collect();
    let every = |load: &str, except: &[&str]| {
        Some(Value::Object(
            tools
                .iter()
                .filter(|tool| !except.contains(tool))
                .map(|tool| ((*tool).to_owned(), Value::from(load)))
                .collect(),
        ))
    };
    let off = "code-mode off sends every RimZ tool native, and OpenCode has no tool search \
               outside `execute`";
    let unapplied = |entry: &str| format!("the mcps entry `{entry}` is not applied: {off}");
    let always = &["rimz:always", "rimz-lsp", "rimz-watch"][..];
    let lsp_always = &["rimz-lsp:always", "rimz-watch"][..];
    let lsp_deferred = &["rimz:always", "rimz-lsp:deferred", "rimz-watch"][..];
    let deferred = &["rimz:deferred", "rimz-lsp:deferred", "rimz-watch"][..];
    let unmarked = &["rimz-lsp", "rimz-watch"][..];
    // code-mode, the mcps list, then the record's `codemode`, the loads, and the unapplied marks.
    type Cell<'a> = (
        Option<bool>,
        &'a [&'a str],
        Option<bool>,
        Option<Value>,
        Vec<String>,
    );
    let cases: Vec<Cell<'_>> = vec![
        (None, always, Some(false), None, vec![]),
        (
            None,
            lsp_always,
            None,
            Some(json!({"lsp": "native"})),
            vec![],
        ),
        (
            None,
            lsp_deferred,
            Some(false),
            Some(json!({"lsp": "code"})),
            vec![],
        ),
        (None, deferred, None, None, vec![]),
        (None, unmarked, None, None, vec![]),
        (Some(true), always, None, every("pinned", &[]), vec![]),
        (
            Some(true),
            lsp_always,
            None,
            Some(json!({"lsp": "pinned"})),
            vec![],
        ),
        (
            Some(true),
            lsp_deferred,
            None,
            every("pinned", &["lsp"]),
            vec![],
        ),
        (Some(true), deferred, None, None, vec![]),
        (Some(true), unmarked, None, None, vec![]),
        (Some(false), always, Some(false), None, vec![]),
        (Some(false), lsp_always, Some(false), None, vec![]),
        (
            Some(false),
            lsp_deferred,
            Some(false),
            None,
            vec![unapplied("rimz-lsp:deferred")],
        ),
        (
            Some(false),
            deferred,
            Some(false),
            None,
            vec![unapplied("rimz:deferred"), unapplied("rimz-lsp:deferred")],
        ),
        (Some(false), unmarked, Some(false), None, vec![]),
    ];
    for (code_mode, mcps, codemode, loads, marks) in cases {
        let served = serve(
            &BTreeMap::new(),
            BTreeMap::new(),
            Some(LSP_WATCH),
            Some(mcps),
            false,
            code_mode,
        );
        let config = served.config.unwrap();
        assert_eq!(
            config["mcp"]["servers"]["rimz"],
            rimz_record(codemode),
            "{code_mode:?} {mcps:?}"
        );
        assert_eq!(
            config.pointer("/experimental/full_codemode"),
            code_mode.map(Value::from).as_ref(),
            "{code_mode:?} {mcps:?}"
        );
        assert_eq!(served.loads, loads, "{code_mode:?} {mcps:?}");
        assert_eq!(
            served.unset.contains("RIMZ_LAUNCH_MCP_TOOLS"),
            served.loads.is_none()
        );
        assert_eq!(served.unapplied, marks, "{code_mode:?} {mcps:?}");
        assert_eq!(served.warnings, Vec::<String>::new());
    }

    // A mark on one of the user's servers is never applied, and RimZ's marks wait for its server.
    let user = "the mcps entry `github:always` is not applied: OpenCode takes a load mode only \
                for RimZ's server and its capabilities";
    let served = serve(
        &BTreeMap::new(),
        BTreeMap::new(),
        Some(LSP_WATCH),
        Some(&["rimz-lsp", "rimz-watch", "github:always"]),
        false,
        None,
    );
    assert_eq!(served.unapplied, [user]);
    let served = serve(
        &BTreeMap::new(),
        BTreeMap::new(),
        None,
        Some(&["rimz-lsp:always", "github:always"]),
        false,
        None,
    );
    assert_eq!(
        served.unapplied,
        [
            "the mcps entry `rimz-lsp:always` is not applied: RimZ's MCP server is off for this \
             launch"
                .to_owned(),
            user.to_owned(),
        ]
    );
}

/// A list without `*` keeps RimZ's server and the user's servers it names through OpenCode
/// `policies`, ahead of the user's own, so a user statement there still decides (the last match
/// wins);
/// a list holding `*` writes none.
#[test]
fn an_mcps_list_without_a_star_writes_server_policies() {
    let deny = json!({"action": "integration.use", "resource": "mcp:*", "effect": "deny"});
    let allow = |name: &str| json!({"action": "integration.use", "resource": format!("mcp:{name}"), "effect": "allow"});
    let user = json!({"action": "provider.use", "resource": "x", "effect": "deny"});
    let user_deny =
        json!({"action": "integration.use", "resource": "mcp:github", "effect": "deny"});
    let inherited = config_env(&json!({"policies": [user.clone(), user_deny.clone()]}).to_string());
    let policies = |served: Option<&[crate::mcp::catalog::Capability]>, mcps: Option<&[&str]>| {
        serve(&inherited, BTreeMap::new(), served, mcps, false, None)
            .config
            .and_then(|config| config.get("policies").cloned())
    };
    assert_eq!(
        policies(Some(LSP_WATCH), Some(&["rimz-lsp", "rimz-watch", "github"])),
        Some(json!([
            deny,
            allow("rimz"),
            allow("github"),
            user,
            user_deny
        ]))
    );
    assert_eq!(
        policies(None, Some(&["github"])),
        Some(json!([deny, allow("github"), user, user_deny]))
    );
    assert_eq!(
        policies(None, Some(&[])),
        Some(json!([deny, user, user_deny]))
    );
    assert_eq!(
        policies(Some(LSP_WATCH), Some(&["*", "github"])),
        Some(json!([user, user_deny]))
    );
    assert_eq!(
        policies(Some(LSP_WATCH), None),
        Some(json!([user, user_deny]))
    );
    assert_eq!(policies(None, Some(&["*", "github"])), None);
}

/// A launch from an OpenCode agent's environment takes back the record, policies, and grant
/// rule its outer launch wrote, keeps the user's own members, and writes its own.
#[test]
fn a_nested_launch_takes_back_the_mcp_members_its_outer_launch_wrote() {
    let user = json!({
        "theme": "dark",
        "mcp": {"servers": {"github": {"type": "local", "command": ["gh"]}}},
        "policies": [{"action": "provider.use", "resource": "x", "effect": "deny"}],
        "permissions": [{"action": "bash", "resource": "*", "effect": "ask"}],
    });
    let outer = serve(
        &config_env(&user.to_string()),
        BTreeMap::new(),
        Some(LSP_WATCH),
        Some(&["rimz-lsp", "rimz-watch", "github"]),
        true,
        None,
    );
    let mut inherited = config_env(&outer.config.unwrap().to_string());
    inherited.insert(
        "RIMZ_OPENCODE_MCP_CONFIG".into(),
        outer.marker.clone().unwrap().to_string(),
    );

    let nested = serve(&inherited, BTreeMap::new(), None, None, true, None);
    assert_eq!(nested.config, Some(user.clone()));
    assert_eq!(nested.marker, None);
    assert!(nested.unset.contains("RIMZ_OPENCODE_MCP_CONFIG"));

    let nested = serve(
        &inherited,
        BTreeMap::new(),
        Some(LSP_WATCH),
        None,
        false,
        None,
    );
    let mut expected = user.clone();
    expected["mcp"]["servers"]["rimz"] = rimz_record(None);
    assert_eq!(nested.config, Some(expected));
    assert_eq!(nested.marker, Some(json!({"server": rimz_record(None)})));
    assert!(!nested.unset.contains("RIMZ_OPENCODE_MCP_CONFIG"));

    // Without the marker, or under a value the launch env sets itself, the members are the
    // user's own and pass through.
    let unmarked = config_env(&inherited["OPENCODE_CONFIG_CONTENT"]);
    assert_eq!(
        serve(&unmarked, BTreeMap::new(), None, None, true, None).config,
        None
    );
    let nested = serve(&inherited, unmarked.clone(), None, None, true, None);
    assert_eq!(
        nested.config,
        Some(serde_json::from_str(&unmarked["OPENCODE_CONFIG_CONTENT"]).unwrap())
    );

    // The outer launch's policies lead the list, so the take-back removes the first run that
    // matches them and leaves a user statement of the same text where the user put it.
    let deny = json!({"action": "integration.use", "resource": "mcp:*", "effect": "deny"});
    let user =
        json!({"policies": [{"action": "provider.use", "resource": "x", "effect": "deny"}, deny]});
    let outer = serve(
        &config_env(&user.to_string()),
        BTreeMap::new(),
        None,
        Some(&[]),
        false,
        None,
    );
    let mut inherited = config_env(&outer.config.unwrap().to_string());
    inherited.insert(
        "RIMZ_OPENCODE_MCP_CONFIG".into(),
        outer.marker.unwrap().to_string(),
    );
    assert_eq!(
        serve(&inherited, BTreeMap::new(), None, None, false, None).config,
        Some(user)
    );
}

#[test]
fn native_subagent_denial_preserves_opencode_config_and_appends_plugin_selector() {
    let selector = "-opencode.tool.subagent";
    let allow = json!({ "action": "other", "resource": "review", "effect": "allow" });
    let raw = format!(
        "{{/* JSONC */\"theme\":\"dark\",\"permissions\":[{allow},],\"plugins\":[\"custom\",],}}"
    );
    let mut actual = Vec::new();
    for value in [
        None,
        Some(raw.as_str()),
        Some(r#"{"plugins":["custom","-opencode.tool.subagent"]}"#),
    ] {
        let inherited = value
            .map(|value| BTreeMap::from([("OPENCODE_CONFIG_CONTENT".into(), value.into())]))
            .unwrap_or_default();
        let mut env = BTreeMap::new();
        let warning = finalize(&inherited, &mut env, true, None, None, false)
            .into_iter()
            .next();
        actual.push((
            serde_json::from_str::<Value>(&env["OPENCODE_CONFIG_CONTENT"]).unwrap(),
            warning,
        ));
    }
    assert_eq!(
        actual,
        vec![
            (json!({"plugins":[selector]}), None),
            (
                json!({"theme":"dark", "permissions":[allow], "plugins":["custom",selector]}),
                None
            ),
            (json!({"plugins":["custom",selector]}), None),
        ]
    );
}

#[test]
fn native_subagent_denial_warns_when_opencode_config_is_ignored() {
    let mut actual = Vec::new();
    for (raw, problem) in [
        ("not JSON", "not valid JSONC"),
        ("[]", "not a JSON object"),
        (r#"{"plugins":{}}"#, "not an array"),
    ] {
        let inherited = BTreeMap::from([("OPENCODE_CONFIG_CONTENT".into(), raw.into())]);
        let mut env = BTreeMap::new();
        let warning = finalize(&inherited, &mut env, true, None, None, false)
            .into_iter()
            .next()
            .unwrap_or_default();
        actual.push((
            serde_json::from_str::<Value>(&env["OPENCODE_CONFIG_CONTENT"]).unwrap(),
            [
                "OPENCODE_CONFIG_CONTENT",
                problem,
                "ignored the configured value",
                "JSON object",
                "plugins",
                "array",
                "unset",
            ]
            .iter()
            .all(|part| warning.contains(part)),
        ));
    }
    assert_eq!(
        actual,
        vec![(json!({"plugins":["-opencode.tool.subagent"]}), true); 3]
    );
}

#[test]
fn managed_launch_hides_tabs_preserving_other_cli_config() {
    let mut actual = Vec::new();
    for raw in [
        None,
        Some(""),
        Some(
            r#"{/* JSONC */"theme":"dark","tabs":{
                "mode":"on","layout":"vertical","enabled":true,},}"#,
        ),
        Some(r#"{"tabs":{"mode":"auto"}}"#),
        Some(r#"{"theme":"light"}"#),
    ] {
        let mut inherited = BTreeMap::from([("OPENCODE_CONFIG_CONTENT".into(), "keep".into())]);
        if let Some(raw) = raw {
            inherited.insert("OPENCODE_CLI_CONFIG_CONTENT".into(), raw.into());
        }
        let mut env = BTreeMap::new();
        let warning = finalize(&inherited, &mut env, false, None, None, false)
            .into_iter()
            .next();
        actual.push((
            env.get("OPENCODE_CLI_CONFIG_CONTENT")
                .map(|raw| serde_json::from_str::<Value>(raw).unwrap()),
            warning,
            env.contains_key("OPENCODE_CONFIG_CONTENT"),
        ));
    }
    assert_eq!(
        actual,
        vec![
            (Some(json!({"tabs":{"mode":"off"}})), None, false),
            (Some(json!({"tabs":{"mode":"off"}})), None, false),
            (
                Some(json!({"theme":"dark","tabs":{
                    "mode":"off","layout":"vertical","enabled":true
                }})),
                None,
                false,
            ),
            (Some(json!({"tabs":{"mode":"off"}})), None, false),
            (
                Some(json!({"theme":"light","tabs":{"mode":"off"}})),
                None,
                false
            ),
        ]
    );
}

#[test]
fn managed_launch_warns_when_opencode_cli_config_is_ignored() {
    let mut actual = Vec::new();
    for (raw, problem) in [
        ("not JSON", "not valid JSONC"),
        (" ", "not valid JSONC"),
        ("[]", "not a JSON object"),
        ("null", "not a JSON object"),
        ("42", "not a JSON object"),
        ("true", "not a JSON object"),
        (r#""config""#, "not a JSON object"),
        (r#"{"tabs":"off"}"#, "tabs to a value that is not an object"),
        (r#"{"tabs":null}"#, "tabs to a value that is not an object"),
        (r#"{"tabs":[]}"#, "tabs to a value that is not an object"),
        (r#"{"tabs":42}"#, "tabs to a value that is not an object"),
        (r#"{"tabs":false}"#, "tabs to a value that is not an object"),
    ] {
        let inherited = BTreeMap::from([("OPENCODE_CLI_CONFIG_CONTENT".into(), raw.into())]);
        let mut env = BTreeMap::new();
        let warning = finalize(&inherited, &mut env, false, None, None, false)
            .into_iter()
            .next()
            .unwrap_or_default();
        actual.push((
            env.get("OPENCODE_CLI_CONFIG_CONTENT")
                .map(|raw| serde_json::from_str::<Value>(raw).unwrap()),
            [
                "OPENCODE_CLI_CONFIG_CONTENT",
                problem,
                "ignored the configured value",
                "JSON object",
                "tabs",
                "unset",
            ]
            .iter()
            .all(|part| warning.contains(part)),
        ));
    }
    assert_eq!(
        actual,
        vec![(Some(json!({"tabs":{"mode":"off"}})), true); 12]
    );
}

#[test]
fn outdated_bundle_lists_changed_missing_and_legacy_files() {
    let dir = tempfile::tempdir().unwrap();
    let bundle = OpencodeBundle {
        directory: dir.path().join("plugins/rimz"),
        legacy: Some(dir.path().join("plugin/rimz.ts")),
    };
    assert!(bundle.outdated_files().is_empty());
    std::fs::create_dir_all(&bundle.directory).unwrap();
    let index = bundle.directory.join("index.ts");
    let tui = bundle.directory.join("tui.ts");
    std::fs::write(&index, PLUGIN_SOURCE).unwrap();
    std::fs::write(&tui, TUI_SOURCE).unwrap();
    assert!(bundle.outdated_files().is_empty());
    std::fs::write(&tui, "// _rimz_managed older client\n").unwrap();
    assert_eq!(bundle.outdated_files(), std::slice::from_ref(&tui));
    std::fs::remove_file(&tui).unwrap();
    assert_eq!(bundle.outdated_files(), std::slice::from_ref(&tui));
    std::fs::write(&tui, TUI_SOURCE).unwrap();
    let legacy = bundle.legacy.as_ref().unwrap();
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(legacy, "// _rimz_managed older plugin\n").unwrap();
    assert_eq!(bundle.outdated_files(), std::slice::from_ref(legacy));
    std::fs::write(&index, "// user's plugin\n").unwrap();
    assert!(bundle.outdated_files().is_empty());
}

#[test]
#[cfg(unix)]
fn install_blocker_refuses_only_old_opencode() {
    use std::os::unix::fs::PermissionsExt as _;
    for (banner, blocked) in [
        ("opencode v1.19.0", true),
        ("opencode v2.0.27", false),
        ("unknown", false),
    ] {
        // The version probe memoizes per path and file stamp, so a same-length
        // rewrite of one path within an mtime tick would read the last case's output.
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("opencode");
        std::fs::write(&binary, format!("#!/bin/sh\nprintf '%s\\n' '{banner}'\n")).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let reason = install_blocker_at(&binary);
        assert_eq!(reason.is_some(), blocked, "{banner}");
        if let Some(reason) = reason {
            assert_eq!(
                reason,
                format!(
                    "opencode 1.19.0 at {} is older than 2.0.0, the oldest release RimZ supports; run `opencode upgrade --major`",
                    binary.display()
                )
            );
        }
    }
}

#[test]
fn opencode_observes_lifecycle_enrichment_and_boundaries() {
    let mut snapshots = Vec::new();
    let mut expected = Vec::new();
    for (event, signal) in [
        ("client_idle", LifecycleSignal::Registered),
        (
            "client_running",
            LifecycleSignal::TurnStarted { turn_id: None },
        ),
    ] {
        for stamp in [
            Some(json!(true)),
            Some(json!(false)),
            Some(json!(null)),
            None,
        ] {
            let mut payload = json!({
                "session_id": "ses_1", "prompt": "not a submitted prompt", "plan_proposed": true
            });
            if let Some(stamp) = &stamp {
                payload["on_screen"] = stamp.clone();
            }
            let output = hook_output(&OpencodeAdapter, event, &payload);
            snapshots.push(output.lifecycle().map(|observation| {
                (
                    observation.signal.clone(),
                    observation.state_snapshot,
                    observation.on_screen,
                    observation.prompt.clone(),
                    output.final_message().map(ToOwned::to_owned),
                )
            }));
            expected.push(Some((
                signal.clone(),
                Some(true),
                (stamp == Some(json!(true))).then_some(true),
                None,
                None,
            )));
        }
    }
    let mut edges = Vec::new();
    for event in OPENCODE_HOOKS {
        let mut payload: Value = serde_json::from_str(event.test_payload).unwrap();
        payload["on_screen"] = json!(true);
        payload["state_snapshot"] = json!(true);
        let output = hook_output(&OpencodeAdapter, event.event, &payload);
        let observation = output.lifecycle().unwrap();
        payload.as_object_mut().unwrap().remove("on_screen");
        edges.push((
            observation.on_screen,
            observation.state_snapshot,
            hook_lifecycle(&OpencodeAdapter, event.event, &payload).on_screen,
        ));
    }
    assert_eq!(
        (snapshots, edges),
        (
            expected,
            vec![(Some(true), None, None); OPENCODE_HOOKS.len()]
        ),
        "only client arrivals are snapshots; every stamped edge keeps its meaning"
    );

    let registered = hook_lifecycle(
        &OpencodeAdapter,
        "session_created",
        &json!({
            "session_id": "ses_1",
            "cwd": "/home/u/repo",
            "model": "claude-sonnet-4.5",
            "effort": "xhigh",
            "input_tokens": 100,
            "cache_write_input_tokens": 40,
            "cache_read_input_tokens": 30,
            "output_tokens": 20,
            "total_tokens": 190
        }),
    );
    assert_eq!(registered.agent_id.as_deref(), Some("ses_1"));
    assert_eq!(registered.signal, LifecycleSignal::Registered);
    assert_eq!(registered.worktree_path.as_deref(), Some("/home/u/repo"));
    assert_eq!(
        registered.launch.model.as_deref(),
        Some("claude-sonnet-4.5")
    );
    assert_eq!(registered.launch.effort.as_deref(), Some("xhigh"));
    assert_eq!(registered.usage.context_window, Some(200_000));
    assert_eq!(registered.usage.context_capacity, None);
    assert_eq!(registered.usage.fresh_input_tokens, Some(100));
    assert_eq!(registered.usage.cache_write_input_tokens, Some(40));
    assert_eq!(registered.usage.cache_read_input_tokens, Some(30));
    assert_eq!(registered.usage.output_tokens, Some(20));
    assert_eq!(registered.usage.total_tokens, Some(190));

    // A non-Claude session has no local fallback window. The plugin stamps
    // the usable window and published total from the same catalog entry.
    let catalog_window = hook_lifecycle(
        &OpencodeAdapter,
        "chat_message",
        &json!({
            "session_id": "ses_2",
            "model": "gpt-5.5",
            "provider_id": "openai",
            "context_window": 200_000,
            "context_capacity": 400_000
        }),
    );
    assert_eq!(catalog_window.usage.context_window, Some(200_000));
    assert_eq!(catalog_window.usage.context_capacity, Some(400_000));
    let older_plugin = hook_lifecycle(
        &OpencodeAdapter,
        "chat_message",
        &json!({ "session_id": "ses_2", "context_window": 200_000 }),
    );
    assert_eq!(older_plugin.usage.context_capacity, None);
    // Without a stamped window, a non-Claude model stays unknown (Claude-only fallback).
    let unknown_window = hook_lifecycle(
        &OpencodeAdapter,
        "chat_message",
        &json!({ "session_id": "ses_2", "model": "gpt-5.5", "provider_id": "openai" }),
    );
    assert_eq!(unknown_window.usage.context_window, None);

    let prompt = hook_lifecycle(
        &OpencodeAdapter,
        "chat_message",
        &json!({ "session_id": "ses_1", "prompt": "  fix auth  " }),
    );
    assert_eq!(
        prompt.signal,
        LifecycleSignal::TurnStarted { turn_id: None }
    );
    assert_eq!(prompt.prompt.as_deref(), Some("fix auth"));
    assert_eq!(prompt.task.as_deref(), Some("fix auth"));

    let injected = hook_lifecycle(
        &OpencodeAdapter,
        "chat_message",
        &json!({ "session_id": "ses_1", "prompt": "<system-reminder>noise" }),
    );
    assert_eq!(injected.prompt, None);
    assert_eq!(injected.task, None);

    let idle = hook_lifecycle(
        &OpencodeAdapter,
        "session_idle",
        &json!({ "session_id": "ses_1" }),
    );
    assert_eq!(
        idle.signal,
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        }
    );
    let proposed_plan = hook_lifecycle(
        &OpencodeAdapter,
        "session_idle",
        &json!({ "session_id": "ses_1", "plan_proposed": true }),
    );
    assert_eq!(
        proposed_plan.signal,
        LifecycleSignal::AwaitingInput {
            extra_questions: 0,
            kind: AskKind::PlanApproval,
            ask_id: None,
            detail: None,
            native_key: None,
        }
    );
    let error = hook_lifecycle(
        &OpencodeAdapter,
        "session_error",
        &json!({ "session_id": "ses_1", "error_message": "boom" }),
    );
    assert_eq!(
        error.signal,
        LifecycleSignal::TurnEnded {
            errored: true,
            parked_on_background: false,
            turn_id: None,
        }
    );

    let ended_decoded = hook_output(
        &OpencodeAdapter,
        "session_ended",
        &json!({ "session_id": "ses_1", "reason": "deleted" }),
    );
    let ended = ended_decoded.lifecycle().cloned().expect("end observation");
    assert_eq!(ended.signal, LifecycleSignal::Ended);
    assert!(ended_decoded.ends_session());
    assert!(
        !hook_output(
            &OpencodeAdapter,
            "session_error",
            &json!({ "session_id": "ses_1" })
        )
        .ends_session()
    );
}

#[test]
fn opencode_observes_rich_context_from_plugin_envelopes() {
    let output = hook_output(
        &OpencodeAdapter,
        "session_updated",
        &json!({
            "session_id": "ses_1",
            "session_name": "Fix OpenCode metadata",
            "model_display_name": "GPT-5.5 Codex",
            "agent_version": "1.18.23"
        }),
    );
    let observation = output.observed_context().expect("rich context");
    assert_eq!(observation.agent_id.as_str(), "ses_1");
    assert_eq!(observation.context.source, "opencode");
    assert_eq!(
        observation.context.session_name.as_deref(),
        Some("Fix OpenCode metadata")
    );
    assert_eq!(
        observation.context.model_display_name.as_deref(),
        Some("GPT-5.5 Codex")
    );
    assert_eq!(
        observation.context.agent_version.as_deref(),
        Some("1.18.23")
    );
    assert!(output.lifecycle().is_none());

    assert!(
        hook_output(
            &OpencodeAdapter,
            "session_idle",
            &json!({ "session_id": "ses_1" })
        )
        .observed_context()
        .is_none()
    );
}

#[test]
fn opencode_observes_native_questions() {
    let observed = hook_lifecycle(
        &OpencodeAdapter,
        "question_ask",
        &json!({ "session_id": "ses_1", "title": "Which database?" }),
    );
    assert_eq!(
        observed.signal,
        LifecycleSignal::AwaitingInput {
            extra_questions: 0,
            kind: AskKind::Question,
            ask_id: None,
            detail: Some("Which database?".to_owned()),
            native_key: None,
        }
    );
}

#[test]
fn taken_question_summary_overrides_title_and_counts_before_cap() {
    let mut output = hook_output(
        &OpencodeAdapter,
        "question_ask",
        &json!({
            "session_id": "ses_1", "title": "Questions",
            "questions": [
                {"question": "First question?\nMore context"},
                {"question": "Second?"}, {"question": "Third?"},
                {"question": "Fourth?"}, {"question": "Fifth?"}, {"question": "Sixth?"},
                {"question": "  "}
            ]
        }),
    );
    assert_eq!(output.questions().len(), 4);
    let LifecycleSignal::AwaitingInput {
        detail,
        extra_questions,
        ..
    } = output.take_lifecycle().unwrap().signal
    else {
        panic!("question wait")
    };
    assert_eq!(
        (detail.as_deref(), extra_questions),
        (Some("First question?"), 5)
    );
}

#[test]
fn opencode_parses_structured_native_questions() {
    let questions = hook_output(
        &OpencodeAdapter,
        "question_ask",
        &json!({
            "questions": [
                {
                    "question": " Which database? ",
                    "header": "Database",
                    "options": [
                        { "label": " Postgres ", "description": "Relational" },
                        { "label": "", "description": "ignored" }
                    ],
                    "multiple": true,
                    "custom": true
                },
                { "question": "  ", "options": [] }
            ]
        }),
    )
    .questions()
    .to_vec();
    assert_eq!(
        questions,
        vec![AskQuestion {
            question: "Which database?".to_owned(),
            options: vec![AskOption {
                label: "Postgres".to_owned(),
                description: Some("Relational".to_owned()),
                caution: None,
            }],
            multi_select: true,
            has_option_previews: false,
        }]
    );
    assert!(
        hook_output(
            &OpencodeAdapter,
            "question_ask",
            &json!({ "questions": "malformed" })
        )
        .questions()
        .to_vec()
        .is_empty()
    );
    assert!(
        hook_output(
            &OpencodeAdapter,
            "permission_ask",
            &json!({ "questions": [] })
        )
        .questions()
        .to_vec()
        .is_empty()
    );
}

#[test]
fn opencode_caps_normalized_questions_after_filtering() {
    let questions = hook_output(
        &OpencodeAdapter,
        "question_ask",
        &json!({
            "questions": [
                {"question": "one"},
                {"question": "   "},
                {"question": "two"},
                {"question": "three"},
                {"question": "four"},
                {"question": "five"}
            ]
        }),
    )
    .questions()
    .iter()
    .map(|question| question.question.clone())
    .collect::<Vec<_>>();
    assert_eq!(questions, ["one", "two", "three", "four"]);
}

#[test]
fn opencode_records_native_permission_and_question_answers() {
    assert_eq!(
        hook_output(
            &OpencodeAdapter,
            "permission_replied",
            &json!({ "session_id": "ses_1", "reply": "always" })
        )
        .native_answers()
        .map(<[_]>::to_vec),
        Some(vec![AskAnswer {
            question: None,
            chosen: vec!["always".to_owned()],
            note: None,
        }])
    );
    assert_eq!(
        hook_output(
            &OpencodeAdapter,
            "question_replied",
            &json!({
                "session_id": "ses_1",
                "answers": [["Postgres"], ["Fast", "Safe"]]
            })
        )
        .native_answers()
        .map(<[_]>::to_vec),
        Some(vec![
            AskAnswer {
                question: None,
                chosen: vec!["Postgres".to_owned()],
                note: None,
            },
            AskAnswer {
                question: None,
                chosen: vec!["Fast".to_owned(), "Safe".to_owned()],
                note: None,
            },
        ])
    );
    assert_eq!(
        hook_output(
            &OpencodeAdapter,
            "question_rejected",
            &json!({ "session_id": "ses_1" })
        )
        .native_answers()
        .map(<[_]>::to_vec),
        Some(vec![AskAnswer {
            question: None,
            chosen: vec!["(rejected)".to_owned()],
            note: None,
        }])
    );
    assert!(
        hook_output(
            &OpencodeAdapter,
            "question_replied",
            &json!({ "answers": [] })
        )
        .native_answers()
        .map(<[_]>::to_vec)
        .is_none()
    );
}

#[test]
fn opencode_asks_carry_native_request_ids() {
    let actual = ["question_ask", "permission_ask"].map(|event| {
        hook_lifecycle(
            &OpencodeAdapter,
            event,
            &json!({"session_id":"ses_1","request_id":"request-1"}),
        )
        .signal
    });
    assert_eq!(
        actual,
        [AskKind::Question, AskKind::Permission].map(|kind| {
            LifecycleSignal::AwaitingInput {
                extra_questions: 0,
                kind,
                ask_id: None,
                detail: None,
                native_key: Some("request-1".to_owned()),
            }
        })
    );
}

#[test]
fn opencode_replies_resolve_their_native_request_ids() {
    let actual = [
        "permission_replied",
        "question_replied",
        "question_rejected",
    ]
    .map(|event| {
        hook_lifecycle(
            &OpencodeAdapter,
            event,
            &json!({"session_id":"ses_1","request_id":"request-1"}),
        )
        .signal
    });
    assert_eq!(
        actual.to_vec(),
        vec![
            LifecycleSignal::AskResolved {
                native_key: Some("request-1".to_owned()),
            };
            3
        ]
    );
}

#[test]
fn opencode_tool_compaction_subagent_and_unknown_events_map_cleanly() {
    for (tool_name, expected) in [
        (
            "edit",
            Some(LifecycleSignal::ToolUsed {
                effect: ToolEffect::Editing,
                name: Some("edit".to_owned()),
                native_key: None,
                turn_id: None,
            }),
        ),
        (
            "bash",
            Some(LifecycleSignal::ToolUsed {
                effect: ToolEffect::Mutating,
                name: Some("bash".to_owned()),
                native_key: None,
                turn_id: None,
            }),
        ),
        (
            "read",
            Some(LifecycleSignal::ToolUsed {
                effect: ToolEffect::Observed,
                name: Some("read".to_owned()),
                native_key: None,
                turn_id: None,
            }),
        ),
    ] {
        let observed = hook_observation(
            &OpencodeAdapter,
            "tool_after",
            &json!({ "session_id": "ses_1", "tool_name": tool_name, "request_id": "request-1" }),
        );
        assert_eq!(observed.map(|obs| obs.signal), expected, "{tool_name}");
    }

    let replies = [
        "permission_replied",
        "question_replied",
        "question_rejected",
    ]
    .map(|event| hook_lifecycle(&OpencodeAdapter, event, &json!({ "session_id": "ses_1" })).signal);
    assert_eq!(
        replies.to_vec(),
        vec![LifecycleSignal::AskResolved { native_key: None }; 3]
    );

    let compacting = hook_lifecycle(
        &OpencodeAdapter,
        "session_compacting",
        &json!({ "session_id": "ses_1" }),
    );
    assert_eq!(compacting.signal, LifecycleSignal::Compacting);
    let compacted = hook_lifecycle(
        &OpencodeAdapter,
        "session_compacted",
        &json!({ "session_id": "ses_1" }),
    );
    assert_eq!(
        compacted.signal,
        LifecycleSignal::CompactionEnded {
            auto: None,
            failed: false,
            echo: false,
        }
    );

    let failed = hook_lifecycle(
        &OpencodeAdapter,
        "session_compacted",
        &json!({ "session_id": "ses_1", "is_error": true }),
    );
    assert_eq!(
        failed.signal,
        LifecycleSignal::CompactionEnded {
            auto: None,
            failed: true,
            echo: false,
        }
    );

    let child = hook_lifecycle(
        &OpencodeAdapter,
        "SubagentStart",
        &json!({
            "session_id": "ses_child",
            "parent_session_id": "ses_parent",
            "prompt": "review auth",
            "model": "claude-sonnet-4-5"
        }),
    );
    assert_eq!(child.agent_id.as_deref(), Some("ses_child"));
    assert_eq!(child.parent_agent_id.as_deref(), Some("ses_parent"));
    assert_eq!(child.signal, LifecycleSignal::SubagentStarted);
    assert_eq!(child.task.as_deref(), Some("review auth"));
    assert_eq!(child.launch.model.as_deref(), Some("claude-sonnet-4-5"));

    let child_stopped = hook_lifecycle(
        &OpencodeAdapter,
        "SubagentStop",
        &json!({
            "session_id": "ses_child",
            "parent_session_id": "ses_parent",
            "is_error": true
        }),
    );
    assert_eq!(
        child_stopped.signal,
        LifecycleSignal::SubagentStopped { errored: true }
    );
    assert_eq!(
        hook_observation(
            &OpencodeAdapter,
            "SubagentStart",
            &json!({ "session_id": "same", "parent_session_id": "same" })
        ),
        None
    );
    assert_eq!(
        hook_observation(&OpencodeAdapter, "bogus", &json!({})),
        None
    );
}

#[test]
fn neutral_decision_shape_is_pinned() {
    let rendered = hook_output(&OpencodeAdapter, "permission_ask", &Value::Null)
        .json_reply()
        .cloned();
    insta::assert_snapshot!(format!("{rendered:?}"), @"None");
}

#[test]
fn plugin_source_pins_rimz_wire_contract() {
    for (source, pid) in [(PLUGIN_SOURCE, "process.ppid"), (TUI_SOURCE, "process.pid")] {
        assert!(source.lines().next().unwrap().contains("_rimz_managed"));
        assert!(source.contains("\"hooks\", \"feed\", \"--source\", \"opencode\""));
        assert!(source.contains(&format!("RIMZ_AGENT_PID: String({pid})")));
        assert!(source.contains("RIMZ_BIN"));
        for hook in OPENCODE_HOOKS {
            if pid == "process.pid"
                && (hook.event.starts_with("Subagent") || hook.event == "session_created")
            {
                continue;
            }
            assert!(
                source.contains(hook.event),
                "plugin source missing {}",
                hook.event
            );
        }
    }
    // Only the server reads a feed's stdout, and only for the deadline rung of a tool end.
    assert!(TUI_SOURCE.contains("stdio: [\"pipe\", \"ignore\", \"ignore\"]"));
    assert!(PLUGIN_SOURCE.contains("stdio: [\"pipe\", reply ? \"pipe\" : \"ignore\", \"ignore\"]"));
    assert!(PLUGIN_SOURCE.contains(".additional_context"));
    // The per-tool loads ride one variable the server plugin claims once, applied to RimZ's
    // tools alone.
    assert!(PLUGIN_SOURCE.contains("process.env.RIMZ_LAUNCH_MCP_TOOLS"));
    assert!(PLUGIN_SOURCE.contains("delete process.env.RIMZ_LAUNCH_MCP_TOOLS"));
    assert!(PLUGIN_SOURCE.contains("tool.options?.namespace !== \"rimz\""));
    for load in ["\"native\"", "\"pinned\"", "\"code\""] {
        assert!(PLUGIN_SOURCE.contains(load), "plugin source missing {load}");
    }
    assert!(!TUI_SOURCE.contains("RIMZ_LAUNCH_MCP_TOOLS"));
    for event in ["client_idle", "client_running"] {
        assert!(TUI_SOURCE.contains(event), "client source missing {event}");
        assert!(
            !PLUGIN_SOURCE.contains(event),
            "server must not send {event}"
        );
    }
}
