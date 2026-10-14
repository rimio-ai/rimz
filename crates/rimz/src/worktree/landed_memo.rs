//! Commit-pair landed proofs, shared by a repository's checkouts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::{LandedVerdict, content_landed, git_stdout, rev_list_count};
use crate::utils::time::unix_now_ms;

const VERSION: u32 = 1;
const PRUNE_AFTER_MS: u64 = 30 * 24 * 60 * 60 * 1000;

#[derive(Clone, Deserialize, Serialize)]
struct Entry {
    landed: bool,
    ahead: u32,
    seen_at_ms: u64,
}

#[derive(Deserialize, Serialize)]
struct File {
    version: u32,
    entries: HashMap<String, Entry>,
}

impl Default for File {
    fn default() -> Self {
        Self {
            version: VERSION,
            entries: HashMap::new(),
        }
    }
}

pub struct LandedMemo {
    path: Option<PathBuf>,
    entries: Mutex<HashMap<String, Entry>>,
}

impl LandedMemo {
    pub fn load(checkout: &Path) -> Self {
        let path = git_stdout(
            checkout,
            ["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )
        .ok()
        .filter(|_| {
            git_stdout(checkout, ["rev-parse", "--is-shallow-repository"])
                .is_ok_and(|shallow| shallow == "false")
                && git_stdout(
                    checkout,
                    ["for-each-ref", "--format=%(refname)", "refs/replace/"],
                )
                .is_ok_and(|refs| refs.is_empty())
        })
        .map(|dir| PathBuf::from(dir).join("rimz-landed-memo.json"));
        let entries = path.as_deref().map(read).unwrap_or_default().entries;
        Self {
            path,
            entries: Mutex::new(entries),
        }
    }

    pub fn save(&self) {
        let Some(path) = self.path.as_deref() else {
            return;
        };
        let mut file = read(path);
        for (key, entry) in self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
        {
            if file
                .entries
                .get(key)
                .is_none_or(|prior| prior.seen_at_ms <= entry.seen_at_ms)
            {
                file.entries.insert(key.clone(), entry.clone());
            }
        }
        let cutoff = unix_now_ms().saturating_sub(PRUNE_AFTER_MS);
        file.entries.retain(|_, entry| entry.seen_at_ms >= cutoff);
        if let Err(error) = crate::disk::atomic::write_temp_then_rename_cache(path, &file) {
            tracing::debug!(%error, "could not save landed memo");
        }
    }

    pub(super) fn proof(
        &self,
        cwd: &Path,
        comparison: &str,
        head: &str,
    ) -> (LandedVerdict, Option<u32>) {
        let comparison = format!("{comparison}^{{commit}}");
        let head = format!("{head}^{{commit}}");
        let Ok(oids) = git_stdout(
            cwd,
            [
                "rev-parse",
                "--revs-only",
                "--end-of-options",
                &comparison,
                &head,
            ],
        ) else {
            return (LandedVerdict::Unknown, None);
        };
        let mut oids = oids.lines();
        let (Some(comparison), Some(head)) = (oids.next(), oids.next()) else {
            return (LandedVerdict::Unknown, None);
        };
        let key = format!("{comparison}:{head}");
        if self.path.is_some()
            && let Some(entry) = self
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&key)
                .filter(|entry| entry.seen_at_ms >= unix_now_ms().saturating_sub(PRUNE_AFTER_MS))
        {
            return (
                if entry.landed {
                    LandedVerdict::Landed
                } else {
                    LandedVerdict::Unlanded
                },
                Some(entry.ahead),
            );
        }
        let Some(ahead) = rev_list_count(cwd, &format!("{comparison}..{head}")) else {
            return (LandedVerdict::Unknown, None);
        };
        let landed = content_landed(cwd, comparison, head);
        if self.path.is_some() && landed != LandedVerdict::Unknown {
            self.entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(
                    key,
                    Entry {
                        landed: landed == LandedVerdict::Landed,
                        ahead,
                        seen_at_ms: unix_now_ms(),
                    },
                );
        }
        (landed, Some(ahead))
    }
}

fn read(path: &Path) -> File {
    let file: File = crate::disk::atomic::read_json_cache(path);
    if file.version == VERSION {
        file
    } else {
        File::default()
    }
}
