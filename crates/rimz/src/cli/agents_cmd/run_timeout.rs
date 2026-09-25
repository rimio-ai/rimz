//! Hidden helper that settles an overdue supervised run and reclaims its pane.

use anyhow::{Context, Result};
use jiff::Timestamp;

use rimz::harness::deadline::{Rung, kill_due, stop_due_by_pane};
use rimz::harness::run_timeout::RunTimeoutRequest;
use rimz::store::run::RunRecord;

use super::Ctx;

pub fn run_timeout(request: RunTimeoutRequest, globals: &super::GlobalFlags) -> Result<()> {
    let paths = rimz::StatePaths::for_workspace(request.workspace_id.clone())
        .context("preparing store paths")?;
    let initial = rimz::harness::run::load(&paths, &request.run_id).context("loading timed run")?;
    let mux_hint = initial.pane_id.as_ref().map(|pane| pane.mux());
    let ctx = Ctx::for_workspace(request.workspace_id, mux_hint)?;
    let provider_process = initial
        .subagent
        .then(|| provider_process_for_run(&initial))
        .flatten();
    let now = Timestamp::now();
    if stop_due_by_pane(&initial, now) {
        if let Some(rung) =
            rimz::harness::run::claim_rung(ctx.store.paths(), &request.run_id, now, |rung| {
                matches!(rung, Rung::Stop { .. })
            })?
        {
            let Some(target) = deadline_stop_target(&initial) else {
                tracing::debug!(run_id = %request.run_id, "deadline stop has no agent session id");
                return Ok(());
            };
            let outcome = rimz::message::dispatch::dispatch(
                &ctx.workspace,
                &ctx.store,
                rimz::message::dispatch::DispatchRequest {
                    target,
                    text: rung.text(),
                    target_scope: None,
                    current_channel: None,
                    caller: None,
                    sender: rimz::store::message::MessageSender::Harness {
                        notice: rimz::store::message::HarnessNotice::Deadline,
                    },
                    automated: true,
                    allow_fanout: false,
                    reply: None,
                    mux: globals.mux,
                    mode: rimz::message::dispatch::DispatchMode::Steer {
                        enter: true,
                        force: false,
                        auto_compact: None,
                    },
                },
            );
            match outcome {
                Ok(outcome) => {
                    tracing::debug!(run_id = %request.run_id, outcomes = ?outcome.outcomes, "dispatched deadline stop")
                }
                Err(error) => {
                    tracing::warn!(run_id = %request.run_id, %error, "dispatching deadline stop failed")
                }
            }
        }
        return Ok(());
    }
    let harvest = kill_due(&initial, now)
        .then(|| harvest_last_message(&initial))
        .flatten();
    let (record, wrote) =
        rimz::harness::run::timeout_if_due(ctx.store.paths(), &request.run_id, now, harvest)?;
    let deadline_due = rimz::harness::deadline::kill_at(&record).is_some_and(|at| at <= now);
    if !wrote && !(record.status == rimz::store::run::RunStatus::TimedOut && deadline_due) {
        return Ok(());
    }
    if wrote {
        rimz::store::run::wake_run(ctx.store.runtime_paths(), &record);
    }
    if wrote && let Some((pid, process_start)) = provider_process {
        let _ = rimz::child_process::signal_process_term(pid, Some(&process_start));
    }
    if retains_pane_after_timeout(&record) {
        // The wrapper observes the terminal record and stops the provider, but
        // `--keep` retains the subagent pane until explicit stop or gc.
        return Ok(());
    }
    super::supervised::stop_supervised_run(&ctx.workspace, &ctx.store, globals, &record)
        .context("reclaiming timed-out run pane")
}

fn deadline_stop_target(record: &RunRecord) -> Option<String> {
    record
        .agent_id
        .as_ref()
        .filter(|id| !id.as_str().is_empty())
        .map(|id| format!("@{id}"))
}

fn harvest_last_message(record: &RunRecord) -> Option<String> {
    if record.last_message.is_some() {
        return None;
    }
    let path = record.transcript_path.as_deref()?;
    let adapter = rimz::agents::definition_by_kind(record.kind.as_str()).ok()?;
    match adapter.read_transcript_messages(std::path::Path::new(path), record.agent_id.as_ref()) {
        Ok(messages) => messages
            .into_iter()
            .rev()
            .find(|message| {
                message.role == rimz::agents::transcript::TranscriptRole::Assistant
                    && !message.text.trim().is_empty()
            })
            .map(|message| message.text),
        Err(error) => {
            tracing::warn!(run_id = %record.run_id, %error, "harvesting timed-out response failed");
            None
        }
    }
}

fn retains_pane_after_timeout(record: &rimz::store::run::RunRecord) -> bool {
    record.subagent && record.keep
}

fn provider_process_for_run(record: &rimz::store::run::RunRecord) -> Option<(u32, String)> {
    Some((record.provider_pid?, record.provider_process_start.clone()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_stop_resolves_only_its_child_across_channels() {
        let mut record = RunRecord::new(
            rimz::WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-run")),
            rimz::ids::AgentKind::new_unchecked("kimi"),
            rimz::agents::PermissionMode::Auto,
            "test".to_owned(),
            std::path::PathBuf::from("/tmp/rimz-run"),
        );
        let mut child = rimz::agents::AgentState::stub(
            "kimi",
            "session-child",
            rimz::agents::AgentStatus::Running,
        );
        child.name = Some("deadline-child".to_owned());
        child.channel = Some("child-lane".to_owned());
        let mut other = child.clone();
        other.agent_id = "session-other".into();
        other.channel = Some("producer-lane".to_owned());
        record.agent_id = Some(child.agent_id.clone());
        record.agent_name = child.name.clone();
        let target = deadline_stop_target(&record).unwrap();
        for channel in [Some("producer-lane"), Some("child-lane"), None] {
            let resolved =
                rimz::address::resolve_agent(&target, None, channel, &[&child, &other]).unwrap();
            assert_eq!(resolved.agent_id, child.agent_id);
        }
        record.agent_name = None;
        assert_eq!(deadline_stop_target(&record), Some(target));
        record.agent_id = None;
        assert_eq!(deadline_stop_target(&record), None);
    }

    #[test]
    fn only_kept_subagent_timeouts_retain_the_pane() {
        let mut record = rimz::store::run::RunRecord::new(
            rimz::WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-run")),
            rimz::ids::AgentKind::new_unchecked("codex"),
            rimz::agents::PermissionMode::Auto,
            "test".to_owned(),
            std::path::PathBuf::from("/tmp/rimz-run"),
        );
        assert!(!retains_pane_after_timeout(&record));

        record.subagent = true;
        assert!(!retains_pane_after_timeout(&record));

        record.keep = true;
        assert!(retains_pane_after_timeout(&record));
    }

    #[test]
    fn timeout_backstop_uses_persisted_provider_not_wrapper_owner() {
        let mut record = rimz::store::run::RunRecord::new(
            rimz::WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-run")),
            rimz::ids::AgentKind::new_unchecked("codex"),
            rimz::agents::PermissionMode::Auto,
            "test".to_owned(),
            std::path::PathBuf::from("/tmp/rimz-run"),
        );
        record.provider_pid = Some(42);
        record.provider_process_start = Some("provider-start".to_owned());
        let mut provisional = rimz::agents::AgentState::stub(
            "codex",
            "launch-child",
            rimz::agents::AgentStatus::Running,
        );
        provisional.runtime_owner = Some(rimz::store::runtime::process_owner(
            rimz::RuntimeOwnerKind::Agent,
            provisional.agent_id.as_str(),
            84,
        ));

        assert_eq!(
            provider_process_for_run(&record),
            Some((42, "provider-start".to_owned()))
        );
        assert_ne!(
            provider_process_for_run(&record).map(|(pid, _)| pid),
            provisional.runtime_owner.map(|owner| owner.pid)
        );
    }
}
