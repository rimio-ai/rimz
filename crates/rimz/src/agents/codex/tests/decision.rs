use super::*;

#[test]
fn codex_decision_stdout_shapes_are_pinned() {
    let permission = fixture(FeedKind::Permission);
    let allow = Resolution::new(json!({ "choice": "allow" }), ResolutionMethod::HookBridge);
    let rendered = CodexAdapter.render_decision(&permission, &allow).unwrap();
    insta::assert_json_snapshot!(rendered, @r###"
        {
          "hookSpecificOutput": {
            "decision": {
              "behavior": "allow"
            },
            "hookEventName": "PermissionRequest"
          }
        }
        "###);
    assert_eq!(
        rendered["hookSpecificOutput"]["decision"]["behavior"],
        "allow"
    );
    assert!(rendered.get("updatedInput").is_none());
    assert!(rendered.get("updatedPermissions").is_none());
    assert!(rendered.get("interrupt").is_none());

    let mut deny = Resolution::new(json!({ "choice": "deny" }), ResolutionMethod::HookBridge);
    deny.reason = Some("blocked by rimz policy".to_owned());
    let rendered = CodexAdapter.render_decision(&permission, &deny).unwrap();

    insta::assert_json_snapshot!(rendered, @r###"
        {
          "hookSpecificOutput": {
            "decision": {
              "behavior": "deny",
              "message": "blocked by rimz policy"
            },
            "hookEventName": "PermissionRequest"
          }
        }
        "###);

    let question = fixture(FeedKind::Question);
    let answer = Resolution::new(
        json!({ "choice": "allow", "updatedInput": { "question": "ready?" } }),
        ResolutionMethod::HookBridge,
    );
    let rendered = CodexAdapter.render_decision(&question, &answer).unwrap();

    insta::assert_json_snapshot!(rendered, @r###"
        {
          "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": {
              "question": "ready?"
            }
          }
        }
        "###);

    let mut deny_question =
        Resolution::new(json!({ "choice": "deny" }), ResolutionMethod::HookBridge);
    deny_question.reason = Some("question declined".to_owned());
    let rendered = CodexAdapter
        .render_decision(&question, &deny_question)
        .unwrap();

    insta::assert_json_snapshot!(rendered, @r###"
        {
          "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": "question declined"
          }
        }
        "###);

    let rendered = CodexAdapter.render_neutral("PermissionRequest").unwrap();
    insta::assert_snapshot!(
        serde_json::to_string(&rendered).unwrap(),
        @"null"
    );
}
