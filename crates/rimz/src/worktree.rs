//! Rimz-owned git worktree lifecycle.
//!
//! Worktrees are identified by a marker stored in the linked worktree's git
//! admin directory, not in the checkout. The checkout remains pristine, and
//! cleanup only ever removes marked worktrees.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::{WorktreeBase, WorktreeConfig};
use crate::forge;

const MARKER_FILE: &str = "rimz-worktree.json";
const MARKER_VERSION: u32 = 3;
const LANDED_BASE_SCAN_CAP: u32 = 500;
const AUTO_ADJECTIVES: &[&str] = &[
    "brisk", "calm", "clear", "daring", "fleet", "fresh", "keen", "lively", "nimble", "quiet",
    "rapid", "ready", "sharp", "steady", "swift", "vivid",
];
const AUTO_NOUNS: &[&str] = &[
    "anchor", "bridge", "cedar", "delta", "ember", "field", "harbor", "ion", "juniper", "keel",
    "lantern", "meadow", "north", "orbit", "pilot", "quartz",
];

#[derive(Debug, thiserror::Error)]
pub enum WorktreeErr {
    #[error("rimz worktrees require a git repository; run from a repo checkout")]
    NotRepo,
    #[error("invalid worktree name `{0}`; use letters, numbers, `_`, or `-`")]
    InvalidName(String),
    #[error("worktree `{name}` already exists at {path}")]
    Exists { name: String, path: PathBuf },
    #[error("worktree `{name}` is not a Rimz-managed worktree at {path}")]
    Unmarked { name: String, path: PathBuf },
    #[error(
        "worktree `{name}` has local changes or work not proven landed; use --force to remove it"
    )]
    Dirty { name: String },
    #[error("git command failed in {cwd}: git {args}: {stderr}")]
    Git {
        cwd: PathBuf,
        args: String,
        stderr: String,
    },
    #[error("could not fetch PR #{number} from {remote}: {stderr}")]
    PrFetch {
        number: u64,
        remote: String,
        stderr: String,
    },
    #[error("could not parse git output: {0}")]
    Parse(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Atomic(#[from] crate::ledger::atomic::AtomicErr),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, WorktreeErr>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeMarker {
    pub version: u32,
    pub name: String,
    pub branch: String,
    #[serde(default)]
    pub base_branch: Option<String>,
    pub base_ref: String,
    pub repo_root: PathBuf,
    pub worktree_path: PathBuf,
    pub created_at: jiff::Timestamp,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CreatedWorktree {
    pub name: String,
    pub path: PathBuf,
    pub branch: String,
    pub base_branch: Option<String>,
    pub base_ref: String,
    pub reused: bool,
    /// Files copied into the worktree from the project's `.worktreeinclude`.
    /// Zero for a reused worktree, which is never re-seeded.
    pub included: usize,
    /// Directories symlinked into the worktree from the project's `.worktreelink`.
    /// Zero for a reused worktree, which is never re-seeded.
    pub linked: usize,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct WorktreeListEntry {
    pub name: String,
    pub path: PathBuf,
    pub branch: Option<String>,
    pub base_ref: String,
    pub dirty: bool,
    pub landed: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeRow {
    pub path: PathBuf,
    pub branch: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorktreeStatus {
    pub dirty: bool,
    pub landed: LandedVerdict,
}

impl Default for WorktreeStatus {
    fn default() -> Self {
        Self {
            dirty: false,
            landed: LandedVerdict::Landed,
        }
    }
}

impl WorktreeStatus {
    pub const fn safe_to_remove(self) -> bool {
        !self.dirty && self.landed.is_landed()
    }

    pub const fn unknown() -> Self {
        Self {
            dirty: false,
            landed: LandedVerdict::Unknown,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LandedVerdict {
    Landed,
    Pending,
    Unknown,
}

impl LandedVerdict {
    pub const fn is_landed(self) -> bool {
        matches!(self, Self::Landed)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CleanupDecision {
    RemoveClean,
    PromptDirty,
    Skip,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BranchDeletion {
    Deleted,
    KeptUnmerged,
}

pub fn create(
    repo_root: &Path,
    config: &WorktreeConfig,
    name: Option<&str>,
    base: Option<WorktreeBase>,
    branch: Option<&str>,
    reuse_existing: bool,
) -> Result<CreatedWorktree> {
    ensure_repo(repo_root)?;
    let FreshWorktree { name, path } =
        match resolve_fresh_worktree(repo_root, config, name, None, reuse_existing)? {
            WorktreeCreateTarget::Fresh(fresh) => fresh,
            WorktreeCreateTarget::Reuse(reused) => return Ok(reused),
        };

    let base = base.unwrap_or_else(|| config.base.clone());
    let checkout_base_ref = base.as_refspec().to_owned();
    let base_ref = resolve_base_commit(repo_root, &checkout_base_ref)?;
    let base_branch = resolve_base_branch(repo_root, &base);
    add_and_seed(
        repo_root,
        name,
        path,
        branch,
        base_branch,
        base_ref,
        &checkout_base_ref,
    )
}

pub fn create_from_pr(
    repo_root: &Path,
    config: &WorktreeConfig,
    pr: &forge::PrTarget,
    name: Option<&str>,
    branch: Option<&str>,
    reuse_existing: bool,
) -> Result<CreatedWorktree> {
    ensure_repo(repo_root)?;
    let default_name = format!("pr-{}", pr.number);
    let FreshWorktree { name, path } = match resolve_fresh_worktree(
        repo_root,
        config,
        name,
        Some(default_name.as_str()),
        reuse_existing,
    )? {
        WorktreeCreateTarget::Fresh(fresh) => fresh,
        WorktreeCreateTarget::Reuse(reused) => return Ok(reused),
    };

    let remote =
        git_stdout(repo_root, ["remote", "get-url", "origin"]).map_err(|err| match err {
            WorktreeErr::Git { .. } => WorktreeErr::Parse(format!(
                "could not fetch PR #{}: git remote `origin` is not configured",
                pr.number
            )),
            other => other,
        })?;
    let forge = pr.forge.unwrap_or_else(|| forge::forge_for_remote(&remote));
    let refspec = forge.pr_refspec(pr.number);
    git_run(repo_root, ["fetch", "origin", refspec.as_str()]).map_err(|err| match err {
        WorktreeErr::Git { stderr, .. } => WorktreeErr::PrFetch {
            number: pr.number,
            remote: remote.clone(),
            stderr,
        },
        other => other,
    })?;
    let pr_head = git_stdout(repo_root, ["rev-parse", "FETCH_HEAD"])?;
    let base_branch = trunk_ref(repo_root);
    let base_ref_name = base_branch.as_deref().unwrap_or("origin/HEAD");
    let base_ref =
        resolve_base_commit(repo_root, base_ref_name).unwrap_or_else(|_| pr_head.clone());
    add_and_seed(
        repo_root,
        name,
        path,
        branch,
        base_branch,
        base_ref,
        pr_head.as_str(),
    )
}

pub fn remove(
    repo_root: &Path,
    config: &WorktreeConfig,
    name: &str,
    force: bool,
) -> Result<BranchDeletion> {
    ensure_repo(repo_root)?;
    validate_name(name)?;
    let path = worktree_path(repo_root, config, name)?;
    let marker = read_marker_for_worktree(&path)?.ok_or_else(|| WorktreeErr::Unmarked {
        name: name.to_owned(),
        path: path.clone(),
    })?;
    let status = status(&path, &marker)?;
    if !force && !status.safe_to_remove() {
        return Err(WorktreeErr::Dirty {
            name: name.to_owned(),
        });
    }
    remove_marked_worktree(repo_root, &path, &marker, force)
}

pub fn remove_marked_worktree(
    repo_root: &Path,
    path: &Path,
    marker: &WorktreeMarker,
    force: bool,
) -> Result<BranchDeletion> {
    ensure_repo(repo_root)?;
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    let path_arg = path.to_string_lossy();
    args.push(path_arg.as_ref());
    git_run(repo_root, args)?;
    delete_branch(repo_root, marker, force)
}

pub fn list(repo_root: &Path) -> Result<Vec<WorktreeListEntry>> {
    ensure_repo(repo_root)?;
    let rows = parse_worktree_list(&git_stdout(repo_root, ["worktree", "list", "--porcelain"])?);
    let mut entries = Vec::new();
    for row in rows {
        let Some(marker) = read_marker_for_worktree(&row.path)? else {
            continue;
        };
        let status = status(&row.path, &marker).unwrap_or_else(|_| WorktreeStatus::unknown());
        entries.push(WorktreeListEntry {
            name: marker.name,
            path: row.path,
            branch: row.branch,
            base_ref: marker.base_ref,
            dirty: status.dirty,
            landed: landed_json(status.landed),
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

pub fn prune(repo_root: &Path) -> Result<()> {
    ensure_repo(repo_root)?;
    git_run(repo_root, ["worktree", "prune"])
}

pub fn status(worktree: &Path, marker: &WorktreeMarker) -> Result<WorktreeStatus> {
    let porcelain = git_stdout(worktree, ["status", "--porcelain"])?;
    let landed = comparison_ref(worktree, marker)
        .map(|comparison| content_landed(worktree, &comparison, "HEAD"))
        .unwrap_or(LandedVerdict::Unknown);
    Ok(WorktreeStatus {
        dirty: !porcelain.trim().is_empty(),
        landed,
    })
}

pub fn cleanup_decision(
    status: WorktreeStatus,
    marker_present: bool,
    other_pane_inside: bool,
) -> CleanupDecision {
    if !marker_present || other_pane_inside {
        return CleanupDecision::Skip;
    }
    if status.safe_to_remove() {
        CleanupDecision::RemoveClean
    } else {
        CleanupDecision::PromptDirty
    }
}

pub fn sweepable_worktrees(
    rows: &[WorktreeRow],
    marked: &BTreeSet<PathBuf>,
    live_cwds: &[PathBuf],
    statuses: &BTreeMap<PathBuf, WorktreeStatus>,
) -> Vec<PathBuf> {
    rows.iter()
        .filter(|row| marked.contains(&row.path))
        .filter(|row| !live_cwds.iter().any(|cwd| path_inside(cwd, &row.path)))
        .filter(|row| {
            statuses
                .get(&row.path)
                .is_some_and(|status| status.safe_to_remove())
        })
        .map(|row| row.path.clone())
        .collect()
}

pub fn parse_worktree_list(raw: &str) -> Vec<WorktreeRow> {
    let mut rows = Vec::new();
    let mut path: Option<PathBuf> = None;
    let mut branch: Option<String> = None;
    for line in raw.lines().chain(std::iter::once("")) {
        let line = line.trim();
        if line.is_empty() {
            if let Some(path) = path.take() {
                rows.push(WorktreeRow {
                    path,
                    branch: branch.take(),
                });
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(rest));
        } else if let Some(rest) = line.strip_prefix("branch ") {
            branch = Some(rest.strip_prefix("refs/heads/").unwrap_or(rest).to_owned());
        }
    }
    rows
}

pub fn worktree_parent(repo_root: &Path, config: &WorktreeConfig) -> Result<PathBuf> {
    let repo = repo_root
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| WorktreeErr::Parse("repo root has no basename".to_owned()))?;
    let expanded = config.dir.replace("{repo}", repo);
    let path = PathBuf::from(expanded);
    Ok(if path.is_absolute() {
        path
    } else {
        repo_root.join(path)
    })
}

pub fn worktree_path(repo_root: &Path, config: &WorktreeConfig, name: &str) -> Result<PathBuf> {
    validate_name(name)?;
    Ok(worktree_parent(repo_root, config)?.join(name))
}

pub fn read_marker_for_worktree(path: &Path) -> Result<Option<WorktreeMarker>> {
    let marker = match marker_path(path) {
        Ok(path) => path,
        Err(WorktreeErr::Git { .. }) | Err(WorktreeErr::Io(_)) => return Ok(None),
        Err(err) => return Err(err),
    };
    match std::fs::read_to_string(&marker) {
        Ok(text) => serde_json::from_str(&text).map(Some).map_err(Into::into),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

pub fn marker_path(worktree: &Path) -> Result<PathBuf> {
    let git_dir = git_stdout(worktree, ["rev-parse", "--git-dir"])?;
    let path = PathBuf::from(git_dir.trim());
    Ok(if path.is_absolute() {
        path
    } else {
        worktree.join(path)
    }
    .join(MARKER_FILE))
}

pub fn path_inside(path: &Path, parent: &Path) -> bool {
    path == parent || path.starts_with(parent)
}

pub fn normalize_path_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

struct FreshWorktree {
    name: String,
    path: PathBuf,
}

enum WorktreeCreateTarget {
    Fresh(FreshWorktree),
    Reuse(CreatedWorktree),
}

fn resolve_fresh_worktree(
    repo_root: &Path,
    config: &WorktreeConfig,
    name: Option<&str>,
    default_name: Option<&str>,
    reuse_existing: bool,
) -> Result<WorktreeCreateTarget> {
    let name = match (name, default_name) {
        (Some(raw), _) | (None, Some(raw)) => {
            validate_name(raw)?;
            raw.to_owned()
        }
        (None, None) => available_auto_name(repo_root, config)?,
    };
    let path = worktree_path(repo_root, config, &name)?;
    if path.exists() {
        if reuse_existing {
            let marker = read_marker_for_worktree(&path)?.ok_or_else(|| WorktreeErr::Unmarked {
                name: name.clone(),
                path: path.clone(),
            })?;
            return Ok(WorktreeCreateTarget::Reuse(CreatedWorktree {
                name,
                path,
                branch: marker.branch,
                base_branch: marker.base_branch,
                base_ref: marker.base_ref,
                reused: true,
                included: 0,
                linked: 0,
            }));
        }
        return Err(WorktreeErr::Exists { name, path });
    }
    Ok(WorktreeCreateTarget::Fresh(FreshWorktree { name, path }))
}

fn add_and_seed(
    repo_root: &Path,
    name: String,
    path: PathBuf,
    branch: Option<&str>,
    base_branch: Option<String>,
    base_ref: String,
    checkout_ref: &str,
) -> Result<CreatedWorktree> {
    let branch = branch
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| name.clone());
    if branch.trim().is_empty() {
        return Err(WorktreeErr::Parse(
            "worktree branch cannot be empty".to_owned(),
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let path_arg = path.to_string_lossy().into_owned();
    git_run(
        repo_root,
        vec![
            "worktree",
            "add",
            "-b",
            branch.as_str(),
            path_arg.as_str(),
            checkout_ref,
        ],
    )?;
    let marker = WorktreeMarker {
        version: MARKER_VERSION,
        name: name.clone(),
        branch: branch.clone(),
        base_branch: base_branch.clone(),
        base_ref: base_ref.clone(),
        repo_root: repo_root.to_path_buf(),
        worktree_path: path.clone(),
        created_at: jiff::Timestamp::now(),
    };
    write_marker(&path, &marker)?;
    let included = crate::worktree_include::copy_includes(repo_root, &path);
    let linked = crate::worktree_link::link_dirs(repo_root, &path);
    Ok(CreatedWorktree {
        name,
        path,
        branch,
        base_branch,
        base_ref,
        reused: false,
        included,
        linked,
    })
}

fn write_marker(path: &Path, marker: &WorktreeMarker) -> Result<()> {
    crate::ledger::atomic::write_temp_then_rename(&marker_path(path)?, marker).map_err(Into::into)
}

fn resolve_base_commit(repo_root: &Path, base_ref: &str) -> Result<String> {
    let commitish = format!("{base_ref}^{{commit}}");
    git_stdout(repo_root, ["rev-parse", "--verify", commitish.as_str()])
}

fn resolve_base_branch(repo_root: &Path, base: &WorktreeBase) -> Option<String> {
    match base {
        WorktreeBase::Head => current_branch(repo_root),
        WorktreeBase::Fresh => origin_head(repo_root),
        WorktreeBase::Explicit(value) => resolve_explicit_base_branch(repo_root, value),
    }
    .filter(|name| !name.trim().is_empty())
}

fn resolve_explicit_base_branch(repo_root: &Path, value: &str) -> Option<String> {
    if value.starts_with('-') {
        return Some(value.to_owned());
    }
    let Ok(symbolic) = git_stdout(repo_root, ["rev-parse", "--symbolic-full-name", value]) else {
        return Some(value.to_owned());
    };
    if symbolic == "HEAD" {
        return current_branch(repo_root);
    }
    if let Some(branch) = symbolic.strip_prefix("refs/heads/") {
        return Some(branch.to_owned());
    }
    Some(value.to_owned())
}

fn current_branch(repo_root: &Path) -> Option<String> {
    git_stdout(repo_root, ["symbolic-ref", "--short", "HEAD"]).ok()
}

fn origin_head(repo_root: &Path) -> Option<String> {
    git_stdout(
        repo_root,
        ["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .ok()
}

fn comparison_ref(cwd: &Path, marker: &WorktreeMarker) -> Option<String> {
    let trunk = trunk_ref(cwd);
    if let Some(base_branch) = marker.base_branch.as_deref()
        && ref_resolves(cwd, base_branch)
        && !base_branch_superseded(cwd, base_branch, trunk.as_deref())
    {
        return Some(base_branch.to_owned());
    }
    trunk.or_else(|| {
        base_ref_is_snapshot_commit(cwd, &marker.base_ref).then(|| marker.base_ref.clone())
    })
}

/// The repository trunk for landed comparisons: the first of `main`, `master`,
/// or the `origin/HEAD` default branch that resolves in this worktree.
fn trunk_ref(cwd: &Path) -> Option<String> {
    for candidate in ["main", "master"] {
        if ref_resolves(cwd, candidate) {
            return Some(candidate.to_owned());
        }
    }
    origin_head(cwd).filter(|head| ref_resolves(cwd, head))
}

/// A base branch is a live destination until its own commits have landed on the
/// trunk. Once a diverged base branch is itself content-landed there, the trunk
/// is the authoritative comparison for work built on top of it. An ancestor base
/// branch stays the comparison.
fn base_branch_superseded(cwd: &Path, base_branch: &str, trunk: Option<&str>) -> bool {
    let Some(trunk) = trunk else {
        return false;
    };
    base_branch != trunk
        && !is_ancestor(cwd, base_branch, trunk)
        && content_landed(cwd, trunk, base_branch) == LandedVerdict::Landed
}

fn is_ancestor(cwd: &Path, ancestor: &str, descendant: &str) -> bool {
    git_run(cwd, ["merge-base", "--is-ancestor", ancestor, descendant]).is_ok()
}

fn landed_json(verdict: LandedVerdict) -> Option<bool> {
    match verdict {
        LandedVerdict::Landed => Some(true),
        LandedVerdict::Pending => Some(false),
        LandedVerdict::Unknown => None,
    }
}

fn ref_resolves(cwd: &Path, name: &str) -> bool {
    let commitish = format!("{name}^{{commit}}");
    git_run(
        cwd,
        ["rev-parse", "--verify", "--quiet", commitish.as_str()],
    )
    .is_ok()
}

pub fn content_landed(cwd: &Path, comparison_ref: &str, head_ref: &str) -> LandedVerdict {
    let Some(ancestry_count) = rev_list_count(cwd, &format!("{comparison_ref}..{head_ref}")) else {
        return LandedVerdict::Unknown;
    };
    if ancestry_count == 0 {
        return LandedVerdict::Landed;
    }

    if tree_id(cwd, comparison_ref)
        .zip(tree_id(cwd, head_ref))
        .is_some_and(|(comparison, head)| comparison == head)
    {
        return LandedVerdict::Landed;
    }

    let range = format!("{comparison_ref}...{head_ref}");
    let non_merge = match git_stdout(
        cwd,
        [
            "log",
            "--right-only",
            "--cherry-pick",
            "--no-merges",
            "--format=%H",
            range.as_str(),
        ],
    ) {
        Ok(output) => output,
        Err(_) => return LandedVerdict::Unknown,
    };
    if non_merge.lines().any(|line| !line.trim().is_empty()) {
        return LandedVerdict::Pending;
    }

    let merge_trees = match git_stdout(
        cwd,
        [
            "log",
            "--right-only",
            "--merges",
            "--format=%T",
            range.as_str(),
        ],
    ) {
        Ok(output) => output,
        Err(_) => return LandedVerdict::Unknown,
    };
    let merge_trees: Vec<&str> = merge_trees
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if merge_trees.is_empty() {
        return LandedVerdict::Landed;
    }

    let cap = LANDED_BASE_SCAN_CAP.to_string();
    let base_trees = match git_stdout(
        cwd,
        ["log", "--format=%T", "-n", cap.as_str(), comparison_ref],
    ) {
        Ok(output) => output,
        Err(_) => return LandedVerdict::Unknown,
    };
    let base_trees: HashSet<&str> = base_trees
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if merge_trees.iter().all(|tree| base_trees.contains(tree)) {
        LandedVerdict::Landed
    } else {
        LandedVerdict::Pending
    }
}

fn tree_id(cwd: &Path, ref_name: &str) -> Option<String> {
    let tree_ref = format!("{ref_name}^{{tree}}");
    git_stdout(cwd, ["rev-parse", tree_ref.as_str()]).ok()
}

fn rev_list_count(cwd: &Path, range: &str) -> Option<u32> {
    git_stdout(cwd, ["rev-list", "--count", range])
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .map(|count| count.min(u64::from(u32::MAX)) as u32)
}

fn base_ref_is_snapshot_commit(worktree: &Path, base_ref: &str) -> bool {
    let Ok(resolved) = git_stdout(
        worktree,
        ["rev-parse", "--verify", &format!("{base_ref}^{{commit}}")],
    ) else {
        return false;
    };
    resolved == base_ref
}

fn available_auto_name(repo_root: &Path, config: &WorktreeConfig) -> Result<String> {
    let seed = Uuid::now_v7();
    for attempt in 0..64 {
        let candidate = auto_name_from_uuid(seed, attempt);
        let path = worktree_path(repo_root, config, &candidate)?;
        if !path.exists() {
            return Ok(candidate);
        }
    }
    Err(WorktreeErr::Parse(
        "could not find an unused auto worktree name after 64 attempts".to_owned(),
    ))
}

fn auto_name_from_uuid(seed: Uuid, attempt: u8) -> String {
    let value = seed.as_u128() ^ u128::from(attempt);
    let adj = AUTO_ADJECTIVES[(value as usize) % AUTO_ADJECTIVES.len()];
    let noun = AUTO_NOUNS[((value >> 8) as usize) % AUTO_NOUNS.len()];
    if attempt == 0 {
        format!("{adj}-{noun}")
    } else {
        format!("{adj}-{noun}-{attempt}")
    }
}

fn validate_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
    if valid {
        Ok(())
    } else {
        Err(WorktreeErr::InvalidName(name.to_owned()))
    }
}

fn ensure_repo(repo_root: &Path) -> Result<()> {
    git_stdout(repo_root, ["rev-parse", "--show-toplevel"])
        .map(|_| ())
        .map_err(|err| match err {
            WorktreeErr::Git { .. } => WorktreeErr::NotRepo,
            other => other,
        })
}

fn delete_branch(repo_root: &Path, marker: &WorktreeMarker, force: bool) -> Result<BranchDeletion> {
    let branch = marker.branch.as_str();
    let flag = if force { "-D" } else { "-d" };
    match git_run(repo_root, ["branch", flag, branch]) {
        Ok(()) => Ok(BranchDeletion::Deleted),
        Err(WorktreeErr::Git { stderr, .. })
            if stderr.contains("not found") || stderr.contains("not a branch") =>
        {
            Ok(BranchDeletion::Deleted)
        }
        Err(err) if force => Err(err),
        Err(WorktreeErr::Git { stderr, .. }) if branch_delete_failed_unmerged(&stderr) => {
            if branch_landed(repo_root, marker) {
                force_delete_branch(repo_root, branch)
            } else {
                Ok(BranchDeletion::KeptUnmerged)
            }
        }
        Err(err) => Err(err),
    }
}

fn branch_delete_failed_unmerged(stderr: &str) -> bool {
    stderr.contains("not fully merged") || stderr.contains("not merged")
}

fn branch_landed(repo_root: &Path, marker: &WorktreeMarker) -> bool {
    let Some(comparison) = comparison_ref(repo_root, marker) else {
        return false;
    };
    content_landed(repo_root, &comparison, &marker.branch) == LandedVerdict::Landed
}

fn force_delete_branch(repo_root: &Path, branch: &str) -> Result<BranchDeletion> {
    match git_run(repo_root, ["branch", "-D", branch]) {
        Ok(()) => Ok(BranchDeletion::Deleted),
        Err(WorktreeErr::Git { stderr, .. })
            if stderr.contains("not found") || stderr.contains("not a branch") =>
        {
            Ok(BranchDeletion::Deleted)
        }
        Err(err) => Err(err),
    }
}

fn git_run<'a, I>(cwd: &Path, args: I) -> Result<()>
where
    I: IntoIterator<Item = &'a str>,
{
    git_output(cwd, args).map(|_| ())
}

fn git_stdout<'a, I>(cwd: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let output = git_output(cwd, args)?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn git_output<'a, I>(cwd: &Path, args: I) -> Result<std::process::Output>
where
    I: IntoIterator<Item = &'a str>,
{
    let args: Vec<&str> = args.into_iter().collect();
    let output = Command::new("git")
        .args(&args)
        .current_dir(cwd)
        .env("LC_ALL", "C")
        .output()?;
    if output.status.success() {
        return Ok(output);
    }
    Err(WorktreeErr::Git {
        cwd: cwd.to_path_buf(),
        args: args.join(" "),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_expands_relative_to_repo_root() {
        let config = WorktreeConfig::default();
        assert_eq!(
            worktree_parent(Path::new("/code/query-engine"), &config).expect("parent"),
            PathBuf::from("/code/query-engine/../query-engine-worktrees")
        );
    }

    #[test]
    fn auto_name_is_deterministic_and_retries_with_suffix() {
        let seed = Uuid::parse_str("01890f3c-0000-7000-8000-000000000001").expect("uuid");
        assert_eq!(auto_name_from_uuid(seed, 0), auto_name_from_uuid(seed, 0));
        assert!(auto_name_from_uuid(seed, 1).ends_with("-1"));
    }

    #[test]
    fn landed_verdict_and_status_constructors() {
        assert!(LandedVerdict::Landed.is_landed());
        assert!(!LandedVerdict::Pending.is_landed());
        assert!(!LandedVerdict::Unknown.is_landed());

        assert_eq!(
            WorktreeStatus::default(),
            WorktreeStatus {
                dirty: false,
                landed: LandedVerdict::Landed,
            }
        );
        assert!(WorktreeStatus::default().safe_to_remove());
        assert_eq!(
            WorktreeStatus::unknown(),
            WorktreeStatus {
                dirty: false,
                landed: LandedVerdict::Unknown,
            }
        );
        assert!(!WorktreeStatus::unknown().safe_to_remove());
    }

    #[test]
    fn cleanup_decision_table() {
        let clean = WorktreeStatus::default();
        let dirty = WorktreeStatus {
            dirty: true,
            landed: LandedVerdict::Landed,
        };
        assert_eq!(
            cleanup_decision(clean, true, false),
            CleanupDecision::RemoveClean
        );
        assert_eq!(
            cleanup_decision(dirty, true, false),
            CleanupDecision::PromptDirty
        );
        assert_eq!(
            cleanup_decision(WorktreeStatus::unknown(), true, false),
            CleanupDecision::PromptDirty
        );
        assert_eq!(cleanup_decision(clean, false, false), CleanupDecision::Skip);
        assert_eq!(cleanup_decision(clean, true, true), CleanupDecision::Skip);
    }

    #[test]
    fn parses_git_worktree_porcelain() {
        let raw = "\
worktree /code/query-engine
HEAD abc
branch refs/heads/main

worktree /code/query-engine-worktrees/swift-otter
HEAD def
branch refs/heads/swift-otter

";
        assert_eq!(
            parse_worktree_list(raw),
            vec![
                WorktreeRow {
                    path: PathBuf::from("/code/query-engine"),
                    branch: Some("main".to_owned())
                },
                WorktreeRow {
                    path: PathBuf::from("/code/query-engine-worktrees/swift-otter"),
                    branch: Some("swift-otter".to_owned())
                }
            ]
        );
    }

    #[test]
    fn marker_v2_json_parses_without_base_branch() {
        let raw = r#"{
            "version": 2,
            "name": "demo",
            "branch": "demo",
            "base_ref": "0123456789abcdef0123456789abcdef01234567",
            "repo_root": "/repo",
            "worktree_path": "/repo-worktrees/demo",
            "created_at": "2026-06-10T00:00:00Z"
        }"#;

        let marker: WorktreeMarker = serde_json::from_str(raw).expect("marker");

        assert_eq!(marker.version, 2);
        assert_eq!(marker.base_branch, None);
    }

    #[test]
    fn sweep_selection_requires_marker_clean_status_and_no_live_pane() {
        let a = PathBuf::from("/repo-wt/a");
        let b = PathBuf::from("/repo-wt/b");
        let rows = vec![
            WorktreeRow {
                path: a.clone(),
                branch: Some("a".to_owned()),
            },
            WorktreeRow {
                path: b.clone(),
                branch: Some("b".to_owned()),
            },
        ];
        let marked = BTreeSet::from([a.clone(), b.clone()]);
        let live = vec![b.join("subdir")];
        let statuses = BTreeMap::from([
            (a.clone(), WorktreeStatus::default()),
            (
                b,
                WorktreeStatus {
                    dirty: false,
                    landed: LandedVerdict::Pending,
                },
            ),
        ]);
        assert_eq!(
            sweepable_worktrees(&rows, &marked, &live, &statuses),
            vec![a]
        );
    }
}
