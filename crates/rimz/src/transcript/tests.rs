use super::*;
use crate::ids::WorkspaceId;
use serde_json::json;
use tempfile::tempdir;

fn ts(raw: &str) -> Timestamp {
    raw.parse().expect("timestamp")
}

fn paths() -> (tempfile::TempDir, StatePaths) {
    let dir = tempdir().expect("tempdir");
    let id = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(id, dir.path()).expect("state paths");
    (dir, paths)
}

#[test]
fn workspaces_with_channel_finds_transcript_evidence() {
    let dir = tempdir().expect("tempdir");
    let state_root = dir.path().join("state");
    let mut ids = Vec::new();
    for (name, channel) in [("alpha", "x"), ("beta", "y"), ("gone", "x")] {
        let project = dir.path().join(name);
        fs::create_dir_all(&project).expect("mkdir project");
        let project = project.canonicalize().expect("canonical project");
        let id = WorkspaceId::from_project_root(&project);
        let paths = StatePaths::under(id.clone(), &state_root).expect("state paths");
        fs::create_dir_all(&paths.root).expect("mkdir workspace");
        crate::workspace::record::write(
            &paths,
            &crate::workspace::record::WorkspaceRecord {
                layout: 2,
                workspace_id: id.clone(),
                project_root: project.clone(),
                worktree_root: None,
                session_name: format!("rimz-{name}"),
                root_class: crate::workspace::RootClass::Directory,
                rimz_bin: None,
                rimz_build: None,
                logins: None,
                updated_at: Timestamp::UNIX_EPOCH,
            },
        )
        .expect("write record");
        let mut line = entry(TranscriptKind::Prompt, "hi", "2026-06-01T00:00:00Z");
        line.channel = Some(channel.to_owned());
        append(&paths, &line).expect("append");
        ids.push(id);
    }
    fs::remove_dir_all(dir.path().join("gone")).expect("remove gone project");

    let found = workspaces_with_channel_under(&state_root, "x");

    assert_eq!(
        found
            .iter()
            .map(|workspace| &workspace.workspace_id)
            .collect::<Vec<_>>(),
        [&ids[0]]
    );
    assert!(workspaces_with_channel_under(&state_root, "z").is_empty());
}

fn entry(entry: TranscriptKind, text: &str, at: &str) -> TranscriptEntry {
    TranscriptEntry::new(
        ts(at),
        AgentKind::new_unchecked("claude"),
        AgentSessionId::from("sess-1"),
        entry,
        text.to_owned(),
    )
}

#[test]
fn reading_skips_legacy_paste_fragments_without_rewriting_the_log() {
    let (_dir, paths) = paths();
    // A real prompt carrying a paste pair mid-text is the user's, and a
    // harness-authored prompt is out of the predicate's reach.
    let genuine = "here is the trace\n\n<pasted_content id=\"e676\">\nthread panicked\n\
                   </pasted_content id=\"e676\">\n\nwhat now?";
    let mut harness = entry(
        TranscriptKind::Prompt,
        "<pasted_content id=\"e676\">",
        "2026-06-01T00:00:05Z",
    );
    harness.from = Some(HARNESS_FROM.to_owned());
    for line in [
        entry(
            TranscriptKind::Prompt,
            "<pasted_content id=\"e676\">",
            "2026-06-01T00:00:01Z",
        ),
        entry(TranscriptKind::Wait, "the delivery", "2026-06-01T00:00:02Z"),
        entry(
            TranscriptKind::Prompt,
            "</pasted_content id=\"e676\">",
            "2026-06-01T00:00:03Z",
        ),
        entry(TranscriptKind::Prompt, genuine, "2026-06-01T00:00:04Z"),
        harness,
    ] {
        append(&paths, &line).expect("append");
    }
    let bucket = bucket_path(&paths, ts("2026-06-01T00:00:01Z"));
    let bytes = fs::read(&bucket).expect("read bucket");

    let read = read_all(&paths).expect("read all");

    assert_eq!(
        read.iter()
            .map(|entry| (entry.entry, entry.text.as_str()))
            .collect::<Vec<_>>(),
        [
            (TranscriptKind::Wait, "the delivery"),
            (TranscriptKind::Prompt, genuine),
            (TranscriptKind::Prompt, "<pasted_content id=\"e676\">"),
        ],
    );
    assert_eq!(fs::read(&bucket).expect("reread bucket"), bytes);
}

#[test]
fn bucket_names_align_to_file_day_windows() {
    assert_eq!(
        bucket_file_name(ts("1970-01-08T00:00:00Z"), 1),
        "1970-01-08.jsonl"
    );
    assert_eq!(
        bucket_file_name(ts("1970-01-07T23:59:59Z"), 7),
        "1970-01-01.jsonl"
    );
    assert_eq!(
        bucket_file_name(ts("1970-01-08T00:00:00Z"), 7),
        "1970-01-08.jsonl"
    );
    assert_eq!(
        bucket_file_name(ts("1970-01-30T23:59:59Z"), 30),
        "1970-01-01.jsonl"
    );
    assert_eq!(
        bucket_file_name(ts("1970-01-31T00:00:00Z"), 30),
        "1970-01-31.jsonl"
    );
}

#[test]
fn chat_entry_round_trips_and_skips_empty_optionals() {
    for kind in [
        TranscriptKind::Prompt,
        TranscriptKind::Message,
        TranscriptKind::SubagentReport,
        TranscriptKind::Wait,
        TranscriptKind::Assistant,
        TranscriptKind::Ask,
        TranscriptKind::Answer,
        TranscriptKind::Error,
    ] {
        let entry = entry(kind, "hello", "2026-06-01T00:00:00Z");
        let json = serde_json::to_string(&entry).expect("serialize");
        assert!(!json.contains("request_id"));
        assert!(!json.contains("message_id"));
        assert!(!json.contains("reply_to"));
        assert!(!json.contains("channel"));
        assert!(!json.contains("questions"));
        assert!(!json.contains("answers"));
        let decoded: TranscriptEntry = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded, entry);
    }
}

#[test]
fn chat_entry_round_trips_message_causality_and_decodes_legacy_lines() {
    let mut entry = entry(TranscriptKind::Message, "hello", "2026-06-01T00:00:00Z");
    assert!(
        serde_json::to_value(&entry)
            .unwrap()
            .get("enqueued_at")
            .is_none()
    );
    entry.message_id = Some(MessageId::parse("msg_0123456789abcdef").unwrap());
    entry.enqueued_at = Some(ts("2026-05-31T23:58:00Z"));
    entry.reply_to = vec![MessageId::parse("msg_123456789abcdef0").unwrap()];

    let json = serde_json::to_value(&entry).expect("serialize");
    let decoded: TranscriptEntry = serde_json::from_value(json.clone()).expect("decode");
    assert_eq!(decoded, entry);

    let mut legacy = json;
    legacy.as_object_mut().unwrap().remove("message_id");
    legacy.as_object_mut().unwrap().remove("enqueued_at");
    legacy.as_object_mut().unwrap().remove("reply_to");
    let decoded: TranscriptEntry = serde_json::from_value(legacy).expect("legacy decode");
    assert_eq!(decoded.message_id, None);
    assert_eq!(decoded.enqueued_at, None);
    assert!(decoded.reply_to.is_empty());
}

#[test]
fn chat_entry_round_trips_structured_asks_and_answers() {
    let mut entry = entry(TranscriptKind::Ask, "lead-in", "2026-06-01T00:00:00Z");
    entry.questions = vec![AskQuestion {
        question: "Choose deployment path?".to_owned(),
        options: vec![
            AskOption::from("safe".to_owned()),
            AskOption::from("fast".to_owned()),
        ],
        multi_select: false,
        has_option_previews: false,
    }];
    entry.answers = vec![AskAnswer {
        question: Some("Choose deployment path?".to_owned()),
        chosen: vec!["safe".to_owned()],
        note: Some("use prod window".to_owned()),
    }];

    let json = serde_json::to_string(&entry).expect("serialize");
    assert!(json.contains("questions"));
    assert!(json.contains("answers"));
    let decoded: TranscriptEntry = serde_json::from_str(&json).expect("decode");

    assert_eq!(decoded, entry);
}

#[test]
fn read_all_decodes_mixed_option_shapes() {
    let (_dir, paths) = paths();
    fs::create_dir_all(&paths.transcript_dir).expect("mkdir transcript");
    fs::write(
        paths.transcript_dir.join("2026-06-01.jsonl"),
        serde_json::json!({
            "at": "2026-06-01T00:00:00Z",
            "kind": "claude",
            "agent_id": "sess-1",
            "entry": "ask",
            "text": "",
            "questions": [{
                "question": "Choose deployment path?",
                "options": [
                    {
                        "label": "safe",
                        "description": "Use staged rollout."
                    },
                    "fast"
                ]
            }]
        })
        .to_string(),
    )
    .expect("write log");

    let entries = read_all(&paths).expect("read log");

    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].questions[0].options,
        vec![
            AskOption {
                label: "safe".to_owned(),
                description: Some("Use staged rollout.".to_owned()),
                caution: None,
            },
            AskOption::from("fast".to_owned()),
        ]
    );
}

#[test]
fn read_all_sorts_by_timestamp_and_skips_malformed_lines() {
    let (_dir, paths) = paths();
    fs::create_dir_all(&paths.transcript_dir).expect("mkdir transcript");
    let first = entry(TranscriptKind::Prompt, "first", "2026-06-01T00:00:02Z");
    let second = entry(TranscriptKind::Assistant, "second", "2026-06-01T00:00:01Z");
    fs::write(
        paths.transcript_dir.join("2026-06-01.jsonl"),
        format!("{}\nnot json\n", serde_json::to_string(&first).unwrap()),
    )
    .expect("write first");
    fs::write(
        paths.transcript_dir.join("2026-06-08.jsonl"),
        format!("{}\n", serde_json::to_string(&second).unwrap()),
    )
    .expect("write second");

    let entries = read_all(&paths).expect("read log");

    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.text.as_str())
            .collect::<Vec<_>>(),
        vec!["second", "first"]
    );
}

#[test]
fn latest_assistant_picks_the_agents_newest_final_message() {
    let (_dir, paths) = paths();
    let older = entry(TranscriptKind::Assistant, "older", "2026-06-01T00:00:00Z");
    let newer = entry(TranscriptKind::Assistant, "newer", "2026-06-08T00:00:00Z");
    let prompt = entry(
        TranscriptKind::Prompt,
        "later prompt",
        "2026-06-08T00:00:01Z",
    );
    let mut other = entry(
        TranscriptKind::Assistant,
        "other agent",
        "2026-06-08T00:00:02Z",
    );
    other.agent_id = AgentSessionId::from("someone-else");
    for entry in [&older, &newer, &prompt, &other] {
        append(&paths, entry).expect("append entry");
    }

    let since = |at: &str| at.parse::<Timestamp>().expect("timestamp");
    let latest = |at| {
        latest_assistant(&paths, &newer.kind, &newer.agent_id, since(at))
            .expect("read log")
            .map(|entry| entry.text)
    };

    assert_eq!(latest("2026-06-01T00:00:00Z"), Some("newer".to_owned()));
    assert_eq!(latest("2026-06-08T00:00:01Z"), None);
}

#[test]
fn latest_open_ask_finds_unanswered_ask() {
    let (_dir, paths) = paths();
    let ask = entry(TranscriptKind::Ask, "approve?", "2026-06-01T00:00:00Z");
    append(&paths, &ask).expect("append ask");

    let open = latest_open_ask(&paths, &ask.kind, &ask.agent_id).expect("read ask");

    assert_eq!(
        open.as_ref().map(|entry| entry.text.as_str()),
        Some("approve?")
    );
}

#[test]
fn latest_open_ask_treats_later_answer_as_closed() {
    let (_dir, paths) = paths();
    let ask = entry(TranscriptKind::Ask, "approve?", "2026-06-01T00:00:00Z");
    let answer = entry(TranscriptKind::Answer, "yes", "2026-06-01T00:00:01Z");
    append(&paths, &ask).expect("append ask");
    append(&paths, &answer).expect("append answer");

    let open = latest_open_ask(&paths, &ask.kind, &ask.agent_id).expect("read ask");

    assert_eq!(open, None);
}

#[test]
fn latest_open_ask_pairs_answers_by_id() {
    let (_dir, paths) = paths();
    let mut old_ask = entry(TranscriptKind::Ask, "old?", "2026-06-01T00:00:00Z");
    old_ask.id = Some(AskId::parse("ask_0123456789abcdea").unwrap());
    let mut new_ask = entry(TranscriptKind::Ask, "new?", "2026-06-01T00:00:01Z");
    new_ask.id = Some(AskId::parse("ask_0123456789abcdeb").unwrap());
    let mut unrelated = entry(TranscriptKind::Answer, "old answer", "2026-06-01T00:00:02Z");
    unrelated.id = old_ask.id.clone();
    append(&paths, &old_ask).expect("append old ask");
    append(&paths, &new_ask).expect("append new ask");
    append(&paths, &unrelated).expect("append unrelated answer");

    let open = latest_open_ask(&paths, &new_ask.kind, &new_ask.agent_id)
        .expect("read ask")
        .expect("new ask stays open");
    assert_eq!(open.id, new_ask.id);

    let mut matching = entry(TranscriptKind::Answer, "new answer", "2026-06-01T00:00:03Z");
    matching.id = new_ask.id.clone();
    append(&paths, &matching).expect("append matching answer");
    assert!(
        latest_open_ask(&paths, &new_ask.kind, &new_ask.agent_id)
            .expect("read closed ask")
            .is_none()
    );
}

#[test]
fn latest_open_ask_newest_bucket_decides() {
    let (_dir, paths) = paths();
    let older_answer = entry(TranscriptKind::Answer, "yes", "2026-06-01T00:00:00Z");
    let newer_ask = entry(TranscriptKind::Ask, "approve?", "2026-06-08T00:00:00Z");
    append(&paths, &older_answer).expect("append answer");
    append(&paths, &newer_ask).expect("append ask");

    let open = latest_open_ask(&paths, &newer_ask.kind, &newer_ask.agent_id).expect("read ask");

    assert_eq!(
        open.as_ref().map(|entry| entry.text.as_str()),
        Some("approve?")
    );
}

#[test]
fn latest_open_ask_missing_dir_is_empty() {
    let (_dir, paths) = paths();

    let open = latest_open_ask(
        &paths,
        &AgentKind::new_unchecked("claude"),
        &AgentSessionId::from("sess-1"),
    )
    .expect("read missing dir");

    assert_eq!(open, None);
}

#[test]
fn append_creates_transcript_dir_lazily() {
    let (_dir, paths) = paths();
    let entry = entry(TranscriptKind::Prompt, "hello", "2026-06-01T00:00:00Z");

    append(&paths, &entry).expect("append");

    assert!(paths.transcript_dir.is_dir());
    assert_eq!(read_all(&paths).expect("read").len(), 1);
}

#[test]
fn answer_append_is_idempotent_by_ask_id() {
    let (_dir, paths) = paths();
    let mut answer = entry(TranscriptKind::Answer, "safe", "2026-06-01T00:00:00Z");
    answer.id = Some(AskId::parse("ask_0123456789abcdef").unwrap());

    assert!(append_answer_if_missing(&paths, &answer).expect("first append"));
    assert!(!append_answer_if_missing(&paths, &answer).expect("duplicate append"));
    assert_eq!(read_all(&paths).expect("read").len(), 1);
}

#[test]
fn ask_option_deserializes_legacy_string_shape() {
    let option: AskOption = serde_json::from_value(json!("safe")).expect("decode option");

    assert_eq!(
        option,
        AskOption {
            label: "safe".to_owned(),
            description: None,
            caution: None,
        }
    );
}

#[test]
fn ask_option_deserializes_object_shape() {
    let option: AskOption =
        serde_json::from_value(json!({"label": "safe", "description": "Use staged rollout"}))
            .expect("decode option");

    assert_eq!(
        option,
        AskOption {
            label: "safe".to_owned(),
            description: Some("Use staged rollout".to_owned()),
            caution: None,
        }
    );
}

#[test]
fn ask_option_deserializes_object_without_description() {
    let option: AskOption =
        serde_json::from_value(json!({"label": "safe"})).expect("decode option");

    assert_eq!(option, AskOption::from("safe".to_owned()));
}

#[test]
fn ask_option_serializes_label_only_as_string_and_description_as_object() {
    assert_eq!(
        serde_json::to_value(AskOption::from("safe".to_owned())).expect("serialize option"),
        json!("safe")
    );
    assert_eq!(
        serde_json::to_value(AskOption {
            label: "safe".to_owned(),
            description: Some("Use staged rollout".to_owned()),
            caution: None,
        })
        .expect("serialize option"),
        json!({"label": "safe", "description": "Use staged rollout"})
    );
}
