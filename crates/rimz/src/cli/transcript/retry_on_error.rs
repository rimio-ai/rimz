use super::*;
use std::collections::{BTreeSet, HashMap};

use rimz::ids::{AgentKind, AgentSessionId};
use rimz::ledger::transcript_log::{TranscriptEntry, TranscriptKind};

fn ts(raw: &str) -> jiff::Timestamp {
    raw.parse().expect("timestamp")
}

fn log_entry(
    kind: &str,
    session_id: &str,
    entry: TranscriptKind,
    text: &str,
    at: &str,
    channel: Option<&str>,
) -> TranscriptEntry {
    TranscriptEntry {
        at: ts(at),
        kind: AgentKind::new_unchecked(kind),
        agent_id: AgentSessionId::from(session_id),
        channel: channel.map(ToOwned::to_owned),
        name: None,
        profile: None,
        role: None,
        entry,
        request_id: None,
        from: None,
        text: text.to_owned(),
        questions: Vec::new(),
        answers: Vec::new(),
    }
}

fn focus_key(scope: &Scope) -> AgentKey {
    scope
        .focus_keys
        .as_ref()
        .and_then(|keys| keys.iter().next().cloned())
        .expect("focus key")
}

#[test]
fn error_entry_projects_as_agent_error_line() {
    let entry = log_entry(
        "claude",
        "receiver",
        TranscriptKind::Error,
        "API Error: Bad Request",
        "2026-06-01T00:00:00Z",
        Some("chat"),
    );
    let identities = build_identities(std::slice::from_ref(&entry));

    let chat = chat_entry_for_log_entry(&entry, &identities, false);

    assert_eq!(chat.from, "@claude");
    assert!(chat.error);
    assert_eq!(chat.text, "API Error: Bad Request");
}

#[test]
fn agent_target_prefers_live_session_over_stale_same_handle() {
    let stale = log_entry(
        "claude",
        "old-sess",
        TranscriptKind::Prompt,
        "old",
        "2026-06-01T00:00:00Z",
        Some("chat"),
    );
    let live = log_entry(
        "claude",
        "live-sess",
        TranscriptKind::Prompt,
        "live",
        "2026-06-01T00:01:00Z",
        Some("chat"),
    );
    let identities = build_identities(&[stale, live.clone()]);
    let live_keys = BTreeSet::from([entry_key(&live)]);

    let scope = resolve_scope(Some("@claude"), None, Some("chat"), &identities, &live_keys)
        .expect("live session resolves");

    assert_eq!(focus_key(&scope), entry_key(&live));
}

#[test]
fn agent_target_uses_latest_when_no_match_is_live() {
    let old = log_entry(
        "claude",
        "old-sess",
        TranscriptKind::Prompt,
        "old",
        "2026-06-01T00:00:00Z",
        Some("chat"),
    );
    let latest = log_entry(
        "claude",
        "latest-sess",
        TranscriptKind::Prompt,
        "latest",
        "2026-06-01T00:02:00Z",
        Some("chat"),
    );
    let identities = build_identities(&[old, latest.clone()]);

    let scope = resolve_scope(
        Some("@claude"),
        None,
        Some("chat"),
        &identities,
        &BTreeSet::new(),
    )
    .expect("latest session resolves");

    assert_eq!(focus_key(&scope), entry_key(&latest));
}

#[test]
fn exact_session_id_resolves_outside_the_current_channel() {
    let exact = log_entry(
        "claude",
        "sess-exact",
        TranscriptKind::Prompt,
        "hello",
        "2026-06-01T00:00:00Z",
        Some("other"),
    );
    let identities = build_identities(std::slice::from_ref(&exact));

    let scope = resolve_scope(
        Some("sess-exact"),
        None,
        Some("current"),
        &identities,
        &BTreeSet::new(),
    )
    .expect("exact session resolves across channels");

    assert_eq!(scope.channel.as_deref(), Some("other"));
    assert_eq!(focus_key(&scope), entry_key(&exact));
    assert!(entry_in_scope(&exact, &scope));
}

#[test]
fn channel_and_all_targets_keep_channel_scope() {
    let identities = HashMap::new();

    let channel = resolve_scope(
        Some("#docs"),
        None,
        Some("main"),
        &identities,
        &BTreeSet::new(),
    )
    .expect("channel scope");
    assert_eq!(channel.channel.as_deref(), Some("docs"));
    assert_eq!(channel.channel_filter.as_deref(), Some("docs"));
    assert!(channel.focus_keys.is_none());

    let all = resolve_scope(
        Some("@all#docs"),
        None,
        Some("main"),
        &identities,
        &BTreeSet::new(),
    )
    .expect("all channel scope");
    assert_eq!(all.channel.as_deref(), Some("docs"));
    assert_eq!(all.channel_filter.as_deref(), Some("docs"));
    assert!(all.focus_keys.is_none());
}
