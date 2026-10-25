//! Pi account path and active-provider discovery.

use std::ffi::OsStr;
use std::io::{BufRead as _, BufReader, Read as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::spend::{pi_config_dir, pi_session_roots};
use crate::agents::delegated_account::{Adapter, Config};
use crate::agents::{AccountUsageProbe, read_transcript_tail};

const ACCOUNT_KEY_DOMAIN: &[u8] = b"rimz/pi-oauth-account-key/v1";
const INPUTS_STAMP_DOMAIN: &[u8] = b"rimz/pi-account-inputs/v1";
const MAX_SESSION_HEADER_BYTES: usize = 1024 * 1024;

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

/// Provider of the freshest root Pi session, tail-scanned newest-first.
fn used_provider() -> Option<String> {
    used_provider_from(&pi_session_roots())
}

fn used_provider_from(roots: &[PathBuf]) -> Option<String> {
    let directories = roots.iter().flat_map(|root| {
        let subdirectories = std::fs::read_dir(root)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                std::fs::metadata(entry.path())
                    .ok()?
                    .is_dir()
                    .then(|| entry.path())
            });
        std::iter::once(root.clone()).chain(subdirectories)
    });
    let mut candidates: Vec<_> = directories
        .flat_map(|directory| std::fs::read_dir(directory).into_iter().flatten().flatten())
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension()? != "jsonl" {
                return None;
            }
            let metadata = std::fs::metadata(&path).ok()?;
            metadata.is_file().then_some((mtime(&metadata)?, path))
        })
        .collect();
    candidates.sort_unstable();
    let (_, newest) = candidates
        .into_iter()
        .rev()
        .find(|(_, path)| !has_parent_session(path))?;
    let tail = read_transcript_tail(&newest)?;
    tail.lines().rev().find_map(provider_of_line).or_else(|| {
        tail.lines()
            .rev()
            .filter_map(|line| serde_json::from_str::<ProviderEntry>(line).ok())
            .filter(|entry| entry.kind.as_deref() == Some("model_change"))
            .find_map(|entry| entry.provider.filter(|provider| !provider.is_empty()))
    })
}

fn has_parent_session(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut first = String::new();
    if BufReader::new(file.take(MAX_SESSION_HEADER_BYTES as u64 + 1))
        .read_line(&mut first)
        .is_err()
        || first.len() > MAX_SESSION_HEADER_BYTES
    {
        return false;
    }
    serde_json::from_str::<SessionHeader>(&first)
        .ok()
        .and_then(|header| header.parent_session)
        .is_some_and(|parent| !parent.is_empty())
}

#[derive(Deserialize)]
struct SessionHeader {
    #[serde(rename = "parentSession")]
    parent_session: Option<String>,
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
        used_provider_from(&[dir.path().to_owned()])
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
        let old = dir.path().join("--cwd--/old.jsonl");
        let newest = dir.path().join("--cwd--/new.jsonl");
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
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
            used_provider_from(&[dir.path().to_owned()]).as_deref(),
            Some("openai-codex")
        );
    }

    fn write_session(
        path: &Path,
        parent: Option<&Path>,
        provider: Option<&str>,
        modified_secs: u64,
    ) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut header = serde_json::json!({"type": "session"});
        if let Some(parent) = parent {
            header["parentSession"] = serde_json::json!(parent);
        }
        let message = serde_json::json!({
            "type": "message",
            "message": {"role": "assistant", "provider": provider}
        });
        std::fs::write(path, format!("{header}\n{message}\n")).unwrap();
        std::fs::File::open(path)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(modified_secs))
            .unwrap();
    }

    #[test]
    fn nicobailon_nested_run_does_not_select_the_account_provider() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("--cwd--/parent.jsonl");
        let child = dir.path().join("--cwd--/parent/run-id/run-0/session.jsonl");
        write_session(&parent, None, Some("anthropic"), 1);
        write_session(&child, None, Some("openai-codex"), 2);

        assert_eq!(
            used_provider_from(&[dir.path().to_owned()]).as_deref(),
            Some("anthropic")
        );
    }

    #[test]
    fn tintinweb_sibling_does_not_select_the_account_provider() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("--cwd--/parent.jsonl");
        let child = dir.path().join("--cwd--/child.jsonl");
        write_session(&parent, None, Some("anthropic"), 1);
        write_session(&child, Some(&parent), Some("openai-codex"), 2);

        assert_eq!(
            used_provider_from(&[dir.path().to_owned()]).as_deref(),
            Some("anthropic")
        );
    }

    #[test]
    fn flat_root_excludes_the_shallowest_nicobailon_child() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("parent.jsonl");
        let child = dir.path().join("parent/async-id/child.jsonl");
        write_session(&parent, None, Some("anthropic"), 1);
        write_session(&child, None, Some("openai-codex"), 2);

        assert_eq!(
            used_provider_from(&[dir.path().to_owned()]).as_deref(),
            Some("anthropic")
        );
    }

    #[test]
    fn newest_root_without_a_provider_does_not_fall_back_to_an_older_session() {
        let dir = tempfile::tempdir().unwrap();
        write_session(
            &dir.path().join("--cwd--/old.jsonl"),
            None,
            Some("anthropic"),
            1,
        );
        write_session(&dir.path().join("--cwd--/new.jsonl"), None, None, 2);

        assert_eq!(used_provider_from(&[dir.path().to_owned()]), None);
    }
}
