use super::*;
use crate::agents::AgentStatus;
use crate::ids::WorkspaceId;
use crate::pane::RuntimeOwnerKind;
use crate::sidebar::produce::test_support;
use crate::store::runtime::{AgentLiveness, agent_liveness};
use jiff::Timestamp;

struct Fixture {
    _dir: tempfile::TempDir,
    runtime: RuntimePaths,
    workspace: WorkspaceId,
    now: Timestamp,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("runtime root");
        let workspace = WorkspaceId::from_project_root(std::path::Path::new("/repo/main"));
        let runtime = RuntimePaths::under(workspace.clone(), dir.path()).expect("runtime");
        Self {
            _dir: dir,
            runtime,
            workspace,
            now: Timestamp::now(),
        }
    }

    fn agent(&self, kind: &str, id: &str, pane: Option<&str>) -> AgentState {
        let mut agent = AgentState {
            status: AgentStatus::Running,
            ..crate::testkit::agent_state(kind, id, self.now)
        };
        agent.pane = pane.map(|pane| test_support::pane(pane, None, Some("/repo/main")));
        agent.worktree_path = Some("/repo/main".to_owned());
        agent
    }

    fn read(&self, agents: Vec<AgentState>, listed: Vec<crate::pane::PaneRef>) -> HostingRead {
        let snapshot = SidebarSnapshot::build_with_agents(self.workspace.clone(), agents, self.now);
        let frame = assemble_frame(listed, unix_now_ms(), "room".to_owned());
        fold(MuxName::Zellij, Vec::new(), snapshot, frame, &self.runtime)
    }
}

fn key(kind: &str, id: &str) -> (AgentKind, AgentSessionId) {
    (AgentKind::new_unchecked(kind), id.into())
}

#[test]
fn a_listed_pane_hosts_its_agent_and_a_stored_pane_absent_from_the_listing_hosts_nothing() {
    let fixture = Fixture::new();
    let listed = fixture.agent("claude", "listed", Some("terminal_1"));
    let gone = fixture.agent("claude", "gone", Some("terminal_2"));
    let read = fixture.read(
        vec![listed, gone],
        vec![test_support::pane(
            "terminal_1",
            Some("claude"),
            Some("/repo/main"),
        )],
    );
    let (kind, id) = key("claude", "listed");
    assert_eq!(
        read.session_hosting(&kind, &id),
        Hosting::Pane {
            pane_id: PaneId::from_parts(MuxName::Zellij, "terminal_1"),
            session_name: "room".to_owned(),
        }
    );
    let (kind, id) = key("claude", "gone");
    assert_eq!(read.session_hosting(&kind, &id), Hosting::Nowhere);
    assert!(read.agent(&kind, &id).is_some(), "the row is still read");
}

#[test]
fn a_known_dead_process_is_hosted_nowhere_while_its_pane_is_listed() {
    let fixture = Fixture::new();
    let mut dead = fixture.agent("claude", "dead", Some("terminal_1"));
    let mut owner =
        crate::store::runtime::process_owner(RuntimeOwnerKind::Agent, "dead", std::process::id());
    owner.process_start = Some("stale-start-token".to_owned());
    dead.runtime_owner = Some(owner);
    let read = fixture.read(
        vec![dead],
        vec![test_support::pane("terminal_1", None, Some("/repo/main"))],
    );
    let (kind, id) = key("claude", "dead");
    assert_eq!(
        agent_liveness(read.agent(&kind, &id).unwrap()),
        AgentLiveness::Dead
    );
    assert_eq!(read.session_hosting(&kind, &id), Hosting::Nowhere);
}

/// A listed pane with no program evidence whose root process is this test
/// process, which [`owned`] rows record as their owner.
fn rooted_in_this_process(id: &str) -> crate::pane::PaneRef {
    crate::pane::PaneRef {
        pane_pid: Some(std::process::id()),
        ..test_support::pane(id, None, Some("/repo/main"))
    }
}

fn owned(mut agent: AgentState, start: Option<&str>) -> AgentState {
    let mut owner = crate::store::runtime::process_owner(
        RuntimeOwnerKind::Agent,
        agent.agent_id.as_str(),
        std::process::id(),
    );
    if let Some(start) = start {
        owner.process_start = Some(start.to_owned());
    }
    agent.runtime_owner = Some(owner);
    agent
}

#[test]
fn a_dead_owners_open_turn_never_takes_the_pane_of_the_live_row_beside_it() {
    let fixture = Fixture::new();
    let dead = owned(
        fixture.agent("claude", "dead", Some("terminal_1")),
        Some("stale-start-token"),
    );
    let mut live = owned(fixture.agent("claude", "live", Some("terminal_1")), None);
    live.status = AgentStatus::Idle;
    let read = fixture.read(vec![dead, live], vec![rooted_in_this_process("terminal_1")]);
    let (kind, id) = key("claude", "live");
    assert!(
        matches!(read.session_hosting(&kind, &id), Hosting::Pane { .. }),
        "{:?}",
        read.session_hosting(&kind, &id)
    );
    let (kind, id) = key("claude", "dead");
    assert_eq!(read.session_hosting(&kind, &id), Hosting::Nowhere);
    assert!(
        read.agent(&kind, &id).is_some(),
        "the dead row is still read"
    );
}

#[test]
fn an_ended_row_with_later_activity_never_takes_the_pane_of_an_unended_one() {
    let fixture = Fixture::new();
    let mut ended = owned(fixture.agent("claude", "ended", Some("terminal_1")), None);
    ended.status = AgentStatus::Idle;
    ended.last_activity = fixture.now;
    ended.ended_at = Some(fixture.now);
    let mut unended = owned(fixture.agent("claude", "unended", Some("terminal_1")), None);
    unended.status = AgentStatus::Idle;
    unended.last_activity = fixture.now - jiff::SignedDuration::from_secs(60);
    let read = fixture.read(
        vec![ended, unended],
        vec![rooted_in_this_process("terminal_1")],
    );
    let (kind, id) = key("claude", "unended");
    assert!(
        matches!(read.session_hosting(&kind, &id), Hosting::Pane { .. }),
        "{:?}",
        read.session_hosting(&kind, &id)
    );
    let (kind, id) = key("claude", "ended");
    assert_eq!(read.session_hosting(&kind, &id), Hosting::Nowhere);
    assert!(
        read.agent(&kind, &id).is_some(),
        "the ended row is still read"
    );
}

#[test]
fn the_pick_is_by_exact_session_and_never_by_a_shared_name() {
    let fixture = Fixture::new();
    let mut held = fixture.agent("claude", "held", Some("terminal_1"));
    held.name = Some("pet".to_owned());
    let mut namesake = fixture.agent("claude", "namesake", None);
    namesake.name = Some("pet".to_owned());
    let read = fixture.read(
        vec![held, namesake],
        vec![test_support::pane("terminal_1", None, Some("/repo/main"))],
    );
    let (kind, id) = key("claude", "namesake");
    assert_eq!(read.session_hosting(&kind, &id), Hosting::Nowhere);
}

#[test]
fn a_lazy_provider_binds_through_its_hosted_process_evidence() {
    let fixture = Fixture::new();
    let mut agent = fixture.agent("codex", "lazy", Some("terminal_1"));
    agent.last_activity = fixture.now;
    let bare = test_support::pane("terminal_1", None, Some("/repo/main"));
    let mut evidenced = bare.clone();
    evidenced.hosted_agent_kind = Some(AgentKind::new_unchecked("codex"));
    let (kind, id) = key("codex", "lazy");
    let read = fixture.read(vec![agent.clone()], vec![bare]);
    assert_eq!(read.session_hosting(&kind, &id), Hosting::Nowhere);
    let read = fixture.read(vec![agent], vec![evidenced]);
    assert!(matches!(
        read.session_hosting(&kind, &id),
        Hosting::Pane { .. }
    ));
}

#[test]
fn a_disproven_owner_is_hosted_nowhere_for_its_agent_and_its_session() {
    let fixture = Fixture::new();
    let mut agent = owned(
        fixture.agent("claude", "elsewhere", Some("terminal_1")),
        None,
    );
    agent.launch_id = Some("launch_agent".into());
    let owner = agent.runtime_owner.as_ref().expect("owned row");
    assert!(!owner.ancestors.is_empty(), "the owner records its chain");
    let mut pane = test_support::pane("terminal_1", Some("claude"), Some("/repo/main"));
    pane.pane_pid = Some(u32::MAX);
    let read = fixture.read(vec![agent.clone()], vec![pane.clone()]);
    let kind = AgentKind::new_unchecked("claude");
    assert_eq!(
        read.session_hosting(&kind, &"elsewhere".into()),
        Hosting::Nowhere
    );
    assert_eq!(
        read.agent_hosting(&kind, &"launch_agent".into()).hosting,
        Hosting::Nowhere
    );
    pane.pane_pid = Some(std::process::id());
    let read = fixture.read(vec![agent], vec![pane]);
    assert_eq!(
        read.session_hosting(&kind, &"elsewhere".into()),
        pane_of("terminal_1"),
        "the same row binds once the pane's root is on its chain"
    );
}

fn pane_of(id: &str) -> Hosting {
    Hosting::Pane {
        pane_id: PaneId::from_parts(MuxName::Zellij, id),
        session_name: "room".to_owned(),
    }
}

/// Panes running Claude, the program an unowned Claude row binds by.
fn listed(ids: &[&str]) -> Vec<crate::pane::PaneRef> {
    ids.iter()
        .map(|id| test_support::pane(id, Some("claude"), Some("/repo/main")))
        .collect()
}

fn launch_session(
    fixture: &Fixture,
    kind: &str,
    id: &str,
    epoch: u32,
    pane: Option<&str>,
) -> AgentState {
    let mut agent = fixture.agent(kind, id, pane);
    agent.launch_id = Some("launch_agent".into());
    agent.epoch = epoch;
    agent
}

fn holder_id(answer: &AgentHosting<'_>) -> Option<String> {
    answer.holder.map(|holder| holder.agent_id.to_string())
}

#[test]
fn a_swap_with_both_panes_listed_answers_the_holder_for_the_agent_and_each_session_its_own() {
    let fixture = Fixture::new();
    let old = launch_session(&fixture, "claude", "sess-old", 0, Some("terminal_1"));
    let new = launch_session(&fixture, "claude", "sess-new", 1, Some("terminal_2"));
    let read = fixture.read(vec![old, new], listed(&["terminal_1", "terminal_2"]));
    let kind = AgentKind::new_unchecked("claude");
    let answer = read.agent_hosting(&kind, &"launch_agent".into());
    assert_eq!(holder_id(&answer), Some("sess-new".to_owned()));
    assert_eq!(answer.hosting, pane_of("terminal_2"));
    assert_eq!(
        read.session_hosting(&kind, &"sess-old".into()),
        pane_of("terminal_1"),
        "the replaced session still answers its own pane"
    );
    assert_eq!(
        read.session_hosting(&kind, &"sess-new".into()),
        pane_of("terminal_2")
    );
}

#[test]
fn a_holder_of_another_kind_answers_for_the_agent_asked_by_its_old_kind() {
    let fixture = Fixture::new();
    let mut old = launch_session(&fixture, "codex", "sess-codex", 0, None);
    old.ended_at = Some(fixture.now);
    let molted = launch_session(&fixture, "claude", "sess-claude", 1, Some("terminal_1"));
    let read = fixture.read(vec![old, molted], listed(&["terminal_1"]));
    let answer = read.agent_hosting(&AgentKind::new_unchecked("codex"), &"launch_agent".into());
    assert_eq!(holder_id(&answer), Some("sess-claude".to_owned()));
    assert_eq!(
        answer.holder.map(|holder| holder.kind.as_str()),
        Some("claude")
    );
    assert_eq!(answer.hosting, pane_of("terminal_1"));
}

#[test]
fn an_agent_without_a_launch_id_is_asked_by_its_session_id() {
    let fixture = Fixture::new();
    let solo = fixture.agent("claude", "solo", Some("terminal_1"));
    let read = fixture.read(vec![solo], listed(&["terminal_1"]));
    let answer = read.agent_hosting(&AgentKind::new_unchecked("claude"), &"solo".into());
    assert_eq!(holder_id(&answer), Some("solo".to_owned()));
    assert_eq!(answer.hosting, pane_of("terminal_1"));
}

#[test]
fn two_unended_holders_answer_the_launch_row_for_the_agent_and_each_session_its_own() {
    let fixture = Fixture::new();
    let first = launch_session(&fixture, "claude", "sess-a", 0, Some("terminal_1"));
    let second = launch_session(&fixture, "claude", "sess-b", 0, Some("terminal_2"));
    let read = fixture.read(vec![first, second], listed(&["terminal_1", "terminal_2"]));
    let kind = AgentKind::new_unchecked("claude");
    let launch = "launch_agent".into();
    let picked = crate::address::launch_row(read.agents(), &kind, &launch)
        .expect("the resolver picks one of the two")
        .agent_id
        .to_string();
    let answer = read.agent_hosting(&kind, &launch);
    assert_eq!(holder_id(&answer), Some(picked.clone()));
    let own = |id: &str| {
        if id == "sess-a" {
            "terminal_1"
        } else {
            "terminal_2"
        }
    };
    assert_eq!(answer.hosting, pane_of(own(&picked)));
    for id in ["sess-a", "sess-b"] {
        assert_eq!(read.session_hosting(&kind, &id.into()), pane_of(own(id)));
    }
}

#[test]
fn two_live_holders_are_ordered_by_their_rest_certificates_as_the_resolver_orders_them() {
    let fixture = Fixture::new();
    // Claude keeps the primary: between two holders with open turns, the earlier
    // registration wins, so without certificates `sess-a` would answer.
    let mut first = launch_session(&fixture, "claude", "sess-a", 0, Some("terminal_1"));
    first.registered_at = Some(fixture.now - jiff::SignedDuration::from_secs(20));
    let mut second = launch_session(&fixture, "claude", "sess-b", 0, Some("terminal_2"));
    second.registered_at = Some(fixture.now - jiff::SignedDuration::from_secs(10));
    let error_at = fixture.now + jiff::SignedDuration::from_secs(1);
    let rested = crate::agents::AgentContext {
        turn_error: Some(crate::agents::AgentTurnError {
            model: None,
            ended_turn: false,
            class: crate::agents::TurnErrorClass::PausedOverloaded,
            at: error_at,
            label: Some("server_overloaded".to_owned()),
            refill_at: None,
        }),
        ..crate::agents::AgentContext::new("claude", error_at)
    };
    crate::store::agent_context::write(&fixture.runtime, "claude", "sess-a", &rested, None, false)
        .expect("context sidecar");
    let read = fixture.read(vec![first, second], listed(&["terminal_1", "terminal_2"]));
    let answer = read.agent_hosting(&AgentKind::new_unchecked("claude"), &"launch_agent".into());
    assert_eq!(
        holder_id(&answer),
        Some("sess-b".to_owned()),
        "a holder whose turn rests on an error yields to the one holding an open turn"
    );
    assert_eq!(answer.hosting, pane_of("terminal_2"));
}

#[test]
fn a_swap_reservation_holds_the_agent_once_bound_and_alone_is_hosted_nowhere() {
    let fixture = Fixture::new();
    let standing = launch_session(&fixture, "claude", "sess-old", 0, Some("terminal_1"));
    let mut reservation = launch_session(&fixture, "claude", "launch_swap", 1, None);
    let read = fixture.read(
        vec![standing.clone(), reservation.clone()],
        listed(&["terminal_1"]),
    );
    let kind = AgentKind::new_unchecked("claude");
    let answer = read.agent_hosting(&kind, &"launch_agent".into());
    assert_eq!(holder_id(&answer), Some("sess-old".to_owned()));
    assert_eq!(answer.hosting, pane_of("terminal_1"));

    reservation.launch_bound = true;
    reservation.pane = Some(test_support::pane("terminal_2", None, Some("/repo/main")));
    let read = fixture.read(
        vec![standing, reservation],
        listed(&["terminal_1", "terminal_2"]),
    );
    let answer = read.agent_hosting(&kind, &"launch_agent".into());
    assert_eq!(holder_id(&answer), Some("launch_swap".to_owned()));
    assert_eq!(answer.hosting, pane_of("terminal_2"));

    let mut alone = fixture.agent("claude", "launch_first", None);
    alone.launch_id = Some("launch_first".into());
    let read = fixture.read(
        vec![alone],
        vec![test_support::pane("terminal_1", None, Some("/repo/main"))],
    );
    let answer = read.agent_hosting(&kind, &"launch_first".into());
    assert_eq!(holder_id(&answer), Some("launch_first".to_owned()));
    assert_eq!(answer.hosting, Hosting::Nowhere);
}

#[test]
fn a_compaction_successor_in_its_predecessors_pane_answers_for_the_agent() {
    let fixture = Fixture::new();
    let mut predecessor = owned(
        launch_session(&fixture, "codex", "sess-before", 0, Some("terminal_1")),
        None,
    );
    predecessor.ended_at = Some(fixture.now);
    let mut successor = owned(
        launch_session(&fixture, "codex", "sess-after", 0, Some("terminal_1")),
        None,
    );
    successor.runtime_owner = predecessor.runtime_owner.clone();
    let mut pane = test_support::pane("terminal_1", None, Some("/repo/main"));
    pane.hosted_agent_kind = Some(AgentKind::new_unchecked("codex"));
    let read = fixture.read(vec![predecessor, successor], vec![pane]);
    let kind = AgentKind::new_unchecked("codex");
    let answer = read.agent_hosting(&kind, &"launch_agent".into());
    assert_eq!(holder_id(&answer), Some("sess-after".to_owned()));
    assert_eq!(answer.hosting, pane_of("terminal_1"));
    assert_eq!(
        read.session_hosting(&kind, &"sess-before".into()),
        Hosting::Nowhere
    );
}

#[test]
fn an_ended_agent_and_an_unknown_launch_are_hosted_nowhere() {
    let fixture = Fixture::new();
    let mut ended = launch_session(&fixture, "claude", "sess-ended", 0, Some("terminal_1"));
    ended.ended_at = Some(fixture.now);
    let read = fixture.read(vec![ended], listed(&["terminal_1"]));
    let kind = AgentKind::new_unchecked("claude");
    let answer = read.agent_hosting(&kind, &"launch_agent".into());
    assert_eq!(holder_id(&answer), Some("sess-ended".to_owned()));
    assert_eq!(answer.hosting, Hosting::Nowhere);
    let unknown = read.agent_hosting(&kind, &"launch_unknown".into());
    assert_eq!(
        unknown,
        AgentHosting {
            holder: None,
            hosting: Hosting::Nowhere,
        }
    );
}

fn asked_by_session(read: &HostingRead, kind: &str, id: &str) -> (Option<String>, Hosting) {
    let answer = read.agent_hosting_of_session(&AgentKind::new_unchecked(kind), &id.into());
    (holder_id(&answer), answer.hosting)
}

#[test]
fn a_session_that_holds_its_agent_answers_itself_even_beside_a_live_occupant() {
    let fixture = Fixture::new();
    let mut first = launch_session(&fixture, "claude", "sess-a", 0, Some("terminal_1"));
    first.registered_at = Some(fixture.now - jiff::SignedDuration::from_secs(20));
    let mut second = launch_session(&fixture, "claude", "sess-b", 0, Some("terminal_2"));
    second.registered_at = Some(fixture.now - jiff::SignedDuration::from_secs(10));
    let read = fixture.read(vec![first, second], listed(&["terminal_1", "terminal_2"]));
    assert_eq!(
        holder_id(&read.agent_hosting(&AgentKind::new_unchecked("claude"), &"launch_agent".into())),
        Some("sess-a".to_owned()),
        "the earlier registration occupies the launch"
    );
    assert_eq!(
        asked_by_session(&read, "claude", "sess-b"),
        (Some("sess-b".to_owned()), pane_of("terminal_2")),
        "the non-occupant answers itself, not the occupant's pane"
    );
    assert_eq!(
        asked_by_session(&read, "claude", "sess-a"),
        (Some("sess-a".to_owned()), pane_of("terminal_1"))
    );
}

#[test]
fn a_replaced_session_answers_its_launchs_holder_of_any_kind() {
    let fixture = Fixture::new();
    let old = launch_session(&fixture, "claude", "sess-old", 0, Some("terminal_1"));
    let new = launch_session(&fixture, "claude", "sess-new", 1, Some("terminal_2"));
    let read = fixture.read(vec![old, new], listed(&["terminal_1", "terminal_2"]));
    assert_eq!(
        asked_by_session(&read, "claude", "sess-old"),
        (Some("sess-new".to_owned()), pane_of("terminal_2"))
    );

    let mut codex = launch_session(&fixture, "codex", "sess-codex", 0, None);
    codex.ended_at = Some(fixture.now);
    let molted = launch_session(&fixture, "claude", "sess-claude", 1, Some("terminal_1"));
    let read = fixture.read(vec![codex, molted], listed(&["terminal_1"]));
    let answer =
        read.agent_hosting_of_session(&AgentKind::new_unchecked("codex"), &"sess-codex".into());
    assert_eq!(
        answer
            .holder
            .map(|holder| (holder.kind.as_str(), holder.agent_id.as_str())),
        Some(("claude", "sess-claude"))
    );
    assert_eq!(answer.hosting, pane_of("terminal_1"));
}

#[test]
fn an_id_with_no_row_answers_the_adopting_row_or_no_holder() {
    let fixture = Fixture::new();
    let mut adopted = fixture.agent("claude", "sess-registered", Some("terminal_1"));
    adopted.launch_id = Some("launch_first".into());
    let read = fixture.read(vec![adopted], listed(&["terminal_1"]));
    assert_eq!(
        asked_by_session(&read, "claude", "launch_first"),
        (Some("sess-registered".to_owned()), pane_of("terminal_1"))
    );
    for id in ["sess-unknown", "zellij:terminal_1"] {
        assert_eq!(
            asked_by_session(&read, "claude", id),
            (None, Hosting::Nowhere),
            "{id}"
        );
    }
}

#[test]
fn an_ended_session_answers_itself_hosted_nowhere() {
    let fixture = Fixture::new();
    let mut ended = fixture.agent("claude", "sess-ended", Some("terminal_1"));
    ended.ended_at = Some(fixture.now);
    let read = fixture.read(vec![ended], listed(&["terminal_1"]));
    assert_eq!(
        asked_by_session(&read, "claude", "sess-ended"),
        (Some("sess-ended".to_owned()), Hosting::Nowhere)
    );
}

/// A card's stored row with no pane of its own is still found where its process runs: the
/// sidebar close stops it there rather than closing the card's pane.
#[test]
fn a_paneless_session_answers_the_pane_its_hosted_process_binds() {
    let fixture = Fixture::new();
    let mut agent = fixture.agent("codex", "lazy", None);
    agent.last_activity = fixture.now;
    let mut evidenced = test_support::pane("terminal_1", None, Some("/repo/main"));
    evidenced.hosted_agent_kind = Some(AgentKind::new_unchecked("codex"));
    let read = fixture.read(vec![agent], vec![evidenced]);
    assert_eq!(
        asked_by_session(&read, "codex", "lazy"),
        (Some("lazy".to_owned()), pane_of("terminal_1"))
    );
}

#[test]
fn an_absent_session_hosts_nothing_and_any_other_listing_failure_is_an_error() {
    let absent = absent_session_as_empty(
        Err(ProduceErr::PaneDiscovery(
            crate::mux::MuxErr::SessionNotFound {
                session: "room".to_owned(),
            },
        )),
        "room",
    )
    .expect("an absent session is an answer");
    assert_eq!(absent.pane_states().count(), 0);
    let failed = absent_session_as_empty(
        Err(ProduceErr::PaneDiscovery(crate::mux::MuxErr::Timeout {
            program: "zellij".to_owned(),
            args: "pane query".to_owned(),
            timeout: std::time::Duration::from_secs(1),
        })),
        "room",
    );
    assert!(matches!(failed, Err(ProduceErr::PaneDiscovery(_))));
}

/// A pane of `tab`: the sidebar, a tiled pane, or a floating one.
fn in_tab(id: &str, tab: &str, command: &str, is_floating: bool) -> crate::pane::PaneRef {
    let mut pane = test_support::pane(id, Some(command), Some("/repo/main"));
    pane.view_id = Some(tab.to_owned());
    pane.is_floating = is_floating;
    pane
}

#[test]
fn a_tab_closes_only_beside_another_with_every_working_pane_a_target_and_no_caller_in_it() {
    let fixture = Fixture::new();
    let sidebar = crate::pane::SIDEBAR_CHROME_TITLE;
    let id = |raw: &str| PaneId::from_parts(MuxName::Zellij, raw);
    let tab = vec![
        in_tab("terminal_1", "tab_1", sidebar, false),
        in_tab("terminal_7", "tab_1", "claude", false),
        in_tab("terminal_8", "tab_1", "sh", false),
        in_tab("terminal_11", "tab_1", "sh", true),
    ];
    let mut beside = tab.clone();
    beside.push(in_tab("terminal_9", "tab_2", "claude", false));
    let read = fixture.read(Vec::new(), beside.clone());
    let both = [id("terminal_7"), id("terminal_8")];
    assert_eq!(read.tab_anchor(&both, None), Some(&both[0]));
    assert_eq!(
        read.tab_anchor(&both[..1], None),
        None,
        "terminal_8 is a working pane no target names"
    );
    assert_eq!(
        read.tab_anchor(&both, Some(&id("terminal_9"))),
        Some(&both[0])
    );
    assert_eq!(read.tab_anchor(&both, Some(&id("terminal_11"))), None);

    let mut last = beside;
    last.retain(|pane| pane.pane_id != id("terminal_8"));
    let read = fixture.read(Vec::new(), last);
    assert!(read.closing_empties_tab(&id("terminal_7"), None));
    assert!(!read.closing_empties_tab(&id("terminal_7"), Some(&id("terminal_1"))));

    let read = fixture.read(Vec::new(), tab.clone());
    assert_eq!(
        read.tab_anchor(&both, None),
        None,
        "the session's only tab is never closed"
    );
    let mut sole = tab;
    sole.retain(|pane| pane.pane_id != id("terminal_8"));
    let read = fixture.read(Vec::new(), sole);
    assert!(!read.closing_empties_tab(&id("terminal_7"), None));
}

fn owner_id(read: &HostingRead, pane: &str) -> Result<Option<String>, PaneUnlisted> {
    read.pane_owner(&PaneId::from_parts(MuxName::Zellij, pane))
        .map(|owner| owner.map(|agent| agent.agent_id.to_string()))
}

#[test]
fn a_pane_owner_is_the_bound_session_a_raw_pane_or_a_refusal() {
    let fixture = Fixture::new();
    let bound = fixture.agent("claude", "bound", Some("terminal_1"));
    let read = fixture.read(
        vec![bound],
        vec![
            test_support::pane("terminal_1", Some("claude"), Some("/repo/main")),
            test_support::pane("terminal_2", Some("zsh"), Some("/repo/main")),
        ],
    );
    assert_eq!(owner_id(&read, "terminal_1"), Ok(Some("bound".to_owned())));
    assert_eq!(owner_id(&read, "terminal_2"), Ok(None));
    let refusal = owner_id(&read, "terminal_9").unwrap_err();
    assert_eq!(
        refusal.to_string(),
        "pane zellij:terminal_9 is not in session room"
    );
}

#[test]
fn a_launched_childs_pane_is_owned_by_the_child() {
    let fixture = Fixture::new();
    let parent = fixture.agent("claude", "root", Some("terminal_1"));
    let mut child = fixture.agent("claude", "kid", Some("terminal_2"));
    child.parent_agent_id = Some("root".into());
    child.launch_depth = Some(1);
    let read = fixture.read(vec![parent, child], listed(&["terminal_1", "terminal_2"]));
    assert_eq!(owner_id(&read, "terminal_2"), Ok(Some("kid".to_owned())));
    assert_eq!(owner_id(&read, "terminal_1"), Ok(Some("root".to_owned())));
}

#[test]
fn a_pane_owner_follows_a_swap_to_the_launch_holder() {
    let fixture = Fixture::new();
    let old = launch_session(&fixture, "claude", "sess-old", 0, Some("terminal_1"));
    let new = launch_session(&fixture, "claude", "sess-new", 1, Some("terminal_2"));
    let read = fixture.read(vec![old, new], listed(&["terminal_1", "terminal_2"]));
    assert_eq!(
        owner_id(&read, "terminal_1"),
        Ok(Some("sess-new".to_owned()))
    );
}

/// The proven race: a reopen binds its agent to a new pane while a sidebar view still folds
/// that pane without the binding, as a sessionless card keyed by the pane id. The close's
/// own read resolves the bound agent, so it stops and records it rather than closing raw.
#[test]
fn a_pane_the_store_binds_resolves_its_agent_whatever_a_stale_view_drew() {
    let fixture = Fixture::new();
    let pane = listed(&["terminal_1"]);
    let stale = fixture.read(Vec::new(), pane.clone());
    assert!(
        stale
            .snapshot
            .agent_panes
            .iter()
            .any(|bound| bound.pane_id.to_string() == "zellij:terminal_1"
                && bound.agent_id.is_none()),
        "{:?}",
        stale.snapshot.agent_panes
    );
    let (kind, pane_key) = key("claude", "zellij:terminal_1");
    assert!(
        stale
            .agent_hosting_of_session(&kind, &pane_key)
            .holder
            .is_none(),
        "a card keyed by its pane id names no session"
    );
    assert_eq!(owner_id(&stale, "terminal_1"), Ok(None));
    let reopened = fixture.agent("claude", "reopened", Some("terminal_1"));
    let read = fixture.read(vec![reopened], pane);
    assert_eq!(
        owner_id(&read, "terminal_1"),
        Ok(Some("reopened".to_owned()))
    );
}
