//! Shared surface for `rimz message`: prompt parsing, common flags, sender
//! attribution, and the live-pane send path. `--steer` always sends now. The
//! default message path sends now when the target can receive, and parks a
//! durable record when a turn-boundary gate, pending ask, FIFO head, schedule,
//! or missing pane requires delivery later.

use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Args;

use rimz::agents::AgentState;
use rimz::feed::pending_ask_for;
use rimz::ids::{AgentKind, AgentSessionId, MessageId, WorkspaceId};
use rimz::message::{
    AutoCompact, DeliveryGate, MessageBody, MessageRecord, MessageSender, MessageStatus,
    delivery_window_from_env,
};
use rimz::schema::event::EventKind;
use rimz::workspace::ResolvedWorkspace;
use rimz::{PaneAgent, SidebarSnapshot};

/// The flags shared by immediate and parked message delivery.
#[derive(Debug, Args)]
pub(crate) struct SendFlags {
    /// Restrict matches to one worktree branch, name, or path.
    #[arg(long, conflicts_with = "channel")]
    pub(crate) worktree: Option<String>,
    /// Restrict matches to one named channel.
    #[arg(long, value_name = "NAME", conflicts_with = "worktree")]
    pub(crate) channel: Option<String>,
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
    /// Use Rimz's smart compact-first send when the agent's context window is at
    /// least this full: a percentage (`70%`) or an occupied-token count
    /// (`120000`). Defaults from `[harness] smart_compact` when omitted.
    #[arg(long, value_name = "PCT|TOKENS", value_parser = AutoCompact::parse)]
    pub(crate) smart_compact: Option<AutoCompact>,
    /// Read the prompt verbatim from a file instead of inline argv. A file already
    /// carries real newlines and literal backslashes, so it is sent as-is with no
    /// `\n`/`\\` interpretation. Conflicts with inline text.
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

/// Resolve the prompt a send-style invocation carries from its two sources:
/// inline argv, or a `--file` path. Exactly one applies — a file is read verbatim
/// (it already holds real newlines and literal backslashes), while inline argv
/// goes through `\n`/`\\` interpretation.
pub(crate) fn resolve_message(parts: &[String], file: Option<&Path>) -> Result<String> {
    match file {
        Some(path) => {
            if !parts.is_empty() {
                bail!("pass a prompt inline or with `--file`, not both");
            }
            read_prompt_file(path)
        }
        None => message_text(parts),
    }
}

/// The caller identity for `message`. Rimz-launched agents carry
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

/// What happened to one live-pane send in a fan-out. Every resolved pane target
/// carries a live pane, so the only soft skip is a pending ask reserving the
/// next input.
pub(crate) enum Outcome {
    Sent {
        label: String,
        message_id: MessageId,
    },
    SkippedPending {
        label: String,
        message_id: MessageId,
        request_id: String,
    },
}

/// How a live-pane send is delivered: whether to send past a pending ask,
/// and pacing state.
pub(crate) struct LiveSend {
    pub(crate) force: bool,
    pub(crate) pacer: Pacer,
}

pub(crate) struct MessageDraft {
    pub(crate) text: String,
    pub(crate) body: MessageBody,
    pub(crate) enter: bool,
    pub(crate) gate: DeliveryGate,
    pub(crate) sender: MessageSender,
    pub(crate) force: bool,
    pub(crate) auto_compact: Option<AutoCompact>,
}

pub(crate) struct SentPrompt {
    pub(crate) outcome: Outcome,
    pub(crate) compacted: Option<MessageId>,
}

pub(crate) fn message_for_target(
    workspace_id: WorkspaceId,
    target: &PaneAgent,
    bound: Option<&AgentState>,
    scope_channel: Option<&str>,
    draft: MessageDraft,
) -> MessageRecord {
    let now = jiff::Timestamp::now();
    let agent_id = bound
        .map(|agent| agent.agent_id.clone())
        .or_else(|| target.agent_id.clone())
        .unwrap_or_else(|| synthetic_session_for_pane(&target.pane_id));
    let agent_name = bound
        .and_then(|agent| agent.name.clone())
        .or_else(|| target.name.clone());
    MessageRecord {
        message_id: MessageId::new(),
        workspace_id,
        kind: target.kind.clone(),
        agent_id,
        agent_name,
        channel: rimz::target::recipient_channel(target, bound, scope_channel),
        sender: draft.sender,
        body: draft.body,
        text: draft.text,
        enter: draft.enter,
        gate: draft.gate,
        force: draft.force,
        pane_id: Some(target.pane_id.clone()),
        status: MessageStatus::Created,
        enqueued_at: now,
        updated_at: now,
        attempts: 0,
        last_attempt_at: None,
        last_error: None,
        delivered_at: None,
        not_before: None,
        auto_compact: draft.auto_compact,
        compacted_context_tokens: None,
    }
}

pub(crate) fn send_prompt_to_live_pane(
    workspace: &ResolvedWorkspace,
    ledger: &rimz::Ledger,
    snapshot: &SidebarSnapshot,
    target: &PaneAgent,
    bound: Option<&AgentState>,
    prompt: &MessageRecord,
    send: &mut LiveSend,
) -> Result<SentPrompt> {
    let mut compacted = None;
    if prompt.body == MessageBody::Prompt
        && let Some(command) = compact_message_for_target(ledger, target, bound, prompt)
    {
        match send_to_live_pane(workspace, ledger, snapshot, target, bound, &command, send) {
            Ok(Outcome::Sent { message_id, .. }) => {
                compacted = Some(message_id);
            }
            Ok(skipped @ Outcome::SkippedPending { .. }) => {
                return Ok(SentPrompt {
                    outcome: skipped,
                    compacted: None,
                });
            }
            Err(err) => {
                ledger.record_send_error(&command, &err.to_string(), &workspace.session_name)?;
                return Err(err);
            }
        }
    }
    let outcome = send_to_live_pane(workspace, ledger, snapshot, target, bound, prompt, send)?;
    Ok(SentPrompt { outcome, compacted })
}

fn synthetic_session_for_pane(pane_id: &rimz::ids::PaneId) -> AgentSessionId {
    let mut rendered = String::from("pane_");
    rendered.extend(pane_id.as_str().chars().map(|ch| match ch {
        'a'..='z' | 'A'..='Z' | '0'..='9' => ch,
        _ => '_',
    }));
    AgentSessionId::from(rendered)
}

pub(crate) struct Pacer {
    interval: Duration,
    started: bool,
}

impl Pacer {
    pub(crate) fn new(interval: Duration) -> Self {
        Self {
            interval,
            started: false,
        }
    }

    /// Sleep before every delivered message after the first, so fan-outs land
    /// paced rather than coalesced.
    pub(crate) fn tick(&mut self) {
        self.tick_with(sleep);
    }

    fn tick_with(&mut self, sleeper: impl FnOnce(Duration)) -> bool {
        let should_sleep = self.started && !self.interval.is_zero();
        if should_sleep {
            sleeper(self.interval);
        }
        self.started = true;
        should_sleep
    }
}

/// Type into one live agent pane, recording the message between the paste and
/// the submit Enter. A pending ask skips the agent rather than aborting a broadcast;
/// mux failures return errors.
pub(crate) fn send_to_live_pane(
    workspace: &ResolvedWorkspace,
    ledger: &rimz::Ledger,
    snapshot: &SidebarSnapshot,
    target: &PaneAgent,
    bound: Option<&AgentState>,
    message: &MessageRecord,
    send: &mut LiveSend,
) -> Result<Outcome> {
    let label = handle_for_pane_target(snapshot, target, bound);
    if !send.force
        && let Some(agent) = bound
        && let Some(ask) = pending_ask_for(
            agent,
            snapshot
                .needs_attention
                .iter()
                .chain(snapshot.resolver_working.iter()),
        )
    {
        return Ok(Outcome::SkippedPending {
            label,
            message_id: message.message_id.clone(),
            request_id: ask.request_id.to_string(),
        });
    }
    let pane_id = &target.pane_id;
    let backend = rimz::mux::backend_for(pane_id.mux());
    send.pacer.tick();
    match message.body {
        MessageBody::Command => super::pane::send_text(backend.as_ref(), pane_id, &message.text)?,
        MessageBody::Prompt => {
            let peers: Vec<&AgentState> = snapshot
                .agents
                .iter()
                .filter(|agent| agent.parent_agent_id.is_none())
                .collect();
            let payload = match rimz::target::sender_prefix(
                &message.sender,
                &peers,
                message.channel.as_deref(),
            ) {
                Some(prefix) => format!("{prefix}{}", message.text),
                None => message.text.clone(),
            };
            super::pane::paste_text(backend.as_ref(), pane_id, &payload)?;
        }
    }
    // Record the send once the text lands and before the submit keystroke, so a
    // submitted message is always preceded by its durable record and audit event.
    ledger.record_sent_message(message, &workspace.session_name)?;
    if message.enter {
        super::pane::send_enter(backend.as_ref(), pane_id)?;
    }
    Ok(Outcome::Sent {
        label,
        message_id: message.message_id.clone(),
    })
}

pub(crate) fn handle_for_pane_target(
    snapshot: &SidebarSnapshot,
    target: &PaneAgent,
    bound: Option<&AgentState>,
) -> String {
    if let Some(agent) = bound {
        let peers: Vec<&AgentState> = snapshot
            .agents
            .iter()
            .filter(|agent| agent.parent_agent_id.is_none())
            .collect();
        rimz::target::agent_handle(agent, &peers, true)
    } else {
        format!("@{}", target.label())
    }
}

pub(crate) fn compact_message_for_target(
    ledger: &rimz::Ledger,
    target: &PaneAgent,
    bound: Option<&AgentState>,
    prompt: &MessageRecord,
) -> Option<MessageRecord> {
    let threshold = prompt.auto_compact?;
    let agent = bound?;
    if !threshold.triggered(agent) {
        return None;
    }
    if agent.compacting_since.is_some() {
        return None;
    }
    let command = rimz::agents::find_adapter(target.kind.as_str())?.compact_command()?;
    let occupied = agent.occupied_context_tokens();
    if let Some(used) = occupied
        && already_compacted_at(ledger, agent, command, used)
    {
        return None;
    }
    let mut record = message_for_target(
        prompt.workspace_id.clone(),
        target,
        bound,
        prompt.channel.as_deref(),
        MessageDraft {
            text: command.to_owned(),
            body: MessageBody::Command,
            enter: true,
            gate: prompt.gate,
            sender: prompt.sender.clone(),
            force: prompt.force,
            auto_compact: None,
        },
    );
    record.compacted_context_tokens = occupied;
    Some(record)
}

fn already_compacted_at(
    ledger: &rimz::Ledger,
    agent: &AgentState,
    command: &str,
    used: u64,
) -> bool {
    let live = ledger
        .list_messages()
        .map(|messages| {
            messages.iter().any(|message| {
                message.body == MessageBody::Command
                    && message.text == command
                    && message.compacted_context_tokens == Some(used)
                    && message.same_agent_card(agent)
            })
        })
        .unwrap_or(false);
    if live {
        return true;
    }
    ledger
        .read_events()
        .map(|events| {
            events.into_iter().any(|event| {
                let EventKind::Message { payload, .. } = event.kind() else {
                    return false;
                };
                payload.body == MessageBody::Command
                    && matches!(
                        payload.status,
                        MessageStatus::Sent | MessageStatus::Delivered
                    )
                    && payload.compacted_context_tokens == Some(used)
                    && payload.kind == agent.kind
                    && (payload.agent_id == agent.agent_id
                        || (agent.name.is_some()
                            && payload.agent_name.as_deref() == agent.name.as_deref()))
            })
        })
        .unwrap_or(false)
}

/// The rollup session behind a bound pane target. A lazy pane carries no session,
/// so it never gates on pending asks or context compaction.
pub(crate) fn bound_agent<'a>(
    snapshot: &'a SidebarSnapshot,
    target: &PaneAgent,
) -> Option<&'a AgentState> {
    let agent_id = target.agent_id.as_ref()?;
    snapshot
        .agents
        .iter()
        .find(|agent| agent.kind == target.kind && &agent.agent_id == agent_id)
}

pub(crate) fn wait_for_message_until(
    ledger: &rimz::Ledger,
    message_id: &MessageId,
    session_name: &str,
    deadline: Instant,
) -> Result<MessageStatus> {
    const POLL: Duration = Duration::from_millis(500);

    loop {
        if let Some(message) = ledger
            .list_messages()?
            .into_iter()
            .find(|message| message.message_id == *message_id)
        {
            match message.status {
                MessageStatus::Delivered
                | MessageStatus::Errored
                | MessageStatus::TimedOut
                | MessageStatus::Removed
                | MessageStatus::Abandoned
                | MessageStatus::Archived => return Ok(message.status),
                MessageStatus::Sent if Instant::now() >= deadline => {
                    let timed_out =
                        ledger.mark_message_timed_out(message_id, session_name, Some("wait"))?;
                    return Ok(timed_out
                        .map(|message| message.status)
                        .unwrap_or(MessageStatus::TimedOut));
                }
                MessageStatus::Created
                | MessageStatus::Queued
                | MessageStatus::Claimed
                | MessageStatus::Sent => {}
            }
        } else if let Some(status) = latest_terminal_message_status(ledger, message_id)? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            return Ok(MessageStatus::TimedOut);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        std::thread::sleep(remaining.min(POLL));
    }
}

fn latest_terminal_message_status(
    ledger: &rimz::Ledger,
    message_id: &MessageId,
) -> Result<Option<MessageStatus>> {
    let mut latest = None;
    for event in ledger.read_events()? {
        let EventKind::Message { payload, .. } = event.kind() else {
            continue;
        };
        if payload.message_id == *message_id && payload.status.is_terminal() {
            latest = Some(payload.status);
        }
    }
    Ok(latest)
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
    use std::time::Instant;

    #[test]
    fn pacer_sleeps_after_first_tick() {
        let mut pacer = Pacer::new(Duration::from_millis(40));

        assert!(!pacer.tick_with(|_| panic!("first tick must not sleep")));

        let second = Instant::now();
        pacer.tick();
        assert!(
            second.elapsed() >= Duration::from_millis(40),
            "second tick should sleep at least the configured interval"
        );
    }

    #[test]
    fn zero_interval_pacer_never_sleeps() {
        let mut pacer = Pacer::new(Duration::ZERO);

        for _ in 0..4 {
            assert!(!pacer.tick_with(|_| panic!("zero interval must not sleep")));
        }
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
