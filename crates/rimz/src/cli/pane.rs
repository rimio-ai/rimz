//! `rimz pane` — the public pane primitives: list, bandwidth, capture, send, focus.

mod bandwidth;

use std::collections::{HashMap, HashSet};
use std::io::Write;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use super::GlobalFlags;
use crate::cli::render;
use rimz::agents::{AgentState, TurnPhase};
use rimz::disk::paths::RuntimePaths;
use rimz::ids::PaneId;
use rimz::mux::{
    MuxBackend, PaneListOptions, PaneWriter, SplitPaneOptions, SplitPlacement, SplitTarget,
};
use rimz::pane::PaneRef;
use rimz::pane::keys::NamedKey;
use rimz::workspace::{ResolvedWorkspace, WorkspaceResolver};

#[derive(Debug, Args)]
pub struct PaneArgs {
    #[command(subcommand)]
    command: PaneSubcmd,
}

#[derive(Debug, Subcommand)]
enum PaneSubcmd {
    /// Show panes: who is in each, its status, command, and id.
    List {
        /// Limit to a channel, worktree, branch, or directory name (`#` optional).
        #[arg(conflicts_with = "worktree")]
        scope: Option<String>,
        /// Limit to a worktree by name.
        #[arg(short, long, value_name = "NAME")]
        worktree: Option<String>,
        /// Include sidebar panes in the table. JSON always includes them.
        #[arg(long)]
        all: bool,
        /// Emit JSON.
        #[arg(long)]
        json: bool,
        /// Session to list. Defaults to the cwd's workspace session.
        #[arg(long)]
        session_name: Option<String>,
    },
    /// Print what a pane shows right now.
    Capture {
        /// Pane id, agent address, or `sidebar` (`zellij:terminal_3`, `tmux:%1`, `@coder#lane`).
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::pane_targets
        ))]
        target: String,
        /// Print the last N lines, reaching into scrollback. Default: the visible screen
        #[arg(long, value_name = "N", value_parser = clap::value_parser!(u16).range(1..))]
        lines: Option<u16>,
        /// Emit JSON.
        #[arg(long)]
        json: bool,
        /// Keep ANSI colors/attributes.
        #[arg(long)]
        ansi: bool,
    },
    /// Type text and press keys in a pane.
    #[command(after_help = format!("Sends in a fixed order: TEXT, then each --key, then Enter. To press a key first, run a separate send.\n\nKeys: {}\n\nTo prompt an agent, use `rimz message`; to answer its question, `rimz answer`.", NamedKey::NAMES.join(", ")))]
    Send {
        /// Pane id, agent address, or `sidebar` (`zellij:terminal_3`, `tmux:%1`, `@coder#lane`).
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::pane_targets
        ))]
        target: String,
        /// Press Enter last.
        #[arg(long)]
        enter: bool,
        /// Press a named key; repeat for several, pressed in the order given.
        #[arg(long, value_parser = parse_key)]
        key: Vec<NamedKey>,
        /// Literal text to type. May start with `-`; use `--` to escape a command flag, including `-h`/`--help`.
        #[arg(allow_hyphen_values = true)]
        text: Option<String>,
    },
    /// Jump to a pane.
    Focus {
        /// Pane id, agent address, or `sidebar` (`zellij:terminal_3`, `tmux:%1`, `@coder#lane`).
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::pane_targets
        ))]
        target: String,
        /// Room session the pane belongs to. Defaults to the cwd's workspace session.
        #[arg(long)]
        session_name: Option<String>,
        /// Refuse to focus if this pane id has been reused since the snapshot.
        #[arg(long, hide = true)]
        pane_process_start: Option<String>,
    },
    /// Toggle fullscreen for the focused pane.
    ///
    /// If the sidebar is focused, focus and fullscreen a working sibling instead.
    Zoom {
        /// Session to inspect. Defaults to the cwd's workspace session.
        #[arg(long)]
        session_name: Option<String>,
    },
    /// Open a shell in a new pane beside this one.
    ///
    /// Prints the new pane's id. From a shell outside any pane, opens the pane in the room's session.
    Split,
    /// Leave the room running and detach.
    ///
    /// Zellij detaches only the client it is run from and ignores --session-name.
    /// tmux detaches every client of the session.
    Detach {
        /// Session to detach. Defaults to the cwd's workspace session.
        #[arg(long)]
        session_name: Option<String>,
    },
    /// Measure which panes write the most output.
    Bandwidth {
        /// Sampling window in seconds.
        #[arg(long, default_value_t = 5)]
        secs: u64,
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },
}

pub fn run(args: PaneArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        PaneSubcmd::Bandwidth { secs, json } => bandwidth::run(secs, json, globals),
        PaneSubcmd::List {
            scope,
            worktree,
            all,
            json,
            session_name,
        } => {
            let mux = rimz::mux::auto_detect_backend(globals.mux)?;
            let backend = rimz::mux::backend_for(mux);
            list(
                &*backend,
                globals,
                json,
                session_name,
                scope.or(worktree).as_deref(),
                all,
            )
        }
        PaneSubcmd::Capture {
            target,
            lines,
            json,
            ansi,
        } => {
            let target = resolve_pane_target(&target, globals)?;
            let session_name = resolve_session_name(globals, target.session_name)?;
            let backend = rimz::mux::backend_for(target.pane.mux());
            backend.require_pane_in_session(&target.pane, &session_name)?;
            capture(&*backend, &target.pane, &session_name, lines, json, ansi)
        }
        PaneSubcmd::Send {
            target,
            enter,
            key,
            text,
        } => {
            let target = resolve_pane_target(&target, globals)?;
            let session_name = resolve_session_name(globals, target.session_name)?;
            rimz::mux::backend_for(target.pane.mux())
                .require_pane_in_session(&target.pane, &session_name)?;
            send(
                &RuntimePaths::shared(),
                &target.pane,
                &session_name,
                text.as_deref(),
                &key,
                enter,
            )
        }
        PaneSubcmd::Focus {
            target,
            session_name,
            pane_process_start,
            ..
        } => {
            let target = resolve_pane_target(&target, globals)?;
            let backend = rimz::mux::backend_for(target.pane.mux());
            let session_name = resolve_session_name(globals, session_name.or(target.session_name))?;
            focus(&*backend, &target.pane, &session_name, pane_process_start)
        }
        PaneSubcmd::Zoom { session_name } => {
            let mux = rimz::mux::auto_detect_backend(globals.mux)?;
            let backend = rimz::mux::backend_for(mux);
            zoom(&*backend, globals, session_name)
        }
        PaneSubcmd::Split => {
            let mux = rimz::mux::auto_detect_backend(globals.mux)?;
            let backend = rimz::mux::backend_for(mux);
            split(&*backend, globals)
        }
        PaneSubcmd::Detach { session_name } => {
            let mux = rimz::mux::auto_detect_backend(globals.mux)?;
            let backend = rimz::mux::backend_for(mux);
            detach(&*backend, globals, session_name)
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum PaneTarget {
    Id(PaneId),
    Address(String),
    Sidebar,
}

struct ResolvedPaneTarget {
    pane: PaneId,
    session_name: Option<String>,
}

fn classify_pane_target(raw: &str) -> Result<PaneTarget> {
    if raw.starts_with('#') {
        bail!(
            "`{raw}` is a channel; a channel holds several panes. Run `rimz pane list` and pass one pane's id or `@handle#channel`"
        );
    }
    if raw.starts_with('@') {
        return Ok(PaneTarget::Address(raw.to_owned()));
    }
    if raw == "sidebar" {
        return Ok(PaneTarget::Sidebar);
    }
    PaneId::parse(raw).map(PaneTarget::Id).map_err(|_| {
        anyhow::anyhow!(
            "invalid pane target `{raw}`: expected a pane id (`zellij:terminal_3`, `tmux:%1`), an agent address (`@coder`, `@coder#lane`), or `sidebar`; run `rimz pane list` to see panes"
        )
    })
}

fn resolve_pane_target(raw: &str, globals: &GlobalFlags) -> Result<ResolvedPaneTarget> {
    if raw.strip_prefix('%').is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    }) && rimz::mux::auto_detect_backend(globals.mux)? == rimz::MuxName::Tmux
    {
        return Ok(ResolvedPaneTarget {
            pane: PaneId::from_parts(rimz::MuxName::Tmux, raw),
            session_name: None,
        });
    }
    match classify_pane_target(raw)? {
        PaneTarget::Id(pane) => Ok(ResolvedPaneTarget {
            pane,
            session_name: None,
        }),
        PaneTarget::Address(address) => {
            let ctx = crate::cli::Ctx::open(globals)?;
            let snapshot = ctx.cached_snapshot()?;
            let agent = crate::cli::resolve_agent_one(
                &ctx.store,
                &snapshot,
                &address,
                None,
                &ctx.address_context(),
            )?;
            let pane = agent
                .pane
                .as_ref()
                .map(|pane| pane.pane_id.clone())
                .ok_or_else(|| anyhow::anyhow!("agent {} has no bound pane", agent_name(agent)))?;
            Ok(ResolvedPaneTarget {
                pane,
                session_name: Some(ctx.workspace.session_name.clone()),
            })
        }
        PaneTarget::Sidebar => {
            let mux = rimz::mux::auto_detect_backend(globals.mux)?;
            let backend = rimz::mux::backend_for(mux);
            let session_name =
                WorkspaceResolver::resolve_participant(".", globals.root.clone())?.session_name;
            let listing = backend
                .list_panes(PaneListOptions {
                    session_name: Some(session_name.clone()),
                    ..Default::default()
                })
                .context("listing panes")?;
            let own_view = rimz::mux::own_pane_id(mux).and_then(|own| {
                listing
                    .panes
                    .iter()
                    .find(|pane| pane.pane_id == own)
                    .and_then(|pane| pane.view_id.clone())
            });
            let focused_view = backend
                .client_view(rimz::mux::ClientFocusOptions {
                    session_name: Some(session_name.clone()),
                    ..Default::default()
                })
                .ok()
                .and_then(|view| {
                    let mut viewed = view.viewed_panes;
                    viewed.sort_by_key(ToString::to_string);
                    viewed.dedup();
                    match viewed.as_slice() {
                        [pane] => Some(pane.clone()),
                        _ => None,
                    }
                })
                .and_then(|focused| {
                    listing
                        .panes
                        .iter()
                        .find(|pane| pane.pane_id == focused)
                        .and_then(|pane| pane.view_id.clone())
                });
            let pane = rimz::pane::select_sidebar_pane(&listing.panes, &[own_view, focused_view])
                .map(|pane| pane.pane_id.clone())
                .ok_or_else(|| anyhow::anyhow!("session {session_name} has no sidebar pane"))?;
            Ok(ResolvedPaneTarget {
                pane,
                session_name: Some(session_name),
            })
        }
    }
}

fn agent_name(agent: &AgentState) -> &str {
    agent.name.as_deref().unwrap_or(agent.agent_id.as_str())
}

/// List the room as panes: every pane grouped by its native tab, each labelled
/// with the agent-colleague that lives in it (`@kind#worktree`), `sidebar`, or
/// `process` for a plain pane, alongside its status and working directory.
///
/// The pane enumeration is the spine and always works. The agent annotations are
/// a best-effort overlay folded from the workspace snapshot the same way the
/// sidebar reads it — when no snapshot is available (no store, foreign session),
/// panes still list, just labelled `process` rather than carrying a `@handle`.
/// A scoped listing requires the overlay to establish tab membership.
fn list(
    backend: &dyn MuxBackend,
    globals: &GlobalFlags,
    json: bool,
    session_name: Option<String>,
    scope: Option<&str>,
    all: bool,
) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone()).ok();
    if let Some(name) = &session_name
        && let Ok(sessions) = backend.list_sessions()
        && !sessions.contains(name)
    {
        let live = if sessions.is_empty() {
            "none".to_owned()
        } else {
            sessions.join(", ")
        };
        bail!("session `{name}` is not active; live sessions: {live}");
    }
    let session = match session_name {
        Some(name) => name,
        None => workspace
            .as_ref()
            .map(|workspace| workspace.session_name.clone())
            .context("resolving the cwd's workspace session; pass --session-name")?,
    };
    let panes: Vec<PaneRef> = backend
        .list_panes(PaneListOptions {
            session_name: Some(session.clone()),
            ..Default::default()
        })?
        .panes;
    let self_pane = workspace
        .as_ref()
        .filter(|workspace| workspace.session_name == session)
        .and_then(|_| rimz::mux::own_pane_id(backend.name()));
    // Only overlay agents when listing this workspace's own session — a foreign
    // session's pane ids carry no meaning in our rollup.
    let overlay = workspace
        .as_ref()
        .filter(|workspace| workspace.session_name == session)
        .and_then(|workspace| load_agent_overlay(workspace, &panes));
    let panes = if let Some(scope) = scope {
        let snapshot = overlay.as_ref().with_context(|| {
            format!("cannot scope panes in session `{session}`: no agent records available; omit the scope to list all panes")
        })?;
        let panes = filter_panes_by_scope(panes, snapshot, scope);
        if panes.is_empty() && !json {
            writeln!(
                render::err(),
                "{}",
                render::paint(
                    render::palette::faint(),
                    &format!("No panes match `{scope}` in session `{session}`.")
                )
            )?;
            return Ok(());
        }
        panes
    } else {
        panes
    };
    let agents: Vec<&AgentState> = overlay
        .as_ref()
        .map(|snapshot| snapshot.pane_bound_roots().collect())
        .unwrap_or_default();
    // Bind through the snapshot so the overlay matches the room the sidebar
    // renders: the same stamped-id + process-start guard, never a bare pane-id
    // lookup that a reused pane could mislabel.
    let agent_for = |pane: &PaneRef| -> Option<&AgentState> {
        overlay
            .as_ref()
            .and_then(|snapshot| snapshot.agent_bound_to_pane(pane))
    };

    if json {
        let tabs = group_by_tab(&panes);
        let payload = PaneListJson {
            session: &session,
            mux: backend.name().as_str(),
            tabs: tabs
                .iter()
                .map(|tab| TabJson {
                    view_id: tab.view_id.as_deref(),
                    name: tab.name.as_deref(),
                    panes: tab
                        .panes
                        .iter()
                        .map(|pane| {
                            let is_self = self_pane.as_ref() == Some(&pane.pane_id);
                            pane_json(pane, agent_for(pane), &agents, is_self)
                        })
                        .collect(),
                })
                .collect(),
        };
        return render::json_pretty(&payload);
    }

    let (table, hidden) = pane_table(&panes, overlay.as_ref(), self_pane.as_ref(), all);
    let mut out = render::out();
    table.render(&mut out)?;
    if hidden > 0 {
        writeln!(out, "{}", sidebar_hint(hidden))?;
    }
    Ok(())
}

/// Best-effort snapshot for the agent overlay: the cached rollup the sidebar
/// reads, or `None` when no store is reachable.
fn load_agent_overlay(
    workspace: &ResolvedWorkspace,
    panes: &[PaneRef],
) -> Option<rimz::store::snapshot::SidebarSnapshot> {
    let store = crate::cli::open_existing_store(workspace).ok().flatten()?;
    let mut snapshot = store.snapshot_cached().ok()?;
    let runtime = rimz::RuntimePaths::for_project_root(&workspace.project_root).ok()?;
    snapshot = snapshot.with_agent_context(rimz::store::agent_context::read_all(&runtime));
    Some(snapshot.with_live_panes(panes.to_vec(), None))
}

/// One native tab/window and the panes inside it, in listing order.
struct TabGroup<'a> {
    view_id: Option<String>,
    name: Option<String>,
    panes: Vec<&'a PaneRef>,
}

impl TabGroup<'_> {
    /// The section header: the mux's own tab name (already `#<worktree>` for a
    /// worktree launch, `<kind>:<dir>` otherwise), falling back to the view id.
    fn label(&self) -> String {
        self.name
            .clone()
            .or_else(|| self.view_id.clone())
            .unwrap_or_else(|| "(panes)".to_owned())
    }
}

/// Bucket panes by native tab, preserving first-seen tab order and pane order.
fn group_by_tab(panes: &[PaneRef]) -> Vec<TabGroup<'_>> {
    let mut tabs: Vec<TabGroup> = Vec::new();
    for pane in panes {
        let group = match tabs.iter_mut().find(|tab| tab.view_id == pane.view_id) {
            Some(group) => group,
            None => {
                tabs.push(TabGroup {
                    view_id: pane.view_id.clone(),
                    name: pane
                        .view_name
                        .as_deref()
                        .map(clean_tab_name)
                        .map(str::to_owned),
                    panes: Vec::new(),
                });
                tabs.last_mut().expect("just pushed")
            }
        };
        if group.name.is_none() {
            group.name = pane
                .view_name
                .as_deref()
                .map(clean_tab_name)
                .map(str::to_owned);
        }
        group.panes.push(pane);
    }
    tabs
}

fn clean_tab_name(name: &str) -> &str {
    rimz::theme::strip_status_glyph_suffix(name, &crate::cli::machine_config().theme)
}

/// The styled cells for one pane row: occupant (agent handle, `sidebar`, or
/// `process`), status, command, cwd, and the pane id.
fn pane_row(
    pane: &PaneRef,
    agent: Option<&AgentState>,
    peers: &[&AgentState],
    is_self: bool,
) -> Vec<render::Cell> {
    let agent = agent.filter(|_| !pane.is_rimz_sidebar());
    let occupant_cell = if pane.is_rimz_sidebar() {
        render::cell("sidebar").fg(render::palette::muted())
    } else {
        match agent {
            Some(agent) => render::cell(rimz::address::agent_handle(agent, peers, true))
                .fg(render::palette::accent()),
            None => render::cell("process").fg(render::palette::muted()),
        }
    };
    let status_cell = match agent {
        Some(agent) => {
            let status = agent.effective_status();
            let phase = if status == rimz::agents::AgentStatus::Running {
                agent.phase
            } else {
                TurnPhase::Idle
            };
            render::cell(status.as_str()).fg(render::status::agent(status, phase))
        }
        None => render::cell("-").dash(),
    };
    let command_cell = match agent {
        Some(agent) => {
            render::cell(agent.kind.as_str()).fg(render::palette::identity(agent.kind.as_str()))
        }
        None => render::cell(pane.command.as_deref().unwrap_or("-")).dash(),
    };
    let cwd = pane
        .cwd
        .as_deref()
        .map_or_else(|| "-".to_owned(), render::home_relative);
    let mut pane_cell = render::cell(pane.pane_id.to_string()).fg(render::palette::meta());
    if is_self {
        pane_cell = pane_cell.suffix("(self)", render::palette::faint());
    }
    vec![
        occupant_cell,
        status_cell,
        command_cell,
        render::cell(cwd).dash(),
        pane_cell,
    ]
}

#[derive(serde::Serialize)]
struct PaneListJson<'a> {
    session: &'a str,
    mux: &'a str,
    tabs: Vec<TabJson<'a>>,
}

#[derive(serde::Serialize)]
struct TabJson<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    view_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    panes: Vec<PaneJson<'a>>,
}

#[derive(serde::Serialize)]
struct PaneJson<'a> {
    pane_id: String,
    /// `sidebar` for RimZ chrome, `agent` when an overlay binds, `process` otherwise.
    kind: &'static str,
    #[serde(rename = "self", skip_serializing_if = "is_false")]
    is_self: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<AgentJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pid: Option<u32>,
}

#[derive(serde::Serialize)]
struct AgentJson {
    kind: String,
    handle: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    worktree: Option<String>,
}

fn pane_json<'a>(
    pane: &'a PaneRef,
    agent: Option<&AgentState>,
    peers: &[&AgentState],
    is_self: bool,
) -> PaneJson<'a> {
    let agent = agent.filter(|_| !pane.is_rimz_sidebar());
    PaneJson {
        pane_id: pane.pane_id.to_string(),
        kind: if pane.is_rimz_sidebar() {
            "sidebar"
        } else if agent.is_some() {
            "agent"
        } else {
            "process"
        },
        is_self,
        agent: agent.map(|agent| AgentJson {
            kind: agent.kind.to_string(),
            handle: rimz::address::agent_handle(agent, peers, true),
            status: agent.effective_status().as_str().to_owned(),
            worktree: agent.channel(),
        }),
        command: pane.command.as_deref(),
        cwd: pane.cwd.as_deref(),
        pid: pane.pane_pid,
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn capture(
    backend: &dyn MuxBackend,
    pane: &PaneId,
    session_name: &str,
    lines: Option<u16>,
    json: bool,
    ansi: bool,
) -> Result<()> {
    let capture = backend.capture_pane(pane, session_name, lines, ansi)?;
    if json {
        render::json_pretty(&capture)?;
    } else {
        #[expect(clippy::print_stdout, reason = "raw capture text")]
        {
            print!("{}", capture.raw_text);
        }
    }
    Ok(())
}

fn focus(
    backend: &dyn MuxBackend,
    pane: &PaneId,
    session_name: &str,
    pane_process_start: Option<String>,
) -> Result<()> {
    validate_pane_not_reused(
        backend,
        pane,
        Some(session_name),
        pane_process_start.as_deref(),
    )?;
    let workspace_id = rimz::room::session::workspace_record_for_session(session_name)?
        .map(|record| record.workspace_id)
        .ok_or_else(|| anyhow::anyhow!("pane focus requires a managed RimZ room session"))?;
    let runtime = rimz::RuntimePaths::for_workspace(workspace_id)?;
    rimz::mux::focus_anchor::execute_action(backend, &runtime, session_name, pane.clone())?;
    Ok(())
}

fn zoom(
    backend: &dyn MuxBackend,
    globals: &GlobalFlags,
    session_name: Option<String>,
) -> Result<()> {
    let session_name = resolve_session_name(globals, session_name)?;
    let listing = backend
        .list_panes(PaneListOptions {
            session_name: Some(session_name.clone()),
            ..Default::default()
        })
        .context("listing panes for zoom")?;
    let view = backend
        .client_view(rimz::mux::ClientFocusOptions {
            session_name: Some(session_name.clone()),
            ..Default::default()
        })
        .context("sampling attached client focus for zoom")?;
    let live = listing
        .panes
        .iter()
        .map(|pane| pane.pane_id.clone())
        .collect::<HashSet<_>>();
    let focused =
        rimz::mux::ClientView::unique_live_focus(&view.clients, &view.viewed_panes, &live)
            .ok_or_else(|| {
                anyhow::anyhow!("pane zoom requires one attached client focused on one live pane")
            })?;
    let target = if listing
        .panes
        .iter()
        .find(|pane| pane.pane_id == focused)
        .is_some_and(PaneRef::is_rimz_sidebar)
    {
        let sidebar_view = listing
            .panes
            .iter()
            .find(|pane| pane.pane_id == focused)
            .and_then(|pane| pane.view_id.as_ref());
        let Some(sibling) = listing
            .panes
            .iter()
            .filter(|pane| pane.pane_id != focused && !pane.is_rimz_sidebar())
            .find(|pane| sidebar_view.is_none() || pane.view_id.as_ref() == sidebar_view)
        else {
            writeln!(
                render::err(),
                "rimz: sidebar is the only pane in its view; nothing to zoom"
            )?;
            return Ok(());
        };
        let workspace = rimz::room::session::workspace_record_for_session(&session_name)?
            .ok_or_else(|| anyhow::anyhow!("pane zoom requires a managed RimZ room session"))?;
        let runtime = rimz::RuntimePaths::for_project_root(&workspace.project_root)?;
        rimz::mux::focus_anchor::execute_action(
            backend,
            &runtime,
            &session_name,
            sibling.pane_id.clone(),
        )
        .context("focusing working pane before zoom")?;
        &sibling.pane_id
    } else {
        &focused
    };
    backend
        .toggle_fullscreen(target, Some(&session_name))
        .context("toggling pane fullscreen")
}

fn validate_pane_not_reused(
    backend: &dyn MuxBackend,
    pane: &PaneId,
    session_name: Option<&str>,
    expected_start: Option<&str>,
) -> Result<()> {
    let Some(expected_start) = expected_start else {
        return Ok(());
    };
    let panes = backend
        .list_panes(PaneListOptions {
            session_name: session_name.map(str::to_owned),
            ..Default::default()
        })?
        .panes;
    let Some(live) = panes.iter().find(|candidate| candidate.pane_id == *pane) else {
        bail!("pane {pane} is no longer present");
    };
    if let Some(actual) = live.pane_process_start
        && actual.to_string() != expected_start
    {
        bail!("pane {pane} was reused since the sidebar snapshot");
    }
    Ok(())
}

fn split(backend: &dyn MuxBackend, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let direction = rimz::mux::detect_terminal_size()
        .map(|(cols, rows)| rimz::mux::split_along_longer_edge(cols, rows))
        .unwrap_or_default();
    let created = backend.split_pane(SplitPaneOptions {
        target: rimz::mux::own_pane_id(backend.name()).map_or_else(
            || SplitTarget::Session(workspace.session_name.clone()),
            SplitTarget::Pane,
        ),
        cwd: Some(workspace.worktree_root.display().to_string()),
        command: None,
        title: None,
        close_on_exit: false,
        env: rimz::room::pane_identity_env(&workspace, &workspace.worktree_root, None, true),
        placement: SplitPlacement::Directional(direction),
        focus: true,
    })?;
    match created {
        Some(pane) => writeln!(render::out(), "{pane}")?,
        None => writeln!(render::err(), "Pane opened; Zellij reported no pane id.")?,
    }
    Ok(())
}

fn detach(
    backend: &dyn MuxBackend,
    globals: &GlobalFlags,
    session_name: Option<String>,
) -> Result<()> {
    let session_name = resolve_session_name(globals, session_name)?;
    backend.detach(&session_name).map_err(Into::into)
}

fn resolve_session_name(globals: &GlobalFlags, session_name: Option<String>) -> Result<String> {
    match session_name {
        Some(name) => Ok(name),
        None => Ok(WorkspaceResolver::resolve_participant(".", globals.root.clone())?.session_name),
    }
}

fn send(
    runtime: &RuntimePaths,
    pane: &PaneId,
    session_name: &str,
    text: Option<&str>,
    keys: &[NamedKey],
    enter: bool,
) -> Result<()> {
    // The generic primitive types raw — a target may be a bare shell where
    // bracketed-paste markers would echo literally. Agent-composer submits
    // (`message`) take the bracketed `submit_message` path instead.
    if text.is_none_or(str::is_empty) && keys.is_empty() && !enter {
        bail!("expected text, --key, or --enter");
    }
    let writer = PaneWriter::open(runtime, pane, session_name)?;
    if let Some(text) = text.filter(|text| !text.is_empty()) {
        writer.type_text(text)?;
    }
    for key in keys {
        writer.press(*key)?;
    }
    if enter {
        writer.press(NamedKey::Enter)?;
    }
    Ok(())
}

fn parse_key(raw: &str) -> std::result::Result<NamedKey, String> {
    raw.parse::<NamedKey>().map_err(|err| err.to_string())
}

fn pane_table(
    panes: &[PaneRef],
    overlay: Option<&rimz::store::snapshot::SidebarSnapshot>,
    self_pane: Option<&PaneId>,
    all: bool,
) -> (render::Table, usize) {
    let mut table = render::Table::new(["AGENT", "STATUS", "COMMAND", "CWD", "PANE"]);
    let peers: Vec<_> = overlay
        .map(|snapshot| snapshot.pane_bound_roots().collect())
        .unwrap_or_default();
    let mut headings = HashMap::new();
    let mut hidden = 0;
    for tab in group_by_tab(panes) {
        let visible: Vec<_> = tab
            .panes
            .iter()
            .copied()
            .filter(|pane| all || !pane.is_rimz_sidebar())
            .collect();
        hidden += tab.panes.len() - visible.len();
        if visible.is_empty() {
            continue;
        }
        let label = tab.label();
        let count = headings.entry(label.clone()).or_insert(0);
        *count += 1;
        table.section(if *count == 1 {
            label
        } else {
            format!("{label} ({count})")
        });
        for pane in visible {
            let agent = overlay.and_then(|snapshot| snapshot.agent_bound_to_pane(pane));
            table.row(pane_row(
                pane,
                agent,
                &peers,
                self_pane == Some(&pane.pane_id),
            ));
        }
    }
    (table, hidden)
}

fn filter_panes_by_scope(
    panes: Vec<PaneRef>,
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    scope: &str,
) -> Vec<PaneRef> {
    let scope = scope.trim_start_matches('#');
    let views: HashSet<_> = panes
        .iter()
        .filter(|pane| {
            !pane.is_rimz_sidebar()
                && snapshot
                    .agent_bound_to_pane(pane)
                    .is_some_and(|agent| rimz::address::agent_in_worktree(agent, scope))
        })
        .filter_map(|pane| pane.view_id.clone())
        .collect();
    panes
        .into_iter()
        .filter(|pane| {
            if !pane.is_rimz_sidebar()
                && let Some(agent) = snapshot.agent_bound_to_pane(pane)
            {
                return rimz::address::agent_in_worktree(agent, scope);
            }
            pane.view_id
                .as_ref()
                .is_some_and(|view| views.contains(view))
        })
        .collect()
}

fn sidebar_hint(hidden: usize) -> String {
    if hidden == 1 {
        "+1 sidebar · --all shows it".to_owned()
    } else {
        format!("+{hidden} sidebars · --all shows them")
    }
}

#[cfg(test)]
mod tests;
