use super::scope::entry_key;
use super::*;
use std::collections::HashMap;

use rimz::ids::{AgentKind, AgentSessionId};
use rimz::transcript::{TranscriptEntry, TranscriptKind};

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
    let mut entry = TranscriptEntry::new(
        ts(at),
        AgentKind::new_unchecked(kind),
        AgentSessionId::from(session_id),
        entry,
        text.to_owned(),
    );
    entry.channel = channel.map(ToOwned::to_owned);
    entry
}

fn focus_key(scope: &Scope) -> AgentKey {
    scope
        .focus_keys
        .as_ref()
        .and_then(|keys| keys.iter().next().cloned())
        .expect("focus key")
}

fn snapshot(agents: Vec<rimz::agents::AgentState>) -> rimz::store::snapshot::SidebarSnapshot {
    rimz::store::snapshot::SidebarSnapshot::build_with_agents(
        rimz::WorkspaceId::from_project_root(std::path::Path::new("/repo")),
        agents,
        ts("2026-06-01T00:00:00Z"),
    )
}

fn context(channel: &str) -> rimz::address::AddressContext {
    rimz::address::AddressContext {
        channel: Some(channel.to_owned()),
        origin: rimz::address::ChannelOrigin::Stamped,
        project_root: "/repo".into(),
    }
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
    let mut agent = rimz::testkit::agent_state("claude", "live-sess", live.at);
    agent.channel = Some("chat".to_owned());
    let snapshot = snapshot(vec![agent]);
    let scope = resolve_scope(
        Some("@claude"),
        None,
        &context("chat"),
        &identities,
        None,
        &snapshot,
    )
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
        &context("chat"),
        &identities,
        None,
        &snapshot(vec![]),
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
        &context("current"),
        &identities,
        None,
        &snapshot(vec![]),
    )
    .expect("exact session resolves across channels");

    assert_eq!(scope.channel.as_deref(), Some("other"));
    assert_eq!(focus_key(&scope), entry_key(&exact));
    assert!(entry_in_scope(&exact, &scope, &identities));
}

#[test]
fn channel_and_all_targets_keep_channel_scope() {
    let identities = HashMap::new();

    let channel = resolve_scope(
        Some("#docs"),
        None,
        &context("main"),
        &identities,
        None,
        &snapshot(vec![]),
    )
    .expect("channel scope");
    assert_eq!(channel.channel.as_deref(), Some("docs"));
    assert_eq!(channel.channel_filter.as_deref(), Some("docs"));
    assert!(channel.focus_keys.is_none());

    let all = resolve_scope(
        Some("@all#docs"),
        None,
        &context("main"),
        &identities,
        None,
        &snapshot(vec![]),
    )
    .expect("all channel scope");
    assert_eq!(all.channel.as_deref(), Some("docs"));
    assert_eq!(all.channel_filter.as_deref(), Some("docs"));
    assert!(all.focus_keys.is_none());
}

#[test]
fn degenerate_agent_targets_use_resolver_errors() {
    let identities = HashMap::new();

    for raw in ["@", "foo:bar"] {
        let err = resolve_scope(
            Some(raw),
            None,
            &context("main"),
            &identities,
            None,
            &snapshot(vec![]),
        )
        .expect_err("degenerate target should not parse as resolver target");
        assert!(
            matches!(
                err.downcast_ref::<rimz::address::TargetErr>(),
                Some(
                    rimz::address::TargetErr::NoMatch { .. }
                        | rimz::address::TargetErr::InvalidPaneId(_)
                )
            ),
            "{err}"
        );
    }
}
