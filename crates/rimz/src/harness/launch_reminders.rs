//! One system reminder carrying team context, model identity, the loop fire that launched the agent, launch environment (cwd, worktree provenance, shell), scoped view, subagent policy, and shared language servers.

use std::path::{Path, PathBuf};

use super::launch::{ExecRequest, ReminderSection};
use super::launch_context::{self, escape_reminder_text};
use super::subagent_policy::{self, SubagentCatalog};
use crate::agents::payload::{RimzBlock, wrap_rimz_block};
use crate::agents::{LaunchParams, model_display::display_model};
use crate::config::skill_is_callable;

pub use super::launch_context::TeamReminder;

pub(super) struct LaunchReminders {
    /// The Environment bullets (cwd, worktree, and shell) are on.
    pub env: bool,
    pub worktree: Option<crate::worktree::LinkedWorktree>,
    /// The launch gets its memory-file listing and sampled git state at prompt submit, so the reminder carries neither.
    pub runtime_env: bool,
    pub long_cache: bool,
    pub disable_native_idle_compaction: bool,
    /// RimZ's `resume.auto_continue` owns the resume after a usage limit on this launch.
    pub disable_native_auto_continue: bool,
    /// The configured `[agents] shell`, which the launch runs under in place
    /// of the user's own shell.
    pub agent_shell: Option<std::path::PathBuf>,
    /// Launch artifact directory and provider login environment; absent in preflight.
    pub settings: Option<(
        std::path::PathBuf,
        std::collections::BTreeMap<String, String>,
    )>,
    /// Routine RimZ permissions for the launch: the private settings-artifact
    /// dir, then the temp unit and shared dir as the agent sees them. `None` for
    /// preflight compiles and when `allow-routine-rimz` is off.
    pub routine_rimz: Option<(std::path::PathBuf, [std::path::PathBuf; 2])>,
    /// Room-root write grants; absent in preflight compiles.
    pub permission_roots: Option<[PathBuf; 3]>,
    /// The canonical automation and team notes runs; empty in preflight compiles.
    pub notes_runs: Vec<PathBuf>,
    /// The launch's temp unit and the room's shared dir; every compiled launch has them.
    pub files: Option<TempFiles>,
    pub workflow_definition: Option<PathBuf>,
    /// The launched profile's `model-reminder`; on when unset or when the launch has no profile.
    pub model: bool,
    /// The launch runs inside the RimZ scoped view.
    pub sandbox: bool,
    pub lsp_configured: bool,
    pub lsp_servers: Vec<String>,
    pub subagent_catalog: Option<SubagentCatalog>,
    pub system_prompt_file: Option<PathBuf>,
    pub team: Option<TeamReminder>,
}

/// The two Environment bullets naming where an agent writes files.
pub(super) struct TempFiles {
    /// The temp unit as the agent sees it: `/tmp` in a scoped view, its host path otherwise.
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
            long_cache: false,
            disable_native_idle_compaction: false,
            disable_native_auto_continue: false,
            agent_shell: None,
            settings: None,
            routine_rimz: None,
            permission_roots: None,
            notes_runs: Vec::new(),
            files: None,
            workflow_definition: None,
            model: true,
            sandbox: false,
            lsp_configured: false,
            lsp_servers: Vec::new(),
            subagent_catalog: None,
            system_prompt_file: None,
            team: None,
        }
    }
}

impl LaunchReminders {
    pub(super) fn disable_native_subagents(&self, request: &ExecRequest) -> bool {
        request.subagent
            || (skill_is_callable(request.callable_skills(), "rimz-subagents")
                && matches!(self.subagent_catalog, Some(SubagentCatalog::Available(_))))
    }
}

const SUBAGENT_REMINDER_BODY: &str = concat!(
    "### Subagents\n\n",
    "You are a subagent: a supervised child launched by another agent to ",
    "complete the task you were given. The task is scoped to this one run, so do the work ",
    "yourself. Report the result in your final response: it is the answer your caller reads ",
    "once this run settles."
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
    let skills = request.callable_skills();
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
    if let Some(section) = &request.reminder_section {
        let (heading, body) = match section {
            ReminderSection::Loop(body) => ("Loop", body),
            ReminderSection::Judge(body) => ("Judge", body),
            ReminderSection::Workflow(body) => ("Workflow", body),
        };
        paragraphs.push(format!("### {heading}\n\n{body}"));
    }
    let lsp_servers = if skill_is_callable(skills, "rimz-lsp") {
        reminders.lsp_servers.as_slice()
    } else {
        &[]
    };
    if reminders.env
        || !lsp_servers.is_empty()
        || team_context.is_some()
        || reminders.files.is_some()
        || reminders.workflow_definition.is_some()
        || !params.add_dirs.is_empty()
    {
        paragraphs.push(env_paragraph(
            reminders,
            lsp_servers,
            reminders.env.then_some(shell).flatten(),
            cwd,
            team_context.as_ref(),
            params,
            request.kind.as_str(),
        ));
    }
    if request.subagent {
        paragraphs.push(SUBAGENT_REMINDER_BODY.to_owned());
    } else if skill_is_callable(skills, "rimz-subagents")
        && let Some(catalog) = reminders.subagent_catalog.as_ref()
    {
        paragraphs.push(subagent_policy::reminder(catalog));
    }
    if skills.is_none_or(|skills| {
        skills
            .iter()
            .any(|skill| skill.as_str().starts_with("rimz-"))
    }) {
        paragraphs.push(SKILLS_REMINDER_BODY.to_owned());
    }
    wrap(&paragraphs.join("\n\n"))
}

fn env_paragraph(
    reminders: &LaunchReminders,
    lsp_servers: &[String],
    shell: Option<&Path>,
    cwd: &Path,
    team: Option<&launch_context::TeamLaunchContext>,
    params: &LaunchParams,
    kind: &str,
) -> String {
    let env = reminders.env;
    let mut lines = vec!["### Environment\n".to_owned()];
    if env || team.is_some() || !params.add_dirs.is_empty() {
        lines.push(format!(
            "- cwd: {}",
            escape_reminder_text(&cwd.to_string_lossy())
        ));
    }
    let target = params.checkout.as_deref().filter(|target| {
        params.add_dirs.iter().any(|dir| dir == target)
            && crate::disk::paths::is_under_root(cwd, &crate::disk::paths::empty_dir_root())
    });
    if let Some(target) = target {
        // Launch compilation resolves the kind before rendering its reminder.
        let adapter = crate::agents::find_definition(kind)
            .expect("the launch kind was resolved before rendering reminders");
        let provider = adapter.spec().display_name;
        let files = adapter.added_dirs_unloaded_files();
        let reason = if params.untrusted_checkout {
            format!(
                "{provider} has no trust decision for this folder, so you started in an empty \
                 directory of your own, and"
            )
        } else {
            "Your caller started you in an empty directory of your own, so".to_owned()
        };
        lines.push(format!(
            "- target: {}\n  (Your caller asked you to work here. {reason} its {files} were \
             not loaded. Read, search, and cd into it freely; read its instruction files \
             yourself if the task needs them. If the task needs code run inside it, stop \
             and say so in your report.)",
            escape_reminder_text(target)
        ));
    }
    for dir in params
        .add_dirs
        .iter()
        .filter(|dir| Some(dir.as_str()) != target)
    {
        lines.push(format!("- added dir: {}", escape_reminder_text(dir)));
    }
    if (env || team.is_some())
        && let Some(worktree) = &reminders.worktree
    {
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
    if let Some(definition) = &reminders.workflow_definition {
        lines.push(format!(
            "- workflow: {} (`$RIMZ_WORKFLOW_DEFINITION`): this workflow's definition folder.",
            escape_reminder_text(&definition.to_string_lossy())
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
mod tests;
