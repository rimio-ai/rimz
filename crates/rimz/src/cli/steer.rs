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
        wait,
    } = args.send;
    let wait = send::wait_duration(wait);
    send::validate_wait(!no_enter, wait)?;
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
        pacer: send::Pacer::new(rimz::message::message_interval_from_env()),
    };
    let mut outcomes = Vec::with_capacity(targets.len());
    for target in &targets {
        let bound = send::bound_agent(&snapshot, target);
        let message = send::message_for_target(
            workspace.workspace_id.clone(),
            target,
            bound,
            text.clone(),
            !no_enter,
            rimz::message::DeliveryGate::Any,
            sender.clone(),
            force,
            auto_compact,
        );
        let outcome = match send::send_to_live_pane(
            &workspace,
            &ledger,
            &snapshot,
            target,
            bound,
            &message,
            &mut live_send,
        ) {
            Ok(outcome) => outcome,
            Err(err) => {
                ledger.record_send_error(&message, &err.to_string(), &workspace.session_name)?;
                return Err(err);
            }
        };
        outcomes.push(outcome);
    }

    report(
        &ledger,
        &workspace.session_name,
        wait,
        &args.target,
        targets.len(),
        &outcomes,
    )
}

/// Report a fan-out. A lone agent that was skipped fails with the same message
/// the single-target path always returned; a broadcast always prints its
/// sent/skipped summary and succeeds — a blocked agent never aborts the rest.
fn report(
    ledger: &rimz::Ledger,
    session_name: &str,
    wait: Option<std::time::Duration>,
    target: &str,
    total: usize,
    outcomes: &[send::Outcome],
) -> Result<()> {
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
            ..
        } => Some(label.as_str()),
        _ => None,
    });
    if let Some(timeout) = wait {
        let mut failed = false;
        for outcome in outcomes {
            if let send::Outcome::Sent {
                label, message_id, ..
            } = outcome
            {
                let status = send::wait_for_message(ledger, message_id, session_name, timeout)?;
                if status != rimz::message::MessageStatus::Delivered {
                    failed = true;
                }
                #[expect(clippy::print_stdout, reason = "wait status")]
                {
                    println!("{} {label}", wait_status_label(status));
                }
            }
        }
        if failed {
            std::process::exit(1);
        }
        return Ok(());
    }
    if total == 1 {
        if !sent.is_empty() {
            let label = sent[0];
            let suffix = if compacted.first() == Some(&label) {
                " (compacted first)"
            } else {
                ""
            };
            #[expect(clippy::print_stdout, reason = "steer confirmation")]
            {
                println!("sent {label}{suffix}");
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
    let mut line = format!("sent {} agent(s)", sent.len());
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

fn wait_status_label(status: rimz::message::MessageStatus) -> &'static str {
    match status {
        rimz::message::MessageStatus::Delivered => "delivered",
        rimz::message::MessageStatus::Errored => "errored",
        rimz::message::MessageStatus::TimedOut => "timed out",
        rimz::message::MessageStatus::Removed => "removed",
        rimz::message::MessageStatus::Abandoned => "abandoned",
        rimz::message::MessageStatus::Created
        | rimz::message::MessageStatus::Queued
        | rimz::message::MessageStatus::Claimed
        | rimz::message::MessageStatus::Sent => "timed out",
    }
}
