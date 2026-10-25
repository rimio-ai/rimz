//! Pi account path and active-provider discovery.

use serde::Deserialize;

use super::PiAdapter;
use super::spend::pi_config_dir;
use crate::agents::capabilities::SpendingCapability as _;
use crate::agents::delegated_account::{Adapter, Config};
use crate::agents::{AccountUsageProbe, read_transcript_tail};

const ACCOUNT_KEY_DOMAIN: &[u8] = b"rimz/pi-oauth-account-key/v1";

fn config() -> Config {
    Config {
        adapter: Adapter::Pi,
        load_credentials: Box::new(|| {
            crate::agents::delegated_account::load_auth_file(
                &pi_config_dir().join("auth.json"),
                Adapter::Pi,
            )
        }),
        used_provider,
        api_key_types: &["api_key"],
        account_key_domain: ACCOUNT_KEY_DOMAIN,
    }
}

pub(super) fn probe() -> crate::agents::account::AccountProbe {
    crate::agents::delegated_account::probe_account(&config())
}

pub(super) fn probe_usage() -> AccountUsageProbe {
    crate::agents::delegated_account::probe_account_usage(&config())
}

/// Provider of the freshest Pi session, tail-scanned newest-first.
fn used_provider() -> Option<String> {
    used_provider_from(PiAdapter.transcript_files(&crate::agents::ambient_env()))
}

fn used_provider_from(files: impl IntoIterator<Item = std::path::PathBuf>) -> Option<String> {
    let (_, newest) = files
        .into_iter()
        .filter_map(|path| {
            let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
            Some((modified, path))
        })
        .max_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)))?;
    let tail = read_transcript_tail(&newest)?;
    tail.lines().rev().find_map(provider_of_line).or_else(|| {
        tail.lines()
            .rev()
            .filter_map(|line| serde_json::from_str::<ProviderEntry>(line).ok())
            .filter(|entry| entry.kind.as_deref() == Some("model_change"))
            .find_map(|entry| entry.provider.filter(|provider| !provider.is_empty()))
    })
}

#[derive(Deserialize)]
struct ProviderEntry {
    #[serde(rename = "type")]
    kind: Option<String>,
    provider: Option<String>,
    message: Option<ProviderMessage>,
}

#[derive(Deserialize)]
struct ProviderMessage {
    provider: Option<String>,
}

fn provider_of_line(line: &str) -> Option<String> {
    if !line.contains(r#""provider""#) {
        return None;
    }
    serde_json::from_str::<ProviderEntry>(line)
        .ok()?
        .message?
        .provider
        .filter(|provider| !provider.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_line_reads_assistant_provider() {
        let line = r#"{"type":"message","message":{"role":"assistant","provider":"openai-codex"}}"#;
        assert_eq!(provider_of_line(line).as_deref(), Some("openai-codex"));
        assert_eq!(provider_of_line(r#"{"type":"session"}"#), None);
    }

    fn session_provider(contents: &str) -> Option<String> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, contents).unwrap();
        used_provider_from([path])
    }

    #[test]
    fn new_session_uses_last_nonempty_model_change() {
        let provider = session_provider(
            r#"{"type":"session"}
{"type":"model_change","provider":"anthropic"}
{"type":"model_change","provider":"openai-codex"}
{"type":"model_change","provider":""}
{"type":"message","message":{"role":"user","content":"hello"}}"#,
        );
        assert_eq!(provider.as_deref(), Some("openai-codex"));
    }

    #[test]
    fn assistant_provider_overrides_initial_model_change() {
        let provider = session_provider(
            r#"{"type":"model_change","provider":"anthropic"}
{"type":"message","message":{"role":"assistant","provider":"openai-codex"}}"#,
        );
        assert_eq!(provider.as_deref(), Some("openai-codex"));
    }

    #[test]
    fn assistant_provider_overrides_later_model_change() {
        let provider = session_provider(
            r#"{"type":"message","message":{"role":"assistant","provider":"anthropic"}}
{"type":"message","message":{"role":"assistant","provider":"openai-codex"}}
{"type":"message","message":{"role":"assistant","provider":""}}
{"type":"model_change","provider":"anthropic"}"#,
        );
        assert_eq!(provider.as_deref(), Some("openai-codex"));
    }

    #[test]
    fn usage_provider_is_not_an_account_provider() {
        assert_eq!(
            session_provider(r#"{"type":"usage","provider":"openai-codex"}"#),
            None
        );
    }

    #[test]
    fn newest_complete_session_selects_the_account_provider() {
        use std::time::{Duration, SystemTime};

        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("history/old.jsonl");
        let newest = dir.path().join("history/deep/new.jsonl");
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::create_dir_all(newest.parent().unwrap()).unwrap();
        std::fs::write(
            &old,
            r#"{"type":"message","message":{"provider":"anthropic"}}"#,
        )
        .unwrap();
        std::fs::write(
            &newest,
            r#"{"type":"message","message":{"provider":"openai-codex"}}"#,
        )
        .unwrap();
        std::fs::File::open(&old)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(60))
            .unwrap();

        assert_eq!(
            used_provider_from([newest, old]).as_deref(),
            Some("openai-codex")
        );
    }
}
