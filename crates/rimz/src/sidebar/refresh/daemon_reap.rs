//! Codex daemon-mode session reap cache.
//!
//! The refresh lane probes daemon PIDs and Codex's loaded-thread list when daemon-hooked sessions need reaping or the remote-control badge needs a health signal. Readers accept publications beyond the producer's re-probe TTL so replacement probes overlap the previous evidence. The fold applies the published inputs without proc scans or app-server reads.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::agents::AgentState;

use super::super::timing::{CODEX_DAEMON_REAP_STALE, CODEX_DAEMON_REAP_TTL};

/// Producer-published inputs for the Codex daemon ghost reaper. Consumers read
/// this cache so the fast lane can apply the same reap without proc scans or
/// app-server probes.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub(in crate::sidebar) struct CodexDaemonReap {
    pub produced_at_ms: u64,
    pub daemon_pids: BTreeSet<u32>,
    pub loaded: Option<BTreeSet<String>>,
}

pub(in crate::sidebar) fn codex_daemon_reap_path(runtime: &RuntimePaths) -> PathBuf {
    runtime.root.join("codex-daemon-reap.json")
}

fn write_codex_daemon_reap(
    runtime: &RuntimePaths,
    cache: &CodexDaemonReap,
) -> crate::disk::atomic::Result<()> {
    crate::disk::atomic::write_temp_then_rename_cache(&codex_daemon_reap_path(runtime), cache)
}

/// Read the publication within the reader's stale bound of `now_ms`; absent, unreadable, or stale records return `None`, so readers keep every session. Saturating age treats a clock-ahead stamp as fresh, like `snapshot_cache_is_fresh`.
pub(in crate::sidebar) fn read_codex_daemon_reap(
    runtime: &RuntimePaths,
    now_ms: u64,
) -> Option<CodexDaemonReap> {
    crate::disk::atomic::read_json_cache::<Option<CodexDaemonReap>>(&codex_daemon_reap_path(
        runtime,
    ))
    .filter(|cache| {
        now_ms.saturating_sub(cache.produced_at_ms) <= CODEX_DAEMON_REAP_STALE.as_millis() as u64
    })
}

fn daemon_reap_due(cache: &Option<CodexDaemonReap>, now_ms: u64) -> bool {
    cache.as_ref().is_none_or(|cache| {
        now_ms.saturating_sub(cache.produced_at_ms) > CODEX_DAEMON_REAP_TTL.as_millis() as u64
    })
}

fn should_probe_codex_daemon_reap(agents: &[AgentState], codex_rc_enabled: bool) -> bool {
    codex_rc_enabled
        || agents.iter().any(|agent| {
            let daemon_hooked = crate::agents::spec_by_kind(agent.kind.as_str())
                .is_some_and(|definition| definition.capabilities.daemon_hooked_sessions);
            daemon_hooked && !agent.is_provider_subagent()
        })
}

pub(super) fn refresh_codex_daemon_reap_cache(
    agents: &[AgentState],
    runtime: &RuntimePaths,
    logins: &crate::agents::RoomLoginSet,
    now_ms: u64,
    codex_rc_enabled: bool,
) {
    // Re-probe before the reader's stale bound so replacement probes overlap the previous publication.
    let current = crate::disk::atomic::read_json_cache(&codex_daemon_reap_path(runtime));
    if !should_probe_codex_daemon_reap(agents, codex_rc_enabled)
        || !daemon_reap_due(&current, now_ms)
    {
        return;
    }
    let Some(login) = logins.login("codex") else {
        return;
    };
    let login_env = logins.env(&login);
    let evidence = crate::agents::session::daemon_session_evidence("codex", &login_env);
    let inputs = CodexDaemonReap {
        produced_at_ms: now_ms,
        daemon_pids: evidence.pids,
        loaded: evidence.loaded_session_ids,
    };
    if let Err(err) = write_codex_daemon_reap(runtime, &inputs) {
        tracing::debug!(
            error = %err,
            "codex daemon reap cache write failed"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use jiff::Timestamp;

    use super::*;
    use crate::ids::WorkspaceId;
    use crate::sidebar::test_support::root_agent;
    use crate::store::snapshot::SidebarSnapshot;
    use crate::{RuntimeOwner, RuntimeOwnerKind};

    #[test]
    fn read_codex_daemon_reap_expires_past_stale_bound() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let ttl_ms = CODEX_DAEMON_REAP_TTL.as_millis() as u64;
        let stale_ms = CODEX_DAEMON_REAP_STALE.as_millis() as u64;
        let produced_at_ms = ttl_ms * 2 + 10;

        assert!(read_codex_daemon_reap(&runtime, produced_at_ms).is_none());
        write_codex_daemon_reap(
            &runtime,
            &CodexDaemonReap {
                produced_at_ms,
                ..Default::default()
            },
        )
        .unwrap();
        let due = read_codex_daemon_reap(&runtime, produced_at_ms + ttl_ms + 1);
        assert!(due.is_some());
        assert!(daemon_reap_due(&due, produced_at_ms + ttl_ms + 1));
        assert!(read_codex_daemon_reap(&runtime, produced_at_ms + stale_ms).is_some());
        assert!(read_codex_daemon_reap(&runtime, produced_at_ms + stale_ms + 1).is_none());
        assert!(read_codex_daemon_reap(&runtime, produced_at_ms - 1).is_some());
    }

    #[test]
    fn daemon_reap_due_tracks_cache_ttl() {
        let ttl_ms = CODEX_DAEMON_REAP_TTL.as_millis() as u64;
        let produced_at_ms = ttl_ms * 2;
        let cache = Some(CodexDaemonReap {
            produced_at_ms,
            ..Default::default()
        });
        assert!(daemon_reap_due(&None, produced_at_ms));
        assert!(!daemon_reap_due(&cache, produced_at_ms + ttl_ms));
        assert!(daemon_reap_due(&cache, produced_at_ms + ttl_ms + 1));
        assert!(!daemon_reap_due(&cache, produced_at_ms - 1));
    }

    #[test]
    fn daemon_reap_probe_source_includes_pane_stamped_roots() {
        let mut codex = root_agent("codex", "pane-stamped", None);
        codex.pane = Some(crate::pane::PaneRef::from_id(
            crate::ids::PaneId::from_parts(crate::ids::MuxName::Tmux, "%1"),
        ));
        let mut sub = root_agent("codex", "sub", None);
        sub.parent_agent_id = Some("pane-stamped".into());

        assert!(should_probe_codex_daemon_reap(&[codex], false));
        assert!(!should_probe_codex_daemon_reap(&[sub], false));
    }

    #[test]
    fn daemon_reap_probe_source_includes_remote_control_health() {
        assert!(should_probe_codex_daemon_reap(&[], true));
        assert!(!should_probe_codex_daemon_reap(&[], false));
    }

    #[test]
    fn refresh_uses_pre_reap_daemon_probe_source() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let state = crate::StatePaths::under(workspace.clone(), dir.path()).unwrap();
        state.ensure_dirs().unwrap();

        let mut agent = root_agent("codex", "live-thread", None);
        agent.runtime_owner = Some(RuntimeOwner::new(
            RuntimeOwnerKind::Agent,
            "live-thread",
            77,
            None,
        ));
        let pre_reap =
            SidebarSnapshot::build_with_agents(workspace.clone(), vec![agent], Timestamp::now());
        let produced_at_ms =
            crate::utils::time::unix_now_ms() - CODEX_DAEMON_REAP_TTL.as_millis() as u64 - 1;
        write_codex_daemon_reap(
            &runtime,
            &CodexDaemonReap {
                produced_at_ms,
                daemon_pids: BTreeSet::from([77]),
                loaded: Some(BTreeSet::new()),
            },
        )
        .unwrap();

        let mut base = pre_reap.clone();
        let inputs = read_codex_daemon_reap(&runtime, crate::utils::time::unix_now_ms())
            .expect("due publication still serves readers");
        base.reap_runtime(crate::store::snapshot::RuntimeReapInputs {
            daemon_pids: &inputs.daemon_pids,
            loaded: inputs.loaded.as_ref(),
            frame_panes: None,
            exclude_pane: None,
        });
        assert!(
            base.agents.is_empty(),
            "due publication reaps the intermediate base"
        );

        let _ = super::super::refresh_heavy_lanes(
            &base,
            &pre_reap.agents,
            &state,
            &runtime,
            &crate::config::MachineConfig::default(),
            crate::agents::spending::service::SpendingServiceStartup::OneShot,
            &mut Default::default(),
        );

        assert_ne!(
            read_codex_daemon_reap(&runtime, crate::utils::time::unix_now_ms())
                .expect("codex reap cache")
                .produced_at_ms,
            produced_at_ms,
            "the refresh probes from the unreaped CLI snapshot, not the reaped base"
        );
    }
}
