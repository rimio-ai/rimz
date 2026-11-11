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

mod panes {
    use super::*;

    fn shell(raw: &str, birth_ms: i64) -> SidebarRow {
        let mut row = activity_row(false, None, at(birth_ms), Path::new("/repo"));
        row.id = raw.to_owned();
        let mut pane = crate::sidebar::test_support::pane(raw, "zsh", "/repo");
        pane.first_seen_at_ms = Some(birth_ms as u64);
        row.pane = Some(pane);
        row
    }

    fn pane_entry(stamps: &AttentionStamps, raw: &str) -> serde_json::Value {
        serde_json::to_value(&stamps.panes).unwrap()[format!("zellij:{raw}")].clone()
    }

    #[test]
    fn launch_inherits_only_the_same_process_incarnation_in_interim_and_final_folds() {
        let actual = [
            (Some(2_000), Some(2_000), false, 2_000),
            (None, Some(2_000), false, 2_000),
            (Some(2_000), None, false, 2_000),
            (Some(2_000), Some(4_000), false, 9_000),
            (Some(2_000), Some(2_000), true, 9_000),
            (None, None, false, 2_000),
        ]
        .map(|(stored_seen, current_seen, agent_tenant, expected)| {
            let mut stamps = AttentionStamps::default();
            let mut process = shell("terminal_1", 2_000);
            process.pane.as_mut().unwrap().pane_process_start = Some(at(2_000));
            process.pane.as_mut().unwrap().first_seen_at_ms = stored_seen;
            let mut snapshot = snapshot(vec![
                card("old", 1_000, AgentStatus::Idle),
                process.clone(),
                card("new", 3_000, AgentStatus::Idle),
            ]);
            reconcile(&mut stamps, &mut snapshot);
            if agent_tenant {
                stamps.panes.get_mut("zellij:terminal_1").unwrap().tenant =
                    PaneTenant::Agent { active_ms: None };
            }
            let mut agent = card("agent", 9_000, AgentStatus::Idle);
            agent.pane = process.pane;
            agent.pane.as_mut().unwrap().first_seen_at_ms = current_seen;
            *row_mut(&mut snapshot, "terminal_1") = agent;
            snapshot.now = at(12_000);
            stamps.derive(&mut snapshot);
            snapshot.sort_groups_for_presentation();
            let interim = (stamp(&snapshot, "agent"), order(&snapshot));
            reconcile(&mut stamps, &mut snapshot);
            let final_fold = (stamp(&snapshot, "agent"), order(&snapshot));
            snapshot.now = at(13_000);
            reconcile(&mut stamps, &mut snapshot);
            let later = (stamp(&snapshot, "agent"), order(&snapshot));
            let expected_order = if expected == 2_000 {
                vec!["new".to_owned(), "agent".to_owned(), "old".to_owned()]
            } else {
                vec!["agent".to_owned(), "new".to_owned(), "old".to_owned()]
            };
            (interim, final_fold, later, (Some(expected), expected_order))
        });
        assert_eq!(
            actual
                .clone()
                .map(|(interim, final_fold, later, _)| (interim, final_fold, later)),
            actual.map(|(_, _, _, expected)| (expected.clone(), expected.clone(), expected))
        );
    }

    #[test]
    fn inherited_launch_still_restamps_on_attention_crossings_and_new_panes_lead() {
        let mut stamps = AttentionStamps::default();
        let process = shell("terminal_1", 2_000);
        let asking = shell("terminal_2", 2_500);
        let mut snapshot = snapshot(vec![
            process.clone(),
            asking.clone(),
            card("old", 3_000, AgentStatus::Idle),
        ]);
        reconcile(&mut stamps, &mut snapshot);
        let mut agent = card("agent", 9_000, AgentStatus::Idle);
        agent.pane = process.pane;
        *row_mut(&mut snapshot, "terminal_1") = agent;
        let mut waiting = card("waiting", 9_500, AgentStatus::Waiting);
        waiting.pane = asking.pane;
        *row_mut(&mut snapshot, "terminal_2") = waiting;
        reconcile(&mut stamps, &mut snapshot);
        assert_eq!(
            [stamp(&snapshot, "agent"), stamp(&snapshot, "waiting")],
            [Some(2_000), Some(9_500)],
            "only a calm first sighting inherits the shell's slot"
        );
        row_mut(&mut snapshot, "agent")
            .as_agent_mut()
            .unwrap()
            .status = AgentStatus::Waiting;
        snapshot.now = at(12_000);
        reconcile(&mut stamps, &mut snapshot);
        assert_eq!(stamp(&snapshot, "agent"), Some(12_000));

        let mut fresh = card("fresh", 13_000, AgentStatus::Idle);
        fresh.pane = shell("terminal_3", 13_000).pane;
        Arc::make_mut(&mut Arc::make_mut(&mut snapshot.worktree_groups)[0].rows).push(fresh);
        snapshot.now = at(14_000);
        reconcile(&mut stamps, &mut snapshot);
        assert_eq!(stamp(&snapshot, "fresh"), Some(13_000));
        assert_eq!(order(&snapshot), ["agent", "waiting", "fresh", "old"]);
    }

    #[test]
    fn an_unbound_wrapper_row_leaves_the_shell_slot_to_the_bound_card() {
        let mut stamps = AttentionStamps::default();
        let process = shell("terminal_1", 2_000);
        let mut snapshot = snapshot(vec![
            card("old", 1_000, AgentStatus::Idle),
            process.clone(),
            card("new", 3_000, AgentStatus::Idle),
        ]);
        reconcile(&mut stamps, &mut snapshot);
        let mut unbound = card("zellij:terminal_1", 9_000, AgentStatus::Idle);
        unbound.pane = process.pane.clone();
        *row_mut(&mut snapshot, "terminal_1") = unbound;
        snapshot.now = at(11_000);
        reconcile(&mut stamps, &mut snapshot);
        assert_eq!(stamp(&snapshot, "zellij:terminal_1"), Some(2_000));
        assert!(!stamps.cards.contains_key("zellij:terminal_1"));
        assert_eq!(
            pane_entry(&stamps, "terminal_1"),
            serde_json::json!({
                "tenant": "process", "at_ms": 2_000, "active_ms": 2_000, "seen_ms": 2_000,
            })
        );

        let mut bound = card("bound", 9_000, AgentStatus::Idle);
        bound.pane = process.pane;
        *row_mut(&mut snapshot, "zellij:terminal_1") = bound;
        snapshot.now = at(12_000);
        stamps.derive(&mut snapshot);
        snapshot.sort_groups_for_presentation();
        let interim = (stamp(&snapshot, "bound"), order(&snapshot));
        reconcile(&mut stamps, &mut snapshot);
        let final_fold = (stamp(&snapshot, "bound"), order(&snapshot));
        let expected = (
            Some(2_000),
            vec!["new".to_owned(), "bound".to_owned(), "old".to_owned()],
        );
        assert_eq!([interim, final_fold], [expected.clone(), expected]);
    }

    #[test]
    fn pane_slot_records_birth_launch_and_exit_and_keeps_the_mixed_order() {
        let mut stamps = AttentionStamps::default();
        let process = shell("terminal_1", 2_000);
        let pane = process.pane.clone();
        let mut snapshot = snapshot(vec![
            card("old", 1_000, AgentStatus::Idle),
            process.clone(),
            card("new", 3_000, AgentStatus::Idle),
        ]);
        let mut entries = Vec::new();
        let mut positions = Vec::new();
        for (now_ms, tenant) in [
            (10_000, process.clone()),
            (12_000, {
                let mut agent = card("agent", 2_100, AgentStatus::Idle);
                agent.pane = pane.clone();
                agent
            }),
            (13_000, process),
        ] {
            snapshot.now = at(now_ms);
            let rows = Arc::make_mut(&mut Arc::make_mut(&mut snapshot.worktree_groups)[0].rows);
            let index = rows.iter().position(|row| row.pane == pane).unwrap();
            rows[index] = tenant;
            reconcile(&mut stamps, &mut snapshot);
            entries.push(pane_entry(&stamps, "terminal_1"));
            positions.push(snapshot.rows().position(|row| row.pane == pane).unwrap());
        }
        assert_eq!(positions, [1; 3]);
        assert_eq!(
            entries,
            [
                serde_json::json!({
                    "tenant": "process", "at_ms": 2_000, "active_ms": 2_000, "seen_ms": 2_000,
                }),
                serde_json::json!({"tenant": "agent", "at_ms": 2_000, "seen_ms": 2_000}),
                serde_json::json!({
                    "tenant": "process", "at_ms": 2_000, "active_ms": 13_000, "seen_ms": 2_000,
                }),
            ]
        );
        assert_eq!(snapshot.rows().nth(1).unwrap().last_activity, at(2_000));
        assert!(
            !stamps.reconcile(&snapshot),
            "a stable tenant writes nothing"
        );
    }

    #[test]
    fn a_new_pane_birth_replaces_either_kind_of_old_slot() {
        for entry in [
            serde_json::json!({"tenant": "agent", "at_ms": 1_000, "seen_ms": 1_000}),
            serde_json::json!({
                "tenant": "process", "at_ms": 1_000, "active_ms": 500, "seen_ms": 1_000,
            }),
        ] {
            let mut stamps = AttentionStamps::default();
            stamps.panes.insert(
                "zellij:terminal_1".into(),
                serde_json::from_value(entry).unwrap(),
            );
            let mut snapshot = snapshot(vec![shell("terminal_1", 2_000)]);
            stamps.derive(&mut snapshot);
            assert_eq!(stamp(&snapshot, "terminal_1"), Some(2_000));
            reconcile(&mut stamps, &mut snapshot);
            assert_eq!(
                pane_entry(&stamps, "terminal_1"),
                serde_json::json!({
                    "tenant": "process", "at_ms": 2_000, "active_ms": 2_000, "seen_ms": 2_000,
                })
            );
        }
    }

    #[test]
    fn a_resumed_agent_exit_keeps_its_stamp_and_index_despite_a_later_pane_birth() {
        let process = shell("terminal_1", 5_000);
        let mut agent = card("resumed", 1_000, AgentStatus::Idle);
        agent.pane = process.pane.clone();
        let mut snapshot = snapshot(vec![
            card("older", 500, AgentStatus::Idle),
            agent,
            card("newer", 2_000, AgentStatus::Idle),
        ]);
        let mut stamps = AttentionStamps::default();
        reconcile(&mut stamps, &mut snapshot);
        assert_eq!(order(&snapshot), ["newer", "resumed", "older"]);
        *row_mut(&mut snapshot, "resumed") = process;
        snapshot.now = at(12_000);
        let before = stamps.panes.clone();
        stamps.derive(&mut snapshot);
        snapshot.sort_groups_for_presentation();
        let interim = (stamp(&snapshot, "terminal_1"), order(&snapshot));
        assert_eq!(stamps.panes, before, "derive does not persist the exit");
        reconcile(&mut stamps, &mut snapshot);
        let expected = (
            Some(1_000),
            vec![
                "newer".to_owned(),
                "terminal_1".to_owned(),
                "older".to_owned(),
            ],
        );
        assert_eq!(
            [interim, (stamp(&snapshot, "terminal_1"), order(&snapshot))],
            [expected.clone(), expected]
        );
        assert_eq!(
            pane_entry(&stamps, "terminal_1"),
            serde_json::json!({
                "tenant": "process", "at_ms": 1_000, "active_ms": 12_000, "seen_ms": 5_000,
            })
        );
    }

    #[test]
    fn pane_sightings_replace_only_known_different_incarnations_not_older_stamps() {
        let mut actual = Vec::new();
        let mut expected = Vec::new();
        for (seen_ms, current_seen_ms, at_ms, expected_at_ms) in [
            (Some(2_000), Some(2_000), 1_000, 1_000),
            (Some(1_000), Some(2_000), 9_000, 2_000),
            (None, Some(2_000), 1_000, 1_000),
            (Some(1_000), None, 1_000, 1_000),
        ] {
            for agent in [false, true] {
                let mut entry = if agent {
                    serde_json::json!({"tenant": "agent", "at_ms": at_ms})
                } else {
                    serde_json::json!({"tenant": "process", "at_ms": at_ms, "active_ms": 700})
                };
                if let Some(seen_ms) = seen_ms {
                    entry["seen_ms"] = serde_json::json!(seen_ms);
                }
                let mut stamps = AttentionStamps::default();
                stamps.panes.insert(
                    "zellij:terminal_1".into(),
                    serde_json::from_value(entry).unwrap(),
                );
                let mut process = shell("terminal_1", 2_000);
                process.pane.as_mut().unwrap().first_seen_at_ms = current_seen_ms;
                let mut snapshot = snapshot(vec![process]);
                stamps.derive(&mut snapshot);
                let derived = stamp(&snapshot, "terminal_1");
                reconcile(&mut stamps, &mut snapshot);
                let recorded = pane_entry(&stamps, "terminal_1");
                actual.push((
                    derived,
                    recorded["at_ms"].as_i64(),
                    recorded["active_ms"].as_i64(),
                    recorded["seen_ms"].as_u64(),
                ));
                let replaced = matches!((seen_ms, current_seen_ms), (Some(a), Some(b)) if a != b);
                expected.push((
                    Some(expected_at_ms),
                    Some(expected_at_ms),
                    Some(if replaced {
                        2_000
                    } else if agent {
                        10_000
                    } else {
                        700
                    }),
                    current_seen_ms.or(seen_ms),
                ));
            }
        }
        assert_eq!(actual, expected);
    }

    #[test]
    fn agent_exit_keeps_hot_cooling_and_archived_bands_and_positions() {
        let mut actual = Vec::new();
        let mut expected = Vec::new();
        let facts = |snapshot: &SidebarSnapshot| {
            let (index, row) = snapshot
                .rows()
                .enumerate()
                .find(|(_, row)| row.pane.is_some())
                .unwrap();
            (row.attention_at_ms, row.inactive, row.archived, index)
        };
        for (active_ms, inactive, archived, index) in [
            (9_500, false, false, 0),
            (8_000, true, false, 1),
            (7_000, true, true, 1),
        ] {
            let process = shell("terminal_1", 2_000);
            let mut agent = card("agent", 2_100, AgentStatus::Idle);
            agent.pane = process.pane.clone();
            agent.last_activity = at(active_ms);
            let mut older = card("archived", 500, AgentStatus::Idle);
            older.last_activity = at(6_000);
            let mut snapshot = snapshot(vec![card("hot", 1_000, AgentStatus::Idle), agent, older]);
            snapshot.attention.inactive_after_secs = std::num::NonZeroU32::new(1).unwrap();
            snapshot.attention.archive_after_secs = std::num::NonZeroU32::new(2).unwrap();
            snapshot.score_scenario_rows();
            let mut stamps = AttentionStamps::default();
            reconcile(&mut stamps, &mut snapshot);
            let before = facts(&snapshot);
            let agent_active = pane_entry(&stamps, "terminal_1")["active_ms"].as_i64();
            *row_mut(&mut snapshot, "agent") = process;
            snapshot.now = at(10_500);
            let interim_and_final = [false, true].map(|persist| {
                if persist {
                    stamps.reconcile(&snapshot);
                }
                stamps.derive(&mut snapshot);
                snapshot.sort_groups_for_presentation();
                facts(&snapshot)
            });
            let exit_active = pane_entry(&stamps, "terminal_1")["active_ms"].as_i64();
            snapshot.now = at(10_600);
            stamps.derive(&mut snapshot);
            snapshot.sort_groups_for_presentation();
            actual.push((
                before,
                interim_and_final,
                facts(&snapshot),
                agent_active,
                exit_active,
            ));
            let expected_facts = (Some(2_100), inactive, archived, index);
            expected.push((
                expected_facts,
                [expected_facts; 2],
                expected_facts,
                (inactive || archived).then_some(active_ms),
                Some(if inactive || archived {
                    active_ms
                } else {
                    10_500
                }),
            ));
        }
        assert_eq!(actual, expected);
    }

    #[test]
    fn legacy_attention_files_load_cards_and_gain_pane_slots() {
        let mut stamps = AttentionStamps::from_bytes(
            br#"{
        "v": "rimz.attention.v1", "cards": {"known": {"zone": "calm", "at_ms": 777}}
    }"#,
        );
        let mut snapshot = snapshot(vec![
            card("known", 1_000, AgentStatus::Idle),
            shell("terminal_1", 2_000),
        ]);
        reconcile(&mut stamps, &mut snapshot);
        assert_eq!(stamp(&snapshot, "known"), Some(777));
        assert_eq!(
            pane_entry(&stamps, "terminal_1"),
            serde_json::json!({
                "tenant": "process", "at_ms": 2_000, "active_ms": 2_000, "seen_ms": 2_000,
            })
        );
    }

    #[test]
    fn interim_exit_derives_the_next_slot_and_ages_busy_processes_from_exit() {
        let mut stamps = AttentionStamps::default();
        stamps.panes.insert(
            "zellij:terminal_1".into(),
            serde_json::from_value(serde_json::json!({"tenant": "agent", "at_ms": 2_100})).unwrap(),
        );
        let before = stamps.panes.clone();
        let mut process = shell("terminal_1", 2_000);
        process.inactive = true;
        process.archived = true;
        process.as_process_mut().unwrap().state = crate::store::snapshot::ProcessState::Busy;
        let mut final_fold = snapshot(vec![process]);
        final_fold.attention.inactive_after_secs = std::num::NonZeroU32::new(1).unwrap();
        final_fold.attention.archive_after_secs = std::num::NonZeroU32::new(2).unwrap();
        let mut interim = final_fold.clone();
        stamps.derive(&mut interim);
        let row = interim.rows().next().unwrap();
        assert_eq!(
            (row.attention_at_ms, row.inactive, row.archived),
            (Some(2_100), false, false)
        );
        assert_eq!(stamps.panes, before, "derive never persists an exit");
        reconcile(&mut stamps, &mut final_fold);
        assert_eq!(
            interim.worktree_groups[0].rows,
            final_fold.worktree_groups[0].rows
        );
        assert_eq!(
            pane_entry(&stamps, "terminal_1"),
            serde_json::json!({
                "tenant": "process", "at_ms": 2_100, "active_ms": 10_000, "seen_ms": 2_000,
            })
        );
        let actual = [11_000, 12_000, 13_000].map(|now_ms| {
            final_fold.now = at(now_ms);
            stamps.derive(&mut final_fold);
            let row = final_fold.rows().next().unwrap();
            (
                row.inactive,
                row.archived,
                row.attention_score,
                row.last_activity,
            )
        });
        assert_eq!(
            actual,
            [
                (false, false, 0, at(2_000)),
                (true, false, 0, at(2_000)),
                (true, true, 0, at(2_000)),
            ]
        );
    }

    #[test]
    fn panes_without_birth_keep_existing_slots_and_new_slots_use_the_fold_clock() {
        for entry in [
            None,
            Some(serde_json::json!({"tenant": "agent", "at_ms": 1_000})),
            Some(serde_json::json!({"tenant": "process", "at_ms": 1_000, "active_ms": 2_000})),
        ] {
            let mut stamps = AttentionStamps::default();
            if let Some(entry) = &entry {
                stamps.panes.insert(
                    "zellij:terminal_1".into(),
                    serde_json::from_value(entry.clone()).unwrap(),
                );
            }
            let mut process = shell("terminal_1", 10_000);
            process.pane.as_mut().unwrap().first_seen_at_ms = None;
            let mut snapshot = snapshot(vec![process]);
            reconcile(&mut stamps, &mut snapshot);
            let at_ms = entry
                .as_ref()
                .map_or(10_000, |entry| entry["at_ms"].as_i64().unwrap());
            let active_ms = entry
                .as_ref()
                .and_then(|entry| entry.get("active_ms"))
                .map_or(10_000, |value| value.as_i64().unwrap());
            let expected = serde_json::json!({
                "tenant": "process", "at_ms": at_ms, "active_ms": active_ms,
            });
            assert_eq!(pane_entry(&stamps, "terminal_1"), expected);
            snapshot.now = at(12_000);
            row_mut(&mut snapshot, "terminal_1").last_activity = snapshot.now;
            assert!(!stamps.reconcile(&snapshot));
            assert_eq!(pane_entry(&stamps, "terminal_1"), expected);
        }
    }

    #[test]
    fn absent_pane_slots_prune_strictly_past_retention_and_present_panes_stay() {
        let mut stamps = AttentionStamps::default();
        for (id, at_ms) in [("old", 999), ("boundary", 1_000), ("present", 999)] {
            stamps.panes.insert(
                format!("zellij:{id}"),
                serde_json::from_value(
                    serde_json::json!({"tenant": "process", "at_ms": at_ms, "active_ms": at_ms}),
                )
                .unwrap(),
            );
        }
        let mut snapshot = snapshot(vec![shell("present", 0)]);
        snapshot.now = at(1_000 + DEFAULT_RETENTION.as_millis() as i64);
        reconcile(&mut stamps, &mut snapshot);
        assert_eq!(
            stamps.panes.keys().map(String::as_str).collect::<Vec<_>>(),
            ["zellij:boundary", "zellij:present"]
        );
    }

    #[test]
    fn pane_slots_load_without_resetting_exit_activity() {
        let mut stamps = AttentionStamps::from_bytes(
            br#"{
        "v": "rimz.attention.v1", "cards": {}, "panes": {
            "zellij:terminal_1": {"tenant": "process", "at_ms": 2100, "active_ms": 9000}
        }
    }"#,
        );
        let mut snapshot = snapshot(vec![shell("terminal_1", 2_000)]);
        stamps.derive(&mut snapshot);
        assert_eq!(stamp(&snapshot, "terminal_1"), Some(2_100));
        assert_eq!(
            pane_entry(&stamps, "terminal_1"),
            serde_json::json!({
                "tenant": "process", "at_ms": 2_100, "active_ms": 9_000,
            })
        );
        assert!(
            stamps.reconcile(&snapshot),
            "a legacy slot gains its sighting"
        );
        assert_eq!(
            pane_entry(&stamps, "terminal_1"),
            serde_json::json!({
                "tenant": "process", "at_ms": 2_100, "active_ms": 9_000, "seen_ms": 2_000,
            })
        );
        assert!(!stamps.reconcile(&snapshot));
    }
}

#[test]
fn agent_slots_write_activity_on_cooling_and_clear_it_on_new_activity() {
    let mut agent = card("agent", 2_100, AgentStatus::Idle);
    agent.pane = Some(crate::sidebar::test_support::pane(
        "terminal_1",
        "claude",
        "/repo",
    ));
    let mut snapshot = snapshot(vec![agent]);
    snapshot.attention.inactive_after_secs = std::num::NonZeroU32::new(1).unwrap();
    snapshot.attention.archive_after_secs = std::num::NonZeroU32::new(2).unwrap();
    let mut stamps = AttentionStamps::default();
    let actual = [
        (10_000, 10_000),
        (12_000, 10_000),
        (13_000, 10_000),
        (14_000, 14_000),
    ]
    .map(|(now_ms, active_ms)| {
        snapshot.now = at(now_ms);
        row_mut(&mut snapshot, "agent").last_activity = at(active_ms);
        snapshot.score_scenario_rows();
        let changed = stamps.reconcile(&snapshot);
        let entry = serde_json::to_value(&stamps.panes).unwrap();
        (
            changed,
            entry["zellij:terminal_1"]["active_ms"].as_i64(),
            snapshot.rows().next().unwrap().inactive,
            snapshot.rows().next().unwrap().archived,
        )
    });
    assert_eq!(
        actual,
        [
            (true, None, false, false),
            (true, Some(10_000), true, false),
            (false, Some(10_000), true, true),
            (true, None, false, false),
        ]
    );
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
            compacting: false,
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
