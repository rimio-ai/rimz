//! A workflow, agent or team fire: the gates in front of the lease, admission, overlap by the
//! last launch's aliveness or a hand-started run, the launch child.

use super::*;

/// A workflow, agent or team fire: a launch child running the row's launch.
pub(super) struct Launch {
    pub(super) launch: TaskLaunch,
}

impl Fire<'_> {
    /// A workflow, agent or team fire: the ladder of a command fire, overlap by whether the
    /// checkout's last fire's launch is alive (`allow` passes it), then the launch child and the
    /// outcome its kind gives.
    pub(super) fn prepare_launch(
        &mut self,
        launch: &mut Launch,
        before_launch: &mut dyn FnMut(&Path) -> Result<()>,
    ) -> Result<TaskFirePlan> {
        if let Some(plan) = self.before_lease()? {
            return Ok(plan);
        }
        if let Some(plan) = self.admit(before_launch)? {
            return Ok(plan);
        }
        let state = StatePaths::for_project_root(&self.entry.resolved_root())?;
        if self.entry.overlap != Some(OverlapPolicy::Allow)
            && let Some(blocker) = self.alive_launch(&state, launch.launch.kind)?
        {
            return self.prepare_overlap(&blocker);
        }
        let workflow = launch.launch.kind == LaunchKind::Workflow;
        if workflow && let Some(run) = self.hand_started_run(&state, &launch.launch.target)? {
            return Ok(TaskFirePlan::Done(self.record_hand_started(&run)));
        }
        let trigger = self.effect_prompt(String::new());
        let (fire, displaced) = self.open_fire()?;
        let checkout = self.fire_dir();
        let start = workflow.then(|| crate::harness::workflow::TaskStart {
            task: self.name.clone(),
            loop_fire: fire.clone(),
            fire: self.run_fire(&trigger),
            fire_facts: self.fire_facts(),
            manual: self.mode == LoopRunMode::Manual,
        });
        let booked_by = self.entry.booked_by.clone();
        let spawned = std::fs::metadata(&checkout)
            .with_context(|| format!("task checkout {}", checkout.display()))
            .and_then(|_| {
                crate::harness::schedule::launch::spawn(
                    &crate::harness::schedule::launch::Request {
                        launch: &launch.launch,
                        fire: &fire,
                        task: &self.name,
                        root: &self.entry.resolved_root(),
                        checkout: &checkout,
                        trigger: &trigger,
                        booked_by: &booked_by,
                        on: self.entry.on,
                        start: start.as_ref(),
                    },
                )
            });
        let spawned = match spawned {
            Ok(spawned) => spawned,
            Err(error) => {
                if let Err(withdraw) = fires::withdraw(&fire, displaced) {
                    tracing::warn!(task = %self.name, %fire, error = %withdraw, "an unlaunched fire's mark stays open");
                }
                return Err(error);
            }
        };
        if let Err(error) = fires::spawned(&fire, spawned.pid()) {
            tracing::warn!(task = %self.name, %fire, %error, "the fire's mark does not name its launch child");
        }
        let agent = launch.launch.kind == LaunchKind::Agent;
        let ended = spawned.wait(|| agent.then(|| published_run(&state, &fire)).flatten())?;
        let kind = launch.launch.kind;
        self.launch_outcome(kind, &state, fire, ended)
    }

    /// The fire's outcome by its kind: a workflow run carrying the fire reports through its own
    /// collection; otherwise the fire is `launched` once an agent fire's `-p` run is published
    /// (the run's own end is not the fire's outcome), and else the child's exit decides at the
    /// fire (0 `launched`, 125 the neutral budget skip, else `error` with the child's words). A
    /// workflow launch that exited 0 published its run, which may already have left the live
    /// directory: its id is the one the child printed. One refused by a live run in its checkout
    /// (exit 122) is the neutral `overlapped` naming that run, as a hand-started run's skip is; no
    /// run of this fire will report, so the backlog drains at release.
    pub(super) fn launch_outcome(
        &mut self,
        kind: LaunchKind,
        state: &StatePaths,
        fire: FireId,
        ended: crate::harness::schedule::launch::Ended,
    ) -> Result<TaskFirePlan> {
        let published = match &ended {
            _ if kind != LaunchKind::Workflow => None,
            crate::harness::schedule::launch::Ended::Exited {
                code: Some(0), run, ..
            } => Some(
                crate::store::workflow::live_runs(state)?
                    .into_iter()
                    .find(|live| live.loop_fire.as_ref() == Some(&fire))
                    .map(|live| (Some(live.id), live.created_at))
                    .unwrap_or((run.clone(), self.now)),
            ),
            _ => crate::store::workflow::live_runs(state)?
                .into_iter()
                .find(|live| live.loop_fire.as_ref() == Some(&fire))
                .map(|live| (Some(live.id), live.created_at)),
        };
        if let Some((run, at)) = published {
            return self.launched(fire, run, at);
        }
        if kind == LaunchKind::Workflow
            && let crate::harness::schedule::launch::Ended::Exited {
                code: Some(crate::harness::workflow::TaskStart::OVERLAP_EXIT_CODE),
                tail,
                run: blocker,
            } = &ended
        {
            let detail = match blocker {
                Some(run) => format!("workflow run {run} holds the checkout — skipped"),
                None => tail.clone(),
            };
            let blocker = blocker.clone();
            return Ok(TaskFirePlan::Done(self.record_terminal_with(
                LoopRunResult::Overlapped,
                LoopRunPresentation::default(),
                TaskFireNotice::Overlap {
                    detail: Some(detail.clone()),
                },
                None,
                |record| {
                    record.fire = Some(fire);
                    record.blocking_run = blocker;
                    record.error = Some(detail);
                },
            )));
        }
        let (run, exit) = match ended {
            crate::harness::schedule::launch::Ended::Published(run) => (Some(run), None),
            crate::harness::schedule::launch::Ended::Exited { code, tail, .. } => {
                let run = (kind == LaunchKind::Agent)
                    .then(|| published_run(state, &fire))
                    .flatten();
                (run, Some((code, tail)))
            }
        };
        let (result, error) = match (run.is_some(), exit) {
            (true, _) => (LoopRunResult::Launched, None),
            (false, Some((Some(0), _))) => (LoopRunResult::Launched, None),
            (false, Some((Some(125), tail))) => (LoopRunResult::BudgetSkipped, Some(tail)),
            // A launch child ends by a signal only when it is stopped (`rimz loop stop`, a
            // `replace`, the user): neutral, as a stopped shell fire is.
            (false, Some((None, _))) => (LoopRunResult::Canceled, None),
            (false, exit) => {
                let (code, tail) = exit.unwrap_or_default();
                let code =
                    code.map_or_else(|| "a signal".to_owned(), |code| format!("exit {code}"));
                let words = if tail.is_empty() {
                    String::new()
                } else {
                    format!(": {tail}")
                };
                (
                    LoopRunResult::Errored,
                    Some(format!(
                        "the {} launch failed ({code}){words}",
                        kind.as_str()
                    )),
                )
            }
        };
        let mut error = error;
        if result == LoopRunResult::Launched {
            error = error.or(self.note_launched(&fire, None, self.now));
            if self.entry.deadline.is_none() {
                self.consume_ephemeral()?;
            }
        }
        let launched = (result == LoopRunResult::Launched)
            .then(|| launched_addresses(state, &fire))
            .flatten();
        Ok(TaskFirePlan::Done(self.record_terminal_with(
            result,
            LoopRunPresentation::default(),
            TaskFireNotice::None,
            None,
            |record| {
                record.fire = Some(fire);
                record.target = launched;
                record.error = error;
            },
        )))
    }

    /// The checkout's last fire, named by what of it is alive, when its launch is: its launch
    /// child runs, or a run or agent carrying its fire id is alive by the durable rule.
    fn alive_launch(&self, state: &StatePaths, kind: LaunchKind) -> Result<Option<Blocker>> {
        let records = task_record::load()?;
        let Some(mark) = records
            .get(&self.task_id)
            .and_then(|record| record.marks.get(&self.launch_checkout()))
        else {
            return Ok(None);
        };
        let rows = match kind {
            LaunchKind::Agent | LaunchKind::Team => room_agents(state)?,
            LaunchKind::Workflow | LaunchKind::Shell => Vec::new(),
        };
        let alive = crate::harness::schedule::launches::alive(&self.task_id, state, rows.iter())?
            .into_iter()
            .filter(|alive| alive.fire == mark.fire && alive.launch.launched_by(kind))
            .collect::<Vec<_>>();
        Ok((!alive.is_empty()).then(|| Blocker::Launch {
            fire: mark.fire.clone(),
            holder: crate::harness::schedule::launches::describe(&alive, &rows),
        }))
    }

    /// A live run of workflow `name` in the firing checkout that a person started, which the
    /// fire would only collide with; a try (`run --once`) collides with nothing. A deployed run
    /// through another link to the same real directory is the same workflow, as the overlap
    /// check in `create` holds. An unreadable deployed set only loses that wording: `create`
    /// still refuses the launch.
    pub(super) fn hand_started_run(
        &self,
        state: &StatePaths,
        name: &str,
    ) -> Result<Option<crate::store::workflow::RunRecord>> {
        // A row placed with `-w` runs its workflow in that tree, where no run can be live
        // before the tree exists.
        let checkout = match self.entry.worktree.as_deref() {
            Some(name) => {
                let tree = crate::worktree::worktree_path(
                    &self.entry.resolved_root(),
                    &self.config.agents.worktree,
                    name,
                )?;
                match tree.canonicalize() {
                    Ok(tree) => tree,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Err(error) => return Err(error.into()),
                }
            }
            None => self.launch_checkout().canonicalize()?,
        };
        let (named, others): (Vec<_>, Vec<_>) = crate::store::workflow::live_runs(state)?
            .into_iter()
            .filter(|run| {
                !run.state.is_terminal()
                    && run.task.is_none()
                    && !run.once
                    && run.scope.path == checkout
            })
            .partition(|run| run.workflow == name);
        if let Some(run) = named.into_iter().next() {
            return Ok(Some(run));
        }
        if others.is_empty() {
            return Ok(None);
        }
        let home = crate::disk::paths::agents_home();
        let Ok(deployed) = crate::harness::workflow::Deployed::load(&home) else {
            return Ok(None);
        };
        let Some(dir) = deployed
            .find(name)
            .ok()
            .flatten()
            .and_then(|dir| dir.canonicalize().ok())
        else {
            return Ok(None);
        };
        Ok(others.into_iter().find(|run| {
            run.origin == crate::store::workflow::RunOrigin::Deployed
                && run
                    .spec_dir
                    .as_deref()
                    .and_then(|live| live.canonicalize().ok())
                    .is_some_and(|live| live == dir)
        }))
    }

    /// The neutral `overlapped` row naming the hand-started run; the trigger is not held.
    fn record_hand_started(&mut self, run: &crate::store::workflow::RunRecord) -> TaskFireFinished {
        let detail = format!(
            "run {} of `{}` was started by hand — skipped",
            run.id, run.workflow
        );
        self.record_terminal_with(
            LoopRunResult::Overlapped,
            LoopRunPresentation::default(),
            TaskFireNotice::Overlap {
                detail: Some(detail.clone()),
            },
            None,
            |record| {
                record.blocking_run = Some(run.id.clone());
                record.error = Some(detail);
            },
        )
    }
}

/// The supervised run published under `fire`, newest first.
fn published_run(state: &StatePaths, fire: &FireId) -> Option<RunId> {
    crate::harness::run::list(state)
        .ok()?
        .into_iter()
        .filter(|run| run.loop_fire.as_ref() == Some(fire))
        .max_by_key(|run| run.started_at)
        .map(|run| run.run_id)
}

/// What `fire` launched that is alive, by address; a room that does not read names nothing,
/// since the launch itself succeeded.
fn launched_addresses(state: &StatePaths, fire: &FireId) -> Option<String> {
    room_agents(state)
        .and_then(|rows| crate::harness::schedule::launches::launched(fire, state, &rows))
        .unwrap_or_else(|error| {
            tracing::warn!(
                %fire,
                error = %format!("{error:#}"),
                "the fire's launches are not named"
            );
            None
        })
}
