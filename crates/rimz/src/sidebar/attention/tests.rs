use super::*;

use std::path::Path;
use std::sync::Arc;

use crate::agents::AgentStatus;
use crate::sidebar::test_support::{activity_row, worktree_group};
use crate::store::snapshot::{SidebarRow, group_live_agents_by_worktree};

fn at(ms: i64) -> jiff::Timestamp {
    jiff::Timestamp::from_millisecond(ms).unwrap()
}

fn card(id: &str, birth_ms: i64, status: AgentStatus) -> SidebarRow {
    let mut row = activity_row(true, Some(status), at(10_000), Path::new("/repo"));
    row.id = id.to_owned();
    row.as_agent_mut().unwrap().registered_at = Some(at(birth_ms));
    row
}

fn snapshot(rows: Vec<SidebarRow>) -> SidebarSnapshot {
    let workspace = crate::ids::WorkspaceId::from_project_root(Path::new("/repo"));
    let mut snapshot = SidebarSnapshot::build_with_agents(workspace, Vec::new(), at(10_000));
    snapshot.panes_produced_at_ms = Some(1);
    snapshot.worktree_groups = vec![worktree_group(Path::new("/repo"), rows)].into();
    snapshot
}

fn row_mut<'a>(snapshot: &'a mut SidebarSnapshot, id: &str) -> &'a mut SidebarRow {
    Arc::make_mut(&mut Arc::make_mut(&mut snapshot.worktree_groups)[0].rows)
        .iter_mut()
        .find(|row| row.id == id)
        .unwrap()
}

fn order(snapshot: &SidebarSnapshot) -> Vec<String> {
    snapshot.rows().map(|row| row.id.clone()).collect()
}

fn stamp(snapshot: &SidebarSnapshot, id: &str) -> Option<i64> {
    snapshot.rows().find(|row| row.id == id)?.attention_at_ms
}

fn reconcile(stamps: &mut AttentionStamps, snapshot: &mut SidebarSnapshot) {
    stamps.reconcile(snapshot);
    stamps.derive(snapshot);
    snapshot.sort_groups_for_presentation();
}

#[test]
fn only_zone_crossings_restamp_not_prompts_results_parks_or_read_state() {
    let mut stamps = AttentionStamps::default();
    let mut snapshot = snapshot(vec![card("a", 1_000, AgentStatus::Idle)]);
    let mut actual = Vec::new();
    for (index, status) in [
        AgentStatus::Idle,
        AgentStatus::Running,
        AgentStatus::Success,
        AgentStatus::Paused,
        AgentStatus::Idle,
        AgentStatus::Waiting,
        AgentStatus::Failed,
        AgentStatus::Waiting,
        AgentStatus::Running,
    ]
    .into_iter()
    .enumerate()
    {
        snapshot.now = at(10_000 + index as i64 * 1_000);
        let row = row_mut(&mut snapshot, "a");
        row.last_activity = at(10_000 + index as i64 * 1_000);
        row.unread = index % 2 == 0;
        row.as_agent_mut().unwrap().status = status;
        reconcile(&mut stamps, &mut snapshot);
        actual.push(stamp(&snapshot, "a"));
    }
    assert_eq!(
        actual,
        [
            1_000, 1_000, 1_000, 1_000, 1_000, 15_000, 15_000, 15_000, 18_000
        ]
        .map(Some)
    );
}

#[test]
fn answering_three_asks_oldest_first_keeps_every_row_still() {
    let mut stamps = AttentionStamps::default();
    let mut snapshot = snapshot(vec![
        card("a", 1_000, AgentStatus::Waiting),
        card("b", 2_000, AgentStatus::Waiting),
        card("c", 3_000, AgentStatus::Waiting),
    ]);
    reconcile(&mut stamps, &mut snapshot);
    let mut orders = vec![order(&snapshot)];
    for (index, id) in ["a", "b", "c"].into_iter().enumerate() {
        snapshot.now = at(20_000 + index as i64 * 1_000);
        row_mut(&mut snapshot, id).as_agent_mut().unwrap().status = AgentStatus::Running;
        reconcile(&mut stamps, &mut snapshot);
        orders.push(order(&snapshot));
    }
    assert_eq!(orders, vec![vec!["c", "b", "a"]; 4]);
}

#[test]
fn unreconciled_folds_rank_pending_crossings_as_the_next_reconcile_does() {
    let mut stamps = AttentionStamps::default();
    let mut snapshot = snapshot(vec![
        card("old", 1_000, AgentStatus::Idle),
        card("asker", 2_000, AgentStatus::Waiting),
        card("young", 3_000, AgentStatus::Idle),
    ]);
    reconcile(&mut stamps, &mut snapshot);
    let mut interim_orders = Vec::new();
    let mut final_orders = Vec::new();
    for (index, (id, status)) in [
        ("old", AgentStatus::Waiting),
        ("asker", AgentStatus::Running),
        ("fresh", AgentStatus::Waiting),
    ]
    .into_iter()
    .enumerate()
    {
        snapshot.now = at(20_000 + index as i64 * 1_000);
        if id == "fresh" {
            Arc::make_mut(&mut Arc::make_mut(&mut snapshot.worktree_groups)[0].rows)
                .push(card(id, 4_000, status));
        } else {
            row_mut(&mut snapshot, id).as_agent_mut().unwrap().status = status;
        }
        let mut interim = snapshot.clone();
        stamps.derive(&mut interim);
        interim.sort_groups_for_presentation();
        interim_orders.push(order(&interim));
        reconcile(&mut stamps, &mut snapshot);
        final_orders.push(order(&snapshot));
    }
    assert_eq!(interim_orders, final_orders);
    assert_eq!(
        final_orders,
        [
            vec!["old", "asker", "young"],
            vec!["old", "asker", "young"],
            vec!["old", "fresh", "asker", "young"],
        ]
    );
}

#[test]
fn birth_and_recovery_lead_calm_without_churning_other_cards() {
    let mut stamps = AttentionStamps::default();
    let mut snapshot = snapshot(vec![
        card("old", 1_000, AgentStatus::Running),
        card("new", 2_000, AgentStatus::Idle),
    ]);
    reconcile(&mut stamps, &mut snapshot);
    Arc::make_mut(&mut Arc::make_mut(&mut snapshot.worktree_groups)[0].rows).push(card(
        "birth",
        9_000,
        AgentStatus::Idle,
    ));
    reconcile(&mut stamps, &mut snapshot);
    let born = order(&snapshot);
    row_mut(&mut snapshot, "old").as_agent_mut().unwrap().status = AgentStatus::Failed;
    reconcile(&mut stamps, &mut snapshot);
    snapshot.now = at(11_000);
    row_mut(&mut snapshot, "old").as_agent_mut().unwrap().status = AgentStatus::Idle;
    reconcile(&mut stamps, &mut snapshot);
    assert_eq!(
        (born, order(&snapshot)),
        (
            ["birth", "new", "old"].map(String::from).to_vec(),
            ["old", "birth", "new"].map(String::from).to_vec()
        )
    );
}

#[test]
fn displayed_child_attention_and_missing_birth_use_the_snapshot_clock() {
    let mut stamps = AttentionStamps::default();
    let mut row = card("parent", 1_000, AgentStatus::Running);
    row.as_agent_mut().unwrap().registered_at = None;
    let mut snapshot = snapshot(vec![row]);
    reconcile(&mut stamps, &mut snapshot);
    let birth = stamp(&snapshot, "parent");
    snapshot.now = at(12_000);
    row_mut(&mut snapshot, "parent")
        .as_agent_mut()
        .unwrap()
        .sub_agents
        .push(crate::store::snapshot::SidebarSubAgent {
            id: "child".into(),
            kind: crate::ids::AgentKind::new_unchecked("claude"),
            pane: None,
            prior_turn: false,
            name: "child".into(),
            petname: None,
            provider_native: true,
            status: AgentStatus::Waiting,
            stalled: false,
            phase: crate::agents::TurnPhase::Idle,
            turn_error_label: None,
            task: None,
            first_prompt: None,
            profile: None,
            model: None,
            fast: false,
            effort: None,
            description: None,
            tokens: None,
            context_window: None,
            compaction_count: 0,
            cost_usd: None,
            elapsed_secs: None,
            started_at: None,
            finished_at: None,
            last_activity: at(12_000),
            registered_at: Some(at(12_000)),
        });
    reconcile(&mut stamps, &mut snapshot);
    assert_eq!(
        (birth, stamp(&snapshot, "parent")),
        (Some(10_000), Some(12_000))
    );
}

#[test]
fn launch_stamp_survives_clear_compaction_and_absence_before_resume() {
    let mut stamps = AttentionStamps::default();
    let mut snapshot = snapshot(vec![card("original", 1_000, AgentStatus::Idle)]);
    let mut agent = crate::testkit::agent_state("claude", "original", at(1_000));
    agent.launch_id = Some("launch_same".into());
    snapshot.agents = vec![agent].into();
    reconcile(&mut stamps, &mut snapshot);
    snapshot.now = at(12_000);
    row_mut(&mut snapshot, "original")
        .as_agent_mut()
        .unwrap()
        .status = AgentStatus::Waiting;
    reconcile(&mut stamps, &mut snapshot);
    let mut actual = vec![stamp(&snapshot, "original")];
    for (index, id) in ["cleared", "compacted", "resumed"].into_iter().enumerate() {
        let mut agent = crate::testkit::agent_state("claude", id, at(13_000 + index as i64));
        agent.launch_id = Some("launch_same".into());
        snapshot.agents = vec![agent].into();
        Arc::make_mut(&mut snapshot.worktree_groups)[0].rows =
            vec![card(id, 13_000 + index as i64, AgentStatus::Waiting)].into();
        reconcile(&mut stamps, &mut snapshot);
        actual.push(stamp(&snapshot, id));
        let mut absent = snapshot.clone();
        absent.worktree_groups = Default::default();
        reconcile(&mut stamps, &mut absent);
    }
    assert_eq!(actual, [Some(12_000); 4]);
}

#[test]
fn frameless_folds_neither_open_nor_restamp_nor_prune() {
    let mut stamps = AttentionStamps::default();
    let mut snapshot = snapshot(vec![card("a", 1_000, AgentStatus::Idle)]);
    reconcile(&mut stamps, &mut snapshot);
    let before = stamps.cards.clone();
    snapshot.panes_produced_at_ms = None;
    snapshot.now = at(2_000_000_000);
    Arc::make_mut(&mut snapshot.worktree_groups)[0].rows =
        vec![card("b", 2_000, AgentStatus::Waiting)].into();
    reconcile(&mut stamps, &mut snapshot);
    assert_eq!((before.len(), &stamps.cards), (1, &before));
}

#[test]
fn absent_stamps_prune_strictly_past_retention_but_present_cards_stay() {
    let mut stamps = AttentionStamps::default();
    let mut snapshot = snapshot(vec![card("present", 1_000, AgentStatus::Idle)]);
    snapshot.now = at(1_000 + DEFAULT_RETENTION.as_millis() as i64);
    for (id, at_ms) in [("old", 999), ("boundary", 1_000), ("present", 999)] {
        stamps.cards.insert(
            id.into(),
            CardStamp {
                zone: Zone::Calm,
                at_ms,
            },
        );
    }
    reconcile(&mut stamps, &mut snapshot);
    assert_eq!(
        stamps.cards.keys().map(String::as_str).collect::<Vec<_>>(),
        ["boundary", "present"]
    );
}

#[test]
fn roster_uses_derived_stamps_instead_of_birth_order() {
    let mut stamps = AttentionStamps::default();
    let mut snapshot = snapshot(vec![
        card("old", 1_000, AgentStatus::Idle),
        card("new", 2_000, AgentStatus::Idle),
    ]);
    snapshot.agents = ["old", "new"]
        .map(|id| {
            let mut agent = crate::testkit::agent_state(
                "claude",
                id,
                at(if id == "old" { 1_000 } else { 2_000 }),
            );
            agent.status = AgentStatus::Idle;
            agent.worktree_path = Some("/repo".into());
            agent
        })
        .to_vec()
        .into();
    reconcile(&mut stamps, &mut snapshot);
    row_mut(&mut snapshot, "old").as_agent_mut().unwrap().status = AgentStatus::Waiting;
    reconcile(&mut stamps, &mut snapshot);
    row_mut(&mut snapshot, "old").as_agent_mut().unwrap().status = AgentStatus::Idle;
    snapshot.now = at(12_000);
    reconcile(&mut stamps, &mut snapshot);
    let agents = snapshot.agents.iter().collect::<Vec<_>>();
    let roster = group_live_agents_by_worktree(&agents, &snapshot);
    assert_eq!(
        (
            roster[0]
                .agents
                .iter()
                .map(|agent| agent.agent_id.as_str())
                .collect::<Vec<_>>(),
            order(&snapshot)
        ),
        (
            vec!["old", "new"],
            ["old", "new"].map(String::from).to_vec()
        )
    );
}

#[test]
fn codec_accepts_v1_and_treats_garbage_or_unknown_versions_as_absent() {
    let actual = [
        br#"{"v":"rimz.attention.v1","cards":{"a":{"zone":"top","at_ms":1000}}}"#.as_slice(),
        br#"{"v":"rimz.attention.v2","cards":{"a":{"zone":"top","at_ms":1000}}}"#.as_slice(),
        b"not json".as_slice(),
        br#"{"v":"rimz.attention.v1","cards":{"a":{"zone":"unknown","at_ms":1000}}}"#.as_slice(),
    ]
    .map(|bytes| AttentionStamps::from_bytes(bytes).cards.get("a").cloned());
    assert_eq!(
        actual,
        [
            Some(CardStamp {
                zone: Zone::Top,
                at_ms: 1000
            }),
            None,
            None,
            None
        ]
    );
}

#[test]
fn runtime_record_opens_at_birth_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(
        crate::ids::WorkspaceId::from_project_root(dir.path()),
        dir.path(),
    )
    .unwrap();
    runtime.ensure_dirs().unwrap();
    let mut stamps = AttentionStamps::load(&runtime);
    let mut snapshot = snapshot(vec![card("a", 1_000, AgentStatus::Waiting)]);
    reconcile(&mut stamps, &mut snapshot);
    stamps.persist(&runtime).unwrap();
    assert_eq!(
        AttentionStamps::load(&runtime).cards.get("a"),
        Some(&CardStamp {
            zone: Zone::Top,
            at_ms: 1_000
        })
    );
}
