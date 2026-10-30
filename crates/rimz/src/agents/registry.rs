//! The agent registry — the single registration point.
//!
//! Built-ins live in `BUILTINS`; validated machine-tier process plugins join
//! them through [`all_definitions`]. Every behavioral dispatch site resolves
//! through this module, so no consumer grows a per-agent match arm.

use std::collections::BTreeMap;

use super::adapters::amp::AmpAdapter;
use super::adapters::antigravity::AntigravityAdapter;
use super::adapters::claude::ClaudeAdapter;
use super::adapters::codex::CodexAdapter;
use super::adapters::copilot::CopilotAdapter;
use super::adapters::cursor::CursorAdapter;
use super::adapters::droid::DroidAdapter;
use super::adapters::grok::GrokAdapter;
use super::adapters::hermes::HermesAdapter;
use super::adapters::kimi::KimiAdapter;
use super::adapters::kiro::KiroAdapter;
use super::adapters::minimax::MiniMaxAdapter;
use super::adapters::muse::MuseAdapter;
use super::adapters::omp::OmpAdapter;
use super::adapters::opencode::OpencodeAdapter;
use super::adapters::pi::PiAdapter;
use super::adapters::qoder::QoderAdapter;
use super::adapters::qwen::QwenAdapter;
use super::definition::{
    AgentDefinition, AgentSpec, CompactInstruction, ConcernCoverage, IntegrationConcern,
};
use super::{AgentErr, AgentState, Result};
use crate::config::HarnessConfig;
use crate::ids::AgentSessionId;

const PROCESS_DESCENT_DEPTH: usize = 8;

static CLAUDE: AgentDefinition = AgentDefinition::new(&ClaudeAdapter);
static CODEX: AgentDefinition = AgentDefinition::new(&CodexAdapter);
static AMP: AgentDefinition = AgentDefinition::new(&AmpAdapter);
static COPILOT: AgentDefinition = AgentDefinition::new(&CopilotAdapter);
static KIMI: AgentDefinition = AgentDefinition::new(&KimiAdapter);
static PI: AgentDefinition = AgentDefinition::new(&PiAdapter);
static OPENCODE: AgentDefinition = AgentDefinition::new(&OpencodeAdapter);
static ANTIGRAVITY: AgentDefinition = AgentDefinition::new(&AntigravityAdapter);
static CURSOR: AgentDefinition = AgentDefinition::new(&CursorAdapter);
static DROID: AgentDefinition = AgentDefinition::new(&DroidAdapter);
static KIRO: AgentDefinition = AgentDefinition::new(&KiroAdapter);
static QWEN: AgentDefinition = AgentDefinition::new(&QwenAdapter);
static GROK: AgentDefinition = AgentDefinition::new(&GrokAdapter);
static HERMES: AgentDefinition = AgentDefinition::new(&HermesAdapter);
static QODER: AgentDefinition = AgentDefinition::new(&QoderAdapter);
static OMP: AgentDefinition = AgentDefinition::new(&OmpAdapter);
static MUSE: AgentDefinition = AgentDefinition::new(&MuseAdapter);
static MINIMAX: AgentDefinition = AgentDefinition::new(&MiniMaxAdapter);

/// Every built-in definition, in stable display order.
pub(super) static BUILTINS: &[&AgentDefinition] = &[
    &CLAUDE,
    &CODEX,
    &AMP,
    &COPILOT,
    &KIMI,
    &PI,
    &OPENCODE,
    &ANTIGRAVITY,
    &CURSOR,
    &DROID,
    &KIRO,
    &QWEN,
    &GROK,
    &HERMES,
    &QODER,
    &OMP,
    &MUSE,
    &MINIMAX,
];

/// Every built-in and valid machine-tier plugin definition, in display order.
pub fn all_definitions() -> impl Iterator<Item = &'static AgentDefinition> {
    BUILTINS.iter().copied().chain(
        super::adapters::plugin::loaded()
            .definitions
            .iter()
            .copied(),
    )
}

/// Resolve a definition for the `--source <agent>` CLI tag.
pub fn definition_by_kind(kind: &str) -> Result<&'static AgentDefinition> {
    find_definition(kind).ok_or_else(|| AgentErr::Unknown(kind.to_owned()))
}

/// [`definition_by_kind`] for callers that treat an unknown kind as absence.
pub fn find_definition(kind: &str) -> Option<&'static AgentDefinition> {
    all_definitions().find(|definition| {
        definition.spec().kind == kind || definition.spec().aliases.contains(&kind)
    })
}

/// The static definition for `kind`, for sites that need only const data
/// (branding, capabilities, tool tables) without the behavioral trait.
pub fn spec_by_kind(kind: &str) -> Option<&'static AgentSpec> {
    find_definition(kind).map(AgentDefinition::spec)
}

/// The compaction command for `agent`, carrying the harness brief for its
/// seat; `None` when its adapter has no native compaction command.
pub fn compact_command(agent: &AgentState, harness: &HarnessConfig) -> Option<String> {
    spec_by_kind(agent.kind.as_str())?
        .launch
        .compact_command(harness.compact_instruction(agent.compact_seat()))
}

/// Why an adapter cannot take part in mid-turn soft compaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SoftCompactSkip {
    NoCompactCommand,
    NoLiveContextReading,
    NoCompactionSignal,
}

impl std::fmt::Display for SoftCompactSkip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NoCompactCommand => "no compact command",
            Self::NoLiveContextReading => "no live context reading",
            Self::NoCompactionSignal => "no compaction signal",
        })
    }
}

/// Derive mid-turn participation from the adapter's declared command and coverage.
pub fn soft_compact_skip(definition: &AgentDefinition) -> Option<SoftCompactSkip> {
    if definition.spec().launch.compact_command.is_none() {
        return Some(SoftCompactSkip::NoCompactCommand);
    }
    if matches!(
        definition.concern_coverage(IntegrationConcern::Compaction),
        ConcernCoverage::Unsupported { .. }
    ) {
        return Some(SoftCompactSkip::NoCompactionSignal);
    }
    if matches!(
        definition.concern_coverage(IntegrationConcern::ContextUsage),
        ConcernCoverage::Unsupported { .. }
    ) {
        return Some(SoftCompactSkip::NoLiveContextReading);
    }
    None
}

/// Compose the wrap-up steer, carrying the seat's brief when the command cannot.
pub fn soft_compact_prompt(agent: &AgentState, harness: &HarnessConfig) -> Option<String> {
    let command = spec_by_kind(agent.kind.as_str())?.launch.compact_command?;
    let prefix = concat!(
        "Context is getting high, so I'm going to compact you once this turn ends. ",
        "Wrap up the step you're on, without leaving any file half-edited, ",
        "and don't start anything new. Then end your turn"
    );
    let brief = harness.compact_instruction(agent.compact_seat());
    Some(
        if command.instruction == CompactInstruction::Trailing || brief.is_empty() {
            format!("{prefix}.")
        } else {
            format!("{prefix} with a note for yourself after the compaction:\n\n{brief}")
        },
    )
}

/// Display-order kinds — the walk doctor and the wiring probes iterate.
pub fn known_kinds() -> impl Iterator<Item = &'static str> {
    all_definitions().map(|definition| definition.spec().kind)
}

/// Agent kind for an interactive command, after shell syntax and process
/// wrappers are normalized by `crate::proc::command`.
pub(crate) fn command_agent_kind(command: &str) -> Option<&'static str> {
    command_agent_kind_with_comm(command, None)
}

pub(crate) fn command_agent_kind_with_comm(
    command: &str,
    comm: Option<&str>,
) -> Option<&'static str> {
    let program = crate::proc::command::effective_program_info(command);
    if let Some(adapter) = adapter_for_program(program) {
        return adapter
            .is_interactive_process(command)
            .then(|| adapter.spec().kind);
    }
    let adapter = comm.and_then(adapter_for_comm)?;
    let policy_command = (!command.trim().is_empty())
        .then_some(command)
        .or(comm)
        .unwrap_or_default();
    adapter
        .is_interactive_process(policy_command)
        .then(|| adapter.spec().kind)
}

/// Whether `command` can be the requested agent kind after caller-owned durable
/// identity already established that kind. This admits an ambiguous executable
/// basename for liveness and provider-local enrichment; generic process
/// classification continues through [`command_agent_kind`] and abstains.
pub(crate) fn command_may_be_agent_kind(command: &str, kind: &str) -> bool {
    let Some(adapter) = find_definition(kind) else {
        return false;
    };
    let program = crate::proc::command::effective_program_info(command);
    adapter_matches_program(adapter, program) && adapter.is_interactive_process(command)
}

/// Candidate kind for best-effort enrichment that cannot create agent truth.
/// Provider-local discovery uses this to find inputs for a hook-bound session;
/// pane presence and routing stay on [`command_agent_kind`].
pub(crate) fn command_agent_kind_candidate(command: &str) -> Option<&'static str> {
    let program = crate::proc::command::effective_program_info(command);
    let adapter = all_definitions().find(|adapter| adapter_matches_program(adapter, program))?;
    adapter
        .is_interactive_process(command)
        .then(|| adapter.spec().kind)
}

fn adapter_for_program(
    program: crate::proc::command::EffectiveProgram<'_>,
) -> Option<&'static AgentDefinition> {
    let label = crate::proc::command::basename(program.program);
    let from_launcher = program.program != program.root_program
        && crate::proc::command::is_launcher(program.root_program);
    all_definitions()
        .find(|adapter| {
            adapter.spec().ambiguous_bin_identity(label).is_none()
                && adapter_matches_program(adapter, program)
        })
        .or_else(|| {
            (!from_launcher)
                .then(|| adapter_for_comm(program.program))
                .flatten()
        })
}

fn adapter_matches_program(
    adapter: &AgentDefinition,
    program: crate::proc::command::EffectiveProgram<'_>,
) -> bool {
    let definition = adapter.spec();
    let label = crate::proc::command::basename(program.program);
    let from_launcher = program.program != program.root_program
        && crate::proc::command::is_launcher(program.root_program);
    definition.launches_as(label)
        || (!from_launcher && definition.runs_as(label))
        || (from_launcher
            && crate::proc::command::agent_script_path_names_kind(program.program, definition.kind))
}

fn adapter_for_comm(comm: &str) -> Option<&'static AgentDefinition> {
    let comm = crate::proc::command::basename(comm.trim());
    if crate::proc::command::is_launcher(comm) {
        return None;
    }
    let mut matches = all_definitions().filter(|adapter| {
        let definition = adapter.spec();
        definition.ambiguous_bin_identity(comm).is_none() && definition.runs_as(comm)
    });
    let adapter = matches.next()?;
    matches.next().is_none().then_some(adapter)
}

/// Adapter-owned enrichment environment for a new room. Backends receive one
/// opaque map and remain independent of provider protocols.
pub(crate) fn room_env(runtime: &crate::disk::paths::RuntimePaths) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for adapter in all_definitions() {
        env.extend(adapter.room_env(runtime));
    }
    env
}

/// Dispatch a command line to the one adapter that recognizes its native
/// resume syntax. Multiple matches abstain rather than guessing identity.
pub(crate) fn resumed_session_id_from_cmdline(cmdline: &str) -> Option<AgentSessionId> {
    let mut matches =
        all_definitions().filter_map(|adapter| adapter.resumed_session_id_from_cmdline(cmdline));
    let session = matches.next()?;
    matches.next().is_none().then_some(session)
}

/// Find a resumed session in the pane root's shallow process chain, following
/// [`crate::proc::sole_agent_child`] at each step so sibling agents cannot donate
/// an unrelated session identity. Pass exec wrappers and wrapper prefixes, but
/// stop at the provider's own process without listing its children. A no-id
/// answer settles only when no other agent-classified wrapper was passed.
pub(crate) fn resumed_session_id_for_root(root_pid: u32) -> ResumeProbe {
    resume_probe_for_root_with(root_pid, &crate::proc::cmdline, &crate::proc::children)
}

#[cfg(test)]
fn resumed_session_id_for_root_with(
    root_pid: u32,
    cmdline: &dyn Fn(u32) -> Option<String>,
    children: &dyn Fn(u32) -> Vec<u32>,
) -> Option<AgentSessionId> {
    match resume_probe_for_root_with(root_pid, cmdline, children) {
        ResumeProbe::Resumed(session) => Some(session),
        ResumeProbe::Settled | ResumeProbe::Unsettled => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ResumeProbe {
    Resumed(AgentSessionId),
    Settled,
    Unsettled,
}

fn resume_probe_for_root_with(
    root_pid: u32,
    cmdline: &dyn Fn(u32) -> Option<String>,
    children: &dyn Fn(u32) -> Vec<u32>,
) -> ResumeProbe {
    let mut pid = root_pid;
    let mut command = cmdline(root_pid);
    let mut through_prefix = false;
    for _ in 0..=PROCESS_DESCENT_DEPTH {
        if let Some(command) = command.as_deref() {
            if let Some(session) = resumed_session_id_from_cmdline(command) {
                return ResumeProbe::Resumed(session);
            }
            match resume_hop(command) {
                Some(ResumeHop::Provider) => return provider_resume(command, through_prefix),
                Some(ResumeHop::Prefix) => through_prefix = true,
                Some(ResumeHop::ExecWrapper) | None => {}
            }
        }
        let Some((child, child_command)) = crate::proc::sole_agent_child(children(pid), cmdline)
        else {
            return ResumeProbe::Unsettled;
        };
        pid = child;
        command = Some(child_command);
    }
    ResumeProbe::Unsettled
}

/// The walk's answer at the provider's own process. A JS launcher hands its
/// script's argv to the agent, which may be a native child it spawns (Grok's npm
/// shim), so that argv is read as the agent's own.
fn provider_resume(command: &str, through_prefix: bool) -> ResumeProbe {
    if let Some(session) = crate::proc::command::launched_script_command(command)
        .and_then(resumed_session_id_from_cmdline)
    {
        return ResumeProbe::Resumed(session);
    }
    if through_prefix {
        ResumeProbe::Unsettled
    } else {
        ResumeProbe::Settled
    }
}

/// An agent-classified process on the resume walk: the provider's own process,
/// RimZ's exec wrapper, whose provider argv is fixed for its life, or another
/// prefix (`sudo`, `time`, a shell `-c` script) that may start a different
/// command after its current child.
enum ResumeHop {
    Provider,
    ExecWrapper,
    Prefix,
}

fn resume_hop(command: &str) -> Option<ResumeHop> {
    use crate::proc::command::{argv0_label, basename, effective_program_info, is_launcher};
    command_agent_kind(command)?;
    let program = effective_program_info(command);
    let own_program = argv0_label(command);
    if own_program == basename(program.program)
        || (is_launcher(own_program) && own_program == basename(program.root_program))
    {
        return Some(ResumeHop::Provider);
    }
    Some(if own_program == "rimz" {
        ResumeHop::ExecWrapper
    } else {
        ResumeHop::Prefix
    })
}

#[cfg(test)]
mod tests;
