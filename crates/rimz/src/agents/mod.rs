//! Agent integration interface.
//!
//! Each adapter classifies an incoming hook event, observes lifecycle
//! transitions (status + mode), renders the agent-native neutral stdout
//! payload, and (when a resolver answer is available) renders the
//! agent-native decision JSON. Adapters never touch the ledger directly;
//! they're called by `rimz hooks <agent>` which owns the ledger writes.
//!
//! Adapters also own hook install and uninstall — translating the trait
//! defaults into whatever per-agent config file the upstream agent reads.

pub mod claude;
pub mod codex;

use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::feed::{AgentMode, AgentStatus, FeedItem, FeedKind, Resolution};

/// Default hook cap used by the Claude and Codex adapters. Both upstreams
/// effectively cap blocking hooks at ~5 minutes today. A per-adapter override
/// is fine when the upstream protocol changes; the default keeps the bridge
/// from ever blocking the agent indefinitely.
pub const DEFAULT_HOOK_CAP: Duration = Duration::from_secs(300);

pub use claude::ClaudeIntegration;
pub use codex::CodexIntegration;

#[derive(Debug, thiserror::Error)]
pub enum AgentErr {
    #[error("unknown agent integration `{0}`")]
    Unknown(String),
    #[error("cannot render decision for {agent}: {reason}")]
    Render { agent: &'static str, reason: String },
    #[error("missing required field `{field}` in {agent} decision")]
    MissingField {
        agent: &'static str,
        field: &'static str,
    },
    #[error("install failed for {agent}: {reason}")]
    Install { agent: &'static str, reason: String },
    #[error("io error installing {agent} hooks at {path}: {source}")]
    InstallIo {
        agent: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("serializing {agent} hook config: {source}")]
    InstallSerialize {
        agent: &'static str,
        #[source]
        source: Box<toml::ser::Error>,
    },
    #[error("parsing existing {agent} hook config at {path}: {source}")]
    InstallParse {
        agent: &'static str,
        path: PathBuf,
        #[source]
        source: Box<toml::de::Error>,
    },
    #[error("serializing {agent} hook config: {source}")]
    InstallSerializeJson {
        agent: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("parsing existing {agent} hook config at {path}: {source}")]
    InstallParseJson {
        agent: &'static str,
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("writing hook config: {0}")]
    WriteHookConfig(#[from] crate::ledger::atomic::AtomicErr),
}

pub type Result<T> = std::result::Result<T, AgentErr>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentHookClass {
    Lifecycle,
    BlockingFeed,
    Telemetry,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassifiedHook {
    pub class: AgentHookClass,
    pub feed_kind: Option<FeedKind>,
    pub event_name: String,
}

/// Status + mode transition observed from a lifecycle hook. Returned by
/// [`AgentIntegration::observe_lifecycle`] so the CLI layer can record an
/// `agent.lifecycle` event without each adapter touching the ledger.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentLifecycleObservation {
    /// Agent-supplied session/process identifier (e.g. Claude `session_id`,
    /// Codex `session_id`). The CLI uses this as the `agent_id`.
    pub agent_id: Option<String>,
    pub status: AgentStatus,
    pub mode: AgentMode,
    /// Optional worktree branch label observed from the payload, surfaced in
    /// the sidebar's worktree grouping.
    pub worktree_branch: Option<String>,
}

/// Result of installing hooks. Surfaced to the CLI so the user sees which
/// files were touched. Serialized verbatim as the `rimz hooks install` JSON
/// output — fields are part of the user-visible report contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HookInstallReport {
    pub agent: &'static str,
    /// Absolute path of the config file the installer wrote.
    pub config_path: PathBuf,
    /// Event names installed (e.g. `SessionStart`, `PermissionRequest`).
    pub installed_events: Vec<String>,
    /// True when the installer wrote into an existing config (merge), false
    /// when the file was created fresh.
    pub merged: bool,
    /// True when telemetry hooks were included.
    pub telemetry: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HookUninstallReport {
    pub agent: &'static str,
    pub config_path: PathBuf,
    pub removed_events: Vec<String>,
    /// True when the config file existed before uninstall.
    pub existed: bool,
}

pub trait AgentIntegration: Send + Sync {
    fn name(&self) -> &'static str;
    fn classify_hook(&self, event_name: &str, payload: &Value) -> ClassifiedHook;
    /// Render the agent-native decision JSON for this resolution. Called
    /// only when the hook is on the bridge and a resolver has answered.
    fn render_decision(&self, item: &FeedItem, resolution: &Resolution) -> Result<Value>;
    /// Render the neutral stdout payload — the "agent's own UI is the answer"
    /// fallback path. `None` means the hook should print nothing on this event.
    fn render_neutral(&self, event_name: &str) -> Result<Option<Value>>;
    /// Maximum time the hook may block on the bridge before falling back to
    /// the neutral payload. Defaults to [`DEFAULT_HOOK_CAP`].
    fn hook_cap(&self) -> Duration {
        DEFAULT_HOOK_CAP
    }

    /// Observe a lifecycle event payload and translate it into the
    /// status/mode transition the ledger should record. Returns `None` when
    /// the event is not a lifecycle event the adapter recognises.
    fn observe_lifecycle(
        &self,
        _event_name: &str,
        _payload: &Value,
    ) -> Option<AgentLifecycleObservation> {
        None
    }

    /// Write or merge the adapter's hook config into the agent's per-user
    /// config file. Telemetry hooks are included when `telemetry` is true.
    /// Defaults to an explicit "not implemented" error until an adapter
    /// owns installation.
    fn install_hooks(&self, _telemetry: bool) -> Result<HookInstallReport> {
        Err(AgentErr::Install {
            agent: self.name(),
            reason: "install not implemented for this adapter".to_owned(),
        })
    }

    /// Remove the adapter's hook entries from the agent's per-user config
    /// file. Defaults to an explicit "not implemented" error.
    fn uninstall_hooks(&self) -> Result<HookUninstallReport> {
        Err(AgentErr::Install {
            agent: self.name(),
            reason: "uninstall not implemented for this adapter".to_owned(),
        })
    }
}

/// Lookup table for `--source <agent>` on the hook CLI.
pub fn integration_by_name(name: &str) -> Result<Box<dyn AgentIntegration>> {
    match name {
        "claude" => Ok(Box::new(ClaudeIntegration)),
        "codex" => Ok(Box::new(CodexIntegration)),
        other => Err(AgentErr::Unknown(other.to_owned())),
    }
}

/// Common helper: does the resolver decision read as an "allow"?
pub(crate) fn choice_is_allow(resolution: &Resolution) -> bool {
    resolution
        .decision
        .get("choice")
        .and_then(Value::as_str)
        .map(|v| matches!(v, "allow" | "yes" | "approve"))
        .unwrap_or(false)
}
