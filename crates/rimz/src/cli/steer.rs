//! `rimz steer` — send state-gated text to live agent panes.

use anyhow::{Result, bail};
use clap::Args;

use super::send::{self, SendFlags, resolve_message};
use super::{GlobalFlags, current_channel, open_ledger};
use rimz::workspace::WorkspaceResolver;

#[derive(Debug, Args)]
pub struct SteerArgs {
    /// Agent mention: `@codex-2`, `@swift-otter`, `@codex` (every codex), `@all`,
    /// optionally `#worktree`; or a pane id (`tmux:%1`).
    target: String,
    #[command(flatten)]
    send: SendFlags,
    /// Text to type into the agent pane. `\n` is a soft newline; `\\` a literal
    /// backslash. Omit it and pass `--file` to send a file's contents verbatim.
    #[arg(last = true)]
    text: Vec<String>,
}

pub fn run(args: SteerArgs, globals: &GlobalFlags) -> Result<()> {
    rimz::target::require_mention(&args.target)?;
    let SendFlags {
        worktree,
        no_enter,
        force,
        all,
        create,
        yes,
        smart_compact,
        file,
        no_from,
    } = args.send;
    let auto_compact = smart_compact.or_else(|| super::machine_config().harness.smart_compact);
    let text = resolve_message(&args.text, file.as_deref())?;
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let ledger = open_ledger(&workspace)?;
    let mut snapshot = super::resolution_snapshot(&workspace, &ledger, globals)?;
    // `--smart-compact` reads context fill, which the resolution snapshot does not
    // carry; fold the disposable context sidecars in for the freshest gauge.
    if auto_compact.is_some()
        && let Ok(runtime) = rimz::RuntimePaths::for_workspace(workspace.workspace_id.clone())
    {
        snapshot = snapshot.with_agent_context(rimz::ledger::agent_context::read_all(&runtime));
    }
    let channel = current_channel(&workspace);
    let sender = send::sender_from_env(channel.as_deref(), no_from);
    let targets = match super::resolve_pane_targets(
        &snapshot,
        &args.target,
        worktree.as_deref(),
        channel.as_deref(),
    ) {
        Ok(targets) => targets,
        // Create-on-miss: a kind/profile address with --create launches a fresh
        // agent with this text as its first prompt, so no separate steer follows.
        Err(_) if create => {
            return super::agents_cmd::create_on_miss(
                &args.target,
                worktree.as_deref(),
                channel.as_deref(),
                &text,
                globals,
            );
        }
        Err(err) => return Err(err),
    };

    if targets.len() > 1 {
        let labels: Vec<String> = targets.iter().map(|target| target.label()).collect();
        if !all && !rimz::target::is_broadcast(&args.target) {
            return Err(super::ambiguous_fanout("steer", &args.target, &labels));
        }
        if !yes {
            super::confirm_fanout("Steer", &args.target, &labels)?;
        }
    }

    let mut live_send = send::LiveSend {
        force,
        enter: !no_enter,
        auto_compact,
        sender,
        pacer: send::Pacer::new(rimz::message::message_interval_from_env()),
    };
    let mut outcomes = Vec::with_capacity(targets.len());
    for target in &targets {
        outcomes.push(send::send_to_live_pane(
            &workspace,
            &ledger,
            &snapshot,
            target,
            send::bound_agent(&snapshot, target),
            &text,
            &mut live_send,
        )?);
    }

    report(&args.target, targets.len(), &outcomes)
}

/// Report a fan-out. A lone agent that was skipped fails with the same message
/// the single-target path always returned; a broadcast always prints its
/// sent/skipped summary and succeeds — a blocked agent never aborts the rest.
fn report(target: &str, total: usize, outcomes: &[send::Outcome]) -> Result<()> {
    let labels = |pick: fn(&send::Outcome) -> Option<&str>| -> Vec<&str> {
        outcomes.iter().filter_map(pick).collect()
    };
    let sent = labels(|outcome| match outcome {
        send::Outcome::Sent { label, .. } => Some(label.as_str()),
        _ => None,
    });
    let compacted = labels(|outcome| match outcome {
        send::Outcome::Sent {
            label,
            compacted: true,
        } => Some(label.as_str()),
        _ => None,
    });
    if total == 1 {
        if !sent.is_empty() {
            // A single steer stays quiet on success, but a compaction it ran on
            // the user's behalf is reported so the extra turn is never silent.
            if let Some(label) = compacted.first() {
                #[expect(clippy::print_stdout, reason = "steer compaction notice")]
                {
                    println!("compacted {label}, then steered");
                }
            }
            return Ok(());
        }
        match outcomes.first() {
            Some(send::Outcome::SkippedPending { label, request_id }) => {
                bail!("{label} has pending ask {request_id}; resolve it or pass --force")
            }
            _ => bail!("no agent matches `{target}`"),
        }
    }
    let pending = labels(|outcome| match outcome {
        send::Outcome::SkippedPending { label, .. } => Some(label.as_str()),
        _ => None,
    });
    let mut line = format!("steered {} agent(s)", sent.len());
    if !sent.is_empty() {
        line.push_str(&format!(": {}", sent.join(", ")));
    }
    if !compacted.is_empty() {
        line.push_str(&format!("; compacted first: {}", compacted.join(", ")));
    }
    if !pending.is_empty() {
        line.push_str(&format!("; skipped pending ask: {}", pending.join(", ")));
    }
    #[expect(clippy::print_stdout, reason = "steer fan-out summary")]
    {
        println!("{line}");
    }
    Ok(())
}
