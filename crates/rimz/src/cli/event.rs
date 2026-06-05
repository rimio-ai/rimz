//! `rimz event` — emit events into the workspace ledger.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use serde_json::{Value, json};

use super::{GlobalFlags, open_ledger};
use rimz::EventEnvelope;
use rimz::workspace::WorkspaceResolver;

#[derive(Debug, Args)]
pub struct EventArgs {
    #[command(subcommand)]
    command: EventSubcmd,
}

#[derive(Debug, Subcommand)]
enum EventSubcmd {
    /// Emit a workspace event into the ledger.
    Emit {
        /// Kind tag (free-form; agent integrations prefer `<source>.<verb>`).
        #[arg(long)]
        kind: String,
        /// One-line summary.
        #[arg(long)]
        title: Option<String>,
        /// Optional body text.
        #[arg(long)]
        body: Option<String>,
        /// Optional structured payload as a JSON literal.
        #[arg(long)]
        json: Option<String>,
    },
}

pub fn run(args: EventArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        EventSubcmd::Emit {
            kind,
            title,
            body,
            json: payload,
        } => {
            let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
            let ledger = open_ledger(&workspace)?;
            let payload: Value = match payload {
                Some(raw) => serde_json::from_str(&raw).context("parsing --json")?,
                None => Value::Null,
            };
            let event = EventEnvelope::new(
                workspace.workspace_id.clone(),
                workspace.session_name.clone(),
                "rimz",
                "cli",
                "event.emit",
                json!({ "kind": kind, "title": title, "body": body, "payload": payload }),
            );
            ledger.append_event(&event)?;
            #[expect(clippy::print_stdout, reason = "command result is event id")]
            {
                println!("{}", event.event_id);
            }
            Ok(())
        }
    }
}
