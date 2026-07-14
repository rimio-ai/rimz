//! Secret-safe Copilot login-identity probe from local application state.

use std::path::Path;

use serde::Deserialize;

use crate::agents::account::AccountProbe;
use crate::agents::context::AgentAccount;

use super::oauth_usage::GitHubHost;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    last_logged_in_user: Option<LoginIdentity>,
    #[serde(default)]
    logged_in_users: Vec<LoginIdentity>,
}

#[derive(Deserialize)]
struct LoginIdentity {
    host: Option<String>,
    login: Option<String>,
}

pub(super) fn probe() -> AccountProbe {
    probe_home(
        super::paths::copilot_home().as_deref(),
        super::oauth_usage::has_environment_token(),
    )
}

fn probe_home(home: Option<&Path>, environment_token: bool) -> AccountProbe {
    let Some(home) = home else {
        return token_account(environment_token);
    };
    probe_at(&home.join("config.json"), environment_token)
}

fn probe_at(path: &Path, environment_token: bool) -> AccountProbe {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return token_account(environment_token);
        }
        Err(_) if environment_token => return token_account(true),
        Err(_) => return AccountProbe::Unavailable,
    };
    let config = match crate::agents::jsonc::from_slice::<Config>(&bytes) {
        Ok(config) => config,
        Err(_) if environment_token => return token_account(true),
        Err(_) => return AccountProbe::Unavailable,
    };
    let account_id = config
        .last_logged_in_user
        .into_iter()
        .chain(config.logged_in_users)
        .find_map(normalized_identity);
    if account_id.is_none() && !environment_token {
        return AccountProbe::LoggedOut;
    }
    let credentials_updated_at_ms = account_id
        .as_ref()
        .and_then(|_| crate::agents::account::credentials_updated_at_ms(path));
    found_account(account_id, credentials_updated_at_ms)
}

fn normalized_identity(identity: LoginIdentity) -> Option<String> {
    let login = identity.login?.trim().to_owned();
    if login.is_empty() {
        return None;
    }
    let host = match identity.host.as_deref().map(str::trim) {
        None | Some("") => GitHubHost::public(),
        Some(host) => GitHubHost::parse(host).ok()?,
    };
    if host.as_str() == "github.com" {
        Some(login)
    } else {
        Some(format!("{login}@{}", host.as_str()))
    }
}

fn token_account(present: bool) -> AccountProbe {
    if !present {
        return AccountProbe::LoggedOut;
    }
    found_account(None, None)
}

fn found_account(
    account_id: Option<String>,
    credentials_updated_at_ms: Option<u64>,
) -> AccountProbe {
    AccountProbe::Found(AgentAccount {
        scope: Default::default(),
        plan: None,
        account_id,
        metered: Some(true),
        version: None,
        sub_provider: None,
        credentials_updated_at_ms,
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

        let account = found(probe_at(&path, false));
        assert_eq!(account.account_id.as_deref(), Some("octocat"));
        assert_eq!(account.plan, None);
        assert_eq!(account.metered, Some(true));
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
            found(probe_at(&path, false)).account_id.as_deref(),
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
                "lastLoggedInUser": {"host":"https://bad host","login":"invalid"},
                "loggedInUsers": [
                    {"host":"github.example","login":"enterprise-user"},
                    {"host":"github.com","login":"second"}
                ]
            }"#,
        )
        .unwrap();

        assert_eq!(
            found(probe_at(&path, false)).account_id.as_deref(),
            Some("enterprise-user@github.example")
        );
    }

    #[test]
    fn empty_or_missing_identity_is_logged_out() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"loggedInUsers":[]}"#).unwrap();

        assert!(matches!(probe_at(&path, false), AccountProbe::LoggedOut));
        assert!(matches!(
            probe_at(&dir.path().join("missing.json"), false),
            AccountProbe::LoggedOut
        ));
        assert!(matches!(probe_home(None, false), AccountProbe::LoggedOut));
    }

    #[test]
    fn corrupt_or_unreadable_config_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, "not json").unwrap();
        assert!(matches!(probe_at(&path, false), AccountProbe::Unavailable));

        let directory = dir.path().join("config-directory");
        std::fs::create_dir(&directory).unwrap();
        assert!(matches!(
            probe_at(&directory, false),
            AccountProbe::Unavailable
        ));
    }

    #[test]
    fn host_qualified_identities_do_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            r#"{
                "lastLoggedInUser": {"host":"https://GitHub.Example/path","login":"octocat"},
                "loggedInUsers": [{"host":"github.com","login":"octocat"}]
            }"#,
        )
        .unwrap();

        assert_eq!(
            found(probe_at(&path, false)).account_id.as_deref(),
            Some("octocat@github.example")
        );
    }

    #[test]
    fn documented_environment_token_creates_a_metered_identityless_account() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.json");
        let account = found(probe_at(&missing, true));
        assert_eq!(account.account_id, None);
        assert_eq!(account.metered, Some(true));
        assert_eq!(account.credentials_updated_at_ms, None);

        std::fs::write(&missing, "not json").unwrap();
        let account = found(probe_at(&missing, true));
        assert_eq!(account.account_id, None);
        assert_eq!(account.metered, Some(true));

        std::fs::write(&missing, r#"{"loggedInUsers":[]}"#).unwrap();
        let account = found(probe_at(&missing, true));
        assert_eq!(account.account_id, None);
        assert_eq!(account.credentials_updated_at_ms, None);
    }
}
