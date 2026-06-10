//! Rimz-owned git worktree lifecycle.
//!
//! Worktrees are identified by a marker stored in the linked worktree's git
//! admin directory, not in the checkout. The checkout remains pristine, and
//! cleanup only ever removes marked worktrees.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::{WorktreeBase, WorktreeConfig};

const MARKER_FILE: &str = "rimz-worktree.json";
const MARKER_VERSION: u32 = 3;
const PATCH_EQUIVALENCE_UPSTREAM_COMMIT_CAP: u32 = 500;
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
    #[error("worktree `{name}` has local changes or unmerged commits; use --force to remove it")]
    Dirty { name: String },
    #[error("git command failed in {cwd}: git {args}: {stderr}")]
    Git {
        cwd: PathBuf,
        args: String,
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
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct WorktreeListEntry {
    pub name: String,
    pub path: PathBuf,
    pub branch: Option<String>,
    pub base_ref: String,
    pub dirty: bool,
    pub commits_unmerged: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeRow {
    pub path: PathBuf,
    pub branch: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorktreeStatus {
    pub dirty: bool,
    pub commits_unmerged: Option<u32>,
}

impl Default for WorktreeStatus {
    fn default() -> Self {
        Self {
            dirty: false,
            commits_unmerged: Some(0),
        }
    }
}

impl WorktreeStatus {
    pub const fn clean(self) -> bool {
        !self.dirty && matches!(self.commits_unmerged, Some(0))
    }

    pub const fn unknown() -> Self {
        Self {
            dirty: false,
            commits_unmerged: None,
        }
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
    let name = match name {
        Some(raw) => {
            validate_name(raw)?;
            raw.to_owned()
        }
        None => available_auto_name(repo_root, config)?,
    };
    let path = worktree_path(repo_root, config, &name)?;
    if path.exists() {
        if reuse_existing {
            let marker = read_marker_for_worktree(&path)?.ok_or_else(|| WorktreeErr::Unmarked {
                name: name.clone(),
                path: path.clone(),
            })?;
            return Ok(CreatedWorktree {
                name,
                path,
                branch: marker.branch,
                base_branch: marker.base_branch,
                base_ref: marker.base_ref,
                reused: true,
            });
        }
        return Err(WorktreeErr::Exists { name, path });
    }

    let base = base.unwrap_or_else(|| config.base.clone());
    let checkout_base_ref = base.as_refspec().to_owned();
    let base_ref = resolve_base_commit(repo_root, &checkout_base_ref)?;
    let base_branch = resolve_base_branch(repo_root, &base);
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
            checkout_base_ref.as_str(),
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
    Ok(CreatedWorktree {
        name,
        path,
        branch,
        base_branch,
        base_ref,
        reused: false,
    })
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
    if !force && !status.clean() {
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
            commits_unmerged: status.commits_unmerged,
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
    let unmerged = commits_unmerged(worktree, marker);
    Ok(WorktreeStatus {
        dirty: !porcelain.trim().is_empty(),
        commits_unmerged: unmerged,
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
    if status.clean() {
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
        .filter(|row| statuses.get(&row.path).is_some_and(|status| status.clean()))
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

fn commits_unmerged(worktree: &Path, marker: &WorktreeMarker) -> Option<u32> {
    let comparison = comparison_ref(worktree, marker)?;
    commits_unmerged_against(worktree, &comparison, "HEAD")
}

fn comparison_ref(cwd: &Path, marker: &WorktreeMarker) -> Option<String> {
    if let Some(base_branch) = marker.base_branch.as_deref()
        && ref_resolves(cwd, base_branch)
    {
        return Some(base_branch.to_owned());
    }
    for candidate in ["main", "master"] {
        if ref_resolves(cwd, candidate) {
            return Some(candidate.to_owned());
        }
    }
    if let Some(remote_head) = origin_head(cwd)
        && ref_resolves(cwd, &remote_head)
    {
        return Some(remote_head);
    }
    if base_ref_is_snapshot_commit(cwd, &marker.base_ref) {
        return Some(marker.base_ref.clone());
    }
    None
}

fn ref_resolves(cwd: &Path, name: &str) -> bool {
    let commitish = format!("{name}^{{commit}}");
    git_run(
        cwd,
        ["rev-parse", "--verify", "--quiet", commitish.as_str()],
    )
    .is_ok()
}

fn commits_unmerged_against(cwd: &Path, comparison_ref: &str, head_ref: &str) -> Option<u32> {
    let ancestry_count = rev_list_count(cwd, &format!("{comparison_ref}..{head_ref}"))?;
    if ancestry_count == 0 {
        return Some(0);
    }
    let Some(merge_base) = git_stdout(cwd, ["merge-base", comparison_ref, head_ref]).ok() else {
        return Some(ancestry_count);
    };
    if final_tree_matches_after_branch_change(cwd, comparison_ref, head_ref, &merge_base) {
        return Some(0);
    }
    let upstream_count = rev_list_count(cwd, &format!("{merge_base}..{comparison_ref}"))
        .unwrap_or(PATCH_EQUIVALENCE_UPSTREAM_COMMIT_CAP.saturating_add(1));
    if upstream_count > PATCH_EQUIVALENCE_UPSTREAM_COMMIT_CAP {
        return Some(ancestry_count);
    }
    let cherry = match git_stdout(cwd, ["cherry", comparison_ref, head_ref]) {
        Ok(output) => output,
        Err(_) => return Some(ancestry_count),
    };
    let patch_unmerged = cherry
        .lines()
        .filter(|line| line.starts_with("+ "))
        .count()
        .min(u32::MAX as usize) as u32;
    if patch_unmerged == 0
        || (ancestry_count > 1
            && branch_squash_equivalent(cwd, comparison_ref, head_ref, &merge_base))
    {
        Some(0)
    } else {
        Some(patch_unmerged)
    }
}

fn final_tree_matches_after_branch_change(
    cwd: &Path,
    comparison_ref: &str,
    head_ref: &str,
    merge_base: &str,
) -> bool {
    let Some(comparison_tree) = tree_id(cwd, comparison_ref) else {
        return false;
    };
    let Some(head_tree) = tree_id(cwd, head_ref) else {
        return false;
    };
    let Some(merge_base_tree) = tree_id(cwd, merge_base) else {
        return false;
    };
    comparison_tree == head_tree && merge_base_tree != head_tree
}

fn tree_id(cwd: &Path, ref_name: &str) -> Option<String> {
    let tree_ref = format!("{ref_name}^{{tree}}");
    git_stdout(cwd, ["rev-parse", tree_ref.as_str()]).ok()
}

fn branch_squash_equivalent(
    cwd: &Path,
    comparison_ref: &str,
    head_ref: &str,
    merge_base: &str,
) -> bool {
    let tree = format!("{head_ref}^{{tree}}");
    // The synthetic commit is unreachable; normal Git gc collects it after the
    // human-scale cleanup/gc probe uses it.
    let Ok(synthetic) = git_stdout(
        cwd,
        [
            "commit-tree",
            tree.as_str(),
            "-p",
            merge_base,
            "-m",
            "rimz-squash-probe",
        ],
    ) else {
        return false;
    };
    git_stdout(cwd, ["cherry", comparison_ref, synthetic.as_str()])
        .ok()
        .is_some_and(|output| output.lines().any(|line| line.starts_with("- ")))
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
    commits_unmerged_against(repo_root, &comparison, &marker.branch) == Some(0)
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
    fn cleanup_decision_table() {
        let clean = WorktreeStatus::default();
        let dirty = WorktreeStatus {
            dirty: true,
            commits_unmerged: Some(0),
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
                    commits_unmerged: Some(1),
                },
            ),
        ]);
        assert_eq!(
            sweepable_worktrees(&rows, &marked, &live, &statuses),
            vec![a]
        );
    }
}
