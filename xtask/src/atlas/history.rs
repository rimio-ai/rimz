use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::modules::{rust_module_for_path, scope_for_matching};

#[derive(Clone, Debug)]
pub(super) struct Commit {
    pub(super) id: String,
    pub(super) short: String,
    pub(super) time: i64,
    pub(super) subject: String,
    pub(super) body: String,
    changes: Vec<Change>,
}

#[derive(Clone, Debug)]
enum Change {
    Touch(PathBuf),
    Delete(PathBuf),
    Rename { old: PathBuf, new: PathBuf },
}

#[derive(Debug, Default)]
struct Identity {
    path: Option<PathBuf>,
    commits: BTreeSet<usize>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(super) struct PaceMetrics {
    pub(super) commits: usize,
    pub(super) share: f64,
    pub(super) pace: Option<f64>,
    pub(super) noisy: bool,
}

#[derive(Debug)]
pub(super) struct PaceReport {
    pub(super) commits: usize,
    pub(super) modules: BTreeMap<String, PaceMetrics>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct CochangeEdge {
    pub(super) left: String,
    pub(super) right: String,
    pub(super) commits: usize,
}

#[derive(Debug)]
pub(super) struct CochangeReport {
    pub(super) commits: usize,
    pub(super) edges: Vec<CochangeEdge>,
}

#[derive(Debug)]
pub(super) struct Log {
    commits: Vec<Commit>,
}

struct IntroducingCandidate {
    commit: Commit,
    path: PathBuf,
}

impl Log {
    #[cfg(test)]
    pub(super) fn empty() -> Self {
        Self {
            commits: Vec::new(),
        }
    }

    pub(super) fn read(root: &Path, scope: &Path) -> Result<Self> {
        let commits = parse_history(&git_history(root, scope)?)?;
        if commits.is_empty() {
            bail!("no non-merge commits touch `{}`", scope.display());
        }
        Ok(Self { commits })
    }

    pub(super) fn first_time(&self) -> i64 {
        self.commits.first().map_or(0, |commit| commit.time)
    }

    pub(super) fn last_time(&self) -> i64 {
        self.commits.last().map_or(0, |commit| commit.time)
    }
}

pub(super) fn introducing_commits(root: &Path, file: &Path, name: &str) -> Result<Vec<Commit>> {
    let output = Command::new("git")
        .args([
            "log",
            "--follow",
            "--name-only",
            "--format=%x1e%H%x00%h%x00%ct%x00%s%x00%b%x00",
        ])
        .arg(format!("-S{name}"))
        .arg("--")
        .arg(file)
        .current_dir(root)
        .output()
        .with_context(|| format!("finding commits that introduced `{name}`"))?;
    if !output.status.success() {
        bail!(
            "git log failed for {}: {}",
            file.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let raw = String::from_utf8(output.stdout).context("git log returned non-UTF-8 text")?;
    let mut commits = parse_introducing_candidates(&raw)?
        .into_iter()
        .filter_map(|candidate| {
            let output = Command::new("git")
                .args(["show", "--format="])
                .arg(&candidate.commit.id)
                .arg("--")
                .arg(&candidate.path)
                .current_dir(root)
                .output();
            output
                .is_ok_and(|output| {
                    output.status.success()
                        && String::from_utf8_lossy(&output.stdout)
                            .lines()
                            .any(|line| declares_added_name(line, name))
                })
                .then_some(candidate.commit)
        })
        .collect::<Vec<_>>();
    commits.reverse();
    Ok(commits)
}

pub(super) fn fix_markers(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| {
            line.split(|character: char| !character.is_alphanumeric() && character != '_')
                .any(|word| {
                    matches!(
                        word.to_ascii_lowercase().as_str(),
                        "fix" | "bug" | "incident" | "regression"
                    )
                })
                || line
                    .as_bytes()
                    .windows(2)
                    .any(|pair| pair[0] == b'#' && pair[1].is_ascii_digit())
        })
        .map(str::trim)
        .map(str::to_owned)
        .collect()
}

fn parse_introducing_candidates(raw: &str) -> Result<Vec<IntroducingCandidate>> {
    raw.split('\x1e')
        .filter_map(|record| {
            let record = record.trim_matches('\n');
            (!record.is_empty()).then_some(record)
        })
        .map(|record| {
            let fields = record.splitn(6, '\0').collect::<Vec<_>>();
            if fields.len() != 6 {
                bail!("malformed git log record for introducing commits");
            }
            let paths = fields[5]
                .lines()
                .map(str::trim)
                .filter(|path| !path.is_empty())
                .collect::<Vec<_>>();
            let [path] = paths.as_slice() else {
                bail!("malformed historical path for introducing commit");
            };
            Ok(IntroducingCandidate {
                commit: Commit {
                    id: fields[0].to_owned(),
                    short: fields[1].to_owned(),
                    time: fields[2]
                        .parse()
                        .with_context(|| format!("invalid git commit time `{}`", fields[2]))?,
                    subject: fields[3].to_owned(),
                    body: fields[4].trim_end().to_owned(),
                    changes: Vec::new(),
                },
                path: PathBuf::from(path),
            })
        })
        .collect()
}

fn declares_added_name(line: &str, name: &str) -> bool {
    if !line.starts_with('+') || line.starts_with("+++") {
        return false;
    }
    let tokens = line[1..]
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    tokens.windows(2).any(|pair| {
        matches!(
            pair[0],
            "fn" | "struct"
                | "enum"
                | "union"
                | "type"
                | "trait"
                | "const"
                | "static"
                | "mod"
                | "macro_rules"
        ) && pair[1] == name
    })
}

pub(super) fn pace(
    log: &Log,
    root: &Path,
    scope: &Path,
    window_pct: usize,
    noise_lifetime: usize,
    noise_window: usize,
) -> Result<PaceReport> {
    Ok(fold_pace(
        root,
        scope,
        &log.commits,
        window_pct,
        noise_lifetime,
        noise_window,
    ))
}

pub(super) fn cochange(
    log: &Log,
    root: &Path,
    scope: &Path,
    since: Option<&str>,
    window_pct: usize,
    max_commit_files: usize,
) -> Result<CochangeReport> {
    let selected;
    let commits = if let Some(reference) = since {
        let ids = commits_since(root, reference)?;
        selected = log
            .commits
            .iter()
            .filter(|commit| ids.contains(&commit.id))
            .cloned()
            .collect::<Vec<_>>();
        selected.as_slice()
    } else {
        recent_window(&log.commits, window_pct)
    };
    Ok(CochangeReport {
        commits: commits.len(),
        edges: fold_cochange(commits, scope, max_commit_files),
    })
}

fn recent_window(commits: &[Commit], window_pct: usize) -> &[Commit] {
    let first = commits
        .len()
        .saturating_sub(window_size(commits.len(), window_pct));
    &commits[first..]
}

fn fold_cochange(commits: &[Commit], scope: &Path, max_commit_files: usize) -> Vec<CochangeEdge> {
    let mut counts = BTreeMap::<(String, String), usize>::new();
    for commit in commits {
        let files = commit
            .changes
            .iter()
            .map(|change| match change {
                Change::Touch(path) | Change::Delete(path) => path,
                Change::Rename { new, .. } => new,
            })
            .filter(|path| path.starts_with(scope_for_matching(scope)))
            .filter(|path| rust_module_for_path(path, scope).is_some())
            .collect::<BTreeSet<_>>();
        if files.len() > max_commit_files {
            continue;
        }
        let modules = files
            .into_iter()
            .filter_map(|path| rust_module_for_path(path, scope))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        for (index, left) in modules.iter().enumerate() {
            for right in &modules[index + 1..] {
                *counts.entry((left.clone(), right.clone())).or_default() += 1;
            }
        }
    }
    let mut edges = counts
        .into_iter()
        .map(|((left, right), commits)| CochangeEdge {
            left,
            right,
            commits,
        })
        .collect::<Vec<_>>();
    edges.sort_by(|left, right| {
        right
            .commits
            .cmp(&left.commits)
            .then_with(|| left.left.cmp(&right.left))
            .then_with(|| left.right.cmp(&right.right))
    });
    edges
}

fn git_history(root: &Path, scope: &Path) -> Result<String> {
    let mut command = Command::new("git");
    command.args([
        "-c",
        "core.quotePath=false",
        "log",
        "--reverse",
        "--topo-order",
        "--no-merges",
        "--format=@%H %ct",
        "--name-status",
        "-M",
    ]);
    let output = command
        .arg("--")
        .arg(scope)
        .current_dir(root)
        .output()
        .context("running git log for atlas")?;
    if !output.status.success() {
        bail!(
            "git log failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout).context("git log returned non-UTF-8 paths")
}

fn commits_since(root: &Path, reference: &str) -> Result<BTreeSet<String>> {
    let output = Command::new("git")
        .args(["rev-list", &format!("{reference}..HEAD")])
        .current_dir(root)
        .output()
        .with_context(|| format!("listing commits since `{reference}`"))?;
    if !output.status.success() {
        bail!(
            "git rev-list `{reference}..HEAD` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)
        .context("git rev-list returned non-UTF-8 commit ids")?
        .lines()
        .map(str::to_owned)
        .collect())
}

fn parse_history(input: &str) -> Result<Vec<Commit>> {
    let mut commits = Vec::<Commit>::new();
    for line in input.lines().filter(|line| !line.is_empty()) {
        if let Some(header) = line.strip_prefix('@').filter(|line| !line.contains('\t')) {
            let (id, time) = header
                .split_once(' ')
                .map_or((header, "0"), |(id, time)| (id, time));
            commits.push(Commit {
                id: id.to_owned(),
                short: id.chars().take(7).collect(),
                time: time
                    .parse()
                    .with_context(|| format!("invalid git commit time `{time}`"))?,
                subject: String::new(),
                body: String::new(),
                changes: Vec::new(),
            });
            continue;
        }
        let commit = commits
            .last_mut()
            .context("git history contained a change before its commit header")?;
        let fields = line.split('\t').collect::<Vec<_>>();
        let status = fields.first().copied().unwrap_or_default();
        let change = match status.as_bytes().first() {
            Some(b'R') if fields.len() == 3 => Change::Rename {
                old: PathBuf::from(fields[1]),
                new: PathBuf::from(fields[2]),
            },
            Some(b'R') => bail!("malformed git rename record `{line}`"),
            Some(b'D') => Change::Delete(PathBuf::from(
                fields
                    .get(1)
                    .with_context(|| format!("malformed git delete record `{line}`"))?,
            )),
            Some(_) => Change::Touch(PathBuf::from(
                fields
                    .last()
                    .filter(|_| fields.len() >= 2)
                    .with_context(|| format!("malformed git change record `{line}`"))?,
            )),
            None => bail!("malformed git status record `{line}`"),
        };
        commit.changes.push(change);
    }
    Ok(commits)
}

fn fold_pace(
    root: &Path,
    scope: &Path,
    commits: &[Commit],
    window_pct: usize,
    noise_lifetime: usize,
    noise_window: usize,
) -> PaceReport {
    let mut paths = HashMap::<PathBuf, usize>::new();
    let mut identities = Vec::<Identity>::new();
    for (commit_index, commit) in commits.iter().enumerate() {
        for change in &commit.changes {
            match change {
                Change::Touch(path) => {
                    let identity = identity_for_path(path, &mut paths, &mut identities);
                    identities[identity].commits.insert(commit_index);
                }
                Change::Delete(path) => {
                    let identity = identity_for_path(path, &mut paths, &mut identities);
                    identities[identity].commits.insert(commit_index);
                    paths.remove(path);
                    identities[identity].path = None;
                }
                Change::Rename { old, new } => {
                    let identity = paths
                        .remove(old)
                        .unwrap_or_else(|| new_identity(old, &mut identities));
                    if let Some(replaced) = paths.remove(new) {
                        identities[replaced].path = None;
                    }
                    paths.insert(new.clone(), identity);
                    identities[identity].path = Some(new.clone());
                    identities[identity].commits.insert(commit_index);
                }
            }
        }
    }

    let mut module_commits = BTreeMap::<String, BTreeSet<usize>>::new();
    for identity in identities {
        let Some(path) = identity.path.filter(|path| {
            path.starts_with(scope_for_matching(scope)) && root.join(path).is_file()
        }) else {
            continue;
        };
        let Some(module) = rust_module_for_path(&path, scope) else {
            continue;
        };
        module_commits
            .entry(module)
            .or_default()
            .extend(identity.commits);
    }
    let total = commits.len();
    let window_size = window_size(total, window_pct);
    let first = total.saturating_sub(window_size);
    let modules = module_commits
        .into_iter()
        .map(|(module, commits)| {
            let lifetime = commits.len();
            let window = commits.range(first..).count();
            let share = lifetime as f64 / total as f64;
            let noisy = pace_is_noisy(lifetime, window, noise_lifetime, noise_window);
            let pace =
                (!noisy && share > 0.0).then_some((window as f64 / window_size as f64) / share);
            (
                module,
                PaceMetrics {
                    commits: lifetime,
                    share,
                    pace,
                    noisy,
                },
            )
        })
        .collect();
    PaceReport {
        commits: total,
        modules,
    }
}

fn window_size(commits: usize, pct: usize) -> usize {
    commits.saturating_mul(pct).div_ceil(100).max(1)
}

fn pace_is_noisy(
    lifetime_commits: usize,
    window_commits: usize,
    noise_lifetime: usize,
    noise_window: usize,
) -> bool {
    lifetime_commits < noise_lifetime || window_commits < noise_window
}

fn identity_for_path(
    path: &Path,
    paths: &mut HashMap<PathBuf, usize>,
    identities: &mut Vec<Identity>,
) -> usize {
    if let Some(identity) = paths.get(path) {
        return *identity;
    }
    let identity = new_identity(path, identities);
    paths.insert(path.to_path_buf(), identity);
    identity
}

fn new_identity(path: &Path, identities: &mut Vec<Identity>) -> usize {
    let identity = identities.len();
    identities.push(Identity {
        path: Some(path.to_path_buf()),
        commits: BTreeSet::new(),
    });
    identity
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn introducing_commits_follow_a_renamed_file() {
        let root = tempfile::tempdir().unwrap();
        run_git(root.path(), &["init", "-q"]);
        run_git(root.path(), &["config", "user.email", "atlas@example.com"]);
        run_git(root.path(), &["config", "user.name", "Atlas Test"]);
        fs::create_dir(root.path().join("src")).unwrap();
        fs::write(root.path().join("src/old.rs"), "pub fn target() {}\n").unwrap();
        run_git(root.path(), &["add", "."]);
        run_git(root.path(), &["commit", "-qm", "introduce target"]);
        fs::rename(
            root.path().join("src/old.rs"),
            root.path().join("src/new.rs"),
        )
        .unwrap();
        run_git(root.path(), &["add", "-A"]);
        run_git(root.path(), &["commit", "-qm", "rename source"]);

        let commits = introducing_commits(root.path(), Path::new("src/new.rs"), "target").unwrap();

        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "introduce target");
    }

    #[test]
    fn cochange_history_parser_preserves_renames() {
        let commits =
            parse_history("@a\nM\tcli/a.rs\nM\tcli/b.rs\n@b\nR100\tcli/a.rs\tcli/c.rs\n").unwrap();
        assert_eq!(commits.len(), 2);
        assert!(matches!(
            commits[1].changes[0],
            Change::Rename { ref old, ref new }
                if old == Path::new("cli/a.rs") && new == Path::new("cli/c.rs")
        ));
    }

    #[test]
    fn rename_folding_attributes_old_commits_to_the_head_module() {
        let root = tempfile::tempdir().unwrap();
        let current = root.path().join("src/new/current.rs");
        fs::create_dir_all(current.parent().unwrap()).unwrap();
        fs::write(&current, "fn current() {}\n").unwrap();
        let commits = parse_history(
            "@a\nA\tsrc/old/current.rs\n\
             @b\nM\tsrc/old/current.rs\n\
             @c\nR100\tsrc/old/current.rs\tsrc/new/current.rs\n\
             @d\nM\tsrc/new/current.rs\n\
             @e\nA\tsrc/dead.rs\n\
             @f\nD\tsrc/dead.rs\n",
        )
        .unwrap();

        let report = fold_pace(root.path(), Path::new("src"), &commits, 25, 1, 1);
        assert_eq!(report.commits, 6);
        assert_eq!(report.modules.len(), 1);
        assert_eq!(report.modules["new"].commits, 4);
    }

    #[test]
    fn windows_round_up_and_noise_checks_both_populations() {
        assert_eq!(window_size(1, 10), 1);
        assert_eq!(window_size(10, 10), 1);
        assert_eq!(window_size(11, 10), 2);
        assert_eq!(window_size(11, 25), 3);
        assert!(pace_is_noisy(19, 10, 20, 5));
        assert!(pace_is_noisy(20, 4, 20, 5));
        assert!(!pace_is_noisy(20, 5, 20, 5));
    }

    #[test]
    fn cochange_fold_counts_module_pairs_and_omits_broad_commits() {
        let commits = parse_history(
            "@a\nM\tsrc/a/one.rs\nM\tsrc/b/one.rs\n\
             @b\nM\tsrc/a/two.rs\nM\tsrc/b/two.rs\n\
             @c\nM\tsrc/a/three.rs\nM\tsrc/b/three.rs\nM\tsrc/c/three.rs\n",
        )
        .unwrap();
        let edges = fold_cochange(&commits, Path::new("src"), 2);
        assert_eq!(edges.len(), 1);
        assert_eq!(
            (&edges[0].left, &edges[0].right, edges[0].commits),
            (&"a".to_owned(), &"b".to_owned(), 2)
        );
    }

    #[test]
    fn cochange_ignores_non_rust_test_data() {
        let commits = parse_history(
            "@a\nM\tsrc/a.rs\nM\tsrc/snapshots/a.snap\n\
             @b\nM\tsrc/a.rs\nM\tsrc/b.rs\nM\tsrc/snapshots/b.snap\n",
        )
        .unwrap();

        let edges = fold_cochange(&commits, Path::new("src"), 2);

        assert_eq!(edges.len(), 1);
        assert_eq!(
            (&edges[0].left, &edges[0].right),
            (&"a".into(), &"b".into())
        );
    }

    #[test]
    fn cochange_window_keeps_the_most_recent_commits() {
        let commits = parse_history(
            "@a\nM\tsrc/a/one.rs\nM\tsrc/b/one.rs\n\
             @b\nM\tsrc/a/two.rs\nM\tsrc/b/two.rs\n\
             @c\nM\tsrc/a/three.rs\nM\tsrc/b/three.rs\n\
             @d\nM\tsrc/a/four.rs\nM\tsrc/c/four.rs\n",
        )
        .unwrap();

        let recent = recent_window(&commits, 25);
        let edges = fold_cochange(recent, Path::new("src"), 10);

        assert_eq!(recent.len(), 1);
        assert_eq!(edges.len(), 1);
        assert_eq!(
            (&edges[0].left, &edges[0].right),
            (&"a".into(), &"c".into())
        );
    }

    fn run_git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
