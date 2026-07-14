//! OpenCode account path and active-provider discovery.

use std::path::PathBuf;

use super::spend::{latest_message_provider, opencode_data_dirs};
use crate::agents::delegated_account::{Adapter, Config};
use crate::agents::{AccountUsageIdentity, AccountUsageProbe};

const ACCOUNT_KEY_DOMAIN: &[u8] = b"rimz/opencode-oauth-account-key/v1";

fn config() -> Config {
    Config {
        adapter: Adapter::OpenCode,
        auth_path: auth_path(),
        used_provider: latest_message_provider(),
        api_key_types: &["api", "api_key"],
        account_key_domain: ACCOUNT_KEY_DOMAIN,
    }
}

pub(crate) fn probe() -> crate::agents::account::AccountProbe {
    crate::agents::delegated_account::probe_account(&config())
}

pub(crate) fn probe_usage() -> AccountUsageProbe {
    crate::agents::delegated_account::probe_account_usage(&config())
}

pub(crate) fn account_usage_identity() -> AccountUsageIdentity {
    crate::agents::delegated_account::account_usage_identity(&config())
}

fn auth_path() -> Option<PathBuf> {
    opencode_data_dirs()
        .into_iter()
        .map(|dir| dir.join("auth.json"))
        .find(|path| path.exists())
}
