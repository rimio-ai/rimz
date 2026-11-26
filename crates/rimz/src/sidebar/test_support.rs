use std::path::Path;

use jiff::Timestamp;

use crate::agents::{AgentState, AgentStatus, RateLimitWindow, TurnPhase};
use crate::ids::{MuxName, PaneId, WorkspaceId};
use crate::pane::PaneRef;
use crate::store::snapshot::{SidebarSnapshot, SidebarWorktreeGroup, SidebarWorktreeKind};

pub(crate) fn pane(id: &str, command: &str, cwd: &str) -> PaneRef {
    PaneRef {
        pane_id: PaneId::from_parts(MuxName::Zellij, id),
        session_name: "rimz-test".to_owned(),
        view_id: Some("@0".to_owned()),
        view_kind: None,
        view_name: None,
        title: None,
        is_floating: false,
        command: Some(command.to_owned()),
        foreground_cmdline: None,
        spawn_command: None,
        cwd: Some(cwd.to_owned()),
        pane_pid: None,
        pane_process_start: None,
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    }
}

pub(crate) fn pane_in_tab(id: &str, view_id: &str) -> PaneRef {
    PaneRef {
        view_id: Some(view_id.to_owned()),
        ..pane(id, "zsh", "/tmp")
    }
}

/// A 5-hour budget window for tests — a known `duration_mins` so the
/// projection and per-duration reconciliation have something to key on.
pub(crate) fn rl_window(used: u8, resets_at: Option<Timestamp>) -> RateLimitWindow {
    rl_window_mins(used, resets_at, 300)
}

pub(crate) fn rl_window_mins(
    used: u8,
    resets_at: Option<Timestamp>,
    duration_mins: u32,
) -> RateLimitWindow {
    RateLimitWindow {
        used_percentage: Some(used),
        resets_at,
        duration_mins: Some(duration_mins),
        ..Default::default()
    }
}

pub(crate) fn provider_panel(
    kind: &str,
    windows: Vec<RateLimitWindow>,
) -> crate::store::snapshot::SidebarProviderPanel {
    crate::store::snapshot::SidebarProviderPanel {
        account: Default::default(),
        kind: kind.to_owned(),
        sub_provider: None,
        account_scope: Default::default(),
        entitlement: Default::default(),
        account_key: None,
        product_name: kind.to_owned(),
        art: Vec::new(),
        art_tints: Vec::new(),
        color: 0,
        color_rgb: None,
        color_role: None,
        version: None,
        plan: None,
        metered: true,
        undecided: false,
        alternates: Vec::new(),
        remote_control: Default::default(),
        active_sessions: 0,
        spending: None,
        day_budget: None,
        month_budget: None,
        extra_credits: None,
        reset_credits: None,
        redeem_forecast: None,
        window_placeholders: Vec::new(),
        windows,
    }
}

pub(crate) fn snapshot_with_panels(
    workspace: WorkspaceId,
    panels: Vec<crate::store::snapshot::SidebarProviderPanel>,
) -> SidebarSnapshot {
    let mut snapshot = SidebarSnapshot::build(workspace, Vec::new(), Timestamp::now());
    snapshot.providers = panels;
    snapshot
}

pub(crate) fn root_agent(kind: &str, agent_id: &str, model: Option<&str>) -> AgentState {
    let now = Timestamp::now();
    AgentState {
        name: Some(test_agent_name(agent_id)),
        kind_ordinal: Some(test_agent_ordinal(agent_id)),
        status: AgentStatus::Running,
        model: model.map(ToOwned::to_owned),
        ..crate::testkit::agent_state(kind, agent_id, now)
    }
}

pub(crate) fn test_agent_name(agent_id: &str) -> String {
    let slug = agent_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    format!("test-{slug}")
}

pub(crate) fn test_agent_ordinal(agent_id: &str) -> u32 {
    agent_id
        .bytes()
        .fold(1u32, |acc, byte| acc.wrapping_add(byte as u32))
}

pub(crate) fn child_agent(kind: &str, parent_id: &str, agent_id: &str) -> AgentState {
    let mut agent = root_agent(kind, agent_id, None);
    agent.parent_agent_id = Some(parent_id.into());
    agent
}

pub(crate) fn activity_row(
    is_agent: bool,
    status: Option<AgentStatus>,
    last_activity: Timestamp,
    worktree_path: &Path,
) -> crate::store::snapshot::SidebarRow {
    crate::store::snapshot::SidebarRow {
        checkout: None,
        id: "row".to_owned(),
        name: "claude".to_owned(),
        pane: None,
        worktree_path: Some(worktree_path.display().to_string()),
        worktree_branch: None,
        channel: None,
        lane: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        attention_at_ms: None,
        last_activity,
        card: if is_agent {
            crate::store::snapshot::RowCard::Agent(std::sync::Arc::new(
                crate::store::snapshot::AgentCard {
                    status: status.unwrap_or(AgentStatus::Idle),
                    phase: TurnPhase::Idle,
                    ..crate::store::snapshot::AgentCard::default()
                },
            ))
        } else {
            crate::store::snapshot::RowCard::Process(crate::store::snapshot::ProcessCard::default())
        },
    }
}

pub(crate) fn worktree_group(
    path: &Path,
    rows: Vec<crate::store::snapshot::SidebarRow>,
) -> SidebarWorktreeGroup {
    SidebarWorktreeGroup {
        stalled_since: None,
        pr_stack: Default::default(),
        key: path.display().to_string(),
        label: "wt".to_owned(),
        label_qualifier: None,
        branch: None,
        kind: SidebarWorktreeKind::Worktree,
        team: None,
        cohort_effort: None,
        pipeline: None,
        workflow: None,
        status_counts: status_counts_from_rows(&rows),
        rows: rows.into(),
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
        place: Some(crate::store::snapshot::SidebarPlace {
            path: path.to_path_buf(),
            foreign: false,
        }),
        root_lane: false,
        inactive: None,
        clean: None,
        landed: None,
        trunk_sync: None,
        pr_state: None,
        pr_queue: None,
        pr_conflicting: false,
        ci: None,
        pr_number: None,
        pr_url: None,
    }
}

pub(crate) fn status_counts_from_rows(
    rows: &[crate::store::snapshot::SidebarRow],
) -> Vec<crate::store::snapshot::SidebarStatusCount> {
    use crate::store::snapshot::SidebarStatusCount;
    let mut counts: Vec<SidebarStatusCount> = Vec::new();
    for status in rows
        .iter()
        .filter_map(crate::store::snapshot::SidebarRow::attention_status)
    {
        if let Some(count) = counts.iter_mut().find(|count| count.status == status) {
            count.count += 1;
        } else {
            counts.push(SidebarStatusCount { status, count: 1 });
        }
    }
    counts
}

/// A card's bookings in every state the renderer draws, in draw order: one needing a
/// fix, a fired agent running, a fired team running at its stage in its lane (its lead
/// waiting) followed by the adopted entry that waits `after` it, a fired launch at rest
/// whose probe found no pull request, a finished one, a waiting entry on a raw term, one on
/// a timed trigger with no text of its own, one counted `later`, and one counted `older`.
/// No entry's text repeats its task name.
pub(crate) fn bookings_in_each_state(
    now: Timestamp,
) -> Vec<crate::store::snapshot::SidebarBooking> {
    use crate::store::snapshot::{
        BookingBand, BookingBlocker, BookingFire, BookingLive, BookingPr, BookingSibling,
        BookingStage, BookingStarts, BookingState, BookingWait, SiblingState, SidebarBooking,
    };
    let ago = |secs: i64| now - jiff::SignedDuration::from_secs(secs);
    let profile = |name: &str| BookingStarts::Profile(name.to_owned());
    let team = |name: &str| BookingStarts::Team(name.to_owned());
    let booking = |task: &str, starts, band, state| SidebarBooking {
        task: task.to_owned(),
        starts,
        band,
        needs_fix: None,
        state,
        lane: None,
        description: None,
    };
    let described = |text: &str, booking: SidebarBooking| SidebarBooking {
        description: Some(text.to_owned()),
        ..booking
    };
    let fired = |at, live, ended_at, pr, finished| {
        BookingState::Fired(BookingFire {
            at,
            live,
            ended_at,
            starting: false,
            pr,
            finished,
            depended_on: false,
        })
    };
    let running =
        |status, description: Option<&str>, stage: Option<&str>, pane: &str| BookingLive {
            status,
            phase: TurnPhase::Acting,
            description: description.map(str::to_owned),
            stage: stage.map(|stage| BookingStage {
                stage: stage.to_owned(),
                done: false,
            }),
            pane: Some(PaneId::from_parts(MuxName::Zellij, pane)),
        };
    let waiting = |after: Option<(BookingStarts, Option<&str>, SiblingState)>,
                   key: &str,
                   reading: Option<&str>| {
        BookingState::Waiting(BookingWait {
            blocker: Some(BookingBlocker {
                after: after.map(|(starts, lane, state)| BookingSibling {
                    state,
                    starts,
                    lane: lane.map(str::to_owned),
                }),
                key: key.to_owned(),
                reading: reading.map(str::to_owned),
                pr: reading.map(|_| 1640),
                more: 0,
            }),
            timing: None,
        })
    };
    vec![
        SidebarBooking {
            needs_fix: Some("invalid: unknown term `pr[`".to_owned()),
            ..booking(
                "broken",
                profile("coder"),
                BookingBand::NeedsFix,
                BookingState::Waiting(BookingWait::default()),
            )
        },
        described(
            "type the language",
            booking(
                "lang-typed",
                profile("coder"),
                BookingBand::Fired,
                fired(
                    ago(3_600),
                    Some(running(
                        AgentStatus::Running,
                        Some("type the workflow language"),
                        None,
                        "terminal_7",
                    )),
                    None,
                    BookingPr::Open { number: Some(1641) },
                    false,
                ),
            ),
        ),
        SidebarBooking {
            lane: Some("lane-readers".to_owned()),
            ..booking(
                "lane-readers",
                team("forge"),
                BookingBand::Fired,
                fired(
                    ago(3_000),
                    Some(running(
                        AgentStatus::Waiting,
                        None,
                        Some("Build"),
                        "terminal_8",
                    )),
                    None,
                    BookingPr::Unread,
                    false,
                ),
            )
        },
        described(
            "document the lane readers",
            booking(
                "lane-readers-docs",
                profile("writer"),
                BookingBand::Next,
                waiting(
                    Some((team("forge"), Some("lane-readers"), SiblingState::Running)),
                    "pr[lane-readers]",
                    None,
                ),
            ),
        ),
        described(
            "try the parser",
            booking(
                "spike",
                profile("coder"),
                BookingBand::Fired,
                fired(ago(2_400), None, Some(ago(600)), BookingPr::None, false),
            ),
        ),
        described(
            "fix the store",
            booking(
                "landed",
                profile("coder"),
                BookingBand::Fired,
                fired(
                    ago(2_000),
                    None,
                    Some(ago(300)),
                    BookingPr::Merged { number: Some(1630) },
                    true,
                ),
            ),
        ),
        described(
            "document the language",
            booking(
                "lang-docs",
                profile("writer"),
                BookingBand::Next,
                waiting(None, "pr[workflow-language]", Some("open")),
            ),
        ),
        booking(
            "nightly",
            profile("coder"),
            BookingBand::Next,
            BookingState::Waiting(BookingWait {
                blocker: None,
                timing: Some("once at 2026-11-14 03:00".to_owned()),
            }),
        ),
        described(
            "release the language",
            booking(
                "lang-release",
                profile("coder"),
                BookingBand::Later,
                waiting(
                    Some((profile("writer"), None, SiblingState::Waiting)),
                    "pr[lang-docs]",
                    None,
                ),
            ),
        ),
        described(
            "spike the old parser",
            booking(
                "old-spike",
                profile("coder"),
                BookingBand::Older,
                fired(
                    ago(90_000),
                    None,
                    Some(ago(86_400)),
                    BookingPr::Closed { number: Some(1500) },
                    false,
                ),
            ),
        ),
    ]
}

/// Interleaved logins of two kinds, then one of a kind no adapter serves: the
/// list a per-kind queue test runs [`KindProbes`] over.
pub(crate) fn kind_queue_logins() -> Vec<crate::agents::ProviderLogin> {
    use crate::agents::ProviderLogin;
    use crate::ids::AgentKind;
    let named = |kind: &str, name: &str| {
        ProviderLogin::named(
            AgentKind::new_unchecked(kind),
            name.parse().unwrap(),
            std::path::PathBuf::from(format!("/tmp/{kind}-{name}")),
        )
        .unwrap()
    };
    vec![
        ProviderLogin::default_for(AgentKind::new_unchecked("claude")),
        ProviderLogin::default_for(AgentKind::new_unchecked("codex")),
        named("claude", "work"),
        named("codex", "spare"),
        named("claude", "home"),
        ProviderLogin::default_for(AgentKind::new_unchecked("plugin-kind")),
    ]
}

/// Slow fake provider requests that record, per kind, the most in flight at
/// once and the order logins started in. Claude's first request holds until
/// every other kind's requests finish, so a kind queued behind it shows.
pub(crate) struct KindProbes {
    others: usize,
    live: std::sync::Mutex<std::collections::BTreeMap<String, usize>>,
    most_live: std::sync::Mutex<std::collections::BTreeMap<String, usize>>,
    most_kinds_live: std::sync::atomic::AtomicUsize,
    started: std::sync::Mutex<Vec<crate::ids::LoginKey>>,
    others_done: std::sync::atomic::AtomicUsize,
    others_finished_while_held: std::sync::atomic::AtomicBool,
}

impl KindProbes {
    const HELD: &str = "claude";

    pub(crate) fn new(logins: &[crate::agents::ProviderLogin]) -> Self {
        Self {
            others: logins
                .iter()
                .filter(|login| login.kind().as_str() != Self::HELD)
                .count(),
            live: Default::default(),
            most_live: Default::default(),
            most_kinds_live: Default::default(),
            started: Default::default(),
            others_done: Default::default(),
            others_finished_while_held: Default::default(),
        }
    }

    pub(crate) fn probe(&self, login: &crate::agents::ProviderLogin) {
        use std::sync::atomic::Ordering;
        use std::time::{Duration, Instant};
        let kind = login.kind().to_string();
        {
            let mut live = self.live.lock().unwrap();
            let count = live.entry(kind.clone()).or_default();
            *count += 1;
            let mut most_live = self.most_live.lock().unwrap();
            let most = most_live.entry(kind.clone()).or_default();
            *most = (*most).max(*count);
            let kinds_live = live.values().filter(|count| **count > 0).count();
            self.most_kinds_live.fetch_max(kinds_live, Ordering::SeqCst);
        }
        let first_held = {
            let mut started = self.started.lock().unwrap();
            started.push(login.key());
            kind == Self::HELD
                && started
                    .iter()
                    .filter(|key| key.kind.as_str() == Self::HELD)
                    .count()
                    == 1
        };
        std::thread::sleep(Duration::from_millis(20));
        if first_held {
            let deadline = Instant::now() + Duration::from_secs(10);
            while self.others_done.load(Ordering::SeqCst) < self.others && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            self.others_finished_while_held.store(
                self.others_done.load(Ordering::SeqCst) == self.others,
                Ordering::SeqCst,
            );
        }
        *self.live.lock().unwrap().get_mut(&kind).unwrap() -= 1;
        if kind != Self::HELD {
            self.others_done.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Each kind ran one request at a time, in the list's order, at least two
    /// kinds ran at once, and the other kinds finished while Claude's first
    /// request held.
    #[track_caller]
    pub(crate) fn assert_queued(&self, logins: &[crate::agents::ProviderLogin]) {
        let per_kind = |keys: Vec<crate::ids::LoginKey>| {
            let mut kinds = std::collections::BTreeMap::<String, Vec<_>>::new();
            for key in keys {
                kinds.entry(key.kind.to_string()).or_default().push(key);
            }
            kinds
        };
        let once = per_kind(logins.iter().map(|login| login.key()).collect())
            .into_keys()
            .map(|kind| (kind, 1))
            .collect();
        assert_eq!(
            (
                self.most_live.lock().unwrap().clone(),
                per_kind(self.started.lock().unwrap().clone()),
                self.most_kinds_live
                    .load(std::sync::atomic::Ordering::SeqCst)
                    >= 2,
                self.others_finished_while_held
                    .load(std::sync::atomic::Ordering::SeqCst),
            ),
            (
                once,
                per_kind(logins.iter().map(|login| login.key()).collect()),
                true,
                true
            ),
            "(most in flight per kind, start order per kind, two or more kinds at once, \
             others finished while one held)"
        );
    }
}
