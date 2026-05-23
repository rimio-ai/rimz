use std::collections::BTreeMap;

use anyhow::Result;
use clap::{Args, Subcommand};

use super::GlobalFlags;
use rimz::ids::PaneId;
use rimz::mux::{PaneListOptions, SplitPaneOptions};
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
    Focus { pane_id: String },
    /// Split off a new pane in the current view.
    Split,
}

pub fn run(args: PaneArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve(".", globals.root.clone())?;
    let mux = rimz::mux::auto_detect_backend(globals.mux)?;
    let backend = rimz::mux::backend_for(mux);

    match args.command {
        PaneSubcmd::List { json } => {
            let panes = backend.list_panes(PaneListOptions {
                session_name: Some(workspace.session_name.clone()),
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
        PaneSubcmd::Capture {
            pane_id,
            lines,
            json,
            ansi,
        } => {
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
        PaneSubcmd::Send { pane_id, text } => {
            let pane = PaneId::parse(&pane_id)?;
            backend.send_keys(&pane, &text).map_err(Into::into)
        }
        PaneSubcmd::Focus { pane_id } => {
            let pane = PaneId::parse(&pane_id)?;
            backend.focus_pane(&pane).map_err(Into::into)
        }
        PaneSubcmd::Split => {
            let env = BTreeMap::from([
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
            ]);
            backend
                .split_pane(SplitPaneOptions {
                    target_pane_id: None,
                    cwd: Some(workspace.worktree_root.display().to_string()),
                    command: None,
                    env,
                })
                .map_err(Into::into)
        }
    }
}
