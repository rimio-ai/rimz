//! Secret-safe Copilot login-identity probe from local application state.

use std::path::Path;

use serde::Deserialize;

use crate::agents::account::AccountProbe;
use crate::agents::context::AgentAccount;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    last_logged_in_user: Option<LoginIdentity>,
    #[serde(default)]
    logged_in_users: Vec<LoginIdentity>,
}

#[derive(Deserialize)]
struct LoginIdentity {
    #[serde(rename = "host")]
    _host: Option<String>,
    login: Option<String>,
}

pub(super) fn probe() -> AccountProbe {
    probe_home(super::paths::copilot_home().as_deref())
}

fn probe_home(home: Option<&Path>) -> AccountProbe {
    let Some(home) = home else {
        return AccountProbe::LoggedOut;
    };
    probe_at(&home.join("config.json"))
}

fn probe_at(path: &Path) -> AccountProbe {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return AccountProbe::LoggedOut;
        }
        Err(_) => return AccountProbe::Unavailable,
    };
    let Ok(config) = crate::agents::jsonc::from_slice::<Config>(&bytes) else {
        return AccountProbe::Unavailable;
    };
    let login = config
        .last_logged_in_user
        .into_iter()
        .chain(config.logged_in_users)
        .filter_map(|identity| identity.login)
        .find(|login| !login.trim().is_empty());
    let Some(account_id) = login else {
        return AccountProbe::LoggedOut;
    };
    AccountProbe::Found(AgentAccount {
        plan: None,
        account_id: Some(account_id),
        metered: None,
        version: None,
        sub_provider: None,
        credentials_updated_at_ms: crate::agents::account::credentials_updated_at_ms(path),
    })
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
    fn logged_in_config_uses_last_identity_and_ignores_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            r#"{
                "lastLoggedInUser": {"host":"github.com","login":"octocat"},
                "loggedInUsers": [{"host":"github.com","login":"fallback"}],
                "copilotTokens": {"github.com":"secret-token-material"}
            }"#,
        )
        .unwrap();

        let account = found(probe_at(&path));
        assert_eq!(account.account_id.as_deref(), Some("octocat"));
        assert_eq!(account.plan, None);
        assert_eq!(account.metered, None);
        assert!(account.credentials_updated_at_ms.is_some());
    }

    #[test]
    fn commented_config_with_trailing_commas_resolves_login() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            r#"{
                // Copilot writes JSONC-compatible user configuration.
                "lastLoggedInUser": {"host":"github.com","login":"octocat",},
                "loggedInUsers": [],
            }"#,
        )
        .unwrap();

        assert_eq!(
            found(probe_at(&path)).account_id.as_deref(),
            Some("octocat")
        );
    }

    #[test]
    fn first_logged_in_user_is_the_fallback_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            r#"{
                "lastLoggedInUser": {"host":"github.com","login":""},
                "loggedInUsers": [
                    {"host":"github.example","login":"enterprise-user"},
                    {"host":"github.com","login":"second"}
                ]
            }"#,
        )
        .unwrap();

        assert_eq!(
            found(probe_at(&path)).account_id.as_deref(),
            Some("enterprise-user")
        );
    }

    #[test]
    fn empty_or_missing_identity_is_logged_out() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"loggedInUsers":[]}"#).unwrap();

        assert!(matches!(probe_at(&path), AccountProbe::LoggedOut));
        assert!(matches!(
            probe_at(&dir.path().join("missing.json")),
            AccountProbe::LoggedOut
        ));
        assert!(matches!(probe_home(None), AccountProbe::LoggedOut));
    }

    #[test]
    fn corrupt_or_unreadable_config_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, "not json").unwrap();
        assert!(matches!(probe_at(&path), AccountProbe::Unavailable));

        let directory = dir.path().join("config-directory");
        std::fs::create_dir(&directory).unwrap();
        assert!(matches!(probe_at(&directory), AccountProbe::Unavailable));
    }
}
