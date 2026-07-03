//! Adapter-normalized provider-native transcript message types.
//!
//! Adapters normalize their native transcript files into [`TranscriptMessage`].
//! The durable Rimz-owned cross-provider chat log lives in [`crate::chat`].

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptMessage {
    pub role: TranscriptRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<Timestamp>,
    pub text: String,
}
