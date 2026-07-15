//! PR-based worktree checkout.
//!
//! Strategy selection keeps review-only, same-repository, and fork checkout paths separate. Forge CLI head resolution and fork remote configuration stay here with the PR-specific git plumbing.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::config::WorktreeConfig;
use crate::forge;

use super::{
    Checkout, CreatedWorktree, FreshWorktree, MarkerProvenance, PushDestination, Result,
    WorktreeCreateTarget, WorktreeErr, add_worktree, ensure_repo, git_run, git_stdout, is_ancestor,
    parse_worktree_list, resolve_base_commit, resolve_branch, resolve_fresh_worktree, trunk_ref,
};

const PR_HEAD_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

struct PrContext {
    number: u64,
    remote: String,
    refspec: String,
}

pub fn create_from_pr(
    repo_root: &Path,
    config: &WorktreeConfig,
    pr: &forge::PrTarget,
    name: Option<&str>,
    branch: Option<&str>,
    reuse_existing: bool,
) -> Result<CreatedWorktree> {
    ensure_repo(repo_root)?;
    let default_name = format!("pr-{}", pr.number);
    let fresh = match resolve_fresh_worktree(
        repo_root,
        config,
        name,
        Some(default_name.as_str()),
        reuse_existing,
    )? {
        WorktreeCreateTarget::Fresh(fresh) => fresh,
        WorktreeCreateTarget::Reuse(reused) => return Ok(reused),
    };
    let review_branch = branch.or(fresh.branch.as_deref());

    let remote = git_stdout(repo_root, ["config", "--get", "remote.origin.url"]).map_err(
        |err| match err {
            WorktreeErr::Git { .. } => WorktreeErr::Parse(format!(
                "could not fetch PR #{}: git remote `origin` is not configured",
                pr.number
            )),
            other => other,
        },
    )?;
    let forge = pr.forge.unwrap_or_else(|| forge::forge_for_remote(&remote));
    let context = PrContext {
        number: pr.number,
        refspec: forge.pr_refspec(pr.number),
        remote,
    };

    if let Some(branch) = review_branch {
        let branch = resolve_branch(Some(branch), None, &fresh.name)?;
        return review_only_checkout(repo_root, fresh, &context, branch);
    }

    let advertised = git_stdout(repo_root, ["ls-remote", "origin"])
        .map_err(pr_fetch_err(context.number, &context.remote))?;
    let (_advertised_head, head_branches) = forge::pr_head_branches(&advertised, &context.refspec)
        .ok_or_else(|| WorktreeErr::PrFetch {
            number: context.number,
            remote: context.remote.clone(),
            stderr: format!("remote did not advertise {}", context.refspec),
        })?;

    let same_repo_branch = match head_branches.as_slice() {
        [branch] => Some(branch.clone()),
        [] => None,
        branches => {
            let head = resolve_pr_head_with_cli(repo_root, context.number, &context.remote)?;
            Some(
                branches
                    .contains(&head.branch)
                    .then_some(head.branch)
                    .ok_or_else(|| WorktreeErr::PrHeadUnresolved {
                        number: context.number,
                        reason: "forge CLI head branch did not match an advertised origin branch"
                            .to_owned(),
                    })?,
            )
        }
    };

    if let Some(branch) = same_repo_branch {
        same_repo_checkout(repo_root, fresh, &context, branch)
    } else {
        fork_checkout(repo_root, fresh, &context)
    }
}

fn review_only_checkout(
    repo_root: &Path,
    fresh: FreshWorktree,
    context: &PrContext,
    branch: String,
) -> Result<CreatedWorktree> {
    let pr_head = fetch_pr_head(repo_root, context.number, &context.remote, &context.refspec)?;
    add_pr_worktree(
        repo_root,
        fresh,
        branch,
        pr_head.clone(),
        pr_head.as_str(),
        context.number,
    )
}

fn same_repo_checkout(
    repo_root: &Path,
    fresh: FreshWorktree,
    context: &PrContext,
    branch: String,
) -> Result<CreatedWorktree> {
    validate_pr_branch(repo_root, context.number, &branch)?;
    let remote_ref = format!("origin/{branch}");
    let fetch_refspec = format!("+refs/heads/{branch}:refs/remotes/{remote_ref}");
    git_run(repo_root, ["fetch", "origin", fetch_refspec.as_str()])
        .map_err(pr_fetch_err(context.number, &context.remote))?;
    let remote_head = git_stdout(repo_root, ["rev-parse", remote_ref.as_str()])?;
    let provenance = pr_marker_provenance(repo_root, &remote_head, context.number);
    let checkout = match prepare_local_pr_branch(repo_root, &branch, &remote_ref, &remote_head)? {
        LocalPrBranch::New => Checkout::Tracking(&remote_ref),
        LocalPrBranch::Existing => Checkout::Existing,
    };
    add_worktree(
        repo_root, fresh.name, fresh.path, branch, provenance, checkout,
    )
}

fn fork_checkout(
    repo_root: &Path,
    fresh: FreshWorktree,
    context: &PrContext,
) -> Result<CreatedWorktree> {
    let head = resolve_pr_head_with_cli(repo_root, context.number, &context.remote)?;
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
    let fork_url = forge::sibling_repo_url(&context.remote, &repo_full_name).ok_or_else(|| {
        WorktreeErr::PrHeadUnresolved {
            number: context.number,
            reason: "could not build the fork clone URL from origin".to_owned(),
        }
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
        pr_head.clone(),
        pr_head.as_str(),
        context.number,
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

fn fetch_pr_head(repo_root: &Path, number: u64, remote: &str, refspec: &str) -> Result<String> {
    git_run(repo_root, ["fetch", "origin", refspec]).map_err(pr_fetch_err(number, remote))?;
    git_stdout(repo_root, ["rev-parse", "FETCH_HEAD"])
}

fn add_pr_worktree(
    repo_root: &Path,
    fresh: FreshWorktree,
    branch: String,
    pr_head: String,
    checkout_ref: &str,
    pr_number: u64,
) -> Result<CreatedWorktree> {
    add_worktree(
        repo_root,
        fresh.name,
        fresh.path,
        branch,
        pr_marker_provenance(repo_root, &pr_head, pr_number),
        Checkout::NewBranch(checkout_ref),
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

fn resolve_pr_head_with_cli(repo_root: &Path, number: u64, remote: &str) -> Result<forge::PrHead> {
    let cli = forge::forge_cli_for_remote(remote).ok_or_else(|| WorktreeErr::PrHeadUnresolved {
        number,
        reason: "origin has no supported forge CLI".to_owned(),
    })?;
    let parsed = match cli {
        forge::ForgeCli::Gh => gh_pr_head(repo_root, number),
        forge::ForgeCli::Tea => tea_pr_head(repo_root, number, remote),
    };
    parsed.map_err(|reason| WorktreeErr::PrHeadUnresolved { number, reason })
}

fn gh_pr_head(repo_root: &Path, number: u64) -> std::result::Result<forge::PrHead, String> {
    let number_arg = number.to_string();
    let args = [
        "pr",
        "view",
        number_arg.as_str(),
        "--json",
        "headRefName,headRepository,headRepositoryOwner",
    ];
    let raw = pr_command_stdout(repo_root, "gh", &args)?;
    forge::parse_gh_pr_view_json(&raw)
}

fn tea_pr_head(
    repo_root: &Path,
    number: u64,
    remote: &str,
) -> std::result::Result<forge::PrHead, String> {
    let number_arg = number.to_string();
    let repo = forge::remote_repo_slug(remote)
        .ok_or_else(|| "could not derive the origin repository for tea".to_owned())?;
    let args = [
        "pr",
        number_arg.as_str(),
        "--output",
        "json",
        "--repo",
        repo.as_str(),
    ];
    let raw = pr_command_stdout(repo_root, "tea", &args)?;
    forge::parse_tea_pr_head_json(&raw)
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
) -> Result<LocalPrBranch> {
    let Some(local_head) = local_branch_tip(repo_root, branch) else {
        return Ok(LocalPrBranch::New);
    };
    if let Some(path) = branch_worktree(repo_root, branch)? {
        return Err(WorktreeErr::PrBranchConflict {
            branch: branch.to_owned(),
            detail: format!("it is checked out at {}", path.display()),
        });
    }
    if local_head != remote_head {
        if is_ancestor(repo_root, &local_head, remote_head) {
            git_run(repo_root, ["branch", "-f", branch, remote_ref])?;
        } else {
            let detail = if is_ancestor(repo_root, remote_head, &local_head) {
                "the local branch is ahead of the PR head"
            } else {
                "the local branch has diverged from the PR head"
            };
            return Err(WorktreeErr::PrBranchConflict {
                branch: branch.to_owned(),
                detail: detail.to_owned(),
            });
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

fn branch_worktree(repo_root: &Path, branch: &str) -> Result<Option<PathBuf>> {
    let rows = parse_worktree_list(&git_stdout(repo_root, ["worktree", "list", "--porcelain"])?);
    Ok(rows
        .into_iter()
        .find(|row| row.branch.as_deref() == Some(branch))
        .map(|row| row.path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worktree::read_marker_for_worktree;

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
        )
        .expect("PR worktree");

        let marker = read_marker_for_worktree(&path)
            .expect("read marker")
            .expect("marker");
        assert_eq!(marker.version, 4);
        assert_eq!(marker.from_pr, Some(69));
    }
}
