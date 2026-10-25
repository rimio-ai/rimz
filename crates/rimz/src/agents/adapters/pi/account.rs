//! Pi account path and active-provider discovery.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::PiAdapter;
use super::spend::{pi_config_dir, pi_session_roots};
use crate::agents::capabilities::SpendingCapability as _;
use crate::agents::delegated_account::{Adapter, Config};
use crate::agents::{AccountUsageProbe, read_transcript_tail};

const ACCOUNT_KEY_DOMAIN: &[u8] = b"rimz/pi-oauth-account-key/v1";
const INPUTS_STAMP_DOMAIN: &[u8] = b"rimz/pi-account-inputs/v1";

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

pub(super) fn inputs_stamp() -> u64 {
    inputs_stamp_from(&pi_config_dir().join("auth.json"), &pi_session_roots())
}

/// Hashes `auth.json`'s mtime and each session root's entries one level
/// down: Pi creates a session file at its first message, which moves its
/// directory's mtime, while an append moves only the file, which the stamp
/// leaves out. A missing path hashes as absent, so a later login still moves
/// the stamp.
fn inputs_stamp_from(auth: &Path, roots: &[PathBuf]) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(INPUTS_STAMP_DOMAIN);
    hash_modified(&mut hasher, path_mtime(auth));
    for root in roots {
        hash_name(&mut hasher, root.as_os_str());
        hash_modified(&mut hasher, path_mtime(root));
        let mut directories: Vec<_> = std::fs::read_dir(root)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let metadata = std::fs::metadata(entry.path()).ok()?;
                metadata
                    .is_dir()
                    .then(|| (entry.file_name(), mtime(&metadata)))
            })
            .collect();
        directories.sort();
        hasher.update((directories.len() as u64).to_le_bytes());
        for (name, modified) in directories {
            hash_name(&mut hasher, &name);
            hash_modified(&mut hasher, modified);
        }
    }
    let digest = hasher.finalize();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_le_bytes(bytes)
}

fn path_mtime(path: &Path) -> Option<Duration> {
    mtime(&std::fs::metadata(path).ok()?)
}

fn mtime(metadata: &std::fs::Metadata) -> Option<Duration> {
    metadata
        .modified()
        .ok()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
}

fn hash_name(hasher: &mut Sha256, name: &OsStr) {
    let bytes = name.as_encoded_bytes();
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn hash_modified(hasher: &mut Sha256, modified: Option<Duration>) {
    match modified {
        Some(modified) => {
            hasher.update([1]);
            hasher.update(modified.as_nanos().to_le_bytes());
        }
        None => hasher.update([0]),
    }
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
    fn inputs_stamp_moves_on_new_sessions_and_auth_changes_but_not_appends() {
        use std::io::Write as _;

        let dir = tempfile::tempdir().unwrap();
        let auth = dir.path().join("auth.json");
        let root = dir.path().join("sessions");
        let cwd = root.join("--home-project--");
        let session = cwd.join("old.jsonl");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(&auth, "{}").unwrap();
        std::fs::write(&session, "{}\n").unwrap();
        // Age every stamped path, so the next change lands on a distinct
        // mtime whatever the filesystem's timestamp granularity.
        let settle = || {
            for path in [&auth, &root, &cwd]
                .into_iter()
                .filter(|path| path.exists())
            {
                std::fs::File::open(path)
                    .unwrap()
                    .set_modified(SystemTime::now() - Duration::from_secs(3600))
                    .unwrap();
            }
            inputs_stamp_from(&auth, std::slice::from_ref(&root))
        };
        let stamp = || inputs_stamp_from(&auth, std::slice::from_ref(&root));

        let settled = settle();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&session)
            .unwrap()
            .write_all(b"{}\n")
            .unwrap();
        assert_eq!(stamp(), settled, "an append to a session file");

        std::fs::write(cwd.join("new.jsonl"), "{}\n").unwrap();
        assert_ne!(stamp(), settled, "a new session file in a known directory");

        let settled = settle();
        std::fs::create_dir(root.join("--home-other--")).unwrap();
        assert_ne!(stamp(), settled, "a new session directory");

        let settled = settle();
        std::fs::write(&auth, r#"{"openai-codex":{}}"#).unwrap();
        assert_ne!(stamp(), settled, "an auth.json rewrite");

        let settled = settle();
        std::fs::remove_file(&auth).unwrap();
        assert_ne!(stamp(), settled, "an auth.json removal");
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
