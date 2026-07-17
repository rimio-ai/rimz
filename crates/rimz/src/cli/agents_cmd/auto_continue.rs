//! `rimz agents auto-continue` — the hidden helper the sidebar producer spawns to
//! resume a parked agent when its class-specific condition is due.
//!
//! The producer decides *which* agent and *when* (`sidebar::enrich`
//! auto-continue, opt-in via `[resume] auto_continue*`); this helper performs the
//! side effect the sidebar's read-only import graph must not: it queues or
//! redelivers a resume-gated message through the shared delivery pipeline.
//! Best-effort by contract — it inherits the producer's frame-validated target,
//! so a vanished pane leaves a message error instead of a false resume audit.

use anyhow::{Context, Result};
use clap::Args;
use std::time::Duration;

use rimz::ids::{AgentKind, AgentSessionId, MessageId, PaneId, WorkspaceId};
use rimz::message::{DeliveryGate, MessageRecord, MessageSender, deliver};
use rimz::store::workspace_record;
use rimz::workspace::ResolvedWorkspace;
use rimz::{RuntimePaths, StatePaths, Store};

#[derive(Debug, Args)]
pub struct AutoContinueArgs {
    #[arg(long)]
    workspace_id: String,
    #[arg(long)]
    kind: String,
    #[arg(long)]
    agent_id: String,
    /// Normalized pane id (`tmux:%3`, `zellij:terminal_3`).
    #[arg(long)]
    pane: String,
    #[arg(long)]
    text: String,
    #[arg(long)]
    reason: String,
    #[arg(long)]
    message_id: Option<String>,
}

pub fn run_auto_continue(args: AutoContinueArgs) -> Result<()> {
    let workspace_id: WorkspaceId = args.workspace_id.parse().context("parsing workspace id")?;
    let pane_id = PaneId::parse(&args.pane).context("parsing pane id")?;
    let kind = AgentKind::new_unchecked(args.kind);
    let agent_id = AgentSessionId::from(args.agent_id.as_str());
    let retry_message_id = args
        .message_id
        .as_deref()
        .map(MessageId::parse)
        .transpose()
        .context("parsing message id")?;
    let text = args.text.trim();
    if text.is_empty() {
        return Ok(());
    }

    let paths = StatePaths::for_workspace(workspace_id.clone()).context("preparing store paths")?;
    let runtime =
        RuntimePaths::for_workspace(workspace_id.clone()).context("preparing runtime paths")?;
    let record = workspace_record::read(&paths.workspace_record).with_context(|| {
        format!(
            "reading workspace record `{}`",
            paths.workspace_record.display()
        )
    })?;
    let store = Store::open(paths, runtime).context("opening store")?;
    let workspace = ResolvedWorkspace {
        workspace_id: workspace_id.clone(),
        project_root: record.project_root.clone(),
        root_class: record.root_class,
        worktree_root: record.project_root.clone(),
        worktree_branch: None,
        session_name: record.session_name,
        mux_hint: Some(pane_id.mux()),
    };
    let mut snapshot =
        rimz::sidebar::produce::resolution_snapshot(&workspace, &store, Some(pane_id.mux()))
            .context("reading auto-continue delivery snapshot")?;
    snapshot =
        snapshot.with_agent_context(rimz::store::agent_context::read_all(store.runtime_paths()));
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.kind == kind && agent.agent_id == agent_id)
        .context("auto-continue target agent is no longer in the rollup")?;
    snapshot
        .agent_panes
        .iter()
        .find(|pane| {
            pane.kind == kind
                && pane.agent_id.as_ref() == Some(&agent_id)
                && pane.pane_id == pane_id
        })
        .context("auto-continue target pane is no longer bound to the agent")?;

    let message_id = if let Some(message_id) = retry_message_id {
        message_id
    } else {
        let gate = if args.reason == "budget_day_reset" {
            DeliveryGate::Done
        } else {
            DeliveryGate::Resume
        };
        let message = MessageRecord::new(workspace_id, agent, text.to_owned(), true, gate)
            .with_channel(rimz::harness::target::agent_channel(agent))
            .with_sender(MessageSender::Human)
            .with_pane_id(pane_id.clone());
        let message_id = message.message_id.clone();
        store
            .queue_message(&message, &workspace.session_name)
            .context("queueing auto-continue resume message")?;
        message_id
    };
    let delivered = deliver::deliver_one(
        &workspace,
        &store,
        &message_id,
        Duration::ZERO,
        Some(pane_id.mux()),
        deliver::DeliveryPolicy::Boundary,
    )
    .context("delivering auto-continue resume message")?;
    if !delivered {
        let reason = format!("resume delivery gate closed ({})", args.reason);
        store
            .record_message_delivery_failures(
                std::slice::from_ref(&message_id),
                None,
                rimz::store::DeliveryFailureDisposition::Retry,
                &reason,
                &workspace.session_name,
            )
            .context("recording auto-continue delivery miss")?;
    }
    Ok(())
}
