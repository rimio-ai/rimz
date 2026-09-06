//! The PR-state record the sidebar's forge probe publishes and harness policy reads.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::store::snapshot::{WorktreePrCi, WorktreePrState};

#[derive(Debug, Default, Clone, Serialize)]
pub struct PrStateCache {
    /// PR link by absolute worktree path.
    #[serde(default)]
    pub states: BTreeMap<String, PrLink>,
    /// Commit-level CI verdict for paths without a PR link.
    #[serde(default)]
    pub branch_ci: BTreeMap<String, WorktreePrCi>,
    /// Probe freshness by origin repo key.
    #[serde(default)]
    pub(crate) repos: BTreeMap<String, RepoProbe>,
    /// Last HEAD SHA observed when a worktree's repo was probed.
    #[serde(default)]
    pub(crate) head_seen: BTreeMap<String, String>,
    /// Last repo classification seen for a worktree. Supported repos store
    /// their repo key; unsupported/undiscoverable paths store an empty marker.
    /// This preserves the no-fork fresh path: per-repo TTL can be evaluated
    /// before shelling out for branch/remote metadata.
    #[serde(default)]
    pub(crate) path_repos: BTreeMap<String, String>,
    /// Branch/incarnation identity observed for each supported worktree.
    #[serde(default)]
    pub(crate) target_seen: BTreeMap<String, TargetStamp>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TargetStamp {
    pub(crate) branch: String,
    #[serde(default)]
    pub(crate) incarnation: Option<jiff::Timestamp>,
}

impl TargetStamp {
    pub(crate) fn owns_link(&self, link: &PrLink) -> bool {
        Self::owns(&self.branch, self.incarnation, link)
    }

    pub(crate) fn owns(branch: &str, incarnation: Option<jiff::Timestamp>, link: &PrLink) -> bool {
        link.branch.as_deref() == Some(branch) && link.incarnation == incarnation
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrLink {
    /// Head branch this link was resolved for. Legacy path-only links have no
    /// stamp and are re-resolved before reuse.
    #[serde(default)]
    pub branch: Option<String>,
    /// RimZ worktree creation time this link was resolved for. Legacy links
    /// have no stamp and are re-resolved before reuse by managed worktrees.
    #[serde(default)]
    pub incarnation: Option<jiff::Timestamp>,
    pub state: WorktreePrState,
    #[serde(default)]
    pub number: Option<u64>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub ci: Option<WorktreePrCi>,
    #[serde(default)]
    pub merge_sha: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RepoProbe {
    pub refreshed_at_ms: u64,
    /// Whether the last repo probe completed without an infrastructure
    /// failure. A logged-in repo with no PR is a success and keeps an empty map.
    #[serde(default = "pr_state_probe_ok_default")]
    pub ok: bool,
    /// Consecutive failed probes, for escalating retry backoff on deterministic
    /// forge CLI failures.
    #[serde(default)]
    pub consecutive_failures: u32,
}

impl Default for RepoProbe {
    fn default() -> Self {
        Self {
            refreshed_at_ms: 0,
            ok: true,
            consecutive_failures: 0,
        }
    }
}

impl<'de> Deserialize<'de> for PrStateCache {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct DiskCache {
            #[serde(default)]
            states: BTreeMap<String, PrLink>,
            #[serde(default)]
            branch_ci: Option<BTreeMap<String, WorktreePrCi>>,
            #[serde(default)]
            repos: BTreeMap<String, RepoProbe>,
            #[serde(default)]
            head_seen: BTreeMap<String, String>,
            #[serde(default)]
            path_repos: BTreeMap<String, String>,
            #[serde(default)]
            target_seen: BTreeMap<String, TargetStamp>,
        }

        let disk = DiskCache::deserialize(deserializer)?;
        let legacy = disk.branch_ci.is_none();
        let mut cache = Self {
            states: disk.states,
            branch_ci: disk.branch_ci.unwrap_or_default(),
            repos: disk.repos,
            head_seen: disk.head_seen,
            path_repos: disk.path_repos,
            target_seen: disk.target_seen,
        };
        if legacy {
            // Old freshness stamps predate branch CI, and trunk paths were
            // classified as unsupported. Reclassify every path once so all
            // no-PR branches enter the commit probe without waiting for TTL.
            cache.repos.clear();
            cache.head_seen.clear();
            cache.path_repos.clear();
            cache.target_seen.clear();
        }
        Ok(cache)
    }
}

fn pr_state_probe_ok_default() -> bool {
    true
}

pub fn read_pr_state_cache(path: &Path) -> PrStateCache {
    crate::disk::atomic::read_json_cache(path)
}
