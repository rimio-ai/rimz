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
    host: Option<String>,
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
    let config = match crate::agents::jsonc::from_slice::<Config>(&bytes) {
        Ok(config) => config,
        Err(_) => return AccountProbe::Unavailable,
    };
    let Some(account_id) = config
        .last_logged_in_user
        .into_iter()
        .chain(config.logged_in_users)
        .find_map(normalized_identity)
    else {
        return AccountProbe::LoggedOut;
    };
    found_account(account_id, crate::agents::account::file_mtime_ms(path))
}

fn normalized_identity(identity: LoginIdentity) -> Option<String> {
    let login = identity.login?.trim().to_owned();
    if login.is_empty() {
        return None;
    }
    let host = identity
        .host
        .as_deref()
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(normalized_host)
        .unwrap_or_else(|| Some("github.com".to_owned()))?;
    if host == "github.com" {
        Some(login)
    } else {
        Some(format!("{login}@{host}"))
    }
}

pub(super) fn normalized_host(raw: &str) -> Option<String> {
    let authority_and_path = if let Some((scheme, rest)) = raw.split_once("://") {
        matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https").then_some(rest)?
    } else {
        raw
    };
    let authority = authority_and_path
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    if authority.is_empty() || authority.contains('@') || authority.chars().any(char::is_whitespace)
    {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => {
            let port = port.parse::<u16>().ok().filter(|port| *port > 0)?;
            (host, Some(port))
        }
        Some(_) => return None,
        None => (authority, None),
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty()
        || host.split('.').any(|label| {
            label.is_empty()
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
        })
    {
        return None;
    }
    Some(match port {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}

fn found_account(account_id: String, credentials_updated_at_ms: Option<u64>) -> AccountProbe {
    AccountProbe::Found(AgentAccount {
        scope: Default::default(),
        plan: None,
        account_id: Some(account_id),
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

        let account = found(probe_at(&path));
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
                "lastLoggedInUser": {"host":"https://bad host","login":"invalid"},
                "loggedInUsers": [
                    {"host":"github.example","login":"enterprise-user"},
                    {"host":"github.com","login":"second"}
                ]
            }"#,
        )
        .unwrap();

        assert_eq!(
            found(probe_at(&path)).account_id.as_deref(),
            Some("enterprise-user@github.example")
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

    #[test]
    fn host_qualified_identities_do_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            r#"{
                "lastLoggedInUser": {"host":"https://GitHub.Example.:8443/path","login":"octocat"},
                "loggedInUsers": [{"host":"github.com","login":"octocat"}]
            }"#,
        )
        .unwrap();

        assert_eq!(
            found(probe_at(&path)).account_id.as_deref(),
            Some("octocat@github.example:8443")
        );
    }

    #[test]
    fn public_host_forms_are_unqualified() {
        for host in [
            None,
            Some(""),
            Some("GitHub.COM."),
            Some("https://github.com/path"),
        ] {
            assert_eq!(
                normalized_identity(LoginIdentity {
                    host: host.map(str::to_owned),
                    login: Some("octocat".to_owned()),
                })
                .as_deref(),
                Some("octocat")
            );
        }
    }

    #[test]
    fn malformed_hosts_and_empty_logins_are_rejected() {
        for host in [
            "ssh://github.example",
            "https://bad host",
            "user@github.example",
            ".github.example",
            "github.example:0",
            "github.example:not-a-port",
            "[::1]",
        ] {
            assert!(
                normalized_identity(LoginIdentity {
                    host: Some(host.to_owned()),
                    login: Some("octocat".to_owned()),
                })
                .is_none(),
                "accepted malformed host {host}"
            );
        }
        assert!(
            normalized_identity(LoginIdentity {
                host: None,
                login: Some("   ".to_owned()),
            })
            .is_none()
        );
    }
}
