//! A shell fire: the gates in front of the lease, its one-node graph, admission, then overlap
//! against the task's live run in `workflow::create`.

use super::*;

/// A shell fire: the workflow run of its one command check.
pub(super) struct Shell {
    pub(super) argv: Vec<String>,
}

impl Fire<'_> {
    /// A command fire starts the workflow run of its one command check, behind the ladder:
    /// deadline and memory, the admission lease, then overlap against the live run, which
    /// `create` refuses.
    pub(super) fn prepare_shell(
        &mut self,
        shell: &mut Shell,
        before_launch: &mut dyn FnMut(&Path) -> Result<()>,
    ) -> Result<TaskFirePlan> {
        if let Some(plan) = self.before_lease()? {
            return Ok(plan);
        }
        let definition = command_definition(&self.name, &self.entry, &shell.argv)?;
        if let Some(plan) = self.admit(before_launch)? {
            return Ok(plan);
        }
        let root = self.entry.resolved_root();
        let state = StatePaths::for_project_root(&root)?;
        // The run lock and the reconcile may have switched the trigger this fire serves; the
        // run's fire evidence, which its command reads as `RIMZ_LOOP_TRIGGER`, names the one
        // served.
        let trigger = self.effect_prompt(String::new());
        let created_tree = self.prepare_row_tree()?;
        // Before the fire opens its mark: a failed test leaves the checkout's mark as it is.
        let checkout = self.fire_dir();
        std::fs::metadata(&checkout)
            .with_context(|| format!("task checkout {}", checkout.display()))?;
        let (fire, displaced) = self.open_fire()?;
        #[cfg(feature = "testkit")]
        crate::testkit::rendezvous("RIMZ_TEST_LOOP_AFTER_OPEN_FIRE");
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
        self.launched(fire, Some(run.id), run.created_at)
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
