//! Command-side send flags, prompt sources shared by `rimz message` and
//! supervised runs, and sender attribution.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Args;

use rimz::ids::AgentKind;
pub(crate) use rimz::message::send::wait_for_message_until;
use rimz::message::{AutoCompact, MessageSender, delivery_window_from_env};

/// The flags shared by immediate and parked message delivery.
#[derive(Debug, Args)]
pub(crate) struct SendFlags {
    /// Restrict matches to one worktree name or path.
    #[arg(long, conflicts_with = "channel")]
    pub(crate) worktree: Option<String>,
    /// Restrict matches to one named channel.
    #[arg(long, value_name = "NAME", conflicts_with = "worktree")]
    pub(crate) channel: Option<String>,
    /// Type the text but leave it unsubmitted — no Enter after it lands.
    #[arg(long)]
    pub(crate) no_enter: bool,
    /// Send even when the agent is Waiting.
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
    /// Use Rimz's smart compact-first send when the agent's context window is at
    /// least this full: a percentage (`70%`) or an occupied-token count
    /// (`120000`, `180k`). Defaults from `[harness] smart_compact` when omitted.
    #[arg(long, value_name = "PCT|TOKENS", value_parser = AutoCompact::parse)]
    pub(crate) smart_compact: Option<AutoCompact>,
    /// Read the prompt verbatim from a file instead of inline argv. A file already
    /// carries real newlines and literal backslashes, so it is sent as-is with no
    /// `\n`/`\\` interpretation. Conflicts with inline text and piped stdin.
    #[arg(long, value_name = "PATH")]
    pub(crate) file: Option<PathBuf>,
    /// Deliver the text verbatim with no `from @sender:` prefix, even for an agent
    /// caller. No effect for a human caller, which is already verbatim.
    #[arg(long)]
    pub(crate) no_from: bool,
    /// Wait until the agent confirms the submitted message (`30s`, `5m`, `1h`).
    /// Bare `--wait` uses `RIMZ_MESSAGE_DELIVERY_WINDOW_MS` or the default window.
    #[arg(long, value_name = "DURATION", num_args = 0..=1, value_parser = parse_wait_duration)]
    pub(crate) wait: Option<Option<Duration>>,
}

/// Resolve the prompt a send-style invocation carries from inline argv, piped
/// stdin, or a `--file` path. A file is read verbatim (it already holds real
/// newlines and literal backslashes), while inline argv goes through `\n`/`\\`
/// interpretation. Piped stdin is also verbatim and follows an inline
/// instruction inside `<stdin>` tags when both are present.
pub(crate) fn resolve_message(
    parts: &[String],
    file: Option<&Path>,
    piped: Option<&str>,
) -> Result<String> {
    if file.is_some() && piped.is_some_and(|text| !text.trim().is_empty()) {
        bail!("pipe stdin or pass `--file`, not both");
    }
    match file {
        Some(path) => {
            if !parts.is_empty() {
                bail!("pass a prompt inline or with `--file`, not both");
            }
            read_prompt_file(path)
        }
        None => {
            let inline = message_text(parts);
            combine_text_prompt(inline.as_deref(), piped).ok_or_else(|| {
                anyhow::anyhow!(
                    "expected non-empty text inline, from piped stdin, or with `--file`"
                )
            })
        }
    }
}

/// Build a text-mode prompt with the positional instruction first; when both
/// positional text and piped stdin are present, wrap stdin in `<stdin>` tags.
pub(crate) fn combine_text_prompt(positional: Option<&str>, piped: Option<&str>) -> Option<String> {
    let positional = positional
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty());
    let piped = piped.map(str::trim).filter(|prompt| !prompt.is_empty());
    match (positional, piped) {
        (Some(positional), Some(piped)) => {
            Some(format!("{positional}\n\n<stdin>\n{piped}\n</stdin>"))
        }
        (Some(positional), None) => Some(positional.to_owned()),
        (None, Some(piped)) => Some(piped.to_owned()),
        (None, None) => None,
    }
}

pub(crate) fn read_piped_text_prompt() -> Result<Option<String>> {
    use std::io::{IsTerminal as _, Read as _};

    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Ok(None);
    }
    let mut buf = String::new();
    stdin
        .lock()
        .read_to_string(&mut buf)
        .context("reading stdin")?;
    Ok(Some(buf))
}

/// The caller identity for `message`. Rimz-launched agents carry
/// `RIMZ_AGENT_KIND`; ordinary room shells carry `RIMZ` identity vars without it,
/// so they stay human-authored unless an agent kind is present.
pub(crate) fn sender_from_env(channel: Option<&str>, no_from: bool) -> MessageSender {
    if no_from {
        return MessageSender::Human;
    }
    let Some(kind) = env_string(rimz::harness::run::ENV_AGENT_KIND) else {
        return MessageSender::Human;
    };
    MessageSender::Agent {
        kind: AgentKind::new_unchecked(kind),
        name: env_string(rimz::harness::run::ENV_AGENT_NAME),
        profile: env_string(rimz::harness::run::ENV_AGENT_PROFILE),
        role: env_string(rimz::harness::run::ENV_AGENT_ROLE),
        channel: channel.map(ToOwned::to_owned),
    }
}

pub(crate) fn wait_duration(wait: Option<Option<Duration>>) -> Option<Duration> {
    wait.map(|duration| duration.unwrap_or_else(delivery_window_from_env))
}

pub(crate) fn validate_wait(enter: bool, wait: Option<Duration>) -> Result<()> {
    if wait.is_some() && !enter {
        bail!("--wait requires submitting the message; remove --no-enter");
    }
    Ok(())
}

fn parse_wait_duration(raw: &str) -> std::result::Result<Duration, String> {
    super::parse::parse_duration_units(raw, &[("s", 1), ("m", 60), ("h", 3600)])
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

/// Join inline argv into one message, interpreting `\n` as a soft newline and
/// `\\` as a literal backslash, so a multi-line prompt can be typed inline. The
/// bracketed-paste send path delivers each `\n` as a composer line break rather
/// than a submit. Every other escape keeps its backslash, so a regex or a
/// Windows path in a prompt (`\d+`, `C:\tmp`) survives untouched.
fn message_text(parts: &[String]) -> Option<String> {
    let text = unescape(&parts.join(" "));
    (!text.is_empty()).then_some(text)
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
        assert!(message_text(&[]).is_none());
        assert!(message_text(&[String::new()]).is_none());
    }

    #[test]
    fn resolve_takes_inline_text_when_no_file() {
        let parts = ["fix".to_owned(), "the".to_owned(), "parser".to_owned()];
        assert_eq!(
            resolve_message(&parts, None, None).unwrap(),
            "fix the parser"
        );
    }

    #[test]
    fn resolve_trims_inline_text() {
        assert_eq!(
            resolve_message(&["  review this\\n  ".to_owned()], None, None).unwrap(),
            "review this"
        );
    }

    #[test]
    fn resolve_rejects_text_and_file_together() {
        // A conflict fails before the path is touched, so the bogus path is safe.
        let err = resolve_message(&["hi".to_owned()], Some(Path::new("/nope")), None)
            .expect_err("text and file conflict");
        assert!(err.to_string().contains("not both"), "{err}");
    }

    #[test]
    fn resolve_rejects_no_source() {
        let err = resolve_message(&[], None, None).expect_err("missing prompt source");
        let message = err.to_string();
        assert!(message.contains("inline"), "{message}");
        assert!(message.contains("piped stdin"), "{message}");
        assert!(message.contains("--file"), "{message}");
    }

    #[test]
    fn resolve_accepts_piped_text() {
        assert_eq!(
            resolve_message(&[], None, Some("piped body\n")).unwrap(),
            "piped body"
        );
        assert_eq!(
            resolve_message(&[String::new()], None, Some("piped body\n")).unwrap(),
            "piped body"
        );
    }

    #[test]
    fn resolve_wraps_piped_text_after_inline_instruction() {
        assert_eq!(
            resolve_message(&["review this".to_owned()], None, Some("diff body\n")).unwrap(),
            "review this\n\n<stdin>\ndiff body\n</stdin>"
        );
    }

    #[test]
    fn resolve_rejects_file_and_piped_text() {
        let err = resolve_message(&[], Some(Path::new("/nope")), Some("diff body"))
            .expect_err("piped stdin and file conflict");
        assert_eq!(err.to_string(), "pipe stdin or pass `--file`, not both");
    }

    #[test]
    fn text_prompt_accepts_positional_only() {
        assert_eq!(
            combine_text_prompt(Some("explain"), None).as_deref(),
            Some("explain")
        );
    }

    #[test]
    fn text_prompt_ignores_empty_piped_input() {
        assert_eq!(
            combine_text_prompt(Some("ping"), Some("\n\t")).as_deref(),
            Some("ping")
        );
    }

    #[test]
    fn text_prompt_trims_surrounding_whitespace() {
        assert_eq!(
            combine_text_prompt(Some("  explain  "), Some("\nboom\t")).as_deref(),
            Some("explain\n\n<stdin>\nboom\n</stdin>")
        );
    }

    #[test]
    fn text_prompt_rejects_empty_inputs() {
        assert_eq!(combine_text_prompt(Some("  "), Some("\n\t")), None);
    }
}
