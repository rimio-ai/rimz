//! `blackboard_flip` and `blackboard_record`: the jobs of `rimz teams flip` and `rimz teams
//! record`, on the board of the caller's own live cohort, which the caller's recorded launch
//! names; the server's directory never chooses the board.

use jiff::Timestamp;
use serde::Serialize;

use super::Tools;
use super::room::Room;
use crate::address::{TeamCohort, team_cohorts};
use crate::agents::AgentState;
use crate::harness::ancestry::{resolve_caller, resolve_launch_caller};
use crate::harness::board::{self, BoardSection, RecordRequest};
use crate::harness::team_stage::{self, CohortFlipErr, CohortFlipRequest, Delivery, FlipErr};
use crate::mcp::catalog::{BlackboardFlipArgs, BlackboardRecordArgs, RecordSection};
use crate::mcp::{Answer, Failure};
use crate::utils::path::normalize_path_lexical;

const FLIP_HINT: &str = "flip with `rimz teams flip <Stage> \"<note>\"`";
const RECORD_HINT: &str = "record with `rimz teams record <Goal|Decisions|Result> \"<text>\"`";

pub(super) fn flip(tools: &Tools, args: &BlackboardFlipArgs) -> Result<Answer, Failure> {
    let room = Room::open(&tools.start_dir)?;
    let snapshot = room.resolution_snapshot_with_context()?;
    let (member, cohort) = caller_cohort(&snapshot.agents, FLIP_HINT)?;
    let checkout = board_checkout(&cohort)?;
    let worktree = normalize_path_lexical(&checkout);
    let on_board = team_cohorts(snapshot.agents.iter())
        .into_iter()
        .filter(|other| other.team() == cohort.team() && other.in_worktree(&worktree))
        .collect::<Vec<_>>();
    team_stage::refuse_shared_board(&on_board, &worktree.display().to_string())
        .map_err(|error| Failure::new(error.to_string()))?;
    let flipped = team_stage::flip_cohort(CohortFlipRequest {
        workspace: &room.workspace,
        store: &room.store,
        snapshot: &snapshot,
        cohort: &cohort,
        worktree: &checkout,
        member: Some(member),
        to: &args.stage,
        note: &args.note,
        mux: None,
        now: Timestamp::now(),
    })
    .map_err(|error| match error {
        // A partial flip's message already names its cause, which the chain would repeat.
        CohortFlipErr::Flip(partial @ FlipErr::PartialFlip { .. }) => {
            Failure::new(partial.to_string())
        }
        error => Failure::from(anyhow::Error::from(error)),
    })?;
    if let Err(error) = &flipped.report {
        tracing::warn!(%error, instance = %cohort.instance, "team report failed");
    }
    let receipt = &flipped.receipt;
    let owner = match &receipt.delivery {
        Delivery::SelfOwned => Some("you".to_owned()),
        Delivery::Terminal => None,
        Delivery::Sent { .. } | Delivery::Queued { .. } | Delivery::OwnerNotLive { .. } => {
            receipt.owner.as_ref().map(|owner| format!("@{owner}"))
        }
    };
    Ok(Answer::data(&Flipped {
        stage: &receipt.to,
        owner,
    }))
}

/// What `blackboard_flip` answers: the stage it opened and, short of `Done`, its owner.
#[derive(Serialize)]
struct Flipped<'a> {
    stage: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner: Option<String>,
}

pub(super) fn record(tools: &Tools, args: &BlackboardRecordArgs) -> Result<Answer, Failure> {
    let room = Room::open(&tools.start_dir)?;
    let snapshot = room.resolution_snapshot_with_context()?;
    let (member, cohort) = caller_cohort(&snapshot.agents, RECORD_HINT)?;
    let checkout = board_checkout(&cohort)?;
    let role = member
        .role
        .as_deref()
        .ok_or_else(|| Failure::new("calling team member has no role"))?;
    let section = match args.section {
        RecordSection::Goal => BoardSection::Goal,
        RecordSection::Decisions => BoardSection::Decisions,
        RecordSection::Result => BoardSection::Result,
    };
    board::record(RecordRequest {
        store: &room.store,
        worktree: &checkout,
        team: member.team.as_deref(),
        section,
        by: role,
        text: &args.text,
        now: Timestamp::now(),
    })
    .map_err(anyhow::Error::from)?;
    Ok(Answer::Text(format!("recorded {section} @{role}")))
}

/// The caller's recorded launch and the live cohort it is a member of, or the refusal naming
/// the shell command in `hint`.
fn caller_cohort<'a>(
    agents: &'a [AgentState],
    hint: &str,
) -> Result<(&'a AgentState, TeamCohort<'a>), Failure> {
    let member = resolve_caller(agents)
        .and_then(|caller| resolve_launch_caller(agents, &caller).ok())
        .ok_or_else(|| Failure::new("the calling agent has no recorded launch").with_hint(hint))?;
    let cohort = team_cohorts(agents.iter())
        .into_iter()
        .find(|cohort| cohort.contains(member))
        .ok_or_else(|| Failure::new("the calling agent is on no live team").with_hint(hint))?;
    Ok((member, cohort))
}

fn board_checkout(cohort: &TeamCohort<'_>) -> Result<std::path::PathBuf, Failure> {
    cohort.board_checkout().ok_or_else(|| {
        Failure::new("the calling agent's cohort has no single worktree for its board")
    })
}
