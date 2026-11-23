//! The production workflow `AttemptHost`: it reads the room's facts for the engine and carries
//! out its answers through the shared launcher and the tree stop. `harness::workflow` decides;
//! this host prints nothing and hands every notice to the caller's [`Interaction`].

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use jiff::Timestamp;

use super::stop::{self, StopRoom};
use super::{Interaction, LaunchOutcome, Notice, Preparation};
use crate::agents::{AgentState, ProviderCapacity, WorkflowAttemptRole};
use crate::harness::takeover::{self, Takeover};
use crate::harness::workflow::{self, WorkflowErr};
use crate::ids::{EventId, MuxName, WorkflowRunId};
use crate::store::run::{ReportTo, RunLifecycle, RunRecord};
use crate::store::snapshot::SidebarSnapshot;
use crate::store::workflow::{AttemptOutcome, AttemptRun, Children};
use crate::{ResolvedWorkspace, RuntimePaths, StatePaths, Store};

/// The workflow host of one room. Launch decisions and notices go to `interaction`: a terminal
/// asks there, an unattended caller refuses.
pub struct Host<'a> {
    state: &'a StatePaths,
    /// The backend a command pinned with `--mux`, if any.
    mux: Option<MuxName>,
    /// The room root a command named with `--root`, if any.
    root: Option<PathBuf>,
    woken_by: Option<EventId>,
    interaction: RefCell<Box<dyn Interaction + 'a>>,
}

/// The room a host call acts in, opened for that call.
struct Ctx {
    workspace: ResolvedWorkspace,
    store: Store,
}

impl Ctx {
    fn cached_snapshot(&self) -> Result<SidebarSnapshot> {
        self.store
            .snapshot_cached()
            .context("reading agent snapshot")
    }

    fn alive_snapshot(&self) -> Result<SidebarSnapshot> {
        Ok(crate::sidebar::consumer::cached_alive_snapshot(
            self.cached_snapshot()?,
            self.store.runtime_paths(),
            &self.workspace.session_name,
        ))
    }

    fn runtime(&self) -> &RuntimePaths {
        self.store.runtime_paths()
    }
}

impl<'a> Host<'a> {
    pub fn new(
        state: &'a StatePaths,
        mux: Option<MuxName>,
        root: Option<PathBuf>,
        woken_by: Option<EventId>,
        interaction: Box<dyn Interaction + 'a>,
    ) -> Self {
        Self {
            state,
            mux,
            root,
            woken_by,
            interaction: RefCell::new(interaction),
        }
    }

    fn ctx(&self) -> Result<Ctx> {
        let workspace_id = self.state.workspace_id.clone();
        let no_room = || {
            crate::workspace::WorkspaceErr::NoRoom {
                location: format!("for workspace {workspace_id}"),
            }
            .to_string()
        };
        let paths =
            StatePaths::for_workspace(workspace_id.clone()).context("preparing store paths")?;
        let record =
            crate::workspace::record::read(&paths.workspace_record).with_context(no_room)?;
        let runtime = RuntimePaths::for_state(&paths).context("preparing runtime paths")?;
        let store = Store::open_existing(paths, runtime).with_context(no_room)?;
        let workspace = ResolvedWorkspace {
            workspace_id: workspace_id.clone(),
            project_root: record.project_root.clone(),
            cwd_project_root: None,
            root_class: record.root_class,
            worktree_root: record.project_root,
            worktree_branch: None,
            session_name: record.session_name,
            mux_hint: self.mux,
        };
        Ok(Ctx { workspace, store })
    }

    /// Hand each backend notice to the interaction.
    fn notify(&self, notices: Vec<String>) -> Result<()> {
        let mut interaction = self.interaction.borrow_mut();
        for notice in &notices {
            interaction.notice(Notice::Room(notice))?;
        }
        Ok(())
    }

    fn stop_room<'c>(&self, ctx: &'c Ctx) -> StopRoom<'c> {
        StopRoom {
            workspace: &ctx.workspace,
            store: &ctx.store,
            mux: self.mux,
        }
    }

    /// Read the checkout again once the launch is prepared: a takeover stops its occupants,
    /// so a launch the preparation refuses (an untrusted folder, untrusted hooks) stops no one.
    /// The checkout is free only by a read taken after the last stop issued: a read that finds
    /// stoppable occupants stops them and reads again, up to `EVICT_STOP_ROUNDS` times. A read
    /// that finds blockers, or occupants left once the rounds are spent, returns the checkout's
    /// occupancy, which the step waits on as it would on the first read.
    fn evict(
        &self,
        eviction: &workflow::Eviction,
        stops: bool,
    ) -> Result<Option<workflow::Occupancy>> {
        let ctx = self.ctx()?;
        let mut rounds = 0;
        loop {
            let snapshot = ctx.alive_snapshot()?;
            let card_status = crate::harness::CardStatus::new(&ctx.store);
            let takeover = takeover::read(
                &snapshot,
                &card_status,
                &eviction.checkout,
                &eviction.project_root,
                &|agent| owed(eviction.beside.as_ref(), agent),
            )?;
            let peers = crate::address::addressable_agents(&snapshot);
            if let Takeover::Stop(agents) = &takeover
                && !agents.is_empty()
                && stops
                && rounds < EVICT_STOP_ROUNDS
            {
                let mut notices = Vec::new();
                let results =
                    stop::stop_resolved(self.stop_room(&ctx), &snapshot, agents, &mut notices);
                self.notify(notices)?;
                let results = results?;
                for (agent, stop) in agents.iter().zip(results) {
                    stop.into_result(&crate::address::named_handle(agent, &peers, true))?;
                }
                rounds += 1;
                continue;
            }
            return Ok(match occupancy_of(takeover, &peers) {
                workflow::Occupancy::Free => None,
                taken => Some(taken),
            });
        }
    }

    /// Retire the push-back the run's attempt queued, so no ending of the attempt (a stop or a
    /// detach) leaves it to be typed.
    fn retire_push(&self, ctx: &Ctx, run: &RunRecord) -> Result<()> {
        let Some(pin) = &run.workflow else {
            return Ok(());
        };
        // A run that has left the live directory owns no push-back; its agent still stops.
        let Some(record) = live_record(self.state, &pin.run)? else {
            return Ok(());
        };
        if let Some(push) = pushing_attempt(&record, pin)
            .filter(|current| current.seq == pin.seq)
            .and_then(|current| current.push.as_ref())
        {
            ctx.store.retire_message(
                &push.message,
                &ctx.workspace.session_name,
                "workflow attempt ended",
            )?;
        }
        Ok(())
    }

    fn owns_push(&self, run: &RunRecord, push: &crate::store::workflow::PushBack) -> Result<bool> {
        let pin = run
            .workflow
            .as_ref()
            .context("workflow leader has no pin")?;
        // A run that has left the live directory owns no push-back.
        let Some(record) = live_record(self.state, &pin.run)? else {
            return Ok(false);
        };
        Ok(pushing_attempt(&record, pin).is_some_and(|current| {
            current.seq == pin.seq
                && current.ending.is_none()
                && current
                    .push
                    .as_ref()
                    .is_some_and(|current| current.message == push.message)
        }))
    }
}

/// The attempt that may own the push-back of the agent `pin` names: a step's session, or a
/// judge's attempt, which its reminder reaches.
fn pushing_attempt<'r>(
    record: &'r crate::store::workflow::RunRecord,
    pin: &crate::agents::WorkflowAttemptRef,
) -> Option<&'r crate::store::workflow::LiveAttempt> {
    match pin.role {
        WorkflowAttemptRole::Judge => record
            .attempt
            .as_ref()
            .filter(|attempt| attempt.kind == crate::store::workflow::AttemptKind::Judge),
        WorkflowAttemptRole::Step => record.session_attempt(),
    }
}

/// The run `id` while it is live in this RimZ's live directory.
fn live_record(
    state: &StatePaths,
    id: &WorkflowRunId,
) -> Result<Option<crate::store::workflow::RunRecord>> {
    use crate::store::workflow::{FoundRun, RunClass};
    Ok(match crate::store::workflow::find(state, id)? {
        Some(FoundRun {
            class: RunClass::Current(record),
            finished: false,
            ..
        }) => Some(*record),
        _ => None,
    })
}

/// How many times one eviction stops the occupants a fresh read finds before it returns them
/// as the checkout's occupancy.
const EVICT_STOP_ROUNDS: usize = 3;

fn host_error(error: impl std::fmt::Display) -> WorkflowErr {
    WorkflowErr::Refused(error.to_string())
}

/// Whether `agent` is the starter a run owes its report, which runs beside the run's launches.
fn owed(starter: Option<&crate::store::workflow::RunStarter>, agent: &AgentState) -> bool {
    starter.is_some_and(|starter| starter.is_agent(agent))
}

/// The checkout's occupants as a workflow step reads them, by handle.
fn occupancy_of(takeover: Takeover<'_>, peers: &[&AgentState]) -> workflow::Occupancy {
    let handle = |agent: &AgentState| crate::address::agent_handle(agent, peers, true);
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
    notice: crate::store::message::HarnessNotice,
    about_run: Option<crate::ids::RunId>,
    message: &crate::ids::MessageId,
    text: &str,
) -> Result<()> {
    let mut record = crate::message::synthetic::SyntheticMessage {
        agent,
        text: text.into(),
        sender: crate::store::message::MessageSender::Harness { notice },
        gate: crate::store::message::DeliveryGate::Done,
        premises: crate::store::message::Premise::NONE,
    }
    .record(&ctx.workspace)
    .with_automated(true)
    .with_about_run(about_run);
    record.message_id = message.clone();
    ctx.store
        .queue_message_once(&record, &ctx.workspace.session_name)?;
    crate::message::synthetic::attempt_now(
        &ctx.workspace,
        &ctx.store,
        message,
        None,
        crate::store::writer::DeliveryFailureDisposition::Retry,
        "workflow notice delivery deferred",
    )?;
    Ok(())
}

impl workflow::AttemptHost for Host<'_> {
    fn occupancy(
        &self,
        scope: &Path,
        root: &Path,
        beside: Option<&crate::store::workflow::RunStarter>,
    ) -> Result<workflow::Occupancy, WorkflowErr> {
        (|| -> Result<_> {
            let ctx = self.ctx()?;
            let snapshot = ctx.alive_snapshot()?;
            let card_status = crate::harness::CardStatus::new(&ctx.store);
            let beside = |agent: &AgentState| owed(beside, agent);
            let takeover = takeover::read(&snapshot, &card_status, scope, root, &beside)?;
            let peers = crate::address::addressable_agents(&snapshot);
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
        // A resumed session drops the step's account, system prompt, and worktree overrides and
        // runs in the run's scope: the launcher picks the room's current account, and a resume
        // across history pools refuses there.
        let fresh = step.resume.is_none();
        let set = step.overrides;
        let worktree = set.worktree.filter(|_| fresh);
        let cwd = worktree.is_none().then_some(step.cwd);
        let overrides = super::LaunchOverrides {
            login: set.account.filter(|_| fresh),
            mode: set.mode,
            // A judge answers unattended unless its profile says otherwise.
            default_mode: (step.workflow.role == WorkflowAttemptRole::Judge)
                .then_some(crate::agents::PermissionMode::Auto),
            preset: crate::agents::LaunchPreset {
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
            let (mux, notices) = match crate::room::session::pick_mux_for_session(
                &ctx.workspace.session_name,
                self.mux,
            ) {
                Ok(pick) => (Ok(pick.mux), pick.notices),
                Err(err) => (Err(err.source), err.notices),
            };
            self.notify(notices)?;
            let mux = mux?;
            let request = super::LayoutRequest {
                context: super::LaunchContext {
                    workspace: ctx.workspace,
                    store: ctx.store,
                    machine: crate::config::MachineConfig::load_lenient(),
                    mux: Some(mux),
                    root: self.root.clone(),
                    channel: None,
                },
                launch: super::LayoutLaunch {
                    spec: Some(step.agent),
                    prompt: Some(step.prompt),
                    cwd: cwd.clone(),
                    cohort: super::CohortRequest {
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
                entry: super::RoomEntry::Birth,
                starter: super::Starter::Workflow,
                workflow: Some(step.workflow.clone()),
                automation: Some(step.automation),
                leader_run: Some(super::LeaderRun {
                    resume: step.resume,
                    reminder_section: Some(crate::harness::launch::ReminderSection::Workflow(
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
                    loop_fire: None,
                    loop_booked_by: step.booked_by,
                    workflow: Some(step.workflow),
                    throttle_turn: step.throttle_turn,
                    reader: None,
                    response_receipt: false,
                    completion: super::RunCompletion::Detached,
                }),
            };
            let prepared = super::prepare_layout(request, &mut **self.interaction.borrow_mut())?;
            match prepared {
                Preparation::Prepared(prepared) => {
                    if let Some(eviction) = &evict
                        && let Some(taken) = self
                            .evict(eviction, step.takeover)
                            .inspect_err(|_| evict_failed = true)?
                    {
                        return Ok(Err(taken));
                    }
                    super::execute(*prepared, &mut **self.interaction.borrow_mut()).map(Ok)
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
            Ok(Ok(LaunchOutcome::BudgetExceeded { reason })) => {
                return Ok(Err(workflow::NotLaunched::Budget(reason)));
            }
            Ok(Ok(LaunchOutcome::Declined | LaunchOutcome::Existing { .. })) => {
                (AttemptOutcome::Died, "workflow launch aborted".into())
            }
            // A trust problem the user clears in place waits for the fix; any other ends the
            // attempt.
            Err(ref error)
                if let Some(refusal) = error
                    .chain()
                    .find_map(|cause| cause.downcast_ref::<super::trust::TrustRefusal>())
                    && refusal.clearable() =>
            {
                return Ok(Err(workflow::NotLaunched::Trust(refusal.detail())));
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
            let run = crate::harness::run::load(self.state, &attempt.run_id)?;
            let mut snapshot = ctx.cached_snapshot()?;
            crate::harness::budget::project_parks(
                |_| false,
                &mut snapshot,
                ctx.runtime(),
                Some(ctx.store.paths()),
                &crate::config::MachineConfig::load_lenient(),
            );
            workflow::observe(
                &ctx.store,
                &run,
                &snapshot,
                observation,
                self.woken_by.as_ref(),
                |agent| Ok(crate::agents::read_open_ask(self.state, agent, None)?),
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
        push: &crate::store::workflow::PushBack,
        text: &str,
    ) -> Result<workflow::Pushed, WorkflowErr> {
        (|| -> Result<_> {
            let ctx = self.ctx()?;
            let run = crate::harness::run::load(self.state, &attempt.run_id)?;
            if !self.owns_push(&run, push)? {
                return Ok(workflow::Pushed { queued: false });
            }
            let snapshot = ctx.cached_snapshot()?;
            let root = workflow::leader(&snapshot, &run)
                .context("workflow leader is no longer in the room")?;
            let mut message = crate::message::synthetic::SyntheticMessage {
                agent: root,
                text: text.into(),
                sender: crate::store::message::MessageSender::System,
                gate: crate::store::message::DeliveryGate::Done,
                premises: crate::store::message::Premise::NONE,
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
            crate::message::synthetic::attempt_now(
                &ctx.workspace,
                &ctx.store,
                &push.message,
                None,
                crate::store::writer::DeliveryFailureDisposition::Retry,
                "workflow push-back delivery deferred",
            )?;
            Ok(workflow::Pushed { queued: true })
        })()
        .map_err(host_error)
    }

    fn forward(
        &self,
        attempt: &AttemptRun,
        message: &crate::ids::MessageId,
        text: &str,
    ) -> Result<(), WorkflowErr> {
        (|| -> Result<_> {
            let ctx = self.ctx()?;
            let run = crate::harness::run::load(self.state, &attempt.run_id)?;
            let snapshot = ctx.cached_snapshot()?;
            let root = workflow::leader(&snapshot, &run)
                .context("workflow leader is no longer in the room")?;
            queue_notice(
                &ctx,
                root,
                crate::store::message::HarnessNotice::Signal,
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
            let run = crate::harness::run::load(self.state, &attempt.run_id)?;
            self.retire_push(&ctx, &run)?;
            stop::cancel_run(&ctx.store, &run)?;
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
        session: Option<&crate::ids::AgentSessionId>,
        message: &crate::ids::MessageId,
        text: &str,
    ) -> Result<std::result::Result<String, String>, WorkflowErr> {
        (|| -> Result<_> {
            let ctx = self.ctx()?;
            let snapshot = ctx.cached_snapshot()?;
            let living = crate::address::addressable_agents(&snapshot)
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
                let handle = crate::address::launch_row(
                    snapshot.agents.iter(),
                    &queued.kind,
                    &queued.agent_id,
                )
                .map_or_else(
                    || target.to_owned(),
                    |agent| crate::address::agent_handle(agent, &living, true),
                );
                return Ok(Ok(handle));
            }
            let channel = crate::address::AddressContext {
                channel: super::context::directory_channel(&ctx.workspace, scope),
                origin: crate::address::ChannelOrigin::Directory,
            };
            let agent = match crate::address::resolve_agent(target, None, &channel, &living) {
                Ok(agent) => agent,
                Err(error) => return Ok(Err(error.to_string())),
            };
            // A handle outlives its agent: a reused one names another agent, while a
            // restart or molt keeps the agent the pin names under a new session.
            if let Some(session) = session
                && !pin_names(ctx.store.audit_rows()?.iter(), session, agent)
            {
                return Ok(Err(format!(
                    "{target} is session {}, not the pinned session {session}",
                    agent.agent_id
                )));
            }
            queue_notice(
                &ctx,
                agent,
                crate::store::message::HarnessNotice::Wait,
                None,
                message,
                text,
            )?;
            Ok(Ok(crate::address::agent_handle(agent, &living, true)))
        })()
        .map_err(host_error)
    }

    fn retire(&self, message: &crate::ids::MessageId) -> Result<(), WorkflowErr> {
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
            let run = crate::harness::run::load(self.state, &attempt.run_id)?;
            self.retire_push(&ctx, &run)?;
            let snapshot = ctx.cached_snapshot()?;
            let Some(cohort) = workflow::stop_cohort(&snapshot, &run) else {
                let label = run.agent_name.as_deref().unwrap_or(run.run_id.as_str());
                let mut notices = Vec::new();
                let stopped = stop::stop_run_agent(self.stop_room(&ctx), &run, &mut notices);
                self.notify(notices)?;
                stopped?
                    .into_result(label)
                    .context("stopping workflow attempt")?;
                return Ok(Children::default());
            };
            let peers = crate::address::addressable_agents(&snapshot);
            let terms = stop::StopTerms {
                run: Some(&run),
                still_due: None,
                read: None,
                tab: stop::TabClose::Never,
            };
            let mut notices = Vec::new();
            let stops = stop::stop_resolved_with(
                self.stop_room(&ctx),
                &snapshot,
                &cohort.agents,
                terms,
                &mut notices,
            );
            self.notify(notices)?;
            let stops = stops?.unwrap_or_default();
            for (root, stop) in cohort.agents.iter().zip(stops) {
                let handle = crate::address::named_handle(root, &peers, true);
                stop.into_result(&handle)
                    .context("stopping workflow attempt")?;
            }
            Ok(cohort.children)
        })()
        .map_err(host_error)
    }

    fn orphan(
        &self,
        state: &crate::StatePaths,
        attempt: &crate::agents::WorkflowAttemptRef,
    ) -> Result<Option<AttemptRun>, WorkflowErr> {
        workflow::orphan(state, attempt)
    }

    fn committed(
        &self,
        before: &crate::store::workflow::RunRecord,
        after: &crate::store::workflow::RunRecord,
    ) {
        use crate::harness::schedule::signal::{emit_in_process, workflow};
        let signals = workflow::run_signals(before, after);
        if signals.is_empty() {
            return;
        }
        let ctx = match self.ctx() {
            Ok(ctx) => ctx,
            Err(error) => {
                tracing::warn!(%error, run = %after.id, "could not open the room to fire workflow signals");
                return;
            }
        };
        for signal in &signals {
            emit_in_process(
                &ctx.store,
                &ctx.workspace.session_name,
                &ctx.workspace.project_root,
                signal,
            );
        }
    }

    fn leader_session(&self, run: &AttemptRun) -> Option<crate::ids::AgentSessionId> {
        crate::harness::run::load(self.state, &run.run_id)
            .ok()
            .and_then(|run| run.agent_id)
    }

    fn start_groups(
        &self,
        record: &crate::store::workflow::RunRecord,
        agent: &str,
        account: Option<&crate::ids::LoginName>,
    ) -> Result<Vec<crate::harness::throttle::StartGroup>, WorkflowErr> {
        workflow::launch_groups(record, agent, account)
    }

    fn budget_refusal(
        &self,
        record: &crate::store::workflow::RunRecord,
        groups: &[crate::harness::throttle::StartGroup],
    ) -> Result<Option<String>, WorkflowErr> {
        workflow::budget_refusal(record, groups)
    }
}

/// Whether the step's pinned `session` names `agent`: the pin resolves to its launch under the
/// kind of each row the session is (a molt may have moved the agent to another kind), or as a
/// launch id under any kind, and that launch must be `agent`'s.
fn pin_names<'a>(
    rows: impl IntoIterator<Item = &'a crate::agents::AgentState> + Clone,
    session: &crate::ids::AgentSessionId,
    agent: &crate::agents::AgentState,
) -> bool {
    let mut kinds: Vec<_> = rows
        .clone()
        .into_iter()
        .filter(|row| &row.agent_id == session)
        .map(|row| &row.kind)
        .collect();
    kinds.push(&agent.kind);
    kinds.into_iter().any(|kind| {
        crate::agents::state::launch_members(rows.clone(), kind, session)
            .iter()
            .any(|row| row.kind == agent.kind && row.agent_id == agent.agent_id)
    })
}

#[cfg(test)]
mod tests {
    use crate::agents::{AgentState, AgentStatus};

    #[test]
    fn a_session_pin_names_its_agent_after_a_molt_to_another_kind() {
        let mut old = AgentState::stub("claude", "sess-old", AgentStatus::Idle);
        old.launch_id = Some("launch_coder".into());
        let mut holder = AgentState::stub("codex", "sess-new", AgentStatus::Idle);
        holder.launch_id = old.launch_id.clone();
        holder.epoch = 1;
        let mut reused = AgentState::stub("codex", "sess-other", AgentStatus::Idle);
        reused.launch_id = Some("launch_other".into());
        let rows = [old, holder.clone(), reused.clone()];
        for pin in ["sess-old", "sess-new", "launch_coder"] {
            assert!(super::pin_names(rows.iter(), &pin.into(), &holder), "{pin}");
            assert!(
                !super::pin_names(rows.iter(), &pin.into(), &reused),
                "{pin}"
            );
        }
        assert!(!super::pin_names(
            rows.iter(),
            &"ses-elsewhere".into(),
            &holder
        ));
    }
}
