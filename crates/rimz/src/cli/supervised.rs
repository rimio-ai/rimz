//! Command-neutral supervised-run effects and presentation.

use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::ValueEnum;

use crate::cli::GlobalFlags;
use rimz::agents::AgentDefinition;
use rimz::harness::run::RunCancellation;
use rimz::store::run::RunRecord;

pub(super) mod output;
pub(super) mod pane;
pub(super) mod run;
pub(super) mod stream;
mod verify;

/// Output projection for a supervised `--print` run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub(in crate::cli) enum OutputFormat {
    /// The run's answer as plain text: every turn of a parked run, in order.
    #[default]
    Text,
    /// The full run record as pretty JSON.
    Json,
    /// Newline-delimited JSON run events (NDJSON).
    StreamJson,
}

pub(in crate::cli) struct SupervisedPresentation {
    pub(in crate::cli) output_format: OutputFormat,
    pub(in crate::cli) stream_text: bool,
    pub(in crate::cli) worktree_fresh_command: Option<String>,
}

static RUN_INTERRUPT_SIGNAL_RECEIVED: OnceLock<RunCancellation> = OnceLock::new();
static RUN_INTERRUPT_HANDLERS_INSTALLED: OnceLock<()> = OnceLock::new();

#[cfg(test)]
use output::RunStreamEvent;
#[cfg(test)]
use pane::{latest_resolved_run_pane, resolve_run_pane_in_snapshot};
#[cfg(test)]
use stream::{stream_attached_run, stream_blocking_run};

pub(crate) fn cancel_supervised_run(store: &rimz::Store, run: &RunRecord) -> Result<()> {
    if !run.status.is_terminal() {
        rimz::harness::run::cancel_and_wake(store, &run.run_id)?;
    }
    Ok(())
}

fn run_interrupt_flag() -> RunCancellation {
    RUN_INTERRUPT_SIGNAL_RECEIVED
        .get_or_init(RunCancellation::new)
        .clone()
}

#[cfg(unix)]
fn install_run_interrupt_handlers(cancellation: RunCancellation) -> Result<()> {
    use signal_hook::consts::signal::SIGINT;

    if RUN_INTERRUPT_HANDLERS_INSTALLED.get().is_some() {
        return Ok(());
    }
    let flag = cancellation.signal_flag();
    signal_hook::flag::register_conditional_shutdown(SIGINT, 130, flag.clone())?;
    signal_hook::flag::register(SIGINT, flag)?;
    let _ = RUN_INTERRUPT_HANDLERS_INSTALLED.set(());
    Ok(())
}

#[cfg(not(unix))]
fn install_run_interrupt_handlers(_cancellation: RunCancellation) -> Result<()> {
    Ok(())
}

pub(super) fn parse_timeout(raw: &str) -> std::result::Result<Duration, String> {
    rimz::utils::time::parse_compact_duration(raw).map_err(|err| err.to_string())
}

/// Extract the prompt from stream-json user messages on stdin. Each non-empty
/// line is one JSON object; `{"type":"user"}` envelopes contribute their
/// `message.content` text (a bare string, or the `text` of each text block),
/// joined with newlines. This is the standard headless stream-json input
/// schema, so the parser is provider-agnostic.
pub(super) fn read_stream_json_prompt<R: std::io::BufRead>(reader: R) -> Result<String> {
    let mut parts: Vec<String> = Vec::new();
    for line in reader.lines() {
        let line = line.context("reading stdin")?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(trimmed)
            .with_context(|| format!("parsing stream-json line `{trimmed}`"))?;
        if value.get("type").and_then(serde_json::Value::as_str) != Some("user") {
            continue;
        }
        match value
            .get("message")
            .and_then(|message| message.get("content"))
        {
            Some(serde_json::Value::String(text)) => parts.push(text.clone()),
            Some(serde_json::Value::Array(blocks)) => {
                for block in blocks {
                    if block.get("type").and_then(serde_json::Value::as_str) == Some("text")
                        && let Some(text) = block.get("text").and_then(serde_json::Value::as_str)
                    {
                        parts.push(text.to_owned());
                    }
                }
            }
            _ => {}
        }
    }
    Ok(parts.join("\n"))
}

#[cfg(test)]
mod tests;
