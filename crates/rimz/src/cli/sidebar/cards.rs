use std::collections::HashSet;
use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use rimz::ids::{AgentKind, AgentSessionId, MuxName, PaneId};
use rimz::mux::{ClientFocusOptions, ClientView, PaneListOptions, PaneReadConsistency};
use rimz::sidebar::cache::read_snapshot_cache;
use rimz::sidebar::produce::hosting::{HostingRead, read_hosting};
use rimz::sidebar_pane::{CardReply, CardRequest};
use rimz::wakeup::events::SidebarEvent;
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
    match rimz::sidebar_pane::request_card(mux, &session, request)? {
        CardReply::Done => Ok(()),
        CardReply::Refused { reason } => Ok(writeln!(render::err(), "{reason}")?),
        CardReply::CloseTarget { pane } => {
            let (ctx, globals) = open_room(globals, &session, mux)?;
            let mut read = read_room(&ctx, &globals, "")?;
            render::room::print_notices(read.take_notices())?;
            close(
                &ctx,
                &globals,
                &session,
                vec![pane],
                TabClose::WhenLast,
                read,
            )
            .map(drop)
        }
    }
}

/// The mark a close that names its pane puts on that pane's card in every renderer of the
/// session. It goes out first, before the room opens or the mux is read, so the press shows
/// while the close works; a close that goes ahead leaves it to leave with the card, and any
/// other end lifts it. Best effort, like every wakeup: the renderer bounds a mark whose
/// clear is lost.
struct ClosingMark {
    runtime: Option<rimz::RuntimePaths>,
    session: String,
    pane: PaneId,
}

impl ClosingMark {
    fn send(session: &str, pane: PaneId) -> Self {
        let runtime = rimz::room::session::workspace_record_for_session(session)
            .ok()
            .flatten()
            .and_then(|record| rimz::RuntimePaths::for_project_root(&record.project_root).ok());
        let mark = Self {
            runtime,
            session: session.to_owned(),
            pane,
        };
        mark.broadcast(SidebarEvent::CardClosing {
            pane_id: mark.pane.clone(),
        });
        mark
    }

    /// End the mark with the close's outcome: lift it unless the close went ahead.
    fn settle(self, closed: Result<bool>) -> Result<()> {
        if !matches!(closed, Ok(true)) {
            self.broadcast(SidebarEvent::CardClosingCleared {
                pane_id: self.pane.clone(),
            });
        }
        closed.map(drop)
    }

    fn broadcast(&self, event: SidebarEvent) {
        let Some(runtime) = &self.runtime else {
            return;
        };
        if let Err(err) = rimz::wakeup::broadcast(runtime, Some(&self.session), event) {
            tracing::debug!(pane = %self.pane, error = %err, "closing mark broadcast failed");
        }
    }
}

/// Close the card that owns `pane`, as `--pane` and `--from-pane` do, by the close's own
/// hosting read: the sidebar host is not asked, since its view can lag the store. A sidebar
/// pane or a pane in the dashboard closes nothing.
pub(super) fn close_from_pane(
    globals: &GlobalFlags,
    session: Option<String>,
    pane: PaneId,
) -> Result<()> {
    let (session, mux) = resolve_session(globals, session)?;
    let mark = ClosingMark::send(&session, pane.clone());
    mark.settle(close_pressed(globals, &session, mux, pane))
}

/// Whether the press went on to a close.
fn close_pressed(globals: &GlobalFlags, session: &str, mux: MuxName, pane: PaneId) -> Result<bool> {
    if published_closes_nothing(session, &pane) {
        return Ok(false);
    }
    // The press's one pane listing: it answers the sidebar test, the pane's owner, and the
    // tab-when-last verdict the close acts on.
    let (ctx, room_globals) = open_room(globals, session, mux)?;
    let mut read = read_room(&ctx, &room_globals, "listing panes: ")?;
    if read.listed(&pane).is_some_and(closes_nothing) {
        return Ok(false);
    }
    render::room::print_notices(read.take_notices())?;
    close(
        &ctx,
        &room_globals,
        session,
        vec![pane],
        TabClose::WhenLast,
        read,
    )
}

/// The room's hosting read, the one pane listing a close acts on, its notices left for the
/// caller to print. A read that fails prints them and closes nothing; `failure` leads its
/// reason.
fn read_room(ctx: &Ctx, globals: &GlobalFlags, failure: &str) -> Result<HostingRead> {
    match read_hosting(&ctx.workspace, &ctx.store, globals.mux) {
        Ok(read) => Ok(read),
        Err(mut err) => {
            render::room::print_notices(std::mem::take(&mut err.notices))?;
            bail!("sidebar close: {failure}{err}; nothing was closed");
        }
    }
}

/// Whether the published pane frame names `pane` as a sidebar or dashboard pane, so the press
/// raises no host refusal. A pane keeps that role for life, so a cached answer cannot go stale
/// in the direction that matters; the read that gates an actual close stays the fresh one.
fn published_closes_nothing(session: &str, pane: &PaneId) -> bool {
    let Some(runtime) = rimz::room::session::workspace_record_for_session(session)
        .ok()
        .flatten()
        .and_then(|record| rimz::RuntimePaths::for_project_root(&record.project_root).ok())
    else {
        return false;
    };
    read_snapshot_cache(&runtime.pane_frame_path(), session).is_some_and(|frame| {
        frame
            .to_pane_refs()
            .iter()
            .any(|listed| listed.pane_id == *pane && closes_nothing(listed))
    })
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
    let panes = listing
        .panes
        .iter()
        .filter(|pane| {
            pane.view_id == focused.view_id && !pane.is_floating && !pane.is_rimz_sidebar()
        })
        .map(|pane| pane.pane_id.clone())
        .collect::<Vec<_>>();
    let (ctx, globals) = open_room(globals, &session, mux)?;
    let tab = TabClose::Whole {
        panes: panes.clone(),
        loose: Vec::new(),
    };
    let mut read = read_room(&ctx, &globals, "")?;
    render::room::print_notices(read.take_notices())?;
    close(&ctx, &globals, &session, panes, tab, read).map(drop)
}

/// Close the cards of `panes` through `read`, one hosting read, which also names each pane's
/// owner ([`HostingRead::pane_owner`]): a pane `read` does not list refuses the whole close,
/// with nothing closed; a pane no session holds closes as a raw pane; and every owning agent
/// stops in one batch, children before parents, acting
/// on that read unless a pane closed here sat between it and the stop. `tab` is the stop's
/// [`TabClose`], and these closes keep it: under `WhenLast` (Alt+x) a pane the gating read
/// finds the last working pane of its tab closes with its tab, and under `Whole` (Alt+Shift+X)
/// none closes here, the stop carries them as its `loose` panes, and the tab closes once,
/// after the stop or, with no agent to stop, on this read. A read that gives the tab no
/// anchor (this process in it, the session's only tab) closes each pane on its own. Each
/// stopped root that offers a resume enters the closed-card list, in `panes` order, as the
/// session its stop stopped. Whether the close went ahead.
fn close(
    ctx: &Ctx,
    globals: &GlobalFlags,
    session: &str,
    panes: Vec<PaneId>,
    tab: TabClose,
    read: HostingRead,
) -> Result<bool> {
    let mut owners = Vec::new();
    for pane in panes {
        match read.pane_owner(&pane) {
            Ok(owner) => owners.push((pane, owner)),
            Err(unlisted) => {
                writeln!(
                    render::err(),
                    "sidebar close: {unlisted}; nothing was closed"
                )?;
                return Ok(false);
            }
        }
    }
    let snapshot = ctx.cached_snapshot()?;
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
    for (pane, owner) in owners {
        match owner {
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
        return Ok(true);
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
        if !agents_cmd::offers_resume(agent)
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
    Ok(true)
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
