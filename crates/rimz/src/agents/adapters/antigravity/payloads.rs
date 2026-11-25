//! Typed projections of Antigravity's command-hook payloads.

use serde::Deserialize;
use serde_json::Value;

use crate::agents::TurnLimitStop;
use crate::agents::payload::non_empty_trimmed;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct CommonPayload {
    pub conversation_id: Option<String>,
    #[serde(rename = "conversationId")]
    pub conversation_id_camel: Option<String>,
    #[serde(rename = "workspacePaths")]
    pub workspace_paths: Vec<String>,
    #[serde(rename = "transcriptPath")]
    pub transcript_path: Option<String>,
    #[serde(rename = "modelName")]
    pub model_name: Option<String>,
    /// The user input that started the execution; releases before 1.5.13 may omit it.
    #[serde(rename = "lastUserInput")]
    pub last_user_input: Option<String>,
}

impl CommonPayload {
    pub(super) fn conversation_id(&self) -> Option<&str> {
        self.conversation_id
            .as_deref()
            .or(self.conversation_id_camel.as_deref())
    }

    /// Whether the execution runs a bare `/compact`: `/compact <message>` is a prompt.
    pub(super) fn runs_compact_command(&self) -> bool {
        self.last_user_input.as_deref().map(str::trim) == Some(super::COMPACT_COMMAND)
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct InvocationPayload {
    #[serde(flatten)]
    pub common: CommonPayload,
    #[serde(rename = "invocationNum")]
    pub invocation_num: Option<u64>,
    /// The transcript's step count when the invocation began; a row at or past it and before
    /// the invocation's answer arrived during the invocation.
    #[serde(rename = "initialNumSteps")]
    pub initial_num_steps: Option<u64>,
}

impl InvocationPayload {
    /// Whether this opens a native subagent's first turn. On 1.5.17 a child's first invocation
    /// starts on an empty transcript with an empty `lastUserInput`, while a top-level
    /// conversation's carries its prompt row and that prompt. The payload names no parent.
    pub(super) fn opens_native_subagent(&self) -> bool {
        self.invocation_num == Some(0)
            && self.initial_num_steps == Some(0)
            && self
                .common
                .last_user_input
                .as_deref()
                .is_some_and(|input| input.trim().is_empty())
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct PostToolPayload {
    #[serde(flatten)]
    pub common: CommonPayload,
    pub error: Option<Value>,
    /// The transcript step index of the completed tool call; paired with `conversationId` it
    /// forms a `run_command` background task's id (`<conversationId>/task-<stepIdx>`).
    #[serde(rename = "stepIdx")]
    pub step_idx: Option<u64>,
    /// The completed tool call; releases that omit it keep unnamed tool progress.
    #[serde(rename = "toolCall")]
    pub tool_call: Option<ToolCall>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct ToolCall {
    pub name: Option<String>,
    pub args: Option<Value>,
}

impl PostToolPayload {
    pub(super) fn tool_name(&self) -> Option<String> {
        self.tool_call
            .as_ref()
            .and_then(|call| call.name.as_deref())
            .and_then(non_empty_trimmed)
    }

    pub(super) fn failed(&self) -> bool {
        match self.error.as_ref() {
            None | Some(Value::Null) => false,
            Some(Value::String(value)) => !value.trim().is_empty(),
            Some(Value::Bool(false)) => false,
            Some(Value::Array(values)) => !values.is_empty(),
            Some(Value::Object(values)) => !values.is_empty(),
            Some(_) => true,
        }
    }

    /// The background task id this tool completion ended, if any: `run_command` finishes its
    /// own step id on clean or failed exit, and a successful `manage_task` with `Action: "kill"`
    /// ends its `TaskId`.
    pub(super) fn finished_task_id(&self, conversation_id: &str) -> Option<String> {
        match self.tool_name().as_deref()? {
            "run_command" => self
                .step_idx
                .map(|step| format!("{conversation_id}/task-{step}")),
            "manage_task" if !self.failed() => {
                let args = self.tool_call.as_ref()?.args.as_ref()?.as_object()?;
                let action = args
                    .get("Action")
                    .and_then(Value::as_str)
                    .and_then(non_empty_trimmed);
                (action.as_deref() == Some("kill")).then_some(())?;
                args.get("TaskId")
                    .and_then(Value::as_str)
                    .and_then(non_empty_trimmed)
            }
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct StopPayload {
    #[serde(flatten)]
    pub common: CommonPayload,
    #[serde(rename = "terminationReason")]
    pub termination_reason: Option<String>,
    pub error: Option<Value>,
    #[serde(rename = "fullyIdle")]
    pub fully_idle: Option<bool>,
}

/// The `terminationReason` values that end a turn failed, compared without case: 1.5.14 sends
/// the uppercase enum names, and `max_steps_exceeded` is the 1.3.3 limit. Every other reason,
/// `USER_CANCELED` included, is a clean end.
const FAILED_REASONS: [&str; 5] = [
    "ERROR",
    "MAX_INVOCATIONS",
    "MAX_FORCED_INVOCATIONS",
    "MAX_TOKEN_BUDGET_EXCEEDED",
    "max_steps_exceeded",
];

/// The `terminationReason` agy sends when a bare `/compact` finishes. Every other `Stop`
/// reason ends an ordinary turn, even while `lastUserInput` still names `/compact` on a
/// post-compaction continuation.
const COMPACTION_STOP_REASON: &str = "TERMINAL_CUSTOM_HOOK";

impl StopPayload {
    pub(super) fn closes_compaction(&self) -> bool {
        self.common.runs_compact_command()
            && self
                .termination_reason
                .as_deref()
                .map(str::trim)
                .is_some_and(|reason| reason.eq_ignore_ascii_case(COMPACTION_STOP_REASON))
    }

    pub(super) fn error_message(&self) -> Option<&str> {
        self.error
            .as_ref()
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|message| !message.is_empty())
    }

    pub(super) fn failed(&self) -> bool {
        let reason = self
            .termination_reason
            .as_deref()
            .map(str::trim)
            .unwrap_or_default();
        FAILED_REASONS
            .iter()
            .any(|failed| failed.eq_ignore_ascii_case(reason))
            || value_is_error(self.error.as_ref())
    }
}

impl StopPayload {
    /// The provider limit a failed stop names: a string `error` holding one of
    /// `QUOTA_STATUSES` whole, as agy 1.5.17 sends for a spent quota. The label is
    /// the provider's sentence once, without the transport prefix or its `Resets in` clause,
    /// and the refill is that clause's Go duration.
    pub(super) fn limit_stop(&self) -> Option<TurnLimitStop> {
        let Some(Value::String(error)) = self.error.as_ref() else {
            return None;
        };
        if !self.failed() {
            return None;
        }
        let message_at = QUOTA_STATUSES
            .iter()
            .filter_map(|status| error.rfind(status).map(|at| at + status.len()))
            .max()?;
        let message = error[message_at..].trim_start_matches(':');
        let label = message
            .split(REFILL_CLAUSE)
            .next()
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .map(ToOwned::to_owned);
        let refill_in_secs = message
            .rsplit_once(REFILL_CLAUSE)
            .and_then(|(_, after)| after.split_whitespace().next())
            .map(|duration| duration.trim_end_matches(['.', ':']))
            .and_then(go_duration_secs);
        Some(TurnLimitStop {
            label,
            refill_in_secs,
        })
    }
}

/// The two gRPC status forms agy puts before a quota message, each matched whole so a status
/// and a code from different parts of the text never combine.
const QUOTA_STATUSES: [&str; 2] = [
    "(ResourceExhausted, HTTP 429)",
    "RESOURCE_EXHAUSTED (code 429)",
];

/// The clause agy appends to a quota message, followed by the refill as a Go duration.
const REFILL_CLAUSE: &str = "Resets in ";

/// Whole seconds, rounded up, in a Go `time.Duration` string (`1h26m55s`, `1.5s`, `500ms`),
/// or `None` for a negative duration or text jiff's duration parser refuses.
fn go_duration_secs(text: &str) -> Option<u64> {
    let duration: jiff::SignedDuration = text.parse().ok()?;
    if duration.is_negative() {
        return None;
    }
    let whole = u64::try_from(duration.as_secs()).ok()?;
    if duration.subsec_nanos() > 0 {
        whole.checked_add(1)
    } else {
        Some(whole)
    }
}

fn value_is_error(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::String(value)) => !value.trim().is_empty(),
        Some(Value::Bool(false)) => false,
        Some(Value::Array(values)) => !values.is_empty(),
        Some(Value::Object(values)) => !values.is_empty(),
        Some(_) => true,
    }
}

pub(super) fn parse_invocation(payload: &Value) -> Option<InvocationPayload> {
    Deserialize::deserialize(payload).ok()
}

pub(super) fn parse_common(payload: &Value) -> Option<CommonPayload> {
    Deserialize::deserialize(payload).ok()
}

pub(super) fn parse_post_tool(payload: &Value) -> Option<PostToolPayload> {
    Deserialize::deserialize(payload).ok()
}

pub(super) fn parse_stop(payload: &Value) -> Option<StopPayload> {
    Deserialize::deserialize(payload).ok()
}
