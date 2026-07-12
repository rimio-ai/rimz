//! Best-effort Kimi Code OAuth account probe.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::agents::account::AccountProbe;
use crate::agents::context::AgentAccount;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CredentialShape {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_at: Option<serde_json::Value>,
}

pub fn credentials_path() -> PathBuf {
    super::wire::kimi_home().join("credentials/kimi-code.json")
}

pub fn probe() -> AccountProbe {
    probe_at(&credentials_path())
}

pub(crate) fn probe_at(path: &Path) -> AccountProbe {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return AccountProbe::LoggedOut;
        }
        Err(_) => return AccountProbe::Unavailable,
    };
    let Ok(shape) = serde_json::from_slice::<CredentialShape>(&bytes) else {
        return AccountProbe::Unavailable;
    };
    let has_access = shape
        .access_token
        .as_deref()
        .is_some_and(|token| !token.is_empty());
    let refreshable = shape
        .refresh_token
        .as_deref()
        .is_some_and(|token| !token.is_empty());
    if !has_access && !refreshable {
        return AccountProbe::LoggedOut;
    }
    if !refreshable && shape.expires_at.as_ref().is_some_and(expired) {
        return AccountProbe::LoggedOut;
    }
    AccountProbe::Found(AgentAccount {
        plan: Some("Code".to_owned()),
        account_id: None,
        metered: Some(true),
        version: None,
        sub_provider: None,
        credentials_updated_at_ms: crate::agents::account::credentials_updated_at_ms(path),
    })
}

fn expired(value: &serde_json::Value) -> bool {
    let seconds = value
        .as_i64()
        .or_else(|| value.as_f64().map(|value| value as i64))
        .or_else(|| {
            value
                .as_str()
                .and_then(|value| value.parse::<jiff::Timestamp>().ok())
                .map(|value| value.as_second())
        });
    seconds.is_some_and(|seconds| seconds <= jiff::Timestamp::now().as_second())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_login_carries_credential_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kimi-code.json");
        std::fs::write(&path, r#"{"refresh_token":"refresh"}"#).unwrap();
        let AccountProbe::Found(account) = probe_at(&path) else {
            panic!("refreshable credential must report an account");
        };
        assert!(account.credentials_updated_at_ms.is_some());
    }
}
