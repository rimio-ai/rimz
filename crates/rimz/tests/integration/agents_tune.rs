use super::*;

fn seat() -> Env {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-tune", "feature-tune");
    env
}

fn report(env: &Env, args: &[&str]) -> serde_json::Value {
    serde_json::from_str(&loop_ok(env, args)).unwrap()
}

#[test]
fn tune_sets_reports_and_resets_each_key() {
    let env = seat();
    for (key, value) in [
        ("keep-warm", "2h"),
        ("idle-compact", "120k"),
        ("smart-compact", "70%"),
    ] {
        let before = report(&env, &["agents", "tune", "@claude", key, "--json"]);
        assert_eq!(before["source"], "config");
        let set = report(&env, &["agents", "tune", "@claude", key, value, "--json"]);
        assert_eq!(set["setting"], key);
        assert_eq!(set["value"], value);
        assert_eq!(set["source"], "override");
        assert!(set["set_at"].is_string());
        assert!(set.get("set_by").is_none());
        assert_eq!(set["configured"], before["configured"]);
        let text = loop_ok(&env, &["agents", "tune", "@claude", key]);
        assert!(
            text.contains(value) && text.contains("override") && text.contains("config"),
            "{text}"
        );
        let all = report(&env, &["agents", "tune", "@claude", "--json"]);
        assert!(all["agent"].is_string());
        assert_eq!(all["settings"].as_array().unwrap().len(), 3);
        assert!(all["settings"][0].get("agent").is_none());
        assert!(all["budget"].get("agent").is_none());
        assert!(all["budget"].get("cap_usd").is_some());
        let reset = report(&env, &["agents", "tune", "@claude", key, "reset", "--json"]);
        assert_eq!(reset, before);
        let count = env.read_events().len();
        loop_ok(&env, &["agents", "tune", "@claude", key, "reset"]);
        assert_eq!(
            env.read_events().len(),
            count,
            "a redundant reset writes nothing"
        );
        let off = report(&env, &["agents", "tune", "@claude", key, "off", "--json"]);
        assert_eq!(off["value"], "off");
        loop_ok(&env, &["agents", "tune", "@claude", key, "reset"]);
    }
    let text = loop_ok(&env, &["agents", "tune", "@claude"]);
    for key in ["keep-warm", "idle-compact", "smart-compact", "budget"] {
        assert!(text.contains(key), "{text}");
    }
}

#[test]
fn tune_audit_names_an_agent_actor_but_not_a_shell() {
    let env = seat();
    loop_ok(&env, &["agents", "tune", "@claude", "keep-warm", "2h"]);
    let first = env
        .read_events()
        .into_iter()
        .find(|event| event.method == "agent.tune")
        .unwrap();
    assert_eq!(first.source, "claude");
    assert_eq!(
        first.params_value(),
        json!({"agent_id": "sess-tune", "setting": "keep-warm", "value": "2h"})
    );
    let output = calling_loop(&env, "sess-tune")
        .args([
            "agents",
            "tune",
            "@claude",
            "smart-compact",
            "70%",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let set: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let event = env
        .read_events()
        .into_iter()
        .rfind(|event| event.method == "agent.tune")
        .unwrap();
    assert_eq!(event.params_value()["by"], set["set_by"]);
    assert!(set["set_by"].as_str().unwrap().starts_with('@'));
    let text = loop_ok(&env, &["agents", "tune", "@claude", "smart-compact"]);
    assert!(
        text.contains(&format!("by {}", set["set_by"].as_str().unwrap())),
        "{text}"
    );
}

#[test]
fn tune_refuses_ended_and_provider_subagents_but_reads_them() {
    let env = seat();
    run_hook(
        &env,
        json!({"hook_event_name": "SessionEnd", "session_id": "sess-tune"}),
        &env.project_root,
    );
    for key in ["keep-warm", "idle-compact", "smart-compact"] {
        let output = env
            .rimz()
            .args(["agents", "tune", "sess-tune", key, "off"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("ended"),
            "{output:?}"
        );
        loop_ok(&env, &["agents", "tune", "sess-tune", key, "--json"]);
    }
    let mut observation = rimz::agents::AgentLifecycleObservation::new(
        Some("native-child".into()),
        rimz::agents::LifecycleSignal::SubagentStarted,
    );
    observation.parent_agent_id = Some("sess-tune".into());
    env.store()
        .append_event(&rimz::store::event::EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            "session",
            "claude",
            "test",
            &observation,
        ))
        .unwrap();
    let output = env
        .rimz()
        .args(["agents", "tune", "native-child", "keep-warm", "off"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("provider subagent"),
        "{output:?}"
    );
    let provider = report(
        &env,
        &["agents", "tune", "native-child", "keep-warm", "--json"],
    );
    assert_eq!(provider["agent"], "@native-child");
    let budget = loop_ok(&env, &["agents", "tune", "native-child", "budget"]);
    assert!(
        !budget.contains("rimz agents tune @native-child budget 5"),
        "{budget}"
    );
    for (id, reason) in [
        ("sess-tune", "session has ended"),
        ("native-child", "session is a provider subagent"),
    ] {
        loop_ok(&env, &["agents", "tune", id, "--json"]);
        loop_ok(&env, &["agents", "budget", id, "--json"]);
        let text = loop_ok(&env, &["agents", "tune", id, "budget"]);
        assert!(!text.contains("rimz agents tune"), "{text}");
        for value in ["5", "5/day", "+5", "off", "clear", "reset"] {
            for args in [
                vec!["agents", "tune", id, "budget", value],
                vec!["agents", "budget", id, value],
            ] {
                let output = env.rimz().args(args).output().unwrap();
                assert_eq!(output.status.code(), Some(1), "{output:?}");
                assert!(
                    String::from_utf8_lossy(&output.stderr).contains(reason),
                    "{output:?}"
                );
                assert!(
                    rimz::harness::budget::read_ledger(
                        &env.runtime_paths(),
                        &AgentKind::new_unchecked("claude"),
                        &id.into()
                    )
                    .is_none()
                );
            }
        }
        let kind = AgentKind::new_unchecked("claude");
        let mut ledger = rimz::harness::budget::BudgetLedger::new("5".parse().unwrap());
        ledger.raised_cap_usd = Some(5.0);
        write_ledger(&env.runtime_paths(), &kind, &id.into(), &ledger).unwrap();
        let text = loop_ok(&env, &["agents", "tune", id, "budget"]);
        assert!(text.contains("$5.00"), "{text}");
        assert!(!text.contains("rimz agents tune"), "{text}");
    }
}

#[test]
fn tune_trimmed_reset_uses_same_grammar_for_every_setting() {
    let env = seat();
    for (setting, value) in [
        ("keep-warm", "2h"),
        ("idle-compact", "120k"),
        ("smart-compact", "70%"),
        ("budget", "5"),
    ] {
        loop_ok(&env, &["agents", "tune", "@claude", setting, value]);
        let reset = report(
            &env,
            &["agents", "tune", "@claude", setting, " reset ", "--json"],
        );
        assert_eq!(
            reset["source"],
            if setting == "budget" {
                "none"
            } else {
                "config"
            }
        );
    }
}

#[test]
fn tune_rejects_invalid_values_and_nonbudget_no_continue() {
    let env = seat();
    for key in ["keep-warm", "idle-compact", "smart-compact", "budget"] {
        let output = env
            .rimz()
            .args(["agents", "tune", "@claude", key, "nonsense"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{output:?}");
    }
    let output = env
        .rimz()
        .args([
            "agents",
            "tune",
            "@claude",
            "keep-warm",
            "off",
            "--no-continue",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let output = env
        .rimz()
        .args(["agents", "tune", "@claude", "unknown"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
}

#[test]
fn tune_budget_alias_is_identical_and_reset_removes_an_unlaunched_cap() {
    let env = seat();
    for value in [Some("5"), Some("+10"), Some("off"), None, Some("reset")] {
        let mut args = vec!["agents", "tune", "@claude", "budget"];
        if let Some(value) = value {
            args.push(value);
        }
        args.push("--no-continue");
        let tuned = loop_ok(&env, &args);
        let alias = loop_ok(&env, &["agents", "budget", "@claude"]);
        assert_eq!(alias, tuned);
        let new_json = loop_ok(&env, &["agents", "tune", "@claude", "budget", "--json"]);
        assert_eq!(
            loop_ok(&env, &["agents", "budget", "@claude", "--json"]),
            new_json
        );
    }
    let reset = report(&env, &["agents", "tune", "@claude", "budget", "--json"]);
    assert_eq!(reset["source"], "none");
    assert!(
        rimz::harness::budget::read_ledger(
            &env.runtime_paths(),
            &AgentKind::new_unchecked("claude"),
            &"sess-tune".into()
        )
        .is_none()
    );
    let count = env.read_events().len();
    loop_ok(&env, &["agents", "tune", "@claude", "budget", "reset"]);
    assert_eq!(env.read_events().len(), count);
}

#[test]
fn tune_budget_reset_restores_launch_spec_across_window_transitions() {
    for launch in ["20/day", "20"] {
        let env = seat();
        let mut observation = rimz::agents::AgentLifecycleObservation::new(
            Some("sess-tune".into()),
            rimz::agents::LifecycleSignal::Registered,
        );
        observation.launch.budget = Some(launch.into());
        env.store()
            .append_event(&rimz::store::event::EventEnvelope::agent_lifecycle(
                env.workspace_id.clone(),
                "session",
                "claude",
                "test",
                &observation,
            ))
            .unwrap();
        loop_ok(
            &env,
            &[
                "agents",
                "tune",
                "@claude",
                "budget",
                if launch == "20" { "5/day" } else { "5" },
            ],
        );
        let reset = report(
            &env,
            &["agents", "tune", "@claude", "budget", "reset", "--json"],
        );
        assert_eq!(reset["source"], "launch");
        assert_eq!(reset["cap_usd"], 20.0);
        assert_eq!(
            reset["window"],
            if launch == "20" { "session" } else { "day" }
        );
        let ledger = rimz::harness::budget::read_ledger(
            &env.runtime_paths(),
            &AgentKind::new_unchecked("claude"),
            &"sess-tune".into(),
        )
        .unwrap();
        assert_eq!(ledger.day_baseline.is_some(), launch != "20");
        assert!(!ledger.disabled && ledger.raised_cap_usd.is_none());
    }
}

#[test]
fn tune_budget_continue_is_withdrawn_once_the_agent_is_prompted() {
    use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};
    use rimz::harness::budget::{BudgetLedger, BudgetParkStamp, BudgetSpec, write_ledger};
    use rimz::store::message::{MessageStatus, Premise};

    let env = seat();
    let kind = AgentKind::new_unchecked("claude");
    let now = jiff::Timestamp::now();
    let mut ledger = BudgetLedger::new(BudgetSpec {
        cap_usd: 5.0,
        window: rimz::agents::BudgetWindow::Session,
    });
    ledger.parked = Some(BudgetParkStamp {
        at_cost: 5.0,
        at: now,
    });
    ledger.last_interrupt_at = Some(now);
    write_ledger(&env.runtime_paths(), &kind, &"sess-tune".into(), &ledger).unwrap();

    loop_ok(&env, &["agents", "tune", "@claude", "budget", "off"]);
    let store = env.store();
    let queued = store.list_messages().unwrap();
    let [continued] = queued.as_slice() else {
        panic!("one budget continue queued: {queued:?}");
    };
    assert!(
        matches!(
            continued.premises.as_slice(),
            [Premise::NotPromptedSince { .. }]
        ),
        "{continued:?}"
    );

    let prompt = AgentLifecycleObservation::new(
        Some("sess-tune".into()),
        LifecycleSignal::TurnStarted { turn_id: None },
    );
    store
        .append_event(&rimz::store::event::EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            "session",
            "claude",
            "UserPromptSubmit",
            &prompt,
        ))
        .unwrap();
    loop_ok(&env, &["message", "sweep"]);

    let history = store.list_message_history().unwrap();
    let withdrawn = history
        .iter()
        .find(|record| record.message_id == continued.message_id)
        .unwrap_or_else(|| panic!("continue withdrawn into history: {history:?}"));
    assert_eq!(
        (withdrawn.status, withdrawn.last_error.as_deref()),
        (
            MessageStatus::Canceled,
            Some("agent was prompted after this message was queued")
        )
    );
    assert!(
        withdrawn.last_sent_at.is_none(),
        "never typed: {withdrawn:?}"
    );
}
