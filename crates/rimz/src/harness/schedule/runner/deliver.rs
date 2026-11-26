//! A delivery to a living session: the scope gates, the run lock, the deadline and a watch's
//! verdict, then the prepared wake the CLI sends, and its finish.

use super::*;

/// A delivery to a living session, which the CLI executes.
pub(super) struct Delivery {
    pub(super) target: TaskTarget,
    /// A delivery was prepared and awaits `finish`.
    pub(super) pending: bool,
}

pub(super) struct FireScope {
    scope_runtime: RuntimePaths,
    scope_state: StatePaths,
    /// The room's [`task_login`] for `kind`.
    login: Option<ProviderLogin>,
}

impl FireScope {
    /// The scope a delivery to `target` is gated in: the room the task's root persists.
    pub(super) fn resolve(entry: &TaskEntry, target: &TaskTarget) -> Result<Self> {
        let project_root = WorkspaceResolver::persisted_project_root(entry.resolved_root())?;
        let scope_runtime = RuntimePaths::for_project_root(&project_root)?;
        let login = task_login(
            &LaunchLogin::RoomDefault,
            target.kind.as_str(),
            &scope_runtime,
        )?;
        Ok(Self {
            scope_state: StatePaths::for_project_root(&project_root)?,
            scope_runtime,
            login,
        })
    }

    fn login_key(&self) -> Option<crate::ids::LoginKey> {
        self.login.as_ref().map(ProviderLogin::key)
    }
}

impl Fire<'_> {
    /// An unguarded wait: scope gates, the lease, the deadline and a watch's verdict, then the
    /// delivery to its living session.
    pub(super) fn prepare_delivery(&mut self, delivery: &mut Delivery) -> Result<TaskFirePlan> {
        let scope = FireScope::resolve(&self.entry, &delivery.target)?;
        if let Some((result, reason)) = self.scope_refusal(&scope) {
            return Ok(TaskFirePlan::Done(self.record_gate(result, reason)));
        }
        if let Some(done) = self.prepare_run_lock()? {
            return Ok(TaskFirePlan::Done(done));
        }
        if let Some(done) = self.prepare_deadline()? {
            return Ok(TaskFirePlan::Done(done));
        }
        if let Some(done) = self.prepare_watch()? {
            return Ok(TaskFirePlan::Done(done));
        }
        if let Some(done) = self.prepare_joined()? {
            return Ok(TaskFirePlan::Done(done));
        }
        let target = delivery.target.clone();
        if catalog::delivery_holder(&self.entry.resolved_root(), &target)?.is_none() {
            if self.mode == LoopRunMode::Scheduled {
                self.remove_schedule()?;
            }
            let handle = target.handle;
            return Ok(TaskFirePlan::Done(self.record_terminal_with(
                LoopRunResult::TargetGone,
                LoopRunPresentation::default(),
                TaskFireNotice::TargetGone {
                    handle: handle.clone(),
                },
                None,
                |record| {
                    record.target = Some(handle);
                },
            )));
        }
        let prompt = self.effect_prompt(self.entry.prompt.clone().unwrap_or_default());
        // The one-shot row is consumed in `finish`, once the wake's message
        // record exists: turn-completion waits read the row, then the queue,
        // so the agent never looks rested between the two.
        delivery.pending = true;
        Ok(TaskFirePlan::Deliver(PreparedDelivery {
            root: self.entry.resolved_root(),
            target,
            prompt,
            supersedes: self.superseded_check_ins(),
            intent: if self
                .task
                .trigger()
                .as_ref()
                .is_ok_and(|parsed| matches!(parsed.trigger, Trigger::Signal { .. }))
            {
                DeliveryIntent::Signal
            } else if self.entry.wait_meta.is_some() {
                DeliveryIntent::SelfWait
            } else {
                DeliveryIntent::Wait
            },
        }))
    }

    /// A shell's terminal outcome a live join claims: the row is consumed and the outcome
    /// recorded as joined, and no message is sent. A check-in passes on, claim or not.
    fn prepare_joined(&mut self) -> Result<Option<TaskFireFinished>> {
        let terminal = self.watch_spec().is_some()
            && self
                .watch_outcome()
                .is_some_and(|watch| watch.verdict.is_terminal());
        if !terminal
            || self.mode != LoopRunMode::Scheduled
            || !self.catalog.consume_joined(&self.name, Timestamp::now())?
        {
            return Ok(None);
        }
        let supersedes = self.superseded_check_ins();
        let mut finished = self.record_terminal_with(
            LoopRunResult::Joined,
            LoopRunPresentation::default(),
            TaskFireNotice::None,
            None,
            |_| {},
        );
        finished.supersedes = supersedes;
        Ok(Some(finished))
    }

    /// The first scope gate that refuses a delivery, in ladder order; every one reads the
    /// room's login.
    fn scope_refusal(&self, scope: &FireScope) -> Option<(LoopRunResult, String)> {
        let key = scope.login_key();
        if let Some(reason) = crate::harness::budget::scope_gate(
            &scope.scope_runtime,
            &scope.scope_state,
            key.as_ref(),
            &self.config,
            self.now,
        ) {
            return Some((LoopRunResult::BudgetSkipped, reason));
        }
        None
    }

    pub(super) fn finish_delivery(
        &mut self,
        target: &TaskTarget,
        effect: TaskFireEffect,
    ) -> Result<TaskFireFinished> {
        match effect {
            TaskFireEffect::Delivered(message_id) => {
                self.consume_ephemeral()?;
                let handle = target.handle.clone();
                Ok(self.record_terminal_with(
                    LoopRunResult::Delivered,
                    LoopRunPresentation::default(),
                    TaskFireNotice::None,
                    None,
                    |record| {
                        record.target = Some(handle);
                        record.message_id = Some(message_id);
                    },
                ))
            }
            TaskFireEffect::TargetGone => {
                if self.mode == LoopRunMode::Scheduled {
                    self.remove_schedule()?;
                }
                let handle = target.handle.clone();
                Ok(self.record_terminal_with(
                    LoopRunResult::TargetGone,
                    LoopRunPresentation::default(),
                    TaskFireNotice::TargetGone {
                        handle: handle.clone(),
                    },
                    None,
                    |record| {
                        record.target = Some(handle.clone());
                    },
                ))
            }
        }
    }

    /// Message ids of this arming's earlier check-ins, which a shell's or a self wait's final
    /// fire makes stale (a fire with a terminal verdict, or a condition's with none); empty
    /// for a check-in and every other delivery.
    fn superseded_check_ins(&self) -> Vec<MessageId> {
        let checked_in = self.watch_spec().is_some() || self.entry.wait_meta.is_some();
        let final_fire = self
            .watch_outcome()
            .is_none_or(|outcome| outcome.verdict.is_terminal());
        if !checked_in || !final_fire {
            return Vec::new();
        }
        let armed_at = self.entry.wait_meta.as_ref().map(|meta| meta.armed_at);
        run_log::task_records(
            &logs_dir(),
            &self.task_id,
            Some(&self.entry.resolved_root()),
        )
        .into_iter()
        .filter(|record| {
            record.result == LoopRunResult::Delivered
                && record
                    .watch
                    .as_ref()
                    .is_some_and(|watch| !watch.is_terminal())
                && armed_at.is_none_or(|armed_at| record.at >= armed_at)
        })
        .filter_map(|record| record.message_id)
        .collect()
    }
}
