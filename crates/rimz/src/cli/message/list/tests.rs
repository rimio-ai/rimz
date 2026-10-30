use super::*;

use rimz::agents::AgentStatus;
use rimz::ids::{AgentKind, MuxName, PaneId, WorkspaceId};
use rimz::pane::PaneRef;

#[test]
fn machine_repeats_keep_the_newest_id_age_and_snippet() {
    let mut newest = machine_row("newest notice");
    newest.status = MessageStatus::Delivered;
    newest.enqueued_at = Timestamp::now() - jiff::SignedDuration::from_secs(180);
    let mut oldest = newest.clone();
    oldest.message_id = MessageId::new();
    oldest.enqueued_at -= jiff::SignedDuration::from_secs(840);
    oldest.text = Some("older notice about another command".to_owned());
    let output = render_digest(vec![newest.clone(), oldest.clone()], LaneScope::Root, None);

    assert_eq!(output.matches(" → ").count(), 1, "{output}");
    assert!(output.contains("delivered ×2  3m ago"), "{output}");
    assert!(output.contains(newest.message_id.as_str()), "{output}");
    assert!(!output.contains(oldest.message_id.as_str()), "{output}");
    assert!(output.contains("over 14m · newest notice"), "{output}");
    assert!(!output.contains("older notice"), "{output}");
}

#[test]
fn textless_machine_repeats_keep_the_newest_reason() {
    let mut newest = machine_row("discarded text");
    newest.text = None;
    newest.last_error = Some("newest reason".to_owned());
    let mut oldest = newest.clone();
    oldest.enqueued_at -= jiff::SignedDuration::from_secs(840);
    oldest.last_error = Some("older reason".to_owned());
    let output = render_digest(vec![newest, oldest], LaneScope::Root, None);
    assert_eq!(output.matches(" → ").count(), 1, "{output}");
    assert!(output.contains("over 14m · newest reason"), "{output}");
    assert!(!output.contains("older reason"), "{output}");
}

#[test]
fn conversation_repeats_never_fold() {
    let counts = [MessageSender::Human, agent_sender("planner", None)].map(|sender| {
        let row = message_row_with_sender("receiver", None, "same text", sender);
        render_digest(vec![row.clone(), row], LaneScope::Root, None)
            .matches(" → ")
            .count()
    });
    assert_eq!(counts, [2, 2]);
}

#[test]
fn machine_repeat_keys_split_on_typed_cells() {
    let newest = machine_row("notice");
    let mut cases = vec![newest.clone(); 5];
    cases[0].sender = MessageSender::Harness {
        notice: rimz::store::message::HarnessNotice::Signal,
    };
    cases[1].kind = AgentKind::new_unchecked("codex");
    cases[2].agent_id = "other-session".into();
    cases[2].agent_name = Some("other-name".to_owned());
    cases[3].body = MessageBody::Command;
    cases[4].status = MessageStatus::Delivered;
    let counts = cases
        .into_iter()
        .map(|older| {
            render_digest(vec![newest.clone(), older], LaneScope::Root, None)
                .matches(" → ")
                .count()
        })
        .collect::<Vec<_>>();
    assert_eq!(counts, [2; 5]);
}

#[test]
fn machine_repeat_keys_ignore_unrendered_fields_and_match_receiver_cards() {
    let newest = machine_row("newest text");
    let mut cases = vec![newest.clone(); 7];
    cases[0].text = None;
    cases[1].text = Some("different older text".to_owned());
    cases[2].last_error = Some("different error".to_owned());
    cases[3].about_run = Some(rimz::ids::RunId::new());
    cases[4].gate = DeliveryGate::Any;
    cases[4].force = true;
    cases[4].enter = false;
    cases[5].agent_name = Some("renamed-same-session".to_owned());
    cases[6].agent_id = "new-session-same-name".into();
    let outputs = cases
        .into_iter()
        .map(|older| render_digest(vec![newest.clone(), older], LaneScope::Root, None))
        .collect::<Vec<_>>();
    let counts = outputs
        .iter()
        .map(|output| output.matches(" → ").count())
        .collect::<Vec<_>>();
    assert_eq!(counts, [1; 7], "{outputs:#?}");
    assert!(outputs.iter().all(|output| output.contains("newest text")));
}

#[test]
fn all_machine_senders_and_statuses_fold() {
    let senders = [
        machine_row("notice").sender,
        MessageSender::System,
        MessageSender::Subagent {
            kind: AgentKind::new_unchecked("codex"),
            name: "child".to_owned(),
        },
    ];
    let counts = senders
        .into_iter()
        .flat_map(|sender| {
            [
                MessageStatus::Queued,
                MessageStatus::Claimed,
                MessageStatus::Sent,
                MessageStatus::Delivered,
                MessageStatus::Canceled,
                MessageStatus::Archived,
                MessageStatus::Expired,
                MessageStatus::TimedOut,
                MessageStatus::Errored,
                MessageStatus::Abandoned,
            ]
            .map(|status| {
                let mut row = machine_row("notice");
                row.sender = sender.clone();
                row.status = status;
                render_digest(vec![row.clone(), row], LaneScope::All, None)
                    .matches(" → ")
                    .count()
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(counts, [1; 30]);
}

#[test]
fn repeat_adjacency_is_within_the_rendered_lane() {
    let newest = machine_row("newest notice");
    let older = machine_row("older notice");
    let outputs = [None, Some("ops")].map(|channel| {
        render_digest(
            vec![
                newest.clone(),
                message_row("interleaved", channel, "conversation"),
                older.clone(),
            ],
            LaneScope::All,
            None,
        )
    });
    assert_eq!(
        outputs
            .each_ref()
            .map(|output| output.matches(" → ").count()),
        [3, 2],
        "{outputs:#?}"
    );
}

#[test]
fn message_limit_keeps_a_whole_repeat_and_counts_hidden_messages() {
    let newest = machine_row("newest notice");
    let older = machine_row("older notice");
    let hidden = message_row("human", None, "hidden conversation");
    let mut out = Vec::new();
    render_message_digest(
        &mut out,
        vec![newest, older, hidden],
        &[],
        &LaneScope::Root,
        Some("project"),
        1,
        0,
        "empty",
    )
    .unwrap();
    let output = String::from_utf8(out).unwrap();
    assert_eq!(output.matches(" → ").count(), 1, "{output}");
    assert!(output.contains("queued ×2"), "{output}");
    assert!(!output.contains("hidden conversation"), "{output}");
    assert!(
        output.ends_with("... 1 older messages hidden (--limit 0 for all)\n"),
        "{output}"
    );
}

#[test]
fn message_limit_counts_members_of_hidden_folds() {
    let newest = machine_row("shown notice");
    let mut hidden = machine_row("hidden notice");
    hidden.sender = MessageSender::System;
    let mut out = Vec::new();
    render_message_digest(
        &mut out,
        vec![newest.clone(), newest, hidden.clone(), hidden],
        &[],
        &LaneScope::All,
        Some("project"),
        1,
        0,
        "empty",
    )
    .unwrap();
    let output = String::from_utf8(out).unwrap();
    assert_eq!(output.matches(" → ").count(), 1, "{output}");
    assert!(
        output.ends_with("... 2 older messages hidden (--limit 0 for all)\n"),
        "{output}"
    );
}

#[test]
fn a_single_machine_message_has_no_fold_annotation() {
    let output = render_digest(vec![machine_row("one notice")], LaneScope::Root, None);
    assert_eq!(output.matches(" → ").count(), 1, "{output}");
    assert!(output.contains("one notice"), "{output}");
    assert!(!output.contains('×'), "{output}");
    assert!(!output.contains("over "), "{output}");
}

#[test]
fn message_snippet_preserves_conditions_when_clipping_text() {
    let mut row = machine_row(&"long text ".repeat(20));
    row.after.push(AfterCondition {
        kind: row.kind.clone(),
        agent_id: "planner".into(),
        agent_name: None,
        address: "@planner".to_owned(),
        met_at: None,
    });
    row.when.push(WhenCondition {
        kind: row.kind.clone(),
        agent_id: "reviewer".into(),
        agent_name: None,
        address: "@reviewer".to_owned(),
        status: AgentStatus::Idle,
        dwell_secs: 60,
        met_at: None,
    });
    assert_eq!(
        message_snippet(&row, 52, ""),
        "long te... · after @planner · when @reviewer idle 1m"
    );
    assert_eq!(message_snippet(&row, 5, ""), "lo...");
}

#[test]
fn message_snippet_preserves_fold_span_and_marker_at_narrow_width() {
    let mut row = machine_row(&"long text ".repeat(20));
    row.after.push(AfterCondition {
        kind: row.kind.clone(),
        agent_id: "planner".into(),
        agent_name: None,
        address: "@planner".to_owned(),
        met_at: None,
    });
    assert_eq!(
        message_snippet(&row, 38, "over 14m · "),
        "over 14m · long te... · after @planner"
    );
}

fn machine_row(text: &str) -> MessageListRow {
    message_row_with_sender(
        "receiver",
        None,
        text,
        MessageSender::Harness {
            notice: rimz::store::message::HarnessNotice::Wait,
        },
    )
}

#[test]
fn message_snippet_names_text_not_kept() {
    let mut row = MessageListRow::from_record(MessageRecord::new(
        workspace_id(),
        &agent("sess-coder", AgentStatus::Idle),
        "text".to_owned(),
        DeliveryGate::Done,
    ));
    row.text = None;
    assert!(message_snippet(&row, 120, "").contains("(text no longer kept)"));
}

#[test]
fn message_target_keeps_single_sigil() {
    let mut coder = agent("sess-coder", AgentStatus::Idle);
    coder.role = Some("coder".to_owned());
    let snapshot = SidebarSnapshot::build_with_agents(workspace_id(), vec![coder], now());
    let message = MessageRecord::new(
        workspace_id(),
        &snapshot.agents[0],
        "work".to_owned(),
        DeliveryGate::Done,
    );
    let message = MessageListRow::from_record(message);
    let agents = rimz::address::addressable_agents(&snapshot);
    assert_eq!(message_target(&message, &agents, None), "@coder#project");
}

#[test]
fn message_snippet_marks_only_unmet_after_conditions() {
    let upstream = agent("sess-planner", AgentStatus::Running);
    let condition = AfterCondition {
        kind: upstream.kind.clone(),
        agent_id: upstream.agent_id.clone(),
        agent_name: upstream.name.clone(),
        address: "@planner".to_owned(),
        met_at: None,
    };
    let receiver = agent("sess-coder", AgentStatus::Running);
    let waiting = MessageListRow::from_record(
        MessageRecord::new(
            workspace_id(),
            &receiver,
            "read plan".to_owned(),
            DeliveryGate::Done,
        )
        .with_after(vec![condition.clone()]),
    );
    let met = MessageListRow::from_record(
        MessageRecord::new(
            workspace_id(),
            &receiver,
            "read plan".to_owned(),
            DeliveryGate::Done,
        )
        .with_after(vec![AfterCondition {
            met_at: Some(now()),
            ..condition
        }]),
    );

    assert_eq!(
        message_snippet(&waiting, 120, ""),
        "read plan · after @planner"
    );
    assert_eq!(message_snippet(&met, 120, ""), "read plan");
}

#[test]
fn message_target_uses_stored_address_before_fallbacks() {
    let message = MessageRecord::new(
        workspace_id(),
        &agent("sess-coder", AgentStatus::Idle),
        "work".to_owned(),
        DeliveryGate::Done,
    )
    .with_channel(Some("project".to_owned()))
    .with_address(Some("@saved#project".to_owned()));
    let message = MessageListRow::from_record(message);

    assert_eq!(message_target(&message, &[], None), "@saved#project");
}

#[test]
fn message_target_falls_back_to_agent_name_and_channel_when_agent_is_gone() {
    let message = MessageRecord::new(
        workspace_id(),
        &agent("sess-coder", AgentStatus::Idle),
        "work".to_owned(),
        DeliveryGate::Done,
    )
    .with_channel(Some("project".to_owned()));
    let message = MessageListRow::from_record(message);

    assert_eq!(
        message_target(&message, &[], None),
        "@sess-coder-name#project"
    );
}

#[test]
fn message_target_falls_back_to_kind_id_for_nameless_records() {
    let mut receiver = agent("sess-coder", AgentStatus::Idle);
    receiver.name = None;
    let message = MessageRecord::new(
        workspace_id(),
        &receiver,
        "work".to_owned(),
        DeliveryGate::Done,
    );
    let message = MessageListRow::from_record(message);

    assert_eq!(message_target(&message, &[], None), "claude:sess-coder");
}

#[test]
fn scoped_handle_drops_matching_lane_suffix() {
    assert_eq!(
        scoped_handle("@coder#project".to_owned(), Some("project")),
        "@coder"
    );
    // Lane membership is exact: a team lane keeps its suffix under the
    // directory filter.
    assert_eq!(
        scoped_handle("@coder#project/forge".to_owned(), Some("project")),
        "@coder#project/forge"
    );
    assert_eq!(
        scoped_handle("@coder#ops".to_owned(), Some("project")),
        "@coder#ops"
    );
    assert_eq!(scoped_handle("you".to_owned(), Some("project")), "you");
}

#[test]
fn preview_respects_width_and_flattens_control_whitespace() {
    assert_eq!(preview("a\nb\tc", 10), "a b c");
    assert_eq!(preview("abcdef", 4), "a...");
    assert_eq!(preview("abcdef", 3), "...");
}

#[test]
fn message_digest_groups_all_lanes_once_by_latest_activity() {
    let output = render_digest(
        vec![
            message_row("sess-docs-new", Some("docs"), "new docs"),
            message_row("sess-ops", Some("ops"), "ops"),
            message_row("sess-docs-old", Some("docs"), "old docs"),
        ],
        LaneScope::All,
        None,
    );

    assert_eq!(output.matches("#docs").count(), 1);
    assert_eq!(output.matches("#ops").count(), 1);
    assert!(output.find("#docs").unwrap() < output.find("new docs").unwrap());
    assert!(output.find("new docs").unwrap() < output.find("old docs").unwrap());
    assert!(output.find("old docs").unwrap() < output.find("#ops").unwrap());

    let lines: Vec<&str> = output.lines().collect();
    let snippet = lines
        .iter()
        .position(|line| line.contains("new docs"))
        .unwrap();
    assert!(lines[snippet - 1].starts_with("  "));
    assert!(lines[snippet].starts_with("    "));
}

#[test]
fn root_scope_includes_legacy_and_basename_records() {
    assert!(LaneScope::Root.contains_channel(None, Some("project")));
    assert!(LaneScope::Root.contains_channel(Some("project"), Some("project")));
    assert!(!LaneScope::Root.contains_channel(Some("docs"), Some("project")));
}

#[test]
fn all_digest_groups_root_records_under_main_once() {
    let output = render_digest(
        vec![
            message_row("root", Some("project"), "root text"),
            message_row("legacy", None, "legacy text"),
        ],
        LaneScope::All,
        None,
    );
    assert_eq!(output.matches("#main").count(), 1, "{output}");
    assert!(!output.contains("#project"), "{output}");
}

#[test]
fn message_digest_scopes_handles_by_row_lane() {
    let output = render_digest(
        vec![
            message_row_with_sender(
                "sess-same",
                Some("main"),
                "own lane",
                agent_sender("planner", Some("main")),
            ),
            message_row_with_sender(
                "sess-cross",
                Some("main"),
                "cross lane",
                agent_sender("reviewer", Some("docs")),
            ),
        ],
        LaneScope::All,
        None,
    );

    assert!(output.contains("@planner"));
    assert!(!output.contains("@planner#main"));
    assert!(output.contains("@reviewer#docs"));
}

#[test]
fn retain_conversation_keeps_human_and_agent_rows() {
    let mut rows: Vec<_> = [
        MessageSender::Human,
        agent_sender("planner", None),
        MessageSender::System,
        MessageSender::Harness {
            notice: rimz::store::message::HarnessNotice::Wait,
        },
        MessageSender::Subagent {
            kind: AgentKind::new_unchecked("codex"),
            name: "child".to_owned(),
        },
    ]
    .into_iter()
    .map(|sender| message_row_with_sender("sess-coder", None, "task", sender))
    .collect();
    assert_eq!(retain_conversation(&mut rows), 3);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].sender, MessageSender::Human);
    assert_eq!(rows[1].sender, agent_sender("planner", None));
}

#[test]
fn message_digest_reports_hidden_system_rows() {
    for rows in [Vec::new(), vec![message_row("sess-coder", None, "task")]] {
        let mut out = Vec::new();
        render_message_digest(
            &mut out,
            rows,
            &[],
            &LaneScope::Root,
            None,
            0,
            2,
            &empty_message_digest(&LaneScope::Root, None),
        )
        .unwrap();
        let output = String::from_utf8(out).unwrap();
        assert!(output.ends_with("... 2 system messages hidden (--system shows them)\n"));
    }
    assert!(!render_digest(Vec::new(), LaneScope::Root, None).contains("system messages hidden"));
}

#[test]
fn message_digest_empty_state_describes_scope_and_status() {
    let all = render_digest(Vec::new(), LaneScope::All, None);
    assert!(all.contains("no messages"));
    assert!(!all.contains("shows every channel"));

    let main = render_digest(Vec::new(), LaneScope::Root, None);
    assert!(main.contains("no messages in #main"));
    assert!(main.contains("rimz message list --all shows every channel"));

    let named = render_digest(
        Vec::new(),
        LaneScope::Named("ops".to_owned()),
        Some(MessageStatus::Queued),
    );
    assert!(named.contains("no queued messages in #ops"));
    assert!(named.contains("rimz message list --all shows every channel"));
}

#[test]
fn collapse_home_in_snippet_handles_mid_text_home_and_no_home() {
    assert_eq!(
        collapse_home_in_snippet_to(
            Some("/home/dev"),
            "see /home/dev/worktree/plan.md then /tmp"
        ),
        "see ~/worktree/plan.md then /tmp"
    );
    assert_eq!(
        collapse_home_in_snippet_to(None, "see /home/dev/worktree"),
        "see /home/dev/worktree"
    );
    assert_eq!(
        collapse_home_in_snippet_to(Some("/home/dev"), "see /home/development/plan.md"),
        "see /home/development/plan.md"
    );
    assert_eq!(
        collapse_home_in_snippet_to(Some("/home/dev/"), "see /home/dev/worktree"),
        "see ~/worktree"
    );
    assert_eq!(collapse_home_in_snippet_to(Some("/"), "/tmp"), "/tmp");
    assert_eq!(collapse_home_in_snippet_to(Some(""), "/tmp"), "/tmp");
}

#[test]
fn system_sender_renders_like_rimz_attribution() {
    assert_eq!(
        rendered_sender(&MessageSender::System, "rimz"),
        rendered_sender(&agent_sender("rimz", None), "rimz")
    );
}

#[test]
fn other_lanes_line_counts_only_queued_as_undelivered() {
    let mut claimed = message_row("sess-a", Some("ops"), "claimed");
    claimed.status = MessageStatus::Claimed;
    let queued = message_row("sess-b", Some("ops"), "queued");
    assert_eq!(
        other_lanes_line(
            &[claimed.clone(), queued],
            &LaneScope::Root,
            Some("project"),
            false
        )
        .as_deref(),
        Some(
            "no messages in #main. 2 in 1 other lane, 1 not yet delivered — rimz message list --all --status queued"
        )
    );
    assert_eq!(
        other_lanes_line(&[claimed], &LaneScope::Root, Some("project"), false).as_deref(),
        Some("no messages in #main. 1 in 1 other lane — rimz message list --all")
    );
    assert_eq!(
        other_lanes_line(
            &[
                message_row("legacy", None, "legacy root"),
                message_row("current", Some("project"), "current root"),
            ],
            &LaneScope::Named("ops".to_owned()),
            Some("project"),
            false,
        )
        .as_deref(),
        Some(
            "no messages in #ops. 2 in 1 other lane, 2 not yet delivered — rimz message list --all --status queued"
        )
    );
}

#[test]
fn known_channels_line_caps_at_twenty_names() {
    let known = (0..23)
        .map(|n| format!("lane-{n:02}"))
        .collect::<std::collections::BTreeSet<_>>();
    let line = known_channels_line(&known);
    assert!(line.starts_with("lane-00, lane-01, "), "{line}");
    assert!(line.ends_with("lane-19, …and 3 more"), "{line}");
    assert_eq!(
        known_channels_line(&std::collections::BTreeSet::new()),
        "(none)"
    );
}

fn workspace_id() -> WorkspaceId {
    WorkspaceId::parse("ws_000000000000000000000000").unwrap()
}

fn render_digest(
    messages: Vec<MessageListRow>,
    lane_scope: LaneScope,
    status: Option<MessageStatus>,
) -> String {
    let mut out = anstream::StripStream::new(Vec::new());
    render_message_digest(
        &mut out,
        messages,
        &[],
        &lane_scope,
        Some("project"),
        0,
        0,
        &empty_message_digest(&lane_scope, status),
    )
    .unwrap();
    String::from_utf8(out.into_inner()).unwrap()
}

fn message_row(id: &str, channel: Option<&str>, text: &str) -> MessageListRow {
    message_row_with_sender(id, channel, text, MessageSender::Human)
}

fn message_row_with_sender(
    id: &str,
    channel: Option<&str>,
    text: &str,
    sender: MessageSender,
) -> MessageListRow {
    let message = MessageRecord::new(
        workspace_id(),
        &agent(id, AgentStatus::Idle),
        text.to_owned(),
        DeliveryGate::Done,
    )
    .with_channel(channel.map(ToOwned::to_owned))
    .with_sender(sender);
    MessageListRow::from_record(message)
}

fn agent_sender(role: &str, channel: Option<&str>) -> MessageSender {
    MessageSender::Agent {
        agent_id: None,
        kind: AgentKind::new_unchecked("codex"),
        name: None,
        profile: None,
        role: Some(role.to_owned()),
        channel: channel.map(ToOwned::to_owned),
    }
}

fn agent(id: &str, status: AgentStatus) -> AgentState {
    let mut agent = AgentState::stub("claude", id, status);
    agent.pane = Some(PaneRef::from_id(PaneId::from_parts(
        MuxName::Zellij,
        "terminal_3",
    )));
    agent.worktree_path = Some("/repo/project".to_owned());
    agent.worktree_branch = Some("project".to_owned());
    agent
}

fn now() -> jiff::Timestamp {
    jiff::Timestamp::UNIX_EPOCH
}
