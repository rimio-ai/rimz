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
    max_commit_files: usize,
) -> Result<Vec<CochangeEdge>> {
    let commits = parse_history(&git_history(root, scope, since)?)?;
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
    Ok(edges)
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
    let window_size = total.saturating_mul(window_pct).div_ceil(100).max(1);
    let first = total.saturating_sub(window_size);
    let modules = module_commits
        .into_iter()
        .map(|(module, commits)| {
            let lifetime = commits.len();
            let window = commits.range(first..).count();
            let share = lifetime as f64 / total as f64;
            let noisy = lifetime < noise_lifetime || window < noise_window;
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
}
