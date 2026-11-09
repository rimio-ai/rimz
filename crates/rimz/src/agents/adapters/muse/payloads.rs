//! Typed fields from Muse's native hook envelope.

use crate::agents::hook_types::SessionSource;
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct HookPayload {
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub source: SessionSource,
    pub turn_id: Option<String>,
    pub prompt: Option<String>,
    pub tool_name: Option<String>,
    pub tool_use_id: Option<String>,
    pub last_assistant_message: Option<String>,
}
