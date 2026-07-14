use super::*;
use crate::agents::{ATTENTION_AGE_CEILING_SECS, DEFAULT_INACTIVE_AFTER_SECS};
use crate::{RowCard, WorktreeTrunkSync};

fn ranked_snapshot(mut agents: Vec<AgentState>) -> SidebarSnapshot {
    let mut panes = Vec::new();
    for (idx, agent) in agents.iter_mut().enumerate() {
        let raw = format!("%tier-{idx}");
        let mut live = pane(
            &raw,
            agent.kind.as_str(),
            agent.worktree_path.as_deref().unwrap_or("/repo/main"),
        );
        if agent.worktree_path.is_none() {
            live.cwd = None;
        }
        agent.pane = Some(live.clone());
        panes.push(live);
    }

    room(agents).with_live_panes(panes, None)
}

fn labels(snapshot: &SidebarSnapshot) -> Vec<String> {
    snapshot
        .worktree_groups
        .iter()
        .map(|group| group.label.clone())
        .collect()
}

#[test]
fn group_tiering_floats_attention_and_always_tails_external() {
    let labels_for = |agents: Vec<AgentState>| labels(&ranked_snapshot(agents));
    let external = |id: &str, status: AgentStatus| agent("claude", id, status, 1_000);

    // A calm external sinks below calm project worktrees; an attention
    // worktree leads regardless of its name.
    assert_eq!(
        labels_for(vec![
            agent_in("a1", "/repo/alpha", AgentStatus::Failed, 1_000),
            agent_in("a2", "/repo/alpha", AgentStatus::Idle, 1_000),
            agent_in("b1", "/repo/beta", AgentStatus::Idle, 1_000),
            agent_in("b2", "/repo/beta", AgentStatus::Idle, 1_000),
            external("e1", AgentStatus::Idle),
        ]),
        vec!["alpha", "beta", "external"]
    );

    // The external catch-all always tails, even when it holds an attention
    // agent: out-of-project work never displaces a project worktree.
    assert_eq!(
        labels_for(vec![
            agent_in("b1", "/repo/beta", AgentStatus::Idle, 1_000),
            agent_in("b2", "/repo/beta", AgentStatus::Idle, 1_000),
            external("e1", AgentStatus::Failed),
        ]),
        vec!["beta", "external"]
    );

    // It tails below even an inactive project group — a stale, calm worktree
    // still outranks an out-of-project agent holding `failed`.
    assert_eq!(
        labels_for(vec![
            agent_in("a1", "/repo/alpha", AgentStatus::Idle, 1_000)
                .active_ago(ATTENTION_AGE_CEILING_SECS + 1),
            external("e1", AgentStatus::Failed),
        ]),
        vec!["alpha", "external"]
    );
}

#[test]
fn git_rung_orders_calm_worktree_groups() {
    let mut snapshot = ranked_snapshot(vec![
        agent_in("unknown", "/repo/unknown", AgentStatus::Idle, 1_000),
        agent_in("merged", "/repo/merged", AgentStatus::Idle, 1_000),
        agent_in("dirty", "/repo/dirty", AgentStatus::Idle, 1_000),
        agent_in("clean", "/repo/clean", AgentStatus::Idle, 1_000),
    ]);
    for group in &mut snapshot.worktree_groups {
        match group.label.as_str() {
            "dirty" => group.clean = Some(false),
            "clean" => {
                group.clean = Some(true);
                group.landed = Some(false);
            }
            "merged" => {
                group.clean = Some(true);
                group.trunk_sync = Some(WorktreeTrunkSync::Merged);
            }
            "unknown" => {}
            label => panic!("unexpected group {label}"),
        }
    }
    snapshot.sort_groups_for_presentation();

    assert_eq!(
        labels(&snapshot),
        vec!["dirty", "clean", "unknown", "merged"],
        "git rung decides among calm groups after band and urgency tie"
    );
}

#[test]
fn calm_activity_precedes_git_for_working_and_success_groups() {
    let mut snapshot = ranked_snapshot(vec![
        agent_in("success", "/repo/success", AgentStatus::Success, 1_000),
        agent_in("running", "/repo/working", AgentStatus::Running, 2_000),
        agent_in("idle", "/repo/working", AgentStatus::Idle, 3_000),
    ]);
    for group in &mut snapshot.worktree_groups {
        match group.label.as_str() {
            "success" => group.clean = Some(false),
            "working" => {
                group.clean = Some(true);
                group.trunk_sync = Some(WorktreeTrunkSync::Merged);
            }
            label => panic!("unexpected group {label}"),
        }
    }
    snapshot.sort_groups_for_presentation();

    assert_eq!(labels(&snapshot), vec!["working", "success"]);
    assert!(
        !snapshot.worktree_groups[0].finished,
        "a running member revives a git-finished line of work"
    );
}

#[test]
fn clean_success_group_leads_merged_success_group() {
    let mut snapshot = ranked_snapshot(vec![
        agent_in("merged", "/repo/merged", AgentStatus::Success, 1_000),
        agent_in("clean", "/repo/clean", AgentStatus::Success, 2_000),
    ]);
    for group in &mut snapshot.worktree_groups {
        group.clean = Some(true);
        if group.label == "merged" {
            group.trunk_sync = Some(WorktreeTrunkSync::Merged);
        }
    }
    snapshot.sort_groups_for_presentation();

    assert_eq!(labels(&snapshot), vec!["clean", "merged"]);
    assert!(!snapshot.worktree_groups[0].finished);
    assert!(snapshot.worktree_groups[1].finished);
}

#[test]
fn merged_success_archives_immediately_and_attention_revives_it() {
    let mut snapshot = ranked_snapshot(vec![
        agent_in("done", "/repo/done", AgentStatus::Success, 1_000).active_ago(5 * 60),
        agent_in("warm", "/repo/warm", AgentStatus::Idle, 2_000)
            .active_ago(i64::from(DEFAULT_INACTIVE_AFTER_SECS) + 1),
    ]);
    let done = snapshot
        .worktree_groups
        .iter_mut()
        .find(|group| group.label == "done")
        .expect("done group present");
    done.clean = Some(true);
    done.trunk_sync = Some(WorktreeTrunkSync::Merged);
    snapshot.sort_groups_for_presentation();

    assert_eq!(labels(&snapshot), vec!["warm", "done"]);
    let done = snapshot
        .worktree_groups
        .iter()
        .find(|group| group.label == "done")
        .expect("done group present");
    assert!(done.finished);
    assert!(
        done.rows.iter().all(|row| !row.inactive && !row.archived),
        "the group archives from its terminal verdict, not its activity clock"
    );

    let done = snapshot
        .worktree_groups
        .iter_mut()
        .find(|group| group.label == "done")
        .expect("done group present");
    done.rows[0].as_agent_mut().expect("agent row").status = AgentStatus::Running;
    snapshot.sort_groups_for_presentation();
    assert_eq!(labels(&snapshot), vec!["done", "warm"]);
    assert!(!snapshot.worktree_groups[0].finished);

    snapshot.worktree_groups[0].rows[0]
        .as_agent_mut()
        .expect("agent row")
        .status = AgentStatus::Waiting;
    snapshot.sort_groups_for_presentation();
    assert_eq!(labels(&snapshot), vec!["done", "warm"]);
    assert!(
        !snapshot.worktree_groups[0].finished,
        "attention keeps a merged group hot"
    );
}

#[test]
fn pristine_landed_fork_stays_clean_until_work_is_finished() {
    let mut snapshot = ranked_snapshot(vec![
        agent_in("merged", "/repo/merged", AgentStatus::Success, 1_000),
        agent_in("pristine", "/repo/pristine", AgentStatus::Success, 2_000),
    ]);
    for group in &mut snapshot.worktree_groups {
        group.clean = Some(true);
        group.landed = Some(true);
        match group.label.as_str() {
            "merged" => group.trunk_sync = Some(WorktreeTrunkSync::Merged),
            "pristine" => group.trunk_sync = Some(WorktreeTrunkSync::Pristine),
            label => panic!("unexpected group {label}"),
        }
    }
    snapshot.sort_groups_for_presentation();

    assert_eq!(labels(&snapshot), vec!["pristine", "merged"]);
    assert!(!snapshot.worktree_groups[0].finished);
    assert!(snapshot.worktree_groups[1].finished);
}

#[test]
fn process_only_group_tails_calm_agent_groups() {
    let mut snapshot = ranked_snapshot(vec![
        agent_in("process", "/repo/process", AgentStatus::Idle, 1_000),
        agent_in("idle", "/repo/idle", AgentStatus::Idle, 2_000),
    ]);
    let process = snapshot
        .worktree_groups
        .iter_mut()
        .find(|group| group.label == "process")
        .expect("process group present");
    process.rows[0].card = RowCard::Process(crate::ProcessCard::default());
    process.status_counts.clear();
    snapshot.sort_groups_for_presentation();

    assert_eq!(labels(&snapshot), vec!["idle", "process"]);
}

#[test]
fn merged_pr_sinks_calm_worktree_group() {
    let mut snapshot = ranked_snapshot(vec![
        agent_in("done-idle", "/repo/done", AgentStatus::Idle, 1_000),
        agent_in("working-idle", "/repo/working", AgentStatus::Idle, 1_000),
    ]);
    for group in &mut snapshot.worktree_groups {
        group.clean = Some(true);
        group.landed = Some(false);
        match group.label.as_str() {
            "done" => group.pr_state = Some(WorktreePrState::Merged),
            "working" => {}
            label => panic!("unexpected group {label}"),
        }
    }
    snapshot.sort_groups_for_presentation();

    assert_eq!(labels(&snapshot), vec!["working", "done"]);
}

#[test]
fn closed_pr_sinks_calm_worktree_group() {
    let mut snapshot = ranked_snapshot(vec![
        agent_in("done-idle", "/repo/done", AgentStatus::Idle, 1_000),
        agent_in("working-idle", "/repo/working", AgentStatus::Idle, 1_000),
    ]);
    for group in &mut snapshot.worktree_groups {
        group.clean = Some(true);
        group.landed = Some(false);
        match group.label.as_str() {
            "done" => group.pr_state = Some(WorktreePrState::Closed),
            "working" => {}
            label => panic!("unexpected group {label}"),
        }
    }
    snapshot.sort_groups_for_presentation();

    assert_eq!(labels(&snapshot), vec!["working", "done"]);
}

#[test]
fn open_pr_does_not_sink_calm_worktree_group() {
    let mut snapshot = ranked_snapshot(vec![
        agent_in("open-idle", "/repo/open", AgentStatus::Idle, 1_000),
        agent_in("working-idle", "/repo/working", AgentStatus::Idle, 1_000),
        agent_in("done-idle", "/repo/done", AgentStatus::Idle, 1_000),
    ]);
    for group in &mut snapshot.worktree_groups {
        group.clean = Some(true);
        group.landed = Some(false);
        match group.label.as_str() {
            "open" => group.pr_state = Some(WorktreePrState::Open),
            "working" => {}
            "done" => group.pr_state = Some(WorktreePrState::Merged),
            label => panic!("unexpected group {label}"),
        }
    }
    snapshot.sort_groups_for_presentation();

    assert_eq!(labels(&snapshot), vec!["open", "working", "done"]);
}

#[test]
fn merged_pr_sinks_git_backed_channel_group() {
    let mut snapshot = ranked_snapshot(vec![
        agent_in("done-idle", "/repo/done-channel", AgentStatus::Idle, 1_000),
        agent_in(
            "working-idle",
            "/repo/working-channel",
            AgentStatus::Idle,
            1_000,
        ),
    ]);
    for group in &mut snapshot.worktree_groups {
        group.kind = SidebarWorktreeKind::Channel;
        group.clean = Some(true);
        group.landed = Some(false);
        match group.label.as_str() {
            "done-channel" => group.pr_state = Some(WorktreePrState::Merged),
            "working-channel" => {}
            label => panic!("unexpected group {label}"),
        }
    }
    snapshot.sort_groups_for_presentation();

    assert_eq!(labels(&snapshot), vec!["working-channel", "done-channel"]);
}

#[test]
fn attention_member_overrides_git_rung() {
    let mut snapshot = ranked_snapshot(vec![
        agent_in("dirty-idle", "/repo/dirty", AgentStatus::Idle, 1_000),
        agent_in("merged-wait", "/repo/merged", AgentStatus::Waiting, 1_000),
    ]);
    for group in &mut snapshot.worktree_groups {
        match group.label.as_str() {
            "dirty" => group.clean = Some(false),
            "merged" => {
                group.clean = Some(true);
                group.trunk_sync = Some(WorktreeTrunkSync::Merged);
            }
            label => panic!("unexpected group {label}"),
        }
    }
    snapshot.sort_groups_for_presentation();

    assert_eq!(
        labels(&snapshot),
        vec!["merged", "dirty"],
        "attention urgency beats the merged git rung"
    );
}

#[test]
fn external_group_tails_even_merged_project_work() {
    let mut snapshot = ranked_snapshot(vec![
        agent_in("merged-idle", "/repo/merged", AgentStatus::Idle, 1_000),
        agent("claude", "external-fail", AgentStatus::Failed, 1_000),
    ]);
    let merged = snapshot
        .worktree_groups
        .iter_mut()
        .find(|group| group.label == "merged")
        .expect("merged group present");
    merged.clean = Some(true);
    merged.trunk_sync = Some(WorktreeTrunkSync::Merged);
    snapshot.sort_groups_for_presentation();

    assert_eq!(
        labels(&snapshot),
        vec!["merged", "external"],
        "external stays the hard tail below even a calm merged project group"
    );
}
