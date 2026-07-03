//! Codex daemon-mode session reap cache.
//!
//! The refresh lane probes daemon PIDs and Codex's loaded-thread list on a TTL,
//! then the fold applies the published inputs without proc scans or app-server
//! reads.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::agents::AgentState;

use super::super::timing::CODEX_DAEMON_REAP_TTL;

/// Producer-published inputs for the Codex daemon ghost reaper. Consumers read
/// this cache so the fast lane can apply the same reap without proc scans or
/// app-server probes.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CodexDaemonReap {
    pub produced_at_ms: u64,
    pub daemon_pids: BTreeSet<u32>,
    pub loaded: Option<BTreeSet<String>>,
}

fn codex_daemon_reap_path(runtime: &RuntimePaths) -> PathBuf {
    runtime.root.join("codex-daemon-reap.json")
}

pub fn write_codex_daemon_reap(
    runtime: &RuntimePaths,
    cache: &CodexDaemonReap,
) -> crate::ledger::atomic::Result<()> {
    crate::ledger::atomic::write_temp_then_rename_cache(&codex_daemon_reap_path(runtime), cache)
}

pub fn read_codex_daemon_reap(runtime: &RuntimePaths) -> Option<CodexDaemonReap> {
    let bytes = std::fs::read(codex_daemon_reap_path(runtime)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub(crate) fn daemon_reap_due(cache: &Option<CodexDaemonReap>, now_ms: u64) -> bool {
    cache.as_ref().is_none_or(|cache| {
        now_ms.saturating_sub(cache.produced_at_ms) > CODEX_DAEMON_REAP_TTL.as_millis() as u64
    })
}

fn should_probe_codex_daemon_reap(agents: &[AgentState]) -> bool {
    agents.iter().any(|agent| {
        let daemon_hooked = crate::agents::descriptor_by_kind(agent.kind.as_str())
            .is_some_and(|descriptor| descriptor.capabilities.daemon_hooked_sessions);
        daemon_hooked && agent.parent_agent_id.is_none()
    })
}

pub(crate) fn refresh_codex_daemon_reap_cache(
    agents: &[AgentState],
    runtime: &RuntimePaths,
    now_ms: u64,
) -> CodexDaemonReap {
    let current = read_codex_daemon_reap(runtime);
    if !should_probe_codex_daemon_reap(agents) || !daemon_reap_due(&current, now_ms) {
        return current.unwrap_or_default();
    }
    let daemon_pids = crate::agents::codex::codex_daemon_pids();
    let loaded = if daemon_pids.is_empty() {
        None
    } else {
        crate::agents::codex::loaded_daemon_threads()
    };
    let inputs = CodexDaemonReap {
        produced_at_ms: now_ms,
        daemon_pids,
        loaded,
    };
    if let Err(err) = write_codex_daemon_reap(runtime, &inputs) {
        tracing::debug!(
            error = %err,
            "codex daemon reap cache write failed"
        );
    }
    inputs
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use jiff::Timestamp;

    use super::*;
    use crate::agents::spending::SpendingWalker;
    use crate::ids::WorkspaceId;
    use crate::sidebar::test_support::root_agent;
    use crate::{RuntimeOwner, RuntimeOwnerKind, SidebarSnapshot};

    #[test]
    fn daemon_reap_due_tracks_cache_ttl() {
        let ttl_ms = CODEX_DAEMON_REAP_TTL.as_millis() as u64;
        let now_ms = ttl_ms * 2 + 10;

        assert!(daemon_reap_due(&None, now_ms));
        assert!(!daemon_reap_due(
            &Some(CodexDaemonReap {
                produced_at_ms: now_ms.saturating_sub(ttl_ms),
                daemon_pids: BTreeSet::new(),
                loaded: None,
            }),
            now_ms
        ));
        assert!(daemon_reap_due(
            &Some(CodexDaemonReap {
                produced_at_ms: now_ms.saturating_sub(ttl_ms).saturating_sub(1),
                daemon_pids: BTreeSet::new(),
                loaded: None,
            }),
            now_ms
        ));
    }

    #[test]
    fn daemon_reap_probe_source_includes_pane_stamped_roots() {
        let mut codex = root_agent("codex", "pane-stamped", None);
        codex.pane = Some(crate::pane::PaneRef::from_id(
            crate::ids::PaneId::from_parts(crate::ids::MuxName::Tmux, "%1"),
        ));
        let mut sub = root_agent("codex", "sub", None);
        sub.parent_agent_id = Some("pane-stamped".into());

        assert!(should_probe_codex_daemon_reap(&[codex]));
        assert!(!should_probe_codex_daemon_reap(&[sub]));
    }

    #[test]
    fn refresh_uses_pre_reap_daemon_probe_source() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let messages = dir.path().join("messages");
        std::fs::create_dir_all(&messages).unwrap();

        let mut agent = root_agent("codex", "live-thread", None);
        agent.runtime_owner = Some(RuntimeOwner::new(
            RuntimeOwnerKind::Agent,
            "live-thread",
            77,
            None,
        ));
        let pre_reap = SidebarSnapshot::build_with_agents(
            workspace.clone(),
            Vec::new(),
            vec![agent],
            Timestamp::now(),
        );
        write_codex_daemon_reap(
            &runtime,
            &CodexDaemonReap {
                produced_at_ms: 1,
                daemon_pids: BTreeSet::from([77]),
                loaded: Some(BTreeSet::new()),
            },
        )
        .unwrap();

        let mut base = pre_reap.clone();
        let daemon_pids = BTreeSet::from([77]);
        let loaded = BTreeSet::new();
        base.reap_runtime(crate::ledger::snapshot::RuntimeReapInputs {
            daemon_pids: &daemon_pids,
            loaded: Some(&loaded),
            frame_panes: None,
            exclude_pane: None,
        });
        assert!(
            base.agents.is_empty(),
            "stale cache reaps the intermediate base"
        );

        let _ = super::super::refresh_heavy_lanes(
            &base,
            &pre_reap.agents,
            &messages,
            &runtime,
            &crate::config::MachineConfig::default(),
            &mut SpendingWalker::new(),
        );

        assert_ne!(
            read_codex_daemon_reap(&runtime)
                .expect("codex reap cache")
                .produced_at_ms,
            1,
            "the refresh probes from the unreaped CLI snapshot, not the reaped base"
        );
    }
}
