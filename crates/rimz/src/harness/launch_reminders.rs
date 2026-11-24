//! One system reminder carrying team context, model identity, the loop fire that launched the agent, launch environment (cwd, worktree provenance, shell), scoped view, subagent policy, shared language servers, and how the launch reaches each RimZ capability it has.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::launch::{ExecRequest, ReminderSection};
use super::launch_context::{self, escape_reminder_text};
use super::subagent_policy::{self, SubagentCatalog};
use crate::agents::payload::{RimzBlock, wrap_rimz_block};
use crate::agents::{LaunchParams, model_display::display_model};
use crate::mcp::catalog::Capability;

pub use super::launch_context::TeamReminder;

pub(super) struct LaunchReminders {
    /// The Environment bullets (cwd, worktree, and shell) are on.
    pub env: bool,
    pub worktree: Option<crate::worktree::LinkedWorktree>,
    /// The launch gets its memory-file listing and sampled git state at prompt submit, so the reminder carries neither.
    pub runtime_env: bool,
    pub long_cache: bool,
    pub disable_native_idle_compaction: bool,
    /// RimZ's `resume.auto_continue` is on, so the provider's own usage-limit resume is too.
    pub enable_native_auto_continue: bool,
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
    /// RimZ's MCP server for the launch: its program and arguments. `None` for
    /// preflight compiles and when the launch hands over no server.
    pub mcp_server: Option<(std::path::PathBuf, Vec<String>)>,
    /// The launch argument, as `(flag, value)`, that switches the handed-over
    /// server off, so the launch serves none of its tools.
    pub mcp_switched_off: Option<(String, String)>,
    /// The launch directory as the agent sees it, on a kind whose provider starts RimZ's
    /// server itself and takes the launch's selection from its env.
    pub mcp_cwd: Option<PathBuf>,
    /// Room-root write grants; absent in preflight compiles.
    pub permission_roots: Option<[PathBuf; 3]>,
    /// The canonical automation and team notes runs; empty in preflight compiles.
    pub notes_runs: Vec<PathBuf>,
    /// The canonical home-wide note store, when it exists.
    pub notes_store: Option<PathBuf>,
    /// The launch's temp unit and the room's shared dir; every compiled launch has them.
    pub files: Option<TempFiles>,
    pub workflow_definition: Option<PathBuf>,
    /// The launched profile's `model-reminder`; on when unset or when the launch has no profile.
    pub model: bool,
    /// The launch runs inside the RimZ scoped view.
    pub sandbox: bool,
    pub lsp_configured: bool,
    pub lsp_servers: Vec<String>,
    /// The `rimz-*` skills on disk for the launch's kind and login; empty in
    /// preflight compiles.
    pub rimz_skills: BTreeSet<String>,
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
            enable_native_auto_continue: false,
            agent_shell: None,
            settings: None,
            routine_rimz: None,
            mcp_server: None,
            mcp_switched_off: None,
            mcp_cwd: None,
            permission_roots: None,
            notes_runs: Vec::new(),
            notes_store: None,
            files: None,
            workflow_definition: None,
            model: true,
            sandbox: false,
            lsp_configured: false,
            lsp_servers: Vec::new(),
            rimz_skills: BTreeSet::new(),
            subagent_catalog: None,
            system_prompt_file: None,
            team: None,
        }
    }
}

impl LaunchReminders {
    /// The RimZ capabilities whose MCP tools the launch serves; empty when it
    /// hands over no server or its arguments switch the server off.
    pub(super) fn mcp_capabilities(&self, request: &ExecRequest) -> Vec<Capability> {
        if self.mcp_server.is_none() || self.mcp_switched_off.is_some() {
            return Vec::new();
        }
        request.mcp_capabilities(&self.denied_capabilities(request))
    }

    /// The launch gets no subagent surface (no `subagent` tool, no `rimz-subagents` skill, no
    /// Subagents paragraph): a child, whose every launch would be refused; a judge, whose child
    /// would get a writable checkout; and a caller whose profile allows no subagents.
    fn no_subagents(&self, request: &ExecRequest) -> bool {
        request.cannot_delegate()
            || matches!(self.subagent_catalog, Some(SubagentCatalog::NoneAllowed))
    }

    /// The capabilities the launch is denied whatever its lists say: every capability that
    /// launches agents where `ExecRequest::cannot_delegate` holds, and `rimz-subagents` on a
    /// launch with no subagent surface. A profile's `subagents = []` limits its children only,
    /// so it keeps `rimz-agents` and `rimz-teams`.
    pub(super) fn denied_capabilities(&self, request: &ExecRequest) -> Vec<Capability> {
        let no_subagents = self.no_subagents(request);
        Capability::ALL
            .into_iter()
            .filter(|capability| {
                (request.cannot_delegate() && capability.launches_agents())
                    || (no_subagents && *capability == Capability::Subagents)
            })
            .collect()
    }

    /// The variables that hand the launch's selection to a server its provider starts itself
    /// (`crate::mcp::launch_env`); `None` when the launch serves nothing that way.
    pub(super) fn mcp_launch_env(
        &self,
        request: &ExecRequest,
    ) -> Option<[(&'static str, String); 2]> {
        use crate::mcp::launch_env;
        let cwd = self.mcp_cwd.as_ref()?;
        let served = self.mcp_capabilities(request);
        if served.is_empty() {
            return None;
        }
        let marks: Vec<_> = request
            .served_marks()
            .into_iter()
            .filter(|(capability, _)| served.contains(capability))
            .collect();
        Some([
            (launch_env::ENV_SERVE, launch_env::render(&marks)),
            (launch_env::ENV_CWD, cwd.display().to_string()),
        ])
    }

    /// The rimz skills hidden from the model because the launch registers the
    /// MCP tools of the same name: none where the launch cannot withhold a skill,
    /// on the host of a kind with no per-launch skill switch.
    pub(super) fn hidden_skills(&self, request: &ExecRequest) -> Vec<&'static str> {
        let withholds = self.sandbox
            || crate::agents::find_definition(request.kind.as_str()).is_some_and(|adapter| {
                matches!(
                    adapter.spec().host_skills,
                    crate::agents::skills::HostSkills::Switch { .. }
                )
            });
        if !withholds {
            return Vec::new();
        }
        self.mcp_capabilities(request)
            .iter()
            .map(|capability| capability.name())
            .collect()
    }

    /// The skills withheld from the model, on every host kind: the skill of each capability
    /// the launch is denied (`LaunchReminders::denied_capabilities`), then the hidden ones.
    pub(super) fn withheld_skills(&self, request: &ExecRequest) -> Vec<&'static str> {
        self.denied_capabilities(request)
            .into_iter()
            .map(Capability::name)
            .chain(self.hidden_skills(request))
            .collect()
    }

    /// The skills callable for the launch, less those its MCP tools hide.
    fn callable_skills(&self, request: &ExecRequest) -> Option<Vec<crate::config::SkillName>> {
        request.callable_skills(&self.hidden_skills(request))
    }

    /// How the launch reaches `capability`: through its MCP tools where the
    /// launch serves them, else through its skill.
    fn reach(&self, request: &ExecRequest, capability: Capability) -> Reach {
        if self.mcp_capabilities(request).contains(&capability) {
            Reach::Tools(capability)
        } else {
            Reach::Skill
        }
    }

    /// The launch can use `capability`, through its MCP tools or its skill. Under a RimZ grant
    /// the skill counts only when it is on disk, as a `skills` list is checked on disk.
    fn callable(&self, request: &ExecRequest, capability: Capability) -> bool {
        let name = capability.name();
        self.reach(request, capability) == Reach::Tools(capability)
            || (request.skill_callable(&self.hidden_skills(request), name)
                && (request.skill_grant().is_none() || self.rimz_skills.contains(name)))
    }

    /// The launch can query the room's shared language servers.
    pub(super) fn lsp_callable(&self, request: &ExecRequest) -> bool {
        self.callable(request, Capability::Lsp)
    }

    /// Every RimZ capability the launch has, by name, and how it reaches each:
    /// those its MCP server serves, in `Capability::ALL` order, then each
    /// `rimz-*` skill on disk that its lists make callable and that it does not
    /// withhold (`LaunchReminders::withheld_skills`). A capability the launch has
    /// as both tools and skill is its tools.
    pub(super) fn rimz_capabilities(&self, request: &ExecRequest) -> Vec<(String, Reach)> {
        let served = self.mcp_capabilities(request);
        let hidden = self.hidden_skills(request);
        let withheld = self.withheld_skills(request);
        let skills = self
            .rimz_skills
            .iter()
            .filter(|name| {
                !withheld.contains(&name.as_str())
                    && request.skill_callable(&hidden, name)
                    && !served.iter().any(|capability| capability.name() == *name)
            })
            .map(|name| (name.clone(), Reach::Skill));
        served
            .iter()
            .map(|capability| (capability.name().to_owned(), Reach::Tools(*capability)))
            .chain(skills)
            .collect()
    }

    /// The reminder's line for each RimZ capability the launch has: its name and how the
    /// launch reaches it.
    pub(super) fn capability_lines(&self, request: &ExecRequest) -> Vec<(String, String)> {
        let server = server_name(request);
        self.rimz_capabilities(request)
            .into_iter()
            .map(|(name, reach)| {
                let via = reach_phrase(&name, reach, server);
                (name, via)
            })
            .collect()
    }

    pub(super) fn disable_native_subagents(&self, request: &ExecRequest) -> bool {
        request.subagent
            || (self.callable(request, Capability::Subagents)
                && matches!(self.subagent_catalog, Some(SubagentCatalog::Available(_))))
    }
}

/// How a launch reaches one RimZ capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Reach {
    /// The capability's tools on RimZ's MCP server.
    Tools(Capability),
    Skill,
}

/// Resolve RimZ capability `name` under `reach`: `mcp(<tool>)` (comma-separated
/// when a capability serves several tools) when served on RimZ's MCP server,
/// `skill(<name>)` when reached as a skill, or `None` when unreachable.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "wired by prompt rewrite and reminders next")
)]
pub(super) fn resolve_reach(name: &str, reach: Option<Reach>) -> Option<String> {
    let reach = reach?;
    Some(match reach {
        Reach::Skill => format!("skill({})", escape_reminder_text(name)),
        Reach::Tools(capability) => crate::mcp::catalog::TOOLS
            .iter()
            .filter(|tool| tool.capability == Some(capability))
            .map(|tool| format!("mcp({})", tool.name))
            .collect::<Vec<_>>()
            .join(", "),
    })
}

/// How the launch reaches capability `name`, as every reminder sentence words
/// it: the tools `TOOLS` assigns the capability, on RimZ's MCP server under the
/// `server` name the provider knows it by, or the skill.
fn reach_phrase(name: &str, reach: Reach, server: &str) -> String {
    let Reach::Tools(capability) = reach else {
        return format!("Skill({})", escape_reminder_text(name));
    };
    let tools: Vec<_> = crate::mcp::catalog::TOOLS
        .iter()
        .filter(|tool| tool.capability == Some(capability))
        .map(|tool| format!("`{}`", tool.name))
        .collect();
    format!("the {} of the `{server}` MCP server", tool_list(&tools))
}

/// `tools`, already quoted, as a list with its noun: "`a` tool", "`a` and `b` tools",
/// "`a`, `b`, and `c` tools".
fn tool_list(tools: &[String]) -> String {
    let (list, noun) = match tools {
        [] | [_] => (tools.concat(), "tool"),
        [first, second] => (format!("{first} and {second}"), "tools"),
        [rest @ .., last] => (format!("{}, and {last}", rest.join(", ")), "tools"),
    };
    format!("{list} {noun}")
}

/// The name RimZ's MCP server goes by for the request's kind.
fn server_name(request: &ExecRequest) -> &'static str {
    crate::agents::find_definition(request.kind.as_str())
        .map_or(crate::mcp::catalog::SERVER_NAME, |adapter| {
            adapter.mcp_server_name()
        })
}

/// The RimZ capabilities paragraph; none when the launch has no capability.
fn capabilities_paragraph(lines: &[(String, String)]) -> Option<String> {
    if lines.is_empty() {
        return None;
    }
    let bullets = lines
        .iter()
        .map(|(name, via)| format!("- {}: {via}", escape_reminder_text(name)))
        .collect::<Vec<_>>()
        .join("\n");
    Some(format!(
        "### RimZ capabilities\n\nA name written `rimz(x)` or `Skill(rimz-x)` in any text means \
         RimZ's capability `rimz-x`, which you reach as its bullet says:\n\n{bullets}"
    ))
}

const SUBAGENT_REMINDER_BODY: &str = concat!(
    "### Subagents\n\n",
    "You are a subagent: a supervised child launched by another agent to ",
    "complete the task you were given. The task is scoped to this one run, so do the work ",
    "yourself. Report the result in your final response: it is the answer your caller reads ",
    "once this run settles. Send your caller no progress notes: each one stops its wait and ",
    "costs it a turn. When you need its answer to go on, end your turn with the question as ",
    "your final response."
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
    let skills = reminders.callable_skills(request);
    let skills = skills.as_deref();
    let team_context = reminders
        .team
        .as_ref()
        .filter(|_| !request.subagent)
        .and_then(|team| {
            launch_context::team_launch_context(
                params,
                request.identity.name.as_deref(),
                &request.action,
                team,
                cwd,
            )
        });
    if let Some(context) = &team_context {
        paragraphs.push(format!("### Team\n\n{}", launch_context::reminder(context)));
    } else if let Some(model) = reminders.model.then(|| model_fragment(params)).flatten() {
        paragraphs.push(model_line(params, request.identity.name.as_deref(), &model));
    }
    if let Some(section) = &request.reminder_section {
        let (heading, body) = match section {
            ReminderSection::Loop(body) => ("Loop", body),
            ReminderSection::Workflow(body) => ("Workflow", body),
        };
        paragraphs.push(format!("### {heading}\n\n{body}"));
    }
    let lsp_servers = if reminders.lsp_callable(request) {
        reminders.lsp_servers.as_slice()
    } else {
        &[]
    };
    let server = server_name(request);
    let lsp_via = format!(
        "via {}",
        reach_phrase(
            Capability::Lsp.name(),
            reminders.reach(request, Capability::Lsp),
            server
        )
    );
    if reminders.env
        || !lsp_servers.is_empty()
        || team_context.is_some()
        || reminders.files.is_some()
        || reminders.workflow_definition.is_some()
        || !params.add_dirs.is_empty()
    {
        paragraphs.push(env_paragraph(
            reminders,
            (lsp_servers, &lsp_via),
            reminders.env.then_some(shell).flatten(),
            cwd,
            team_context.as_ref(),
            params,
            request.kind.as_str(),
        ));
    }
    if request.subagent {
        paragraphs.push(SUBAGENT_REMINDER_BODY.to_owned());
    } else if !reminders.no_subagents(request)
        && reminders.callable(request, Capability::Subagents)
        && let Some(catalog) = reminders.subagent_catalog.as_ref()
    {
        let via = reach_phrase(
            Capability::Subagents.name(),
            reminders.reach(request, Capability::Subagents),
            server,
        );
        paragraphs.push(subagent_policy::reminder(catalog, &via));
    }
    paragraphs.extend(capabilities_paragraph(&reminders.capability_lines(request)));
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
    (lsp_servers, lsp_via): (&[String], &str),
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
        let not_loaded = |files: &str| {
            let reason = if params.untrusted_checkout {
                format!(
                    "{provider} has no trust decision for this folder, so you started in an \
                     empty directory of your own, and"
                )
            } else {
                "Your caller started you in an empty directory of your own, so".to_owned()
            };
            format!(
                "(Your caller asked you to work here. {reason} its {files} were not loaded. \
                 Read, search, and cd into it freely; read its instruction files yourself if \
                 the task needs them. If the task needs code run inside it, stop and say so in \
                 your report.)"
            )
        };
        let target_line = match adapter.added_dirs() {
            crate::agents::capabilities::AddedDirs::FileAccess { unloaded } => not_loaded(unloaded),
            crate::agents::capabilities::AddedDirs::Unsupported => {
                not_loaded("project configuration and instruction files")
            }
            crate::agents::capabilities::AddedDirs::Workspace => format!(
                "(Your caller asked you to work here. It started you in an empty directory of \
                 your own, and {provider} opened this one as a workspace, loading its \
                 instructions, rules, and skills and running its hooks.)"
            ),
        };
        lines.push(format!(
            "- target: {}\n  {target_line}",
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
            "- lsp: {}, {lsp_via}",
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
fn model_line(params: &LaunchParams, name: Option<&str>, fragment: &str) -> String {
    match params.role.as_deref().or(params.profile.as_deref()) {
        Some(handle) => format!(
            "You are {}, running {fragment}.",
            escape_reminder_text(&crate::address::handle_with_name(
                &format!("@{handle}"),
                name
            ))
        ),
        None => format!("You are running {fragment}."),
    }
}

#[cfg(test)]
mod tests;
