//! Provider-neutral runtime-control and daemon coordination services.

use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeControlReadiness {
    Disabled,
    Ready { host_argv: Option<Vec<String>> },
    Uninstalled(RuntimeControlIssue),
    Blocked(RuntimeControlIssue),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeControlIssue {
    kind: &'static str,
    code: &'static str,
    message: String,
}

impl RuntimeControlIssue {
    pub(crate) fn new(
        kind: &'static str,
        code: &'static str,
        issue: &dyn std::fmt::Display,
    ) -> Self {
        Self {
            kind,
            code,
            message: issue.to_string(),
        }
    }

    #[doc(hidden)]
    pub fn from_parts(kind: &'static str, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            code,
            message: message.into(),
        }
    }

    pub const fn kind(&self) -> &'static str {
        self.kind
    }

    pub const fn code(&self) -> &'static str {
        self.code
    }

    pub fn is_uninstalled_host(&self) -> bool {
        matches!(self.code, "uninstalled" | "standalone_missing")
    }
}

impl std::fmt::Display for RuntimeControlIssue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for RuntimeControlIssue {}

#[derive(Debug, thiserror::Error)]
#[error("{kind} runtime-control transition failed: {message}")]
pub struct RuntimeControlError {
    kind: &'static str,
    message: String,
}

impl RuntimeControlError {
    pub(crate) fn new(kind: &'static str, error: impl std::fmt::Display) -> Self {
        Self {
            kind,
            message: error.to_string(),
        }
    }
}

pub fn readiness(kind: &str, enabled: bool) -> RuntimeControlReadiness {
    super::find_definition(kind).map_or(RuntimeControlReadiness::Disabled, |definition| {
        definition.runtime_control_readiness(enabled)
    })
}

pub fn host_argv(kind: &str) -> Option<Vec<String>> {
    super::find_definition(kind)?.runtime_control_host_argv()
}

pub fn ensure(kind: &str, enabled: bool) {
    if let Some(definition) = super::find_definition(kind) {
        definition.ensure_runtime_control(enabled);
    }
}

pub fn reconcile(kind: &str, enabled: bool) -> Result<(), RuntimeControlError> {
    super::find_definition(kind).map_or(Ok(()), |definition| {
        definition.reconcile_runtime_control(enabled)
    })
}

pub fn updater_advisory(kind: &str) -> Option<String> {
    super::find_definition(kind)?.runtime_control_advisory()
}

pub fn wiring_input_path(kind: &str) -> Option<PathBuf> {
    super::find_definition(kind)?.runtime_control_wiring_input_path()
}

/// Ceiling on one realtime provider account-usage read. A stale socket costs
/// little before the caller falls back to its cached reading.
#[cfg(test)]
pub const MAX_REALTIME_ACCOUNT_USAGE_DURATION: std::time::Duration =
    std::time::Duration::from_secs(10);

/// Host one session's provider broker on `socket_path` until the pane closes.
/// Codex is the only provider with a warm app-server to hold; the entry stays
/// neutral so `cli/` never reaches past the private-adapter boundary.
pub fn serve_broker(
    session_name: Option<&str>,
    socket_path: &std::path::Path,
) -> std::io::Result<()> {
    super::adapters::codex::broker::serve(super::adapters::codex::broker::BrokerInfo {
        session: session_name,
        socket_path,
    })
}
