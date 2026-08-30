//! Declared lifecycle-event subscribers for hook-triggered nudges.

use super::*;

const DELIVERY_FILTER: rimz::agents::SignalSet =
    rimz::agents::DELIVERY_CHECKPOINT.union(rimz::agents::CONDITION_CHECKPOINT);
const RUN_WAKE_FILTER: rimz::agents::SignalSet = rimz::agents::SignalSet::TURN_ENDED
    .union(rimz::agents::SignalSet::TURN_INTERRUPTED)
    .union(rimz::agents::SignalSet::ENDED);
const ARCHIVE_FILTER: rimz::agents::SignalSet = rimz::agents::SignalSet::ENDED;

struct Reactor {
    filter: rimz::agents::SignalSet,
    run: fn(&ReactorCtx<'_>, &rimz::agents::LifecycleEvent),
}

static REACTORS: &[Reactor] = &[
    Reactor {
        filter: DELIVERY_FILTER,
        run: queued_delivery,
    },
    Reactor {
        filter: RUN_WAKE_FILTER,
        run: run_wake,
    },
    Reactor {
        filter: ARCHIVE_FILTER,
        run: archive_ended,
    },
];

pub(super) struct ReactorCtx<'a> {
    pub(super) workspace: &'a ResolvedWorkspace,
    pub(super) store: &'a Store,
    pub(super) primary_event_id: Option<&'a rimz::ids::EventId>,
    pub(super) run_completion: Option<&'a rimz::harness::run::RunRecord>,
}

pub(super) fn dispatch(ctx: &ReactorCtx<'_>, events: &[rimz::agents::LifecycleEvent]) {
    for event in events {
        for reactor in REACTORS {
            if reactor.filter.contains(&event.signal) {
                (reactor.run)(ctx, event);
            }
        }
    }
}

fn queued_delivery(ctx: &ReactorCtx<'_>, event: &rimz::agents::LifecycleEvent) {
    spawn_queue_delivery_if_checkpoint(ctx.workspace, ctx.store, event);
}

fn run_wake(ctx: &ReactorCtx<'_>, event: &rimz::agents::LifecycleEvent) {
    if ctx.primary_event_id != Some(&event.event_id) {
        return;
    }
    let Some(record) = ctx.run_completion else {
        return;
    };
    if let Err(err) = rimz::harness::run_wake::wake_run(ctx.store.runtime_paths(), record) {
        warn!(
            kind = %event.kind,
            agent_id = %event.agent_id,
            run_id = %record.run_id,
            error = %err,
            "lifecycle: failed to wake the completed run",
        );
    }
}

fn archive_ended(ctx: &ReactorCtx<'_>, event: &rimz::agents::LifecycleEvent) {
    if let Err(err) = ctx.store.archive_messages_watching_card(
        &event.kind,
        &event.agent_id,
        event.agent_name.as_deref(),
        &ctx.workspace.session_name,
    ) {
        warn!(
            error = %err,
            kind = %event.kind,
            agent_id = %event.agent_id,
            "lifecycle: failed to archive messages watching ended agent",
        );
    }
    if let Err(err) = ctx.store.archive_messages_for_card(
        &event.kind,
        &event.agent_id,
        event.agent_name.as_deref(),
        "receiver ended",
        &ctx.workspace.session_name,
    ) {
        warn!(
            error = %err,
            kind = %event.kind,
            agent_id = %event.agent_id,
            "lifecycle: failed to archive receiver messages",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signals() -> Vec<LifecycleSignal> {
        vec![
            LifecycleSignal::Registered,
            LifecycleSignal::TurnStarted,
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
            LifecycleSignal::TurnInterrupted { turn_id: None },
            LifecycleSignal::SubagentStarted,
            LifecycleSignal::SubagentStopped { errored: false },
            LifecycleSignal::ToolUsed {
                mutates: true,
                edits: false,
                name: None,
                native_key: None,
                turn_id: None,
            },
            LifecycleSignal::AwaitingInput {
                kind: rimz::agents::AskKind::Question,
                ask_id: None,
                detail: None,
                native_key: None,
            },
            LifecycleSignal::Compacting,
            LifecycleSignal::CompactionEnded {
                auto: None,
                failed: false,
            },
            LifecycleSignal::Ended,
            LifecycleSignal::Lost,
        ]
    }

    #[test]
    fn reactor_filters_declare_their_complete_trigger_tables() {
        let signals = signals();
        let matching = |filter: rimz::agents::SignalSet| {
            signals
                .iter()
                .filter(|signal| filter.contains(signal))
                .map(LifecycleSignal::tag)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            matching(DELIVERY_FILTER),
            vec![
                "registered",
                "turn_started",
                "turn_ended",
                "turn_interrupted",
                "subagent_started",
                "subagent_stopped",
                "awaiting_input",
                "compaction_ended",
            ]
        );
        assert_eq!(
            matching(RUN_WAKE_FILTER),
            vec!["turn_ended", "turn_interrupted", "ended"]
        );
        assert_eq!(matching(ARCHIVE_FILTER), vec!["ended"]);
    }
}
