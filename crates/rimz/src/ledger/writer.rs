//! The ledger write path: every mutation's lock → feed-write → event-append
//! critical section, and the off-lock wakeup + publish tail that follows a
//! commit. The read side (snapshots, projections) stays in `mod.rs`; nothing
//! here is imported outside the ledger module.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use crate::feed::FeedItem;
use crate::feed::RuntimeOwnerKind;
use crate::schema::event::{AgentLaunchPayload, EventEnvelope};
use crate::workspace::ResolvedWorkspace;

use super::{
    AgentLaunchAppend, AgentLaunchIdentity, AgentLaunchName, AgentLaunchRequest, AskExpiry,
    EventLogRotationOutcome, Ledger, LedgerErr, Result, StatePaths, WorkspaceRewriteOutcome,
    event_log, feed_store, lock, message_store, runtime, snapshot, workspace_record,
};

mod debounce;
mod expiry;
mod publish;
mod queue;
mod reset;
mod resolve;

fn stage_agent_carryover_for_rotation(paths: &StatePaths, min_bytes: u64) -> Result<(bool, usize)> {
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
        return Ok((false, existing.agents.len()));
    }

    let (cache, merged_agents) = snapshot::catch_up_rollup(paths)?;
    let carryover_agents = merged_agents.len();
    snapshot::write_carryover(
        &paths.agents_carryover,
        &snapshot::EventCarryover {
            agents: merged_agents,
            agent_identity: cache.agent_identity.without_consumed_launches(),
        },
    )?;
    Ok((true, carryover_agents))
}

impl Ledger {
    /// Persist the project-root index used by maintenance commands. This does
    /// not change feed state and does not wake sidebars.
    #[must_use = "durability barrier; check the result"]
    pub fn record_workspace(&self, workspace: &ResolvedWorkspace) -> Result<()> {
        let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
        let record = workspace_record::WorkspaceRecord::from_resolved(workspace);
        workspace_record::write(&self.inner.paths, &record)?;
        Ok(())
    }

    /// Rewrite durable workspace identity after a project root move.
    ///
    /// The caller has already moved the state directory to the new
    /// `<workspace_id>` path. This method updates feed files, event envelopes,
    /// the workspace metadata record, and the rebuilt snapshot under one
    /// workspace lock.
    #[must_use = "durability barrier; check the result"]
    pub fn rewrite_workspace_identity(
        &self,
        workspace: &ResolvedWorkspace,
    ) -> Result<WorkspaceRewriteOutcome> {
        let (feed_items_rewritten, messages_rewritten, events_rewritten) = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            // Also fence the snapshot publishers: this rewrite replaces the
            // caches in place, and a publisher mid-fold must not clobber
            // them. Ordering is workspace → publish; publishers take only
            // the publish lock, so the pair can never deadlock.
            let _publish_guard = lock::WorkspaceLock::acquire(&self.inner.paths.publish_lock)?;

            let mut items = feed_store::list(&self.inner.paths.feed_dir)?;
            let feed_items_rewritten = items.len();
            for item in &mut items {
                item.workspace_id = workspace.workspace_id.clone();
                feed_store::write(&self.inner.paths.feed_dir, item)?;
            }

            let mut messages = message_store::list(&self.inner.paths.queue_dir)?;
            let messages_rewritten = messages.len();
            for message in &mut messages {
                message.workspace_id = workspace.workspace_id.clone();
                message_store::write(&self.inner.paths.queue_dir, message)?;
            }

            let mut events = event_log::read_all(&self.inner.paths.events_log)?;
            let events_rewritten = events.len();
            for event in &mut events {
                event.workspace_id = workspace.workspace_id.clone();
            }
            event_log::replace_all(&self.inner.paths.events_log, &events)?;

            let record = workspace_record::WorkspaceRecord::from_resolved(workspace);
            workspace_record::write(&self.inner.paths, &record)?;
            // The log was wholesale-replaced: every byte offset in the fold
            // base is void. Reseed it as a new generation before rebuilding.
            publish::retract_publish_stamp(&self.inner.paths);
            snapshot::reseed_rollup_cache_for_rotation(&self.inner.paths)?;
            snapshot::rebuild(&self.inner.paths)?;

            (feed_items_rewritten, messages_rewritten, events_rewritten)
        };

        Ok(WorkspaceRewriteOutcome {
            workspace_id: workspace.workspace_id.clone(),
            feed_items_rewritten,
            messages_rewritten,
            events_rewritten,
        })
    }

    /// Append a freestanding event (no feed item write).
    #[must_use = "durability barrier; check the result"]
    pub fn append_event(&self, event: &EventEnvelope) -> Result<()> {
        self.append_event_and_expire(event, None).map(|_| ())
    }

    /// Allocate final agent card identities from the current live rollup and
    /// append their launch events under the same workspace lock.
    #[must_use = "durability barrier; check the result"]
    pub fn append_agent_launches_allocating(
        &self,
        requests: &[AgentLaunchRequest],
        append: &AgentLaunchAppend,
    ) -> Result<Vec<AgentLaunchIdentity>> {
        let (abandoned, events, identities) = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            let abandoned =
                expiry::sweep_dead_owned_items_debounced(&self.inner.paths, &append.session_name)?;
            let snapshot = snapshot::build_from(&self.inner.paths)?;
            let identities = allocate_agent_launch_identities(requests, &snapshot.agents)?;
            let events = identities
                .iter()
                .map(|identity| agent_launch_event(append, identity))
                .collect::<Vec<_>>();
            for event in &events {
                event_log::append(&self.inner.paths.events_log, event)?;
            }
            (abandoned, events, identities)
        };
        for request_id in &abandoned {
            self.wake_sidebars_best_effort(request_id);
        }
        for event in &events {
            self.wake_sidebars_for_event_best_effort(event);
        }
        self.publish_snapshot_best_effort();
        Ok(identities)
    }

    /// Append a lifecycle event and expire the session's superseded pending
    /// asks under one lock cycle with one snapshot rebuild — the
    /// highest-cadence hook path (a turn boundary from every live agent)
    /// pays one flock acquire instead of two. `expiry` carries
    /// `(source, agent_id, scope)`; `None` is a plain append. Returns the
    /// number of asks expired.
    #[must_use = "durability barrier; check the result"]
    pub fn append_event_and_expire(
        &self,
        event: &EventEnvelope,
        expiry: Option<(&str, &str, AskExpiry)>,
    ) -> Result<usize> {
        let (abandoned, expired) = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            let abandoned =
                expiry::sweep_dead_owned_items_debounced(&self.inner.paths, &event.session_name)?;
            event_log::append(&self.inner.paths.events_log, event)?;
            let expired = match expiry {
                Some((source, agent_id, scope)) => expiry::expire_agent_asks_locked(
                    &self.inner.paths,
                    source,
                    agent_id,
                    &event.session_name,
                    scope,
                )?,
                None => Vec::new(),
            };
            (abandoned, expired)
        };
        for request_id in abandoned.iter().chain(expired.iter()) {
            self.wake_sidebars_best_effort(request_id);
        }
        self.wake_sidebars_for_event_best_effort(event);
        self.publish_snapshot_best_effort();
        Ok(expired.len())
    }

    /// Write a new feed item to disk, append a `feed.push` event, and rebuild
    /// the latest snapshot. The whole sequence is taken under the workspace
    /// lock so partial writes can't surface to the sidebar.
    #[must_use = "durability barrier; check the result"]
    pub fn push_feed_item(&self, item: &FeedItem, session_name: &str) -> Result<()> {
        self.push_feed_item_superseding(item, None, session_name)
    }

    /// [`Self::push_feed_item`] that also expires the session's prior
    /// native_ui asks in the same critical section — a fresh ask supersedes
    /// them before being pushed, under one lock cycle with one snapshot
    /// rebuild. `supersede` carries `(source, agent_id)`; `None` is a plain
    /// push.
    #[must_use = "durability barrier; check the result"]
    pub fn push_feed_item_superseding(
        &self,
        item: &FeedItem,
        supersede: Option<(&str, &str)>,
        session_name: &str,
    ) -> Result<()> {
        let (abandoned, expired) = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            let abandoned =
                expiry::sweep_dead_owned_items_debounced(&self.inner.paths, session_name)?;
            let expired = match supersede {
                Some((source, agent_id)) => expiry::expire_agent_asks_locked(
                    &self.inner.paths,
                    source,
                    agent_id,
                    session_name,
                    AskExpiry::MovedOn,
                )?,
                None => Vec::new(),
            };
            feed_store::write(&self.inner.paths.feed_dir, item)?;
            event_log::append(
                &self.inner.paths.events_log,
                &EventEnvelope::feed_pushed(item, session_name),
            )?;
            (abandoned, expired)
        };
        for request_id in abandoned.iter().chain(expired.iter()) {
            self.wake_sidebars_best_effort(request_id);
        }
        self.wake_sidebars_best_effort(&item.request_id);
        self.publish_snapshot_best_effort();
        Ok(())
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

            let (_will_rotate, carryover_agents) =
                stage_agent_carryover_for_rotation(&self.inner.paths, min_bytes)?;

            let rotation = rotate(
                &self.inner.paths.events_log,
                &self.inner.paths.events_archive_dir,
                min_bytes,
            )?;

            if rotation.is_rotated() {
                // Retract the published snapshot before anything else: its
                // extent stamp describes the renamed-away log, and the
                // freshness check compares offsets only. A crash anywhere
                // between here and the rebuild below must leave readers on
                // the fold-it-yourself path — never a stale stamp that could
                // alias once the fresh log regrows to the stamped length.
                match std::fs::remove_file(&self.inner.paths.latest_snapshot) {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(source) => {
                        return Err(event_log::EventLogErr::Io {
                            path: self.inner.paths.latest_snapshot.clone(),
                            source,
                        }
                        .into());
                    }
                }
                // The fresh log is a new generation: reseed the fold base at
                // offset zero with the generation bumped, so a reader's
                // pre-rotation extent can never alias into the new log.
                publish::retract_publish_stamp(&self.inner.paths);
                snapshot::reseed_rollup_cache_for_rotation(&self.inner.paths)?;
                snapshot::rebuild(&self.inner.paths)?;
            }

            let pruned = if let Some(older_than) = archive_older_than {
                event_log::prune_archive(&self.inner.paths.events_archive_dir, older_than)?
            } else {
                event_log::PruneOutcome::default()
            };

            EventLogRotationOutcome {
                rotation,
                pruned,
                carryover_agents,
            }
        };
        Ok(outcome)
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
            for stale in [
                &self.inner.paths.latest_snapshot,
                &self.inner.paths.rollup_cache,
            ] {
                match std::fs::remove_file(stale) {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(source) => {
                        return Err(event_log::EventLogErr::Io {
                            path: (*stale).clone(),
                            source,
                        }
                        .into());
                    }
                }
            }
            publish::retract_publish_stamp(&self.inner.paths);
            snapshot::rebuild(&self.inner.paths)?;
        }
        Ok(outcome)
    }
}

fn allocate_agent_launch_identities(
    requests: &[AgentLaunchRequest],
    agents: &[crate::feed::AgentState],
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
    crate::petname::valid_name(name)
        && !crate::petname::collides_with_reserved_prefix(name, crate::agents::known_kinds())
}

fn name_taken(name: &str, taken: &BTreeSet<String>, session_ids: &[&str]) -> bool {
    taken.contains(name) || session_ids.iter().any(|session| session.starts_with(name))
}

fn mint_available_agent_name(taken: &BTreeSet<String>, session_ids: &[&str]) -> String {
    loop {
        let candidate = crate::petname::mint(taken.iter().map(String::as_str));
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
            kind_ordinal: None,
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
        },
    )
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use serde_json::json;

    use super::*;
    use crate::agents::TurnPhase;
    use crate::feed::{AgentState, AgentStatus};
    use crate::ids::WorkspaceId;
    use crate::ids::{AgentKind, AgentSessionId};
    use crate::ledger::paths::{RuntimePaths, StatePaths};

    #[test]
    fn rotate_event_log_writes_carryover_before_archiving_active_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).expect("state paths");
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime paths");
        let ledger = Ledger::open(paths.clone(), runtime).expect("open ledger");
        event_log::append(
            &paths.events_log,
            &EventEnvelope::new(
                workspace_id,
                "rimz-test",
                "rimz",
                "cli",
                "test.event",
                json!({}),
            ),
        )
        .expect("seed event");

        let rotate_called = Cell::new(false);
        ledger
            .rotate_event_log_with(1, None, |events_log, archive_dir, min_bytes| {
                rotate_called.set(true);
                assert!(
                    paths.agents_carryover.exists(),
                    "rotation must persist carryover before archiving the only active-log copy"
                );
                event_log::rotate(events_log, archive_dir, min_bytes)
            })
            .expect("rotate event log");

        assert!(rotate_called.get(), "test rotate hook should run");
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
                    kind_ordinal: None,
                    state: crate::schema::event::AgentLaunchState::Bound,
                    run_id: None,
                    pane_id: None,
                    runtime_owner: None,
                    worktree_path: Some(dir.path().to_string_lossy().into_owned()),
                    worktree_branch: Some("main".to_owned()),
                    prompt: Some("boot".to_owned()),
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
            run_id: None,
        };
        let prefix = AgentLaunchRequest {
            kind: AgentKind::new_unchecked("claude"),
            agent_id: AgentSessionId::from("launch_b"),
            name: AgentLaunchName::Explicit("prefix".to_owned()),
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
            alias: None,
            status: AgentStatus::Idle,
            phase: TurnPhase::Idle,
            pane: None,
            agent_pid: None,
            agent_process_start: None,
            runtime_owner: None,
            parent_agent_id: None,
            worktree_path: None,
            worktree_branch: None,
            task: None,
            prompt: None,
            transcript_path: None,
            recent_prompts: Vec::new(),
            model: None,
            effort: None,
            context_pct: None,
            context_window: None,
            total_tokens: None,
            cache_read_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
            todo_done: None,
            todo_total: None,
            context: None,
            subagent_description: None,
            subagent_started_at: None,
            turn_started_at: None,
            compacting_since: None,
            compaction_count: 0,
            last_seen: now,
            last_activity: now,
            registered_at: Some(now),
        }
    }
}
