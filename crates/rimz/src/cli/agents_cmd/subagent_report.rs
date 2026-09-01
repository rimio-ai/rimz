//! Parent-facing completion digests emitted by the last settling child wrapper.
//!
//! Durable run records remain truth and `rimz subagents wait` reads their
//! results. The parked digest carries status only. Sibling state is read before
//! the rows are stamped, so two children settling together may each queue a
//! truthful digest rather than lose the completion notice.

use rimz::agents::AgentState;
use rimz::harness::run::{self, RunRecord, RunStatus};
use rimz::ids::MessageId;
use rimz::message::{DeliveryGate, HarnessNotice, MessageSender};
use rimz::workspace::ResolvedWorkspace;
use rimz::{RuntimeScope, Store};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ReportOutcome {
    Queued {
        message_id: MessageId,
        delivered: bool,
        parent: String,
    },
    ChildMissing,
    NoParent,
    ParentEnded,
    SiblingsRunning,
    NothingToReport,
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

type Result<T> = std::result::Result<T, ReportErr>;

pub(super) fn report_settled_child(
    workspace: &ResolvedWorkspace,
    store: &Store,
    run: &RunRecord,
) -> Result<ReportOutcome> {
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
    let children = rimz::harness::target::launched_children(&projection.agents, parent)
        .into_iter()
        .filter_map(|child| {
            let run = newest_run_for_agent(&runs, child)?;
            Some((child, run))
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

    let text = compose_digest(&rows);
    let sender = MessageSender::Harness {
        notice: HarnessNotice::SubagentReport,
    };
    let pane_id = parent.pane.as_ref().map(|pane| &pane.pane_id);
    let message_id = rimz::message::deliver::queue_synthetic(
        workspace,
        store,
        parent,
        sender,
        text,
        DeliveryGate::Done,
        pane_id,
    )?;
    for (_, run) in rows {
        run::report::record_report_message(store.paths(), &run.run_id, message_id.clone())?;
    }
    let delivered = match pane_id {
        Some(pane_id) => rimz::message::deliver::deliver_one(
            workspace,
            store,
            &message_id,
            std::time::Duration::ZERO,
            Some(pane_id.mux()),
            rimz::message::deliver::DeliveryPolicy::Boundary,
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

fn compose_digest(rows: &[(&AgentState, &RunRecord)]) -> String {
    let heading = if rows.len() == 1 {
        "Your subagent settled, read with `rimz subagents wait`.".to_owned()
    } else {
        format!(
            "All {} settled, read with `rimz subagents wait`.",
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
    let elapsed_seconds = finished_at.duration_since(run.started_at).as_secs().max(0) as u64;
    let elapsed = format_compact_duration(elapsed_seconds);
    let preposition = if run.status == RunStatus::TimedOut {
        "after"
    } else {
        "in"
    };
    let mut row = format!(
        "@{} — {} {preposition} {elapsed}, {}",
        child_name(child, run),
        crate::cli::supervised::output::status_label(run.status),
        result_size(run),
    );
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
    if run.status == RunStatus::Completed {
        let lines = run
            .last_message
            .as_deref()
            .into_iter()
            .flat_map(str::lines)
            .filter(|line| !line.trim().is_empty())
            .count();
        return match lines {
            0 => "no final message".to_owned(),
            1 => "1 line".to_owned(),
            lines => format!("{lines} lines"),
        };
    }

    let reason = run
        .failure_tail
        .as_deref()
        .and_then(|tail| tail.lines().rev().find(|line| !line.trim().is_empty()))
        .map(str::trim);
    match reason {
        Some(reason) => format!("no result: {reason}"),
        None => "no result".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;
    use rimz::agents::{AgentLifecycleObservation, AgentStatus, LifecycleSignal, PermissionMode};
    use rimz::ids::{AgentKind, AgentSessionId, WorkspaceId};
    use rimz::store::writer::AgentLifecycleIntent;
    use rimz::store::{RuntimePaths, StatePaths};
    use rimz::workspace::RootClass;
    use std::path::{Path, PathBuf};

    fn child(name: &str, description: Option<&str>) -> AgentState {
        let mut child = AgentState::stub("codex", "sess-child", AgentStatus::Success);
        child.name = Some(name.to_owned());
        child.profile = Some("explorer".to_owned());
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

    fn report_fixture() -> (tempfile::TempDir, ResolvedWorkspace, Store) {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let state = StatePaths::under(workspace_id.clone(), &dir.path().join("state"))
            .expect("state paths");
        let runtime = RuntimePaths::under(workspace_id.clone(), &dir.path().join("runtime"))
            .expect("runtime paths");
        let store = Store::open(state, runtime).expect("store");
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

    fn append_agent(store: &Store, name: &str, parent: Option<&str>, signal: LifecycleSignal) {
        let mut observation =
            AgentLifecycleObservation::new(Some(AgentSessionId::from(name)), signal);
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
            .expect("append agent");
    }

    fn child_run(workspace_id: &WorkspaceId, name: &str, status: RunStatus) -> RunRecord {
        let mut run = RunRecord::new(
            workspace_id.clone(),
            AgentKind::new_unchecked("codex"),
            PermissionMode::Auto,
            format!("task for {name}"),
            PathBuf::from("/tmp/subagent-report"),
        );
        run.agent_id = Some(AgentSessionId::from(name));
        run.agent_name = Some(name.to_owned());
        run.subagent = true;
        run.status = status;
        run.started_at = Timestamp::from_second(1_000).unwrap();
        run.updated_at = Timestamp::from_second(1_252).unwrap();
        run.completed_at = status
            .is_terminal()
            .then(|| Timestamp::from_second(1_252).unwrap());
        run
    }

    #[test]
    fn single_digest_reports_result_size_without_result_text() {
        let mut run = run(RunStatus::Completed);
        run.last_message = Some("Done.\n\nTwo paragraphs.\n".to_owned());
        let child = child("naming", Some("map spec/profile surfaces"));

        assert_eq!(
            compose_digest(&[(&child, &run)]),
            "Your subagent settled, read with `rimz subagents wait`.\n\n\
             @naming — completed in 4m12s, 2 lines — map spec/profile surfaces"
        );
    }

    #[test]
    fn fleet_digest_formats_mixed_outcomes() {
        let mut completed = run(RunStatus::Completed);
        completed.last_message = Some("Done.\nSecond line.\n".to_owned());
        let blank = run(RunStatus::Completed);
        let mut timed_out = run(RunStatus::TimedOut);
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
            "All 3 settled, read with `rimz subagents wait`.\n\n\
             @naming — completed in 4m12s, 2 lines — map spec/profile surfaces\n\
             @runtime — completed in 4m12s, no final message\n\
             @slow-reviewer — timed out after 4m12s, no result: provider did not stop — review correctness"
        );
    }

    #[test]
    fn missing_child_and_parent_are_distinct_skip_outcomes() {
        let (_dir, workspace, store) = report_fixture();
        let missing_child = child_run(&workspace.workspace_id, "missing", RunStatus::Completed);
        assert_eq!(
            report_settled_child(&workspace, &store, &missing_child).unwrap(),
            ReportOutcome::ChildMissing
        );

        append_agent(
            &store,
            "child",
            Some("missing-parent"),
            LifecycleSignal::Registered,
        );
        let missing_parent = child_run(&workspace.workspace_id, "child", RunStatus::Completed);
        assert_eq!(
            report_settled_child(&workspace, &store, &missing_parent).unwrap(),
            ReportOutcome::NoParent
        );
    }

    #[test]
    fn ended_parent_suppresses_the_report() {
        let (_dir, workspace, store) = report_fixture();
        append_agent(&store, "parent", None, LifecycleSignal::Registered);
        append_agent(&store, "child", Some("parent"), LifecycleSignal::Registered);
        append_agent(&store, "parent", None, LifecycleSignal::Ended);

        let run = child_run(&workspace.workspace_id, "child", RunStatus::Completed);
        assert_eq!(
            report_settled_child(&workspace, &store, &run).unwrap(),
            ReportOutcome::ParentEnded
        );
    }

    #[test]
    fn siblings_running_queues_nothing() {
        let (_dir, workspace, store) = report_fixture();
        append_agent(&store, "parent", None, LifecycleSignal::Registered);
        for name in ["child", "running"] {
            append_agent(&store, name, Some("parent"), LifecycleSignal::Registered);
        }
        let child = child_run(&workspace.workspace_id, "child", RunStatus::Completed);
        let running = child_run(&workspace.workspace_id, "running", RunStatus::Running);
        for run in [&child, &running] {
            run::create(store.paths(), run).expect("create run");
        }

        assert_eq!(
            report_settled_child(&workspace, &store, &child).unwrap(),
            ReportOutcome::SiblingsRunning
        );
        assert!(store.list_messages().expect("messages").is_empty());
        assert_eq!(
            run::load(store.paths(), &child.run_id)
                .expect("stored child")
                .report_message_id,
            None
        );
    }

    #[test]
    fn joined_and_reported_rows_are_excluded() {
        let (_dir, workspace, store) = report_fixture();
        append_agent(&store, "parent", None, LifecycleSignal::Registered);
        for name in ["fresh", "joined", "reported"] {
            append_agent(&store, name, Some("parent"), LifecycleSignal::Registered);
        }
        let fresh = child_run(&workspace.workspace_id, "fresh", RunStatus::Completed);
        let joined = child_run(&workspace.workspace_id, "joined", RunStatus::Completed);
        let reported = child_run(&workspace.workspace_id, "reported", RunStatus::Completed);
        for run in [&fresh, &joined, &reported] {
            run::create(store.paths(), run).expect("create run");
        }
        run::report::mark_joined(store.paths(), &joined.run_id).expect("mark joined");
        let previous = MessageId::new();
        run::report::record_report_message(store.paths(), &reported.run_id, previous.clone())
            .expect("mark reported");

        let ReportOutcome::Queued {
            message_id,
            delivered: false,
            parent,
        } = report_settled_child(&workspace, &store, &fresh).unwrap()
        else {
            panic!("digest should queue");
        };
        assert_eq!(parent, "parent");
        let messages = store.list_messages().expect("messages");
        let digest = messages
            .iter()
            .find(|message| message.message_id == message_id)
            .expect("queued digest");
        assert!(digest.text.contains("@fresh"));
        assert!(!digest.text.contains("@joined"));
        assert!(!digest.text.contains("@reported"));
        assert_eq!(
            run::load(store.paths(), &reported.run_id)
                .expect("reported run")
                .report_message_id,
            Some(previous)
        );
    }

    #[test]
    fn queued_digest_stamps_every_row() {
        let (_dir, workspace, store) = report_fixture();
        append_agent(&store, "parent", None, LifecycleSignal::Registered);
        for name in ["first", "second"] {
            append_agent(&store, name, Some("parent"), LifecycleSignal::Registered);
        }
        let first = child_run(&workspace.workspace_id, "first", RunStatus::Completed);
        let second = child_run(&workspace.workspace_id, "second", RunStatus::Canceled);
        for run in [&first, &second] {
            run::create(store.paths(), run).expect("create run");
        }

        let ReportOutcome::Queued {
            message_id,
            delivered: false,
            parent,
        } = report_settled_child(&workspace, &store, &first).unwrap()
        else {
            panic!("digest should queue");
        };
        assert_eq!(parent, "parent");
        assert_eq!(
            run::load(store.paths(), &first.run_id)
                .expect("first run")
                .report_message_id,
            Some(message_id.clone())
        );
        assert_eq!(
            run::load(store.paths(), &second.run_id)
                .expect("second run")
                .report_message_id,
            Some(message_id.clone())
        );
        let messages = store.list_messages().expect("messages");
        let digest = messages
            .iter()
            .find(|message| message.message_id == message_id)
            .expect("queued digest");
        assert!(digest.text.contains("All 2 settled"));
        assert!(digest.text.contains("@first — completed"));
        assert!(digest.text.contains("@second — canceled"));
        assert!(matches!(
            &digest.sender,
            MessageSender::Harness {
                notice: HarnessNotice::SubagentReport
            }
        ));
    }
}
