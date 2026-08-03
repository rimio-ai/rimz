use super::*;
use rimz::transcript::{AskOption, AskQuestion};

fn question(multi_select: bool) -> AskQuestion {
    AskQuestion {
        question: "Choose?".to_owned(),
        options: vec![
            AskOption::from("safe".to_owned()),
            AskOption::from("fast".to_owned()),
        ],
        multi_select,
        has_option_previews: false,
    }
}

#[test]
fn selectors_accept_indices_labels_and_multiselect() {
    let options = question(false).options;
    assert_eq!(resolve_selector("1", &options).unwrap(), 0);
    assert_eq!(resolve_selector("FAST", &options).unwrap(), 1);
    assert!(
        resolve_selector("3", &options)
            .unwrap_err()
            .contains("out of range")
    );

    let reply = validate_reply(
        AskKind::Question,
        &question(true),
        AskReply {
            picks: vec![0, 1],
            ..AskReply::default()
        },
    )
    .unwrap();
    assert_eq!(reply.picks, vec![0, 1]);
    assert!(
        validate_reply(
            AskKind::Question,
            &question(false),
            AskReply {
                picks: vec![0, 1],
                ..AskReply::default()
            },
        )
        .unwrap_err()
        .contains("single-select")
    );
    assert!(
        validate_reply(
            AskKind::Question,
            &question(true),
            AskReply {
                picks: vec![0, 0],
                ..AskReply::default()
            },
        )
        .unwrap_err()
        .contains("only once")
    );
}

#[test]
fn structured_answers_require_one_object_per_question() {
    let questions = vec![question(false), question(true)];
    let error = normalize_json_answers(
        &[JsonAnswer {
            pick: vec![JsonPick::Label("safe".to_owned())],
            text: None,
        }],
        AskKind::Question,
        &questions,
    )
    .unwrap_err();
    assert!(error.contains("expected 2 JSON answer objects"));
}

#[test]
fn label_resolution_rejects_case_insensitive_ambiguity() {
    let options = vec![
        AskOption::from("Safe".to_owned()),
        AskOption::from("safe".to_owned()),
    ];
    assert!(
        resolve_selector("SAFE", &options)
            .unwrap_err()
            .contains("ambiguous")
    );
}

#[test]
fn menu_only_actions_name_the_agent_pane() {
    for (kind, option, rejected) in [
        (AskKind::Permission, "allow", "deny"),
        (AskKind::PlanApproval, "approve", "keep-planning"),
    ] {
        let question = AskQuestion {
            question: "Continue?".to_owned(),
            options: vec![AskOption::from(option.to_owned())],
            multi_select: false,
            has_option_previews: false,
        };
        let selector_error = resolve_answer_selector(kind, rejected, &question).unwrap_err();
        assert!(selector_error.contains("agent pane"));
        assert!(selector_error.contains(&format!("valid options: 1={option}")));

        let text_error = validate_reply(
            kind,
            &question,
            AskReply {
                text: Some("instructions".to_owned()),
                ..AskReply::default()
            },
        )
        .unwrap_err();
        assert!(text_error.contains("agent pane"));
    }
}

#[test]
fn ask_id_resolution_preserves_root_and_subagent_scopes() {
    let now = jiff::Timestamp::from_second(1_000).unwrap();
    let ask_id = AskId::parse("ask_0123456789abcdef").unwrap();
    let mut child = rimz::testkit::agent_state("codex", "child", now);
    child.status = rimz::agents::AgentStatus::Waiting;
    child.waiting_since = Some(now);
    child.parent_agent_id = Some(rimz::ids::AgentSessionId::from("parent"));
    child.open_ask = Some(rimz::agents::OpenAsk {
        id: ask_id.clone(),
        kind: AskKind::Question,
        detail: None,
        native_key: Some("native-ask".to_owned()),
        since: now,
    });
    let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
        rimz::WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-ask")),
        vec![child],
        now,
    );

    assert!(
        crate::cli::resolve_open_ask(&snapshot, ask_id.as_str(), None, true)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        crate::cli::resolve_open_ask(&snapshot, ask_id.as_str(), None, false)
            .unwrap()
            .unwrap()
            .agent_id
            .as_str(),
        "child"
    );
}
