//! Statusline datasource. A provider's managed statusline command invokes
//! `rimz statusline feed`: it captures the rich JSON on stdin, persists the
//! per-session agent-context sidecar, then passes the JSON unchanged to any
//! wrapped user command and forwards its stdout + exit code so the user's
//! statusline renders exactly as before.
//!
//! This path is deliberately store-free — it runs on every statusline render.
//! It resolves only the workspace runtime dir, takes the session sidecar's
//! short advisory lock, writes one atomic file, and (when wrapping) spawns one
//! child. It never blocks on the workspace lock and never opens the event log.
//!
//! Stdio discipline: stdout is reserved for the wrapped command's output (what
//! Claude renders); diagnostics go to stderr via `tracing`. The wrapped child's
//! stdio is fully piped — never the inherited variant (the hook-stdout CI
//! invariant) — so its stderr can't leak onto the statusline and the parent
//! stays in control.
//!
//! No pane stamping: the sidecar keys on the provider's session/conversation
//! id, and the lifecycle-event `AgentState` already owns the pane binding.

use std::io::{self, Read, Write};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use serde_json::Value;
use tracing::warn;

use super::GlobalFlags;
use rimz::RuntimePaths;
use rimz::agents::{
    AgentAdapter, AgentContext, PriceBook, StatusLineInvocation, adapter_by_kind, pricing,
};
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
        /// Agent the statusline belongs to (`claude`, `cursor`, `qwen`, `antigravity`).
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
    let (wrapped, invocation) = adapter_by_kind(&source)
        .ok()
        .map(|agent| {
            let wrapped = if subagent {
                agent.wrapped_subagent_status_line_command()
            } else {
                agent.wrapped_status_line_command()
            };
            (wrapped, agent.status_line_invocation())
        })
        .unwrap_or((None, StatusLineInvocation::Shell));

    // Best-effort context capture. Never fatal, never blocks on the store.
    let persisted = if subagent {
        persist_subagent_context(&source, &buf, globals)
    } else {
        persist_context(&source, &buf, globals)
    };
    if let Err(err) = persisted {
        warn!(source = %source, subagent, error = %err, "statusline: context capture failed");
    }

    // Pass-through. Always emit something so the statusline never blanks. With
    // no wrapped command we print nothing, so an agent configured to stack its
    // built-in line keeps that line (or, for `--subagent`, its own child rows).
    match wrapped {
        Some(command) => forward_to_wrapped(&command, invocation, &buf),
        None => Ok(()),
    }
}

/// Parse the payload, normalize it via the agent adapter, and write the sidecar.
/// Resolves only `RuntimePaths` (no store open or workspace lock) to stay fast
/// on the per-render path.
fn persist_context(source: &str, stdin: &[u8], globals: &GlobalFlags) -> Result<()> {
    let payload: Value = serde_json::from_slice(stdin).context("parsing statusline payload")?;
    let session_id =
        payload_session_id(&payload).context("statusline payload carries no session id")?;
    let agent = adapter_by_kind(source)?;
    let Some(mut context) = agent.observe_context(source, &payload) else {
        // The adapter has no rich-context source (e.g. codex): nothing to store.
        return Ok(());
    };
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let runtime =
        RuntimePaths::for_workspace(workspace.workspace_id).context("preparing runtime paths")?;
    runtime.ensure_dirs().context("preparing runtime dirs")?;
    let prices = pricing::cached_book(&runtime.shared_pricing_cache_path());
    attach_context_cost(agent, &payload, &prices, &mut context);
    rimz::store::agent_context::write(&runtime, agent.descriptor().kind, session_id, &context)
        .context("writing agent-context sidecar")?;
    // Push the update so the `$`/token figure repaints within a wakeup rather
    // than waiting for the sidebar's next poll tick. Best-effort, like every
    // other wakeup: a send failure never fails the statusline render.
    let _ = rimz::store::wakeup::wake_sidebars(&runtime);
    Ok(())
}

fn attach_context_cost(
    agent: &dyn AgentAdapter,
    payload: &Value,
    prices: &PriceBook,
    context: &mut AgentContext,
) {
    if let Some(cost) = agent.estimate_context_cost(payload, prices) {
        context.cost = Some(cost);
    }
}

/// Parse a `subagentStatusLine` payload, harvest its `tasks`, and write one
/// per-child sidecar keyed by `(kind, task id)` — the same id the child's
/// `SubagentStart` lifecycle row is keyed under, so the snapshot fold attaches
/// it. Like [`persist_context`] this resolves only `RuntimePaths` (no store or
/// workspace lock) to stay fast on the per-render path. Nothing to
/// persist (a non-Claude source, or a payload with no attributable task) is
/// success, not an error.
fn persist_subagent_context(source: &str, stdin: &[u8], globals: &GlobalFlags) -> Result<()> {
    let payload: Value =
        serde_json::from_slice(stdin).context("parsing subagent statusline payload")?;
    let agent = adapter_by_kind(source)?;
    let observations = agent.observe_subagent_context(&payload);
    if observations.is_empty() {
        return Ok(());
    }
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let runtime =
        RuntimePaths::for_workspace(workspace.workspace_id).context("preparing runtime paths")?;
    runtime.ensure_dirs().context("preparing runtime dirs")?;
    for observation in &observations {
        rimz::store::subagent_context::write(
            &runtime,
            agent.descriptor().kind,
            &observation.agent_id,
            &observation.context,
        )
        .context("writing subagent-context sidecar")?;
    }
    // Repaint the parent's expanded card within a wakeup rather than on the next
    // poll tick. Best-effort, like every other wakeup.
    let _ = rimz::store::wakeup::wake_sidebars(&runtime);
    Ok(())
}

/// Session id from the statusline payload, matching the lifecycle key so the
/// sidecar files under the same session. Antigravity spells it
/// `conversation_id`; hook payloads use a distinct camel-case spelling.
fn payload_session_id(payload: &Value) -> Option<&str> {
    [
        "session_id",
        "agent_id",
        "conversation_id",
        "conversationId",
    ]
    .into_iter()
    .find_map(|key| {
        payload
            .get(key)
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
    })
}

/// Spawn the wrapped command with the provider's invocation semantics, feed it
/// the captured JSON on stdin, and forward its stdout plus exit code. Claude
/// uses `sh -c`; Cursor splits direct argv. The command is the user's
/// pre-existing one, so this adds no new trust surface.
fn forward_to_wrapped(
    command: &str,
    invocation: StatusLineInvocation,
    payload: &[u8],
) -> Result<()> {
    let mut command_builder = match invocation {
        StatusLineInvocation::Shell => {
            let mut shell = Command::new("sh");
            shell.arg("-c").arg(command);
            shell
        }
        StatusLineInvocation::DirectArgv => direct_command(command)?,
    };
    let mut child = command_builder
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

fn direct_command(command: &str) -> Result<Command> {
    let argv = direct_argv(command, std::env::var_os("HOME").as_deref())?;
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]);
    Ok(command)
}

fn direct_argv(command: &str, home: Option<&std::ffi::OsStr>) -> Result<Vec<String>> {
    let mut argv = shlex::split(command)
        .with_context(|| format!("parsing wrapped statusline command `{command}`"))?;
    let Some(program) = argv.first_mut() else {
        anyhow::bail!("wrapped statusline command is empty");
    };
    if program == "~" {
        let home = home.context("HOME is not set; cannot expand wrapped statusline program `~`")?;
        *program = home.to_string_lossy().into_owned();
    } else if let Some(rest) = program.strip_prefix("~/") {
        let home = home.context("HOME is not set; cannot expand wrapped statusline program")?;
        *program = std::path::Path::new(home)
            .join(rest)
            .to_string_lossy()
            .into_owned();
    }
    Ok(argv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::agents::{AgentCost, AntigravityAdapter};
    use serde_json::json;

    #[test]
    fn statusline_session_identity_accepts_antigravity_conversations() {
        assert_eq!(
            payload_session_id(&json!({"conversation_id": "agy-session"})),
            Some("agy-session")
        );
        assert_eq!(
            payload_session_id(&json!({"conversationId": "agy-hook-spelling"})),
            Some("agy-hook-spelling")
        );
    }

    #[test]
    fn context_cost_estimate_attaches_only_when_the_payload_is_priceable() {
        let prices = PriceBook::from_litellm_json(
            r#"{"gemini-3.5-flash": {"input_cost_per_token": 1.5e-6, "output_cost_per_token": 9e-6, "cache_read_input_token_cost": 0.15e-6}}"#,
        );
        let priced = json!({
            "model": {"id": "Gemini 3.5 Flash (Medium)"},
            "context_window": {"current_usage": {
                "input_tokens": 2_971,
                "output_tokens": 630,
                "cache_read_input_tokens": 16_270
            }}
        });
        let mut context = AntigravityAdapter
            .observe_context("antigravity", &priced)
            .unwrap();
        attach_context_cost(&AntigravityAdapter, &priced, &prices, &mut context);
        let cost = context.cost.unwrap();
        assert_eq!(cost.basis, rimz::agents::CostBasis::DisplayEstimate);
        assert!((cost.total_cost_usd.unwrap() - 0.012_567).abs() < 1e-15);
        assert_eq!(
            cost,
            AgentCost {
                total_cost_usd: cost.total_cost_usd,
                basis: rimz::agents::CostBasis::DisplayEstimate,
                ..AgentCost::default()
            }
        );

        let unknown = json!({
            "model": {"id": "unknown"},
            "context_window": {"current_usage": {"input_tokens": 10}}
        });
        let mut context = AntigravityAdapter
            .observe_context("antigravity", &unknown)
            .unwrap();
        attach_context_cost(&AntigravityAdapter, &unknown, &prices, &mut context);
        assert!(context.cost.is_none());
    }

    #[test]
    fn direct_argv_preserves_quotes_spaces_and_shell_metacharacters() {
        assert_eq!(
            direct_argv(
                r#""/tmp/status line" --label "a b" ';' '$HOME'"#,
                Some(std::ffi::OsStr::new("/home/user")),
            )
            .unwrap(),
            ["/tmp/status line", "--label", "a b", ";", "$HOME"]
        );
    }

    #[test]
    fn direct_argv_rejects_empty_and_malformed_commands() {
        assert!(direct_argv("", None).is_err());
        assert!(direct_argv("\"unterminated", None).is_err());
    }

    #[test]
    fn direct_argv_expands_only_a_leading_program_tilde() {
        let home = std::ffi::OsStr::new("/home/user space");
        assert_eq!(
            direct_argv("~/bin/statusline '~/literal arg'", Some(home)).unwrap(),
            ["/home/user space/bin/statusline", "~/literal arg"]
        );
    }
}
