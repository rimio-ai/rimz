//! One system reminder carrying team context, model identity, and subagent policy.

use std::path::Path;

use super::launch::ExecRequest;
use super::launch_context::{self, escape_reminder_text};
use super::subagent_policy::{self, SubagentCatalog};
use crate::agents::{LaunchParams, model_display::display_model};
use crate::config::Team;

pub struct LaunchReminders {
    pub model: bool,
    pub subagent_catalog: Option<SubagentCatalog>,
    pub team: Option<Team>,
}

impl Default for LaunchReminders {
    fn default() -> Self {
        Self {
            model: true,
            subagent_catalog: None,
            team: None,
        }
    }
}

const SUBAGENT_REMINDER_BODY: &str = concat!(
    "You are a subagent: a supervised child launched by another agent to ",
    "complete the task you were given. You must not spawn agents or subagents of any kind — do not use ",
    "agent, task, or spawn tools, and do not launch `rimz subagents`, `rimz agents`, or ",
    "`rimz teams`. Do the work yourself with your direct tools and report the result; your final ",
    "message is delivered to your caller as a message when you exit."
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
