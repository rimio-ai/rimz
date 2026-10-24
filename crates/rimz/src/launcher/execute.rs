use super::*;
use crate::harness::run::RunCancellation;
use crate::harness::run_wake::{ExpectedRunFrame, RunWaiter};
use crate::store::run::RunRecord;
use crate::store::writer::{AgentLaunchName, LaunchLogin};
use std::sync::Mutex;
use std::time::Duration;

pub struct SupervisedContext {
    pub root: Option<PathBuf>,
    pub worktree_fresh_command: Option<String>,
    pub machine: Arc<crate::config::MachineConfig>,
    pub mux: Option<crate::ids::MuxName>,
    pub recovery: crate::room::AttendedRecovery,
    pub completion: RunCompletion,
}

#[derive(Clone)]
pub enum RunCompletion {
    Detached,
    Watched {
        cancellation: RunCancellation,
        install_interrupts: fn(RunCancellation) -> Result<()>,
        termination: Option<Arc<Mutex<Option<crate::harness::schedule::runner::CheckSignalGuard>>>>,
    },
}

#[derive(Clone)]
pub struct LeaderRun {
    pub lifecycle: crate::store::run::RunLifecycle,
    pub keep: bool,
    pub report_to: ReportTo,
    pub timeout: Option<Duration>,
    pub retry_of: Option<crate::RunId>,
    pub cleanup: bool,
    pub subagent: bool,
    pub warn: Vec<Duration>,
    pub grace: Option<Duration>,
    pub loop_task: Option<String>,
    pub workflow: Option<crate::agents::WorkflowAttemptRef>,
    pub reader: Option<String>,
    pub response_receipt: bool,
    pub completion: RunCompletion,
}

pub struct PreparedLaunch {
    pub(super) state: PreparedState,
    pub(super) leader_run: Option<LeaderRun>,
}

pub(super) enum PreparedState {
    Layout(Box<PreparedLayout>),
    Ready(Box<FreshLaunch>),
}

pub(super) enum SettledLayout {
    Fresh(Box<FreshLaunch>),
    Handled(LaunchOutcome),
}

#[derive(Clone)]
pub(super) struct FreshLaunch {
    pub(super) context: LaunchContext,
    pub(super) layout: LayoutSpec,
    pub(super) launch: crate::worktree::LaunchCheckout,
    pub(super) checkout: Option<String>,
    pub(super) name: Option<String>,
    pub(super) generated_name: Option<String>,
    pub(super) team: Option<(String, crate::config::Team)>,
    pub(super) channel: Option<String>,
    pub(super) logins: Vec<LoginName>,
    pub(super) managed_launches: Vec<crate::agents::ManagedLaunchState>,
    pub(super) ancestry: Option<LaunchAncestry>,
    pub(super) focus: LaunchFocus,
    pub(super) prompt: Option<String>,
    pub(super) prompt_index: Option<usize>,
    pub(super) leader_index: Option<usize>,
    pub(super) placement: Placement,
    pub(super) pick_session: bool,
    pub(super) loop_reminder: Option<String>,
    pub(super) loop_task: Option<String>,
    pub(super) description: Option<String>,
    pub(super) report_to: ReportTo,
    pub(super) cleanup_worktree: bool,
    pub(super) inherit_cwd: bool,
    pub(super) force_new_tab: bool,
    pub(super) room: Option<Arc<RoomContext>>,
    pub(super) recovery: crate::room::AttendedRecovery,
    pub(super) entry: RoomEntry,
    pub(super) soft_name: bool,
    pub(super) throttle_turn: Option<crate::harness::schedule::throttle::Turn>,
}

pub struct OpenedRun {
    pub record: RunRecord,
    pub waiter: Option<RunWaiter>,
    pub response_path: Option<PathBuf>,
    pub workspace: crate::ResolvedWorkspace,
    pub store: crate::Store,
    pub room: Arc<RoomContext>,
    pub owns_checkout_lifecycle: bool,
    next: Box<FreshLaunch>,
    leader: LeaderRun,
}

impl OpenedRun {
    pub fn retry(mut self, prompt: String, retry_of: crate::RunId) -> PreparedLaunch {
        self.next.prompt = Some(prompt);
        self.next.soft_name = true;
        self.leader.retry_of = Some(retry_of);
        PreparedLaunch {
            state: PreparedState::Ready(self.next),
            leader_run: Some(self.leader),
        }
    }
}

pub fn execute(
    prepared: PreparedLaunch,
    interaction: &mut dyn Interaction,
) -> Result<LaunchOutcome> {
    let fresh = match prepared.state {
        PreparedState::Ready(fresh) => fresh,
        PreparedState::Layout(layout) => match settle_layout(*layout, interaction)? {
            SettledLayout::Fresh(fresh) => fresh,
            SettledLayout::Handled(outcome) => return Ok(outcome),
        },
    };
    execute_fresh(*fresh, prepared.leader_run, interaction)
}

fn budget_gate(fresh: &FreshLaunch, scope: bool) -> Option<String> {
    for (index, cell) in fresh.layout.agent_cells().enumerate() {
        let login = crate::ids::LoginKey {
            kind: cell.kind.clone(),
            name: fresh.logins[index].clone(),
        };
        if let Some(binding) = fresh
            .managed_launches
            .get(index)
            .and_then(|state| state.binding())
            && let Some(reason) = crate::agents::provider_budget_gate(
                fresh.context.store.runtime_paths(),
                &login,
                binding,
                jiff::Timestamp::now(),
            )
        {
            return Some(reason);
        }
        if scope
            && let Some(reason) = crate::harness::budget::scope_gate(
                fresh.context.store.runtime_paths(),
                fresh.context.store.paths(),
                Some(&login),
                &fresh.context.machine,
                jiff::Timestamp::now(),
            )
        {
            return Some(reason);
        }
    }
    None
}

fn execute_fresh(
    mut fresh: FreshLaunch,
    leader: Option<LeaderRun>,
    interaction: &mut dyn Interaction,
) -> Result<LaunchOutcome> {
    if fresh.room.is_none() && matches!(fresh.entry, RoomEntry::Birth) {
        let mux = crate::mux::auto_detect_backend(fresh.context.mux)?;
        let mut room = RoomContext::from_resolved(
            &fresh.context.workspace,
            fresh.context.machine.clone(),
            mux,
            crate::room::RoomSizing::Birth,
        )?;
        if let Some(reason) = budget_gate(&fresh, false) {
            return Ok(LaunchOutcome::BudgetExceeded { reason });
        }
        let birth = room.birth(crate::room::RoomBirth::Supervised {
            cwd: fresh.launch.cwd.clone(),
            recovery: fresh.recovery,
        });
        let reset = match &birth {
            Ok(birth) => birth.reset.as_ref(),
            Err(error) => error
                .downcast_ref::<crate::room::ResetRecoveryError>()
                .map(|error| &error.report),
        };
        if let Some(report) = reset {
            interaction.notice(Notice::RoomReset {
                session: room.session_name(),
                report,
            })?;
        }
        for warning in birth?.skipped {
            interaction.notice(Notice::Warning(&format!(
                "rimz: warning: {}",
                warning.message
            )))?;
        }
        fresh.context.workspace.session_name = room.session_name().to_owned();
        fresh.room = Some(Arc::new(room));
    }
    if leader.is_some()
        && let Some(reason) = budget_gate(&fresh, true)
    {
        return Ok(LaunchOutcome::BudgetExceeded { reason });
    }
    let room = fresh
        .room
        .as_ref()
        .context("prepared launch has no room")?
        .clone();
    open_fresh(fresh, leader, interaction, room.backend())
}

fn open_fresh(
    fresh: FreshLaunch,
    leader: Option<LeaderRun>,
    interaction: &mut dyn Interaction,
    backend: &dyn crate::mux::MuxBackend,
) -> Result<LaunchOutcome> {
    let room = fresh.room.as_ref().context("prepared launch has no room")?;
    let workspace = &fresh.context.workspace;
    let store = &fresh.context.store;
    let cwd = &fresh.launch.cwd;
    let leader_index = fresh.leader_index.unwrap_or(0);
    let mut record = leader
        .as_ref()
        .map(|leader| {
            let cell = fresh
                .layout
                .agent_cells()
                .nth(leader_index)
                .context("run-bearing launch has no leader cell")?;
            let adapter = crate::agents::find_definition(&cell.kind)
                .context("run-bearing launch has no agent definition")?;
            let prompt = fresh.prompt.as_deref().unwrap_or_default();
            crate::harness::launch::compile_provider_argv(
                adapter,
                cell.kind.as_str(),
                &crate::harness::launch::ExecAction::Launch {
                    prompt: Some(prompt.to_owned()),
                    extra_args: cell.args.clone(),
                },
                cwd,
            )?;
            let mut record = RunRecord::new(
                workspace.workspace_id.clone(),
                cell.kind.clone(),
                cell.launch.mode.unwrap_or(PermissionMode::Auto),
                prompt.to_owned(),
                cwd.clone(),
            );
            record.lifecycle = leader.lifecycle;
            record.keep = leader.keep;
            record.report_to = leader.report_to;
            record.subagent = leader.subagent;
            record.budget.clone_from(&cell.launch.budget);
            record.deadline_at = leader
                .timeout
                .map(|timeout| record.started_at.checked_add(timeout))
                .transpose()
                .context("computing supervised run deadline")?;
            if leader.subagent {
                record.timeout = leader.timeout;
                record.warn.clone_from(&leader.warn);
                record.grace = leader.grace.filter(|grace| !grace.is_zero());
            }
            record.retry_of.clone_from(&leader.retry_of);
            record.loop_task.clone_from(&leader.loop_task);
            record.workflow.clone_from(&leader.workflow);
            record.reader.clone_from(&leader.reader);
            Ok::<_, anyhow::Error>(record)
        })
        .transpose()?;
    let mut requests = launch_identity_requests(
        &fresh.layout,
        fresh.name.as_deref(),
        fresh
            .generated_name
            .as_deref()
            .or_else(|| fresh.launch.generated_name()),
        fresh.team.as_ref().map(|(name, _)| name.as_str()),
        fresh.team.as_ref().map(|(_, team)| team.roles.as_slice()),
        fresh.channel.as_deref(),
        fresh.prompt.as_deref().zip(fresh.prompt_index),
        None,
        fresh.ancestry.as_ref(),
    )?;
    for (index, (request, login)) in requests.iter_mut().zip(&fresh.logins).enumerate() {
        request.login = LaunchLogin::Pinned(login.clone());
        request.launch.checkout.clone_from(&fresh.checkout);
        if let Some(leader) = &leader {
            request.launch.workflow.clone_from(&leader.workflow);
            if fresh.soft_name
                && let AgentLaunchName::Explicit(name) = &request.name
            {
                request.name = AgentLaunchName::Soft(name.clone());
            }
            if index == leader_index {
                request.run_id = record.as_ref().map(|record| record.run_id.clone());
            }
        }
    }
    if leader.is_none()
        && let Some(index) = fresh.prompt_index
    {
        requests[index]
            .launch
            .loop_task
            .clone_from(&fresh.loop_task);
    }
    let batch = store.begin_agent_launch_batch(
        &requests,
        AgentLaunchScope {
            session_name: workspace.session_name.clone(),
            cwd: cwd.clone(),
            branch: fresh.launch.branch.clone(),
            description: fresh.description.clone(),
        },
    )?;
    let peer = if leader.is_none() {
        prepare_peer_prompt(store, batch.identities(), cwd, fresh.report_to).inspect_err(|_| {
            let _ = store.fail_agent_launch_batch(&batch);
        })?
    } else {
        None
    };
    let fail_peer = || {
        if let Some((peer, _)) = &peer {
            match crate::harness::run::fail_peer_run(store, peer, "peer launch failed") {
                Ok(Some(_)) => {
                    if let Some((_, launcher)) = peer.launcher() {
                        crate::harness::orphan_sweep::spawn_digest_helper(
                            store.runtime_paths(),
                            launcher.clone(),
                        );
                    }
                }
                Ok(None) => {}
                Err(error) => tracing::warn!(%error, "could not fail peer launch run"),
            }
        }
    };
    let in_place = fresh.placement == Placement::SamePane && leader.is_none();
    let mut panes = compile_layout_panes(
        &fresh.layout,
        LayoutPaneParams {
            runtime: store.runtime_paths(),
            cwd,
            cleanup_worktree: fresh.cleanup_worktree && fresh.launch.owns_checkout_lifecycle(),
            in_place,
            pick_session: fresh.pick_session,
            resume_seeds: None,
            launch_identities: batch.identities(),
            fallback_channel: None,
            loop_reminder: fresh.loop_reminder.as_deref(),
            leader_run: leader
                .as_ref()
                .zip(record.as_ref())
                .map(|(leader, record)| crate::harness::plan::LayoutRunParams {
                    run_id: &record.run_id,
                    leader_index,
                    managed_launch: fresh.managed_launches.get(leader_index),
                    cleanup: leader.cleanup,
                    subagent: leader.subagent,
                }),
        },
    )
    .inspect_err(|_| {
        let _ = store.fail_agent_launch_batch(&batch);
        fail_peer();
    })?;
    panes.focused_pane = team_leader_pane(&fresh.layout, fresh.team.as_ref().map(|(_, team)| team));
    let waiter = if let (Some(leader), Some(record)) = (&leader, &mut record) {
        record.agent_name = Some(batch.identities()[leader_index].name.clone());
        publish_run(store, record, &leader.completion)?
    } else {
        None
    };
    if let Some(turn) = &fresh.throttle_turn {
        turn.report_launch(
            &workspace.workspace_id,
            &batch.identities()[leader_index].agent_id,
        );
    }
    if fresh.pick_session && in_place {
        interaction.notice(Notice::PickerReceipt {
            kind: batch.identities()[0].kind.as_str(),
            channel: fresh.channel.as_deref(),
            cwd: batch.identities()[0]
                .launch
                .checkout
                .as_deref()
                .map(Path::new)
                .unwrap_or(cwd),
            project: &workspace.project_root,
        })?;
    }
    let title = if let Some(leader) = &leader {
        leader.loop_task.as_ref().map_or_else(
            || {
                placement::run_tab_title(
                    store,
                    fresh.channel.as_deref(),
                    leader.subagent,
                    &batch.identities()[leader_index].kind,
                )
            },
            |task| format!("loop {task}"),
        )
    } else {
        fresh.channel.as_deref().map_or_else(
            || {
                crate::harness::spec::default_tab_title(
                    &fresh.layout,
                    fresh.launch.worktree_name.as_deref(),
                    fresh.team.as_ref().map(|(name, _)| name.as_str()),
                )
            },
            |channel| format!("#{channel}"),
        )
    };
    let placement = PlacementRequest {
        placement: fresh.placement,
        mux: room.mux_name(),
        tab_anchor: if leader.is_some() {
            crate::mux::own_pane_id(room.mux_name())
        } else {
            placement::resolve_tab_anchor(
                fresh.placement,
                fresh.team.is_some(),
                crate::mux::own_pane_id(room.mux_name()),
                checkout::pinned_room_root().as_deref(),
                &workspace.project_root,
            )
        },
        cwd: cwd.clone(),
        title,
        panes,
        sidebar: room.sidebar_options(cwd),
        identity_env: crate::room::pane_identity_env(
            workspace,
            fresh.checkout.as_deref().map(Path::new).unwrap_or(cwd),
            fresh.channel.as_deref(),
            fresh.inherit_cwd,
        ),
        focus: fresh.focus,
        errors: if leader.is_some() {
            PlacementErrors {
                new_tab: "opening run pane",
                new_pane: "opening run pane",
                same_pane: "opening run pane",
            }
        } else {
            LAUNCH_PLACEMENT_ERRORS
        },
    };
    let opened = if let (Some(leader), Some(record)) = (&leader, &record) {
        placement::execute_run(
            backend,
            workspace,
            store,
            &batch,
            placement,
            &record.run_id,
            fresh.force_new_tab,
            leader.subagent,
        )
    } else {
        placement::execute(backend, store, &batch, placement, || {
            crate::harness::assist_log::record_tier_fallbacks(batch.identities())
        })
    };
    opened.inspect_err(|error| {
        fail_peer();
        if let Some(record) = &record {
            let _ = crate::harness::run::fail_if_nonterminal(
                store.paths(),
                &record.run_id,
                &failure_message(error),
            );
            let _ = store.fail_agent_launch_batch(&batch);
        }
    })?;
    let run = record.zip(leader).map(|(record, leader)| OpenedRun {
        response_path: leader
            .response_receipt
            .then(|| crate::harness::run::response_path(store.paths(), &record))
            .flatten(),
        record,
        waiter,
        workspace: workspace.clone(),
        store: store.clone(),
        room: room.clone(),
        owns_checkout_lifecycle: fresh.launch.owns_checkout_lifecycle(),
        next: Box::new(fresh.clone()),
        leader,
    });
    Ok(LaunchOutcome::Opened(Box::new(OpenedLayout {
        identities: batch.identities().to_vec(),
        leader_index: fresh.leader_index,
        team: fresh.team,
        channel: fresh.channel,
        cwd: fresh.launch.cwd,
        in_place,
        pick_session: fresh.pick_session,
        peer: peer.map(|(_, run)| run),
        run,
    })))
}

fn failure_message(error: &anyhow::Error) -> String {
    let mut messages: Vec<String> = Vec::new();
    for cause in error.chain() {
        let message = cause.to_string();
        let message = message.trim();
        if !messages.last().is_some_and(|last| last.contains(message)) {
            messages.push(message.to_owned());
        }
    }
    messages.join(": ")
}

fn publish_run(
    store: &crate::Store,
    record: &RunRecord,
    completion: &RunCompletion,
) -> Result<Option<RunWaiter>> {
    let waiter = match completion {
        RunCompletion::Detached => None,
        RunCompletion::Watched {
            cancellation,
            termination,
            install_interrupts,
        } => {
            install_interrupts(cancellation.clone())?;
            let waiter = RunWaiter::bind(
                store.runtime_paths(),
                ExpectedRunFrame {
                    workspace_id: record.workspace_id.clone(),
                    run_id: record.run_id.clone(),
                },
                cancellation.clone(),
            )
            .context("binding run socket")?;
            if let Some(slot) = termination {
                *slot.lock().unwrap_or_else(|error| error.into_inner()) = Some(
                    crate::harness::schedule::runner::cancel_run_on_sigterm(cancellation.clone())?,
                );
            }
            Some(waiter)
        }
    };
    crate::harness::run::create(store.paths(), record).context("recording run")?;
    Ok(waiter)
}

#[cfg(test)]
mod tests;
