//! Account standing: which account a launch at one project root uses for each
//! provider kind, and which layers make each account a default. `rimz accounts
//! list`, `rimz providers`, and `rimz doctor` all read it, so they say the same.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result};

use crate::config::MachineConfig;
use crate::ids::{AgentKind, LoginKey, LoginName, RoomLogins};
use crate::trust::ProjectLogins;
use crate::{RuntimePaths, StatePaths, Store};

/// One layer that makes an account a default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    ThisRoom,
    ThisProject,
    NewRooms,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ThisRoom => "this room",
            Self::ThisProject => "this project",
            Self::NewRooms => "new rooms",
        }
    }
}

/// The layers an account is the default for; serializes as an array of
/// snake_case words in display order.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(transparent)]
pub struct Scopes(BTreeSet<Scope>);

impl Scopes {
    /// The words joined in the order `this room`, `this project`, `new
    /// rooms`; `-` when the account is no layer's default.
    pub fn label(&self) -> String {
        if self.0.is_empty() {
            return "-".to_owned();
        }
        self.0
            .iter()
            .map(|scope| scope.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// The layer whose selection a launch at the position follows for one kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Deciding {
    /// The room recorded at the position; `rimz accounts use` moves it.
    Room,
    /// The trusted project's `[accounts]`.
    Project,
    /// The machine's `[accounts.use]`, or `default` when it names nothing;
    /// `rimz accounts use --global` moves it.
    Machine,
}

/// The account layers at one project root: the room recorded there, the
/// project's own selection, and the machine's, plus the named accounts the
/// machine declares.
#[derive(Debug)]
pub struct AccountStanding {
    recorded: Option<RoomLogins>,
    /// `None` when the position's layers could not be read.
    project: Option<ProjectLogins>,
    machine: RoomLogins,
    declared: BTreeSet<LoginKey>,
}

impl AccountStanding {
    /// The standing at `project_root` under `machine`'s `[accounts.use]`.
    pub fn at(project_root: &Path, machine: &MachineConfig) -> Result<Self> {
        let state = StatePaths::for_project_root(project_root).context("preparing store paths")?;
        let recorded = crate::workspace::record::read_optional(&state.workspace_record)
            .context("reading the room's accounts")?
            .and_then(|record| record.logins);
        Ok(Self {
            recorded,
            project: Some(crate::trust::project_logins(project_root)?),
            ..Self::machine_only(machine)
        })
    }

    /// The standing with no room and no project: the machine layer alone.
    pub fn machine_only(machine: &MachineConfig) -> Self {
        Self {
            recorded: None,
            project: Some(ProjectLogins::Unconfigured),
            machine: machine.accounts.use_accounts.clone(),
            declared: declared_accounts(machine),
        }
    }

    /// The standing when the position's room or project layer could not be
    /// read: no account is active, since birth would not launch there, and
    /// only the machine layer's scopes are known.
    pub fn unread(machine: &MachineConfig) -> Self {
        Self {
            project: None,
            ..Self::machine_only(machine)
        }
    }

    /// The account a launch here uses for `kind`; `None` where no launch
    /// resolves: no room is recorded and the project's selection is blocked
    /// or unreadable, or the deciding layer names an undeclared account.
    pub fn active(&self, kind: &AgentKind) -> Option<LoginName> {
        let name = self.selection(kind)?;
        (name.is_default()
            || self
                .declared
                .contains(&LoginKey::new(kind.clone(), name.clone())))
        .then_some(name)
    }

    fn selection(&self, kind: &AgentKind) -> Option<LoginName> {
        if let Some(recorded) = &self.recorded {
            return Some(recorded.get(kind).cloned().unwrap_or_default());
        }
        let project = match self.project.as_ref()? {
            ProjectLogins::Unconfigured => &RoomLogins::new(),
            ProjectLogins::Apply(logins) => logins,
            ProjectLogins::Blocked(_) => return None,
        };
        Some(crate::agents::birth_name(
            kind,
            &RoomLogins::new(),
            project,
            &self.machine,
        ))
    }

    /// The layer the active account of `kind` follows; `None` exactly when
    /// [`Self::active`] is.
    pub fn deciding(&self, kind: &AgentKind) -> Option<Deciding> {
        self.active(kind)?;
        if self.recorded.is_some() {
            return Some(Deciding::Room);
        }
        match self.project.as_ref()? {
            ProjectLogins::Apply(logins) if logins.contains_key(kind) => Some(Deciding::Project),
            _ => Some(Deciding::Machine),
        }
    }

    /// The layers `name` is the default of for `kind`.
    pub fn scopes(&self, kind: &AgentKind, name: &LoginName) -> Scopes {
        let mut scopes = BTreeSet::new();
        if self
            .recorded
            .as_ref()
            .is_some_and(|recorded| recorded.get(kind).cloned().unwrap_or_default() == *name)
        {
            scopes.insert(Scope::ThisRoom);
        }
        if let Some(ProjectLogins::Apply(logins)) = &self.project
            && logins.get(kind) == Some(name)
        {
            scopes.insert(Scope::ThisProject);
        }
        if self.machine.get(kind).cloned().unwrap_or_default() == *name {
            scopes.insert(Scope::NewRooms);
        }
        Scopes(scopes)
    }

    /// Every account some layer names, declared or not.
    pub fn selected(&self) -> BTreeSet<LoginKey> {
        let project = match &self.project {
            Some(ProjectLogins::Apply(logins)) => Some(logins),
            _ => None,
        };
        self.recorded
            .iter()
            .chain(project)
            .chain(std::iter::once(&self.machine))
            .flatten()
            .map(|(kind, name)| LoginKey::new(kind.clone(), name.clone()))
            .collect()
    }

    /// Whether `name` is named by the machine layer for `kind`, so its
    /// problem is reported against `[accounts.use]`.
    pub fn machine_selects(&self, kind: &AgentKind, name: &LoginName) -> bool {
        self.machine.get(kind) == Some(name)
    }

    /// Why no account is active for any kind, with the fix: the project's
    /// selection is blocked and no room is recorded here.
    pub fn blocked(&self) -> Option<String> {
        match (&self.recorded, &self.project) {
            (None, Some(ProjectLogins::Blocked(state))) => Some(blocked_project_logins(*state)),
            _ => None,
        }
    }
}

fn declared_accounts(machine: &MachineConfig) -> BTreeSet<LoginKey> {
    crate::agents::known_kinds()
        .map(AgentKind::new_unchecked)
        .flat_map(|kind| {
            machine
                .accounts
                .named(&kind)
                .into_iter()
                .flat_map(BTreeMap::keys)
                .map(move |name| LoginKey::new(kind.clone(), name.clone()))
        })
        .collect()
}

/// The refusal for a project account selection behind a closed trust gate.
pub(super) fn blocked_project_logins(state: crate::trust::TrustState) -> String {
    format!(
        "project account selections in .rimz/config.toml are {}; {}",
        state.as_str(),
        crate::trust::blocked_fix(state)
    )
}

/// Live pane-backed agents per account across every live room, each room read
/// without creating its store.
pub fn live_agents_by_login() -> Result<BTreeMap<LoginKey, usize>, super::LiveRoomErr> {
    let mut counts = BTreeMap::new();
    for agents in live_room_agents()? {
        count_live_logins(&mut counts, &agents);
    }
    Ok(counts)
}

/// Agents on `account` whose provider can be writing, across every live room,
/// besides the `launching` one. A row counts once a pane is attached to it: a
/// seat of a batch that has not started and a row a rebirth recovered have
/// none.
pub fn other_live_agents_on(
    account: &LoginKey,
    launching: &[crate::ids::AgentSessionId],
) -> Result<usize, super::LiveRoomErr> {
    Ok(live_room_agents()?
        .iter()
        .map(|agents| count_others_on(agents, account, launching))
        .sum())
}

fn live_room_agents() -> Result<Vec<Vec<crate::agents::AgentState>>, super::LiveRoomErr> {
    super::session::room_inventory()?
        .live
        .into_iter()
        .map(|room| {
            // A live room whose agents cannot be read could hold any account, so
            // every count is unknown rather than short by that room.
            StatePaths::for_workspace(room.workspace_id.clone())
                .ok()
                .zip(RuntimePaths::for_workspace(room.workspace_id.clone()).ok())
                .and_then(|(paths, runtime)| Store::open_existing(paths, runtime))
                .and_then(|store| store.snapshot_cached().ok())
                .map(|snapshot| snapshot.agents)
                .ok_or(super::LiveRoomErr::RoomAgents {
                    session_name: room.session_name,
                    project_root: room.project_root,
                })
        })
        .collect()
}

fn count_live_logins(counts: &mut BTreeMap<LoginKey, usize>, agents: &[crate::agents::AgentState]) {
    for key in crate::agents::live_login_keys(agents) {
        *counts.entry(key).or_default() += 1;
    }
}

fn count_others_on(
    agents: &[crate::agents::AgentState],
    account: &LoginKey,
    launching: &[crate::ids::AgentSessionId],
) -> usize {
    agents
        .iter()
        .filter(|agent| {
            agent.ended_at.is_none()
                && agent.pane.is_some()
                && !agent.is_provider_subagent()
                && agent.login_key() == *account
                && !launching.contains(&agent.agent_id)
        })
        .count()
}

#[cfg(test)]
mod tests;
