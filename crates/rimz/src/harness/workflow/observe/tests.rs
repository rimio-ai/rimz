use std::cell::Cell;
use std::time::Duration;

use super::*;
use crate::agents::{
    AgentContext, AgentStatus, AgentTurnError, AskRoute, BudgetPark, BudgetScope, BudgetWindow,
    OpenAsk, PermissionMode,
};
use crate::ids::{AgentKind, AskId, WorkspaceId};
use crate::store::run::{RunStatus, StoppedTurn};
use crate::transcript::AskQuestion;
use crate::{RuntimePaths, StatePaths};

fn now() -> Timestamp {
    Timestamp::from_second(1_750_000_000).unwrap()
}

fn ago(secs: u64) -> Timestamp {
    now() - Duration::from_secs(secs)
}

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let id = WorkspaceId::from_project_root(dir.path());
    let state = StatePaths::under(id.clone(), &dir.path().join("state")).unwrap();
    let runtime = RuntimePaths::under(id, &dir.path().join("runtime")).unwrap();
    (dir, Store::open(state, runtime).unwrap())
}

fn row(id: &str, registered: u64) -> AgentState {
    let mut agent = AgentState::seed(
        AgentKind::new_unchecked("claude"),
        id.into(),
        AgentStatus::Running,
        ago(60),
    );
    agent.name = Some(id.into());
    agent.registered_at = Some(ago(registered));
    agent.last_activity = ago(30);
    agent
}

fn child(id: &str, parent: &str, registered: u64) -> AgentState {
    let mut agent = row(id, registered);
    agent.parent_agent_id = Some(parent.into());
    agent.parent_agent_kind = Some(AgentKind::new_unchecked("claude"));
    agent.launch_depth = Some(1);
    agent
}

fn errored(mut agent: AgentState, secs: u64, class: TurnErrorClass) -> AgentState {
    agent.context = Some(AgentContext {
        turn_error: Some(AgentTurnError {
            ended_turn: false,
            class,
            at: ago(secs),
            label: Some("provider".into()),
        }),
        ..AgentContext::default()
    });
    agent
}

fn parked(mut agent: AgentState, secs: u64) -> AgentState {
    agent.budget_park = Some(BudgetPark {
        cap_usd: 1.0,
        spend_usd: 2.0,
        window: BudgetWindow::Day,
        at: ago(secs),
        scope: BudgetScope::default(),
        account_kind: None,
        resets_at: None,
        reset_window: None,
    });
    agent
}

fn ask(kind: AskKind, delivery: AskDelivery) -> OpenAskDetail {
    OpenAskDetail {
        open: OpenAsk {
            id: AskId::new(),
            kind,
            detail: None,
            extra_questions: 0,
            native_key: None,
            since: ago(20),
        },
        delivery,
        context: None,
        raised_by: None,
        questions: vec![AskQuestion {
            question: "which way?".into(),
            options: Vec::new(),
            multi_select: false,
            has_option_previews: false,
        }],
        route: AskRoute::Pane,
        pane_actions: None,
    }
}

fn leader_run(leader: &AgentState) -> RunRecord {
    let mut run = RunRecord::new(
        WorkspaceId::from_project_root(Path::new("/repo")),
        leader.kind.clone(),
        PermissionMode::Auto,
        "task".into(),
        "/repo".into(),
    );
    run.agent_id = Some(leader.agent_id.clone());
    run.status = RunStatus::Running;
    run.cost_usd = Some(1.5);
    run
}

fn snapshot(agents: Vec<AgentState>) -> SidebarSnapshot {
    SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(Path::new("/repo")),
        agents,
        now(),
    )
}

/// Observe with readers that answer per agent id and count their calls.
struct Readers<'a> {
    asks: &'a [(&'a str, OpenAskDetail)],
    resets: &'a [(&'a str, Timestamp)],
    ask_reads: Cell<Vec<String>>,
    reset_reads: Cell<Vec<String>>,
}

impl<'a> Readers<'a> {
    fn new(asks: &'a [(&'a str, OpenAskDetail)], resets: &'a [(&'a str, Timestamp)]) -> Self {
        Self {
            asks,
            resets,
            ask_reads: Cell::new(Vec::new()),
            reset_reads: Cell::new(Vec::new()),
        }
    }

    fn observe(&self, run: &RunRecord, snapshot: &SidebarSnapshot) -> Observed {
        let (_dir, store) = store();
        let result: Result<Observed, StoreErr> = observe(
            &store,
            run,
            snapshot,
            Observation::Attempt,
            None,
            |agent| {
                let mut reads = self.ask_reads.take();
                reads.push(agent.agent_id.to_string());
                self.ask_reads.set(reads);
                Ok(self
                    .asks
                    .iter()
                    .find(|(id, _)| *id == agent.agent_id.as_str())
                    .map(|(_, ask)| ask.clone()))
            },
            |agent| {
                let mut reads = self.reset_reads.take();
                reads.push(agent.agent_id.to_string());
                self.reset_reads.set(reads);
                self.resets
                    .iter()
                    .find(|(id, _)| *id == agent.agent_id.as_str())
                    .map(|(_, at)| *at)
            },
        );
        result.unwrap()
    }
}

#[test]
fn a_missing_leader_row_keeps_the_run_record_defaults() {
    let leader = row("leader", 50);
    let mut run = leader_run(&leader);
    run.stopped_turn = Some(StoppedTurn {
        at: ago(5),
        disposition: StoppedTurnDisposition::Failed,
    });
    let readers = Readers::new(&[], &[]);
    let observed = readers.observe(&run, &snapshot(vec![row("other", 40)]));
    assert_eq!(observed.status, RunStatus::Running);
    assert_eq!(observed.cost_usd, Some(1.5));
    assert_eq!(
        observed.stopped,
        Some(Stopped {
            since: ago(5),
            kind: StopKind::Error
        })
    );
    assert!(observed.ask.is_none() && observed.limited.is_none() && observed.turn.is_none());
    assert_eq!(observed.children, Children::default());
    assert!(readers.ask_reads.take().is_empty());
}

#[test]
fn the_stopped_turn_seeds_error_for_a_failure_and_interrupt_for_a_cancel() {
    let leader = row("leader", 50);
    for (disposition, kind) in [
        (StoppedTurnDisposition::Failed, StopKind::Error),
        (StoppedTurnDisposition::Canceled, StopKind::Interrupt),
    ] {
        let mut run = leader_run(&leader);
        run.stopped_turn = Some(StoppedTurn {
            at: ago(5),
            disposition,
        });
        let observed = Readers::new(&[], &[]).observe(&run, &snapshot(vec![leader.clone()]));
        assert_eq!(observed.stopped.map(|stopped| stopped.kind), Some(kind));
    }
}

#[test]
fn a_raised_or_async_ask_does_not_block_and_the_next_agent_is_read() {
    let leader = row("leader", 50);
    let run = leader_run(&leader);
    for delivery in [AskDelivery::Raised, AskDelivery::Async] {
        let asks = [
            ("leader", ask(AskKind::Question, delivery)),
            ("child", ask(AskKind::Permission, AskDelivery::Blocking)),
        ];
        let readers = Readers::new(&asks, &[]);
        let observed = readers.observe(
            &run,
            &snapshot(vec![leader.clone(), child("child", "leader", 40)]),
        );
        let asking = observed.ask.unwrap();
        assert_eq!(asking.ask.kind, "permission");
        assert_eq!(asking.ask.handle, "@child");
        assert_eq!(readers.ask_reads.take(), ["leader", "child"]);
    }
}

#[test]
fn each_ask_kind_has_its_label() {
    let leader = row("leader", 50);
    let run = leader_run(&leader);
    for (kind, label) in [
        (AskKind::Question, "question"),
        (AskKind::Permission, "permission"),
        (AskKind::PlanApproval, "plan_approval"),
    ] {
        let asks = [("leader", ask(kind, AskDelivery::Blocking))];
        let observed = Readers::new(&asks, &[]).observe(&run, &snapshot(vec![leader.clone()]));
        let asking = observed.ask.unwrap();
        assert_eq!(asking.ask.kind, label);
        assert_eq!(
            asking.since,
            ago(20),
            "a non-review ask counts from its open"
        );
        assert_eq!(
            asking.ask.handle, "@claude",
            "a lone agent is addressed by kind"
        );
    }
}

#[test]
fn ask_text_joins_the_context_then_each_question() {
    let leader = row("leader", 50);
    let mut detail = ask(AskKind::Question, AskDelivery::Blocking);
    detail.context = Some("Context line".into());
    let mut second = detail.questions[0].clone();
    second.question = "and then?".into();
    detail.questions.push(second);
    let asks = [("leader", detail)];
    let observed = Readers::new(&asks, &[]).observe(&leader_run(&leader), &snapshot(vec![leader]));
    assert_eq!(
        observed.ask.unwrap().ask.text,
        "Context line\n\nwhich way?\n\nand then?"
    );
}

#[test]
fn the_leader_ask_wins_and_no_ask_is_read_after_the_first_blocking_one() {
    let leader = row("leader", 50);
    let asks = [
        ("leader", ask(AskKind::Question, AskDelivery::Blocking)),
        ("child", ask(AskKind::Permission, AskDelivery::Blocking)),
    ];
    let readers = Readers::new(&asks, &[]);
    let observed = readers.observe(
        &leader_run(&leader),
        &snapshot(vec![leader, child("child", "leader", 40)]),
    );
    assert_eq!(observed.ask.unwrap().ask.handle, "@leader");
    assert_eq!(readers.ask_reads.take(), ["leader"]);
}

fn reviewing(mut agent: AgentState, opened: u64) -> AgentState {
    agent.kind = AgentKind::new_unchecked("codex");
    agent.status = AgentStatus::Waiting;
    agent.mode = Some(PermissionMode::Auto);
    agent.turn_started_at = Some(ago(100));
    agent.turn_ended_at = None;
    agent.last_activity = ago(opened + 1);
    agent.open_ask = Some(OpenAsk {
        id: AskId::new(),
        kind: AskKind::Permission,
        detail: Some("echo hi".into()),
        extra_questions: 0,
        native_key: Some("permission".into()),
        since: ago(opened),
    });
    agent
}

#[test]
fn a_reviewing_permission_is_never_read_and_counts_from_its_review_end_after() {
    let under_review = reviewing(row("leader", 50), 10);
    assert!(under_review.is_reviewing_permission(now()));
    let mut detail = ask(AskKind::Permission, AskDelivery::Blocking);
    detail.open = under_review.open_ask.clone().unwrap();
    let asks = [("leader", detail)];
    let readers = Readers::new(&asks, &[]);
    let mut run = leader_run(&under_review);
    run.kind = under_review.kind.clone();
    let observed = readers.observe(&run, &snapshot(vec![under_review]));
    assert!(observed.ask.is_none());
    assert!(readers.ask_reads.take().is_empty());

    let reviewed = reviewing(row("leader", 50), 120);
    let until = reviewed.permission_review_until().unwrap();
    assert!(until < now() && until != ago(120));
    let readers = Readers::new(&asks, &[]);
    let observed = readers.observe(&run, &snapshot(vec![reviewed]));
    assert_eq!(observed.ask.unwrap().since, until);
    assert_eq!(readers.ask_reads.take(), ["leader"]);
}

#[test]
fn rate_and_spend_limits_stop_as_limits_and_the_first_limited_agent_reads_its_reset() {
    for class in [
        TurnErrorClass::PausedRateLimit,
        TurnErrorClass::PausedSpendLimit,
    ] {
        let leader = errored(row("leader", 50), 10, class);
        let limited_child = errored(child("child", "leader", 40), 20, class);
        let resets = [("leader", ago(1)), ("child", ago(2))];
        let readers = Readers::new(&[], &resets);
        let observed =
            readers.observe(&leader_run(&leader), &snapshot(vec![leader, limited_child]));
        assert_eq!(
            observed.limited,
            Some(Limited {
                reset_at: Some(ago(1))
            })
        );
        assert_eq!(readers.reset_reads.take(), ["leader"]);
        assert_eq!(
            observed.stopped,
            Some(Stopped {
                since: ago(20),
                kind: StopKind::Limit
            }),
            "the earlier child limit is the stop"
        );
    }
}

#[test]
fn a_non_limit_error_stops_as_an_error_without_a_reset_read() {
    let leader = errored(row("leader", 50), 10, TurnErrorClass::Failed);
    let resets = [("leader", ago(1))];
    let readers = Readers::new(&[], &resets);
    let observed = readers.observe(&leader_run(&leader), &snapshot(vec![leader]));
    assert_eq!(
        observed.stopped,
        Some(Stopped {
            since: ago(10),
            kind: StopKind::Error
        })
    );
    assert!(observed.limited.is_none());
    assert!(readers.reset_reads.take().is_empty());
}

#[test]
fn stop_precedence_replaces_an_interrupt_and_keeps_the_earliest_then_the_first_scanned() {
    let stop = |since, kind| Some(Stopped { since, kind });
    let seeded = |disposition, at| {
        let leader = row("leader", 50);
        let mut run = leader_run(&leader);
        run.stopped_turn = Some(StoppedTurn { at, disposition });
        run
    };
    let observe = |run: &RunRecord, agents| Readers::new(&[], &[]).observe(run, &snapshot(agents));
    let failed = TurnErrorClass::Failed;
    // An interrupt seed is replaced even by a later stop.
    let run = seeded(StoppedTurnDisposition::Canceled, ago(50));
    let observed = observe(&run, vec![errored(row("leader", 50), 10, failed)]);
    assert_eq!(observed.stopped, stop(ago(10), StopKind::Error));
    // A strictly earlier stop replaces the seed; a later one does not.
    let run = seeded(StoppedTurnDisposition::Failed, ago(15));
    let observed = observe(&run, vec![parked(row("leader", 50), 20)]);
    assert_eq!(observed.stopped, stop(ago(20), StopKind::Budget));
    let observed = observe(&run, vec![parked(row("leader", 50), 10)]);
    assert_eq!(observed.stopped, stop(ago(15), StopKind::Error));
    // A tie keeps the seed, then the leader before its child.
    let observed = observe(&run, vec![parked(row("leader", 50), 15)]);
    assert_eq!(observed.stopped, stop(ago(15), StopKind::Error));
    let run = leader_run(&row("leader", 50));
    let observed = observe(
        &run,
        vec![
            parked(row("leader", 50), 15),
            errored(child("child", "leader", 40), 15, failed),
        ],
    );
    assert_eq!(observed.stopped, stop(ago(15), StopKind::Budget));
    // On one agent the error is read before the budget park, so a tie keeps the error.
    let observed = observe(
        &run,
        vec![parked(errored(row("leader", 50), 15, failed), 15)],
    );
    assert_eq!(observed.stopped, stop(ago(15), StopKind::Error));
}

#[test]
fn the_walk_skips_an_ended_child_and_never_reaches_its_live_children() {
    let leader = row("leader", 50);
    let mut ended = child("ended", "leader", 40);
    ended.ended_at = Some(ago(5));
    let mut grandchild = child("grandchild", "ended", 30);
    grandchild.launch_depth = Some(2);
    let asks = [("grandchild", ask(AskKind::Question, AskDelivery::Blocking))];
    let readers = Readers::new(&asks, &[]);
    let observed = readers.observe(
        &leader_run(&leader),
        &snapshot(vec![
            leader,
            ended,
            errored(grandchild, 5, TurnErrorClass::Failed),
        ]),
    );
    assert!(observed.ask.is_none() && observed.stopped.is_none());
    assert_eq!(readers.ask_reads.take(), ["leader"]);
    assert_eq!(observed.children, Children::default());
}

#[test]
fn live_descendants_are_scanned_depth_first_and_reported_stopped() {
    let leader = row("leader", 50);
    let mut grandchild = child("grandchild", "first", 30);
    grandchild.launch_depth = Some(2);
    let readers = Readers::new(&[], &[]);
    let observed = readers.observe(
        &leader_run(&leader),
        &snapshot(vec![
            leader,
            child("first", "leader", 45),
            child("second", "leader", 40),
            grandchild,
        ]),
    );
    assert_eq!(
        readers.ask_reads.take(),
        ["leader", "second", "first", "grandchild"]
    );
    let handles = |children: &[ChildRef]| {
        children
            .iter()
            .map(|child| child.handle.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        handles(&observed.children.stopped),
        ["@second", "@first", "@grandchild"]
    );
    assert!(observed.children.live.is_empty());
}

fn launched_by(mut agent: AgentState, launcher: &str) -> AgentState {
    agent.launched_by = Some(crate::agents::LaunchedBy {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: launcher.into(),
    });
    agent
}

fn seat(agent: AgentState, team: &str) -> AgentState {
    let mut agent = agent;
    agent.team = Some(team.into());
    agent
}

fn pinned(mut agent: AgentState, pin: &crate::agents::WorkflowAttemptRef) -> AgentState {
    agent.workflow = Some(pin.clone());
    agent
}

fn pin() -> crate::agents::WorkflowAttemptRef {
    crate::agents::WorkflowAttemptRef {
        run: crate::ids::WorkflowRunId::new(),
        seq: 1,
        role: crate::agents::WorkflowAttemptRole::Step,
    }
}

fn handles(children: &[ChildRef]) -> Vec<&str> {
    children.iter().map(|child| child.handle.as_str()).collect()
}

#[test]
fn children_keep_live_launches_and_launched_cohorts_once_without_ended_or_stopped_rows() {
    let leader = row("leader", 50);
    let mut ended = launched_by(row("ended-peer", 45), "leader");
    ended.ended_at = Some(ago(5));
    let lane = tempfile::tempdir().unwrap();
    let board = crate::harness::scratch::board_path(lane.path());
    std::fs::create_dir_all(board.parent().unwrap()).unwrap();
    std::fs::write(&board, "Stage: review (@seat)\n").unwrap();
    let mut staged = seat(launched_by(row("staged", 30), "leader"), "crew");
    staged.worktree_path = Some(lane.path().display().to_string());
    let observed = Readers::new(&[], &[]).observe(
        &leader_run(&leader),
        &snapshot(vec![
            leader,
            child("child", "leader", 48),
            launched_by(row("peer", 46), "leader"),
            ended,
            seat(launched_by(row("seat", 40), "leader"), "forge"),
            seat(row("mate", 35), "forge"),
            seat(row("stranger", 34), "other"),
            staged,
        ]),
    );
    let children = observed.children;
    assert_eq!(handles(&children.stopped), ["@child"]);
    let lane_name = lane.path().file_name().unwrap().to_str().unwrap();
    let staged_handle = format!("@claude#{lane_name}");
    assert_eq!(
        handles(&children.live),
        ["@peer", staged_handle.as_str(), "@seat", "@mate"],
        "the fleet by registration, then each launched cohort by team and channel"
    );
    let team = |handle: &str| {
        children
            .live
            .iter()
            .find(|child| child.handle == handle)
            .and_then(|child| child.team.clone())
    };
    assert_eq!(team("@peer"), None);
    assert_eq!(
        team("@seat"),
        Some(TeamRef {
            name: "forge".into(),
            channel: "external".into(),
            stage: None,
        }),
        "a seat with no lane reads the external channel and no stage"
    );
    let staged = team(&staged_handle).unwrap();
    assert_eq!(staged.stage.as_deref(), Some("review"));
    assert_eq!(staged.channel, lane_name);
}

#[test]
fn a_stop_takes_pinned_and_detached_group_roots_and_reports_the_rest_live() {
    let pin = pin();
    let mut leader = pinned(row("leader", 50), &pin);
    leader.launch_group = Some("group".into());
    let mut run = leader_run(&leader);
    run.workflow = Some(pin.clone());
    let mut groupmate = row("groupmate", 44);
    groupmate.launch_group = Some("group".into());
    let mut ended_companion = pinned(row("gone", 43), &pin);
    ended_companion.ended_at = Some(ago(5));
    let agents = vec![
        leader.clone(),
        child("child", "leader", 49),
        pinned(launched_by(row("companion", 48), "leader"), &pin),
        launched_by(row("peer", 47), "companion"),
        child("grandchild", "companion", 46),
        groupmate,
        ended_companion,
        row("unrelated", 42),
    ];
    let room = snapshot(agents);
    let cohort = stop_cohort(&room, &run).unwrap();
    let roots = cohort
        .agents
        .iter()
        .map(|agent| agent.agent_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(roots, ["leader", "companion", "groupmate"]);
    assert_eq!(
        handles(&cohort.children.stopped),
        ["@child", "@grandchild", "@companion", "@groupmate"],
        "every root's tree and every root but the leader stop, each once"
    );
    assert_eq!(
        handles(&cohort.children.live),
        ["@peer"],
        "a stopped companion leaves the leader's live list"
    );

    let mut unpinned = run.clone();
    unpinned.workflow = None;
    assert!(stop_cohort(&room, &unpinned).is_none());
    let mut ended = leader;
    ended.ended_at = Some(ago(1));
    assert!(stop_cohort(&snapshot(vec![ended]), &run).is_none());
}

#[test]
fn a_detach_releases_every_pinned_row_and_reports_live_ones_but_the_leader() {
    let pin = pin();
    let leader = pinned(row("leader", 50), &pin);
    let mut run = leader_run(&leader);
    run.workflow = Some(pin.clone());
    let mut gone = pinned(row("gone", 40), &pin);
    gone.ended_at = Some(ago(5));
    let agents = vec![
        leader.clone(),
        pinned(launched_by(row("companion", 48), "leader"), &pin),
        launched_by(row("peer", 47), "companion"),
        child("child", "leader", 46),
        gone,
        row("unrelated", 42),
    ];
    let room = snapshot(agents.clone());
    let cohort = detach_cohort(&room, agents.iter(), &run);
    let released = cohort
        .agents
        .iter()
        .map(|agent| agent.agent_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(released, ["leader", "companion", "gone"]);
    assert!(cohort.children.stopped.is_empty());
    assert_eq!(
        handles(&cohort.children.live),
        ["@companion", "@child", "@peer"],
        "each live child once, the leader never"
    );
    run.workflow = None;
    let cohort = detach_cohort(&room, agents.iter(), &run);
    assert!(cohort.agents.is_empty() && cohort.children == Children::default());
}

#[test]
fn an_excluded_handle_leaves_with_its_live_launched_tree() {
    let room = snapshot(vec![
        row("leader", 50),
        child("child", "leader", 48),
        launched_by(row("peer", 46), "leader"),
        row("other", 40),
    ]);
    let ids = |snapshot: &SidebarSnapshot| {
        snapshot
            .agents
            .iter()
            .map(|agent| agent.agent_id.to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&without_excluded(room.clone(), &[])), ids(&room));
    let kept = without_excluded(room, &["@leader".into(), "@missing".into()]);
    assert_eq!(ids(&kept), ["peer", "other"]);
}
