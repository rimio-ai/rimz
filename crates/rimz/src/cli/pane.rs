//! `rimz pane` — the public pane primitives: list, capture, send, focus.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use clap::{Args, Subcommand};

use super::GlobalFlags;
use rimz::ResolvedWorkspace;
use rimz::ids::PaneId;
use rimz::mux::{MuxBackend, PaneListOptions, SplitPaneOptions};
use rimz::workspace::WorkspaceResolver;

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
    /// Send text to a pane as if it were typed.
    Send { pane_id: String, text: String },
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
        PaneSubcmd::Send { pane_id, text } => {
            let pane = PaneId::parse(&pane_id)?;
            send_text(backend.as_ref(), &pane, &text)
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

fn list(
    backend: &dyn MuxBackend,
    globals: &GlobalFlags,
    json: bool,
    session_name: Option<String>,
) -> Result<()> {
    let session_name = resolve_session_name(globals, session_name)?;
    let panes = backend.list_panes(PaneListOptions {
        session_name: Some(session_name),
        ..Default::default()
    })?;
    if json {
        let rendered = serde_json::to_string_pretty(&panes)?;
        #[expect(clippy::print_stdout, reason = "json emitter")]
        {
            println!("{rendered}");
        }
    } else {
        for pane in panes {
            #[expect(clippy::print_stdout, reason = "human listing")]
            {
                println!("{}\t{}", pane.pane_id, pane.session_name);
            }
        }
    }
    Ok(())
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
    validate_pane_not_reused(backend, &pane, session_name, pane_process_start.as_deref())?;
    backend.focus_pane(&pane).map_err(Into::into)
}

fn validate_pane_not_reused(
    backend: &dyn MuxBackend,
    pane: &PaneId,
    session_name: Option<String>,
    expected_start: Option<&str>,
) -> Result<()> {
    let Some(expected_start) = expected_start else {
        return Ok(());
    };
    let panes = backend.list_panes(PaneListOptions {
        session_name,
        ..Default::default()
    })?;
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
    backend
        .split_pane(SplitPaneOptions {
            target_pane_id: None,
            cwd: Some(workspace.worktree_root.display().to_string()),
            command: None,
            env: split_env(&workspace),
        })
        .map_err(Into::into)
}

fn split_env(workspace: &ResolvedWorkspace) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("RIMZ".to_owned(), "1".to_owned()),
        (
            "RIMZ_WORKSPACE_ID".to_owned(),
            workspace.workspace_id.to_string(),
        ),
        (
            "RIMZ_PROJECT_ROOT".to_owned(),
            workspace.project_root.display().to_string(),
        ),
        (
            "RIMZ_WORKTREE_PATH".to_owned(),
            workspace.worktree_root.display().to_string(),
        ),
    ])
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
