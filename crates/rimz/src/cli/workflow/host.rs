use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use rimz::agents::{AgentState, AskDelivery, AskKind, ProviderCapacity, TurnErrorClass};
use rimz::harness::schedule::takeover::{self, Takeover};
use rimz::harness::workflow::{self, WorkflowErr};
use rimz::ids::EventId;
use rimz::launcher::{self, LaunchOutcome, Preparation};
use rimz::store::run::{ReportTo, RunLifecycle, RunRecord, StoppedTurnDisposition};
use rimz::store::snapshot::SidebarSnapshot;
use rimz::store::workflow::{
    Asked, Asking, AttemptOutcome, AttemptRun, ChildRef, Children, Limited, TeamRef,
};

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

fn agent<'a>(snapshot: &'a SidebarSnapshot, run: &RunRecord) -> Option<&'a AgentState> {
    run.agent_id
        .as_ref()
        .and_then(|id| rimz::address::launch_row(snapshot.agents.iter(), &run.kind, id))
}

fn descendants<'a>(agents: &'a [AgentState], root: &AgentState) -> Vec<&'a AgentState> {
    let mut pending = rimz::address::launched_children(agents, root);
    let mut found = Vec::new();
    let mut seen = BTreeSet::new();
    while let Some(child) = pending.pop() {
        if child.ended_at.is_some() || !seen.insert((&child.kind, &child.agent_id)) {
            continue;
        }
        pending.extend(rimz::address::launched_children(agents, child));
        found.push(child);
    }
    found
}

fn child_ref(agent: &AgentState, peers: &[&AgentState]) -> ChildRef {
    ChildRef {
        handle: rimz::address::agent_handle(agent, peers, true),
        kind: agent.kind.clone(),
        team: agent.team.as_ref().map(|name| TeamRef {
            name: name.clone(),
            channel: agent.channel().unwrap_or_else(|| "external".into()),
            stage: agent
                .lane_path()
                .and_then(|path| rimz::harness::scratch::board_stage(Path::new(path)))
                .map(|stage| stage.name),
        }),
    }
}

fn children(snapshot: &SidebarSnapshot, root: &AgentState, stopped: &[&AgentState]) -> Children {
    let peers = rimz::address::addressable_agents(snapshot);
    let mut live = rimz::address::launched_fleet(snapshot.agents.iter(), root);
    for cohort in rimz::address::team_cohorts(snapshot.agents.iter()) {
        if cohort.members.iter().any(|member| member.launcher_is(root)) {
            live.extend(cohort.members);
        }
    }
    let mut seen = BTreeSet::new();
    live.retain(|agent| {
        agent.ended_at.is_none()
            && rimz::address::is_launch_row(snapshot.agents.iter(), agent)
            && !stopped
                .iter()
                .any(|child| child.kind == agent.kind && child.agent_id == agent.agent_id)
            && seen.insert((&agent.kind, &agent.agent_id))
    });
    Children {
        stopped: stopped
            .iter()
            .map(|child| child_ref(child, &peers))
            .collect(),
        live: live
            .into_iter()
            .map(|child| child_ref(child, &peers))
            .collect(),
    }
}

impl workflow::AttemptHost for Host<'_> {
    fn occupancy(&self, scope: &Path, root: &Path) -> Result<workflow::Occupancy, WorkflowErr> {
        (|| -> Result<_> {
            let ctx = self.ctx()?;
            let snapshot = ctx.alive_snapshot()?;
            let card_status = rimz::harness::CardStatus::new(&ctx.store);
            let takeover = takeover::read(&snapshot, &card_status, scope, root)?;
            let peers = rimz::address::addressable_agents(&snapshot);
            let handle = |agent: &AgentState| rimz::address::agent_handle(agent, &peers, true);
            Ok(match takeover {
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
            })
        })()
        .map_err(host_error)
    }

    fn evict(&self, scope: &Path, root: &Path) -> Result<Vec<String>, WorkflowErr> {
        (|| -> Result<_> {
            let ctx = self.ctx()?;
            let snapshot = ctx.alive_snapshot()?;
            let card_status = rimz::harness::CardStatus::new(&ctx.store);
            let Takeover::Stop(agents) = takeover::read(&snapshot, &card_status, scope, root)?
            else {
                bail!("checkout acquired an open turn before takeover");
            };
            let peers = rimz::address::addressable_agents(&snapshot);
            let stops = agents_cmd::stop_resolved(&ctx, self.globals, &snapshot, &agents)?;
            let mut stopped = Vec::new();
            for (agent, stop) in agents.into_iter().zip(stops) {
                let handle = rimz::address::agent_handle(agent, &peers, true);
                stop.into_result(&handle)?;
                stopped.push(handle);
            }
            Ok(stopped)
        })()
        .map_err(host_error)
    }

    fn launch(
        &self,
        step: workflow::StepLaunch,
    ) -> Result<Result<AttemptRun, workflow::LaunchFailure>, WorkflowErr> {
        let resume = step.resume.clone();
        let workflow_id = step.workflow.run.clone();
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
                    cwd: Some(step.cwd.clone()),
                    cohort: launcher::CohortRequest {
                        bg: true,
                        new_tab: true,
                        ..Default::default()
                    },
                    report_to: ReportTo::Nobody,
                    ..Default::default()
                },
                cwd: Some(step.cwd),
                allow_in_place: false,
                entry: launcher::RoomEntry::Birth,
                starter: launcher::Starter::Workflow,
                resident_loop: None,
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
                    loop_task: None,
                    workflow: Some(step.workflow),
                    throttle_turn: step.throttle_turn,
                    reader: None,
                    response_receipt: false,
                    completion: launcher::RunCompletion::Detached,
                }),
            };
            let mut interaction = crate::cli::launch_interaction::CliInteraction;
            match launcher::prepare_layout(request, &mut interaction)? {
                Preparation::Prepared(prepared) => launcher::execute(*prepared, &mut interaction),
                Preparation::Handled(outcome) => Ok(outcome),
            }
        })();
        let (outcome, reason) = match result {
            Ok(LaunchOutcome::Opened(opened)) => {
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
            Ok(LaunchOutcome::BudgetExceeded { reason }) => (AttemptOutcome::Limited, reason),
            Ok(LaunchOutcome::Declined | LaunchOutcome::Existing) => {
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
        Ok(Err(workflow::LaunchFailure { outcome, reason }))
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
            let mut observed = workflow::Observed {
                status: run.status,
                ask: None,
                limited: None,
                stopped: run.stopped_turn.as_ref().map(|stopped| workflow::Stopped {
                    since: stopped.at,
                    kind: match stopped.disposition {
                        StoppedTurnDisposition::Failed => workflow::StopKind::Error,
                        StoppedTurnDisposition::Canceled => workflow::StopKind::Interrupt,
                    },
                }),
                children: Children::default(),
                cost_usd: run.cost_usd,
                turn: None,
            };
            let Some(root) = agent(&snapshot, &run) else {
                return Ok(observed);
            };
            let stopped = descendants(&snapshot.agents, root);
            observed.children = children(&snapshot, root, &stopped);
            let peers = rimz::address::addressable_agents(&snapshot);
            for current in std::iter::once(root).chain(stopped) {
                if observed.ask.is_none()
                    && !current.is_reviewing_permission(snapshot.now)
                    && let Some(ask) = rimz::agents::read_open_ask(self.state, current, None)?
                    && ask.delivery != AskDelivery::Async
                {
                    let text = ask
                        .context
                        .into_iter()
                        .chain(ask.questions.into_iter().map(|question| question.question))
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    observed.ask = Some(Asking {
                        since: current.permission_review_until().unwrap_or(ask.open.since),
                        ask: Asked {
                            text,
                            kind: match ask.open.kind {
                                AskKind::Question => "question",
                                AskKind::Permission => "permission",
                                AskKind::PlanApproval => "plan_approval",
                            }
                            .into(),
                            handle: rimz::address::agent_handle(current, &peers, true),
                        },
                    });
                }
                let error = current
                    .displayed_turn_error(snapshot.now)
                    .map(|(class, error)| {
                        let limited = matches!(
                            class,
                            TurnErrorClass::PausedRateLimit | TurnErrorClass::PausedSpendLimit
                        );
                        if limited && observed.limited.is_none() {
                            observed.limited = Some(Limited {
                                reset_at: ProviderCapacity::read(
                                    ctx.runtime(),
                                    &current.login_key(),
                                )
                                .and_then(|capacity| {
                                    capacity.latest_spent_window_reset_for_model(
                                        Timestamp::now(),
                                        current.model.as_deref(),
                                    )
                                }),
                            });
                        }
                        workflow::Stopped {
                            since: error.at,
                            kind: if limited {
                                workflow::StopKind::Limit
                            } else {
                                workflow::StopKind::Error
                            },
                        }
                    });
                let budget = current.budget_park.as_ref().map(|park| workflow::Stopped {
                    since: park.at,
                    kind: workflow::StopKind::Budget,
                });
                for stopped in error.into_iter().chain(budget) {
                    if observed.stopped.as_ref().is_none_or(|previous| {
                        previous.kind == workflow::StopKind::Interrupt
                            || stopped.since < previous.since
                    }) {
                        observed.stopped = Some(stopped);
                    }
                }
            }
            if matches!(observation, workflow::Observation::LeaderTurn)
                && let Some(workflow) = &run.workflow
            {
                observed.turn = workflow::leader_turn(
                    &ctx.store,
                    root,
                    &workflow.run,
                    &run.run_id,
                    self.woken_by.as_ref(),
                )?;
            }
            Ok(observed)
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
            let root =
                agent(&snapshot, &run).context("workflow leader is no longer in the room")?;
            let mut message = rimz::message::synthetic::SyntheticMessage {
                agent: root,
                text: text.into(),
                sender: rimz::store::message::MessageSender::System,
                gate: rimz::store::message::DeliveryGate::Done,
                pane_id: None,
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
            let mut result = Children::default();
            let peers = rimz::address::addressable_agents(&snapshot);
            for root in rows
                .iter()
                .filter(|agent| agent.workflow.as_ref() == Some(pin))
            {
                result.live.extend(children(&snapshot, root, &[]).live);
                if root.ended_at.is_none() && !run.matches_agent(root) {
                    result.live.push(child_ref(root, &peers));
                }
                ctx.store.clear_workflow_pin(
                    &root.kind,
                    &root.agent_id,
                    pin,
                    &ctx.workspace.session_name,
                )?;
            }
            let mut seen = BTreeSet::new();
            result
                .live
                .retain(|child| seen.insert((child.kind.clone(), child.handle.clone())));
            Ok(result)
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
            let launch_group = snapshot
                .agents
                .iter()
                .find(|agent| run.matches_agent(agent))
                .and_then(|agent| agent.launch_group.as_ref());
            let roots = snapshot
                .agents
                .iter()
                .filter(|agent| {
                    let detached_cohort = agent.workflow.is_none()
                        && (run.matches_agent(agent)
                            || (launch_group.is_some()
                                && agent.launch_group.as_ref() == launch_group));
                    agent.ended_at.is_none()
                        && run.workflow.is_some()
                        && (agent.workflow == run.workflow || detached_cohort)
                })
                .collect::<Vec<_>>();
            if roots.is_empty() {
                supervised::stop_supervised_run(&ctx.workspace, &ctx.store, self.globals, &run)?;
                return Ok(Children::default());
            }
            let mut stopped = Vec::new();
            let mut result = Children::default();
            for root in &roots {
                let tree = descendants(&snapshot.agents, root);
                result.live.extend(children(&snapshot, root, &tree).live);
                stopped.extend(tree);
                if !run.matches_agent(root) {
                    stopped.push(*root);
                }
            }
            let mut seen = BTreeSet::new();
            stopped.retain(|agent| seen.insert((&agent.kind, &agent.agent_id)));
            let peers = rimz::address::addressable_agents(&snapshot);
            result.stopped = stopped
                .iter()
                .map(|agent| child_ref(agent, &peers))
                .collect();
            let mut live = BTreeSet::new();
            result.live.retain(|agent| {
                !result
                    .stopped
                    .iter()
                    .any(|stopped| stopped.kind == agent.kind && stopped.handle == agent.handle)
                    && live.insert((agent.kind.clone(), agent.handle.clone()))
            });
            supervised::cancel_supervised_run(&ctx.store, &run)?;
            for (root, stop) in roots.iter().zip(agents_cmd::stop_resolved(
                &ctx,
                self.globals,
                &snapshot,
                &roots,
            )?) {
                let handle = rimz::address::agent_handle(root, &peers, true);
                stop.into_result(&handle)
                    .context("stopping workflow attempt")?;
            }
            Ok(result)
        })()
        .map_err(host_error)
    }
}
