//! The ledger write path: every mutation's lock → feed-write → event-append
//! critical section, and the off-lock wakeup + publish tail that follows a
//! commit. The read side (snapshots, projections) stays in `mod.rs`; nothing
//! here is imported outside the ledger module.

use std::time::Duration;

use crate::feed::FeedItem;
use crate::schema::event::EventEnvelope;
use crate::workspace::ResolvedWorkspace;

use super::{
    AskExpiry, EventLogRotationOutcome, Ledger, Result, WorkspaceRewriteOutcome, event_log,
    feed_store, lock, message_store, snapshot, workspace_record,
};

mod debounce;
mod expiry;
mod publish;
mod queue;
mod resolve;

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
        let outcome = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            // Fence the snapshot publishers across the rename + reseed, same
            // workspace → publish ordering as the identity rewrite.
            let _publish_guard = lock::WorkspaceLock::acquire(&self.inner.paths.publish_lock)?;

            let events = event_log::read_all(&self.inner.paths.events_log)?;
            let existing = snapshot::read_carryover(&self.inner.paths.agents_carryover)?;
            let merged = snapshot::agent_rollup_with_carryover(&events, existing.agents.clone());

            let rotation = event_log::rotate(
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
                let carryover = snapshot::EventCarryover { agents: merged };
                snapshot::write_carryover(&self.inner.paths.agents_carryover, &carryover)?;
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

            let carryover_agents = match &rotation {
                event_log::RotationOutcome::Rotated { .. } => {
                    snapshot::read_carryover(&self.inner.paths.agents_carryover)?
                        .agents
                        .len()
                }
                event_log::RotationOutcome::Skipped { .. } => existing.agents.len(),
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
