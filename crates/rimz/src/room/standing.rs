//! Account standing: which account a launch at one project root uses for each
//! provider kind, and which layers make each account a default. `rimz accounts
//! list`, `rimz providers`, and `rimz doctor` all read it, so they say the same.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result};

use crate::config::MachineConfig;
use crate::ids::{AgentKind, LoginKey, LoginName, RoomLogins};
use crate::store::runtime::AgentLiveness;
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
    /// Whether the account is `scope`'s default.
    pub fn contains(&self, scope: Scope) -> bool {
        self.0.contains(&scope)
    }

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
    accounts: Option<crate::agents::RoomAccounts>,
    machine: RoomLogins,
}

impl AccountStanding {
    /// The standing at `project_root` under the live pin-or-inherit selection.
    pub fn at(project_root: &Path, machine: &MachineConfig) -> Result<Self> {
        let state = StatePaths::for_project_root(project_root).context("preparing store paths")?;
        let accounts =
            crate::agents::room_accounts(&state.workspace_record, Some(project_root), machine)?;
        Ok(Self {
            accounts: Some(accounts),
            machine: machine.accounts.use_accounts.clone(),
        })
    }

    /// The standing with no room and no project: the machine layer alone.
    pub fn machine_only(machine: &MachineConfig) -> Self {
        Self {
            accounts: Some(crate::agents::RoomAccounts::machine(machine)),
            machine: machine.accounts.use_accounts.clone(),
        }
    }

    /// An unreadable position has no active account; machine scopes remain known.
    pub fn unread(machine: &MachineConfig) -> Self {
        Self {
            accounts: None,
            machine: machine.accounts.use_accounts.clone(),
        }
    }

    /// The kind's current account, or none when that kind cannot launch here.
    pub fn active(&self, kind: &AgentKind) -> Option<LoginName> {
        self.accounts.as_ref()?.name(kind).ok()
    }

    /// The layer a launch of this kind follows.
    pub fn deciding(&self, kind: &AgentKind) -> Option<Deciding> {
        Some(match self.accounts.as_ref()?.source(kind).ok()? {
            crate::agents::LoginSource::Pinned => Deciding::Room,
            crate::agents::LoginSource::Project => Deciding::Project,
            crate::agents::LoginSource::Machine | crate::agents::LoginSource::Provider => {
                Deciding::Machine
            }
        })
    }

    /// The layers that explicitly select `name` for `kind`.
    pub fn scopes(&self, kind: &AgentKind, name: &LoginName) -> Scopes {
        let mut scopes = BTreeSet::new();
        if self
            .accounts
            .as_ref()
            .and_then(|accounts| accounts.pin(kind))
            == Some(name)
        {
            scopes.insert(Scope::ThisRoom);
        }
        if let Some(ProjectLogins::Apply(logins)) = self
            .accounts
            .as_ref()
            .and_then(crate::agents::RoomAccounts::project)
            && logins.get(kind) == Some(name)
        {
            scopes.insert(Scope::ThisProject);
        }
        if self.machine.get(kind).cloned().unwrap_or_default() == *name {
            scopes.insert(Scope::NewRooms);
        }
        Scopes(scopes)
    }

    /// Every account a layer names, declared or not.
    pub fn selected(&self) -> BTreeSet<LoginKey> {
        let pins = self
            .accounts
            .as_ref()
            .map(crate::agents::RoomAccounts::pinned_names)
            .unwrap_or_default();
        let project = match self
            .accounts
            .as_ref()
            .and_then(crate::agents::RoomAccounts::project)
        {
            Some(ProjectLogins::Apply(logins)) => Some(logins),
            _ => None,
        };
        std::iter::once(&pins)
            .chain(project)
            .chain(std::iter::once(&self.machine))
            .flatten()
            .map(|(kind, name)| LoginKey::new(kind.clone(), name.clone()))
            .collect()
    }

    /// Whether the machine selects `name`, for source-specific diagnostics.
    pub fn machine_selects(&self, kind: &AgentKind, name: &LoginName) -> bool {
        self.machine.get(kind) == Some(name)
    }

    /// The kind-specific refusals, without hiding other kinds' active accounts.
    pub fn blocked(&self) -> Option<String> {
        let accounts = self.accounts.as_ref()?;
        let problems: BTreeSet<_> = crate::agents::known_kinds()
            .filter_map(|kind| {
                accounts
                    .account(&AgentKind::new_unchecked(kind))
                    .err()
                    .map(|error| error.to_string())
            })
            .collect();
        (!problems.is_empty()).then(|| problems.into_iter().collect::<Vec<_>>().join("; "))
    }
}

/// Agents per account whose provider can be writing, across every live room,
/// each room read without creating its store: for every account, the number
/// [`other_live_agents_on`] answers with nothing launching.
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
/// none. Every producer of the launch wrapper places it in a pane, a
/// supervised headless run included (held by the `deep` journey's
/// `tmux_supervised_run_holds_its_account_history_link_until_it_dies` and its
/// Zellij twin), and the wrapper links the account before it binds that pane
/// and starts the provider. A row whose recorded owner process is dead does
/// not count: a room's published rollup can still hold it when the launch
/// failed or was killed after its pane was bound. A session a remote-control
/// daemon serves has no pane of its own and stays out: the account-link
/// reconciler asks the provider for the daemon itself.
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
    for agent in agents.iter().filter(|agent| can_write(agent)) {
        *counts.entry(agent.login_key()).or_default() += 1;
    }
}

/// Whether a provider behind this row can be writing its account's history.
fn can_write(agent: &crate::agents::AgentState) -> bool {
    agent.ended_at.is_none()
        && agent.pane.is_some()
        && !agent.is_provider_subagent()
        && crate::store::runtime::agent_liveness(agent) != AgentLiveness::Dead
}

fn count_others_on(
    agents: &[crate::agents::AgentState],
    account: &LoginKey,
    launching: &[crate::ids::AgentSessionId],
) -> usize {
    agents
        .iter()
        .filter(|agent| {
            can_write(agent)
                && agent.login_key() == *account
                && !launching.contains(&agent.agent_id)
        })
        .count()
}

#[cfg(test)]
mod tests;
