//! Team stage transitions: locked board edits, durable signals, owner delivery, and hand-off compaction.

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde_json::{Map, json};

use crate::Store;
use crate::agents::AgentState;
use crate::config::{DONE_STAGE, MachineConfig, Team};
use crate::disk::{atomic, lock::WorkspaceLock};
use crate::ids::{EventId, MessageId, MuxName, PaneId};
use crate::message::compact::{self, CompactErr, CompactOutcome, CompactRequest};
use crate::message::dispatch::{self, DispatchMode, DispatchOutcome, DispatchRequest};
use crate::store::event::SignalSource;
use crate::store::message::{AutoCompact, DeliveryGate, HarnessNotice, MessageSender};
use crate::workspace::ResolvedWorkspace;

use super::assist_log::{self, Assist, AssistRecord};
use super::schedule::signal::{Signal, fire_signal};
use super::scratch::parse_board_stage;

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
    NotHandedOff,
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
    #[error(transparent)]
    Config(#[from] super::spec::LayoutErr),
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Lock(#[from] crate::disk::lock::LockErr),
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
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
    let worktree = canonical_worktree(request.worktree)?;
    let board = worktree.join("blackboard.md");
    let _lock = WorkspaceLock::acquire(&request.store.runtime_paths().board_lock(&worktree))?;
    let text = read_board(&board)?;
    let from = parse_board_stage(&text).map(|stage| stage.name);
    let ledger = ledger_line(
        request.now,
        request.by.name(),
        from.as_deref(),
        request.to,
        request.note,
    );
    let rewritten = rewrite_board(&text, request.to, owner, &ledger);
    atomic::write_bytes_atomically(&board, rewritten.as_bytes())?;
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
        by: request.by.name(),
        self_owned: matches!(&request.by, Flipper::Member { role, .. } if Some(*role) == owner),
        note: Some(request.note),
        mux: request.mux,
        now: request.now,
        rewake: false,
    };
    let (signal_event, delivery) = open_stage(&opening, "board")?;
    let compaction = compact_flipper(&request, from.as_deref(), &delivery);
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

#[allow(clippy::too_many_arguments)]
pub fn rewake(
    workspace: &ResolvedWorkspace,
    store: &Store,
    team_name: &str,
    team: &Team,
    member: &AgentState,
    members: &[AgentState],
    worktree: &Path,
    mux: Option<MuxName>,
    now: Timestamp,
) -> Result<Option<FlipReceipt>, FlipErr> {
    if team.owned_stages().next().is_none() {
        return Ok(None);
    }
    let worktree = canonical_worktree(worktree)?;
    let board = worktree.join("blackboard.md");
    let _lock = WorkspaceLock::acquire(&store.runtime_paths().board_lock(&worktree))?;
    let text = read_board(&board)?;
    let Some(stage) = parse_board_stage(&text) else {
        return Ok(None);
    };
    if stage.name == DONE_STAGE
        || team.owner_of(&stage.name) != member.role.as_deref()
        || member.role.is_none()
    {
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
            by: "rimz",
            self_owned: false,
            note: None,
            mux,
            now,
            rewake: true,
        },
        "registration",
    )?;
    Ok(Some(FlipReceipt {
        board,
        from: Some(stage.name.clone()),
        to: stage.name,
        owner: owner.map(str::to_owned),
        delivery,
        compaction: Compaction::NotHandedOff,
        signal_event,
    }))
}

fn canonical_worktree(worktree: &Path) -> Result<PathBuf, FlipErr> {
    worktree.canonicalize().map_err(|source| FlipErr::Io {
        path: worktree.to_path_buf(),
        source,
    })
}

fn read_board(board: &Path) -> Result<String, FlipErr> {
    match std::fs::read_to_string(board) {
        Ok(text) => Ok(text),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(source) => Err(FlipErr::Io {
            path: board.to_path_buf(),
            source,
        }),
    }
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
    let declared: Vec<_> = if team.stages.is_empty() {
        team.owned_stages().map(str::to_owned).collect()
    } else {
        team.stages.clone()
    };
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
    let mut payload = Map::from_iter([
        ("team".to_owned(), json!(opening.team_name)),
        (
            "instance".to_owned(),
            json!(format!("{}#{}", opening.team_name, opening.channel)),
        ),
        ("to".to_owned(), json!(opening.to)),
        ("by".to_owned(), json!(opening.by)),
        ("board".to_owned(), json!(opening.board)),
        ("at".to_owned(), json!(opening.now.to_string())),
    ]);
    if let Some(from) = opening.from {
        payload.insert("from".to_owned(), json!(from));
    }
    if let Some(owner) = opening.owner {
        payload.insert("owner".to_owned(), json!(owner));
    }
    if let Some(note) = opening.note {
        payload.insert("note".to_owned(), json!(note));
    }
    let signal = Signal {
        // A fixed reserved signal name always satisfies the signal grammar.
        name: "team.stage".parse().expect("static signal name"),
        payload,
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
        return Ok((event, Delivery::Terminal));
    };
    if opening.self_owned {
        return Ok((event, Delivery::SelfOwned));
    }
    let Some(member) = opening.members.iter().find(|member| {
        member.role.as_deref() == Some(owner)
            && member.team.as_deref() == Some(opening.team_name)
            && member.channel().as_deref() == Some(opening.channel)
    }) else {
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
            ),
            target_scope: None,
            current_channel: Some(opening.channel.to_owned()),
            caller: None,
            sender: MessageSender::Harness {
                notice: HarnessNotice::Stage,
            },
            automated: true,
            allow_fanout: false,
            reply: None,
            mux: opening.mux,
            mode: DispatchMode::Boundary {
                enter: true,
                gate: DeliveryGate::Done,
                force: false,
                auto_compact: None,
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

fn stage_open_body(
    from: Option<&str>,
    to: &str,
    by: &str,
    note: Option<&str>,
    rewake: bool,
) -> String {
    if rewake {
        return format!(
            "The team resumed at stage {to}, which is yours. Nothing flipped since the board's last Progress line: reread blackboard.md and continue from where it stops."
        );
    }
    let mut body = match from {
        Some(from) if from == to => {
            format!("@{by} re-opened {to}. It is still yours: pick it up from blackboard.md.")
        }
        Some(from) => format!(
            "@{by} flipped the stage {from} -> {to}. {to} is yours: pick it up from blackboard.md."
        ),
        None => {
            format!("@{by} opened the stage {to}. {to} is yours: pick it up from blackboard.md.")
        }
    };
    if let Some(note) = note {
        body.push_str("\n\nNote: ");
        body.push_str(note);
    }
    body
}

fn compact_flipper(
    request: &FlipRequest<'_>,
    from: Option<&str>,
    delivery: &Delivery,
) -> Compaction {
    let Flipper::Member { role, agent, pane } = &request.by else {
        return Compaction::NotConfigured;
    };
    let Some(policy) = request.flip_compact else {
        return Compaction::NotConfigured;
    };
    if from.and_then(|stage| request.team.owner_of(stage)) != Some(*role)
        || !matches!(
            delivery,
            Delivery::Sent { .. }
                | Delivery::Queued { .. }
                | Delivery::OwnerNotLive { .. }
                | Delivery::Terminal
        )
    {
        return Compaction::NotHandedOff;
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
        let command = crate::agents::spec_by_kind(agent.kind.as_str())
            .and_then(|spec| {
                spec.launch
                    .compact_command(machine.harness.compact_instruction())
            })
            .ok_or_else(|| {
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
                _ => Some(message_id.to_string()),
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

fn ledger_line(now: Timestamp, by: &str, from: Option<&str>, to: &str, note: &str) -> String {
    let at = now.to_zoned(MachineConfig::load_lenient().time_zone());
    let transition = match from {
        Some(from) => format!("{from} -> {to}"),
        None => format!("opened {to}"),
    };
    format!(
        "- {} @{by}: {transition} — {}",
        at.strftime("%Y-%m-%d %H:%M"),
        note.replace(['\r', '\n'], " ")
    )
}

fn rewrite_board(text: &str, to: &str, owner: Option<&str>, ledger: &str) -> String {
    let mut result = String::with_capacity(text.len() + ledger.len() + to.len() + 64);
    let stage = match owner {
        Some(owner) => format!("Stage: {to} (@{owner})"),
        None => format!("Stage: {to}"),
    };
    let mut replaced = false;
    for line in text.split_inclusive('\n') {
        if !replaced && line.starts_with("Stage:") {
            result.push_str(&stage);
            if line.ends_with("\r\n") {
                result.push_str("\r\n");
            } else if line.ends_with('\n') {
                result.push('\n');
            }
            replaced = true;
        } else {
            result.push_str(line);
        }
    }
    if !replaced {
        let at = if text.starts_with("# ") {
            text.find('\n').map_or(text.len(), |at| at + 1)
        } else {
            0
        };
        let prefix = if at > 0 && !text[..at].ends_with('\n') {
            "\n"
        } else {
            ""
        };
        result.insert_str(at, &format!("{prefix}{stage}\n"));
    }
    let mut offset = 0;
    let mut insertion = None;
    for line in result.split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']);
        if insertion.is_some() && content.starts_with('#') {
            break;
        }
        offset += line.len();
        if matches!(content, "## Progress" | "## Progress log")
            || (insertion.is_some() && !content.trim().is_empty())
        {
            insertion = Some(offset);
        }
    }
    match insertion {
        Some(at) => {
            let prefix = if result[..at].ends_with('\n') {
                ""
            } else {
                "\n"
            };
            result.insert_str(at, &format!("{prefix}{ledger}\n"));
        }
        None => {
            if !result.ends_with('\n') {
                result.push('\n');
            }
            result.push_str(&format!("\n## Progress\n{ledger}\n"));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_rewrite_preserves_freeform_sections_and_appends_inside_ledger() {
        let board = "# Blackboard\r\nStage:Plan (@planner)\r\n\r\n## Goal\r\nKeep this.\r\n\r\n## Progress log\r\n- old entry\r\n\r\n## Result\r\nUnchanged.\r\n";
        assert_eq!(
            rewrite_board(board, "Implement", Some("coder"), "- next"),
            "# Blackboard\r\nStage: Implement (@coder)\r\n\r\n## Goal\r\nKeep this.\r\n\r\n## Progress log\r\n- old entry\r\n- next\n\r\n## Result\r\nUnchanged.\r\n"
        );
        assert_eq!(
            rewrite_board("Stage: Plan", "Done", None, "- done"),
            "Stage: Done\n\n## Progress\n- done\n"
        );
        assert_eq!(
            rewrite_board("# Board\n", "Done", None, "- done"),
            "# Board\nStage: Done\n\n## Progress\n- done\n"
        );
    }

    #[test]
    fn board_rewrite_handles_empty_ledger_and_unterminated_lines() {
        for (board, expected) in [
            (
                "Stage: Plan\n## Progress log\n\n## Result\n",
                "Stage: Done\n## Progress log\n- done\n\n## Result\n",
            ),
            (
                "Stage: Plan\n## Progress log",
                "Stage: Done\n## Progress log\n- done\n",
            ),
            (
                "Stage: Plan\n## Progress log\n- previous",
                "Stage: Done\n## Progress log\n- previous\n- done\n",
            ),
            (
                "Stage: Plan\nStage: untouched\n",
                "Stage: Done\nStage: untouched\n\n## Progress\n- done\n",
            ),
        ] {
            assert_eq!(rewrite_board(board, "Done", None, "- done"), expected);
        }
    }

    #[test]
    fn multiline_note_cannot_inject_a_board_heading() {
        let line = ledger_line(
            "2026-09-12T14:02:00Z".parse().unwrap(),
            "planner",
            Some("Plan"),
            "Implement",
            "ready\n## Result\r\nStage: Done",
        );
        assert_eq!(line.lines().count(), 1);
        assert!(line.ends_with("@planner: Plan -> Implement — ready ## Result  Stage: Done"));
    }

    #[test]
    fn first_flip_bootstraps_stage_and_progress_without_losing_freeform_text() {
        for (text, expected) in [
            ("", "Stage: Explore (@planner)\n\n## Progress\n- opened\n"),
            (
                "# Work",
                "# Work\nStage: Explore (@planner)\n\n## Progress\n- opened\n",
            ),
            (
                "# Work\n\n## Goal\nFind the bug.\n",
                "# Work\nStage: Explore (@planner)\n\n## Goal\nFind the bug.\n\n## Progress\n- opened\n",
            ),
            (
                "A freeform board.\n",
                "Stage: Explore (@planner)\nA freeform board.\n\n## Progress\n- opened\n",
            ),
            (
                "## Progress\n- existing\n\n## Result\n",
                "Stage: Explore (@planner)\n## Progress\n- existing\n- opened\n\n## Result\n",
            ),
        ] {
            assert_eq!(
                rewrite_board(text, "Explore", Some("planner"), "- opened"),
                expected
            );
        }
        let ledger = ledger_line(
            "2026-09-12T14:02:00Z".parse().unwrap(),
            "planner",
            None,
            "Explore",
            "sweep aimed",
        );
        assert!(ledger.ends_with("@planner: opened Explore — sweep aimed"));
    }

    #[test]
    fn stage_notices_are_prose_for_handoff_refire_and_recovery() {
        assert_eq!(
            stage_open_body(
                Some("Plan"),
                "Implement",
                "planner",
                Some("plan ready"),
                false
            ),
            "@planner flipped the stage Plan -> Implement. Implement is yours: pick it up from blackboard.md.\n\nNote: plan ready"
        );
        assert_eq!(
            stage_open_body(None, "Explore", "planner", Some("opened"), false),
            "@planner opened the stage Explore. Explore is yours: pick it up from blackboard.md.\n\nNote: opened"
        );
        assert_eq!(
            stage_open_body(Some("Implement"), "Implement", "user", Some("retry"), false),
            "@user re-opened Implement. It is still yours: pick it up from blackboard.md.\n\nNote: retry"
        );
        assert_eq!(
            stage_open_body(Some("Implement"), "Implement", "rimz", None, true),
            "The team resumed at stage Implement, which is yours. Nothing flipped since the board's last Progress line: reread blackboard.md and continue from where it stops."
        );
    }
}
