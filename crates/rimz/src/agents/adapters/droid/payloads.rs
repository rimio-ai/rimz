//! Typed input structs for Factory Droid's native hook protocol.
//!
//! Droid's common fields and lifecycle enums mirror Claude's hook wire. Sparse,
//! malformed, and forward-extended payloads fall back to defaults so hooks stay
//! best-effort enrichment rather than a reason to interrupt the agent.
use serde::Deserialize;
use serde_json::Value;

use crate::agents::hook_types::SessionSource;
use crate::agents::lifecycle::AskKind;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DroidSessionStart {
    pub source: SessionSource,
    /// Parent session: the compacted session on `source: compact`, a fork
    /// parent otherwise. Absent before Droid added the field.
    pub previous_session_id: Option<String>,
}

impl DroidSessionStart {
    /// The session a compaction close belongs to when Droid rotated the id.
    pub fn compacted_session_id(&self) -> Option<&str> {
        if self.source != SessionSource::Compact {
            return None;
        }
        self.previous_session_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DroidNotification {
    pub message: Option<String>,
    pub notification_type: Option<DroidNotificationType>,
}

/// Droid's `Notification.notification_type`. Releases before the field, and
/// values RimZ does not route (`auth_success`), stay silent enrichment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DroidNotificationType {
    /// Tool confirmation batch, including the spec-mode exit plan approval.
    PermissionPrompt,
    /// `AskUser` questions.
    ElicitationDialog,
    /// Sent only after a user interrupt, never on a timer.
    IdlePrompt,
    #[serde(other)]
    Other,
}

impl DroidNotification {
    pub fn ask_kind(&self) -> Option<AskKind> {
        match self.notification_type? {
            DroidNotificationType::PermissionPrompt => Some(AskKind::Permission),
            DroidNotificationType::ElicitationDialog => Some(AskKind::Question),
            DroidNotificationType::IdlePrompt | DroidNotificationType::Other => None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DroidUserPromptSubmit {
    pub prompt: Option<String>,
}

macro_rules! parse_fn {
    ($name:ident, $ty:ty) => {
        pub fn $name(payload: &Value) -> $ty {
            serde_json::from_value(payload.clone()).unwrap_or_default()
        }
    };
}

parse_fn!(parse_session_start, DroidSessionStart);
parse_fn!(parse_user_prompt_submit, DroidUserPromptSubmit);
parse_fn!(parse_notification, DroidNotification);

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parsers_keep_consumed_fields_and_tolerate_drift() {
        let start = parse_session_start(&json!({
            "source": "startup",
            "future_field": {"nested": true},
        }));
        assert_eq!(start.source, SessionSource::Startup);
        assert_eq!(
            parse_session_start(&json!([])).source,
            SessionSource::Startup
        );
        assert_eq!(
            parse_user_prompt_submit(&json!({"prompt": "ship it"}))
                .prompt
                .as_deref(),
            Some("ship it")
        );

        let compact = parse_session_start(&json!({
            "source": "compact",
            "previous_session_id": " old ",
        }));
        assert_eq!(compact.compacted_session_id(), Some("old"));
        let resume = parse_session_start(&json!({
            "source": "resume",
            "previous_session_id": "parent",
        }));
        assert_eq!(resume.compacted_session_id(), None);

        for (kind, ask) in [
            ("permission_prompt", Some(AskKind::Permission)),
            ("elicitation_dialog", Some(AskKind::Question)),
            ("idle_prompt", None),
            ("auth_success", None),
        ] {
            assert_eq!(
                parse_notification(&json!({"notification_type": kind})).ask_kind(),
                ask,
                "{kind}"
            );
        }
        assert_eq!(
            parse_notification(&json!({"message": "hi"})).ask_kind(),
            None
        );
    }
}
