//! Definition model defaults and provider-neutral tool vocabulary.

use super::definition::{CodeModeSwitch, DefinitionSpec, DefinitionTools};
use super::{all_definitions, spec_by_kind};

/// RimZ tool names beside Pi's, read forward by Pi's renderer and backward by parsing.
pub(super) const PI_TOOL_NAMES: &[(&str, &str)] = &[
    ("Bash", "bash"),
    ("Read", "read"),
    ("Edit", "edit"),
    ("MultiEdit", "edit"),
    ("Write", "write"),
    ("Grep", "grep"),
    ("Glob", "find"),
    ("LS", "ls"),
    ("AskUserQuestion", "ask_user_question"),
    ("powershell", "powershell"),
];

/// The RimZ spelling of a Pi-native tool name; any other name as written.
pub(crate) fn canonical_tool_name(name: &str) -> &str {
    PI_TOOL_NAMES
        .iter()
        .find_map(|&(canonical, native)| (name == native).then_some(canonical))
        .unwrap_or(name)
}

/// Parsed definition tools, rendered for the final launch provider.
///
/// Restriction text inside `Base(...)` is dropped at parse: only the bases and the `Agent(...)`
/// type list reach a launch, and `restrictions` only records which bases carried one, for the
/// notice that says it was dropped. A pack update's widening review compares those two lists
/// alone, so a restriction that changes grants nothing it can see.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolSet {
    bases: Vec<String>,
    agent_types: Vec<String>,
    restrictions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ToolNotice {
    Dropped(Vec<String>),
    Restrictions(Vec<String>),
    NotApplied,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ToolErr {
    #[error("invalid tool entry `{entry}`: expected a tool name")]
    Invalid { entry: String },
    #[error("tools are required for this agent kind")]
    Missing,
    #[error("unknown subagent type `{name}`; known types: {known}")]
    UnknownAgent { name: String, known: String },
}

impl ToolSet {
    pub(crate) fn parse(entries: &[String]) -> Result<Self, ToolErr> {
        let mut tools = Self {
            bases: Vec::new(),
            agent_types: Vec::new(),
            restrictions: Vec::new(),
        };
        for entry in entries {
            let entry_text = entry.trim();
            let (base, suffix) = match entry_text.split_once('(') {
                Some((base, tail)) => {
                    let Some(suffix) = tail.strip_suffix(')') else {
                        return Err(ToolErr::Invalid {
                            entry: entry.clone(),
                        });
                    };
                    if suffix.contains(['(', ')']) {
                        return Err(ToolErr::Invalid {
                            entry: entry.clone(),
                        });
                    }
                    (base.trim(), Some(suffix))
                }
                None => (entry_text, None),
            };
            if !base.starts_with(|c: char| c.is_ascii_alphabetic())
                || !base.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                return Err(ToolErr::Invalid {
                    entry: entry.clone(),
                });
            }
            let base = canonical_tool_name(base);
            if !tools.has(base) {
                tools.bases.push(base.to_owned());
            }
            if suffix.is_some() && !tools.restrictions.iter().any(|name| name == base) {
                tools.restrictions.push(base.to_owned());
            }
            if base == "Agent" {
                for name in suffix
                    .into_iter()
                    .flat_map(|s| s.split(','))
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    if !tools.agent_types.iter().any(|known| known == name) {
                        tools.agent_types.push(name.to_owned());
                    }
                }
            }
        }
        Ok(tools)
    }

    pub(crate) fn has(&self, base: &str) -> bool {
        self.bases.iter().any(|name| name == base)
    }

    pub fn bases(&self) -> &[String] {
        &self.bases
    }

    pub(crate) fn agent_types(&self) -> &[String] {
        &self.agent_types
    }

    pub(super) fn restrictions(&self) -> &[String] {
        &self.restrictions
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DefinitionDefaults {
    pub effort: Option<&'static str>,
}

fn definitions(kind: &str) -> DefinitionSpec {
    spec_by_kind(kind).map_or(DefinitionSpec::EMPTY, |spec| spec.launch.definitions)
}

pub fn definition_model_kind(model: &str) -> Option<&'static str> {
    all_definitions()
        .find(|definition| {
            definition
                .spec()
                .launch
                .definitions
                .models
                .iter()
                .any(|entry| entry.name == model || entry.id == model)
        })
        .or_else(|| {
            all_definitions().find(|definition| {
                definition
                    .spec()
                    .launch
                    .definitions
                    .prefixes
                    .iter()
                    .any(|prefix| model.starts_with(prefix))
            })
        })
        .map(|definition| definition.spec().kind)
}

pub(crate) fn definition_defaults(kind: &str, requested_model: Option<&str>) -> DefinitionDefaults {
    spec_by_kind(kind).map_or(DefinitionDefaults { effort: None }, |spec| {
        defaults_for_launch(&spec.launch, requested_model)
    })
}

fn defaults_for_launch(
    launch: &super::LaunchSpec,
    requested_model: Option<&str>,
) -> DefinitionDefaults {
    let spec = launch.definitions;
    let effort = requested_model
        .and_then(|model| {
            spec.models
                .iter()
                .find(|entry| entry.name == model)
                .and_then(|entry| entry.effort)
                .or_else(|| {
                    spec.models
                        .iter()
                        .find(|entry| entry.id == model)
                        .and_then(|entry| entry.effort)
                })
        })
        .or(spec.effort)
        .filter(|_| {
            launch
                .preset_arg_matcher(super::PresetField::Effort)
                .is_some()
        });
    DefinitionDefaults { effort }
}

pub(crate) fn tools_required(kind: &str) -> bool {
    matches!(definitions(kind).tools, DefinitionTools::Required(_))
}

/// Whether a supplied tools list applies to this kind and excludes `Skill`.
pub fn skills_lack_skill_tool(kind: &str, tools: Option<&ToolSet>) -> bool {
    tools.is_some_and(|tools| !tools.has("Skill"))
        && matches!(
            definitions(kind).tools,
            DefinitionTools::Required(_) | DefinitionTools::Optional(_)
        )
}

/// Whether `kind` runs models named by other providers' vocabularies.
pub fn runs_other_providers_models(kind: &str) -> bool {
    definitions(kind).runs_other_providers_models
}

pub(crate) fn render_tool_args(
    kind: &str,
    tools: Option<&ToolSet>,
) -> Result<Vec<String>, ToolErr> {
    match (definitions(kind).tools, tools) {
        (DefinitionTools::Required(render), tools) => render(tools.ok_or(ToolErr::Missing)?, &[]),
        (DefinitionTools::Optional(render), Some(tools)) => Ok(render(tools, &[]).0),
        _ => Ok(Vec::new()),
    }
}

/// A launch's tool argv, with the kind's code-mode tools or argv when `code_mode` is on, and
/// its off argv when the kind switches off by argv.
pub(crate) fn render_launch_tool_args(
    kind: &str,
    tools: Option<&ToolSet>,
    code_mode: Option<bool>,
) -> Result<(Vec<String>, Vec<ToolNotice>), ToolErr> {
    let switch = spec_by_kind(kind).map(|spec| spec.launch.presets.code_mode);
    let switch_args = match (switch, code_mode) {
        (Some(CodeModeSwitch::Args { on, .. }), Some(true)) => on,
        (Some(CodeModeSwitch::Args { off, .. }), Some(false)) => off,
        _ => &[],
    };
    let code_mode = code_mode == Some(true);
    let Some(tools) = tools else {
        let argv = match switch {
            Some(CodeModeSwitch::Supported { without_tools, .. }) if code_mode => without_tools,
            _ => switch_args,
        };
        return Ok((
            argv.iter().map(|arg| (*arg).to_owned()).collect(),
            Vec::new(),
        ));
    };
    let extra = switch.map_or(&[][..], |switch| switch.tools(code_mode));
    let (mut argv, notices) = match definitions(kind).tools {
        DefinitionTools::Required(render) => (render(tools, extra)?, Vec::new()),
        DefinitionTools::Optional(render) => render(tools, extra),
        DefinitionTools::Unsupported => (Vec::new(), vec![ToolNotice::NotApplied]),
        DefinitionTools::Ignored => (Vec::new(), Vec::new()),
    };
    argv.extend(switch_args.iter().map(|arg| (*arg).to_owned()));
    Ok((argv, notices))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools(entries: &[&str]) -> ToolSet {
        ToolSet::parse(&entries.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn skills_require_a_supplied_applied_tools_list_without_skill() {
        let without_skill = tools(&["Read", "Bash"]);
        let with_skill = tools(&["Skill", "Read"]);
        let actual: Vec<_> = ["claude", "codex", "pi", "opencode", "omp"]
            .into_iter()
            .map(|kind| {
                (
                    kind,
                    [None, Some(&without_skill), Some(&with_skill)]
                        .map(|tools| skills_lack_skill_tool(kind, tools)),
                )
            })
            .collect();
        assert_eq!(
            actual,
            [
                ("claude", [false, true, false]),
                ("codex", [false, true, false]),
                ("pi", [false, true, false]),
                ("opencode", [false, false, false]),
                ("omp", [false, false, false]),
            ]
        );
    }

    #[test]
    fn pi_tool_aliases_have_one_portable_spelling() {
        let parsed = tools(&[
            "read",
            "Read(path)",
            "bash",
            "Bash",
            "edit",
            "write",
            "grep",
            "find",
            "ls",
            "ask_user_question",
            "AskUserQuestion",
            "powershell",
        ]);
        assert_eq!(
            parsed.bases(),
            [
                "Read",
                "Bash",
                "Edit",
                "Write",
                "Grep",
                "Glob",
                "LS",
                "AskUserQuestion",
                "powershell"
            ]
        );
        assert_eq!(parsed.restrictions(), ["Read"]);
    }

    #[test]
    fn pi_reports_restrictions_only_for_mapped_tools() {
        let parsed = tools(&[
            "Read(path)",
            "Agent(Explore)",
            "WebFetch(url)",
            "Bash(git *)",
        ]);
        let (args, notices) = render_launch_tool_args("pi", Some(&parsed), None).unwrap();
        assert_eq!(args, ["--tools", "read,bash", "--no-skills"]);
        assert_eq!(
            notices,
            [
                ToolNotice::Dropped(vec!["Agent".into(), "WebFetch".into()]),
                ToolNotice::Restrictions(vec!["Read".into(), "Bash".into()]),
            ]
        );
    }

    #[test]
    fn pi_keeps_skill_only_beside_a_reader() {
        for (entries, expected_args, expected_notices) in [
            (vec!["Skill", "Read"], vec!["--tools", "read"], vec![]),
            (vec!["Bash", "Skill"], vec!["--tools", "bash"], vec![]),
            (
                vec!["Skill(foo)", "Read"],
                vec!["--tools", "read"],
                vec![ToolNotice::Restrictions(vec!["Skill".into()])],
            ),
            (
                vec!["Skill(foo)"],
                vec!["--no-tools"],
                vec![ToolNotice::Dropped(vec!["Skill".into()])],
            ),
        ] {
            let (args, notices) =
                render_launch_tool_args("pi", Some(&tools(&entries)), None).unwrap();
            assert_eq!(args, expected_args, "{entries:?}");
            assert_eq!(notices, expected_notices, "{entries:?}");
        }
    }

    #[test]
    fn code_mode_appends_each_declared_tool_once_and_off_renders_as_today() {
        let launch_in = |kind: &str, entries: Option<&[&str]>, code_mode: Option<bool>| {
            let parsed = entries.map(tools);
            render_launch_tool_args(kind, parsed.as_ref(), code_mode)
                .unwrap()
                .0
        };
        let launch = |kind: &str, entries: Option<&[&str]>, on: bool| {
            launch_in(kind, entries, on.then_some(true))
        };
        let read_bash: &[&str] = &["Read", "Bash"];
        // Kind, profile tools, argv with code mode on, argv with it off.
        type Case = (
            &'static str,
            Option<&'static [&'static str]>,
            &'static [&'static str],
            &'static [&'static str],
        );
        let cases: [Case; 8] = [
            (
                "claude",
                Some(read_bash),
                &["--tools", "Read,Bash,REPL,Poll"],
                &["--tools", "Read,Bash"],
            ),
            (
                "claude",
                Some(&["REPL", "Read"]),
                &["--tools", "REPL,Read,Poll"],
                &["--tools", "REPL,Read"],
            ),
            (
                "claude",
                Some(&[]),
                &["--tools", "REPL,Poll"],
                &["--tools", ""],
            ),
            ("claude", None, &[], &[]),
            ("pi", None, &["--tools", "+codemode"], &[]),
            (
                "pi",
                Some(read_bash),
                &["--tools", "read,bash,codemode", "--no-skills"],
                &["--tools", "read,bash", "--no-skills"],
            ),
            (
                "pi",
                Some(&["WebSearch"]),
                &["--tools", "codemode", "--no-skills"],
                &["--no-tools", "--no-skills"],
            ),
            (
                "pi",
                Some(&["codemode", "Read", "Skill"]),
                &["--tools", "read,codemode"],
                &["--tools", "read"],
            ),
        ];
        for (kind, entries, on, off) in cases {
            assert_eq!(launch(kind, entries, true), on, "{kind} {entries:?} on");
            assert_eq!(launch(kind, entries, false), off, "{kind} {entries:?} off");
            // Claude's off rides the env and Pi's the final argv, so the tools render as unset.
            assert_eq!(
                launch_in(kind, entries, Some(false)),
                off,
                "{kind} {entries:?}"
            );
        }
        // Codex takes the switch as config argv after any tools list.
        for entries in [Some(read_bash), None] {
            let mut on = launch("codex", entries, false);
            on.extend(["-c".to_owned(), "features.code_mode=true".to_owned()]);
            assert_eq!(launch("codex", entries, true), on, "codex {entries:?}");
            // Off renders its own setting at the same position, over a saved config value.
            let mut off = launch("codex", entries, false);
            off.extend(["-c".to_owned(), "features.code_mode=false".to_owned()]);
            assert_eq!(
                launch_in("codex", entries, Some(false)),
                off,
                "codex {entries:?}"
            );
        }
        for kind in ["opencode", "omp", "unregistered"] {
            for entries in [Some(read_bash), None] {
                assert_eq!(
                    launch(kind, entries, true),
                    launch(kind, entries, false),
                    "{kind} {entries:?}"
                );
            }
        }
    }

    #[test]
    fn claude_tools_and_generated_planner_parity() {
        let plain = tools(&[
            "Bash",
            "Read",
            "Grep",
            "Glob",
            "Edit",
            "Write",
            "AskUserQuestion",
            "LSP",
            "Skill",
        ]);
        // Literal args the planner agent launches with.
        let expected =
            shlex::split("--tools 'Bash,Read,Grep,Glob,Edit,Write,AskUserQuestion,LSP,Skill'")
                .unwrap();
        assert_eq!(render_tool_args("claude", Some(&plain)).unwrap(), expected);
        let named = tools(&["Agent(Explore, Plan)", "Bash", "Agent(Explore)"]);
        assert_eq!(
            render_tool_args("claude", Some(&named)).unwrap(),
            [
                "--disallowedTools",
                "Agent(general-purpose)",
                "Agent(statusline-setup)",
                "Agent(fork)",
                "--tools",
                "Agent,Bash"
            ]
        );
        let error = render_tool_args("claude", Some(&tools(&["Agent(unknown)"]))).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("known types: Explore, Plan, general-purpose, statusline-setup, fork")
        );
    }

    #[test]
    fn codex_tool_switches() {
        let ask = tools(&["AskUserQuestion", "Bash", "Skill"]);
        let expected = shlex::split(r#"--strict-config -c 'web_search="disabled"' -c agents.enabled=false -c features.goals=false -c features.multi_agent=false -c features.multi_agent_v2=false -c features.shell_snapshot=true -c features.shell_tool=true -c features.skill_mcp_dependency_install=false -c features.tool_call_mcp_elicitation=false -c features.browser_use=false -c features.browser_use_external=false -c features.computer_use=false -c features.in_app_browser=false -c features.image_generation=false -c features.tool_suggest=false -c features.memories=false -c features.default_mode_request_user_input=true -c tools.experimental_request_user_input.enabled=true -c skills.include_instructions=true"#).unwrap();
        assert_eq!(render_tool_args("codex", Some(&ask)).unwrap(), expected);
        let web = tools(&["Bash", "Read", "WebFetch", "Agent"]);
        let expected = shlex::split(r#"--strict-config -c 'web_search="cached"' -c agents.enabled=true -c features.goals=false -c features.multi_agent=true -c features.multi_agent_v2=true -c features.shell_snapshot=true -c features.shell_tool=true -c features.skill_mcp_dependency_install=false -c features.tool_call_mcp_elicitation=false -c features.browser_use=false -c features.browser_use_external=false -c features.computer_use=false -c features.in_app_browser=false -c features.image_generation=false -c features.tool_suggest=false -c features.memories=false -c features.default_mode_request_user_input=false -c tools.experimental_request_user_input.enabled=false -c skills.include_instructions=false"#).unwrap();
        assert_eq!(render_tool_args("codex", Some(&web)).unwrap(), expected);
    }

    #[test]
    fn generated_implementer_parity() {
        // Literal args the implementer agent launches with.
        let expected = shlex::split(r#"--strict-config -c 'web_search="disabled"' -c 'agents.enabled=false' -c 'features.goals=false' -c 'features.multi_agent=false' -c 'features.multi_agent_v2=false' -c 'features.shell_snapshot=true' -c 'features.shell_tool=true' -c 'features.skill_mcp_dependency_install=false' -c 'features.tool_call_mcp_elicitation=false' -c 'features.browser_use=false' -c 'features.browser_use_external=false' -c 'features.computer_use=false' -c 'features.in_app_browser=false' -c 'features.image_generation=false' -c 'features.tool_suggest=false' -c 'features.memories=false' -c 'features.default_mode_request_user_input=false' -c 'tools.experimental_request_user_input.enabled=false' -c 'skills.include_instructions=false'"#).unwrap();
        assert_eq!(
            render_tool_args(
                "codex",
                Some(&tools(&[
                    "Bash", "Read", "Grep", "Glob", "Edit", "Write", "LSP"
                ]))
            )
            .unwrap(),
            expected
        );
    }

    #[test]
    fn tool_vocabulary_and_support() {
        let parsed = tools(&[" Bash ", "Agent(Explore, Plan)", "Bash", "Agent(Plan)"]);
        assert_eq!(parsed.bases(), ["Bash", "Agent"]);
        assert_eq!(parsed.agent_types(), ["Explore", "Plan"]);
        assert!(parsed.has("Agent"));
        for entry in [
            "",
            " ",
            "--Bash",
            "Bash extra",
            "123",
            "Agent(Explore",
            "Bash)",
        ] {
            assert!(ToolSet::parse(&[entry.to_owned()]).is_err(), "{entry}");
        }
        for kind in ["claude", "codex"] {
            assert!(tools_required(kind));
            assert!(matches!(
                render_tool_args(kind, None),
                Err(ToolErr::Missing)
            ));
        }
        assert!(!tools_required("pi"));
        assert!(render_tool_args("pi", None).unwrap().is_empty());
        assert_eq!(
            render_tool_args("pi", Some(&parsed)).unwrap(),
            ["--tools", "bash", "--no-skills"]
        );
        for (entries, expected) in [
            (vec![], vec!["--no-tools", "--no-skills"]),
            (vec!["WebSearch"], vec!["--no-tools", "--no-skills"]),
            (vec!["Bash(git *)"], vec!["--tools", "bash", "--no-skills"]),
            (
                vec![
                    "Bash",
                    "Read",
                    "Edit",
                    "MultiEdit",
                    "Write",
                    "Grep",
                    "Glob",
                    "LS",
                    "AskUserQuestion",
                ],
                vec![
                    "--tools",
                    "bash,read,edit,write,grep,find,ls,ask_user_question",
                    "--no-skills",
                ],
            ),
            (
                vec!["Edit", "MultiEdit", "Read", "Bash", "read"],
                vec!["--tools", "edit,read,bash", "--no-skills"],
            ),
            (
                vec![
                    "read",
                    "bash",
                    "powershell",
                    "edit",
                    "write",
                    "grep",
                    "find",
                    "ls",
                    "ask_user_question",
                ],
                vec![
                    "--tools",
                    "read,bash,powershell,edit,write,grep,find,ls,ask_user_question",
                    "--no-skills",
                ],
            ),
            (
                vec![
                    "AskUserQuestion",
                    "Bash",
                    "Edit",
                    "Glob",
                    "Grep",
                    "Read",
                    "Skill",
                    "WebFetch",
                    "WebSearch",
                    "Write",
                ],
                vec![
                    "--tools",
                    "ask_user_question,bash,edit,find,grep,read,write",
                ],
            ),
        ] {
            assert_eq!(
                render_tool_args("pi", Some(&tools(&entries))).unwrap(),
                expected,
                "{entries:?}"
            );
        }
        assert!(render_tool_args("unregistered", Some(&parsed)).is_ok_and(|args| args.is_empty()));
        assert!(render_tool_args("unregistered", None).unwrap().is_empty());
    }

    #[test]
    fn model_catalog_and_defaults() {
        for (model, kind) in [
            ("fable", Some("claude")),
            ("gpt-6-astra", Some("codex")),
            ("astra", Some("codex")),
            ("claude-opus-4-6", Some("claude")),
            ("gpt-x", Some("codex")),
            ("custom", None),
        ] {
            assert_eq!(definition_model_kind(model), kind);
        }
        for (alias, id) in [
            ("astra", "gpt-6-astra"),
            ("sol", "gpt-6-sol"),
            ("luna", "gpt-6-luna"),
            ("terra", "gpt-5.6-terra"),
        ] {
            assert!(
                definitions("codex")
                    .models
                    .iter()
                    .any(|entry| entry.name == alias && entry.id == id)
            );
            assert_eq!(definition_model_kind(id), Some("codex"));
        }
        assert_eq!(
            definition_defaults("claude", Some("fable")),
            DefinitionDefaults {
                effort: Some("high")
            }
        );
        assert_eq!(
            definition_defaults("codex", Some("astra")),
            DefinitionDefaults {
                effort: Some("xhigh")
            }
        );
        assert_eq!(definition_defaults("pi", None).effort, Some("xhigh"));
        assert_eq!(
            definition_defaults("unregistered", None),
            DefinitionDefaults { effort: None }
        );
    }

    #[test]
    fn definition_effort_defaults_require_a_matcher() {
        let mut launch = super::super::LaunchSpec {
            definitions: DefinitionSpec {
                effort: Some("xhigh"),
                ..DefinitionSpec::EMPTY
            },
            ..super::super::LaunchSpec::EMPTY
        };
        let unsupported = defaults_for_launch(&launch, None);
        launch.presets.effort = Some(super::super::StaticPresetMatcher::Flag(&["--effort"]));
        let supported = defaults_for_launch(&launch, None);
        assert_eq!(
            [unsupported, supported],
            [
                DefinitionDefaults { effort: None },
                DefinitionDefaults {
                    effort: Some("xhigh"),
                },
            ]
        );
    }
}
