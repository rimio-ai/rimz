use super::*;

fn ts(raw: &str) -> jiff::Timestamp {
    raw.parse().expect("timestamp")
}

fn agent_key() -> AgentKey {
    agent_key_for("claude", "sess-1")
}

fn agent_key_for(kind: &str, session_id: &str) -> AgentKey {
    (
        AgentKind::new_unchecked(kind),
        AgentSessionId::from(session_id),
    )
}

fn render_entry(
    kind: TranscriptKind,
    from: &str,
    to: Option<&str>,
    at: &str,
    text: &str,
) -> RenderEntry {
    RenderEntry {
        kind,
        agent: agent_key(),
        chat: ChatLine {
            from: from.to_owned(),
            to: to.map(ToOwned::to_owned),
            at: Some(ts(at)),
            text: text.to_owned(),
            message_id: None,
            reply_to: Vec::new(),
            error: kind == TranscriptKind::Error,
            questions: Vec::new(),
            answers: Vec::new(),
        },
    }
}

fn entry(at: &str, text: &str) -> RenderEntry {
    render_entry(TranscriptKind::Prompt, "user", Some("@claude"), at, text)
}

fn assistant_entry(at: &str, text: &str) -> RenderEntry {
    render_entry(TranscriptKind::Assistant, "@claude", None, at, text)
}

fn ask_entry(at: &str, text: &str) -> RenderEntry {
    render_entry(TranscriptKind::Ask, "@claude", None, at, text)
}

fn answer_entry(at: &str, text: &str) -> RenderEntry {
    render_entry(TranscriptKind::Answer, "you", Some("@claude"), at, text)
}

fn message_id(index: u64) -> String {
    format!("msg_{index:016x}")
}

fn linked(mut entry: RenderEntry, id: Option<u64>, parents: &[u64]) -> RenderEntry {
    entry.chat.message_id = id.map(message_id);
    entry.chat.reply_to = parents.iter().copied().map(message_id).collect();
    entry
}

fn ask_option(label: &str) -> AskOption {
    AskOption::from(label.to_owned())
}

fn described_ask_option(label: &str, description: &str) -> AskOption {
    AskOption {
        label: label.to_owned(),
        description: Some(description.to_owned()),
        caution: None,
    }
}

fn render(entries: &[RenderEntry], today: Date) -> String {
    render_with_archive(entries, 0, today)
}

fn render_with_archive(entries: &[RenderEntry], archive_prefix: usize, today: Date) -> String {
    let tz = TimeZone::get("America/New_York").expect("timezone");
    let mut out = anstream::StripStream::new(Vec::new());
    render_chat_to(&mut out, None, entries, archive_prefix, &tz, today).expect("render");
    String::from_utf8(out.into_inner()).expect("utf8")
}

fn render_raw(entries: &[RenderEntry], today: Date) -> String {
    let tz = TimeZone::get("America/New_York").expect("timezone");
    let mut out = Vec::new();
    render_chat_to(&mut out, None, entries, 0, &tz, today).expect("render");
    String::from_utf8(out).expect("utf8")
}

fn render_threaded(entries: &[RenderEntry], today: Date) -> String {
    let tz = TimeZone::get("America/New_York").expect("timezone");
    let display = assemble_threads(entries, 0, false);
    let mut out = anstream::StripStream::new(Vec::new());
    render_display_chat_to(&mut out, None, &display, &tz, today).expect("render");
    String::from_utf8(out.into_inner()).expect("utf8")
}

#[test]
fn thread_assembly_expands_interleaved_components_in_place() {
    let entries = vec![
        linked(entry("2026-06-28T04:00:00Z", "root a"), Some(1), &[]),
        linked(entry("2026-06-28T04:01:00Z", "root b"), Some(2), &[]),
        linked(
            assistant_entry("2026-06-28T04:02:00Z", "reply a"),
            None,
            &[1],
        ),
        linked(
            assistant_entry("2026-06-28T04:03:00Z", "reply b"),
            None,
            &[2],
        ),
    ];

    let display = assemble_threads(&entries, 0, false);

    assert_eq!(
        display
            .iter()
            .map(|entry| entry.entry.chat.text.as_str())
            .collect::<Vec<_>>(),
        vec!["root a", "reply a", "root b", "reply b"]
    );
    assert!(display[0].lane.is_margin());
    assert!(!display[1].lane.is_margin());
    assert!(display[2].lane.is_margin());
    assert!(!display[3].lane.is_margin());
}

#[test]
fn hand_off_roots_a_new_thread_and_reply_back_continues_it() {
    let entries = vec![
        linked(
            render_entry(
                TranscriptKind::Message,
                "@planner",
                Some("@coder"),
                "2026-06-28T04:00:00Z",
                "plan",
            ),
            Some(1),
            &[],
        ),
        linked(
            render_entry(
                TranscriptKind::Message,
                "@coder",
                Some("@reviewer"),
                "2026-06-28T04:01:00Z",
                "review",
            ),
            Some(2),
            &[1],
        ),
        linked(
            render_entry(
                TranscriptKind::Assistant,
                "@coder",
                None,
                "2026-06-28T04:02:00Z",
                "implemented and committed",
            ),
            None,
            &[1],
        ),
        linked(
            render_entry(
                TranscriptKind::Message,
                "@reviewer",
                Some("@coder"),
                "2026-06-28T04:03:00Z",
                "clear to PR",
            ),
            Some(3),
            &[2],
        ),
        linked(
            render_entry(
                TranscriptKind::Assistant,
                "@coder",
                None,
                "2026-06-28T04:04:00Z",
                "PR opened",
            ),
            None,
            &[3],
        ),
    ];

    let display = assemble_threads(&entries, 0, false);

    assert_eq!(
        display
            .iter()
            .map(|entry| entry.entry.chat.text.as_str())
            .collect::<Vec<_>>(),
        vec![
            "plan",
            "implemented and committed",
            "review",
            "clear to PR",
            "PR opened"
        ]
    );
    assert!(display[0].lane.is_margin());
    assert!(!display[1].lane.is_margin());
    assert!(display[2].lane.is_margin());
    assert!(!display[3].lane.is_margin());
    assert!(!display[4].lane.is_margin());
    assert_eq!(display[0].block, 0);
    assert_eq!(display[1].block, 0);
    assert_eq!(display[2].block, 1);
    assert_eq!(display[3].block, 1);
    assert_eq!(display[4].block, 1);
}

#[test]
fn message_to_third_party_after_user_prompt_stays_at_margin() {
    let entries = vec![
        linked(entry("2026-06-28T04:00:00Z", "fix this"), Some(1), &[]),
        linked(
            render_entry(
                TranscriptKind::Message,
                "@coder",
                Some("@reviewer"),
                "2026-06-28T04:01:00Z",
                "review",
            ),
            Some(2),
            &[1],
        ),
    ];

    let display = assemble_threads(&entries, 0, false);

    assert!(display[0].lane.is_margin());
    assert!(display[1].lane.is_margin());
}

#[test]
fn reply_back_matches_base_handles_across_channels() {
    let entries = vec![
        linked(
            render_entry(
                TranscriptKind::Message,
                "@coder#feat-a",
                Some("@reviewer#feat-a"),
                "2026-06-28T04:00:00Z",
                "review",
            ),
            Some(1),
            &[],
        ),
        linked(
            render_entry(
                TranscriptKind::Message,
                "@reviewer",
                Some("@coder"),
                "2026-06-28T04:01:00Z",
                "clear to PR",
            ),
            Some(2),
            &[1],
        ),
    ];

    let display = assemble_threads(&entries, 0, false);

    assert!(display[0].lane.is_margin());
    assert!(!display[1].lane.is_margin());
    assert_eq!(display[1].block, display[0].block);
}

#[test]
fn thread_assembly_unions_multi_parent_turns_and_orphans_stay_flat() {
    let entries = vec![
        linked(entry("2026-06-28T04:00:00Z", "first"), Some(1), &[]),
        linked(entry("2026-06-28T04:01:00Z", "second"), Some(2), &[]),
        linked(
            assistant_entry("2026-06-28T04:02:00Z", "combined"),
            None,
            &[1, 2],
        ),
        linked(
            assistant_entry("2026-06-28T04:03:00Z", "missing parent"),
            None,
            &[99],
        ),
    ];

    let display = assemble_threads(&entries, 0, false);

    assert_eq!(display[0].entry.chat.text, "first");
    assert!(display[0].lane.is_margin());
    assert!(!display[1].lane.is_margin());
    assert!(!display[2].lane.is_margin());
    assert_eq!(display[3].entry.chat.text, "missing parent");
    assert!(display[3].lane.is_margin());
}

#[test]
fn unlinked_turn_outputs_join_the_latest_opener_for_their_agent() {
    let mut other_agent_output = assistant_entry("2026-06-28T04:05:00Z", "other agent");
    other_agent_output.agent = agent_key_for("codex", "sess-2");
    let entries = vec![
        entry("2026-06-28T04:00:00Z", "first prompt"),
        assistant_entry("2026-06-28T04:01:00Z", "first reply"),
        entry("2026-06-28T04:02:00Z", "second prompt"),
        ask_entry("2026-06-28T04:03:00Z", "second output"),
        render_entry(
            TranscriptKind::Error,
            "@claude",
            None,
            "2026-06-28T04:04:00Z",
            "second error",
        ),
        other_agent_output,
    ];

    let display = assemble_threads(&entries, 0, false);

    assert_eq!(
        display
            .iter()
            .map(|entry| entry.entry.chat.text.as_str())
            .collect::<Vec<_>>(),
        vec![
            "first prompt",
            "first reply",
            "second prompt",
            "second output",
            "second error",
            "other agent"
        ]
    );
    assert!(display[0].lane.is_margin());
    assert!(!display[1].lane.is_margin());
    assert!(display[2].lane.is_margin());
    assert!(!display[3].lane.is_margin());
    assert!(!display[4].lane.is_margin());
    assert!(display[5].lane.is_margin());
    assert_eq!(display[0].block, display[1].block);
    assert_eq!(display[2].block, display[3].block);
    assert_eq!(display[2].block, display[4].block);
    assert_ne!(display[0].block, display[2].block);
}

#[test]
fn flat_and_last_apply_to_display_order() {
    let entries = vec![
        linked(entry("2026-06-28T04:00:00Z", "root a"), Some(1), &[]),
        linked(entry("2026-06-28T04:01:00Z", "root b"), Some(2), &[]),
        linked(
            assistant_entry("2026-06-28T04:02:00Z", "reply a"),
            None,
            &[1],
        ),
        linked(
            assistant_entry("2026-06-28T04:03:00Z", "reply b"),
            None,
            &[2],
        ),
    ];

    let flat = assemble_threads(&entries, 0, true);
    assert_eq!(
        flat.iter()
            .map(|entry| entry.entry.chat.text.as_str())
            .collect::<Vec<_>>(),
        vec!["root a", "root b", "reply a", "reply b"]
    );

    let mut threaded = assemble_threads(&entries, 0, false);
    keep_last_blocks(&mut threaded, Some(3));
    assert_eq!(
        threaded
            .iter()
            .map(|entry| entry.entry.chat.text.as_str())
            .collect::<Vec<_>>(),
        vec!["root a", "reply a", "root b", "reply b"]
    );

    let mut boundary_tail = assemble_threads(&entries, 0, false);
    keep_last_blocks(&mut boundary_tail, Some(2));
    assert_eq!(
        boundary_tail
            .iter()
            .map(|entry| entry.entry.chat.text.as_str())
            .collect::<Vec<_>>(),
        vec!["root b", "reply b"]
    );

    let view = RenderedChat {
        channel: Some("chat".to_owned()),
        focus: None,
        entries,
        archive_prefix: 0,
        archived_hidden: 0,
        newest_archived_at: None,
        empty_message: None,
        last: Some(3),
        flat: false,
    };
    assert_eq!(
        selected_chat_lines(&view)
            .iter()
            .map(|entry| entry.text.as_str())
            .collect::<Vec<_>>(),
        vec!["root a", "root b", "reply a", "reply b"],
        "JSON selection returns the display tail in chronological order"
    );
}

#[test]
fn threaded_render_puts_replies_behind_a_spine() {
    let mut root_a = render_entry(
        TranscriptKind::Message,
        "@planner",
        Some("@coder"),
        "2026-06-28T04:00:00Z",
        "handoff",
    );
    root_a = linked(root_a, Some(1), &[]);
    let root_b = linked(
        render_entry(
            TranscriptKind::Message,
            "@reviewer",
            Some("@planner"),
            "2026-06-28T04:01:00Z",
            "review",
        ),
        Some(2),
        &[],
    );
    let reply_a = linked(
        render_entry(
            TranscriptKind::Assistant,
            "@coder",
            None,
            "2026-06-29T04:02:00Z",
            "question",
        ),
        None,
        &[1],
    );
    let reply_b = linked(
        render_entry(
            TranscriptKind::Assistant,
            "@planner",
            None,
            "2026-06-28T04:03:00Z",
            "answer",
        ),
        None,
        &[2],
    );

    let out = render_threaded(
        &[root_a, root_b, reply_a, reply_b],
        jiff::civil::date(2026, 6, 28),
    );

    let first_root = out.find("handoff").unwrap();
    let first_reply = out.find("│ @coder").unwrap();
    let second_root = out.find("review").unwrap();
    assert!(
        first_root < first_reply && first_reply < second_root,
        "{out}"
    );
    assert!(out.contains("│   question"), "{out}");
    assert!(out.contains("Jun 29 2026 · 00:02"), "{out}");
}

fn log_entry(
    kind: &str,
    session_id: &str,
    entry: TranscriptKind,
    from: Option<&str>,
    text: &str,
) -> TranscriptEntry {
    let mut entry = TranscriptEntry::new(
        ts("2026-06-01T00:00:00Z"),
        AgentKind::new_unchecked(kind),
        AgentSessionId::from(session_id),
        entry,
        text.to_owned(),
    );
    entry.channel = Some("chat".to_owned());
    entry.from = from.map(ToOwned::to_owned);
    entry
}

fn transcript_paths() -> (tempfile::TempDir, rimz::StatePaths) {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let workspace_id = rimz::ids::WorkspaceId::from_project_root(dir.path());
    let paths = rimz::StatePaths::under(workspace_id, dir.path()).expect("state paths");
    (dir, paths)
}

fn run_record(paths: &rimz::StatePaths, agent_id: Option<&str>) -> rimz::RunId {
    let mut record = rimz::harness::run::RunRecord::new(
        paths.workspace_id.clone(),
        AgentKind::new_unchecked("codex"),
        rimz::harness::run::PermissionMode::Auto,
        "prompt".to_owned(),
        std::path::Path::new("/tmp/rimz-run").to_path_buf(),
    );
    record.agent_id = agent_id.map(AgentSessionId::from);
    rimz::harness::run::create(paths, &record).expect("create run");
    record.run_id
}

#[test]
fn run_target_resolves_to_bound_agent_session() {
    let (_dir, paths) = transcript_paths();
    let run_id = run_record(&paths, Some("sess-1"));

    let target = resolve_run_target(&paths, Some(run_id.as_str())).expect("resolve run target");

    assert_eq!(target.as_deref(), Some("sess-1"));
}

#[test]
fn run_target_rejects_unbound_run() {
    let (_dir, paths) = transcript_paths();
    let run_id = run_record(&paths, None);

    let err = resolve_run_target(&paths, Some(run_id.as_str())).expect_err("unbound run errors");

    assert!(
        err.to_string()
            .contains("has not bound an agent session yet"),
        "{err}"
    );
}

#[test]
fn run_target_leaves_non_run_target_unchanged() {
    let (_dir, paths) = transcript_paths();

    let target = resolve_run_target(&paths, Some("@codex#chat")).expect("resolve target");

    assert_eq!(target.as_deref(), Some("@codex#chat"));
}

fn transcript_entry(
    entry: TranscriptKind,
    text: &str,
    at: &str,
) -> rimz::transcript::TranscriptEntry {
    let mut entry = rimz::transcript::TranscriptEntry::new(
        ts(at),
        AgentKind::new_unchecked("claude"),
        AgentSessionId::from("sess-1"),
        entry,
        text.to_owned(),
    );
    entry.questions = vec![rimz::transcript::AskQuestion {
        question: "Choose path?".to_owned(),
        options: vec![ask_option("safe"), ask_option("fast")],
        multi_select: false,
        has_option_previews: false,
    }];
    entry
}

fn waiting_agent() -> rimz::agents::AgentState {
    let waiting_since = ts("2026-06-01T00:00:00Z");
    let mut agent =
        rimz::agents::AgentState::stub("claude", "sess-1", rimz::agents::AgentStatus::Waiting);
    agent.waiting_since = Some(waiting_since);
    agent.last_activity = waiting_since;
    agent
}

#[test]
fn latest_ask_view_returns_open_ask() {
    let (_dir, paths) = transcript_paths();
    let ask = transcript_entry(TranscriptKind::Ask, "details", "2026-06-01T00:00:00Z");
    rimz::transcript::append(&paths, &ask).expect("append ask");

    let view = latest_ask_view_from_paths(&paths, &waiting_agent())
        .expect("latest ask")
        .expect("open ask");

    assert_eq!(view.title, "Choose path?");
    assert_eq!(view.body.as_deref(), Some("details"));
    assert_eq!(view.options, vec!["safe".to_owned(), "fast".to_owned()]);
}

#[test]
fn latest_ask_view_returns_none_after_later_answer() {
    let (_dir, paths) = transcript_paths();
    let ask = transcript_entry(TranscriptKind::Ask, "details", "2026-06-01T00:00:00Z");
    let answer = transcript_entry(TranscriptKind::Answer, "safe", "2026-06-01T00:00:01Z");
    rimz::transcript::append(&paths, &ask).expect("append ask");
    rimz::transcript::append(&paths, &answer).expect("append answer");

    let view = latest_ask_view_from_paths(&paths, &waiting_agent()).expect("latest ask");

    assert!(view.is_none());
}

#[test]
fn latest_ask_view_ignores_older_answer() {
    let (_dir, paths) = transcript_paths();
    let answer = transcript_entry(TranscriptKind::Answer, "safe", "2026-06-01T00:00:00Z");
    let ask = transcript_entry(TranscriptKind::Ask, "details", "2026-06-01T00:00:01Z");
    rimz::transcript::append(&paths, &answer).expect("append answer");
    rimz::transcript::append(&paths, &ask).expect("append ask");

    let view = latest_ask_view_from_paths(&paths, &waiting_agent())
        .expect("latest ask")
        .expect("open ask");

    assert_eq!(view.title, "Choose path?");
}

#[test]
fn latest_ask_view_skips_log_when_agent_not_waiting() {
    let (_dir, paths) = transcript_paths();
    let mut agent = waiting_agent();
    agent.status = rimz::agents::AgentStatus::Idle;

    let view = latest_ask_view_from_paths(&paths, &agent).expect("latest ask");

    assert!(view.is_none());
    assert!(!paths.transcript_dir.exists());
}

#[test]
fn message_entry_projects_structured_sender_and_receiver() {
    let mut entry = log_entry(
        "claude",
        "receiver",
        TranscriptKind::Message,
        Some("@planner"),
        "ship it",
    );
    entry.message_id = Some(rimz::ids::MessageId::parse("msg_0123456789abcdef").unwrap());
    entry.reply_to = vec![rimz::ids::MessageId::parse("msg_123456789abcdef0").unwrap()];
    let identities = build_identities(std::slice::from_ref(&entry));

    let chat = chat_entry_for_log_entry(&entry, &identities, false);

    assert_eq!(chat.from, "@planner");
    assert_eq!(chat.to.as_deref(), Some("@claude"));
    assert_eq!(chat.text, "ship it");
    assert_eq!(chat.message_id.as_deref(), Some("msg_0123456789abcdef"));
    assert_eq!(chat.reply_to, vec!["msg_123456789abcdef0"]);
    let json = serde_json::to_value(&chat).unwrap();
    assert_eq!(json["message_id"], "msg_0123456789abcdef");
    assert_eq!(json["reply_to"][0], "msg_123456789abcdef0");
}

#[test]
fn focus_keeps_messages_sent_by_the_focal_agent() {
    let focal = log_entry("claude", "sender", TranscriptKind::Prompt, None, "start");
    let sent = log_entry(
        "codex",
        "receiver",
        TranscriptKind::Message,
        Some("@claude"),
        "ack",
    );
    let local = log_entry("codex", "receiver", TranscriptKind::Prompt, None, "local");
    let identities = build_identities(&[focal.clone(), sent.clone()]);
    let scope = Scope {
        channel: Some("chat".to_owned()),
        channel_filter: Some("chat".to_owned()),
        focus: Some("@claude".to_owned()),
        focus_keys: Some(BTreeSet::from([entry_key(&focal)])),
        include_channel: false,
    };

    let sent_chat = chat_entry_for_log_entry(&sent, &identities, false);
    let local_chat = chat_entry_for_log_entry(&local, &identities, false);

    assert!(entry_matches_focus(&sent, &sent_chat, &scope, &identities));
    assert!(!entry_matches_focus(
        &local,
        &local_chat,
        &scope,
        &identities
    ));
}

#[test]
fn chat_renders_configured_zone_and_day_boundaries() {
    let today = jiff::civil::date(2026, 6, 28);
    let out = render(
        &[
            entry("2026-06-27T03:30:00Z", "late friday"),
            entry("2026-06-27T14:00:00Z", "saturday"),
            entry("2026-06-28T16:00:00Z", "today"),
        ],
        today,
    );

    let friday = out.find("──── Fri, Jun 26 2026 ────").expect("friday");
    let saturday = out.find("──── Sat, Jun 27 2026 ────").expect("saturday");
    let today = out.find("──── Today ").expect("today");
    assert!(friday < saturday);
    assert!(saturday < today);
    assert!(out.contains("23:30"));
    assert!(out.contains("10:00"));
    assert!(out.contains("12:00"));
    assert!(!out.contains("03:30:00"));
}

#[test]
fn today_only_chat_omits_day_delimiter() {
    let out = render(
        &[entry("2026-06-28T04:30:00Z", "same day")],
        jiff::civil::date(2026, 6, 28),
    );

    assert!(!out.contains("────"));
    assert!(out.contains("00:30"));
}

#[test]
fn chat_renders_speaker_headers_and_body_indent() {
    let out = render(
        &[
            entry("2026-06-28T04:00:00Z", "hello\n\nagain"),
            assistant_entry("2026-06-28T04:00:01Z", "answer"),
        ],
        jiff::civil::date(2026, 6, 28),
    );

    assert!(
        out.contains("user → @claude  00:00\n  hello\n\n  again\n\n@claude  00:00\n  answer"),
        "{out}"
    );
    assert!(!out.contains("user:"), "{out}");
}

#[test]
fn chat_groups_same_sender_receiver_inside_window() {
    let out = render(
        &[
            entry("2026-06-28T04:00:00Z", "first"),
            entry("2026-06-28T04:04:59Z", "second"),
            entry("2026-06-28T04:10:01Z", "third"),
        ],
        jiff::civil::date(2026, 6, 28),
    );

    assert_eq!(out.matches("user → @claude").count(), 2, "{out}");
    assert!(out.contains("first\n  second"), "{out}");
}

#[test]
fn chat_breaks_group_on_receiver_change_and_ask_cards() {
    let mut routed = entry("2026-06-28T04:02:00Z", "to codex");
    routed.chat.to = Some("@codex".to_owned());
    let out = render(
        &[
            entry("2026-06-28T04:00:00Z", "first"),
            routed,
            ask_entry("2026-06-28T04:03:00Z", "Approve tool?"),
            entry("2026-06-28T04:04:00Z", "after ask"),
        ],
        jiff::civil::date(2026, 6, 28),
    );

    assert_eq!(out.matches("user →").count(), 3, "{out}");
    assert!(out.contains("  ▌ Approve tool?\n  ▌ ◌ unanswered"), "{out}");
}

#[test]
fn mention_painting_highlights_agents_and_channels() {
    let raw = paint_mentions_with("ping @codex in (#feat-auth). keep email@host plain", None);

    assert!(raw.contains("\u{1b}["), "{raw:?}");
    assert!(raw.contains("@codex"), "{raw}");
    assert!(raw.contains("#feat-auth"), "{raw}");
    assert!(raw.contains(&render::paint(render::palette::COOL.bold(), "#feat-auth")));
    assert!(raw.contains("email@host"), "{raw}");
}

#[test]
fn structured_ask_card_folds_selected_answer_with_note() {
    let mut ask = ask_entry("2026-06-28T18:00:00Z", "I checked both paths.");
    ask.chat.questions = vec![rimz::transcript::AskQuestion {
        question: "Choose deployment path?".to_owned(),
        options: vec![
            described_ask_option("safe", "Use staged rollout with rollback ready."),
            described_ask_option("fast", "Ship immediately and monitor closely."),
        ],
        multi_select: false,
        has_option_previews: false,
    }];
    let mut answer = answer_entry("2026-06-28T18:01:00Z", "safe");
    answer.chat.answers = vec![rimz::transcript::AskAnswer {
        question: Some("Choose deployment path?".to_owned()),
        chosen: vec!["safe".to_owned()],
        note: Some("use prod window".to_owned()),
    }];

    let out = render(&[ask, answer], jiff::civil::date(2026, 6, 28));

    assert!(
        out.contains("@claude  14:00\n  I checked both paths."),
        "{out}"
    );
    assert!(out.contains("  ▌ Choose deployment path?"), "{out}");
    assert!(
        out.contains("  ▌ ● safe — you · “use prod window”"),
        "{out}"
    );
    assert!(
        out.contains("  ▌     Use staged rollout with rollback ready."),
        "{out}"
    );
    assert!(out.contains("  ▌ ○ fast"), "{out}");
    assert!(
        out.contains("  ▌     Ship immediately and monitor closely."),
        "{out}"
    );
    assert!(!out.contains("you → @claude"), "{out}");

    let mut raw_ask = ask_entry("2026-06-28T18:00:00Z", "");
    raw_ask.chat.questions = vec![rimz::transcript::AskQuestion {
        question: "Choose deployment path?".to_owned(),
        options: vec![
            described_ask_option("safe", "Tell @ops before rollout."),
            ask_option("fast"),
        ],
        multi_select: false,
        has_option_previews: false,
    }];
    let mut raw_answer = answer_entry("2026-06-28T18:01:00Z", "safe");
    raw_answer.chat.answers = vec![rimz::transcript::AskAnswer {
        question: Some("Choose deployment path?".to_owned()),
        chosen: vec!["safe".to_owned()],
        note: None,
    }];
    let raw = render_raw(&[raw_ask, raw_answer], jiff::civil::date(2026, 6, 28));
    assert!(raw.contains(&render::paint(render::palette::GOOD.bold(), "safe")));
    assert!(raw.contains(&render::paint(render::palette::FAINT, "@ops")));
    assert!(!raw.contains(&render::paint(render::palette::COOL.bold(), "@ops")));
}

#[test]
fn structured_ask_card_renders_other_and_multi_question_answers() {
    let mut ask = ask_entry("2026-06-28T18:00:00Z", "");
    ask.chat.questions = vec![
        rimz::transcript::AskQuestion {
            question: "Merge strategy?".to_owned(),
            options: vec![ask_option("squash"), ask_option("rebase")],
            multi_select: false,
            has_option_previews: false,
        },
        rimz::transcript::AskQuestion {
            question: "Notify team?".to_owned(),
            options: vec![ask_option("yes"), ask_option("no")],
            multi_select: false,
            has_option_previews: false,
        },
    ];
    let mut answer = answer_entry("2026-06-28T18:01:00Z", "live repro first\nyes");
    answer.chat.answers = vec![
        rimz::transcript::AskAnswer {
            question: Some("Notify team?".to_owned()),
            chosen: vec!["yes".to_owned()],
            note: None,
        },
        rimz::transcript::AskAnswer {
            question: Some("Merge strategy?".to_owned()),
            chosen: vec!["live repro first".to_owned()],
            note: None,
        },
    ];

    let out = render(&[ask, answer], jiff::civil::date(2026, 6, 28));

    assert!(out.contains("  ▌ ● other: live repro first — you"), "{out}");
    assert!(out.contains("  ▌\n  ▌ Notify team?"), "{out}");
    assert!(out.contains("  ▌ ● yes — you"), "{out}");
}

#[test]
fn text_ask_card_folds_text_answer_or_stays_unanswered() {
    let ask = ask_entry("2026-06-28T18:00:00Z", "Choose path? [safe, fast]");
    let answer = answer_entry("2026-06-28T18:01:00Z", "safe");

    let answered = render(&[ask.clone(), answer], jiff::civil::date(2026, 6, 28));
    let unanswered = render(&[ask], jiff::civil::date(2026, 6, 28));

    assert!(
        answered.contains("  ▌ Choose path? [safe, fast]\n  ▌ ● safe — you"),
        "{answered}"
    );
    assert!(
        unanswered.contains("  ▌ Choose path? [safe, fast]\n  ▌ ◌ unanswered"),
        "{unanswered}"
    );
}

#[test]
fn exit_plan_text_falls_back_to_text_card() {
    let out = render(
        &[ask_entry(
            "2026-06-28T18:00:00Z",
            "Requesting plan approval:\n\n1. Edit parser\n2. Run tests",
        )],
        jiff::civil::date(2026, 6, 28),
    );

    assert!(out.contains("  ▌ Requesting plan approval:"), "{out}");
    assert!(out.contains("  ▌ 1. Edit parser"), "{out}");
    assert!(out.contains("  ▌ ◌ unanswered"), "{out}");
}

#[test]
fn question_lines_paint_mentions() {
    let mut ask = ask_entry("2026-06-28T18:00:00Z", "");
    ask.chat.questions = vec![rimz::transcript::AskQuestion {
        question: "Ask @codex about #cli-docs?".to_owned(),
        options: vec![ask_option("yes"), ask_option("no")],
        multi_select: false,
        has_option_previews: false,
    }];

    let raw = render_raw(&[ask], jiff::civil::date(2026, 6, 28));

    assert!(raw.contains(&render::paint(render::palette::COOL.bold(), "@codex")));
    assert!(raw.contains(&render::paint(render::palette::COOL.bold(), "#cli-docs")));
}

#[test]
fn card_lines_wrap_with_spine_and_option_hanging_indent() {
    let mut ask = ask_entry("2026-06-28T18:00:00Z", "");
    ask.chat.questions = vec![rimz::transcript::AskQuestion {
            question: "Which deployment plan should the release captain choose when the fallback window is narrow and every reviewer needs one clear sentence of context?".to_owned(),
            options: vec![
                described_ask_option(
                    "safe path with a carefully staged rollout and a rollback checkpoint before traffic moves while the on-call lead watches dashboards and keeps incident notes open",
                    "Choose this path when stakeholders need an especially detailed explanation that keeps wrapping under the option description indentation.",
                ),
                ask_option("fast path"),
            ],
            multi_select: false,
            has_option_previews: false,
        }];

    let out = render(&[ask], jiff::civil::date(2026, 6, 28));

    assert!(
        out.contains("\n  ▌ every reviewer needs one clear sentence"),
        "{out}"
    );
    assert!(
        out.contains("\n  ▌   the on-call lead watches dashboards"),
        "{out}"
    );
    assert!(
        out.contains("\n  ▌     Choose this path when stakeholders need"),
        "{out}"
    );
    assert!(
        out.contains("\n  ▌     wrapping under the option description indentation."),
        "{out}"
    );
}

#[test]
fn answer_without_matching_agent_ask_stays_plain() {
    let ask = ask_entry("2026-06-28T18:00:00Z", "Native question?");
    let mut answer = answer_entry("2026-06-28T18:01:00Z", "allow");
    answer.agent = agent_key_for("codex", "sess-2");
    answer.chat.to = Some("@codex".to_owned());

    let out = render(&[ask, answer], jiff::civil::date(2026, 6, 28));

    assert!(
        out.contains("  ▌ Native question?\n  ▌ ◌ unanswered"),
        "{out}"
    );
    assert!(out.contains("you → @codex"), "{out}");
    assert!(out.contains("  allow"), "{out}");
}

#[test]
fn timestamped_asks_advance_day_delimiter() {
    let out = render(
        &[
            entry("2026-06-27T14:00:00Z", "yesterday"),
            ask_entry("2026-06-28T16:00:00Z", "Approve tool?"),
        ],
        jiff::civil::date(2026, 6, 28),
    );

    let yesterday = out.find("──── Sat, Jun 27 2026 ────").expect("yesterday");
    let today = out.find("──── Today ").expect("today");
    let ask = out.find("Approve tool?").expect("ask");
    assert!(yesterday < today);
    assert!(today < ask);
}

#[test]
fn archive_prefix_renders_archive_and_live_markers() {
    let out = render_with_archive(
        &[
            entry("2026-06-28T14:00:00Z", "prior life"),
            entry("2026-06-28T15:00:00Z", "current life"),
        ],
        1,
        jiff::civil::date(2026, 6, 28),
    );

    let archive = out
        .find("History archive · earlier today · 10:00")
        .expect("archive marker");
    let live = out
        .find("Live session · earlier today · 11:00")
        .expect("live marker");
    assert!(archive < live, "{out}");
    assert!(out.contains("prior life"), "{out}");
    assert!(out.contains("current life"), "{out}");
}

#[test]
fn archive_prefix_zero_keeps_plain_chat_shape() {
    let out = render_with_archive(
        &[entry("2026-06-28T04:30:00Z", "same day")],
        0,
        jiff::civil::date(2026, 6, 28),
    );

    assert_eq!(out, "user → @claude  00:30\n  same day\n");
}

#[test]
fn format_marker_when_uses_time_today_and_full_date_otherwise() {
    let tz = TimeZone::get("America/New_York").expect("timezone");
    let today = jiff::civil::date(2026, 6, 28);

    assert_eq!(
        format_marker_when(ts("2026-06-28T14:05:00Z"), &tz, today),
        "earlier today · 10:05"
    );
    assert_eq!(
        format_marker_when(ts("2026-06-27T14:05:00Z"), &tz, today),
        "Sat, Jun 27 2026"
    );
}

#[test]
fn live_boundary_uses_channel_cohort_or_focus_key() {
    let channel_a = agent_key_for("claude", "sess-a");
    let channel_b = agent_key_for("codex", "sess-b");
    let other = agent_key_for("claude", "sess-other");
    let live = vec![
        LiveRootAgent {
            key: channel_a.clone(),
            channel: Some("chat".to_owned()),
            registered_at: Some(ts("2026-06-01T00:03:00Z")),
        },
        LiveRootAgent {
            key: channel_b,
            channel: Some("chat".to_owned()),
            registered_at: Some(ts("2026-06-01T00:02:00Z")),
        },
        LiveRootAgent {
            key: other.clone(),
            channel: Some("elsewhere".to_owned()),
            registered_at: Some(ts("2026-06-01T00:01:00Z")),
        },
    ];

    assert_eq!(
        live_boundary(&single_channel_scope("chat".to_owned()), &live),
        Some(ts("2026-06-01T00:02:00Z"))
    );
    assert_eq!(
        live_boundary(
            &Scope {
                channel: Some("chat".to_owned()),
                channel_filter: Some("chat".to_owned()),
                focus: Some("@claude".to_owned()),
                focus_keys: Some(BTreeSet::from([other])),
                include_channel: false,
            },
            &live,
        ),
        Some(ts("2026-06-01T00:01:00Z"))
    );
    assert_eq!(
        live_boundary(&single_channel_scope("missing".to_owned()), &live),
        None
    );
}

#[test]
fn channel_filter_matches_exact_lanes() {
    assert!(channel_matches(Some("web-token"), Some("web-token")));
    assert!(!channel_matches(Some("web-token/forge"), Some("web-token")));
    assert!(!channel_matches(
        Some("web-token-other/forge"),
        Some("web-token")
    ));
    assert!(!channel_matches(
        Some("web-token/forge"),
        Some("web-token/ops")
    ));
    assert!(channel_matches(Some("web-token/forge"), None));
}
