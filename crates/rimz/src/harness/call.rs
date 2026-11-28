//! What an `agent` or `team` call asks of its launch beyond opening it: the answer an agent's
//! `schema` asks for, handed in with `rimz submit`, and the stop at the call's `timeout`. The
//! record is `store::call`'s, keyed by the launch, so a workflow run's step reads and writes
//! the same record as a tool call's launch.

use std::time::Duration;

use jiff::Timestamp;
use serde_json::{Map, Value};

use crate::Store;
use crate::disk::paths::StatePaths;
use crate::harness::launch::ReminderSection;
use crate::ids::{AgentSessionId, MessageId, RunId};
use crate::store::call::{CallRecord, CallStoreErr, HandedAnswer, Seat};
use crate::store::message::{DeliveryGate, MessageRecord, MessageSender};

/// What a call asks of its launch beyond the launch itself.
#[derive(Clone, Debug)]
pub struct CallSpec {
    pub stay: bool,
    pub schema: Option<Map<String, Value>>,
    pub timeout: Option<Duration>,
}

impl CallSpec {
    /// The launch reminder section that tells the agent its schema and how to hand in the answer.
    pub fn reminder(&self) -> Option<ReminderSection> {
        self.schema
            .as_ref()
            .map(crate::harness::launch_reminders::answer_section)
    }

    /// Publish the call record of the launch that opened `seats`, its leader first, when the call
    /// was given a schema or a timeout; a call given neither keeps none.
    pub fn publish(
        &self,
        paths: &StatePaths,
        seats: Vec<Seat>,
        now: Timestamp,
    ) -> Result<(), CallStoreErr> {
        if self.schema.is_none() && self.timeout.is_none() {
            return Ok(());
        }
        let Some(leader) = seats.first() else {
            return Ok(());
        };
        crate::store::call::create(
            paths,
            &CallRecord {
                launch_id: leader.launch_id.clone(),
                seats,
                stay: self.stay,
                schema: self.schema.clone(),
                created_at: now,
                deadline: self.timeout.map(|timeout| now + timeout),
                answer: None,
                asked: None,
                timed_out_at: None,
            },
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SubmitErr {
    #[error(
        "this launch was given no `schema`, so it takes no answer: end your turn with your answer \
         as your last message"
    )]
    NoSchema,
    #[error("the answer is not JSON: {0}; nothing was stored")]
    NotJson(serde_json::Error),
    #[error("the answer does not fit its schema: {0}; nothing was stored")]
    Misfit(String),
    #[error(transparent)]
    Store(#[from] CallStoreErr),
}

/// Hand in `raw` as the answer of launch `launch_id`: stored once it is JSON that fits the
/// launch's schema, in place of an earlier answer; refused with nothing stored otherwise.
pub fn submit(
    paths: &StatePaths,
    launch_id: &AgentSessionId,
    raw: &str,
    now: Timestamp,
) -> Result<Value, SubmitErr> {
    let schema = crate::store::call::load(paths, launch_id)?
        .and_then(|record| record.schema)
        .ok_or(SubmitErr::NoSchema)?;
    let value: Value = serde_json::from_str(raw).map_err(SubmitErr::NotJson)?;
    if let Some(reason) = crate::harness::block::answer_misfit(&schema, &value) {
        return Err(SubmitErr::Misfit(reason));
    }
    let stored = crate::store::call::update(paths, launch_id, |record| {
        record.answer = Some(HandedAnswer {
            value: value.clone(),
            at: now,
        });
        Some(())
    })?;
    stored.map(|_| value).ok_or(SubmitErr::NoSchema)
}

/// The JSON text of the answer launch `launch_id` handed in from `since` to `until`: the
/// response of the turn that started and ended then, in place of its last message. An answer
/// handed in after the turn ended is a later turn's, which a late turn end never takes.
pub fn handed_answer(
    paths: &StatePaths,
    launch_id: &AgentSessionId,
    since: Timestamp,
    until: Timestamp,
) -> Option<String> {
    let record = crate::store::call::load(paths, launch_id)
        .inspect_err(|error| tracing::warn!(%launch_id, %error, "could not read a call record"))
        .ok()??;
    let answer = record
        .answer
        .filter(|answer| since <= answer.at && answer.at <= until)?;
    serde_json::to_string_pretty(&answer.value).ok()
}

/// Queue the one notice that tells the agent of run `run_id`, parked at a clean turn end on the
/// answer it owes, how to hand it in: a System notice into its pane, gated on its rest and linked
/// to the run, so a run that ended first withdraws it. A run parked on anything else (its own
/// shell, its subagents) is woken by that and told nothing. The notice's id is stored before it
/// is queued, so a later turn end queues a notice the first never queued (a queue error, a stop
/// between the two writes) under that id, which `queue_message_once` keeps to one.
pub fn queue_answer_notice(
    store: &Store,
    session_name: &str,
    run_id: &RunId,
) -> crate::store::Result<()> {
    let Ok(run) = crate::harness::run::load(store.paths(), run_id) else {
        return Ok(());
    };
    let (Some(launch_id), Some(agent_id)) = (run.launch_id.as_ref(), run.agent_id.as_ref()) else {
        return Ok(());
    };
    if run.status.is_terminal() || run.parked_at.is_none() {
        return Ok(());
    }
    let Some(call) =
        crate::store::call::load(store.paths(), launch_id)?.filter(CallRecord::owes_answer)
    else {
        return Ok(());
    };
    if crate::harness::owed::owed_wake(store, &run.kind, agent_id)?
        != Some(crate::harness::owed::OwedWake::Answer)
    {
        return Ok(());
    }
    let snapshot = store.snapshot_cached()?;
    let Some(row) =
        crate::store::snapshot::find_agent(&snapshot.agents, run.kind.as_str(), agent_id)
    else {
        return Ok(());
    };
    let message_id = match call.asked {
        Some(id) => id,
        None => {
            let asked = crate::store::call::update(store.paths(), launch_id, |call| {
                (call.owes_answer() && call.asked.is_none()).then(|| {
                    let id = MessageId::new();
                    call.asked = Some(id.clone());
                    id
                })
            })?;
            let Some((_, id)) = asked else {
                return Ok(());
            };
            id
        }
    };
    let mut notice = MessageRecord::new(
        run.workspace_id.clone(),
        row,
        ANSWER_OWED.to_owned(),
        DeliveryGate::Done,
    )
    .with_channel(row.lane_name())
    .with_sender(MessageSender::System)
    .with_about_run(Some(run_id.clone()));
    notice.message_id = message_id;
    store.queue_message_once(&notice, session_name)
}

const ANSWER_OWED: &str = "Your launcher has no answer for this task yet: your turn ended without \
`rimz submit`. Hand in the answer as one JSON value that fits the schema in your launch \
reminder with `rimz submit '<answer>'`, then end your turn. Your session stays open until a turn \
ends with the answer handed in.";

/// Claim the deadline's stop of launch `launch_id` at `now`, once: the record as claimed, or
/// none when it has no deadline, its deadline has not passed, or an earlier claim took it.
pub fn claim_timeout(
    paths: &StatePaths,
    launch_id: &AgentSessionId,
    now: Timestamp,
) -> Result<Option<CallRecord>, CallStoreErr> {
    Ok(crate::store::call::update(paths, launch_id, |call| {
        (call.timed_out_at.is_none() && call.deadline.is_some_and(|deadline| deadline <= now))
            .then(|| call.timed_out_at = Some(now))
    })?
    .map(|(call, ())| call))
}

/// The live rows of `call`'s seats: each seat's current row, when it still holds that seat's own
/// launch id and has not ended. A row that revived a seat under a new launch id is another
/// launch, which this call's deadline never stops.
pub fn live_seats<'a>(
    call: &CallRecord,
    agents: impl IntoIterator<Item = &'a crate::agents::AgentState> + Clone,
) -> Vec<&'a crate::agents::AgentState> {
    call.seats
        .iter()
        .filter_map(|seat| {
            crate::address::launch_row(agents.clone(), &seat.kind, &seat.launch_id)
                .filter(|row| row.launch_key() == &seat.launch_id && row.ended_at.is_none())
        })
        .collect()
}

/// The failure a timed-out launch's report and `join` read: the timeout that stopped it.
pub fn timeout_failure(call: &CallRecord) -> String {
    let timeout = call
        .deadline
        .map(|deadline| deadline.duration_since(call.created_at).as_secs().max(0) as u64)
        .unwrap_or_default();
    format!(
        "its {} timeout stopped it",
        crate::theme::fmt::format_compact_duration(timeout)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room() -> (tempfile::TempDir, StatePaths) {
        let dir = tempfile::tempdir().unwrap();
        let paths =
            StatePaths::for_project_root_under(&dir.path().join("project"), dir.path()).unwrap();
        std::fs::create_dir_all(&paths.calls_dir).unwrap();
        (dir, paths)
    }

    fn record(launch_id: &AgentSessionId, now: Timestamp) -> CallRecord {
        CallRecord {
            launch_id: launch_id.clone(),
            seats: Vec::new(),
            stay: false,
            schema: serde_json::json!({"type": "object", "required": ["verdict"]})
                .as_object()
                .cloned(),
            created_at: now,
            deadline: Some(now + Duration::from_secs(1800)),
            answer: None,
            asked: None,
            timed_out_at: None,
        }
    }

    #[test]
    fn a_fitting_answer_is_stored_and_a_misfit_or_schemaless_one_is_refused() {
        let (_dir, paths) = room();
        let launch: AgentSessionId = "launch-task".into();
        let now = Timestamp::now();
        let bare: AgentSessionId = "launch-bare".into();
        assert!(matches!(
            submit(&paths, &bare, "{}", now),
            Err(SubmitErr::NoSchema)
        ));
        crate::store::call::create(&paths, &record(&launch, now)).unwrap();
        assert!(crate::store::call::owes_answer(&paths, &launch).unwrap());

        let misfit = submit(&paths, &launch, r#"{"note": 1}"#, now).unwrap_err();
        assert_eq!(
            misfit.to_string(),
            "the answer does not fit its schema: `answer` lacks the required `verdict`; \
             nothing was stored"
        );
        assert!(matches!(
            submit(&paths, &launch, "not json", now),
            Err(SubmitErr::NotJson(_))
        ));
        let later = now + Duration::from_secs(1);
        assert_eq!(
            handed_answer(&paths, &launch, now, later),
            None,
            "a refusal stores nothing"
        );
        assert!(crate::store::call::owes_answer(&paths, &launch).unwrap());

        submit(&paths, &launch, r#"{"verdict": "ok"}"#, now).unwrap();
        assert_eq!(
            handed_answer(&paths, &launch, now, later).as_deref(),
            Some("{\n  \"verdict\": \"ok\"\n}")
        );
        assert_eq!(
            handed_answer(&paths, &launch, later, later),
            None,
            "an answer from before the turn is not its response"
        );
        assert_eq!(
            handed_answer(
                &paths,
                &launch,
                now - Duration::from_secs(1),
                now - Duration::from_secs(1)
            ),
            None,
            "an answer from after the turn ended is a later turn's"
        );
        assert!(!crate::store::call::owes_answer(&paths, &launch).unwrap());
    }

    #[test]
    fn a_deadline_is_claimed_once_and_only_once_it_has_passed() {
        let (_dir, paths) = room();
        let launch: AgentSessionId = "launch-timed".into();
        let now = Timestamp::now();
        crate::store::call::create(&paths, &record(&launch, now)).unwrap();
        assert!(claim_timeout(&paths, &launch, now).unwrap().is_none());
        let due = now + Duration::from_secs(1800);
        let claimed = claim_timeout(&paths, &launch, due)
            .unwrap()
            .expect("the claim");
        assert_eq!(claimed.timed_out_at, Some(due));
        assert_eq!(timeout_failure(&claimed), "its 30m timeout stopped it");
        assert!(claim_timeout(&paths, &launch, due).unwrap().is_none());
        assert!(
            !crate::store::call::owes_answer(&paths, &launch).unwrap(),
            "a stopped launch owes nothing"
        );
    }

    #[test]
    fn an_unreadable_record_is_skipped_by_the_listing() {
        let (_dir, paths) = room();
        let launch: AgentSessionId = "launch-listed".into();
        crate::store::call::create(&paths, &record(&launch, Timestamp::now())).unwrap();
        std::fs::write(paths.calls_dir.join("launch-torn.json"), b"{\"launch_id\":").unwrap();
        let listed: Vec<_> = crate::store::call::list(&paths)
            .unwrap()
            .into_iter()
            .map(|call| call.launch_id)
            .collect();
        assert_eq!(listed, [launch]);
    }
}
