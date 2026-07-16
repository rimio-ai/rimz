//! Rewind-aware normalization of Grok Build's durable session branch.

use std::path::Path;

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;

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
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        apply_row(&mut folded, &row);
    }
    folded.flush_user();
    folded
}

fn apply_row(folded: &mut FoldedSession, row: &Value) {
    let Some(method) = row.get("method").and_then(Value::as_str) else {
        return;
    };
    if !matches!(method, SESSION_UPDATE_METHOD | XAI_SESSION_UPDATE_METHOD) {
        return;
    }
    let params = row.get("params").unwrap_or(row);
    let update = params.get("update").unwrap_or(params);
    let tag = update.get("sessionUpdate").and_then(Value::as_str);
    let at_secs = row.get("timestamp").and_then(Value::as_u64).unwrap_or(0);
    let at = i64::try_from(at_secs)
        .ok()
        .and_then(|seconds| Timestamp::from_second(seconds).ok());

    if tag == Some("user_message_chunk") {
        let Some(text) = visible_text(update) else {
            folded.flush_user();
            folded.last_assistant_event = None;
            return;
        };
        let prompt_index = update
            .get("_meta")
            .and_then(|meta| meta.get("promptIndex"))
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok());
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
        pending.text.push_str(text);
        pending.at = at.or(pending.at);
        return;
    }

    folded.flush_user();
    if method == XAI_SESSION_UPDATE_METHOD && tag == Some("rewind_marker") {
        if let Some(target) = update
            .get("target_prompt_index")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
        {
            folded.rewind(target);
        }
        return;
    }

    if tag == Some("agent_message_chunk") && !is_sidechain(params, update) {
        if let Some(text) = visible_text(update) {
            folded.push_assistant(text, at);
        }
    } else if tag != Some("agent_thought_chunk") {
        folded.last_assistant_event = None;
    }

    if folded.active_prompt
        && tag == Some("turn_completed")
        && let Some(completion) = completion_from_row(row)
    {
        folded.events.push(BranchEvent::Completion(completion));
    }

    if folded.active_prompt
        && let Some(total_tokens) = params
            .get("_meta")
            .or_else(|| update.get("_meta"))
            .and_then(|meta| meta.get("totalTokens"))
            .and_then(Value::as_u64)
    {
        let context_window_tokens = params
            .get("_meta")
            .or_else(|| update.get("_meta"))
            .and_then(|meta| meta.get("contextWindowTokens"))
            .and_then(Value::as_u64)
            .filter(|value| *value > 0);
        folded.events.push(BranchEvent::TokenSample(TokenSample {
            total_tokens,
            context_window_tokens,
        }));
    }
}

fn completion_from_row(row: &Value) -> Option<TurnCompletion> {
    if row.get("method").and_then(Value::as_str) != Some(XAI_SESSION_UPDATE_METHOD) {
        return None;
    }
    let params = row.get("params").unwrap_or(row);
    let update = params.get("update").unwrap_or(params);
    if update.get("sessionUpdate").and_then(Value::as_str) != Some("turn_completed") {
        return None;
    }
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

pub(super) fn physical_completions(lines: &str) -> Vec<TurnCompletion> {
    lines
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|row| completion_from_row(&row))
        .collect()
}

pub(super) fn contains_rewind(lines: &str) -> bool {
    lines.lines().any(|line| {
        serde_json::from_str::<Value>(line).ok().is_some_and(|row| {
            if row.get("method").and_then(Value::as_str) != Some(XAI_SESSION_UPDATE_METHOD) {
                return false;
            }
            let params = row.get("params").unwrap_or(&row);
            let update = params.get("update").unwrap_or(params);
            update.get("sessionUpdate").and_then(Value::as_str) == Some("rewind_marker")
        })
    })
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
    let rows = lines.lines().collect::<Vec<_>>();
    let start = rows
        .iter()
        .rposition(|line| {
            serde_json::from_str::<Value>(line).ok().is_some_and(|row| {
                if row.get("method").and_then(Value::as_str) != Some(XAI_SESSION_UPDATE_METHOD) {
                    return false;
                }
                let params = row.get("params").unwrap_or(&row);
                let update = params.get("update").unwrap_or(params);
                update.get("sessionUpdate").and_then(Value::as_str) == Some("rewind_marker")
            })
        })
        .map_or(0, |index| index + 1);
    let mut messages = Vec::<String>::new();
    let mut adjacent = false;
    for line in &rows[start..] {
        let (text, assistant, thought) = serde_json::from_str::<Value>(line)
            .ok()
            .map(|row| {
                if row.get("method").and_then(Value::as_str) != Some(SESSION_UPDATE_METHOD) {
                    return (None, false, false);
                }
                let params = row.get("params").unwrap_or(&row);
                let update = params.get("update").unwrap_or(params);
                let tag = update.get("sessionUpdate").and_then(Value::as_str);
                let assistant = tag == Some("agent_message_chunk") && !is_sidechain(params, update);
                (
                    visible_text(update).map(|text| text.trim().to_owned()),
                    assistant,
                    tag == Some("agent_thought_chunk"),
                )
            })
            .unwrap_or((None, false, false));
        if thought {
            continue;
        }
        let Some(text) = text else {
            adjacent = false;
            continue;
        };
        if !assistant || text.is_empty() {
            adjacent = false;
            continue;
        }
        if adjacent {
            let message = messages
                .last_mut()
                .expect("adjacent assistant has prior text");
            message.push(' ');
            message.push_str(&text);
        } else {
            messages.push(text);
        }
        adjacent = true;
    }
    messages
}

pub(super) fn read(path: &Path) -> std::io::Result<FoldedSession> {
    std::fs::read_to_string(path).map(|text| fold(&text))
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

pub(super) fn combined_stat(transcript: &Path) -> Option<TranscriptStat> {
    let mut stat = TranscriptStat::from_path(transcript)?;
    let parent = transcript.parent()?;
    let companions = [parent.join("summary.json"), parent.join("signals.json")]
        .into_iter()
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
