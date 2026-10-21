//! Worktree and caller resolution shared by the board commands.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use rimz::address::TeamCohort;
use rimz::agents::AgentState;
use rimz::harness::ancestry::{resolve_caller, resolve_launch_caller};
use rimz::harness::resume::{self, TeamHold, TeamHoldReason};
use rimz::utils::path::normalize_path_lexical;

pub(super) fn resolve<'a>(
    ctx: &super::super::Ctx,
    agents: &'a [AgentState],
    reference: Option<&str>,
    team: Option<&str>,
) -> Result<(TeamHold, Vec<&'a AgentState>)> {
    let address = reference.and_then(|reference| reference.split_once('#'));
    let team = address.map(|(team, _)| team).or(team);
    let checkout = address.is_none().then(worktree).transpose()?;
    let place = checkout.as_deref().map_or_else(
        || reference.unwrap_or_default().to_owned(),
        |path| path.display().to_string(),
    );
    let candidates = rimz::address::team_cohorts(agents)
        .into_iter()
        .filter(|cohort| team.is_none_or(|team| cohort.team == team))
        .filter(|cohort| match address {
            Some((_, lane)) => super::cohort::matches_worktree(cohort, lane),
            None => checkout
                .as_deref()
                .is_some_and(|path| in_worktree(cohort, path)),
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        let audit = ctx.store.runtime_projection(rimz::RuntimeScope::Audit)?;
        let hold = match address {
            Some((team, lane)) => resume::inspect_cohort_hold(&audit.agents, team, lane),
            None => checkout
                .as_deref()
                .and_then(|path| resume::inspect_team_hold(&audit.agents, path)),
        }
        .filter(|hold| team.is_none_or(|team| hold.team == team));
        if let Some(hold) = hold {
            return Ok((hold, Vec::new()));
        }
        bail!("no matching live or held team cohort in {place}");
    }
    let cohort = match candidates.as_slice() {
        [cohort] => cohort,
        [first, rest @ ..] if rest.iter().all(|cohort| cohort.team == first.team) => bail!(
            "team `{}` has multiple live cohorts in worktree {} in lanes {}; stop the extra cohorts with `rimz teams stop {}#<lane>` before flipping",
            first.team,
            place,
            candidates
                .iter()
                .map(|cohort| format!("#{}", cohort.channel))
                .collect::<Vec<_>>()
                .join(", "),
            first.team
        ),
        _ => bail!(
            "multiple live team cohorts in worktree {}; select a team with --team <name>: {}",
            place,
            candidates
                .iter()
                .map(|cohort| format!("{}#{}", cohort.team, cohort.channel))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    let checkout = cohort
        .members
        .first()
        .and_then(|agent| agent.worktree_path.as_deref())
        .map(PathBuf::from)
        .filter(|path| in_worktree(cohort, &normalize_path_lexical(path)))
        .context("selected cohort has no single worktree for its board")?;
    Ok((
        TeamHold {
            team: cohort.team.to_owned(),
            channel: cohort.channel.clone(),
            checkout,
            reason: TeamHoldReason::LiveMember,
        },
        cohort.members.clone(),
    ))
}

pub(super) fn worktree() -> Result<PathBuf> {
    let worktree = match std::env::var_os(rimz::workspace::ENV_WORKTREE_PATH) {
        Some(path) => normalize_path_lexical(Path::new(&path)),
        None => {
            let cwd = std::env::current_dir()?;
            let output = Command::new("git")
                .args(["rev-parse", "--show-toplevel"])
                .current_dir(&cwd)
                .output()
                .context("resolve the current worktree with git")?;
            if !output.status.success() {
                bail!("cannot resolve the current worktree; run from a git worktree");
            }
            let path =
                String::from_utf8(output.stdout).context("git worktree path is not UTF-8")?;
            let root = normalize_path_lexical(Path::new(path.trim()));
            nearest_team_checkout(&normalize_path_lexical(&cwd), &root)
        }
    };
    Ok(worktree)
}

/// The nearest directory from `cwd` up to the Git `root` whose team link
/// resolves, so a user in a `--cwd` team's subdirectory reaches that team's
/// board; `root` when none does.
pub(super) fn nearest_team_checkout(cwd: &Path, root: &Path) -> PathBuf {
    cwd.ancestors()
        .take_while(|dir| dir.starts_with(root))
        .find(|dir| {
            rimz::harness::scratch::board_path(dir)
                .parent()
                .is_some_and(Path::is_dir)
        })
        .unwrap_or(root)
        .to_path_buf()
}

pub(super) fn caller(agents: &[AgentState]) -> Result<Option<&AgentState>> {
    Ok(resolve_caller(agents)
        .map(|caller| resolve_launch_caller(agents, &caller))
        .transpose()?)
}

pub(super) fn in_worktree(cohort: &TeamCohort<'_>, worktree: &Path) -> bool {
    cohort.members.iter().all(|member| {
        member
            .worktree_path
            .as_deref()
            .is_some_and(|path| normalize_path_lexical(Path::new(path)) == worktree)
    })
}
