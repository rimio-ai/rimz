//! The agy custom-agent file that carries a fresh launch's composed system prompt, launch
//! reminder, and tools. agy reads a plugin-shipped agent selected by `--agent <name>` once at
//! conversation start; its `tools` key is an allowlist, and a name agy does not register stops
//! agy before its first turn, so one mapping decides both the layout notices and the file.
//! Observed on agy 1.5.13.

use sha2::{Digest, Sha256};

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

/// Written only while the launch keeps native subagents on.
const SUBAGENT_TOOLS: [&str; 3] = ["invoke_subagent", "define_subagent", "manage_subagents"];

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
    let rest =
        format!("description: \"{description}\"\nmainAgent: true\ntools: [{tools}]\n---\n{body}");
    let hash = hex::encode(Sha256::digest(rest.as_bytes()));
    let name = format!("rimz-{label}-{}", &hash[..12]);
    Ok(AgentFile {
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
        let all_but_subagents: Vec<&str> = DEFAULT_TOOLS
            .iter()
            .copied()
            .filter(|name| !SUBAGENT_TOOLS.contains(name))
            .collect();
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
                    "edit_file",
                    "generate_image",
                ],
            ),
            (
                Some(&["Agent", "invoke_subagent", "Write"]),
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
