use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use rimz::ids::{AgentKind, AgentSessionId, MuxName};
use rimz::sidebar_pane::{CardReply, CardRequest, CardTarget};
use serde::{Deserialize, Serialize};

use crate::cli::{GlobalFlags, agents_cmd, ctx::Ctx, render};

const CLOSED_CARD_LIMIT: usize = 32;

#[derive(Deserialize, Serialize)]
struct ClosedCard {
    kind: AgentKind,
    agent_id: AgentSessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    team: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    worktree: Option<String>,
    closed_at: u64,
}

fn resolve_session(globals: &GlobalFlags, session: Option<String>) -> Result<(String, MuxName)> {
    let session = match session {
        Some(session) => session,
        None => {
            rimz::WorkspaceResolver::resolve_participant(".", globals.root.clone())?.session_name
        }
    };
    Ok((session, rimz::mux::auto_detect_backend(globals.mux)?))
}

fn open_room(globals: &GlobalFlags, session: &str, mux: MuxName) -> Result<(Ctx, GlobalFlags)> {
    let record = rimz::room::session::workspace_record_for_session(session)?
        .with_context(|| format!("no room for session {session}"))?;
    let globals = GlobalFlags {
        root: Some(record.project_root),
        mux: Some(mux),
        ..globals.clone()
    };
    Ok((Ctx::open(&globals)?, globals))
}

pub(super) fn request(
    globals: &GlobalFlags,
    session: Option<String>,
    request: CardRequest,
) -> Result<()> {
    let (session, mux) = resolve_session(globals, session)?;
    match rimz::sidebar_pane::request_card(mux, &session, request)? {
        CardReply::Done => Ok(()),
        CardReply::Refused { reason } => Ok(writeln!(render::err(), "{reason}")?),
        CardReply::CloseTarget { target } => {
            let (ctx, globals) = open_room(globals, &session, mux)?;
            close(&ctx, &globals, &session, target)
        }
    }
}

fn close(ctx: &Ctx, globals: &GlobalFlags, session: &str, target: CardTarget) -> Result<()> {
    let (kind, agent_id, pane) = match target {
        CardTarget::Agent {
            kind,
            agent_id,
            pane,
        } => (kind, agent_id, pane),
        CardTarget::Pane(pane) => {
            rimz::mux::backend_for(pane.mux()).close_pane(session, &pane)?;
            return Ok(());
        }
    };
    let snapshot = ctx.cached_snapshot()?;
    let Some(agent) = snapshot
        .agents
        .iter()
        .find(|agent| agent.kind == kind && agent.agent_id == agent_id)
        .filter(|agent| agent.pane.is_some())
    else {
        rimz::mux::backend_for(pane.mux()).close_pane(session, &pane)?;
        return Ok(());
    };
    let stop = agents_cmd::stop_resolved(ctx, globals, &snapshot, &[agent])?.remove(0);
    stop.into_result(agent.agent_id.as_str())?;
    if !agents_cmd::offers_resume(&ctx.workspace, agent)
        || !rimz::harness::resume::resume_session_present(agent)
    {
        return Ok(());
    }
    let mut entries = read_closed(ctx);
    entries.push(ClosedCard {
        kind,
        agent_id,
        team: agent.team.clone(),
        role: agent.role.clone(),
        worktree: agents_cmd::resume_worktree_name(&ctx.workspace, agent).map(str::to_owned),
        closed_at: rimz::utils::time::unix_now_ms(),
    });
    write_closed(ctx, &entries)
}

fn read_closed(ctx: &Ctx) -> Vec<ClosedCard> {
    std::fs::read(ctx.runtime().sidebar_closed_cards_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_closed(ctx: &Ctx, entries: &[ClosedCard]) -> Result<()> {
    let path = ctx.runtime().sidebar_closed_cards_path();
    let entries = &entries[entries.len().saturating_sub(CLOSED_CARD_LIMIT)..];
    rimz::disk::atomic::write_temp_then_rename_cache(&path, &entries)
        .with_context(|| format!("writing closed cards at {}", path.display()))
}

pub(super) fn reopen(globals: &GlobalFlags, session: Option<String>) -> Result<()> {
    let (session, mux) = resolve_session(globals, session)?;
    let (ctx, _) = open_room(globals, &session, mux)?;
    let mut entries = read_closed(&ctx);
    let alive = ctx.alive_snapshot()?;
    let newest = entries.iter().rposition(|entry| {
        !alive
            .agents
            .iter()
            .any(|agent| agent.kind == entry.kind && agent.agent_id == entry.agent_id)
    });
    let Some(index) = newest else {
        writeln!(render::err(), "sidebar reopen: no closed card to reopen")?;
        return Ok(());
    };
    let entry = &entries[index];
    let args = agents_cmd::resume_command(
        &format!("@{}", entry.agent_id),
        entry.team.as_deref().zip(entry.role.as_deref()),
        entry.worktree.as_deref(),
    );
    let output = Command::new(rimz::proc::rimz_exe())
        .arg("--root")
        .arg(&ctx.workspace.project_root)
        .args(["--mux", mux.as_str()])
        .args(args)
        .env_remove("TMUX_PANE")
        .env_remove("ZELLIJ_PANE_ID")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("sidebar reopen: starting the resume command");
    if output.as_ref().is_ok_and(|output| output.status.success()) {
        entries.truncate(index);
    } else {
        entries.remove(index);
    }
    write_closed(&ctx, &entries)?;
    let output = output?;
    if !output.status.success() {
        bail!(
            "sidebar reopen: resume failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}
