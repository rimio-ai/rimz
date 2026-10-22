use super::*;
use jiff::Timestamp;

#[test]
fn unnamed_history_zone_falls_back_to_published_utc() {
    let unnamed = jiff::tz::TimeZone::fixed(jiff::tz::offset(8));
    let (zone, name) = super::super::aggregate::history_zone(unnamed);
    assert_eq!(zone.to_offset(Timestamp::UNIX_EPOCH).seconds(), 0);
    assert_eq!(name, "UTC");
}

#[test]
fn history_days_follow_published_zone_including_rolled_rows() {
    let second = |s: &str| s.parse::<Timestamp>().unwrap().as_second() as u64;
    let file = PathBuf::from("/x/opencode.db");
    let mut rolled = cached_entry(second("2026-01-01T16:30:00Z"), 4.0, "old");
    rolled.rolled = true;
    let cache = SpendingDiskCache {
        files: HashMap::from([cached_file(
            &file,
            vec![
                cached_entry(second("2026-01-12T15:30:00Z"), 1.0, "before"),
                cached_entry(second("2026-01-12T16:30:00Z"), 2.0, "after"),
                rolled,
            ],
        )]),
        ..Default::default()
    };
    let files = vec![spending_file(opencode_adapter(), file)];
    let counted = dedup_cached_entries(&files, &cache).into_counted();
    let result = aggregate_counted_rollups(
        &files,
        &cache,
        &counted,
        None,
        HeadlineContext {
            user_inputs: &[],
            now_secs: second("2026-01-12T17:00:00Z"),
            spec: &HeadlineSpec {
                timezone: Some("Asia/Shanghai".into()),
                ..Default::default()
            },
        },
        Some(&Default::default()),
    );
    let day = |s| (second(s) / 86_400) as i64;
    assert_eq!(result.days[&day("2026-01-12T00:00:00Z")].usd, 1.0);
    assert_eq!(result.days[&day("2026-01-13T00:00:00Z")].usd, 2.0);
    assert_eq!(result.days[&day("2026-01-02T00:00:00Z")].usd, 4.0);
    assert_eq!(result.spending.total.day.usd, 2.0);
    assert_eq!(result.spending.total.week.usd, 3.0);
    assert_eq!(result.spending.total.day.sessions, 1);
    assert_eq!(result.spending.total.week.sessions, 2);
    assert_eq!(result.spending.total.headline.usd, 0.0);
    assert_eq!(result.models[""].tally.day.usd, 2.0);
    assert!(result.roles.subagents.is_empty());
    assert_eq!(result.spending.by_provider["opencode"].day.usd, 2.0);
    assert_eq!(
        result.spending.by_login.values().next().unwrap().day.usd,
        2.0
    );
    assert_eq!(
        result.spending.total.day.display_tokens(),
        result.days[&day("2026-01-13T00:00:00Z")].tokens
    );
    assert_eq!(
        result.days[&day("2026-01-13T00:00:00Z")].usd,
        result.provider_day.values().map(|day| day.usd).sum::<f64>()
    );
    let publication = ProviderSpendingCache::from_walk(&result, 0);
    assert_eq!(
        serde_json::to_value(publication).unwrap()["timezone"],
        "Asia/Shanghai"
    );
}

#[test]
fn model_identity_uses_year_spend_then_tokens_then_kind() {
    let paths = [
        PathBuf::from("/x/claude.jsonl"),
        PathBuf::from("/x/codex.jsonl"),
    ];
    let files = vec![
        spending_file(claude_adapter(), paths[0].clone()),
        spending_file(codex_adapter(), paths[1].clone()),
    ];
    for (claude_usd, claude_tokens, expected) in [
        (2.0, 1, "claude"),
        (1.0, 3, "claude"),
        (1.0, 1, "claude"),
        (0.5, 9, "codex"),
    ] {
        let mut claude = cached_entry(NOW_SECS, claude_usd, "claude-thread");
        claude.model = Some("shared-model".into());
        claude.input = claude_tokens;
        let codex = CachedEntry {
            cost_usd: 1.0,
            input: 1,
            thread_id: Some("codex-thread".into()),
            ..claude.clone()
        };
        let cache = SpendingDiskCache {
            files: HashMap::from([
                cached_file(&paths[0], vec![claude]),
                cached_file(&paths[1], vec![codex]),
            ]),
            ..Default::default()
        };
        let locations = dedup_cached_entry_locations(&files, &cache);
        let counted = indexed_counted_entries(&files, &cache, &locations);
        let result = aggregate_counted_rollups(
            &files,
            &cache,
            &counted,
            None,
            HeadlineContext {
                user_inputs: &[],
                now_secs: NOW_SECS,
                spec: &HeadlineSpec::default(),
            },
            Some(&Default::default()),
        );
        let models = serde_json::to_value(&result.models).unwrap();
        assert_eq!(models["shared-model"]["kind"], expected);
        let roles = serde_json::to_value(&result.roles).unwrap();
        assert_eq!(
            roles.as_object().unwrap().keys().collect::<Vec<_>>(),
            [
                "agents",
                "effort_since_secs",
                "recorded_since_secs",
                "subagents",
                "teams"
            ]
        );
        assert!(roles["agents"].as_object().unwrap().is_empty());
        assert!(roles["recorded_since_secs"].is_null());
        assert!(roles["subagents"].as_object().unwrap().is_empty());
        assert!(roles["teams"].as_object().unwrap().is_empty());
    }
}

#[test]
fn effort_uses_spend_windows_and_shared_placement_without_requiring_spend() {
    use super::super::session_effort::{SessionEffortEvent, SessionEffortRecord};
    use super::super::session_roles::{self, SessionRoleRecord};
    use crate::ids::{AgentKind, WorkspaceId};
    use serde_json::json;

    let dir = tempfile::tempdir().unwrap();
    let paths =
        crate::RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    let at = Timestamp::from_second(NOW_SECS as i64).unwrap();
    for (kind, session, role) in [
        (
            "opencode",
            "solo",
            json!({"role":"agent", "profile":"solo.dotted", "seat":"solo"}),
        ),
        (
            "claude",
            "team",
            json!({"role":"agent", "profile":"plain", "seat":{"team":"forge", "role":"coder"}}),
        ),
        (
            "claude",
            "legacy",
            json!({"role":"agent", "profile":"forge.planner"}),
        ),
        (
            "codex",
            "child",
            json!({"role":"subagent", "profile":"fast"}),
        ),
    ] {
        session_roles::record(
            &paths,
            &SessionRoleRecord {
                kind: AgentKind::new_unchecked(kind),
                session: session.into(),
                at,
                role: serde_json::from_value(role).unwrap(),
            },
        )
        .unwrap();
    }
    let spec = HeadlineSpec {
        timezone: Some("Asia/Shanghai".into()),
        ..Default::default()
    };
    let day = local_day_start_secs(NOW_SECS, spec.timezone.as_deref()).unwrap();
    let seconds = [
        NOW_SECS + 1,
        day,
        day - 1,
        NOW_SECS - 7 * 86_400 + 1,
        NOW_SECS - 7 * 86_400,
        NOW_SECS - 30 * 86_400 + 1,
        NOW_SECS - 30 * 86_400,
        NOW_SECS - 365 * 86_400 + 1,
        NOW_SECS - 365 * 86_400,
    ];
    let mut records: Vec<_> = seconds
        .iter()
        .map(|second| SessionEffortRecord {
            kind: AgentKind::new_unchecked("opencode"),
            session: "solo".into(),
            at: Timestamp::from_second(*second as i64).unwrap(),
            event: SessionEffortEvent::Active { active_ms: 1 },
        })
        .collect();
    let receiver = SessionEffortRecord {
        kind: AgentKind::new_unchecked("claude"),
        session: "team".into(),
        at,
        event: SessionEffortEvent::FromUser,
    };
    records.extend([
        receiver.clone(),
        SessionEffortRecord {
            event: SessionEffortEvent::FromAgent {
                sender_kind: Some(AgentKind::new_unchecked("opencode")),
                sender_session: Some("solo".into()),
            },
            ..receiver.clone()
        },
        SessionEffortRecord {
            event: SessionEffortEvent::FromAgent {
                sender_kind: None,
                sender_session: None,
            },
            ..receiver.clone()
        },
        SessionEffortRecord {
            session: "legacy".into(),
            event: SessionEffortEvent::Active { active_ms: 100 },
            ..receiver.clone()
        },
        SessionEffortRecord {
            session: "orphan".into(),
            event: SessionEffortEvent::FromAgent {
                sender_kind: Some(AgentKind::new_unchecked("codex")),
                sender_session: Some("child".into()),
            },
            ..receiver
        },
    ]);
    let segment = paths.shared_session_effort(at.to_zoned(jiff::tz::TimeZone::UTC).date());
    std::fs::create_dir_all(segment.parent().unwrap()).unwrap();
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, &record).unwrap();
        bytes.push(b'\n');
    }
    std::fs::write(segment, bytes).unwrap();
    let history = session_roles::load_history(&paths.shared_spending_cursor_path(), NOW_SECS);
    let transcript = PathBuf::from("/x/opencode.db");
    let files = vec![spending_file(opencode_adapter(), transcript.clone())];
    let entries = seconds
        .iter()
        .map(|second| CachedEntry {
            input: 1,
            output: 0,
            cache_write: 0,
            ..cached_entry(*second, 1.0, "solo")
        })
        .collect();
    let cache = SpendingDiskCache {
        files: HashMap::from([cached_file(&transcript, entries)]),
        ..Default::default()
    };
    let result = aggregate_walk_publish(&files, &cache, &[], NOW_SECS, None, &spec, &history);
    let roles = serde_json::to_value(&result.roles).unwrap();
    assert_eq!(roles["effort_since_secs"], NOW_SECS - 365 * 86_400);
    for window in ["day", "week", "month", "year"] {
        assert_eq!(
            roles["agents"]["solo.dotted"]["effort"][window]["active_ms"],
            roles["agents"]["solo.dotted"][window]["tokens"],
            "effort must match spend membership at {window} boundaries"
        );
        assert_eq!(
            roles["agents"]["solo.dotted"]["effort"][window]["to_teammates"],
            1
        );
        let team = &roles["teams"]["forge"]["coder"];
        assert_eq!(
            team[window]["usd"], 0.0,
            "effort alone creates a role entry"
        );
        assert_eq!(
            team["effort"][window],
            json!({"active_ms":0,"from_user":1,"from_teammates":2,"to_teammates":0})
        );
        assert_eq!(
            roles["teams"]["forge"]["planner"]["effort"][window]["active_ms"],
            100
        );
        assert_eq!(
            roles["subagents"]["fast"]["effort"][window]["to_teammates"], 1,
            "unknown receiver must not hide a known sender"
        );
    }
    assert_eq!(roles["agents"].as_object().unwrap().len(), 1);
    assert_eq!(roles["teams"]["forge"].as_object().unwrap().len(), 2);
    assert_eq!(roles["subagents"].as_object().unwrap().len(), 1);
}

#[test]
fn roles_join_native_sessions_after_dedup_and_conserve_every_window() {
    use super::super::session_roles::SessionRole;
    let parent = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let child = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let named = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
    let paths = [
        PathBuf::from(format!("/x/{parent}.jsonl")),
        PathBuf::from(format!("/x/{parent}/subagents/agent-native.jsonl")),
        PathBuf::from(format!("/x/rollout-2026-01-01T00-00-00-{child}.jsonl")),
        PathBuf::from("/x/opencode.db"),
        PathBuf::from(format!("/x/{named}.jsonl")),
    ];
    let mut root = cached_entry(NOW_SECS, 1.0, "");
    root.thread_id = None;
    root.model = Some("root-model".into());
    root.message_id = Some("m".into());
    root.request_id = Some("r".into());
    let replay = CachedEntry {
        is_sidechain: true,
        ..root.clone()
    };
    let native = CachedEntry {
        message_id: None,
        request_id: None,
        cost_usd: 2.0,
        ..replay.clone()
    };
    let codex = CachedEntry {
        message_id: None,
        request_id: None,
        cost_usd: 3.0,
        model: Some("child-model".into()),
        ..root.clone()
    };
    let files = vec![
        spending_file(claude_adapter(), paths[0].clone()),
        spending_file(claude_adapter(), paths[1].clone()),
        spending_file(codex_adapter(), paths[2].clone()),
        spending_file(opencode_adapter(), paths[3].clone()),
        spending_file(claude_adapter(), paths[4].clone()),
    ];
    let cache = SpendingDiskCache {
        files: HashMap::from([
            cached_file(&paths[0], vec![root]),
            cached_file(&paths[1], vec![replay, native]),
            cached_file(&paths[2], vec![codex]),
            cached_file(
                &paths[3],
                vec![
                    cached_entry(NOW_SECS - 10 * 86_400, 4.0, "peer"),
                    cached_entry(NOW_SECS - 40 * 86_400, 5.0, "unseen"),
                ],
            ),
            cached_file(
                &paths[4],
                vec![cached_entry(NOW_SECS - 2 * 86_400, 6.0, named)],
            ),
        ]),
        ..Default::default()
    };
    let roles: super::super::session_roles::RoleMap = BTreeMap::from([
        (
            crate::ids::AgentKind::new_unchecked("claude"),
            BTreeMap::from([
                (
                    parent.into(),
                    serde_json::from_value(serde_json::json!({
                        "role":"agent", "profile":"plain-profile",
                        "seat":{"team":"forge", "role":"coder"}
                    }))
                    .unwrap(),
                ),
                (
                    named.into(),
                    SessionRole::Agent {
                        profile: Some("forge.coder".into()),
                        seat: None,
                    },
                ),
            ]),
        ),
        (
            crate::ids::AgentKind::new_unchecked("codex"),
            BTreeMap::from([(
                child.into(),
                SessionRole::Subagent {
                    profile: "fast".into(),
                },
            )]),
        ),
        (
            crate::ids::AgentKind::new_unchecked("opencode"),
            BTreeMap::from([(
                "peer".into(),
                SessionRole::Agent {
                    profile: Some("peer-profile".into()),
                    seat: None,
                },
            )]),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let runtime = crate::disk::paths::RuntimePaths::under(
        crate::ids::WorkspaceId::from_project_root(dir.path()),
        dir.path(),
    )
    .unwrap();
    let first = NOW_SECS - 100;
    for (kind, sessions) in &roles {
        for (session, role) in sessions {
            super::super::session_roles::record(
                &runtime,
                &super::super::session_roles::SessionRoleRecord {
                    kind: kind.clone(),
                    session: session.clone(),
                    role: role.clone(),
                    at: Timestamp::from_second(
                        (first + u64::from(matches!(role, SessionRole::Subagent { .. }))) as i64,
                    )
                    .unwrap(),
                },
            )
            .unwrap();
        }
    }
    let roles = super::super::session_roles::load(&runtime.shared_spending_cursor_path(), NOW_SECS);
    let locations = dedup_cached_entry_locations(&files, &cache);
    let counted = indexed_counted_entries(&files, &cache, &locations);
    let result = aggregate_counted_rollups(
        &files,
        &cache,
        &counted,
        None,
        HeadlineContext {
            user_inputs: &[],
            now_secs: NOW_SECS,
            spec: &HeadlineSpec {
                mode: SpendWindowMode::Trailing24h,
                timezone: Some("UTC".into()),
            },
        },
        Some(&roles),
    );
    let published_roles = serde_json::to_value(&result.roles).unwrap();
    assert_eq!(published_roles["recorded_since_secs"], first);
    assert_eq!(
        published_roles["agents"].as_object().map(|rows| rows.len()),
        Some(1)
    );
    assert_eq!(published_roles["teams"].as_object().unwrap().len(), 1);
    assert_eq!(
        published_roles["teams"]["forge"].as_object().unwrap().len(),
        1
    );
    for window in ["today", "day", "week", "month", "year"] {
        let recent = matches!(window, "today" | "day");
        assert_eq!(
            published_roles["teams"]["forge"]["coder"][window]["usd"],
            if recent { 3.0 } else { 9.0 }
        );
        assert_eq!(
            published_roles["teams"]["forge"]["coder"][window]["sessions"],
            if recent { 1 } else { 2 }
        );
        let peer_in_window = matches!(window, "month" | "year");
        assert_eq!(
            published_roles["agents"]["peer-profile"][window]["usd"],
            if peer_in_window { 4.0 } else { 0.0 }
        );
        assert_eq!(
            published_roles["agents"]["peer-profile"][window]["sessions"],
            u32::from(peer_in_window)
        );
    }
    assert_eq!(published_roles["subagents"].as_object().unwrap().len(), 1);
    for window in ["today", "day", "week", "month", "year"] {
        assert_eq!(published_roles["subagents"]["fast"][window]["usd"], 3.0);
        assert_eq!(published_roles["subagents"]["fast"][window]["sessions"], 1);
    }
    let windows: [fn(&SpendTally) -> &SpendWindow; 5] = [
        |t| &t.headline,
        |t| &t.day,
        |t| &t.week,
        |t| &t.month,
        |t| &t.year,
    ];
    for window in windows {
        let mut sum = SpendWindow::default();
        for tally in result
            .roles
            .agents
            .values()
            .chain(result.roles.subagents.values())
        {
            sum.merge(window(&tally.tally));
        }
        for team in published_roles["teams"].as_object().unwrap().values() {
            for role in team.as_object().unwrap().values() {
                let tally: SpendTally = serde_json::from_value(role.clone()).unwrap();
                sum.merge(window(&tally));
            }
        }
        assert!(sum.usd <= window(&result.spending.total).usd);
        assert!(sum.tokens <= window(&result.spending.total).tokens);
        assert!(sum.cache_read <= window(&result.spending.total).cache_read);
    }
    let empty = aggregate_counted_rollups(
        &files,
        &cache,
        &counted,
        None,
        HeadlineContext {
            user_inputs: &[],
            now_secs: NOW_SECS,
            spec: &HeadlineSpec {
                mode: SpendWindowMode::Trailing24h,
                timezone: Some("UTC".into()),
            },
        },
        Some(&Default::default()),
    );
    let empty = serde_json::to_value(&empty.roles).unwrap();
    assert!(empty["agents"].as_object().unwrap().is_empty());
    assert!(empty["subagents"].as_object().unwrap().is_empty());
    assert!(empty["teams"].as_object().unwrap().is_empty());
    assert!(empty["recorded_since_secs"].is_null());
}

fn user_input(second: u64, kind: &str, origin: Option<&Path>) -> user_input::UserInputRecord {
    user_input::UserInputRecord {
        at: Timestamp::from_second(i64::try_from(second).expect("timestamp fits i64"))
            .expect("timestamp"),
        kind: crate::ids::AgentKind::new_unchecked(kind),
        origin: origin.map(Path::to_path_buf),
    }
}

#[test]
fn token_windows_and_native_sessions_populate_public_tallies() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let session = dir.path().join("sess-1");
    std::fs::create_dir_all(session.join("subagents")).unwrap();
    let main = write_jsonl(
        &session,
        "chat.jsonl",
        &[&format!(
            r#"{{"timestamp":"{today}T15:00:00.000Z","costUSD":0.5,"requestId":"req-1","message":{{"id":"msg-1","usage":{{"input_tokens":12000,"output_tokens":64000,"cache_creation_input_tokens":12000,"cache_read_input_tokens":68000}}}}}}"#
        )],
    );
    let sub = write_jsonl(
        &session.join("subagents"),
        "worker.jsonl",
        &[&format!(
            r#"{{"timestamp":"{today}T15:01:00.000Z","costUSD":0.1,"requestId":"req-2","isSidechain":true,"message":{{"id":"msg-2","usage":{{"input_tokens":1000,"output_tokens":500,"cache_read_input_tokens":2000}}}}}}"#
        )],
    );
    let total = compute_total(&[main, sub], &mut SpendingDiskCache::default());
    assert_eq!(
        (
            total.headline.input,
            total.headline.output,
            total.headline.tokens,
            total.headline.cache_write,
            total.headline.cache_read,
            total.headline.sessions,
        ),
        (25_000, 64_500, 89_500, 12_000, 70_000, 1)
    );

    const HOUR: u64 = 3_600;
    const DAY: u64 = 86_400;
    let aged = write_jsonl(
        dir.path(),
        "aged.jsonl",
        &[
            &claude_line_ago(2 * HOUR, 1.0, "msg-1", "req-1"),
            &claude_line_ago(3 * DAY, 0.5, "msg-2", "req-2"),
            &claude_line_ago(20 * DAY, 0.25, "msg-3", "req-3"),
            &claude_line_ago(100 * DAY, 0.1, "msg-4", "req-4"),
            &claude_line_ago(400 * DAY, 9.0, "msg-5", "req-5"),
        ],
    );
    let aged = compute_total(&[aged], &mut SpendingDiskCache::default());
    assert_eq!(
        (
            aged.headline.tokens,
            aged.week.tokens,
            aged.month.tokens,
            aged.year.tokens,
        ),
        (15, 30, 45, 60)
    );
    assert!((aged.headline.usd - 1.0).abs() < 1e-9);
    assert!((aged.week.usd - 1.5).abs() < 1e-9);
    assert!((aged.month.usd - 1.75).abs() < 1e-9);
    assert!((aged.year.usd - 1.85).abs() < 1e-9);

    let native_file = PathBuf::from("/x/opencode.db");
    let cache = SpendingDiskCache {
        files: HashMap::from([cached_file(
            &native_file,
            vec![
                cached_entry(NOW_SECS, 0.01, "session-a"),
                cached_entry(NOW_SECS - 2 * DAY, 0.01, "session-b"),
                cached_entry(NOW_SECS - 3 * DAY, 0.01, "session-b"),
            ],
        )]),
        ..Default::default()
    };
    let files: Vec<SpendingFile> = vec![spending_file(opencode_adapter(), native_file)];
    let counted = dedup_cached_entries(&files, &cache).into_counted();
    let spending = aggregate_spending(&files, &cache, &counted, NOW_SECS, &HeadlineSpec::default());
    assert_eq!(spending.total.headline.sessions, 1);
    assert_eq!(spending.total.week.sessions, 2);
    assert_eq!(spending.by_provider["opencode"].week.sessions, 2);
}

#[test]
fn tool_calls_populate_each_trailing_window_and_provider_tally() {
    let file = PathBuf::from("/x/opencode.db");
    let mut entry = cached_entry(NOW_SECS - 8 * 86_400, 0.01, "session-a");
    entry.tool_calls = BTreeMap::from([("bash".to_owned(), 2), ("read".to_owned(), 3)]);
    let cache = SpendingDiskCache {
        files: HashMap::from([cached_file(&file, vec![entry])]),
        ..Default::default()
    };
    let files = vec![spending_file(opencode_adapter(), file)];
    let counted = dedup_cached_entries(&files, &cache).into_counted();

    let spending = aggregate_spending(&files, &cache, &counted, NOW_SECS, &HeadlineSpec::default());

    assert_eq!(spending.total.week.tool_calls, 0);
    assert_eq!(spending.total.month.tool_calls, 5);
    assert_eq!(spending.total.year.tools["read"], 3);
    assert_eq!(spending.by_provider["opencode"].year.tools["bash"], 2);
}

#[test]
fn headline_cutoffs_are_global_and_scoped() {
    const HOUR: u64 = 3_600;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let other = dir.path().join("other");
    let claude_file = write_jsonl(dir.path(), "claude.jsonl", &[]);
    let codex_file = write_codex(dir.path(), &[]);
    let day_start = (NOW_SECS / 86_400) * 86_400;
    let files: Vec<SpendingFile> = vec![
        spending_file(claude_adapter(), claude_file.clone()),
        spending_file(codex_adapter(), codex_file.clone()),
    ];

    let today_cache = SpendingDiskCache {
        files: HashMap::from([
            cached_file_with_origin(
                &claude_file,
                &project,
                vec![
                    cached_entry(day_start - 1, 1.0, "before-midnight"),
                    cached_entry(day_start + 60, 2.0, "after-midnight"),
                ],
            ),
            cached_file_with_origin(
                &codex_file,
                &other,
                vec![cached_entry(day_start + 60, 4.0, "outside")],
            ),
        ]),
        ..Default::default()
    };
    let today_spec = HeadlineSpec {
        mode: SpendWindowMode::Today,
        timezone: Some("UTC".to_owned()),
    };
    let counted = dedup_cached_entries(&files, &today_cache).into_counted();
    let global = aggregate_spending(&files, &today_cache, &counted, NOW_SECS, &today_spec);
    let scoped = compute_scoped_tally(
        &files,
        &today_cache,
        &SpendScope::from_roots(Some(&project), &[]),
        NOW_SECS,
        &today_spec,
    );
    assert!((global.total.headline.usd - 6.0).abs() < 1e-9);
    assert!((scoped.headline.usd - 2.0).abs() < 1e-9);
    assert!((scoped.week.usd - 3.0).abs() < 1e-9);

    let mut session_cache = SpendingDiskCache {
        files: HashMap::from([
            cached_file(
                &claude_file,
                vec![
                    cached_entry(NOW_SECS - 8 * HOUR, 3.0, "claude"),
                    cached_entry(NOW_SECS, 1.0, "claude"),
                ],
            ),
            cached_file(
                &codex_file,
                vec![cached_entry(NOW_SECS - 4 * HOUR, 2.0, "codex")],
            ),
        ]),
        ..Default::default()
    };
    let session_spec = HeadlineSpec {
        mode: SpendWindowMode::Session,
        timezone: None,
    };
    let (spending, _) = compute_spending_with_origins_and_scope(
        &files,
        &mut session_cache,
        &PriceBook::default(),
        NOW_SECS,
        &HashMap::new(),
        None,
        &session_spec,
    );
    assert!((spending.total.headline.usd - 6.0).abs() < 1e-9);
    assert!((spending.by_provider["claude"].headline.usd - 4.0).abs() < 1e-9);
    assert!((spending.by_provider["codex"].headline.usd - 2.0).abs() < 1e-9);

    let idle = SpendingDiskCache {
        files: HashMap::from([cached_file_with_origin(
            &claude_file,
            &project,
            vec![
                cached_entry(NOW_SECS - 8 * HOUR, 2.0, "idle"),
                cached_entry(NOW_SECS - 7 * HOUR, 3.0, "idle"),
            ],
        )]),
        ..Default::default()
    };
    let scoped = compute_scoped_tally(
        &files,
        &idle,
        &SpendScope::from_roots(Some(&project), &[]),
        NOW_SECS,
        &session_spec,
    );
    assert_eq!(scoped.headline.usd, 0.0);
    assert!((scoped.week.usd - 5.0).abs() < 1e-9);
}

#[test]
fn local_day_rollups_ignore_headline_mode_and_publish_live_baselines() {
    let now: Timestamp = "2025-06-01T04:30:00Z".parse().expect("now");
    let before: Timestamp = "2025-06-01T03:59:00Z".parse().expect("before midnight");
    let after: Timestamp = "2025-06-01T04:01:00Z".parse().expect("after midnight");
    let project = PathBuf::from("/repo/project");
    let file = PathBuf::from("/tmp/rimz/day.jsonl");
    let named_file = PathBuf::from("/tmp/rimz/work/day.jsonl");
    let named_login: LoginKey = "claude@work".parse().unwrap();
    let files = vec![
        spending_file(claude_adapter(), file.clone()),
        SpendingFile {
            adapter: claude_adapter(),
            login: named_login.clone(),
            path: named_file.clone(),
        },
    ];
    let mut cache = SpendingDiskCache {
        files: HashMap::from([cached_file_with_origin(
            &file,
            &project,
            vec![
                cached_entry(before.as_second() as u64, 1.0, "before"),
                cached_entry(after.as_second() as u64, 2.0, "live"),
            ],
        )]),
        ..Default::default()
    };
    let (key, value) = cached_file_with_origin(
        &named_file,
        Path::new("/elsewhere"),
        vec![
            cached_entry(before.as_second() as u64, 8.0, "work-before"),
            cached_entry(after.as_second() as u64, 4.0, "work-live"),
        ],
    );
    cache.files.insert(key, value);
    let locations = dedup_cached_entry_locations(&files, &cache);
    let counted = indexed_counted_entries(&files, &cache, &locations);
    let user_inputs = user_inputs_from_counted(&counted);
    let scope = SpendScope::from_roots(Some(&project), &[]);
    let spec = HeadlineSpec {
        mode: SpendWindowMode::Session,
        timezone: Some("America/New_York".to_owned()),
    };
    let rollups = aggregate_counted_rollups(
        &files,
        &cache,
        &counted,
        Some(&scope),
        HeadlineContext {
            user_inputs: &user_inputs,
            now_secs: now.as_second() as u64,
            spec: &spec,
        },
        None,
    );

    assert_eq!(
        rollups.day_cutoff_secs,
        "2025-06-01T04:00:00Z"
            .parse::<Timestamp>()
            .expect("cutoff")
            .as_second() as u64
    );
    assert_eq!(rollups.login_day.len(), 2);
    assert_eq!(rollups.login_day[&named_login].usd, 4.0);
    assert_eq!(rollups.login_day[&named_login].sessions, 1);
    assert_eq!(
        rollups.login_day[&LoginKey::default_for(crate::ids::AgentKind::new_unchecked("claude"))]
            .usd,
        2.0
    );
    assert_eq!(
        rollups.login_day.values().map(|day| day.usd).sum::<f64>(),
        rollups.provider_day["claude"].usd
    );
    let published = ProviderSpendingCache::from_walk(&rollups, 1);
    assert_eq!(published.day_by_login, rollups.login_day);
    assert_eq!(published.spending.by_login[&named_login].week.usd, 12.0);
    assert_eq!(published.spending.by_login[&named_login].week.sessions, 2);
    assert_eq!(rollups.workspace.day.usd, 2.0);
    assert_eq!(rollups.workspace.day.tokens, 15);
    assert!((rollups.workspace.tally.headline.usd - 3.0).abs() < 1e-9);
    assert_eq!(
        rollups.workspace.live_baselines,
        BTreeMap::from([
            ("claude:before".to_owned(), 1.0),
            ("claude:live".to_owned(), 2.0),
        ])
    );
}

#[test]
fn scoped_spending_publishes_full_walked_session_baselines() {
    let project = PathBuf::from("/repo/project");
    let file = PathBuf::from("/tmp/rimz/live.jsonl");
    let files: Vec<SpendingFile> = vec![spending_file(claude_adapter(), file.clone())];
    let cache = SpendingDiskCache {
        files: HashMap::from([cached_file_with_origin(
            &file,
            &project,
            vec![cached_entry(NOW_SECS, 1.25, "live")],
        )]),
        ..Default::default()
    };
    let scope = SpendScope::from_roots(Some(&project), &[]);
    let scoped = compute_scoped_spending(
        &files,
        &cache,
        &user_inputs_from_cache(&files, &cache),
        &scope,
        NOW_SECS,
        &HeadlineSpec::default(),
    );
    assert_eq!(scoped.tally.headline.usd, 1.25);
    assert_eq!(scoped.tally.headline.tokens, 15);
    assert_eq!(scoped.tally.headline.sessions, 1);
    assert!((scoped.tally.week.usd - 1.25).abs() < 1e-9);
    assert_eq!(
        scoped.live_baselines,
        BTreeMap::from([("claude:live".to_owned(), 1.25)])
    );

    let session = PathBuf::from("/tmp/claude/sess-1");
    let main = session.join("chat.jsonl");
    let sub = session.join("subagents/worker.jsonl");
    let mut main_entry = cached_entry(NOW_SECS, 0.50, "");
    main_entry.thread_id = None;
    let mut sub_entry = cached_entry(NOW_SECS, 0.10, "");
    sub_entry.thread_id = None;
    let files: Vec<SpendingFile> = vec![
        spending_file(claude_adapter(), main.clone()),
        spending_file(claude_adapter(), sub.clone()),
    ];
    let cache = SpendingDiskCache {
        files: HashMap::from([
            cached_file_with_origin(&main, &project, vec![main_entry]),
            cached_file_with_origin(&sub, &project, vec![sub_entry]),
        ]),
        ..Default::default()
    };
    let scoped = compute_scoped_spending(
        &files,
        &cache,
        &user_inputs_from_cache(&files, &cache),
        &scope,
        NOW_SECS,
        &HeadlineSpec::default(),
    );
    assert_eq!(scoped.tally.headline.usd, 0.60);
    assert_eq!(scoped.tally.headline.tokens, 30);
    assert_eq!(scoped.tally.headline.sessions, 1);
    assert!((scoped.tally.week.usd - 0.60).abs() < 1e-9);
    assert_eq!(
        scoped.live_baselines,
        BTreeMap::from([(session.to_string_lossy().into_owned(), 0.60)])
    );

    let codex_file = PathBuf::from("/tmp/codex/rollout.jsonl");
    let files: Vec<SpendingFile> = vec![spending_file(codex_adapter(), codex_file.clone())];
    let cache = SpendingDiskCache {
        files: HashMap::from([cached_file_with_origin(
            &codex_file,
            &project,
            vec![
                cached_entry(NOW_SECS, 2.00, "live"),
                cached_entry(NOW_SECS, 3.00, "sibling"),
            ],
        )]),
        ..Default::default()
    };
    let scoped = compute_scoped_spending(
        &files,
        &cache,
        &user_inputs_from_cache(&files, &cache),
        &scope,
        NOW_SECS,
        &HeadlineSpec::default(),
    );
    assert!((scoped.tally.headline.usd - 5.00).abs() < 1e-9);
    assert_eq!(scoped.tally.headline.tokens, 30);
    assert!((scoped.tally.week.usd - 5.00).abs() < 1e-9);
    assert_eq!(
        scoped.live_baselines,
        BTreeMap::from([
            ("codex:live".to_owned(), 2.0),
            ("codex:sibling".to_owned(), 3.0),
        ])
    );
}

#[test]
fn live_baselines_sum_year_entries_only_for_recent_sessions() {
    const DAY: u64 = 86_400;
    let project = PathBuf::from("/repo/project");
    let file = PathBuf::from("/tmp/rimz/baselines.jsonl");
    let files: Vec<SpendingFile> = vec![spending_file(claude_adapter(), file.clone())];
    let cache = SpendingDiskCache {
        files: HashMap::from([cached_file_with_origin(
            &file,
            &project,
            vec![
                cached_entry(NOW_SECS, 1.0, "recent"),
                cached_entry(NOW_SECS - 10 * DAY, 3.0, "recent"),
                cached_entry(NOW_SECS - RAW_RETAIN_SECS, 5.0, "stale"),
                cached_entry(NOW_SECS - 20 * DAY, 7.0, "stale"),
                cached_entry(NOW_SECS - WIDEST_SPEND_WINDOW_SECS - DAY, 11.0, "recent"),
            ],
        )]),
        ..Default::default()
    };
    let scope = SpendScope::from_roots(Some(&project), &[]);
    let scoped = compute_scoped_spending(
        &files,
        &cache,
        &[user_input(NOW_SECS, "claude", Some(&project))],
        &scope,
        NOW_SECS,
        &HeadlineSpec::default(),
    );

    assert_eq!(
        scoped.live_baselines,
        BTreeMap::from([("claude:recent".to_owned(), 4.0)])
    );
}

#[test]
fn priced_entries_bridge_but_never_open_a_session() {
    const HOUR: u64 = 3_600;
    let first_file = PathBuf::from("/tmp/rimz/first.jsonl");
    let chatter_file = PathBuf::from("/tmp/rimz/chatter.jsonl");
    let second_file = PathBuf::from("/tmp/rimz/second.jsonl");
    let files: Vec<SpendingFile> = vec![
        spending_file(claude_adapter(), first_file.clone()),
        spending_file(claude_adapter(), chatter_file.clone()),
        spending_file(claude_adapter(), second_file.clone()),
    ];
    let cache = SpendingDiskCache {
        files: HashMap::from([
            cached_file(
                &first_file,
                vec![cached_entry(NOW_SECS - 9 * HOUR, 1.0, "session")],
            ),
            cached_file(
                &chatter_file,
                vec![cached_entry(NOW_SECS - 4 * HOUR - HOUR / 2, 0.5, "session")],
            ),
            cached_file(&second_file, vec![cached_entry(NOW_SECS, 2.0, "session")]),
        ]),
        ..Default::default()
    };
    let spec = HeadlineSpec {
        mode: SpendWindowMode::Session,
        timezone: None,
    };
    let counted = dedup_cached_entries(&files, &cache).into_counted();
    let empty = aggregate_spending_with_user_inputs(&files, &cache, &counted, &[], NOW_SECS, &spec);
    let separated_prompts = [
        user_input(NOW_SECS - 9 * HOUR, "claude", None),
        user_input(NOW_SECS, "claude", None),
    ];
    let bounded = aggregate_spending_with_user_inputs(
        &files,
        &cache,
        &counted,
        &separated_prompts,
        NOW_SECS,
        &spec,
    );

    assert_eq!(empty.total.headline, SpendWindow::default());
    assert!((bounded.total.headline.usd - 3.5).abs() < 1e-9);
    assert_eq!(bounded.total.headline.tokens, 45);
    assert!((bounded.total.week.usd - 3.5).abs() < 1e-9);
}

#[test]
fn session_burst_resets_at_an_exact_activity_gap() {
    const HOUR: u64 = 3_600;
    let file = PathBuf::from("/tmp/rimz/activity-gap.jsonl");
    let files: Vec<SpendingFile> = vec![spending_file(claude_adapter(), file.clone())];
    let cache = SpendingDiskCache {
        files: HashMap::from([cached_file(
            &file,
            vec![
                cached_entry(NOW_SECS - 10 * HOUR, 1.0, "old"),
                cached_entry(NOW_SECS - 5 * HOUR, 2.0, "bridge"),
                cached_entry(NOW_SECS, 3.0, "current"),
            ],
        )]),
        ..Default::default()
    };
    let counted = dedup_cached_entries(&files, &cache).into_counted();
    let prompts = [
        user_input(NOW_SECS - 10 * HOUR, "claude", None),
        user_input(NOW_SECS, "claude", None),
    ];
    let spending = aggregate_spending_with_user_inputs(
        &files,
        &cache,
        &counted,
        &prompts,
        NOW_SECS,
        &HeadlineSpec::default(),
    );

    assert_eq!(spending.total.headline.usd, 3.0);
}

#[test]
fn bridged_session_cutoff_stays_stable_while_activity_is_current() {
    const HOUR: u64 = 3_600;
    let project = PathBuf::from("/repo/project");
    let file = PathBuf::from("/tmp/rimz/stable-cutoff.jsonl");
    let files: Vec<SpendingFile> = vec![spending_file(claude_adapter(), file.clone())];
    let cache = SpendingDiskCache {
        files: HashMap::from([cached_file_with_origin(
            &file,
            &project,
            vec![
                cached_entry(NOW_SECS - 8 * HOUR, 1.0, "opened"),
                cached_entry(NOW_SECS - 4 * HOUR, 2.0, "bridge"),
                cached_entry(NOW_SECS, 3.0, "current"),
            ],
        )]),
        ..Default::default()
    };
    let scope = SpendScope::from_roots(Some(&project), &[]);
    let prompts = [user_input(NOW_SECS - 8 * HOUR, "claude", Some(&project))];
    let first = compute_scoped_spending(
        &files,
        &cache,
        &prompts,
        &scope,
        NOW_SECS,
        &HeadlineSpec::default(),
    );
    let later = compute_scoped_spending(
        &files,
        &cache,
        &prompts,
        &scope,
        NOW_SECS + HOUR,
        &HeadlineSpec::default(),
    );

    assert_eq!(first.headline_cutoff_secs, NOW_SECS - 8 * HOUR);
    assert_eq!(later.headline_cutoff_secs, first.headline_cutoff_secs);
}

#[test]
fn autonomous_spend_inside_user_prompt_burst_counts() {
    const HOUR: u64 = 3_600;
    let first_file = PathBuf::from("/tmp/rimz/burst-first.jsonl");
    let automation_file = PathBuf::from("/tmp/rimz/burst-automation.jsonl");
    let second_file = PathBuf::from("/tmp/rimz/burst-second.jsonl");
    let files: Vec<SpendingFile> = vec![
        spending_file(claude_adapter(), first_file.clone()),
        spending_file(claude_adapter(), automation_file.clone()),
        spending_file(claude_adapter(), second_file.clone()),
    ];
    let cache = SpendingDiskCache {
        files: HashMap::from([
            cached_file(
                &first_file,
                vec![cached_entry(NOW_SECS - HOUR, 1.0, "human-a")],
            ),
            cached_file(
                &automation_file,
                vec![cached_entry(NOW_SECS - HOUR / 2, 0.5, "automation")],
            ),
            cached_file(&second_file, vec![cached_entry(NOW_SECS, 2.0, "human-b")]),
        ]),
        ..Default::default()
    };
    let spec = HeadlineSpec {
        mode: SpendWindowMode::Session,
        timezone: None,
    };
    let counted = dedup_cached_entries(&files, &cache).into_counted();
    let prompts = [
        user_input(NOW_SECS - HOUR, "claude", None),
        user_input(NOW_SECS, "claude", None),
    ];
    let spending =
        aggregate_spending_with_user_inputs(&files, &cache, &counted, &prompts, NOW_SECS, &spec);

    assert!((spending.total.headline.usd - 3.5).abs() < 1e-9);
    assert_eq!(spending.total.headline.tokens, 45);
    assert_eq!(spending.total.headline.sessions, 3);
}

#[test]
fn provider_headlines_share_the_machine_global_burst() {
    const HOUR: u64 = 3_600;
    let claude_file = PathBuf::from("/tmp/rimz/provider-claude.jsonl");
    let codex_file = PathBuf::from("/tmp/rimz/provider-codex.jsonl");
    let files: Vec<SpendingFile> = vec![
        spending_file(claude_adapter(), claude_file.clone()),
        spending_file(codex_adapter(), codex_file.clone()),
    ];
    let cache = SpendingDiskCache {
        files: HashMap::from([
            cached_file(&claude_file, vec![cached_entry(NOW_SECS, 1.0, "claude")]),
            cached_file(
                &codex_file,
                vec![
                    cached_entry(NOW_SECS - 6 * HOUR, 4.0, "stale-codex"),
                    cached_entry(NOW_SECS, 2.0, "current-codex"),
                ],
            ),
        ]),
        ..Default::default()
    };
    let counted = dedup_cached_entries(&files, &cache).into_counted();
    let prompts = [
        user_input(NOW_SECS, "claude", None),
        user_input(NOW_SECS - 6 * HOUR, "codex", None),
    ];
    let spec = HeadlineSpec {
        mode: SpendWindowMode::Session,
        timezone: None,
    };

    let spending =
        aggregate_spending_with_user_inputs(&files, &cache, &counted, &prompts, NOW_SECS, &spec);

    assert!((spending.total.headline.usd - 3.0).abs() < 1e-9);
    assert!((spending.by_provider["claude"].headline.usd - 1.0).abs() < 1e-9);
    assert!((spending.by_provider["codex"].headline.usd - 2.0).abs() < 1e-9);
    assert_eq!(spending.by_provider["codex"].headline.sessions, 1);
}

#[test]
fn workspace_session_cutoff_uses_only_scoped_user_inputs() {
    let project = PathBuf::from("/repo/project");
    let other = PathBuf::from("/repo/other");
    let project_file = PathBuf::from("/tmp/rimz/scoped-project.jsonl");
    let other_file = PathBuf::from("/tmp/rimz/scoped-other.jsonl");
    let files: Vec<SpendingFile> = vec![
        spending_file(claude_adapter(), project_file.clone()),
        spending_file(claude_adapter(), other_file.clone()),
    ];
    let cache = SpendingDiskCache {
        files: HashMap::from([
            cached_file_with_origin(
                &project_file,
                &project,
                vec![cached_entry(NOW_SECS, 1.0, "project")],
            ),
            cached_file_with_origin(
                &other_file,
                &other,
                vec![cached_entry(NOW_SECS, 2.0, "other")],
            ),
        ]),
        ..Default::default()
    };
    let scope = SpendScope::from_roots(Some(&project), &[]);
    let spec = HeadlineSpec {
        mode: SpendWindowMode::Session,
        timezone: None,
    };
    let project_prompt = [user_input(NOW_SECS, "claude", Some(&project))];
    let outside_prompt = [user_input(NOW_SECS, "claude", Some(&other))];

    let included =
        compute_scoped_spending(&files, &cache, &project_prompt, &scope, NOW_SECS, &spec);
    let empty = compute_scoped_spending(&files, &cache, &outside_prompt, &scope, NOW_SECS, &spec);

    assert!((included.tally.headline.usd - 1.0).abs() < 1e-9);
    assert_eq!(empty.tally.headline, SpendWindow::default());
    assert_eq!(empty.headline_cutoff_secs, NO_BURST_CUTOFF);
}

#[test]
fn workspace_scope_uses_roots_worktree_home_and_file_origin() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let ts = format!("{today}T15:00:00.000Z");
    let project = dir.path().join("repo");
    let linked = dir.path().join("linked-worktree");
    let home = dir.path().join("repo-worktrees");
    let removed = home.join("budget-reset");
    let other = dir.path().join("other-project");
    let mut files = Vec::new();

    for (name, cwd, usd) in [
        ("project", project.join("src"), 1.0),
        ("linked", linked.clone(), 2.0),
        ("removed", removed.join("crates/rimz"), 3.0),
        ("other", other.clone(), 4.0),
    ] {
        let session = dir.path().join(format!("sessions/{name}"));
        std::fs::create_dir_all(&session).unwrap();
        files.push(spending_file(
            claude_adapter(),
            write_jsonl(
                &session,
                "chat.jsonl",
                &[&claude_line_ts_in(
                    &ts,
                    usd,
                    &format!("msg-{name}"),
                    &format!("req-{name}"),
                    &cwd,
                )],
            ),
        ));
    }

    let origin_file = write_jsonl(
        dir.path(),
        "origin.jsonl",
        &[
            &format!(
                r#"{{"timestamp":"{today}T15:00:00.000Z","cwd":null,"costUSD":0.5,"requestId":"req-origin-a","message":{{"id":"msg-origin-a","usage":{{"input_tokens":10,"output_tokens":5}}}}}}"#
            ),
            &claude_line_ts_in(
                &format!("{today}T15:01:00.000Z"),
                0.25,
                "msg-origin-b",
                "req-origin-b",
                &project,
            ),
        ],
    );
    files.push(spending_file(claude_adapter(), origin_file.clone()));

    let mut cache = SpendingDiskCache::default();
    let scope =
        SpendScope::for_workspace(Some(&project), std::slice::from_ref(&linked), Some(&home));
    let (global, scoped) = compute_spending_with_origins_and_scope(
        &files,
        &mut cache,
        &PriceBook::default(),
        NOW_SECS,
        &HashMap::new(),
        Some(&scope),
        &HeadlineSpec::default(),
    );
    assert!((global.total.headline.usd - 10.75).abs() < 1e-9);
    assert!((scoped.headline.usd - 6.75).abs() < 1e-9);
    assert_eq!(scoped.headline.sessions, 4);
    assert_eq!(
        cache.files[&origin_file.to_string_lossy().into_owned()]
            .origin_path
            .as_deref(),
        Some(project.as_path())
    );

    let live_only = SpendScope::from_roots(Some(&project), std::slice::from_ref(&linked));
    let scoped = compute_scoped_tally(
        &files,
        &cache,
        &live_only,
        NOW_SECS,
        &HeadlineSpec::default(),
    );
    assert!((scoped.headline.usd - 3.75).abs() < 1e-9);
}

#[test]
fn workspace_scope_normalizes_parent_components_on_contains() {
    let scope = SpendScope::from_roots(Some(Path::new("/workspace/project")), &[]);

    assert!(scope.contains(Path::new("/workspace/project/src")));
    assert!(!scope.contains(Path::new("/workspace/project/../other")));
    assert!(!scope.contains(Path::new("workspace/project")));
}

#[test]
fn claude_replay_dedup_collapses_before_store_and_rollups() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let main_line = claude_line(&today, 1.0, "msg-a", "req-a");
    let replay_line = claude_sidechain_line(&today, 9.0, "msg-a", "req-a");
    let mixed = write_jsonl(dir.path(), "mixed.jsonl", &[&replay_line, &main_line]);
    let duplicate = write_jsonl(dir.path(), "duplicate.jsonl", &[&main_line]);
    let lone_sidechain = write_jsonl(
        dir.path(),
        "sidechain-only.jsonl",
        &[&claude_sidechain_line(&today, 0.20, "msg-x", "req-x")],
    );

    let mut cache = SpendingDiskCache::default();
    let total = compute_total(&[mixed.clone(), duplicate], &mut cache);
    assert!((total.headline.usd - 1.0).abs() < 1e-9);
    assert_eq!(total.headline.tokens, 15);
    let stored = &cache.files[&mixed.to_string_lossy().into_owned()].entries;
    assert_eq!(stored.len(), 1);
    assert!(!stored[0].is_sidechain);
    assert_eq!(stored[0].input, 10);

    let kept = compute_total(&[lone_sidechain], &mut SpendingDiskCache::default());
    assert!((kept.headline.usd - 0.20).abs() < 1e-9);
    assert_eq!(kept.headline.tokens, 50_005);
}

#[test]
fn claude_duplicate_dedup_keeps_the_richest_main_thread_record() {
    let dir = TempDir::new().unwrap();
    let paths = [
        "sidechain.jsonl",
        "small.jsonl",
        "large.jsonl",
        "fast.jsonl",
    ]
    .map(|name| write_jsonl(dir.path(), name, &[]));
    let base = CachedEntry {
        ts_secs: NOW_SECS,
        cost_usd: 9.0,
        input: 9_000,
        output: 0,
        cache_write: 0,
        cache_read: 0,
        tool_calls: Default::default(),
        message_id: Some("msg-rich".to_owned()),
        request_id: Some("req-rich".to_owned()),
        dedup_key: None,
        thread_id: None,
        is_sidechain: true,
        has_speed: false,
        fast: false,
        model: Some("claude-opus-4-8".to_owned()),
        rolled: false,
        timing: None,
    };
    let small = CachedEntry {
        cost_usd: 1.0,
        input: 10,
        is_sidechain: false,
        ..base.clone()
    };
    let large = CachedEntry {
        cost_usd: 2.0,
        input: 20,
        ..small.clone()
    };
    let fast = CachedEntry {
        cost_usd: 4.0,
        has_speed: true,
        ..large.clone()
    };
    let cache = SpendingDiskCache {
        files: HashMap::from([
            cached_file(&paths[0], vec![base]),
            cached_file(&paths[1], vec![small]),
            cached_file(&paths[2], vec![large]),
            cached_file(&paths[3], vec![fast]),
        ]),
        ..Default::default()
    };
    let files = paths
        .into_iter()
        .map(|path| spending_file(claude_adapter(), path))
        .collect::<Vec<_>>();

    let counted = dedup_cached_entries(&files, &cache).into_counted();
    assert_eq!(counted.len(), 1);
    let spending = aggregate_spending(&files, &cache, &counted, NOW_SECS, &HeadlineSpec::default());
    assert_eq!(spending.total.headline.input, 20);
    assert_eq!(spending.total.headline.usd, 4.0);
}

#[test]
fn codex_cross_file_dedup_uses_the_exact_native_event_fingerprint() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let duplicate = codex_token_line(&today, 1_000, 400, 500);
    let distinct_millisecond = duplicate.replace("15:00:00.000Z", "15:00:00.001Z");
    let model = r#"{"type":"turn_context","payload":{"model":"gpt-4o"}}"#;
    let files = [
        write_codex(&dir.path().join("one"), &[model, &duplicate]),
        write_codex(&dir.path().join("two"), &[model, &duplicate]),
        write_codex(&dir.path().join("three"), &[model, &distinct_millisecond]),
    ];
    let tagged = files
        .into_iter()
        .map(|path| spending_file(codex_adapter(), path))
        .collect::<Vec<_>>();

    let spending = compute_spending(
        &tagged,
        &mut SpendingDiskCache::default(),
        &gpt4o_book(),
        NOW_SECS,
    );
    assert_eq!(spending.total.headline.input, 1_200);
    assert_eq!(spending.total.headline.cache_read, 800);
    assert_eq!(spending.total.headline.output, 1_000);
    assert_eq!(
        spending.total.headline.sessions, 3,
        "event dedup preserves the fact that each session ran"
    );
    assert!((spending.total.headline.usd - 0.003_28).abs() < 1e-9);
}

#[test]
fn codex_resume_pricing_provider_origin_and_unknown_heal_stay_intact() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let resumable = write_codex(
        dir.path(),
        &[
            r#"{"type":"turn_context","payload":{"model":"gpt-4o"}}"#,
            &codex_total_line(&today, 1000, 500),
        ],
    );
    let mut cache = SpendingDiskCache::default();
    let first = compute_spending(
        &[spending_file(codex_adapter(), resumable.clone())],
        &mut cache,
        &gpt4o_book(),
        NOW_SECS,
    );
    append_line(&resumable, &codex_total_line(&today, 1600, 800));
    let second = compute_spending(
        &[spending_file(codex_adapter(), resumable)],
        &mut cache,
        &gpt4o_book(),
        NOW_SECS,
    );
    assert_eq!(
        (first.total.headline.input, first.total.headline.output),
        (1000, 500)
    );
    assert_eq!(
        (second.total.headline.input, second.total.headline.output),
        (1600, 800)
    );

    let project = dir.path().join("repo");
    let claude_file = write_jsonl(
        dir.path(),
        "claude.jsonl",
        &[&claude_line(&today, 0.5, "msg-1", "req-1")],
    );
    let codex_file = write_codex(
        dir.path(),
        &[
            r#"{"type":"turn_context","payload":{"model":"gpt-4o"}}"#,
            &codex_token_line(&today, 1000, 400, 500),
        ],
    );
    let files = vec![spending_file(codex_adapter(), codex_file.clone())];
    let scope = SpendScope::from_roots(Some(&project), &[]);
    let mut cache = SpendingDiskCache::default();
    let unpriced = PriceBook::from_litellm_json("{}");
    let first =
        compute_spending_with_origins(&files, &mut cache, &unpriced, NOW_SECS, &HashMap::new());
    assert_eq!(first.total.headline.usd, 0.0);
    assert_eq!(
        (
            first.total.headline.input,
            first.total.headline.output,
            first.total.headline.cache_read,
            first.total.headline.tokens,
        ),
        (600, 500, 400, 1_100)
    );
    assert!(
        compute_scoped_tally(&files, &cache, &scope, NOW_SECS, &HeadlineSpec::default()).is_zero()
    );

    let _ = compute_spending_with_origins(
        &files,
        &mut cache,
        &unpriced,
        NOW_SECS,
        &HashMap::from([(codex_file.clone(), project.clone())]),
    );
    let cache_key = codex_file.to_string_lossy().into_owned();
    cache.dirty = false;
    let healed =
        compute_spending_with_origins(&files, &mut cache, &gpt4o_book(), NOW_SECS, &HashMap::new());
    let scoped = compute_scoped_tally(&files, &cache, &scope, NOW_SECS, &HeadlineSpec::default());
    assert!((healed.total.headline.usd - 0.00164).abs() < 1e-9);
    assert!(cache.files[&cache_key].unknown_models.is_empty());
    assert!(cache.dirty);
    assert!((scoped.headline.usd - 0.00164).abs() < 1e-9);
    assert_eq!(
        cache.files[&cache_key].origin_path.as_deref(),
        Some(project.as_path())
    );

    let spending = compute_spending(
        &[
            spending_file(claude_adapter(), claude_file),
            spending_file(codex_adapter(), codex_file),
        ],
        &mut SpendingDiskCache::default(),
        &gpt4o_book(),
        NOW_SECS,
    );
    let codex = &spending.by_provider["codex"];
    assert!((codex.headline.usd - 0.00164).abs() < 1e-9);
    assert_eq!(
        (
            codex.headline.input,
            codex.headline.output,
            codex.headline.cache_read
        ),
        (600, 500, 400)
    );
    assert!((spending.by_provider["claude"].headline.usd - 0.5).abs() < 1e-9);
    assert!((spending.total.headline.usd - 0.50164).abs() < 1e-9);
}

#[test]
fn daily_and_model_rollups_share_the_dedup_pass() {
    let main = CachedEntry {
        ts_secs: NOW_SECS,
        cost_usd: 1.0,
        input: 100,
        output: 50,
        cache_write: 10,
        cache_read: 70,
        tool_calls: BTreeMap::from([("Read".to_owned(), 2)]),
        message_id: Some("msg-1".to_owned()),
        request_id: Some("req-1".to_owned()),
        dedup_key: None,
        thread_id: None,
        is_sidechain: false,
        has_speed: false,
        fast: false,
        model: Some("claude-opus-4-8".to_owned()),
        rolled: false,
        timing: None,
    };
    let replay = CachedEntry {
        is_sidechain: true,
        cost_usd: 9.0,
        input: 9_000,
        cache_read: 900,
        ..main.clone()
    };
    let codex = CachedEntry {
        ts_secs: NOW_SECS - 2 * 86_400,
        cost_usd: 2.0,
        input: 200,
        output: 20,
        cache_write: 0,
        cache_read: 40,
        tool_calls: BTreeMap::from([("Bash".to_owned(), 1)]),
        message_id: None,
        request_id: None,
        dedup_key: None,
        thread_id: None,
        is_sidechain: false,
        has_speed: false,
        fast: false,
        model: Some("gpt-5-codex".to_owned()),
        rolled: false,
        timing: None,
    };
    let old = CachedEntry {
        ts_secs: NOW_SECS - 40 * 86_400,
        cost_usd: 3.0,
        input: 300,
        output: 30,
        cache_read: 60,
        model: Some("gpt-5-old".to_owned()),
        ..codex.clone()
    };
    let claude_file = PathBuf::from("/x/claude.jsonl");
    let codex_file = PathBuf::from("/x/codex.jsonl");
    let cache = SpendingDiskCache {
        files: HashMap::from([
            cached_file(&claude_file, vec![main, replay]),
            cached_file(&codex_file, vec![codex, old]),
        ]),
        ..Default::default()
    };
    let files: Vec<SpendingFile> = vec![
        spending_file(claude_adapter(), claude_file),
        spending_file(codex_adapter(), codex_file),
    ];

    let daily = compute_daily_spend(&files, &cache);
    assert_eq!(daily[&((NOW_SECS / 86_400) as i64)].tokens, 230);
    assert_eq!(
        daily[&(((NOW_SECS - 2 * 86_400) / 86_400) as i64)].tokens,
        260
    );
    assert_eq!(
        daily[&(((NOW_SECS - 40 * 86_400) / 86_400) as i64)].tokens,
        390
    );
    let by_model = compute_model_breakdown(&files, &cache, NOW_SECS);
    assert_eq!(by_model["claude-opus-4-8"].year.tokens, 160);
    assert_eq!(by_model["claude-opus-4-8"].year.tools["Read"], 2);
    assert_eq!(by_model["gpt-5-codex"].year.tokens, 220);
    assert_eq!(by_model["gpt-5-codex"].year.tools["Bash"], 1);
    assert_eq!(by_model["gpt-5-old"].year.tokens, 330);
    assert_eq!(by_model["gpt-5-old"].month.tokens, 0);
}
#[test]
fn spend_window_merge_preserves_every_displayed_dimension() {
    let mut total = SpendWindow {
        usd: 1.0,
        tokens: 10,
        input: 4,
        output: 6,
        cache_write: 1,
        cache_read: 2,
        tool_calls: 1,
        tools: BTreeMap::from([("shell".to_owned(), 1)]),
        sessions: 1,
        timed_ms: 2_000,
        timed_output: 100,
    };
    total.merge(&SpendWindow {
        usd: 2.0,
        tokens: 20,
        input: 8,
        output: 12,
        cache_write: 3,
        cache_read: 4,
        tool_calls: 2,
        tools: BTreeMap::from([("shell".to_owned(), 1), ("apply_patch".to_owned(), 1)]),
        sessions: 2,
        timed_ms: 4_000,
        timed_output: 300,
    });

    assert_eq!(total.usd, 3.0);
    assert_eq!(total.display_tokens(), 36);
    assert_eq!(total.tool_calls, 3);
    assert_eq!(total.tools["shell"], 2);
    assert_eq!(total.tools["apply_patch"], 1);
    assert_eq!(total.sessions, 3);
    assert_eq!(total.timed_ms, 6_000);
    assert_eq!(total.timed_output, 400);
    assert_eq!(total.tokens_per_sec(), Some(67));
}

#[test]
fn response_timing_filters_boundaries() {
    for (output, ms) in [(99, 1_000), (100, 0), (100, 200), (100, 300_001)] {
        assert_eq!(response_timing(output, ms), None);
    }
    assert_eq!(
        response_timing(100, 201),
        Some(ResponseTiming {
            ms: 201,
            output: 100
        })
    );
    assert_eq!(
        response_timing(100, 300_000),
        Some(ResponseTiming {
            ms: 300_000,
            output: 100
        })
    );
}

#[test]
fn timed_windows_use_ratio_of_sums() {
    let path = PathBuf::from("/x/claude.jsonl");
    let mut first = cached_entry(NOW_SECS, 1.0, "session");
    first.timing = Some(ResponseTiming {
        ms: 2_000,
        output: 100,
    });
    let mut second = first.clone();
    second.timing = Some(ResponseTiming {
        ms: 4_000,
        output: 300,
    });
    let untimed = cached_entry(NOW_SECS, 2.0, "session");
    let cache = SpendingDiskCache {
        files: HashMap::from([cached_file(&path, vec![first, second, untimed])]),
        ..Default::default()
    };
    let files = vec![spending_file(claude_adapter(), path)];
    let counted = dedup_cached_entries(&files, &cache).into_counted();
    let spending = aggregate_spending(&files, &cache, &counted, NOW_SECS, &HeadlineSpec::default());
    assert_eq!(spending.total.year.tokens_per_sec(), Some(67));
    assert_eq!(SpendWindow::default().tokens_per_sec(), None);
    assert_eq!(
        SpendWindow {
            timed_output: u64::MAX,
            timed_ms: 1_000,
            ..Default::default()
        }
        .tokens_per_sec(),
        Some(u64::MAX)
    );
}

#[test]
fn duplicate_timing_breaks_only_usage_ties_in_both_folds() {
    let mut first = cached_entry(NOW_SECS, 1.0, "session");
    first.message_id = Some("message".to_owned());
    let mut timed = first.clone();
    timed.timing = Some(ResponseTiming {
        ms: 1_000,
        output: 100,
    });
    let mut latest = timed.clone();
    latest.timing.as_mut().unwrap().ms = 2_000;
    let rows = vec![first, timed, latest.clone()];
    let mut chunk = rows.clone();
    refresh::dedup_chunk(&mut chunk);
    assert_eq!(chunk, vec![latest.clone()]);
    let path = PathBuf::from("/x/claude.jsonl");
    let cache = SpendingDiskCache {
        files: HashMap::from([cached_file(&path, rows)]),
        ..Default::default()
    };
    let files = vec![spending_file(claude_adapter(), path)];
    let counted = dedup_cached_entries(&files, &cache).into_counted();
    assert_eq!(counted.len(), 1);
    assert_eq!(aggregate::DedupPayload::entry(&counted[0]), &latest);
    let mut richer = latest.clone();
    richer.has_speed = true;
    richer.timing = None;
    assert!(aggregate::should_replace_duplicate(&richer, &latest));
    assert!(!aggregate::should_replace_duplicate(&latest, &richer));
}
