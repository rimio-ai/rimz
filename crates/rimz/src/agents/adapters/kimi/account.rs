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
    expires_at: Option<f64>,
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
        .is_some_and(|token| !token.trim().is_empty());
    let refreshable = shape
        .refresh_token
        .as_deref()
        .is_some_and(|token| !token.trim().is_empty());
    if !has_access && !refreshable {
        return AccountProbe::LoggedOut;
    }
    let access_fresh = has_access
        && shape.expires_at.is_some_and(|seconds| {
            seconds.is_finite() && seconds > jiff::Timestamp::now().as_second() as f64
        });
    if !refreshable && !access_fresh {
        return AccountProbe::LoggedOut;
    }
    AccountProbe::Found(AgentAccount {
        scope: Default::default(),
        plan: Some("Code".to_owned()),
        account_id: None,
        metered: Some(true),
        version: None,
        sub_provider: None,
        credentials_updated_at_ms: crate::agents::account::file_mtime_ms(path),
    })
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

    #[test]
    fn refresh_token_preserves_login_after_access_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kimi-code.json");
        let now = jiff::Timestamp::now().as_second();
        std::fs::write(
            &path,
            format!(
                r#"{{"access_token":"expired","refresh_token":"refresh","expires_at":{}}}"#,
                now - 1
            ),
        )
        .unwrap();
        assert!(matches!(probe_at(&path), AccountProbe::Found(_)));

        std::fs::write(
            &path,
            format!(r#"{{"access_token":"expired","expires_at":{}}}"#, now - 1),
        )
        .unwrap();
        assert!(matches!(probe_at(&path), AccountProbe::LoggedOut));
    }
}
