//! Shared send flags, prompt sources, sender attribution, and outcome presentation.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Args;

use rimz::ids::AgentKind;
use rimz::message::dispatch::{DispatchOutcome, ParkReason};
use rimz::message::{AutoCompact, MessageSender};

const AGENT_WAIT_DEADLINE: Duration = Duration::from_secs(3600);

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
    /// Use RimZ's smart compact-first send when the agent's context window is at
    /// least this full: a percentage (`70%`) or an occupied-token count
    /// (`120000`, `180k`). Defaults from `[harness] smart_compact` when omitted.
    #[arg(long, value_name = "PCT|TOKENS", value_parser = AutoCompact::parse)]
    pub(crate) smart_compact: Option<AutoCompact>,
    /// Read the prompt verbatim from a file instead of inline argv. A file already
    /// carries real newlines and literal backslashes, so it is sent as-is with no
    /// `\n`/`\\` interpretation. Conflicts with inline text and `--stdin`.
    #[arg(long, value_name = "PATH")]
    pub(crate) file: Option<PathBuf>,
    /// Read stdin to EOF as prompt content. With inline text, the instruction
    /// goes first and stdin follows inside `<stdin>` tags. Conflicts with `--file`.
    #[arg(long, conflicts_with = "file")]
    pub(crate) stdin: bool,
    /// Deliver the text verbatim with no message header.
    #[arg(long)]
    pub(crate) no_from: bool,
    /// Wait for the target agents' replies and print or gather their final messages.
    /// Bare `--wait` has no deadline for humans and a 1h deadline for agent callers;
    /// use `--wait=5m` to set the whole wait explicitly.
    #[arg(long, value_name = "DURATION", num_args = 0..=1, require_equals = true, value_parser = parse_wait_duration)]
    pub(crate) wait: Option<Option<Duration>>,
    /// Emit replies as one JSON map keyed by agent handle. Requires `--wait`.
    #[arg(long)]
    pub(crate) json: bool,
    /// Return when the first reply leg finishes instead of gathering all replies.
    /// Requires `--wait`.
    #[arg(long)]
    pub(crate) any: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReplyWait {
    Off,
    Indefinite,
    Deadline(Duration),
    DefaultDeadline(Duration),
}

impl ReplyWait {
    pub(crate) fn is_on(self) -> bool {
        !matches!(self, Self::Off)
    }

    pub(crate) fn deadline_from(self, started: Instant) -> Option<Instant> {
        match self {
            Self::Off | Self::Indefinite => None,
            Self::Deadline(duration) | Self::DefaultDeadline(duration) => Some(started + duration),
        }
    }

    pub(crate) fn uses_agent_default(self) -> bool {
        matches!(self, Self::DefaultDeadline(_))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WaitSpec {
    pub(crate) mode: ReplyWait,
    pub(crate) any: bool,
    pub(crate) json: bool,
}

impl WaitSpec {
    #[cfg(test)]
    const OFF: Self = Self {
        mode: ReplyWait::Off,
        any: false,
        json: false,
    };

    pub(crate) fn is_on(self) -> bool {
        self.mode.is_on()
    }

    pub(crate) fn deadline_from(self, started: Instant) -> Option<Instant> {
        self.mode.deadline_from(started)
    }
}

/// Resolve the prompt a send-style invocation carries from inline argv,
/// `--stdin`, or a `--file` path. A file is read verbatim (it already holds real
/// newlines and literal backslashes), while inline argv goes through `\n`/`\\`
/// interpretation. Explicit stdin is also verbatim and follows an inline
/// instruction inside `<stdin>` tags when both are present.
pub(crate) fn resolve_message(
    parts: &[String],
    file: Option<&Path>,
    piped: Option<&str>,
) -> Result<String> {
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
                anyhow::anyhow!("expected non-empty text inline, from `--stdin`, or with `--file`")
            })
        }
    }
}

/// Build a text-mode prompt with the positional instruction first; when both
/// positional text and explicit stdin are present, wrap stdin in `<stdin>` tags.
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

pub(crate) fn read_stdin_prompt() -> Result<Option<String>> {
    use std::io::Read as _;

    let mut buf = String::new();
    std::io::stdin()
        .lock()
        .read_to_string(&mut buf)
        .context("reading stdin")?;
    Ok(Some(buf))
}

#[cfg(unix)]
fn buffered_stdin_bytes(fd: std::os::fd::BorrowedFd<'_>) -> Option<u64> {
    rustix::io::ioctl_fionread(fd).ok()
}

pub(crate) fn warn_ignored_stdin() {
    use std::io::IsTerminal as _;

    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return;
    }
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::fd::AsFd as _;

        if buffered_stdin_bytes(stdin.as_fd()).is_some_and(|bytes| bytes > 0) {
            let _ = writeln!(
                std::io::stderr().lock(),
                "rimz: stdin has data but --stdin was not passed; ignoring it"
            );
        }
    }
}

/// The caller identity for `message`. `--no-from` is deliberately unattributed
/// system text so delivery stays verbatim. Otherwise RimZ-launched agents carry
/// `RIMZ_AGENT_KIND`; ordinary room shells stay human-authored.
pub(crate) fn sender_from_env(channel: Option<&str>, no_from: bool) -> MessageSender {
    if no_from {
        return MessageSender::System;
    }
    let Some(kind) = env_string(rimz::harness::launch::ENV_AGENT_KIND) else {
        return MessageSender::Human;
    };
    MessageSender::Agent {
        kind: AgentKind::new_unchecked(kind),
        name: env_string(rimz::harness::launch::ENV_AGENT_NAME),
        profile: env_string(rimz::harness::launch::ENV_AGENT_PROFILE),
        role: env_string(rimz::harness::launch::ENV_AGENT_ROLE),
        channel: channel.map(ToOwned::to_owned),
    }
}

pub(crate) fn agent_caller() -> bool {
    env_string(rimz::harness::launch::ENV_AGENT_KIND).is_some()
}

pub(crate) fn agent_caller_identity() -> Option<(AgentKind, String)> {
    Some((
        AgentKind::new_unchecked(env_string(rimz::harness::launch::ENV_AGENT_KIND)?),
        env_string(rimz::harness::launch::ENV_AGENT_NAME)?,
    ))
}

pub(crate) fn reply_wait(wait: Option<Option<Duration>>, agent_caller: bool) -> ReplyWait {
    match wait {
        None => ReplyWait::Off,
        Some(None) if agent_caller => ReplyWait::DefaultDeadline(AGENT_WAIT_DEADLINE),
        Some(None) => ReplyWait::Indefinite,
        Some(Some(duration)) => ReplyWait::Deadline(duration),
    }
}

pub(crate) fn validate_reply_wait(
    wait: WaitSpec,
    enter: bool,
    create: bool,
    scheduled: bool,
) -> Result<()> {
    if !wait.is_on() {
        if wait.json {
            bail!("--json requires --wait");
        }
        if wait.any {
            bail!("--any requires --wait");
        }
        return Ok(());
    }
    if !enter {
        bail!("--wait requires submitting the message; remove --no-enter");
    }
    if create {
        bail!("--wait requires an existing agent; remove --create");
    }
    if scheduled {
        bail!("--wait sends now or parks for a turn boundary; remove --schedule");
    }
    Ok(())
}

#[derive(Clone, Copy)]
pub(crate) enum ReportMode {
    Steer,
    Boundary,
}

pub(crate) fn render_dispatch_outcome(outcome: &DispatchOutcome) -> Option<String> {
    match outcome {
        DispatchOutcome::Sent { label, message_id } => {
            Some(format!("delivered to {label} ({message_id})"))
        }
        DispatchOutcome::Queued {
            label,
            message_id,
            reason,
        } => Some(match reason {
            Some(ParkReason::Status(status)) => format!(
                "queued for {label} ({message_id}) — {label} is {}; send now: rimz message steer {message_id}",
                status.as_str()
            ),
            Some(ParkReason::WaitingOnPrompt) => format!(
                "queued for {label} ({message_id}) — {label} is waiting on input in its pane; answer it or force: rimz message steer {message_id} --force"
            ),
            None => format!("queued for {label} ({message_id})"),
        }),
        DispatchOutcome::CompactionPending { label, message_id } => Some(format!(
            "compacting {label}; queued {message_id} (delivers when compaction completes)"
        )),
        DispatchOutcome::SkippedWaiting { .. } => None,
    }
}

/// Present dispatch outcomes shared by direct messages and loop delivery.
pub(crate) fn report_dispatch(
    mode: ReportMode,
    target: &str,
    outcomes: &[DispatchOutcome],
    compacted: &[String],
) -> Result<()> {
    match mode {
        ReportMode::Boundary => report_boundary(outcomes, compacted),
        ReportMode::Steer => report_steer(target, outcomes, compacted),
    }
}

fn report_boundary(outcomes: &[DispatchOutcome], compacted: &[String]) -> Result<()> {
    for label in compacted {
        #[expect(clippy::print_stdout, reason = "command result")]
        {
            println!("compacted {label}");
        }
    }
    for outcome in outcomes {
        if let Some(line) = render_dispatch_outcome(outcome) {
            #[expect(clippy::print_stdout, reason = "command result")]
            {
                println!("{line}");
            }
        }
    }
    Ok(())
}

fn report_steer(target: &str, outcomes: &[DispatchOutcome], compacted: &[String]) -> Result<()> {
    let mut sent = Vec::new();
    let mut sent_labels = Vec::new();
    let mut queued = Vec::new();
    let mut pending = Vec::new();
    for outcome in outcomes {
        match outcome {
            DispatchOutcome::Sent { label, message_id } => {
                sent.push(format!("{label} ({message_id})"));
                sent_labels.push(label.as_str());
            }
            DispatchOutcome::Queued {
                label, message_id, ..
            } => {
                queued.push(format!("{label} ({message_id})"));
            }
            DispatchOutcome::CompactionPending { label, message_id } => {
                queued.push(format!("{label} ({message_id}; waiting for compaction)"));
            }
            DispatchOutcome::SkippedWaiting { label, message_id } => {
                pending.push(format!("{label} ({message_id})"));
            }
        }
    }
    if outcomes.len() == 1 {
        if !sent.is_empty() {
            let label = sent_labels[0];
            print_compacted_if_needed(label, compacted);
            #[expect(clippy::print_stdout, reason = "message confirmation")]
            {
                println!("sent to {}", sent[0]);
            }
            return Ok(());
        }
        if !queued.is_empty() {
            #[expect(clippy::print_stdout, reason = "message confirmation")]
            {
                println!("queued for {}", queued[0]);
            }
            return Ok(());
        }
        match outcomes.first() {
            Some(DispatchOutcome::SkippedWaiting { label, message_id }) => bail!(
                "{label} ({message_id}) is waiting on your input in its pane; answer it or pass --force"
            ),
            Some(DispatchOutcome::CompactionPending { .. }) => {
                unreachable!("compaction-pending outcomes are included in queued summary")
            }
            _ => bail!("no agent matches `{target}`"),
        }
    }
    let mut line = format!("sent {} agent(s)", sent.len());
    if !sent.is_empty() {
        line.push_str(&format!(": {}", sent.join(", ")));
    }
    if !queued.is_empty() {
        line.push_str(&format!("; queued: {}", queued.join(", ")));
    }
    if !compacted.is_empty() {
        line.push_str(&format!("; compacted: {}", compacted.join(", ")));
    }
    if !pending.is_empty() {
        line.push_str(&format!("; waiting in pane: {}", pending.join(", ")));
    }
    #[expect(clippy::print_stdout, reason = "message fan-out summary")]
    {
        println!("{line}");
    }
    Ok(())
}

fn print_compacted_if_needed(label: &str, compacted: &[String]) {
    if compacted.iter().any(|compacted| compacted == label) {
        #[expect(clippy::print_stdout, reason = "message compact confirmation")]
        {
            println!("compacted {label}");
        }
    }
}

fn parse_wait_duration(raw: &str) -> std::result::Result<Duration, String> {
    rimz::harness::schedule::parse_duration_units(raw, &[("s", 1), ("m", 60), ("h", 3600)])
}

fn env_string(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

/// Read a prompt file as-is, trimming only the trailing newline an editor adds so
/// it never lands as a blank composer line before the submit.
pub(crate) fn read_prompt_file(path: &Path) -> Result<String> {
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

    #[cfg(unix)]
    #[test]
    fn fionread_counts_buffered_pipe_bytes() {
        use std::os::fd::AsFd as _;

        let (read_end, write_end) = nix::unistd::pipe().unwrap();
        assert_eq!(buffered_stdin_bytes(read_end.as_fd()), Some(0));
        nix::unistd::write(&write_end, b"x").unwrap();
        assert_eq!(buffered_stdin_bytes(read_end.as_fd()), Some(1));
    }

    #[test]
    fn resolves_reply_wait_modes() {
        assert_eq!(reply_wait(None, false), ReplyWait::Off);
        assert_eq!(reply_wait(Some(None), false), ReplyWait::Indefinite);
        assert_eq!(
            reply_wait(Some(None), true),
            ReplyWait::DefaultDeadline(Duration::from_secs(3600))
        );
        assert_eq!(
            reply_wait(Some(Some(Duration::from_secs(5))), true),
            ReplyWait::Deadline(Duration::from_secs(5))
        );
    }

    #[test]
    fn reply_output_modes_require_wait() {
        for (wait, expected) in [
            (
                WaitSpec {
                    json: true,
                    ..WaitSpec::OFF
                },
                "--json requires --wait",
            ),
            (
                WaitSpec {
                    any: true,
                    ..WaitSpec::OFF
                },
                "--any requires --wait",
            ),
        ] {
            assert_eq!(
                validate_reply_wait(wait, true, false, false)
                    .unwrap_err()
                    .to_string(),
                expected
            );
        }
    }

    #[test]
    fn reply_wait_accepts_fanout_output_modes() {
        let wait = WaitSpec {
            mode: ReplyWait::Indefinite,
            any: true,
            json: true,
        };
        validate_reply_wait(wait, true, false, false).unwrap();
    }

    #[test]
    fn boundary_confirmations_distinguish_delivery_and_park_reasons() {
        let message_id = rimz::ids::MessageId::parse("msg_0123456789abcdef").unwrap();
        let label = "@coder".to_owned();
        let outcome = |reason| DispatchOutcome::Queued {
            label: label.clone(),
            message_id: message_id.clone(),
            reason,
        };

        assert_eq!(
            render_dispatch_outcome(&DispatchOutcome::Sent {
                label: label.clone(),
                message_id: message_id.clone(),
            })
            .as_deref(),
            Some("delivered to @coder (msg_0123456789abcdef)")
        );
        assert_eq!(
            render_dispatch_outcome(&outcome(Some(ParkReason::Status(
                rimz::agents::AgentStatus::Running,
            ))))
            .as_deref(),
            Some(
                "queued for @coder (msg_0123456789abcdef) — @coder is running; send now: rimz message steer msg_0123456789abcdef"
            )
        );
        assert_eq!(
            render_dispatch_outcome(&outcome(Some(ParkReason::WaitingOnPrompt))).as_deref(),
            Some(
                "queued for @coder (msg_0123456789abcdef) — @coder is waiting on input in its pane; answer it or force: rimz message steer msg_0123456789abcdef --force"
            )
        );
        assert_eq!(
            render_dispatch_outcome(&outcome(None)).as_deref(),
            Some("queued for @coder (msg_0123456789abcdef)")
        );
    }

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
        assert!(message.contains("--stdin"), "{message}");
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
