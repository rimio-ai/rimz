//! Durable parent-facing completion digests for launched subagent fleets.
//!
//! Run records are stamped together before the digest enters the message
//! queue. This makes the complete row set visible to inline join cancellation
//! and lets the wrapper fast path race safely with the producer backstop.

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Context;
use rimz::agents::AgentState;
use rimz::disk::atomic::{AtomicErr, write_bytes_atomically};
use rimz::disk::paths::{PathErr, StatePaths};
use rimz::disk::summary::FileSummary;
use rimz::harness::run;
use rimz::ids::{AgentKind, AgentSessionId, MessageId, RunId};
use rimz::message::deliver::{DeliveryPolicy, deliver_one};
use rimz::sandbox::TmpView;
use rimz::store::message::{DeliveryGate, HarnessNotice, MessageRecord, MessageSender};
use rimz::store::run::{RunRecord, RunStatus, RunStoreErr};
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
    Run(#[from] RunStoreErr),
    #[error(transparent)]
    Deliver(#[from] rimz::message::deliver::DeliverErr),
    #[error(transparent)]
    Paths(#[from] PathErr),
    #[error(transparent)]
    Atomic(#[from] AtomicErr),
    #[error("measuring subagent response file {path}: {source}")]
    Response {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

struct ResponseFile {
    path: PathBuf,
    summary: FileSummary,
}

fn write_response_files(
    paths: &StatePaths,
    view: &TmpView,
    rows: &[(&AgentState, &RunRecord)],
) -> Result<Vec<Option<ResponseFile>>, ReportErr> {
    paths.ensure_tmp_dir()?;
    rows.iter()
        .map(|(child, run)| {
            let Some(message) = run
                .last_message
                .as_deref()
                .filter(|message| !message.is_empty())
            else {
                return Ok(None);
            };
            let path = paths
                .subagents_dir
                .join(format!("{}.output", child_name(child, run)));
            let mut bytes = message.as_bytes().to_vec();
            if !bytes.ends_with(b"\n") {
                bytes.push(b'\n');
            }
            write_bytes_atomically(&path, &bytes)?;
            let summary = FileSummary::measure(&path).map_err(|source| ReportErr::Response {
                path: path.clone(),
                source,
            })?;
            Ok(Some(ResponseFile {
                path: view.agent_path(&path),
                summary,
            }))
        })
        .collect()
}

pub(super) fn report_fleet(
    workspace: &ResolvedWorkspace,
    store: &Store,
    parent_id: &AgentSessionId,
) -> Result<ReportOutcome, ReportErr> {
    report_fleet_with_kind(workspace, store, parent_id, None)
}

fn report_parent<'a>(
    agents: &'a [AgentState],
    parent_id: &AgentSessionId,
    parent_kind: Option<&AgentKind>,
) -> Option<&'a AgentState> {
    let kind = match parent_kind {
        Some(kind) => kind,
        None => {
            &agents
                .iter()
                .find(|agent| {
                    &agent.agent_id == parent_id || agent.launch_id.as_ref() == Some(parent_id)
                })?
                .kind
        }
    };
    rimz::address::launch_row(agents, kind, parent_id)
}

fn report_fleet_with_kind(
    workspace: &ResolvedWorkspace,
    store: &Store,
    parent_id: &AgentSessionId,
    parent_kind: Option<&AgentKind>,
) -> Result<ReportOutcome, ReportErr> {
    let projection = store.runtime_projection(RuntimeScope::Audit)?;
    let Some(parent) = report_parent(&projection.agents, parent_id, parent_kind) else {
        return Ok(ReportOutcome::NoParent);
    };
    if parent.ended_at.is_some() {
        return Ok(ReportOutcome::ParentEnded);
    }

    let runs = run::list(store.paths())?;
    let mut seen = HashSet::<RunId>::new();
    let children = rimz::address::launched_children(&projection.agents, parent)
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

    let view = TmpView::current(store.paths());
    let responses = write_response_files(store.paths(), &view, &rows)?;
    let digest_rows = rows
        .iter()
        .zip(&responses)
        .map(|((child, run), response)| (*child, *run, response.as_ref()))
        .collect::<Vec<_>>();

    let sender = MessageSender::Harness {
        notice: HarnessNotice::SubagentReport,
    };
    let pane_id = parent.pane.as_ref().map(|pane| &pane.pane_id);
    let mut message = MessageRecord::new(
        workspace.workspace_id.clone(),
        parent,
        compose_digest(&digest_rows),
        true,
        DeliveryGate::Done,
    )
    .with_channel(parent.channel())
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
    if run::report::digest_fully_joined(store.paths(), &message_id)? {
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
        agent.kind == run.kind
            && run.agent_id.as_ref().map_or_else(
                || agent.name.as_deref() == run.agent_name.as_deref(),
                |agent_id| &agent.agent_id == agent_id,
            )
    }) else {
        return Ok(ReportOutcome::ChildMissing);
    };
    let Some(parent_id) = child.parent_agent_id.as_ref() else {
        return Ok(ReportOutcome::NoParent);
    };
    report_fleet_with_kind(
        workspace,
        store,
        parent_id,
        Some(child.parent_agent_kind.as_ref().unwrap_or(&child.kind)),
    )
}

pub(super) fn backstop_digest(request: super::SubagentDigestRequest) -> anyhow::Result<()> {
    let ctx = super::Ctx::for_workspace(request.workspace_id, None)
        .context("resolving subagent digest workspace")?;
    let outcome = report_fleet(&ctx.workspace, &ctx.store, &request.parent_agent_id)
        .context("reconstructing subagent fleet digest")?;
    let ReportOutcome::Queued { message_id, .. } = outcome else {
        return Ok(());
    };
    rimz::diag::DiagSink::for_workspace(
        ctx.workspace.workspace_id.clone(),
        ctx.workspace.session_name.clone(),
        None,
    )
    .emit(rimz::diag::record::DiagEvent::SubagentDigestBackstopped {
        parent_agent_id: request.parent_agent_id,
        message_id,
    });
    Ok(())
}

fn compose_digest(rows: &[(&AgentState, &RunRecord, Option<&ResponseFile>)]) -> String {
    let heading = if rows.len() == 1 {
        "Your subagent settled:".to_owned()
    } else {
        format!("All {} subagents settled:", rows.len())
    };
    let rows = rows
        .iter()
        .map(|(child, run, response)| compose_digest_row(child, run, *response))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{heading}\n{rows}")
}

fn compose_digest_row(
    child: &AgentState,
    run: &RunRecord,
    response: Option<&ResponseFile>,
) -> String {
    let finished_at = run.completed_at.unwrap_or(run.updated_at);
    let elapsed =
        format_compact_duration(finished_at.duration_since(run.started_at).as_secs().max(0) as u64);
    let preposition = if run.status == RunStatus::TimedOut {
        "after"
    } else {
        "in"
    };
    let mut row = format!(
        "- @{}: {} {preposition} {elapsed}",
        child_name(child, run),
        status_label(run.status),
    );
    if run.status != RunStatus::Completed
        && let Some(reason) = failure_reason(run)
    {
        row.push_str("; ");
        row.push_str(reason);
    }
    if let Some(task) = child
        .description
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(std::borrow::Cow::Borrowed)
        .or_else(|| {
            run.prompt
                .lines()
                .next()
                .filter(|line| !line.is_empty())
                .map(rimz::theme::fmt::command_preview)
        })
    {
        row.push_str(&format!(", task: \"{task}\""));
    }
    match response {
        Some(response) => row.push_str(&format!(
            ", response: {} ({})",
            response.path.display(),
            response.summary.lines_label(),
        )),
        None => row.push_str(", no response"),
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
    use rimz::disk::paths::{RuntimePaths, StatePaths};
    use rimz::ids::{AgentKind, WorkspaceId};
    use rimz::store::message::MessageStatus;
    use rimz::store::writer::AgentLifecycleIntent;
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
    fn digest_parent_routes_to_live_successor_with_corroborated_kind() {
        let mut old = child("OLD", None);
        old.launch_id = Some(AgentSessionId::from("L"));
        old.ended_at = Some(Timestamp::from_second(1_000).unwrap());
        let mut new = child("NEW", None);
        new.launch_id = old.launch_id.clone();
        let wrong = AgentState::stub("claude", "L", AgentStatus::Idle);
        let agents = [wrong, old, new.clone()];

        for parent_id in ["L", "OLD"] {
            let parent =
                report_parent(&agents, &AgentSessionId::from(parent_id), Some(&new.kind)).unwrap();
            assert_eq!(parent.agent_id, new.agent_id);
            assert!(parent.ended_at.is_none());
        }
        assert_eq!(
            report_parent(&agents[1..], &AgentSessionId::from("OLD"), None)
                .unwrap()
                .agent_id,
            new.agent_id
        );
    }

    #[test]
    fn digest_lists_a_single_result_without_a_trailing_command() {
        let mut result = run(RunStatus::Completed);
        result.last_message = Some("Done.\n\nTwo paragraphs.\n".to_owned());
        let child = child("naming", Some("map spec/profile surfaces"));
        let response = ResponseFile {
            path: PathBuf::from("/tmp/rimz-subagents/naming.output"),
            summary: FileSummary {
                bytes: 23,
                lines: 3,
            },
        };

        assert_eq!(
            compose_digest(&[(&child, &result, Some(&response))]),
            "Your subagent settled:\n\
             - @naming: completed in 4m12s, task: \"map spec/profile surfaces\", response: /tmp/rimz-subagents/naming.output (3 lines)"
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
        let response = ResponseFile {
            path: PathBuf::from("/tmp/rimz-subagents/naming.output"),
            summary: FileSummary {
                bytes: 19,
                lines: 2,
            },
        };
        let partial = ResponseFile {
            path: PathBuf::from("/tmp/rimz-subagents/slow-reviewer.output"),
            summary: FileSummary {
                bytes: 15,
                lines: 1,
            },
        };

        assert_eq!(
            compose_digest(&[
                (&naming, &completed, Some(&response)),
                (&runtime, &blank, None),
                (&reviewer, &timed_out, Some(&partial)),
            ]),
            "All 3 subagents settled:\n\
             - @naming: completed in 4m12s, task: \"map spec/profile surfaces\", response: /tmp/rimz-subagents/naming.output (2 lines)\n\
             - @runtime: completed in 4m12s, task: \"map it\", no response\n\
             - @slow-reviewer: timed out after 4m12s; provider did not stop, task: \"review correctness\", response: /tmp/rimz-subagents/slow-reviewer.output (1 line)"
        );
    }

    #[test]
    fn digest_task_falls_back_to_prompt_preview_or_is_omitted() {
        let child = child("naming", None);
        let mut result = run(RunStatus::Completed);
        result.prompt = format!("{}\nnot part of the task", "x".repeat(150));
        let row = compose_digest_row(&child, &result, None);
        assert!(row.contains(&format!(
            ", task: \"{}\", no response",
            rimz::theme::fmt::command_preview(&"x".repeat(150))
        )));
        assert!(!row.contains("not part of the task"));
        result.prompt.clear();
        assert_eq!(
            compose_digest_row(&child, &result, None),
            "- @naming: completed in 4m12s, no response"
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

    #[test]
    fn dismissed_child_is_not_reported() {
        let (_dir, workspace, store) = fixture();
        append_agent(&store, "parent", None);
        for name in ["first", "second"] {
            append_agent(&store, name, Some("parent"));
        }
        let first = child_run(&workspace.workspace_id, "first", RunStatus::Completed);
        let second = child_run(&workspace.workspace_id, "second", RunStatus::Running);
        for record in [&first, &second] {
            run::create(store.paths(), record).unwrap();
        }
        run::report::join_and_settle_digest(
            &store,
            &workspace.session_name,
            &second.run_id,
            "stopped by parent",
        )
        .unwrap();
        run::cancel(store.paths(), &second.run_id).unwrap();

        assert!(matches!(
            report_fleet(&workspace, &store, &AgentSessionId::from("parent")).unwrap(),
            ReportOutcome::Queued { .. }
        ));
        let messages = store.list_messages().unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].text.contains("Your subagent settled"));
        assert!(messages[0].text.contains("@first"));
        assert!(!messages[0].text.contains("@second"));
    }

    #[test]
    fn dismissing_every_child_reports_nothing() {
        let (_dir, workspace, store) = fixture();
        append_agent(&store, "parent", None);
        for name in ["first", "second"] {
            append_agent(&store, name, Some("parent"));
            let record = child_run(&workspace.workspace_id, name, RunStatus::Running);
            run::create(store.paths(), &record).unwrap();
            run::report::join_and_settle_digest(
                &store,
                &workspace.session_name,
                &record.run_id,
                "stopped by parent",
            )
            .unwrap();
            run::cancel(store.paths(), &record.run_id).unwrap();
        }

        assert_eq!(
            report_fleet(&workspace, &store, &AgentSessionId::from("parent")).unwrap(),
            ReportOutcome::NothingToReport
        );
        assert!(store.list_messages().unwrap().is_empty());
    }

    #[test]
    fn dismissing_every_row_of_a_queued_digest_cancels_it() {
        let (_dir, workspace, store) = fixture();
        append_agent(&store, "parent", None);
        let records = ["first", "second"].map(|name| {
            append_agent(&store, name, Some("parent"));
            let record = child_run(&workspace.workspace_id, name, RunStatus::Completed);
            run::create(store.paths(), &record).unwrap();
            record
        });
        let ReportOutcome::Queued { message_id, .. } =
            report_fleet(&workspace, &store, &AgentSessionId::from("parent")).unwrap()
        else {
            panic!("digest should queue");
        };

        for record in &records {
            run::report::join_and_settle_digest(
                &store,
                &workspace.session_name,
                &record.run_id,
                "stopped by parent",
            )
            .unwrap();
        }

        assert!(store.list_messages().unwrap().is_empty());
        let history = store.list_message_history().unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].message_id, message_id);
        assert_eq!(history[0].status, MessageStatus::Canceled);
    }
}
