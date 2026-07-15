//! Cursor's documented CLI-only account probe.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;

use crate::agents::account::AccountProbe;
use crate::agents::{AgentAccount, AgentDescriptor, locate_binary};

#[derive(Debug)]
struct ProbeOutput {
    success: bool,
    stdout: Vec<u8>,
}

pub(super) fn probe(descriptor: &AgentDescriptor) -> AccountProbe {
    probe_with(descriptor, locate_binary, run)
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
        success: !output.timed_out && output.status.success(),
        stdout: output.stdout,
    })
}

fn probe_with(
    descriptor: &AgentDescriptor,
    locate: impl FnOnce(&AgentDescriptor) -> Option<PathBuf>,
    mut run: impl FnMut(&Path, &[&str]) -> Option<ProbeOutput>,
) -> AccountProbe {
    let Some(binary) = locate(descriptor) else {
        return AccountProbe::Unavailable;
    };
    let Some(status_output) = run(&binary, &["status", "--format", "json"]) else {
        return AccountProbe::Unavailable;
    };
    if !status_output.success {
        return AccountProbe::Unavailable;
    }
    let Ok(status) = serde_json::from_slice::<StatusShape>(&status_output.stdout) else {
        return AccountProbe::Unavailable;
    };
    let status_fact = match status.status.as_deref().map(parse_status_fact) {
        Some(Some(fact)) => Some(fact),
        Some(None) if non_blank(status.status).is_some() => return AccountProbe::Unavailable,
        _ => None,
    };
    let auth_fact = match (status_fact, status.is_authenticated) {
        (Some(left), Some(right)) if left != right => return AccountProbe::Unavailable,
        (Some(fact), _) | (_, Some(fact)) => fact,
        (None, None) => return AccountProbe::Unavailable,
    };
    if !auth_fact {
        return AccountProbe::LoggedOut;
    }

    let Some(about_output) = run(&binary, &["about", "--format", "json"]) else {
        return AccountProbe::Unavailable;
    };
    if !about_output.success {
        return AccountProbe::Unavailable;
    }
    let Ok(about) = serde_json::from_slice::<AboutShape>(&about_output.stdout) else {
        return AccountProbe::Unavailable;
    };
    let status_email = status.user_info.and_then(|info| non_blank(info.email));
    let about_email = non_blank(about.user_email);
    if let (Some(status_email), Some(about_email)) = (&status_email, &about_email)
        && !status_email.eq_ignore_ascii_case(about_email)
    {
        return AccountProbe::Unavailable;
    }
    let plan = non_blank(about.subscription_tier);
    AccountProbe::Found(AgentAccount {
        account_id: status_email.or(about_email),
        metered: plan.as_ref().map(|_| true),
        plan,
        version: non_blank(about.cli_version),
        sub_provider: None,
        scope: Default::default(),
        credentials_updated_at_ms: None,
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

    fn cursor() -> &'static AgentDescriptor {
        crate::agents::descriptor_by_kind("cursor").unwrap()
    }

    fn output(stdout: &str) -> ProbeOutput {
        ProbeOutput {
            success: true,
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
                AccountProbe::Unavailable
            ));
        }
    }

    #[test]
    fn malformed_spawn_and_nonzero_results_retry() {
        assert!(matches!(
            probe_outputs(vec![output("not json")]).0,
            AccountProbe::Unavailable
        ));
        let (spawn_failure, _) = probe_outputs(Vec::new());
        assert!(matches!(spawn_failure, AccountProbe::Unavailable));
        let (nonzero, _) = probe_outputs(vec![ProbeOutput {
            success: false,
            stdout: b"{}".to_vec(),
        }]);
        assert!(matches!(nonzero, AccountProbe::Unavailable));
        let (about_failure, _) = probe_outputs(vec![
            output(r#"{"isAuthenticated":true}"#),
            ProbeOutput {
                success: false,
                stdout: b"{}".to_vec(),
            },
        ]);
        assert!(matches!(about_failure, AccountProbe::Unavailable));
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
