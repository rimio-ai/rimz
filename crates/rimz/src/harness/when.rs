//! State predicates for clock-evaluated loop conditions.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::account::{RateLimitsCache, read_rate_limits_cache};
use crate::agents::{ProviderCapacity, RateLimitWindow, RoomLoginSet, WindowSpan};
use crate::forge::pr_state::{
    PrQueueFact, PrReview, PrStateCache, SettledMergeability, UNSUPPORTED_REPO_KEY,
};
use crate::ids::{AgentKind, LoginKey, LoginName};
use crate::store::snapshot::{WorktreeCi, WorktreePrState};

pub mod condition;

/// A room's last-known CI and PR readings. Absence of this source means unknown.
pub struct CiSource(PrStateCache);

/// Satisfaction of a conjunction, counting repeated leaves separately.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ConditionCount {
    pub met: usize,
    pub total: usize,
    pub not_read: usize,
}

/// Why a forge clause has no reading in the cached probe result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnreadReason {
    NotChecked,
    NoForge,
    LookupFailing { last_tried: u64 },
    NoPr { branch: Option<String> },
    NoCi { branch: Option<String> },
    NoOpenPr { branch: Option<String> },
    PrFactsUnavailable { branch: Option<String> },
}

/// A `team.stage` condition key: the scope checkout's board, or with `[branch]` the board of the
/// checkout on that branch. Every reader of a `team.stage` term parses it here.
#[derive(Clone, Copy)]
pub(in crate::harness) struct TeamStageKey<'a> {
    branch: Option<&'a str>,
}

impl<'a> TeamStageKey<'a> {
    const NAME: &'static str = "team.stage";

    fn parse(key: &'a str) -> Option<Self> {
        let rest = key.strip_prefix(Self::NAME)?;
        if rest.is_empty() {
            return Some(Self { branch: None });
        }
        let branch = rest.strip_prefix('[')?.strip_suffix(']')?;
        Some(Self {
            branch: Some(branch),
        })
    }

    pub(in crate::harness) fn branch(self) -> Option<&'a str> {
        self.branch
    }

    /// The board's stage. A branch reads the checkout on it from the cached `git worktree list`
    /// rows, primary and linked alike, and reads nothing when no checkout is on that branch.
    fn read(self, scope: &Path) -> Option<String> {
        let checkout = match self.branch {
            None => scope.to_path_buf(),
            Some(branch) => crate::worktree::checkout_on_branch_cached(scope, branch)
                .unwrap_or_else(|error| {
                    tracing::debug!(%error, branch, "listing checkouts for team.stage");
                    None
                })?,
        };
        crate::harness::scratch::board_stage(&checkout).map(|stage| stage.name)
    }
}

/// The condition keys read from the room's forge cache. This is the one place
/// that names them: the parser, the probe scopes, and the no-room hint ask it.
#[derive(Clone, Copy)]
enum ForgeKey<'a> {
    Ci(Option<&'a str>),
    Pr(Option<&'a str>),
    PrQueue(Option<&'a str>),
    PrMergeable(Option<&'a str>),
    PrReview(Option<&'a str>),
}

impl<'a> ForgeKey<'a> {
    fn all(branch: Option<&'a str>) -> [Self; 5] {
        [
            Self::Ci(branch),
            Self::Pr(branch),
            Self::PrQueue(branch),
            Self::PrMergeable(branch),
            Self::PrReview(branch),
        ]
    }

    fn name(self) -> &'static str {
        match self {
            Self::Ci(_) => "ci",
            Self::Pr(_) => "pr",
            Self::PrQueue(_) => "pr.queue",
            Self::PrMergeable(_) => "pr.mergeable",
            Self::PrReview(_) => "pr.review",
        }
    }

    fn parse(key: &'a str) -> Option<Self> {
        let (key, branch) = match key.split_once('[') {
            Some((key, branch)) => (key, Some(branch.strip_suffix(']')?)),
            None => (key, None),
        };
        Self::all(branch)
            .into_iter()
            .find(|candidate| candidate.name() == key)
    }

    fn branch(self) -> Option<&'a str> {
        match self {
            Self::Ci(branch)
            | Self::Pr(branch)
            | Self::PrQueue(branch)
            | Self::PrMergeable(branch)
            | Self::PrReview(branch) => branch,
        }
    }

    fn values(self) -> &'static [&'static str] {
        match self {
            Self::Ci(_) => &["passed", "failed", "pending", "none"],
            Self::Pr(_) => &["open", "merged", "closed", "none"],
            Self::PrQueue(_) => &["queued", "dequeued", "armed", "none"],
            Self::PrMergeable(_) => &["mergeable", "conflicting", "none"],
            Self::PrReview(_) => &["draft", "changes_requested", "required", "approved", "none"],
        }
    }

    fn read(self, cache: &PrStateCache, scope: &Path) -> Option<&'static str> {
        let scope = scope.to_string_lossy();
        let (link, head_ci) = if let Some(branch) = self.branch() {
            let repo = cache.path_repos.get(scope.as_ref())?;
            let row = cache.branches.get(repo)?.get(branch)?;
            (row.pr.as_ref(), row.ci)
        } else {
            (
                cache.states.get(scope.as_ref()),
                cache.branch_ci.get(scope.as_ref()).copied(),
            )
        };
        let open = link
            .filter(|link| link.state == WorktreePrState::Open)
            .and_then(|link| link.open.as_ref());
        match self {
            Self::Ci(_) => link
                .filter(|link| {
                    matches!(link.state, WorktreePrState::Open | WorktreePrState::Merged)
                })
                .and_then(|link| link.ci)
                .or(head_ci)
                .map(|ci| match ci {
                    WorktreeCi::Passing => "passed",
                    WorktreeCi::Failing => "failed",
                    WorktreeCi::Pending => "pending",
                }),
            Self::Pr(_) => link.map(|link| match link.state {
                WorktreePrState::Open => "open",
                WorktreePrState::Merged => "merged",
                WorktreePrState::Closed => "closed",
            }),
            Self::PrQueue(_) => link.map(|link| match link.queue() {
                Some(PrQueueFact::Queued { .. }) => "queued",
                Some(PrQueueFact::Dequeued { .. }) => "dequeued",
                None if open.is_some_and(|facts| facts.auto_merge) => "armed",
                None => "none",
            }),
            Self::PrMergeable(_) => link?.current_mergeability().map(|settled| match settled {
                SettledMergeability::Mergeable(_) => "mergeable",
                SettledMergeability::Conflicting(_) => "conflicting",
            }),
            Self::PrReview(_) => open?.review.map(|review| match review {
                PrReview::Draft => "draft",
                PrReview::ChangesRequested => "changes_requested",
                PrReview::Required => "required",
                PrReview::Approved => "approved",
                PrReview::None => "none",
            }),
        }
    }
}

impl CiSource {
    /// The cached PR number for this checkout, without probing the forge.
    pub fn pr_number(&self, scope: &Path) -> Option<u64> {
        self.0.states.get(scope.to_string_lossy().as_ref())?.number
    }

    /// Explain an absent forge reading without changing condition truth.
    pub fn unread_reason(&self, key: &str, scope: &Path) -> Option<UnreadReason> {
        if self.reading(ForgeKey::parse(key)?, scope).is_some() {
            return None;
        }
        self.probe_unread_reason(key, scope)
    }

    /// Explain a missing forge reading from the probe, ignoring retained facts.
    pub(crate) fn probe_unread_reason(&self, key: &str, scope: &Path) -> Option<UnreadReason> {
        Some(self.probe_unread(ForgeKey::parse(key)?, scope))
    }

    /// A forge key's reading: its value, or `none` when the last successful probe found no PR,
    /// no CI, or no open PR to carry the fact. A probe that has not succeeded, and an open PR
    /// whose fact the forge has not reported, leave it unread.
    fn reading(&self, key: ForgeKey<'_>, scope: &Path) -> Option<&'static str> {
        key.read(&self.0, scope)
            .or_else(|| match self.probe_unread(key, scope) {
                UnreadReason::NoPr { .. }
                | UnreadReason::NoCi { .. }
                | UnreadReason::NoOpenPr { .. } => Some("none"),
                UnreadReason::NotChecked
                | UnreadReason::NoForge
                | UnreadReason::LookupFailing { .. }
                | UnreadReason::PrFactsUnavailable { .. } => None,
            })
    }

    fn probe_unread(&self, key: ForgeKey<'_>, scope: &Path) -> UnreadReason {
        let path = scope.to_string_lossy();
        let Some(repo) = self.0.path_repos.get(path.as_ref()) else {
            return UnreadReason::NotChecked;
        };
        if repo.is_empty() || repo == UNSUPPORTED_REPO_KEY {
            return UnreadReason::NoForge;
        }
        let Some(probe) = self.0.repos.get(repo) else {
            return UnreadReason::NotChecked;
        };
        if !probe.ok {
            return UnreadReason::LookupFailing {
                last_tried: probe.refreshed_at_ms,
            };
        }
        let link = match key.branch() {
            // A probe writes a row for every branch it looked at; no row is no look.
            Some(branch) => match self.0.branches.get(repo).and_then(|rows| rows.get(branch)) {
                Some(row) => row.pr.as_ref(),
                None => return UnreadReason::NotChecked,
            },
            None => self.0.states.get(path.as_ref()),
        };
        let branch = key.branch().map(str::to_owned);
        if link.is_none() && !matches!(key, ForgeKey::Ci(None)) {
            return UnreadReason::NoPr { branch };
        }
        match key {
            ForgeKey::Ci(_) | ForgeKey::Pr(_) => UnreadReason::NoCi { branch },
            ForgeKey::PrQueue(_) | ForgeKey::PrMergeable(_) | ForgeKey::PrReview(_) => {
                if link.is_some_and(|link| link.state != WorktreePrState::Open) {
                    UnreadReason::NoOpenPr { branch }
                } else {
                    UnreadReason::PrFactsUnavailable { branch }
                }
            }
        }
    }

    /// The successful probe time, available only when the latest attempt succeeded.
    pub fn checked_at(&self, scope: &Path) -> Option<Timestamp> {
        let repo = self.0.path_repos.get(scope.to_string_lossy().as_ref())?;
        if repo.is_empty() || repo == UNSUPPORTED_REPO_KEY {
            return None;
        }
        let probe = self.0.repos.get(repo).filter(|probe| probe.ok)?;
        Timestamp::from_millisecond(i64::try_from(probe.refreshed_at_ms).ok()?).ok()
    }

    /// The open PR's known head and the commit its CI verdict describes, if known.
    pub(in crate::harness) fn ci_commits(&self, scope: &Path) -> Option<(&str, Option<&str>)> {
        let link = self.0.states.get(scope.to_string_lossy().as_ref())?;
        if link.state != WorktreePrState::Open {
            return None;
        }
        let head = link.open.as_ref()?.head.as_str();
        if head.is_empty() {
            return None;
        }
        Some((head, link.ci_sha.as_deref().filter(|sha| !sha.is_empty())))
    }

    pub fn read(runtime: &crate::RuntimePaths) -> Self {
        Self(crate::forge::pr_state::read_pr_state_cache(
            &runtime.pr_state_path(),
        ))
    }

    /// Why bare `ci` must not decide in `scope` under the commit fence, if it must not.
    ///
    /// Compares the CI SHA with the cached PR head, and that head with a fresh local HEAD read
    /// after the cache. Settlement uses the PR head at `checked_at`, without re-reading under a
    /// lock: a later commit or push is a new fact for the next evaluation.
    pub(in crate::harness) fn ci_fence(&self, scope: &Path) -> Option<String> {
        let (pr_head, ci_sha) = self.ci_commits(scope)?;
        let local_head = head(scope);
        let on_pr = local_head
            .as_deref()
            .filter(|local| *local != pr_head)
            .and_then(|local| is_ancestor(scope, local, pr_head));
        ci_unread(pr_head, ci_sha, local_head.as_deref(), on_pr)
    }
}

/// The instant a forge reading must be probed at or after to decide, and the reason a key
/// probed before it shows.
#[derive(Clone, Copy)]
pub(in crate::harness) struct Floor<'a> {
    pub at: Timestamp,
    pub stale: &'a str,
}

/// Each of `forge_keys` whose reading must not decide, with its reason: every key unless the
/// last successful probe (`checked_at`) is at or after the floor, and bare `ci` when
/// `ci_fence` holds it. `probe_unread` words a key no successful probe covers.
pub(in crate::harness) fn forge_unread<'k>(
    forge_keys: impl IntoIterator<Item = &'k str>,
    checked_at: Option<Timestamp>,
    floor: Floor<'_>,
    ci_fence: Option<String>,
    probe_unread: impl Fn(&str) -> String,
) -> BTreeMap<String, String> {
    let fresh = checked_at.is_some_and(|at| at >= floor.at);
    forge_keys
        .into_iter()
        .filter_map(|key| {
            let reason = match (fresh, checked_at) {
                (false, Some(_)) => floor.stale.to_owned(),
                (false, None) => probe_unread(key),
                (true, _) if key == "ci" => ci_fence.clone()?,
                (true, _) => return None,
            };
            Some((key.to_owned(), reason))
        })
        .collect()
}

pub(in crate::harness) fn ci_unread(
    pr_head: &str,
    ci_sha: Option<&str>,
    local_head: Option<&str>,
    on_pr: Option<bool>,
) -> Option<String> {
    if pr_head.is_empty() {
        return None;
    }
    if let Some(local) = local_head.filter(|local| *local != pr_head)
        && on_pr == Some(false)
    {
        return Some(format!(
            "local HEAD {local:.7} is not on the PR, PR head {pr_head:.7}"
        ));
    }
    let ci_sha = ci_sha.filter(|sha| !sha.is_empty() && *sha != pr_head)?;
    Some(format!("read at {ci_sha:.7}, PR head {pr_head:.7}"))
}

/// The checkout's HEAD commit, read by `git rev-parse` within two seconds.
pub(in crate::harness) fn head(path: &Path) -> Option<String> {
    let output = crate::proc::run_bounded_git_output(
        path,
        &["rev-parse", "HEAD"],
        std::time::Duration::from_secs(2),
    )
    .ok()?;
    if output.timed_out || !output.status.success() {
        return None;
    }
    Some(String::from_utf8(output.stdout).ok()?.trim().to_owned())
}

/// `Some(false)` only on a definite exit 1; an unknown object (128), any other exit, a timeout,
/// or a spawn error is `None`.
fn is_ancestor(path: &Path, commit: &str, tip: &str) -> Option<bool> {
    let output = crate::proc::run_bounded_git_output(
        path,
        &["merge-base", "--is-ancestor", "--", commit, tip],
        std::time::Duration::from_secs(2),
    )
    .ok()?;
    if output.timed_out {
        return None;
    }
    match output.status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

/// The stored kind-wide provider windows of every account, with the room's
/// account selection, read at most once and only when a `window.*` term asks
/// for one.
pub struct WindowReadings<'a> {
    runtime: Option<&'a crate::RuntimePaths>,
    now: Timestamp,
    capacities: std::cell::OnceCell<(RoomLoginSet, BTreeMap<LoginKey, ProviderCapacity>)>,
}

impl<'a> WindowReadings<'a> {
    pub fn new(runtime: Option<&'a crate::RuntimePaths>, now: Timestamp) -> Self {
        Self {
            runtime,
            now,
            capacities: std::cell::OnceCell::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn fixed(now: Timestamp, capacities: BTreeMap<LoginKey, ProviderCapacity>) -> Self {
        Self {
            runtime: None,
            now,
            capacities: std::cell::OnceCell::from((RoomLoginSet::native(), capacities)),
        }
    }

    /// Percent of the `span` window left, projected to now, on `account` of
    /// `kind`, else on the room's account for it; a lifted window has no limit.
    fn left(
        &self,
        kind: &AgentKind,
        account: Option<&LoginName>,
        span: WindowSpan,
    ) -> Option<String> {
        let (room, capacities) = self.capacities.get_or_init(|| self.read());
        let key = match account {
            Some(name) => LoginKey::new(kind.clone(), name.clone()),
            None => room.default_key(kind)?,
        };
        let window = capacities.get(&key)?.window_of_span(span, self.now)?;
        Some(percent_left(&window)?.to_string())
    }

    fn read(&self) -> (RoomLoginSet, BTreeMap<LoginKey, ProviderCapacity>) {
        let Some(runtime) = self.runtime else {
            return (RoomLoginSet::native(), BTreeMap::new());
        };
        let capacities = read_rate_limits_cache(&RateLimitsCache::shared_path(runtime))
            .entries
            .into_iter()
            .filter(|(key, entry)| key.scope.is_kind_wide() && entry.scope.is_kind_wide())
            .map(|(key, entry)| {
                (
                    key.login,
                    ProviderCapacity::from_windows(entry.limits.windows),
                )
            })
            .collect();
        (RoomLoginSet::for_runtime(runtime), capacities)
    }
}

/// Percent of a window left; a lifted window has no limit.
pub(super) fn percent_left(window: &RateLimitWindow) -> Option<u8> {
    if window.lifted {
        return Some(100);
    }
    Some(100 - window.used_percentage?.min(100))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    /// The expression's three-valued truth: `None` is unknown.
    pub truth: Option<bool>,
    pub readings: BTreeMap<String, Option<String>>,
    /// Each forge key held unread by [`Verdict::freshen`], with its reason; each reads `None`.
    pub unread: BTreeMap<String, String>,
}

impl Verdict {
    /// Hold a loop condition's verdict to the forge freshness rule: every forge key is unread
    /// without a source or without a successful probe at or after `fired_at`, the task's fire
    /// stamp, and bare `ci` under the commit fence when `fence` is set. The fence's git reads
    /// run only for a true verdict on a fresh probe. Each unread key reads `None`, and the
    /// verdict's truth is the Kleene walk over what remains: an unread key leaves it unknown only
    /// where the known terms do not settle it. Pass `now` to age a failing lookup's reason.
    pub fn freshen(
        mut self,
        expr: &WhenExpr,
        scope: &Path,
        source: Option<&CiSource>,
        fired_at: Option<Timestamp>,
        now: Option<Timestamp>,
        fence: bool,
    ) -> Self {
        let forge_keys = expr
            .terms()
            .filter(|term| term.reads_forge())
            .map(|term| term.key.as_str())
            .collect::<BTreeSet<_>>();
        self.unread = match source {
            None => forge_keys
                .iter()
                .map(|key| ((*key).to_owned(), "no sidebar source".to_owned()))
                .collect(),
            Some(source) => {
                let checked_at = source.checked_at(scope);
                let floor = Floor {
                    at: fired_at.unwrap_or(Timestamp::MIN),
                    stale: "probed before the last fire or first sight",
                };
                let fence = (fence
                    && self.truth == Some(true)
                    && forge_keys.contains("ci")
                    && checked_at.is_some_and(|at| at >= floor.at))
                .then(|| source.ci_fence(scope))
                .flatten();
                forge_unread(forge_keys, checked_at, floor, fence, |key| {
                    condition::unread_reason(source.probe_unread_reason(key, scope), now)
                })
            }
        };
        for key in self.unread.keys() {
            self.readings.insert(key.clone(), None);
        }
        self.truth = expr.truth(&self.readings);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhenState {
    #[serde(default)]
    pub(super) fingerprint: Option<(String, Option<std::time::Duration>, std::path::PathBuf)>,
    pub since: Timestamp,
    pub fired: bool,
}

impl WhenState {
    pub(super) fn matches_condition(
        &self,
        expr: &WhenExpr,
        hold: Option<std::time::Duration>,
        run_dir: &Path,
    ) -> bool {
        self.fingerprint
            .as_ref()
            .is_some_and(|(when, duration, scope)| {
                when == &expr.to_string() && *duration == hold && scope == run_dir
            })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConditionEvidence {
    #[serde(default)]
    pub since: Option<Timestamp>,
    pub when: String,
    pub hold: Option<String>,
    pub held_ms: u64,
    pub readings: BTreeMap<String, Option<String>>,
}

pub fn evaluate(
    expr: &WhenExpr,
    scope: &Path,
    ci_source: Option<&CiSource>,
    provider: Option<&AgentKind>,
    account: Option<&LoginName>,
    windows: &WindowReadings<'_>,
) -> Verdict {
    let mut readings = BTreeMap::new();
    for term in expr.terms() {
        readings.entry(term.key.clone()).or_insert_with(|| {
            if let Some(key) = TeamStageKey::parse(&term.key) {
                return key.read(scope);
            }
            if let Some(key) = ForgeKey::parse(&term.key) {
                return ci_source
                    .and_then(|source| source.reading(key, scope))
                    .map(str::to_owned);
            }
            // WhenExpr can only be constructed through the key-validating parser.
            let span = window_span(&term.key)
                .expect("the parser admits only team.stage, forge, and window keys");
            provider.and_then(|kind| windows.left(kind, account, span))
        });
    }
    Verdict {
        truth: expr.truth(&readings),
        readings,
        unread: BTreeMap::new(),
    }
}

/// The span a `window.<span>.left` key reads.
fn window_span(key: &str) -> Option<WindowSpan> {
    key.strip_prefix("window.")?
        .strip_suffix(".left")?
        .parse()
        .ok()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WhenExpr(Node);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Node {
    Term(WhenTerm),
    Not(Box<Self>),
    And(Box<Self>, Box<Self>),
    Or(Box<Self>, Box<Self>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WhenTerm {
    pub key: String,
    op: Op,
    pub values: Vec<String>,
}

/// How a term compares its reading: membership for named states, order for window percents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    In,
    NotIn,
    Ge,
    Le,
    Gt,
    Lt,
}

impl Op {
    /// Comparisons in lexing order: a two-byte token before its one-byte prefix.
    const COMPARISONS: [Self; 4] = [Self::Ge, Self::Le, Self::Gt, Self::Lt];

    fn token(self) -> &'static str {
        match self {
            Self::In => "=",
            Self::NotIn => "!=",
            Self::Ge => ">=",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Lt => "<",
        }
    }
}

impl WhenTerm {
    /// Whether this term reads the room's forge cache.
    pub fn reads_forge(&self) -> bool {
        ForgeKey::parse(&self.key).is_some()
    }

    /// The parsed key of a `team.stage` term, with or without a branch selector.
    pub(in crate::harness) fn team_stage(&self) -> Option<TeamStageKey<'_>> {
        TeamStageKey::parse(&self.key)
    }

    /// The emitted selector for this term's reading and its explicit matches, excluding forge
    /// branch and window terms. A branch stage matches any `board`, so the scope's default board
    /// does not narrow it to the scope's own flips.
    pub(in crate::harness) fn wake_subscription(
        &self,
    ) -> Option<(
        crate::harness::signal::SignalSelector,
        BTreeMap<String, String>,
    )> {
        use crate::harness::signal::SignalSelector;
        if let Some(key) = TeamStageKey::parse(&self.key) {
            let matches = key
                .branch()
                .map(|_| ("board".to_owned(), "*".to_owned()))
                .into_iter()
                .collect();
            return Some((
                SignalSelector::Exact(
                    TeamStageKey::NAME
                        .parse()
                        .expect("team.stage is a valid signal name"),
                ),
                matches,
            ));
        }
        let key = ForgeKey::parse(&self.key)?;
        if key.branch().is_some() {
            return None;
        }
        let family = match key {
            ForgeKey::Ci(_) => "ci",
            ForgeKey::Pr(_)
            | ForgeKey::PrQueue(_)
            | ForgeKey::PrMergeable(_)
            | ForgeKey::PrReview(_) => "pr",
        };
        Some((SignalSelector::Family(family.into()), BTreeMap::new()))
    }

    /// The term against a known reading, with the operator turned to show which way it fails.
    pub(in crate::harness) fn comparison_evidence(&self, reading: &str) -> String {
        let op = if self.matches(reading) {
            self.op.token()
        } else {
            match self.op {
                Op::In => "!=",
                Op::NotIn => "=",
                Op::Ge => "<",
                Op::Le => ">",
                Op::Gt => "<=",
                Op::Lt => ">=",
            }
        };
        format!("{}={reading}{op}{}", self.key, self.values.join(","))
    }

    /// The term's truth over a reading; an unread key (`None`) leaves it unknown.
    pub fn truth(&self, reading: Option<&str>) -> Option<bool> {
        reading.map(|reading| self.matches(reading))
    }

    /// Whether a known reading satisfies the term.
    pub fn matches(&self, reading: &str) -> bool {
        if self.op == Op::In {
            return self.values.iter().any(|value| value == reading);
        }
        if self.op == Op::NotIn {
            return !self.values.iter().any(|value| value == reading);
        }
        let (Ok(reading), Some(Ok(bound))) = (
            reading.parse::<u8>(),
            self.values.first().map(|value| value.parse::<u8>()),
        ) else {
            return false;
        };
        match self.op {
            Op::Ge => reading >= bound,
            Op::Le => reading <= bound,
            Op::Gt => reading > bound,
            Op::Lt => reading < bound,
            Op::In | Op::NotIn => unreachable!("membership returned above"),
        }
    }
}

impl fmt::Display for WhenTerm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}{}{}",
            self.key,
            self.op.token(),
            self.values.join(",")
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("clause {clause}, position {position}: {fix}")]
pub struct WhenError {
    clause: usize,
    position: usize,
    fix: String,
}

impl WhenExpr {
    /// Three-valued (Kleene) truth: an unread leaf is unknown, `true || unknown` is true and
    /// `false && unknown` is false. Every reader of a condition decides on this one walk.
    pub fn truth(&self, readings: &BTreeMap<String, Option<String>>) -> Option<bool> {
        self.0.truth(readings)
    }

    /// Leaves met, counting repeats; a leaf over an unread key counts as not read. A
    /// `satisfied` expression counts every leaf met.
    pub(super) fn condition_count(
        &self,
        readings: &[(String, Option<String>)],
        satisfied: bool,
    ) -> Option<ConditionCount> {
        if !self.0.is_conjunction() {
            return None;
        }
        let mut count = ConditionCount {
            met: 0,
            total: 0,
            not_read: 0,
        };
        for (term, negated) in self.leaves() {
            count.total += 1;
            let reading = readings
                .iter()
                .find(|(key, _)| key == &term.key)
                .and_then(|(_, value)| value.as_deref());
            match term.truth(reading).map(|truth| truth != negated) {
                _ if satisfied => count.met += 1,
                Some(true) => count.met += 1,
                Some(false) => {}
                None => count.not_read += 1,
            }
        }
        Some(count)
    }

    /// The leaf that keeps the expression from true: its comparison against the reading, or
    /// the key named unread.
    pub(crate) fn blocking_evidence(&self, verdict: &Verdict) -> String {
        let (term, negated) = self.0.blocking_term(&verdict.readings, true);
        match verdict.readings[&term.key].as_deref() {
            Some(reading) => format!(
                "{}{}",
                if negated { "!" } else { "" },
                term.comparison_evidence(reading)
            ),
            None => format!("{} unread", term.key),
        }
    }

    pub fn parse(clauses: &[String]) -> Result<Self, WhenError> {
        Self::parse_with_stages(clauses, None)
    }

    /// Validate stage names against the effective configuration at admission.
    pub fn parse_with_stages(
        clauses: &[String],
        stages: Option<&std::collections::BTreeSet<String>>,
    ) -> Result<Self, WhenError> {
        let mut joined = None;
        let mut nodes = 0;
        for (index, clause) in clauses.iter().enumerate() {
            let mut parser = Parser {
                input: clause,
                pos: 0,
                clause: index + 1,
                depth: 0,
                nodes,
                stages,
            };
            if joined.is_some() {
                parser.node()?;
            }
            let node = parser.or()?;
            parser.space();
            if parser.pos != clause.len() {
                return Err(parser.error(if parser.rest().starts_with(')') {
                    "unbalanced parentheses; remove the extra ')'"
                } else {
                    "expected && or || between terms"
                }));
            }
            nodes = parser.nodes;
            joined = Some(match joined {
                None => node,
                Some(previous) => Node::And(Box::new(previous), Box::new(node)),
            });
        }
        joined.map(Self).ok_or_else(|| WhenError {
            clause: 1,
            position: 1,
            fix: "empty term; use key=value".to_owned(),
        })
    }

    /// Leaf terms in expression order, including repeated keys.
    pub fn terms(&self) -> impl Iterator<Item = &WhenTerm> {
        self.leaves().map(|(term, _)| term)
    }

    /// Leaf terms in expression order, each with whether an odd number of `!` wraps it:
    /// a leaf's truth is `term.truth(reading)` flipped when `negated`.
    pub fn leaves(&self) -> impl Iterator<Item = (&WhenTerm, bool)> {
        let mut leaves = Vec::new();
        self.0.leaves(false, &mut leaves);
        leaves.into_iter()
    }

    /// Whether any term reads the room's forge cache.
    pub fn reads_forge(&self) -> bool {
        self.terms().any(WhenTerm::reads_forge)
    }

    /// Branch selectors named by forge terms, including repeats.
    pub(crate) fn forge_branches(&self) -> impl Iterator<Item = &str> {
        self.terms()
            .filter_map(|term| ForgeKey::parse(&term.key)?.branch())
    }

    /// The first key that reads the scope checkout rather than a named branch.
    pub fn scope_checkout_key(&self) -> Option<&str> {
        self.terms()
            .find(|term| {
                TeamStageKey::parse(&term.key).is_some_and(|key| key.branch().is_none())
                    || ForgeKey::parse(&term.key).is_some_and(|key| key.branch().is_none())
            })
            .map(|term| term.key.as_str())
    }

    /// The spans the expression's `window.*` terms read, in expression order.
    pub(super) fn window_spans(&self) -> impl Iterator<Item = WindowSpan> {
        self.terms().filter_map(|term| window_span(&term.key))
    }

    /// Unique readings in expression order rather than the map's key order.
    pub fn readings<'a>(
        &'a self,
        verdict: &'a Verdict,
    ) -> impl Iterator<Item = (&'a str, Option<&'a str>)> {
        let mut seen = std::collections::BTreeSet::new();
        self.terms()
            .filter(move |term| seen.insert(term.key.as_str()))
            .map(|term| {
                (
                    term.key.as_str(),
                    verdict
                        .readings
                        .get(&term.key)
                        .and_then(|value| value.as_deref()),
                )
            })
    }
}

impl fmt::Display for WhenExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.display(f, 0)
    }
}

impl Node {
    fn is_conjunction(&self) -> bool {
        match self {
            Self::Term(_) => true,
            Self::And(left, right) => left.is_conjunction() && right.is_conjunction(),
            Self::Not(node) => {
                let mut literal = node.as_ref();
                while let Self::Not(inner) = literal {
                    literal = inner;
                }
                matches!(literal, Self::Term(_))
            }
            Self::Or(..) => false,
        }
    }

    fn blocking_term(
        &self,
        readings: &BTreeMap<String, Option<String>>,
        expected: bool,
    ) -> (&WhenTerm, bool) {
        match self {
            Self::Term(term) => (term, !expected),
            Self::Not(node) => node.blocking_term(readings, !expected),
            // A child sharing the node's own truth decides it, so an unread child that the
            // known one outweighs is never named.
            Self::And(left, right) | Self::Or(left, right) => {
                if left.truth(readings) == self.truth(readings) {
                    left.blocking_term(readings, expected)
                } else {
                    right.blocking_term(readings, expected)
                }
            }
        }
    }

    fn leaves<'a>(&'a self, negated: bool, leaves: &mut Vec<(&'a WhenTerm, bool)>) {
        match self {
            Self::Term(term) => leaves.push((term, negated)),
            Self::Not(node) => node.leaves(!negated, leaves),
            Self::And(left, right) | Self::Or(left, right) => {
                left.leaves(negated, leaves);
                right.leaves(negated, leaves);
            }
        }
    }

    fn truth(&self, readings: &BTreeMap<String, Option<String>>) -> Option<bool> {
        match self {
            Self::Term(term) => term.truth(readings.get(&term.key).and_then(Option::as_deref)),
            Self::Not(node) => node.truth(readings).map(|value| !value),
            Self::And(left, right) => match (left.truth(readings), right.truth(readings)) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (Some(true), Some(true)) => Some(true),
                _ => None,
            },
            Self::Or(left, right) => match (left.truth(readings), right.truth(readings)) {
                (Some(true), _) | (_, Some(true)) => Some(true),
                (Some(false), Some(false)) => Some(false),
                _ => None,
            },
        }
    }

    fn display(&self, f: &mut fmt::Formatter<'_>, parent: u8) -> fmt::Result {
        let precedence = match self {
            Self::Or(..) => 1,
            Self::And(..) => 2,
            Self::Not(_) => 3,
            Self::Term(_) => 4,
        };
        if precedence < parent {
            f.write_str("(")?;
        }
        match self {
            Self::Term(term) => write!(f, "{term}")?,
            Self::Not(node) => {
                f.write_str("!")?;
                node.display(f, precedence)?;
            }
            Self::And(left, right) | Self::Or(left, right) => {
                left.display(f, precedence)?;
                f.write_str(if matches!(self, Self::And(..)) {
                    " && "
                } else {
                    " || "
                })?;
                right.display(f, precedence)?;
            }
        }
        if precedence < parent {
            f.write_str(")")?;
        }
        Ok(())
    }
}

struct Parser<'a> {
    input: &'a str,
    pos: usize,
    clause: usize,
    depth: usize,
    nodes: usize,
    stages: Option<&'a std::collections::BTreeSet<String>>,
}

impl Parser<'_> {
    fn node(&mut self) -> Result<(), WhenError> {
        if self.nodes >= 128 {
            return Err(self.error("expression exceeds 128 nodes; simplify the condition"));
        }
        self.nodes += 1;
        Ok(())
    }

    fn rest(&self) -> &str {
        &self.input[self.pos..]
    }
    fn space(&mut self) {
        self.pos += self.rest().len() - self.rest().trim_start().len();
    }
    fn take(&mut self, token: &str) -> bool {
        self.space();
        if self.rest().starts_with(token) {
            self.pos += token.len();
            true
        } else {
            false
        }
    }
    fn error(&self, fix: impl Into<String>) -> WhenError {
        WhenError {
            clause: self.clause,
            position: self.input[..self.pos].chars().count() + 1,
            fix: fix.into(),
        }
    }
    fn word(&mut self) -> Result<String, WhenError> {
        self.space();
        let len = self
            .rest()
            .bytes()
            .take_while(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(byte))
            .count();
        if len == 0 {
            return Err(self.error("empty term; use key=value"));
        }
        let word = self.rest()[..len].to_owned();
        self.pos += len;
        Ok(word)
    }

    fn key(&mut self) -> Result<String, WhenError> {
        let mut key = self.word()?;
        let forge_keys = ForgeKey::all(None).map(ForgeKey::name);
        let branch_keys = std::iter::once(TeamStageKey::NAME)
            .chain(forge_keys)
            .collect::<Vec<_>>()
            .join(", ");
        if key != TeamStageKey::NAME
            && window_span(&key).is_none()
            && ForgeKey::parse(&key).is_none()
        {
            let keys = [TeamStageKey::NAME]
                .into_iter()
                .chain(forge_keys)
                .chain(["window.5h.left", "window.7d.left"])
                .collect::<Vec<_>>();
            let suggestion = crate::utils::suggest::nearest_name(&key, keys.iter().copied())
                .map(|name| format!("did you mean {name}? "))
                .unwrap_or_default();
            return Err(self.error(format!(
                "{suggestion}unknown key; keys: {}; {branch_keys} also accept [branch]",
                keys.join(", "),
            )));
        }
        if !self.take("[") {
            return Ok(key);
        }
        if window_span(&key).is_some() {
            return Err(self.error(format!(
                "{key} cannot take a branch selector; only {branch_keys} take one; use {key}>=40"
            )));
        }
        let value = ForgeKey::parse(&key).map_or("Done", |forge| forge.values()[0]);
        let fix = format!("{key}[branch]={value}");
        let Some(end) = self.rest().find(']') else {
            return Err(self.error(format!("missing ']' in branch selector; use {fix}")));
        };
        let branch = &self.rest()[..end];
        if branch.is_empty() || branch.chars().any(|ch| ch.is_whitespace() || ch == '[') {
            return Err(self.error(format!(
                "branch selector must be nonempty, without whitespace, '[' or ']'; use {fix}"
            )));
        }
        key.push('[');
        key.push_str(branch);
        key.push(']');
        self.pos += end + 1;
        Ok(key)
    }
    fn or(&mut self) -> Result<Node, WhenError> {
        let mut left = self.and()?;
        while self.take("||") {
            self.node()?;
            self.operand()?;
            left = Node::Or(Box::new(left), Box::new(self.and()?));
        }
        Ok(left)
    }
    fn and(&mut self) -> Result<Node, WhenError> {
        let mut left = self.not()?;
        while self.take("&&") {
            self.node()?;
            self.operand()?;
            left = Node::And(Box::new(left), Box::new(self.not()?));
        }
        Ok(left)
    }
    fn operand(&mut self) -> Result<(), WhenError> {
        self.space();
        if self.rest().is_empty() || self.rest().starts_with(')') {
            return Err(self.error("dangling operator; add a term"));
        }
        Ok(())
    }
    fn not(&mut self) -> Result<Node, WhenError> {
        if self.depth >= 128 {
            return Err(self.error("expression nesting is too deep; simplify the condition"));
        }
        self.depth += 1;
        let node = if self.take("!") {
            self.node()?;
            self.operand()?;
            Node::Not(Box::new(self.not()?))
        } else if self.take("(") {
            let node = self.or()?;
            if !self.take(")") {
                return Err(self.error("unbalanced parentheses; add ')'"));
            }
            node
        } else {
            self.node()?;
            let key = self.key()?;
            if window_span(&key).is_some() {
                return self.window_term(key);
            }
            let forge_key = ForgeKey::parse(&key);
            if Op::COMPARISONS.iter().any(|op| self.take(op.token())) {
                let value = self.word()?;
                return Err(self.error(format!(
                    "comparisons apply only to window keys; use {key}={value}"
                )));
            }
            let op = if self.take("!=") {
                Op::NotIn
            } else if self.take("=") {
                Op::In
            } else {
                return Err(self.error("expected '=' or '!='; use key=value"));
            };
            let mut values = vec![self.word()?];
            while self.take(",") {
                values.push(self.word()?);
            }
            if values.iter().any(|value| value == "unknown") {
                return Err(self.error(
                    "unknown is not a value: a term over an unread key is unknown by itself; \
                     match a value the key reads",
                ));
            }
            if TeamStageKey::parse(&key).is_some()
                && let Some(stages) = self.stages
            {
                for value in &values {
                    if !stages.contains(value) {
                        let suggestion = crate::utils::suggest::nearest_name(
                            value,
                            stages.iter().map(String::as_str),
                        )
                        .map(|name| format!("did you mean {name}? "))
                        .unwrap_or_default();
                        return Err(self.error(format!(
                            "{suggestion}no team defines stage {value}; stages: {}",
                            stages.iter().cloned().collect::<Vec<_>>().join(", ")
                        )));
                    }
                }
            }
            if let Some(allowed) = forge_key.map(ForgeKey::values)
                && let Some(value) = values
                    .iter()
                    .find(|value| !allowed.contains(&value.as_str()))
            {
                let suggestion =
                    crate::utils::suggest::nearest_name(value, allowed.iter().copied())
                        .map(|name| format!("did you mean {name}? "))
                        .unwrap_or_default();
                return Err(self.error(format!(
                    "{suggestion}unknown {key} value; values: {}",
                    allowed.join(", ")
                )));
            }
            Node::Term(WhenTerm { key, op, values })
        };
        self.depth -= 1;
        Ok(node)
    }

    /// A `window.<span>.left` comparison against a whole percent.
    fn window_term(&mut self, key: String) -> Result<Node, WhenError> {
        if self.take("!=") {
            let bound = self.percent(&key, Op::Lt)?;
            return Err(self.error(format!(
                "!= is not supported; use {key}<{bound} || {key}>{bound}"
            )));
        }
        if let Some(op) = Op::COMPARISONS.into_iter().find(|op| self.take(op.token())) {
            let bound = self.percent(&key, op)?;
            return Ok(Node::Term(WhenTerm {
                key,
                op,
                values: vec![bound.to_string()],
            }));
        }
        if self.take("=") {
            let bound = self.percent(&key, Op::Ge)?;
            return Err(self.error(format!(
                "= is not supported on window keys; use {key}>={bound} && {key}<={bound}"
            )));
        }
        Err(self.error(format!(
            "expected a comparison (>=, <=, >, <); use {key}>=40"
        )))
    }

    fn percent(&mut self, key: &str, op: Op) -> Result<u8, WhenError> {
        let op = op.token();
        let Ok(word) = self.word() else {
            return Err(self.error(format!(
                "expected a whole percent from 0 to 100; use {key}{op}40"
            )));
        };
        match word.parse::<i64>() {
            Ok(value) => u8::try_from(value)
                .ok()
                .filter(|percent| *percent <= 100)
                .ok_or_else(|| {
                    self.error(format!(
                        "percent left runs from 0 to 100; use {key}{op}{}",
                        value.clamp(0, 100)
                    ))
                }),
            Err(_) => Err(self.error(format!(
                "expected a whole percent from 0 to 100; use {key}{op}40"
            ))),
        }
    }
}

#[cfg(test)]
#[path = "when_tests.rs"]
mod tests;
