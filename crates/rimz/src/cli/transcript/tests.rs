use super::*;

fn ts(raw: &str) -> jiff::Timestamp {
    raw.parse().expect("timestamp")
}

fn agent_key() -> AgentKey {
    (
        AgentKind::new_unchecked("claude"),
        AgentSessionId::from("sess-1"),
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
        request_id: None,
        agent: agent_key(),
        chat: rimz::agents::ChatEntry {
            from: from.to_owned(),
            to: to.map(ToOwned::to_owned),
            at: Some(ts(at)),
            text: text.to_owned(),
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

fn ask_option(label: &str) -> AskOption {
    AskOption::from(label.to_owned())
}

fn described_ask_option(label: &str, description: &str) -> AskOption {
    AskOption {
        label: label.to_owned(),
        description: Some(description.to_owned()),
    }
}

fn render(entries: &[RenderEntry], today: Date) -> String {
    let tz = TimeZone::get("America/New_York").expect("timezone");
    let mut out = anstream::StripStream::new(Vec::new());
    render_chat_to(&mut out, None, entries, &tz, today).expect("render");
    String::from_utf8(out.into_inner()).expect("utf8")
}

fn render_raw(entries: &[RenderEntry], today: Date) -> String {
    let tz = TimeZone::get("America/New_York").expect("timezone");
    let mut out = Vec::new();
    render_chat_to(&mut out, None, entries, &tz, today).expect("render");
    String::from_utf8(out).expect("utf8")
}

fn log_entry(
    kind: &str,
    session_id: &str,
    entry: TranscriptKind,
    from: Option<&str>,
    text: &str,
) -> TranscriptEntry {
    TranscriptEntry {
        at: ts("2026-06-01T00:00:00Z"),
        kind: AgentKind::new_unchecked(kind),
        agent_id: AgentSessionId::from(session_id),
        channel: Some("chat".to_owned()),
        name: None,
        profile: None,
        role: None,
        entry,
        request_id: None,
        from: from.map(ToOwned::to_owned),
        text: text.to_owned(),
        questions: Vec::new(),
        answers: Vec::new(),
    }
}

#[test]
fn message_entry_projects_structured_sender_and_receiver() {
    let entry = log_entry(
        "claude",
        "receiver",
        TranscriptKind::Message,
        Some("@planner"),
        "ship it",
    );
    let identities = build_identities(std::slice::from_ref(&entry));

    let chat = chat_entry_for_log_entry(&entry, &identities, false);

    assert_eq!(chat.from, "@planner");
    assert_eq!(chat.to.as_deref(), Some("@claude"));
    assert_eq!(chat.text, "ship it");
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
    assert!(raw.contains(&render::paint(render::palette::ACCENT.bold(), "#feat-auth")));
    assert!(raw.contains("email@host"), "{raw}");
}

#[test]
fn structured_ask_card_folds_selected_answer_with_note() {
    let request_id = RequestId::new();
    let mut ask = ask_entry("2026-06-28T18:00:00Z", "I checked both paths.");
    ask.request_id = Some(request_id.clone());
    ask.chat.questions = vec![rimz::agents::AskQuestion {
        question: "Choose deployment path?".to_owned(),
        options: vec![
            described_ask_option("safe", "Use staged rollout with rollback ready."),
            described_ask_option("fast", "Ship immediately and monitor closely."),
        ],
    }];
    let mut answer = answer_entry("2026-06-28T18:01:00Z", "safe");
    answer.request_id = Some(request_id);
    answer.chat.answers = vec![rimz::agents::AskAnswer {
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
    raw_ask.chat.questions = vec![rimz::agents::AskQuestion {
        question: "Choose deployment path?".to_owned(),
        options: vec![
            described_ask_option("safe", "Tell @ops before rollout."),
            ask_option("fast"),
        ],
    }];
    let mut raw_answer = answer_entry("2026-06-28T18:01:00Z", "safe");
    raw_answer.chat.answers = vec![rimz::agents::AskAnswer {
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
        rimz::agents::AskQuestion {
            question: "Merge strategy?".to_owned(),
            options: vec![ask_option("squash"), ask_option("rebase")],
        },
        rimz::agents::AskQuestion {
            question: "Notify team?".to_owned(),
            options: vec![ask_option("yes"), ask_option("no")],
        },
    ];
    let mut answer = answer_entry("2026-06-28T18:01:00Z", "live repro first\nyes");
    answer.chat.answers = vec![
        rimz::agents::AskAnswer {
            question: Some("Notify team?".to_owned()),
            chosen: vec!["yes".to_owned()],
            note: None,
        },
        rimz::agents::AskAnswer {
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
fn legacy_ask_card_folds_text_answer_or_stays_unanswered() {
    let mut ask = ask_entry("2026-06-28T18:00:00Z", "Choose path? [safe, fast]");
    ask.request_id = Some(RequestId::new());
    let mut answer = answer_entry("2026-06-28T18:01:00Z", "safe");
    answer.request_id = ask.request_id.clone();

    let answered = render(&[ask.clone(), answer], jiff::civil::date(2026, 6, 28));
    let unanswered = render(&[ask], jiff::civil::date(2026, 6, 28));

    assert!(
        answered.contains("  ▌ Choose path?\n  ▌ ● safe — you"),
        "{answered}"
    );
    assert!(answered.contains("  ▌ ○ fast"), "{answered}");
    assert!(
        unanswered.contains("  ▌ Choose path?\n  ▌ ○ safe\n  ▌ ○ fast\n  ▌ ◌ unanswered"),
        "{unanswered}"
    );
}

#[test]
fn legacy_ask_card_parses_lead_in_notes_and_multiselect_answers() {
    let ask = ask_entry(
        "2026-06-28T18:00:00Z",
        "Here is my read.\n\nChoose scopes? [a, b, c]\nNotify #cli-docs? [yes, no]",
    );
    let mut answer = answer_entry("2026-06-28T18:01:00Z", "a, b (note: least risky)\nyes");
    answer.request_id = ask.request_id.clone();

    let out = render(&[ask, answer], jiff::civil::date(2026, 6, 28));

    assert!(
        out.contains("  Here is my read.\n  ▌ Choose scopes?"),
        "{out}"
    );
    assert!(out.contains("  ▌ ● a — you · “least risky”"), "{out}");
    assert!(out.contains("  ▌ ● b"), "{out}");
    assert!(out.contains("  ▌ ○ c"), "{out}");
    assert!(out.contains("  ▌ Notify #cli-docs?"), "{out}");
    assert!(out.contains("  ▌ ● yes — you"), "{out}");
}

#[test]
fn legacy_exit_plan_text_falls_back_to_text_card() {
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
    ask.chat.questions = vec![rimz::agents::AskQuestion {
        question: "Ask @codex about #cli-docs?".to_owned(),
        options: vec![ask_option("yes"), ask_option("no")],
    }];

    let raw = render_raw(&[ask], jiff::civil::date(2026, 6, 28));

    assert!(raw.contains(&render::paint(render::palette::COOL.bold(), "@codex")));
    assert!(raw.contains(&render::paint(render::palette::ACCENT.bold(), "#cli-docs")));
}

#[test]
fn card_lines_wrap_with_spine_and_option_hanging_indent() {
    let mut ask = ask_entry("2026-06-28T18:00:00Z", "");
    ask.chat.questions = vec![rimz::agents::AskQuestion {
            question: "Which deployment plan should the release captain choose when the fallback window is narrow and every reviewer needs one clear sentence of context?".to_owned(),
            options: vec![
                described_ask_option(
                    "safe path with a carefully staged rollout and a rollback checkpoint before traffic moves while the on-call lead watches dashboards and keeps incident notes open",
                    "Choose this path when stakeholders need an especially detailed explanation that keeps wrapping under the option description indentation.",
                ),
                ask_option("fast path"),
            ],
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
fn request_id_answer_without_matching_ask_stays_plain() {
    let mut ask = ask_entry("2026-06-28T18:00:00Z", "Native question?");
    ask.request_id = Some(RequestId::new());
    let mut answer = answer_entry("2026-06-28T18:01:00Z", "allow");
    answer.request_id = Some(RequestId::new());

    let out = render(&[ask, answer], jiff::civil::date(2026, 6, 28));

    assert!(
        out.contains("  ▌ Native question?\n  ▌ ◌ unanswered"),
        "{out}"
    );
    assert!(out.contains("you → @claude"), "{out}");
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
