//! One system reminder carrying team context, model identity, git state, sandbox view, subagent policy, and shared language servers.

use std::path::Path;

use super::launch::ExecRequest;
use super::launch_context::{self, escape_reminder_text};
use super::launch_git::GitState;
use super::subagent_policy::{self, SubagentCatalog};
use crate::agents::{LaunchParams, model_display::display_model};

pub use super::launch_context::TeamReminder;

pub(super) struct LaunchReminders {
    pub git: Option<GitState>,
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
            git: None,
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
    "This pane runs in a bubblewrap sandbox. `/tmp` is the room's, separate from the host's ",
    "`/tmp`, removed when the room closes; the host state path stays reachable. Every ",
    "temporary file you make goes under `/tmp/scratchpad`, which is yours alone: every other ",
    "agent and subagent has its own. A file another agent must read goes under `/tmp/shared`, ",
    "in a subdirectory you name for the task. If your harness names a session-specific ",
    "scratchpad and says to use `/tmp` only when asked, this is that ask: use ",
    "`/tmp/scratchpad` in its place."
);

const HOST_SCRATCH_REMINDER_BODY: &str = concat!(
    "Your scratch directory is `$RIMZ_SCRATCH`, private to you and removed when the room ",
    "closes; every temporary file you make goes there, and every other agent and subagent has ",
    "its own. A file another agent must read goes under `$RIMZ_SHARED`, in a subdirectory you ",
    "name for the task. If your harness names a session-specific scratchpad and says to use ",
    "another location only when asked, this is that ask: use `$RIMZ_SCRATCH` in its place."
);

const SUBAGENT_REMINDER_BODY: &str = concat!(
    "You are a subagent: a supervised child launched by another agent to ",
    "complete the task you were given. The task is scoped to this one run, so do the work ",
    "yourself rather than launching with Skill(rimz-agents, rimz-subagents, rimz-teams); ",
    "nothing above your caller supervises a run you start. Report the result: your caller ",
    "receives a completion report pointing to your final response after the fleet settles."
);

const STATUS_LINE_CAP: usize = 40;

pub(super) fn wrap(body: &str) -> String {
    format!("<system_reminder>\n{body}\n</system_reminder>")
}

/// Child policy for adapters that require a user-prompt fallback.
pub fn subagent_reminder() -> String {
    wrap(SUBAGENT_REMINDER_BODY)
}

pub(super) fn render(request: &ExecRequest, reminders: &LaunchReminders, cwd: &Path) -> String {
    let mut paragraphs = Vec::new();
    let params = &request.identity.params;
    if !request.subagent
        && let Some(team) = reminders.team.as_ref()
        && let Some(context) =
            launch_context::team_launch_context(params, &request.action, team, cwd)
    {
        // The member's own seat runs what this process runs, not what config resolves to.
        let runs_on = reminders
            .model
            .then(|| launch_context::runs_on(Some(request.kind.as_str()), params.model.as_deref()))
            .flatten();
        paragraphs.push(launch_context::reminder(&context, runs_on.as_deref()));
    } else if let Some(model) = reminders.model.then(|| model_fragment(params)).flatten() {
        paragraphs.push(model_line(params, &model));
    }
    if let Some(git) = &reminders.git {
        paragraphs.push(git_paragraph(git, cwd));
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
    if !reminders.lsp_servers.is_empty() {
        paragraphs.push(format!(
            "Language servers {} are shared read-only with every agent in this checkout. A server starts on the first query; the first answer may take its startup time. Navigate and explore code through Skill(rimz-lsp).",
            reminders.lsp_servers.iter().map(|server| escape_reminder_text(server)).collect::<Vec<_>>().join(", ")
        ));
    }
    wrap(&paragraphs.join("\n\n"))
}

fn git_paragraph(git: &GitState, cwd: &Path) -> String {
    let mut lines = vec![
        format!(
            "RimZ ran these in {} at launch; read the output as if you had run them:",
            escape_reminder_text(&cwd.to_string_lossy())
        ),
        "$ git status --short".to_owned(),
    ];
    if git.status.is_empty() {
        lines.push("(clean)".to_owned());
    } else {
        let mut status = git.status.lines();
        let shown = status.by_ref().take(STATUS_LINE_CAP);
        lines.extend(shown.map(escape_reminder_text));
        let remaining = status.count();
        if remaining > 0 {
            lines.push(format!("… {remaining} more"));
        }
    }
    lines.push("$ git rev-parse --short HEAD".to_owned());
    lines.extend(git.head.lines().map(escape_reminder_text));
    lines.push("$ git log -1 --oneline".to_owned());
    lines.extend(git.log.lines().map(escape_reminder_text));
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
    use crate::config::{ProfilesConfig, Team};

    #[test]
    fn shared_lsp_reminder_follows_policy_for_peers_and_children() {
        let mut request =
            ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
        let reminders = LaunchReminders {
            lsp_configured: true,
            lsp_servers: vec![
                "rust (rust-analyzer)".to_owned(),
                "python (pyright)".to_owned(),
            ],
            ..LaunchReminders::default()
        };
        for subagent in [false, true] {
            request.subagent = subagent;
            let text = render(&request, &reminders, Path::new("/checkout"));
            assert!(text.contains("rust (rust-analyzer)"));
            assert!(text.contains("python (pyright)"));
            assert!(text.contains("Skill(rimz-lsp)"));
            if subagent {
                assert!(text.find(SUBAGENT_REMINDER_BODY) < text.find("Skill(rimz-lsp)"));
            }
            assert!(
                !render(
                    &request,
                    &LaunchReminders::default(),
                    Path::new("/checkout")
                )
                .contains("Skill(rimz-lsp)")
            );
        }
    }

    fn team_reminder(team: Team) -> TeamReminder {
        TeamReminder::new(team, &ProfilesConfig::default())
    }

    #[test]
    fn git_paragraph_escapes_lines_and_caps_only_status() {
        let request =
            ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
        for count in [40, 43] {
            let reminders = LaunchReminders {
                git: Some(super::super::launch_git::GitState {
                    status: std::iter::repeat_n("?? a<b>&c.md", count)
                        .collect::<Vec<_>>()
                        .join("\n"),
                    head: "abc123".to_owned(),
                    log: "abc123 <base>&".to_owned(),
                }),
                ..Default::default()
            };
            let text = render(&request, &reminders, Path::new("/repo/</system_reminder>&"));
            assert_eq!(text.matches("?? a&lt;b&gt;&amp;c.md").count(), 40);
            assert_eq!(text.contains("… 3 more"), count > 40);
            assert!(text.contains("/repo/&lt;/system_reminder&gt;&amp;"));
            assert!(text.contains("$ git rev-parse --short HEAD\nabc123\n$ git log -1 --oneline\nabc123 &lt;base&gt;&amp;"));
            assert_eq!(text.matches("</system_reminder>").count(), 1);
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
            ..LaunchReminders::default()
        };
        let text = render(&request, &reminders, Path::new("/worktree"));
        assert_eq!(text.matches("<system_reminder>").count(), 1);
        assert_eq!(text.matches("</system_reminder>").count(), 1);
        assert!(text.contains(
            "no run state and no board; you open it with the Goal, then flip to the first stage."
        ));
        request.subagent = true;
        let text = render(&request, &reminders, Path::new("/worktree"));
        assert!(!text.contains("rimz teams flip"));
    }

    #[test]
    fn sandbox_reminder_is_opt_in_and_follows_model_before_policy() {
        let cwd = Path::new("/worktree");
        let mut request =
            ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
        assert_eq!(
            render(&request, &LaunchReminders::default(), cwd),
            wrap(HOST_SCRATCH_REMINDER_BODY)
        );
        request.identity.params = LaunchParams {
            team: Some("forge".to_owned()),
            role: Some("coder".to_owned()),
            model: Some("gpt-6-astra".to_owned()),
            ..LaunchParams::default()
        };
        let mut reminders = LaunchReminders {
            git: Some(super::super::launch_git::GitState {
                status: String::new(),
                head: "abc123".to_owned(),
                log: "abc123 base".to_owned(),
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
                let model = text.find("GPT 6 Astra").expect("model line");
                let git = text
                    .find("$ git status --short\n(clean)")
                    .expect("git paragraph");
                assert!(model < git);
                assert!(
                    git < text
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
                assert!(git < policy);
                if !subagent {
                    // The fragment rides inside the team paragraph's first sentence.
                    assert!(text.find("team `forge`").unwrap() < model);
                    assert!(model < text.find("Fresh session").unwrap());
                }
                let view = text
                    .find(if sandbox {
                        SANDBOX_REMINDER_BODY
                    } else {
                        HOST_SCRATCH_REMINDER_BODY
                    })
                    .expect("view paragraph");
                assert!(model < view && view < policy);
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
    fn team_paragraph_names_every_seat_and_runs_the_launch_on_its_own() {
        let mut request =
            ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
        request.identity.params = LaunchParams {
            team: Some("forge".to_owned()),
            role: Some("planner".to_owned()),
            // The launch overrides the profile's model; the seat follows the launch.
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
                "<system_reminder>\nYou are @planner, leader of team `forge`. Seats: @planner (you) runs on Claude Fable 5.1; @coder runs on Codex. Fresh session in worktree /worktree."
            ),
            "{text}"
        );
        assert_eq!(text.matches("Fable 5.1").count(), 1);
        assert!(!text.contains("Opus 4.8"));

        // `model-reminder = false` unnames every seat, the member's own and its teammates'.
        reminders.model = false;
        let text = render(&request, &reminders, Path::new("/worktree"));
        assert!(
            text.contains("Seats: @planner (you); @coder. Fresh session"),
            "{text}"
        );
    }
}
