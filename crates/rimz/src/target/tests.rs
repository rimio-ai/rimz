//! Unit tests for the agent-address grammar.

use jiff::Timestamp;

use super::*;
use crate::agents::AgentStatus;
use crate::ids::{AgentKind, AgentSessionId, MuxName, WorkspaceId};
use crate::pane::PaneRef;

#[test]
fn resolve_prefers_name_ordinal_kind_then_session_prefix() {
    let mut snapshot = empty_snapshot();
    let mut alpha = agent("claude", "session-alpha", Some("main"), "terminal_1");
    alpha.name = Some("lucid-atlas".to_owned());
    alpha.kind_ordinal = Some(1);
    let mut beta = agent("claude", "session-beta", Some("feature/x.y"), "terminal_2");
    beta.name = Some("bright-beacon".to_owned());
    beta.kind_ordinal = Some(2);
    snapshot.agents = vec![alpha, beta];

    assert_eq!(
        resolve_one(&snapshot, "@lucid-atlas", None, None)
            .unwrap()
            .agent_id
            .as_str(),
        "session-alpha"
    );
    assert_eq!(
        resolve_one(&snapshot, "@claude-2", None, None)
            .unwrap()
            .agent_id
            .as_str(),
        "session-beta"
    );
    assert!(matches!(
        resolve_one(&snapshot, "@claude", None, None),
        Err(TargetErr::Ambiguous { .. })
    ));
    // Compact channel form picks the branch.
    assert_eq!(
        resolve_one(&snapshot, "@claude#feature/x.y", None, None)
            .unwrap()
            .agent_id
            .as_str(),
        "session-beta"
    );
    assert_eq!(
        resolve_one(&snapshot, "@session-a", None, None)
            .unwrap()
            .agent_id
            .as_str(),
        "session-alpha"
    );
}

#[test]
fn provisional_launch_id_is_not_a_session_prefix() {
    let mut snapshot = empty_snapshot();
    let mut launch = agent("codex", "launch_queued", Some("main"), "terminal_1");
    launch.name = Some("swift-otter".to_owned());
    launch.kind_ordinal = Some(1);
    snapshot.agents = vec![launch];

    assert!(matches!(
        resolve_one(&snapshot, "@launch_queued", None, None),
        Err(TargetErr::NoMatch { .. })
    ));
    assert_eq!(
        resolve_one(&snapshot, "@swift-otter", None, None)
            .unwrap()
            .agent_id
            .as_str(),
        "launch_queued"
    );
}

#[test]
fn require_mention_demands_the_sigil() {
    // message enforces `@`; pane ids are exempt.
    assert!(require_mention("claude").is_err());
    assert!(require_mention("@claude").is_ok());
    assert!(require_mention("@all").is_ok());
    assert!(require_mention("tmux:%1").is_ok());
    // The removed `selector@worktree` infix is not a mention.
    assert!(require_mention("claude@main").is_err());
}

#[test]
fn old_infix_no_longer_scopes_by_worktree() {
    let mut snapshot = empty_snapshot();
    let agent = agent("claude", "session-alpha", Some("main"), "terminal_1");
    snapshot.agents = vec![agent];
    // `claude@main` is just an unknown name now, not "claude in main".
    assert!(matches!(
        resolve_one(&snapshot, "@claude@main", None, None),
        Err(TargetErr::NoMatch { .. })
    ));
}

#[test]
fn pane_id_bypasses_sigils() {
    let mut snapshot = empty_snapshot();
    let agent = agent("claude", "session-pane", Some("main"), "terminal_7");
    snapshot.agents = vec![agent];
    assert_eq!(
        resolve_one(&snapshot, "zellij:terminal_7", None, Some("other"))
            .unwrap()
            .agent_id
            .as_str(),
        "session-pane"
    );
}

#[test]
fn at_kind_ordinal_never_falls_through_to_session_prefix() {
    let mut snapshot = empty_snapshot();
    let mut agent = agent("codex", "claude-1-session", Some("main"), "terminal_1");
    agent.name = Some("solid-lumen".to_owned());
    snapshot.agents = vec![agent];

    assert!(matches!(
        resolve_one(&snapshot, "@claude-1", None, None),
        Err(TargetErr::NoMatch { .. })
    ));
}

#[test]
fn at_kind_fans_out_but_resolve_one_is_ambiguous() {
    let mut snapshot = empty_snapshot();
    let mut one = agent("claude", "session-1", Some("main"), "terminal_1");
    one.kind_ordinal = Some(1);
    let mut two = agent("claude", "session-2", Some("main"), "terminal_2");
    two.kind_ordinal = Some(2);
    let codex = agent("codex", "session-3", Some("main"), "terminal_3");
    snapshot.agents = vec![one, two, codex];

    let many = resolve_many(&snapshot, "@claude", None, None).unwrap();
    assert_eq!(many.len(), 2);
    assert!(matches!(
        resolve_one(&snapshot, "@claude", None, None),
        Err(TargetErr::Ambiguous { .. })
    ));
    // A specific ordinal stays single.
    assert_eq!(
        resolve_one(&snapshot, "@claude-2", None, None)
            .unwrap()
            .agent_id
            .as_str(),
        "session-2"
    );
}

#[test]
fn at_all_fans_to_the_channel_only() {
    let mut snapshot = empty_snapshot();
    let feat_claude = agent("claude", "session-a", Some("feat"), "terminal_1");
    let feat_codex = agent("codex", "session-b", Some("feat"), "terminal_2");
    let main_claude = agent("claude", "session-c", Some("main"), "terminal_3");
    snapshot.agents = vec![feat_claude, feat_codex, main_claude];

    let ids: Vec<&str> = resolve_many(&snapshot, "@all", None, Some("feat"))
        .unwrap()
        .iter()
        .map(|agent| agent.agent_id.as_str())
        .collect();
    assert_eq!(ids, vec!["session-a", "session-b"]);
}

#[test]
fn current_channel_default_applies() {
    let mut snapshot = empty_snapshot();
    let feat = agent("claude", "session-feat", Some("feat"), "terminal_1");
    let main = agent("claude", "session-main", Some("main"), "terminal_2");
    snapshot.agents = vec![feat, main];

    assert_eq!(
        resolve_one(&snapshot, "@claude", None, Some("feat"))
            .unwrap()
            .agent_id
            .as_str(),
        "session-feat"
    );
}

#[test]
fn exact_session_id_bypasses_current_channel() {
    let mut snapshot = empty_snapshot();
    let feat = agent("claude", "session-feat", Some("feat"), "terminal_1");
    let main = agent("claude", "session-main", Some("main"), "terminal_2");
    snapshot.agents = vec![feat, main];

    assert_eq!(
        resolve_one(&snapshot, "@session-feat", None, Some("main"))
            .unwrap()
            .agent_id
            .as_str(),
        "session-feat"
    );
}

#[test]
fn none_current_channel_means_all_channels() {
    let mut snapshot = empty_snapshot();
    let feat = agent("claude", "session-feat", Some("feat"), "terminal_1");
    let main = agent("claude", "session-main", Some("main"), "terminal_2");
    snapshot.agents = vec![feat, main];

    // No current channel must not silently narrow — both are visible.
    assert!(matches!(
        resolve_one(&snapshot, "@claude", None, None),
        Err(TargetErr::Ambiguous { .. })
    ));
}

#[test]
fn in_place_team_channel_uses_directory_and_team() {
    let mut snapshot = empty_snapshot();
    let mut planner = agent("claude", "session-planner", None, "terminal_1");
    planner.worktree_path = Some("/code/team-channel".to_owned());
    planner.team = Some("pcr".to_owned());
    let mut coder = agent("codex", "session-coder", None, "terminal_2");
    coder.worktree_path = Some("/code/team-channel".to_owned());
    coder.team = Some("pcr".to_owned());
    let mut other = agent("codex", "session-other", None, "terminal_3");
    other.worktree_path = Some("/code/team-channel".to_owned());
    other.team = Some("docs".to_owned());
    snapshot.agents = vec![planner, coder, other];

    assert_eq!(
        agent_channel(&snapshot.agents[0]).as_deref(),
        Some("team-channel/pcr")
    );
    assert_eq!((&snapshot.agents[0]).channel_label(), "team-channel/pcr");
    assert!((&snapshot.agents[0]).in_worktree("team-channel/pcr"));
    assert!((&snapshot.agents[0]).in_worktree("team-channel"));

    let ids: Vec<&str> = resolve_many(&snapshot, "@all", None, Some("team-channel/pcr"))
        .unwrap()
        .iter()
        .map(|agent| agent.agent_id.as_str())
        .collect();
    assert_eq!(ids, vec!["session-planner", "session-coder"]);
}

#[test]
fn branch_channel_wins_over_team() {
    let mut team_agent = agent("claude", "session-feat", Some("feat/auth"), "terminal_1");
    team_agent.team = Some("pcr".to_owned());

    assert_eq!(agent_channel(&team_agent).as_deref(), Some("feat/auth"));
    assert_eq!((&team_agent).channel_label(), "feat/auth");
    assert!((&team_agent).in_worktree("feat/auth"));
    assert!(!(&team_agent).in_worktree("auth/pcr"));
}

#[test]
fn zero_in_channel_but_matches_elsewhere() {
    let mut snapshot = empty_snapshot();
    let main_codex = agent("codex", "session-codex", Some("main"), "terminal_1");
    snapshot.agents = vec![main_codex];

    let err = resolve_one(&snapshot, "@codex#cli-docs", None, None).unwrap_err();
    let message = err.to_string();
    assert!(
        matches!(err, TargetErr::NoMatchInChannel { .. }),
        "expected a channel-scoped miss: {message}"
    );
    assert!(
        message.contains("`main`"),
        "names the real channel: {message}"
    );
}

#[test]
fn rejects_conflicting_channels() {
    let snapshot = empty_snapshot();
    assert!(matches!(
        resolve_one(&snapshot, "@claude#main", Some("docs"), None),
        Err(TargetErr::WorktreeMismatch { .. })
    ));
}

#[test]
fn splits_channel_at_first_hash() {
    let mut snapshot = empty_snapshot();
    let mut agent = agent("claude", "session-alpha", Some("feature/x.y"), "terminal_1");
    agent.name = Some("lucid-atlas".to_owned());
    snapshot.agents = vec![agent];

    assert_eq!(
        resolve_one(&snapshot, "@lucid-atlas#feature/x.y", None, None)
            .unwrap()
            .agent_id
            .as_str(),
        "session-alpha"
    );
}

#[test]
fn no_match_points_to_the_list_without_dumping_the_roster() {
    let mut snapshot = empty_snapshot();
    let names = ["calm-fox", "bold-pine", "warm-dune"];
    snapshot.agents = names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let mut agent = agent(
                "claude",
                &format!("session-{i}"),
                Some("main"),
                "terminal_1",
            );
            agent.name = Some((*name).to_owned());
            agent
        })
        .collect();

    let err = resolve_one(&snapshot, "@missing-name", None, None).unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("run `rimz agents list`"),
        "points at the list: {message}"
    );
    assert!(
        names.iter().all(|name| !message.contains(name)),
        "no roster names leak in: {message}"
    );
}

#[test]
fn no_match_suggests_close_pet_names() {
    let mut snapshot = empty_snapshot();
    let mut close = agent("claude", "session-1", Some("main"), "terminal_1");
    close.name = Some("otter-swift".to_owned());
    let mut far = agent("claude", "session-2", Some("main"), "terminal_2");
    far.name = Some("calm-fox".to_owned());
    snapshot.agents = vec![close, far];

    let message = resolve_one(&snapshot, "@swift-otter", None, None)
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("did you mean") && message.contains("otter-swift"),
        "suggests the token-sharing name: {message}"
    );
    assert!(
        !message.contains("calm-fox"),
        "skips unrelated names: {message}"
    );
}

#[test]
fn handle_is_shortest_unambiguous_and_round_trips() {
    let mut snapshot = empty_snapshot();
    let mut solo = agent("claude", "session-solo", Some("docs"), "terminal_1");
    solo.kind_ordinal = Some(1);
    let mut a = agent("claude", "session-a", Some("main"), "terminal_2");
    a.kind_ordinal = Some(1);
    let mut b = agent("claude", "session-b", Some("main"), "terminal_3");
    b.kind_ordinal = Some(2);
    snapshot.agents = vec![solo, a, b];
    let peers: Vec<&AgentState> = snapshot.agents.iter().collect();

    // The only claude in `docs` reads as the bare kind; two claudes sharing
    // `main` each grow the disambiguating ordinal.
    assert_eq!(agent_handle(peers[0], &peers, true), "@claude#docs");
    assert_eq!(agent_handle(peers[1], &peers, true), "@claude-1#main");
    assert_eq!(agent_handle(peers[2], &peers, true), "@claude-2#main");
    // The grouped form drops the channel — it is the section header.
    assert_eq!(agent_handle(peers[1], &peers, false), "@claude-1");

    // Every rendered handle resolves back to exactly its own agent.
    for &agent in &peers {
        let handle = agent_handle(agent, &peers, true);
        assert_eq!(
            resolve_one(&snapshot, &handle, None, None)
                .unwrap()
                .agent_id
                .as_str(),
            agent.agent_id.as_str(),
            "{handle} round-trips to its agent"
        );
    }
}

#[test]
fn handle_falls_back_to_petname_without_an_ordinal() {
    let mut snapshot = empty_snapshot();
    // Two codex sessions share a channel but carry no ordinal — the stable
    // petname is the only thing that still names one.
    let mut a = agent("codex", "session-a", Some("main"), "terminal_1");
    a.name = Some("swift-otter".to_owned());
    let mut b = agent("codex", "session-b", Some("main"), "terminal_2");
    b.name = Some("brave-lark".to_owned());
    snapshot.agents = vec![a, b];
    let peers: Vec<&AgentState> = snapshot.agents.iter().collect();

    assert_eq!(agent_handle(peers[0], &peers, false), "@swift-otter");
    assert_eq!(agent_handle(peers[1], &peers, false), "@brave-lark");
}

#[test]
fn channelless_handle_disambiguates_against_same_kind_elsewhere() {
    let mut snapshot = empty_snapshot();
    // A claude running outside any worktree, beside a claude in `main`. The
    // loose one has no `#channel` suffix to scope a bare `@claude`, so its
    // handle must fall to the globally-unique petname — and round-trip.
    let mut loose = agent("claude", "session-loose", None, "terminal_1");
    loose.name = Some("lucid-atlas".to_owned());
    loose.kind_ordinal = Some(1);
    let mut in_main = agent("claude", "session-main", Some("main"), "terminal_2");
    in_main.kind_ordinal = Some(2);
    snapshot.agents = vec![loose, in_main];
    let peers: Vec<&AgentState> = snapshot.agents.iter().collect();

    assert_eq!(agent_handle(peers[0], &peers, true), "@lucid-atlas");
    // The in-main claude is the only one of its kind there — bare kind + channel.
    assert_eq!(agent_handle(peers[1], &peers, true), "@claude#main");
    for &one in &peers {
        let handle = agent_handle(one, &peers, true);
        assert_eq!(
            resolve_one(&snapshot, &handle, None, None)
                .unwrap()
                .agent_id
                .as_str(),
            one.agent_id.as_str(),
            "{handle} round-trips to its agent"
        );
    }
}

#[test]
fn channelless_handle_falls_back_to_session_without_a_petname() {
    let mut snapshot = empty_snapshot();
    // No petname and a same-kind agent elsewhere: the session id is the only
    // address left that still names exactly the loose agent.
    let loose = agent("claude", "session-loose", None, "terminal_1");
    let in_main = agent("claude", "session-main", Some("main"), "terminal_2");
    snapshot.agents = vec![loose, in_main];
    let peers: Vec<&AgentState> = snapshot.agents.iter().collect();

    let handle = agent_handle(peers[0], &peers, true);
    assert_eq!(handle, "@session-loose");
    assert_eq!(
        resolve_one(&snapshot, &handle, None, None)
            .unwrap()
            .agent_id
            .as_str(),
        "session-loose"
    );
}

#[test]
fn profile_resolves_as_a_profile_handle_and_renders_first() {
    let mut snapshot = empty_snapshot();
    let mut planner = agent("claude", "session-planner", Some("auth"), "terminal_1");
    planner.profile = Some("planner".to_owned());
    planner.kind_ordinal = Some(1);
    let mut explorer = agent("claude", "session-explorer", Some("auth"), "terminal_2");
    explorer.profile = Some("explorer".to_owned());
    explorer.kind_ordinal = Some(2);
    snapshot.agents = vec![planner, explorer];
    let peers: Vec<&AgentState> = snapshot.agents.iter().collect();

    // The profile names exactly its agent; the shared kind names both, so it is
    // an explicit ambiguity — `@claude` matches the profileed claudes too.
    assert_eq!(
        resolve_one(&snapshot, "@planner", None, None)
            .unwrap()
            .agent_id
            .as_str(),
        "session-planner"
    );
    assert!(matches!(
        resolve_one(&snapshot, "@claude", None, None),
        Err(TargetErr::Ambiguous { .. })
    ));
    // The handle prefers the profile and round-trips back to its own agent.
    assert_eq!(agent_handle(peers[0], &peers, true), "@planner#auth");
    assert_eq!(agent_handle(peers[1], &peers, true), "@explorer#auth");
    for &one in &peers {
        let handle = agent_handle(one, &peers, true);
        assert_eq!(
            resolve_one(&snapshot, &handle, None, None)
                .unwrap()
                .agent_id,
            one.agent_id,
            "{handle} round-trips"
        );
    }
}

#[test]
fn kind_name_profile_handle_uses_round_tripping_disambiguator() {
    let mut snapshot = empty_snapshot();
    let mut bare = agent("claude", "session-bare", Some("main"), "terminal_1");
    bare.kind_ordinal = Some(1);
    let mut profiled = agent("claude", "session-profile", Some("main"), "terminal_2");
    profiled.profile = Some("claude".to_owned());
    profiled.kind_ordinal = Some(2);
    snapshot.agents = vec![bare, profiled];
    let peers: Vec<&AgentState> = snapshot.agents.iter().collect();

    assert!(matches!(
        resolve_one(&snapshot, "@claude#main", None, None),
        Err(TargetErr::Ambiguous { .. })
    ));
    let handle = agent_handle(peers[1], &peers, true);
    assert_eq!(handle, "@claude-2#main");
    assert_eq!(
        resolve_one(&snapshot, &handle, None, None)
            .unwrap()
            .agent_id
            .as_str(),
        "session-profile"
    );
}

#[test]
fn shared_profile_degrades_to_the_kind_ordinal_handle() {
    let mut snapshot = empty_snapshot();
    // Two `planner`s share one channel: the profile is no longer unique, so the
    // handle falls back to the disambiguating kind ordinal.
    let mut a = agent("claude", "session-a", Some("auth"), "terminal_1");
    a.profile = Some("planner".to_owned());
    a.kind_ordinal = Some(1);
    let mut b = agent("claude", "session-b", Some("auth"), "terminal_2");
    b.profile = Some("planner".to_owned());
    b.kind_ordinal = Some(2);
    snapshot.agents = vec![a, b];
    let peers: Vec<&AgentState> = snapshot.agents.iter().collect();

    assert!(matches!(
        resolve_one(&snapshot, "@planner", None, None),
        Err(TargetErr::Ambiguous { .. })
    ));
    assert_eq!(agent_handle(peers[0], &peers, true), "@claude-1#auth");
    assert_eq!(agent_handle(peers[1], &peers, true), "@claude-2#auth");
    for &one in &peers {
        let handle = agent_handle(one, &peers, true);
        assert_eq!(
            resolve_one(&snapshot, &handle, None, None)
                .unwrap()
                .agent_id,
            one.agent_id,
            "{handle} round-trips"
        );
    }
}

#[test]
fn role_resolves_before_profile_and_renders_first_when_unique() {
    let mut snapshot = empty_snapshot();
    let mut planner = agent("claude", "session-planner", Some("auth"), "terminal_1");
    planner.profile = Some("claude-planner".to_owned());
    planner.role = Some("planner".to_owned());
    planner.kind_ordinal = Some(1);
    let mut coder = agent("codex", "session-coder", Some("auth"), "terminal_2");
    coder.profile = Some("codex-coder".to_owned());
    coder.role = Some("coder".to_owned());
    coder.kind_ordinal = Some(1);
    snapshot.agents = vec![planner, coder];
    let peers: Vec<&AgentState> = snapshot.agents.iter().collect();

    assert_eq!(
        resolve_one(&snapshot, "@coder#auth", None, None)
            .unwrap()
            .agent_id
            .as_str(),
        "session-coder"
    );
    assert_eq!(agent_handle(peers[0], &peers, true), "@planner#auth");
    assert_eq!(agent_handle(peers[1], &peers, true), "@coder#auth");
}

#[test]
fn shared_role_requires_fanout_and_degrades_to_profile_or_ordinal() {
    let mut snapshot = empty_snapshot();
    let mut a = agent("claude", "session-a", Some("auth"), "terminal_1");
    a.profile = Some("planner-a".to_owned());
    a.role = Some("planner".to_owned());
    a.kind_ordinal = Some(1);
    let mut b = agent("claude", "session-b", Some("auth"), "terminal_2");
    b.profile = Some("planner-b".to_owned());
    b.role = Some("planner".to_owned());
    b.kind_ordinal = Some(2);
    snapshot.agents = vec![a, b];
    let peers: Vec<&AgentState> = snapshot.agents.iter().collect();

    assert_eq!(
        resolve_many(&snapshot, "@planner", None, Some("auth"))
            .unwrap()
            .len(),
        2
    );
    assert!(matches!(
        resolve_one(&snapshot, "@planner", None, Some("auth")),
        Err(TargetErr::Ambiguous { .. })
    ));
    assert_eq!(agent_handle(peers[0], &peers, true), "@planner-a#auth");
    assert_eq!(agent_handle(peers[1], &peers, true), "@planner-b#auth");
}

#[test]
fn sender_prefix_skips_human_messages() {
    let peers: Vec<&AgentState> = Vec::new();
    assert_eq!(
        sender_prefix(&crate::message::MessageSender::Human, &peers, None),
        None
    );
}

#[test]
fn sender_prefix_uses_live_handle_and_channel_only_when_crossing_channels() {
    let mut snapshot = empty_snapshot();
    let mut sender = agent("claude", "session-sender", Some("docs"), "terminal_1");
    sender.name = Some("lucid-atlas".to_owned());
    let target = agent("codex", "session-target", Some("docs"), "terminal_2");
    snapshot.agents = vec![sender, target];
    let peers: Vec<&AgentState> = snapshot.agents.iter().collect();
    let sender = crate::message::MessageSender::Agent {
        kind: AgentKind::new_unchecked("claude"),
        name: Some("lucid-atlas".to_owned()),
        profile: None,
        role: None,
        channel: Some("fallback".to_owned()),
    };

    assert_eq!(
        sender_prefix(&sender, &peers, Some("docs")).unwrap(),
        "from @claude: "
    );
    assert_eq!(
        sender_prefix(&sender, &peers, Some("main")).unwrap(),
        "from @claude#docs: "
    );
}

#[test]
fn sender_prefix_live_handle_disambiguates_same_kind_peers() {
    let mut snapshot = empty_snapshot();
    let mut one = agent("claude", "session-a", Some("main"), "terminal_1");
    one.name = Some("calm-fox".to_owned());
    one.kind_ordinal = Some(1);
    let mut two = agent("claude", "session-b", Some("main"), "terminal_2");
    two.name = Some("bright-lark".to_owned());
    two.kind_ordinal = Some(2);
    snapshot.agents = vec![one, two];
    let peers: Vec<&AgentState> = snapshot.agents.iter().collect();
    let sender = crate::message::MessageSender::Agent {
        kind: AgentKind::new_unchecked("claude"),
        name: Some("bright-lark".to_owned()),
        profile: None,
        role: None,
        channel: Some("main".to_owned()),
    };

    assert_eq!(
        sender_prefix(&sender, &peers, Some("main")).unwrap(),
        "from @claude-2: "
    );
}

#[test]
fn sender_prefix_falls_back_to_stored_identity_when_sender_is_absent() {
    let peers: Vec<&AgentState> = Vec::new();
    let sender = crate::message::MessageSender::Agent {
        kind: AgentKind::new_unchecked("codex"),
        name: Some("lucid-atlas".to_owned()),
        profile: Some("reviewer".to_owned()),
        role: None,
        channel: Some("docs".to_owned()),
    };

    assert_eq!(
        sender_prefix(&sender, &peers, Some("main")).unwrap(),
        "from @lucid-atlas#docs: "
    );
    assert_eq!(
        sender_prefix(&sender, &peers, Some("docs")).unwrap(),
        "from @lucid-atlas: "
    );
}

#[test]
fn sender_prefix_fallback_keeps_petname_when_alias_matches_another_peer() {
    let mut snapshot = empty_snapshot();
    let mut other_planner = agent("claude", "session-other", Some("auth"), "terminal_2");
    other_planner.name = Some("bright-lark".to_owned());
    other_planner.profile = Some("planner".to_owned());
    snapshot.agents = vec![other_planner];
    let peers: Vec<&AgentState> = snapshot.agents.iter().collect();
    let sender = crate::message::MessageSender::Agent {
        kind: AgentKind::new_unchecked("claude"),
        name: Some("calm-fox".to_owned()),
        profile: Some("planner".to_owned()),
        role: None,
        channel: Some("auth".to_owned()),
    };

    assert_eq!(
        sender_prefix(&sender, &peers, Some("auth")).unwrap(),
        "from @calm-fox: "
    );
}

#[test]
fn is_broadcast_only_for_at_all() {
    assert!(is_broadcast("@all"));
    assert!(is_broadcast("@all#main"));
    assert!(is_broadcast("all"));
    assert!(!is_broadcast("@claude"));
    assert!(!is_broadcast("@planner"));
    assert!(!is_broadcast("tmux:%1"));
}

#[test]
fn create_mention_extracts_type_handles_but_not_panes_or_broadcast() {
    // A kind/profile mention yields its selector and resolved channel.
    let create = create_mention("@planner#auth", None, None)
        .unwrap()
        .expect("creatable mention");
    assert_eq!(create.selector, "planner");
    assert_eq!(create.channel.as_deref(), Some("auth"));
    // The current channel fills in when none is named.
    let create = create_mention("@codex", None, Some("main"))
        .unwrap()
        .expect("creatable mention");
    assert_eq!(create.selector, "codex");
    assert_eq!(create.channel.as_deref(), Some("main"));
    // An instance handle still returns its selector — the CLI refuses it by
    // recognising it is neither a kind nor an profile.
    let create = create_mention("@claude-2", None, Some("main"))
        .unwrap()
        .expect("mention");
    assert_eq!(create.selector, "claude-2");
    // A pane address and the broadcast handle cannot create.
    assert!(create_mention("tmux:%1", None, None).unwrap().is_none());
    assert!(
        create_mention("@all", None, Some("main"))
            .unwrap()
            .is_none()
    );
}

fn empty_snapshot() -> SidebarSnapshot {
    SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-target-test")),
        Vec::new(),
        Vec::new(),
        Timestamp::now(),
    )
}

fn agent(kind: &str, id: &str, branch: Option<&str>, raw_pane: &str) -> AgentState {
    let now = Timestamp::now();
    AgentState {
        agent_id: AgentSessionId::from(id),
        kind: AgentKind::new_unchecked(kind),
        name: None,
        kind_ordinal: None,
        profile: None,
        role: None,
        team: None,
        status: AgentStatus::Idle,
        phase: crate::agents::TurnPhase::Idle,
        pane: Some(PaneRef::from_id(PaneId::from_parts(
            MuxName::Zellij,
            raw_pane,
        ))),
        agent_pid: None,
        agent_process_start: None,
        runtime_owner: None,
        parent_agent_id: None,
        worktree_path: branch.map(|branch| format!("/repo/{branch}")),
        worktree_branch: branch.map(ToOwned::to_owned),
        task: None,
        prompt: None,
        description: None,
        transcript_path: None,
        recent_prompts: Vec::new(),
        model: None,
        effort: None,
        context_pct: None,
        context_window: None,
        total_tokens: None,
        cache_read_input_tokens: None,
        cache_write_input_tokens: None,
        fresh_input_tokens: None,
        output_tokens: None,
        context: None,
        subagent_description: None,
        subagent_started_at: None,
        turn_started_at: None,
        compacting_since: None,
        compaction_count: 0,
        last_seen: now,
        last_activity: now,
        registered_at: Some(now),
    }
}

/// A lazy (sessionless) agent pane as the producer would emit it into
/// `agent_panes` — kind and pane only.
fn lazy_pane(kind: &str, worktree_path: &str, raw_pane: &str) -> PaneAgent {
    PaneAgent {
        kind: AgentKind::new_unchecked(kind),
        kind_ordinal: None,
        name: None,
        profile: None,
        role: None,
        team: None,
        agent_id: None,
        pane_id: PaneId::from_parts(MuxName::Zellij, raw_pane),
        worktree_path: Some(worktree_path.to_owned()),
        worktree_branch: None,
    }
}

/// A bound agent pane: a session with its pet name, ordinal, and the pane the
/// producer bound it to (which may differ from the session's own record).
fn bound_pane(
    kind: &str,
    ordinal: u32,
    name: &str,
    session: &str,
    branch: &str,
    raw_pane: &str,
) -> PaneAgent {
    PaneAgent {
        kind: AgentKind::new_unchecked(kind),
        kind_ordinal: Some(ordinal),
        name: Some(name.to_owned()),
        profile: None,
        role: None,
        team: None,
        agent_id: Some(AgentSessionId::from(session)),
        pane_id: PaneId::from_parts(MuxName::Zellij, raw_pane),
        worktree_path: Some(format!("/repo/{branch}")),
        worktree_branch: Some(branch.to_owned()),
    }
}

#[test]
fn at_kind_matches_a_lazy_agent_pane() {
    // A bare codex pane (no session yet) is a steer target by kind.
    let mut snapshot = empty_snapshot();
    snapshot.agent_panes = vec![lazy_pane("codex", "/repo/shimmer-effect", "terminal_170")];

    let targets = resolve_targets(&snapshot, "@codex", None, Some("shimmer-effect")).unwrap();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].kind.as_str(), "codex");
    assert_eq!(targets[0].agent_id, None);
    assert_eq!(targets[0].pane_id.to_string(), "zellij:terminal_170");
}

#[test]
fn lazy_panes_skip_ordinal_pet_name_and_session_selectors() {
    // A lazy pane carries no ordinal, pet name, or session id — only
    // `@kind`/`@all` reach it.
    let mut snapshot = empty_snapshot();
    snapshot.agent_panes = vec![lazy_pane("codex", "/repo/shimmer-effect", "terminal_170")];

    assert!(matches!(
        resolve_targets(&snapshot, "@codex-1", None, Some("shimmer-effect")),
        Err(TargetErr::NoMatch { .. }) | Err(TargetErr::NoMatchInChannel { .. })
    ));
    assert!(matches!(
        resolve_targets(&snapshot, "@swift-otter", None, Some("shimmer-effect")),
        Err(TargetErr::NoMatch { .. })
    ));
}

#[test]
fn bound_pane_reaches_its_producer_bound_pane_by_petname_and_ordinal() {
    // A cwd-bound session is steerable by pet name, ordinal, and session
    // prefix, each landing on the pane the producer bound — even when the
    // rollup session itself carries no stamped pane.
    let mut snapshot = empty_snapshot();
    snapshot.agent_panes = vec![bound_pane(
        "codex",
        1,
        "swift-otter",
        "session-x",
        "shimmer-effect",
        "terminal_5",
    )];

    for raw in ["@swift-otter", "@codex-1", "@session-x"] {
        let targets = resolve_targets(&snapshot, raw, None, Some("shimmer-effect")).unwrap();
        assert_eq!(targets.len(), 1, "{raw}");
        assert_eq!(targets[0].pane_id.to_string(), "zellij:terminal_5", "{raw}");
        assert_eq!(
            targets[0].agent_id.as_ref().map(AgentSessionId::as_str),
            Some("session-x"),
            "{raw}"
        );
    }
}

#[test]
fn management_resolution_never_sees_agent_panes() {
    // resolve_many resolves the rollup; agent_panes never leaks into it.
    let mut snapshot = empty_snapshot();
    snapshot.agent_panes = vec![lazy_pane("codex", "/repo/shimmer-effect", "terminal_170")];

    assert!(matches!(
        resolve_many(&snapshot, "@codex", None, Some("shimmer-effect")),
        Err(TargetErr::NoMatch { .. }) | Err(TargetErr::NoMatchInChannel { .. })
    ));
}

#[test]
fn at_all_fans_to_in_channel_panes_only() {
    let mut snapshot = empty_snapshot();
    snapshot.agent_panes = vec![
        bound_pane(
            "claude",
            1,
            "calm-fox",
            "session-c",
            "shimmer-effect",
            "terminal_1",
        ),
        lazy_pane("codex", "/repo/shimmer-effect", "terminal_170"),
        lazy_pane("codex", "/repo/other", "terminal_9"),
    ];

    let kinds: Vec<String> = resolve_targets(&snapshot, "@all", None, Some("shimmer-effect"))
        .unwrap()
        .iter()
        .map(|target| target.kind.to_string())
        .collect();
    // bound claude + the in-channel lazy codex; the other channel's pane is out.
    assert_eq!(kinds.len(), 2);
    assert!(kinds.contains(&"claude".to_owned()));
    assert!(kinds.contains(&"codex".to_owned()));
}
