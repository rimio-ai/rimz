use super::*;

#[test]
fn permission_decision_has_no_reserved_keys() {
    let item = fixture(FeedKind::Permission);
    let resolution = Resolution::new(json!({ "choice": "allow" }), ResolutionMethod::HookBridge);
    let rendered = CodexAdapter.render_decision(&item, &resolution).unwrap();
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
}

#[test]
fn permission_deny_shape_is_pinned() {
    let item = fixture(FeedKind::Permission);
    let resolution = Resolution::new(json!({ "choice": "deny" }), ResolutionMethod::HookBridge);
    let rendered = CodexAdapter.render_decision(&item, &resolution).unwrap();

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
fn neutral_payload_is_empty_stdout() {
    let rendered = CodexAdapter.render_neutral("PermissionRequest").unwrap();

    insta::assert_snapshot!(
        serde_json::to_string(&rendered).unwrap(),
        @"null"
    );
}
