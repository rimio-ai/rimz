//! The store write path: every mutation's lock → event-append
//! critical section, and the off-lock wakeup + publish tail that follows a
//! commit. The read side (snapshots, projections) stays in `mod.rs`; nothing
//! here is imported outside the store module.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(test)]
use crate::agents::LaunchParams;
use crate::pane::RuntimeOwnerKind;
use crate::store::event::{
    AgentAttachPayload, AgentLaunchPayload, AgentLaunchState, EventEnvelope,
};
use crate::workspace::ResolvedWorkspace;

use super::{
    AgentLaunchBatch, AgentLaunchIdentity, AgentLaunchName, AgentLaunchRequest, AgentLaunchScope,
    EventLogRotationOutcome, Result, StatePaths, Store, StoreErr, WorkspaceRewriteOutcome,
    event_log, lock, message_store, runtime, snapshot, workspace_record,
};

mod debounce;
mod lifecycle;
mod publish;
mod queue;
mod reap;
mod reset;

pub use lifecycle::{AgentLifecycleIntent, AgentLifecycleReceipt, DEFAULT_EVENT_LOG_ROTATE_BYTES};
pub(crate) use queue::DeliverySweepUpdate;
pub use queue::{DeliveryAck, DeliveryFailureDisposition, EditOutcome, MessageEdit};

pub(super) struct Txn<'a> {
    pub(super) paths: &'a StatePaths,
    events: Vec<EventEnvelope>,
    force_publish: bool,
}

impl Txn<'_> {
    pub(super) fn append(&mut self, event: &EventEnvelope) -> Result<()> {
        event_log::append(&self.paths.events_log, event)?;
        self.events.push(event.clone());
        Ok(())
    }

    pub(super) fn append_batch(&mut self, events: &[EventEnvelope]) -> Result<()> {
        event_log::append_batch(&self.paths.events_log, events)?;
        self.events.extend_from_slice(events);
        Ok(())
    }

    pub(super) fn force_publish(&mut self) {
        self.force_publish = true;
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
        Err(source) => Err(StoreErr::Io {
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

/// Preserve every agent within retention across log rotation, including ended rows.
///
/// Rotation and soft reset are storage boundaries, so they keep the audit
/// rollup's resumable identity even when an agent's runtime owner has exited.
/// A hard reset remains the explicit forget boundary.
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
    let retained_agents = prune_old_dead_agents(merged_agents, event_log::DEFAULT_RETENTION);
    let carryover_agents = retained_agents.len();
    snapshot::write_carryover(
        &paths.agents_carryover,
        &snapshot::EventCarryover {
            agents: retained_agents,
            agent_identity: cache.agent_identity.without_consumed_launches(),
            resume_outcomes,
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

impl Store {
    fn commit<T>(&self, f: impl FnOnce(&mut Txn<'_>) -> Result<T>) -> Result<T> {
        let (out, txn) = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            let mut txn = Txn {
                paths: &self.inner.paths,
                events: Vec::new(),
                force_publish: false,
            };
            let out = f(&mut txn)?;
            (out, txn)
        };

        for event in &txn.events {
            self.wake_sidebars_for_event_best_effort(event);
        }
        let ran_publish_tail = if txn.force_publish {
            self.publish_snapshot_forced();
            true
        } else if !txn.events.is_empty() {
            self.publish_snapshot_best_effort();
            true
        } else {
            false
        };
        if ran_publish_tail {
            self.reap_dead_sessions_if_due();
        }
        Ok(out)
    }

    /// Persist the project-root index used by maintenance commands. This does
    /// not change agent state or wake sidebars, and republishes the snapshot
    /// when identity-visible record fields change.
    #[must_use = "durability barrier; check the result"]
    pub fn record_workspace(&self, workspace: &ResolvedWorkspace) -> Result<()> {
        self.commit(|txn| {
            let prior = workspace_record::read(&txn.paths.workspace_record).ok();
            let record = workspace_record_preserving_rimz_target(prior.as_ref(), workspace, None);
            if prior.as_ref().is_none_or(|prior| {
                prior.project_root != record.project_root
                    || prior.session_name != record.session_name
                    || prior.root_class != record.root_class
            }) {
                txn.force_publish();
            }
            workspace_record::write(txn.paths, &record)?;
            Ok(())
        })
    }

    /// Persist the room-owning RimZ binary for session-local helpers. Generic
    /// re-records preserve this value; only room owner flows update it.
    #[must_use = "durability barrier; check the result"]
    pub fn record_room_bin(
        &self,
        workspace: &ResolvedWorkspace,
        rimz_bin: PathBuf,
        rimz_build: String,
    ) -> Result<()> {
        self.commit(|txn| {
            let prior = workspace_record::read(&txn.paths.workspace_record).ok();
            let room_bin_target = rimz_bin.clone();
            let record = workspace_record_preserving_rimz_target(
                prior.as_ref(),
                workspace,
                Some((rimz_bin, rimz_build)),
            );
            if prior.as_ref().is_none_or(|prior| {
                prior.project_root != record.project_root
                    || prior.session_name != record.session_name
                    || prior.root_class != record.root_class
            }) {
                txn.force_publish();
            }
            workspace_record::write(txn.paths, &record)?;
            crate::store::atomic::link_executable_atomically(&room_bin_target, &txn.paths.room_bin)
                .map_err(workspace_record::WorkspaceRecordErr::from)?;
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
            }
            message_store::replace_all(&self.inner.paths.messages_dir, &messages)?;

            let mut events = event_log::read_all(&self.inner.paths.events_log)?;
            let events_rewritten = events.len();
            for event in &mut events {
                event.workspace_id = workspace.workspace_id.clone();
            }
            event_log::replace_all(&self.inner.paths.events_log, &events)?;

            let prior = workspace_record::read(&self.inner.paths.workspace_record).ok();
            let record = workspace_record_preserving_rimz_target(prior.as_ref(), workspace, None);
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
        self.commit(|txn| {
            txn.append(event)?;
            Ok(())
        })
    }

    /// Allocate final agent card identities from the durable agent fold and
    /// append their launch events under the same workspace lock.
    #[must_use = "durability barrier; check the result"]
    pub fn begin_agent_launch_batch(
        &self,
        requests: &[AgentLaunchRequest],
        scope: AgentLaunchScope,
    ) -> Result<AgentLaunchBatch> {
        self.commit(|txn| {
            let (_cache, base_agents, _resume_outcomes) = snapshot::catch_up_rollup(txn.paths)?;
            let identities = allocate_agent_launch_identities(requests, &base_agents)?;
            let events = identities
                .iter()
                .map(|identity| {
                    self.agent_launch_event(
                        identity,
                        AgentLaunchState::Starting,
                        &scope.session_name,
                        &scope.cwd,
                        scope.worktree_name.as_deref(),
                        scope.channel.as_deref(),
                        scope.description.as_deref(),
                        None,
                    )
                })
                .collect::<Vec<_>>();
            for event in &events {
                txn.append(event)?;
            }
            Ok(AgentLaunchBatch { identities, scope })
        })
    }

    /// Mark every identity in a same-process launch batch failed. Each identity
    /// commits in slice order so partial-failure and wake behavior match the
    /// original launch transitions.
    #[must_use = "durability barrier; check the result"]
    pub fn fail_agent_launch_batch(&self, batch: &AgentLaunchBatch) -> Result<()> {
        self.fail_agent_launch_batch_with(batch, Store::fail_agent_launch_in_scope)
    }

    fn fail_agent_launch_batch_with(
        &self,
        batch: &AgentLaunchBatch,
        mut fail: impl FnMut(&Store, &AgentLaunchIdentity, &AgentLaunchScope) -> Result<()>,
    ) -> Result<()> {
        for identity in &batch.identities {
            fail(self, identity, &batch.scope)?;
        }
        Ok(())
    }

    fn fail_agent_launch_in_scope(
        &self,
        identity: &AgentLaunchIdentity,
        scope: &AgentLaunchScope,
    ) -> Result<()> {
        self.commit(|txn| {
            txn.append(&self.agent_launch_event(
                identity,
                AgentLaunchState::Failed,
                &scope.session_name,
                &scope.cwd,
                scope.worktree_name.as_deref(),
                scope.channel.as_deref(),
                None,
                None,
            ))
        })
    }

    /// Bind one provisional launch to the pane observed by its wrapper.
    #[must_use = "durability barrier; check the result"]
    pub fn bind_agent_launch(
        &self,
        identity: &AgentLaunchIdentity,
        session_name: &str,
        cwd: &Path,
        pane_id: &crate::ids::PaneId,
    ) -> Result<()> {
        self.commit(|txn| {
            txn.append(&self.agent_launch_event(
                identity,
                AgentLaunchState::Bound,
                session_name,
                cwd,
                None,
                None,
                None,
                Some(pane_id),
            ))
        })
    }

    /// Bind one resumed session to the pane its wrapper occupies. Placement
    /// evidence only: the fold moves `pane` and `runtime_owner` and nothing else.
    #[must_use = "durability barrier; check the result"]
    pub fn attach_agent_pane(
        &self,
        kind: &crate::ids::AgentKind,
        agent_id: &crate::ids::AgentSessionId,
        session_name: &str,
        pane_id: &crate::ids::PaneId,
    ) -> Result<()> {
        let runtime_owner =
            runtime::current_process_owner(RuntimeOwnerKind::Agent, agent_id.as_str());
        self.commit(|txn| {
            txn.append(&EventEnvelope::agent_attached(
                self.inner.paths.workspace_id.clone(),
                session_name,
                kind,
                AgentAttachPayload {
                    agent_id: agent_id.clone(),
                    pane_id: pane_id.clone(),
                    pane_pid: Some(std::process::id()),
                    runtime_owner,
                },
            ))
        })
    }

    /// Mark one provisional launch failed across a wrapper or restart process
    /// boundary, retaining only evidence available in that process.
    #[must_use = "durability barrier; check the result"]
    pub fn fail_agent_launch(
        &self,
        identity: &AgentLaunchIdentity,
        session_name: &str,
        cwd: &Path,
    ) -> Result<()> {
        self.commit(|txn| {
            txn.append(&self.agent_launch_event(
                identity,
                AgentLaunchState::Failed,
                session_name,
                cwd,
                None,
                None,
                None,
                None,
            ))
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn agent_launch_event(
        &self,
        identity: &AgentLaunchIdentity,
        state: AgentLaunchState,
        session_name: &str,
        cwd: &Path,
        worktree_name: Option<&str>,
        scope_channel: Option<&str>,
        description: Option<&str>,
        pane_id: Option<&crate::ids::PaneId>,
    ) -> EventEnvelope {
        let runtime_owner = pane_id.map(|_| {
            runtime::current_process_owner(RuntimeOwnerKind::Agent, identity.agent_id.as_str())
        });
        let mut launch = identity.launch.clone();
        launch.channel = launch
            .channel
            .or_else(|| scope_channel.map(ToOwned::to_owned));
        EventEnvelope::agent_launched(
            self.inner.paths.workspace_id.clone(),
            session_name,
            &identity.kind,
            AgentLaunchPayload {
                agent_id: identity.agent_id.clone(),
                launch_id: Some(identity.agent_id.clone()),
                agent_name: identity.name.clone(),
                agent_name_explicit: identity.name_explicit,
                launch,
                state,
                run_id: identity.run_id.clone(),
                pane_id: pane_id.cloned(),
                runtime_owner,
                worktree_path: Some(cwd.to_string_lossy().into_owned()),
                worktree_branch: worktree_name.map(ToOwned::to_owned),
                prompt: identity
                    .prompt
                    .as_deref()
                    .filter(|prompt| !prompt.trim().is_empty())
                    .map(ToOwned::to_owned),
                description: description
                    .map(str::trim)
                    .filter(|description| !description.is_empty())
                    .map(ToOwned::to_owned),
            },
        )
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

fn workspace_record_preserving_rimz_target(
    prior: Option<&workspace_record::WorkspaceRecord>,
    workspace: &ResolvedWorkspace,
    rimz_target: Option<(PathBuf, String)>,
) -> workspace_record::WorkspaceRecord {
    let mut record = workspace_record::WorkspaceRecord::from_resolved(workspace);
    match rimz_target {
        Some((rimz_bin, rimz_build)) => {
            record.rimz_bin = Some(rimz_bin);
            record.rimz_build = Some(rimz_build);
        }
        None => {
            if let Some(prior) = prior {
                record.rimz_bin.clone_from(&prior.rimz_bin);
                record.rimz_build.clone_from(&prior.rimz_build);
            }
        }
    }
    record
}

fn allocate_agent_launch_identities(
    requests: &[AgentLaunchRequest],
    agents: &[crate::agents::AgentState],
) -> Result<Vec<AgentLaunchIdentity>> {
    // Retained ended rows keep their names reserved so an address stays
    // unambiguous until rotation prunes the row at the retention boundary.
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
        let name_explicit = matches!(&request.name, AgentLaunchName::Explicit(_));
        let name = match &request.name {
            AgentLaunchName::Explicit(name) => {
                validate_agent_launch_name(name)?;
                if name_taken(name, &taken, &session_ids) {
                    return Err(StoreErr::AgentLaunchIdentity(format!(
                        "agent name `{name}` is already in use"
                    )));
                }
                name.clone()
            }
            AgentLaunchName::Soft(name)
                if crate::harness::petname::valid_agent_name(name)
                    && !name_taken(name, &taken, &session_ids) =>
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
            name_explicit,
            launch: request.launch.clone(),
            run_id: request.run_id.clone(),
            prompt: request.prompt.clone(),
        });
    }
    Ok(identities)
}

fn validate_agent_launch_name(name: &str) -> Result<()> {
    if crate::harness::petname::valid_agent_name(name) {
        Ok(())
    } else {
        Err(StoreErr::AgentLaunchIdentity(format!(
            "invalid agent name `{name}`; use ASCII letters, numbers, and `-`"
        )))
    }
}

fn name_taken(name: &str, taken: &BTreeSet<String>, session_ids: &[&str]) -> bool {
    taken.contains(name) || session_ids.iter().any(|session| session.starts_with(name))
}

fn mint_available_agent_name(taken: &BTreeSet<String>, session_ids: &[&str]) -> String {
    loop {
        let candidate = crate::harness::petname::mint(taken.iter().map(String::as_str));
        if crate::harness::petname::valid_agent_name(&candidate)
            && !name_taken(&candidate, taken, session_ids)
        {
            return candidate;
        }
    }
}

#[cfg(test)]
mod tests;
