//! Durable parent-facing completion digests for launched subagent fleets.
//!
//! Run records are stamped together before the digest enters the message
//! queue. This makes the complete row set visible to inline join cancellation
//! and lets the wrapper fast path race safely with the producer backstop.

use std::collections::HashSet;

use rimz::agents::AgentState;
use rimz::harness::run::{self as run, RunRecord, RunStatus};
use rimz::ids::{AgentSessionId, MessageId, RunId};
use rimz::message::deliver::{DeliveryPolicy, deliver_one};
use rimz::message::{DeliveryGate, HarnessNotice, MessageRecord, MessageSender};
use rimz::workspace::ResolvedWorkspace;
use rimz::{RuntimeScope, Store};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ReportOutcome {
    Queued {
        message_id: MessageId,
        delivered: bool,
        parent: String,
    },
    NoParent,
    ParentEnded,
    SiblingsRunning,
    NothingToReport,
    ChildMissing,
    NotRequested,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum ReportErr {
    #[error(transparent)]
    Store(#[from] rimz::store::StoreErr),
    #[error(transparent)]
    Run(#[from] run::RunStoreErr),
    #[error(transparent)]
    Deliver(#[from] rimz::message::deliver::DeliverErr),
}

pub(super) fn report_fleet(
    workspace: &ResolvedWorkspace,
    store: &Store,
    parent_id: &AgentSessionId,
) -> Result<ReportOutcome, ReportErr> {
    let projection = store.runtime_projection(RuntimeScope::Audit)?;
    let Some(parent) = projection
        .agents
        .iter()
        .find(|agent| &agent.agent_id == parent_id || agent.launch_id.as_ref() == Some(parent_id))
    else {
        return Ok(ReportOutcome::NoParent);
    };
    if parent.ended_at.is_some() {
        return Ok(ReportOutcome::ParentEnded);
    }

    let runs = run::list(store.paths())?;
    let mut seen = HashSet::<RunId>::new();
    let children = rimz::harness::target::launched_children(&projection.agents, parent)
        .into_iter()
        .filter_map(|child| {
            let run = newest_run_for_agent(&runs, child)?;
            seen.insert(run.run_id.clone()).then_some((child, run))
        })
        .collect::<Vec<_>>();
    if children.iter().any(|(_, run)| !run.status.is_terminal()) {
        return Ok(ReportOutcome::SiblingsRunning);
    }
    let rows = children
        .into_iter()
        .filter(|(_, run)| run.report_message_id.is_none() && run.joined_at.is_none())
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return Ok(ReportOutcome::NothingToReport);
    }

    let sender = MessageSender::Harness {
        notice: HarnessNotice::SubagentReport,
    };
    let pane_id = parent.pane.as_ref().map(|pane| &pane.pane_id);
    let mut message = MessageRecord::new(
        workspace.workspace_id.clone(),
        parent,
        compose_digest(&rows),
        true,
        DeliveryGate::Done,
    )
    .with_channel(rimz::harness::target::agent_channel(parent))
    .with_sender(sender);
    if let Some(pane_id) = pane_id {
        message = message.with_pane_id(pane_id.clone());
    }
    let message_id = message.message_id.clone();
    let run_ids = rows
        .iter()
        .map(|(_, run)| run.run_id.clone())
        .collect::<Vec<_>>();
    let stamped = run::report::record_report_messages(store.paths(), &run_ids, Some(&message_id))?;
    if stamped
        .iter()
        .any(|run| run.report_message_id.as_ref() != Some(&message_id))
    {
        return Ok(ReportOutcome::NothingToReport);
    }
    if let Err(err) = store.queue_message(&message, &workspace.session_name) {
        let _ = run::report::record_report_messages(store.paths(), &run_ids, None);
        return Err(err.into());
    }
    if digest_fully_joined(store, &message_id)? {
        store.cancel_message(&message_id, &workspace.session_name, "joined inline")?;
        return Ok(ReportOutcome::Queued {
            message_id,
            delivered: false,
            parent: parent
                .name
                .clone()
                .unwrap_or_else(|| parent.agent_id.to_string()),
        });
    }
    let delivered = match pane_id {
        Some(pane_id) => deliver_one(
            workspace,
            store,
            &message_id,
            std::time::Duration::ZERO,
            Some(pane_id.mux()),
            DeliveryPolicy::Boundary,
        )?,
        None => false,
    };
    Ok(ReportOutcome::Queued {
        message_id,
        delivered,
        parent: parent
            .name
            .clone()
            .unwrap_or_else(|| parent.agent_id.to_string()),
    })
}

pub(super) fn report_settled_child(
    workspace: &ResolvedWorkspace,
    store: &Store,
    run: &RunRecord,
) -> Result<ReportOutcome, ReportErr> {
    if !run.subagent || !run.status.is_terminal() {
        return Ok(ReportOutcome::NotRequested);
    }
    let projection = store.runtime_projection(RuntimeScope::Audit)?;
    let Some(child) = projection.agents.iter().find(|agent| {
        run.agent_id.as_ref().map_or_else(
            || agent.name.as_deref() == run.agent_name.as_deref(),
            |agent_id| &agent.agent_id == agent_id,
        )
    }) else {
        return Ok(ReportOutcome::ChildMissing);
    };
    let Some(parent_id) = child.parent_agent_id.as_ref() else {
        return Ok(ReportOutcome::NoParent);
    };
    report_fleet(workspace, store, parent_id)
}

fn digest_fully_joined(store: &Store, message_id: &MessageId) -> Result<bool, run::RunStoreErr> {
    let rows = run::list(store.paths())?
        .into_iter()
        .filter(|run| run.report_message_id.as_ref() == Some(message_id))
        .collect::<Vec<_>>();
    Ok(!rows.is_empty() && rows.iter().all(|run| run.joined_at.is_some()))
}

fn compose_digest(rows: &[(&AgentState, &RunRecord)]) -> String {
    let names = rows
        .iter()
        .map(|(child, run)| format!("@{}", child_name(child, run)))
        .collect::<Vec<_>>()
        .join(" ");
    let heading = if rows.len() == 1 {
        format!("Your subagent settled, read with `rimz subagents wait {names}`.")
    } else {
        format!(
            "All {} settled, read with `rimz subagents wait {names}`.",
            rows.len()
        )
    };
    let rows = rows
        .iter()
        .map(|(child, run)| compose_digest_row(child, run))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{heading}\n\n{rows}")
}

fn compose_digest_row(child: &AgentState, run: &RunRecord) -> String {
    let finished_at = run.completed_at.unwrap_or(run.updated_at);
    let elapsed =
        format_compact_duration(finished_at.duration_since(run.started_at).as_secs().max(0) as u64);
    let preposition = if run.status == RunStatus::TimedOut {
        "after"
    } else {
        "in"
    };
    let mut row = format!(
        "@{} — {} {preposition} {elapsed}, {}",
        child_name(child, run),
        status_label(run.status),
        result_size(run),
    );
    if run.status != RunStatus::Completed
        && let Some(reason) = failure_reason(run)
    {
        row.push_str("; ");
        row.push_str(reason);
    }
    if let Some(description) = child
        .description
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        row.push_str(" — ");
        row.push_str(description);
    }
    row
}

fn newest_run_for_agent<'a>(runs: &'a [RunRecord], agent: &AgentState) -> Option<&'a RunRecord> {
    runs.iter()
        .filter(|run| {
            run.agent_id.as_ref() == Some(&agent.agent_id)
                || run.agent_name.as_deref() == agent.name.as_deref()
        })
        .max_by_key(|run| run.started_at)
}

fn format_compact_duration(mut seconds: u64) -> String {
    let mut rendered = String::new();
    for (unit_seconds, suffix) in [(86_400, "d"), (3_600, "h"), (60, "m")] {
        let amount = seconds / unit_seconds;
        if amount > 0 {
            rendered.push_str(&format!("{amount}{suffix}"));
            seconds %= unit_seconds;
        }
    }
    if seconds > 0 || rendered.is_empty() {
        rendered.push_str(&format!("{seconds}s"));
    }
    rendered
}

fn child_name<'a>(child: &'a AgentState, run: &'a RunRecord) -> &'a str {
    child
        .name
        .as_deref()
        .or(run.agent_name.as_deref())
        .unwrap_or_else(|| child.agent_id.as_str())
}

fn result_size(run: &RunRecord) -> String {
    match run
        .last_message
        .as_deref()
        .into_iter()
        .flat_map(str::lines)
        .filter(|line| !line.trim().is_empty())
        .count()
    {
        0 => "no result".to_owned(),
        1 => "1 line".to_owned(),
        lines => format!("{lines} lines"),
    }
}

fn failure_reason(run: &RunRecord) -> Option<&str> {
    run.failure_tail
        .as_deref()?
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
}

fn status_label(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Pending => "pending",
        RunStatus::Running => "running",
        RunStatus::Completed => "completed",
        RunStatus::Failed => "failed",
        RunStatus::VerifyFailed => "verify failed",
        RunStatus::TimedOut => "timed out",
        RunStatus::BudgetExceeded => "budget exceeded",
        RunStatus::Canceled => "canceled",
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use jiff::Timestamp;

    use rimz::agents::{AgentLifecycleObservation, AgentStatus, LifecycleSignal, PermissionMode};
    use rimz::ids::{AgentKind, WorkspaceId};
    use rimz::store::writer::AgentLifecycleIntent;
    use rimz::store::{RuntimePaths, StatePaths};
    use rimz::workspace::RootClass;

    use super::*;

    fn child(name: &str, description: Option<&str>) -> AgentState {
        let mut child = AgentState::stub("codex", name, AgentStatus::Success);
        child.name = Some(name.to_owned());
        child.description = description.map(str::to_owned);
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
    fn digest_names_its_single_result_command() {
        let mut result = run(RunStatus::Completed);
        result.last_message = Some("Done.\n\nTwo paragraphs.\n".to_owned());
        let child = child("naming", Some("map spec/profile surfaces"));

        assert_eq!(
            compose_digest(&[(&child, &result)]),
            "Your subagent settled, read with `rimz subagents wait @naming`.\n\n\
             @naming — completed in 4m12s, 2 lines — map spec/profile surfaces"
        );
    }

    #[test]
    fn digest_sizes_non_completed_results_and_appends_reason() {
        let mut completed = run(RunStatus::Completed);
        completed.last_message = Some("Done.\nSecond line.\n".to_owned());
        let blank = run(RunStatus::Completed);
        let mut timed_out = run(RunStatus::TimedOut);
        timed_out.last_message = Some("partial answer\n".to_owned());
        timed_out.failure_tail = Some("first detail\n\nprovider did not stop\n".to_owned());
        let naming = child("naming", Some("map spec/profile surfaces"));
        let runtime = child("runtime", None);
        let reviewer = child("slow-reviewer", Some("review correctness"));

        assert_eq!(
            compose_digest(&[
                (&naming, &completed),
                (&runtime, &blank),
                (&reviewer, &timed_out),
            ]),
            "All 3 settled, read with `rimz subagents wait @naming @runtime @slow-reviewer`.\n\n\
             @naming — completed in 4m12s, 2 lines — map spec/profile surfaces\n\
             @runtime — completed in 4m12s, no result\n\
             @slow-reviewer — timed out after 4m12s, 1 line; provider did not stop — review correctness"
        );
    }

    fn fixture() -> (tempfile::TempDir, ResolvedWorkspace, Store) {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let state = StatePaths::under(workspace_id.clone(), &dir.path().join("state")).unwrap();
        let runtime =
            RuntimePaths::under(workspace_id.clone(), &dir.path().join("runtime")).unwrap();
        let store = Store::open(state, runtime).unwrap();
        let workspace = ResolvedWorkspace {
            workspace_id,
            project_root: dir.path().to_path_buf(),
            cwd_project_root: None,
            root_class: RootClass::Directory,
            worktree_root: dir.path().to_path_buf(),
            worktree_branch: None,
            session_name: "report-test".to_owned(),
            mux_hint: None,
        };
        (dir, workspace, store)
    }

    fn append_agent(store: &Store, name: &str, parent: Option<&str>) {
        let mut observation = AgentLifecycleObservation::new(
            Some(AgentSessionId::from(name)),
            LifecycleSignal::Registered,
        );
        observation.agent_name = Some(name.to_owned());
        if let Some(parent) = parent {
            observation.launch.parent_agent_id = Some(AgentSessionId::from(parent));
            observation.launch.parent_agent_kind = Some(AgentKind::new_unchecked("codex"));
            observation.launch.launch_depth = Some(1);
        }
        store
            .append_agent_lifecycle(AgentLifecycleIntent {
                session_name: "report-test",
                agent_kind: AgentKind::new_unchecked("codex"),
                event_name: "test",
                observation: &observation,
                spawned_subagents: &[],
            })
            .unwrap();
    }

    fn child_run(workspace_id: &WorkspaceId, name: &str, status: RunStatus) -> RunRecord {
        let mut record = run(status);
        record.workspace_id = workspace_id.clone();
        record.agent_id = Some(AgentSessionId::from(name));
        record.agent_name = Some(name.to_owned());
        record.subagent = true;
        record
    }

    #[test]
    fn report_fleet_stamps_all_rows_before_queueing_once() {
        let (_dir, workspace, store) = fixture();
        append_agent(&store, "parent", None);
        for name in ["first", "second"] {
            append_agent(&store, name, Some("parent"));
        }
        let first = child_run(&workspace.workspace_id, "first", RunStatus::Completed);
        let second = child_run(&workspace.workspace_id, "second", RunStatus::Canceled);
        for record in [&first, &second] {
            run::create(store.paths(), record).unwrap();
        }

        let ReportOutcome::Queued { message_id, .. } =
            report_fleet(&workspace, &store, &AgentSessionId::from("parent")).unwrap()
        else {
            panic!("digest should queue");
        };
        for record in [&first, &second] {
            assert_eq!(
                run::load(store.paths(), &record.run_id)
                    .unwrap()
                    .report_message_id,
                Some(message_id.clone())
            );
        }
        assert_eq!(
            report_fleet(&workspace, &store, &AgentSessionId::from("parent")).unwrap(),
            ReportOutcome::NothingToReport
        );
        assert_eq!(store.list_messages().unwrap().len(), 1);
    }
}
