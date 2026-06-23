//! The per-worktree git facts: the activity-tiered, single-flighted diff-stats
//! refresh (trunk ref → merge-base → numstat + rev-list ×2 + status → landed
//! verdict → marker/merge state → branch), the group-root enumeration
//! (worktree checkouts / child repos), and their parsers.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::ledger::atomic;
use crate::ledger::single_flight::{self, Coalesced};
use crate::sidebar::cache::{
    DIFF_STATS_FOCUSED_COMMIT_TTL, DIFF_STATS_FOCUSED_LOCAL_TTL, DIFF_STATS_IDLE_TTL,
    DIFF_STATS_TTL, DiffStats, DiffStatsCache, DiffStatsCacheEntry, read_diff_stats_cache,
    unix_now_ms,
};
use crate::sidebar::enrich::{
    focused_worktree_paths, hot_worktree_paths, needed_worktree_paths, project_diff_stats,
};
use crate::worktree::{self, LandedVerdict};

/// How a non-producing sidebar waits for the elected producer's diff-stats
/// write before refreshing locally. ~300ms total (15 × 20ms) — wider than the
/// snapshot's ~200ms because the per-worktree git chain runs longer, yet still
/// well under the ~2s backstop tick.
const DIFF_STATS_WAIT_STEP: Duration = Duration::from_millis(20);
const DIFF_STATS_WAIT_STEPS: u32 = 15;

mod roots;

#[cfg(test)]
use crate::workspace::RootClass;
pub(super) use roots::project_group_roots;
#[cfg(test)]
use roots::{list_child_repo_roots, list_group_roots, list_worktree_roots};

/// Refresh the producer's per-worktree git facts, then project them onto the
/// snapshot's worktree groups. The git forks are the producer's job — a
/// consumer reads the published frame in process via
/// [`crate::sidebar::consumer::read_published_snapshot`] and never reaches here.
/// `configured_trunk` is the per-machine `[sidebar] trunk` preference the trunk
/// ladder tries first.
pub(super) fn enrich_worktree_groups(
    snapshot: &mut crate::SidebarSnapshot,
    runtime: &crate::RuntimePaths,
    configured_trunk: Option<&str>,
) {
    let cache_path = runtime.root.join("diff-stats.json");
    let now_ms = unix_now_ms();
    // The producer refreshes the live worktrees' diff stats (single-flighted,
    // git forks parallel across worktrees), then the shared projection folds the
    // resulting cache onto the groups — the same projection a consumer applies.
    // Focus tiers the edit-sensitive facts first; activity still keeps
    // recently-worked background worktrees on the hot TTL while the rest decay
    // to the idle TTL.
    let needed = needed_worktree_paths(snapshot);
    let focused = focused_worktree_paths(snapshot);
    let hot = hot_worktree_paths(snapshot);
    let cache = refresh_diff_stats(
        &cache_path,
        runtime,
        &needed,
        &focused,
        &hot,
        now_ms,
        configured_trunk,
    );
    project_diff_stats(snapshot, &cache);
}

/// Refresh the diff stats for `needed` worktree paths and return the cache map
/// to project. Single-flighted across the fleet, mirroring the snapshot cache:
/// the common case — every needed entry already fresh — touches no lock and
/// forks no git. Otherwise one elected producer forks git for the stale entries
/// and writes the shared cache once; the rest read its write back, or (if it
/// wedges) refresh locally for their own frame without writing — never
/// clobbering the producer's fresher map.
fn refresh_diff_stats(
    cache_path: &Path,
    runtime: &crate::RuntimePaths,
    needed: &[String],
    focused: &BTreeSet<String>,
    hot: &BTreeSet<String>,
    now_ms: u64,
    configured_trunk: Option<&str>,
) -> DiffStatsCache {
    // One closure carries the focus/activity tiering, and every freshness verdict —
    // the no-lock fast path, the single-flight loser's probe, and both produce
    // arms — goes through it, so the tiers cannot disagree and a loser never
    // spin-produces what the winner correctly skipped.
    let stale = |cache: &DiffStatsCache| -> Vec<(String, DueFacts)> {
        needed
            .iter()
            .filter_map(|path| {
                let (local_ttl, commit_ttl) = diff_stats_tier(path, focused, hot);
                let Some(entry) = cache.entries.get(path.as_str()) else {
                    return Some((path.clone(), DueFacts::all()));
                };
                let mut due = DueFacts {
                    local: !entry.local_fresh_for(now_ms, local_ttl),
                    commit: !entry.commit_fresh_for(now_ms, commit_ttl),
                };
                if local_ttl == commit_ttl {
                    due = DueFacts::same(due.local || due.commit);
                }
                due.any().then(|| (path.clone(), due))
            })
            .collect()
    };

    let cache = read_diff_stats_cache(cache_path);
    // Fast path: nothing stale — no lock, no git, as the all-fresh tick already
    // behaved before the single-flight.
    if stale(&cache).is_empty() {
        return cache;
    }

    let lock_path = runtime.root.join("diff-stats.lock");
    let fresh = || {
        let cache = read_diff_stats_cache(cache_path);
        stale(&cache).is_empty().then_some(cache)
    };
    match single_flight::coalesce(
        &lock_path,
        DIFF_STATS_WAIT_STEP,
        DIFF_STATS_WAIT_STEPS,
        fresh,
    ) {
        // A peer already refreshed every entry we need.
        Coalesced::Shared(cache) => cache,
        // We won: re-read (a peer may have written between our miss and the
        // lock), refresh only what is still stale against that read — git forks
        // run in parallel across worktrees — and write once.
        Coalesced::Produce(_guard) => {
            let mut cache = read_diff_stats_cache(cache_path);
            let refreshed = refresh_entries(&stale(&cache), &cache, configured_trunk);
            let changed = !refreshed.is_empty();
            for (path, entry) in refreshed {
                cache.entries.insert(path, entry);
            }
            if changed && let Err(err) = atomic::write_temp_then_rename_cache(cache_path, &cache) {
                tracing::warn!(path = %cache_path.display(), error = %err, "sidebar diff-stats cache write failed");
            }
            cache
        }
        // The producer wedged: refresh locally for our own frame, but do not
        // write — the producer's map will be fresher.
        Coalesced::ProduceLocal => {
            let mut cache = cache;
            for (path, entry) in refresh_entries(&stale(&cache), &cache, configured_trunk) {
                cache.entries.insert(path, entry);
            }
            cache
        }
    }
}

fn diff_stats_tier(
    path: &str,
    focused: &BTreeSet<String>,
    hot: &BTreeSet<String>,
) -> (Duration, Duration) {
    if focused.contains(path) {
        (DIFF_STATS_FOCUSED_LOCAL_TTL, DIFF_STATS_FOCUSED_COMMIT_TTL)
    } else if hot.contains(path) {
        (DIFF_STATS_TTL, DIFF_STATS_TTL)
    } else {
        (DIFF_STATS_IDLE_TTL, DIFF_STATS_IDLE_TTL)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DueFacts {
    local: bool,
    commit: bool,
}

impl DueFacts {
    fn all() -> Self {
        Self::same(true)
    }

    fn same(due: bool) -> Self {
        Self {
            local: due,
            commit: due,
        }
    }

    fn any(self) -> bool {
        self.local || self.commit
    }
}

struct LocalFacts {
    stats: Option<DiffStats>,
    branch: Option<String>,
    clean: Option<bool>,
    merge_in_progress: Option<bool>,
}

struct CommitFacts {
    commits: Option<u32>,
    behind: Option<u32>,
    trunk: Option<String>,
    landed: Option<bool>,
    did_work: Option<bool>,
}

/// Most worktrees probed concurrently. Each worktree's own chain stays
/// sequential (merge-base needs the trunk ref), but independent worktrees run in
/// parallel; the cap keeps a many-worktree fleet from bursting a fork storm.
const MAX_PARALLEL_GIT: usize = 8;

/// Refresh several worktrees' due diff-stats facts concurrently, returning each
/// path's merged entry. Independent worktrees run in parallel — bounded to
/// [`MAX_PARALLEL_GIT`] live `git` chains at a time — while each path's own
/// `trunk ref → merge-base → selected facts` chain stays sequential. Runs on
/// the diff-stats producer (the fetch worker), never the render thread.
fn refresh_entries(
    paths: &[(String, DueFacts)],
    cache: &DiffStatsCache,
    configured_trunk: Option<&str>,
) -> Vec<(String, DiffStatsCacheEntry)> {
    let mut out = Vec::with_capacity(paths.len());
    for chunk in paths.chunks(MAX_PARALLEL_GIT) {
        std::thread::scope(|scope| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|(path, due)| {
                    let prior = cache.entries.get(path.as_str()).cloned();
                    scope.spawn(move || {
                        (
                            path.clone(),
                            refresh_entry(path, prior.as_ref(), *due, configured_trunk),
                        )
                    })
                })
                .collect();
            for handle in handles {
                if let Ok(entry) = handle.join() {
                    out.push(entry);
                }
            }
        });
    }
    out
}

/// Produce a merged diff-stats entry for one worktree path. The trunk and
/// merge-base are resolved once when either half is due; local and commit facts
/// update only their own fields and completion stamps, so a focused worktree's
/// edit tick does not pay for commit/landed facts.
fn refresh_entry(
    path: &str,
    prior: Option<&DiffStatsCacheEntry>,
    due: DueFacts,
    configured_trunk: Option<&str>,
) -> DiffStatsCacheEntry {
    let mut entry = prior.cloned().unwrap_or_default();
    let worktree = Path::new(path);
    let trunk = trunk_ref(worktree, configured_trunk);
    let base = trunk
        .as_deref()
        .and_then(|trunk| diff_base(worktree, trunk));

    if due.local {
        let local = refresh_local_facts(worktree, base.as_deref());
        entry.added = local.stats.map(|stats| stats.added);
        entry.removed = local.stats.map(|stats| stats.removed);
        entry.branch = local.branch;
        entry.clean = local.clean;
        entry.merge_in_progress = local.merge_in_progress;
    }
    if due.commit {
        let commit = refresh_commit_facts(worktree, base.as_deref(), trunk.as_deref(), entry.clean);
        entry.commits = commit.commits;
        entry.behind = commit.behind;
        entry.trunk = commit.trunk;
        entry.landed = commit.landed;
        entry.did_work = commit.did_work;
    }

    let completed_at_ms = unix_now_ms();
    if due.local {
        entry.refreshed_at_ms = completed_at_ms;
    }
    if due.commit {
        entry.commit_refreshed_at_ms = Some(completed_at_ms);
    }
    entry
}

fn refresh_local_facts(worktree: &Path, base: Option<&str>) -> LocalFacts {
    let stats = base.and_then(|base| worktree_diff_stats(worktree, base));
    let status = worktree_status(worktree);
    let clean = status.as_ref().map(|status| status.clean);
    let merge_in_progress = merge_in_progress(worktree);
    // Untracked content is change the diff is blind to: fold its line count
    // into the `+` churn so an untracked-only worktree reads as carrying work,
    // never as landed.
    let stats = match (stats, &status) {
        (Some(stats), Some(status)) => Some(DiffStats {
            added: stats.added.saturating_add(status.untracked_added),
            removed: stats.removed,
        }),
        (stats, _) => stats,
    };
    LocalFacts {
        stats,
        branch: worktree_branch(worktree),
        clean,
        merge_in_progress,
    }
}

fn refresh_commit_facts(
    worktree: &Path,
    base: Option<&str>,
    trunk: Option<&str>,
    clean: Option<bool>,
) -> CommitFacts {
    let commits = base.and_then(|base| worktree_commits_ahead(worktree, base));
    let behind = base
        .zip(trunk)
        .and_then(|(base, trunk)| worktree_commits_behind(worktree, base, trunk));
    let landed = match (commits, clean, trunk) {
        (Some(0), _, _) => Some(true),
        (Some(_), Some(true), Some(trunk)) => {
            match worktree::content_landed(worktree, trunk, "HEAD") {
                LandedVerdict::Landed => Some(true),
                LandedVerdict::Pending => Some(false),
                LandedVerdict::Unknown => None,
            }
        }
        _ => None,
    };
    let head_sha = git_line(worktree, &["rev-parse", "HEAD"]);
    let did_work = worktree::read_marker_for_worktree(worktree)
        .ok()
        .flatten()
        .and_then(|marker| {
            head_sha
                .as_deref()
                .map(|head| head != marker.base_ref.as_str())
        });
    CommitFacts {
        commits,
        behind,
        trunk: trunk.map(ToOwned::to_owned),
        landed,
        did_work,
    }
}

fn merge_in_progress(worktree: &Path) -> Option<bool> {
    let mut args = vec!["rev-parse"];
    for name in [
        "rebase-merge",
        "rebase-apply",
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
    ] {
        args.push("--git-path");
        args.push(name);
    }
    let output = git_output(worktree, &args)?;
    if !output.status.success() {
        return None;
    }
    for raw in String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
    {
        if raw.is_empty() {
            continue;
        }
        if git_path_from_rev_parse(worktree, raw).exists() {
            return Some(true);
        }
    }
    Some(false)
}

fn git_path_from_rev_parse(worktree: &Path, raw: &str) -> std::path::PathBuf {
    let path = Path::new(raw);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        worktree.join(path)
    }
}

fn worktree_branch(worktree: &Path) -> Option<String> {
    let branch = git_line(worktree, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    // A detached HEAD has no branch to track — keep the reducer's path-basename
    // label rather than printing the literal "HEAD".
    if branch == "HEAD" { None } else { Some(branch) }
}

/// The total diff the worktree carries relative to `main`: committed, staged,
/// and unstaged changes folded into one `+/-`. We diff the *working tree*
/// against the `base` merge-base with the trunk, so it counts what this branch
/// added on top of where it forked — never the trunk's own progress since the
/// fork — and `git diff <commit>` reads the tree on disk, so staged and unstaged
/// work land in the same number as committed work. Untracked files are
/// invisible to `git diff`; [`refresh_local_facts`] folds their line count in
/// from the status probe.
fn worktree_diff_stats(worktree: &Path, base: &str) -> Option<DiffStats> {
    let output = git_output(
        worktree,
        &["diff", "--no-ext-diff", "--numstat", base, "--"],
    )?;
    if !output.status.success() {
        return None;
    }
    Some(parse_numstat(&String::from_utf8_lossy(&output.stdout)))
}

/// The commits the worktree carries ahead of the trunk — `git rev-list --count
/// <base>..HEAD`, the committed work waiting to land. Measured off the same
/// merge-base as the diff, so it counts this branch's own commits since the
/// fork, never the trunk's. The diff's `+/-` also folds in staged/unstaged
/// change; this column is committed work alone.
fn worktree_commits_ahead(worktree: &Path, base: &str) -> Option<u32> {
    rev_list_count(worktree, &format!("{base}..HEAD"))
}

/// The commits the trunk has advanced past the worktree's fork point — `git
/// rev-list --count <base>..<trunk>`, the work a rebase would pick up. The
/// mirror of [`worktree_commits_ahead`], off the same merge-base. This column
/// splits the header's two content-landed markers: zero behind gets `≡`, and
/// any trunk movement past the worktree gets `✓` — safe to remove either way.
fn worktree_commits_behind(worktree: &Path, base: &str, trunk: &str) -> Option<u32> {
    rev_list_count(worktree, &format!("{base}..{trunk}"))
}

/// One worktree's `git status` verdict: whether the working tree is clean — no
/// staged, unstaged, or untracked change, which the header's content-landed
/// `≡`/`✓` markers require — plus the added lines its untracked files carry,
/// folded into the header's `+` churn.
struct WorktreeStatus {
    clean: bool,
    untracked_added: u32,
}

/// `git status --porcelain=v1 -z --untracked-files=all`: emptiness → clean, and
/// each `??` entry's file line-counts into the untracked churn.
/// `--untracked-files=all` lists files inside untracked directories
/// individually so the count sees them; `-z` keeps newline-bearing filenames
/// parseable (paths decode lossily, so a non-UTF-8 path still dirties the tree
/// but counts no lines); and `--no-optional-locks` keeps this background probe
/// from taking `index.lock`, so it never races the user's own git commands in
/// the worktree.
fn worktree_status(worktree: &Path) -> Option<WorktreeStatus> {
    let output = git_output(
        worktree,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
        ],
    )?;
    if !output.status.success() {
        return None;
    }
    let mut budget = UNTRACKED_READ_BUDGET;
    Some(parse_status_entries(
        &String::from_utf8_lossy(&output.stdout),
        |path| untracked_added_lines(&worktree.join(path), &mut budget),
    ))
}

/// Fold a porcelain v1 `-z` status stream: any entry means a dirty tree, and
/// each untracked (`??`) path feeds the line counter. A rename/copy entry —
/// detected in either status column — carries its source path as a second
/// NUL-separated token, consumed and skipped so it never reads as an entry of
/// its own.
fn parse_status_entries(
    output: &str,
    mut untracked_lines: impl FnMut(&str) -> u32,
) -> WorktreeStatus {
    let mut clean = true;
    let mut untracked_added: u32 = 0;
    let mut tokens = output.split('\0').filter(|token| !token.is_empty());
    while let Some(entry) = tokens.next() {
        clean = false;
        let Some((code, path)) = entry.split_at_checked(3) else {
            continue;
        };
        if code.get(..2).is_some_and(|xy| xy.contains(['R', 'C'])) {
            tokens.next();
        }
        if code.starts_with("??") {
            untracked_added = untracked_added.saturating_add(untracked_lines(path));
        }
    }
    WorktreeStatus {
        clean,
        untracked_added,
    }
}

/// Shared byte budget for one status probe's untracked reads. A file past the
/// remaining budget still marks the tree dirty through its status entry; the
/// budget only bounds the churn line count, so one refresh never reads more
/// than this however many untracked files the tree holds.
const UNTRACKED_READ_BUDGET: u64 = 8 * 1024 * 1024;

/// The added lines one untracked file contributes to the `+` churn — what
/// numstat would report if the file were tracked — spending the probe's shared
/// read `budget`. Unreadable, over-budget, and non-file paths contribute
/// nothing; the status entry already marks the tree dirty, so an uncounted
/// file costs accuracy, never the markers.
fn untracked_added_lines(path: &Path, budget: &mut u64) -> u32 {
    let Ok(meta) = std::fs::metadata(path) else {
        return 0;
    };
    if !meta.is_file() || meta.len() > *budget {
        return 0;
    }
    let Ok(bytes) = std::fs::read(path) else {
        return 0;
    };
    *budget = budget.saturating_sub(bytes.len() as u64);
    count_added_lines(&bytes)
}

/// Line count of a blob the way numstat counts an added file: newlines plus a
/// trailing partial line. A NUL in the first 8000 bytes reads as binary (git's
/// own heuristic) and counts nothing, mirroring numstat's `-` cells.
fn count_added_lines(bytes: &[u8]) -> u32 {
    if bytes[..bytes.len().min(8000)].contains(&0) {
        return 0;
    }
    let newlines = bytes.iter().filter(|byte| **byte == b'\n').count();
    let tail = usize::from(bytes.last().is_some_and(|byte| *byte != b'\n'));
    (newlines + tail).min(u32::MAX as usize) as u32
}

/// `git rev-list --count <range>` as a capped `u32` — the shared tail of the
/// ahead/behind columns.
fn rev_list_count(worktree: &Path, range: &str) -> Option<u32> {
    let count = git_line(worktree, &["rev-list", "--count", range])?;
    count
        .parse::<u64>()
        .ok()
        .map(|count| count.min(u64::from(u32::MAX)) as u32)
}

/// The commit a worktree's diff is measured against: the merge-base between its
/// HEAD and the repo's trunk — the fork point a PR diffs against. Returns
/// `None` (so the header simply omits stats) when there is no shared ancestor,
/// e.g. an orphan branch.
fn diff_base(worktree: &Path, trunk: &str) -> Option<String> {
    git_line(worktree, &["merge-base", "HEAD", trunk])
}

/// The repo's trunk branch: the configured `[sidebar] trunk` when it resolves
/// in this repo, else the local `main`/`master` a worktree forks from and
/// merges back into, falling back to the remote's advertised default for a
/// non-standard name. The configured name is a machine-wide *preference* — a
/// repo without that branch falls through to detection rather than losing its
/// stats — and an option-shaped name (leading `-`) is never handed to git.
/// Branch refs are shared across a repo's worktrees, so this resolves from
/// inside any of them.
fn trunk_ref(worktree: &Path, configured: Option<&str>) -> Option<String> {
    let configured = configured.filter(|name| !name.is_empty() && !name.starts_with('-'));
    for name in configured.into_iter().chain(["main", "master"]) {
        if git_line(worktree, &["rev-parse", "--verify", "--quiet", name]).is_some() {
            return Some(name.to_owned());
        }
    }
    git_line(
        worktree,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
}

/// Run `git -C <worktree> <args>` and return its stdout's first non-empty line,
/// or `None` on a missing git binary, a non-zero exit, or empty output.
fn git_line(worktree: &Path, args: &[&str]) -> Option<String> {
    let output = git_output(worktree, args)?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if line.is_empty() { None } else { Some(line) }
}

fn git_output(worktree: &Path, args: &[&str]) -> Option<std::process::Output> {
    crate::proc::testkit::count_spawn();
    Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(args)
        .output()
        .ok()
}

fn parse_numstat(output: &str) -> DiffStats {
    let mut stats = DiffStats::default();
    for line in output.lines() {
        let mut columns = line.split('\t');
        stats.added = stats
            .added
            .saturating_add(parse_numstat_cell(columns.next()));
        stats.removed = stats
            .removed
            .saturating_add(parse_numstat_cell(columns.next()));
    }
    stats
}

fn parse_numstat_cell(cell: Option<&str>) -> u32 {
    cell.and_then(|value| value.parse::<u64>().ok())
        .map(|value| value.min(u64::from(u32::MAX)) as u32)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
