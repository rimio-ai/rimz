//! PR-based worktree checkout.
//!
//! Strategy selection keeps review-only, same-repository, and fork checkout paths separate. Forge CLI head resolution and fork remote configuration stay here with the PR-specific git plumbing.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::config::{WorktreeConfig, WorktreeHooks};
use crate::forge;

use super::{
    Checkout, CreatedWorktree, FreshWorktree, MarkerProvenance, PushDestination, Result,
    WorktreeCreateTarget, WorktreeErr, add_worktree, ensure_repo, git_network_output, git_run,
    git_stdout, is_ancestor, parse_worktree_list, read_marker_for_worktree, resolve_base_commit,
    resolve_branch, resolve_fresh_worktree, trunk_ref, write_marker,
};

const PR_HEAD_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
// PR refs are incremental fetches into an existing clone, but still need enough
// room for ordinary remote latency and repository negotiation.
const PR_FETCH_TIMEOUT: Duration = Duration::from_secs(120);
static TEMP_REF_NONCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrBranchChoice {
    Local,
    Remote,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrBranchDivergence {
    Behind {
        behind: u32,
    },
    Rebased {
        ahead: u32,
        behind: u32,
    },
    Diverged {
        ahead: u32,
        behind: u32,
        conflicts: bool,
    },
}

fn classify_divergence(repo: &Path, local: &str, remote: &str) -> Result<PrBranchDivergence> {
    let counts = git_stdout(
        repo,
        [
            "rev-list",
            "--left-right",
            "--count",
            &format!("{local}...{remote}"),
        ],
    )?;
    let mut counts = counts.split_whitespace();
    let mut count = || {
        counts
            .next()
            .and_then(|value| value.parse::<u32>().ok())
            .ok_or_else(|| WorktreeErr::Parse("invalid ahead/behind counts".into()))
    };
    let ahead = count()?;
    let behind = count()?;
    if is_ancestor(repo, local, remote) {
        return Ok(PrBranchDivergence::Behind { behind });
    }
    let patches = git_stdout(
        repo,
        [
            "log",
            "--cherry-pick",
            "--right-only",
            "--no-merges",
            &format!("{remote}...{local}"),
        ],
    )?;
    if patches.is_empty() {
        return Ok(PrBranchDivergence::Rebased { ahead, behind });
    }
    let conflicts = git_stdout(repo, ["merge-tree", "--write-tree", local, remote]).is_err();
    Ok(PrBranchDivergence::Diverged {
        ahead,
        behind,
        conflicts,
    })
}

impl std::fmt::Display for PrBranchDivergence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Behind { behind } => write!(f, "{behind} commits behind the PR head"),
            Self::Rebased { ahead, behind } => write!(
                f,
                "the PR head was rebased: {ahead} local commits are all contained in it ({behind} PR commits are not local)"
            ),
            Self::Diverged {
                ahead,
                behind,
                conflicts,
            } => write!(
                f,
                "{ahead} local commits are not in the PR head ({behind} PR commits are not local); they {}",
                if *conflicts {
                    "conflict"
                } else {
                    "merge cleanly"
                }
            ),
        }
    }
}

pub(super) fn alignment_commands(branch: &str, holder: Option<&Path>) -> String {
    let quote = |value: &str| {
        shlex::Quoter::new()
            .allow_nul(true)
            .quote(value)
            .expect("allow_nul disables the quoter's only error")
            .into_owned()
    };
    let remote = quote(&format!("origin/{branch}"));
    let branch = quote(branch);
    let take_remote = match holder {
        Some(path) => format!(
            "git -C {} reset --keep {remote}",
            quote(&path.to_string_lossy())
        ),
        None => format!("git branch -f {branch} {remote}"),
    };
    let changes = if holder.is_some() {
        " (commit or stash changes there first)"
    } else {
        ""
    };
    format!(
        "take the PR head with `{take_remote}`{changes}, or push the local tip with `git push origin {branch}`; then rerun, or rerun in a terminal to choose"
    )
}

struct PrContext<'a> {
    hooks: &'a WorktreeHooks,
    number: u64,
    remote: String,
    remote_repo: Option<forge::RemoteRepo>,
    refspec: String,
}

pub fn create_from_pr(
    repo_root: &Path,
    config: &WorktreeConfig,
    pr: &forge::PrTarget,
    name: Option<&str>,
    branch: Option<&str>,
    reuse_existing: bool,
    choice: Option<PrBranchChoice>,
) -> Result<CreatedWorktree> {
    config.hooks.validate()?;
    ensure_repo(repo_root)?;
    let default_name = format!("pr-{}", pr.number);
    let target = resolve_fresh_worktree(
        repo_root,
        config,
        name,
        Some(default_name.as_str()),
        reuse_existing,
    );
    let target = match target {
        Ok(WorktreeCreateTarget::Reuse(reused)) if reused.marker.from_pr.is_some() => {
            if reused.marker.from_pr != Some(pr.number) {
                return Err(WorktreeErr::PrWorktreeMismatch {
                    name: reused.marker.name,
                    existing: reused.marker.from_pr,
                    requested: pr.number,
                });
            }
            let remote = origin_remote(repo_root, pr.number)?;
            let remote_repo = forge::RemoteRepo::parse(&remote);
            validate_pr_origin(pr, &remote, remote_repo.as_ref())?;
            return Ok(*reused);
        }
        target => target,
    };
    let requested = name.map(super::parse_requested_name).transpose()?;
    let review_branch = branch.or(requested.as_ref().and_then(|name| name.branch.as_deref()));

    let remote = origin_remote(repo_root, pr.number)?;
    let remote_repo = forge::RemoteRepo::parse(&remote);
    validate_pr_origin(pr, &remote, remote_repo.as_ref())?;
    let remote_forge = pr.forge.unwrap_or_else(|| {
        remote_repo
            .as_ref()
            .map_or(forge::Forge::GitHubStyle, forge::RemoteRepo::forge)
    });
    let context = PrContext {
        hooks: &config.hooks,
        number: pr.number,
        refspec: remote_forge.pr_refspec(pr.number),
        remote,
        remote_repo,
    };

    if let Some(branch) = review_branch {
        let fresh = require_fresh(target)?;
        let branch = resolve_branch(Some(branch), None, &fresh.name)?;
        return review_only_checkout(repo_root, fresh, &context, branch, None);
    }

    let Some((remote_repo, cli)) = context
        .remote_repo
        .as_ref()
        .and_then(|remote| remote.forge_cli().map(|cli| (remote, cli)))
    else {
        let fresh = require_fresh(target)?;
        let branch = resolve_branch(None, fresh.branch.as_deref(), &fresh.name)?;
        return review_only_checkout(
            repo_root,
            fresh,
            &context,
            branch,
            Some("origin has no supported forge CLI".to_owned()),
        );
    };
    let program = cli.program();
    if which::which(program).is_err() {
        let fresh = require_fresh(target)?;
        let branch = resolve_branch(None, fresh.branch.as_deref(), &fresh.name)?;
        return review_only_checkout(
            repo_root,
            fresh,
            &context,
            branch,
            Some(format!("`{program}` is not installed")),
        );
    }
    let head = resolve_pr_head_with_cli(repo_root, context.number, cli, remote_repo)?;
    let same_repo = match head.is_cross_repository {
        Some(cross_repository) => !cross_repository,
        None => head
            .repo_full_name
            .as_deref()
            .zip(remote_repo.repo_slug())
            .map(|(head_repo, origin_repo)| head_repo.eq_ignore_ascii_case(origin_repo))
            .ok_or_else(|| WorktreeErr::PrHeadUnresolved {
                number: context.number,
                reason: "forge CLI output did not identify the head repository".to_owned(),
            })?,
    };

    if same_repo {
        same_repo_checkout(
            repo_root,
            target,
            &context,
            head.branch,
            requested.as_ref().map(|name| name.name.as_str()),
            choice,
        )
    } else {
        fork_checkout(repo_root, require_fresh(target)?, &context, head)
    }
}

fn require_fresh(target: Result<WorktreeCreateTarget>) -> Result<FreshWorktree> {
    match target? {
        WorktreeCreateTarget::Fresh(fresh) => Ok(fresh),
        WorktreeCreateTarget::Reuse(reused) => Err(WorktreeErr::Exists {
            name: reused.marker.name,
            path: reused.marker.worktree_path,
        }),
    }
}

fn origin_remote(repo_root: &Path, number: u64) -> Result<String> {
    git_stdout(repo_root, ["config", "--get", "remote.origin.url"]).map_err(|err| match err {
        WorktreeErr::Git { .. } => WorktreeErr::Parse(format!(
            "could not fetch PR #{}: git remote `origin` is not configured",
            number
        )),
        other => other,
    })
}

fn validate_pr_origin(
    pr: &forge::PrTarget,
    remote: &str,
    remote_repo: Option<&forge::RemoteRepo>,
) -> Result<()> {
    if pr.host.is_some() && !remote_repo.is_some_and(|remote| remote.matches_target(pr)) {
        return Err(WorktreeErr::PrRepoMismatch {
            url_repo: pr.repo.clone().unwrap_or_default(),
            origin_repo: remote_repo
                .and_then(forge::RemoteRepo::repo_slug)
                .unwrap_or(remote)
                .to_owned(),
        });
    }
    Ok(())
}

fn review_only_checkout(
    repo_root: &Path,
    fresh: FreshWorktree,
    context: &PrContext<'_>,
    branch: String,
    review_only_reason: Option<String>,
) -> Result<CreatedWorktree> {
    let pr_head = fetch_pr_head(repo_root, context.number, &context.remote, &context.refspec)?;
    let mut created = add_pr_worktree(
        repo_root,
        fresh,
        branch,
        pr_head.oid.clone(),
        pr_head.oid.as_str(),
        context.number,
        context.hooks,
    )?;
    created.review_only_reason = review_only_reason;
    Ok(created)
}

fn same_repo_checkout(
    repo_root: &Path,
    target: Result<WorktreeCreateTarget>,
    context: &PrContext<'_>,
    branch: String,
    requested_name: Option<&str>,
    choice: Option<PrBranchChoice>,
) -> Result<CreatedWorktree> {
    validate_pr_branch(repo_root, context.number, &branch)?;
    let remote_ref = format!("origin/{branch}");
    let fetch_refspec = format!("+refs/heads/{branch}:refs/remotes/{remote_ref}");
    git_network_output(
        repo_root,
        [
            "fetch",
            "--no-tags",
            "--no-recurse-submodules",
            "origin",
            fetch_refspec.as_str(),
        ],
        PR_FETCH_TIMEOUT,
    )
    .map_err(pr_fetch_err(context.number, &context.remote))?;
    let remote_head = git_stdout(repo_root, ["rev-parse", remote_ref.as_str()])?;
    let holder = branch_worktree(repo_root, &branch)?;
    let marker = match holder.as_ref() {
        Some((path, main)) => {
            if *main {
                return Err(WorktreeErr::PrBranchConflict {
                    branch,
                    detail: format!(
                        "it is checked out in the main checkout at {}; switch that checkout to another branch",
                        path.display()
                    ),
                });
            }
            let marker = read_marker_for_worktree(path)?.ok_or_else(|| WorktreeErr::PrBranchConflict { branch: branch.clone(), detail: format!("it is checked out at {}, which RimZ does not manage; free the branch there or remove that checkout", path.display()) })?;
            if marker
                .from_pr
                .is_some_and(|number| number != context.number)
            {
                return Err(WorktreeErr::PrWorktreeMismatch {
                    name: marker.name,
                    existing: marker.from_pr,
                    requested: context.number,
                });
            }
            if requested_name.is_some_and(|name| name != marker.name) {
                return Err(WorktreeErr::PrBranchConflict {
                    branch,
                    detail: format!(
                        "it is checked out at {}; omit -w, or pass -w {}",
                        path.display(),
                        marker.name
                    ),
                });
            }
            Some(marker)
        }
        None => None,
    };
    // Validate the destination before a choice can move a branch.
    let fresh = if holder.is_none() {
        Some(require_fresh(target)?)
    } else {
        None
    };
    let provenance = pr_marker_provenance(repo_root, &remote_head, context.number);
    let checkout = match prepare_local_pr_branch(
        repo_root,
        &branch,
        &remote_ref,
        &remote_head,
        holder.as_ref().map(|(path, _)| path.as_path()),
        choice,
    )? {
        LocalPrBranch::New => Checkout::Tracking(&remote_ref),
        LocalPrBranch::Existing => Checkout::Existing,
    };
    if let Some(mut marker) = marker {
        if marker.from_pr.is_none() {
            marker.from_pr = Some(context.number);
            write_marker(&marker.worktree_path, &marker)?;
        }
        return Ok(CreatedWorktree {
            stale_base: None,
            marker,
            reused: true,
            included: 0,
            linked: 0,
            push_destination: None,
            review_only_reason: None,
        });
    }
    let fresh = fresh.expect("a checkout without a holder has a validated fresh destination");
    add_worktree(
        repo_root,
        fresh.name,
        fresh.path,
        branch,
        provenance,
        checkout,
        context.hooks,
    )
}

fn fork_checkout(
    repo_root: &Path,
    fresh: FreshWorktree,
    context: &PrContext<'_>,
    head: forge::PrHead,
) -> Result<CreatedWorktree> {
    validate_pr_branch(repo_root, context.number, &head.branch)?;
    let owner = head
        .owner
        .filter(|owner| !owner.trim().is_empty())
        .ok_or_else(|| WorktreeErr::PrHeadUnresolved {
            number: context.number,
            reason: "forge CLI output did not identify the head repository owner".to_owned(),
        })?;
    let repo_full_name = head
        .repo_full_name
        .ok_or_else(|| WorktreeErr::PrHeadUnresolved {
            number: context.number,
            reason: "forge CLI output did not identify the head repository".to_owned(),
        })?;
    let fork_url = context
        .remote_repo
        .as_ref()
        .and_then(|remote| remote.sibling_url(&repo_full_name))
        .ok_or_else(|| WorktreeErr::PrHeadUnresolved {
            number: context.number,
            reason: "could not build the fork clone URL from origin".to_owned(),
        })?;
    let pr_head = fetch_pr_head(repo_root, context.number, &context.remote, &context.refspec)?;
    let head_branch = head.branch;
    let branch = if local_branch_tip(repo_root, &head_branch).is_none() {
        head_branch.clone()
    } else {
        let prefixed = format!("{owner}/{head_branch}");
        validate_pr_branch(repo_root, context.number, &prefixed)?;
        if local_branch_tip(repo_root, &prefixed).is_some() {
            return Err(WorktreeErr::PrBranchConflict {
                branch: prefixed,
                detail: "both the bare and owner-prefixed branch names already exist".to_owned(),
            });
        }
        prefixed
    };
    let mut created = add_pr_worktree(
        repo_root,
        fresh,
        branch.clone(),
        pr_head.oid.clone(),
        pr_head.oid.as_str(),
        context.number,
        context.hooks,
    )?;
    let remote_key = format!("branch.{branch}.remote");
    git_run(
        repo_root,
        ["config", remote_key.as_str(), fork_url.as_str()],
    )?;
    let merge_key = format!("branch.{branch}.merge");
    let merge_ref = format!("refs/heads/{head_branch}");
    git_run(
        repo_root,
        ["config", merge_key.as_str(), merge_ref.as_str()],
    )?;
    created.push_destination = Some(PushDestination {
        remote: fork_url,
        merge_ref,
    });
    Ok(created)
}

fn pr_fetch_err(number: u64, remote: &str) -> impl FnOnce(WorktreeErr) -> WorktreeErr + '_ {
    move |err| match err {
        WorktreeErr::Git { stderr, .. } => WorktreeErr::PrFetch {
            number,
            remote: remote.to_owned(),
            stderr,
        },
        other => other,
    }
}

struct TempPrHead<'a> {
    repo_root: &'a Path,
    ref_name: String,
    oid: String,
}

impl Drop for TempPrHead<'_> {
    fn drop(&mut self) {
        let _ = git_run(self.repo_root, ["update-ref", "-d", self.ref_name.as_str()]);
    }
}

fn fetch_pr_head<'a>(
    repo_root: &'a Path,
    number: u64,
    remote: &str,
    refspec: &str,
) -> Result<TempPrHead<'a>> {
    let nonce = TEMP_REF_NONCE.fetch_add(1, Ordering::Relaxed);
    let ref_name = format!("refs/rimz/pr/{number}-{}-{nonce}", std::process::id());
    let mut head = TempPrHead {
        repo_root,
        ref_name,
        oid: String::new(),
    };
    let fetch_refspec = format!("+{refspec}:{}", head.ref_name);
    git_network_output(
        repo_root,
        [
            "fetch",
            "--no-tags",
            "--no-recurse-submodules",
            "origin",
            fetch_refspec.as_str(),
        ],
        PR_FETCH_TIMEOUT,
    )
    .map_err(pr_fetch_err(number, remote))?;
    head.oid = git_stdout(repo_root, ["rev-parse", head.ref_name.as_str()])?;
    Ok(head)
}

fn add_pr_worktree(
    repo_root: &Path,
    fresh: FreshWorktree,
    branch: String,
    pr_head: String,
    checkout_ref: &str,
    pr_number: u64,
    hooks: &WorktreeHooks,
) -> Result<CreatedWorktree> {
    add_worktree(
        repo_root,
        fresh.name,
        fresh.path,
        branch,
        pr_marker_provenance(repo_root, &pr_head, pr_number),
        Checkout::NewBranch(checkout_ref),
        hooks,
    )
}

fn pr_marker_provenance(
    repo_root: &Path,
    fallback_commit: &str,
    pr_number: u64,
) -> MarkerProvenance {
    let base_branch = trunk_ref(repo_root);
    let base_ref_name = base_branch.as_deref().unwrap_or("origin/HEAD");
    let base_ref = resolve_base_commit(repo_root, base_ref_name)
        .unwrap_or_else(|_| fallback_commit.to_owned());
    MarkerProvenance {
        base_branch,
        base_ref,
        from_pr: Some(pr_number),
    }
}

fn resolve_pr_head_with_cli(
    repo_root: &Path,
    number: u64,
    cli: forge::ForgeCli,
    remote: &forge::RemoteRepo,
) -> Result<forge::PrHead> {
    let parsed = (|| {
        let args = cli.pr_head_args(number, remote.repo_slug())?;
        let args = args.iter().map(String::as_str).collect::<Vec<_>>();
        let raw = pr_command_stdout(repo_root, cli.program(), &args)?;
        cli.decode_pr_head(&raw)
    })();
    parsed.map_err(|reason| WorktreeErr::PrHeadUnresolved { number, reason })
}

fn pr_command_stdout(
    cwd: &Path,
    program: &str,
    args: &[&str],
) -> std::result::Result<String, String> {
    let mut command = Command::new(program);
    command.current_dir(cwd).args(args).env("LC_ALL", "C");
    let output = crate::proc::run_bounded_output(&mut command, PR_HEAD_COMMAND_TIMEOUT)
        .map_err(|err| format!("could not run {program}: {err}"))?;
    if output.timed_out {
        return Err(format!("{program} timed out"));
    }
    if !output.status.success() {
        return Err(format!(
            "{program} exited with {} (install it and log in)",
            output.status
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn validate_pr_branch(repo_root: &Path, number: u64, branch: &str) -> Result<()> {
    git_run(repo_root, ["check-ref-format", "--branch", branch]).map_err(|_| {
        WorktreeErr::PrHeadUnresolved {
            number,
            reason: format!("forge reported invalid branch name `{branch}`"),
        }
    })
}

enum LocalPrBranch {
    New,
    Existing,
}

fn prepare_local_pr_branch(
    repo_root: &Path,
    branch: &str,
    remote_ref: &str,
    remote_head: &str,
    holder: Option<&Path>,
    choice: Option<PrBranchChoice>,
) -> Result<LocalPrBranch> {
    let Some(local_head) = local_branch_tip(repo_root, branch) else {
        return Ok(LocalPrBranch::New);
    };
    if local_head != remote_head {
        let divergence = classify_divergence(repo_root, &local_head, remote_head)?;
        let Some(choice) = choice else {
            return Err(WorktreeErr::PrBranchDiverged {
                branch: branch.to_owned(),
                holder: holder.map(Path::to_path_buf),
                divergence,
            });
        };
        if choice == PrBranchChoice::Remote {
            if let Some(path) = holder {
                git_run(path, ["reset", "--keep", remote_ref]).map_err(|err| {
                    WorktreeErr::PrBranchAlignFailed {
                        branch: branch.to_owned(),
                        holder: path.to_path_buf(),
                        detail: err.to_string(),
                    }
                })?;
            } else {
                git_run(repo_root, ["branch", "-f", branch, remote_ref])?;
            }
        }
    }
    git_run(
        repo_root,
        ["branch", "--set-upstream-to", remote_ref, branch],
    )?;
    Ok(LocalPrBranch::Existing)
}

fn local_branch_tip(repo_root: &Path, branch: &str) -> Option<String> {
    let ref_name = format!("refs/heads/{branch}^{{commit}}");
    git_stdout(
        repo_root,
        ["rev-parse", "--verify", "--quiet", ref_name.as_str()],
    )
    .ok()
}

fn branch_worktree(repo_root: &Path, branch: &str) -> Result<Option<(PathBuf, bool)>> {
    let rows = parse_worktree_list(&git_stdout(repo_root, ["worktree", "list", "--porcelain"])?);
    Ok(rows
        .into_iter()
        .enumerate()
        .find(|(_, row)| row.branch.as_deref() == Some(branch))
        .map(|(index, row)| (row.path, index == 0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worktree::read_marker_for_worktree;

    #[test]
    fn adopted_local_tip_survives_failed_creation_hook() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git_run(&repo, ["init"]).unwrap();
        git_run(&repo, ["config", "user.email", "rimz@example.test"]).unwrap();
        git_run(&repo, ["config", "user.name", "Test"]).unwrap();
        git_run(&repo, ["commit", "--allow-empty", "-m", "local work"]).unwrap();
        git_run(&repo, ["branch", "feature"]).unwrap();
        let head = git_stdout(&repo, ["rev-parse", "feature"]).unwrap();
        let path = dir.path().join("feature");
        let result = add_worktree(
            &repo,
            "feature".into(),
            path.clone(),
            "feature".into(),
            pr_marker_provenance(&repo, &head, 1),
            Checkout::Existing,
            &WorktreeHooks {
                created: Some("exit 1".into()),
                ..WorktreeHooks::default()
            },
        );
        assert!(matches!(result, Err(WorktreeErr::CreatedHook { .. })));
        assert!(!path.exists());
        assert_eq!(local_branch_tip(&repo, "feature"), Some(head));
    }

    #[test]
    fn classifies_behind_rebased_and_both_merge_outcomes() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git_run(repo, ["init"]).unwrap();
        git_run(repo, ["config", "user.name", "Test"]).unwrap();
        git_run(repo, ["config", "user.email", "test@example.test"]).unwrap();
        git_run(repo, ["commit", "--allow-empty", "-m", "base"]).unwrap();
        let base = git_stdout(repo, ["rev-parse", "HEAD"]).unwrap();
        std::fs::write(repo.join("patch"), "remote").unwrap();
        git_run(repo, ["add", "."]).unwrap();
        git_run(repo, ["commit", "-m", "patch"]).unwrap();
        let remote = git_stdout(repo, ["rev-parse", "HEAD"]).unwrap();
        assert_eq!(
            classify_divergence(repo, &base, &remote).unwrap(),
            PrBranchDivergence::Behind { behind: 1 }
        );
        git_run(repo, ["reset", "--hard", &base]).unwrap();
        std::fs::write(repo.join("other"), "base").unwrap();
        git_run(repo, ["add", "."]).unwrap();
        git_run(repo, ["commit", "-m", "other"]).unwrap();
        let local = git_stdout(repo, ["rev-parse", "HEAD"]).unwrap();
        assert_eq!(
            classify_divergence(repo, &local, &remote).unwrap(),
            PrBranchDivergence::Diverged {
                ahead: 1,
                behind: 1,
                conflicts: false
            }
        );
        git_run(repo, ["cherry-pick", &remote]).unwrap();
        let rebased = git_stdout(repo, ["rev-parse", "HEAD"]).unwrap();
        assert_eq!(
            classify_divergence(repo, &remote, &rebased).unwrap(),
            PrBranchDivergence::Rebased {
                ahead: 1,
                behind: 2
            }
        );
        git_run(repo, ["reset", "--hard", &base]).unwrap();
        std::fs::write(repo.join("patch"), "local").unwrap();
        git_run(repo, ["add", "."]).unwrap();
        git_run(repo, ["commit", "-m", "conflict"]).unwrap();
        let local = git_stdout(repo, ["rev-parse", "HEAD"]).unwrap();
        assert_eq!(
            classify_divergence(repo, &local, &remote).unwrap(),
            PrBranchDivergence::Diverged {
                ahead: 1,
                behind: 1,
                conflicts: true
            }
        );
    }

    #[test]
    fn pr_worktree_marker_records_pr_number() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).expect("repo dir");
        git_run(&repo, ["init"]).expect("git init");
        git_run(&repo, ["config", "user.email", "rimz@example.test"]).expect("git email");
        git_run(&repo, ["config", "user.name", "RimZ Test"]).expect("git name");
        git_run(&repo, ["commit", "--allow-empty", "-m", "base"]).expect("initial commit");
        let head = git_stdout(&repo, ["rev-parse", "HEAD"]).expect("head");
        let path = dir.path().join("review-69");

        add_pr_worktree(
            &repo,
            FreshWorktree {
                name: "review-69".to_owned(),
                path: path.clone(),
                branch: None,
            },
            "review-69".to_owned(),
            head.clone(),
            &head,
            69,
            &WorktreeHooks::default(),
        )
        .expect("PR worktree");

        let marker = read_marker_for_worktree(&path)
            .expect("read marker")
            .expect("marker");
        assert_eq!(marker.version, 4);
        assert_eq!(marker.from_pr, Some(69));
    }
}
