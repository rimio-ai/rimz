use super::*;

#[test]
fn permission_allow_shape_is_pinned() {
    let item = fixture(FeedKind::Permission);
    let resolution = Resolution::new(json!({ "choice": "allow" }), ResolutionMethod::HookBridge);
    let rendered = ClaudeAdapter.render_decision(&item, &resolution).unwrap();
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
    assert_eq!(
        rendered["hookSpecificOutput"]["hookEventName"],
        "PermissionRequest"
    );
}

#[test]
fn plan_approval_requires_updated_input() {
    let item = fixture(FeedKind::PlanApproval);
    let resolution = Resolution::new(json!({ "choice": "allow" }), ResolutionMethod::HookBridge);
    let err = ClaudeAdapter
        .render_decision(&item, &resolution)
        .unwrap_err();
    assert!(matches!(
        err,
        AgentErr::MissingField {
            agent: "claude",
            field: "updatedInput"
        }
    ));
}

#[test]
fn neutral_payload_is_empty_stdout() {
    let value = ClaudeAdapter.render_neutral("PermissionRequest").unwrap();
    insta::assert_snapshot!(
        serde_json::to_string(&value).unwrap(),
        @"null"
    );
    assert_eq!(value, None);
}

#[test]
fn permission_deny_shape_is_pinned() {
    let item = fixture(FeedKind::Permission);
    let resolution = Resolution::new(json!({ "choice": "deny" }), ResolutionMethod::HookBridge);
    let rendered = ClaudeAdapter.render_decision(&item, &resolution).unwrap();

    insta::assert_json_snapshot!(rendered, @r###"
        {
          "hookSpecificOutput": {
            "decision": {
              "behavior": "deny"
            },
            "hookEventName": "PermissionRequest"
          }
        }
        "###);
}

#[test]
fn plan_approval_allow_shape_is_pinned() {
    let item = fixture(FeedKind::PlanApproval);
    let resolution = Resolution::new(
        json!({ "choice": "allow", "updatedInput": "ship the plan" }),
        ResolutionMethod::HookBridge,
    );
    let rendered = ClaudeAdapter.render_decision(&item, &resolution).unwrap();

    insta::assert_json_snapshot!(rendered, @r###"
        {
          "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": "ship the plan"
          }
        }
        "###);
}

#[test]
fn ask_user_question_allow_shape_carries_updated_input_object() {
    let item = fixture(FeedKind::Question);
    let resolution = Resolution::new(
        json!({ "choice": "allow", "updatedInput": { "question": "ready?" } }),
        ResolutionMethod::HookBridge,
    );
    let rendered = ClaudeAdapter.render_decision(&item, &resolution).unwrap();

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
}
