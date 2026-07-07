//! `rimz pane` — the public pane primitives: list, capture, send, focus.

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use super::GlobalFlags;
use crate::cli::render;
use rimz::agents::{AgentState, TurnPhase};
use rimz::ids::PaneId;
use rimz::mux::{MuxBackend, NamedKey, PaneListOptions, SplitPaneOptions};
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
        #[arg(long)]
        json: bool,
        /// Session to list. Defaults to the cwd's workspace session.
        #[arg(long)]
        session_name: Option<String>,
    },
    /// Capture a pane's visible text.
    Capture {
        pane_id: String,
        #[arg(long)]
        lines: Option<u16>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        ansi: bool,
    },
    /// Send text or named keys to a pane as if typed.
    Send {
        pane_id: String,
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
        pane_id: String,
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
    let mux = rimz::mux::auto_detect_backend(globals.mux)?;
    let backend = rimz::mux::backend_for(mux);

    match args.command {
        PaneSubcmd::List { json, session_name } => list(&*backend, globals, json, session_name),
        PaneSubcmd::Capture {
            pane_id,
            lines,
            json,
            ansi,
        } => capture(&*backend, pane_id, lines, json, ansi),
        PaneSubcmd::Send {
            pane_id,
            enter,
            key,
            text,
        } => {
            let pane = PaneId::parse(&pane_id)?;
            send(backend.as_ref(), &pane, text.as_deref(), &key, enter)
        }
        PaneSubcmd::Focus {
            pane_id,
            session_name,
            pane_process_start,
            ..
        } => focus(&*backend, pane_id, session_name, pane_process_start),
        PaneSubcmd::Split => split(&*backend, globals),
        PaneSubcmd::Detach { session_name } => detach(&*backend, globals, session_name),
    }
}

/// List the room as panes: every pane grouped by its native tab, each labelled
/// with the agent-colleague that lives in it (`@kind#worktree`) or `process` for
/// a plain pane, alongside its status and working directory. Rimz's own sidebar
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
    // Drop Rimz's own sidebar chrome: it is never a routing target, so it has no
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
        let rendered = serde_json::to_string_pretty(&payload)?;
        #[expect(clippy::print_stdout, reason = "json emitter")]
        {
            println!("{rendered}");
        }
        return Ok(());
    }

    let mut table = render::Table::new(["", "AGENT", "STATUS", "CWD", "PANE"]);
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

/// The styled cells for one pane row: focus dot, occupant (agent handle or the
/// literal `process`), status, cwd, and the pane id.
fn pane_row(
    pane: &PaneRef,
    agent: Option<&AgentState>,
    peers: &[&AgentState],
) -> Vec<render::Cell> {
    let focus = if pane.is_focused { "●" } else { "" };
    let occupant_cell = match agent {
        Some(agent) => render::cell(rimz::harness::target::agent_handle(agent, peers, true))
            .fg(render::palette::ACCENT),
        None => render::cell("process").fg(render::palette::MUTED),
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
        render::cell(focus).fg(render::palette::ACCENT),
        occupant_cell,
        status_cell,
        render::cell(cwd).dash(),
        render::cell(pane.pane_id.to_string()).fg(render::palette::META),
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
    focused: bool,
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
        focused: pane.is_focused,
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
    pane_id: String,
    lines: Option<u16>,
    json: bool,
    ansi: bool,
) -> Result<()> {
    let pane = PaneId::parse(&pane_id)?;
    let capture = backend.capture_pane(&pane, lines, ansi)?;
    if json {
        let rendered = serde_json::to_string_pretty(&capture)?;
        #[expect(clippy::print_stdout, reason = "json emitter")]
        {
            println!("{rendered}");
        }
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
    pane_id: String,
    session_name: Option<String>,
    pane_process_start: Option<String>,
) -> Result<()> {
    let pane = PaneId::parse(&pane_id)?;
    validate_pane_not_reused(
        backend,
        &pane,
        session_name.as_deref(),
        pane_process_start.as_deref(),
    )?;
    backend
        .focus_pane(&pane, session_name.as_deref())
        .map_err(Into::into)
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
            target_pane_id: rimz::mux::own_pane_id(backend.name()),
            cwd: Some(workspace.worktree_root.display().to_string()),
            command: None,
            env: crate::cli::agents_launch::launch_identity_env(&workspace, None, true),
            direction,
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
    use rimz::ids::{AgentKind, AgentSessionId, MuxName};

    fn pane(raw: &str, view: &str, name: &str, command: &str, cwd: &str, focused: bool) -> PaneRef {
        PaneRef {
            view_id: Some(view.to_owned()),
            view_name: Some(name.to_owned()),
            is_focused: focused,
            is_floating: false,
            command: Some(command.to_owned()),
            cwd: Some(cwd.to_owned()),
            ..PaneRef::from_id(PaneId::from_parts(MuxName::Zellij, raw))
        }
    }

    fn agent_on(pane_raw: &str, kind: &str, branch: &str) -> AgentState {
        let now = Timestamp::now();
        AgentState {
            agent_id: AgentSessionId::from("sess-1"),
            kind: AgentKind::new_unchecked(kind),
            name: None,
            name_explicit: false,
            kind_ordinal: Some(1),
            profile: None,
            role: None,
            team: None,
            launch_group: None,
            launch_ordinal: None,
            channel: None,
            status: AgentStatus::Running,
            phase: rimz::agents::TurnPhase::Reasoning,
            pane: Some(PaneRef::from_id(PaneId::from_parts(
                MuxName::Zellij,
                pane_raw,
            ))),
            runtime_owner: None,
            parent_agent_id: None,
            worktree_path: Some(format!("/repo/{branch}")),
            worktree_branch: Some(branch.to_owned()),
            task: None,
            prompt: None,
            description: None,
            transcript_path: None,
            origin: None,
            recent_prompts: Vec::new(),
            model: None,
            effort: None,
            context_pct: None,
            context_window: None,
            total_tokens: None,
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
            context: None,
            subagent_description: None,
            subagent_started_at: None,
            turn_started_at: None,
            waiting_since: None,
            compacting_since: None,
            compaction_count: 0,
            last_compact_command_tokens: None,
            last_seen: now,
            last_activity: now,
            registered_at: Some(now),
        }
    }

    #[test]
    fn group_by_tab_buckets_panes_in_first_seen_order() {
        let panes = vec![
            pane("terminal_1", "tab_0", "#auth", "claude", "/repo/auth", true),
            pane("terminal_2", "tab_1", "shell", "zsh", "/repo", false),
            pane("terminal_3", "tab_0", "#auth", "zsh", "/repo/auth", false),
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
        let pane = pane("terminal_1", "tab_0", "#main", "claude", "/repo/main", true);
        let agent = agent_on("terminal_1", "claude", "main");
        let peers: Vec<&AgentState> = vec![&agent];
        let json = pane_json(&pane, Some(&agent), &peers);
        assert_eq!(json.kind, "agent");
        let bound = json.agent.expect("agent bound");
        assert_eq!(bound.handle, "@claude#main");
        assert_eq!(bound.kind, "claude");
        assert_eq!(bound.worktree.as_deref(), Some("main"));
        assert!(json.focused);
        assert_eq!(json.pane_id, "zellij:terminal_1");
    }

    #[test]
    fn pane_json_leaves_a_plain_pane_unannotated() {
        let pane = pane("terminal_2", "tab_1", "shell", "zsh", "/home/x", false);
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
            Vec::new(),
            vec![agent],
            t2,
        );

        // terminal_1 is now a shell whose process started after the agent's last
        // activity: the pane was reused, so nothing binds.
        let reused = PaneRef {
            command: Some("zsh".to_owned()),
            pane_process_start: Some(t2),
            ..pane("terminal_1", "tab_0", "shell", "zsh", "/repo", false)
        };
        assert!(
            snapshot.agent_bound_to_pane(&reused).is_none(),
            "a reused pane carries no agent"
        );

        // The same pane still running codex binds as before.
        let live = pane("terminal_1", "tab_0", "#main", "codex", "/repo/main", true);
        let bound = snapshot
            .agent_bound_to_pane(&live)
            .expect("the live codex pane still binds");
        assert_eq!(bound.kind.as_str(), "codex");
    }
}
