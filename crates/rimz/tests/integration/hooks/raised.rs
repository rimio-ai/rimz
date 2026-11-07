use super::*;
use rimz::agents::state::{RaisedAsk, RaisedAskState};
use rimz::ids::{AgentKind, AskId};
use rimz::store::raised::{self, Entry};

fn feed(env: &Env, event: &str, extra: Value) {
    let mut payload = json!({"hook_event_name": event, "session_id": "raised-hook"});
    payload
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let output = env.run_hook("claude", &payload.to_string());
    assert!(output.status.success(), "{output:?}");
}

fn room() -> Env {
    let env = Env::new();
    env.record(&env.project_root);
    feed(&env, "SessionStart", json!({"source": "startup"}));
    feed(&env, "UserPromptSubmit", json!({"prompt": "work"}));
    env
}

fn raise(env: &Env, state: RaisedAskState, text: Option<&str>) -> AskId {
    let at = Timestamp::now();
    let id = AskId::new();
    raised::raise(
        env.store().paths(),
        Entry {
            kind: AgentKind::new_unchecked("claude"),
            agent_id: "raised-hook".into(),
            value: RaisedAsk {
                ask_id: id.clone(),
                state,
                raised_at: at,
                basis_at: at,
                opened_at: (state == RaisedAskState::Open).then_some(at),
                text: text.map(ToOwned::to_owned),
                options: vec![],
                raised_by: None,
            },
        },
    )
    .unwrap();
    id
}

#[test]
fn raised_hook_human_prompts_close_but_compaction_and_harness_prompts_do_not() {
    let env = room();
    for prompt in [
        "  /compact extra instructions",
        "Type: WAIT\nFrom: @rimz\nContent:\nready",
        "Type: AGENT_MESSAGE\nFrom: @peer\nContent:\nreviewed",
        "Type: SYSTEM\nFrom: @rimz\nContent:\ncontinue",
    ] {
        let id = raise(&env, RaisedAskState::Open, Some("Review?"));
        feed(&env, "UserPromptSubmit", json!({"prompt": prompt}));
        assert_eq!(
            env.store().snapshot_cached().unwrap().agents[0]
                .raised_ask
                .as_ref()
                .map(|ask| &ask.ask_id),
            Some(&id),
            "{prompt}"
        );
    }
    for prompt in ["yes", "/review the result"] {
        raise(&env, RaisedAskState::Open, Some("Review?"));
        feed(&env, "UserPromptSubmit", json!({"prompt": prompt}));
        assert!(
            env.store().snapshot_cached().unwrap().agents[0]
                .raised_ask
                .is_none(),
            "{prompt}"
        );
    }
    let closes = env
        .read_events()
        .into_iter()
        .filter(|event| event.method == "ask.closed")
        .collect::<Vec<_>>();
    assert_eq!(closes.len(), 2);
    assert!(
        closes
            .iter()
            .all(|event| event.params_value()["cause"] == "user_turn")
    );
}

#[test]
fn raised_hook_opens_only_on_stop_and_preserves_explicit_text() {
    for text in [None, Some("Explicit question?")] {
        let env = room();
        let id = raise(&env, RaisedAskState::Armed, text);
        assert!(
            env.store().snapshot_cached().unwrap().agents[0]
                .raised_ask
                .is_none()
        );
        feed(
            &env,
            "Stop",
            json!({"last_assistant_message": "Final reply?"}),
        );
        let snapshot = env.store().snapshot_cached().unwrap();
        let ask = snapshot.agents[0]
            .raised_ask
            .as_ref()
            .expect("Stop opens the armed ask");
        assert_eq!(ask.ask_id, id);
        assert_eq!(ask.text.as_deref(), Some(text.unwrap_or("Final reply?")));
        assert!(ask.opened_at.unwrap() >= ask.raised_at);
    }
}

#[test]
fn raised_hook_later_turn_drops_an_unseen_armed_ask_once() {
    let env = room();
    let id = raise(&env, RaisedAskState::Armed, None);
    feed(
        &env,
        "UserPromptSubmit",
        json!({"prompt": "Type: WAIT\nFrom: @rimz\nContent:\nready"}),
    );
    assert!(raised::read(env.store().paths()).asks.is_empty());
    feed(&env, "Stop", json!({"last_assistant_message": "Too late?"}));
    let closes = env
        .read_events()
        .into_iter()
        .filter(|event| event.method == "ask.closed")
        .collect::<Vec<_>>();
    assert_eq!(closes.len(), 1);
    assert_eq!(closes[0].params_value()["ask_id"], id.as_str());
    assert_eq!(closes[0].params_value()["cause"], "turn_unseen");
}
