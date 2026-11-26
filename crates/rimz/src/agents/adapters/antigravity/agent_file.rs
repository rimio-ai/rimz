//! The agy custom-agent file that carries a fresh launch's composed system prompt, launch
//! reminder, tools, and the limit on the account's MCP servers. agy reads a plugin-shipped
//! agent selected by `--agent <name>` once at conversation start, and a resume keeps what it
//! read. Its `tools` key is an allowlist, and a name agy does not register stops agy before its
//! first turn, so one mapping decides both the layout notices and the file. Its `mcpServers`
//! items limit the account's servers only beside `inheritMcp: false`. Observed on agy 1.5.13.

use sha2::{Digest, Sha256};

use crate::agents::capabilities::McpLaunch;
use crate::agents::tools::{ToolNotice, ToolSet};
use crate::agents::{AgentFile, AgentFileInput, AgentFileRefusal};

/// The agent selection flag.
pub(super) const AGENT_FLAG: &str = "--agent";

/// The tools an agentless agy session reports, each of which agy constructs under a
/// custom agent; a launch with no tools list gets these.
const DEFAULT_TOOLS: [&str; 13] = [
    "ask_question",
    "define_subagent",
    "edit_file",
    "invoke_subagent",
    "manage_subagents",
    "manage_task",
    "read_file",
    "run_command",
    "run_workflow",
    "schedule",
    "search_web",
    "send_message",
    "write_file",
];

/// Registered beyond the default set, so accepted when a list names it.
const EXTRA_TOOLS: [&str; 1] = ["generate_image"];

/// Written only while the launch keeps native subagents on. `run_workflow` fans work out to
/// subagents (agy 1.5.9), so it goes with them.
const SUBAGENT_TOOLS: [&str; 4] = [
    "invoke_subagent",
    "define_subagent",
    "manage_subagents",
    "run_workflow",
];

/// RimZ tool names beside the agy tools each grants. `Skill` is RimZ's switch for
/// skill callability and grants no agy tool.
const TOOL_NAMES: &[(&str, &[&str])] = &[
    ("Read", &["read_file"]),
    ("Grep", &["read_file"]),
    ("Glob", &["read_file"]),
    ("LS", &["read_file"]),
    ("Edit", &["edit_file"]),
    ("MultiEdit", &["edit_file"]),
    ("Write", &["write_file"]),
    ("Bash", &["run_command"]),
    ("WebSearch", &["search_web"]),
    ("WebFetch", &["search_web"]),
    ("AskUserQuestion", &["ask_question"]),
    ("Agent", &SUBAGENT_TOOLS),
    ("Skill", &[]),
];

/// The agy names a tools list grants, each once, and the list's names agy cannot take.
fn map_tools(bases: &[String]) -> (Vec<&'static str>, Vec<String>) {
    let mut names = Vec::new();
    let mut dropped = Vec::new();
    for base in bases {
        let granted = TOOL_NAMES
            .iter()
            .find_map(|(name, granted)| (name == base).then_some(*granted))
            .or_else(|| {
                DEFAULT_TOOLS
                    .iter()
                    .chain(&EXTRA_TOOLS)
                    .find(|native| *native == base)
                    .map(std::slice::from_ref)
            });
        let Some(granted) = granted else {
            dropped.push(base.clone());
            continue;
        };
        for name in granted {
            if !names.contains(name) {
                names.push(*name);
            }
        }
    }
    (names, dropped)
}

/// The layout notices for a tools list: names agy cannot take, then the restrictions
/// dropped from the names it can.
pub(super) fn render_definition_tools(tools: &ToolSet, _extra: &[&str]) -> Vec<ToolNotice> {
    let (_, dropped) = map_tools(tools.bases());
    let restricted: Vec<String> = tools
        .restrictions()
        .iter()
        .filter(|base| !dropped.contains(base))
        .cloned()
        .collect();
    let mut notices = Vec::new();
    if !dropped.is_empty() {
        notices.push(ToolNotice::Dropped(dropped));
    }
    if !restricted.is_empty() {
        notices.push(ToolNotice::Restrictions(restricted));
    }
    notices
}

/// The `tools` the file grants.
fn file_tools(tools: Option<&[String]>, native_subagents: bool) -> Vec<&'static str> {
    let mut names = tools.map_or_else(|| DEFAULT_TOOLS.to_vec(), |bases| map_tools(bases).0);
    if !native_subagents {
        names.retain(|name| !SUBAGENT_TOOLS.contains(name));
    }
    names
}

/// The `inheritMcp` and `mcpServers` lines: a name-only item for each of the user's servers a
/// limiting list keeps. RimZ's own server is the rimz plugin's entry, which every session of
/// the account loads, so the file never names it: on agy 1.5.14 `inheritMcp: false` with
/// name-only user items still loads and calls it. An agent that lists any item and omits
/// `inheritMcp` loses the account's servers, so the key is written whenever a line is; with no
/// limit the file names no MCP key and agy loads every server.
/// The list is one line of JSON flow style, the form agy 1.5.13 was observed to accept.
fn mcp_lines(mcp: &McpLaunch<'_>) -> String {
    if !crate::config::mcps_limit(mcp.mcps) {
        return String::new();
    }
    let items: Vec<String> = mcp
        .mcps
        .unwrap_or_default()
        .iter()
        .filter_map(crate::config::McpName::user_server)
        .map(|name| format!("{{\"name\": {}}}", serde_json::Value::from(name)))
        .collect();
    let mut lines = "inheritMcp: false\n".to_owned();
    if !items.is_empty() {
        lines.push_str(&format!("mcpServers: [{}]\n", items.join(", ")));
    }
    lines
}

/// The file name agy selects: `rimz-<label>-<hash>`, the hash over everything else the
/// file holds, so equal content shares one file and any change gets its own.
pub(super) fn plan(input: &AgentFileInput<'_>) -> Result<AgentFile, AgentFileRefusal> {
    let plugin = super::install::launch_plugin_dir(input.env)?;
    let label = input.profile.map_or_else(
        || "antigravity".to_owned(),
        |profile| {
            profile
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                        c
                    } else {
                        '-'
                    }
                })
                .collect()
        },
    );
    let description = match input.profile {
        Some(_) => format!("Written by RimZ for a launch of profile {label}"),
        None => "Written by RimZ for an antigravity launch".to_owned(),
    };
    let tools = file_tools(input.tools, input.native_subagents).join(", ");
    let body = if input.reminder.is_empty() {
        input.prompt.to_owned()
    } else {
        format!(
            "{}\n\n{}",
            input.prompt.trim_end_matches(['\r', '\n']),
            input.reminder
        )
    };
    let mcp = mcp_lines(&input.mcp);
    let rest = format!(
        "description: \"{description}\"\nmainAgent: true\ntools: [{tools}]\n{mcp}---\n{body}"
    );
    let hash = hex::encode(Sha256::digest(rest.as_bytes()));
    let name = format!("rimz-{label}-{}", &hash[..12]);
    Ok(AgentFile {
        unapplied: input.mcp.unapplied_marks(
            "Antigravity takes a load mode only for RimZ's capabilities",
            None,
        ),
        path: plugin.join("agents").join(format!("{name}.md")),
        bytes: format!("---\nname: {name}\n{rest}").into_bytes(),
        name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bases(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn tools_map_once_drop_unknown_names_and_follow_native_subagents() {
        let all_but_subagents = vec![
            "ask_question",
            "edit_file",
            "manage_task",
            "read_file",
            "run_command",
            "schedule",
            "search_web",
            "send_message",
            "write_file",
        ];
        type Case<'a> = (Option<&'a [&'a str]>, bool, Vec<&'a str>);
        let cases: [Case<'_>; 7] = [
            (None, true, DEFAULT_TOOLS.to_vec()),
            (None, false, all_but_subagents),
            (Some(&[]), true, vec![]),
            (
                Some(&["Read", "Grep", "Glob", "LS", "Bash", "Skill", "read_file"]),
                true,
                vec!["read_file", "run_command"],
            ),
            (
                Some(&["Agent", "TodoWrite", "Edit", "generate_image"]),
                true,
                vec![
                    "invoke_subagent",
                    "define_subagent",
                    "manage_subagents",
                    "run_workflow",
                    "edit_file",
                    "generate_image",
                ],
            ),
            (
                Some(&["Agent", "invoke_subagent", "run_workflow", "Write"]),
                false,
                vec!["write_file"],
            ),
            (
                Some(&["WebFetch", "WebSearch", "AskUserQuestion", "MultiEdit"]),
                true,
                vec!["search_web", "ask_question", "edit_file"],
            ),
        ];
        for (tools, native_subagents, expected) in cases {
            let tools = tools.map(bases);
            assert_eq!(
                file_tools(tools.as_deref(), native_subagents),
                expected,
                "{tools:?} native subagents {native_subagents}"
            );
        }
    }

    /// The two MCP keys for each limiting list, inside the hashed part, whether or not the
    /// launch serves RimZ's server, which the plugin's entry carries and the file never does;
    /// with no limit, the file is byte for byte what it was before the keys.
    #[test]
    fn mcp_lines_follow_the_server_and_the_list() {
        let home = tempfile::tempdir().unwrap();
        let plugin = home.path().join(".gemini/config/plugins/rimz");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(
            plugin.join("plugin.json"),
            r#"{"name":"rimz","author":"RimZ"}"#,
        )
        .unwrap();
        let env = std::collections::BTreeMap::from([(
            "HOME".to_owned(),
            home.path().display().to_string(),
        )]);
        let render = |server: bool, mcps: Option<&[&str]>| {
            let mcps: Option<Vec<crate::config::McpName>> =
                mcps.map(|names| names.iter().map(|name| name.parse().unwrap()).collect());
            let file = plan(&AgentFileInput {
                profile: None,
                prompt: "Body.",
                reminder: "",
                tools: Some(&[]),
                native_subagents: true,
                env: &env,
                mcp: crate::agents::capabilities::McpLaunch {
                    server: server.then(|| crate::agents::capabilities::McpServer {
                        command: "/room dir/rimz".into(),
                        args: vec!["mcp".to_owned(), "serve".to_owned()],
                        grant: true,
                        env_names: ["HOME".to_owned()].into(),
                    }),
                    mcps: mcps.as_deref(),
                    granted: false,
                    login_env: &env,
                    rendered_tools: None,
                    code_mode: None,
                    served: &[],
                },
            })
            .unwrap();
            let text = String::from_utf8(file.bytes).unwrap();
            let rest = text
                .strip_prefix(&format!("---\nname: {}\n", file.name))
                .unwrap()
                .to_owned();
            assert_eq!(
                file.name,
                format!(
                    "rimz-antigravity-{}",
                    &hex::encode(Sha256::digest(rest.as_bytes()))[..12]
                )
            );
            rest
        };
        let head = "description: \"Written by RimZ for an antigravity launch\"\nmainAgent: true\ntools: []\n";
        let cases: [(bool, Option<&[&str]>, String); 8] = [
            (false, None, String::new()),
            (false, Some(&["*", "github"]), String::new()),
            (true, None, String::new()),
            (true, Some(&["*"]), String::new()),
            (
                true,
                Some(&["rimz-lsp", "github", "notion"]),
                "inheritMcp: false\nmcpServers: [{\"name\": \"github\"}, {\"name\": \"notion\"}]\n"
                    .to_owned(),
            ),
            (true, Some(&["rimz-lsp"]), "inheritMcp: false\n".to_owned()),
            (
                false,
                Some(&["github"]),
                "inheritMcp: false\nmcpServers: [{\"name\": \"github\"}]\n".to_owned(),
            ),
            (false, Some(&[]), "inheritMcp: false\n".to_owned()),
        ];
        for (server, mcps, lines) in cases {
            assert_eq!(
                render(server, mcps),
                format!("{head}{lines}---\nBody."),
                "server {server} mcps {mcps:?}"
            );
        }
    }

    #[test]
    fn notices_name_dropped_and_restricted_tools() {
        let parsed = ToolSet::parse(&bases(&[
            "Bash(git *)",
            "TodoWrite",
            "Skill",
            "swarm(x)",
            "Read",
        ]))
        .unwrap();
        assert_eq!(
            render_definition_tools(&parsed, &[]),
            [
                ToolNotice::Dropped(vec!["TodoWrite".to_owned(), "swarm".to_owned()]),
                ToolNotice::Restrictions(vec!["Bash".to_owned()]),
            ]
        );
        let plain = ToolSet::parse(&bases(&["Read", "Skill"])).unwrap();
        assert!(render_definition_tools(&plain, &[]).is_empty());
    }
}
