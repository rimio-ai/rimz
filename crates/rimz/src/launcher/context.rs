//! Caller attribution and current-lane facts shared by launch and message paths.

use crate::agents::AgentState;
use crate::ids::LaneName;
use crate::store::message::MessageSender;
use anyhow::Result;
use std::path::Path;

pub enum CurrentChannel {
    Named(LaneName),
    Derived(LaneName),
    Root(LaneName),
    Unscoped,
}

impl CurrentChannel {
    pub fn as_deref(&self) -> Option<&LaneName> {
        match self {
            Self::Named(name) | Self::Derived(name) | Self::Root(name) => Some(name),
            Self::Unscoped => None,
        }
    }

    pub fn into_name(self) -> Option<LaneName> {
        match self {
            Self::Named(name) | Self::Derived(name) | Self::Root(name) => Some(name),
            Self::Unscoped => None,
        }
    }

    pub fn address_context(&self) -> crate::address::AddressContext {
        crate::address::AddressContext {
            channel: self.as_deref().cloned(),
            origin: match self {
                Self::Derived(_) => crate::address::ChannelOrigin::Directory,
                Self::Root(_) => crate::address::ChannelOrigin::Root,
                Self::Named(_) | Self::Unscoped => crate::address::ChannelOrigin::Stamped,
            },
        }
    }
}

/// The current channel a command runs in: the lane `RIMZ_CHANNEL` names, read
/// as typed input so trunk's aliases mean trunk, else the caller's stored card
/// channel, else the worktree rule. A human shell at the project root has no
/// current channel. The value keeps its provenance, so create-on-miss launches
/// in place on a derived one.
pub fn current_channel(
    workspace: &crate::ResolvedWorkspace,
    store: Option<&crate::Store>,
) -> CurrentChannel {
    current_channel_with(
        workspace,
        std::env::var(crate::workspace::ENV_CHANNEL).ok(),
        || {
            let store = store?;
            let caller = resolve_caller(store).ok()??;
            let rows = store
                .runtime_projection(crate::RuntimeScope::Audit)
                .ok()?
                .agents;
            Some((caller, rows))
        },
    )
}

pub fn current_channel_with(
    workspace: &crate::ResolvedWorkspace,
    explicit: Option<String>,
    caller: impl FnOnce() -> Option<(crate::harness::ancestry::CallerIdentity, Vec<AgentState>)>,
) -> CurrentChannel {
    if let Some(channel) = explicit.filter(|channel| !channel.is_empty()) {
        let project_root = &workspace.project_root;
        if crate::address::LaneSelector::parse(Some(&channel), Some(project_root)).is_trunk() {
            return CurrentChannel::Root(crate::address::trunk_lane_name(project_root));
        }
        return CurrentChannel::Named(LaneName::dashed(&channel));
    }
    if let Some((caller, rows)) = caller()
        && let Ok(agent) = crate::harness::ancestry::resolve_launch_caller(&rows, &caller)
        && let Some(channel) = agent.lane_name()
    {
        return if agent.in_trunk() {
            CurrentChannel::Root(channel)
        } else if agent.own_lane_path().is_some() {
            CurrentChannel::Derived(channel)
        } else {
            CurrentChannel::Named(channel)
        };
    }
    directory_channel(workspace, &workspace.worktree_root)
        .map_or(CurrentChannel::Unscoped, CurrentChannel::Derived)
}

pub fn directory_channel(workspace: &crate::ResolvedWorkspace, cwd: &Path) -> Option<LaneName> {
    crate::harness::lanes::directory_channel(&workspace.project_root, cwd)
}

/// The lane a fresh launch's caller is in: the calling agent's row lane, else
/// a caller-less pane's pin (`RIMZ_CHANNEL` homed at `RIMZ_WORKTREE_PATH`). A
/// harness-owned launch has neither.
pub fn caller_lane(
    project_root: &Path,
    caller: Option<&AgentState>,
    harness_owned: bool,
) -> Option<crate::harness::lanes::CallerLane> {
    use crate::harness::lanes::{CallerLane, row_lane};
    if let Some(caller) = caller {
        return row_lane(caller, project_root).map(CallerLane::Agent);
    }
    if harness_owned {
        return None;
    }
    let pin = pane_pin(project_root)?;
    pin.name().is_some().then_some(CallerLane::Pin(pin))
}

/// The lane name a pane split at `dir` carries on: the calling pane's pin name
/// when `dir` is in the pin's checkout, else none.
pub fn pin_channel_at(project_root: &Path, dir: &Path) -> Option<String> {
    let pin = pane_pin(project_root)?;
    let same_checkout = crate::harness::lanes::own_lane(project_root, dir).path() == pin.path();
    same_checkout
        .then(|| pin.stored_name().map(ToOwned::to_owned))
        .flatten()
}

/// The calling pane's pin: its `RIMZ_CHANNEL` adopted at the checkout its
/// `RIMZ_WORKTREE_PATH` classifies into.
fn pane_pin(project_root: &Path) -> Option<crate::ids::Lane> {
    let name = std::env::var(crate::workspace::ENV_CHANNEL).ok()?;
    let home = std::env::var_os(crate::workspace::ENV_WORKTREE_PATH)?;
    Some(crate::harness::lanes::own_lane(project_root, Path::new(&home)).adopt(Some(&name)))
}

/// The caller identity for `message`. `--no-from` is deliberately unattributed
/// system text so delivery stays verbatim. RimZ-launched agents carry their
/// identity in the environment; a bare-resumed provider is recognised through
/// its live durable owner in the process ancestry. Ordinary shells stay human.
pub fn sender_for(
    caller: Option<&crate::harness::ancestry::CallerIdentity>,
    channel: Option<&LaneName>,
    no_from: bool,
) -> MessageSender {
    if no_from {
        return MessageSender::System;
    }
    let Some(caller) = caller else {
        return MessageSender::Human;
    };
    MessageSender::Agent {
        agent_id: caller.launch_id.clone(),
        kind: caller.kind.clone(),
        name: caller.name.clone(),
        profile: caller.profile.clone(),
        role: caller.role.clone(),
        channel: channel.cloned(),
    }
}

pub fn resolve_caller(
    store: &crate::Store,
) -> Result<Option<crate::harness::ancestry::CallerIdentity>> {
    let caller = crate::harness::ancestry::CallerIdentity::from_env();
    if caller
        .as_ref()
        .is_some_and(|caller| caller.launch_id.is_some())
    {
        return Ok(caller);
    }
    let projection = store.runtime_projection(crate::RuntimeScope::Audit)?;
    Ok(resolve_caller_from(&projection.agents, caller))
}

pub fn resolve_caller_from(
    agents: &[crate::agents::AgentState],
    caller: Option<crate::harness::ancestry::CallerIdentity>,
) -> Option<crate::harness::ancestry::CallerIdentity> {
    let mut caller = caller
        .or_else(|| crate::harness::ancestry::CallerIdentity::from_process_ancestry(agents))?;
    if caller.launch_id.is_none()
        && let Ok(agent) = crate::harness::ancestry::resolve_launch_caller(agents, &caller)
    {
        caller.launch_id = Some(
            agent
                .launch_id
                .clone()
                .unwrap_or_else(|| agent.agent_id.clone()),
        );
    }
    Some(caller)
}
