//! The one headless judge: the workflow answer contract, a bounded attempt classed
//! by how it failed, and the retry rule workflow judges use.

use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use serde_json::Value;

use super::{HeadlessErr, HeadlessProfile, ProcessErr};
use crate::agents::{AgentDefinition, HeadlessErrorKind, HeadlessRequest, RateLimitsCache};
use crate::config::MachineConfig;
use crate::disk::paths::RuntimePaths;
use crate::harness::launch::{ExecAction, ExecIdentity, ExecRequest, ReminderSection};
use crate::harness::launch_plan::LaunchPlan;
use crate::proc::BoundedOutput;

const WORKFLOW_SCHEMA: &str = r#"{"type":"object","properties":{"outcome":{"type":"string"},"reason":{"type":"string"},"recheck_at":{"type":"string"},"key":{}},"required":["outcome"]}"#;

/// A closed outcome-word answer contract, the fixture adapter tests decode a closed schema with.
#[cfg(test)]
pub(crate) const LOOP_ANSWER_SCHEMA: &str = r#"{"type":"object","properties":{"outcome":{"type":"string","enum":["pass","fail"]},"reason":{"type":"string"}},"required":["outcome","reason"],"additionalProperties":false}"#;

/// The answer [`LOOP_ANSWER_SCHEMA`] admits; a decode failure is a shape the schema refuses.
#[cfg(test)]
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LoopAnswer {
    #[expect(dead_code, reason = "decoded to pin the closed shape")]
    outcome: LoopOutcome,
    #[expect(dead_code, reason = "decoded to pin the closed shape")]
    reason: String,
}

#[cfg(test)]
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum LoopOutcome {
    Pass,
    Fail,
}

#[cfg(test)]
impl LoopAnswer {
    pub(crate) fn from_value(value: Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(value)
    }
}

/// How a judge attempt failed. The labels are the words workflow journals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FailureKind {
    /// The process failed, exited nonzero, or returned no readable object.
    Died,
    /// The object does not satisfy the answer contract.
    Invalid,
    /// The judge outran its own deadline.
    Timeout,
    /// A provider limit stopped the judge.
    Limited,
    /// The judge was interrupted.
    Canceled,
}

impl FailureKind {
    fn label(self) -> &'static str {
        match self {
            Self::Died => "died",
            Self::Invalid => "invalid",
            Self::Timeout => "timeout",
            Self::Limited => "limited",
            Self::Canceled => "canceled",
        }
    }

    pub(crate) fn from_label(label: &str) -> Option<Self> {
        [
            Self::Died,
            Self::Invalid,
            Self::Timeout,
            Self::Limited,
            Self::Canceled,
        ]
        .into_iter()
        .find(|kind| kind.label() == label)
    }

    /// The one judge retry rule: a first `died` or `invalid` attempt is owed exactly
    /// one more, and no other failure ever is.
    pub(crate) fn retry_owed(self, retried: bool) -> bool {
        !retried && matches!(self, Self::Died | Self::Invalid)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Failure {
    pub kind: FailureKind,
    pub text: String,
    /// When a `limited` judge's spent window resets, if the account cache knows.
    pub reset_at: Option<Timestamp>,
}

impl Failure {
    fn new(kind: FailureKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
            reset_at: None,
        }
    }

    /// The failure of a judge whose `timeout` deadline ran out.
    pub(crate) fn timed_out(timeout: Duration) -> Self {
        Self::new(
            FailureKind::Timeout,
            format!(
                "judge timed out after {}",
                crate::harness::schedule::arm::duration_label(timeout)
            ),
        )
    }
}

/// One judge process run to its end.
pub(crate) struct Attempt {
    pub answer: Result<Value, Failure>,
    pub cost_usd: Option<f64>,
    /// The process output, absent when the judge never ran to an exit.
    pub output: Option<BoundedOutput>,
}

pub(crate) struct Request<'a> {
    pub prompt: String,
    pub reminder: ReminderSection,
    pub identity: ExecIdentity,
    pub dir: &'a Path,
    pub root: &'a Path,
    pub schema_file: PathBuf,
    pub verdict_file: PathBuf,
    /// The deadline the caller's execute step enforces, named in a timeout's text.
    pub timeout: Duration,
}

/// A judge launch plan, ready for as many attempts as the retry rule allows.
pub(crate) struct Prepared {
    plan: LaunchPlan,
    adapter: &'static AgentDefinition,
    login: crate::ids::LoginKey,
    headless: HeadlessRequest,
    runtime: RuntimePaths,
    timeout: Duration,
    _schema: SchemaFile,
}

/// Provider schema files are transient launch artifacts, removed with the plan.
struct SchemaFile(PathBuf);

impl Drop for SchemaFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Compiles and materializes the judge launch, returning its launch warnings.
pub(crate) fn prepare(
    profile: HeadlessProfile,
    request: Request<'_>,
    config: &MachineConfig,
    runtime: &RuntimePaths,
) -> Result<(Prepared, Vec<String>), HeadlessErr> {
    let schema = WORKFLOW_SCHEMA;
    let headless = HeadlessRequest {
        schema_open: true,
        schema: schema.into(),
        schema_file: request.schema_file,
        verdict_file: request.verdict_file,
    };
    std::fs::write(&headless.schema_file, schema)?;
    let schema_file = SchemaFile(headless.schema_file.clone());
    let adapter = profile.adapter;
    let login = profile.login.key();
    let mut exec = ExecRequest::fresh(&profile.cell, request.identity, None, false);
    exec.reminder_section = Some(request.reminder);
    exec.action = ExecAction::Launch {
        prompt: Some(request.prompt),
        extra_args: profile.cell.args.clone(),
    };
    exec.headless = Some(headless.clone());
    let (plan, warnings) = profile.prepare(exec, config, request.dir, request.root)?;
    Ok((
        Prepared {
            plan,
            adapter,
            login,
            headless,
            runtime: runtime.clone(),
            timeout: request.timeout,
            _schema: schema_file,
        },
        warnings,
    ))
}

impl Prepared {
    pub(crate) fn model(&self) -> Option<&str> {
        self.plan.request.identity.params.model.as_deref()
    }

    /// Runs one attempt. Launch refusals are `Err`; every process-level end is an
    /// attempt, classed by how it failed.
    pub(crate) fn attempt(
        &self,
        execute: impl FnOnce(
            &AgentDefinition,
            &LaunchPlan,
            &HeadlessRequest,
        ) -> Result<BoundedOutput, ProcessErr>,
    ) -> Result<Attempt, ProcessErr> {
        let output = match execute(self.adapter, &self.plan, &self.headless) {
            Ok(output) => output,
            Err(ProcessErr::Io(error)) => {
                let kind = if error.kind() == std::io::ErrorKind::Interrupted {
                    FailureKind::Canceled
                } else {
                    FailureKind::Died
                };
                return Ok(Attempt {
                    answer: Err(Failure::new(kind, error.to_string())),
                    cost_usd: None,
                    output: None,
                });
            }
            Err(refusal) => return Err(refusal),
        };
        let form = self
            .adapter
            .spec()
            .launch
            .headless
            .expect("resolved headless profile");
        let prices = crate::agents::pricing::cached_book(&self.runtime.shared_pricing_table_path());
        let response = form.read_result(
            &output.stdout,
            &output.stderr,
            &self.headless,
            self.model(),
            self.plan.request.identity.params.fast,
            &prices,
        );
        let answer = self.classify(&output, response.verdict, response.error);
        Ok(Attempt {
            answer,
            cost_usd: response.cost_usd,
            output: Some(output),
        })
    }

    fn classify(
        &self,
        output: &BoundedOutput,
        verdict: Option<Value>,
        error: Option<crate::agents::HeadlessError>,
    ) -> Result<Value, Failure> {
        if output.timed_out {
            return Err(Failure::timed_out(self.timeout));
        }
        if let Some(error) = &error
            && error.kind == HeadlessErrorKind::ProviderLimit
        {
            let cache = crate::agents::account::read_rate_limits_cache(
                &RateLimitsCache::shared_path(&self.runtime),
            );
            return Err(Failure {
                kind: FailureKind::Limited,
                text: error.text.clone(),
                reset_at: latest_limit_reset(&cache, &self.login, Timestamp::now(), self.model()),
            });
        }
        if !output.status.success() {
            return Err(Failure::new(
                FailureKind::Died,
                error.map_or_else(
                    || super::exit_failure("judge", output.status),
                    |error| error.text,
                ),
            ));
        }
        object(verdict, &self.headless.verdict_file).map_err(|reason| {
            Failure::new(FailureKind::Died, error.map_or(reason, |error| error.text))
        })
    }
}

fn latest_limit_reset(
    cache: &RateLimitsCache,
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

/// The provider's structured answer, saved at the verdict path, else the file the
/// judge wrote there.
fn object(verdict: Option<Value>, output_path: &Path) -> Result<Value, String> {
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
mod tests;
