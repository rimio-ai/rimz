//! One system reminder carrying team context, model identity, sandbox view, and subagent policy.

use std::path::Path;

use super::launch::ExecRequest;
use super::launch_context::{self, escape_reminder_text};
use super::subagent_policy::{self, SubagentCatalog};
use crate::agents::{LaunchParams, model_display::display_model};
use crate::config::Team;

pub struct LaunchReminders {
    /// The launched profile's `model-reminder`; on when unset or when the launch has no profile.
    pub model: bool,
    pub sandbox: bool,
    pub subagent_catalog: Option<SubagentCatalog>,
    pub team: Option<Team>,
}

impl Default for LaunchReminders {
    fn default() -> Self {
        Self {
            model: true,
            sandbox: false,
            subagent_catalog: None,
            team: None,
        }
    }
}

const SANDBOX_REMINDER_BODY: &str = concat!(
    "This pane runs under a bubblewrap sandbox. `/tmp` belongs to this RimZ room: ",
    "teammates and subagents in the room share it, it is separate from the host's `/tmp`, ",
    "and it is removed when the room closes. The room's host state path remains accessible. ",
    "Use it freely for temporary files, and use `/tmp/scratchpad` as your scratchpad ",
    "directory. RimZ writes its own outputs there too: `rimz wake` command output under ",
    "`/tmp/rimz-wakes/` and settled subagent responses under `/tmp/rimz-subagents/`."
);

const SUBAGENT_REMINDER_BODY: &str = concat!(
    "You are a subagent: a supervised child launched by another agent to ",
    "complete the task you were given. The task is scoped to this one run, so do the work ",
    "yourself rather than launching with Skill(rimz-agents, rimz-subagents, rimz-teams); ",
    "nothing above your caller supervises a run you start. Report the result: your caller ",
    "receives a completion report pointing to your final response after the fleet settles."
);

pub(super) fn wrap(body: &str) -> String {
    format!("<system_reminder>\n{body}\n</system_reminder>")
}

/// Child policy for adapters that require a user-prompt fallback.
pub fn subagent_reminder() -> String {
    wrap(SUBAGENT_REMINDER_BODY)
}

pub(super) fn render(
    request: &ExecRequest,
    reminders: &LaunchReminders,
    cwd: &Path,
) -> Option<String> {
    let mut paragraphs = Vec::new();
    if !request.subagent
        && let Some(team) = reminders.team.as_ref()
        && let Some(context) = launch_context::team_launch_context(
            &request.identity.params,
            &request.action,
            team,
            cwd,
        )
    {
        paragraphs.push(launch_context::reminder(&context));
    }
    if reminders.model
        && let Some(model) = model_reminder(&request.identity.params, paragraphs.is_empty())
    {
        paragraphs.push(model);
    }
    if reminders.sandbox {
        paragraphs.push(SANDBOX_REMINDER_BODY.to_owned());
    }
    if request.subagent {
        paragraphs.push(SUBAGENT_REMINDER_BODY.to_owned());
    } else if let Some(catalog) = reminders.subagent_catalog.as_ref() {
        paragraphs.push(subagent_policy::reminder(catalog));
    }
    if paragraphs.is_empty() {
        return None;
    }
    Some(wrap(&paragraphs.join("\n\n")))
}

fn model_reminder(params: &LaunchParams, with_handle: bool) -> Option<String> {
    if params.model.is_none() && params.effort.is_none() {
        return None;
    }
    let mut text = if !with_handle {
        "You run".to_owned()
    } else if let Some(handle) = params.role.as_deref().or(params.profile.as_deref()) {
        format!("You are @{}, running", escape_reminder_text(handle))
    } else {
        "You are running".to_owned()
    };
    if let Some(model) = params.model.as_deref() {
        text.push_str(&format!(
            " on {}",
            escape_reminder_text(&display_model(model))
        ));
    }
    if let Some(effort) = params.effort.as_deref() {
        text.push_str(&format!(" at {} effort", escape_reminder_text(effort)));
    }
    text.push('.');
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

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
            team: Some(team),
            ..LaunchReminders::default()
        };
        let text = render(&request, &reminders, Path::new("/worktree")).expect("reminder");
        assert_eq!(text.matches("<system_reminder>").count(), 1);
        assert_eq!(text.matches("</system_reminder>").count(), 1);
        assert!(text.contains("rimz teams flip <stage> [-m note]"));
        request.subagent = true;
        let text = render(&request, &reminders, Path::new("/worktree")).expect("child reminder");
        assert!(!text.contains("rimz teams flip"));
    }

    #[test]
    fn sandbox_reminder_is_opt_in_and_follows_model_before_policy() {
        let cwd = Path::new("/worktree");
        let mut request =
            ExecRequest::bare_launch(crate::ids::AgentKind::new_unchecked("claude"), Vec::new());
        assert!(render(&request, &LaunchReminders::default(), cwd).is_none());
        request.identity.params = LaunchParams {
            team: Some("forge".to_owned()),
            role: Some("coder".to_owned()),
            model: Some("gpt-6-astra".to_owned()),
            ..LaunchParams::default()
        };
        let mut reminders = LaunchReminders {
            team: Some(Team {
                roles: Vec::new(),
                leader: None,
                layout: None,
                scratch_files: Vec::new(),
                stages: Vec::new(),
            }),
            subagent_catalog: Some(SubagentCatalog::Disabled),
            ..LaunchReminders::default()
        };
        for subagent in [false, true] {
            request.subagent = subagent;
            for sandbox in [false, true] {
                reminders.sandbox = sandbox;
                let text = render(&request, &reminders, cwd).expect("reminder");
                assert_eq!(text.contains(SANDBOX_REMINDER_BODY), sandbox);
                assert_eq!(text.contains("in team `forge`"), !subagent);
                assert_eq!(text.matches("<system_reminder>").count(), 1);
                assert_eq!(text.matches("</system_reminder>").count(), 1);
                let model = text.find("GPT 6 Astra").expect("model line");
                let policy = text
                    .find(if subagent {
                        SUBAGENT_REMINDER_BODY
                    } else {
                        "Subagents are disabled"
                    })
                    .expect("policy paragraph");
                assert!(model < policy);
                if !subagent {
                    assert!(text.find("in team `forge`").unwrap() < model);
                }
                if sandbox {
                    let sandbox = text.find(SANDBOX_REMINDER_BODY).unwrap();
                    assert!(model < sandbox && sandbox < policy);
                }
            }
        }
    }

    #[test]
    fn model_reminder_names_handle_model_and_effort() {
        let params = LaunchParams {
            role: Some("planner".to_owned()),
            profile: Some("writer".to_owned()),
            model: Some("claude-fable-5-1-20260801".to_owned()),
            effort: Some("high".to_owned()),
            ..LaunchParams::default()
        };
        for (params, with_handle, expected) in [
            (
                params.clone(),
                true,
                Some("You are @planner, running on Fable 5.1 at high effort."),
            ),
            (
                params.clone(),
                false,
                Some("You run on Fable 5.1 at high effort."),
            ),
            (
                LaunchParams {
                    role: None,
                    ..params.clone()
                },
                true,
                Some("You are @writer, running on Fable 5.1 at high effort."),
            ),
            (
                LaunchParams {
                    effort: None,
                    ..params.clone()
                },
                true,
                Some("You are @planner, running on Fable 5.1."),
            ),
            (
                LaunchParams {
                    model: None,
                    ..params.clone()
                },
                true,
                Some("You are @planner, running at high effort."),
            ),
            (
                LaunchParams {
                    role: None,
                    profile: None,
                    ..params.clone()
                },
                true,
                Some("You are running on Fable 5.1 at high effort."),
            ),
            (
                LaunchParams {
                    model: None,
                    effort: None,
                    ..params.clone()
                },
                true,
                None,
            ),
            (
                LaunchParams {
                    role: Some("<role>".to_owned()),
                    model: Some("<model>".to_owned()),
                    effort: Some("high\n<effort>".to_owned()),
                    ..params
                },
                true,
                Some(
                    "You are @&lt;role&gt;, running on &lt;model&gt; at high\\n&lt;effort&gt; effort.",
                ),
            ),
        ] {
            assert_eq!(model_reminder(&params, with_handle).as_deref(), expected);
        }
    }
}
