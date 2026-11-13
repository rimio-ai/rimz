use std::path::Path;

use anyhow::{Context, Result};
use jiff::Timestamp;
use rimz::agents::{AgentState, ProviderCapacity};
use rimz::harness::takeover::{self, Takeover};
use rimz::harness::workflow::{self, WorkflowErr};
use rimz::ids::EventId;
use rimz::launcher::{self, LaunchOutcome, Preparation};
use rimz::store::run::{ReportTo, RunLifecycle, RunRecord};
use rimz::store::workflow::{AttemptOutcome, AttemptRun, Children};

use crate::cli::{GlobalFlags, agents_cmd, ctx::Ctx, supervised};

pub(super) struct Host<'a> {
    state: &'a rimz::StatePaths,
    globals: &'a GlobalFlags,
    woken_by: Option<EventId>,
}

impl<'a> Host<'a> {
    pub(super) fn new(
        state: &'a rimz::StatePaths,
        globals: &'a GlobalFlags,
        woken_by: Option<EventId>,
    ) -> Self {
        Self {
            state,
            globals,
            woken_by,
        }
    }

    fn ctx(&self) -> Result<Ctx> {
        Ctx::for_workspace(self.state.workspace_id.clone(), self.globals.mux)
    }

    /// Read the checkout again once the launch is prepared: a takeover stops its occupants,
    /// so a launch the preparation refuses (an untrusted folder, untrusted hooks) stops no one.
    /// An occupant that arrived while the launch was prepared returns the checkout's occupancy,
    /// which the step waits on as it would on the first read.
    fn evict(
        &self,
        eviction: &workflow::Eviction,
        stops: bool,
    ) -> Result<Option<workflow::Occupancy>> {
        let ctx = self.ctx()?;
        let snapshot = workflow::without_excluded(ctx.alive_snapshot()?, &eviction.excluded);
        let card_status = rimz::harness::CardStatus::new(&ctx.store);
        let takeover = takeover::read(
            &snapshot,
            &card_status,
            &eviction.checkout,
            &eviction.project_root,
        )?;
        let peers = rimz::address::addressable_agents(&snapshot);
        if let Takeover::Stop(agents) = &takeover
            && stops
        {
            let results = agents_cmd::stop_resolved(&ctx, self.globals, &snapshot, agents)?;
            for (agent, stop) in agents.iter().zip(results) {
                stop.into_result(&rimz::address::agent_handle(agent, &peers, true))?;
            }
            return Ok(None);
        }
        Ok(match occupancy_of(takeover, &peers) {
            workflow::Occupancy::Free => None,
            taken => Some(taken),
        })
    }

    fn owns_push(&self, run: &RunRecord, push: &rimz::store::workflow::PushBack) -> Result<bool> {
        let pin = run
            .workflow
            .as_ref()
            .context("workflow leader has no pin")?;
        let record = rimz::store::workflow::load(&self.state.workflow(&pin.run))?;
        Ok(record.session_attempt().is_some_and(|current| {
            current.seq == pin.seq
                && current.ending.is_none()
                && current
                    .push
                    .as_ref()
                    .is_some_and(|current| current.message == push.message)
        }))
    }
}

fn host_error(error: impl std::fmt::Display) -> WorkflowErr {
    WorkflowErr::Refused(error.to_string())
}

/// The checkout's occupants as a workflow step reads them, by handle.
fn occupancy_of(takeover: Takeover<'_>, peers: &[&AgentState]) -> workflow::Occupancy {
    let handle = |agent: &AgentState| rimz::address::agent_handle(agent, peers, true);
    match takeover {
        Takeover::Stop(agents) if agents.is_empty() => workflow::Occupancy::Free,
        Takeover::Stop(agents) => {
            workflow::Occupancy::Occupied(agents.into_iter().map(handle).collect())
        }
        Takeover::Blocked(agents) => workflow::Occupancy::Blocked(
            agents
                .into_iter()
                .map(|(agent, turn)| (handle(agent), turn))
                .collect(),
        ),
    }
}

/// Queue an automated `@rimz` notice once under `message` for `agent`'s next done boundary,
/// then send it now if the agent rests; `about_run` withdraws it once that run ends.
fn queue_notice(
    ctx: &Ctx,
    agent: &AgentState,
    notice: rimz::store::message::HarnessNotice,
    about_run: Option<rimz::ids::RunId>,
    message: &rimz::ids::MessageId,
    text: &str,
) -> Result<()> {
    let mut record = rimz::message::synthetic::SyntheticMessage {
        agent,
        text: text.into(),
        sender: rimz::store::message::MessageSender::Harness { notice },
        gate: rimz::store::message::DeliveryGate::Done,
    }
    .record(&ctx.workspace)
    .with_automated(true)
    .with_about_run(about_run);
    record.message_id = message.clone();
    ctx.store
        .queue_message_once(&record, &ctx.workspace.session_name)?;
    rimz::message::synthetic::attempt_now(
        &ctx.workspace,
        &ctx.store,
        message,
        None,
        rimz::store::writer::DeliveryFailureDisposition::Retry,
        "workflow notice delivery deferred",
    )?;
    Ok(())
}

impl workflow::AttemptHost for Host<'_> {
    fn occupancy(
        &self,
        scope: &Path,
        root: &Path,
        excluded: &[String],
    ) -> Result<workflow::Occupancy, WorkflowErr> {
        (|| -> Result<_> {
            let ctx = self.ctx()?;
            let snapshot = workflow::without_excluded(ctx.alive_snapshot()?, excluded);
            let card_status = rimz::harness::CardStatus::new(&ctx.store);
            let takeover = takeover::read(&snapshot, &card_status, scope, root)?;
            let peers = rimz::address::addressable_agents(&snapshot);
            Ok(occupancy_of(takeover, &peers))
        })()
        .map_err(host_error)
    }

    fn launch(
        &self,
        step: workflow::StepLaunch,
    ) -> Result<Result<AttemptRun, workflow::NotLaunched>, WorkflowErr> {
        let resume = step.resume.clone();
        let workflow_id = step.workflow.run.clone();
        let evict = step.evict;
        // A resumed session keeps the account, system prompt, and checkout it was launched with.
        let fresh = step.resume.is_none();
        let set = step.overrides;
        let worktree = set.worktree.filter(|_| fresh);
        let cwd = worktree.is_none().then_some(step.cwd);
        let overrides = launcher::LaunchOverrides {
            login: set.account.filter(|_| fresh),
            mode: set.mode,
            preset: rimz::agents::LaunchPreset {
                effort: set.effort,
                fast: set.fast,
                system_prompt_file: set.system_prompt_file.filter(|_| fresh),
                ..Default::default()
            },
            ..Default::default()
        };
        // Only a failed eviction started nothing: the retry prepares again over the checkout
        // preparation created, and stops the occupants again.
        let mut evict_failed = false;
        let result = (|| -> Result<_> {
            let ctx = self.ctx()?;
            let mux = crate::cli::render::room::present_mux_pick(
                rimz::room::session::pick_mux_for_session(
                    &ctx.workspace.session_name,
                    self.globals.mux,
                ),
            )?;
            let request = launcher::LayoutRequest {
                context: launcher::LaunchContext {
                    workspace: ctx.workspace,
                    store: ctx.store,
                    machine: rimz::config::MachineConfig::load_lenient(),
                    mux: Some(mux),
                    root: self.globals.root.clone(),
                    channel: None,
                },
                launch: launcher::LayoutLaunch {
                    spec: Some(step.agent),
                    prompt: Some(step.prompt),
                    cwd: cwd.clone(),
                    cohort: launcher::CohortRequest {
                        fresh: worktree.is_some() && step.takeover,
                        worktree,
                        budget: set.budget,
                        bg: true,
                        new_tab: true,
                        ..Default::default()
                    },
                    overrides,
                    report_to: ReportTo::Nobody,
                    ..Default::default()
                },
                cwd,
                allow_in_place: false,
                entry: launcher::RoomEntry::Birth,
                starter: launcher::Starter::Workflow,
                workflow: Some(step.workflow.clone()),
                automation: Some(step.automation),
                leader_run: Some(launcher::LeaderRun {
                    resume: step.resume,
                    reminder_section: Some(rimz::harness::launch::ReminderSection::Workflow(
                        step.workflow_note,
                    )),
                    lifecycle: if step.lifecycle.is_turn() {
                        RunLifecycle::Turn
                    } else {
                        RunLifecycle::Session
                    },
                    keep: false,
                    report_to: ReportTo::Nobody,
                    timeout: None,
                    retry_of: None,
                    cleanup: !step.keep,
                    subagent: false,
                    warn: Vec::new(),
                    grace: None,
                    loop_task: step.task,
                    loop_booked_by: step.booked_by,
                    workflow: Some(step.workflow),
                    throttle_turn: step.throttle_turn,
                    reader: None,
                    response_receipt: false,
                    completion: launcher::RunCompletion::Detached,
                }),
            };
            let mut interaction = crate::cli::launch_interaction::CliInteraction;
            match launcher::prepare_layout(request, &mut interaction)? {
                Preparation::Prepared(prepared) => {
                    if let Some(eviction) = &evict
                        && let Some(taken) = self
                            .evict(eviction, step.takeover)
                            .inspect_err(|_| evict_failed = true)?
                    {
                        return Ok(Err(taken));
                    }
                    launcher::execute(*prepared, &mut interaction).map(Ok)
                }
                Preparation::Handled(outcome) => Ok(Ok(outcome)),
            }
        })();
        let (outcome, reason) = match result {
            Ok(Err(taken)) => return Ok(Err(workflow::NotLaunched::Taken(taken))),
            Ok(Ok(LaunchOutcome::Opened(opened))) => {
                let run = opened
                    .run
                    .context("workflow launch did not publish its leader run")
                    .map_err(host_error)?;
                let name = if let Some(session) = &resume {
                    let ctx = self.ctx().map_err(host_error)?;
                    let rows = ctx.store.audit_rows().map_err(host_error)?;
                    rows.iter()
                        .find(|agent| agent.kind == run.record.kind && &agent.agent_id == session)
                        .and_then(|agent| agent.name.clone())
                } else {
                    run.record.agent_name
                };
                let name = name
                    .context("workflow run has no leader name")
                    .map_err(host_error)?;
                return Ok(Ok(AttemptRun {
                    handle: format!("@{name}"),
                    run_id: run.record.run_id,
                }));
            }
            Ok(Ok(LaunchOutcome::BudgetExceeded { reason })) => (AttemptOutcome::Limited, reason),
            Ok(Ok(LaunchOutcome::Declined | LaunchOutcome::Existing)) => {
                (AttemptOutcome::Died, "workflow launch aborted".into())
            }
            Err(error) => (
                AttemptOutcome::Died,
                if resume.is_some() {
                    format!(
                        "{error:#}; use rimz workflow restart {workflow_id} for a fresh agent step"
                    )
                } else {
                    format!("{error:#}")
                },
            ),
        };
        let failure = workflow::LaunchFailure { outcome, reason };
        Ok(Err(if evict_failed {
            workflow::NotLaunched::Unstarted(failure)
        } else {
            workflow::NotLaunched::Failed(failure)
        }))
    }

    fn observe(
        &self,
        attempt: &AttemptRun,
        observation: workflow::Observation,
    ) -> Result<workflow::Observed, WorkflowErr> {
        (|| -> Result<_> {
            let ctx = self.ctx()?;
            let run = rimz::harness::run::load(self.state, &attempt.run_id)?;
            let mut snapshot = ctx.cached_snapshot()?;
            rimz::harness::budget::project_parks(
                |_| false,
                &mut snapshot,
                ctx.runtime(),
                Some(ctx.store.paths()),
                &rimz::config::MachineConfig::load_lenient(),
            );
            workflow::observe(
                &ctx.store,
                &run,
                &snapshot,
                observation,
                self.woken_by.as_ref(),
                |agent| Ok(rimz::agents::read_open_ask(self.state, agent, None)?),
                |agent| {
                    ProviderCapacity::read(ctx.runtime(), &agent.login_key()).and_then(|capacity| {
                        capacity.latest_spent_window_reset_for_model(
                            Timestamp::now(),
                            agent.model.as_deref(),
                        )
                    })
                },
            )
        })()
        .map_err(host_error)
    }

    fn push(
        &self,
        attempt: &AttemptRun,
        push: &rimz::store::workflow::PushBack,
        text: &str,
    ) -> Result<workflow::Pushed, WorkflowErr> {
        (|| -> Result<_> {
            let ctx = self.ctx()?;
            let run = rimz::harness::run::load(self.state, &attempt.run_id)?;
            if !self.owns_push(&run, push)? {
                return Ok(workflow::Pushed { queued: false });
            }
            let snapshot = ctx.cached_snapshot()?;
            let root = workflow::leader(&snapshot, &run)
                .context("workflow leader is no longer in the room")?;
            let mut message = rimz::message::synthetic::SyntheticMessage {
                agent: root,
                text: text.into(),
                sender: rimz::store::message::MessageSender::System,
                gate: rimz::store::message::DeliveryGate::Done,
            }
            .record(&ctx.workspace);
            message.message_id = push.message.clone();
            ctx.store
                .queue_message_once(&message, &ctx.workspace.session_name)?;
            if !self.owns_push(&run, push)? {
                ctx.store.retire_message(
                    &push.message,
                    &ctx.workspace.session_name,
                    "workflow attempt ended",
                )?;
                return Ok(workflow::Pushed { queued: true });
            }
            rimz::message::synthetic::attempt_now(
                &ctx.workspace,
                &ctx.store,
                &push.message,
                None,
                rimz::store::writer::DeliveryFailureDisposition::Retry,
                "workflow push-back delivery deferred",
            )?;
            Ok(workflow::Pushed { queued: true })
        })()
        .map_err(host_error)
    }

    fn forward(
        &self,
        attempt: &AttemptRun,
        message: &rimz::ids::MessageId,
        text: &str,
    ) -> Result<(), WorkflowErr> {
        (|| -> Result<_> {
            let ctx = self.ctx()?;
            let run = rimz::harness::run::load(self.state, &attempt.run_id)?;
            let snapshot = ctx.cached_snapshot()?;
            let root = workflow::leader(&snapshot, &run)
                .context("workflow leader is no longer in the room")?;
            queue_notice(
                &ctx,
                root,
                rimz::store::message::HarnessNotice::Signal,
                Some(attempt.run_id.clone()),
                message,
                text,
            )
        })()
        .map_err(host_error)
    }

    fn detach(&self, attempt: &AttemptRun) -> Result<Children, WorkflowErr> {
        (|| -> Result<_> {
            let ctx = self.ctx()?;
            let run = rimz::harness::run::load(self.state, &attempt.run_id)?;
            supervised::cancel_supervised_run(&ctx.store, &run)?;
            let Some(pin) = &run.workflow else {
                return Ok(Children::default());
            };
            let snapshot = ctx.cached_snapshot()?;
            let rows = ctx.store.audit_rows()?;
            let cohort = workflow::detach_cohort(&snapshot, rows.iter(), &run);
            for root in cohort.agents {
                ctx.store.clear_workflow_pin(
                    &root.kind,
                    &root.agent_id,
                    pin,
                    &ctx.workspace.session_name,
                )?;
            }
            Ok(cohort.children)
        })()
        .map_err(host_error)
    }

    fn message(
        &self,
        scope: &Path,
        target: &str,
        session: Option<&rimz::ids::AgentSessionId>,
        message: &rimz::ids::MessageId,
        text: &str,
    ) -> Result<std::result::Result<String, String>, WorkflowErr> {
        (|| -> Result<_> {
            let ctx = self.ctx()?;
            let snapshot = ctx.cached_snapshot()?;
            let living = rimz::address::addressable_agents(&snapshot)
                .into_iter()
                .filter(|agent| agent.ended_at.is_none())
                .collect::<Vec<_>>();
            let (queue, history) = ctx.store.list_messages_and_history()?;
            if let Some(queued) = queue
                .iter()
                .chain(&history)
                .find(|row| row.message_id == *message)
            {
                // A lost host already queued this notice; its target stands.
                let handle = rimz::address::launch_row(
                    snapshot.agents.iter(),
                    &queued.kind,
                    &queued.agent_id,
                )
                .map_or_else(
                    || target.to_owned(),
                    |agent| rimz::address::agent_handle(agent, &living, true),
                );
                return Ok(Ok(handle));
            }
            let channel = rimz::address::AddressContext {
                channel: launcher::context::directory_channel(&ctx.workspace, scope),
                origin: rimz::address::ChannelOrigin::Directory,
            };
            let agent = match rimz::address::resolve_agent(target, None, &channel, &living) {
                Ok(agent) => agent,
                Err(error) => return Ok(Err(error.to_string())),
            };
            // A handle outlives its session: a reused one names another agent.
            if let Some(session) = session
                && agent.agent_id != *session
            {
                return Ok(Err(format!(
                    "{target} is session {}, not the pinned session {session}",
                    agent.agent_id
                )));
            }
            queue_notice(
                &ctx,
                agent,
                rimz::store::message::HarnessNotice::Wait,
                None,
                message,
                text,
            )?;
            Ok(Ok(rimz::address::agent_handle(agent, &living, true)))
        })()
        .map_err(host_error)
    }

    fn retire(&self, message: &rimz::ids::MessageId) -> Result<(), WorkflowErr> {
        (|| -> Result<_> {
            let ctx = self.ctx()?;
            ctx.store.retire_message(
                message,
                &ctx.workspace.session_name,
                "workflow attempt ended",
            )?;
            Ok(())
        })()
        .map_err(host_error)
    }

    fn stop(&self, attempt: &AttemptRun) -> Result<Children, WorkflowErr> {
        (|| -> Result<_> {
            let ctx = self.ctx()?;
            let run = rimz::harness::run::load(self.state, &attempt.run_id)?;
            if let Some(pin) = &run.workflow {
                let record = rimz::store::workflow::load(&self.state.workflow(&pin.run))?;
                if let Some(push) = record
                    .session_attempt()
                    .filter(|current| current.seq == pin.seq)
                    .and_then(|current| current.push.as_ref())
                {
                    ctx.store.retire_message(
                        &push.message,
                        &ctx.workspace.session_name,
                        "workflow attempt ended",
                    )?;
                }
            }
            let snapshot = ctx.cached_snapshot()?;
            let Some(cohort) = workflow::stop_cohort(&snapshot, &run) else {
                supervised::stop_supervised_run(&ctx.workspace, &ctx.store, self.globals, &run)?;
                return Ok(Children::default());
            };
            let peers = rimz::address::addressable_agents(&snapshot);
            supervised::cancel_supervised_run(&ctx.store, &run)?;
            for (root, stop) in cohort.agents.iter().zip(agents_cmd::stop_resolved(
                &ctx,
                self.globals,
                &snapshot,
                &cohort.agents,
            )?) {
                let handle = rimz::address::agent_handle(root, &peers, true);
                stop.into_result(&handle)
                    .context("stopping workflow attempt")?;
            }
            Ok(cohort.children)
        })()
        .map_err(host_error)
    }

    fn orphan(
        &self,
        state: &rimz::StatePaths,
        attempt: &rimz::agents::WorkflowAttemptRef,
    ) -> Result<Option<AttemptRun>, WorkflowErr> {
        workflow::orphan(state, attempt)
    }

    fn leader_session(&self, run: &AttemptRun) -> Option<rimz::ids::AgentSessionId> {
        rimz::harness::run::load(self.state, &run.run_id)
            .ok()
            .and_then(|run| run.agent_id)
    }

    fn start_groups(
        &self,
        record: &rimz::store::workflow::RunRecord,
        agent: &str,
        account: Option<&rimz::ids::LoginName>,
    ) -> Result<Vec<rimz::harness::throttle::StartGroup>, WorkflowErr> {
        workflow::launch_groups(record, agent, account)
    }
}
