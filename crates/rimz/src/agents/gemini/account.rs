//! Best-effort Gemini login identity probe from non-secret local metadata.

use std::path::Path;

use serde::Deserialize;

use crate::agents::account::AccountProbe;
use crate::agents::context::AgentAccount;
use crate::agents::transcript_fs::home_dir;

use super::install;

#[derive(Deserialize)]
struct Settings {
    security: Option<Security>,
}

#[derive(Deserialize)]
struct Security {
    auth: Option<Auth>,
}

#[derive(Deserialize)]
struct Auth {
    #[serde(rename = "selectedType")]
    selected_type: Option<String>,
}

#[derive(Deserialize)]
struct GoogleAccounts {
    active: Option<String>,
}

pub(super) fn probe() -> AccountProbe {
    let Ok(settings) = install::settings_path() else {
        return AccountProbe::LoggedOut;
    };
    probe_at(&settings, &home_dir().join(".gemini/google_accounts.json"))
}

fn probe_at(settings_path: &Path, accounts_path: &Path) -> AccountProbe {
    let Ok(settings) = read_json::<Settings>(settings_path) else {
        return AccountProbe::LoggedOut;
    };
    let Some(auth_type) = settings
        .security
        .and_then(|security| security.auth)
        .and_then(|auth| auth.selected_type)
        .filter(|auth_type| !auth_type.trim().is_empty())
    else {
        return AccountProbe::LoggedOut;
    };

    let (plan, account_id, metered, credential_path) = match auth_type.as_str() {
        "oauth-personal" => {
            let Ok(accounts) = read_json::<GoogleAccounts>(accounts_path) else {
                return AccountProbe::LoggedOut;
            };
            let Some(active) = accounts.active.filter(|active| !active.trim().is_empty()) else {
                return AccountProbe::LoggedOut;
            };
            ("OAuth".to_owned(), Some(active), Some(true), accounts_path)
        }
        "gemini-api-key" => ("API Key".to_owned(), None, Some(false), settings_path),
        "vertex-ai" => ("Vertex AI".to_owned(), None, Some(false), settings_path),
        "cloud-shell" => ("Cloud Shell".to_owned(), None, None, settings_path),
        "compute-default-credentials" => (
            "Compute Default Credentials".to_owned(),
            None,
            None,
            settings_path,
        ),
        "gateway" => ("Gateway".to_owned(), None, None, settings_path),
        other => (other.to_owned(), None, None, settings_path),
    };
    AccountProbe::Found(AgentAccount {
        plan: Some(plan),
        account_id,
        metered,
        version: None,
        sub_provider: None,
        credentials_updated_at_ms: crate::agents::account::credentials_updated_at_ms(
            credential_path,
        ),
    })
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, ()> {
    std::fs::read(path)
        .map_err(|_| ())
        .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|_| ()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(probe: AccountProbe) -> AgentAccount {
        match probe {
            AccountProbe::Found(account) => account,
            other => panic!("expected account, got {other:?}"),
        }
    }

    #[test]
    fn oauth_uses_active_non_secret_identity() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        let accounts = dir.path().join("google_accounts.json");
        std::fs::write(
            &settings,
            r#"{"security":{"auth":{"selectedType":"oauth-personal"}}}"#,
        )
        .unwrap();
        std::fs::write(&accounts, r#"{"active":"user@example.com"}"#).unwrap();
        let account = found(probe_at(&settings, &accounts));
        assert_eq!(account.plan.as_deref(), Some("OAuth"));
        assert_eq!(account.account_id.as_deref(), Some("user@example.com"));
        assert_eq!(account.metered, Some(true));
        assert!(account.credentials_updated_at_ms.is_some());
    }

    #[test]
    fn api_key_does_not_require_account_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        std::fs::write(
            &settings,
            r#"{"security":{"auth":{"selectedType":"gemini-api-key"}}}"#,
        )
        .unwrap();
        let account = found(probe_at(&settings, &dir.path().join("missing.json")));
        assert_eq!(account.plan.as_deref(), Some("API Key"));
        assert_eq!(account.metered, Some(false));
        assert!(account.credentials_updated_at_ms.is_some());
    }

    #[test]
    fn missing_or_malformed_state_is_logged_out() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.json");
        assert!(matches!(
            probe_at(&missing, &missing),
            AccountProbe::LoggedOut
        ));
        let settings = dir.path().join("settings.json");
        std::fs::write(&settings, "not json").unwrap();
        assert!(matches!(
            probe_at(&settings, &missing),
            AccountProbe::LoggedOut
        ));
    }
}
