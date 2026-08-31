//! Unit tests for the agent-address grammar.

use jiff::Timestamp;

use super::*;
use crate::agents::AgentStatus;
use crate::ids::{AgentKind, AgentSessionId, MuxName, WorkspaceId};
use crate::message::MessageSender;
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
    // Compact channel form picks the worktree basename.
    assert_eq!(
        resolve_one(&snapshot, "@claude#x.y", None, None)
            .unwrap()
            .agent_id
            .as_str(),
        "session-beta"
    );
    assert_eq!(
        resolve_one(&snapshot, "@lucid-atlas#main", None, None)
            .unwrap()
            .agent_id
            .as_str(),
        "session-alpha"
    );
    assert_eq!(
        resolve_one(&snapshot, "@bright-beacon#x.y", None, None)
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
    // `claude@main` is just an unknown name now, not "claude in main".
    assert!(matches!(
        resolve_one(&snapshot, "@claude@main", None, None),
        Err(TargetErr::NoMatch { .. })
    ));
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
fn broadcast_and_group_prefix_selectors() {
    assert!(is_broadcast("@all"));
    assert!(is_broadcast("@all#main"));
    assert!(is_broadcast("all"));
    assert!(!is_broadcast("@claude"));
    assert!(!is_broadcast("@planner"));
    assert!(!is_broadcast("tmux:%1"));

    assert_eq!(group_prefixed("@all", "hi"), "@all, hi");
    assert_eq!(group_prefixed("@all#main", "hi"), "@all, hi");
    assert_eq!(group_prefixed("@claude", "go"), "@claude, go");
    assert_eq!(group_prefixed("@claude#design", "go"), "@claude, go");
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
fn pane_owner_shadows_co_resident_session_from_every_address() {
    let mut snapshot = empty_snapshot();
    let mut older = agent("codex", "session-older", Some("main"), "terminal_1");
    older.name = Some("coder-agent".to_owned());
    older.kind_ordinal = Some(1);
    older.role = Some("coder".to_owned());
    older.origin = Some(crate::agents::SessionOrigin::Fresh);
    let mut owner = agent("codex", "session-owner", Some("main"), "terminal_1");
    owner.name = Some("coder-agent".to_owned());
    owner.kind_ordinal = Some(2);
    owner.role = Some("coder".to_owned());
    owner.origin = Some(crate::agents::SessionOrigin::Fresh);
    let mut hidden_fork = agent("codex", "session-fork", Some("main"), "terminal_1");
    hidden_fork.name = Some("coder-agent".to_owned());
    hidden_fork.kind_ordinal = Some(3);
    hidden_fork.role = Some("coder".to_owned());
    hidden_fork.origin = Some(crate::agents::SessionOrigin::Forked);
    snapshot.agents = vec![older, owner, hidden_fork];
    let mut pane = bound_pane("codex", 2, "owner", "session-owner", "main", "terminal_1");
    pane.role = Some("coder".to_owned());
    snapshot.agent_panes = vec![pane];

    let matches = resolve_many(&snapshot, "@coder", None, Some("main")).unwrap();
    assert_eq!(
        matches
            .iter()
            .map(|agent| agent.agent_id.as_str())
            .collect::<Vec<_>>(),
        ["session-owner"]
    );
    assert!(matches!(
        resolve_one(&snapshot, "@session-older", None, None),
        Err(TargetErr::NoMatch { .. })
    ));
    assert_eq!(
        resolve_many(&snapshot, "@all", None, Some("main"))
            .unwrap()
            .len(),
        1
    );

    let unfiltered = snapshot.root_agents().collect::<Vec<_>>();
    assert_eq!(
        agent_handle(&snapshot.agents[1], &unfiltered, false),
        "@codex-2"
    );
    let peers = addressable_agents(&snapshot);
    assert_eq!(agent_handle(peers[0], &peers, false), "@coder");
    let sender = MessageSender::Agent {
        kind: AgentKind::new_unchecked("codex"),
        name: Some("coder-agent".to_owned()),
        profile: None,
        role: Some("coder".to_owned()),
        channel: Some("main".to_owned()),
    };
    assert_eq!(
        message_header(&sender, &peers, Some("main")).as_deref(),
        Some("Type: AGENT_MESSAGE\nFrom: @coder\nContent:\n")
    );
}

#[test]
fn pane_shadowing_preserves_distinct_and_closed_agents() {
    let mut snapshot = empty_snapshot();
    let mut first = agent("codex", "session-first", Some("main"), "terminal_1");
    first.role = Some("coder".to_owned());
    let mut second = agent("codex", "session-second", Some("main"), "terminal_2");
    second.role = Some("coder".to_owned());
    let mut closed = agent("codex", "session-closed", Some("main"), "terminal_closed");
    closed.role = Some("coder".to_owned());
    snapshot.agents = vec![first, second, closed];
    snapshot.agent_panes = vec![
        bound_pane("codex", 1, "first", "session-first", "main", "terminal_1"),
        bound_pane("codex", 2, "second", "session-second", "main", "terminal_2"),
        lazy_pane("codex", "/repo/main", "terminal_lazy"),
    ];

    assert_eq!(
        resolve_many(&snapshot, "@coder", None, Some("main"))
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        resolve_one(&snapshot, "@session-closed", None, None)
            .unwrap()
            .agent_id
            .as_str(),
        "session-closed"
    );
}

#[test]
fn current_channel_scoping_rules() {
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
    assert_eq!(
        resolve_one(&snapshot, "@session-feat", None, Some("main"))
            .unwrap()
            .agent_id
            .as_str(),
        "session-feat"
    );
    // No current channel must not silently narrow — both are visible.
    assert!(matches!(
        resolve_one(&snapshot, "@claude", None, None),
        Err(TargetErr::Ambiguous { .. })
    ));
}

#[test]
fn stamped_in_place_team_channel_scopes_without_team_fallback() {
    let mut snapshot = empty_snapshot();
    let mut planner = agent("claude", "session-planner", None, "terminal_1");
    planner.worktree_path = Some("/code/team-channel".to_owned());
    planner.channel = Some("team-channel/forge".to_owned());
    let mut coder = agent("codex", "session-coder", None, "terminal_2");
    coder.worktree_path = Some("/code/team-channel".to_owned());
    coder.channel = Some("team-channel/forge".to_owned());
    let mut other = agent("codex", "session-other", None, "terminal_3");
    other.worktree_path = Some("/code/team-channel".to_owned());
    other.channel = Some("team-channel/docs".to_owned());
    snapshot.agents = vec![planner, coder, other];

    assert_eq!(
        agent_channel(&snapshot.agents[0]).as_deref(),
        Some("team-channel/forge")
    );
    assert_eq!((&snapshot.agents[0]).channel_label(), "team-channel/forge");
    assert!((&snapshot.agents[0]).in_worktree("team-channel/forge"));
    assert!(!(&snapshot.agents[0]).in_worktree("team-channel"));

    let ids: Vec<&str> = resolve_many(&snapshot, "@all", None, Some("team-channel/forge"))
        .unwrap()
        .iter()
        .map(|agent| agent.agent_id.as_str())
        .collect();
    assert_eq!(ids, vec!["session-planner", "session-coder"]);
}

#[test]
fn channel_team_reads_the_stamped_team_of_each_lane_shape() {
    // In place: the lane is `<dir>/<team>`.
    let mut planner = agent("claude", "session-planner", None, "terminal_1");
    planner.worktree_path = Some("/code/team-channel".to_owned());
    planner.channel = Some("team-channel/forge".to_owned());
    planner.team = Some("forge".to_owned());
    // Worktree: the lane is the directory basename, which names no team at all.
    let mut scout = agent("codex", "session-scout", None, "terminal_2");
    scout.worktree_path = Some("/code/feat-x".to_owned());
    scout.channel = Some("feat-x".to_owned());
    scout.team = Some("forge".to_owned());
    // A teamless agent contributes nothing.
    let mut solo = agent("claude", "session-solo", None, "terminal_3");
    solo.worktree_path = Some("/code/team-channel".to_owned());
    solo.channel = Some("team-channel/docs".to_owned());
    let agents = vec![planner, scout, solo];

    assert_eq!(channel_team(&agents, "team-channel/forge"), Some("forge"));
    assert_eq!(channel_team(&agents, "feat-x"), Some("forge"));
    assert_eq!(channel_team(&agents, "team-channel/docs"), None);
    assert_eq!(channel_team(&agents, "unknown-lane"), None);
}

#[test]
fn channel_team_declines_a_lane_holding_two_teams() {
    let mut forge = agent("claude", "session-forge", None, "terminal_1");
    forge.channel = Some("shared".to_owned());
    forge.team = Some("forge".to_owned());
    let mut docs = agent("codex", "session-docs", None, "terminal_2");
    docs.channel = Some("shared".to_owned());
    docs.team = Some("docs".to_owned());
    let agents = vec![forge, docs];

    assert_eq!(channel_team(&agents, "shared"), None);
}

#[test]
fn room_channel_resolver_prefers_explicit_worktree_then_in_place_team() {
    assert_eq!(
        resolve_room_channel(
            std::path::Path::new("/code/project"),
            std::path::Path::new("/code/project-wt/auth"),
            Some("forge"),
            None,
        )
        .as_deref(),
        Some("auth")
    );
    assert_eq!(
        resolve_room_channel(
            std::path::Path::new("/code/project"),
            std::path::Path::new("/code/project"),
            Some("forge"),
            None,
        )
        .as_deref(),
        Some("project/forge")
    );
    assert_eq!(
        resolve_room_channel(
            std::path::Path::new("/code/project"),
            std::path::Path::new("/code/project"),
            None,
            None,
        ),
        None
    );
    assert_eq!(
        resolve_room_channel(
            std::path::Path::new("/code/project"),
            std::path::Path::new("/code/project-wt/auth"),
            Some("forge"),
            Some("design"),
        )
        .as_deref(),
        Some("design")
    );
}

#[test]
fn launch_stamped_worktree_team_channel_renders_flat_worktree() {
    let mut team_agent = agent("claude", "session-feat", Some("feat/auth"), "terminal_1");
    team_agent.team = Some("forge".to_owned());
    team_agent.channel = Some("auth".to_owned());

    assert_eq!(agent_channel(&team_agent).as_deref(), Some("auth"));
    assert_eq!((&team_agent).channel_label(), "auth");
    assert!((&team_agent).in_worktree("auth"));
    assert!(!(&team_agent).in_worktree("auth/forge"));
}

#[test]
fn agent_channel_and_in_worktree_use_directory_not_branch() {
    let mut team_agent = agent("claude", "session-feat", Some("feat/auth"), "terminal_1");
    team_agent.team = Some("forge".to_owned());
    team_agent.worktree_branch = Some("scratch".to_owned());

    assert_eq!(agent_channel(&team_agent).as_deref(), Some("auth"));
    assert_eq!((&team_agent).channel_label(), "auth");
    assert!(!(&team_agent).in_worktree("auth/forge"));
    assert!((&team_agent).in_worktree("auth"));
    assert!(!(&team_agent).in_worktree("scratch"));
    assert!(!(&team_agent).in_worktree("feat/auth"));
}

#[test]
fn branch_style_worktree_filter_matches_dashed_channel() {
    let mut agent = agent("claude", "session-feat", Some("feat-great"), "terminal_1");
    agent.worktree_branch = Some("feat/great".to_owned());

    assert_eq!(agent_channel(&agent).as_deref(), Some("feat-great"));
    assert!((&agent).in_worktree("feat/great"));
}

#[test]
fn explicit_named_channel_wins_over_worktree_and_team() {
    let mut agent = agent("claude", "session-design", Some("feat/auth"), "terminal_1");
    agent.worktree_path = Some("/code/repo".to_owned());
    agent.team = Some("forge".to_owned());
    agent.channel = Some("design".to_owned());

    assert_eq!(agent_channel(&agent).as_deref(), Some("design"));
    assert_eq!((&agent).channel_label(), "design");
    assert!((&agent).in_worktree("design"));
    assert!(!(&agent).in_worktree("feat/auth"));
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
        Err(TargetErr::ChannelMismatch { .. })
    ));
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
fn absent_durable_agent_keeps_its_best_effort_handle() {
    let mut offline = agent(
        "claude",
        "session-offline",
        Some("auth"),
        "terminal_offline",
    );
    offline.role = Some("coder".to_owned());

    assert_eq!(agent_handle(&offline, &[], true), "@coder#auth");
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
fn message_header_parser_round_trips_attributed_senders() {
    let body = "ship it";
    let sender = MessageSender::Agent {
        kind: AgentKind::new_unchecked("codex"),
        name: None,
        profile: None,
        role: None,
        channel: Some("design".to_owned()),
    };

    let same_channel = message_header(&sender, &[], Some("design")).unwrap() + body;
    assert_eq!(
        parse_message_header(&same_channel),
        Some((HeaderKind::Agent, "@codex".to_owned(), body.to_owned()))
    );

    let cross_channel = message_header(&sender, &[], Some("main")).unwrap() + body;
    assert_eq!(
        parse_message_header(&cross_channel),
        Some((
            HeaderKind::Agent,
            "@codex#design".to_owned(),
            body.to_owned()
        ))
    );

    let human = message_header(&MessageSender::Human, &[], None).unwrap() + body;
    assert_eq!(
        parse_message_header(&human),
        Some((HeaderKind::User, "@user".to_owned(), body.to_owned()))
    );
    assert_eq!(message_header(&MessageSender::System, &[], None), None);

    let subagent = MessageSender::Subagent {
        kind: AgentKind::new_unchecked("codex"),
        name: "lucid-atlas".to_owned(),
    };
    let report = message_header(&subagent, &[], None).unwrap() + body;
    assert_eq!(
        parse_message_header(&report),
        Some((
            HeaderKind::Subagent,
            "@lucid-atlas".to_owned(),
            body.to_owned()
        ))
    );
}

#[test]
fn message_header_parser_rejects_near_misses() {
    for text in [
        "Type: SYSTEM_MESSAGE\nFrom: @rimz\nContent:\nship it",
        "Type: AGENT_MESSAGE\nFrom: @coder\nship it",
        "Type: AGENT_MESSAGE\nFrom: coder\nContent:\nship it",
        "Type: AGENT_MESSAGE\nFrom: @code r\nContent:\nship it",
        "ordinary text: with colon",
    ] {
        assert_eq!(parse_message_header(text), None, "{text}");
    }
}

#[test]
fn align_submitted_prompt_consumes_human_header() {
    let recipient = agent("claude", "session-recipient", Some("main"), "terminal_1");
    let record = MessageRecord::new(
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-target-test")),
        &recipient,
        "ship it".to_owned(),
        true,
        crate::message::DeliveryGate::Done,
    );
    let prompt = "Type: USER_MESSAGE\nFrom: @user\nContent:\nship it";

    assert_eq!(
        align_submitted_prompt(prompt, &[&record]),
        Some(vec![prompt])
    );
}

#[test]
fn align_submitted_prompt_consumes_subagent_header() {
    let recipient = agent("claude", "session-recipient", Some("main"), "terminal_1");
    let record = MessageRecord::new(
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-target-test")),
        &recipient,
        "ship it".to_owned(),
        true,
        crate::message::DeliveryGate::Done,
    )
    .with_sender(MessageSender::Subagent {
        kind: AgentKind::new_unchecked("codex"),
        name: "lucid-atlas".to_owned(),
    });
    let prompt = "Type: SUBAGENT_REPORT\nFrom: @lucid-atlas\nContent:\nship it";

    assert_eq!(
        align_submitted_prompt(prompt, &[&record]),
        Some(vec![prompt])
    );
}

#[test]
fn split_batched_prompt_splits_only_on_typed_sections() {
    let agent = "Type: AGENT_MESSAGE\nFrom: @planner\nContent:\nfirst";
    let subagent = "Type: SUBAGENT_REPORT\nFrom: @lucid-atlas\nContent:\nreport";
    let human = "Type: USER_MESSAGE\nFrom: @user\nContent:\nsecond";
    assert_eq!(
        split_batched_prompt(&format!("{agent}\n\n{human}")),
        vec![agent, human]
    );
    assert_eq!(
        split_batched_prompt(&format!("human note\n\n{agent}")),
        vec!["human note", agent]
    );
    assert_eq!(
        split_batched_prompt(&format!("{agent}\n\n\n{human}")),
        vec![agent, human]
    );
    assert_eq!(
        split_batched_prompt(&format!("{agent}\n\n{subagent}\n\n{human}")),
        vec![agent, subagent, human]
    );
    assert_eq!(
        split_batched_prompt(&format!("{agent}\n\nsecond paragraph")),
        vec![format!("{agent}\n\nsecond paragraph")]
    );
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
fn explicit_name_renders_and_round_trips_before_profile() {
    let mut snapshot = empty_snapshot();
    let mut named = agent("claude", "session-writer", Some("auth"), "terminal_1");
    named.name = Some("writer".to_owned());
    named.name_explicit = true;
    named.profile = Some("docs".to_owned());
    let mut profiled = agent("codex", "session-profile", Some("auth"), "terminal_2");
    profiled.profile = Some("writer".to_owned());
    snapshot.agents = vec![named, profiled];
    let peers: Vec<&AgentState> = snapshot.agents.iter().collect();

    let handle = agent_handle(peers[0], &peers, true);
    assert_eq!(handle, "@writer#auth");
    assert_eq!(
        resolve_one(&snapshot, &handle, None, None)
            .unwrap()
            .agent_id
            .as_str(),
        "session-writer"
    );

    // The agent whose profile text `writer` is now claimed by the explicit name
    // falls through to its kind, so its rendered handle still round-trips to
    // itself instead of colliding with `@writer`.
    let profiled_handle = agent_handle(peers[1], &peers, true);
    assert_eq!(profiled_handle, "@codex#auth");
    assert_eq!(
        resolve_one(&snapshot, &profiled_handle, None, None)
            .unwrap()
            .agent_id
            .as_str(),
        "session-profile"
    );
}

#[test]
fn minted_name_stays_fallback_for_solo_agent() {
    let mut snapshot = empty_snapshot();
    let mut minted = agent("claude", "session-claude", Some("auth"), "terminal_1");
    minted.name = Some("lucid-atlas".to_owned());
    snapshot.agents = vec![minted];
    let peers: Vec<&AgentState> = snapshot.agents.iter().collect();

    assert_eq!(agent_handle(peers[0], &peers, true), "@claude#auth");
    assert_eq!(
        resolve_one(&snapshot, "@claude#auth", None, None)
            .unwrap()
            .agent_id
            .as_str(),
        "session-claude"
    );
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
fn message_header_uses_live_handle_and_channel_only_when_crossing_channels() {
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
        message_header(&sender, &peers, Some("docs")).unwrap(),
        "Type: AGENT_MESSAGE\nFrom: @claude\nContent:\n"
    );
    assert_eq!(
        message_header(&sender, &peers, Some("main")).unwrap(),
        "Type: AGENT_MESSAGE\nFrom: @claude#docs\nContent:\n"
    );
}

#[test]
fn message_header_uses_explicit_live_handle() {
    let mut snapshot = empty_snapshot();
    let mut sender = agent("claude", "session-sender", Some("docs"), "terminal_1");
    sender.name = Some("writer".to_owned());
    sender.name_explicit = true;
    let target = agent("codex", "session-target", Some("docs"), "terminal_2");
    snapshot.agents = vec![sender, target];
    let peers: Vec<&AgentState> = snapshot.agents.iter().collect();
    let sender = crate::message::MessageSender::Agent {
        kind: AgentKind::new_unchecked("claude"),
        name: Some("writer".to_owned()),
        profile: None,
        role: None,
        channel: Some("fallback".to_owned()),
    };

    assert_eq!(
        message_header(&sender, &peers, Some("docs")).unwrap(),
        "Type: AGENT_MESSAGE\nFrom: @writer\nContent:\n"
    );
}

#[test]
fn recipient_channel_prefers_bound_then_pane_then_scope() {
    let fresh = fresh_pane("codex", "terminal_9");
    assert_eq!(
        recipient_channel(&fresh, None, Some("bandwidth-profiling")).as_deref(),
        Some("bandwidth-profiling")
    );

    let lazy = lazy_pane("codex", "/repo/main", "terminal_9");
    assert_eq!(
        recipient_channel(&lazy, None, Some("other")).as_deref(),
        Some("main")
    );

    let bound_target = bound_pane(
        "codex",
        1,
        "swift-otter",
        "session-x",
        "pane-channel",
        "terminal_9",
    );
    let bound = agent("codex", "session-x", Some("auth"), "terminal_9");

    assert_eq!(
        recipient_channel(&bound_target, Some(&bound), Some("other")).as_deref(),
        Some("auth")
    );
}

#[test]
fn message_header_uses_recipient_channel_for_same_lane_fresh_pane() {
    let target = fresh_pane("codex", "terminal_9");
    let sender = MessageSender::Agent {
        kind: AgentKind::new_unchecked("claude"),
        name: None,
        profile: None,
        role: None,
        channel: Some("bandwidth-profiling".to_owned()),
    };

    let same_channel = recipient_channel(&target, None, Some("bandwidth-profiling"));
    assert_eq!(
        message_header(&sender, &[], same_channel.as_deref()).unwrap(),
        "Type: AGENT_MESSAGE\nFrom: @claude\nContent:\n"
    );

    let cross_channel = recipient_channel(&target, None, Some("other"));
    assert_eq!(
        message_header(&sender, &[], cross_channel.as_deref()).unwrap(),
        "Type: AGENT_MESSAGE\nFrom: @claude#bandwidth-profiling\nContent:\n"
    );
}

#[test]
fn message_header_live_handle_disambiguates_same_kind_peers() {
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
        message_header(&sender, &peers, Some("main")).unwrap(),
        "Type: AGENT_MESSAGE\nFrom: @claude-2\nContent:\n"
    );
}

#[test]
fn message_header_falls_back_to_stored_identity_when_sender_is_absent() {
    let peers: Vec<&AgentState> = Vec::new();
    let sender = crate::message::MessageSender::Agent {
        kind: AgentKind::new_unchecked("codex"),
        name: Some("lucid-atlas".to_owned()),
        profile: Some("reviewer".to_owned()),
        role: None,
        channel: Some("docs".to_owned()),
    };

    assert_eq!(
        message_header(&sender, &peers, Some("main")).unwrap(),
        "Type: AGENT_MESSAGE\nFrom: @reviewer#docs\nContent:\n"
    );
    assert_eq!(
        message_header(&sender, &peers, Some("docs")).unwrap(),
        "Type: AGENT_MESSAGE\nFrom: @reviewer\nContent:\n"
    );
}

#[test]
fn message_header_fallback_uses_profile_before_petname() {
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
        message_header(&sender, &peers, Some("auth")).unwrap(),
        "Type: AGENT_MESSAGE\nFrom: @planner\nContent:\n"
    );
}

#[test]
fn pane_binding_distinguishes_exact_provisional_and_lazy_targets() {
    let mut snapshot = empty_snapshot();
    let exact = agent("codex", "session-exact", Some("auth"), "terminal_1");
    let mut provisional = agent("claude", "launch_pending", Some("docs"), "terminal_2");
    provisional.name = Some("fresh-launch".to_owned());
    snapshot.agents = vec![exact, provisional];

    let exact_pane = bound_pane("codex", 1, "exact", "session-exact", "auth", "terminal_1");
    let provisional_pane = lazy_pane("claude", "/repo/docs", "terminal_2");
    let lazy = lazy_pane("codex", "/repo/other", "terminal_3");

    let binding = pane_binding(&snapshot, &exact_pane, None).unwrap();
    assert_eq!(binding.kind, PaneBindingKind::Exact);
    assert_eq!(
        binding.exact_agent.map(|agent| agent.agent_id.as_str()),
        Some("session-exact")
    );

    let binding = pane_binding(&snapshot, &provisional_pane, None).unwrap();
    assert_eq!(binding.kind, PaneBindingKind::Provisional);
    assert_eq!(
        binding.agent.map(|agent| agent.agent_id.as_str()),
        Some("launch_pending")
    );
    assert!(binding.exact_agent.is_none());

    let binding = pane_binding(&snapshot, &lazy, None).unwrap();
    assert_eq!(binding.kind, PaneBindingKind::Lazy);
    assert!(binding.agent.is_none());
}

#[test]
fn pane_binding_rejects_wrong_channel_stale_and_wrong_pinned_panes() {
    let mut snapshot = empty_snapshot();
    let provisional = agent("claude", "launch_pending", Some("docs"), "terminal_1");
    snapshot.agents = vec![provisional];

    let wrong_channel = lazy_pane("claude", "/repo/auth", "terminal_1");
    assert_eq!(
        pane_binding(&snapshot, &wrong_channel, None).unwrap().kind,
        PaneBindingKind::Lazy
    );

    let stale = bound_pane("claude", 1, "stale", "session-gone", "docs", "terminal_2");
    assert!(pane_binding(&snapshot, &stale, None).is_none());

    let matching = lazy_pane("claude", "/repo/docs", "terminal_3");
    let other_pane = PaneId::from_parts(MuxName::Zellij, "terminal_4");
    assert!(pane_binding(&snapshot, &matching, Some(&other_pane)).is_none());
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
        Timestamp::now(),
    )
}

fn agent(kind: &str, id: &str, branch: Option<&str>, raw_pane: &str) -> AgentState {
    let mut agent = crate::sidebar::test_support::root_agent(kind, id, None);
    agent.name = None;
    agent.kind_ordinal = None;
    agent.status = AgentStatus::Idle;
    agent.pane = Some(PaneRef::from_id(PaneId::from_parts(
        MuxName::Zellij,
        raw_pane,
    )));
    agent.worktree_path = branch.map(|branch| format!("/repo/{branch}"));
    agent.worktree_branch = branch.map(ToOwned::to_owned);
    agent
}

/// A lazy (sessionless) agent pane as the producer would emit it into
/// `agent_panes` — kind and pane only.
fn lazy_pane(kind: &str, worktree_path: &str, raw_pane: &str) -> PaneAgent {
    PaneAgent {
        kind: AgentKind::new_unchecked(kind),
        kind_ordinal: None,
        name: None,
        name_explicit: false,
        profile: None,
        role: None,
        channel: None,
        agent_id: None,
        pane_id: PaneId::from_parts(MuxName::Zellij, raw_pane),
        pane_pid: None,
        worktree_path: Some(worktree_path.to_owned()),
        worktree_branch: None,
    }
}

/// A freshly registered live pane before cwd/channel capture lands.
fn fresh_pane(kind: &str, raw_pane: &str) -> PaneAgent {
    PaneAgent {
        kind: AgentKind::new_unchecked(kind),
        kind_ordinal: None,
        name: None,
        name_explicit: false,
        profile: None,
        role: None,
        channel: None,
        agent_id: None,
        pane_id: PaneId::from_parts(MuxName::Zellij, raw_pane),
        pane_pid: None,
        worktree_path: None,
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
        name_explicit: false,
        profile: None,
        role: None,
        channel: None,
        agent_id: Some(AgentSessionId::from(session)),
        pane_id: PaneId::from_parts(MuxName::Zellij, raw_pane),
        pane_pid: None,
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

#[test]
fn team_cohorts_group_live_root_members_by_team_and_lane() {
    let mut planner = agent("claude", "planner", Some("auth"), "terminal_1");
    planner.team = Some("forge".to_owned());
    planner.channel = Some("auth".to_owned());
    let mut coder = agent("codex", "coder", Some("auth"), "terminal_2");
    coder.team = Some("forge".to_owned());
    coder.channel = Some("auth".to_owned());
    let mut docs = agent("codex", "docs", Some("docs"), "terminal_3");
    docs.team = Some("forge".to_owned());
    docs.channel = Some("docs".to_owned());
    let mut child = agent("codex", "child", Some("auth"), "terminal_4");
    child.team = Some("forge".to_owned());
    child.parent_agent_id = Some(planner.agent_id.clone());
    let mut launched = agent("codex", "launched", Some("auth"), "terminal_6");
    launched.team = Some("forge".to_owned());
    launched.channel = Some("auth".to_owned());
    launched.parent_agent_id = Some(planner.agent_id.clone());
    launched.parent_agent_kind = Some(planner.kind.clone());
    launched.launch_depth = Some(1);
    let mut ended = agent("codex", "ended", Some("auth"), "terminal_5");
    ended.team = Some("forge".to_owned());
    ended.ended_at = Some(Timestamp::UNIX_EPOCH);

    let agents = [planner, coder, docs, child, launched, ended];
    let cohorts = team_cohorts(&agents);

    assert_eq!(cohorts.len(), 2);
    assert_eq!(
        cohorts
            .iter()
            .map(|cohort| (cohort.team, cohort.channel.as_str(), cohort.members.len()))
            .collect::<Vec<_>>(),
        vec![("forge", "auth", 3), ("forge", "docs", 1)]
    );
}

#[test]
fn team_cohorts_keep_only_the_owner_of_an_inherited_launch_identity() {
    let mut rested = agent("codex", "conversation-a", Some("auth"), "terminal_1");
    rested.launch_id = Some(AgentSessionId::from("launch_coder"));
    rested.team = Some("forge".to_owned());
    rested.role = Some("coder".to_owned());
    rested.channel = Some("auth".to_owned());
    rested.origin = Some(crate::agents::SessionOrigin::Fresh);
    rested.status = AgentStatus::Success;
    rested.runtime_owner = Some(crate::pane::RuntimeOwner::new(
        crate::pane::RuntimeOwnerKind::Agent,
        "conversation-a",
        42,
        Some("agent-start".to_owned()),
    ));
    let mut successor = agent("codex", "conversation-b", Some("auth"), "terminal_1");
    successor.launch_id = Some(AgentSessionId::from("launch_coder"));
    successor.team = Some("forge".to_owned());
    successor.role = Some("coder".to_owned());
    successor.channel = Some("auth".to_owned());
    successor.origin = Some(crate::agents::SessionOrigin::Fresh);
    successor.status = AgentStatus::Running;
    successor.runtime_owner = Some(crate::pane::RuntimeOwner::new(
        crate::pane::RuntimeOwnerKind::Agent,
        "conversation-b",
        42,
        Some("agent-start".to_owned()),
    ));
    let agents = [rested, successor];

    let cohorts = team_cohorts(&agents);

    assert_eq!(cohorts.len(), 1);
    assert_eq!(cohorts[0].members.len(), 1);
    assert_eq!(cohorts[0].members[0].agent_id.as_str(), "conversation-b");
    assert_eq!(
        agent_handle(cohorts[0].members[0], &cohorts[0].members, false),
        "@coder"
    );
}

#[test]
fn launch_role_follows_the_current_conversation_without_a_pane_frame() {
    let mut primary = agent("codex", "primary", Some("auth"), "terminal_1");
    primary.launch_id = Some(AgentSessionId::from("launch_coder"));
    primary.name = Some("primary-card".to_owned());
    primary.role = Some("coder".to_owned());
    primary.team = Some("forge".to_owned());
    primary.status = AgentStatus::Success;
    primary.last_activity = Timestamp::from_second(1_000).unwrap();
    primary.runtime_owner = Some(crate::pane::RuntimeOwner::new(
        crate::pane::RuntimeOwnerKind::Agent,
        "primary",
        42,
        Some("agent-start".to_owned()),
    ));
    let mut fork = agent("codex", "fork", Some("auth"), "terminal_1");
    fork.launch_id = Some(AgentSessionId::from("launch_coder"));
    fork.name = Some("fork-card".to_owned());
    fork.role = Some("coder".to_owned());
    fork.team = Some("forge".to_owned());
    fork.status = AgentStatus::Success;
    fork.last_activity = Timestamp::from_second(2_000).unwrap();
    fork.runtime_owner = Some(crate::pane::RuntimeOwner::new(
        crate::pane::RuntimeOwnerKind::Agent,
        "fork",
        42,
        Some("agent-start".to_owned()),
    ));
    let mut snapshot = empty_snapshot();
    snapshot.agents = vec![primary.clone(), fork.clone()];

    let resolved = resolve_one(&snapshot, "@coder", None, Some("auth")).unwrap();
    assert_eq!(resolved.agent_id.as_str(), "fork");
    assert_eq!(addressable_agents(&snapshot).len(), 2);
    assert_eq!(
        resolve_one(&snapshot, "@primary-card", None, Some("auth"))
            .unwrap()
            .agent_id
            .as_str(),
        "primary"
    );
    assert_eq!(
        resolve_one(&snapshot, "@primary", None, Some("auth"))
            .unwrap()
            .agent_id
            .as_str(),
        "primary"
    );
    let peers = snapshot.root_agents().collect::<Vec<_>>();
    assert_eq!(
        agent_handle(&snapshot.agents[0], &peers, false),
        "@primary-card"
    );
    assert_eq!(agent_handle(&snapshot.agents[1], &peers, false), "@coder");

    primary.status = AgentStatus::Running;
    primary.last_activity = Timestamp::from_second(3_000).unwrap();
    snapshot.agents = vec![primary, fork];
    let resolved = resolve_one(&snapshot, "@coder", None, Some("auth")).unwrap();
    assert_eq!(resolved.agent_id.as_str(), "primary");
}

#[test]
fn launch_groups_separate_relaunched_processes_reusing_an_id() {
    let mut crashed = agent("codex", "crashed", Some("auth"), "terminal_1");
    crashed.launch_id = Some(AgentSessionId::from("launch_coder"));
    crashed.runtime_owner = Some(crate::pane::RuntimeOwner::new(
        crate::pane::RuntimeOwnerKind::Agent,
        "crashed",
        41,
        Some("old-start".to_owned()),
    ));
    let mut relaunched = agent("codex", "relaunched", Some("auth"), "terminal_1");
    relaunched.launch_id = Some(AgentSessionId::from("launch_coder"));
    relaunched.runtime_owner = Some(crate::pane::RuntimeOwner::new(
        crate::pane::RuntimeOwnerKind::Agent,
        "relaunched",
        42,
        Some("new-start".to_owned()),
    ));

    let agents = [crashed, relaunched];
    let groups = launch_groups(&agents);

    assert_eq!(groups.len(), 2);
    assert!(groups.iter().all(|group| group.len() == 1));
}

#[test]
fn launched_children_matches_a_parent_that_adopted_its_provider_session() {
    let mut parent = agent("codex", "provider-parent", None, "terminal_1");
    parent.launch_id = Some("launch-parent".into());
    let mut child = agent("codex", "provider-child", None, "terminal_2");
    child.parent_agent_id = Some("launch-parent".into());
    child.parent_agent_kind = Some(parent.kind.clone());
    child.launch_depth = Some(1);
    let agents = [parent.clone(), child.clone()];

    let children = launched_children(&agents, &parent);

    assert_eq!(children.len(), 1);
    assert_eq!(children[0].agent_id, child.agent_id);
}
