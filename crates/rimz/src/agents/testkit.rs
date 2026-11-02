//! Shared fixtures for the adapter unit tests: the exhaustive lifecycle-signal
//! enumeration the totality tests sweep, and the hook-decode accessors every
//! adapter suite drives its native payloads through.

use serde_json::Value;

use crate::agents::capabilities::HookCapability;
use crate::agents::lifecycle::LifecycleSignal;
use crate::agents::{AgentLifecycleObservation, AskKind, HookOutput};

pub(super) fn outdated_json_source(
    source: &super::managed_source::ManagedSource,
    catalog: &[super::hook_types::HookEventSpec],
    sync: super::managed_json_hooks::SyncEncoding,
    statusline_pointer: Option<&str>,
) {
    use super::managed_json_hooks::SyncEncoding;
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    assert!(source.outdated_files_at(&path).is_empty());
    source.install_into(&path).unwrap();
    assert!(source.outdated_files_at(&path).is_empty());
    let canonical: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mut with_user = canonical.clone();
    let (event, entries) = with_user["hooks"]
        .as_object_mut()
        .unwrap()
        .iter_mut()
        .next()
        .unwrap();
    let event = event.clone();
    let entries = entries.as_array_mut().unwrap();
    entries.insert(0, json!({"hooks":[{"command":"echo before"}]}));
    entries.push(json!({"hooks":[{"command":"echo after"}]}));
    with_user["theme"] = json!("user preference");
    std::fs::write(&path, with_user.to_string()).unwrap();
    assert!(
        source.outdated_files_at(&path).is_empty(),
        "user entries and formatting"
    );
    let sync_event = catalog.iter().find(|hook| !hook.synchronous).unwrap().event;
    for field in [
        "command",
        "timeout",
        "sync",
        "removed event",
        "missing event",
        "statusline",
    ] {
        if field == "statusline" && statusline_pointer.is_none() {
            continue;
        }
        let mut drift = canonical.clone();
        match field {
            "command" => drift["hooks"][&event][0]["hooks"][0]["command"] = json!("old command"),
            "timeout" => drift["hooks"][&event][0]["hooks"][0]["timeout"] = json!(999),
            "sync" => match sync {
                SyncEncoding::EntryMarker => {
                    drift["hooks"][sync_event][0]["_rimz_sync"] = json!(true)
                }
                SyncEncoding::HandlerAsync => {
                    drift["hooks"][sync_event][0]["hooks"][0]["async"] = json!(true)
                }
                SyncEncoding::None => continue,
            },
            "removed event" => {
                drift["hooks"]["RetiredEvent"] = json!([canonical["hooks"][&event][0]])
            }
            "missing event" => {
                drift["hooks"].as_object_mut().unwrap().remove(&event);
            }
            "statusline" => {
                *drift.pointer_mut(statusline_pointer.unwrap()).unwrap() = json!("old statusline");
            }
            _ => unreachable!(),
        }
        if field == "sync" {
            let pointer = match sync {
                SyncEncoding::EntryMarker => "/_rimz_sync",
                SyncEncoding::HandlerAsync => "/hooks/0/async",
                SyncEncoding::None => continue,
            };
            assert_eq!(
                drift["hooks"][sync_event][0]
                    .pointer(pointer)
                    .and_then(Value::as_bool),
                Some(true),
                "sync fixture must change the non-blocking hook's native encoding"
            );
        }
        std::fs::write(&path, drift.to_string()).unwrap();
        assert_eq!(
            source.outdated_files_at(&path),
            std::slice::from_ref(&path),
            "{field}"
        );
    }
    std::fs::write(
        &path,
        r#"{"hooks":{"UserEvent":[{"hooks":[{"command":"echo user"}]}]}}"#,
    )
    .unwrap();
    assert!(source.outdated_files_at(&path).is_empty());
    std::fs::write(&path, "not json").unwrap();
    assert!(source.outdated_files_at(&path).is_empty());
}

/// Check context attachment and return the native replies for adapter goldens.
pub(crate) fn deadline_context_replies(
    kind: &str,
    post_event: &str,
) -> (Option<Value>, Option<Value>) {
    let adapter = crate::agents::definition_by_kind(kind).unwrap();
    let samples = adapter.conformance().classification;
    let post = samples
        .iter()
        .find(|sample| sample.event_name == post_event)
        .unwrap();
    let mut decoded = adapter.decode_hook(post.event_name, &post.payload).unwrap();
    assert!(adapter.attach_hook_context(&mut decoded, "deadline context"));
    let post_reply = decoded.json_reply().cloned();
    let other = samples
        .iter()
        .find(|sample| sample.event_name != post_event)
        .unwrap();
    let mut decoded = adapter
        .decode_hook(other.event_name, &other.payload)
        .unwrap();
    let before = decoded.reply().clone();
    assert!(!adapter.attach_hook_context(&mut decoded, "deadline context"));
    assert_eq!(decoded.reply(), &before);
    (post_reply, decoded.json_reply().cloned())
}

/// Attach prompt context to the adapter's prompt-submit sample and return the
/// exact line the hook writes to stdout.
pub(crate) fn prompt_context_stdout(kind: &str, prompt_event: &str) -> String {
    let adapter = crate::agents::definition_by_kind(kind).unwrap();
    let samples = adapter.conformance().classification;
    let prompt = samples
        .iter()
        .find(|sample| sample.event_name == prompt_event)
        .unwrap();
    let mut decoded = adapter
        .decode_hook(prompt.event_name, &prompt.payload)
        .unwrap();
    assert!(adapter.attach_prompt_context(&mut decoded, "prompt context"));
    decoded.json_reply().unwrap().to_string()
}

/// Decode one native hook payload, asserting the adapter accepted it.
pub(crate) fn hook_output(
    adapter: &impl HookCapability,
    event: &str,
    payload: &Value,
) -> HookOutput {
    let kind = adapter.spec().kind;
    adapter
        .decode_hook(event, payload)
        .unwrap_or_else(|err| panic!("{kind} decodes {event}: {err}"))
}

/// The lifecycle observation a native hook payload produces, if any.
pub(crate) fn hook_observation(
    adapter: &impl HookCapability,
    event: &str,
    payload: &Value,
) -> Option<AgentLifecycleObservation> {
    hook_output(adapter, event, payload).lifecycle().cloned()
}

/// The lifecycle observation a native hook payload must produce.
pub(crate) fn hook_lifecycle(
    adapter: &impl HookCapability,
    event: &str,
    payload: &Value,
) -> AgentLifecycleObservation {
    let kind = adapter.spec().kind;
    hook_observation(adapter, event, payload)
        .unwrap_or_else(|| panic!("{kind} {event} observes lifecycle"))
}

/// The lifecycle signal a native hook payload must produce.
pub(crate) fn hook_signal(
    adapter: &impl HookCapability,
    event: &str,
    payload: &Value,
) -> LifecycleSignal {
    hook_lifecycle(adapter, event, payload).signal
}

/// Every [`LifecycleSignal`] value, with each payload-carrying variant swept
/// over its full flag space — the enumeration the state machine's totality
/// tests fold through `step`.
pub(crate) fn all_signals() -> Vec<LifecycleSignal> {
    let mut signals = vec![
        LifecycleSignal::Registered,
        LifecycleSignal::TurnStarted { turn_id: None },
        LifecycleSignal::TurnInterrupted {
            turn_id: Some("turn-1".to_owned()),
        },
        LifecycleSignal::SubagentStarted,
        LifecycleSignal::Compacting,
        LifecycleSignal::Ended,
        LifecycleSignal::Lost,
        LifecycleSignal::AskResolved { native_key: None },
        LifecycleSignal::AskResolved {
            native_key: Some("ask-call".to_owned()),
        },
    ];
    for kind in [
        AskKind::Permission,
        AskKind::PlanApproval,
        AskKind::Question,
    ] {
        signals.push(LifecycleSignal::AwaitingInput {
            kind,
            ask_id: None,
            detail: None,
            native_key: None,
        });
    }
    for auto in [None, Some(false), Some(true)] {
        for failed in [false, true] {
            signals.push(LifecycleSignal::CompactionEnded { auto, failed });
        }
    }
    for errored in [false, true] {
        signals.push(LifecycleSignal::SubagentStopped { errored });
    }
    for errored in [false, true] {
        for parked_on_background in [false, true] {
            signals.push(LifecycleSignal::TurnEnded {
                errored,
                parked_on_background,
                turn_id: None,
            });
        }
    }
    for mutates in [false, true] {
        for edits in [false, true] {
            signals.push(LifecycleSignal::ToolUsed {
                mutates,
                edits,
                name: None,
                native_key: None,
                turn_id: Some("turn-1".to_owned()),
            });
        }
    }
    signals
}
