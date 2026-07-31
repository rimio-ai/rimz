use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::modules::{module_for_path, scope_for_matching};

#[derive(Debug)]
struct Commit {
    changes: Vec<Change>,
}

#[derive(Debug)]
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

pub(super) fn pace(
    root: &Path,
    scope: &Path,
    window_pct: usize,
    noise_lifetime: usize,
    noise_window: usize,
) -> Result<PaceReport> {
    let commits = parse_history(&git_history(root, scope, None)?)?;
    if commits.is_empty() {
        bail!("no non-merge commits touch `{}`", scope.display());
    }
    Ok(fold_pace(
        root,
        scope,
        &commits,
        window_pct,
        noise_lifetime,
        noise_window,
    ))
}

pub(super) fn cochange(
    root: &Path,
    scope: &Path,
    since: Option<&str>,
    window_pct: usize,
    max_commit_files: usize,
) -> Result<CochangeReport> {
    let commits = parse_history(&git_history(root, scope, since)?)?;
    let commits = if since.is_some() {
        &commits
    } else {
        recent_window(&commits, window_pct)
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
            .collect::<BTreeSet<_>>();
        if files.len() > max_commit_files {
            continue;
        }
        let modules = files
            .into_iter()
            .filter(|path| path.starts_with(scope_for_matching(scope)))
            .map(|path| module_for_path(path, scope))
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

fn git_history(root: &Path, scope: &Path, since: Option<&str>) -> Result<String> {
    let mut command = Command::new("git");
    command.args([
        "-c",
        "core.quotePath=false",
        "log",
        "--reverse",
        "--topo-order",
        "--no-merges",
        "--format=@%H",
        "--name-status",
        "-M",
    ]);
    if let Some(reference) = since {
        command.arg(format!("{reference}..HEAD"));
    }
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

fn parse_history(input: &str) -> Result<Vec<Commit>> {
    let mut commits = Vec::<Commit>::new();
    for line in input.lines().filter(|line| !line.is_empty()) {
        if line.starts_with('@') && !line.contains('\t') {
            commits.push(Commit {
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
        module_commits
            .entry(module_for_path(&path, scope))
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
}
