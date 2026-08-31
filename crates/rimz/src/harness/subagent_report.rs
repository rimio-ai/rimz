//! Parent-facing completion reports for supervised subagents.
//!
//! The durable run record remains truth. Report delivery is best-effort latency:
//! a parked message returns the settled outcome when the parent still exists.
//! Sibling state is read at send time, so children settling together may each
//! truthfully report that all siblings have finished.

use crate::agents::AgentState;
use crate::harness::run::{self, RunRecord, RunStatus};
use crate::ids::MessageId;
use crate::message::{DeliveryGate, MessageSender};
use crate::workspace::ResolvedWorkspace;
use crate::{RuntimeScope, Store};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReportOutcome {
    Queued {
        message_id: MessageId,
        delivered: bool,
        parent: String,
    },
    NoParent,
    ParentEnded,
    NotRequested,
}

#[derive(Debug, thiserror::Error)]
pub enum ReportErr {
    #[error(transparent)]
    Store(#[from] crate::store::StoreErr),
    #[error(transparent)]
    Run(#[from] run::RunStoreErr),
    #[error(transparent)]
    Deliver(#[from] crate::message::deliver::DeliverErr),
}

pub type Result<T> = std::result::Result<T, ReportErr>;

pub fn report_settled_child(
    workspace: &ResolvedWorkspace,
    store: &Store,
    run: &RunRecord,
) -> Result<ReportOutcome> {
    if !run.subagent || !run.report_to_parent || !run.status.is_terminal() {
        return Ok(ReportOutcome::NotRequested);
    }

    let projection = store.runtime_projection(RuntimeScope::Audit)?;
    let Some(child) = projection.agents.iter().find(|agent| {
        run.agent_id.as_ref().map_or_else(
            || agent.name.as_deref() == run.agent_name.as_deref(),
            |agent_id| &agent.agent_id == agent_id,
        )
    }) else {
        return Ok(ReportOutcome::NoParent);
    };
    let (Some(parent_kind), Some(parent_id)) = (
        child.parent_agent_kind.as_ref(),
        child.parent_agent_id.as_ref(),
    ) else {
        return Ok(ReportOutcome::NoParent);
    };
    let Some(parent) = projection.agents.iter().find(|agent| {
        &agent.kind == parent_kind
            && (&agent.agent_id == parent_id || agent.launch_id.as_ref() == Some(parent_id))
    }) else {
        return Ok(ReportOutcome::NoParent);
    };
    if parent.ended_at.is_some() {
        return Ok(ReportOutcome::ParentEnded);
    }

    let runs = run::list(store.paths())?;
    let still_running = crate::harness::target::launched_children(&projection.agents, parent)
        .into_iter()
        .filter_map(|sibling| {
            let sibling_run = run::newest_run_for_agent(&runs, sibling)?;
            (sibling_run.run_id != run.run_id && !sibling_run.status.is_terminal()).then(|| {
                sibling
                    .name
                    .as_deref()
                    .unwrap_or_else(|| sibling.agent_id.as_str())
            })
        })
        .collect::<Vec<_>>();
    let text = compose_report(child, run, &still_running);
    let name = child_name(child, run).to_owned();
    let sender = MessageSender::Subagent {
        kind: child.kind.clone(),
        name,
    };
    let pane_id = parent.pane.as_ref().map(|pane| &pane.pane_id);
    let (message_id, delivered) = crate::message::deliver::queue_report(
        workspace,
        store,
        parent,
        sender,
        text,
        DeliveryGate::Done,
        pane_id,
    )?;
    Ok(ReportOutcome::Queued {
        message_id,
        delivered,
        parent: parent
            .name
            .clone()
            .unwrap_or_else(|| parent.agent_id.to_string()),
    })
}

pub fn compose_report(child: &AgentState, run: &RunRecord, still_running: &[&str]) -> String {
    let name = child_name(child, run);
    let metadata = [child.profile.as_deref(), child.description.as_deref()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
    let metadata = if metadata.is_empty() {
        String::new()
    } else {
        format!(" ({metadata})")
    };
    let finished_at = run.completed_at.unwrap_or(run.updated_at);
    let elapsed_seconds = finished_at.duration_since(run.started_at).as_secs().max(0) as u64;
    let elapsed = crate::utils::time::format_compact_duration(std::time::Duration::from_secs(
        elapsed_seconds,
    ));
    let mut report = format!(
        "@{name}{metadata} {} in {elapsed}.\n{}",
        run.status.label(),
        sibling_summary(still_running)
    );
    if let Some(detail) = report_detail(run) {
        report.push_str("\n\n");
        report.push_str(&detail);
    }
    report
}

fn child_name<'a>(child: &'a AgentState, run: &'a RunRecord) -> &'a str {
    child
        .name
        .as_deref()
        .or(run.agent_name.as_deref())
        .unwrap_or_else(|| child.agent_id.as_str())
}

fn sibling_summary(still_running: &[&str]) -> String {
    match still_running {
        [] => "All your subagents have finished.".to_owned(),
        [name] => format!("1 subagent still running: @{name}."),
        names => format!(
            "{} subagents still running: {}.",
            names.len(),
            names
                .iter()
                .map(|name| format!("@{name}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn report_detail(run: &RunRecord) -> Option<String> {
    if run.status == RunStatus::Completed {
        return Some(
            run.last_message
                .clone()
                .filter(|message| !message.trim().is_empty())
                .unwrap_or_else(|| match run.transcript_path.as_deref() {
                    Some(path) => format!("(no final message; transcript: {path})"),
                    None => "(no final message)".to_owned(),
                }),
        );
    }

    let mut detail = run.failure_tail.clone().unwrap_or_default();
    if let Some(path) = run.transcript_path.as_deref() {
        if !detail.is_empty() {
            detail.push('\n');
        }
        detail.push_str(&format!("transcript: {path}"));
    }
    (!detail.is_empty()).then_some(detail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{AgentStatus, PermissionMode};
    use crate::ids::{AgentKind, WorkspaceId};
    use jiff::Timestamp;
    use std::path::{Path, PathBuf};

    fn child() -> AgentState {
        let mut child = AgentState::stub("codex", "sess-child", AgentStatus::Success);
        child.name = Some("naming".to_owned());
        child.profile = Some("explorer".to_owned());
        child.description = Some("map spec/profile surfaces".to_owned());
        child
    }

    fn run(status: RunStatus) -> RunRecord {
        let mut run = RunRecord::new(
            WorkspaceId::from_project_root(Path::new("/tmp/subagent-report")),
            AgentKind::new_unchecked("codex"),
            PermissionMode::Auto,
            "map it".to_owned(),
            PathBuf::from("/tmp/subagent-report"),
        );
        run.status = status;
        run.started_at = Timestamp::from_second(1_000).unwrap();
        run.completed_at = Some(Timestamp::from_second(1_252).unwrap());
        run.updated_at = run.completed_at.unwrap();
        run
    }

    #[test]
    fn completed_report_keeps_the_final_message_verbatim() {
        let mut run = run(RunStatus::Completed);
        run.last_message = Some("Done.\n\nTwo paragraphs.\n".to_owned());

        assert_eq!(
            compose_report(&child(), &run, &["runtime"]),
            "@naming (explorer · map spec/profile surfaces) completed in 4m12s.\n\
             1 subagent still running: @runtime.\n\n\
             Done.\n\nTwo paragraphs.\n"
        );
    }

    #[test]
    fn failed_report_includes_siblings_tail_and_transcript() {
        let mut run = run(RunStatus::TimedOut);
        run.failure_tail = Some("provider did not stop".to_owned());
        run.transcript_path = Some("/tmp/transcript.jsonl".to_owned());

        assert_eq!(
            compose_report(&child(), &run, &[]),
            "@naming (explorer · map spec/profile surfaces) timed out in 4m12s.\n\
             All your subagents have finished.\n\n\
             provider did not stop\n\
             transcript: /tmp/transcript.jsonl"
        );
    }
}
