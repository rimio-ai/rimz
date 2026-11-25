//! Declared lifecycle-event subscribers for hook-triggered nudges.

use super::*;

const DELIVERY_FILTER: rimz::agents::SignalSet =
    rimz::agents::DELIVERY_CHECKPOINT.union(rimz::agents::CONDITION_CHECKPOINT);
const RUN_WAKE_FILTER: rimz::agents::SignalSet = rimz::agents::SignalSet::TURN_ENDED
    .union(rimz::agents::SignalSet::TURN_INTERRUPTED)
    .union(rimz::agents::SignalSet::ENDED);
const ARCHIVE_FILTER: rimz::agents::SignalSet = rimz::agents::SignalSet::ENDED;
const MOLT_CLOSE_FILTER: rimz::agents::SignalSet = rimz::agents::SignalSet::TURN_ENDED;

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
    Reactor {
        filter: MOLT_CLOSE_FILTER,
        run: molt_close,
    },
];

pub(super) struct ReactorCtx<'a> {
    pub(super) workspace: &'a ResolvedWorkspace,
    pub(super) store: &'a Store,
    pub(super) primary_event_id: Option<&'a rimz::ids::EventId>,
    pub(super) run_completion: Option<&'a rimz::store::run::RunRecord>,
    /// The events' row is a replaced session of its agent.
    pub(super) replaced: bool,
    /// The observation's launch, which the events do not carry.
    pub(super) launch: Option<&'a rimz::ids::AgentSessionId>,
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
    if ctx.replaced {
        return;
    }
    spawn_queue_delivery_if_checkpoint(ctx.workspace, ctx.store, event, ctx.launch);
}

fn run_wake(ctx: &ReactorCtx<'_>, event: &rimz::agents::LifecycleEvent) {
    if ctx.primary_event_id != Some(&event.event_id) {
        return;
    }
    let Some(record) = ctx.run_completion else {
        return;
    };
    rimz::store::run::wake_run(ctx.store.runtime_paths(), record);
    if record.peer.is_some()
        && let Ok(rows) = ctx.store.audit_rows()
        && let Some(peer) = rows.find(&event.kind, &event.agent_id)
        && let Some(launcher) = peer.launched_by.as_ref()
    {
        rimz::harness::orphan_sweep::spawn_digest_helper(
            ctx.store.runtime_paths(),
            launcher.agent_id.clone(),
        );
    }
}

/// An end archives the queue's debt to the agent only once the agent has
/// ended; a replaced session's end, or one whose agent lives on, archives
/// nothing.
fn archive_ended(ctx: &ReactorCtx<'_>, event: &rimz::agents::LifecycleEvent) {
    if ctx.replaced {
        return;
    }
    if let Err(err) = rimz::message::deliver::archive_for_ended_agent(
        ctx.store,
        &ctx.workspace.session_name,
        &event.kind,
        &event.agent_id,
        event.agent_name.as_deref(),
    ) {
        warn!(
            error = %err,
            kind = %event.kind,
            agent_id = %event.agent_id,
            "lifecycle: failed to archive messages for ended agent",
        );
    }
}

/// A turn end of a session a molt replaced hands its booked close to the helper.
fn molt_close(ctx: &ReactorCtx<'_>, event: &rimz::agents::LifecycleEvent) {
    if ctx.replaced {
        rimz::launcher::molt::close_after_turn(ctx.store, &event.kind, &event.agent_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::agents::ToolEffect;

    fn signals() -> Vec<LifecycleSignal> {
        vec![
            LifecycleSignal::Registered,
            LifecycleSignal::TurnStarted { turn_id: None },
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
                turn_id: None,
            },
            LifecycleSignal::TurnInterrupted { turn_id: None },
            LifecycleSignal::SubagentStarted,
            LifecycleSignal::SubagentStopped { errored: false },
            LifecycleSignal::ToolUsed {
                effect: ToolEffect::Mutating,
                name: None,
                native_key: None,
                turn_id: None,
            },
            LifecycleSignal::AwaitingInput {
                extra_questions: 0,
                kind: rimz::agents::AskKind::Question,
                ask_id: None,
                detail: None,
                native_key: None,
            },
            LifecycleSignal::Compacting,
            LifecycleSignal::CompactionEnded {
                auto: None,
                failed: false,
                echo: false,
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
