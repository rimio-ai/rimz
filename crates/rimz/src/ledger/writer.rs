//! The ledger write path: every mutation's lock → event-append
//! critical section, and the off-lock wakeup + publish tail that follows a
//! commit. The read side (snapshots, projections) stays in `mod.rs`; nothing
//! here is imported outside the ledger module.

use std::collections::BTreeSet;
use std::io;
use std::path::Path;
use std::time::Duration;

use crate::agents::LaunchParams;
use crate::ledger::event::{AgentLaunchPayload, EventEnvelope};
use crate::pane::RuntimeOwnerKind;
use crate::workspace::ResolvedWorkspace;

use super::{
    AgentLaunchAppend, AgentLaunchIdentity, AgentLaunchName, AgentLaunchRequest,
    EventLogRotationOutcome, Ledger, LedgerErr, Result, StatePaths, WorkspaceRewriteOutcome,
    event_log, lock, message_store, runtime, snapshot, workspace_record,
};

mod debounce;
mod publish;
mod queue;
mod reset;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PublishPolicy {
    Debounced,
    Forced,
    Skip,
}

pub(super) struct Txn<'a> {
    pub(super) paths: &'a StatePaths,
    events: Vec<EventEnvelope>,
    publish: PublishPolicy,
}

impl Txn<'_> {
    pub(super) fn append(&mut self, event: &EventEnvelope) -> Result<()> {
        event_log::append(&self.paths.events_log, event)?;
        self.events.push(event.clone());
        if self.publish == PublishPolicy::Skip {
            self.publish = PublishPolicy::Debounced;
        }
        Ok(())
    }

    pub(super) fn set_publish(&mut self, publish: PublishPolicy) {
        self.publish = publish;
    }
}

enum RollupInvalidation {
    Reseed,
    Drop,
}

fn remove_snapshot_file_if_exists(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(LedgerErr::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn invalidate_snapshot_caches(paths: &StatePaths, rollup: RollupInvalidation) -> Result<()> {
    // A swapped or cut event log voids offset-stamped fold bases. Retract the
    // published view before touching the rollup cache so a crash leaves readers
    // folding for themselves, never trusting an extent that can alias into the
    // fresh log after it regrows.
    remove_snapshot_file_if_exists(&paths.latest_snapshot)?;
    publish::retract_publish_stamp(paths);
    match rollup {
        RollupInvalidation::Reseed => snapshot::reseed_rollup_cache_for_rotation(paths)?,
        RollupInvalidation::Drop => remove_snapshot_file_if_exists(&paths.rollup_cache)?,
    }
    Ok(())
}

fn stage_agent_carryover_for_rotation(paths: &StatePaths, min_bytes: u64) -> Result<usize> {
    let current_bytes = match std::fs::metadata(&paths.events_log) {
        Ok(meta) => meta.len(),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => 0,
        Err(source) => {
            return Err(event_log::EventLogErr::Io {
                path: paths.events_log.clone(),
                source,
            }
            .into());
        }
    };
    if current_bytes == 0 || current_bytes < min_bytes {
        let existing = snapshot::read_carryover(&paths.agents_carryover)?;
        return Ok(existing.agents.len());
    }

    let (cache, merged_agents, resume_outcomes) = snapshot::catch_up_rollup(paths)?;
    let live_agents = runtime::RuntimeProjection::from_parts(
        cache.tombstones.iter().cloned().collect(),
        cache.lost.iter().cloned().collect(),
        merged_agents,
        runtime::RuntimeScope::Runtime,
    )
    .agents;
    let live_agents = prune_old_dead_agents(live_agents, event_log::DEFAULT_RETENTION);
    let carryover_agents = live_agents.len();
    snapshot::write_carryover(
        &paths.agents_carryover,
        &snapshot::EventCarryover {
            agents: live_agents,
            agent_identity: cache.agent_identity.without_consumed_launches(),
            resume_outcomes,
            lost: cache.lost,
        },
    )?;
    Ok(carryover_agents)
}

fn prune_old_dead_agents(
    agents: Vec<crate::agents::AgentState>,
    older_than: Duration,
) -> Vec<crate::agents::AgentState> {
    let cutoff = jiff::Timestamp::now() - older_than;
    agents
        .into_iter()
        .filter(|agent| {
            agent.last_seen >= cutoff
                || agent
                    .runtime_owner
                    .as_ref()
                    .is_some_and(runtime::owner_is_live)
        })
        .collect()
}

impl Ledger {
    fn commit<T>(
        &self,
        publish: PublishPolicy,
        f: impl FnOnce(&mut Txn<'_>) -> Result<T>,
    ) -> Result<T> {
        let (out, txn) = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            let mut txn = Txn {
                paths: &self.inner.paths,
                events: Vec::new(),
                publish,
            };
            let out = f(&mut txn)?;
            (out, txn)
        };

        for event in &txn.events {
            self.wake_sidebars_for_event_best_effort(event);
        }
        match txn.publish {
            PublishPolicy::Debounced => self.publish_snapshot_best_effort(),
            PublishPolicy::Forced => self.publish_snapshot_forced(),
            PublishPolicy::Skip if !txn.events.is_empty() => {
                debounce::sync_log_debounced(&self.inner.paths);
            }
            PublishPolicy::Skip => {}
        }
        Ok(out)
    }

    /// Persist the project-root index used by maintenance commands. This does
    /// not change agent state and does not wake sidebars.
    #[must_use = "durability barrier; check the result"]
    pub fn record_workspace(&self, workspace: &ResolvedWorkspace) -> Result<()> {
        self.commit(PublishPolicy::Skip, |txn| {
            let record = workspace_record::WorkspaceRecord::from_resolved(workspace);
            workspace_record::write(txn.paths, &record)?;
            Ok(())
        })
    }

    /// Rewrite durable workspace identity after a project root move.
    ///
    /// The caller has already moved the state directory to the new
    /// `<workspace_id>` path. This method updates event envelopes, the
    /// workspace metadata record, and the rebuilt snapshot under one workspace
    /// lock.
    #[must_use = "durability barrier; check the result"]
    pub fn rewrite_workspace_identity(
        &self,
        workspace: &ResolvedWorkspace,
    ) -> Result<WorkspaceRewriteOutcome> {
        let (messages_rewritten, events_rewritten) = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            // Also fence the snapshot publishers: this rewrite replaces the
            // caches in place, and a publisher mid-fold must not clobber
            // them. Ordering is workspace → publish; publishers take only
            // the publish lock, so the pair can never deadlock.
            let _publish_guard = lock::WorkspaceLock::acquire(&self.inner.paths.publish_lock)?;

            let mut messages = message_store::list(&self.inner.paths.messages_dir)?;
            let messages_rewritten = messages.len();
            for message in &mut messages {
                message.workspace_id = workspace.workspace_id.clone();
                message_store::write(&self.inner.paths.messages_dir, message)?;
            }

            let mut events = event_log::read_all(&self.inner.paths.events_log)?;
            let events_rewritten = events.len();
            for event in &mut events {
                event.workspace_id = workspace.workspace_id.clone();
            }
            event_log::replace_all(&self.inner.paths.events_log, &events)?;

            let record = workspace_record::WorkspaceRecord::from_resolved(workspace);
            workspace_record::write(&self.inner.paths, &record)?;
            // The log was wholesale-replaced; reseed fold caches before rebuilding.
            invalidate_snapshot_caches(&self.inner.paths, RollupInvalidation::Reseed)?;
            snapshot::rebuild(&self.inner.paths)?;

            (messages_rewritten, events_rewritten)
        };

        Ok(WorkspaceRewriteOutcome {
            workspace_id: workspace.workspace_id.clone(),
            messages_rewritten,
            events_rewritten,
        })
    }

    /// Append a freestanding event.
    #[must_use = "durability barrier; check the result"]
    pub fn append_event(&self, event: &EventEnvelope) -> Result<()> {
        self.commit(PublishPolicy::Skip, |txn| {
            txn.append(event)?;
            Ok(())
        })
    }

    /// Allocate final agent card identities from the durable agent fold and
    /// append their launch events under the same workspace lock.
    #[must_use = "durability barrier; check the result"]
    pub fn append_agent_launches_allocating(
        &self,
        requests: &[AgentLaunchRequest],
        append: &AgentLaunchAppend,
    ) -> Result<Vec<AgentLaunchIdentity>> {
        self.commit(PublishPolicy::Skip, |txn| {
            let (_cache, base_agents, _resume_outcomes) = snapshot::catch_up_rollup(txn.paths)?;
            let identities = allocate_agent_launch_identities(requests, &base_agents)?;
            let events = identities
                .iter()
                .map(|identity| agent_launch_event(append, identity))
                .collect::<Vec<_>>();
            for event in &events {
                txn.append(event)?;
            }
            Ok(identities)
        })
    }

    /// Rotate the active event log when it exceeds `min_bytes`, preserving
    /// the agent rollup across the archive boundary.
    ///
    /// Steps under the workspace and publish locks:
    /// 1. Project the current event log's agent rollup, merge it with the
    ///    existing carryover, and persist before the rename so a rotation
    ///    crash leaves both files coherent.
    /// 2. Rename the active log into `events.log.archive/`. UUIDv7 filenames
    ///    keep archives sorted chronologically without an external index.
    /// 3. Retract the published `latest.json` — its extent stamp describes
    ///    the renamed-away log, so a crash before the rebuild below leaves
    ///    readers folding for themselves rather than trusting a stamp that
    ///    could alias into the fresh log.
    /// 4. Reseed the rollup fold base as a new generation and rebuild the
    ///    persisted snapshot (`latest.json`) from the merged rollup so
    ///    neither depends on the rotated log.
    /// 5. Prune archives older than `archive_older_than` when set.
    #[must_use = "durability barrier; check the result"]
    pub fn rotate_event_log(
        &self,
        min_bytes: u64,
        archive_older_than: Option<Duration>,
    ) -> Result<EventLogRotationOutcome> {
        self.rotate_event_log_with(min_bytes, archive_older_than, event_log::rotate)
    }

    fn rotate_event_log_with<F>(
        &self,
        min_bytes: u64,
        archive_older_than: Option<Duration>,
        rotate: F,
    ) -> Result<EventLogRotationOutcome>
    where
        F: FnOnce(&Path, &Path, u64) -> event_log::Result<event_log::RotationOutcome>,
    {
        let outcome = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            // Fence the snapshot publishers across the rename + reseed, same
            // workspace → publish ordering as the identity rewrite.
            let _publish_guard = lock::WorkspaceLock::acquire(&self.inner.paths.publish_lock)?;

            let carryover_agents =
                stage_agent_carryover_for_rotation(&self.inner.paths, min_bytes)?;

            let rotation = rotate(
                &self.inner.paths.events_log,
                &self.inner.paths.events_archive_dir,
                min_bytes,
            )?;

            if rotation.is_rotated() {
                // The active log was swapped; reseed fold caches before rebuilding.
                invalidate_snapshot_caches(&self.inner.paths, RollupInvalidation::Reseed)?;
                snapshot::rebuild(&self.inner.paths)?;
            }

            let pruned = if let Some(older_than) = archive_older_than {
                event_log::prune_archive(&self.inner.paths.events_archive_dir, older_than)?
            } else {
                super::atomic::PruneOutcome::default()
            };
            EventLogRotationOutcome {
                rotation,
                pruned,
                carryover_agents,
            }
        };
        Ok(outcome)
    }

    #[must_use = "durability barrier; check the result"]
    pub fn prune_carryover(&self, older_than: Duration) -> Result<usize> {
        let removed = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            let _publish_guard = lock::WorkspaceLock::acquire(&self.inner.paths.publish_lock)?;
            let mut carryover = snapshot::read_carryover(&self.inner.paths.agents_carryover)?;
            let before = carryover.agents.len();
            carryover.agents = prune_old_dead_agents(carryover.agents, older_than);
            let removed = before.saturating_sub(carryover.agents.len());
            if removed > 0 {
                snapshot::write_carryover(&self.inner.paths.agents_carryover, &carryover)?;
                snapshot::rebuild(&self.inner.paths)?;
            }
            removed
        };
        Ok(removed)
    }

    /// Truncate the event log at its first invalid frame and republish the
    /// snapshot caches from what survives — the answer to a post-power-cut
    /// corpse (`rimz gc`, and the publish tail's self-heal). Locks in the
    /// canonical workspace → publish order, the same nesting rotation uses;
    /// an intact log is a read-only no-op.
    ///
    /// After a cut, both persisted fold bases are retracted before the
    /// rebuild: their extents describe bytes the truncation removed, and once
    /// the log regrows an offset-only stamp could alias into fresh frames —
    /// the same hazard rotation answers by retract-and-reseed. The rebuild
    /// re-folds the repaired log from zero and republishes both. An
    /// in-memory cursor heals itself: its offset either regresses (a
    /// reload), or its warm fold lands mid-frame in regrown bytes, fails the
    /// frame CRC, and retries cold.
    #[must_use = "durability barrier; check the result"]
    pub fn repair_event_log(&self) -> Result<event_log::RepairOutcome> {
        let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
        let _publish_guard = lock::WorkspaceLock::acquire(&self.inner.paths.publish_lock)?;
        let outcome = event_log::repair(&self.inner.paths.events_log)?;
        if outcome.truncated() {
            // The active log was cut; drop fold caches before rebuilding.
            invalidate_snapshot_caches(&self.inner.paths, RollupInvalidation::Drop)?;
            snapshot::rebuild(&self.inner.paths)?;
        }
        Ok(outcome)
    }
}

fn allocate_agent_launch_identities(
    requests: &[AgentLaunchRequest],
    agents: &[crate::agents::AgentState],
) -> Result<Vec<AgentLaunchIdentity>> {
    // Pet names are live-card handles, not permanent ids: ended cards release
    // them so long-lived rooms do not grow a retired-name set. Kind ordinals
    // stay monotonic in the reducer for history/script-stable references.
    let mut taken: BTreeSet<String> = agents
        .iter()
        .filter_map(|agent| agent.name.clone())
        .collect();
    let session_ids = agents
        .iter()
        .map(|agent| agent.agent_id.as_str())
        .collect::<Vec<_>>();
    let mut identities = Vec::with_capacity(requests.len());
    for request in requests {
        let name = match &request.name {
            AgentLaunchName::Explicit(name) => {
                validate_agent_launch_name(name)?;
                if name_taken(name, &taken, &session_ids) {
                    return Err(LedgerErr::AgentLaunchIdentity(format!(
                        "agent name `{name}` is already in use"
                    )));
                }
                name.clone()
            }
            AgentLaunchName::Soft(name)
                if valid_agent_launch_name(name) && !name_taken(name, &taken, &session_ids) =>
            {
                name.clone()
            }
            AgentLaunchName::Soft(_) | AgentLaunchName::Mint => {
                mint_available_agent_name(&taken, &session_ids)
            }
        };
        taken.insert(name.clone());
        identities.push(AgentLaunchIdentity {
            kind: request.kind.clone(),
            agent_id: request.agent_id.clone(),
            name,
            profile: request.profile.clone(),
            role: request.role.clone(),
            model: request.model.clone(),
            effort: request.effort.clone(),
            team: request.team.clone(),
            launch_group: request.launch_group.clone(),
            launch_ordinal: request.launch_ordinal,
            channel: request.channel.clone(),
            run_id: request.run_id.clone(),
        });
    }
    Ok(identities)
}

fn validate_agent_launch_name(name: &str) -> Result<()> {
    if valid_agent_launch_name(name) {
        Ok(())
    } else {
        Err(LedgerErr::AgentLaunchIdentity(format!(
            "invalid agent name `{name}`; use ASCII letters, numbers, and `-`"
        )))
    }
}

fn valid_agent_launch_name(name: &str) -> bool {
    crate::harness::petname::valid_name(name)
        && !crate::harness::petname::collides_with_reserved_prefix(
            name,
            crate::agents::known_kinds(),
        )
}

fn name_taken(name: &str, taken: &BTreeSet<String>, session_ids: &[&str]) -> bool {
    taken.contains(name) || session_ids.iter().any(|session| session.starts_with(name))
}

fn mint_available_agent_name(taken: &BTreeSet<String>, session_ids: &[&str]) -> String {
    loop {
        let candidate = crate::harness::petname::mint(taken.iter().map(String::as_str));
        if valid_agent_launch_name(&candidate) && !name_taken(&candidate, taken, session_ids) {
            return candidate;
        }
    }
}

fn agent_launch_event(append: &AgentLaunchAppend, identity: &AgentLaunchIdentity) -> EventEnvelope {
    let runtime_owner = append.pane_id.as_ref().map(|_| {
        runtime::current_process_owner(RuntimeOwnerKind::Agent, identity.agent_id.as_str())
    });
    EventEnvelope::agent_launched(
        append.workspace_id.clone(),
        append.session_name.clone(),
        &identity.kind,
        AgentLaunchPayload {
            agent_id: identity.agent_id.clone(),
            agent_name: identity.name.clone(),
            launch: LaunchParams {
                profile: identity.profile.clone(),
                role: identity.role.clone(),
                model: identity.model.clone(),
                effort: identity.effort.clone(),
                team: identity.team.clone(),
                launch_group: identity.launch_group.clone(),
                launch_ordinal: identity.launch_ordinal,
                channel: identity.channel.clone().or_else(|| append.channel.clone()),
                kind_ordinal: None,
            },
            state: append.state,
            run_id: identity.run_id.clone(),
            pane_id: append.pane_id.clone(),
            runtime_owner,
            worktree_path: Some(append.cwd.to_string_lossy().into_owned()),
            worktree_branch: append.worktree_name.clone(),
            prompt: append
                .prompt
                .as_deref()
                .filter(|prompt| !prompt.trim().is_empty())
                .map(ToOwned::to_owned),
            description: append
                .description
                .as_deref()
                .map(str::trim)
                .filter(|description| !description.is_empty())
                .map(ToOwned::to_owned),
        },
    )
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use serde_json::json;

    use super::*;
    use crate::agents::TurnPhase;
    use crate::agents::{AgentState, AgentStatus};
    use crate::ids::{AgentKind, AgentSessionId, WorkspaceId};
    use crate::ledger::event::MessageEventMethod;
    use crate::ledger::paths::{RuntimePaths, StatePaths};
    use crate::message::{DeliveryGate, MessageRecord, MessageStatus};

    #[test]
    fn rotate_event_log_writes_carryover_before_archiving_active_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).expect("state paths");
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime paths");
        let ledger = Ledger::open(paths.clone(), runtime).expect("open ledger");
        let mut message = MessageRecord::new(
            workspace_id.clone(),
            &agent_state("claude", "sess-resume", Some("lucid-atlas")),
            "continue".to_owned(),
            true,
            DeliveryGate::Resume,
        );
        message.status = MessageStatus::Delivered;
        event_log::append(
            &paths.events_log,
            &EventEnvelope::message_event(
                &message,
                "rimz-test",
                MessageEventMethod::Delivered,
                None,
            ),
        )
        .expect("seed resume event");

        let rotate_called = Cell::new(false);
        ledger
            .rotate_event_log_with(1, None, |events_log, archive_dir, min_bytes| {
                rotate_called.set(true);
                assert!(
                    paths.agents_carryover.exists(),
                    "rotation must persist carryover before archiving the only active-log copy"
                );
                let carryover =
                    snapshot::read_carryover(&paths.agents_carryover).expect("read carryover");
                assert_eq!(
                    carryover.resume_outcomes.len(),
                    1,
                    "rotation carryover must include terminal resume outcomes"
                );
                event_log::rotate(events_log, archive_dir, min_bytes)
            })
            .expect("rotate event log");

        assert!(rotate_called.get(), "test rotate hook should run");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rotation_carryover_prunes_dead_runtime_owner_agents() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).expect("state paths");
        let runtime_paths =
            RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime paths");
        let ledger = Ledger::open(paths.clone(), runtime_paths).expect("open ledger");
        let kind = AgentKind::new_unchecked("claude");
        let launch = |agent_id: &str, name: &str, pid| {
            EventEnvelope::agent_launched(
                workspace_id.clone(),
                "rimz-test",
                &kind,
                AgentLaunchPayload {
                    agent_id: AgentSessionId::from(agent_id),
                    agent_name: name.to_owned(),
                    launch: LaunchParams::default(),
                    state: crate::ledger::event::AgentLaunchState::Bound,
                    run_id: None,
                    pane_id: None,
                    runtime_owner: Some(runtime::process_owner(
                        RuntimeOwnerKind::Agent,
                        agent_id,
                        pid,
                    )),
                    worktree_path: Some(dir.path().to_string_lossy().into_owned()),
                    worktree_branch: Some("main".to_owned()),
                    prompt: Some("boot".to_owned()),
                    description: None,
                },
            )
        };
        event_log::append(
            &paths.events_log,
            &launch("sess-live", "lucid-atlas", std::process::id()),
        )
        .expect("append live launch");
        event_log::append(
            &paths.events_log,
            &launch("sess-dead", "solid-lumen", u32::MAX),
        )
        .expect("append dead launch");

        ledger.rotate_event_log(1, None).expect("rotate event log");

        let carryover = snapshot::read_carryover(&paths.agents_carryover).expect("read carryover");
        let ids: Vec<&str> = carryover
            .agents
            .iter()
            .map(|agent| agent.agent_id.as_str())
            .collect();
        assert!(
            ids.contains(&"sess-live"),
            "live-owner agent must survive rotation carryover: {ids:?}"
        );
        assert!(
            !ids.contains(&"sess-dead"),
            "dead-owner agent must be pruned from rotation carryover: {ids:?}"
        );
    }

    #[test]
    fn prune_carryover_drops_old_agents_without_live_owner() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).expect("state paths");
        let runtime = RuntimePaths::under(workspace_id, dir.path()).expect("runtime paths");
        let ledger = Ledger::open(paths.clone(), runtime).expect("open ledger");
        let mut old = agent_state("claude", "old", Some("lucid-atlas"));
        old.last_seen = jiff::Timestamp::now() - Duration::from_secs(30 * 86_400);
        old.last_activity = old.last_seen;
        let fresh = agent_state("claude", "fresh", Some("solid-lumen"));
        snapshot::write_carryover(
            &paths.agents_carryover,
            &snapshot::EventCarryover {
                agents: vec![old, fresh],
                agent_identity: Default::default(),
                resume_outcomes: Vec::new(),
                lost: Vec::new(),
            },
        )
        .expect("write carryover");

        let removed = ledger
            .prune_carryover(Duration::from_secs(14 * 86_400))
            .expect("prune carryover");

        assert_eq!(removed, 1);
        let carryover = snapshot::read_carryover(&paths.agents_carryover).expect("read carryover");
        let ids: Vec<&str> = carryover
            .agents
            .iter()
            .map(|agent| agent.agent_id.as_str())
            .collect();
        assert_eq!(ids, vec!["fresh"]);
    }

    #[test]
    fn rotation_carryover_drops_consumed_launch_tombstones() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).expect("state paths");
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime paths");
        let ledger = Ledger::open(paths.clone(), runtime).expect("open ledger");
        let kind = AgentKind::new_unchecked("claude");
        event_log::append(
            &paths.events_log,
            &EventEnvelope::agent_launched(
                workspace_id.clone(),
                "rimz-test",
                &kind,
                AgentLaunchPayload {
                    agent_id: AgentSessionId::from("launch_a"),
                    agent_name: "lucid-atlas".to_owned(),
                    launch: LaunchParams::default(),
                    state: crate::ledger::event::AgentLaunchState::Bound,
                    run_id: None,
                    pane_id: None,
                    runtime_owner: None,
                    worktree_path: Some(dir.path().to_string_lossy().into_owned()),
                    worktree_branch: Some("main".to_owned()),
                    prompt: Some("boot".to_owned()),
                    description: None,
                },
            ),
        )
        .expect("append launch");
        event_log::append(
            &paths.events_log,
            &EventEnvelope::new(
                workspace_id,
                "rimz-test",
                "claude",
                "agent-hook",
                "agent.lifecycle",
                json!({
                    "agent_id": "real-session",
                    "agent_name": "lucid-atlas",
                    "signal": { "signal": "registered" },
                }),
            ),
        )
        .expect("append lifecycle");

        ledger.rotate_event_log(1, None).expect("rotate event log");

        let carryover = std::fs::read_to_string(&paths.agents_carryover).expect("read carryover");
        assert!(carryover.contains("real-session"));
        assert!(
            !carryover.contains("consumed_launches"),
            "launch replay tombstones are active-log state and must not grow across rotations"
        );
    }

    #[test]
    fn launch_identity_allocation_rejects_explicit_live_name_or_session_prefix() {
        let agents = vec![
            agent_state("claude", "sess-live-alpha", Some("lucid-atlas")),
            agent_state("claude", "prefix-session", Some("solid-lumen")),
        ];
        let duplicate = AgentLaunchRequest {
            kind: AgentKind::new_unchecked("claude"),
            agent_id: AgentSessionId::from("launch_a"),
            name: AgentLaunchName::Explicit("lucid-atlas".to_owned()),
            profile: None,
            role: None,
            model: None,
            effort: None,
            team: None,
            launch_group: None,
            launch_ordinal: None,
            channel: None,
            run_id: None,
        };
        let prefix = AgentLaunchRequest {
            kind: AgentKind::new_unchecked("claude"),
            agent_id: AgentSessionId::from("launch_b"),
            name: AgentLaunchName::Explicit("prefix".to_owned()),
            profile: None,
            role: None,
            model: None,
            effort: None,
            team: None,
            launch_group: None,
            launch_ordinal: None,
            channel: None,
            run_id: None,
        };

        assert!(allocate_agent_launch_identities(&[duplicate], &agents).is_err());
        assert!(allocate_agent_launch_identities(&[prefix], &agents).is_err());
    }

    #[test]
    fn soft_launch_name_falls_back_when_it_collides() {
        let agents = vec![agent_state(
            "claude",
            "sess-live-alpha",
            Some("lucid-atlas"),
        )];
        let request = AgentLaunchRequest {
            kind: AgentKind::new_unchecked("claude"),
            agent_id: AgentSessionId::from("launch_a"),
            name: AgentLaunchName::Soft("lucid-atlas".to_owned()),
            profile: None,
            role: None,
            model: None,
            effort: None,
            team: None,
            launch_group: None,
            launch_ordinal: None,
            channel: None,
            run_id: None,
        };

        let identities = allocate_agent_launch_identities(&[request], &agents).unwrap();

        assert_eq!(identities.len(), 1);
        assert_ne!(identities[0].name, "lucid-atlas");
        assert!(valid_agent_launch_name(&identities[0].name));
    }

    fn agent_state(kind: &str, id: &str, name: Option<&str>) -> AgentState {
        let now = jiff::Timestamp::now();
        AgentState {
            agent_id: AgentSessionId::from(id),
            kind: AgentKind::new_unchecked(kind),
            name: name.map(ToOwned::to_owned),
            kind_ordinal: Some(1),
            profile: None,
            role: None,
            team: None,
            launch_group: None,
            launch_ordinal: None,
            channel: None,
            status: AgentStatus::Idle,
            phase: TurnPhase::Idle,
            pane: None,
            runtime_owner: None,
            parent_agent_id: None,
            worktree_path: None,
            worktree_branch: None,
            task: None,
            prompt: None,
            description: None,
            transcript_path: None,
            origin: None,
            recent_prompts: Vec::new(),
            model: None,
            effort: None,
            context_pct: None,
            context_window: None,
            total_tokens: None,
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
            context: None,
            subagent_description: None,
            subagent_started_at: None,
            turn_started_at: None,
            waiting_since: None,
            compacting_since: None,
            compaction_count: 0,
            last_compact_command_tokens: None,
            last_seen: now,
            last_activity: now,
            registered_at: Some(now),
        }
    }
}
