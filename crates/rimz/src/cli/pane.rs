//! `rimz pane` — the public pane primitives: list, bandwidth, capture, send, focus.

mod bandwidth;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use super::GlobalFlags;
use crate::cli::render;
use rimz::agents::{AgentState, TurnPhase};
use rimz::ids::PaneId;
use rimz::mux::{
    MuxBackend, NamedKey, PaneListOptions, SplitPaneOptions, SplitPlacement, SplitTarget,
};
use rimz::pane::PaneRef;
use rimz::workspace::{ResolvedWorkspace, WorkspaceResolver};

#[derive(Debug, Args)]
pub struct PaneArgs {
    #[command(subcommand)]
    command: PaneSubcmd,
}

#[derive(Debug, Subcommand)]
enum PaneSubcmd {
    /// List panes known to the active multiplexer.
    List {
        /// Emit JSON.
        #[arg(long)]
        json: bool,
        /// Session to list. Defaults to the cwd's workspace session.
        #[arg(long)]
        session_name: Option<String>,
    },
    /// Profile the current room's per-pane render output (run on the host serving the room).
    Bandwidth {
        /// Sampling window in seconds.
        #[arg(long, default_value_t = 5)]
        secs: u64,
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },
    /// Capture a pane's visible text.
    Capture {
        /// Pane id or agent address (`zellij:terminal_3`, `tmux:%1`, `@coder#lane`).
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::pane_targets
        ))]
        target: String,
        /// Capture only the last N lines.
        #[arg(long)]
        lines: Option<u16>,
        /// Emit JSON.
        #[arg(long)]
        json: bool,
        /// Keep ANSI colors/attributes.
        #[arg(long)]
        ansi: bool,
    },
    /// Send text or named keys to a pane as if typed.
    Send {
        /// Pane id or agent address (`zellij:terminal_3`, `tmux:%1`, `@coder#lane`).
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::pane_targets
        ))]
        target: String,
        /// Press Enter after text and explicit keys.
        #[arg(long)]
        enter: bool,
        /// Press a named key. Repeat to press several keys in order.
        #[arg(long, value_parser = parse_key)]
        key: Vec<NamedKey>,
        /// Literal text to type. Use `--` before text that begins with `-`.
        text: Option<String>,
    },
    /// Focus a pane.
    Focus {
        /// Pane id or agent address (`zellij:terminal_3`, `tmux:%1`, `@coder#lane`).
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::pane_targets
        ))]
        target: String,
        /// Session to re-check before focusing when process-start metadata is provided.
        #[arg(long)]
        session_name: Option<String>,
        /// Refuse to focus if this pane id has been reused since the snapshot.
        #[arg(long)]
        pane_process_start: Option<String>,
    },
    /// Split off a new pane in the current view.
    Split,
    /// Detach the attached client from a session; the session keeps running in
    /// the background and resurrects on the next attach. The `rimzd` daemon
    /// tab's sidebar issues this once the daemon tab is the only tab left.
    ///
    /// Client semantics differ by backend (accepted, not papered over): Zellij's
    /// `action detach` detaches the client whose process tree this pane belongs
    /// to; tmux's `detach-client -s <session>` detaches every client of the
    /// session.
    Detach {
        /// Session to detach. Defaults to the cwd's workspace session.
        #[arg(long)]
        session_name: Option<String>,
    },
}

pub fn run(args: PaneArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        PaneSubcmd::Bandwidth { secs, json } => bandwidth::run(secs, json, globals),
        PaneSubcmd::List { json, session_name } => {
            let mux = rimz::mux::auto_detect_backend(globals.mux)?;
            let backend = rimz::mux::backend_for(mux);
            list(&*backend, globals, json, session_name)
        }
        PaneSubcmd::Capture {
            target,
            lines,
            json,
            ansi,
        } => {
            let target = resolve_pane_target(&target, globals)?;
            let backend = rimz::mux::backend_for(target.pane.mux());
            capture(&*backend, &target.pane, lines, json, ansi)
        }
        PaneSubcmd::Send {
            target,
            enter,
            key,
            text,
        } => {
            let target = resolve_pane_target(&target, globals)?;
            let backend = rimz::mux::backend_for(target.pane.mux());
            send(backend.as_ref(), &target.pane, text.as_deref(), &key, enter)
        }
        PaneSubcmd::Focus {
            target,
            session_name,
            pane_process_start,
            ..
        } => {
            let target = resolve_pane_target(&target, globals)?;
            let backend = rimz::mux::backend_for(target.pane.mux());
            let session_name = match session_name.or(target.session_name) {
                Some(session_name) => session_name,
                None => {
                    WorkspaceResolver::resolve_participant(".", globals.root.clone())?.session_name
                }
            };
            focus(&*backend, &target.pane, &session_name, pane_process_start)
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
}

struct ResolvedPaneTarget {
    pane: PaneId,
    session_name: Option<String>,
}

fn classify_pane_target(raw: &str) -> Result<PaneTarget> {
    if raw.starts_with('@') {
        return Ok(PaneTarget::Address(raw.to_owned()));
    }
    PaneId::parse(raw).map(PaneTarget::Id).map_err(|_| {
        anyhow::anyhow!(
            "invalid pane target `{raw}`: expected a pane id (`zellij:terminal_3`, `tmux:%1`) or an agent address (`@coder`, `@coder#lane`); run `rimz pane list` to see panes"
        )
    })
}

fn resolve_pane_target(raw: &str, globals: &GlobalFlags) -> Result<ResolvedPaneTarget> {
    match classify_pane_target(raw)? {
        PaneTarget::Id(pane) => Ok(ResolvedPaneTarget {
            pane,
            session_name: None,
        }),
        PaneTarget::Address(address) => {
            let ctx = crate::cli::Ctx::open(globals)?;
            let snapshot = ctx.cached_snapshot()?;
            let agent = crate::cli::resolve_agent_one(&snapshot, &address, None, ctx.channel())?;
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
    }
}

fn agent_name(agent: &AgentState) -> &str {
    agent.name.as_deref().unwrap_or(agent.agent_id.as_str())
}

/// List the room as panes: every pane grouped by its native tab, each labelled
/// with the agent-colleague that lives in it (`@kind#worktree`) or `process` for
/// a plain pane, alongside its status and working directory. RimZ's own sidebar
/// chrome is dropped — it is never a routing target.
///
/// The pane enumeration is the spine and always works. The agent annotations are
/// a best-effort overlay folded from the workspace snapshot the same way the
/// sidebar reads it — when no snapshot is available (no store, foreign session),
/// panes still list, just labelled `process` rather than carrying a `@handle`.
/// Enrichment, never a precondition.
fn list(
    backend: &dyn MuxBackend,
    globals: &GlobalFlags,
    json: bool,
    session_name: Option<String>,
) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone()).ok();
    let session = match session_name {
        Some(name) => name,
        None => workspace
            .as_ref()
            .map(|workspace| workspace.session_name.clone())
            .context("resolving the cwd's workspace session; pass --session-name")?,
    };
    // Drop RimZ's own sidebar chrome: it is never a routing target, so it has no
    // place in either the table or the `--json` tree.
    let panes: Vec<PaneRef> = backend
        .list_panes(PaneListOptions {
            session_name: Some(session.clone()),
            ..Default::default()
        })?
        .panes
        .into_iter()
        .filter(|pane| !pane.is_rimz_sidebar())
        .collect();
    // Only overlay agents when listing this workspace's own session — a foreign
    // session's pane ids carry no meaning in our rollup.
    let overlay = workspace
        .as_ref()
        .filter(|workspace| workspace.session_name == session)
        .and_then(load_agent_overlay);
    let agents: Vec<&AgentState> = overlay
        .as_ref()
        .map(|snapshot| {
            snapshot
                .agents
                .iter()
                .filter(|agent| agent.parent_agent_id.is_none())
                .collect()
        })
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
                        .map(|pane| pane_json(pane, agent_for(pane), &agents))
                        .collect(),
                })
                .collect(),
        };
        return render::json_pretty(&payload);
    }

    let mut table = render::Table::new(["AGENT", "STATUS", "CWD", "PANE"]);
    for tab in group_by_tab(&panes) {
        table.section(tab.label());
        for pane in &tab.panes {
            table.row(pane_row(pane, agent_for(pane), &agents));
        }
    }
    table.render(&mut render::out())?;
    Ok(())
}

/// Best-effort snapshot for the agent overlay: the cached rollup the sidebar
/// reads, or `None` when no store is reachable.
fn load_agent_overlay(workspace: &ResolvedWorkspace) -> Option<rimz::SidebarSnapshot> {
    let store = crate::cli::open_store(workspace).ok()?;
    let mut snapshot = store.snapshot_cached().ok()?;
    let runtime = rimz::RuntimePaths::for_workspace(workspace.workspace_id.clone()).ok()?;
    snapshot = snapshot.with_agent_context(rimz::store::agent_context::read_all(&runtime));
    Some(snapshot)
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
                    name: pane.view_name.clone(),
                    panes: Vec::new(),
                });
                tabs.last_mut().expect("just pushed")
            }
        };
        if group.name.is_none() {
            group.name = pane.view_name.clone();
        }
        group.panes.push(pane);
    }
    tabs
}

/// The styled cells for one pane row: occupant (agent handle or the literal
/// `process`), status, cwd, and the pane id.
fn pane_row(
    pane: &PaneRef,
    agent: Option<&AgentState>,
    peers: &[&AgentState],
) -> Vec<render::Cell> {
    let occupant_cell = match agent {
        Some(agent) => render::cell(rimz::harness::target::agent_handle(agent, peers, true))
            .fg(render::palette::accent()),
        None => render::cell("process").fg(render::palette::muted()),
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
    let cwd = pane
        .cwd
        .as_deref()
        .map_or_else(|| "-".to_owned(), render::home_relative);
    vec![
        occupant_cell,
        status_cell,
        render::cell(cwd).dash(),
        render::cell(pane.pane_id.to_string()).fg(render::palette::meta()),
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
    /// `agent` when an agent overlay binds to the pane, `process` otherwise.
    kind: &'static str,
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
) -> PaneJson<'a> {
    PaneJson {
        pane_id: pane.pane_id.to_string(),
        kind: if agent.is_some() { "agent" } else { "process" },
        agent: agent.map(|agent| AgentJson {
            kind: agent.kind.to_string(),
            handle: rimz::harness::target::agent_handle(agent, peers, true),
            status: agent.effective_status().as_str().to_owned(),
            worktree: rimz::harness::target::agent_channel(agent),
        }),
        command: pane.command.as_deref(),
        cwd: pane.cwd.as_deref(),
        pid: pane.pane_pid,
    }
}

fn capture(
    backend: &dyn MuxBackend,
    pane: &PaneId,
    lines: Option<u16>,
    json: bool,
    ansi: bool,
) -> Result<()> {
    let capture = backend.capture_pane(pane, lines, ansi)?;
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
    rimz::sidebar::focus_anchor::execute_action(
        backend,
        &runtime,
        session_name,
        pane.clone(),
        rimz::sidebar::focus_anchor::FocusOrigin::User,
        None,
    )?;
    Ok(())
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
    backend
        .split_pane(SplitPaneOptions {
            target: rimz::mux::own_pane_id(backend.name())
                .map_or(SplitTarget::Ambient, SplitTarget::Pane),
            cwd: Some(workspace.worktree_root.display().to_string()),
            command: None,
            title: None,
            env: rimz::room::pane_identity_env(&workspace, None, true),
            placement: SplitPlacement::Directional(direction),
            focus: true,
        })
        .map_err(Into::into)
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

pub(super) fn send_text(backend: &dyn MuxBackend, pane: &PaneId, text: &str) -> Result<()> {
    backend.send_keys(pane, text).map_err(Into::into)
}

pub(super) fn send_key(backend: &dyn MuxBackend, pane: &PaneId, key: NamedKey) -> Result<()> {
    backend.send_key(pane, key).map_err(Into::into)
}

fn send(
    backend: &dyn MuxBackend,
    pane: &PaneId,
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
    if let Some(text) = text.filter(|text| !text.is_empty()) {
        send_text(backend, pane, text)?;
    }
    for key in keys {
        send_key(backend, pane, *key)?;
    }
    if enter {
        send_key(backend, pane, NamedKey::Enter)?;
    }
    Ok(())
}

fn parse_key(raw: &str) -> std::result::Result<NamedKey, String> {
    raw.parse::<NamedKey>().map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;
    use rimz::agents::AgentStatus;
    use rimz::ids::MuxName;

    #[test]
    fn classify_pane_target_accepts_ids_and_agent_addresses() {
        assert_eq!(
            classify_pane_target("zellij:terminal_3").expect("zellij pane id"),
            PaneTarget::Id(PaneId::from_parts(MuxName::Zellij, "terminal_3"))
        );
        assert_eq!(
            classify_pane_target("tmux:%1").expect("tmux pane id"),
            PaneTarget::Id(PaneId::from_parts(MuxName::Tmux, "%1"))
        );
        assert_eq!(
            classify_pane_target("@coder#lane").expect("agent address"),
            PaneTarget::Address("@coder#lane".to_owned())
        );
    }

    #[test]
    fn classify_pane_target_error_points_at_addresses_and_pane_list() {
        let err = classify_pane_target("garbage").expect_err("invalid target");
        let message = err.to_string();
        assert!(message.contains("agent address (`@coder`, `@coder#lane`)"));
        assert!(message.contains("rimz pane list"));
    }

    fn pane(raw: &str, view: &str, name: &str, command: &str, cwd: &str) -> PaneRef {
        PaneRef {
            view_id: Some(view.to_owned()),
            view_name: Some(name.to_owned()),
            is_floating: false,
            command: Some(command.to_owned()),
            cwd: Some(cwd.to_owned()),
            ..PaneRef::from_id(PaneId::from_parts(MuxName::Zellij, raw))
        }
    }

    fn agent_on(pane_raw: &str, kind: &str, branch: &str) -> AgentState {
        let now = Timestamp::now();
        AgentState {
            kind_ordinal: Some(1),
            status: AgentStatus::Running,
            phase: rimz::agents::TurnPhase::Reasoning,
            pane: Some(PaneRef::from_id(PaneId::from_parts(
                MuxName::Zellij,
                pane_raw,
            ))),
            worktree_path: Some(format!("/repo/{branch}")),
            worktree_branch: Some(branch.to_owned()),
            ..rimz::testkit::agent_state(kind, "sess-1", now)
        }
    }

    #[test]
    fn group_by_tab_buckets_panes_in_first_seen_order() {
        let panes = vec![
            pane("terminal_1", "tab_0", "#auth", "claude", "/repo/auth"),
            pane("terminal_2", "tab_1", "shell", "zsh", "/repo"),
            pane("terminal_3", "tab_0", "#auth", "zsh", "/repo/auth"),
        ];
        let tabs = group_by_tab(&panes);
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs[0].label(), "#auth");
        assert_eq!(tabs[0].panes.len(), 2, "both auth panes land under one tab");
        assert_eq!(tabs[1].label(), "shell");
        assert_eq!(tabs[1].panes.len(), 1);
    }

    #[test]
    fn pane_json_annotates_the_bound_agent_with_its_handle() {
        let pane = pane("terminal_1", "tab_0", "#main", "claude", "/repo/main");
        let agent = agent_on("terminal_1", "claude", "main");
        let peers: Vec<&AgentState> = vec![&agent];
        let json = pane_json(&pane, Some(&agent), &peers);
        assert_eq!(json.kind, "agent");
        let bound = json.agent.as_ref().expect("agent bound");
        assert_eq!(bound.handle, "@claude#main");
        assert_eq!(bound.kind, "claude");
        assert_eq!(bound.worktree.as_deref(), Some("main"));
        let serialized = serde_json::to_value(&json).expect("pane JSON");
        assert!(serialized.get("focused").is_none());
        assert_eq!(json.pane_id, "zellij:terminal_1");
    }

    #[test]
    fn pane_json_leaves_a_plain_pane_unannotated() {
        let pane = pane("terminal_2", "tab_1", "shell", "zsh", "/home/x");
        let json = pane_json(&pane, None, &[]);
        assert!(json.agent.is_none(), "a bare shell carries no agent");
        assert_eq!(json.kind, "process");
        assert_eq!(json.command, Some("zsh"), "command is retained in json");
        assert_eq!(json.pane_id, "zellij:terminal_2");
    }

    #[test]
    fn overlay_refuses_an_agent_whose_pane_was_reused() {
        // The overlay binds through the snapshot's stamped-pane guard, so a pane
        // the multiplexer has handed to a shell since the agent left never
        // inherits that agent — the same rule the sidebar card binds by.
        let t1: Timestamp = "2026-06-01T00:00:00Z".parse().unwrap();
        let t2: Timestamp = "2026-06-01T01:00:00Z".parse().unwrap();
        let mut agent = agent_on("terminal_1", "codex", "main");
        agent.last_activity = t1;
        let snapshot = rimz::SidebarSnapshot::build_with_agents(
            rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-pane-test")),
            vec![agent],
            t2,
        );

        // terminal_1 is now a shell whose process started after the agent's last
        // activity: the pane was reused, so nothing binds.
        let reused = PaneRef {
            command: Some("zsh".to_owned()),
            pane_process_start: Some(t2),
            ..pane("terminal_1", "tab_0", "shell", "zsh", "/repo")
        };
        assert!(
            snapshot.agent_bound_to_pane(&reused).is_none(),
            "a reused pane carries no agent"
        );

        // The same pane still running codex binds as before.
        let live = pane("terminal_1", "tab_0", "#main", "codex", "/repo/main");
        let bound = snapshot
            .agent_bound_to_pane(&live)
            .expect("the live codex pane still binds");
        assert_eq!(bound.kind.as_str(), "codex");
    }
}
