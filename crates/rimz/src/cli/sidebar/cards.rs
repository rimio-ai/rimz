use std::collections::HashSet;
use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use rimz::ids::{AgentKind, AgentSessionId, MuxName, PaneId};
use rimz::mux::{ClientFocusOptions, ClientView, PaneListOptions, PaneReadConsistency};
use rimz::sidebar::produce::hosting::read_hosting;
use rimz::sidebar_pane::{CardReply, CardRequest, CardTarget};
use serde::{Deserialize, Serialize};

use crate::cli::agents_cmd::TabClose;
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
    request_with(globals, session, mux, request)
}

fn request_with(
    globals: &GlobalFlags,
    session: String,
    mux: MuxName,
    request: CardRequest,
) -> Result<()> {
    match rimz::sidebar_pane::request_card(mux, &session, request)? {
        CardReply::Done => Ok(()),
        CardReply::Refused { reason } => Ok(writeln!(render::err(), "{reason}")?),
        CardReply::CloseTarget { target } => {
            let (ctx, globals) = open_room(globals, &session, mux)?;
            close(&ctx, &globals, &session, vec![target], TabClose::WhenLast)
        }
    }
}

/// Close the card that owns `pane`, the pane a key was pressed in, as `--pane` does. A
/// sidebar pane or a pane in the dashboard closes nothing.
pub(super) fn close_from_pane(
    globals: &GlobalFlags,
    session: Option<String>,
    pane: PaneId,
) -> Result<()> {
    let (session, mux) = resolve_session(globals, session)?;
    let listing = rimz::mux::backend_for(mux)
        .list_panes(PaneListOptions {
            session_name: Some(session.clone()),
            consistency: PaneReadConsistency::RequireAuthoritative,
            ..Default::default()
        })
        .map_err(|err| anyhow!("sidebar close: listing panes: {err}; nothing was closed"))?;
    if listing
        .panes
        .iter()
        .any(|listed| listed.pane_id == pane && closes_nothing(listed))
    {
        return Ok(());
    }
    let request = CardRequest::CloseTarget { pane: Some(pane) };
    request_with(globals, session, mux, request)
}

/// A key pressed on the sidebar or in the dashboard closes no card.
fn closes_nothing(pane: &rimz::pane::PaneRef) -> bool {
    pane.is_rimz_sidebar() || pane.view_name.as_deref() == Some(rimz::pane::VIEW_NAME)
}

/// Close the focused tab's cards: every tiled pane but the sidebar, through one stop of
/// all their agents that ends in one close of the tab. Focus on the sidebar or in the
/// dashboard closes nothing.
pub(super) fn close_tab(globals: &GlobalFlags, session: Option<String>) -> Result<()> {
    let (session, mux) = resolve_session(globals, session)?;
    let backend = rimz::mux::backend_for(mux);
    let listing = backend
        .list_panes(PaneListOptions {
            session_name: Some(session.clone()),
            consistency: PaneReadConsistency::RequireAuthoritative,
            ..Default::default()
        })
        .context("sidebar close --tab: listing panes")?;
    let view = backend
        .client_view(ClientFocusOptions {
            session_name: Some(session.clone()),
            ..Default::default()
        })
        .context("sidebar close --tab: sampling attached client focus")?;
    let live = listing
        .panes
        .iter()
        .map(|pane| pane.pane_id.clone())
        .collect::<HashSet<_>>();
    let focused = ClientView::unique_live_focus(&view.clients, &view.viewed_panes, &live)
        .and_then(|focused| listing.panes.iter().find(|pane| pane.pane_id == focused))
        .context("sidebar close --tab requires one attached client focused on one live pane")?;
    if closes_nothing(focused) {
        return Ok(());
    }
    let mut targets = Vec::new();
    let mut panes = Vec::new();
    for pane in listing.panes.iter().filter(|pane| {
        pane.view_id == focused.view_id && !pane.is_floating && !pane.is_rimz_sidebar()
    }) {
        panes.push(pane.pane_id.clone());
        let request = CardRequest::CloseTarget {
            pane: Some(pane.pane_id.clone()),
        };
        match rimz::sidebar_pane::request_card(mux, &session, request)? {
            CardReply::CloseTarget { target } => targets.push(target),
            CardReply::Refused { reason } => return Ok(writeln!(render::err(), "{reason}")?),
            CardReply::Done => {}
        }
    }
    let (ctx, globals) = open_room(globals, &session, mux)?;
    let tab = TabClose::Whole {
        panes,
        loose: Vec::new(),
    };
    close(&ctx, &globals, &session, targets, tab)
}

/// Close `targets` through one hosting read: raw panes and agents no pane hosts close
/// their pane, and every hosted agent stops in one batch, children before parents, acting
/// on that read unless a pane closed here sat between it and the stop. `tab` is the stop's
/// [`TabClose`], and these closes keep it: under `WhenLast` (Alt+x) a pane the gating read
/// finds the last working pane of its tab closes with its tab, and under `Whole` (Alt+Shift+X)
/// none closes here, the stop carries them as its `loose` panes, and the tab closes once,
/// after the stop or, with no agent to stop, on this read. A read that gives the tab no
/// anchor (this process in it, the session's only tab) closes each pane on its own. Each
/// stopped root that offers a resume enters the closed-card list, in `targets` order, as the
/// session its stop stopped.
fn close(
    ctx: &Ctx,
    globals: &GlobalFlags,
    session: &str,
    targets: Vec<CardTarget>,
    tab: TabClose,
) -> Result<()> {
    let snapshot = ctx.cached_snapshot()?;
    let read = match read_hosting(&ctx.workspace, &ctx.store, globals.mux) {
        Ok(mut read) => {
            render::room::print_notices(read.take_notices())?;
            read
        }
        Err(mut err) => {
            render::room::print_notices(std::mem::take(&mut err.notices))?;
            bail!("sidebar close: {err}; nothing was closed");
        }
    };
    let own = rimz::mux::own_pane_id(read.mux());
    let mut tab = match tab {
        TabClose::Whole { panes, .. } if read.tab_anchor(&panes, own.as_ref()).is_none() => {
            TabClose::Never
        }
        tab => tab,
    };
    let whole = matches!(tab, TabClose::Whole { .. });
    let when_last = matches!(tab, TabClose::WhenLast);
    let mut agents = Vec::new();
    let mut closed_pane = false;
    let mut loose = Vec::new();
    for target in targets {
        let (kind, agent_id, pane) = match target {
            CardTarget::Agent {
                kind,
                agent_id,
                pane,
            } => (kind, agent_id, pane),
            CardTarget::Pane(pane) if whole => {
                loose.push(pane);
                continue;
            }
            CardTarget::Pane(pane) => {
                close_pane(session, &pane, when_last && !closed_pane, &read)?;
                closed_pane = true;
                continue;
            }
        };
        match read.agent_hosting_of_session(&kind, &agent_id).holder {
            Some(agent) if agents.contains(&agent) => {}
            Some(agent) => agents.push(agent),
            None if whole => loose.push(pane),
            None => {
                close_pane(session, &pane, when_last && !closed_pane, &read)?;
                closed_pane = true;
            }
        }
    }
    if let TabClose::Whole { loose: carried, .. } = &mut tab {
        *carried = loose;
    }
    if agents.is_empty() {
        if let TabClose::Whole { panes, .. } = &tab
            && let Some(anchor) = read.tab_anchor(panes, own.as_ref())
        {
            rimz::mux::backend_for(read.mux()).close_tab_of_pane(session, anchor)?;
        }
        return Ok(());
    }
    let owned = agents.into_iter().cloned().collect::<Vec<_>>();
    let agents = owned.iter().collect::<Vec<_>>();
    let terms = agents_cmd::StopTerms {
        read: (!closed_pane).then_some(read),
        tab,
        ..Default::default()
    };
    // Without `still_due` every stop goes ahead.
    let stops = agents_cmd::stop_resolved_with(ctx, globals, &snapshot, &agents, terms)?
        .unwrap_or_default();
    let mut entries = read_closed(ctx);
    let recorded = entries.len();
    let mut failures = Vec::new();
    for (agent, stop) in agents.into_iter().zip(stops) {
        // A swap that bound during the stop leaves its holder the session stopped.
        let agent = &stop.root().unwrap_or(agent).clone();
        if let Err(err) = stop.into_result(agent.agent_id.as_str()) {
            failures.push(format!("{err:#}"));
            continue;
        }
        if !agents_cmd::offers_resume(&ctx.workspace, agent)
            || !rimz::harness::resume::resume_session_present(agent)
        {
            continue;
        }
        entries.push(ClosedCard {
            kind: agent.kind.clone(),
            agent_id: agent.agent_id.clone(),
            team: agent.team.clone(),
            role: agent.role.clone(),
            worktree: agents_cmd::resume_worktree_name(&ctx.workspace, agent).map(str::to_owned),
            closed_at: rimz::utils::time::unix_now_ms(),
        });
    }
    if entries.len() > recorded {
        write_closed(ctx, &entries)?;
    }
    anyhow::ensure!(failures.is_empty(), "{}", failures.join("; "));
    Ok(())
}

/// Close `pane`, with its tab when `close_tab` holds and `read`, which nothing has acted
/// on since, finds it the tab's last working pane.
fn close_pane(
    session: &str,
    pane: &rimz::ids::PaneId,
    close_tab: bool,
    read: &rimz::sidebar::produce::hosting::HostingRead,
) -> Result<()> {
    let backend = rimz::mux::backend_for(pane.mux());
    if close_tab && read.closing_empties_tab(pane, rimz::mux::own_pane_id(pane.mux()).as_ref()) {
        return Ok(backend.close_tab_of_pane(session, pane)?);
    }
    Ok(backend.close_pane(session, pane)?)
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
