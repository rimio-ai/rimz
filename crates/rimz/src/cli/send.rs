//! Shared surface for `rimz steer` and `rimz queue`: the message text and the
//! flags both carry. The two commands mirror each other — steer sends now, queue
//! sends at a turn boundary — so the address filter, the fan-out controls, the
//! Enter/no-Enter choice, the pending-ask override, and the compact-first
//! threshold live here once and flatten into both. The delivery-timing gate
//! (`--on`) is the one knob unique to sending later, so it stays on `queue`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args;

use rimz::ids::AgentKind;
use rimz::message::{AutoCompact, MessageSender};

/// The flags `steer` and `queue` share, flattened into each command's args.
#[derive(Debug, Args)]
pub(crate) struct SendFlags {
    /// Restrict matches to one worktree branch, name, or path (the channel).
    #[arg(long)]
    pub(crate) worktree: Option<String>,
    /// Type the text but leave it unsubmitted — no Enter after it lands.
    #[arg(long)]
    pub(crate) no_enter: bool,
    /// Send even when a pending ask is attached to the agent.
    #[arg(long)]
    pub(crate) force: bool,
    /// Fan out to every agent the address matches. Without it, a selector that
    /// matches more than one agent is an error that lists the handles to pick one.
    #[arg(long)]
    pub(crate) all: bool,
    /// Launch the agent if the address matches none: a kind (`@codex`) or a profile
    /// (`@planner`) opens a fresh agent in the channel with this text as its first
    /// prompt. An instance handle (pet name, ordinal) cannot create.
    #[arg(long)]
    pub(crate) create: bool,
    /// Skip the fan-out confirmation prompt when broadcasting (`@all` or --all).
    #[arg(long, short = 'y')]
    pub(crate) yes: bool,
    /// Use Rimz's smart compact-first send when the agent's context window is at
    /// least this full: a percentage (`70%`) or an occupied-token count
    /// (`120000`). Defaults from `[harness] smart_auto_compact` when omitted.
    #[arg(long, value_name = "PCT|TOKENS", value_parser = AutoCompact::parse)]
    pub(crate) smart_auto_compact: Option<AutoCompact>,
    /// Read the prompt verbatim from a file instead of the `-- text` argv. A file
    /// already carries real newlines and literal backslashes, so it is sent as-is
    /// with no `\n`/`\\` interpretation. Conflicts with inline `-- text`.
    #[arg(long, value_name = "PATH")]
    pub(crate) file: Option<PathBuf>,
    /// Deliver the text verbatim with no `@sender:` prefix, even for an agent
    /// caller. No effect for a human caller, which is already verbatim.
    #[arg(long)]
    pub(crate) no_from: bool,
}

/// Resolve the prompt a `steer`/`queue` invocation carries from its two sources:
/// the trailing `-- text…` argv, or a `--file` path. Exactly one applies — a file
/// is read verbatim (it already holds real newlines and literal backslashes),
/// while inline argv goes through `\n`/`\\` interpretation.
pub(crate) fn resolve_message(parts: &[String], file: Option<&Path>) -> Result<String> {
    match file {
        Some(path) => {
            if !parts.is_empty() {
                bail!("pass a prompt as `-- text` or `--file`, not both");
            }
            read_prompt_file(path)
        }
        None => message_text(parts),
    }
}

/// The caller identity for `steer`/`queue`. Rimz-launched agents carry
/// `RIMZ_AGENT_KIND`; ordinary room shells carry `RIMZ` identity vars without it,
/// so they stay human-authored unless an agent kind is present.
pub(crate) fn sender_from_env(channel: Option<&str>, no_from: bool) -> MessageSender {
    if no_from {
        return MessageSender::Human;
    }
    let Some(kind) = env_string(rimz::run::ENV_AGENT_KIND) else {
        return MessageSender::Human;
    };
    MessageSender::Agent {
        kind: AgentKind::new_unchecked(kind),
        name: env_string(rimz::run::ENV_AGENT_NAME),
        profile: env_string(rimz::run::ENV_AGENT_PROFILE),
        role: env_string(rimz::run::ENV_AGENT_ROLE),
        channel: channel.map(ToOwned::to_owned),
    }
}

fn env_string(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

/// Read a prompt file as-is, trimming only the trailing newline an editor adds so
/// it never lands as a blank composer line before the submit.
fn read_prompt_file(path: &Path) -> Result<String> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading prompt file `{}`", path.display()))?;
    let text = raw.trim_end_matches(['\r', '\n']);
    if text.is_empty() {
        bail!("prompt file `{}` is empty", path.display());
    }
    Ok(text.to_owned())
}

/// Join `-- text…` argv into one message, interpreting `\n` as a soft newline and
/// `\\` as a literal backslash, so a multi-line prompt can be typed inline. The
/// bracketed-paste send path delivers each `\n` as a composer line break rather
/// than a submit. Every other escape keeps its backslash, so a regex or a
/// Windows path in a prompt (`\d+`, `C:\tmp`) survives untouched.
fn message_text(parts: &[String]) -> Result<String> {
    let text = unescape(&parts.join(" "));
    if text.is_empty() {
        bail!("expected non-empty text");
    }
    Ok(text)
}

fn unescape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('\\') => out.push('\\'),
            // Keep unknown escapes verbatim so prose, regexes, and paths survive.
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            // A trailing lone backslash stays literal.
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_argv_with_spaces() {
        let parts = ["fix".to_owned(), "the".to_owned(), "parser".to_owned()];
        assert_eq!(message_text(&parts).unwrap(), "fix the parser");
    }

    #[test]
    fn interprets_a_newline_escape() {
        assert_eq!(
            message_text(&["first\\nsecond".to_owned()]).unwrap(),
            "first\nsecond"
        );
    }

    #[test]
    fn keeps_unknown_escapes_literal() {
        // Only `\n` and `\\` are special; a regex or path in a prompt is untouched.
        let raw = r"match \d+ then open C:\tmp";
        assert_eq!(message_text(&[raw.to_owned()]).unwrap(), raw);
    }

    #[test]
    fn an_escaped_backslash_yields_a_literal_backslash_n() {
        // `\\n` is how a prompt asks for a literal backslash-n, not a newline.
        assert_eq!(message_text(&[r"a\\nb".to_owned()]).unwrap(), r"a\nb");
    }

    #[test]
    fn rejects_empty_text() {
        assert!(message_text(&[]).is_err());
        assert!(message_text(&[String::new()]).is_err());
    }

    #[test]
    fn resolve_takes_inline_text_when_no_file() {
        let parts = ["fix".to_owned(), "the".to_owned(), "parser".to_owned()];
        assert_eq!(resolve_message(&parts, None).unwrap(), "fix the parser");
    }

    #[test]
    fn resolve_rejects_text_and_file_together() {
        // A conflict fails before the path is touched, so the bogus path is safe.
        let err = resolve_message(&["hi".to_owned()], Some(Path::new("/nope")))
            .expect_err("text and file conflict");
        assert!(err.to_string().contains("not both"), "{err}");
    }

    #[test]
    fn resolve_rejects_no_source() {
        assert!(resolve_message(&[], None).is_err());
    }
}
