//! Cursor's documented CLI-only account probe.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;

use crate::agents::account::{AccountProbe, AccountProbeError};
use crate::agents::{AgentAccount, AgentSpec, locate_binary};

#[derive(Debug)]
struct ProbeOutput {
    failure: Option<AccountProbeError>,
    stdout: Vec<u8>,
}

pub(super) fn probe(definition: &AgentSpec) -> AccountProbe {
    probe_with(definition, locate_binary, run)
}

fn run(binary: &Path, args: &[&str]) -> Option<ProbeOutput> {
    let mut command = Command::new(binary);
    command.args(args).stdin(Stdio::null());
    let output = crate::proc::run_bounded_output(
        &mut command,
        crate::agents::account::INFORMATIONAL_PROBE_TIMEOUT,
    )
    .ok()?;
    Some(ProbeOutput {
        failure: if output.timed_out {
            Some(AccountProbeError::TimedOut)
        } else if !output.status.success() {
            Some(AccountProbeError::ExitNonZero(output.status.code()))
        } else {
            None
        },
        stdout: output.stdout,
    })
}

fn probe_with(
    definition: &AgentSpec,
    locate: impl FnOnce(&AgentSpec) -> Option<PathBuf>,
    mut run: impl FnMut(&Path, &[&str]) -> Option<ProbeOutput>,
) -> AccountProbe {
    let Some(binary) = locate(definition) else {
        return AccountProbe::Unavailable(AccountProbeError::SpawnFailed);
    };
    let Some(status_output) = run(&binary, &["status", "--format", "json"]) else {
        return AccountProbe::Unavailable(AccountProbeError::SpawnFailed);
    };
    if let Some(cause) = status_output.failure {
        return AccountProbe::Unavailable(cause);
    }
    let Ok(status) = serde_json::from_slice::<StatusShape>(&status_output.stdout) else {
        return AccountProbe::Unavailable(AccountProbeError::UnparseableOutput);
    };
    let status_fact = match status.status.as_deref().map(parse_status_fact) {
        Some(Some(fact)) => Some(fact),
        Some(None) if non_blank(status.status).is_some() => {
            return AccountProbe::Unavailable(AccountProbeError::UnparseableOutput);
        }
        _ => None,
    };
    let auth_fact = match (status_fact, status.is_authenticated) {
        (Some(left), Some(right)) if left != right => {
            return AccountProbe::Unavailable(AccountProbeError::UnparseableOutput);
        }
        (Some(fact), _) | (_, Some(fact)) => fact,
        (None, None) => return AccountProbe::Unavailable(AccountProbeError::UnparseableOutput),
    };
    if !auth_fact {
        return AccountProbe::LoggedOut;
    }

    let Some(about_output) = run(&binary, &["about", "--format", "json"]) else {
        return AccountProbe::Unavailable(AccountProbeError::SpawnFailed);
    };
    if let Some(cause) = about_output.failure {
        return AccountProbe::Unavailable(cause);
    }
    let Ok(about) = serde_json::from_slice::<AboutShape>(&about_output.stdout) else {
        return AccountProbe::Unavailable(AccountProbeError::UnparseableOutput);
    };
    let status_email = status.user_info.and_then(|info| non_blank(info.email));
    let about_email = non_blank(about.user_email);
    if let (Some(status_email), Some(about_email)) = (&status_email, &about_email)
        && !status_email.eq_ignore_ascii_case(about_email)
    {
        return AccountProbe::Unavailable(AccountProbeError::UnparseableOutput);
    }
    let plan = non_blank(about.subscription_tier);
    AccountProbe::Found(AgentAccount {
        profile_fingerprint: None,
        account_id: status_email.or(about_email),
        metered: plan.as_ref().map(|_| true),
        plan,
        version: non_blank(about.cli_version),
        sub_provider: None,
        scope: Default::default(),
        credentials_updated_at_ms: None,
        ..Default::default()
    })
}

fn parse_status_fact(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "authenticated" => Some(true),
        "logged_out" | "logged-out" | "loggedout" | "unauthenticated" => Some(false),
        _ => None,
    }
}

fn non_blank(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StatusShape {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    is_authenticated: Option<bool>,
    #[serde(default)]
    user_info: Option<UserInfo>,
}

#[derive(Debug, Default, Deserialize)]
struct UserInfo {
    #[serde(default)]
    email: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AboutShape {
    #[serde(default)]
    cli_version: Option<String>,
    #[serde(default)]
    subscription_tier: Option<String>,
    #[serde(default)]
    user_email: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use super::*;

    fn cursor() -> &'static AgentSpec {
        crate::agents::spec_by_kind("cursor").unwrap()
    }

    fn output(stdout: &str) -> ProbeOutput {
        ProbeOutput {
            failure: None,
            stdout: stdout.as_bytes().to_vec(),
        }
    }

    fn probe_outputs(outputs: Vec<ProbeOutput>) -> (AccountProbe, Vec<(PathBuf, Vec<String>)>) {
        let calls = RefCell::new(Vec::new());
        let mut outputs = VecDeque::from(outputs);
        let probe = probe_with(
            cursor(),
            |_| Some(PathBuf::from("/opt/cursor/agent")),
            |binary, args| {
                calls.borrow_mut().push((
                    binary.to_path_buf(),
                    args.iter().map(|arg| (*arg).to_owned()).collect(),
                ));
                outputs.pop_front()
            },
        );
        (probe, calls.into_inner())
    }

    #[test]
    fn authenticated_pair_maps_identity_plan_version_and_reuses_binary() {
        let (probe, calls) = probe_outputs(vec![
            output(
                r#"{"status":"authenticated","isAuthenticated":true,"userInfo":{"email":" User@Example.com "},"newField":1}"#,
            ),
            output(
                r#"{"cliVersion":" 1.2.3 ","subscriptionTier":" Business ","userEmail":"user@example.COM","unknown":true}"#,
            ),
        ]);
        let AccountProbe::Found(account) = probe else {
            panic!("authenticated pair must be found");
        };
        assert_eq!(account.account_id.as_deref(), Some("User@Example.com"));
        assert_eq!(account.plan.as_deref(), Some("Business"));
        assert_eq!(account.version.as_deref(), Some("1.2.3"));
        assert_eq!(account.metered, Some(true));
        assert_eq!(
            calls,
            vec![
                (
                    PathBuf::from("/opt/cursor/agent"),
                    vec!["status".into(), "--format".into(), "json".into()]
                ),
                (
                    PathBuf::from("/opt/cursor/agent"),
                    vec!["about".into(), "--format".into(), "json".into()]
                ),
            ]
        );
    }

    #[test]
    fn logout_short_circuits_about() {
        for status in [r#"{"status":"logged_out"}"#, r#"{"isAuthenticated":false}"#] {
            let (probe, calls) = probe_outputs(vec![output(status)]);
            assert!(matches!(probe, AccountProbe::LoggedOut));
            assert_eq!(calls.len(), 1);
        }
    }

    #[test]
    fn either_positive_auth_fact_accepts_a_tierless_account() {
        for status in [
            r#"{"status":"authenticated"}"#,
            r#"{"isAuthenticated":true}"#,
        ] {
            let (probe, _) = probe_outputs(vec![output(status), output(r#"{}"#)]);
            let AccountProbe::Found(account) = probe else {
                panic!("positive auth fact must be found");
            };
            assert_eq!(account.metered, None);
            assert_eq!(account.plan, None);
        }
    }

    #[test]
    fn conflicts_unknown_shapes_and_email_mismatch_retry() {
        for outputs in [
            vec![output(
                r#"{"status":"authenticated","isAuthenticated":false}"#,
            )],
            vec![output(r#"{"status":"refreshing"}"#)],
            vec![output(r#"{"userInfo":{"email":"user@example.com"}}"#)],
            vec![
                output(r#"{"isAuthenticated":true,"userInfo":{"email":"one@example.com"}}"#),
                output(r#"{"userEmail":"two@example.com"}"#),
            ],
        ] {
            assert!(matches!(
                probe_outputs(outputs).0,
                AccountProbe::Unavailable(AccountProbeError::UnparseableOutput)
            ));
        }
    }

    #[test]
    fn malformed_spawn_and_nonzero_results_retry() {
        assert!(matches!(
            probe_outputs(vec![output("not json")]).0,
            AccountProbe::Unavailable(AccountProbeError::UnparseableOutput)
        ));
        let (spawn_failure, _) = probe_outputs(Vec::new());
        assert!(matches!(
            spawn_failure,
            AccountProbe::Unavailable(AccountProbeError::SpawnFailed)
        ));
        let (nonzero, _) = probe_outputs(vec![ProbeOutput {
            failure: Some(AccountProbeError::ExitNonZero(Some(1))),
            stdout: b"{}".to_vec(),
        }]);
        assert!(matches!(
            nonzero,
            AccountProbe::Unavailable(AccountProbeError::ExitNonZero(Some(1)))
        ));
        let (about_failure, _) = probe_outputs(vec![
            output(r#"{"isAuthenticated":true}"#),
            ProbeOutput {
                failure: Some(AccountProbeError::ExitNonZero(Some(1))),
                stdout: b"{}".to_vec(),
            },
        ]);
        assert!(matches!(
            about_failure,
            AccountProbe::Unavailable(AccountProbeError::ExitNonZero(Some(1)))
        ));
    }

    #[test]
    fn blank_optional_fields_are_discarded() {
        let (probe, _) = probe_outputs(vec![
            output(r#"{"isAuthenticated":true,"userInfo":{"email":" "}}"#),
            output(r#"{"cliVersion":" ","subscriptionTier":" ","userEmail":" "}"#),
        ]);
        let AccountProbe::Found(account) = probe else {
            panic!("authenticated blank optionals must still be found");
        };
        assert_eq!(account, AgentAccount::default());
    }
}
