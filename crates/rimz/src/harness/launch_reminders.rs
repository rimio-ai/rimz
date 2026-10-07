//! One system reminder carrying team context, model identity, the loop fire that launched the agent, launch environment (cwd, worktree provenance, shell), sandbox view, subagent policy, and shared language servers.

use std::path::{Path, PathBuf};

use super::launch::ExecRequest;
use super::launch_context::{self, escape_reminder_text};
use super::subagent_policy::{self, SubagentCatalog};
use crate::agents::payload::{RimzBlock, wrap_rimz_block};
use crate::agents::{LaunchParams, model_display::display_model};

pub use super::launch_context::TeamReminder;

pub(super) struct LaunchReminders {
    /// The Environment bullets (cwd, worktree, and shell) are on.
    pub env: bool,
    pub worktree: Option<crate::worktree::LinkedWorktree>,
    /// The launch gets its memory-file listing and sampled git state at prompt submit, so the reminder carries neither.
    pub runtime_env: bool,
    /// The configured `[agents] shell`, which the launch runs under in place
    /// of the user's own shell.
    pub agent_shell: Option<std::path::PathBuf>,
    /// Launch artifact directory and ambient environment; absent in preflight.
    pub settings: Option<(
        std::path::PathBuf,
        std::collections::BTreeMap<String, String>,
    )>,
    /// Routine RimZ permissions for the launch: the private settings-artifact
    /// dir, then the temp unit and shared dir as the agent sees them. `None` for
    /// preflight compiles and when `allow-routine-rimz` is off.
    pub routine_rimz: Option<(std::path::PathBuf, [std::path::PathBuf; 2])>,
    /// The launch's temp unit and the room's shared dir; every compiled launch has them.
    pub files: Option<TempFiles>,
    /// The launched profile's `model-reminder`; on when unset or when the launch has no profile.
    pub model: bool,
    /// The launch runs inside the RimZ sandbox view: switches off the
    /// provider's native command sandbox.
    pub sandbox: bool,
    pub lsp_configured: bool,
    pub lsp_servers: Vec<String>,
    pub subagent_catalog: Option<SubagentCatalog>,
    pub team: Option<TeamReminder>,
}

/// The two Environment bullets naming where an agent writes files.
pub(super) struct TempFiles {
    /// The temp unit as the agent sees it: `/tmp` in a sandbox, its host path otherwise.
    pub tmp: PathBuf,
    pub shared: PathBuf,
    /// The unit belongs to the launch's caller.
    pub caller: bool,
}

impl Default for LaunchReminders {
    fn default() -> Self {
        Self {
            env: false,
            worktree: None,
            runtime_env: false,
            agent_shell: None,
            settings: None,
            routine_rimz: None,
            files: None,
            model: true,
            sandbox: false,
            lsp_configured: false,
            lsp_servers: Vec::new(),
            subagent_catalog: None,
            team: None,
        }
    }
}

const SUBAGENT_REMINDER_BODY: &str = concat!(
    "### Subagents\n\n",
    "You are a subagent: a supervised child launched by another agent to ",
    "complete the task you were given. The task is scoped to this one run, so do the work ",
    "yourself rather than launching with Skill(rimz-agents, rimz-subagents, rimz-teams); ",
    "nothing above your caller supervises a run you start. Report the result in your final ",
    "response: it is the answer your caller reads once this run settles."
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

/// `shell` is the one the launch wrapper runs under, named when the
/// Environment bullets are on.
pub(super) fn render(
    request: &ExecRequest,
    reminders: &LaunchReminders,
    cwd: &Path,
    shell: Option<&Path>,
) -> String {
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
    if let Some(body) = &request.loop_reminder {
        paragraphs.push(format!("### Loop\n\n{body}"));
    }
    if reminders.env
        || !reminders.lsp_servers.is_empty()
        || team_context.is_some()
        || reminders.files.is_some()
    {
        paragraphs.push(env_paragraph(
            reminders,
            reminders.env.then_some(shell).flatten(),
            cwd,
            team_context.as_ref(),
        ));
    }
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
    reminders: &LaunchReminders,
    shell: Option<&Path>,
    cwd: &Path,
    team: Option<&launch_context::TeamLaunchContext>,
) -> String {
    let env = reminders.env;
    let lsp_servers = &reminders.lsp_servers;
    let mut lines = vec!["### Environment\n".to_owned()];
    if env || team.is_some() {
        lines.push(format!(
            "- cwd: {}",
            escape_reminder_text(&cwd.to_string_lossy())
        ));
        if let Some(worktree) = &reminders.worktree {
            let base = worktree
                .base_branch
                .as_deref()
                .map(|branch| format!("branched from {}; ", escape_reminder_text(branch)))
                .unwrap_or_default();
            lines.push(format!(
                "- worktree: {base}primary checkout at {}",
                escape_reminder_text(&worktree.primary.to_string_lossy())
            ));
        }
    }
    if let Some(kind) = shell.and_then(Path::file_name) {
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
    if let Some(files) = &reminders.files {
        let sharing = if files.caller {
            "You share it with your caller"
        } else {
            "Your subagents share it"
        };
        lines.push(format!(
            "- tmp: {} (`$TMPDIR`): every temporary file you make. {sharing}; no other agent sees it.",
            escape_reminder_text(&files.tmp.to_string_lossy())
        ));
        lines.push(format!(
            "- shared: {} (`$RIMZ_SHARED`): files a peer or teammate must read, in a `<task>/` subdirectory you name.",
            escape_reminder_text(&files.shared.to_string_lossy())
        ));
    }
    if !reminders.runtime_env
        && let Some(files) = team.and_then(launch_context::files_block)
    {
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
            let text = render(&request, &reminders, Path::new("/checkout"), None);
            assert!(text.contains("### Environment\n\n- lsp: rust, python, via Skill(rimz-lsp)"));
            assert!(!text.contains("### Files"));
            if subagent {
                assert!(text.contains(SUBAGENT_REMINDER_BODY));
            }
            assert!(
                !render(
                    &request,
                    &LaunchReminders::default(),
                    Path::new("/checkout"),
                    None
                )
                .contains("### Environment")
            );
        }
    }

    fn team_reminder(team: Team) -> TeamReminder {
        TeamReminder::new(team)
    }

    const SHARED: &str = "/home/marvin/.rimz/ws/rimz-f89e/shared";

    fn files(tmp: &str, caller: bool) -> Option<TempFiles> {
        Some(TempFiles {
            tmp: tmp.into(),
            shared: SHARED.into(),
            caller,
        })
    }

    #[test]
    fn environment_names_the_temp_unit_and_shared_dir() {
        let request =
            ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
        let shared = "- shared: /home/marvin/.rimz/ws/rimz-f89e/shared (`$RIMZ_SHARED`): files a peer or teammate must read, in a `<task>/` subdirectory you name.";
        for (tmp, caller, line) in [
            (
                "/tmp",
                false,
                "- tmp: /tmp (`$TMPDIR`): every temporary file you make. Your subagents share it; no other agent sees it.",
            ),
            (
                "/tmp",
                true,
                "- tmp: /tmp (`$TMPDIR`): every temporary file you make. You share it with your caller; no other agent sees it.",
            ),
            (
                "/home/marvin/.rimz/ws/rimz-f89e/tmp/otter",
                false,
                "- tmp: /home/marvin/.rimz/ws/rimz-f89e/tmp/otter (`$TMPDIR`): every temporary file you make. Your subagents share it; no other agent sees it.",
            ),
        ] {
            let reminders = LaunchReminders {
                files: files(tmp, caller),
                ..LaunchReminders::default()
            };
            assert_eq!(
                render(&request, &reminders, Path::new("/checkout"), None),
                wrap(&format!("### Environment\n\n{line}\n{shared}"))
            );
        }
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
            env: true,
            lsp_servers: vec!["rust".to_owned(), "python".to_owned()],
            files: files("/tmp", false),
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
            render(
                &request,
                &reminders,
                Path::new("/checkout"),
                Some(Path::new("/usr/bin/zsh"))
            ),
            r#"<system_reminder>
You are @brainstormer, running on Opus.

### Environment

- cwd: /checkout
- shell: zsh
- lsp: rust, python, via Skill(rimz-lsp)
- tmp: /tmp (`$TMPDIR`): every temporary file you make. Your subagents share it; no other agent sees it.
- shared: /home/marvin/.rimz/ws/rimz-f89e/shared (`$RIMZ_SHARED`): files a peer or teammate must read, in a `<task>/` subdirectory you name.

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
    fn loop_section_follows_identity_and_precedes_environment() {
        let mut request =
            ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
        let reminders = LaunchReminders {
            env: true,
            ..Default::default()
        };
        let render = |request: &ExecRequest| render(request, &reminders, Path::new("/w"), None);
        assert!(!render(&request).contains("### Loop"));
        request.loop_reminder = Some("The user fired the rule `x` by hand.".to_owned());
        assert_eq!(
            render(&request),
            "<system_reminder>\n### Loop\n\nThe user fired the rule `x` by hand.\n\n### Environment\n\n- cwd: /w\n</system_reminder>"
        );
        request.identity.params.role = Some("fixer".to_owned());
        request.identity.params.model = Some("opus".to_owned());
        assert!(render(&request).starts_with(
            "<system_reminder>\nYou are @fixer, running on Opus.\n\n### Loop\n\nThe user fired the rule `x` by hand.\n\n### Environment\n\n"
        ));
    }

    #[test]
    fn env_paragraph_escapes_cwd_and_names_shell_kind_only() {
        let request =
            ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
        let reminders = LaunchReminders {
            env: true,
            ..Default::default()
        };
        let text = render(
            &request,
            &reminders,
            Path::new("/repo/</system_reminder>&"),
            Some(Path::new("/opt/<>&/bin/zsh")),
        );
        assert!(text.starts_with("<system_reminder>\n### Environment\n\n- cwd: /repo/&lt;/system_reminder&gt;&amp;\n- shell: zsh\n</system_reminder>"));
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
                None,
            );
            assert_eq!(text.contains(SKILLS_REMINDER_BODY), shown);
        }
    }

    #[test]
    fn env_paragraph_omits_unknown_shell() {
        let request =
            ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
        let reminders = LaunchReminders {
            env: true,
            ..Default::default()
        };
        let text = render(&request, &reminders, Path::new("/checkout"), None);
        assert!(text.starts_with("<system_reminder>\n### Environment\n\n- cwd: /checkout\n</"));
        assert!(!text.contains("shell"));
    }

    #[test]
    fn env_paragraph_names_worktree_provenance_and_escapes_it() {
        let request =
            ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
        for (base, primary, bullet) in [
            (
                Some("main"),
                "/primary",
                "branched from main; primary checkout at /primary",
            ),
            (None, "/primary", "primary checkout at /primary"),
            (
                Some("base</system_reminder>&\n"),
                "/repo</system_reminder>&\n",
                "branched from base&lt;/system_reminder&gt;&amp;\\n; primary checkout at /repo&lt;/system_reminder&gt;&amp;\\n",
            ),
        ] {
            let mut reminders = LaunchReminders {
                env: true,
                worktree: Some(crate::worktree::LinkedWorktree {
                    base_branch: base.map(str::to_owned),
                    primary: primary.into(),
                }),
                lsp_servers: vec!["rust".to_owned()],
                ..Default::default()
            };
            let text = render(
                &request,
                &reminders,
                Path::new("/checkout"),
                Some(Path::new("/bin/zsh")),
            );
            assert!(
                text.contains(&format!(
                    "- cwd: /checkout\n- worktree: {bullet}\n- shell: zsh"
                )),
                "{text}"
            );
            assert_eq!(text.matches("</system_reminder>").count(), 1);
            reminders.env = false;
            let text = render(&request, &reminders, Path::new("/checkout"), None);
            assert!(text.contains("- lsp: rust"));
            assert!(!text.contains("- cwd:"));
            assert!(!text.contains("- worktree:"));
            reminders.env = true;
            reminders.worktree = None;
            let text = render(&request, &reminders, Path::new("/checkout"), None);
            assert!(text.contains("- cwd: /checkout\n- lsp:"));
            assert!(!text.contains("- worktree:"));
        }
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
            worktree: Some(crate::worktree::LinkedWorktree {
                base_branch: None,
                primary: "/primary".into(),
            }),
            ..LaunchReminders::default()
        };
        let text = render(&request, &reminders, Path::new("/worktree"), None);
        assert_eq!(text.matches("<system_reminder>").count(), 1);
        assert_eq!(text.matches("</system_reminder>").count(), 1);
        assert!(text.contains(
            "### Team\n\nYou are @coder, leader of team `forge`.\n\nPipeline: Implement (you) → Done"
        ));
        assert!(text.contains("- cwd: /worktree\n- worktree: primary checkout at /primary"));
        request.subagent = true;
        let text = render(&request, &reminders, Path::new("/worktree"), None);
        assert!(!text.contains("### Team"));
        assert!(!text.contains("$ ls"));
    }

    #[test]
    fn environment_follows_identity_and_precedes_policy() {
        let cwd = Path::new("/worktree");
        let mut request =
            ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
        request.identity.params = LaunchParams {
            team: Some("forge".to_owned()),
            role: Some("coder".to_owned()),
            model: Some("gpt-6-astra".to_owned()),
            ..LaunchParams::default()
        };
        let mut reminders = LaunchReminders {
            env: true,
            team: Some(team_reminder(
                toml::from_str("[[roles]]\nrole = 'coder'\nprofile = 'claude'").expect("team"),
            )),
            subagent_catalog: Some(SubagentCatalog::Disabled),
            files: files("/tmp", false),
            ..LaunchReminders::default()
        };
        for subagent in [false, true] {
            request.subagent = subagent;
            for sandbox in [false, true] {
                reminders.sandbox = sandbox;
                let text = render(&request, &reminders, cwd, Some(Path::new("/bin/sh")));
                assert!(!text.contains("### Files"));
                assert_eq!(text.contains("team `forge`"), !subagent);
                assert_eq!(text.matches("<system_reminder>").count(), 1);
                assert_eq!(text.matches("</system_reminder>").count(), 1);
                let identity = text
                    .find(if subagent { "GPT 6 Astra" } else { "### Team" })
                    .expect("identity");
                assert_eq!(text.contains("GPT 6 Astra"), subagent);
                let env = text.find("- shell: sh").expect("environment paragraph");
                let tmp = text.find("- tmp: /tmp").expect("tmp bullet");
                let policy = text
                    .find(if subagent {
                        SUBAGENT_REMINDER_BODY
                    } else {
                        "Subagents are disabled"
                    })
                    .expect("policy paragraph");
                assert!(identity < env && env < tmp && tmp < policy);
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
        let text = render(&request, &reminders, Path::new("/worktree"), None);
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
        let text = render(&request, &reminders, Path::new("/worktree"), None);
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
            env: true,
            lsp_servers: vec!["rust".to_owned()],
            team: Some(team_reminder(team)),
            files: files("/tmp", false),
            ..Default::default()
        };
        for present in [false, true] {
            if present {
                std::fs::write(worktree.path().join("blackboard.md"), "Stage: Done\n").unwrap();
            }
            let text = render(
                &request,
                &reminders,
                worktree.path(),
                Some(Path::new("/bin/zsh")),
            );
            let listing = if present {
                "blackboard.md"
            } else {
                "(no such files)"
            };
            assert!(text.contains(
                "- shell: zsh\n- lsp: rust, via Skill(rimz-lsp)\n- tmp: /tmp (`$TMPDIR`)"
            ));
            assert!(text.contains(&format!("you name.\n\n```\n$ ls blackboard.md *-notes.md\n{listing}\n```\n</system_reminder>")));
            assert_eq!(text.matches(worktree.path().to_str().unwrap()).count(), 1);
            assert_eq!(text.contains("[Done]"), present);
        }
        reminders.env = false;
        reminders.lsp_servers.clear();
        let text = render(
            &request,
            &reminders,
            worktree.path(),
            Some(Path::new("/bin/zsh")),
        );
        assert!(text.contains(&format!(
            "### Environment\n\n- cwd: {}\n- tmp: /tmp",
            worktree.path().display()
        )));
        reminders.team.as_mut().unwrap().team.scratch_files = Some(Vec::new());
        let text = render(
            &request,
            &reminders,
            worktree.path(),
            Some(Path::new("/bin/zsh")),
        );
        assert!(!text.contains("$ ls"));
        assert!(!text.contains("no memory files"));
    }
}
