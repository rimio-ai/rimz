//! Parent-facing completion reports emitted by the in-pane wrapper.
//!
//! The durable run record remains truth. Report delivery is best-effort latency:
//! a parked message returns the settled outcome when the parent still exists.
//! Sibling state is read at send time, so children settling together may each
//! truthfully report that all siblings have finished.

use rimz::agents::AgentState;
use rimz::harness::run::{self, RunRecord, RunStatus};
use rimz::ids::MessageId;
use rimz::message::{DeliveryGate, MessageSender};
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
    Joined,
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
    if run.joined_at.is_some() {
        return Ok(ReportOutcome::Joined);
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
    let still_running = rimz::harness::target::launched_children(&projection.agents, parent)
        .into_iter()
        .filter_map(|sibling| {
            let sibling_run = newest_run_for_agent(&runs, sibling)?;
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
    let message_id = rimz::message::deliver::queue_synthetic(
        workspace,
        store,
        parent,
        sender,
        text,
        DeliveryGate::Done,
        pane_id,
    )?;
    let recorded =
        run::report::record_report_message(store.paths(), &run.run_id, message_id.clone())?;
    if recorded.joined_at.is_some() {
        store.cancel_message(&message_id, &workspace.session_name, "joined inline")?;
        return Ok(ReportOutcome::Joined);
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

fn compose_report(child: &AgentState, run: &RunRecord, still_running: &[&str]) -> String {
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
    let elapsed = format_compact_duration(elapsed_seconds);
    let mut report = format!(
        "@{name}{metadata} {} in {elapsed}.\n{}",
        crate::cli::supervised::output::status_label(run.status),
        sibling_summary(still_running)
    );
    if let Some(detail) = report_detail(run) {
        report.push_str("\n\n");
        report.push_str(&detail);
    }
    report
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
    use jiff::Timestamp;
    use rimz::agents::{AgentLifecycleObservation, AgentStatus, LifecycleSignal, PermissionMode};
    use rimz::ids::{AgentKind, AgentSessionId, WorkspaceId};
    use rimz::store::writer::AgentLifecycleIntent;
    use rimz::store::{RuntimePaths, StatePaths};
    use rimz::workspace::RootClass;
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
    fn queued_report_counts_only_nonterminal_siblings() {
        let (_dir, workspace, store) = report_fixture();
        append_agent(&store, "parent", None, LifecycleSignal::Registered);
        for name in ["child", "running", "finished"] {
            append_agent(&store, name, Some("parent"), LifecycleSignal::Registered);
        }
        let child = child_run(&workspace.workspace_id, "child", RunStatus::Completed);
        let running = child_run(&workspace.workspace_id, "running", RunStatus::Running);
        let finished = child_run(&workspace.workspace_id, "finished", RunStatus::Completed);
        for run in [&child, &running, &finished] {
            run::create(store.paths(), run).expect("create run");
        }

        let ReportOutcome::Queued {
            message_id,
            delivered,
            parent,
        } = report_settled_child(&workspace, &store, &child).unwrap()
        else {
            panic!("report should queue");
        };
        assert!(!delivered);
        assert_eq!(parent, "parent");
        assert_eq!(
            run::load(store.paths(), &child.run_id)
                .expect("stored child")
                .report_message_id,
            Some(message_id.clone())
        );
        let messages = store.list_messages().expect("messages");
        let report = messages
            .iter()
            .find(|message| message.message_id == message_id)
            .expect("queued report");
        assert!(report.text.contains("1 subagent still running: @running."));
        assert!(!report.text.contains("@finished"));
        assert!(matches!(
            &report.sender,
            MessageSender::Subagent { name, .. } if name == "child"
        ));
    }
}
