use super::*;
use crate::diag::DiagSink;
use crate::diag::record::DiagEvent;
use crate::harness::assist_log::{self, Assist, AssistRecord};
use crate::store::workflow::EvaluationOrigin;

pub(super) fn transition(t: &Transition<'_>) {
    let explicit_route = t
        .entries
        .iter()
        .any(|entry| matches!(entry.event, JournalEvent::Route { .. }));
    for entry in &t.entries {
        let route = match &entry.event {
            JournalEvent::Route { outcome, .. } if t.record.wait.is_none() => {
                Some((outcome.clone(), None))
            }
            JournalEvent::Evaluation {
                origin: EvaluationOrigin::Check,
                outcome,
                ..
            } if !explicit_route && t.record.wait.is_none() => Some((outcome.clone(), None)),
            JournalEvent::Evaluation {
                origin: EvaluationOrigin::Engine,
                outcome,
                object,
                ..
            } if !explicit_route => {
                if t.record.state == RunState::Active && outcome == "canceled" {
                    diagnostic(
                        &t.record,
                        DiagEvent::WorkflowRerun {
                            run: t.record.id.clone(),
                            node: entry.node.clone(),
                            seq: entry.seq.unwrap_or_default(),
                        },
                    );
                    continue;
                }
                if outcome == "limited" || t.record.state == RunState::Canceled {
                    continue;
                }
                if t.record.state == RunState::Active && t.record.check_retry_context.is_some() {
                    diagnostic(
                        &t.record,
                        DiagEvent::WorkflowRetry {
                            run: t.record.id.clone(),
                            node: entry.node.clone(),
                            seq: entry.seq.unwrap_or_default(),
                            outcome: outcome.clone(),
                            reason: object["reason"].as_str().unwrap_or_default().to_owned(),
                        },
                    );
                    continue;
                }
                Some((outcome.clone(), None))
            }
            JournalEvent::Attempt { outcome, .. }
                if !explicit_route && t.record.state != RunState::Canceled =>
            {
                Some((
                    match outcome {
                        AttemptOutcome::Ended => "ended",
                        AttemptOutcome::Died => "died",
                        AttemptOutcome::Timeout => "timeout",
                        AttemptOutcome::Asked => "asked",
                        AttemptOutcome::Limited => "limited",
                        AttemptOutcome::Canceled => "canceled",
                    }
                    .into(),
                    None,
                ))
            }
            JournalEvent::Wait { outcome, count, .. } => Some((outcome.clone(), Some(*count))),
            JournalEvent::Park { reason, text } => {
                assist_log::append(&AssistRecord {
                    at: entry.at,
                    assist: Assist::WorkflowPark {
                        run: t.record.id.clone(),
                        workflow: t.record.workflow.clone(),
                        node: entry.node.clone(),
                        reason: *reason,
                        text: text.clone(),
                    },
                });
                None
            }
            JournalEvent::Terminal { state } => {
                assist_log::append(&AssistRecord {
                    at: entry.at,
                    assist: Assist::WorkflowEnd {
                        run: t.record.id.clone(),
                        workflow: t.record.workflow.clone(),
                        state: *state,
                    },
                });
                None
            }
            _ => None,
        };
        if let Some((outcome, count)) = route {
            let target = match (count, t.record.state) {
                (Some(_), _) => "@wait",
                (_, RunState::Parked) => "@user",
                (_, RunState::Done) => "@done",
                (_, RunState::Failed) => "@failed",
                (_, RunState::Active) if t.record.wait.is_some() => "@wait",
                _ => &t.record.node,
            };
            assist_log::append(&AssistRecord {
                at: entry.at,
                assist: Assist::WorkflowRoute {
                    run: t.record.id.clone(),
                    workflow: t.record.workflow.clone(),
                    node: entry.node.clone(),
                    outcome,
                    to: target.into(),
                    count,
                },
            });
        }
    }
}

fn diagnostic(record: &RunRecord, event: DiagEvent) {
    match crate::workspace::WorkspaceResolver::resolve(
        &record.scope.path,
        Some(record.project_root.clone()),
    ) {
        Ok(workspace) => {
            DiagSink::for_workspace(workspace.workspace_id, workspace.session_name, None)
                .emit(event)
        }
        Err(error) => tracing::debug!(%error, "workflow diagnostic unavailable"),
    }
}

pub(super) fn step(record: &RunRecord) {
    let Some((attempt, run)) = record
        .attempt
        .as_ref()
        .and_then(|attempt| Some((attempt, attempt.run.as_ref()?)))
    else {
        return;
    };
    let visit = record
        .visits
        .get(&record.node)
        .and_then(|visits| visits.keys.iter().find(|visit| visit.key == record.key))
        .map_or(0, |visit| visit.visits);
    assist_log::append(&AssistRecord {
        at: Timestamp::now(),
        assist: Assist::WorkflowStep {
            run: record.id.clone(),
            workflow: record.workflow.clone(),
            step: record.node.clone(),
            visit,
            seq: attempt.seq,
            handle: Some(run.handle.clone()),
            run_id: Some(run.run_id.clone()),
        },
    });
}
