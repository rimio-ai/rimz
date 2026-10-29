//! RimZ-owned git worktree lifecycle, landing, and removal policy.
//!
//! Worktrees are identified by a marker stored in the linked worktree's git
//! admin directory, not in the checkout. The checkout remains pristine, and
//! removal only ever removes marked worktrees. Removal assessment combines Git
//! safety with normalized pane and agent occupancy facts, loop-task pins, and team-board pins.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agents::AgentState;
use crate::agents::attribution::{LaneLifetime, LaneLifetimes};
use crate::config::{WorktreeBase, WorktreeConfig, WorktreeHooks};
use crate::forge::PrTarget;
use crate::ids::PaneId;
use crate::pane::PaneRef;
use crate::store::runtime::AgentLiveness;
use crate::utils::path::normalize_path_lexical;
use crate::workspace::{ResolvedWorkspace, RootClass};

mod exclude;
mod hooks;
mod include;
mod landed_memo;
mod link;
mod pr;

pub use exclude::exclude_team_scratch;
pub use landed_memo::LandedMemo;
pub use pr::{PrBranchChoice, PrBranchDivergence, create_from_pr};

const MARKER_FILE: &str = "rimz-worktree.json";
const MARKER_VERSION: u32 = 4;
const TRUNK_FETCH_TIMEOUT: Duration = Duration::from_secs(60);
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
    #[error(transparent)]
    HookConfig(#[from] crate::config::WorktreeHooksConfigErr),
    #[error(
        "worktree.created hook failed: {reason}\n{rollback}; fix agents.worktree.hooks.created"
    )]
    CreatedHook { reason: String, rollback: String },
    #[error("rimz worktrees require a git repository; run from a repo checkout")]
    NotRepo,
    #[error("--worktree requires a git repository-backed room")]
    LaunchWorktreeRequiresRepo,
    #[error("--from-pr requires a git repository-backed room")]
    LaunchPrRequiresRepo,
    #[error(
        "invalid worktree name `{0}`; use letters, numbers, `_`, `-`, with `/` separating branch-style segments; omit the channel's leading `#` (use `feat-a` for `#feat-a`)"
    )]
    InvalidName(String),
    #[error("worktree `{name}` already exists at {path}")]
    Exists { name: String, path: PathBuf },
    #[error("worktree `{name}` does not exist at {path}")]
    Missing { name: String, path: PathBuf },
    #[error("worktree `{name}` is not a RimZ-managed worktree at {path}")]
    Unmarked { name: String, path: PathBuf },
    #[error("{path} is not a linked worktree of {repo_root}")]
    NotLinkedWorktree { path: PathBuf, repo_root: PathBuf },
    #[error(
        "worktree `{name}` has uncommitted changes ({files} files); commit or stash them, or pass --force to discard them"
    )]
    Dirty { name: String, files: u32 },
    #[error(
        "worktree `{name}` has {ahead} commits not landed on {target}; run `rimz worktree merge {name}`, or pass --force to delete them"
    )]
    Unlanded {
        name: String,
        ahead: u32,
        target: String,
    },
    #[error(
        "worktree `{name}` has work RimZ cannot prove landed; land it yourself, or pass --force to delete its branch"
    )]
    Unproven { name: String },
    #[error(
        "worktree `{name}` is in use by a live agent or an open pane; use --force to remove it"
    )]
    InUse { name: String },
    #[error(
        "cannot merge worktree `{name}`: its checkout has local changes; commit or stash them first"
    )]
    MergeDirtyWorktree { name: String },
    #[error("cannot merge worktree `{name}`: it is in use by a live agent or an open pane")]
    MergeInUse { name: String, path: PathBuf },
    #[error(
        "cannot merge worktree `{name}`: the {target} checkout has local changes; commit or stash them first"
    )]
    MergeDirtyTarget { name: String, target: String },
    #[error("cannot merge worktree `{name}`: {checkout} has a Git operation in progress")]
    MergeOperationInProgress { name: String, checkout: String },
    #[error("cannot merge worktree `{name}`: {target} must be checked out in a worktree")]
    TargetNotCheckedOut { name: String, target: String },
    #[error("cannot merge worktree `{name}`: no branch landing target; land it yourself")]
    NoLandingTarget { name: String },
    #[error("cannot merge worktree `{name}`: expected branch `{expected}`, found `{actual}`")]
    MergeBranchMismatch {
        name: String,
        expected: String,
        actual: String,
    },
    #[error(
        "cannot merge worktree `{name}`: rebase onto {target} failed; no commits were landed on {target}: {stderr}"
    )]
    RebaseFailed {
        name: String,
        target: String,
        stderr: String,
    },
    #[error(
        "cannot merge worktree `{name}`: {target} changed while the worktree was rebasing; rerun the command"
    )]
    TargetChanged { name: String, target: String },
    #[error(
        "cannot merge worktree `{name}`: the worktree was rebased onto {target} but now has local changes; {target} was not updated"
    )]
    MergeDirtyAfterRebase { name: String, target: String },
    #[error("cannot merge worktree `{name}`: fast-forwarding {target} failed: {stderr}")]
    FastForwardFailed {
        name: String,
        target: String,
        stderr: String,
    },
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
    #[error(
        "could not move local PR branch `{branch}` to the PR head in {}; commit or stash changes there first: {detail}",
        holder.display()
    )]
    PrBranchAlignFailed {
        branch: String,
        holder: PathBuf,
        detail: String,
    },
    #[error("local PR branch `{branch}` differs from the PR head: {divergence}; {}", pr::alignment_commands(branch, holder.as_deref()))]
    PrBranchDiverged {
        branch: String,
        holder: Option<PathBuf>,
        divergence: PrBranchDivergence,
    },
    #[error("could not parse git output: {0}")]
    Parse(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Atomic(#[from] crate::disk::atomic::AtomicErr),
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

/// What a linked worktree's `.git` metadata records about where it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LinkedWorktree {
    /// The branch the tree was cut from, from the RimZ marker; `None` without a marker or a recorded base.
    pub base_branch: Option<String>,
    /// The primary checkout: the parent of git's common `.git` directory.
    pub primary: PathBuf,
}

/// The base branch sat behind its upstream when the tree was cut, as of the last fetch.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct StaleBase {
    pub branch: String,
    pub upstream: String,
    pub behind: u64,
}

impl std::fmt::Display for StaleBase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let commits = if self.behind == 1 {
            "commit"
        } else {
            "commits"
        };
        write!(
            f,
            "base branch {} is {} {commits} behind {} as last fetched",
            self.branch, self.behind, self.upstream
        )
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CreatedWorktree {
    pub stale_base: Option<StaleBase>,
    pub trunk_fetch: Option<TrunkFetchFailure>,
    pub marker: WorktreeMarker,
    pub reused: bool,
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
    pub stale_base: Option<StaleBase>,
    pub trunk_fetch: Option<TrunkFetchFailure>,
    /// Present only after a fresh managed creation, including its hook, succeeded.
    pub created: Option<WorktreeMarker>,
    pub cwd: PathBuf,
    pub reused: bool,
    pub branch: Option<String>,
    /// Repository root from workspace resolution, or `None` outside Git; `--root` can override it.
    pub repo_root: Option<PathBuf>,
    pub worktree_name: Option<String>,
    pub review_only_reason: Option<String>,
    generated_name: bool,
    ownership: CheckoutOwnership,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CheckoutOwnership {
    User,
    Rimz,
}

impl LaunchCheckout {
    pub fn owns_checkout_lifecycle(&self) -> bool {
        self.ownership == CheckoutOwnership::Rimz
    }

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
pub struct WorktreeMerge {
    pub name: String,
    pub branch: String,
    pub head: String,
}

pub struct LandingPlan {
    pub managed: ManagedWorktree,
    pub target: String,
    pub target_checkout: PathBuf,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct WorktreeSweep {
    pub removed: Vec<SweptWorktree>,
    pub failed: Vec<FailedWorktree>,
    pub kept: Vec<KeptWorktree>,
}

impl WorktreeSweep {
    pub fn bytes(&self) -> u64 {
        self.removed
            .iter()
            .fold(0_u64, |total, removed| total.saturating_add(removed.bytes))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SweptWorktree {
    pub name: String,
    pub branch: String,
    pub path: PathBuf,
    pub bytes: u64,
    #[serde(
        rename = "branch_deleted",
        serialize_with = "serialize_branch_deletion"
    )]
    pub branch_deletion: Option<BranchDeletion>,
    pub archive_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FailedWorktree {
    pub path: PathBuf,
    pub error: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct KeptWorktree {
    pub name: String,
    pub path: PathBuf,
    pub reasons: Vec<KeepReason>,
    pub holders: Holders,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorktreeRow {
    path: PathBuf,
    branch: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeStatus {
    pub changed_files: u32,
    pub ahead: Option<u32>,
    pub landed: LandedVerdict,
    pub target: Option<String>,
}

impl Default for WorktreeStatus {
    fn default() -> Self {
        Self {
            changed_files: 0,
            ahead: None,
            landed: LandedVerdict::Landed,
            target: None,
        }
    }
}

impl WorktreeStatus {
    pub const fn dirty(&self) -> bool {
        self.changed_files > 0
    }

    pub const fn unknown() -> Self {
        Self {
            changed_files: 0,
            ahead: None,
            landed: LandedVerdict::Unknown,
            target: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LandedVerdict {
    Landed,
    Unlanded,
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Holders {
    pub agents: Vec<String>,
    pub panes: u32,
    pub loop_tasks: Vec<String>,
    pub teams: Vec<String>,
}

impl Holders {
    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
            && self.panes == 0
            && self.loop_tasks.is_empty()
            && self.teams.is_empty()
    }
}

impl std::fmt::Display for Holders {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = self.agents.clone();
        match self.panes {
            0 => {}
            1 => parts.push("an open pane".to_owned()),
            count => parts.push(format!("{count} open panes")),
        }
        parts.extend(
            self.loop_tasks
                .iter()
                .map(|name| format!("loop task {name}")),
        );
        parts.extend(self.teams.iter().map(|name| format!("team {name}")));
        f.write_str(&parts.join(", "))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeepReason {
    InUse,
    #[serde(rename = "uncommitted_changes")]
    Dirty,
    Unlanded,
    Unknown,
}

impl KeepReason {
    pub const fn text(self) -> &'static str {
        match self {
            Self::InUse => "in use",
            Self::Dirty => "uncommitted changes",
            Self::Unlanded => "unlanded work",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeState {
    pub status: WorktreeStatus,
    pub holders: Holders,
}

impl WorktreeState {
    pub fn keep_reasons(&self) -> Vec<KeepReason> {
        let mut reasons = Vec::new();
        if !self.holders.is_empty() {
            reasons.push(KeepReason::InUse);
        }
        if self.status.dirty() {
            reasons.push(KeepReason::Dirty);
        }
        match self.status.landed {
            LandedVerdict::Landed => {}
            LandedVerdict::Unlanded => reasons.push(KeepReason::Unlanded),
            LandedVerdict::Unknown => reasons.push(KeepReason::Unknown),
        }
        reasons
    }

    pub fn removable(&self) -> bool {
        self.keep_reasons().is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BranchDeletion {
    Deleted,
    KeptUnmerged,
}

fn serialize_branch_deletion<S: serde::Serializer>(
    deletion: &Option<BranchDeletion>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    deletion
        .map(|deletion| deletion == BranchDeletion::Deleted)
        .serialize(serializer)
}

/// One live mux pane fact gathered before worktree removal assessment.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PaneProtectionFact {
    pane_id: PaneId,
    cwd: Option<PathBuf>,
    sidebar: bool,
}

/// One durable agent fact with process state already probed by CLI.
#[derive(Clone, Debug, PartialEq, Eq)]
struct AgentProtectionFact {
    handle: String,
    pane_id: Option<PaneId>,
    liveness: AgentLiveness,
    stored_path: Option<PathBuf>,
    process_cwd: Option<PathBuf>,
}

/// Normalized paths whose panes, agents, loop tasks, or team boards prevent checkout removal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProtectionSet {
    paths: Vec<(PathBuf, Holder)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Holder {
    Agent(String),
    Pane(PaneId),
    LoopTask(String),
    Team(String),
}

/// Which agent records keep a checkout occupied.
///
/// Automatic reclamation runs with nobody watching, so an agent it cannot
/// prove dead still holds its tree. A person naming one tree is already the
/// decision, so only a live process holds it and a stale session record left
/// by a crash never blocks the command that would retire it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Occupancy {
    Unproven,
    ProvenLive,
}

/// Fold current pane and agent state into worktree removal protection policy.
///
/// Callers add task and team pins separately through [`ProtectionSet`].
pub fn protection_set_from_runtime(
    panes: &[PaneRef],
    agents: &[AgentState],
    own: Option<&PaneId>,
    occupancy: Occupancy,
) -> ProtectionSet {
    let pane_facts = panes
        .iter()
        .map(|pane| PaneProtectionFact {
            pane_id: pane.pane_id.clone(),
            cwd: pane.cwd.as_deref().map(PathBuf::from),
            sidebar: pane.is_rimz_sidebar(),
        })
        .collect::<Vec<_>>();
    let peers = agents.iter().collect::<Vec<_>>();
    let agent_facts = agents
        .iter()
        .map(|agent| {
            let liveness = crate::store::runtime::agent_liveness(agent);
            AgentProtectionFact {
                handle: crate::address::agent_handle(agent, &peers, true),
                pane_id: agent.pane.as_ref().map(|pane| pane.pane_id.clone()),
                liveness,
                stored_path: agent.lane_path().map(PathBuf::from),
                process_cwd: match liveness {
                    AgentLiveness::Live { pid } => crate::proc::cwd(pid),
                    AgentLiveness::Dead | AgentLiveness::Unknown => None,
                },
            }
        })
        .collect::<Vec<_>>();
    ProtectionSet::from_facts(&pane_facts, &agent_facts, own, occupancy)
}

impl ProtectionSet {
    /// Add a task's pinned path without importing the scheduler's policy.
    pub fn insert_loop_task(&mut self, name: &str, path: &Path) {
        self.insert(Some(path), Holder::LoopTask(name.to_owned()));
    }

    /// Add a team's pinned path without importing board discovery or caller policy.
    pub fn insert_team(&mut self, name: &str, path: &Path) {
        self.insert(Some(path), Holder::Team(name.to_owned()));
    }

    pub fn holders(&self, candidate: &Path) -> Holders {
        let candidate = normalize_path_lexical(candidate);
        let mut holders = Holders::default();
        let mut panes = HashSet::new();
        for (path, holder) in &self.paths {
            if !path.starts_with(&candidate) {
                continue;
            }
            match holder {
                Holder::Agent(handle) => {
                    if !holders.agents.contains(handle) {
                        holders.agents.push(handle.clone());
                    }
                }
                Holder::Pane(pane) => {
                    panes.insert(pane);
                }
                Holder::LoopTask(name) => {
                    if !holders.loop_tasks.contains(name) {
                        holders.loop_tasks.push(name.clone());
                    }
                }
                Holder::Team(name) => {
                    if !holders.teams.contains(name) {
                        holders.teams.push(name.clone());
                    }
                }
            }
        }
        holders.panes = panes.len().try_into().unwrap_or(u32::MAX);
        holders
    }

    /// Fold already-probed occupancy facts before adding loop-task and team-board pins.
    fn from_facts(
        panes: &[PaneProtectionFact],
        agents: &[AgentProtectionFact],
        own_pane: Option<&PaneId>,
        occupancy: Occupancy,
    ) -> Self {
        let mut protected = Self::default();
        let agents = agents
            .iter()
            .filter(|agent| {
                !(own_pane.is_some() && agent.pane_id.as_ref() == own_pane)
                    && match agent.liveness {
                        AgentLiveness::Dead => false,
                        AgentLiveness::Unknown => occupancy == Occupancy::Unproven,
                        AgentLiveness::Live { .. } => true,
                    }
            })
            .collect::<Vec<_>>();
        for agent in &agents {
            protected.insert(
                agent.stored_path.as_deref(),
                Holder::Agent(agent.handle.clone()),
            );
            if matches!(agent.liveness, AgentLiveness::Live { .. }) {
                protected.insert(
                    agent.process_cwd.as_deref(),
                    Holder::Agent(agent.handle.clone()),
                );
            }
        }
        for pane in panes {
            if pane.sidebar || own_pane == Some(&pane.pane_id) {
                continue;
            }
            let holder = agents
                .iter()
                .find(|agent| agent.pane_id.as_ref() == Some(&pane.pane_id))
                .map_or_else(
                    || Holder::Pane(pane.pane_id.clone()),
                    |agent| Holder::Agent(agent.handle.clone()),
                );
            protected.insert(pane.cwd.as_deref(), holder);
        }
        protected
    }

    /// Whether a candidate checkout contains any protected path.
    pub fn protects(&self, candidate: &Path) -> bool {
        !self.holders(candidate).is_empty()
    }

    fn insert(&mut self, path: Option<&Path>, holder: Holder) {
        let Some(path) = path else {
            return;
        };
        let entry = (normalize_path_lexical(path), holder);
        if !self.paths.contains(&entry) {
            self.paths.push(entry);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemovalOutcome {
    repo_root: PathBuf,
    from_pr: Option<u64>,
    worktree_name: String,
    branch: String,
    removed_path: PathBuf,
    branch_deletion: BranchDeletion,
    branch_tip: Option<String>,
    target: Option<String>,
}

#[must_use = "both independent cleanup results must be handled"]
#[derive(Debug)]
pub struct RemovalRetirement {
    pub session_retirement: crate::store::Result<usize>,
    pub message_archival: crate::store::Result<usize>,
}

impl RemovalOutcome {
    pub fn branch(&self) -> &str {
        &self.branch
    }

    pub const fn branch_deletion(&self) -> BranchDeletion {
        self.branch_deletion
    }

    pub fn branch_tip(&self) -> Option<&str> {
        self.branch_tip.as_deref()
    }

    pub fn target(&self) -> Option<&str> {
        self.target.as_deref()
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
        store.retire_worktree_sessions(&removed.removed_path, Some(removed.branch()));
    let message_archival =
        store.archive_channel_messages(&removed.worktree_name, archive_reason, session_name);
    let mut signal = crate::harness::schedule::signal::Signal {
        name: "worktree.removed"
            .parse()
            .expect("built-in signal name is valid"),
        payload: serde_json::Map::from_iter([
            ("name".into(), serde_json::json!(removed.worktree_name)),
            ("branch".into(), serde_json::json!(removed.branch)),
            (
                "path".into(),
                serde_json::json!(removed.removed_path.to_string_lossy()),
            ),
            (
                "repo".into(),
                serde_json::json!(removed.repo_root.to_string_lossy()),
            ),
            (
                "branch_deleted".into(),
                serde_json::json!(removed.branch_deletion == BranchDeletion::Deleted),
            ),
        ]),
        source: crate::store::event::SignalSource::Worktree,
        watch: None,
    };
    if let Some(number) = removed.from_pr {
        signal
            .payload
            .insert("from_pr".into(), serde_json::json!(number));
    }
    crate::harness::schedule::signal::emit_in_process(
        store,
        session_name,
        &removed.repo_root,
        &signal,
    );
    RemovalRetirement {
        session_retirement,
        message_archival,
    }
}

/// Base policy for a freshly created launch worktree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchBase {
    /// Use `agents.worktree.base`, or local `HEAD` when unset.
    Configured,
    /// Use the configured base, or the remote default branch when unset. A `fresh` base, set or unset, is fetched before branching.
    Team,
}

/// A failed team trunk fetch and the base used instead.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct TrunkFetchFailure {
    pub remote_ref: String,
    pub fallback: String,
    pub error: String,
}

impl std::fmt::Display for TrunkFetchFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "could not fetch {} for the team worktree ({}); branching from ",
            self.remote_ref, self.error
        )?;
        if self.fallback == self.remote_ref {
            write!(f, "{} as last fetched", self.fallback)
        } else {
            write!(f, "local {}", self.fallback)
        }
    }
}

pub fn create(
    repo_root: &Path,
    config: &WorktreeConfig,
    name: Option<&str>,
    base: Option<WorktreeBase>,
    branch: Option<&str>,
    reuse_existing: bool,
    launch_base: LaunchBase,
) -> Result<CreatedWorktree> {
    config.hooks.validate()?;
    ensure_repo(repo_root)?;
    let FreshWorktree {
        name,
        path,
        branch: derived_branch,
    } = match resolve_fresh_worktree(repo_root, config, name, None, reuse_existing)? {
        WorktreeCreateTarget::Fresh(fresh) => fresh,
        WorktreeCreateTarget::Reuse(reused) => return Ok(*reused),
    };
    let branch = resolve_branch(branch, derived_branch.as_deref(), &name)?;

    let configured_base = base.or_else(|| config.base.clone());
    let defaulted = configured_base.is_none();
    let mut base = configured_base.unwrap_or(match launch_base {
        LaunchBase::Configured => WorktreeBase::Head,
        LaunchBase::Team => WorktreeBase::Fresh,
    });
    let mut trunk_fetch = None;
    let team_trunk = launch_base == LaunchBase::Team && base == WorktreeBase::Fresh;
    if team_trunk {
        if git_stdout(repo_root, ["remote", "get-url", "origin"]).is_ok() {
            (base, trunk_fetch) = fetch_team_trunk(repo_root);
        } else if defaulted {
            base = WorktreeBase::Head;
        }
    }
    let remote_trunk = match &base {
        WorktreeBase::Explicit(remote_ref) if team_trunk => Some(remote_ref),
        _ => None,
    };
    let checkout_base_ref = match remote_trunk {
        Some(remote_ref) => format!("refs/remotes/{remote_ref}"),
        None => base.as_refspec().to_owned(),
    };
    let base_ref = resolve_base_commit(repo_root, &checkout_base_ref)?;
    let base_branch = resolve_base_branch(repo_root, &base);
    let stale_base = match remote_trunk {
        Some(_) => None,
        None => base_branch
            .as_deref()
            .and_then(|branch| stale_base(repo_root, branch)),
    };
    let mut created = add_worktree(
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
        &config.hooks,
    )?;
    created.stale_base = stale_base;
    created.trunk_fetch = trunk_fetch;
    Ok(created)
}

fn fetch_team_trunk(repo_root: &Path) -> (WorktreeBase, Option<TrunkFetchFailure>) {
    let cached_head = git_stdout(repo_root, ["symbolic-ref", "refs/remotes/origin/HEAD"])
        .ok()
        .and_then(|head| {
            head.strip_prefix("refs/remotes/origin/")
                .map(|branch| format!("origin/{branch}"))
        });
    let mut remote_ref = cached_head.clone().unwrap_or_else(|| {
        format!(
            "origin/{}",
            trunk_ref(repo_root).as_deref().unwrap_or("HEAD")
        )
    });
    let fetched = (|| -> Result<()> {
        if cached_head.is_none() {
            let output = git_network_output(
                repo_root,
                ["ls-remote", "--symref", "origin", "HEAD"],
                TRUNK_FETCH_TIMEOUT,
            )?;
            let output = String::from_utf8_lossy(&output.stdout);
            let branch = output
                .lines()
                .find_map(|line| {
                    let (target, name) = line.split_once('\t')?;
                    if name != "HEAD" {
                        return None;
                    }
                    target.strip_prefix("ref: refs/heads/")
                })
                .ok_or_else(|| {
                    WorktreeErr::Parse("origin did not advertise a default branch".into())
                })?;
            remote_ref = format!("origin/{branch}");
        }
        let branch = remote_ref
            .strip_prefix("origin/")
            .ok_or_else(|| WorktreeErr::Parse("origin/HEAD points outside origin".into()))?;
        let refspec = format!("+refs/heads/{branch}:refs/remotes/{remote_ref}");
        git_network_output(
            repo_root,
            [
                "fetch",
                "--no-tags",
                "--no-recurse-submodules",
                "origin",
                &refspec,
            ],
            TRUNK_FETCH_TIMEOUT,
        )?;
        if cached_head.is_none() {
            git_run(
                repo_root,
                [
                    "symbolic-ref",
                    "refs/remotes/origin/HEAD",
                    &format!("refs/remotes/{remote_ref}"),
                ],
            )?;
        }
        Ok(())
    })();
    let Err(error) = fetched else {
        return (WorktreeBase::Explicit(remote_ref), None);
    };
    let (base, fallback) =
        if resolve_base_commit(repo_root, &format!("refs/remotes/{remote_ref}")).is_ok() {
            (
                WorktreeBase::Explicit(remote_ref.clone()),
                remote_ref.clone(),
            )
        } else {
            (
                WorktreeBase::Head,
                current_branch(repo_root).unwrap_or_else(|| "HEAD".into()),
            )
        };
    let error = match error {
        WorktreeErr::Git { stderr, .. } => stderr,
        error => error.to_string(),
    };
    (
        base,
        Some(TrunkFetchFailure {
            remote_ref,
            fallback,
            error: error.split_whitespace().collect::<Vec<_>>().join(" "),
        }),
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
    pr_branch: Option<PrBranchChoice>,
    cwd: Option<&Path>,
    base: LaunchBase,
) -> Result<LaunchCheckout> {
    if let Some(cwd) = cwd {
        return Ok(LaunchCheckout {
            stale_base: None,
            trunk_fetch: None,
            reused: false,
            created: None,
            cwd: cwd.to_path_buf(),
            branch: current_branch(cwd),
            repo_root: None,
            worktree_name: None,
            review_only_reason: None,
            generated_name: false,
            ownership: CheckoutOwnership::User,
        });
    }
    let repo_root = workspace.launch_repo_root();
    if let Some(pr) = from_pr {
        if workspace.cwd_project_root.is_none() && workspace.root_class != RootClass::Repo {
            return Err(WorktreeErr::LaunchPrRequiresRepo);
        }
        let name = worktree.map(str::trim).filter(|name| !name.is_empty());
        let created = create_from_pr(repo_root, config, pr, name, None, name.is_some(), pr_branch)?;
        let created_marker = (!created.reused).then(|| created.marker.clone());
        let review_only_reason = created.review_only_reason;
        let marker = created.marker;
        return Ok(LaunchCheckout {
            stale_base: None,
            trunk_fetch: None,
            reused: created.reused,
            created: created_marker,
            branch: current_branch(&marker.worktree_path),
            cwd: marker.worktree_path,
            repo_root: Some(repo_root.to_path_buf()),
            worktree_name: Some(marker.name),
            review_only_reason,
            generated_name: false,
            ownership: CheckoutOwnership::Rimz,
        });
    }

    let Some(raw_name) = worktree else {
        return Ok(LaunchCheckout {
            stale_base: None,
            trunk_fetch: None,
            reused: false,
            created: None,
            branch: current_branch(&workspace.worktree_root),
            cwd: workspace.worktree_root.clone(),
            repo_root: workspace.cwd_project_root.clone(),
            worktree_name: None,
            review_only_reason: None,
            generated_name: false,
            ownership: CheckoutOwnership::User,
        });
    };
    if workspace.cwd_project_root.is_none() && workspace.root_class != RootClass::Repo {
        return Err(WorktreeErr::LaunchWorktreeRequiresRepo);
    }
    let name = raw_name.trim();
    let created = create(
        repo_root,
        config,
        (!name.is_empty()).then_some(name),
        None,
        None,
        !name.is_empty(),
        base,
    )?;
    let created_marker = (!created.reused).then(|| created.marker.clone());
    let marker = created.marker;
    Ok(LaunchCheckout {
        stale_base: created.stale_base,
        trunk_fetch: created.trunk_fetch,
        reused: created.reused,
        created: created_marker,
        branch: current_branch(&marker.worktree_path),
        cwd: marker.worktree_path,
        repo_root: Some(repo_root.to_path_buf()),
        worktree_name: Some(marker.name),
        review_only_reason: None,
        generated_name: name.is_empty(),
        ownership: CheckoutOwnership::Rimz,
    })
}

/// Resolve a user-owned linked checkout without adopting or seeding it.
/// Callers must obtain confirmation before launching into the returned checkout.
pub fn resolve_unmanaged_launch_checkout(
    workspace: &ResolvedWorkspace,
    config: &WorktreeConfig,
    name: &str,
) -> Result<LaunchCheckout> {
    let repo_root = workspace.launch_repo_root();
    let name = parse_requested_name(name)?.name;
    let path = worktree_path(repo_root, config, &name)?;
    let canonical_path = path.canonicalize()?;
    let common_dir = git_stdout(
        repo_root,
        ["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let classify_checkout_error = |err| match err {
        WorktreeErr::Git { .. } => WorktreeErr::NotLinkedWorktree {
            path: path.clone(),
            repo_root: repo_root.to_path_buf(),
        },
        other => other,
    };
    let checkout_common_dir = git_stdout(
        &path,
        ["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .map_err(classify_checkout_error)?;
    let checkout_root =
        git_stdout(&path, ["rev-parse", "--show-toplevel"]).map_err(classify_checkout_error)?;
    if Path::new(&common_dir).canonicalize()? != Path::new(&checkout_common_dir).canonicalize()?
        || Path::new(&checkout_root).canonicalize()? != canonical_path
        || repo_root.canonicalize()? == canonical_path
    {
        return Err(WorktreeErr::NotLinkedWorktree {
            path,
            repo_root: repo_root.to_path_buf(),
        });
    }
    Ok(LaunchCheckout {
        stale_base: None,
        trunk_fetch: None,
        reused: false,
        created: None,
        branch: current_branch(&path),
        cwd: path,
        repo_root: Some(repo_root.to_path_buf()),
        worktree_name: Some(name),
        review_only_reason: None,
        generated_name: false,
        ownership: CheckoutOwnership::User,
    })
}

/// Remove the named worktree checked out at `path`, refusing unless `force`
/// when `protections` or the checkout's own Git state says it is unsafe.
/// Callers pass the tree's real path, which can sit outside the configured
/// worktree directory, and the same [`ProtectionSet`] the wrapper and `gc`
/// paths build, so an explicit removal answers to the live room rather than to
/// Git alone.
pub fn remove(
    repo_root: &Path,
    config: &WorktreeConfig,
    name: &str,
    path: &Path,
    force: bool,
    protections: &ProtectionSet,
    memo: &LandedMemo,
) -> Result<RemovalOutcome> {
    ensure_repo(repo_root)?;
    let marker = owned_marker(name, path)?;
    let state = inspect(path, &marker, protections, memo)?;
    if !force {
        match state.keep_reasons().first() {
            None => {}
            Some(KeepReason::InUse) => {
                return Err(WorktreeErr::InUse {
                    name: name.to_owned(),
                });
            }
            Some(KeepReason::Dirty) => {
                return Err(WorktreeErr::Dirty {
                    name: name.to_owned(),
                    files: state.status.changed_files,
                });
            }
            Some(KeepReason::Unlanded) => {
                if let (Some(ahead), Some(target)) = (state.status.ahead, state.status.target) {
                    return Err(WorktreeErr::Unlanded {
                        name: name.to_owned(),
                        ahead,
                        target,
                    });
                }
                return Err(WorktreeErr::Unproven {
                    name: name.to_owned(),
                });
            }
            Some(KeepReason::Unknown) => {
                return Err(WorktreeErr::Unproven {
                    name: name.to_owned(),
                });
            }
        }
    }
    remove_marked_worktree(repo_root, path, &marker, force, &config.hooks, memo)
}

/// Resolve one named RimZ-owned worktree without accepting an arbitrary Git
/// checkout at the configured path.
pub fn resolve_owned(
    repo_root: &Path,
    config: &WorktreeConfig,
    name: &str,
) -> Result<ManagedWorktree> {
    ensure_repo(repo_root)?;
    let path = worktree_path(repo_root, config, name)?;
    let marker = owned_marker(name, &path)?;
    let branch = current_branch(&path);
    Ok(ManagedWorktree {
        marker,
        path,
        branch,
    })
}

/// Resolve a local landing branch from the status comparison and its checkout.
pub fn landing_plan(repo_root: &Path, config: &WorktreeConfig, name: &str) -> Result<LandingPlan> {
    let managed = resolve_owned(repo_root, config, name)?;
    let memo = LandedMemo::load(repo_root);
    let target = comparison_ref(&managed.path, &managed.marker, &memo);
    memo.save();
    let target = target
        .and_then(|target| {
            let symbolic = git_stdout(
                &managed.path,
                ["rev-parse", "--symbolic-full-name", &target],
            )
            .ok()?;
            if let Some(branch) = symbolic.strip_prefix("refs/heads/") {
                return Some(branch.to_owned());
            }
            let remote = symbolic.strip_prefix("refs/remotes/")?;
            let branches = git_stdout(
                repo_root,
                [
                    "for-each-ref",
                    "--format=%(refname:short) %(upstream:short)",
                    "refs/heads",
                ],
            )
            .ok()?;
            let branches: Vec<_> = branches
                .lines()
                .map(|row| row.split_once(' ').unwrap_or((row, "")))
                .collect();
            branches
                .iter()
                .find_map(|(branch, upstream)| (*upstream == remote).then(|| (*branch).to_owned()))
                .or_else(|| {
                    let trunk = trunk_ref(repo_root)?;
                    let trunk = trunk.strip_prefix("origin/").unwrap_or(&trunk);
                    branches
                        .iter()
                        .find_map(|(branch, _)| (*branch == trunk).then(|| (*branch).to_owned()))
                })
        })
        .ok_or_else(|| WorktreeErr::NoLandingTarget {
            name: name.to_owned(),
        })?;
    let target_checkout =
        parse_worktree_list(&git_stdout(repo_root, ["worktree", "list", "--porcelain"])?)
            .into_iter()
            .find(|row| row.branch.as_deref() == Some(target.as_str()))
            .map(|row| row.path)
            .ok_or_else(|| WorktreeErr::TargetNotCheckedOut {
                name: name.to_owned(),
                target: target.clone(),
            })?;
    Ok(LandingPlan {
        managed,
        target,
        target_checkout,
    })
}

/// Rebase a clean managed branch onto its planned target, then fast-forward it.
/// A failed rebase is aborted before returning; the target is never forced.
pub fn land(
    plan: &LandingPlan,
    protections: &ProtectionSet,
    memo: &LandedMemo,
) -> Result<WorktreeMerge> {
    let LandingPlan {
        managed,
        target,
        target_checkout,
    } = plan;
    let name = &managed.marker.name;
    let state = inspect(&managed.path, &managed.marker, protections, memo)?;
    if !state.holders.is_empty() {
        return Err(WorktreeErr::MergeInUse {
            name: name.to_owned(),
            path: managed.path.clone(),
        });
    }
    if current_branch(target_checkout).as_deref() != Some(target) {
        return Err(WorktreeErr::TargetNotCheckedOut {
            name: name.to_owned(),
            target: target.clone(),
        });
    }
    if repository_operation_in_progress(target_checkout) {
        return Err(WorktreeErr::MergeOperationInProgress {
            name: name.to_owned(),
            checkout: format!("the {target} checkout"),
        });
    }
    if repository_operation_in_progress(&managed.path) {
        return Err(WorktreeErr::MergeOperationInProgress {
            name: name.to_owned(),
            checkout: "the worktree checkout".to_owned(),
        });
    }
    if !git_stdout(target_checkout, ["status", "--porcelain"])?.is_empty() {
        return Err(WorktreeErr::MergeDirtyTarget {
            name: name.to_owned(),
            target: target.clone(),
        });
    }
    if state.status.dirty() {
        return Err(WorktreeErr::MergeDirtyWorktree {
            name: name.to_owned(),
        });
    }
    let branch = current_branch(&managed.path);
    let actual_branch = branch.as_deref().unwrap_or("detached HEAD");
    if actual_branch != managed.marker.branch {
        return Err(WorktreeErr::MergeBranchMismatch {
            name: name.to_owned(),
            expected: managed.marker.branch.clone(),
            actual: actual_branch.to_owned(),
        });
    }

    let target_head = git_stdout(target_checkout, ["rev-parse", "HEAD"])?;
    if let Err(err) = git_run(
        &managed.path,
        ["-c", "rebase.updateRefs=false", "rebase", target],
    ) {
        let _ = git_run(&managed.path, ["rebase", "--abort"]);
        return Err(WorktreeErr::RebaseFailed {
            name: name.to_owned(),
            target: target.clone(),
            stderr: git_error_detail(err),
        });
    }
    if !git_stdout(&managed.path, ["status", "--porcelain"])?.is_empty() {
        return Err(WorktreeErr::MergeDirtyAfterRebase {
            name: name.to_owned(),
            target: target.clone(),
        });
    }
    if current_branch(target_checkout).as_deref() != Some(target)
        || git_stdout(target_checkout, ["rev-parse", "HEAD"])? != target_head
        || !git_stdout(target_checkout, ["status", "--porcelain"])?.is_empty()
    {
        return Err(WorktreeErr::TargetChanged {
            name: name.to_owned(),
            target: target.clone(),
        });
    }

    let head = git_stdout(&managed.path, ["rev-parse", "HEAD"])?;
    if let Err(err) = git_run(target_checkout, ["merge", "--ff-only", head.as_str()]) {
        return Err(WorktreeErr::FastForwardFailed {
            name: name.to_owned(),
            target: target.clone(),
            stderr: git_error_detail(err),
        });
    }
    Ok(WorktreeMerge {
        name: managed.marker.name.clone(),
        branch: managed.marker.branch.clone(),
        head,
    })
}

/// Remove every clean, landed RimZ-owned worktree with no holders in one repo.
/// Per-worktree failures are retained in the report so one broken checkout
/// does not prevent the remaining safe candidates from being reclaimed.
pub fn sweep_owned(
    repo_root: &Path,
    protections: &ProtectionSet,
    store: &crate::Store,
    session_name: &str,
    dry_run: bool,
    hooks: &WorktreeHooks,
) -> Result<WorktreeSweep> {
    hooks.validate()?;
    let memo = LandedMemo::load(repo_root);
    let entries = inspect_owned_with_memo(repo_root, protections, &memo)?;
    let (candidates, kept) = partition_sweep_candidates(entries);
    let mut sweep = WorktreeSweep {
        kept,
        ..WorktreeSweep::default()
    };
    for inspected in candidates {
        let entry = inspected.managed;
        let bytes = crate::disk::usage::dir_size(&entry.path);
        if dry_run {
            sweep.removed.push(SweptWorktree {
                name: entry.marker.name,
                branch: entry.marker.branch,
                path: entry.path,
                bytes,
                branch_deletion: None,
                archive_error: None,
            });
            continue;
        }
        match remove_marked_worktree(repo_root, &entry.path, &entry.marker, false, hooks, &memo) {
            Ok(removed) => {
                let retirement = retire_removal(
                    store,
                    &removed,
                    WORKTREE_REMOVED_ARCHIVE_REASON,
                    session_name,
                );
                if let Err(err) = retirement.session_retirement {
                    tracing::warn!(
                        worktree = %removed.worktree_name,
                        error = %err,
                        "could not retire sessions for removed worktree",
                    );
                }
                let archive_error = retirement.message_archival.err().map(|err| err.to_string());
                sweep.removed.push(SweptWorktree {
                    name: removed.worktree_name,
                    branch: removed.branch,
                    path: removed.removed_path,
                    bytes,
                    branch_deletion: Some(removed.branch_deletion),
                    archive_error,
                });
            }
            Err(err) => sweep.failed.push(FailedWorktree {
                path: entry.path,
                error: err.to_string(),
            }),
        }
    }
    if !sweep.removed.is_empty()
        && !dry_run
        && let Err(err) = git_run(repo_root, ["worktree", "prune"])
    {
        tracing::debug!(error = %err, "could not prune worktree metadata after sweep");
    }
    memo.save();
    Ok(sweep)
}

fn partition_sweep_candidates(
    entries: Vec<InspectedWorktree>,
) -> (Vec<InspectedWorktree>, Vec<KeptWorktree>) {
    let mut candidates = Vec::new();
    let mut kept = Vec::new();
    for entry in entries {
        let reasons = entry.state.keep_reasons();
        if !reasons.is_empty() {
            kept.push(KeptWorktree {
                name: entry.managed.marker.name,
                path: entry.managed.path,
                reasons,
                holders: entry.state.holders,
            });
        } else {
            candidates.push(entry);
        }
    }
    (candidates, kept)
}

pub fn remove_marked_worktree(
    repo_root: &Path,
    path: &Path,
    marker: &WorktreeMarker,
    force: bool,
    hooks: &WorktreeHooks,
    memo: &LandedMemo,
) -> Result<RemovalOutcome> {
    let target = comparison_ref(repo_root, marker, memo);
    remove_checkout(repo_root, path, marker, force, hooks)?;
    let branch_tip = git_stdout(
        repo_root,
        [
            "rev-parse",
            "--verify",
            &format!("refs/heads/{}", marker.branch),
        ],
    )
    .ok();
    let branch_deletion = delete_branch(repo_root, marker, force, memo)?;
    Ok(RemovalOutcome {
        repo_root: repo_root.to_path_buf(),
        from_pr: marker.from_pr,
        worktree_name: marker.name.clone(),
        branch: marker.branch.clone(),
        removed_path: path.to_owned(),
        branch_deletion,
        branch_tip,
        target,
    })
}

fn remove_checkout(
    repo_root: &Path,
    path: &Path,
    marker: &WorktreeMarker,
    force: bool,
    hooks: &WorktreeHooks,
) -> Result<()> {
    hooks.validate()?;
    ensure_repo(repo_root)?;
    leave_worktree_before_removal(repo_root, path)?;
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    let path_arg = path.to_string_lossy();
    args.push(path_arg.as_ref());
    git_run(repo_root, args)?;
    if let Err(error) =
        crate::lsp::registry::stop_checkout(path, crate::lsp::registry::StopReason::CheckoutRemoved)
    {
        tracing::debug!(%error, "language-server checkout removal notification failed");
    }
    if let Err(error) = hooks::run_hook(
        hooks,
        hooks::WorktreeHookEvent::Removed,
        marker,
        hooks::TIMEOUT,
    ) {
        tracing::warn!(worktree = %marker.name, error = %error, "worktree.removed hook failed");
    }
    Ok(())
}

fn repository_operation_in_progress(cwd: &Path) -> bool {
    [
        "rebase-merge",
        "rebase-apply",
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
    ]
    .iter()
    .any(|name| {
        git_stdout(cwd, ["rev-parse", "--git-path", name])
            .map(|path| {
                let path = PathBuf::from(path);
                if path.is_absolute() {
                    path.exists()
                } else {
                    cwd.join(path).exists()
                }
            })
            .unwrap_or(true)
    })
}

fn git_error_detail(err: WorktreeErr) -> String {
    match err {
        WorktreeErr::Git { stderr, .. } => stderr,
        other => other.to_string(),
    }
}

pub fn discover_owned(repo_root: &Path) -> Result<Vec<ManagedWorktree>> {
    ensure_repo(repo_root)?;
    let rows = parse_worktree_list(&git_stdout(repo_root, ["worktree", "list", "--porcelain"])?);
    owned_worktrees(&rows)
}

fn owned_worktrees(rows: &[WorktreeRow]) -> Result<Vec<ManagedWorktree>> {
    let mut entries = Vec::new();
    for row in rows {
        let Some(marker) = read_marker_from_checkout_metadata(&row.path)? else {
            continue;
        };
        entries.push(ManagedWorktree {
            marker,
            path: row.path.clone(),
            branch: row.branch.clone(),
        });
    }
    entries.sort_by(|a, b| a.marker.name.cmp(&b.marker.name));
    Ok(entries)
}

const OWNED_WORKTREE_ROWS_TTL: Duration = Duration::from_secs(60);

struct OwnedWorktreeRows {
    stamp: Option<crate::disk::parse_cache::FileStamp>,
    observed_at: Instant,
    rows: Vec<WorktreeRow>,
}

static OWNED_WORKTREE_ROWS: LazyLock<Mutex<HashMap<PathBuf, OwnedWorktreeRows>>> =
    LazyLock::new(Mutex::default);

/// Reuse Git enumeration for host polling, revalidating ownership markers by stat each call.
pub(crate) fn discover_owned_cached(repo_root: &Path) -> Result<Vec<ManagedWorktree>> {
    discover_owned_cached_at(repo_root, Instant::now())
}

fn discover_owned_cached_at(repo_root: &Path, now: Instant) -> Result<Vec<ManagedWorktree>> {
    let Some(git_dir) = git_admin_dir_from_checkout_metadata(repo_root)? else {
        return discover_owned(repo_root);
    };
    let common = std::fs::read_to_string(git_dir.join("commondir"))
        .map(|text| git_dir.join(text.trim()))
        .unwrap_or(git_dir);
    let stamp = std::fs::metadata(common.join("worktrees"))
        .ok()
        .map(|meta| crate::disk::parse_cache::FileStamp::from_metadata(&meta));
    let rows = {
        let mut cache = OWNED_WORKTREE_ROWS
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let fresh = cache.get(repo_root).is_some_and(|cached| {
            cached.stamp == stamp
                && now.saturating_duration_since(cached.observed_at) < OWNED_WORKTREE_ROWS_TTL
        });
        if !fresh {
            ensure_repo(repo_root)?;
            let rows =
                parse_worktree_list(&git_stdout(repo_root, ["worktree", "list", "--porcelain"])?);
            cache.insert(
                repo_root.to_path_buf(),
                OwnedWorktreeRows {
                    stamp,
                    observed_at: now,
                    rows,
                },
            );
        }
        cache[repo_root].rows.clone()
    };
    owned_worktrees(&rows)
}

pub fn status(
    worktree: &Path,
    marker: &WorktreeMarker,
    memo: &LandedMemo,
) -> Result<WorktreeStatus> {
    let porcelain = git_stdout(worktree, ["status", "--porcelain"])?;
    let target = comparison_ref(worktree, marker, memo);
    let (landed, ahead) = target
        .as_deref()
        .map(|comparison| memo.proof(worktree, comparison, "HEAD"))
        .unwrap_or((LandedVerdict::Unknown, None));
    Ok(WorktreeStatus {
        changed_files: porcelain.lines().count().try_into().unwrap_or(u32::MAX),
        ahead,
        landed,
        target,
    })
}

pub fn inspect(
    path: &Path,
    marker: &WorktreeMarker,
    protections: &ProtectionSet,
    memo: &LandedMemo,
) -> Result<WorktreeState> {
    Ok(WorktreeState {
        status: status(path, marker, memo)?,
        holders: protections.holders(path),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InspectedWorktree {
    pub managed: ManagedWorktree,
    pub state: WorktreeState,
}

pub fn inspect_owned(
    repo_root: &Path,
    protections: &ProtectionSet,
) -> Result<Vec<InspectedWorktree>> {
    let memo = LandedMemo::load(repo_root);
    let inspected = inspect_owned_with_memo(repo_root, protections, &memo);
    memo.save();
    inspected
}

fn inspect_owned_with_memo(
    repo_root: &Path,
    protections: &ProtectionSet,
    memo: &LandedMemo,
) -> Result<Vec<InspectedWorktree>> {
    let entries = discover_owned(repo_root)?;
    Ok(crate::utils::parallel::bounded_map(
        crate::lane::WorkLane::Other,
        8,
        &entries,
        |managed| {
            let state =
                inspect(&managed.path, &managed.marker, protections, memo).unwrap_or_else(|_| {
                    WorktreeState {
                        status: WorktreeStatus::unknown(),
                        holders: protections.holders(&managed.path),
                    }
                });
            InspectedWorktree {
                managed: managed.clone(),
                state,
            }
        },
    ))
}

fn leave_worktree_before_removal(repo_root: &Path, path: &Path) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let cwd = normalize_path_lexical(&cwd);
    let path = normalize_path_lexical(path);
    if cwd == path || cwd.starts_with(path) {
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
        name: crate::ids::dashed_name(raw),
        branch: raw.contains('/').then(|| raw.to_owned()),
    })
}

pub fn read_marker_for_worktree(path: &Path) -> Result<Option<WorktreeMarker>> {
    let marker = match marker_path(path) {
        Ok(path) => path,
        Err(WorktreeErr::Git { .. }) | Err(WorktreeErr::Io(_)) => return Ok(None),
        Err(err) => return Err(err),
    };
    read_marker_file(&marker)
}

fn owned_marker(name: &str, path: &Path) -> Result<WorktreeMarker> {
    if let Some(marker) = read_marker_for_worktree(path)? {
        return Ok(marker);
    }
    Err(if path.exists() {
        WorktreeErr::Unmarked {
            name: name.to_owned(),
            path: path.to_owned(),
        }
    } else {
        WorktreeErr::Missing {
            name: name.to_owned(),
            path: path.to_owned(),
        }
    })
}

/// A checkout's relationship to a room's project root.
pub enum CheckoutKind<'a> {
    Main,
    Linked(&'a OsStr),
    Other,
}

/// Classify an exact checkout using Git metadata, without spawning Git.
pub fn classify_checkout<'a>(root: &'a Path, project: &Path) -> CheckoutKind<'a> {
    if root == project {
        return CheckoutKind::Main;
    }
    if root.join(".git").is_file()
        && linked_worktree(root).is_some_and(|linked| linked.primary == project)
        && let Some(name) = root.file_name()
    {
        return CheckoutKind::Linked(name);
    }
    CheckoutKind::Other
}

const MARKER_PARSE_LIMIT: usize = 1_024;

#[derive(Default)]
struct MarkerParses {
    entries: HashMap<PathBuf, (crate::disk::parse_cache::FileStamp, WorktreeMarker)>,
    order: VecDeque<PathBuf>,
}

/// Successful parses shared across all readers' threads, oldest out at 1,024 entries.
/// Sound only because every marker write uses `write_marker`'s temp-then-rename;
/// future writers must do the same. Read-modify-write callers stay uncached.
static MARKER_PARSES: LazyLock<Mutex<MarkerParses>> = LazyLock::new(Mutex::default);

/// Read a RimZ marker by following the checkout's `.git` metadata on every call. This
/// keeps sidebar projection code off the git subprocess path. It may serve a stat-validated
/// memoized parse (`MARKER_PARSES`); a read-modify-write caller uses `read_marker_for_worktree`.
pub(crate) fn read_marker_from_checkout_metadata(path: &Path) -> Result<Option<WorktreeMarker>> {
    let Some(marker) = marker_path_from_checkout_metadata(path)? else {
        return Ok(None);
    };
    let mut cache = MARKER_PARSES.lock().unwrap_or_else(PoisonError::into_inner);
    let stamp = std::fs::metadata(&marker)
        .ok()
        .map(|meta| crate::disk::parse_cache::FileStamp::from_metadata(&meta));
    if let Some(stamp) = stamp
        && let Some((cached_stamp, value)) = cache.entries.get(&marker)
        && *cached_stamp == stamp
    {
        return Ok(Some(value.clone()));
    }
    if cache.entries.remove(&marker).is_some() {
        cache.order.retain(|path| path != &marker);
    }
    let value = read_marker_file(&marker)?;
    if let (Some(stamp), Some(value)) = (stamp, &value) {
        if cache.entries.len() == MARKER_PARSE_LIMIT
            && let Some(oldest) = cache.order.pop_front()
        {
            cache.entries.remove(&oldest);
        }
        cache.order.push_back(marker.clone());
        cache.entries.insert(marker, (stamp, value.clone()));
    }
    Ok(value)
}

/// Resolves the checkout holding `cwd`, the nearest ancestor with a `.git` entry. `None` for a primary checkout, a directory outside git, or metadata that cannot be read. Never spawns git.
pub(crate) fn linked_worktree(cwd: &Path) -> Option<LinkedWorktree> {
    let checkout = cwd.ancestors().find(|dir| dir.join(".git").exists())?;
    let git_dir = match git_admin_dir_from_checkout_metadata(checkout) {
        Ok(Some(path)) => path,
        Ok(None) => return None,
        Err(error) => {
            tracing::debug!(%error, "worktree metadata unavailable");
            return None;
        }
    };
    if git_dir == checkout.join(".git") {
        return None;
    }
    let common = match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(text) => text,
        Err(error) => {
            tracing::debug!(%error, "worktree common directory unavailable");
            return None;
        }
    };
    let common_dir = normalize_path_lexical(&git_dir.join(common.trim()));
    if common_dir.file_name() != Some(OsStr::new(".git")) {
        tracing::debug!(common_dir = %common_dir.display(), "worktree primary checkout unavailable");
        return None;
    }
    let primary = common_dir.parent()?.to_path_buf();
    let base_branch = match read_marker_from_checkout_metadata(checkout) {
        Ok(marker) => marker.and_then(|marker| marker.base_branch),
        Err(error) => {
            tracing::debug!(%error, "worktree base branch unavailable");
            None
        }
    };
    Some(LinkedWorktree {
        base_branch,
        primary,
    })
}

/// Resolve current lane lifetimes once from a report's full record set.
pub fn lane_lifetimes<'a>(records: impl IntoIterator<Item = &'a AgentState>) -> LaneLifetimes {
    let mut by_path = HashMap::new();
    for path in records.into_iter().filter_map(|record| record.lane_path()) {
        let path = Path::new(path);
        if by_path.contains_key(path) {
            continue;
        }
        let lifetime = path
            .try_exists()
            .map_err(WorktreeErr::from)
            .and_then(|exists| {
                if !exists {
                    return Ok(LaneLifetime::Removed);
                }
                read_marker_from_checkout_metadata(path).map(|marker| {
                    marker.map_or(LaneLifetime::Unbounded, |marker| {
                        LaneLifetime::Since(marker.created_at)
                    })
                })
            })
            .unwrap_or_else(|err| LaneLifetime::Unreadable {
                reason: err.to_string(),
            });
        by_path.insert(path.to_owned(), lifetime);
    }
    LaneLifetimes::new(by_path)
}

fn read_marker_file(marker: &Path) -> Result<Option<WorktreeMarker>> {
    match std::fs::read_to_string(marker) {
        Ok(text) => {
            #[cfg(feature = "testkit")]
            crate::testkit::WORK_COUNTERS
                .marker_reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            serde_json::from_str(&text).map(Some).map_err(Into::into)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn marker_path_from_checkout_metadata(worktree: &Path) -> Result<Option<PathBuf>> {
    Ok(git_admin_dir_from_checkout_metadata(worktree)?.map(|path| path.join(MARKER_FILE)))
}

#[derive(Debug, thiserror::Error)]
#[error("cannot read checkout metadata at {path}: {source}")]
pub(crate) struct CheckoutDiscoveryErr {
    pub(crate) path: PathBuf,
    pub(crate) source: std::io::Error,
}

/// Find the enclosing checkout by resolving `.git` metadata and requiring HEAD.
pub(crate) fn discover_checkout(
    start: &Path,
) -> std::result::Result<Option<PathBuf>, CheckoutDiscoveryErr> {
    for ancestor in start.ancestors() {
        let git = git_admin_dir_from_checkout_metadata(ancestor).map_err(|error| {
            CheckoutDiscoveryErr {
                path: ancestor.join(".git"),
                source: match error {
                    WorktreeErr::Io(source) => source,
                    error => std::io::Error::other(error),
                },
            }
        })?;
        if git.is_some_and(|dir| dir.join("HEAD").is_file()) {
            return Ok(Some(ancestor.to_path_buf()));
        }
    }
    Ok(None)
}

/// Resolve a checkout's Git admin directory without spawning Git.
///
/// This does not validate repository contents: even an empty `.git` directory passes.
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

fn marker_path(worktree: &Path) -> Result<PathBuf> {
    let git_dir = git_stdout(worktree, ["rev-parse", "--git-dir"])?;
    let path = PathBuf::from(git_dir.trim());
    Ok(if path.is_absolute() {
        path
    } else {
        worktree.join(path)
    }
    .join(MARKER_FILE))
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
    Reuse(Box<CreatedWorktree>),
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
            return Ok(WorktreeCreateTarget::Reuse(Box::new(CreatedWorktree {
                stale_base: None,
                trunk_fetch: None,
                reused: true,
                marker,
                included: 0,
                linked: 0,
                push_destination: None,
                review_only_reason: None,
            })));
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
    hooks: &WorktreeHooks,
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
                "--no-track",
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
    finish_worktree(repo_root, name, path, branch, provenance, checkout, hooks)
}

fn finish_worktree(
    repo_root: &Path,
    name: String,
    path: PathBuf,
    branch: String,
    provenance: MarkerProvenance,
    checkout: Checkout<'_>,
    hooks: &WorktreeHooks,
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
    if let Err(error) = hooks::run_hook(
        hooks,
        hooks::WorktreeHookEvent::Created,
        &marker,
        hooks::TIMEOUT,
    ) {
        let removal = match checkout {
            Checkout::Existing => remove_checkout(repo_root, &path, &marker, true, hooks),
            Checkout::NewBranch(_) | Checkout::Tracking(_) => {
                let memo = LandedMemo::load(repo_root);
                let removed = remove_marked_worktree(repo_root, &path, &marker, true, hooks, &memo);
                memo.save();
                removed.map(|_| ())
            }
        };
        let rollback = match removal {
            Ok(_) => "tree removed".to_owned(),
            Err(error) => format!("rollback failed: {error}"),
        };
        return Err(WorktreeErr::CreatedHook {
            reason: error.to_string(),
            rollback,
        });
    }
    Ok(CreatedWorktree {
        stale_base: None,
        trunk_fetch: None,
        reused: false,
        marker,
        included,
        linked,
        push_destination: None,
        review_only_reason: None,
    })
}

fn write_marker(path: &Path, marker: &WorktreeMarker) -> Result<()> {
    crate::disk::atomic::write_temp_then_rename(&marker_path(path)?, marker).map_err(Into::into)
}

fn resolve_base_commit(repo_root: &Path, base_ref: &str) -> Result<String> {
    let commitish = format!("{base_ref}^{{commit}}");
    git_stdout(repo_root, ["rev-parse", "--verify", commitish.as_str()])
}

fn stale_base(repo_root: &Path, branch: &str) -> Option<StaleBase> {
    let upstream = git_stdout(
        repo_root,
        [
            "rev-parse",
            "--abbrev-ref",
            "--verify",
            &format!("{branch}@{{upstream}}"),
        ],
    )
    .ok()?;
    let behind = git_stdout(
        repo_root,
        [
            "rev-list",
            "--count",
            &format!("{branch}..{upstream}"),
            "--",
        ],
    )
    .ok()?
    .parse::<u64>()
    .ok()?;
    (behind > 0).then(|| StaleBase {
        branch: branch.to_owned(),
        upstream,
        behind,
    })
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

pub fn current_branch(repo_root: &Path) -> Option<String> {
    git_stdout(repo_root, ["symbolic-ref", "--short", "HEAD"]).ok()
}

fn origin_head(repo_root: &Path) -> Option<String> {
    git_stdout(
        repo_root,
        ["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .ok()
}

fn comparison_ref(cwd: &Path, marker: &WorktreeMarker, memo: &LandedMemo) -> Option<String> {
    let trunk = trunk_ref(cwd);
    if let Some(base_branch) = marker.base_branch.as_deref()
        && ref_resolves(cwd, base_branch)
        && !base_branch_superseded(cwd, base_branch, trunk.as_deref(), memo)
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
fn base_branch_superseded(
    cwd: &Path,
    base_branch: &str,
    trunk: Option<&str>,
    memo: &LandedMemo,
) -> bool {
    let Some(trunk) = trunk else {
        return false;
    };
    base_branch != trunk
        && !is_ancestor(cwd, base_branch, trunk)
        && memo.proof(cwd, trunk, base_branch).0 == LandedVerdict::Landed
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

pub(crate) fn content_landed(cwd: &Path, comparison_ref: &str, head_ref: &str) -> LandedVerdict {
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
            // Fail closed if tree lookup violates the resolved-commit invariant.
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
            LandedVerdict::Unlanded
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
        LandedVerdict::Unlanded
    }
}

/// True when `head` sits on the trunk's first-parent lineage — the chain the
/// trunk itself advanced through. A HEAD there holds no work of its own: it only
/// tracked the trunk by fresh fork, rebase onto a newer trunk, or fast-forward.
/// Landed side-branch tips stay off this lineage. Scan capped at
/// `LANDED_BASE_SCAN_CAP`.
pub(crate) fn on_trunk_first_parent(cwd: &Path, trunk: &str, head: &str) -> bool {
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

fn delete_branch(
    repo_root: &Path,
    marker: &WorktreeMarker,
    force: bool,
    memo: &LandedMemo,
) -> Result<BranchDeletion> {
    let branch = marker.branch.as_str();
    let flag = if force { "-D" } else { "-d" };
    match git_run(repo_root, ["branch", flag, branch]) {
        Ok(()) => Ok(BranchDeletion::Deleted),
        Err(WorktreeErr::Git { stderr, .. }) if branch_already_gone(&stderr) => {
            Ok(BranchDeletion::Deleted)
        }
        Err(err) if force => Err(err),
        Err(WorktreeErr::Git { stderr, .. }) if branch_delete_failed_unmerged(&stderr) => {
            if branch_landed(repo_root, marker, memo) {
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

fn branch_landed(repo_root: &Path, marker: &WorktreeMarker, memo: &LandedMemo) -> bool {
    let Some(comparison) = comparison_ref(repo_root, marker, memo) else {
        return false;
    };
    memo.proof(repo_root, &comparison, &marker.branch).0 == LandedVerdict::Landed
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
