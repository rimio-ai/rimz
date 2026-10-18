use super::*;
use crate::sidebar::timing::REORDER_HOLD;
use std::sync::mpsc::{Receiver, sync_channel};

fn watched_row() -> (Rig, Receiver<ObserveMsg>, PaneId, Instant) {
    let mut rig = Rig::new();
    rig.state.current = agent_snapshot(&rig.ws);
    let row = rig.state.current.rows().next().unwrap();
    let pane = row.pane.as_ref().unwrap().pane_id.clone();
    rig.state.ui.painted_rows = vec![(row.id.clone(), -1)];
    let (tx, rx) = sync_channel(1);
    rig.state.observe_tx = tx;
    let now = Instant::now();
    rig.state.arm_clicked_row(&pane, now);
    (rig, rx, pane, now)
}

fn displacement(rx: &Receiver<ObserveMsg>, renderer: &SidebarInstanceId) -> observe::AnomalyDraft {
    let message = rx.try_recv();
    assert!(
        message.is_ok(),
        "a displaced click must reach OwnAnomaly: {message:?}"
    );
    let ObserveMsg::OwnAnomaly {
        draft,
        renderer: actual,
    } = message.unwrap()
    else {
        panic!("renderer displacement must use OwnAnomaly");
    };
    assert_eq!(&actual, renderer);
    *draft
}

fn landing(viewport_row: Option<isize>, local_click: bool) -> (Rig, Receiver<ObserveMsg>, String) {
    let (mut rig, _, pane, _) = watched_row();
    let (tx, rx) = sync_channel(8);
    rig.state.observe_tx = tx;
    rig.state.clicked_row = None;
    rig.state.ui.selected_index = Some(0);
    rig.state.ui.selected_pane = Some(pane.clone());
    rig.state.dirty = true;
    rig.state
        .paint_now(&mut rig.terminal, Instant::now())
        .unwrap();
    let (id, y) = rig.state.ui.painted_rows[0].clone();
    if local_click {
        rig.state
            .apply_input(
                [Wakeup::MouseClick {
                    column: 2,
                    row: y as u16,
                }],
                &mut rig.terminal,
            )
            .unwrap();
        assert!(rig.state.clicked_row.is_some());
    }
    let anchor = serde_json::from_value(serde_json::json!({
        "nonce": crate::ids::FocusNonce::new(),
        "session_name": "rimz-test",
        "pane_id": pane,
        "origin": "user",
        "issued_at_ms": crate::utils::time::unix_now_ms(),
        "state": "requested",
        "placement": {
            "row_id": id,
            "viewport_row": viewport_row.unwrap_or(y - rig.state.ui.scroll_zone_top as isize),
        },
    }))
    .unwrap();
    crate::mux::focus_anchor::store(&rig.runtime, &anchor).unwrap();
    rig.state.apply_focus_anchor();
    assert!(rig.state.ui.focus_placement.is_some());
    (rig, rx, id)
}

#[test]
fn displaced_landing_records_the_requested_row_once_and_watches_later_paints() {
    let (mut rig, rx, id) = landing(Some(1_000), false);
    rig.state.ui.banner_shown = !rig.state.ui.banner_shown;
    rig.state.dirty = true;
    rig.state
        .paint_now(&mut rig.terminal, Instant::now())
        .unwrap();
    let wire = serde_json::to_value(displacement(&rx, &rig.state.config.instance_id).kind).unwrap();
    let y = rig
        .state
        .ui
        .painted_rows
        .iter()
        .find(|(row, _)| row == &id)
        .unwrap()
        .1;
    assert_eq!(wire["row_id"], id);
    assert_eq!(wire["from_y"], rig.state.ui.scroll_zone_top + 1_000);
    assert_eq!(wire["to_y"], y);
    assert_eq!(wire["cause"], "no_room_clamp");
    assert!(rx.try_recv().is_err(), "one landing record");

    rig.state.ui.anchor_clamped = false;
    rig.state
        .ui
        .painted_rows
        .iter_mut()
        .find(|(row, _)| row == &id)
        .unwrap()
        .1 += 1;
    rig.state.observe_paint(Instant::now());
    let wire = serde_json::to_value(displacement(&rx, &rig.state.config.instance_id).kind).unwrap();
    assert_eq!(wire["from_y"], y);
    assert_eq!(wire["to_y"], y + 1);
    assert_eq!(wire["cause"], "height_above_changed");
}

#[test]
fn exact_landing_emits_no_displacement() {
    let (mut rig, rx, _) = landing(None, false);
    rig.state.dirty = true;
    rig.state
        .paint_now(&mut rig.terminal, Instant::now())
        .unwrap();
    assert!(rig.state.ui.focus_placement.is_none());
    assert!(rx.try_recv().is_err());
}

#[test]
fn same_tab_landing_keeps_the_local_click_watch_without_a_second_record() {
    let (mut rig, rx, id) = landing(Some(1_000), true);
    let clicked_at = rig.state.clicked_row.as_ref().unwrap().clicked_at;
    // Exercise a real landing paint with a local watch already armed for this card.
    rig.state.ui.painted_rows[0].1 -= 1;
    rig.state.clicked_row.as_mut().unwrap().y -= 1;
    rig.state.dirty = true;
    rig.state
        .paint_now(&mut rig.terminal, Instant::now())
        .unwrap();
    let wire = serde_json::to_value(displacement(&rx, &rig.state.config.instance_id).kind).unwrap();
    assert_eq!(wire["row_id"], id);
    assert_eq!(
        rig.state.clicked_row.as_ref().unwrap().clicked_at,
        clicked_at
    );
    assert!(
        rx.try_recv().is_err(),
        "the landing must not double the local detector"
    );
}

#[test]
fn missing_landing_row_records_null_from_the_post_paint_zone() {
    let (mut rig, rx, id) = landing(Some(3), false);
    rig.state.ui.focus_placement = None;
    rig.state.ui.drawn_selection = Some(id);
    rig.state.ui.painted_rows.clear();
    rig.state.ui.scroll_zone_top = 7;
    rig.state.ui.banner_shown = !rig.state.ui.banner_shown;
    rig.state.hold_expired_since_paint = true;
    rig.state
        .observe_paint(Instant::now() + Duration::from_millis(120));
    let wire = serde_json::to_value(displacement(&rx, &rig.state.config.instance_id).kind).unwrap();
    assert_eq!(wire["from_y"], 10);
    assert_eq!(wire["to_y"], serde_json::Value::Null);
    assert_eq!(wire["cause"], "height_above_changed");
    assert!(wire["dt_ms"].as_u64().unwrap() >= 120);
    assert!(rig.state.clicked_row.is_none());
}

#[test]
fn clicked_row_emits_each_cause_in_priority_order_and_rebaselines() {
    for cause in [
        "no_room_clamp",
        "hold_expired",
        "banner_toggled",
        "selection_changed",
        "height_above_changed",
    ] {
        let (mut rig, rx, _, now) = watched_row();
        // Lower-priority changes deliberately coincide with each higher one.
        rig.state.ui.anchor_clamped = cause == "no_room_clamp";
        if matches!(cause, "no_room_clamp" | "hold_expired") {
            super::super::super::order_hold::arm_order_hold(&mut rig.state.ui, 0);
            let prev = rig.state.current.clone();
            rig.state
                .reconcile_selection_and_order(&prev, None, None, false, false, 5_000);
        }
        rig.state.ui.banner_shown = cause != "height_above_changed";
        if cause == "selection_changed" {
            rig.state.ui.banner_shown = false;
        }
        rig.state.ui.drawn_selection =
            (cause != "height_above_changed").then(|| "another-row".into());
        rig.state.ui.painted_rows[0].1 = 3;
        rig.state.observe_paint(now + Duration::from_millis(120));
        let draft = displacement(&rx, &rig.state.config.instance_id);
        let wire = serde_json::to_value(&draft.kind).unwrap();
        assert_eq!(wire["detector"], "clicked_row_displaced");
        assert_eq!(wire["cause"], cause);
        assert_eq!(wire["from_y"], -1);
        assert_eq!(wire["to_y"], 3);
        assert_eq!(wire["dt_ms"], 120);
        assert_eq!(draft.window_ms, Some(5_000));
        assert_eq!(draft.frame.rows, rig.state.current.rows().count());
        rig.state.observe_paint(now + Duration::from_millis(130));
        assert!(rx.try_recv().is_err(), "unchanged paint must stay silent");
        rig.state.ui.anchor_clamped = false;
        rig.state.ui.painted_rows[0].1 = 4;
        rig.state.observe_paint(now + Duration::from_millis(140));
        let wire =
            serde_json::to_value(displacement(&rx, &rig.state.config.instance_id).kind).unwrap();
        assert_eq!(wire["from_y"], 3);
        assert_eq!(
            wire["cause"], "height_above_changed",
            "compare consecutive paints, not the click's selection/banner"
        );
    }
}

#[test]
fn clicked_row_leaving_the_viewport_emits_null_and_disarms() {
    let (mut rig, rx, _, now) = watched_row();
    rig.state.ui.painted_rows.clear();
    rig.state.observe_paint(now + Duration::from_millis(1));
    let wire = serde_json::to_value(displacement(&rx, &rig.state.config.instance_id).kind).unwrap();
    assert_eq!(wire["to_y"], serde_json::Value::Null);
    rig.state.observe_paint(now + Duration::from_millis(2));
    assert!(rx.try_recv().is_err());
}

#[test]
fn clicked_row_watch_ends_on_input_or_at_the_hold_deadline() {
    for action in [
        Wakeup::Scroll { down: true },
        Wakeup::Key(KeyAction::Other),
        Wakeup::Key(KeyAction::Filter(None)),
        Wakeup::Key(KeyAction::Search),
    ] {
        let (mut rig, rx, pane, now) = watched_row();
        rig.state.ui.painted_rows[0].1 = 1;
        rig.state.observe_paint(now + Duration::from_millis(1));
        let _ = displacement(&rx, &rig.state.config.instance_id);
        rig.state.arm_clicked_row(&pane, now);
        rig.state.apply_input([action], &mut rig.terminal).unwrap();
        rig.state.ui.painted_rows.clear();
        rig.state.observe_paint(now + Duration::from_millis(1));
        assert!(
            rx.try_recv().is_err(),
            "an intervening input ends the watch"
        );
    }
    let (mut rig, rx, pane, now) = watched_row();
    rig.state.ui.painted_rows[0].1 = 8;
    rig.state
        .observe_paint(now + REORDER_HOLD - Duration::from_millis(1));
    let _ = displacement(&rx, &rig.state.config.instance_id);
    rig.state.arm_clicked_row(&pane, now);
    rig.state.ui.painted_rows[0].1 = 9;
    rig.state.observe_paint(now + REORDER_HOLD);
    rig.state
        .observe_paint(now + REORDER_HOLD + Duration::from_millis(1));
    assert!(rx.try_recv().is_err(), "the click window is half-open");
}

#[test]
fn clicked_row_rearms_only_for_a_painted_target_and_carries_channel_drops() {
    let (mut rig, rx, pane, now) = watched_row();
    rig.state.ui.painted_rows[0].1 = 2;
    rig.state.observe_paint(now + Duration::from_millis(1));
    rig.state.ui.painted_rows[0].1 = 3;
    rig.state.observe_paint(now + Duration::from_millis(2)); // full channel
    let _ = displacement(&rx, &rig.state.config.instance_id);
    rig.state
        .arm_clicked_row(&pane, now + Duration::from_millis(10));
    rig.state.ui.painted_rows[0].1 = 4;
    rig.state.observe_paint(now + Duration::from_millis(11));
    let draft = displacement(&rx, &rig.state.config.instance_id);
    assert_eq!(draft.dropped_msgs, 1);
    let wire = serde_json::to_value(draft.kind).unwrap();
    assert_eq!(wire["from_y"], 3);
    assert_eq!(wire["dt_ms"], 1);
    rig.state.ui.painted_rows.clear();
    rig.state.arm_clicked_row(&pane, now);
    rig.state.observe_paint(now + Duration::from_millis(1));
    assert!(rx.try_recv().is_err(), "a hidden focus target cannot arm");
}
