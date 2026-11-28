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
    Checkout, CreatedWorktree, ExternalCheckout, FreshWorktree, MarkerProvenance, PushDestination,
    Result, WorktreeCreateTarget, WorktreeErr, add_worktree, ensure_repo, git_network_output,
    git_run, git_stdout, is_ancestor, list_worktrees, merge_tree, read_marker_for_worktree,
    resolve_base_commit, resolve_branch, resolve_fresh_worktree, trunk_ref, write_marker,
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
    let conflicts = merge_tree(repo, local, remote).is_err();
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

/// A pull request head that is on no branch of `origin`: code that is not the user's own, so a
/// checkout holds it only on the caller's confirmation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalPrHead {
    pub number: u64,
    /// The fetched head commit.
    pub commit: String,
    /// The repository the forge names as the head's home, when RimZ asked the forge.
    pub repository: Option<String>,
    /// Why the branches of `origin` could not be read, when they could not: the head then counts
    /// as external unchecked.
    pub branches_unread: Option<String>,
}

impl std::fmt::Display for ExternalPrHead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let commit = self.commit.get(..7).unwrap_or(&self.commit);
        let from = self
            .repository
            .as_ref()
            .map(|repository| format!(" from {repository}"))
            .unwrap_or_default();
        match &self.branches_unread {
            None => write!(
                f,
                "PR #{} is external: its head {commit}{from} is on no branch of origin, so its code \
                 is not verified",
                self.number
            ),
            Some(reason) => write!(
                f,
                "PR #{} is external: origin's branches could not be read ({reason}), so its head \
                 {commit}{from} was not checked and its code is not verified",
                self.number
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

/// Check `pr` out into a tree, or reuse the tree that holds it. A head on no branch of `origin`
/// refuses with [`WorktreeErr::PrExternal`] unless `external` carries the caller's confirmation,
/// which the tree's marker then records.
#[allow(clippy::too_many_arguments)]
pub fn create_from_pr(
    repo_root: &Path,
    config: &WorktreeConfig,
    pr: &forge::PrTarget,
    name: Option<&str>,
    branch: Option<&str>,
    reuse_existing: bool,
    choice: Option<PrBranchChoice>,
    external: Option<ExternalCheckout>,
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
        return review_only_checkout(repo_root, fresh, &context, branch, None, external);
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
            external,
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
            external,
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
        fork_checkout(repo_root, require_fresh(target)?, &context, head, external)
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
    external: Option<ExternalCheckout>,
) -> Result<CreatedWorktree> {
    let pr_head = fetch_pr_head(repo_root, context.number, &context.remote, &context.refspec)?;
    let external = external_record(
        context.number,
        fresh.branch.as_deref().unwrap_or(&fresh.name),
        &pr_head.oid,
        None,
        origin_branch_tips(repo_root),
        external,
    )?;
    let mut created = add_pr_worktree(
        repo_root,
        fresh,
        branch,
        pr_head.oid.as_str(),
        context.number,
        external,
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
            trunk_fetch: None,
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
    external: Option<ExternalCheckout>,
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
    let external = external_record(
        context.number,
        &fresh.name,
        &pr_head.oid,
        Some(&repo_full_name),
        origin_branch_tips(repo_root),
        external,
    )?;
    let mut created = add_pr_worktree(
        repo_root,
        fresh,
        branch.clone(),
        pr_head.oid.as_str(),
        context.number,
        external,
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

/// The `git ls-remote --heads` listing of `origin`, or why it could not be read. Callers read it
/// after the PR fetch, so a branch that moved in between reads as external, the side that asks.
fn origin_branch_tips(repo_root: &Path) -> std::result::Result<String, String> {
    match git_network_output(
        repo_root,
        ["ls-remote", "--heads", "origin"],
        PR_FETCH_TIMEOUT,
    ) {
        Ok(output) => Ok(String::from_utf8_lossy(&output.stdout).into_owned()),
        Err(WorktreeErr::Git { stderr, .. }) => Err(stderr),
        Err(err) => Err(err.to_string()),
    }
}

/// The record a tree holding `commit` carries: none when `commit` is the tip of a branch in
/// `branch_tips`, which makes it the user's own code, else the caller's confirmation, or the
/// refusal that asks for one. Tips that could not be read count as external, the side that asks.
fn external_record(
    number: u64,
    name: &str,
    commit: &str,
    repository: Option<&str>,
    branch_tips: std::result::Result<String, String>,
    confirmed: Option<ExternalCheckout>,
) -> Result<Option<ExternalCheckout>> {
    let branches_unread = match branch_tips {
        Ok(tips)
            if tips
                .lines()
                .any(|line| line.split('\t').next() == Some(commit)) =>
        {
            return Ok(None);
        }
        Ok(_) => None,
        Err(reason) => Some(reason),
    };
    confirmed
        .ok_or_else(|| WorktreeErr::PrExternal {
            head: ExternalPrHead {
                number,
                commit: commit.to_owned(),
                repository: repository.map(str::to_owned),
                branches_unread,
            },
            name: name.to_owned(),
        })
        .map(Some)
}

fn add_pr_worktree(
    repo_root: &Path,
    fresh: FreshWorktree,
    branch: String,
    pr_head: &str,
    pr_number: u64,
    external: Option<ExternalCheckout>,
    hooks: &WorktreeHooks,
) -> Result<CreatedWorktree> {
    add_worktree(
        repo_root,
        fresh.name,
        fresh.path,
        branch,
        MarkerProvenance {
            external,
            ..pr_marker_provenance(repo_root, pr_head, pr_number)
        },
        Checkout::NewBranch(pr_head),
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
        external: None,
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
    let rows = list_worktrees(repo_root)?;
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
        let before_clean = super::super::tests::object_files(repo);
        assert_eq!(
            classify_divergence(repo, &local, &remote).unwrap(),
            PrBranchDivergence::Diverged {
                ahead: 1,
                behind: 1,
                conflicts: false
            }
        );
        let after_clean = super::super::tests::object_files(repo);
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
        let before_conflict = super::super::tests::object_files(repo);
        assert_eq!(
            classify_divergence(repo, &local, &remote).unwrap(),
            PrBranchDivergence::Diverged {
                ahead: 1,
                behind: 1,
                conflicts: true
            }
        );
        let after_conflict = super::super::tests::object_files(repo);
        assert_eq!(
            [before_clean, before_conflict],
            [after_clean, after_conflict],
            "PR divergence checks must not add repository objects"
        );
    }

    /// A head on no branch of `origin` refuses until the caller confirms it, and the tree records
    /// the confirmation; once the head is a branch tip there, the checkout needs none.
    #[test]
    fn external_pr_head_needs_a_confirmation_and_the_tree_records_it() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let origin = dir.path().join("origin.git");
        std::fs::create_dir(&repo).unwrap();
        git_run(&repo, ["init", "-b", "main"]).unwrap();
        git_run(&repo, ["config", "user.email", "rimz@example.test"]).unwrap();
        git_run(&repo, ["config", "user.name", "Test"]).unwrap();
        git_run(&repo, ["commit", "--allow-empty", "-m", "base"]).unwrap();
        git_run(&repo, ["init", "--bare", origin.to_str().unwrap()]).unwrap();
        git_run(&repo, ["remote", "add", "origin", origin.to_str().unwrap()]).unwrap();
        git_run(&repo, ["push", "origin", "main"]).unwrap();
        git_run(&repo, ["commit", "--allow-empty", "-m", "outside work"]).unwrap();
        let head = git_stdout(&repo, ["rev-parse", "HEAD"]).unwrap();
        git_run(&repo, ["push", "origin", "HEAD:refs/pull/1/head"]).unwrap();
        git_run(&repo, ["reset", "--hard", "HEAD~1"]).unwrap();
        let config = WorktreeConfig::default();
        let pr = forge::parse("1").unwrap();
        let checkout = |name, external| {
            create_from_pr(&repo, &config, &pr, Some(name), None, false, None, external)
        };

        let branch_style = checkout("review/x", None).expect_err("an external head");
        assert!(
            branch_style.to_string().ends_with(
                "check it out with `rimz worktree new review/x --from-pr 1 --allow-external`, \
                 then launch again with `--from-pr 1 -w review/x`"
            ),
            "the printed command keeps the branch a branch-style name asks for: {branch_style}"
        );
        let refused = checkout("pr-1", None).expect_err("an external head needs a confirmation");
        assert_eq!(
            refused.to_string(),
            format!(
                "PR #1 is external: its head {} is on no branch of origin, so its code is not \
                 verified; check it out with `rimz worktree new pr-1 --from-pr 1 \
                 --allow-external`, then launch again with `--from-pr 1 -w pr-1`",
                &head[..7]
            )
        );
        assert!(!dir.path().join("repo-worktrees/pr-1").exists());
        assert_eq!(local_branch_tip(&repo, "pr-1"), None);
        assert_eq!(
            git_stdout(&repo, ["for-each-ref", "refs/rimz/"]).unwrap(),
            ""
        );

        let confirmed = ExternalCheckout {
            confirmed_by: super::super::ConfirmedBy::Flag,
            agent: Some("young-cable".to_owned()),
        };
        let created = checkout("pr-1", Some(confirmed.clone())).expect("confirmed checkout");
        assert_eq!(created.marker.external, Some(confirmed.clone()));
        let path = created.marker.worktree_path;
        assert_eq!(git_stdout(&path, ["rev-parse", "HEAD"]).unwrap(), head);
        assert_eq!(
            read_marker_for_worktree(&path).unwrap().unwrap().external,
            Some(confirmed.clone()),
            "the record is durable on the marker"
        );

        git_run(
            &repo,
            ["push", "origin", &format!("{head}:refs/heads/feature")],
        )
        .unwrap();
        let own = checkout("own", Some(confirmed)).expect("own checkout");
        assert_eq!(
            own.marker.external, None,
            "a branch tip on origin is the user's own"
        );
    }

    /// Branches of `origin` that could not be read leave the head unchecked: it counts as external,
    /// and the warning says why it was not checked.
    #[test]
    fn unreadable_origin_branches_count_as_external() {
        let unread = || Err("fatal: could not read from remote repository".to_owned());
        let refused = external_record(
            3,
            "pr-3",
            "1234567890ab",
            Some("alice/fork"),
            unread(),
            None,
        )
        .expect_err("an unchecked head needs a confirmation");
        assert_eq!(
            refused.to_string(),
            "PR #3 is external: origin's branches could not be read (fatal: could not read from \
             remote repository), so its head 1234567 from alice/fork was not checked and its code \
             is not verified; check it out with `rimz worktree new pr-3 --from-pr 3 \
             --allow-external`, then launch again with `--from-pr 3 -w pr-3`"
        );
        let confirmed = ExternalCheckout {
            confirmed_by: super::super::ConfirmedBy::Prompt,
            agent: None,
        };
        assert_eq!(
            external_record(
                3,
                "pr-3",
                "1234567890ab",
                None,
                unread(),
                Some(confirmed.clone())
            )
            .expect("a confirmed unchecked head"),
            Some(confirmed)
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
            &head,
            69,
            None,
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
