use super::*;
use crate::agents::{
    HeadlessErrorKind, HeadlessRequest, RateLimitsCache, WorkflowAttemptRef, WorkflowAttemptRole,
};
use crate::harness::headless::HeadlessProfile;
use crate::harness::launch::{ExecAction, ExecIdentity, ExecRequest};

const SCHEMA: &str = r#"{"type":"object","properties":{"outcome":{"type":"string"},"reason":{"type":"string"},"recheck_at":{"type":"string"},"key":{}},"required":["outcome"]}"#;

pub(super) fn evaluate(
    state: &StatePaths,
    paths: &WorkflowPaths,
    spec: &WorkflowSpec,
    record: &RunRecord,
) -> Evaluation {
    let mut result = Evaluation::failed("died", "judge interrupted");
    if let Err(error) = run(state, paths, spec, record, &mut result) {
        result.object = Err(error);
    }
    result
}

fn run(
    state: &StatePaths,
    paths: &WorkflowPaths,
    spec: &WorkflowSpec,
    record: &RunRecord,
    result: &mut Evaluation,
) -> Result<(), String> {
    let check = &spec.checks[&record.node];
    // Agent checks were validated with a profile and a confined prompt file.
    let file = check
        .prompt_file
        .as_ref()
        .ok_or("judge needs a prompt-file")?;
    let prompt_file = spec::confined_file(&record.spec_dir, file).map_err(|e| e.to_string())?;
    let prompt = std::fs::read_to_string(prompt_file).map_err(|e| e.to_string())?;
    let digests = BTreeMap::from([(file.clone(), digest(prompt.as_bytes()))]);
    let attempt = record.attempt.as_ref().expect("prepared judge attempt");
    let output_path = paths.check_output(attempt.seq);
    let prompt = format!(
        "{prompt}\n\nRIMZ_WORKFLOW_DIR is {}. Read context.json there; output_path is {}. Your final message must be exactly one JSON object: required string outcome; optional string reason, RFC 3339 string recheck_at, and string-or-integer key. Any other fields are carried to the next node as input. Use no prose and no Markdown fence. The scope checkout is read-only; write signs only under signs/<step>.md in RIMZ_WORKFLOW_DIR.",
        paths.work.display(),
        output_path.display()
    );
    let config = crate::config::MachineConfig::load_lenient();
    let runtime = crate::disk::paths::RuntimePaths::for_state(state).map_err(|e| e.to_string())?;
    let profile = HeadlessProfile::resolve(
        &config,
        check.agent.as_deref().ok_or("judge needs an agent")?,
        &record.project_root,
        state,
        &runtime,
        Timestamp::now(),
    )
    .map_err(|e| e.to_string())?;
    let adapter = profile.adapter;
    let login = profile.login.key();
    let form = adapter
        .spec()
        .launch
        .headless
        .expect("resolved headless profile");
    let mut identity = ExecIdentity {
        params: profile.cell.launch.clone(),
        ..Default::default()
    };
    identity.params.workflow = Some(WorkflowAttemptRef {
        run: record.id.clone(),
        seq: attempt.seq,
        role: WorkflowAttemptRole::Judge,
    });
    let mut request = ExecRequest::fresh(&profile.cell, identity, None, false);
    request.action = ExecAction::Launch {
        prompt: Some(prompt),
        extra_args: profile.cell.args.clone(),
    };
    let schema_file = state
        .ensure_temp_unit(None)
        .map_err(|e| e.to_string())?
        .join(format!(
            "workflow-judge-{}.schema.json",
            crate::ids::RunId::new()
        ));
    let headless = HeadlessRequest {
        schema_open: true,
        schema: SCHEMA.into(),
        schema_file,
        verdict_file: output_path,
    };
    // Provider schema files are transient launch artifacts, outside the durable run home.
    std::fs::write(&headless.schema_file, SCHEMA).map_err(|e| e.to_string())?;
    let evaluated = (|| {
        request.headless = Some(headless.clone());
        let (plan, warnings) = profile
            .prepare(request, &config, &record.scope.path, &record.project_root)
            .map_err(|e| e.to_string())?;
        for warning in warnings {
            tracing::warn!(%warning, "workflow judge launch warning");
        }
        let admitted =
            crate::harness::headless::admit(adapter, &plan).map_err(|e| e.to_string())?;
        let output = with_child(
            state,
            paths,
            spec,
            record,
            digests,
            |interrupted, spawned| {
                admitted
                    .execute_hosted(Duration::from_secs(u32::MAX.into()), interrupted, spawned)
                    .map_err(|e| e.to_string())
            },
        )
        .map_err(|error| error.to_string())?;
        let prices = crate::agents::pricing::cached_book(&runtime.shared_pricing_cache_path());
        let response = form.read_result(
            &output.stdout,
            &output.stderr,
            &headless,
            plan.request.identity.params.model.as_deref(),
            plan.request.identity.params.fast,
            &prices,
        );
        result.cost_usd = response.cost_usd;
        result.stderr_tail = Some(crate::proc::tail_output(&output.stderr, 4096));
        if let Some(error) = &response.error
            && error.kind == HeadlessErrorKind::ProviderLimit
        {
            result.failure = "limited";
            let cache = crate::agents::account::read_rate_limits_cache(
                &RateLimitsCache::shared_path(&runtime),
            );
            result.limited = Some(workflow::Limited {
                reset_at: latest_limit_reset(
                    &cache,
                    &login,
                    Timestamp::now(),
                    plan.request.identity.params.model.as_deref(),
                ),
            });
            return Err(error.text.clone());
        }
        if !output.status.success() {
            return Err(response.error.map_or_else(
                || format!("judge exited {}", output.status),
                |error| error.text,
            ));
        }
        result.object = object(response.verdict, &headless.verdict_file)
            .map_err(|reason| response.error.map_or(reason, |error| error.text));
        Ok(())
    })();
    let _ = std::fs::remove_file(&headless.schema_file);
    evaluated
}

fn latest_limit_reset(
    cache: &crate::agents::RateLimitsCache,
    login: &crate::ids::LoginKey,
    now: Timestamp,
    model: Option<&str>,
) -> Option<Timestamp> {
    cache
        .entries
        .iter()
        .filter(|(key, _)| key.login == *login)
        .filter_map(|(_, entry)| {
            crate::agents::ProviderCapacity::from_windows(entry.limits.windows.clone())
                .latest_spent_window_reset_for_model(now, model)
        })
        .max()
}

fn object(verdict: Option<Value>, output_path: &std::path::Path) -> Result<Value, String> {
    if let Some(verdict) = verdict {
        crate::disk::atomic::write_temp_then_rename(output_path, &verdict)
            .map_err(|error| error.to_string())?;
        return Ok(verdict);
    }
    let bytes =
        std::fs::read(output_path).map_err(|error| format!("judge returned no object: {error}"))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("invalid judge object: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn provider_limit_uses_the_latest_spent_reset_across_only_the_requested_login() {
        use crate::agents::{
            AgentRateLimits, RateLimitCacheEntry, RateLimitWindow, RateLimitsCache,
        };
        let now = Timestamp::from_second(2_000_000_000).unwrap();
        let entries = [
            ("pi@default/anthropic/oauth", 100, 1),
            ("pi@default/openai/oauth", 100, 3),
            ("pi@work/openai/oauth", 100, 4),
            ("pi@default", 0, 5),
        ]
        .into_iter()
        .map(|(key, used, hours)| {
            let key: crate::agents::account::AccountUsageKey = key.parse().unwrap();
            let entry = RateLimitCacheEntry {
                scope: key.scope.clone(),
                limits: AgentRateLimits {
                    windows: vec![RateLimitWindow {
                        used_percentage: Some(used),
                        duration_mins: Some(300),
                        resets_at: Some(now + jiff::SignedDuration::from_hours(hours)),
                        ..Default::default()
                    }],
                },
                ..Default::default()
            };
            (key, entry)
        })
        .collect();
        let cache = RateLimitsCache {
            entries,
            ..Default::default()
        };
        assert_eq!(
            latest_limit_reset(&cache, &"pi@default".parse().unwrap(), now, None),
            Some(now + jiff::SignedDuration::from_hours(3))
        );
    }

    #[test]
    fn structured_object_is_saved_before_settlement() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("check.json");
        let value = json!({"outcome":"ok", "detail":"structured"});
        assert_eq!(object(Some(value.clone()), &path), Ok(value.clone()));
        let saved = std::fs::read(&path);
        assert!(
            saved.is_ok(),
            "structured output must be saved at output_path"
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&saved.unwrap()).unwrap(),
            value
        );
    }

    #[test]
    fn file_object_is_used_without_structured_output() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("check.json");
        let value = json!({"outcome":"ok", "detail":"file"});
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(
            object(None, &path),
            Ok(value),
            "output_path is the verdict fallback"
        );
    }
}
