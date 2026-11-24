//! Typed projections of Antigravity's command-hook payloads.

use serde::Deserialize;
use serde_json::Value;

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
