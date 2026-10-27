//! Read-only condition inspection for owned worktrees.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use jiff::Timestamp;

use super::{
    ParsedTrigger, Trigger,
    catalog::LoadedTask,
    fire, launch_ledger,
    when::{self, CiSource, Verdict, WindowReadings},
};
use crate::RuntimePaths;

#[derive(Clone, Debug)]
pub struct CheckoutCondition {
    pub checkout: PathBuf,
    pub name: String,
    pub state: CheckoutState,
}

#[derive(Clone, Debug)]
pub enum CheckoutState {
    Launched {
        leader: String,
        at: Timestamp,
    },
    Workflow {
        run: crate::ids::WorkflowRunId,
        at: Timestamp,
    },
    Declined {
        at: Timestamp,
        reason: String,
        profile: String,
        recheck_at: Option<Timestamp>,
    },
    Holding {
        since: Timestamp,
        hold: Duration,
        verdict: Verdict,
    },
    Ready {
        since: Timestamp,
        verdict: Verdict,
    },
    Waiting {
        verdict: Verdict,
    },
}

pub fn inspect(
    name: &str,
    task: &LoadedTask,
    runtime: &RuntimePaths,
    workflows: &launch_ledger::WorkflowLedger,
    ci_source: Option<&CiSource>,
    now: Timestamp,
) -> Result<Vec<CheckoutCondition>> {
    let Ok(ParsedTrigger {
        trigger: Trigger::Condition { expr, hold },
        ..
    }) = task.trigger()
    else {
        return Ok(Vec::new());
    };
    let entry = task.entry();
    let root = entry.resolved_root();
    let launches = if entry.stay {
        launch_ledger::load_room(runtime, Some(&root))?
    } else {
        Default::default()
    };
    let clocks = fire::last_when_states(runtime);
    let declines = if entry.stay {
        launch_ledger::load_declines_room(runtime, Some(&root))?
    } else {
        Default::default()
    };
    let windows = WindowReadings::new(Some(runtime), now);
    let mut rows = Vec::new();
    for worktree in crate::worktree::discover_owned(&root)? {
        let checkout = crate::utils::path::normalize_path_lexical(&worktree.marker.worktree_path);
        let state = match launch_ledger::launched(entry, name, &checkout, &launches, workflows) {
            Some(launch_ledger::Launch::Resident(launch)) => CheckoutState::Launched {
                leader: launch.leader.clone(),
                at: launch.at,
            },
            Some(launch_ledger::Launch::Workflow(launch)) => CheckoutState::Workflow {
                run: launch.run.clone(),
                at: launch.at,
            },
            None => {
                let verdict = when::evaluate(
                    expr,
                    &checkout,
                    ci_source,
                    entry.provider.as_ref(),
                    entry.account.as_ref(),
                    &windows,
                );
                let clock = clocks
                    .get(&fire::scope_key(name, &checkout))
                    .filter(|clock| clock.matches_condition(expr, *hold, &entry.run_dir()));
                let since = clock.map_or(now, |clock| clock.since);
                if !verdict.ok {
                    CheckoutState::Waiting { verdict }
                } else if let Some(decline) = launch_ledger::holding_decline(
                    &declines,
                    name,
                    entry,
                    &checkout,
                    clock.map(|clock| clock.since),
                    now,
                ) {
                    CheckoutState::Declined {
                        at: decline.at,
                        reason: decline.reason.clone(),
                        profile: decline.profile.clone(),
                        recheck_at: entry
                            .guard()
                            .judges
                            .get(decline.tier)
                            .and_then(|judge| launch_ledger::judge_recheck(judge))
                            .and_then(|duration| decline.at.checked_add(duration).ok()),
                    }
                } else if let Some(hold) = hold
                    && u128::try_from(now.duration_since(since).as_millis()).unwrap_or(0)
                        < hold.as_millis()
                {
                    CheckoutState::Holding {
                        since,
                        hold: *hold,
                        verdict,
                    }
                } else {
                    CheckoutState::Ready { since, verdict }
                }
            }
        };
        rows.push(CheckoutCondition {
            checkout,
            name: worktree.marker.name,
            state,
        });
    }
    Ok(rows)
}
