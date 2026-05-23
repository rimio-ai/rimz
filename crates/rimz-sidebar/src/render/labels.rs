//! Vocabulary → wire-form labels. One table per enum, no logic.

use rimz::FeedStatus;
use rimz::ResolutionMethod;
use rimz::Surface;
use rimz::feed::{AgentMode, AgentStatus, FeedKind};

pub(super) fn status_label(status: FeedStatus, surface: Surface) -> &'static str {
    match (status, surface) {
        (FeedStatus::Pending, Surface::Bridge) => "active",
        (FeedStatus::Pending, _) => "waiting",
        (FeedStatus::Resolved, _) => "answered",
        (FeedStatus::TimedOut, _) => "timed out",
        (FeedStatus::Abandoned, _) => "abandoned",
    }
}

pub(super) fn kind_label(kind: FeedKind) -> &'static str {
    match kind {
        FeedKind::Permission => "permission",
        FeedKind::PlanApproval => "plan",
        FeedKind::Question => "question",
        FeedKind::NeedsInput => "needs input",
        FeedKind::Completion => "completion",
        FeedKind::Failure => "failure",
        FeedKind::ToolTelemetry => "tool",
        FeedKind::SubAgentStarted => "sub-agent started",
        FeedKind::SubAgentStopped => "sub-agent stopped",
        FeedKind::Generic => "activity",
    }
}

pub(super) fn resolution_method(method: ResolutionMethod) -> &'static str {
    match method {
        ResolutionMethod::HookBridge => "hook",
        ResolutionMethod::PaneSend => "pane",
        ResolutionMethod::Cli => "cli",
        ResolutionMethod::Sidebar => "sidebar",
        ResolutionMethod::Dismiss => "dismiss",
        ResolutionMethod::AgentMovedOn => "agent moved on",
    }
}

pub(super) fn agent_status(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Running => "running",
        AgentStatus::Waiting => "waiting",
        AgentStatus::Idle => "idle",
        AgentStatus::Success => "success",
        AgentStatus::Failed => "failed",
    }
}

pub(super) fn agent_mode(mode: AgentMode) -> &'static str {
    match mode {
        AgentMode::Interactive => "interactive",
        AgentMode::Plan => "plan",
        AgentMode::Auto => "auto",
        AgentMode::Bypass => "bypass",
        AgentMode::Unknown => "unknown",
    }
}
