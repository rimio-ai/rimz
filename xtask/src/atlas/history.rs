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
    pub(super) modules: BTreeMap<String, PaceMetrics>,
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

    pub(super) fn len(&self) -> usize {
        self.commits.len()
    }

    pub(super) fn window_len(&self, pct: usize) -> usize {
        window_size(self.commits.len(), pct)
    }

    /// Lifetime commit share for each current file, with pre-rename commits
    /// folded into the file's present path.
    pub(super) fn file_shares(&self, root: &Path, scope: &Path) -> BTreeMap<PathBuf, f64> {
        let total = self.commits.len();
        if total == 0 {
            return BTreeMap::new();
        }
        current_file_commits(root, scope, &self.commits)
            .into_iter()
            .map(|(path, commits)| (path, commits.len() as f64 / total as f64))
            .collect()
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

/// One commit as `git blame` attributes it to a line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct BlameCommit {
    pub(super) short: String,
    pub(super) time: i64,
    pub(super) summary: String,
}

/// The blaming commit of every line in `file`, indexed by 1-based line;
/// one `git blame` per file answers "untouched since introduction" for every
/// item in it without a per-item history walk.
pub(super) fn blame_lines(root: &Path, file: &Path) -> Result<Vec<BlameCommit>> {
    let output = Command::new("git")
        .args(["blame", "--line-porcelain", "--"])
        .arg(file)
        .current_dir(root)
        .output()
        .with_context(|| format!("blaming {}", file.display()))?;
    if !output.status.success() {
        bail!(
            "git blame failed for {}: {}",
            file.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let raw = String::from_utf8(output.stdout).context("git blame returned non-UTF-8 text")?;
    parse_blame(&raw)
}

/// Parses `git blame --line-porcelain`: a `<sha> <orig> <final>` header per
/// line, metadata only on a commit's first appearance, then the tab-led
/// content line.
fn parse_blame(raw: &str) -> Result<Vec<BlameCommit>> {
    let mut lines = Vec::new();
    let mut commits = HashMap::<String, BlameCommit>::new();
    let mut current: Option<String> = None;
    for line in raw.lines() {
        if line.starts_with('\t') {
            let id = current
                .take()
                .context("git blame emitted content before its commit header")?;
            lines.push(commits[&id].clone());
            continue;
        }
        if let Some(id) = &current {
            let commit = commits
                .get_mut(id)
                .expect("the header that set `current` inserted its commit");
            if let Some(time) = line.strip_prefix("committer-time ") {
                commit.time = time
                    .trim()
                    .parse()
                    .with_context(|| format!("invalid git blame committer time `{time}`"))?;
            } else if let Some(summary) = line.strip_prefix("summary ") {
                commit.summary = summary.to_owned();
            }
            continue;
        }
        let Some((id, _)) = line.split_once(' ') else {
            continue;
        };
        if id.len() == 40 && id.chars().all(|character| character.is_ascii_hexdigit()) {
            commits.entry(id.to_owned()).or_insert_with(|| BlameCommit {
                short: id.chars().take(7).collect(),
                time: 0,
                summary: String::new(),
            });
            current = Some(id.to_owned());
        }
    }
    Ok(lines)
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
    let file_commits = current_file_commits(root, scope, commits);
    let mut module_commits = BTreeMap::<String, BTreeSet<usize>>::new();
    for (path, commits) in file_commits {
        let Some(module) = rust_module_for_path(&path, scope) else {
            continue;
        };
        module_commits.entry(module).or_default().extend(commits);
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
    PaceReport { modules }
}

fn current_file_commits(
    root: &Path,
    scope: &Path,
    commits: &[Commit],
) -> BTreeMap<PathBuf, BTreeSet<usize>> {
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

    identities
        .into_iter()
        .filter_map(|identity| {
            let path = identity.path.filter(|path| {
                path.starts_with(scope_for_matching(scope)) && root.join(path).is_file()
            })?;
            Some((path, identity.commits))
        })
        .collect()
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

    #[test]
    fn blame_porcelain_attributes_every_line_and_reuses_commit_metadata() {
        let raw = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 1 1 2\nauthor A\ncommitter-time 100\nsummary feat: add store\nfilename src/store.rs\n\tpub fn open() {}\naaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 2 2\n\tpub fn close() {}\nbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb 3 3 1\nauthor B\ncommitter-time 200\nsummary fix: close twice\nfilename src/store.rs\n\tpub fn reset() {}\n";

        let lines = parse_blame(raw).unwrap();

        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], lines[1]);
        assert_eq!(lines[0].short, "aaaaaaa");
        assert_eq!(lines[0].time, 100);
        assert_eq!(lines[0].summary, "feat: add store");
        assert_eq!(lines[2].short, "bbbbbbb");
        assert_eq!(lines[2].summary, "fix: close twice");
    }
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
    fn history_parser_preserves_renames() {
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
        assert_eq!(report.modules.len(), 1);
        assert_eq!(report.modules["new"].commits, 4);
        let shares = Log { commits }.file_shares(root.path(), Path::new("src"));
        assert_eq!(shares[Path::new("src/new/current.rs")], 4.0 / 6.0);
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
