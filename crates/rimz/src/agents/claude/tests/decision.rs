use super::*;

#[test]
fn claude_decision_stdout_shapes_are_pinned() {
    let permission = fixture(FeedKind::Permission);
    let allow = Resolution::new(json!({ "choice": "allow" }), ResolutionMethod::HookBridge);
    insta::assert_json_snapshot!(ClaudeAdapter.render_decision(&permission, &allow).unwrap(), @r###"
        {
          "hookSpecificOutput": {
            "decision": {
              "behavior": "allow"
            },
            "hookEventName": "PermissionRequest"
          }
        }
        "###);

    let deny = Resolution::new(json!({ "choice": "deny" }), ResolutionMethod::HookBridge);
    insta::assert_json_snapshot!(ClaudeAdapter.render_decision(&permission, &deny).unwrap(), @r###"
        {
          "hookSpecificOutput": {
            "decision": {
              "behavior": "deny"
            },
            "hookEventName": "PermissionRequest"
          }
        }
        "###);

    let plan = fixture(FeedKind::PlanApproval);
    let plan_allow = Resolution::new(
        json!({ "choice": "allow", "updatedInput": "ship the plan" }),
        ResolutionMethod::HookBridge,
    );
    insta::assert_json_snapshot!(ClaudeAdapter.render_decision(&plan, &plan_allow).unwrap(), @r###"
        {
          "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": "ship the plan"
          }
        }
        "###);

    let question = fixture(FeedKind::Question);
    let answer = Resolution::new(
        json!({ "choice": "allow", "updatedInput": { "question": "ready?" } }),
        ResolutionMethod::HookBridge,
    );
    insta::assert_json_snapshot!(ClaudeAdapter.render_decision(&question, &answer).unwrap(), @r###"
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

    let missing_update =
        Resolution::new(json!({ "choice": "allow" }), ResolutionMethod::HookBridge);
    assert!(matches!(
        ClaudeAdapter
            .render_decision(&plan, &missing_update)
            .unwrap_err(),
        AgentErr::MissingField {
            agent: "claude",
            field: "updatedInput"
        }
    ));

    let value = ClaudeAdapter.render_neutral("PermissionRequest").unwrap();
    insta::assert_snapshot!(serde_json::to_string(&value).unwrap(), @"null");
    assert_eq!(value, None);
}
