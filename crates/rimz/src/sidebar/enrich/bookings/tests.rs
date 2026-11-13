use std::path::Path;

use jiff::{SignedDuration, Timestamp};

use super::*;
use crate::agents::{
    LaunchedBy, PendingWait, PendingWaitClass, PendingWaitTrigger, WorkflowAttemptRef,
    WorkflowAttemptRole,
};
use crate::ids::{AgentKind, MuxName, PaneId, WorkflowRunId, WorkspaceId};
use crate::sidebar::body_filter::BodyFilter;
use crate::sidebar::refresh::bookings::{BlockingFacts, FireFacts};
use crate::sidebar::test_support::{activity_row, pane, root_agent, worktree_group};
use crate::store::snapshot::{AgentCard, BookingStarts, SidebarPipeline};

fn now() -> Timestamp {
    "2026-11-13T12:00:00Z".parse().unwrap()
}

/// The same run id for the same name within one test thread.
fn run_id(name: &str) -> WorkflowRunId {
    thread_local! {
        static IDS: std::cell::RefCell<BTreeMap<String, WorkflowRunId>> = Default::default();
    }
    IDS.with(|ids| {
        ids.borrow_mut()
            .entry(name.to_owned())
            .or_insert_with(WorkflowRunId::new)
            .clone()
    })
}

fn ago(secs: i64) -> Timestamp {
    now() - SignedDuration::from_secs(secs)
}

fn agent(id: &str) -> AgentState {
    AgentState {
        launch_id: Some(id.into()),
        registered_at: Some(ago(86_400)),
        ..root_agent("claude", id, None)
    }
}

fn row(agent: &AgentState, pane_at: usize) -> SidebarRow {
    let mut row = activity_row(true, Some(agent.status), now(), Path::new("/w"));
    row.id = agent.agent_id.to_string();
    row.name = agent.kind.to_string();
    row.pane = Some(pane(&format!("terminal_{pane_at}"), "claude", "/w"));
    if let Some(card) = row.as_agent_mut() {
        card.handle = agent.role.clone();
        card.team = agent.team.clone();
    }
    row
}

/// A fold holding `agents`, each with a row in one group unless it has ended.
fn snapshot(agents: Vec<AgentState>) -> SidebarSnapshot {
    let rows = agents
        .iter()
        .enumerate()
        .filter(|(_, agent)| agent.ended_at.is_none())
        .map(|(index, agent)| row(agent, index))
        .collect();
    let mut snapshot = SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(Path::new("/w")),
        agents,
        now(),
    );
    snapshot.worktree_groups = vec![worktree_group(Path::new("/w"), rows)].into();
    snapshot
}

fn link(id: &str) -> LaunchedBy {
    LaunchedBy {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: id.into(),
    }
}

fn facts(task: &str, chain: &[&str]) -> BookingFacts {
    BookingFacts {
        task: task.to_owned(),
        starts: BookingStarts::Profile("coder".to_owned()),
        leader: None,
        booked_by: chain.iter().map(|id| link(id)).collect(),
        branch: None,
        worktree: false,
        needs_fix: None,
        blocking: None,
        unmet: Vec::new(),
        timing: None,
        fire: None,
    }
}

fn fired(task: &str, chain: &[&str], run: &str, at: Timestamp) -> BookingFacts {
    BookingFacts {
        fire: Some(FireFacts {
            at,
            run: Some(run_id(run)),
            ended_at: None,
            pr: BookingPr::Unread,
            team_done: None,
            launches: Vec::new(),
        }),
        ..facts(task, chain)
    }
}

fn waiting_on(task: &str, chain: &[&str], keys: &[&str]) -> BookingFacts {
    BookingFacts {
        blocking: Some(BlockingFacts {
            key: keys[0].to_owned(),
            reading: None,
            pr: None,
            more: keys.len() - 1,
        }),
        unmet: keys.iter().map(|key| (*key).to_owned()).collect(),
        ..facts(task, chain)
    }
}

fn cache(bookings: Vec<BookingFacts>) -> BookingsCache {
    BookingsCache {
        version: 2,
        bookings,
    }
}

/// Each agent row's bookings, by session id.
fn on_cards(snapshot: &SidebarSnapshot) -> BTreeMap<String, Vec<SidebarBooking>> {
    snapshot.worktree_groups[0]
        .rows
        .iter()
        .filter_map(|row| Some((row.id.clone(), row.as_agent()?.bookings.clone())))
        .filter(|(_, bookings)| !bookings.is_empty())
        .collect()
}

fn tasks(bookings: &[SidebarBooking]) -> Vec<(&str, BookingBand)> {
    bookings
        .iter()
        .map(|booking| (booking.task.as_str(), booking.band))
        .collect()
}

fn leader_of(task: &str, run: Option<&str>, registered: Timestamp, id: &str) -> AgentState {
    AgentState {
        loop_task: Some(task.to_owned()),
        registered_at: Some(registered),
        workflow: run.map(|run| WorkflowAttemptRef {
            run: run_id(run),
            seq: 1,
            role: WorkflowAttemptRole::Step,
        }),
        ..agent(id)
    }
}

#[test]
fn a_booking_shows_on_the_nearest_living_link_and_nowhere_when_none_lives() {
    let ended = AgentState {
        ended_at: Some(ago(60)),
        ..agent("booker")
    };
    let chain = ["booker", "parent", "root"];
    let mut fold = snapshot(vec![ended.clone(), agent("parent"), agent("root")]);
    attach(&mut fold, &cache(vec![facts("next", &chain)]));
    assert_eq!(
        on_cards(&fold).keys().collect::<Vec<_>>(),
        ["parent"],
        "the booker's ended row is only a representative; the walk goes up to the parent"
    );

    let mut orphaned = snapshot(vec![ended, agent("other")]);
    attach(&mut orphaned, &cache(vec![facts("next", &chain)]));
    assert!(on_cards(&orphaned).is_empty(), "no link alive: on no card");
}

#[test]
fn bookings_move_no_status_attention_count_sort_or_wait() {
    let waiting = AgentState {
        status: AgentStatus::Waiting,
        ..agent("asker")
    };
    let resting = AgentState {
        status: AgentStatus::Idle,
        ..agent("booker")
    };
    let launched = AgentState {
        status: AgentStatus::Failed,
        ..leader_of("lang", Some("wf_1"), ago(100), "child")
    };
    let mut plain = snapshot(vec![waiting, resting, launched]);
    for row in Arc::make_mut(&mut Arc::make_mut(&mut plain.worktree_groups)[0].rows) {
        if row.id == "booker"
            && let Some(card) = row.as_agent_mut()
        {
            card.pending_waits = vec![PendingWait {
                name: "w".to_owned(),
                trigger: PendingWaitTrigger::Subagent {
                    active_at: now(),
                    deadline_at: None,
                    settled: None,
                },
                armed_at: None,
                class: PendingWaitClass::Inert,
            }];
        }
    }
    let mut booked = plain.clone();
    attach(
        &mut booked,
        &cache(vec![
            fired("lang", &["booker"], "wf_1", ago(200)),
            waiting_on("docs", &["booker"], &["pr[x]"]),
        ]),
    );
    assert_eq!(on_cards(&booked)["booker"].len(), 2);
    let child = &on_cards(&booked)["booker"][0];
    let BookingState::Fired(fire) = &child.state else {
        panic!("fired entry first: {child:?}");
    };
    assert_eq!(
        fire.live.as_ref().map(|live| live.status),
        Some(AgentStatus::Failed),
        "the launched child's `!` shows on the entry's lead"
    );

    let facets = |fold: &mut SidebarSnapshot| {
        fold.sort_groups_for_presentation();
        let group = &fold.worktree_groups[0];
        let rows: Vec<_> = group
            .rows
            .iter()
            .map(|row| {
                let card = row.as_agent().unwrap();
                (
                    row.id.clone(),
                    card.status,
                    row.attention_status(),
                    card.pending_waits.len(),
                    card.sub_agents.len(),
                )
            })
            .collect();
        (
            rows,
            BodyFilter::NeedsYou.total(&fold.worktree_groups),
            group.status_counts.clone(),
        )
    };
    assert_eq!(facets(&mut booked), facets(&mut plain));
    assert_eq!(
        booked.worktree_groups[0]
            .rows
            .iter()
            .find(|row| row.id == "booker")
            .and_then(SidebarRow::attention_status),
        Some(AgentStatus::Idle),
        "a resting booker stays resting, never sleeping"
    );
}

#[test]
fn the_launch_join_survives_a_cleared_workflow_pin() {
    let detached = leader_of("lang", None, ago(100), "child");
    let earlier = leader_of("lang", None, ago(900), "stale");
    let mut fold = snapshot(vec![agent("booker"), detached, earlier]);
    attach(
        &mut fold,
        &cache(vec![fired("lang", &["booker"], "wf_1", ago(200))]),
    );
    let BookingState::Fired(fire) = &on_cards(&fold)["booker"][0].state else {
        panic!("fired");
    };
    assert_eq!(
        fire.live.as_ref().and_then(|live| live.pane.clone()),
        Some(PaneId::from_parts(MuxName::Zellij, "terminal_1")),
        "the pinless leader registered after the fire is the launch; the earlier one is not"
    );

    let mut pinned_elsewhere = snapshot(vec![
        agent("booker"),
        leader_of("lang", Some("wf_other"), ago(100), "child"),
    ]);
    attach(
        &mut pinned_elsewhere,
        &cache(vec![fired("lang", &["booker"], "wf_1", ago(200))]),
    );
    let BookingState::Fired(fire) = &on_cards(&pinned_elsewhere)["booker"][0].state else {
        panic!("fired");
    };
    assert_eq!(
        fire.live, None,
        "a row pinned to another run is not this launch"
    );
}

#[test]
fn a_team_launch_reads_its_cohort_stage_and_focus() {
    let team = |id: &str, role: &str, status| AgentState {
        team: Some("forge".to_owned()),
        role: Some(role.to_owned()),
        status,
        ..leader_of("lang", Some("wf_1"), ago(100), id)
    };
    let mut planner = team("planner", "planner", AgentStatus::Idle);
    let mut coder = team("coder", "coder", AgentStatus::Waiting);
    coder.loop_task = None;
    planner.loop_task = Some("lang".to_owned());
    let mut fold = snapshot(vec![agent("booker"), planner, coder]);
    let mut facts = fired("lang", &["booker"], "wf_1", ago(200));
    facts.starts = BookingStarts::Team("forge".to_owned());
    facts.leader = Some("planner".to_owned());
    attach(&mut fold, &cache(vec![facts.clone()]));
    let BookingState::Fired(fire) = &on_cards(&fold)["booker"][0].state else {
        panic!("fired");
    };
    let live = fire.live.as_ref().unwrap();
    assert_eq!(live.status, AgentStatus::Waiting, "a member's `?` leads");
    assert_eq!(live.stage, None, "a team with no board has no stage");

    Arc::make_mut(&mut fold.worktree_groups)[0].pipeline = Some(SidebarPipeline {
        stages: vec!["Build".to_owned(), "Done".to_owned()],
        stage: "Done".to_owned(),
        owner: None,
        started_at: None,
        stage_started_at: None,
        stage_prior_secs: 0,
        visited: Default::default(),
        done_at: Some(ago(10)),
    });
    attach(&mut fold, &cache(vec![facts]));
    let booking = &on_cards(&fold)["booker"][0];
    assert!(
        booking.is_finished(),
        "a team at Done with nothing waiting on it is finished"
    );
}

#[test]
fn order_bands_siblings_and_adoption() {
    // `c2` is the leader `second`'s fire launched, now ended: its row keeps the launch id
    // its chain link names and the run its fire pinned, under a provider session id.
    let c2_ended = AgentState {
        agent_id: "c2-session".into(),
        launch_id: Some("c2-ended".into()),
        ended_at: Some(ago(30)),
        ..leader_of("second", Some("wf_2"), ago(300), "c2-ended")
    };
    let mut fold = snapshot(vec![
        agent("booker"),
        leader_of("first", Some("wf_1"), ago(500), "c1"),
        leader_of("second", Some("wf_2"), ago(300), "c2"),
        c2_ended,
    ]);
    let mut first = fired("first", &["booker"], "wf_1", ago(600));
    first.branch = Some("first".to_owned());
    let second = fired("second", &["booker"], "wf_2", ago(400));
    let after_first = waiting_on("after-first", &["booker"], &["pr[first]"]);
    let mut gated = waiting_on("gated", &["booker"], &["pr[docs]"]);
    gated.unmet = vec!["pr[docs]".to_owned()];
    let mut docs = waiting_on("docs", &["booker"], &["pr[elsewhere]"]);
    docs.branch = Some("docs".to_owned());
    let broken = BookingFacts {
        needs_fix: Some("invalid: bad trigger".to_owned()),
        ..facts("zz-broken", &["booker"])
    };
    // Booked by `c2`, which `second`'s fire launched, and left to the booker when c2 ends:
    // it lists after `second`.
    let adopted = waiting_on("adopted", &["c2-ended", "booker"], &["pr[x]"]);
    attach(
        &mut fold,
        &cache(vec![
            second,
            gated,
            after_first,
            broken,
            docs,
            first,
            adopted,
        ]),
    );
    let list = &on_cards(&fold)["booker"];
    assert_eq!(
        tasks(list),
        [
            ("zz-broken", BookingBand::NeedsFix),
            ("first", BookingBand::Fired),
            ("second", BookingBand::Fired),
            ("adopted", BookingBand::Next),
            ("after-first", BookingBand::Next),
            ("docs", BookingBand::Next),
            ("gated", BookingBand::Later),
        ]
    );
    let BookingState::Waiting(wait) = &list[4].state else {
        panic!("waiting");
    };
    assert_eq!(
        wait.blocker
            .as_ref()
            .and_then(|blocker| blocker.after.clone()),
        Some(BookingSibling {
            task: "first".to_owned(),
            state: SiblingState::Running,
        })
    );
    let BookingState::Waiting(raw) = &list[5].state else {
        panic!("waiting");
    };
    assert_eq!(
        raw.blocker
            .as_ref()
            .map(|blocker| (blocker.after.clone(), blocker.key.as_str())),
        Some((None, "pr[elsewhere]")),
        "no sibling makes the branch: the raw term"
    );
    let BookingState::Fired(first) = &list[1].state else {
        panic!("fired");
    };
    assert!(first.depended_on);
    assert_eq!(list[0].needs_fix.as_deref(), Some("invalid: bad trigger"));
}

#[test]
fn ended_entries_past_the_newest_five_fold_older_unless_depended_on() {
    let mut fold = snapshot(vec![agent("booker")]);
    let ended = |task: &str, secs: i64| {
        let mut facts = fired(task, &["booker"], task, ago(100_000));
        facts.fire.as_mut().unwrap().ended_at = Some(ago(secs));
        facts
    };
    // The oldest end of all, yet listed: a waiting sibling reads its branch.
    let mut kept = ended("kept", 99_000);
    kept.branch = Some("kept".to_owned());
    attach(
        &mut fold,
        &cache(vec![
            ended("e7", 95_000),
            ended("e6", 90_000),
            ended("e5", 85_000),
            ended("e4", 80_000),
            ended("e3", 75_000),
            ended("e2", 70_000),
            ended("e1", 60),
            kept,
            waiting_on("needs-kept", &["booker"], &["pr[kept]"]),
        ]),
    );
    let list = &on_cards(&fold)["booker"];
    assert_eq!(
        tasks(list),
        [
            ("e1", BookingBand::Fired),
            ("e2", BookingBand::Fired),
            ("e3", BookingBand::Fired),
            ("e4", BookingBand::Fired),
            ("e5", BookingBand::Fired),
            ("kept", BookingBand::Fired),
            ("needs-kept", BookingBand::Next),
            ("e6", BookingBand::Older),
            ("e7", BookingBand::Older),
        ],
        "the five newest ends list however old"
    );
    assert!(list.iter().all(|booking| !booking.is_running()));
}

#[test]
fn an_ended_launch_with_no_branch_to_read_is_finished() {
    let mut fold = snapshot(vec![
        agent("booker"),
        leader_of("treeless-live", Some("wf_live"), ago(50), "c1"),
    ]);
    let mut branched = fired("branched", &["booker"], "wf_b", ago(100));
    branched.branch = Some("branched".to_owned());
    branched.worktree = true;
    branched.fire.as_mut().unwrap().ended_at = Some(ago(50));
    branched.fire.as_mut().unwrap().pr = BookingPr::None;
    let mut treeless = fired("treeless", &["booker"], "wf_t", ago(100));
    treeless.fire.as_mut().unwrap().ended_at = Some(ago(50));
    // A fresh tree per fire whose branch the records lost: it has a worktree, so it is
    // not finished for lacking a branch.
    let mut fresh = fired("fresh", &["booker"], "wf_f", ago(100));
    fresh.worktree = true;
    fresh.fire.as_mut().unwrap().ended_at = Some(ago(50));
    attach(
        &mut fold,
        &cache(vec![
            treeless,
            fresh,
            fired("treeless-live", &["booker"], "wf_live", ago(100)),
            branched,
        ]),
    );
    let list = &on_cards(&fold)["booker"];
    let by_task = |task: &str| list.iter().find(|booking| booking.task == task).unwrap();
    assert!(
        by_task("treeless").is_finished(),
        "nothing alive and no branch whose pull request could ever be read"
    );
    assert!(
        !by_task("treeless-live").is_finished(),
        "a launch with something alive is not finished for lacking a branch"
    );
    assert!(
        !by_task("fresh").is_finished(),
        "a launch into a worktree keeps its entry whatever its branch reading"
    );
    let branched = by_task("branched");
    assert!(
        !branched.is_finished() && branched.ended_without_pr(),
        "an ended launch with a branch and no pull request stays `ended, no PR` (D9)"
    );
}

#[test]
fn two_bookings_on_one_chain_share_the_card_and_a_cleared_cache_clears_it() {
    let mut fold = snapshot(vec![agent("booker")]);
    attach(
        &mut fold,
        &cache(vec![facts("a", &["booker"]), facts("b", &["booker"])]),
    );
    assert_eq!(
        tasks(&on_cards(&fold)["booker"]),
        [("a", BookingBand::Next), ("b", BookingBand::Next)]
    );
    attach(&mut fold, &BookingsCache::default());
    assert!(on_cards(&fold).is_empty());
}

#[test]
fn the_renderer_fixture_round_trips_on_a_card_in_band_order() {
    let bookings = crate::sidebar::test_support::bookings_in_each_state(now());
    let mut bands: Vec<BookingBand> = bookings.iter().map(|booking| booking.band).collect();
    bands.dedup();
    assert_eq!(
        bands,
        [
            BookingBand::NeedsFix,
            BookingBand::Fired,
            BookingBand::Next,
            BookingBand::Fired,
            BookingBand::Next,
            BookingBand::Later,
            BookingBand::Older,
        ],
        "every band, with the adopted entry right after its parent's fired one"
    );
    let card = AgentCard {
        bookings,
        ..AgentCard::default()
    };
    let json = serde_json::to_string(&card).unwrap();
    assert_eq!(serde_json::from_str::<AgentCard>(&json).unwrap(), card);
}

/// A fired booking whose fire launched the rows `launches` name (their launch ids).
fn fired_launching(
    task: &str,
    chain: &[&str],
    run: &str,
    at: Timestamp,
    launches: &[&str],
) -> BookingFacts {
    let mut facts = fired(task, chain, run, at);
    facts.fire.as_mut().unwrap().launches = launches.iter().map(|id| (*id).to_owned()).collect();
    facts
}

/// The fold holds living rows only (`RuntimeProjection::runtime_from_refs` drops ended
/// roots), so an adopted entry's parent is the fired sibling whose recorded launches hold
/// a link of its chain below the card's own.
#[test]
fn adoption_follows_the_fire_that_launched_the_booker_at_every_depth_and_once() {
    let mut fold = snapshot(vec![
        agent("booker"),
        leader_of("z", Some("wf_z"), ago(300), "z-agent"),
    ]);
    attach(
        &mut fold,
        &cache(vec![
            waiting_on("r", &["p-agent", "q-agent", "booker"], &["pr[x]"]),
            fired_launching("p", &["q-agent", "booker"], "wf_p", ago(500), &["p-agent"]),
            fired_launching("z", &["booker"], "wf_z", ago(900), &["z-agent"]),
            fired_launching("q", &["booker"], "wf_q", ago(1_000), &["q-agent"]),
        ]),
    );
    assert_eq!(
        tasks(&on_cards(&fold)["booker"]),
        [
            ("q", BookingBand::Fired),
            ("p", BookingBand::Fired),
            ("r", BookingBand::Next),
            ("z", BookingBand::Fired),
        ],
        "q's fire launched p's booker and p's fire launched r's: each follows its parent"
    );

    let mut cyclic = snapshot(vec![agent("booker")]);
    // Each ended a minute ago, among the newest five, so both stay listed fired.
    let ended = |mut facts: BookingFacts| {
        facts.fire.as_mut().unwrap().ended_at = Some(ago(60));
        facts
    };
    attach(
        &mut cyclic,
        &cache(vec![
            ended(fired_launching(
                "x",
                &["y-agent", "booker"],
                "wf_x",
                ago(500),
                &["x-agent"],
            )),
            ended(fired_launching(
                "y",
                &["x-agent", "booker"],
                "wf_y",
                ago(400),
                &["y-agent"],
            )),
        ]),
    );
    assert_eq!(
        tasks(&on_cards(&cyclic)["booker"]),
        [("x", BookingBand::Fired), ("y", BookingBand::Fired)],
        "a cycle takes the entries' ordinary places, each drawn once"
    );
}

/// Two fired siblings booked on the same chain: the entry follows the one whose fire
/// launched its booker, never a guess from the shared chain.
#[test]
fn adoption_picks_the_sibling_that_launched_the_booker_of_two_sharing_a_chain() {
    let mut fold = snapshot(vec![agent("booker")]);
    attach(
        &mut fold,
        &cache(vec![
            waiting_on("r", &["q-agent", "booker"], &["pr[x]"]),
            fired_launching("p", &["booker"], "wf_p", ago(500), &["p-agent"]),
            fired_launching("q", &["booker"], "wf_q", ago(900), &["q-agent"]),
        ]),
    );
    assert_eq!(
        tasks(&on_cards(&fold)["booker"]),
        [
            ("q", BookingBand::Fired),
            ("r", BookingBand::Next),
            ("p", BookingBand::Fired),
        ],
        "q's fire launched r's booker, though p shares the rest of the chain"
    );
}

/// A stopped booker keeps an unended row in the store and has no card: it owns nothing, so
/// its entry is adopted and follows the sibling whose fire launched it.
#[test]
fn adoption_follows_a_booker_that_lost_its_card_without_ending() {
    let mut fold = snapshot(vec![agent("booker"), agent("p-agent")]);
    let groups = Arc::make_mut(&mut fold.worktree_groups);
    Arc::make_mut(&mut groups[0].rows).retain(|row| row.id != "p-agent");
    attach(
        &mut fold,
        &cache(vec![
            waiting_on("r", &["p-agent", "booker"], &["pr[x]"]),
            fired_launching("p", &["booker"], "wf_p", ago(900), &["p-agent"]),
            fired_launching("q", &["booker"], "wf_q", ago(500), &["q-agent"]),
        ]),
    );
    assert_eq!(
        tasks(&on_cards(&fold)["booker"]),
        [
            ("p", BookingBand::Fired),
            ("r", BookingBand::Next),
            ("q", BookingBand::Fired),
        ]
    );
}

/// No recorded launch holds the booker: a peer the planner launched directly, or a booker
/// that ended before any pass saw it alive. The entry takes its ordinary place even where
/// one fired sibling's chain is the rest of its own.
#[test]
fn adoption_without_a_recorded_launch_takes_the_ordinary_place() {
    for (case, launches) in [
        ("a direct peer", &["s-agent"][..]),
        ("ended unseen", &[][..]),
    ] {
        let mut fold = snapshot(vec![agent("booker")]);
        attach(
            &mut fold,
            &cache(vec![
                waiting_on("r", &["peer", "booker"], &["pr[x]"]),
                fired_launching("s", &["booker"], "wf_s", ago(900), launches),
                fired_launching("t", &["booker", "root"], "wf_t", ago(500), &["t-agent"]),
            ]),
        );
        assert_eq!(
            tasks(&on_cards(&fold)["booker"]),
            [
                ("s", BookingBand::Fired),
                ("t", BookingBand::Fired),
                ("r", BookingBand::Next),
            ],
            "{case}: s's chain is the rest of r's, but s launched no `peer`"
        );
    }
}

#[test]
fn any_unmet_term_of_a_waiting_sibling_makes_an_entry_depended_on() {
    let mut fold = snapshot(vec![agent("booker")]);
    let ended_on = |task: &str| {
        let mut facts = fired(task, &["booker"], task, ago(100_000));
        facts.branch = Some(task.to_owned());
        facts.worktree = true;
        facts.fire.as_mut().unwrap().ended_at = Some(ago(80_000));
        facts
    };
    attach(
        &mut fold,
        &cache(vec![
            ended_on("a"),
            ended_on("b"),
            waiting_on("c", &["booker"], &["pr[b]", "pr[a]"]),
        ]),
    );
    let list = &on_cards(&fold)["booker"];
    let by_task = |task: &str| list.iter().find(|booking| booking.task == task).unwrap();
    for task in ["a", "b"] {
        let BookingState::Fired(fire) = &by_task(task).state else {
            panic!("fired");
        };
        assert!(fire.depended_on, "{task}: c waits on it");
        assert_eq!(
            by_task(task).band,
            BookingBand::Fired,
            "{task} stays listed"
        );
    }
    let BookingState::Waiting(wait) = &by_task("c").state else {
        panic!("waiting");
    };
    assert_eq!(
        wait.blocker
            .as_ref()
            .and_then(|blocker| blocker.after.as_ref())
            .map(|sibling| sibling.task.as_str()),
        Some("b"),
        "`after` still names the blocking term's sibling"
    );
}

#[test]
fn a_team_at_done_stays_finished_after_its_panes_close() {
    let mut fold = snapshot(vec![agent("booker")]);
    let team = |task: &str, done: bool| {
        let mut facts = fired(task, &["booker"], task, ago(1_000));
        facts.starts = BookingStarts::Team("forge".to_owned());
        facts.branch = Some(task.to_owned());
        facts.worktree = true;
        let fire = facts.fire.as_mut().unwrap();
        fire.ended_at = Some(ago(60));
        fire.team_done = Some(done);
        facts
    };
    attach(
        &mut fold,
        &cache(vec![team("done", true), team("closed-early", false)]),
    );
    let list = &on_cards(&fold)["booker"];
    let by_task = |task: &str| list.iter().find(|booking| booking.task == task).unwrap();
    assert!(
        by_task("done").is_finished(),
        "its board reached Done; no living rows"
    );
    assert!(
        !by_task("closed-early").is_finished(),
        "a team that closed before Done is not finished"
    );
}

#[test]
fn a_fire_with_nothing_launched_yet_and_no_end_is_starting() {
    let mut fold = snapshot(vec![agent("booker")]);
    let mut ended = fired("ended", &["booker"], "wf_e", ago(100_000));
    ended.fire.as_mut().unwrap().ended_at = Some(ago(90_000));
    attach(
        &mut fold,
        &cache(vec![
            fired("starting", &["booker"], "wf_s", ago(100_000)),
            ended,
        ]),
    );
    let list = &on_cards(&fold)["booker"];
    let by_task = |task: &str| list.iter().find(|booking| booking.task == task).unwrap();
    let starting = by_task("starting");
    let BookingState::Fired(fire) = &starting.state else {
        panic!("fired");
    };
    assert!(fire.starting, "no row and no end stamp: starting");
    assert!(
        !starting.is_finished(),
        "starting is not finished, treeless or not"
    );
    assert!(!starting.ended_without_pr());
    assert_eq!(starting.band, BookingBand::Fired, "starting is not older");
    let BookingState::Fired(fire) = &by_task("ended").state else {
        panic!("fired");
    };
    assert!(!fire.starting);
}

/// A sibling whose fire has no row and no agent-step end yet is starting in the `after`
/// text of the entry waiting on it, as on its own entry.
#[test]
fn a_sibling_still_starting_is_starting_in_the_after_text() {
    let mut fold = snapshot(vec![agent("booker")]);
    let mut up = fired("up", &["booker"], "wf_up", ago(100));
    up.branch = Some("up".to_owned());
    up.worktree = true;
    attach(
        &mut fold,
        &cache(vec![up, waiting_on("down", &["booker"], &["pr[up]"])]),
    );
    let list = &on_cards(&fold)["booker"];
    let BookingState::Waiting(wait) = &list[1].state else {
        panic!("down waits: {list:?}");
    };
    assert_eq!(
        wait.blocker
            .as_ref()
            .and_then(|blocker| blocker.after.clone()),
        Some(BookingSibling {
            task: "up".to_owned(),
            state: SiblingState::Starting,
        })
    );
}

#[test]
fn a_team_focus_tie_picks_the_pane_rimz_teams_focus_picks() {
    let member = |id: &str, role: &str, task: Option<&str>| AgentState {
        team: Some("forge".to_owned()),
        role: Some(role.to_owned()),
        status: AgentStatus::Idle,
        loop_task: task.map(str::to_owned),
        ..leader_of("lang", Some("wf_1"), ago(100), id)
    };
    let agents = vec![
        agent("booker"),
        member("coder", "coder", None),
        member("planner", "planner", Some("lang")),
    ];
    let mut fold = snapshot(agents.clone());
    let mut facts = fired("lang", &["booker"], "wf_1", ago(200));
    facts.starts = BookingStarts::Team("forge".to_owned());
    attach(&mut fold, &cache(vec![facts]));
    let cohort = crate::address::team_cohorts(fold.agents.iter())
        .into_iter()
        .next()
        .unwrap();
    let expected = cohort.focus_member(None, &HashMap::new(), now());
    let index = agents
        .iter()
        .position(|agent| agent.agent_id == expected.agent_id)
        .unwrap();
    let BookingState::Fired(fire) = &on_cards(&fold)["booker"][0].state else {
        panic!("fired");
    };
    assert_eq!(
        fire.live.as_ref().and_then(|live| live.pane.clone()),
        Some(PaneId::from_parts(
            MuxName::Zellij,
            format!("terminal_{index}")
        )),
        "equal scores and no leader role: the CLI's first member, {}",
        expected.agent_id
    );
}
