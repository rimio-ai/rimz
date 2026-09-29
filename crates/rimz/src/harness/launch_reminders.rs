//! One system reminder carrying team context, model identity, launch environment, sandbox view, subagent policy, and shared language servers.

use std::path::Path;

use super::launch::ExecRequest;
use super::launch_context::{self, escape_reminder_text};
use super::launch_env::LaunchEnv;
use super::subagent_policy::{self, SubagentCatalog};
use crate::agents::payload::{RimzBlock, wrap_rimz_block};
use crate::agents::{LaunchParams, model_display::display_model};

pub use super::launch_context::TeamReminder;

pub(super) struct LaunchReminders {
    pub env: Option<LaunchEnv>,
    /// The launched profile's `model-reminder`; on when unset or when the launch has no profile.
    pub model: bool,
    /// The launch runs inside the RimZ sandbox view: adds the sandbox reminder
    /// and switches off the provider's native command sandbox.
    pub sandbox: bool,
    pub lsp_configured: bool,
    pub lsp_servers: Vec<String>,
    pub subagent_catalog: Option<SubagentCatalog>,
    pub team: Option<TeamReminder>,
}

impl Default for LaunchReminders {
    fn default() -> Self {
        Self {
            env: None,
            model: true,
            sandbox: false,
            lsp_configured: false,
            lsp_servers: Vec::new(),
            subagent_catalog: None,
            team: None,
        }
    }
}

const SANDBOX_REMINDER_BODY: &str = concat!(
    "### Files\n\n",
    "This pane runs in a bubblewrap sandbox. Its `/tmp` belongs to the room: separate from the host's, removed when the room closes. The host state path stays reachable.\n\n",
    "- `/tmp/scratchpad/`: every temporary file you make. Private to you; every agent and subagent has its own.\n",
    "- `/tmp/shared/<task>/`: files another agent must read, in a subdirectory you name for the task.\n\n",
    "If your harness names its own scratchpad and allows `/tmp` only when asked, this is that ask: use `/tmp/scratchpad/` instead."
);

const HOST_SCRATCH_REMINDER_BODY: &str = concat!(
    "### Files\n\n",
    "- `$RIMZ_SCRATCH`: every temporary file you make. Private to you, removed when the room closes; every agent and subagent has its own.\n",
    "- `$RIMZ_SHARED/<task>/`: files another agent must read, in a subdirectory you name for the task.\n\n",
    "If your harness names its own scratchpad and allows another location only when asked, this is that ask: use `$RIMZ_SCRATCH` instead."
);

const SUBAGENT_REMINDER_BODY: &str = concat!(
    "### Subagents\n\n",
    "You are a subagent: a supervised child launched by another agent to ",
    "complete the task you were given. The task is scoped to this one run, so do the work ",
    "yourself rather than launching with Skill(rimz-agents, rimz-subagents, rimz-teams); ",
    "nothing above your caller supervises a run you start. Report the result: your caller ",
    "receives a completion report pointing to your final response after the fleet settles."
);

const SKILLS_REMINDER_BODY: &str = concat!(
    "### Skills\n\n",
    "When a skill's description matches the work in hand, invoke it, even when you know the ",
    "commands by heart: each skill is built for its one task and does it better than you would by hand."
);

pub(super) fn wrap(body: &str) -> String {
    wrap_rimz_block(RimzBlock::SystemReminder, body)
}

/// Child policy for adapters that require a user-prompt fallback.
pub fn subagent_reminder() -> String {
    wrap(SUBAGENT_REMINDER_BODY)
}

pub(super) fn render(request: &ExecRequest, reminders: &LaunchReminders, cwd: &Path) -> String {
    let mut paragraphs = Vec::new();
    let params = &request.identity.params;
    let team_context = reminders
        .team
        .as_ref()
        .filter(|_| !request.subagent)
        .and_then(|team| launch_context::team_launch_context(params, &request.action, team, cwd));
    if let Some(context) = &team_context {
        paragraphs.push(format!("### Team\n\n{}", launch_context::reminder(context)));
    } else if let Some(model) = reminders.model.then(|| model_fragment(params)).flatten() {
        paragraphs.push(model_line(params, &model));
    }
    if reminders.env.is_some() || !reminders.lsp_servers.is_empty() || team_context.is_some() {
        paragraphs.push(env_paragraph(
            reminders.env.as_ref(),
            cwd,
            &reminders.lsp_servers,
            team_context.as_ref(),
        ));
    }
    paragraphs.push(if reminders.sandbox {
        SANDBOX_REMINDER_BODY.to_owned()
    } else {
        HOST_SCRATCH_REMINDER_BODY.to_owned()
    });
    if request.subagent {
        paragraphs.push(SUBAGENT_REMINDER_BODY.to_owned());
    } else if let Some(catalog) = reminders.subagent_catalog.as_ref() {
        paragraphs.push(subagent_policy::reminder(catalog));
    }
    if request
        .skills
        .iter()
        .flatten()
        .any(|skill| skill.as_str().starts_with("rimz-"))
    {
        paragraphs.push(SKILLS_REMINDER_BODY.to_owned());
    }
    wrap(&paragraphs.join("\n\n"))
}

fn env_paragraph(
    env: Option<&LaunchEnv>,
    cwd: &Path,
    lsp_servers: &[String],
    team: Option<&launch_context::TeamLaunchContext>,
) -> String {
    let mut lines = vec!["### Environment\n".to_owned()];
    if env.is_some() || team.is_some() {
        lines.push(format!(
            "- cwd: {}",
            escape_reminder_text(&cwd.to_string_lossy())
        ));
    }
    if let Some(kind) = env
        .and_then(|env| env.shell.as_deref())
        .and_then(Path::file_name)
    {
        lines.push(format!(
            "- shell: {}",
            escape_reminder_text(&kind.to_string_lossy())
        ));
    }
    if !lsp_servers.is_empty() {
        lines.push(format!(
            "- lsp: {}, via Skill(rimz-lsp)",
            lsp_servers
                .iter()
                .map(|server| escape_reminder_text(server))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(files) = team.and_then(launch_context::files_block) {
        lines.push(format!("\n{files}"));
    }
    lines.join("\n")
}

/// `on <model>` when the launch names a model; none otherwise.
fn model_fragment(params: &LaunchParams) -> Option<String> {
    params
        .model
        .as_deref()
        .map(|model| format!("on {}", escape_reminder_text(&display_model(model))))
}

/// The standalone model line for a launch with no team paragraph to carry the fragment.
fn model_line(params: &LaunchParams, fragment: &str) -> String {
    match params.role.as_deref().or(params.profile.as_deref()) {
        Some(handle) => format!(
            "You are @{}, running {fragment}.",
            escape_reminder_text(handle)
        ),
        None => format!("You are running {fragment}."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Team;

    #[test]
    fn shared_lsp_reminder_is_in_environment_for_peers_and_children() {
        let mut request =
            ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
        let reminders = LaunchReminders {
            lsp_configured: true,
            lsp_servers: vec!["rust".to_owned(), "python".to_owned()],
            ..LaunchReminders::default()
        };
        for subagent in [false, true] {
            request.subagent = subagent;
            let text = render(&request, &reminders, Path::new("/checkout"));
            assert!(text.contains(
                "### Environment\n\n- lsp: rust, python, via Skill(rimz-lsp)\n\n### Files"
            ));
            if subagent {
                assert!(text.contains(SUBAGENT_REMINDER_BODY));
            }
            assert!(
                !render(
                    &request,
                    &LaunchReminders::default(),
                    Path::new("/checkout")
                )
                .contains("### Environment")
            );
        }
    }

    fn team_reminder(team: Team) -> TeamReminder {
        TeamReminder::new(team)
    }

    #[test]
    fn sandbox_full_catalog_rendering() {
        let mut request =
            ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
        request.identity.params.role = Some("brainstormer".to_owned());
        request.identity.params.model = Some("opus".to_owned());
        request.skills = Some(vec!["rimz-lsp".parse().unwrap()]);
        let reminders = LaunchReminders {
            sandbox: true,
            env: Some(LaunchEnv {
                shell: Some("/usr/bin/zsh".into()),
            }),
            lsp_servers: vec!["rust".to_owned(), "python".to_owned()],
            subagent_catalog: Some(SubagentCatalog::Available(vec![
                subagent_policy::SubagentProfile {
                    name: "explorer".to_owned(),
                    source: subagent_policy::SubagentProfileSource::Profile,
                    agent: None,
                    model: None,
                    effort: None,
                    description: Some("Finds files and traces code paths".to_owned()),
                },
            ])),
            ..Default::default()
        };
        assert_eq!(
            render(&request, &reminders, Path::new("/checkout")),
            r#"<system_reminder>
You are @brainstormer, running on Opus.

### Environment

- cwd: /checkout
- shell: zsh
- lsp: rust, python, via Skill(rimz-lsp)

### Files

This pane runs in a bubblewrap sandbox. Its `/tmp` belongs to the room: separate from the host's, removed when the room closes. The host state path stays reachable.

- `/tmp/scratchpad/`: every temporary file you make. Private to you; every agent and subagent has its own.
- `/tmp/shared/<task>/`: files another agent must read, in a subdirectory you name for the task.

If your harness names its own scratchpad and allows `/tmp` only when asked, this is that ask: use `/tmp/scratchpad/` instead.

### Subagents

Whether you could do a piece of work yourself settles nothing; you could do all of it. What decides is what the work leaves in your window: when you need its result but not the output behind it (a gate run, a log, a sweep across files, a long command), a profile below takes it. The launch costs you one turn, and the output you read yourself costs you every turn after.

Launch them through Skill(rimz-subagents), subagents available to you:

- `explorer`: Finds files and traces code paths

### Skills

When a skill's description matches the work in hand, invoke it, even when you know the commands by heart: each skill is built for its one task and does it better than you would by hand.
</system_reminder>"#
        );
    }

    #[test]
    fn env_paragraph_escapes_cwd_and_names_shell_kind_only() {
        let request =
            ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
        let reminders = LaunchReminders {
            env: Some(LaunchEnv {
                shell: Some("/opt/<>&/bin/zsh".into()),
            }),
            ..Default::default()
        };
        let text = render(&request, &reminders, Path::new("/repo/</system_reminder>&"));
        assert!(text.starts_with("<system_reminder>\n### Environment\n\n- cwd: /repo/&lt;/system_reminder&gt;&amp;\n- shell: zsh\n\n### Files"));
        assert!(!text.contains("git"));
        assert_eq!(text.matches("</system_reminder>").count(), 1);
    }

    #[test]
    fn skills_section_follows_a_listed_rimz_skill() {
        let mut request =
            ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
        for (skills, shown) in [
            (None, false),
            (Some(vec!["commit"]), false),
            (Some(vec!["commit", "rimz-lsp"]), true),
        ] {
            request.skills = skills.map(|names| names.iter().map(|n| n.parse().unwrap()).collect());
            let text = render(
                &request,
                &LaunchReminders::default(),
                Path::new("/checkout"),
            );
            assert_eq!(text.contains(SKILLS_REMINDER_BODY), shown);
        }
    }

    #[test]
    fn env_paragraph_omits_unknown_shell() {
        let env = LaunchEnv { shell: None };
        let request =
            ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
        let reminders = LaunchReminders {
            env: Some(env),
            ..Default::default()
        };
        let text = render(&request, &reminders, Path::new("/checkout"));
        assert!(
            text.starts_with("<system_reminder>\n### Environment\n\n- cwd: /checkout\n\n### Files")
        );
        assert!(!text.contains("shell"));
    }

    #[test]
    fn stage_handoff_reminder_stays_inside_the_single_team_wrapper() {
        let mut request =
            ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
        request.identity.params.team = Some("forge".to_owned());
        request.identity.params.role = Some("coder".to_owned());
        let team: Team =
            toml::from_str("[[roles]]\nrole = 'coder'\nprofile = 'claude'\nowns = ['Implement']")
                .expect("team");
        let reminders = LaunchReminders {
            team: Some(team_reminder(team)),
            ..LaunchReminders::default()
        };
        let text = render(&request, &reminders, Path::new("/worktree"));
        assert_eq!(text.matches("<system_reminder>").count(), 1);
        assert_eq!(text.matches("</system_reminder>").count(), 1);
        assert!(text.contains(
            "### Team\n\nYou are @coder, leader of team `forge`.\n\nPipeline: Implement (you) → Done"
        ));
        request.subagent = true;
        let text = render(&request, &reminders, Path::new("/worktree"));
        assert!(!text.contains("### Team"));
        assert!(!text.contains("$ ls"));
    }

    #[test]
    fn sandbox_reminder_is_opt_in_and_follows_model_before_policy() {
        let cwd = Path::new("/worktree");
        let mut request =
            ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
        assert_eq!(
            render(&request, &LaunchReminders::default(), cwd),
            wrap(concat!(
                "### Files\n\n",
                "- `$RIMZ_SCRATCH`: every temporary file you make. Private to you, removed when the room closes; every agent and subagent has its own.\n",
                "- `$RIMZ_SHARED/<task>/`: files another agent must read, in a subdirectory you name for the task.\n\n",
                "If your harness names its own scratchpad and allows another location only when asked, this is that ask: use `$RIMZ_SCRATCH` instead."
            ))
        );
        request.identity.params = LaunchParams {
            team: Some("forge".to_owned()),
            role: Some("coder".to_owned()),
            model: Some("gpt-6-astra".to_owned()),
            ..LaunchParams::default()
        };
        let mut reminders = LaunchReminders {
            env: Some(LaunchEnv {
                shell: Some("/bin/sh".into()),
            }),
            team: Some(team_reminder(
                toml::from_str("[[roles]]\nrole = 'coder'\nprofile = 'claude'").expect("team"),
            )),
            subagent_catalog: Some(SubagentCatalog::Disabled),
            ..LaunchReminders::default()
        };
        for subagent in [false, true] {
            request.subagent = subagent;
            for sandbox in [false, true] {
                reminders.sandbox = sandbox;
                let text = render(&request, &reminders, cwd);
                assert_eq!(text.contains(SANDBOX_REMINDER_BODY), sandbox);
                assert_eq!(text.contains(HOST_SCRATCH_REMINDER_BODY), !sandbox);
                assert_eq!(text.contains("team `forge`"), !subagent);
                assert_eq!(text.matches("<system_reminder>").count(), 1);
                assert_eq!(text.matches("</system_reminder>").count(), 1);
                let identity = text
                    .find(if subagent { "GPT 6 Astra" } else { "### Team" })
                    .expect("identity");
                assert_eq!(text.contains("GPT 6 Astra"), subagent);
                let env = text.find("- shell: sh").expect("environment paragraph");
                assert!(identity < env);
                assert!(
                    env < text
                        .find(if sandbox {
                            SANDBOX_REMINDER_BODY
                        } else {
                            HOST_SCRATCH_REMINDER_BODY
                        })
                        .unwrap()
                );
                let policy = text
                    .find(if subagent {
                        SUBAGENT_REMINDER_BODY
                    } else {
                        "Subagents are disabled"
                    })
                    .expect("policy paragraph");
                assert!(env < policy);
                let view = text
                    .find(if sandbox {
                        SANDBOX_REMINDER_BODY
                    } else {
                        HOST_SCRATCH_REMINDER_BODY
                    })
                    .expect("view paragraph");
                assert!(identity < view && view < policy);
            }
        }
    }

    #[test]
    fn model_line_names_handle_and_model_without_effort() {
        let params = LaunchParams {
            role: Some("planner".to_owned()),
            profile: Some("writer".to_owned()),
            model: Some("claude-fable-5-1-20260801".to_owned()),
            effort: Some("high".to_owned()),
            ..LaunchParams::default()
        };
        let line = |params: &LaunchParams| {
            model_fragment(params).map(|fragment| model_line(params, &fragment))
        };
        for (params, expected) in [
            (
                params.clone(),
                Some("You are @planner, running on Fable 5.1."),
            ),
            (
                LaunchParams {
                    role: None,
                    ..params.clone()
                },
                Some("You are @writer, running on Fable 5.1."),
            ),
            (
                LaunchParams {
                    role: None,
                    profile: None,
                    ..params.clone()
                },
                Some("You are running on Fable 5.1."),
            ),
            (
                LaunchParams {
                    model: None,
                    ..params.clone()
                },
                None,
            ),
            (
                LaunchParams {
                    role: Some("<role>".to_owned()),
                    model: Some("<model>".to_owned()),
                    ..params
                },
                Some("You are @&lt;role&gt;, running on &lt;model&gt;."),
            ),
        ] {
            assert_eq!(line(&params).as_deref(), expected);
        }
    }

    #[test]
    fn team_paragraph_names_every_seat_without_models() {
        let mut request =
            ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
        request.identity.params = LaunchParams {
            team: Some("forge".to_owned()),
            role: Some("planner".to_owned()),
            // Neither the launch model nor the configured model appears for a team.
            model: Some("claude-fable-5-1".to_owned()),
            effort: Some("high".to_owned()),
            ..LaunchParams::default()
        };
        let team: Team = toml::from_str(
            "leader = 'planner'\n[[roles]]\nrole = 'planner'\nprofile = 'claude'\nmodel = 'claude-opus-4-8'\n[[roles]]\nrole = 'coder'\nprofile = 'codex'",
        )
        .expect("team");
        let mut reminders = LaunchReminders {
            team: Some(team_reminder(team)),
            ..LaunchReminders::default()
        };
        let text = render(&request, &reminders, Path::new("/worktree"));
        assert!(
            text.starts_with(
                "<system_reminder>\n### Team\n\nYou are @planner, leader of team `forge`.\n\nMembers: @planner (you), @coder."
            ),
            "{text}"
        );
        assert!(!text.contains("Fable 5.1"));
        assert!(!text.contains("Opus 4.8"));

        // The model toggle has no effect for team members.
        let with_model_enabled = text;
        reminders.model = false;
        let text = render(&request, &reminders, Path::new("/worktree"));
        assert_eq!(text, with_model_enabled);
    }

    #[test]
    fn team_files_finish_environment_after_bullets() {
        let worktree = tempfile::tempdir().unwrap();
        let mut request =
            ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
        request.identity.params.team = Some("forge".to_owned());
        request.identity.params.role = Some("planner".to_owned());
        let team: Team = toml::from_str(
            "leader = 'planner'\n[[roles]]\nrole = 'planner'\nprofile = 'claude'\nowns = ['Plan']",
        )
        .unwrap();
        let mut reminders = LaunchReminders {
            env: Some(LaunchEnv {
                shell: Some("/bin/zsh".into()),
            }),
            lsp_servers: vec!["rust".to_owned()],
            team: Some(team_reminder(team)),
            ..Default::default()
        };
        for present in [false, true] {
            if present {
                std::fs::write(worktree.path().join("blackboard.md"), "Stage: Done\n").unwrap();
            }
            let text = render(&request, &reminders, worktree.path());
            let listing = if present {
                "blackboard.md"
            } else {
                "(no such files)"
            };
            assert!(text.contains(&format!("- shell: zsh\n- lsp: rust, via Skill(rimz-lsp)\n\n```\n$ ls blackboard.md *-notes.md\n{listing}\n```\n\n### Files")));
            assert_eq!(text.matches(worktree.path().to_str().unwrap()).count(), 1);
            assert_eq!(text.contains("[Done]"), present);
        }
        reminders.env = None;
        reminders.lsp_servers.clear();
        let text = render(&request, &reminders, worktree.path());
        assert!(text.contains(&format!(
            "### Environment\n\n- cwd: {}\n\n```",
            worktree.path().display()
        )));
        reminders.team.as_mut().unwrap().team.scratch_files = Some(Vec::new());
        let text = render(&request, &reminders, worktree.path());
        assert!(!text.contains("$ ls"));
        assert!(!text.contains("no memory files"));
    }
}
