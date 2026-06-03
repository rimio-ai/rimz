//! Statusline datasource. Claude's `statusLine` command `exec`s into
//! `rimz statusline feed`: it captures the rich JSON Claude pipes on stdin,
//! persists the per-session agent-context sidecar, then passes the JSON
//! unchanged to any wrapped user command and forwards its stdout + exit code so
//! the user's statusline renders exactly as before.
//!
//! This path is deliberately ledger-free and lock-free — it runs on every
//! statusline render. It resolves only the workspace runtime dir, writes one
//! atomic sidecar file, and (when wrapping) spawns one child. It never blocks
//! on the workspace lock and never opens the event log.
//!
//! Stdio discipline: stdout is reserved for the wrapped command's output (what
//! Claude renders); diagnostics go to stderr via `tracing`. The wrapped child's
//! stdio is fully piped — never the inherited variant (the hook-stdout CI
//! invariant) — so its stderr can't leak onto the statusline and the parent
//! stays in control.
//!
//! No pane stamping: the sidecar keys on the payload `session_id`, and the
//! lifecycle-event `AgentState` already owns the pane binding.

use std::io::{self, Read, Write};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use serde_json::Value;
use tracing::warn;

use super::GlobalFlags;
use rimz::RuntimePaths;
use rimz::agents::integration_by_name;
use rimz::workspace::WorkspaceResolver;

#[derive(Debug, Args)]
pub struct StatuslineArgs {
    #[command(subcommand)]
    command: StatuslineSubcmd,
}

#[derive(Debug, Subcommand)]
enum StatuslineSubcmd {
    /// Capture the statusline JSON on stdin, persist the agent-context sidecar,
    /// then pass the JSON through to the wrapped command and forward its output.
    /// With `--subagent` the same datasource serves Claude's `subagentStatusLine`:
    /// the payload's `tasks` array is harvested into one per-child sidecar.
    #[command(hide = true)]
    Feed {
        /// Agent the statusline belongs to (`claude`).
        #[arg(long)]
        source: String,
        /// Treat the payload as a `subagentStatusLine` render (a `tasks` array)
        /// rather than the session `statusLine` blob.
        #[arg(long)]
        subagent: bool,
    },
}

pub fn run(args: StatuslineArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        StatuslineSubcmd::Feed { source, subagent } => run_feed(source, subagent, globals),
    }
}

fn run_feed(source: String, subagent: bool, globals: &GlobalFlags) -> Result<()> {
    // Read all of stdin once — the rich statusline JSON. Keep the bytes for a
    // verbatim pass-through; a parse failure must never blank the statusline.
    let mut buf = Vec::new();
    io::stdin()
        .read_to_end(&mut buf)
        .context("reading statusline stdin")?;

    // Resolve the pass-through target before any fallible payload work, so a
    // parse error can't strand the user's statusline. The two render commands
    // wrap independently, so each mode reads its own wrapped target.
    let wrapped = integration_by_name(&source).ok().and_then(|agent| {
        if subagent {
            agent.wrapped_subagent_status_line_command()
        } else {
            agent.wrapped_status_line_command()
        }
    });

    // Best-effort context capture. Never fatal, never blocks on the ledger.
    let persisted = if subagent {
        persist_subagent_context(&source, &buf, globals)
    } else {
        persist_context(&source, &buf, globals)
    };
    if let Err(err) = persisted {
        warn!(source = %source, subagent, error = %err, "statusline: context capture failed");
    }

    // Pass-through. Always emit something so the statusline never blanks. With
    // no wrapped command we print nothing, so Claude renders its built-in line
    // (or, for `--subagent`, its own child rows).
    match wrapped {
        Some(command) => forward_to_wrapped(&command, &buf),
        None => Ok(()),
    }
}

/// Parse the payload, normalize it via the agent adapter, and write the sidecar.
/// Resolves only `RuntimePaths` (no ledger open/lock) to stay fast and
/// lock-free on the per-render path.
fn persist_context(source: &str, stdin: &[u8], globals: &GlobalFlags) -> Result<()> {
    let payload: Value = serde_json::from_slice(stdin).context("parsing statusline payload")?;
    let session_id =
        payload_session_id(&payload).context("statusline payload carries no session id")?;
    let agent = integration_by_name(source)?;
    let Some(context) = agent.observe_context(source, &payload) else {
        // The adapter has no rich-context source (e.g. codex): nothing to store.
        return Ok(());
    };
    let workspace = WorkspaceResolver::resolve(".", globals.root.clone())?;
    let workspace_id = workspace.workspace_id.clone();
    let runtime =
        RuntimePaths::for_workspace(workspace.workspace_id).context("preparing runtime paths")?;
    runtime.ensure_dirs().context("preparing runtime dirs")?;
    rimz::ledger::agent_context::write(&runtime, agent.name(), session_id, &context)
        .context("writing agent-context sidecar")?;
    // Push the update so the `$`/token figure repaints within a wakeup rather
    // than waiting for the sidebar's next poll tick. Best-effort, like every
    // other wakeup: a send failure never fails the statusline render.
    let _ = rimz::ledger::wakeup::wake_sidebars_for_context(&runtime, &workspace_id);
    Ok(())
}

/// Parse a `subagentStatusLine` payload, harvest its `tasks`, and write one
/// per-child sidecar keyed by `(kind, task id)` — the same id the child's
/// `SubagentStart` lifecycle row is keyed under, so the snapshot fold attaches
/// it. Like [`persist_context`] this resolves only `RuntimePaths` (no ledger
/// open/lock) to stay fast and lock-free on the per-render path. Nothing to
/// persist (a non-Claude source, or a payload with no attributable task) is
/// success, not an error.
fn persist_subagent_context(source: &str, stdin: &[u8], globals: &GlobalFlags) -> Result<()> {
    let payload: Value =
        serde_json::from_slice(stdin).context("parsing subagent statusline payload")?;
    let agent = integration_by_name(source)?;
    let observations = agent.observe_subagent_context(&payload);
    if observations.is_empty() {
        return Ok(());
    }
    let workspace = WorkspaceResolver::resolve(".", globals.root.clone())?;
    let workspace_id = workspace.workspace_id.clone();
    let runtime =
        RuntimePaths::for_workspace(workspace.workspace_id).context("preparing runtime paths")?;
    runtime.ensure_dirs().context("preparing runtime dirs")?;
    for observation in &observations {
        rimz::ledger::subagent_context::write(
            &runtime,
            agent.name(),
            &observation.agent_id,
            &observation.context,
        )
        .context("writing subagent-context sidecar")?;
    }
    // Repaint the parent's expanded card within a wakeup rather than on the next
    // poll tick. Best-effort, like every other wakeup.
    let _ = rimz::ledger::wakeup::wake_sidebars_for_context(&runtime, &workspace_id);
    Ok(())
}

/// Session id from the statusline payload (`session_id`, then `agent_id`),
/// matching the lifecycle key so the sidecar files under the same session.
fn payload_session_id(payload: &Value) -> Option<&str> {
    ["session_id", "agent_id"].into_iter().find_map(|key| {
        payload
            .get(key)
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
    })
}

/// Spawn the wrapped command under a shell, feed it the captured JSON on stdin,
/// and forward its stdout verbatim (what Claude renders) plus its exit code.
/// `sh -c` reproduces Claude's own statusline invocation faithfully; the
/// command is the user's pre-existing one, so this adds no new trust surface.
fn forward_to_wrapped(command: &str, payload: &[u8]) -> Result<()> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        // Fully piped — never `inherit`. The child's stdout is the only thing
        // Claude renders, so we capture and forward it deliberately; its stderr
        // is diagnostics, routed to our stderr, never onto the statusline.
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning wrapped statusline command `{command}`"))?;

    if let Some(mut stdin) = child.stdin.take() {
        let payload = payload.to_vec();
        // Write on a thread so a large payload can't deadlock against a child
        // that writes to stdout before draining its stdin.
        std::thread::spawn(move || {
            let _ = stdin.write_all(&payload);
        });
    }

    let output = child
        .wait_with_output()
        .context("waiting on wrapped statusline command")?;

    {
        let mut stdout = io::stdout().lock();
        stdout
            .write_all(&output.stdout)
            .context("forwarding wrapped statusline stdout")?;
        stdout.flush().ok();
    }
    // The child's stderr is diagnostics — route it to our stderr, never stdout.
    let _ = io::stderr().lock().write_all(&output.stderr);

    // Forward the child's exit code so a failing statusline surfaces as it would
    // without Rimz in the middle.
    if let Some(code) = output.status.code()
        && code != 0
    {
        std::process::exit(code);
    }
    Ok(())
}
