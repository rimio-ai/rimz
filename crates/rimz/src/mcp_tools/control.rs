//! `self_stop`, `self_compact`, `self_tune`, and `self_molt`: the jobs of `rimz agents
//! stop @me --when-idle`, `rimz agents compact @me`, `rimz agents tune @me`, and
//! `rimz agents molt`.

use anyhow::Context;
use serde::Serialize;

use super::Tools;
use super::room::Room;
use crate::address::{addressable_agents, agent_handle};
use crate::agents::AgentState;
use crate::agents::state::TuneSetting;
use crate::agents::state::session_row;
use crate::harness::idle_stop::started::{Seat, Started};
use crate::harness::stop_booking::{IDLE_REST_DEFAULT, StopBooking, book_stop, parse_idle_rest};
use crate::harness::tune::{SettingReport, budget, target_handle, tune as tune_agent};
use crate::launcher::context::{current_channel, resolve_caller, resolve_me, sender_for};
use crate::mcp::catalog::{self, SelfCompactArgs, SelfMoltArgs, SelfStopArgs, SelfTuneArgs};
use crate::mcp::{Answer, Failure};

pub(super) fn stop(tools: &Tools, args: &SelfStopArgs) -> Result<Answer, Failure> {
    let rest = args.when_idle.as_deref().unwrap_or(IDLE_REST_DEFAULT);
    let after = parse_idle_rest(rest)
        .map_err(|err| Failure::new(format!("invalid when_idle `{rest}`: {err}")))?;
    let room = Room::open(&tools.start_dir)?;
    let snapshot = room
        .store
        .snapshot_cached()
        .context("reading agent snapshot")?;
    let agent = resolve_me(&room.store, &snapshot)?;
    let booking = book_stop(room.store.paths(), &snapshot, agent, after, || {
        resolve_caller(&room.store)
    })?;
    let started = matches!(booking, StopBooking::Booked { .. }).then(|| {
        started(&room, agent).map_or_else(
            |err| StartedRead::Unread {
                unread: format!("{err:#}"),
            },
            StartedRead::Read,
        )
    });
    Ok(Answer::data(&Stop { booking, started }))
}

/// What `self_stop` answers: the booking, and once a stop is booked, what holds
/// it, what closes with the agent, and what outlives it, as the command's
/// receipt prints them.
#[derive(Serialize)]
struct Stop {
    #[serde(flatten)]
    booking: StopBooking,
    #[serde(skip_serializing_if = "Option::is_none")]
    started: Option<StartedRead>,
}

/// The agent's started work, or why it could not be read; the stop stays booked
/// either way.
#[derive(Serialize)]
#[serde(untagged)]
enum StartedRead {
    Read(Started),
    Unread { unread: String },
}

/// What `agent` started, read from the room snapshot the sidebar published, as
/// `rimz agents stop @me --when-idle` reads it for its receipt.
pub(super) fn started(room: &Room, agent: &AgentState) -> anyhow::Result<Started> {
    let booked = room.published_snapshot()?;
    let shown = session_row(&booked.agents, agent).context("the agent left the room snapshot")?;
    let reader = current_channel(&room.workspace, Some(&room.store)).address_context();
    let seat = Seat {
        root: &room.workspace.project_root,
        reader: &reader,
        you: true,
        rimz: "rimz",
    };
    Ok(crate::harness::idle_stop::started::started(
        &room.store,
        &booked,
        shown,
        &seat,
        booked.now,
    )?)
}

pub(super) fn compact(tools: &Tools, args: &SelfCompactArgs) -> Result<Answer, Failure> {
    let room = Room::open(&tools.start_dir)?;
    let snapshot = room.resolution_snapshot_with_context()?;
    let agent = resolve_me(&room.store, &snapshot)?;
    let handle = crate::address::named_handle(agent, &addressable_agents(&snapshot), false);
    let caller = resolve_caller(&room.store)?;
    let channel = current_channel(&room.workspace, Some(&room.store));
    let receipt = crate::message::compact::compact_agent(
        &room.workspace,
        &room.store,
        agent,
        &handle,
        args.instruction.as_deref(),
        crate::message::compact::CompactAsk {
            own: true,
            no_continue: args.no_continue,
            hard: false,
        },
        sender_for(caller.as_ref(), channel.as_deref(), false),
    )?;
    Ok(Answer::data(&receipt))
}

/// What `self_tune` answers: the settings it reports, and with no setting
/// named, the dollar cap too, as `rimz agents tune @me --json` prints them.
#[derive(Serialize)]
struct Tuned<'a> {
    agent: String,
    settings: Vec<&'a SettingReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    keep_warm_while: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    budget: Option<serde_json::Value>,
}

pub(super) fn tune(tools: &Tools, args: &SelfTuneArgs) -> Result<Answer, Failure> {
    let room = Room::open(&tools.start_dir)?;
    let snapshot = room.resolution_snapshot_with_context()?;
    let peers = addressable_agents(&snapshot);
    let agent = resolve_me(&room.store, &snapshot)?;
    let by = match &args.value {
        Some(_) => resolve_caller(&room.store)?
            .and_then(|caller| {
                crate::harness::ancestry::resolve_launch_caller(&snapshot.agents, &caller).ok()
            })
            .map(|caller| agent_handle(caller, &peers, true)),
        None => None,
    };
    let setting = args.setting.map(|setting| match setting {
        catalog::TuneSetting::KeepWarm => TuneSetting::KeepWarm,
        catalog::TuneSetting::IdleCompact => TuneSetting::IdleCompact,
        catalog::TuneSetting::SmartCompact => TuneSetting::SmartCompact,
    });
    let tuned = tune_agent(
        &room.workspace,
        &room.store,
        agent,
        setting.zip(args.value.as_deref()),
        by,
    )?;
    let budget = match setting {
        Some(_) => None,
        None => {
            let mut report =
                serde_json::to_value(budget(room.store.runtime_paths(), &tuned.agent, &peers)?)
                    .context("encoding the budget")?;
            if let Some(report) = report.as_object_mut() {
                report.remove("agent");
            }
            Some(report)
        }
    };
    Ok(Answer::data(&Tuned {
        agent: target_handle(&tuned.agent, &peers),
        settings: tuned
            .reports
            .iter()
            .filter(|report| setting.is_none_or(|setting| setting == report.setting))
            .collect(),
        keep_warm_while: tuned.keep_warm_condition.as_deref(),
        budget,
    }))
}

/// What `self_molt` answers: the agent's handle, the kind the new session runs, the rest this
/// session takes after this turn before it closes, and the notices the swap printed.
#[derive(Serialize)]
struct Molt {
    agent: String,
    successor_kind: String,
    closes_after_secs: u64,
    notices: Vec<String>,
}

pub(super) fn molt(tools: &Tools, args: &SelfMoltArgs) -> Result<Answer, Failure> {
    let rest = match args.when_idle.as_deref() {
        Some(raw) => crate::utils::time::parse_compact_duration(raw)
            .map_err(|err| Failure::new(format!("invalid when_idle `{raw}`: {err}")))?,
        None => std::time::Duration::ZERO,
    };
    let room = Room::open(&tools.start_dir)?;
    let molted = crate::launcher::molt::molt(
        &room.workspace,
        &room.store,
        None,
        &crate::config::MachineConfig::load_lenient(),
        args.prompt.clone(),
        args.agent.clone(),
    )
    .map_err(|failure| {
        let mut refused = Failure::new(failure.error.to_string());
        if !failure.notices.is_empty() {
            refused
                .extra
                .insert("notices".to_owned(), failure.notices.into());
        }
        refused
    })?;
    let agent = molted.handle.clone();
    let successor_kind = molted.successor_kind.to_string();
    let mut notices = molted.notices.clone();
    notices.extend(molted.close_when_rested(rest, |_| Ok(())));
    Ok(Answer::data(&Molt {
        agent,
        successor_kind,
        closes_after_secs: rest.as_secs(),
        notices,
    }))
}
