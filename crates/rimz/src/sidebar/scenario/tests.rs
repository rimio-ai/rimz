use super::*;
use crate::agents::AgentStatus;

fn parse(source: &str) -> Scenario {
    let result = Scenario::parse("test", source);
    assert!(
        result.is_ok(),
        "valid scenario refused: {}",
        result.err().unwrap()
    );
    result.unwrap()
}

#[test]
fn refusals_name_clause_position_and_fix() {
    for (source, expected) in [
        (
            "scenario test length 1s\nagent @a claude\n1s @a done",
            "line 1, column 1: length must be positive and include every beat within the timestamp range",
        ),
        (
            "agent @a claude\n0s @a spawns @child codex\n1s ui select @child",
            "line 3, column 14: @child is a child of @a; use ui select @a instead",
        ),
        (
            "agent @a claude\n0s @a spwans @b codex",
            "line 2, column 7: unknown verb \"spwans\", did you mean \"spawns\"?",
        ),
        (
            "agent @a claude\n2s @a idle\n1s @a idle",
            "line 3, column 1: beat times must not decrease",
        ),
        (
            "0s @ghost idle",
            "line 1, column 4: unknown handle @ghost; declare it with agent or spawns first",
        ),
        (
            "agent @a clade",
            "line 1, column 10: unknown kind \"clade\", did you mean \"claude\"?",
        ),
        (
            "preset fleat",
            "line 1, column 8: unknown preset \"fleat\", did you mean \"fleet\"?",
        ),
        (
            "scenario test widht 54",
            "line 1, column 15: unknown setting \"widht\", did you mean \"width\"?",
        ),
        (
            "agent @a claude\n0s @a set cot $2",
            "line 2, column 11: unknown field \"cot\", did you mean \"cost\"?",
        ),
        (
            "agent @a claude\npreset fleet",
            "line 2, column 1: preset must precede agents and beats",
        ),
        (
            "agent @a claude\nagent @a codex",
            "line 2, column 7: handle @a is already declared",
        ),
        (
            "agent @a claude\n0s @a runs unquoted",
            "line 2, column 12: expected quoted task",
        ),
        (
            "agent @a claude\n0s @a set context 101%",
            "line 2, column 19: context must be 0%..100%",
        ),
        (
            "agent @a claude\n0s @a set cost $NaN",
            "line 2, column 16: cost must be a finite non-negative dollar amount",
        ),
        (
            "agent @a claude\n0s @a idle extra",
            "line 2, column 12: unexpected argument",
        ),
    ] {
        let result = Scenario::parse("test", source);
        assert!(result.is_err(), "accepted invalid scenario: {source}");
        assert_eq!(result.err().unwrap().to_string(), expected, "{source}");
    }
}

#[test]
fn declarations_do_not_reuse_preset_process_or_child_panes() {
    let scenario = parse(
        "preset fleet\nagent @new claude\n0s @new spawns @child codex\n1s @new spawns @other claude",
    );
    let snapshot = scenario.at(Duration::from_secs(1));
    let panes: Vec<_> = snapshot.rows().flat_map(|row| row.pane_ids()).collect();
    let unique: std::collections::HashSet<_> = panes.iter().copied().collect();
    assert_eq!(
        unique.len(),
        panes.len(),
        "every process, root agent and child needs its own pane"
    );
}

#[test]
fn child_card_ui_verbs_name_the_parent() {
    for verb in ["select", "expand", "collapse", "click"] {
        let source = format!("agent @a claude\n0s @a spawns @child codex\n1s ui {verb} @child");
        let error = Scenario::parse("test", &source)
            .err()
            .expect("child is not a root card");
        assert!(
            error
                .to_string()
                .contains(&format!("use ui {verb} @a instead"))
        );
    }
}

#[test]
fn declarations_durations_comments_and_ui_beats_are_typed() {
    let scenario = parse(
        "# comment\nscenario story width 60 height 40 length 1m30s pets off glyphs unicode\nagent @a claude in #main\n500ms @a runs \"a # task\" # opening\n1.5s ui expand @a\n2s ui scroll down 3\n2s ui key enter",
    );
    assert_eq!(
        (scenario.name.as_str(), scenario.width, scenario.height),
        ("story", 60, 40)
    );
    assert_eq!(scenario.length, Duration::from_secs(90));
    assert_eq!(scenario.beats[0].at, Duration::from_millis(500));
    assert_eq!(scenario.beats[0].label, "opening");
    assert!(
        matches!(&scenario.beats[1].action, Action::Ui(UiAction::Expand(handle)) if handle == "a")
    );
    assert!(matches!(
        &scenario.beats[2].action,
        Action::Ui(UiAction::Scroll {
            up: false,
            count: 3
        })
    ));
    assert_eq!(
        scenario
            .at(Duration::from_secs(2))
            .rows()
            .next()
            .unwrap()
            .channel
            .as_deref(),
        Some("main")
    );
}

#[test]
fn model_folds_verbs_children_countdown_and_real_attention_scoring() {
    let scenario = parse(
        "agent @planner claude\nagent @coder codex\n0s @coder runs \"implement\"\n1s @coder spawns @fox claude \"review\"\n2s @coder waits 5s \"review\"\n3s @fox done \"looks good\"\n8s @coder asks \"ship it?\"\n9s @coder answered\n10s @coder fails \"network\"\n11s @coder done \"shipped\"\n12s @coder idle\n13s @coder leaves",
    );
    let waiting = scenario.at(Duration::from_secs(4));
    let coder = waiting
        .rows()
        .find(|row| row.as_agent().unwrap().handle.as_deref() == Some("coder"))
        .unwrap();
    assert_eq!(coder.status(), Some(AgentStatus::Sleeping));
    assert_eq!(
        coder.as_agent().unwrap().sub_agents[0].status,
        AgentStatus::Success
    );
    let crate::agents::PendingWaitTrigger::Timer { due, .. } =
        coder.as_agent().unwrap().pending_waits[0].trigger
    else {
        panic!("expected timer")
    };
    assert_eq!(due.duration_since(waiting.now).as_secs(), 3);
    let ask = scenario.at(Duration::from_secs(8));
    let first = ask.rows().next().unwrap();
    assert_eq!(first.as_agent().unwrap().handle.as_deref(), Some("coder"));
    assert_eq!(
        first.as_agent().unwrap().ask_detail.as_deref(),
        Some("ship it?")
    );
    assert!(first.attention_score > 0);
    assert_eq!(
        scenario
            .at(Duration::from_secs(9))
            .rows()
            .next()
            .unwrap()
            .status(),
        Some(AgentStatus::Running)
    );
    assert_eq!(
        scenario
            .at(Duration::from_secs(10))
            .rows()
            .next()
            .unwrap()
            .status(),
        Some(AgentStatus::Failed)
    );
    assert_eq!(
        scenario
            .at(Duration::from_secs(11))
            .rows()
            .next()
            .unwrap()
            .status(),
        Some(AgentStatus::Success)
    );
    assert_eq!(scenario.at(Duration::from_secs(13)).rows().count(), 1);
    assert_eq!(scenario.at(Duration::from_secs(4)), waiting, "seek is pure");
    assert_eq!(
        waiting.now.as_millisecond() - waiting.generated_at.as_millisecond(),
        0
    );
}

#[test]
fn presets_supply_handles_for_later_beats_and_settings_fold() {
    for preset in [
        "empty",
        "fleet",
        "provider",
        "cockpit",
        "focus",
        "economy",
        "reach",
        "clocks",
        "clocks-done",
    ] {
        let scenario = parse(&format!("preset {preset}"));
        let snapshot = scenario.at(Duration::ZERO);
        assert_eq!(snapshot.now.to_string(), "2026-06-09T12:00:00Z");
        if preset != "empty" {
            assert!(snapshot.rows().count() > 0, "{preset}");
        }
    }
    let scenario = parse(
        "scenario settings pets cat glyphs nerd-font\npreset empty\nagent @a claude\n0s @a runs \"work\"\n1s @a set context 50%\n1s @a set cost $1.25\n1s @a set tokens 12345\n1s @a set model test-model",
    );
    let snapshot = scenario.at(Duration::from_secs(1));
    let card = snapshot.rows().next().unwrap().as_agent().unwrap();
    assert_eq!(card.usage.context_pct, Some(50));
    assert_eq!(card.usage.total_tokens, Some(12345));
    assert_eq!(card.model.as_deref(), Some("test-model"));
    assert_eq!(
        card.context
            .as_ref()
            .unwrap()
            .cost
            .as_ref()
            .unwrap()
            .total_cost_usd,
        Some(1.25)
    );
    assert!(snapshot.theme.pets.enabled);
    let changed_preset = parse("preset clocks\n0s @scout asks \"Ready?\"").at(Duration::ZERO);
    assert_eq!(
        changed_preset
            .rows()
            .find(|row| row.display_name() == "scout")
            .unwrap()
            .as_agent()
            .unwrap()
            .ask_detail
            .as_deref(),
        Some("Ready?")
    );
}

#[test]
fn kind_unreachable_fields_and_asks_are_refused() {
    for (script, clause) in [
        (
            "agent @a amp\n0s @a set cost $2",
            "cost is not reachable for kind amp",
        ),
        (
            "agent @a amp\n0s @a asks \"go?\"",
            "asks is not reachable for kind amp",
        ),
    ] {
        let result = Scenario::parse("test", script);
        assert!(result.is_err(), "kind-unreachable state accepted");
        assert!(result.err().unwrap().to_string().contains(clause));
    }
}

#[test]
fn defaults_and_malformed_boundaries() {
    let empty = parse("");
    assert_eq!(
        (empty.name.as_str(), empty.width, empty.height, empty.length),
        ("test", 54, 34, Duration::from_secs(3))
    );
    assert_eq!(
        parse("agent @a claude\n1m30s @a idle").length,
        Duration::from_secs(93)
    );
    for source in [
        "scenario a width 0",
        "scenario a height 65536",
        "scenario a length 0s",
        "scenario a length 1s\nagent @a claude\n2s @a idle",
        "scenario a\nscenario b",
        "preset fleet\npreset empty",
        "agent @a claude\n0s @a idle\nagent @b codex",
        "agent @a claude in #",
        "agent @a claude in ../escape",
        "agent @a claude\n0s @a runs \"unterminated",
        "agent @a claude\n0s @a runs \"quoted\"suffix",
        "agent @a claude\n0s @a waits -1s",
        "agent @a claude\n1e6s @a idle",
        "agent @a claude\n999999999999999999999999m @a idle",
        "agent @a claude\n0s @a set tokens -1",
        "agent @a claude\n0s @a set cost $-1",
        "agent @a claude\n0s @a spawns @a codex",
        "agent @a claude\n0s ui select @missing",
        "0s ui scroll left",
    ] {
        assert!(
            Scenario::parse("test", source).is_err(),
            "accepted {source}"
        );
    }
}
