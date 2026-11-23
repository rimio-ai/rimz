//! The point-of-use agent lookup: where an agent is hosted now, as the multiplexer answers.
//!
//! [`read_hosting`] picks the backend hosting the room's session, lists its panes at
//! `RequireAuthoritative` with no prior frame, and folds that listing over a fresh store
//! rollup. It never answers from a cached topology, a carried pane, a held frame, the
//! published snapshot, or a pane stored on a record: when the mux cannot answer, the read is
//! a [`HostingErr`] naming the failure.
//!
//! A read answers three questions. [`HostingRead::agent_hosting`] takes an agent, a kind and
//! its launch key, and answers the session holding it, the row `address::launch_row` picks,
//! with that row's pane. [`HostingRead::session_hosting`] takes one exact session, so a
//! session a swap replaced still answers its own pane while it is listed.
//! [`HostingRead::agent_hosting_of_session`] takes the session a caller holds (a sidebar
//! card's key) and answers the agent that caller acts on: that session while no swap replaced
//! it, else its launch's holder. From the same listing, [`HostingRead::closing_empties_tab`]
//! says whether closing a pane leaves its tab without a working pane, and
//! [`HostingRead::tab_anchor`] picks a listed pane to close a tab by.

use crate::agents::AgentState;
use crate::ids::{AgentKind, AgentSessionId, MuxName, PaneId, WorkspaceId};
use crate::pane::PaneRef;
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
    /// The listing this read folded, which tab each pane is in.
    panes: Vec<PaneRef>,
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
    ///
    /// One launch can be live in two panes after a revival, and the answer is the occupant,
    /// which need not be the session the caller resolved: a caller that acts on a session it
    /// resolved itself compares `AgentHosting::holder` with that session and refuses when they
    /// differ. Its test stages two live rows of one launch in two listed panes and acts on the
    /// non-occupant, as
    /// `message::pane_acts::answer_refuses_when_another_session_of_the_launch_holds_the_pane`
    /// does.
    pub fn agent_hosting(&self, kind: &AgentKind, launch: &AgentSessionId) -> AgentHosting<'_> {
        let holder = crate::address::launch_row(&self.audit, kind, launch);
        AgentHosting {
            holder,
            hosting: holder.map_or(Hosting::Nowhere, |holder| {
                self.session_hosting(&holder.kind, &holder.agent_id)
            }),
        }
    }

    /// The agent a caller holding the session `(kind, session)` acts on: that session while
    /// no swap replaced it, with its own pane even beside a live occupant of its launch;
    /// else its launch's holder; and with no row for the key, the row that answers to it
    /// as a launch id (the row that adopted a provisional key), or no holder.
    pub fn agent_hosting_of_session(
        &self,
        kind: &AgentKind,
        session: &AgentSessionId,
    ) -> AgentHosting<'_> {
        let Some(row) = self.agent(kind, session) else {
            return self.agent_hosting(kind, session);
        };
        let members =
            crate::agents::state::launch_members(&self.audit, &row.kind, row.launch_key());
        if crate::agents::state::LaunchHolders::new(members.iter().copied()).replaced(row) {
            return self.agent_hosting(kind, row.launch_key());
        }
        AgentHosting {
            holder: Some(row),
            hosting: self.session_hosting(kind, session),
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

    /// Whether closing `pane` empties its tab in this read: the tab's sidebar, once `pane`
    /// is gone, would self-close on the verdict `crate::pane::self_close_verdict` gives.
    /// `false` when `pane` or its tab's sidebar is unlisted, when the tab is the session's only
    /// one, or when `caller`, the pane the asking process runs in, is in that tab.
    pub fn closing_empties_tab(&self, pane: &PaneId, caller: Option<&PaneId>) -> bool {
        let Some(view) = self.closable_tab(pane, caller) else {
            return false;
        };
        let in_view = |listed: &&PaneRef| listed.view_id.as_deref() == Some(view);
        let rest = self
            .panes
            .iter()
            .filter(|listed| &listed.pane_id != pane)
            .cloned()
            .collect::<Vec<_>>();
        rest.iter()
            .filter(in_view)
            .filter(|listed| listed.is_rimz_sidebar())
            .any(|sidebar| {
                matches!(
                    crate::pane::self_close_verdict(&rest, &sidebar.pane_id),
                    crate::pane::SelfCloseVerdict::Empty { .. }
                )
            })
    }

    /// The first of `panes` this read lists, the pane to close their tab by. `None` when it
    /// lists none, when the tab is the session's only one or holds `caller`, the pane the
    /// asking process runs in, or when a tiled pane of the tab other than its sidebar is not
    /// one of `panes`, so a pane that entered the tab since `panes` were listed never goes
    /// with it.
    pub fn tab_anchor<'a>(
        &self,
        panes: &'a [PaneId],
        caller: Option<&PaneId>,
    ) -> Option<&'a PaneId> {
        let anchor = panes.iter().find(|pane| self.view_of(pane).is_some())?;
        let view = self.closable_tab(anchor, caller)?;
        self.panes
            .iter()
            .filter(|listed| listed.view_id.as_deref() == Some(view))
            .filter(|listed| !listed.is_floating && !listed.is_rimz_sidebar())
            .all(|listed| panes.contains(&listed.pane_id))
            .then_some(anchor)
    }

    /// The tab of `pane` when a close may take it: another tab is listed beside it, since
    /// closing a session's only tab ends the session before the closed agent's wrapper can
    /// tell its exit was deliberate (`cli/agents_cmd/exec.rs::close_is_deliberate`), and
    /// `caller`, the pane the asking process runs in, is not in it.
    fn closable_tab(&self, pane: &PaneId, caller: Option<&PaneId>) -> Option<&str> {
        let view = self.view_of(pane)?;
        if caller.is_some_and(|caller| self.view_of(caller) == Some(view)) {
            return None;
        }
        self.panes
            .iter()
            .any(|listed| listed.view_id.as_deref().is_some_and(|other| other != view))
            .then_some(view)
    }

    /// The tab of `pane` in this read's listing.
    fn view_of(&self, pane: &PaneId) -> Option<&str> {
        self.listed(pane)
            .and_then(|listed| listed.view_id.as_deref())
    }

    /// `pane` as this read's listing has it.
    pub fn listed(&self, pane: &PaneId) -> Option<&PaneRef> {
        self.panes.iter().find(|listed| &listed.pane_id == pane)
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
    read_hosting_in(&workspace.workspace_id, &workspace.session_name, store, mux)
}

/// [`read_hosting`] for a caller holding the room's identity without its resolved
/// workspace: the sidebar host, which opens the room's existing store for one read.
pub(crate) fn read_hosting_in(
    workspace_id: &WorkspaceId,
    session: &str,
    store: &Store,
    mux: Option<MuxName>,
) -> Result<HostingRead, HostingErr> {
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
    let diag = crate::diag::DiagSink::for_workspace(workspace_id.clone(), session.to_owned(), None);
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
    let panes = frame.to_pane_refs();
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
        panes,
    }
}

#[cfg(test)]
mod tests;
