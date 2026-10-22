use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use rimz::agents::{AgentState, AskDelivery, AskKind, ProviderCapacity, TurnErrorClass};
use rimz::harness::run::{SupervisedRunOutcome, SupervisedRunRequest};
use rimz::harness::schedule::takeover::{self, Takeover};
use rimz::harness::workflow::{self, WorkflowErr};
use rimz::store::run::{ReportTo, RunRecord};
use rimz::store::snapshot::SidebarSnapshot;
use rimz::store::workflow::{
    Asked, AttemptOutcome, AttemptRun, ChildRef, Children, Limited, TeamRef,
};

use crate::cli::{GlobalFlags, agents_cmd, ctx::Ctx, supervised};

pub(super) struct Host<'a> {
    state: &'a rimz::StatePaths,
    globals: &'a GlobalFlags,
}

impl<'a> Host<'a> {
    pub(super) fn new(state: &'a rimz::StatePaths, globals: &'a GlobalFlags) -> Self {
        Self { state, globals }
    }

    fn ctx(&self) -> Result<Ctx> {
        Ctx::for_workspace(self.state.workspace_id.clone(), self.globals.mux)
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
            let takeover = takeover::read(&snapshot.agents, scope, root)?;
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
            let Takeover::Stop(agents) = takeover::read(&snapshot.agents, scope, root)? else {
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
        let mut globals = self.globals.clone();
        globals.root = Some(step.project_root);
        let mut request = SupervisedRunRequest::new(
            step.profile,
            step.prompt,
            None,
            rimz::agents::ManagedLaunchState::PendingResolution,
        );
        request.cwd = Some(step.cwd);
        request.background = true;
        request.self_cleanup_on_completion = true;
        request.force_new_tab = true;
        request.report_to = ReportTo::Nobody;
        request.workflow = Some(step.workflow);
        let result = (|| -> Result<_> {
            let ctx = self.ctx()?;
            globals.mux = Some(crate::cli::render::room::present_mux_pick(
                rimz::room::session::pick_mux_for_session(&ctx.workspace.session_name, globals.mux),
            )?);
            supervised::run::run_supervised(
                request,
                supervised::SupervisedPresentation::text(false),
                &globals,
                &mut None,
            )
        })();
        let (outcome, reason) = match result {
            Ok(Some(SupervisedRunOutcome::Background {
                run_id, agent_name, ..
            })) => {
                return Ok(Ok(AttemptRun {
                    run_id,
                    handle: format!("@{agent_name}"),
                }));
            }
            Ok(Some(SupervisedRunOutcome::BudgetExceeded { reason })) => {
                (AttemptOutcome::Limited, reason)
            }
            Ok(None) => (AttemptOutcome::Died, "workflow launch aborted".into()),
            Ok(Some(SupervisedRunOutcome::Record(_))) => {
                unreachable!("background request returns a background identity")
            }
            Err(error) => (AttemptOutcome::Died, format!("{error:#}")),
        };
        Ok(Err(workflow::LaunchFailure { outcome, reason }))
    }

    fn observe(&self, attempt: &AttemptRun) -> Result<workflow::Observed, WorkflowErr> {
        (|| -> Result<_> {
            let ctx = self.ctx()?;
            let run = rimz::harness::run::load(self.state, &attempt.run_id)?;
            let snapshot = ctx.cached_snapshot()?;
            let mut observed = workflow::Observed {
                status: run.status,
                ask: None,
                limited: None,
                children: Children::default(),
                cost_usd: run.cost_usd,
            };
            let Some(root) = agent(&snapshot, &run) else {
                return Ok(observed);
            };
            let stopped = descendants(&snapshot.agents, root);
            observed.children = children(&snapshot, root, &stopped);
            let peers = rimz::address::addressable_agents(&snapshot);
            for current in std::iter::once(root).chain(stopped) {
                if observed.ask.is_none()
                    && let Some(ask) = rimz::agents::read_open_ask(self.state, current, None)?
                    && ask.delivery != AskDelivery::Async
                {
                    let text = ask
                        .context
                        .into_iter()
                        .chain(ask.questions.into_iter().map(|question| question.question))
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    observed.ask = Some(Asked {
                        text,
                        kind: match ask.open.kind {
                            AskKind::Question => "question",
                            AskKind::Permission => "permission",
                            AskKind::PlanApproval => "plan_approval",
                        }
                        .into(),
                        handle: rimz::address::agent_handle(current, &peers, true),
                    });
                }
                if observed.limited.is_none()
                    && current.displayed_turn_error().is_some_and(|(class, _)| {
                        matches!(
                            class,
                            TurnErrorClass::PausedRateLimit | TurnErrorClass::PausedSpendLimit
                        )
                    })
                {
                    observed.limited = Some(Limited {
                        reset_at: ProviderCapacity::read(ctx.runtime(), &current.login_key())
                            .and_then(|capacity| {
                                capacity.latest_spent_window_reset_for_model(
                                    Timestamp::now(),
                                    current.model.as_deref(),
                                )
                            }),
                    });
                }
            }
            Ok(observed)
        })()
        .map_err(host_error)
    }

    fn stop(&self, attempt: &AttemptRun) -> Result<Children, WorkflowErr> {
        (|| -> Result<_> {
            let ctx = self.ctx()?;
            let run = rimz::harness::run::load(self.state, &attempt.run_id)?;
            let snapshot = ctx.cached_snapshot()?;
            let Some(root) = agent(&snapshot, &run) else {
                supervised::stop_supervised_run(&ctx.workspace, &ctx.store, self.globals, &run)?;
                return Ok(Children::default());
            };
            let stopped = descendants(&snapshot.agents, root);
            let result = children(&snapshot, root, &stopped);
            let peers = rimz::address::addressable_agents(&snapshot);
            let handle = rimz::address::agent_handle(root, &peers, true);
            agents_cmd::stop_resolved(&ctx, self.globals, &snapshot, &[root])?
                .remove(0)
                .into_result(&handle)
                .context("stopping workflow attempt")?;
            Ok(result)
        })()
        .map_err(host_error)
    }
}
