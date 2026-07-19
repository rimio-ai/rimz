//! RimZ-owned git worktree lifecycle and removal policy.
//!
//! Worktrees are identified by a marker stored in the linked worktree's git
//! admin directory, not in the checkout. The checkout remains pristine, and
//! removal only ever removes marked worktrees. Removal assessment combines Git
//! safety with normalized pane and agent occupancy facts.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agents::AgentState;
use crate::config::{WorktreeBase, WorktreeConfig};
use crate::forge::PrTarget;
use crate::ids::PaneId;
use crate::pane::PaneRef;
use crate::store::runtime::AgentLiveness;
use crate::workspace::{ResolvedWorkspace, RootClass};

mod include;
mod link;
mod pr;

pub use pr::create_from_pr;

const MARKER_FILE: &str = "rimz-worktree.json";
const MARKER_VERSION: u32 = 4;
const LANDED_BASE_SCAN_CAP: u32 = 500;
pub const WORKTREE_REMOVED_ARCHIVE_REASON: &str = "worktree removed";
const AUTO_ADJECTIVES: &[&str] = &[
    "brisk", "calm", "clear", "daring", "fleet", "fresh", "keen", "lively", "nimble", "quiet",
    "rapid", "ready", "sharp", "steady", "swift", "vivid",
];
const AUTO_NOUNS: &[&str] = &[
    "anchor", "birch", "cedar", "delta", "ember", "field", "harbor", "ion", "juniper", "keel",
    "lantern", "meadow", "north", "orbit", "pilot", "quartz",
];

#[derive(Debug, thiserror::Error)]
pub enum WorktreeErr {
    #[error("rimz worktrees require a git repository; run from a repo checkout")]
    NotRepo,
    #[error("--worktree requires a git repository-backed room")]
    LaunchWorktreeRequiresRepo,
    #[error("--from-pr requires a git repository-backed room")]
    LaunchPrRequiresRepo,
    #[error(
        "invalid worktree name `{0}`; use letters, numbers, `_`, `-`, with `/` separating branch-style segments"
    )]
    InvalidName(String),
    #[error("worktree `{name}` already exists at {path}")]
    Exists { name: String, path: PathBuf },
    #[error("worktree `{name}` is not a RimZ-managed worktree at {path}")]
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
    #[error("PR URL targets `{url_repo}` but origin is `{origin_repo}`")]
    PrRepoMismatch {
        url_repo: String,
        origin_repo: String,
    },
    #[error(
        "worktree `{name}` was created from PR {existing:?}, not requested PR {requested}; use another --worktree name or remove it first"
    )]
    PrWorktreeMismatch {
        name: String,
        existing: Option<u64>,
        requested: u64,
    },
    #[error(
        "could not resolve the head branch of PR #{number} ({reason}); install/log in to gh or tea, or pass --branch <name> for a review-only checkout"
    )]
    PrHeadUnresolved { number: u64, reason: String },
    #[error(
        "local PR branch `{branch}` conflicts with the remote head ({detail}); resolve the local branch or pass --branch <name> for a review-only checkout"
    )]
    PrBranchConflict { branch: String, detail: String },
    #[error("could not parse git output: {0}")]
    Parse(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Atomic(#[from] crate::store::atomic::AtomicErr),
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
    #[serde(default)]
    pub from_pr: Option<u64>,
    pub base_ref: String,
    pub repo_root: PathBuf,
    pub worktree_path: PathBuf,
    pub created_at: jiff::Timestamp,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CreatedWorktree {
    pub marker: WorktreeMarker,
    /// Files copied into the worktree from the project's `.worktreeinclude`.
    /// Zero for a reused worktree, which is never re-seeded.
    pub included: usize,
    /// Directories symlinked into the worktree from the project's `.worktreelink`.
    /// Zero for a reused worktree, which is never re-seeded.
    pub linked: usize,
    /// Fork push target established while creating a PR worktree.
    pub push_destination: Option<PushDestination>,
    /// Why a PR checkout intentionally has no configured push destination.
    pub review_only_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct PushDestination {
    pub remote: String,
    pub merge_ref: String,
}

/// Checkout selected for an agent launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchCheckout {
    pub cwd: PathBuf,
    pub worktree_name: Option<String>,
    pub review_only_reason: Option<String>,
    generated_name: bool,
}

impl LaunchCheckout {
    /// Return the auto-generated name for a bare `--worktree` request.
    pub fn generated_name(&self) -> Option<&str> {
        self.generated_name
            .then_some(self.worktree_name.as_deref())?
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestedName {
    pub name: String,
    pub branch: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedWorktree {
    pub marker: WorktreeMarker,
    pub path: PathBuf,
    pub branch: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorktreeRow {
    path: PathBuf,
    branch: Option<String>,
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
pub enum BranchDeletion {
    Deleted,
    KeptUnmerged,
}

/// One live mux pane fact gathered before worktree removal assessment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneProtectionFact {
    pub pane_id: PaneId,
    pub cwd: Option<PathBuf>,
    pub sidebar: bool,
}

/// One durable agent fact with process state already probed by CLI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentProtectionFact {
    pub pane_id: Option<PaneId>,
    pub liveness: AgentLiveness,
    pub stored_path: Option<PathBuf>,
    pub process_cwd: Option<PathBuf>,
}

/// Normalized paths whose live panes or agents prevent checkout removal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProtectionSet {
    paths: Vec<PathBuf>,
}

/// Fold current pane and agent state into worktree removal protection policy.
pub fn protection_set_from_runtime(
    panes: &[PaneRef],
    agents: &[AgentState],
    own: Option<&PaneId>,
) -> ProtectionSet {
    let pane_facts = panes
        .iter()
        .map(|pane| PaneProtectionFact {
            pane_id: pane.pane_id.clone(),
            cwd: pane.cwd.as_deref().map(PathBuf::from),
            sidebar: pane.is_rimz_sidebar(),
        })
        .collect::<Vec<_>>();
    let agent_facts = agents
        .iter()
        .map(|agent| {
            let liveness = crate::store::runtime::agent_liveness(agent);
            AgentProtectionFact {
                pane_id: agent.pane.as_ref().map(|pane| pane.pane_id.clone()),
                liveness,
                stored_path: agent.worktree_path.as_deref().map(PathBuf::from),
                process_cwd: match liveness {
                    AgentLiveness::Live { pid } => crate::proc::cwd(pid),
                    AgentLiveness::Dead | AgentLiveness::Unknown => None,
                },
            }
        })
        .collect::<Vec<_>>();
    ProtectionSet::from_facts(&pane_facts, &agent_facts, own)
}

/// Removal policy result in fixed safety-precedence order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemovalAssessment {
    Removable,
    InUse,
    Dirty,
    NotLanded,
}

impl ProtectionSet {
    /// Fold already-probed pane and agent facts into one normalized set.
    pub fn from_facts(
        panes: &[PaneProtectionFact],
        agents: &[AgentProtectionFact],
        own_pane: Option<&PaneId>,
    ) -> Self {
        let mut protected = Self::default();
        for pane in panes {
            if pane.sidebar || own_pane == Some(&pane.pane_id) {
                continue;
            }
            protected.insert(pane.cwd.as_deref());
        }
        for agent in agents {
            if own_pane.is_some() && agent.pane_id.as_ref() == own_pane {
                continue;
            }
            match agent.liveness {
                AgentLiveness::Dead => {}
                AgentLiveness::Unknown => protected.insert(agent.stored_path.as_deref()),
                AgentLiveness::Live { .. } => {
                    protected.insert(agent.stored_path.as_deref());
                    protected.insert(agent.process_cwd.as_deref());
                }
            }
        }
        protected
    }

    /// Whether a candidate checkout contains any protected path.
    pub fn protects(&self, candidate: &Path) -> bool {
        let candidate = normalize_path_lexical(candidate);
        self.paths.iter().any(|path| path_inside(path, &candidate))
    }

    /// Classify one checkout with in-use taking precedence over work state.
    pub fn assess(&self, candidate: &Path, status: WorktreeStatus) -> RemovalAssessment {
        if self.protects(candidate) {
            RemovalAssessment::InUse
        } else if status.dirty {
            RemovalAssessment::Dirty
        } else if !status.landed.is_landed() {
            RemovalAssessment::NotLanded
        } else {
            RemovalAssessment::Removable
        }
    }

    fn insert(&mut self, path: Option<&Path>) {
        let Some(path) = path else {
            return;
        };
        let path = normalize_path_lexical(path);
        if !self.paths.contains(&path) {
            self.paths.push(path);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemovalOutcome {
    worktree_name: String,
    branch: String,
    repo_root: PathBuf,
    removed_path: PathBuf,
    branch_deletion: BranchDeletion,
}

#[must_use = "both independent cleanup results must be handled"]
#[derive(Debug)]
pub struct RemovalRetirement {
    pub session_retirement: crate::store::Result<usize>,
    pub message_archival: crate::store::Result<usize>,
}

impl RemovalOutcome {
    pub fn worktree_name(&self) -> &str {
        &self.worktree_name
    }

    pub fn branch(&self) -> &str {
        &self.branch
    }

    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    pub fn removed_path(&self) -> &Path {
        &self.removed_path
    }

    pub const fn branch_deletion(&self) -> BranchDeletion {
        self.branch_deletion
    }
}

/// Retire every durable consequence of one removed managed worktree.
///
/// Session retirement runs first, and message archival still runs when it
/// fails because Git removal is already irreversible at this boundary.
pub fn retire_removal(
    store: &crate::Store,
    removed: &RemovalOutcome,
    archive_reason: &str,
    session_name: &str,
) -> RemovalRetirement {
    let session_retirement =
        store.retire_worktree_sessions(removed.removed_path(), Some(removed.branch()));
    let message_archival =
        store.archive_channel_messages(removed.worktree_name(), archive_reason, session_name);
    RemovalRetirement {
        session_retirement,
        message_archival,
    }
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
    let FreshWorktree {
        name,
        path,
        branch: derived_branch,
    } = match resolve_fresh_worktree(repo_root, config, name, None, reuse_existing)? {
        WorktreeCreateTarget::Fresh(fresh) => fresh,
        WorktreeCreateTarget::Reuse(reused) => return Ok(reused),
    };
    let branch = resolve_branch(branch, derived_branch.as_deref(), &name)?;

    let base = base.unwrap_or_else(|| config.base.clone());
    let checkout_base_ref = base.as_refspec().to_owned();
    let base_ref = resolve_base_commit(repo_root, &checkout_base_ref)?;
    let base_branch = resolve_base_branch(repo_root, &base);
    add_worktree(
        repo_root,
        name,
        path,
        branch,
        MarkerProvenance {
            base_branch,
            base_ref,
            from_pr: None,
        },
        Checkout::NewBranch(&checkout_base_ref),
    )
}

/// Resolve the cwd and optional RimZ-owned checkout for an agent launch.
///
/// Callers complete trust and provider preflight before entering this
/// side-effecting boundary.
pub fn resolve_launch_checkout(
    workspace: &ResolvedWorkspace,
    config: &WorktreeConfig,
    worktree: Option<&str>,
    from_pr: Option<&PrTarget>,
) -> Result<LaunchCheckout> {
    if let Some(pr) = from_pr {
        if workspace.root_class != RootClass::Repo {
            return Err(WorktreeErr::LaunchPrRequiresRepo);
        }
        let name = worktree.map(str::trim).filter(|name| !name.is_empty());
        let created = create_from_pr(
            &workspace.project_root,
            config,
            pr,
            name,
            None,
            name.is_some(),
        )?;
        let review_only_reason = created.review_only_reason;
        let marker = created.marker;
        return Ok(LaunchCheckout {
            cwd: marker.worktree_path,
            worktree_name: Some(marker.name),
            review_only_reason,
            generated_name: false,
        });
    }

    let Some(raw_name) = worktree else {
        return Ok(LaunchCheckout {
            cwd: workspace.worktree_root.clone(),
            worktree_name: None,
            review_only_reason: None,
            generated_name: false,
        });
    };
    if workspace.root_class != RootClass::Repo {
        return Err(WorktreeErr::LaunchWorktreeRequiresRepo);
    }
    let name = raw_name.trim();
    let created = create(
        &workspace.project_root,
        config,
        (!name.is_empty()).then_some(name),
        None,
        None,
        !name.is_empty(),
    )?;
    let marker = created.marker;
    Ok(LaunchCheckout {
        cwd: marker.worktree_path,
        worktree_name: Some(marker.name),
        review_only_reason: None,
        generated_name: name.is_empty(),
    })
}

pub fn remove(
    repo_root: &Path,
    config: &WorktreeConfig,
    name: &str,
    force: bool,
) -> Result<RemovalOutcome> {
    ensure_repo(repo_root)?;
    let path = worktree_path(repo_root, config, name)?;
    let marker = read_marker_for_worktree(&path)?.ok_or_else(|| WorktreeErr::Unmarked {
        name: name.to_owned(),
        path: path.clone(),
    })?;
    let status = status(&path, &marker)?;
    if !force && ProtectionSet::default().assess(&path, status) != RemovalAssessment::Removable {
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
) -> Result<RemovalOutcome> {
    ensure_repo(repo_root)?;
    leave_worktree_before_removal(repo_root, path)?;
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    let path_arg = path.to_string_lossy();
    args.push(path_arg.as_ref());
    git_run(repo_root, args)?;
    let branch_deletion = delete_branch(repo_root, marker, force)?;
    Ok(RemovalOutcome {
        worktree_name: marker.name.clone(),
        branch: marker.branch.clone(),
        repo_root: repo_root.to_owned(),
        removed_path: path.to_owned(),
        branch_deletion,
    })
}

pub fn discover_owned(repo_root: &Path) -> Result<Vec<ManagedWorktree>> {
    ensure_repo(repo_root)?;
    let rows = parse_worktree_list(&git_stdout(repo_root, ["worktree", "list", "--porcelain"])?);
    let mut entries = Vec::new();
    for row in rows {
        let Some(marker) = read_marker_from_checkout_metadata(&row.path)? else {
            continue;
        };
        entries.push(ManagedWorktree {
            marker,
            path: row.path,
            branch: row.branch,
        });
    }
    entries.sort_by(|a, b| a.marker.name.cmp(&b.marker.name));
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

fn leave_worktree_before_removal(repo_root: &Path, path: &Path) -> Result<()> {
    let cwd = std::env::current_dir()?;
    if path_inside(&normalize_path_lexical(&cwd), &normalize_path_lexical(path)) {
        std::env::set_current_dir(repo_root)?;
    }
    Ok(())
}

fn parse_worktree_list(raw: &str) -> Vec<WorktreeRow> {
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
    let requested = parse_requested_name(name)?;
    Ok(worktree_parent(repo_root, config)?.join(requested.name))
}

pub fn parse_requested_name(raw: &str) -> Result<RequestedName> {
    for segment in raw.split('/') {
        validate_requested_segment(raw, segment)?;
    }
    Ok(RequestedName {
        name: dashed_name(raw),
        branch: raw.contains('/').then(|| raw.to_owned()),
    })
}

/// The worktree directory name a branch-style request maps to: `/` joins as `-`.
pub fn dashed_name(raw: &str) -> String {
    raw.replace('/', "-")
}

pub fn read_marker_for_worktree(path: &Path) -> Result<Option<WorktreeMarker>> {
    let marker = match marker_path(path) {
        Ok(path) => path,
        Err(WorktreeErr::Git { .. }) | Err(WorktreeErr::Io(_)) => return Ok(None),
        Err(err) => return Err(err),
    };
    read_marker_file(&marker)
}

/// Read a RimZ marker by following the checkout's `.git` metadata only. This
/// keeps sidebar projection code off the git subprocess path.
pub(crate) fn read_marker_from_checkout_metadata(path: &Path) -> Result<Option<WorktreeMarker>> {
    let Some(marker) = marker_path_from_checkout_metadata(path)? else {
        return Ok(None);
    };
    read_marker_file(&marker)
}

fn read_marker_file(marker: &Path) -> Result<Option<WorktreeMarker>> {
    match std::fs::read_to_string(marker) {
        Ok(text) => serde_json::from_str(&text).map(Some).map_err(Into::into),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn marker_path_from_checkout_metadata(worktree: &Path) -> Result<Option<PathBuf>> {
    Ok(git_admin_dir_from_checkout_metadata(worktree)?.map(|path| path.join(MARKER_FILE)))
}

/// Resolve a checkout's Git admin directory without spawning Git.
pub(crate) fn git_admin_dir_from_checkout_metadata(worktree: &Path) -> Result<Option<PathBuf>> {
    let dot_git = worktree.join(".git");
    if dot_git.is_dir() {
        return Ok(Some(dot_git));
    }
    let text = match std::fs::read_to_string(&dot_git) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let Some(git_dir) = text.trim().strip_prefix("gitdir:") else {
        return Ok(None);
    };
    let git_dir = git_dir.trim();
    if git_dir.is_empty() {
        return Ok(None);
    }
    let git_dir = PathBuf::from(git_dir);
    let git_dir = if git_dir.is_absolute() {
        git_dir
    } else {
        worktree.join(git_dir)
    };
    Ok(Some(git_dir))
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
    branch: Option<String>,
}

struct MarkerProvenance {
    base_branch: Option<String>,
    base_ref: String,
    from_pr: Option<u64>,
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
    let requested = match (name, default_name) {
        (Some(raw), _) | (None, Some(raw)) => parse_requested_name(raw)?,
        (None, None) => RequestedName {
            name: available_auto_name(repo_root, config)?,
            branch: None,
        },
    };
    let RequestedName { name, branch } = requested;
    let path = worktree_path(repo_root, config, &name)?;
    if path.exists() {
        if reuse_existing {
            let marker = read_marker_for_worktree(&path)?.ok_or_else(|| WorktreeErr::Unmarked {
                name: name.clone(),
                path: path.clone(),
            })?;
            return Ok(WorktreeCreateTarget::Reuse(CreatedWorktree {
                marker,
                included: 0,
                linked: 0,
                push_destination: None,
                review_only_reason: None,
            }));
        }
        return Err(WorktreeErr::Exists { name, path });
    }
    Ok(WorktreeCreateTarget::Fresh(FreshWorktree {
        name,
        path,
        branch,
    }))
}

enum Checkout<'a> {
    NewBranch(&'a str),
    Tracking(&'a str),
    Existing,
}

fn add_worktree(
    repo_root: &Path,
    name: String,
    path: PathBuf,
    branch: String,
    provenance: MarkerProvenance,
    checkout: Checkout<'_>,
) -> Result<CreatedWorktree> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let path_arg = path.to_string_lossy().into_owned();
    match checkout {
        Checkout::NewBranch(checkout_ref) => git_run(
            repo_root,
            [
                "worktree",
                "add",
                "-b",
                branch.as_str(),
                path_arg.as_str(),
                checkout_ref,
            ],
        )?,
        Checkout::Tracking(remote_ref) => git_run(
            repo_root,
            [
                "worktree",
                "add",
                "--track",
                "-b",
                branch.as_str(),
                path_arg.as_str(),
                remote_ref,
            ],
        )?,
        Checkout::Existing => git_run(
            repo_root,
            ["worktree", "add", path_arg.as_str(), branch.as_str()],
        )?,
    }
    finish_worktree(repo_root, name, path, branch, provenance)
}

fn finish_worktree(
    repo_root: &Path,
    name: String,
    path: PathBuf,
    branch: String,
    provenance: MarkerProvenance,
) -> Result<CreatedWorktree> {
    let MarkerProvenance {
        base_branch,
        base_ref,
        from_pr,
    } = provenance;
    let marker = WorktreeMarker {
        version: MARKER_VERSION,
        name: name.clone(),
        branch: branch.clone(),
        base_branch: base_branch.clone(),
        from_pr,
        base_ref: base_ref.clone(),
        repo_root: repo_root.to_path_buf(),
        worktree_path: path.clone(),
        created_at: jiff::Timestamp::now(),
    };
    write_marker(&path, &marker)?;
    let included = include::copy_includes(repo_root, &path);
    let linked = link::link_dirs(repo_root, &path);
    Ok(CreatedWorktree {
        marker,
        included,
        linked,
        push_destination: None,
        review_only_reason: None,
    })
}

fn write_marker(path: &Path, marker: &WorktreeMarker) -> Result<()> {
    crate::store::atomic::write_temp_then_rename(&marker_path(path)?, marker).map_err(Into::into)
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

    let comparison_tree = tree_id(cwd, comparison_ref);
    let head_tree = tree_id(cwd, head_ref);
    if comparison_tree
        .as_ref()
        .zip(head_tree.as_ref())
        .is_some_and(|(comparison, head)| comparison == head)
    {
        return LandedVerdict::Landed;
    }

    if merge_absorbed(cwd, comparison_ref, head_ref) {
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
        let Some(head_tree) = head_tree else {
            return LandedVerdict::Unknown;
        };
        let exclusive_range = format!("{head_ref}..{comparison_ref}");
        let comparison_trees =
            match git_stdout(cwd, ["log", "--format=%T", exclusive_range.as_str()]) {
                Ok(output) => output,
                Err(_) => return LandedVerdict::Unknown,
            };
        return if comparison_trees
            .lines()
            .map(str::trim)
            .any(|tree| tree == head_tree)
        {
            LandedVerdict::Landed
        } else {
            LandedVerdict::Pending
        };
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

/// True when `head` sits on the trunk's first-parent lineage — the chain the
/// trunk itself advanced through. A HEAD there holds no work of its own: it only
/// tracked the trunk by fresh fork, rebase onto a newer trunk, or fast-forward.
/// Landed side-branch tips stay off this lineage. Scan capped at
/// [`LANDED_BASE_SCAN_CAP`].
pub fn on_trunk_first_parent(cwd: &Path, trunk: &str, head: &str) -> bool {
    let cap = LANDED_BASE_SCAN_CAP.to_string();
    let Ok(commits) = git_stdout(
        cwd,
        ["rev-list", "--first-parent", "-n", cap.as_str(), trunk],
    ) else {
        return false;
    };
    commits.lines().map(str::trim).any(|commit| commit == head)
}

fn tree_id(cwd: &Path, ref_name: &str) -> Option<String> {
    let tree_ref = format!("{ref_name}^{{tree}}");
    git_stdout(cwd, ["rev-parse", tree_ref.as_str()]).ok()
}

/// True when a three-way merge of `head_ref` into `comparison_ref` produces
/// `comparison_ref`'s own tree. The branch adds nothing, so its changes are
/// already present even when patch context drift makes `--cherry-pick` miss it.
fn merge_absorbed(cwd: &Path, comparison_ref: &str, head_ref: &str) -> bool {
    let Some(comparison_tree) = tree_id(cwd, comparison_ref) else {
        return false;
    };
    matches!(
        git_stdout(cwd, ["merge-tree", "--write-tree", comparison_ref, head_ref]),
        Ok(merged_tree) if merged_tree == comparison_tree
    )
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

fn resolve_branch(
    explicit: Option<&str>,
    derived: Option<&str>,
    default_name: &str,
) -> Result<String> {
    let branch = explicit.or(derived).unwrap_or(default_name).to_owned();
    if branch.trim().is_empty() {
        return Err(WorktreeErr::Parse(
            "worktree branch cannot be empty".to_owned(),
        ));
    }
    Ok(branch)
}

fn validate_requested_segment(raw: &str, segment: &str) -> Result<()> {
    let valid = !segment.is_empty()
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
    if valid {
        Ok(())
    } else {
        Err(WorktreeErr::InvalidName(raw.to_owned()))
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
        Err(WorktreeErr::Git { stderr, .. }) if branch_already_gone(&stderr) => {
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

fn branch_already_gone(stderr: &str) -> bool {
    stderr.contains("not found") || stderr.contains("not a branch")
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
        Err(WorktreeErr::Git { stderr, .. }) if branch_already_gone(&stderr) => {
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
    let output = crate::proc::git_command(cwd)
        .args(&args)
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

fn git_network_output<'a, I>(cwd: &Path, args: I, timeout: Duration) -> Result<std::process::Output>
where
    I: IntoIterator<Item = &'a str>,
{
    let args: Vec<&str> = args.into_iter().collect();
    let mut command = crate::proc::git_command(cwd);
    command
        .args(&args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C");
    let output = crate::proc::run_bounded_output(&mut command, timeout)?;
    if output.timed_out {
        return Err(WorktreeErr::Git {
            cwd: cwd.to_path_buf(),
            args: args.join(" "),
            stderr: format!("timed out after {}s", timeout.as_secs()),
        });
    }
    if output.status.success() {
        return Ok(std::process::Output {
            status: output.status,
            stdout: output.stdout,
            stderr: output.stderr,
        });
    }
    Err(WorktreeErr::Git {
        cwd: cwd.to_path_buf(),
        args: args.join(" "),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    })
}

#[cfg(test)]
#[path = "worktree/tests.rs"]
mod tests;
