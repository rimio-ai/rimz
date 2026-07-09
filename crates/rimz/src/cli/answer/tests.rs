use super::*;
use crate::cli::asks::AskAgentView;
use rimz::transcript::{AskOption, AskQuestion};

fn view(questions: Vec<AskQuestion>) -> OpenAskView {
    OpenAskView {
        ask_id: AskId::parse("ask_0123456789abcdef").unwrap(),
        agent: AskAgentView {
            handle: "@planner".to_owned(),
            kind: rimz::ids::AgentKind::new_unchecked("claude"),
            channel: None,
        },
        kind: AskKind::Question,
        since: jiff::Timestamp::UNIX_EPOCH,
        detail: None,
        questions,
    }
}

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
    let view = view(vec![question(false), question(true)]);
    let error = normalize_json_answers(
        &[JsonAnswer {
            pick: vec![JsonPick::Label("safe".to_owned())],
            text: None,
        }],
        &view,
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
