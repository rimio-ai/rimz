//! Caller attribution and current-lane facts shared by launch and message paths.

use crate::agents::AgentState;
use crate::store::message::MessageSender;
use anyhow::Result;
use std::path::Path;

pub enum CurrentChannel {
    Named(String),
    Derived(String),
    Root(String),
    Unscoped,
}

impl CurrentChannel {
    pub fn as_deref(&self) -> Option<&str> {
        match self {
            Self::Named(name) | Self::Derived(name) | Self::Root(name) => Some(name),
            Self::Unscoped => None,
        }
    }

    pub fn into_name(self) -> Option<String> {
        match self {
            Self::Named(name) | Self::Derived(name) | Self::Root(name) => Some(name),
            Self::Unscoped => None,
        }
    }

    pub fn address_context(&self) -> crate::address::AddressContext {
        crate::address::AddressContext {
            channel: self.as_deref().map(ToOwned::to_owned),
            origin: match self {
                Self::Derived(_) => crate::address::ChannelOrigin::Directory,
                Self::Root(_) => crate::address::ChannelOrigin::Root,
                Self::Named(_) | Self::Unscoped => crate::address::ChannelOrigin::Stamped,
            },
        }
    }
}

/// The current channel a command runs in: an explicit named lane from
/// `RIMZ_CHANNEL`, else the caller's stored card channel, else the worktree
/// rule. A human shell at the project root has no current channel. The value
/// keeps its provenance, so create-on-miss launches in place on a derived one.
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
        return CurrentChannel::Named(channel);
    }
    if let Some((caller, rows)) = caller()
        && let Ok(agent) = crate::harness::ancestry::resolve_launch_caller(&rows, &caller)
        && let Some(channel) = agent.channel()
    {
        return if agent.root_lane {
            CurrentChannel::Root(channel)
        } else if agent.channel.is_some() {
            CurrentChannel::Named(channel)
        } else {
            CurrentChannel::Derived(channel)
        };
    }
    directory_channel(workspace, &workspace.worktree_root)
        .map_or(CurrentChannel::Unscoped, CurrentChannel::Derived)
}

pub fn directory_channel(workspace: &crate::ResolvedWorkspace, cwd: &Path) -> Option<String> {
    crate::harness::spec::resolve_room_channel(&workspace.project_root, cwd, None, None)
}

/// The caller identity for `message`. `--no-from` is deliberately unattributed
/// system text so delivery stays verbatim. RimZ-launched agents carry their
/// identity in the environment; a bare-resumed provider is recognised through
/// its live durable owner in the process ancestry. Ordinary shells stay human.
pub fn sender_for(
    caller: Option<&crate::harness::ancestry::CallerIdentity>,
    channel: Option<&str>,
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
        channel: channel.map(ToOwned::to_owned),
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
