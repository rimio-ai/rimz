//! The point-of-use agent lookup: where an agent is hosted now, as the multiplexer answers.
//!
//! [`read_hosting`] picks the backend hosting the room's session, lists its panes at
//! `RequireAuthoritative` with no prior frame, and folds that listing over a fresh store
//! rollup. It never answers from a cached topology, a carried pane, a held frame, the
//! published snapshot, or a pane stored on a record: when the mux cannot answer, the read is
//! a [`HostingErr`] naming the failure.
//!
//! A read answers two questions. [`HostingRead::agent_hosting`] takes an agent, a kind and its
//! launch key, and answers the session holding it, the row `address::launch_row` picks, with
//! that row's pane. [`HostingRead::session_hosting`] takes one exact session, so a session a
//! swap replaced still answers its own pane while it is listed.

use crate::agents::AgentState;
use crate::ids::{AgentKind, AgentSessionId, MuxName, PaneId};
use crate::sidebar::consumer::{RollupCursor, rollup_snapshot};
use crate::sidebar::frame::{PaneFrame, assemble_frame};
use crate::store::runtime::agent_is_runtime_visible;
use crate::store::snapshot::{SidebarSnapshot, find_agent};
use crate::utils::time::unix_now_ms;
use crate::{ResolvedWorkspace, RuntimePaths, Store};

use super::ProduceErr;

/// Where one session is hosted, by [`HostingRead::session_hosting`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hosting {
    /// The pane binder ties the agent's unended row to this listed pane, and its process
    /// is not known dead.
    Pane {
        pane_id: PaneId,
        session_name: String,
    },
    /// No listed pane hosts the agent.
    Nowhere,
}

/// Where an agent is hosted, by [`HostingRead::agent_hosting`].
#[derive(Clone, Debug, PartialEq)]
pub struct AgentHosting<'a> {
    /// The session holding the agent in this read; `None` when no row answers to the launch key.
    pub holder: Option<&'a AgentState>,
    /// Where `holder` is hosted; `Nowhere` without one.
    pub hosting: Hosting,
}

/// One read of the room: the mux's own pane listing at the time of the call, folded
/// over the store rollup read by the same call.
#[derive(Debug)]
pub struct HostingRead {
    mux: MuxName,
    session_name: String,
    notices: Vec<String>,
    /// Every stored row, ended and dead-owner rows included, for judging a stop; live
    /// rows carry their rest certificates, so the holder pick orders them as the resolver
    /// does.
    audit: Vec<AgentState>,
    /// The fold over runtime-visible rows only, so no ended or dead row takes a pane.
    snapshot: SidebarSnapshot,
}

/// The lookup could not get the multiplexer's own answer; the backend pick's notices
/// travel with it.
#[derive(Debug, thiserror::Error)]
#[error("the multiplexer could not be read: {source}")]
pub struct HostingErr {
    pub notices: Vec<String>,
    #[source]
    pub source: ProduceErr,
}

impl HostingRead {
    /// The agent `(kind, launch)`: its current occupant among the launch's holder rows, and
    /// that row's pane.
    pub fn agent_hosting(&self, kind: &AgentKind, launch: &AgentSessionId) -> AgentHosting<'_> {
        let holder = crate::address::launch_row(&self.audit, kind, launch);
        AgentHosting {
            holder,
            hosting: holder.map_or(Hosting::Nowhere, |holder| {
                self.session_hosting(&holder.kind, &holder.agent_id)
            }),
        }
    }

    /// Where the session `(kind, agent_id)` is hosted: an exact key, so a namesake's
    /// pane never answers for it, and an ended row or one whose known process is dead
    /// is hosted nowhere even while a pane it held is listed.
    pub fn session_hosting(&self, kind: &AgentKind, agent_id: &AgentSessionId) -> Hosting {
        if !self
            .agent(kind, agent_id)
            .is_some_and(agent_is_runtime_visible)
        {
            return Hosting::Nowhere;
        }
        match self.snapshot.live_agent_pane(kind, agent_id) {
            Some(pane_id) => Hosting::Pane {
                pane_id,
                session_name: self.session_name.clone(),
            },
            None => Hosting::Nowhere,
        }
    }

    /// The agent row as this read found it in the store.
    pub fn agent(&self, kind: &AgentKind, agent_id: &AgentSessionId) -> Option<&AgentState> {
        find_agent(&self.audit, kind, agent_id)
    }

    /// Every agent row this read found in the store.
    pub fn agents(&self) -> &[AgentState] {
        &self.audit
    }

    /// The backend this read listed.
    pub fn mux(&self) -> MuxName {
        self.mux
    }

    /// The backend pick's notices (a backend skipped on a timeout or a denied socket),
    /// for the caller to print once.
    pub fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }
}

/// Read where the room's agents are hosted now. The backend is the one hosting the
/// room's session: `mux` when given, else the backend listing the session, else the
/// ambient detection. A session the mux reports as absent hosts nothing; any other
/// failure to list is an error, never a stored or cached answer.
pub fn read_hosting(
    workspace: &ResolvedWorkspace,
    store: &Store,
    mux: Option<MuxName>,
) -> Result<HostingRead, HostingErr> {
    let session = &workspace.session_name;
    let (mux, notices) = if super::pane_fixture_active() {
        (mux.unwrap_or(MuxName::Zellij), Vec::new())
    } else {
        match crate::room::session::pick_mux_for_session(session, mux) {
            Ok(pick) => (pick.mux, pick.notices),
            Err(err) => {
                return Err(HostingErr {
                    notices: err.notices,
                    source: ProduceErr::PaneDiscovery(err.source),
                });
            }
        }
    };
    let runtime = store.runtime_paths();
    let diag =
        crate::diag::DiagSink::for_workspace(workspace.workspace_id.clone(), session.clone(), None);
    let listed = absent_session_as_empty(
        super::panes::read_authoritative_pane_frame(runtime, mux, session, &diag),
        session,
    );
    let read = listed.and_then(|frame| {
        let mut snapshot = rollup_snapshot(store.paths(), &mut RollupCursor::new())?;
        // Destructive callers need ended and dead-owner rows' owner identity and
        // recorded end when judging a stop; `fold` binds panes over the visible ones.
        snapshot.agents = store
            .runtime_projection(crate::RuntimeScope::Audit)?
            .agents
            .into();
        Ok((frame, snapshot))
    });
    match read {
        Ok((frame, snapshot)) => Ok(fold(mux, notices, snapshot, frame, runtime)),
        Err(source) => Err(HostingErr { notices, source }),
    }
}

/// A frame for the session's panes, with a session the mux reports absent read as an
/// empty listing.
fn absent_session_as_empty(
    listed: super::Result<PaneFrame>,
    session: &str,
) -> super::Result<PaneFrame> {
    match listed {
        Err(ProduceErr::PaneDiscovery(crate::mux::MuxErr::SessionNotFound { .. })) => Ok(
            assemble_frame(Vec::new(), unix_now_ms(), session.to_owned()),
        ),
        listed => listed,
    }
}

/// Fold `frame` over the runtime-visible rows of `snapshot`, whose rows are the audit
/// rows: an ended or dead row never competes for a listed pane.
fn fold(
    mux: MuxName,
    notices: Vec<String>,
    mut snapshot: SidebarSnapshot,
    frame: PaneFrame,
    runtime: &RuntimePaths,
) -> HostingRead {
    let session_name = frame.session_name.clone();
    let mut audit = snapshot.agents.to_vec();
    // The holder pick orders live rows by their rest certificates, as the address
    // resolver's snapshot does.
    crate::store::agent_context::attach_rest_certificates(runtime, audit.iter_mut());
    snapshot.agents = audit
        .iter()
        .filter(|agent| agent_is_runtime_visible(agent))
        .cloned()
        .collect::<Vec<_>>()
        .into();
    let snapshot = super::fold_resolution_frame(
        snapshot,
        frame,
        None,
        crate::sidebar::agent_projection::probe_current(),
        runtime,
    );
    HostingRead {
        mux,
        session_name,
        notices,
        audit,
        snapshot,
    }
}

#[cfg(test)]
mod tests {
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
            let snapshot =
                SidebarSnapshot::build_with_agents(self.workspace.clone(), agents, self.now);
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
            vec![test_support::pane("terminal_1", None, Some("/repo/main"))],
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
        let mut owner = crate::store::runtime::process_owner(
            RuntimeOwnerKind::Agent,
            "dead",
            std::process::id(),
        );
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
        let read = fixture.read(
            vec![dead, live],
            vec![test_support::pane("terminal_1", None, Some("/repo/main"))],
        );
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
            vec![test_support::pane("terminal_1", None, Some("/repo/main"))],
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

    fn pane_of(id: &str) -> Hosting {
        Hosting::Pane {
            pane_id: PaneId::from_parts(MuxName::Zellij, id),
            session_name: "room".to_owned(),
        }
    }

    fn listed(ids: &[&str]) -> Vec<crate::pane::PaneRef> {
        ids.iter()
            .map(|id| test_support::pane(id, None, Some("/repo/main")))
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
            }),
            ..crate::agents::AgentContext::new("claude", error_at)
        };
        crate::store::agent_context::write(&fixture.runtime, "claude", "sess-a", &rested, None)
            .expect("context sidecar");
        let read = fixture.read(vec![first, second], listed(&["terminal_1", "terminal_2"]));
        let answer =
            read.agent_hosting(&AgentKind::new_unchecked("claude"), &"launch_agent".into());
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
        let read = fixture.read(vec![alone], listed(&["terminal_1"]));
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
}
