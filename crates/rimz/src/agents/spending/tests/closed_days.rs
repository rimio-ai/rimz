use super::*;
use crate::agents::{ProviderLogin, TranscriptStat};
use crate::disk::paths::{RuntimePaths, SpendingHistoryPaths};
use crate::ids::{AgentKind, WorkspaceId};
use jiff::Timestamp;
use serde_json::{Value, json};

const SESSION: &str = "12345678-1234-4234-8234-123456789abc";

fn seconds(at: &str) -> u64 {
    at.parse::<Timestamp>().unwrap().as_second() as u64
}

fn row(at: &str, usd: f64) -> CachedEntry {
    let mut entry = CachedEntry::new(seconds(at), usd, &Default::default());
    entry.input = 10;
    entry.output = 20;
    entry.cache_write = 30;
    entry.cache_read = 40;
    entry.model = Some("test-model".into());
    entry.message_id = Some(format!("message-{at}").into());
    entry.request_id = Some(format!("request-{at}").into());
    entry.timing = Some(ResponseTiming {
        ms: 5000,
        output: 20,
    });
    entry.tool_calls = [("Bash", 3)].into_iter().collect();
    entry.fast = true;
    entry
}

struct Fixture {
    runtime: RuntimePaths,
    home: PathBuf,
    walker: SpendingWalker,
    adapter: &'static AgentDefinition,
    timezone: &'static str,
    prices: PriceBook,
}

impl Fixture {
    fn new(root: &Path, name: &str) -> Self {
        let home = root.join("accounts/claude/shared");
        std::fs::create_dir_all(home.join("projects/project")).unwrap();
        let runtime_root = root.join(name);
        std::fs::create_dir_all(&runtime_root).unwrap();
        let mut runtime =
            RuntimePaths::under(WorkspaceId::from_project_root(root), &runtime_root).unwrap();
        runtime.spending_history = SpendingHistoryPaths::under(&root.join("accounts"));
        Self {
            runtime,
            home,
            walker: SpendingWalker::new(),
            adapter: claude_adapter(),
            timezone: "UTC",
            prices: PriceBook::default(),
        }
    }

    fn path(&self, session: &str) -> PathBuf {
        self.home
            .join("projects/project")
            .join(format!("{session}.jsonl"))
    }

    fn seed(&self, rows: Vec<(PathBuf, Vec<CachedEntry>)>) {
        let cache = SpendingDiskCache {
            version: SPENDING_CACHE_VERSION,
            files: rows
                .into_iter()
                .map(|(path, entries)| {
                    (
                        path.to_string_lossy().into_owned(),
                        FileCacheEntry {
                            stat: TranscriptStat::from_path(&path).unwrap_or(TranscriptStat {
                                mtime_secs: seconds("2026-06-30T12:00:00Z") as i64,
                                ..Default::default()
                            }),
                            cursor: SpendCursor::default(),
                            origin_path: Some(self.home.clone()),
                            entries,
                            unknown_models: BTreeMap::new(),
                        },
                    )
                })
                .collect(),
            ..Default::default()
        };
        assert!(write_spending_cache(
            &self.runtime.shared_spending_cursor_path(),
            &cache
        ));
    }

    fn discover(&mut self, now: u64) -> Vec<SpendingFile> {
        self.walker.discovery.force_complete_for_test();
        self.walker.discovery.discover(
            std::iter::once((
                ProviderLogin::default_for(AgentKind::new_unchecked(self.adapter.spec().kind)),
                self.adapter,
                BTreeMap::from([(
                    match self.adapter.spec().kind {
                        "codex" => "CODEX_HOME",
                        "grok" => "GROK_HOME",
                        _ => "CLAUDE_CONFIG_DIR",
                    }
                    .into(),
                    self.home.to_string_lossy().into_owned(),
                )]),
            )),
            now,
        )
    }

    fn walk(&mut self, now: u64, publish: bool) -> SpendingWalkResult {
        let files = self.discover(now);
        self.walk_files(&files, now, publish)
    }

    fn walk_files(
        &mut self,
        files: &[SpendingFile],
        now: u64,
        publish: bool,
    ) -> SpendingWalkResult {
        let scope = SpendScope::from_roots(Some(&self.home), &[]);
        let request = WalkRequest {
            history: &self.runtime.spending_history,
            files,
            prices: &self.prices,
            now_secs: now,
            origin_overrides: &HashMap::new(),
            user_inputs: &[],
            scope: Some(&scope),
            spec: &HeadlineSpec {
                timezone: Some(self.timezone.into()),
                ..Default::default()
            },
        };
        if publish {
            self.walker.walk(
                &self.runtime.shared_spending_cursor_path(),
                &request,
                &mut SilentWalk,
            )
        } else {
            self.walker.walk_local(
                &self.runtime.shared_spending_cursor_path(),
                &request,
                &mut SilentWalk,
            )
        }
    }

    fn lines(&self) -> Vec<Value> {
        std::fs::read_to_string(self.runtime.spending_history.spend(2026))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }
}

#[test]
fn closed_days_recover_absent_files_and_survive_cursor_loss_across_homes() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    a.seed(vec![(
        a.path(SESSION),
        vec![row("2026-06-10T12:00:00Z", 12.5)],
    )]);
    session_roles::record(
        &a.runtime,
        &session_roles::SessionRoleRecord {
            kind: AgentKind::new_unchecked("claude"),
            session: SESSION.into(),
            at: "2026-06-10T12:00:00Z".parse().unwrap(),
            role: session_roles::SessionRole::Subagent {
                profile: "reviewer".into(),
                owner: None,
            },
        },
    )
    .unwrap();
    let first = a.walk(now, true);
    assert_eq!(
        first.spending.total.month.usd, 12.5,
        "absent cursor rows must enter the fold"
    );
    let lines = a.lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["account"], "claude/shared");
    assert_eq!(lines[0]["rows"][0]["session"], json!({"native": SESSION}));
    assert_eq!(
        lines[0]["rows"][0]["timing"],
        json!({"ms": 5000, "output": 20})
    );
    assert_eq!(lines[0]["rows"][0]["tools"], json!({"Bash": 3}));
    assert_eq!(lines[0]["rows"][0]["fast"], true);
    let mut b = Fixture::new(dir.path(), "b");
    for version in [SPENDING_CACHE_VERSION, SPENDING_CACHE_VERSION - 1] {
        let empty = SpendingDiskCache {
            version,
            ..Default::default()
        };
        assert!(write_spending_cache(
            &b.runtime.shared_spending_cursor_path(),
            &empty
        ));
        let result = b.walk(now, true);
        assert_eq!(result.spending, first.spending);
        assert_eq!(result.models, first.models);
        assert_eq!(result.chart, first.chart);
        assert_eq!(result.days, first.days);
        assert_eq!(result.roles.subagents["reviewer"].tally.month.usd, 12.5);
        assert_eq!(result.spending.total.month.display_tokens(), 100);
        assert_eq!(result.spending.total.year.sessions, 1);
        assert_eq!(result.spending.total.year.timed_ms, 5000);
        assert_eq!(result.login_month.values().next().unwrap().usd, 12.5);
    }
    assert_eq!(
        a.lines(),
        lines,
        "a second home must not append an existing day"
    );
}

#[test]
fn closed_days_first_account_day_wins_but_another_home_can_fill_a_gap() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    a.seed(vec![(
        a.path(SESSION),
        vec![row("2026-06-10T12:00:00Z", 2.0)],
    )]);
    a.walk(now, true);
    let mut b = Fixture::new(dir.path(), "b");
    b.seed(vec![(
        b.path(SESSION),
        vec![
            row("2026-06-10T12:00:00Z", 99.0),
            row("2026-06-11T12:00:00Z", 3.0),
        ],
    )]);
    let result = b.walk(now, true);
    assert_eq!(result.spending.total.month.usd, 5.0);
    assert_eq!(b.lines().len(), 2);
}

#[test]
fn closed_days_close_at_next_midnight_plus_fourteen_days_only_for_authoritative_publishers() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let cutoff = seconds("2026-06-25T00:00:00Z");
    a.seed(vec![(
        a.path(SESSION),
        vec![row("2026-06-10T12:00:00Z", 2.0)],
    )]);
    a.walk(cutoff - 1, true);
    assert!(a.lines().is_empty());
    a.walk(cutoff, false);
    assert!(a.lines().is_empty());
    a.walker.discovery.mark_non_authoritative_for_test();
    a.walk_files(&[], cutoff, true);
    assert!(a.lines().is_empty());
    let result = a.walk(cutoff, true);
    assert_eq!(result.spending.total.month.usd, 2.0);
    assert_eq!(
        a.lines().len(),
        1,
        "the day must close at the exact boundary"
    );
}

#[test]
fn closed_days_recording_scans_only_when_a_day_can_be_added() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    let path = a.path(SESSION);
    std::fs::write(
        &path,
        format!("{}\n", claude_line("2026-06-10", 2.0, "old", "old")),
    )
    .unwrap();
    a.walk(now, true);
    assert_eq!(a.lines().len(), 1);
    assert_eq!(a.walker.closed_days.recording_dedups, 1);

    let observe = |a: &mut Fixture, publish| {
        let before = a.walker.closed_days.recording_dedups;
        let result = a.walk(now, publish);
        (
            result.stats.parse_jobs,
            a.walker.closed_days.recording_dedups - before,
            a.lines().len(),
        )
    };
    let mut observations = Vec::new();
    for (at, cost, id) in [
        ("2026-06-30T12:00:00Z", 3.0, "recent"),
        ("2026-06-10T16:00:00Z", 99.0, "covered"),
        ("2026-06-11T12:00:00Z", 4.0, "uncovered"),
    ] {
        append_line(&path, &claude_line_ts(at, cost, id, id));
        observations.push(observe(&mut a, true));
    }
    observations.push(observe(&mut a, true));

    append_line(&path, &claude_line("2026-06-12", 5.0, "local", "local"));
    observations.push(observe(&mut a, false));
    observations.push(observe(&mut a, true));

    let hold = history::try_maintain(&a.runtime.spending_history)
        .unwrap()
        .unwrap();
    append_line(&path, &claude_line("2026-06-13", 6.0, "locked", "locked"));
    observations.push(observe(&mut a, true));
    drop(hold);
    observations.push(observe(&mut a, true));

    let recorded = a
        .lines()
        .into_iter()
        .map(|line| {
            (
                line["day"].as_str().unwrap().to_owned(),
                line["rows"][0]["usd"].as_f64().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        recorded,
        [
            ("2026-06-10".into(), 2.0),
            ("2026-06-11".into(), 4.0),
            ("2026-06-12".into(), 5.0),
            ("2026-06-13".into(), 6.0),
        ]
    );
    assert_eq!(
        observations,
        [
            (1, 0, 1),
            (1, 0, 1),
            (1, 1, 2),
            (0, 0, 2),
            (1, 0, 2),
            (0, 1, 3),
            (1, 0, 3),
            (0, 1, 4),
        ],
        "only uncovered closed days need recording dedup; deferred changes must survive"
    );
}

#[test]
fn closed_days_obey_shared_lock_and_format_and_expire_only_complete_years() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    a.seed(vec![(
        a.path(SESSION),
        vec![row("2026-06-10T12:00:00Z", 2.0)],
    )]);
    let hold = history::try_maintain(&a.runtime.spending_history)
        .unwrap()
        .unwrap();
    a.walk(now, true);
    assert!(a.lines().is_empty());
    drop(hold);
    for mark in ["2\n", "not an integer\n"] {
        std::fs::write(a.runtime.spending_history.format(), mark).unwrap();
        a.walk(now, true);
        assert!(a.lines().is_empty());
    }
    std::fs::write(a.runtime.spending_history.format(), "1\n").unwrap();
    a.walk(now, true);
    assert_eq!(a.lines().len(), 1);
    let old = a.runtime.spending_history.spend(2024);
    std::fs::write(&old, "old year").unwrap();
    a.walk(now, true);
    assert!(!old.exists(), "expired whole-year segments must be removed");
    assert!(a.runtime.spending_history.spend(2026).exists());
}

#[test]
fn closed_days_dedup_fork_copies_before_recording_and_keep_live_room_figures() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    let source = a.path(SESSION);
    let fork = a.path("22345678-1234-4234-8234-123456789abc");
    let old = row("2026-06-10T12:00:00Z", 2.0);
    let live = row("2026-06-30T11:00:00Z", 3.0);
    std::fs::write(&fork, "").unwrap();
    a.seed(vec![
        (source, vec![old.clone()]),
        (fork.clone(), vec![old, live]),
    ]);
    let without = a.walk(now, false);
    let with = a.walk(now, true);
    assert_eq!(
        with.spending.total.month.usd, 5.0,
        "present and absent copies share one dedup"
    );
    assert_eq!(
        with.workspace, without.workspace,
        "history never changes room or live baselines"
    );
    std::fs::remove_file(fork).unwrap();
    a.seed(Vec::new());
    let after = a.walk(now, true);
    assert_eq!(
        after.spending.total.month.usd, 2.0,
        "the recorded day counts only once after deletion"
    );
}

#[test]
fn closed_days_resolve_account_identity_ignore_unknown_accounts_and_skip_torn_lines() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    let path = a.runtime.spending_history.spend(2026);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let line = |account: &str, usd: f64| {
        json!({
            "kind": "claude",
            "account": account,
            "day": "2026-06-10",
            "rows": [{
                "session": {"native": SESSION},
                "ts": seconds("2026-06-10T12:00:00Z"),
                "usd": usd,
                "future_field": true
            }]
        })
        .to_string()
    };
    let mut malformed: Value = serde_json::from_str(&line("claude/shared", 0.0)).unwrap();
    malformed["rows"][0]["usd"] = json!("invalid");
    std::fs::write(
        &path,
        format!(
            "not-json\n{malformed}\n{}\n{}\n{}\n{{\"kind\":",
            line("claude/shared", 2.0),
            line("claude/shared", 99.0),
            line("claude/unknown", 88.0)
        ),
    )
    .unwrap();
    let result = a.walk(now, false);
    assert_eq!(
        result.spending.total.month.usd, 2.0,
        "first decodable matching account-day wins"
    );
    let other = TempDir::new().unwrap();
    let mut b = Fixture::new(other.path(), "b");
    let copied = b.runtime.spending_history.spend(2026);
    std::fs::create_dir_all(copied.parent().unwrap()).unwrap();
    std::fs::copy(&path, copied).unwrap();
    assert_eq!(
        b.walk(now, false).spending.total.month.usd,
        2.0,
        "relative identities agree across absolute roots"
    );
    a.home = dir.path().join("outside");
    std::fs::create_dir_all(a.home.join("projects")).unwrap();
    a.seed(vec![(
        a.path(SESSION),
        vec![row("2026-06-11T12:00:00Z", 3.0)],
    )]);
    assert_eq!(a.walk(now, true).spending.total.month.usd, 3.0);
    assert!(
        a.lines()
            .iter()
            .any(|line| line["account"] == a.home.to_string_lossy().as_ref())
    );
}

#[test]
fn closed_days_file_sessions_union_live_and_recorded_rows() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    let path = a.path("not-a-native-id");
    std::fs::write(&path, "").unwrap();
    a.seed(vec![(
        path,
        vec![
            row("2026-06-10T12:00:00Z", 2.0),
            row("2026-06-30T11:00:00Z", 3.0),
        ],
    )]);
    a.walk(now, true);
    let result = a.walk(now, true);
    assert_eq!(a.lines().len(), 1);
    assert_eq!(
        a.lines()[0]["rows"][0]["session"],
        json!({"file":"projects/project/not-a-native-id.jsonl"})
    );
    assert_eq!(result.spending.total.month.usd, 5.0);
    assert_eq!(
        result.spending.total.month.sessions, 1,
        "one file session spans both sources"
    );
}

#[test]
fn closed_days_keep_provider_dedup_keys_through_a_prior_local_walk() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    a.adapter = codex_adapter();
    let now = seconds("2026-06-30T12:00:00Z");
    let source = a.home.join("sessions/2026/06/10/rollout-source.jsonl");
    let copy = a.home.join("sessions/2026/06/30/rollout-copy.jsonl");
    std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
    std::fs::write(&copy, "").unwrap();
    let mut entry = row("2026-06-10T12:00:00Z", 2.0);
    entry.message_id = None;
    entry.request_id = None;
    entry.dedup_key = Some("codex:shared-response".into());
    entry.thread_id = Some(SESSION.into());
    a.seed(vec![(source, vec![entry.clone()]), (copy, vec![entry])]);
    a.walk(now, false);
    let result = a.walk(now, true);
    assert_eq!(
        result.spending.total.month.usd, 2.0,
        "an absent provider-keyed copy must not double the recorded day"
    );
}

#[test]
fn closed_days_count_a_compacted_codex_rollout_once_after_archiving() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    a.adapter = codex_adapter();
    a.prices = gpt4o_book();
    let name = format!("rollout-2026-06-10T15-00-00-{SESSION}.jsonl");
    let source = a.home.join("sessions/2026/06/10").join(&name);
    let archived = a.home.join("archived_sessions").join(&name);
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::create_dir_all(archived.parent().unwrap()).unwrap();
    std::fs::write(
        &source,
        format!(
            "{}\n{}\n",
            json!({"type":"turn_context", "payload":{"model":"gpt-4o"}}),
            codex_token_line("2026-06-10", 2_000_000, 0, 0)
        ),
    )
    .unwrap();
    let live = a.walk(seconds("2026-06-19T15:00:00Z"), true);
    assert_eq!(live.spending.total.month.usd, 2.0);
    assert!(a.lines().is_empty());
    assert!(a.walker.cache.files[source.to_str().unwrap()].entries[0].rolled);
    std::fs::rename(&source, &archived).unwrap();
    let recorded = a.walk(seconds("2026-06-30T12:00:00Z"), true);
    assert_eq!(
        recorded.stats.parse_jobs, 1,
        "the moved rollout must reparse"
    );
    assert_eq!(
        recorded.spending.total.month.usd, 2.0,
        "the absent compacted path must not duplicate the archived rollout"
    );
}

#[test]
fn closed_days_keep_a_deleted_subagents_spend_beside_its_parent() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let parent = a.path(SESSION);
    let child = parent
        .with_extension("")
        .join("subagents/agent-child.jsonl");
    let other_child = a
        .path("22345678-1234-4234-8234-123456789abc")
        .with_extension("")
        .join("subagents/agent-child.jsonl");
    std::fs::write(&parent, "").unwrap();
    std::fs::create_dir_all(other_child.parent().unwrap()).unwrap();
    std::fs::write(&other_child, "").unwrap();
    for (session, role) in [
        (
            SESSION,
            json!({
                "role":"agent", "profile":"coder", "seat":{"team":"forge", "role":"coder"},
            }),
        ),
        (
            "22345678-1234-4234-8234-123456789abc",
            json!({
                "role":"subagent", "profile":"reviewer", "owner":{"team":"forge", "role":"coder"},
            }),
        ),
    ] {
        session_roles::record(
            &a.runtime,
            &session_roles::SessionRoleRecord {
                kind: AgentKind::new_unchecked("claude"),
                session: session.into(),
                role: serde_json::from_value(role).unwrap(),
                at: "2026-06-10T12:00:00Z".parse().unwrap(),
            },
        )
        .unwrap();
    }
    a.seed(vec![
        (parent.clone(), vec![row("2026-06-10T12:00:00Z", 2.0)]),
        (child, vec![row("2026-06-10T13:00:00Z", 3.0)]),
        (other_child.clone(), vec![row("2026-06-10T14:00:00Z", 4.0)]),
    ]);
    let recorded = a.walk(seconds("2026-06-30T12:00:00Z"), true);
    assert_eq!(recorded.spending.total.month.usd, 9.0);
    assert_eq!(recorded.spending.total.month.sessions, 2);
    assert_eq!(recorded.roles.teams["forge"]["coder"].tally.month.usd, 9.0);
    assert_eq!(
        recorded.roles.teams["forge"]["coder"].tally.month.sessions,
        1
    );
    assert_eq!(recorded.roles.subagents["reviewer"].tally.month.usd, 4.0);
    std::fs::remove_file(parent).unwrap();
    std::fs::remove_file(other_child).unwrap();
    a.seed(Vec::new());
    let absent = a.walk(seconds("2026-06-30T12:00:00Z"), true);
    assert_eq!(absent.roles, recorded.roles);
}

#[test]
fn closed_days_keep_deleted_sessions_with_a_shared_transcript_filename() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    a.adapter = crate::agents::definition_by_kind("grok").unwrap();
    let deleted = a.home.join("sessions/deleted/updates.jsonl");
    let present = a.home.join("sessions/present/updates.jsonl");
    std::fs::create_dir_all(present.parent().unwrap()).unwrap();
    std::fs::write(&present, "").unwrap();
    a.seed(vec![
        (deleted, vec![row("2026-06-10T12:00:00Z", 2.0)]),
        (present, vec![row("2026-06-10T13:00:00Z", 3.0)]),
    ]);
    let recorded = a.walk(seconds("2026-06-30T12:00:00Z"), true);
    assert_eq!(
        recorded.spending.total.month.usd, 5.0,
        "a shared filename must not hide another session's retained spend"
    );
    assert_eq!(a.lines().len(), 1);
    assert_eq!(recorded.spending.total.month.sessions, 2);
}

#[test]
fn closed_days_keep_sessions_whose_copied_turns_all_lose_dedup() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    let fork = "22345678-1234-4234-8234-123456789abc";
    for (session, profile) in [(SESSION, "source"), (fork, "fork")] {
        std::fs::write(a.path(session), "").unwrap();
        session_roles::record(
            &a.runtime,
            &session_roles::SessionRoleRecord {
                kind: AgentKind::new_unchecked("claude"),
                session: session.into(),
                at: "2026-06-10T12:00:00Z".parse().unwrap(),
                role: session_roles::SessionRole::Subagent {
                    profile: profile.into(),
                    owner: None,
                },
            },
        )
        .unwrap();
    }
    let entries = vec![
        row("2026-06-10T12:00:00Z", 2.0),
        row("2026-06-10T13:00:00Z", 3.0),
    ];
    a.seed(vec![
        (a.path(SESSION), entries.clone()),
        (a.path(fork), entries),
    ]);
    let live = a.walk(now, false);
    assert_eq!(live.spending.total.month.sessions, 2);
    let recorded = a.walk(now, true);
    assert_eq!(
        recorded.spending, live.spending,
        "a dedup-only fork must survive recording as a session, not as spend"
    );
    assert_eq!(recorded.models, live.models);
    assert_eq!(recorded.roles, live.roles);
    assert_eq!(recorded.days, live.days);
    assert_eq!(recorded.chart, live.chart);
    assert_eq!(recorded.login_month, live.login_month);
    let lines = a.lines();
    let session_only = lines[0]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row.get("usd").is_none())
        .expect("a losing session must have a session-only row");
    assert_eq!(session_only.as_object().unwrap().len(), 2);
    assert_eq!(session_only["ts"], seconds("2026-06-10T13:00:00Z"));
    for session in [SESSION, fork] {
        std::fs::remove_file(a.path(session)).unwrap();
    }
    a.seed(Vec::new());
    let absent = a.walk(now, true);
    assert_eq!(absent.spending, live.spending);
    assert_eq!(absent.models, live.models);
    assert_eq!(absent.roles, live.roles);
    assert_eq!(absent.login_month, live.login_month);
}

#[test]
fn closed_days_defer_an_account_until_every_discovered_file_has_a_cursor() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    let first = a.path(SESSION);
    let second = a.path("22345678-1234-4234-8234-123456789abc");
    let empty = a.path("32345678-1234-4234-8234-123456789abc");
    std::fs::write(&first, claude_line("2026-06-10", 2.0, "first", "first")).unwrap();
    std::fs::write(&second, "").unwrap();
    std::fs::write(&empty, "").unwrap();
    let files = a.discover(now);
    std::fs::remove_file(&second).unwrap();
    a.walk_files(&files, now, true);
    assert!(
        a.walker.cache.files[empty.to_str().unwrap()]
            .entries
            .is_empty(),
        "an empty transcript must still receive a cursor"
    );
    assert!(
        a.lines().is_empty(),
        "a file missing its first cursor must hold back the account's whole day"
    );
    std::fs::write(&second, claude_line("2026-06-10", 3.0, "second", "second")).unwrap();
    let recorded = a.walk(now, true);
    assert_eq!(recorded.spending.total.month.usd, 5.0);
    assert_eq!(a.lines().len(), 1);
}

#[test]
fn closed_days_ignore_uncached_files_intentionally_skipped_out_of_window() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    let now = seconds("2026-06-30T12:00:00Z");
    let current = a.path(SESSION);
    let expired = a.path("22345678-1234-4234-8234-123456789abc");
    std::fs::write(
        &current,
        claude_line("2026-06-10", 2.0, "current", "current"),
    )
    .unwrap();
    std::fs::write(
        &expired,
        claude_line("2024-01-01", 99.0, "expired", "expired"),
    )
    .unwrap();
    std::fs::File::open(&expired)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH)
        .unwrap();
    let recorded = a.walk(now, true);
    assert!(!a.walker.cache.files.contains_key(expired.to_str().unwrap()));
    assert_eq!(recorded.spending.total.month.usd, 2.0);
    assert_eq!(
        a.lines().len(),
        1,
        "an intentionally skipped file must not block recording"
    );
}

#[test]
fn closed_days_keep_per_file_local_midnight_and_month_splits() {
    let dir = TempDir::new().unwrap();
    let mut a = Fixture::new(dir.path(), "a");
    a.timezone = "Asia/Shanghai";
    let now = seconds("2026-07-29T12:00:00Z");
    let parent = a.path(SESSION);
    let child = parent
        .with_extension("")
        .join("subagents/agent-child.jsonl");
    std::fs::create_dir_all(child.parent().unwrap()).unwrap();
    std::fs::write(&parent, "").unwrap();
    std::fs::write(&child, "").unwrap();
    a.seed(vec![
        (parent, vec![row("2026-06-29T15:00:00Z", 2.0)]),
        (child, vec![row("2026-06-29T17:00:00Z", 3.0)]),
    ]);
    let live = a.walk(now, false);
    assert_eq!(live.spending.total.month.usd, 3.0);
    let recorded = a.walk(now, true);
    assert_eq!(recorded.spending, live.spending);
    assert_eq!(recorded.days, live.days);
    assert_eq!(recorded.models, live.models);
    assert_eq!(recorded.roles, live.roles);
    let lines = a.lines();
    assert_eq!(lines[0]["rows"].as_array().unwrap().len(), 2);
}
