//! What an agent started that is still alive, grouped by what a stop of it does to each:
//! holding the stop, closing with it, or outliving it.
//!
//! The holding group reads the terms [`decide`](super::decide) reads, through the same
//! predicates, so a stop held for anything the reader can act on lists what holds it. The
//! agent's own turn, its clock, a compaction, and a wake already on its way end by
//! themselves and are no rows. Everything here is read; nothing is written.

use std::collections::{HashSet, VecDeque};
use std::path::Path;

use jiff::Timestamp;
use serde::Serialize;

use super::super::owed::{owes_report_to, owing_members, owing_teams};
use super::super::schedule::catalog::TaskCatalog;
use super::super::schedule::pending::SessionWaits;
use super::super::schedule::{
    TaskAction, TaskTiming, TaskTimingState, attention, fire, task_record,
};
use super::{Hold, board_done, closing_with, debt, open_message, open_run, resting, rests};
use crate::Store;
use crate::address::AddressContext;
use crate::agents::{AgentState, AgentStatus, LaunchedBy, PendingWaitTrigger, TurnPhase};
use crate::config::MachineConfig;
use crate::ids::{AgentKind, TeamInstanceId};
use crate::store::StoreErr;
use crate::store::run::RunRecord;
use crate::store::runtime::AuditRows;
use crate::store::snapshot::SidebarSnapshot;
use crate::store::workflow::RunHeader;

/// What joins the agents of a row's path.
const PATH_SEPARATOR: &str = " › ";

/// A launch chain longer than a booking chain's bound is cut there.
const MAX_PATH: usize = 16;

/// Who reads the list, and from where: commands are offered only where they work.
pub struct Seat<'a> {
    /// The room's project root, whose loop tasks are read.
    pub root: &'a Path,
    /// The reader's address context, which every printed handle resolves in.
    pub reader: &'a AddressContext,
    /// The reader is the agent itself, so its own waits can be canceled from here.
    pub you: bool,
    /// What each printed command starts with: `rimz`, with `--root` when the agent's room is
    /// not the one the reader's commands resolve.
    pub rimz: &'a str,
}

/// One agent's started work, each row in the first group that takes it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Started {
    /// What keeps a booked stop from firing.
    pub holding: Vec<StartedRow>,
    /// Live `rimz subagents` children that a stop closes and that hold nothing.
    pub closing: Vec<StartedRow>,
    /// What the agent launched or booked that keeps running after it closes.
    pub outliving: Vec<StartedRow>,
}

impl Started {
    pub fn is_empty(&self) -> bool {
        self.holding.is_empty() && self.closing.is_empty() && self.outliving.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StartedRow {
    pub kind: RowKind,
    /// The row's name, after the path of agents it was reached through (`@a › @b`).
    pub name: String,
    pub status: String,
    /// The agent status and phase whose state role colours `status`.
    #[serde(skip)]
    pub tone: AgentStatus,
    #[serde(skip)]
    pub phase: TurnPhase,
    /// The provider of the agent the row names, whose identity colours its name.
    #[serde(skip)]
    pub provider: Option<AgentKind>,
    /// When the state `status` names began.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<Timestamp>,
    /// When the row next acts: a task's next fire, a message's earliest delivery.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due: Option<Timestamp>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub detail: Vec<String>,
    /// The existing command that ends the row, run from the reader's seat.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RowKind {
    Subagent,
    Agent,
    Team,
    Booked,
    Wait,
    Shell,
    Native,
    Message,
    Run,
    Workflow,
    Turn,
    Question,
    Budget,
    Board,
}

impl RowKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Subagent => "subagent",
            Self::Agent => "agent",
            Self::Team => "team",
            Self::Booked => "booked",
            Self::Wait => "wait",
            Self::Shell => "shell",
            Self::Native => "native",
            Self::Message => "message",
            Self::Run => "run",
            Self::Workflow => "workflow",
            Self::Turn => "turn",
            Self::Question => "question",
            Self::Budget => "budget",
            Self::Board => "board",
        }
    }
}

/// What `agent` started that is still alive, grouped holding, closing, outliving. `snapshot`
/// is the enriched snapshot `agent` comes from.
pub fn started(
    store: &Store,
    snapshot: &SidebarSnapshot,
    agent: &AgentState,
    seat: &Seat<'_>,
    now: Timestamp,
) -> Result<Started, StoreErr> {
    let audit = store.audit_rows()?;
    let mut list = List {
        snapshot,
        audit: &audit,
        agent,
        seat,
        now,
        seen: HashSet::new(),
        started: Started::default(),
    };
    // An ended agent holds and closes nothing: only what outlives it is left.
    if agent.ended_at.is_none() {
        list.holding(store)?;
    }
    list.outliving(store);
    Ok(list.started)
}

/// One row per thing: `seen` keys every launch and team a row already names.
struct List<'a> {
    snapshot: &'a SidebarSnapshot,
    /// The log's rows, which keep an ended launcher the snapshot no longer carries.
    audit: &'a AuditRows,
    agent: &'a AgentState,
    seat: &'a Seat<'a>,
    now: Timestamp,
    seen: HashSet<String>,
    started: Started,
}

fn agent_key(row: &AgentState) -> String {
    format!("agent {}", row.launch_key())
}

fn team_key(instance: &TeamInstanceId) -> String {
    format!("team {instance}")
}

fn workflow_key(id: &str) -> String {
    format!("workflow {id}")
}

/// The room's live workflow runs whose record this RimZ reads, each with its id.
fn live_runs(store: &Store) -> Result<Vec<(String, RunHeader)>, StoreErr> {
    Ok(crate::store::workflow::live(store.paths())?
        .into_iter()
        .filter_map(|run| {
            let header = run.class.header()?.into_owned();
            let id = run.paths.home.file_name()?.to_string_lossy().into_owned();
            Some((id, header))
        })
        .collect())
}

/// A booking or launch link that names `agent`, by launch id.
fn names(link: &LaunchedBy, agent: &AgentState) -> bool {
    agent.launch_id.as_ref() == Some(&link.agent_id)
        || (link.kind == agent.kind && agent.answers_to(&link.agent_id))
}

fn board_status(root: &Path) -> Option<String> {
    super::super::scratch::board_stage(root).map(|stage| format!("stage {}", stage.name))
}

impl<'a> List<'a> {
    fn rows(&self) -> &'a [AgentState] {
        let snapshot: &'a SidebarSnapshot = self.snapshot;
        &snapshot.agents
    }

    fn handle(&self, row: &AgentState) -> String {
        crate::address::paste_address(row, self.snapshot, self.seat.reader)
    }

    /// `row`'s handle after the launchers between it and the shown agent.
    fn path(&self, row: &AgentState) -> String {
        self.path_names(row).join(PATH_SEPARATOR)
    }

    fn path_names<'r>(&'r self, row: &'r AgentState) -> Vec<String> {
        let mut names = vec![self.handle(row)];
        let mut current = row;
        while !current.launcher_is(self.agent) && names.len() < MAX_PATH {
            let Some(up) = self.launcher_row(current) else {
                break;
            };
            names.push(self.handle(up));
            current = up;
        }
        names.reverse();
        names
    }

    /// The row that launched `row`: the snapshot's holder, else an ended launcher the log keeps.
    fn launcher_row(&self, row: &AgentState) -> Option<&'a AgentState> {
        let (kind, id) = row.launcher()?;
        crate::address::launch_row(self.rows(), kind, id).or_else(|| {
            crate::agents::state::launch_members(self.audit.iter(), kind, id)
                .into_iter()
                .next()
        })
    }

    fn agent_row(&self, row: &AgentState, name: String, stop: bool) -> StartedRow {
        let tone = row.effective_status(self.now);
        let ended = row.ended_at.is_some();
        let since = match tone {
            _ if ended => row.ended_at,
            AgentStatus::Running | AgentStatus::Waiting => row.turn_started_at,
            _ => row.turn_ended_at,
        };
        StartedRow {
            kind: if row.is_launched_child() {
                RowKind::Subagent
            } else {
                RowKind::Agent
            },
            name,
            status: if ended { "ended" } else { tone.as_str() }.to_owned(),
            tone,
            phase: row.phase,
            provider: Some(row.kind.clone()),
            since: since.or(Some(row.last_activity)),
            due: None,
            detail: row.profile.iter().cloned().collect(),
            command: (stop && !ended)
                .then(|| format!("{} agents stop {}", self.seat.rimz, self.handle(row))),
        }
    }

    /// The shown agent's own row for `row`, an audit row, when the snapshot carries it.
    fn shown_row<'r>(&'r self, row: &'r AgentState) -> &'r AgentState {
        crate::store::snapshot::find_agent(self.rows(), &row.kind, &row.agent_id).unwrap_or(row)
    }

    fn push_agent(&mut self, group: Group, row: &AgentState, stop: bool) {
        if !self.seen.insert(agent_key(row)) {
            return;
        }
        let entry = self.agent_row(row, self.path(row), stop);
        self.group(group).push(entry);
    }

    fn team_row(&self, instance: &TeamInstanceId, status: String, kind: RowKind) -> StartedRow {
        StartedRow {
            kind,
            name: instance.to_string(),
            status,
            tone: AgentStatus::Running,
            phase: TurnPhase::Idle,
            provider: None,
            since: None,
            due: None,
            detail: Vec::new(),
            command: instance
                .lane()
                .is_some()
                .then(|| format!("{} teams stop {instance}", self.seat.rimz)),
        }
    }

    fn push_team(&mut self, group: Group, instance: &TeamInstanceId, status: String) {
        self.push_team_named(group, instance, instance.to_string(), status);
    }

    fn push_team_named(
        &mut self,
        group: Group,
        instance: &TeamInstanceId,
        name: String,
        status: String,
    ) {
        if !self.seen.insert(team_key(instance)) {
            return;
        }
        let row = StartedRow {
            name,
            ..self.team_row(instance, status, RowKind::Team)
        };
        self.group(group).push(row);
    }

    fn group(&mut self, group: Group) -> &mut Vec<StartedRow> {
        match group {
            Group::Holding => &mut self.started.holding,
            Group::Closing => &mut self.started.closing,
            Group::Outliving => &mut self.started.outliving,
        }
    }

    /// The holding group, in `decide`'s order: the rollup's terms, the owed wake, messages,
    /// runs, the tree, and the board.
    fn holding(&mut self, store: &Store) -> Result<(), StoreErr> {
        let agent = self.agent;
        let now = self.now;
        if agent.is_awaiting_input() {
            self.started.holding.push(StartedRow {
                kind: RowKind::Question,
                name: "open question".to_owned(),
                status: AgentStatus::Waiting.as_str().to_owned(),
                tone: AgentStatus::Waiting,
                phase: TurnPhase::Idle,
                provider: None,
                since: agent.waiting_since,
                due: None,
                detail: Vec::new(),
                command: None,
            });
        }
        if let Some(park) = &agent.budget_park {
            self.started.holding.push(StartedRow {
                kind: RowKind::Budget,
                name: "budget".to_owned(),
                status: "parked".to_owned(),
                tone: AgentStatus::Paused,
                phase: TurnPhase::Idle,
                provider: None,
                since: Some(park.at),
                due: park.resets_at,
                detail: vec![park.label()],
                command: None,
            });
        }
        self.waits(store);
        for shell in &agent.background_shells {
            self.started.holding.push(StartedRow {
                kind: RowKind::Shell,
                name: shell.id.clone(),
                status: AgentStatus::Running.as_str().to_owned(),
                tone: AgentStatus::Running,
                phase: TurnPhase::Idle,
                provider: None,
                since: Some(shell.started_at),
                due: None,
                detail: shell
                    .command
                    .iter()
                    .chain(&shell.description)
                    .cloned()
                    .collect(),
                command: None,
            });
        }
        if rests(agent, now) == Err(Hold::Busy) {
            self.turn();
            self.natives();
        }
        let runs = crate::harness::run::listing(store.paths())?;
        self.owed(store, &runs)?;
        let messages = store.list_messages()?;
        let (conversation, system): (Vec<_>, Vec<_>) = messages
            .iter()
            .filter(|message| open_message(message, agent))
            .partition(|message| message.sender.is_conversation());
        for message in conversation {
            self.started.holding.push(StartedRow {
                kind: RowKind::Message,
                name: message.message_id.to_string(),
                status: message.status.as_str().to_owned(),
                tone: AgentStatus::Sleeping,
                phase: TurnPhase::Idle,
                provider: None,
                since: Some(message.enqueued_at),
                due: message.not_before,
                detail: Vec::new(),
                // A message on its way into the pane is past canceling.
                command: message
                    .status
                    .is_open()
                    .then(|| format!("{} message cancel {}", self.seat.rimz, message.message_id)),
            });
        }
        // RimZ's own text stays behind one count, as `agents show` lists it.
        if let Some(first) = system.iter().map(|message| message.enqueued_at).min() {
            let plural = if system.len() == 1 { "" } else { "s" };
            self.started.holding.push(StartedRow {
                kind: RowKind::Message,
                name: format!("{} system message{plural}", system.len()),
                status: "queued".to_owned(),
                tone: AgentStatus::Sleeping,
                phase: TurnPhase::Idle,
                provider: None,
                since: Some(first),
                due: None,
                detail: Vec::new(),
                command: None,
            });
        }
        for run in runs.iter().filter(|run| open_run(run, agent)) {
            self.started.holding.push(StartedRow {
                kind: RowKind::Run,
                name: run.run_id.to_string(),
                status: run.status.as_str().to_owned(),
                tone: AgentStatus::Running,
                phase: TurnPhase::Idle,
                provider: None,
                since: Some(run.started_at),
                due: None,
                detail: Vec::new(),
                command: None,
            });
        }
        let mut closing = Vec::new();
        for child in closing_with(self.rows(), agent) {
            if resting(child, now).is_err() || debt(store, &messages, &runs, child)?.is_some() {
                self.push_agent(Group::Holding, child, true);
            } else {
                closing.push(child);
            }
        }
        if agent.is_team_seat()
            && !board_done(agent)
            && let Some(instance) = agent.team_instance()
        {
            let status = agent
                .worktree_path
                .as_deref()
                .and_then(|root| board_status(Path::new(root)))
                .unwrap_or_else(|| "no board".to_owned());
            // A seat's own board ends at Done; stopping its team is no way to release it.
            let row = StartedRow {
                command: None,
                ..self.team_row(&instance, status, RowKind::Board)
            };
            self.started.holding.push(row);
        }
        for child in closing {
            self.push_agent(Group::Closing, child, false);
        }
        Ok(())
    }

    /// Each pending wait, as the agent or team it waits on where it names one.
    /// The snapshot's waits and those armed in the catalog, which `decide` reads whether or
    /// not the snapshot carries them.
    fn waits(&mut self, store: &Store) {
        let agent = self.agent;
        let rows: Vec<&AgentState> = self.rows().iter().collect();
        let armed = SessionWaits::load(store.paths()).of(&rows, agent);
        let catalog = armed.iter().filter(|wait| {
            !agent
                .pending_waits
                .iter()
                .any(|shown| shown.name == wait.name)
        });
        for wait in agent.pending_waits.iter().chain(catalog) {
            match &wait.trigger {
                PendingWaitTrigger::Subagent {
                    target: Some(target),
                    ..
                } => {
                    if let Some(row) =
                        crate::address::launch_row(self.rows(), &target.kind, &target.agent_id)
                    {
                        self.push_agent(Group::Holding, row, true);
                        continue;
                    }
                }
                PendingWaitTrigger::Team { .. } => {
                    if let Ok(instance) = wait.name.parse::<TeamInstanceId>() {
                        let status = self.cohort_status(&instance);
                        self.push_team(Group::Holding, &instance, status);
                        continue;
                    }
                }
                _ => {}
            }
            self.started.holding.push(StartedRow {
                kind: RowKind::Wait,
                name: wait.name.clone(),
                status: AgentStatus::Sleeping.as_str().to_owned(),
                tone: AgentStatus::Sleeping,
                phase: TurnPhase::Idle,
                provider: None,
                since: wait.armed_at,
                due: None,
                detail: vec![wait.label(self.now)],
                command: self
                    .seat
                    .you
                    .then(|| format!("{} wait cancel {}", self.seat.rimz, wait.name)),
            });
        }
    }

    /// A failed or paused turn at rest, which only a new prompt ends.
    fn turn(&mut self) {
        let agent = self.agent;
        let tone = agent.effective_status(self.now);
        if !matches!(tone, AgentStatus::Failed | AgentStatus::Paused) {
            return;
        }
        let class = agent
            .displayed_turn_error(self.now)
            .map(|(class, _)| class.words());
        self.started.holding.push(StartedRow {
            kind: RowKind::Turn,
            name: "last turn".to_owned(),
            status: tone.as_str().to_owned(),
            tone,
            phase: agent.phase,
            provider: None,
            since: agent.turn_ended_at,
            due: None,
            detail: class.into_iter().map(str::to_owned).collect(),
            command: None,
        });
    }

    /// Provider-native subagents still running under the agent's own turn.
    fn natives(&mut self) {
        let agent = self.agent;
        let now = self.now;
        let rows: Vec<StartedRow> = self
            .rows()
            .iter()
            .filter(|row| {
                row.is_provider_subagent()
                    && row.ended_at.is_none()
                    && row.parent_is(agent)
                    && row.effective_status(now) == AgentStatus::Running
            })
            .map(|row| StartedRow {
                kind: RowKind::Native,
                name: row.name.clone().unwrap_or_else(|| row.agent_id.to_string()),
                status: AgentStatus::Running.as_str().to_owned(),
                tone: AgentStatus::Running,
                phase: TurnPhase::Idle,
                provider: None,
                since: row.turn_started_at.or(Some(row.last_activity)),
                due: None,
                detail: Vec::new(),
                command: None,
            })
            .collect();
        self.started.holding.extend(rows);
    }

    /// The launched members and teams the owed wake's fleet and team terms hold on.
    fn owed(&mut self, store: &Store, runs: &[RunRecord]) -> Result<(), StoreErr> {
        let audit = self.audit;
        let Some(launcher) = audit.find(self.agent.kind.as_str(), &self.agent.agent_id) else {
            return Ok(());
        };
        for (member, _) in owing_members(audit.iter(), runs, launcher) {
            // The launch's live holder, which a molt may have moved to another kind.
            let row = crate::address::launch_row(self.rows(), &member.kind, member.launch_key())
                .unwrap_or_else(|| self.shown_row(member))
                .clone();
            self.push_agent(Group::Holding, &row, true);
        }
        for run in owing_teams(audit.iter(), runs, launcher) {
            let Some(team) = &run.team else { continue };
            let status =
                board_status(&run.worktree_path).unwrap_or_else(|| run.status.as_str().to_owned());
            self.push_team(Group::Holding, &team.instance, status);
        }
        for (id, header) in live_runs(store)? {
            if owes_report_to(&header, launcher) {
                self.push_workflow(Group::Holding, &id, &header);
            }
        }
        Ok(())
    }

    /// A workflow run the agent started, cancelable while it has not ended.
    fn push_workflow(&mut self, group: Group, id: &str, header: &RunHeader) {
        if !self.seen.insert(workflow_key(id)) {
            return;
        }
        let ended = header.state.is_some_and(|state| state.is_terminal());
        let status = header.state.map_or("unknown", |state| state.as_str());
        let row = StartedRow {
            kind: RowKind::Workflow,
            name: id.to_owned(),
            status: status.to_owned(),
            tone: AgentStatus::Running,
            phase: TurnPhase::Idle,
            provider: None,
            since: header.created_at,
            due: None,
            detail: header
                .workflow
                .iter()
                .cloned()
                .chain(ended.then(|| "report owed".to_owned()))
                .collect(),
            command: (!ended).then(|| format!("{} workflow cancel {id}", self.seat.rimz)),
        };
        self.group(group).push(row);
    }

    /// Live runs the agent started that owe it nothing, started with `--detach`.
    fn detached_runs(&mut self, store: &Store) {
        let Ok(runs) = live_runs(store) else {
            return;
        };
        for (id, header) in runs {
            let started = header
                .starter
                .as_ref()
                .is_some_and(|starter| starter.is_agent(self.agent));
            if started && header.state.is_some_and(|state| !state.is_terminal()) {
                self.push_workflow(Group::Outliving, &id, &header);
            }
        }
    }

    fn cohort_status(&self, instance: &TeamInstanceId) -> String {
        crate::address::team_cohorts(self.rows())
            .into_iter()
            .find(|cohort| &cohort.instance == instance)
            .and_then(|cohort| {
                cohort
                    .members
                    .iter()
                    .find_map(|member| board_status(Path::new(member.worktree_path.as_deref()?)))
            })
            .unwrap_or_else(|| "no board".to_owned())
    }

    /// Peers down the launch chain with the teams they launched, then booked work. The walk
    /// goes on through an ended peer, which the log keeps after the snapshot drops it, and
    /// through a team's seats, which print as their team.
    fn outliving(&mut self, store: &Store) {
        let mut visited = HashSet::new();
        let mut launchers = VecDeque::from([self.agent]);
        while let Some(launcher) = launchers.pop_front() {
            if !visited.insert(agent_key(launcher)) {
                continue;
            }
            let launched: Vec<&'a AgentState> = self
                .rows()
                .iter()
                .chain(self.audit.iter())
                .filter(|row| {
                    !row.is_launched_child()
                        && !row.is_provider_subagent()
                        && row.launcher_is(launcher)
                })
                .collect();
            for row in launched {
                launchers.push_back(row);
                if row.is_team_seat() {
                    self.push_outliving_team(row, &mut launchers);
                    continue;
                }
                let live = crate::address::launch_row(self.rows(), &row.kind, row.launch_key())
                    .filter(|live| live.ended_at.is_none());
                if let Some(live) = live {
                    self.push_agent(Group::Outliving, live, true);
                }
            }
        }
        self.booked_tasks(store);
        self.detached_runs(store);
        self.booked_launches();
    }

    /// A live seat's team under its launcher's path, with every seat of its cohort to walk.
    fn push_outliving_team(
        &mut self,
        seat: &'a AgentState,
        launchers: &mut VecDeque<&'a AgentState>,
    ) {
        let Some(instance) = seat.team_instance().filter(|_| seat.ended_at.is_none()) else {
            return;
        };
        let rows = self.rows();
        let cohort = crate::address::team_cohorts(rows)
            .into_iter()
            .find(|cohort| cohort.instance == instance);
        launchers.extend(cohort.into_iter().flat_map(|cohort| cohort.members));
        let mut names = self.path_names(seat);
        names.pop();
        names.push(instance.to_string());
        let status = self.cohort_status(&instance);
        self.push_team_named(
            Group::Outliving,
            &instance,
            names.join(PATH_SEPARATOR),
            status,
        );
    }

    /// The handles of a booking chain's links before the shown agent, outermost first.
    fn booking_path(&self, chain: &[LaunchedBy]) -> Option<Vec<String>> {
        let at = chain.iter().position(|link| names(link, self.agent))?;
        Some(
            chain[..at]
                .iter()
                .rev()
                .map(|link| {
                    crate::address::launch_row(self.rows(), &link.kind, &link.agent_id)
                        .map_or_else(|| format!("@{}", link.agent_id), |row| self.handle(row))
                })
                .collect(),
        )
    }

    /// The room's loop tasks whose booking chain names the agent, each with its live workflow
    /// runs, then the live runs such a task fired whose row is gone.
    fn booked_tasks(&mut self, store: &Store) {
        let catalog = TaskCatalog::load_room_lenient(self.seat.root, store.paths());
        let stamps = fire::last_stamps(store.runtime_paths());
        let records = task_record::load().unwrap_or_default();
        let mut runs: Vec<_> = crate::store::workflow::live_runs(store.paths())
            .unwrap_or_default()
            .into_iter()
            .filter(|run| {
                !run.state.is_terminal() && !self.seen.contains(&workflow_key(run.id.as_str()))
            })
            .filter_map(|run| {
                let path = self.booking_path(run.fire.as_ref()?.booked_by())?;
                Some((run, path))
            })
            .collect();
        for (run, _) in &runs {
            self.seen.insert(workflow_key(run.id.as_str()));
        }
        let zone = MachineConfig::load_lenient().time_zone();
        let now = self.now.to_zoned(zone);
        for (name, task) in catalog.visible() {
            let entry = task.entry();
            if entry.wait.is_some() || entry.resolved_root() != self.seat.root {
                continue;
            }
            let Some(mut path) = self.booking_path(&entry.booked_by) else {
                continue;
            };
            path.push(name.clone());
            let key = task.key_for_root(name, self.seat.root);
            let timing = TaskTiming::evaluate(
                task.trigger(),
                task.source(),
                attention::stamp(&stamps, &records, &key, name).copied(),
                records
                    .live(&key)
                    .and_then(|(_, record)| record.arming.as_ref()),
                &now,
            );
            let (status, due) = match timing.state() {
                TaskTimingState::Upcoming(at) | TaskTimingState::Due(at) => ("fires", Some(at)),
                TaskTimingState::Disabled(_) => ("disabled", None),
                TaskTimingState::Paused(_) => ("paused", None),
                _ => ("armed", None),
            };
            let trigger = task.trigger().as_ref().ok();
            let (own, rest): (Vec<_>, Vec<_>) = runs
                .into_iter()
                .partition(|(run, _)| run.task.as_deref() == Some(name.as_str()));
            runs = rest;
            let action = match task.action() {
                Ok(TaskAction::Shell(argv)) => Some(argv.join(" ")),
                Ok(TaskAction::Launch(launch)) => Some(launch.target.clone()),
                Ok(TaskAction::Deliver(_)) | Err(_) => None,
            };
            self.started.outliving.push(StartedRow {
                kind: RowKind::Booked,
                name: path.join(PATH_SEPARATOR),
                status: status.to_owned(),
                tone: AgentStatus::Sleeping,
                phase: TurnPhase::Idle,
                provider: None,
                since: None,
                due,
                detail: trigger
                    .map(|trigger| trigger.describe())
                    .into_iter()
                    .chain(action)
                    .chain(own.iter().map(|(run, _)| format!("run {}", run.id)))
                    .collect(),
                command: Some(format!("{} loop remove {name}", self.seat.rimz)),
            });
        }
        for (run, mut path) in runs {
            path.push(run.id.to_string());
            self.started.outliving.push(StartedRow {
                kind: RowKind::Workflow,
                name: path.join(PATH_SEPARATOR),
                status: run.state.as_str().to_owned(),
                tone: AgentStatus::Running,
                phase: TurnPhase::Idle,
                provider: None,
                since: Some(run.created_at),
                due: None,
                detail: run.task.into_iter().collect(),
                command: Some(format!("{} workflow cancel {}", self.seat.rimz, run.id)),
            });
        }
    }

    /// Live agents a task booked by the agent, or by one it started, launched.
    fn booked_launches(&mut self) {
        let rows = self.rows();
        let booked: Vec<(&AgentState, Vec<String>)> = rows
            .iter()
            .filter(|row| row.ended_at.is_none() && !row.is_provider_subagent())
            .filter_map(|row| Some((row, self.booking_path(&row.loop_booked_by)?)))
            .collect();
        for (row, mut path) in booked {
            if let Some(instance) = row.team_instance() {
                let status = self.cohort_status(&instance);
                path.push(instance.to_string());
                self.push_team_named(
                    Group::Outliving,
                    &instance,
                    path.join(PATH_SEPARATOR),
                    status,
                );
                continue;
            }
            if !self.seen.insert(agent_key(row)) {
                continue;
            }
            path.push(self.handle(row));
            let entry = self.agent_row(row, path.join(PATH_SEPARATOR), true);
            self.started.outliving.push(entry);
        }
    }
}

#[derive(Clone, Copy)]
enum Group {
    Holding,
    Closing,
    Outliving,
}
