use super::*;

/// A shell fire: the workflow run of its one command check.
pub(super) struct Shell {
    pub(super) argv: Vec<String>,
}

impl Fire<'_> {
    /// A command fire starts the workflow run of its one command check, behind the ladder:
    /// deadline and memory, the admission lease, then overlap against the live run, which
    /// `create` refuses.
    pub(super) fn prepare_command(
        &mut self,
        shell: &mut Shell,
        before_launch: &mut dyn FnMut(&Path) -> Result<()>,
    ) -> Result<TaskFirePlan> {
        if let Some(done) = self.prepare_watch()? {
            return Ok(TaskFirePlan::Done(done));
        }
        if let Some(done) = self.prepare_deadline()? {
            return Ok(TaskFirePlan::Done(done));
        }
        if self.workflow_already_launched()? {
            self.finished = true;
            return Ok(TaskFirePlan::AlreadyLaunched);
        }
        let definition = command_definition(&self.name, &self.entry, &shell.argv)?;
        if let Some(done) = self.prepare_run_lock()? {
            return Ok(TaskFirePlan::Done(done));
        }
        let root = self.entry.resolved_root();
        before_launch(&root)?;
        // Another fire may have started a run while this one waited for the room or lease.
        if self.workflow_already_launched()? {
            self.finished = true;
            return Ok(TaskFirePlan::AlreadyLaunched);
        }
        let state = StatePaths::for_project_root(&root)?;
        if let Some(done) = self.reconcile_backlog()? {
            return Ok(TaskFirePlan::Done(done));
        }

        // The run lock and the reconcile may have switched the trigger this fire serves; the
        // run's fire evidence, which its command reads as `RIMZ_LOOP_TRIGGER`, names the one
        // served.
        let trigger = self.effect_prompt(String::new());
        let created_tree = self.prepare_row_tree()?;
        let (fire, displaced) = self.open_fire()?;
        if created_tree {
            self.stamp_row_tree(&fire);
        }
        let definition = crate::harness::workflow::DefinitionSource::Inline {
            task: self.name.clone(),
            loop_fire: fire.clone(),
            definition,

            fire: Some(self.run_fire(&trigger)),
            fire_facts: Some(self.fire_facts()),
        };
        let checkout = self.fire_dir();
        std::fs::metadata(&checkout)
            .with_context(|| format!("task checkout {}", checkout.display()))?;
        let created = crate::harness::workflow::create(
            &state,
            definition,
            crate::harness::workflow::ScopeRoots {
                checkout,
                project: root,
            },
            || Ok(None),
            self.mode == LoopRunMode::Manual,
            crate::harness::workflow::StartArgs::default(),
        );
        let run = match created {
            Ok((run, _advance)) => run,
            Err(error) => {
                // A fire that published no run leaves the checkout's mark as it found it.
                if let Err(withdraw) = fires::withdraw(&fire, displaced) {
                    tracing::warn!(task = %self.name, %fire, error = %withdraw, "an unlaunched fire's mark stays open");
                }
                return self.refused_launch(error);
            }
        };
        self.saw_run = true;

        let error = self.note_launched(&fire, Some(&run.id), run.created_at);
        // A poll-until row lives until a run acts, which the run's report decides.
        if self.entry.deadline.is_none() {
            self.consume_ephemeral()?;
        }
        // The run's report is the fire's one history row; this launch only stamps its proof.
        Ok(TaskFirePlan::Done(self.record_terminal_with(
            LoopRunResult::Launched,
            LoopRunPresentation::default(),
            TaskFireNotice::None,
            None,
            |record| {
                record.run_id = Some(run.id.to_string());
                record.fire = Some(fire);
                record.error = error;
            },
        )))
    }

    /// The plan for a run `create` refused: an overlap with the live run it names, or a
    /// failed start.
    fn refused_launch(
        &mut self,
        error: crate::harness::workflow::WorkflowErr,
    ) -> Result<TaskFirePlan> {
        match error {
            crate::harness::workflow::WorkflowErr::Exists { run, .. } => {
                self.prepare_overlap(&Blocker::Run(run))
            }
            crate::harness::workflow::WorkflowErr::NoRoom(root) => {
                Ok(TaskFirePlan::Done(self.record_gate(
                    LoopRunResult::StartFailed,
                    format!(
                        "no room at {}; run `rimz start` there first",
                        root.display()
                    ),
                )))
            }
            error => Err(error.into()),
        }
    }
}
