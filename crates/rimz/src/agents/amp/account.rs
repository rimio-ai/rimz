//! Secret-safe Amp account presence probe.

use std::path::{Path, PathBuf};

use crate::agents::account::AccountProbe;
use crate::agents::context::AgentAccount;

pub(crate) fn probe() -> AccountProbe {
    let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) else {
        return AccountProbe::Unavailable;
    };
    probe_path(&PathBuf::from(home).join(".local/share/amp/secrets.json"))
}

fn probe_path(path: &Path) -> AccountProbe {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => AccountProbe::Found(AgentAccount {
            plan: None,
            account_id: None,
            metered: Some(false),
            version: None,
            sub_provider: None,
        }),
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

        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(matches!(probe_path(&path), AccountProbe::Unavailable));
    }
}
