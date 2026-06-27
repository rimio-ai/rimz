//! `rimz message` — the agent-comms front door: queue text for a teammate, sent
//! now when they can receive it and parked for their next turn boundary otherwise.

use anyhow::Result;
use clap::Args;

use super::GlobalFlags;
use super::queue::{self, parse_gate};
use super::send::SendFlags;
use rimz::message::DeliveryGate;

#[derive(Debug, Args)]
pub struct MessageArgs {
    /// Agent mention: `@codex-2`, `@swift-otter`, `@codex` (every codex), `@all`,
    /// optionally `#worktree`.
    target: String,
    /// The message, as one quoted argument. `\n` is a soft newline; `\\` a literal
    /// backslash. A message that starts with `-` needs a `--` before it. Place a
    /// value-optional flag (`--wait`) after the message or it captures it. Omit the
    /// message and pass `--file` to deliver a file's contents verbatim.
    text: Option<String>,
    /// Deliver after a successful/idle turn (`done`) or after success/idle/failure (`any`).
    #[arg(long, value_parser = parse_gate, default_value = "done")]
    on: DeliveryGate,
    #[command(flatten)]
    send: SendFlags,
}

pub fn run(args: MessageArgs, globals: &GlobalFlags) -> Result<()> {
    let text: Vec<String> = args.text.into_iter().collect();
    queue::queue_add(args.target, args.on, args.send, text, globals)
}
