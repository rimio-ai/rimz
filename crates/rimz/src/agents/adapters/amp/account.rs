//! Secret-safe Amp account presence probe.

use std::path::{Path, PathBuf};

use crate::agents::account::AccountProbe;
use crate::agents::context::AgentAccount;

pub(crate) fn probe() -> AccountProbe {
    let api_key_present = std::env::var_os("AMP_API_KEY").is_some_and(|value| !value.is_empty());
    let secret_path = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|home| home.join(".local/share/amp/secrets.json"));
    probe_sources(api_key_present, secret_path.as_deref())
}

fn probe_sources(api_key_present: bool, secret_path: Option<&Path>) -> AccountProbe {
    if api_key_present {
        return AccountProbe::Found(pay_per_use_account(None));
    }
    secret_path.map_or(AccountProbe::Unavailable, probe_path)
}

fn pay_per_use_account(credentials_updated_at_ms: Option<u64>) -> AgentAccount {
    AgentAccount {
        scope: Default::default(),
        plan: None,
        account_id: None,
        metered: Some(false),
        version: None,
        sub_provider: None,
        credentials_updated_at_ms,
    }
}

fn probe_path(path: &Path) -> AccountProbe {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => {
            let credentials_updated_at_ms = crate::agents::account::file_mtime_ms(path);
            AccountProbe::Found(pay_per_use_account(credentials_updated_at_ms))
        }
        Ok(_) => AccountProbe::Unavailable,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => AccountProbe::LoggedOut,
        Err(_) => AccountProbe::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presence_probe_never_reads_secret_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        assert!(matches!(probe_path(&path), AccountProbe::LoggedOut));

        std::fs::write(&path, b"not json and deliberately unreadable as data").unwrap();
        let AccountProbe::Found(account) = probe_path(&path) else {
            panic!("present credential file must report an account");
        };
        assert_eq!(account.plan, None);
        assert_eq!(account.metered, Some(false));
        assert!(account.credentials_updated_at_ms.is_some());

        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(matches!(probe_path(&path), AccountProbe::Unavailable));
    }

    #[test]
    fn environment_key_is_a_login_without_a_home_directory() {
        let AccountProbe::Found(account) = probe_sources(true, None) else {
            panic!("AMP_API_KEY must report a pay-per-use account");
        };
        assert_eq!(account.metered, Some(false));
        assert_eq!(account.credentials_updated_at_ms, None);
        assert!(matches!(
            probe_sources(false, None),
            AccountProbe::Unavailable
        ));
    }
}
