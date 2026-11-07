use super::*;

fn tune(id: &str, setting: &str, value: Option<&str>) -> EventEnvelope {
    let mut params = json!({"agent_id": id, "setting": setting, "by": "@operator"});
    if let Some(value) = value {
        params["value"] = json!(value);
    }
    let mut event = EventEnvelope::new(
        workspace(),
        "session",
        "codex",
        "agent",
        "agent.tune",
        params,
    );
    event.timestamp = epoch();
    event
}

fn row(events: &[EventEnvelope], id: &str) -> serde_json::Value {
    serde_json::to_value(
        reduce_agent_states(events)
            .iter()
            .find(|agent| agent.agent_id == id)
            .unwrap(),
    )
    .unwrap()
}

#[test]
fn tune_sets_each_key_with_actor_and_time() {
    for (setting, key, value) in [
        ("keep-warm", "keep_warm", "2h"),
        ("idle-compact", "idle_compact", "120k"),
        ("smart-compact", "smart_compact", "70%"),
    ] {
        let event = tune("seat", setting, Some(value));
        let events = [
            same_process_registration("seat", "coder", 0, "tmux:%1", 100),
            event,
        ];
        assert_eq!(
            row(&events, "seat")["tune"][key],
            json!({
                "value": value, "by": "@operator", "at": epoch(),
            })
        );
    }
}

#[test]
fn tune_reset_clears_only_its_key() {
    let mut events = vec![
        same_process_registration("seat", "coder", 0, "tmux:%1", 100),
        tune("seat", "keep-warm", Some("off")),
        tune("seat", "idle-compact", Some("120k")),
    ];
    assert_eq!(row(&events, "seat")["tune"]["keep_warm"]["value"], "off");
    events.push(tune("seat", "keep-warm", None));
    let state = row(&events, "seat");
    assert!(state["tune"].get("keep_warm").is_none());
    assert_eq!(state["tune"]["idle_compact"]["value"], "120k");
}

#[test]
fn tune_budget_and_unknown_sessions_do_not_change_rows() {
    let mut events = vec![same_process_registration(
        "seat", "coder", 0, "tmux:%1", 100,
    )];
    let before = reduce_agent_states(&events);
    events.push(tune("missing", "keep-warm", Some("2h")));
    events.push(tune("seat", "budget", Some("+10")));
    let unknown = tune("seat", "future-setting", Some("2h"));
    assert!(matches!(unknown.kind(), EventKind::Other { .. }));
    events.push(unknown);
    assert_eq!(reduce_agent_states(&events), before);
}

#[test]
fn tune_survives_registration_and_same_process_clear_but_not_a_fresh_launch() {
    let mut launch = launch_payload("seat", "coder");
    launch.launch_id = Some("seat".into());
    launch.pane_id = Some(PaneId::parse("tmux:%1").unwrap());
    launch.runtime_owner = Some(RuntimeOwner::new(
        RuntimeOwnerKind::Agent,
        "seat",
        100,
        Some("agent-start".into()),
    ));
    let mut launched = launch_event("codex", launch);
    launched.timestamp = epoch();
    let mut events = vec![
        launched,
        same_process_registration("seat", "coder", 0, "tmux:%1", 100),
        tune("seat", "keep-warm", Some("2h")),
    ];
    let expected = json!({"value": "2h", "by": "@operator", "at": epoch()});
    assert_eq!(row(&events, "seat")["tune"]["keep_warm"], expected);
    events.push(same_process_registration(
        "seat", "coder", 1, "tmux:%1", 100,
    ));
    assert_eq!(row(&events, "seat")["tune"]["keep_warm"], expected);
    events.push(same_process_registration(
        "cleared", "coder", 2, "tmux:%1", 100,
    ));
    assert_eq!(row(&events, "cleared")["tune"]["keep_warm"], expected);
    events.push(tune("cleared", "keep-warm", Some("off")));
    events.push(same_process_registration(
        "cleared-again",
        "coder",
        3,
        "tmux:%1",
        100,
    ));
    assert_eq!(
        row(&events, "cleared-again")["tune"]["keep_warm"]["value"],
        "off"
    );
    events.push(tune("cleared-again", "keep-warm", None));
    events.push(same_process_registration(
        "cleared-after-reset",
        "coder",
        4,
        "tmux:%1",
        100,
    ));
    assert!(row(&events, "cleared-after-reset").get("tune").is_none());
    events.push(launch_event("codex", launch_payload("fresh", "new-coder")));
    assert!(row(&events, "fresh").get("tune").is_none());
}
