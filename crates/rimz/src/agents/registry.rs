//! The agent registry — the single registration point.
//!
//! Built-ins live in [`ADAPTERS`]; validated machine-tier process plugins join
//! them through [`all_adapters`]. Every behavioral dispatch site resolves
//! through this module, so no consumer grows a per-agent match arm.

use std::collections::BTreeMap;

use super::amp::AmpAdapter;
use super::antigravity::AntigravityAdapter;
use super::claude::ClaudeAdapter;
use super::codex::CodexAdapter;
use super::copilot::CopilotAdapter;
use super::cursor::CursorAdapter;
use super::descriptor::AgentDescriptor;
use super::droid::DroidAdapter;
use super::grok::GrokAdapter;
use super::kimi::KimiAdapter;
use super::kiro::KiroAdapter;
use super::opencode::OpencodeAdapter;
use super::pi::PiAdapter;
use super::qwen::QwenAdapter;
use super::{AgentAdapter, AgentErr, Result};
use crate::ids::AgentSessionId;

const PROCESS_DESCENT_DEPTH: usize = 8;

/// Every wired agent, in display order. `&'static dyn` — adapters are
/// zero-sized const values, so resolution never allocates.
pub static ADAPTERS: &[&'static dyn AgentAdapter] = &[
    &ClaudeAdapter,
    &CodexAdapter,
    &AmpAdapter,
    &CopilotAdapter,
    &KimiAdapter,
    &PiAdapter,
    &OpencodeAdapter,
    &AntigravityAdapter,
    &CursorAdapter,
    &DroidAdapter,
    &KiroAdapter,
    &QwenAdapter,
    &GrokAdapter,
];

/// Every built-in and valid machine-tier plugin adapter, in display order.
pub fn all_adapters() -> impl Iterator<Item = &'static dyn AgentAdapter> {
    ADAPTERS
        .iter()
        .copied()
        .chain(super::plugin::loaded().adapters.iter().copied())
}

/// Resolve an adapter for the `--source <agent>` CLI tag.
pub fn adapter_by_kind(kind: &str) -> Result<&'static dyn AgentAdapter> {
    find_adapter(kind).ok_or_else(|| AgentErr::Unknown(kind.to_owned()))
}

/// [`adapter_by_kind`] for callers that treat an unknown kind as absence.
pub fn find_adapter(kind: &str) -> Option<&'static dyn AgentAdapter> {
    all_adapters().find(|adapter| adapter.descriptor().kind == kind)
}

/// The static descriptor for `kind`, for sites that need only const data
/// (branding, capabilities, tool tables) without the behavioral trait.
pub fn descriptor_by_kind(kind: &str) -> Option<&'static AgentDescriptor> {
    find_adapter(kind).map(AgentAdapter::descriptor)
}

/// Display-order kinds — the walk doctor and the wiring probes iterate.
pub fn known_kinds() -> impl Iterator<Item = &'static str> {
    all_adapters().map(|adapter| adapter.descriptor().kind)
}

/// Agent kind for an interactive command, after shell syntax and process
/// wrappers are normalized by [`crate::proc::command`].
pub fn command_agent_kind(command: &str) -> Option<&'static str> {
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
            .then(|| adapter.descriptor().kind);
    }
    let adapter = comm.and_then(adapter_for_comm)?;
    let policy_command = (!command.trim().is_empty())
        .then_some(command)
        .or(comm)
        .unwrap_or_default();
    adapter
        .is_interactive_process(policy_command)
        .then(|| adapter.descriptor().kind)
}

fn adapter_for_program(
    program: crate::proc::command::EffectiveProgram<'_>,
) -> Option<&'static dyn AgentAdapter> {
    let label = crate::proc::command::basename(program.program);
    all_adapters()
        .find(|adapter| {
            let descriptor = adapter.descriptor();
            descriptor.launches_as(label)
                || (program.from_launcher
                    && crate::proc::command::agent_script_path_names_kind(
                        program.program,
                        descriptor.kind,
                    ))
        })
        .or_else(|| {
            (!program.from_launcher)
                .then(|| adapter_for_comm(program.program))
                .flatten()
        })
}

fn adapter_for_comm(comm: &str) -> Option<&'static dyn AgentAdapter> {
    let comm = crate::proc::command::basename(comm.trim());
    if crate::proc::command::is_launcher(comm) {
        return None;
    }
    let mut matches = all_adapters().filter(|adapter| adapter.descriptor().runs_as(comm));
    let adapter = matches.next()?;
    matches.next().is_none().then_some(adapter)
}

/// Adapter-owned enrichment environment for a new room. Backends receive one
/// opaque map and remain independent of provider protocols.
pub fn room_env(runtime: &crate::store::RuntimePaths) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for adapter in all_adapters() {
        env.extend(adapter.room_env(runtime));
    }
    env
}

/// Dispatch a command line to the one adapter that recognizes its native
/// resume syntax. Multiple matches abstain rather than guessing identity.
pub fn resumed_session_id_from_cmdline(cmdline: &str) -> Option<AgentSessionId> {
    let mut matches =
        all_adapters().filter_map(|adapter| adapter.resumed_session_id_from_cmdline(cmdline));
    let session = matches.next()?;
    matches.next().is_none().then_some(session)
}

/// Find a resumed session in the pane root's shallow single-child process
/// chain. Branching process trees abstain so sibling agents cannot donate an
/// unrelated session identity.
pub fn resumed_session_id_for_root(root_pid: u32) -> Option<AgentSessionId> {
    resumed_session_id_for_root_with(root_pid, &crate::proc::cmdline, &crate::proc::children)
}

fn resumed_session_id_for_root_with(
    root_pid: u32,
    cmdline: &dyn Fn(u32) -> Option<String>,
    children: &dyn Fn(u32) -> Vec<u32>,
) -> Option<AgentSessionId> {
    let mut pid = root_pid;
    for _ in 0..=PROCESS_DESCENT_DEPTH {
        if let Some(session) = cmdline(pid)
            .as_deref()
            .and_then(resumed_session_id_from_cmdline)
        {
            return Some(session);
        }
        let children = children(pid);
        let [child] = children.as_slice() else {
            return None;
        };
        pid = *child;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{ConcernCoverage, IntegrationConcern};

    #[test]
    fn registry_resolves_kinds_and_sub_providers_without_collisions() {
        // Every kind round-trips through resolution, an unknown kind errors, and
        // no two adapters claim the same kind.
        for adapter in ADAPTERS {
            let kind = adapter.descriptor().kind;
            assert_eq!(
                adapter_by_kind(kind).unwrap().descriptor().kind,
                kind,
                "registry round-trip for {kind}"
            );
        }
        assert!(adapter_by_kind("unknown-agent").is_err());

        let mut kinds: Vec<_> = known_kinds().collect();
        kinds.sort_unstable();
        let before = kinds.len();
        kinds.dedup();
        assert_eq!(kinds.len(), before, "duplicate kind in ADAPTERS");
    }

    #[test]
    fn command_agent_kind_combines_descriptors_with_process_policy() {
        for (command, expected) in [
            ("sudo npm install -g @openai/codex", None),
            ("sudo codex", Some("codex")),
            ("codex-aarch64-apple-darwin", Some("codex")),
            ("/usr/local/bin/codex-x86_64-apple-darwin", Some("codex")),
            ("claude", Some("claude")),
            ("agy", Some("antigravity")),
            ("antigravity", None),
            ("agent", Some("cursor")),
            ("cursor-agent", Some("cursor")),
            ("kiro-cli-chat", Some("kiro")),
            ("kiro-cli-term", None),
            ("node /usr/bin/codex", Some("codex")),
            ("node /opt/claude/cli.js", Some("claude")),
            ("node /tmp/claude-test/cli.js", None),
            ("node", None),
            ("node /srv/app/server.js", None),
            ("codex app-server", None),
            ("codex remote-control start", None),
            (
                "/home/me/.cargo/bin/rimz agents exec codex --worktree-path /repo/wt",
                Some("codex"),
            ),
            ("rimz agents exec unknown", None),
        ] {
            assert_eq!(command_agent_kind(command), expected, "{command}");
        }
        assert_eq!(
            command_agent_kind(
                "/home/u/.local/lib/qwen-code/node/bin/node --expose-gc /home/u/.local/lib/qwen-code/lib/cli.js"
            ),
            Some("qwen")
        );
    }

    #[test]
    fn command_agent_kind_uses_precise_comm_without_claiming_launchers() {
        for (comm, expected) in [
            ("claude", Some("claude")),
            ("codex-aarch64-a", Some("codex")),
            ("agy", Some("antigravity")),
            ("kiro-cli-chat", Some("kiro")),
            ("node", None),
            ("bun", None),
            ("zsh", None),
        ] {
            assert_eq!(command_agent_kind_with_comm("", Some(comm)), expected);
        }
    }

    #[test]
    fn every_adapter_exposes_a_manual_compaction_command() {
        // `--smart-compact` types this into the agent's composer; every wired
        // agent exposes a slash command (`/compact`, Cursor's `/summarize`), so
        // a new adapter that forgets to opt in fails
        // here rather than silently never compacting.
        for adapter in ADAPTERS {
            if let Some(command) = adapter.compact_command() {
                assert!(!command.is_empty() && command.starts_with('/'));
                continue;
            }
            assert!(
                matches!(
                    adapter
                        .descriptor()
                        .concern_coverage(IntegrationConcern::Compaction),
                    ConcernCoverage::Unsupported { .. }
                ),
                "missing compact command for {}",
                adapter.descriptor().kind
            );
        }
    }

    #[test]
    fn sub_providers_are_unique() {
        let mut providers: Vec<_> = ADAPTERS
            .iter()
            .flat_map(|adapter| adapter.descriptor().sub_providers)
            .collect();
        providers.sort_unstable();
        let before = providers.len();
        providers.dedup();
        assert_eq!(providers.len(), before, "sub provider claimed twice");
    }

    #[test]
    fn resume_dispatch_walks_single_child_chains_and_rejects_branches() {
        let session = "sess_11111111-1111-4111-8111-111111111111";
        assert_eq!(
            resumed_session_id_for_root_with(
                1,
                &|pid| (pid == 1).then(|| format!("kiro-cli-chat --resume-id={session}")),
                &|_| Vec::new(),
            )
            .as_deref(),
            Some(session)
        );
        assert_eq!(
            resumed_session_id_for_root_with(
                1,
                &|pid| match pid {
                    1 => Some("zsh".to_owned()),
                    2 => Some(
                        "kiro-cli-chat --resume-id=sess_11111111-1111-4111-8111-111111111111"
                            .to_owned()
                    ),
                    _ => None,
                },
                &|pid| (pid == 1).then_some(vec![2]).unwrap_or_default(),
            )
            .as_deref(),
            Some("sess_11111111-1111-4111-8111-111111111111")
        );
        assert_eq!(
            resumed_session_id_for_root_with(
                1,
                &|pid| match pid {
                    1 => Some("zsh".to_owned()),
                    2 => Some("chezmoi cd".to_owned()),
                    3 => Some("/bin/zsh".to_owned()),
                    4 => Some(format!("kiro-cli-chat --resume-id={session}")),
                    _ => None,
                },
                &|pid| match pid {
                    1 => vec![2],
                    2 => vec![3],
                    3 => vec![4],
                    _ => Vec::new(),
                },
            )
            .as_deref(),
            Some(session)
        );
        assert!(
            resumed_session_id_for_root_with(1, &|_| Some("zsh".to_owned()), &|pid| (pid == 1)
                .then_some(vec![2, 3])
                .unwrap_or_default(),)
            .is_none()
        );
    }
}
