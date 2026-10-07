//! Team lifecycle reactions and stage transitions: subscription retirement and arming, lifecycle signals, locked board edits, owner delivery, and hand-off compaction; the typed `StageSignal` payload and `stage_flips`, the read side `rimz transcript` renders.

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tracing::{debug, warn};

use crate::Store;
use crate::agents::{AgentState, LifecycleSignal};
use crate::config::{DONE_STAGE, MachineConfig, Team};
use crate::ids::{AgentKind, AgentSessionId, EventId, MessageId, MuxName, PaneId};
use crate::message::compact::{self, CompactErr, CompactOutcome, CompactRequest};
use crate::message::dispatch::{self, DispatchMode, DispatchOutcome, DispatchRequest};
use crate::store::event::{EventKind, SignalSource};
use crate::store::message::{
    AutoCompact, DeliveryGate, HarnessNotice, MessageBody, MessageRecord, MessageSender,
    MessageStatus,
};
use crate::store::snapshot::find_agent;
use crate::store::writer::AgentLifecycleReceipt;
use crate::workspace::ResolvedWorkspace;

use super::assist_log::{self, Assist, AssistRecord};
use super::board::{BoardErr, LockedBoard, rewrite_board, stamp};
use super::schedule::signal::{Signal, fire_signal, lifecycle_signal, team_lifecycle_signals};
use super::schedule::{arm, catalog::TaskCatalog, pending::pending_waits_by_session, team};
use super::scratch::parse_board_stage;

const STAGE_SIGNAL: &str = "team.stage";
const REWAKE_BY: &str = "rimz";

/// The `team.stage` signal payload one stage opening appends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageSignal {
    pub team: String,
    /// `<team>#<channel>`.
    pub instance: String,
    pub to: String,
    /// The flipping role, `user`, or `rimz` for a registration re-wake.
    pub by: String,
    pub board: PathBuf,
    pub at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl StageSignal {
    pub fn channel(&self) -> &str {
        self.instance
            .split_once('#')
            .map_or("", |(_, channel)| channel)
    }

    /// A registration re-wake reopens the current stage without a flip.
    fn is_rewake(&self) -> bool {
        self.by == REWAKE_BY
    }

    fn payload(&self) -> Map<String, Value> {
        match serde_json::to_value(self) {
            Ok(Value::Object(payload)) => payload,
            // A struct of strings, a path, and a timestamp always serializes to an object.
            _ => unreachable!("stage signal serializes to a JSON object"),
        }
    }
}

/// Every stage flip in the workspace's active event log, oldest first.
/// Re-wakes are skipped; flips rotated into the event-log archive are not read.
pub fn stage_flips(store: &Store) -> Result<Vec<StageSignal>, crate::store::StoreErr> {
    let mut flips = store
        .read_events()?
        .iter()
        .filter_map(|event| match event.kind() {
            EventKind::Signal(signal) if signal.name.as_str() == STAGE_SIGNAL => {
                serde_json::from_value::<StageSignal>(Value::Object(signal.payload)).ok()
            }
            _ => None,
        })
        .filter(|stage| !stage.is_rewake())
        .collect::<Vec<_>>();
    flips.sort_by_key(|stage| stage.at);
    Ok(flips)
}

pub enum Flipper<'a> {
    Member {
        role: &'a str,
        agent: &'a AgentState,
        pane: Option<PaneId>,
    },
    User,
}

impl Flipper<'_> {
    fn name(&self) -> &str {
        match self {
            Self::Member { role, .. } => role,
            Self::User => "user",
        }
    }
}

pub struct FlipRequest<'a> {
    pub workspace: &'a ResolvedWorkspace,
    pub store: &'a Store,
    pub team_name: &'a str,
    pub team: &'a Team,
    pub channel: &'a str,
    pub worktree: &'a Path,
    pub members: &'a [AgentState],
    pub to: &'a str,
    pub note: &'a str,
    pub flip_compact: Option<AutoCompact>,
    pub by: Flipper<'a>,
    pub mux: Option<MuxName>,
    pub now: Timestamp,
}

#[derive(Debug)]
pub struct FlipReceipt {
    pub board: PathBuf,
    pub from: Option<String>,
    pub to: String,
    pub owner: Option<String>,
    pub delivery: Delivery,
    pub compaction: Compaction,
    pub signal_event: EventId,
}

#[derive(Debug)]
pub enum Delivery {
    Sent { label: String },
    Queued { label: String },
    OwnerNotLive { owner: String },
    SelfOwned,
    Terminal,
}

#[derive(Debug)]
pub enum Compaction {
    NotConfigured,
    Ineligible,
    BelowThreshold,
    Sent {
        message_id: MessageId,
        occupied_tokens: u64,
        threshold: u64,
    },
    Queued {
        message_id: MessageId,
        occupied_tokens: u64,
        threshold: u64,
    },
    Skipped {
        reason: String,
    },
}

#[derive(Debug, thiserror::Error)]
enum FlipCompactErr {
    #[error("{0}")]
    Unavailable(String),
    #[error(transparent)]
    Compact(#[from] CompactErr),
}

#[derive(Debug, thiserror::Error)]
pub enum FlipErr {
    #[error("team `{team}` has no stage owners; add owns = [\"Stage\"] to its roles")]
    NoOwners { team: String },
    #[error("unknown stage `{to}`; choose one of {declared:?} or Done")]
    UnknownStage { to: String, declared: Vec<String> },
    #[error("stage `{to}` has no owner; add it to a role's owns list")]
    UnownedStage { to: String },
    #[error(
        "worktree {} has uncommitted changes: {}; commit or discard them, then flip again",
        worktree.display(),
        paths.join(", ")
    )]
    DirtyWorktree {
        worktree: PathBuf,
        paths: Vec<String>,
    },
    #[error("cannot read git status of {}: {stderr}", worktree.display())]
    GitStatus { worktree: PathBuf, stderr: String },
    #[error(transparent)]
    Config(#[from] super::spec::LayoutErr),
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Board(#[from] BoardErr),
    #[error(transparent)]
    Store(#[from] crate::store::StoreErr),
    #[error(transparent)]
    Dispatch(#[from] dispatch::DispatchErr),
    #[error(
        "completed {done}; {source}; re-run `rimz teams flip {to} \"<progress note>\"` to repeat the signal and delivery"
    )]
    PartialFlip {
        done: &'static str,
        to: String,
        #[source]
        source: Box<FlipErr>,
    },
}

impl FlipErr {
    fn after(self, done: &'static str, to: &str) -> Self {
        Self::PartialFlip {
            done,
            to: to.to_owned(),
            source: Box::new(self),
        }
    }
}

pub fn flip(request: FlipRequest<'_>) -> Result<FlipReceipt, FlipErr> {
    let owner = resolve_owner(request.team_name, request.team, request.to)?;
    let locked = LockedBoard::open(request.store, request.worktree)?;
    let worktree = locked.worktree.clone();
    let board = locked.path.clone();
    let text = &locked.text;
    let from = parse_board_stage(text).map(|stage| stage.name);
    if hands_off(&request.by, request.team, from.as_deref(), owner) {
        let paths = uncommitted_paths(&worktree)?;
        if !paths.is_empty() {
            return Err(FlipErr::DirtyWorktree { worktree, paths });
        }
    }
    let ledger = ledger_line(
        request.now,
        request.by.name(),
        from.as_deref(),
        request.to,
        request.note,
    );
    let rewritten = rewrite_board(text, request.to, owner, &ledger);
    locked.write(&rewritten)?;
    let opening = StageOpening {
        workspace: request.workspace,
        store: request.store,
        team_name: request.team_name,
        channel: request.channel,
        members: request.members,
        board: &board,
        from: from.as_deref(),
        to: request.to,
        owner,
        leader: request.team.leader.as_deref(),
        by: request.by.name(),
        self_owned: matches!(&request.by, Flipper::Member { role, .. } if Some(*role) == owner),
        note: Some(request.note),
        mux: request.mux,
        now: request.now,
        rewake: false,
    };
    let (signal_event, delivery) = open_stage(&opening, "board")?;
    let compaction = compact_flipper(&request, from.as_deref(), owner);
    Ok(FlipReceipt {
        board,
        from,
        to: request.to.to_owned(),
        owner: owner.map(str::to_owned),
        delivery,
        compaction,
        signal_event,
    })
}

/// What re-wakes a stage owner, which decides the Stage notices that already cover it.
#[derive(Clone, Copy, Debug)]
enum RewakeCause {
    /// The exec wrapper resumed the member's session; `since` is captured before the
    /// provider could start, so a notice delivered from then on belongs to this attempt.
    Resume { since: Timestamp },
    /// The member's root session registered, a resume or a fresh start alike.
    Registration,
}

impl RewakeCause {
    /// Whether a Stage notice already wakes `member`: one still in the live queue, or one
    /// delivered to it since the resume attempt began (a provider can register and take its
    /// notice before the wrapper's own check) or, for a registration, within a delivery window
    /// of `now` (Codex registers in the same turn a typed notice starts). A resume ignores a
    /// notice from an earlier attempt, so a member that dies again right after it is re-woken.
    fn already_woken(
        self,
        live: &[MessageRecord],
        history: &[MessageRecord],
        member: &AgentState,
        now: Timestamp,
    ) -> bool {
        let stage_notice = |record: &&MessageRecord| {
            record.sender
                == MessageSender::Harness {
                    notice: HarnessNotice::Stage,
                }
                && record.same_agent_card(member)
        };
        if live.iter().any(|record| stage_notice(&record)) {
            return true;
        }
        let cutoff = match self {
            Self::Resume { since } => since,
            Self::Registration => now - MessageBody::Prompt.delivery_window(),
        };
        history.iter().filter(stage_notice).any(|record| {
            record.status == MessageStatus::Delivered
                && record
                    .delivered_at
                    .is_some_and(|delivered| delivered >= cutoff)
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn rewake(
    workspace: &ResolvedWorkspace,
    store: &Store,
    team_name: &str,
    team: &Team,
    member: &AgentState,
    members: &[AgentState],
    worktree: &Path,
    mux: Option<MuxName>,
    now: Timestamp,
    cause: RewakeCause,
) -> Result<Option<FlipReceipt>, FlipErr> {
    if team.owned_stages().next().is_none() {
        return Ok(None);
    }
    // The board lock serializes the check below with the enqueue, so a wrapper and a
    // registration racing for one member cannot both find it un-woken.
    let locked = LockedBoard::open(store, worktree)?;
    let board = locked.path.clone();
    let text = &locked.text;
    let Some(stage) = parse_board_stage(text) else {
        return Ok(None);
    };
    if stage.name == DONE_STAGE
        || team.owner_of(&stage.name) != member.role.as_deref()
        || member.role.is_none()
    {
        return Ok(None);
    }
    let (live, history) = store.list_messages_and_history()?;
    if cause.already_woken(&live, &history, member, now) {
        return Ok(None);
    }
    let owner = resolve_owner(team_name, team, &stage.name)?;
    let channel = member.channel().unwrap_or_else(|| "external".to_owned());
    let (signal_event, delivery) = open_stage(
        &StageOpening {
            workspace,
            store,
            team_name,
            channel: &channel,
            members,
            board: &board,
            from: Some(&stage.name),
            to: &stage.name,
            owner,
            leader: team.leader.as_deref(),
            by: REWAKE_BY,
            self_owned: false,
            note: None,
            mux,
            now,
            rewake: true,
        },
        match cause {
            RewakeCause::Resume { .. } => "resume",
            RewakeCause::Registration => "registration",
        },
    )?;
    Ok(Some(FlipReceipt {
        board,
        from: Some(stage.name.clone()),
        to: stage.name,
        owner: owner.map(str::to_owned),
        delivery,
        compaction: Compaction::Ineligible,
        signal_event,
    }))
}

/// Re-wake a resumed root team member whose role owns the board's open stage, unless a Stage
/// notice for its card is still queued or was delivered at or after `since`, which the exec
/// wrapper captures before its provider could start. The wrapper calls it on every resume,
/// after the resume stamp has revived the card; a team that no longer loads is logged and
/// re-wakes nobody.
pub fn rewake_resumed(
    workspace: &ResolvedWorkspace,
    store: &Store,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
    mux: Option<MuxName>,
    now: Timestamp,
    since: Timestamp,
) -> Result<Option<FlipReceipt>, FlipErr> {
    let audit = store.runtime_projection(crate::store::runtime::RuntimeScope::Audit)?;
    let Some(member) =
        find_agent(&audit.agents, kind, agent_id).filter(|member| member.parent_agent_id.is_none())
    else {
        return Ok(None);
    };
    let (Some(name), Some(worktree)) = (member.team.as_deref(), member.worktree_path.as_deref())
    else {
        return Ok(None);
    };
    let team = match load_member_team(workspace, name) {
        Ok(team) => team,
        Err(err) => {
            warn!(error = %err, "resume: failed to load team configuration");
            return Ok(None);
        }
    };
    let members = cohort_members(&audit.agents, member)
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    rewake(
        workspace,
        store,
        name,
        &team,
        member,
        &members,
        Path::new(worktree),
        mux,
        now,
        RewakeCause::Resume { since },
    )
}

/// The live members of `member`'s team cohort on its channel.
fn cohort_members<'a>(agents: &'a [AgentState], member: &AgentState) -> Vec<&'a AgentState> {
    let channel = member.channel().unwrap_or_else(|| "external".to_owned());
    crate::address::team_cohorts(agents)
        .into_iter()
        .find(|cohort| Some(cohort.team) == member.team.as_deref() && cohort.channel == channel)
        .map(|cohort| cohort.members)
        .unwrap_or_default()
}

/// React to a committed lifecycle receipt: retire subscriptions, arm registered members, fire signals, and re-wake stage owners.
pub fn react_to_lifecycle(
    workspace: &ResolvedWorkspace,
    store: &Store,
    receipt: &AgentLifecycleReceipt,
    mux: Option<MuxName>,
) {
    // The receipt's commit publishes and runs the session reaper only when it
    // appended an event, and it appends one exactly when the receipt carries a
    // primary id or a derived event. So an observation with neither found no
    // end that was not already there, and pays for neither the projection nor
    // the reconcile below: the repeat side-conversation hook and the read-only
    // tool use, the two frequent ones, both land here.
    if receipt.primary_event_id.is_none() && receipt.events.is_empty() {
        return;
    }
    let audit = store
        .runtime_projection(crate::store::runtime::RuntimeScope::Audit)
        .inspect_err(|err| {
            warn!(error = %err, "lifecycle: failed to read team member state");
        })
        .ok();
    // Every durable end in this workspace retires its rows here, whichever
    // producer stamped it: the store reaper, the exec wrapper, and rebirth
    // append `Ended` without ever reaching a hook. It sits above the side
    // conversation's return because that conversation's own first hook appends
    // its registration, and that commit reaps like any other.
    if let Some(audit) = &audit
        && let Err(err) = arm::retire_ended_sessions(&workspace.project_root, || {
            std::borrow::Cow::Borrowed(&audit.agents)
        })
    {
        warn!(error = %err, "lifecycle: failed to retire ended session deliveries");
    }
    if receipt.side_conversation.is_some() {
        return;
    }
    let pending = store
        .list_pending_messages()
        .inspect_err(|err| {
            warn!(error = %err, "lifecycle: failed to read team signal state");
        })
        .ok();
    for event in &receipt.events {
        if matches!(event.signal, LifecycleSignal::Ended | LifecycleSignal::Lost)
            && let Err(err) = arm::retire_session(
                &workspace.project_root,
                &event.kind,
                &event.agent_id,
                arm::RetireScope::Session,
            )
        {
            warn!(error = %err, "lifecycle: failed to retire session deliveries");
        }
        let member = audit
            .as_ref()
            .and_then(|audit| find_agent(&audit.agents, &event.kind, &event.agent_id));
        let registered_member = member.filter(|member| {
            matches!(event.signal, LifecycleSignal::Registered)
                && event.parent_agent_id.is_none()
                && member.parent_agent_id.is_none()
        });
        let registered_team = registered_member.and_then(|member| {
            load_member_team(workspace, member.team.as_deref()?)
                .inspect_err(|err| {
                    warn!(error = %err, "lifecycle: failed to load team configuration");
                })
                .ok()
        });
        if let (Some(audit), Some(member)) = (&audit, registered_member)
            && let Some(name) = member.loop_task.as_deref()
            && let Some(task) =
                TaskCatalog::load_lenient(Some(&workspace.project_root)).for_run(name)
            && let Err(err) = team::arm_loop(
                workspace,
                &audit.agents,
                member,
                name,
                &task.entry().subscribe,
            )
        {
            warn!(task = %name, error = %err, "lifecycle: failed to arm loop signal bindings");
        }
        let member = member.filter(|member| member.team.is_some());
        if let (Some(audit), Some(member), Some(team)) =
            (&audit, registered_member, registered_team.as_ref())
            && let Err(err) = team::arm_member(workspace, &audit.agents, member, team)
        {
            warn!(error = %err, "lifecycle: failed to arm team signal bindings");
        }
        let mut signals: Vec<_> = lifecycle_signal(event).into_iter().collect();
        let live_members = audit
            .as_ref()
            .zip(member)
            .map(|(audit, member)| cohort_members(&audit.agents, member))
            .unwrap_or_default();
        if let (Some(member), Some(pending)) = (member, &pending) {
            let sleeping = pending_waits_by_session(
                &TaskCatalog::load_lenient(Some(&workspace.project_root)),
                &workspace.project_root,
                &event.at.to_zoned(MachineConfig::load_lenient().time_zone()),
            )
            .into_keys()
            .collect();
            for signal in team_lifecycle_signals(event, member, &live_members, pending, &sleeping) {
                match store.append_signal(&workspace.session_name, (&signal).into()) {
                    Ok(_) => signals.push(signal),
                    Err(err) => {
                        warn!(signal = %signal.name, error = %err, "lifecycle: failed to append team signal")
                    }
                }
            }
        }
        for signal in signals {
            if let Err(err) = fire_signal(store.runtime_paths(), &workspace.project_root, &signal) {
                warn!(
                    signal = %signal.name,
                    error = %err,
                    "lifecycle: failed to fire matching loop tasks",
                );
            }
        }
        if let (Some(member), Some(team)) = (registered_member, registered_team.as_ref())
            && let (Some(name), Some(worktree)) =
                (member.team.as_deref(), member.worktree_path.as_deref())
        {
            let members = live_members
                .iter()
                .map(|member| (*member).clone())
                .collect::<Vec<_>>();
            match rewake(
                workspace,
                store,
                name,
                team,
                member,
                &members,
                Path::new(worktree),
                mux,
                event.at,
                RewakeCause::Registration,
            ) {
                Ok(Some(receipt)) => {
                    debug!(delivery = ?receipt.delivery, "lifecycle: re-woke team stage owner");
                }
                Ok(None) => {}
                Err(err) => {
                    warn!(error = %err, "lifecycle: failed to re-wake team stage owner");
                }
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum MemberTeamErr {
    #[error(transparent)]
    Machine(#[from] crate::config::ConfigErr),
    #[error(transparent)]
    Effective(#[from] crate::config::effective::EffectiveConfigErr),
}

pub(super) fn load_member_team(
    workspace: &ResolvedWorkspace,
    name: &str,
) -> Result<Team, MemberTeamErr> {
    let machine = MachineConfig::load()?;
    let effective = crate::config::effective::load(&machine, &workspace.project_root)?;
    effective.block_untrusted_reference(
        crate::config::effective::ProfileScope::Agents,
        Some(name),
        &machine.agents.commands,
    )?;
    let team = effective.teams.0.get(name).ok_or_else(|| {
        crate::config::effective::EffectiveConfigErr::FailedDefinition(
            machine
                .definition_failure_for(name)
                .unwrap_or_else(|| format!("team `{name}` is no longer configured")),
        )
    })?;
    Ok(team.clone())
}

/// The pipeline as every human surface prints it: `A → [current] → Done`, terminal stage included.
pub fn stage_strip(stages: &[String], current: Option<&str>) -> String {
    stages
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(DONE_STAGE))
        .map(|stage| {
            if current == Some(stage) {
                format!("[{stage}]")
            } else {
                stage.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" → ")
}

fn resolve_owner<'a>(
    team_name: &str,
    team: &'a Team,
    to: &str,
) -> Result<Option<&'a str>, FlipErr> {
    super::spec::validate_team_stages(team_name, team)?;
    if team.owned_stages().next().is_none() {
        return Err(FlipErr::NoOwners {
            team: team_name.to_owned(),
        });
    }
    if to == DONE_STAGE {
        return Ok(None);
    }
    let declared = team.pipeline_stages();
    if !declared.iter().any(|stage| stage == to) {
        return Err(FlipErr::UnknownStage {
            to: to.to_owned(),
            declared,
        });
    }
    team.owner_of(to)
        .map(Some)
        .ok_or_else(|| FlipErr::UnownedStage { to: to.to_owned() })
}

struct StageOpening<'a> {
    workspace: &'a ResolvedWorkspace,
    store: &'a Store,
    team_name: &'a str,
    channel: &'a str,
    members: &'a [AgentState],
    board: &'a Path,
    from: Option<&'a str>,
    to: &'a str,
    owner: Option<&'a str>,
    /// The declared leader; `None` when the team declares none, so no seat gets a channel rule.
    leader: Option<&'a str>,
    by: &'a str,
    self_owned: bool,
    note: Option<&'a str>,
    mux: Option<MuxName>,
    now: Timestamp,
    rewake: bool,
}

fn open_stage(
    opening: &StageOpening<'_>,
    completed: &'static str,
) -> Result<(EventId, Delivery), FlipErr> {
    let stage = StageSignal {
        team: opening.team_name.to_owned(),
        instance: format!("{}#{}", opening.team_name, opening.channel),
        to: opening.to.to_owned(),
        by: opening.by.to_owned(),
        board: opening.board.to_path_buf(),
        at: opening.now,
        from: opening.from.map(ToOwned::to_owned),
        owner: opening.owner.map(ToOwned::to_owned),
        note: opening.note.map(ToOwned::to_owned),
    };
    let signal = Signal {
        // A fixed reserved signal name always satisfies the signal grammar.
        name: STAGE_SIGNAL.parse().expect("static signal name"),
        payload: stage.payload(),
        source: SignalSource::Team,
        watch: None,
    };
    let event = opening
        .store
        .append_signal(&opening.workspace.session_name, (&signal).into())
        .map_err(|err| FlipErr::from(err).after(completed, opening.to))?;
    if let Err(err) = fire_signal(
        opening.store.runtime_paths(),
        &opening.workspace.project_root,
        &signal,
    ) {
        tracing::warn!(error = %err, "failed to fire team.stage subscriptions");
    }
    let Some(owner) = opening.owner else {
        if let Some(root) = opening.board.parent()
            && let Err(error) = crate::lsp::registry::stop_checkout(
                root,
                crate::lsp::registry::StopReason::TeamDone,
            )
        {
            tracing::debug!(%error, "failed to stop language servers at Done");
        }
        return Ok((event, Delivery::Terminal));
    };
    if opening.self_owned {
        return Ok((event, Delivery::SelfOwned));
    }
    let cohort = opening.members.iter().filter(|member| {
        member.team.as_deref() == Some(opening.team_name)
            && member.channel().as_deref() == Some(opening.channel)
    });
    let Some(member) = crate::address::role_holders(cohort, owner)
        .into_iter()
        .next()
    else {
        return Ok((
            event,
            Delivery::OwnerNotLive {
                owner: owner.to_owned(),
            },
        ));
    };
    let result = dispatch::dispatch(
        opening.workspace,
        opening.store,
        DispatchRequest {
            target: format!("@{}", member.agent_id),
            text: stage_open_body(
                opening.from,
                opening.to,
                opening.by,
                opening.note,
                opening.rewake,
                opening.leader.filter(|leader| *leader != owner),
            ),
            target_scope: None,
            current_channel: crate::address::AddressContext {
                channel: Some(opening.channel.to_owned()),
                origin: crate::address::ChannelOrigin::Stamped,
            },
            caller: None,
            sender: MessageSender::Harness {
                notice: HarnessNotice::Stage,
            },
            automated: true,
            allow_fanout: false,
            reply: None,
            mux: opening.mux,
            enter: true,
            force: false,
            auto_compact: None,
            mode: DispatchMode::Boundary {
                gate: DeliveryGate::Done,
                not_before: None,
                after: Vec::new(),
                when: Vec::new(),
            },
        },
    )
    .map_err(|err| {
        FlipErr::from(err).after(
            if opening.rewake {
                "signal"
            } else {
                "board, signal"
            },
            opening.to,
        )
    })?;
    // A single durable session target with fanout disabled has exactly one outcome.
    let outcome = result
        .outcomes
        .into_iter()
        .next()
        .expect("single stage recipient");
    let delivery = match outcome {
        DispatchOutcome::Sent { label, .. } => Delivery::Sent { label },
        DispatchOutcome::Queued { label, .. }
        | DispatchOutcome::CompactionPending { label, .. }
        | DispatchOutcome::SkippedWaiting { label, .. } => Delivery::Queued { label },
    };
    Ok((event, delivery))
}

/// `leader` is set only for an owner who is not the leader: the seat's channel rule rides on
/// every stage open so it is the freshest text in the turn, launch reminder or not.
fn stage_open_body(
    from: Option<&str>,
    to: &str,
    by: &str,
    note: Option<&str>,
    rewake: bool,
    leader: Option<&str>,
) -> String {
    let mut body = if rewake {
        format!(
            "The team resumed at stage {to}, which is yours. Nothing flipped since the board's last Progress line: reread blackboard.md and continue from where it stops."
        )
    } else {
        match from {
            Some(from) if from == to => {
                format!("@{by} re-opened {to}. It is still yours: pick it up from blackboard.md.")
            }
            Some(from) => format!(
                "@{by} flipped the stage {from} -> {to}. {to} is yours: pick it up from blackboard.md."
            ),
            None => {
                format!(
                    "@{by} opened the stage {to}. {to} is yours: pick it up from blackboard.md."
                )
            }
        }
    };
    if let Some(note) = note {
        body.push_str("\n\nNote: ");
        body.push_str(note);
    }
    if let Some(leader) = leader {
        body.push_str(&format!(
            "\n\nYour report goes in your stage file and anything for the user to @{leader}; end the turn with the flip and no pane text."
        ));
    }
    body
}

/// A member leaving a stage its role owns for one it does not own, `Done` included.
fn hands_off(by: &Flipper<'_>, team: &Team, from: Option<&str>, owner: Option<&str>) -> bool {
    let Flipper::Member { role, .. } = by else {
        return false;
    };
    from.and_then(|stage| team.owner_of(stage)) == Some(*role) && owner != Some(*role)
}

/// Every path under the worktree `git status` reports, untracked files included, except the board
/// flip itself writes. A worktree outside any git repository has nothing to commit and reports none.
fn uncommitted_paths(worktree: &Path) -> Result<Vec<String>, FlipErr> {
    let status = crate::proc::git_command(worktree)
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .args(["--", "."])
        .arg(format!(":(exclude){}", super::board::BOARD_FILE))
        .env("LC_ALL", "C")
        .output()
        .map_err(|source| FlipErr::Io {
            path: worktree.to_path_buf(),
            source,
        })?;
    if !status.status.success() {
        let stderr = String::from_utf8_lossy(&status.stderr);
        // `LC_ALL=C` pins git's wording; other failures inside a repo, dubious ownership included, refuse.
        if stderr.contains("not a git repository") {
            return Ok(Vec::new());
        }
        return Err(FlipErr::GitStatus {
            worktree: worktree.to_path_buf(),
            stderr: stderr.trim().to_owned(),
        });
    }
    let mut paths = Vec::new();
    let mut entries = status.stdout.split(|byte| *byte == 0);
    while let Some(entry) = entries.next() {
        let Some(path) = entry.get(3..) else {
            continue;
        };
        paths.push(String::from_utf8_lossy(path).into_owned());
        // A rename or copy entry is followed by its source path.
        if entry[..2].iter().any(|code| matches!(code, b'R' | b'C')) {
            entries.next();
        }
    }
    Ok(paths)
}

fn compact_flipper(
    request: &FlipRequest<'_>,
    from: Option<&str>,
    owner: Option<&str>,
) -> Compaction {
    let Flipper::Member { role, agent, pane } = &request.by else {
        return Compaction::NotConfigured;
    };
    let Some(policy) = request.flip_compact else {
        return Compaction::NotConfigured;
    };
    // Compaction serves the work that continues after the hand-off; `Done` has no owner and no next turn.
    if owner.is_none() || !hands_off(&request.by, request.team, from, owner) {
        return Compaction::Ineligible;
    }
    let occupied = agent.occupied_context_tokens();
    let window = agent.resolved_context_window();
    let Some(occupied_tokens) = occupied.filter(|occupied| policy.reached(*occupied, window))
    else {
        return Compaction::BelowThreshold;
    };
    let threshold = match policy {
        AutoCompact::Tokens(tokens) => tokens,
        // A reached percentage threshold proves the window is known and nonzero.
        AutoCompact::Percent(percent) => (u128::from(window.expect("reached percentage window"))
            * u128::from(percent))
        .div_ceil(100)
        .try_into()
        .unwrap_or(u64::MAX),
    };
    let message_id = MessageId::new();
    let outcome = (|| {
        let pane = pane
            .as_ref()
            .ok_or_else(|| FlipCompactErr::Unavailable("no bound pane".to_owned()))?;
        let machine = MachineConfig::load_lenient();
        let command = crate::agents::compact_command(agent, &machine.harness).ok_or_else(|| {
            FlipCompactErr::Unavailable(format!("{} does not support compaction", agent.kind))
        })?;
        compact::send_compact(
            request.workspace,
            request.store,
            CompactRequest {
                message_id: message_id.clone(),
                agent,
                pane_id: pane.clone(),
                command,
                sender: MessageSender::System,
                automated: true,
            },
        )
        .map_err(FlipCompactErr::from)
    })();
    assist_log::append(&AssistRecord {
        at: request.now,
        assist: Assist::FlipCompact {
            kind: agent.kind.clone(),
            agent_id: agent.agent_id.clone(),
            role: (*role).to_owned(),
            threshold,
            from: from.map(str::to_owned),
            to: request.to.to_owned(),
            occupied_tokens: occupied,
            message_id: match &outcome {
                Err(
                    FlipCompactErr::Unavailable(_)
                    | FlipCompactErr::Compact(
                        CompactErr::Compacting
                        | CompactErr::Pending { .. }
                        | CompactErr::Repeated { .. },
                    ),
                ) => None,
                // Store failures can precede or follow publication; retain the attempted ID.
                Ok(_)
                | Err(FlipCompactErr::Compact(
                    CompactErr::Store(_) | CompactErr::Deliver(_) | CompactErr::Settled { .. },
                )) => Some(message_id.to_string()),
            },
            delivered: matches!(outcome, Ok(CompactOutcome::Sent)),
            error: outcome.as_ref().err().map(ToString::to_string),
        },
    });
    match outcome {
        Ok(CompactOutcome::Sent) => Compaction::Sent {
            message_id,
            occupied_tokens,
            threshold,
        },
        Ok(CompactOutcome::Queued) => Compaction::Queued {
            message_id,
            occupied_tokens,
            threshold,
        },
        Err(reason) => Compaction::Skipped {
            reason: reason.to_string(),
        },
    }
}

pub(super) fn ledger_line(
    now: Timestamp,
    by: &str,
    from: Option<&str>,
    to: &str,
    note: &str,
) -> String {
    let transition = match from {
        Some(from) => format!("{from} -> {to}"),
        None => format!("opened {to}"),
    };
    format!(
        "- {} @{by}: {transition} — {}",
        stamp(now),
        note.replace(['\r', '\n'], " ")
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn stage_strip_matches_exact_names_and_includes_terminal() {
        let stages = vec!["Plan".into(), "Plan review".into()];
        assert_eq!(
            stage_strip(&stages, Some("Plan review")),
            "Plan → [Plan review] → Done"
        );
        assert_eq!(
            stage_strip(&stages, Some("Done")),
            "Plan → Plan review → [Done]"
        );
        assert_eq!(
            stage_strip(&stages, Some("Plan (delta)")),
            "Plan → Plan review → Done"
        );
    }

    #[test]
    fn stage_notices_are_prose_for_handoff_refire_and_recovery() {
        assert_eq!(
            stage_open_body(
                Some("Plan"),
                "Implement",
                "planner",
                Some("plan ready"),
                false,
                None
            ),
            "@planner flipped the stage Plan -> Implement. Implement is yours: pick it up from blackboard.md.\n\nNote: plan ready"
        );
        assert_eq!(
            stage_open_body(None, "Explore", "planner", Some("opened"), false, None),
            "@planner opened the stage Explore. Explore is yours: pick it up from blackboard.md.\n\nNote: opened"
        );
        assert_eq!(
            stage_open_body(
                Some("Implement"),
                "Implement",
                "user",
                Some("retry"),
                false,
                None
            ),
            "@user re-opened Implement. It is still yours: pick it up from blackboard.md.\n\nNote: retry"
        );
        assert_eq!(
            stage_open_body(Some("Implement"), "Implement", "rimz", None, true, None),
            "The team resumed at stage Implement, which is yours. Nothing flipped since the board's last Progress line: reread blackboard.md and continue from where it stops."
        );
    }

    #[test]
    fn hand_off_is_a_member_leaving_its_own_stage_for_another_owner() {
        let team: Team = toml::from_str(
            r#"
            [[roles]]
            role = "coder"
            profile = "claude"
            owns = ["Build", "Polish"]
            [[roles]]
            role = "reviewer"
            profile = "claude"
            owns = ["Review"]
            "#,
        )
        .unwrap();
        let agent = crate::testkit::agent_state("claude", "coder", Timestamp::UNIX_EPOCH);
        let coder = Flipper::Member {
            role: "coder",
            agent: &agent,
            pane: None,
        };
        for (by, from, owner, expected) in [
            (&coder, Some("Build"), Some("reviewer"), true),
            (&coder, Some("Build"), None, true),
            (&coder, Some("Build"), Some("coder"), false),
            (&coder, Some("Review"), Some("coder"), false),
            (&coder, None, Some("reviewer"), false),
            (&Flipper::User, Some("Build"), Some("reviewer"), false),
        ] {
            assert_eq!(
                hands_off(by, &team, from, owner),
                expected,
                "{} {from:?} -> {owner:?}",
                by.name()
            );
        }
    }

    #[test]
    fn uncommitted_paths_list_changes_and_untracked_files() {
        let outside = tempfile::tempdir().unwrap();
        assert!(uncommitted_paths(outside.path()).unwrap().is_empty());
        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .current_dir(repo.path())
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
        };
        git(&["init", "-q"]);
        std::fs::write(repo.path().join("moved.rs"), "fn a() {}\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        std::fs::write(repo.path().join("blackboard.md"), "Stage: Build\n").unwrap();
        assert!(uncommitted_paths(repo.path()).unwrap().is_empty());
        git(&["mv", "moved.rs", "renamed.rs"]);
        std::fs::create_dir(repo.path().join("src")).unwrap();
        std::fs::write(repo.path().join("src/new file.rs"), "").unwrap();
        assert_eq!(
            uncommitted_paths(repo.path()).unwrap(),
            ["renamed.rs", "src/new file.rs"]
        );
    }

    #[test]
    fn rewake_skips_a_member_whose_stage_notice_is_queued_or_already_delivered() {
        let now = Timestamp::from_second(1_000_000).unwrap();
        let member = crate::testkit::agent_state("codex", "sess-coder", now);
        let other = crate::testkit::agent_state("codex", "sess-reviewer", now);
        let window = MessageBody::Prompt.delivery_window();
        let record = |to: &AgentState, sender: MessageSender, delivered_at: Option<Timestamp>| {
            let mut record = MessageRecord::new(
                crate::ids::WorkspaceId::from_project_root(Path::new("/tmp/rimz-rewake")),
                to,
                "stage".to_owned(),
                DeliveryGate::Done,
            );
            record.sender = sender;
            if let Some(at) = delivered_at {
                record.status = MessageStatus::Delivered;
                record.delivered_at = Some(at);
            }
            record
        };
        let stage = MessageSender::Harness {
            notice: HarnessNotice::Stage,
        };
        let wait = MessageSender::Harness {
            notice: HarnessNotice::Wait,
        };
        let queued = record(&member, stage.clone(), None);
        let just_delivered = record(&member, stage.clone(), Some(now - Duration::from_secs(1)));
        let long_delivered = record(&member, stage.clone(), Some(now - window * 2));
        let resume = RewakeCause::Resume {
            since: now - Duration::from_secs(2),
        };
        let before_attempt = record(&member, stage.clone(), Some(now - Duration::from_secs(3)));
        let not_stage = [
            record(&member, wait.clone(), None),
            record(&member, MessageSender::Human, None),
            record(&other, stage.clone(), None),
        ];
        let delivered_not_stage = [
            record(&member, wait, Some(now)),
            record(&member, MessageSender::Human, Some(now)),
            record(&other, stage, Some(now)),
        ];
        for (cause, live, history, expected) in [
            (resume, &[queued.clone()][..], &[][..], true),
            (resume, &[][..], std::slice::from_ref(&just_delivered), true),
            (
                resume,
                &[][..],
                std::slice::from_ref(&before_attempt),
                false,
            ),
            (RewakeCause::Registration, &[queued][..], &[][..], true),
            (
                RewakeCause::Registration,
                &[][..],
                std::slice::from_ref(&just_delivered),
                true,
            ),
            (
                RewakeCause::Registration,
                &[][..],
                std::slice::from_ref(&long_delivered),
                false,
            ),
            (resume, &not_stage[..], &delivered_not_stage[..], false),
            (
                RewakeCause::Registration,
                &not_stage[..],
                &delivered_not_stage[..],
                false,
            ),
        ] {
            assert_eq!(
                cause.already_woken(live, history, &member, now),
                expected,
                "{cause:?} live={} history={}",
                live.len(),
                history.len()
            );
        }
    }

    #[test]
    fn stage_notices_carry_the_channel_rule_for_a_non_leader_owner() {
        assert_eq!(
            stage_open_body(
                Some("Plan"),
                "Implement",
                "planner",
                Some("plan ready"),
                false,
                Some("planner")
            ),
            "@planner flipped the stage Plan -> Implement. Implement is yours: pick it up from blackboard.md.\n\nNote: plan ready\n\nYour report goes in your stage file and anything for the user to @planner; end the turn with the flip and no pane text."
        );
        assert_eq!(
            stage_open_body(
                Some("Implement"),
                "Implement",
                "rimz",
                None,
                true,
                Some("planner")
            ),
            "The team resumed at stage Implement, which is yours. Nothing flipped since the board's last Progress line: reread blackboard.md and continue from where it stops.\n\nYour report goes in your stage file and anything for the user to @planner; end the turn with the flip and no pane text."
        );
    }
}
