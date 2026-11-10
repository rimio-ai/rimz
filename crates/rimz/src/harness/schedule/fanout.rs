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
    let clocks = fire::last_when_states(runtime);
    let stamps = fire::last_stamps(runtime);
    let windows = WindowReadings::new(Some(runtime), now);
    let mut rows = Vec::new();
    for worktree in crate::worktree::discover_owned(&root)? {
        let checkout = crate::utils::path::normalize_path_lexical(&worktree.marker.worktree_path);
        let state = match launch_ledger::launched(entry, name, &checkout, workflows) {
            Some(launch) => match &launch.run {
                Some(run) => CheckoutState::Workflow {
                    run: run.clone(),
                    at: launch.at,
                },
                None => CheckoutState::Launched {
                    leader: launch.leader.clone().unwrap_or_default(),
                    at: launch.at,
                },
            },
            None => {
                let key = fire::scope_key(name, &checkout);
                let verdict = when::evaluate(
                    expr,
                    &checkout,
                    ci_source,
                    entry.provider.as_ref(),
                    entry.account.as_ref(),
                    &windows,
                )
                .freshen(
                    expr,
                    &checkout,
                    ci_source,
                    stamps.get(&key).copied(),
                    Some(now),
                    true,
                );
                let clock = clocks
                    .get(&key)
                    .filter(|clock| clock.matches_condition(expr, *hold, &entry.run_dir()));
                let since = clock.map_or(now, |clock| clock.since);
                if !verdict.ok {
                    CheckoutState::Waiting { verdict }
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
