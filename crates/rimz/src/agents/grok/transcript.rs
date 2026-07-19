//! Rewind-aware normalization of Grok Build's durable session branch.

use std::collections::{HashMap, VecDeque};
use std::path::Path;

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;

use crate::agents::transcript_fs::read_transcript_tail;
use crate::agents::{
    TranscriptCompanionStat, TranscriptMessage, TranscriptRole, TranscriptStat,
    sanitize_user_prompt,
};

const SESSION_UPDATE_METHOD: &str = "session/update";
const XAI_SESSION_UPDATE_METHOD: &str = "_x.ai/session/update";

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct PromptUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cached_read_tokens: u64,
    pub reasoning_tokens: u64,
    pub model_calls: u64,
    pub api_duration_ms: u64,
    pub cost_usd_ticks: Option<i64>,
    pub cost_is_partial: bool,
    #[serde(rename = "modelUsage")]
    pub model_usage: std::collections::BTreeMap<String, ModelUsage>,
    pub num_turns: u64,
    pub usage_is_incomplete: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct ModelUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cached_read_tokens: u64,
    pub reasoning_tokens: u64,
    pub model_calls: u64,
    pub api_duration_ms: u64,
    pub cost_usd_ticks: Option<i64>,
    pub cost_is_partial: bool,
}

#[derive(Clone, Debug)]
pub(super) struct TurnCompletion {
    pub at_secs: u64,
    pub session_id: Option<String>,
    pub prompt_id: String,
    pub stop_reason: String,
    pub agent_result: Option<String>,
    pub usage: Option<PromptUsage>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct TokenSample {
    pub total_tokens: u64,
    pub context_window_tokens: Option<u64>,
}

#[derive(Clone, Debug)]
enum BranchEvent {
    Message(TranscriptMessage),
    Completion(TurnCompletion),
    TokenSample(TokenSample),
}

#[derive(Debug)]
enum DecodedRow {
    UserMessage {
        text: Option<String>,
        prompt_index: Option<usize>,
        at: Option<Timestamp>,
    },
    AssistantMessage {
        text: Option<String>,
        at: Option<Timestamp>,
        streaming: bool,
        token_sample: Option<TokenSample>,
    },
    Thought {
        streaming: bool,
        token_sample: Option<TokenSample>,
    },
    Rewind {
        target: Option<usize>,
    },
    TurnCompletion {
        completion: Option<TurnCompletion>,
        token_sample: Option<TokenSample>,
    },
    TokenSample(TokenSample),
    Other,
    Ignored,
}

#[derive(Default)]
struct PendingUser {
    text: String,
    prompt_index: Option<usize>,
    at: Option<Timestamp>,
}

#[derive(Default)]
pub(super) struct FoldedSession {
    events: Vec<BranchEvent>,
    prompt_starts: Vec<usize>,
    pending_user: Option<PendingUser>,
    seen_indexed_prompt: bool,
    active_prompt: bool,
    last_assistant_event: Option<usize>,
    pub saw_rewind: bool,
}

impl FoldedSession {
    pub(super) fn messages(&self) -> Vec<TranscriptMessage> {
        self.events
            .iter()
            .filter_map(|event| match event {
                BranchEvent::Message(message) => Some(message.clone()),
                BranchEvent::Completion(_) | BranchEvent::TokenSample(_) => None,
            })
            .collect()
    }

    pub(super) fn completions(&self) -> impl DoubleEndedIterator<Item = &TurnCompletion> {
        self.events.iter().filter_map(|event| match event {
            BranchEvent::Completion(completion) => Some(completion),
            BranchEvent::Message(_) | BranchEvent::TokenSample(_) => None,
        })
    }

    pub(super) fn latest_token_sample(&self) -> Option<TokenSample> {
        self.events.iter().rev().find_map(|event| match event {
            BranchEvent::TokenSample(sample) => Some(*sample),
            BranchEvent::Message(_) | BranchEvent::Completion(_) => None,
        })
    }

    pub(super) fn latest_assistant(&self) -> Option<String> {
        self.events.iter().rev().find_map(|event| match event {
            BranchEvent::Completion(completion)
                if completion.stop_reason == "end_turn"
                    && completion
                        .agent_result
                        .as_deref()
                        .is_some_and(|text| !text.trim().is_empty()) =>
            {
                completion
                    .agent_result
                    .as_deref()
                    .map(str::trim)
                    .map(ToOwned::to_owned)
            }
            BranchEvent::Message(message) if message.role == TranscriptRole::Assistant => {
                Some(message.text.clone())
            }
            BranchEvent::Message(_) | BranchEvent::Completion(_) | BranchEvent::TokenSample(_) => {
                None
            }
        })
    }

    fn flush_user(&mut self) {
        let Some(pending) = self.pending_user.take() else {
            return;
        };
        let counted = match pending.prompt_index {
            Some(index) => {
                self.seen_indexed_prompt = true;
                if index < self.prompt_starts.len() {
                    let truncate_at = self.prompt_starts[index];
                    self.events.truncate(truncate_at);
                    self.prompt_starts.truncate(index);
                }
                index <= self.prompt_starts.len()
            }
            None => !self.seen_indexed_prompt,
        };
        self.active_prompt = counted;
        self.last_assistant_event = None;
        if !counted {
            return;
        }
        let Some(text) = sanitize_user_prompt(Some(&pending.text)) else {
            self.active_prompt = false;
            return;
        };
        self.prompt_starts.push(self.events.len());
        self.events.push(BranchEvent::Message(TranscriptMessage {
            role: TranscriptRole::User,
            at: pending.at,
            text,
        }));
    }

    fn rewind(&mut self, target: usize) {
        self.flush_user();
        let truncate_at = self
            .prompt_starts
            .get(target)
            .copied()
            .unwrap_or(self.events.len());
        if target < self.prompt_starts.len() {
            self.events.truncate(truncate_at);
            self.prompt_starts.truncate(target);
        }
        self.active_prompt = false;
        self.last_assistant_event = None;
        self.saw_rewind = true;
    }

    fn push_assistant(&mut self, text: &str, at: Option<Timestamp>) {
        if !self.active_prompt {
            return;
        }
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if let Some(index) = self.last_assistant_event
            && let Some(BranchEvent::Message(message)) = self.events.get_mut(index)
        {
            if !message.text.is_empty() {
                message.text.push(' ');
            }
            message.text.push_str(text);
            message.at = at.or(message.at);
            return;
        }
        let index = self.events.len();
        self.events.push(BranchEvent::Message(TranscriptMessage {
            role: TranscriptRole::Assistant,
            at,
            text: text.to_owned(),
        }));
        self.last_assistant_event = Some(index);
    }
}

pub(super) fn fold(lines: &str) -> FoldedSession {
    let mut folded = FoldedSession::default();
    for line in lines.lines().filter(|line| !line.trim().is_empty()) {
        apply_row(&mut folded, decode_row(line));
    }
    folded.flush_user();
    folded
}

fn decode_row(line: &str) -> DecodedRow {
    let Ok(row) = serde_json::from_str::<Value>(line) else {
        return DecodedRow::Ignored;
    };
    let Some(method) = row.get("method").and_then(Value::as_str) else {
        return DecodedRow::Ignored;
    };
    if !matches!(method, SESSION_UPDATE_METHOD | XAI_SESSION_UPDATE_METHOD) {
        return DecodedRow::Ignored;
    }
    let params = row.get("params").unwrap_or(&row);
    let update = params.get("update").unwrap_or(params);
    let tag = update.get("sessionUpdate").and_then(Value::as_str);
    let at_secs = row.get("timestamp").and_then(Value::as_u64).unwrap_or(0);
    let at = i64::try_from(at_secs)
        .ok()
        .and_then(|seconds| Timestamp::from_second(seconds).ok());

    let token_sample = token_sample(params, update);
    match tag {
        Some("user_message_chunk") => DecodedRow::UserMessage {
            text: visible_text(update).map(ToOwned::to_owned),
            prompt_index: update
                .get("_meta")
                .and_then(|meta| meta.get("promptIndex"))
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok()),
            at,
        },
        Some("rewind_marker") if method == XAI_SESSION_UPDATE_METHOD => DecodedRow::Rewind {
            target: update
                .get("target_prompt_index")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok()),
        },
        Some("agent_message_chunk") if !is_sidechain(params, update) => {
            DecodedRow::AssistantMessage {
                text: visible_text(update).map(ToOwned::to_owned),
                at,
                streaming: method == SESSION_UPDATE_METHOD,
                token_sample,
            }
        }
        Some("agent_thought_chunk") => DecodedRow::Thought {
            streaming: method == SESSION_UPDATE_METHOD,
            token_sample,
        },
        Some("turn_completed") => DecodedRow::TurnCompletion {
            completion: (method == XAI_SESSION_UPDATE_METHOD)
                .then(|| completion_from_update(&row, params, update))
                .flatten(),
            token_sample,
        },
        _ => token_sample.map_or(DecodedRow::Other, DecodedRow::TokenSample),
    }
}

fn apply_row(folded: &mut FoldedSession, row: DecodedRow) {
    if matches!(row, DecodedRow::Ignored) {
        return;
    }
    if let DecodedRow::UserMessage {
        text,
        prompt_index,
        at,
    } = row
    {
        let Some(text) = text else {
            folded.flush_user();
            folded.last_assistant_event = None;
            return;
        };
        if folded
            .pending_user
            .as_ref()
            .is_some_and(|pending| pending.prompt_index != prompt_index)
        {
            folded.flush_user();
        }
        let pending = folded.pending_user.get_or_insert_with(|| PendingUser {
            prompt_index,
            at,
            ..PendingUser::default()
        });
        pending.text.push_str(&text);
        pending.at = at.or(pending.at);
        return;
    }

    folded.flush_user();
    let (completion, token_sample) = match row {
        DecodedRow::AssistantMessage {
            text,
            at,
            token_sample,
            ..
        } => {
            if let Some(text) = text {
                folded.push_assistant(&text, at);
            }
            (None, token_sample)
        }
        DecodedRow::Thought { token_sample, .. } => (None, token_sample),
        DecodedRow::Rewind { target } => {
            if let Some(target) = target {
                folded.rewind(target);
            }
            return;
        }
        DecodedRow::TurnCompletion {
            completion,
            token_sample,
        } => {
            folded.last_assistant_event = None;
            (completion, token_sample)
        }
        DecodedRow::TokenSample(token_sample) => {
            folded.last_assistant_event = None;
            (None, Some(token_sample))
        }
        DecodedRow::Other => {
            folded.last_assistant_event = None;
            (None, None)
        }
        DecodedRow::Ignored => unreachable!("ignored rows return above"),
        DecodedRow::UserMessage { .. } => unreachable!("user rows return above"),
    };

    if folded.active_prompt
        && let Some(completion) = completion
    {
        folded.events.push(BranchEvent::Completion(completion));
    }
    if folded.active_prompt
        && let Some(token_sample) = token_sample
    {
        folded.events.push(BranchEvent::TokenSample(token_sample));
    }
}

fn completion_from_update(row: &Value, params: &Value, update: &Value) -> Option<TurnCompletion> {
    Some(TurnCompletion {
        at_secs: row.get("timestamp").and_then(Value::as_u64).unwrap_or(0),
        session_id: params
            .get("sessionId")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        prompt_id: update.get("prompt_id")?.as_str()?.to_owned(),
        stop_reason: update.get("stop_reason")?.as_str()?.to_owned(),
        agent_result: update
            .get("agent_result")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        usage: update
            .get("usage")
            .cloned()
            .and_then(|usage| serde_json::from_value(usage).ok()),
    })
}

fn token_sample(params: &Value, update: &Value) -> Option<TokenSample> {
    let meta = params.get("_meta").or_else(|| update.get("_meta"))?;
    Some(TokenSample {
        total_tokens: meta.get("totalTokens")?.as_u64()?,
        context_window_tokens: meta
            .get("contextWindowTokens")
            .and_then(Value::as_u64)
            .filter(|value| *value > 0),
    })
}

fn completion_from_row(line: &str) -> Option<TurnCompletion> {
    match decode_row(line) {
        DecodedRow::TurnCompletion {
            completion: Some(completion),
            ..
        } => Some(completion),
        _ => None,
    }
}

pub(super) fn physical_completions(lines: &str) -> Vec<TurnCompletion> {
    lines.lines().filter_map(completion_from_row).collect()
}

pub(super) fn contains_rewind(lines: &str) -> bool {
    lines
        .lines()
        .any(|line| matches!(decode_row(line), DecodedRow::Rewind { .. }))
}

fn visible_text(update: &Value) -> Option<&str> {
    let content = update.get("content")?;
    if content.get("type").and_then(Value::as_str) != Some("text")
        || content
            .get("_meta")
            .and_then(|meta| meta.get("bash_command"))
            .is_some()
    {
        return None;
    }
    content.get("text").and_then(Value::as_str)
}

fn is_sidechain(params: &Value, update: &Value) -> bool {
    [params.get("_meta"), update.get("_meta")]
        .into_iter()
        .flatten()
        .any(|meta| {
            meta.get("isSidechain").and_then(Value::as_bool) == Some(true)
                || meta
                    .get("parentSessionId")
                    .and_then(Value::as_str)
                    .is_some()
                || meta.get("subagentId").and_then(Value::as_str).is_some()
        })
}

pub(super) fn parse_messages(lines: &str) -> Vec<TranscriptMessage> {
    fold(lines).messages()
}

/// A monotonic suffix cannot retract already-emitted output. When a newly read
/// suffix contains a rewind marker, expose only assistant text after its last
/// marker and discard abandoned bytes from that same suffix.
pub(super) fn parse_assistant_suffix(lines: &str) -> Vec<String> {
    let mut messages = Vec::<String>::new();
    let mut adjacent = false;
    for row in lines.lines().map(decode_row) {
        match row {
            DecodedRow::Rewind { .. } => {
                messages.clear();
                adjacent = false;
            }
            DecodedRow::Thought {
                streaming: true, ..
            } => {}
            DecodedRow::AssistantMessage {
                text: Some(text),
                streaming: true,
                ..
            } if !text.trim().is_empty() => {
                let text = text.trim();
                if adjacent {
                    let message = messages
                        .last_mut()
                        .expect("adjacent assistant has prior text");
                    message.push(' ');
                    message.push_str(text);
                } else {
                    messages.push(text.to_owned());
                }
                adjacent = true;
            }
            _ => adjacent = false,
        }
    }
    messages
}

pub(super) fn read(path: &Path) -> std::io::Result<FoldedSession> {
    std::fs::read_to_string(path).map(|text| fold(&text))
}

pub(super) fn last_assistant_message(path: &Path) -> Option<String> {
    let tail = read_transcript_tail(path)?;
    if !contains_rewind(&tail)
        && let Some(message) = physical_completions(&tail)
            .into_iter()
            .rev()
            .find(|completion| completion.stop_reason == "end_turn")
            .and_then(|completion| completion.agent_result)
            .map(|message| message.trim().to_owned())
            .filter(|message| !message.is_empty())
    {
        return Some(message);
    }
    read(path).ok()?.latest_assistant()
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct Summary {
    pub info: SummaryInfo,
    pub session_summary: String,
    pub current_model_id: Option<String>,
    pub generated_title: Option<String>,
    pub agent_name: Option<String>,
    pub reasoning_effort: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct SummaryInfo {
    pub id: Option<String>,
    pub cwd: Option<String>,
}

impl Summary {
    pub(super) fn title(&self) -> Option<String> {
        self.generated_title
            .as_deref()
            .or(Some(self.session_summary.as_str()))
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(ToOwned::to_owned)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct Signals {
    pub context_tokens_used: u64,
    pub context_window_tokens: u64,
}

pub(super) fn read_summary(transcript: &Path) -> Option<Summary> {
    let path = transcript.parent()?.join("summary.json");
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

pub(super) fn read_signals(transcript: &Path) -> Option<Signals> {
    let path = transcript.parent()?.join("signals.json");
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

#[derive(Deserialize)]
struct PermissionRecord {
    ts: String,
    #[serde(flatten)]
    event: PermissionEvent,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum PermissionEvent {
    PermissionRequested {
        tool_name: String,
    },
    PermissionResolved {
        tool_name: String,
    },
    #[serde(other)]
    Other,
}

/// Fold Grok's append-ordered permission brackets without promoting them to
/// lifecycle or ask state. A bounded record-aligned tail keeps this enrichment
/// best-effort when a session has a long event history.
pub(super) fn native_permission_wait(events: &Path) -> Option<Timestamp> {
    let tail = read_transcript_tail(events)?;
    let mut outstanding = HashMap::<String, VecDeque<Timestamp>>::new();
    for line in tail.lines() {
        let Ok(record) = serde_json::from_str::<PermissionRecord>(line) else {
            continue;
        };
        let Ok(at) = record.ts.parse::<Timestamp>() else {
            continue;
        };
        let (tool_name, requested) = match record.event {
            PermissionEvent::PermissionRequested { tool_name } => (tool_name, true),
            PermissionEvent::PermissionResolved { tool_name } => (tool_name, false),
            PermissionEvent::Other => continue,
        };
        if tool_name.trim().is_empty() {
            continue;
        }
        if requested {
            outstanding.entry(tool_name).or_default().push_back(at);
        } else if let Some(requests) = outstanding.get_mut(&tool_name) {
            requests.pop_front();
        }
    }
    outstanding
        .values()
        .flat_map(|requests| requests.iter().copied())
        .max()
}

pub(super) fn combined_stat(transcript: &Path, events: Option<&Path>) -> Option<TranscriptStat> {
    let mut stat = TranscriptStat::from_path(transcript)?;
    let parent = transcript.parent()?;
    let companions = [
        Some(parent.join("summary.json")),
        Some(parent.join("signals.json")),
        events.map(Path::to_path_buf),
    ]
    .into_iter()
    .flatten()
    .filter_map(|path| TranscriptStat::from_path(&path))
    .collect::<Vec<_>>();
    if !companions.is_empty() {
        stat.companion = Some(TranscriptCompanionStat {
            mtime_secs: companions
                .iter()
                .fold(0, |sum, value| sum.wrapping_add(value.mtime_secs)),
            mtime_nanos: companions
                .iter()
                .fold(0, |sum, value| sum.wrapping_add(value.mtime_nanos)),
            len: companions.iter().map(|value| value.len).sum(),
        });
    }
    Some(stat)
}

#[cfg(test)]
#[path = "tests/transcript.rs"]
mod tests;
