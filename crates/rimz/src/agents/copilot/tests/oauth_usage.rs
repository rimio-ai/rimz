use std::collections::BTreeMap;
use std::ffi::OsString;

use crate::agents::credits::AccountUsageReportable;

use super::*;

fn fixture() -> &'static str {
    include_str!("fixtures/oauth_usage_modern.json")
}

fn env(values: &[(&str, &str)]) -> impl FnMut(&str) -> Option<OsString> {
    let values = values
        .iter()
        .map(|(key, value)| ((*key).to_owned(), OsString::from(value)))
        .collect::<BTreeMap<_, _>>();
    move |key| values.get(key).cloned()
}

#[test]
fn modern_payload_normalizes_named_premium_and_chat_quotas() {
    let snapshot = parse_response(fixture()).unwrap();
    assert_eq!(snapshot.plan.as_deref(), Some("individual"));
    assert_eq!(snapshot.extra_credits, None);
    assert_eq!(snapshot.reset_credits, None);
    let windows = snapshot.rate_limits.unwrap().windows;
    assert_eq!(windows.len(), 2);
    assert_eq!(
        windows[0]
            .scope
            .as_ref()
            .map(|scope| (scope.id.as_str(), scope.label.as_str())),
        Some(("premium_interactions", "prm"))
    );
    assert_eq!(windows[0].used_percentage, Some(40));
    assert_eq!(
        windows[1]
            .scope
            .as_ref()
            .map(|scope| (scope.id.as_str(), scope.label.as_str())),
        Some(("chat", "cht"))
    );
    assert_eq!(windows[1].used_percentage, Some(25));
    assert_eq!(windows[0].duration_mins, None);
    assert_eq!(windows[0].resets_at, windows[1].resets_at);
    assert_eq!(
        windows[0].resets_at,
        Some("2026-08-01T00:00:00.123Z".parse().unwrap())
    );
    assert!(
        windows
            .iter()
            .all(|window| window.source.is_authoritative())
    );
}

#[test]
fn modern_and_legacy_categories_merge_independently() {
    let snapshot = parse_response(
        r#"{
            "copilot_plan":"pro",
            "quota_snapshots": {
                "premium_interactions": {"percent_remaining":"90"}
            },
            "monthly_quotas":{"chat":"100"},
            "limited_user_quotas":{"chat":"25"}
        }"#,
    )
    .unwrap();
    let windows = snapshot.rate_limits.unwrap().windows;
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0].used_percentage, Some(10));
    assert_eq!(windows[1].used_percentage, Some(75));
}

#[test]
fn either_named_quota_can_stand_alone() {
    let premium = parse_response(
        r#"{"copilot_plan":"free","quota_snapshots":{"premium_interactions":{"percent_remaining":50}}}"#,
    )
    .unwrap();
    assert_eq!(premium.rate_limits.unwrap().windows.len(), 1);

    let chat = parse_response(
        r#"{"copilot_plan":"free","quota_snapshots":{"chat":{"percent_remaining":25}}}"#,
    )
    .unwrap();
    let window = chat.rate_limits.unwrap().windows.pop().unwrap();
    assert_eq!(window.scope.unwrap().id, "chat");
    assert_eq!(window.used_percentage, Some(75));
}

#[test]
fn percentages_derive_clamp_and_reject_underdetermined_values() {
    let snapshot = parse_response(
        r#"{
            "copilot_plan":"pro",
            "quota_snapshots": {
                "premium_interactions":{"entitlement":"100","remaining":"120"},
                "chat":{"percent_remaining":"-25"}
            }
        }"#,
    )
    .unwrap();
    let windows = snapshot.rate_limits.unwrap().windows;
    assert_eq!(windows[0].used_percentage, Some(0));
    assert_eq!(windows[1].used_percentage, Some(100));

    let snapshot = parse_response(
        r#"{
            "copilot_plan":"free",
            "quota_snapshots":{"chat":{"remaining":10}},
            "monthly_quotas":{"chat":100}
        }"#,
    )
    .unwrap();
    assert_eq!(snapshot.rate_limits, None);
}

#[test]
fn unlimited_and_business_placeholders_stay_truthful() {
    let unlimited = parse_response(
        r#"{
            "copilot_plan":"pro",
            "quota_reset_date":"2026-08-01",
            "quota_snapshots":{"premium_interactions":{"unlimited":true}}
        }"#,
    )
    .unwrap()
    .rate_limits
    .unwrap()
    .windows
    .pop()
    .unwrap();
    assert!(unlimited.lifted);
    assert_eq!(unlimited.used_percentage, Some(0));
    assert_eq!(unlimited.resets_at, None);

    let business = parse_response(
        r#"{
            "copilot_plan":"business",
            "token_based_billing":true,
            "quota_snapshots": {
                "premium_interactions":{"entitlement":0,"remaining":0,"percent_remaining":100},
                "chat":{"entitlement":"0","remaining":"0","percent_remaining":"100"}
            }
        }"#,
    )
    .unwrap();
    assert_eq!(business.plan.as_deref(), Some("business"));
    assert_eq!(business.rate_limits, None);
}

#[test]
fn reset_parser_accepts_rfc3339_fractional_and_date_only() {
    for (value, expected) in [
        ("2026-08-01T01:02:03Z", "2026-08-01T01:02:03Z"),
        ("2026-08-01T01:02:03.456Z", "2026-08-01T01:02:03.456Z"),
        ("2026-08-01", "2026-08-01T00:00:00Z"),
    ] {
        assert_eq!(
            parse_optional_reset(Some(value)).unwrap(),
            Some(expected.parse().unwrap())
        );
    }
    assert!(matches!(
        parse_optional_reset(Some("next month")),
        Err(Error::InvalidReset)
    ));
}

#[test]
fn empty_and_malformed_payloads_are_reportable() {
    assert!(matches!(
        parse_response("{}"),
        Err(Error::MalformedResponse)
    ));
    assert!(matches!(parse_response("not-json"), Err(Error::Parse(_))));
    assert!(Error::MalformedResponse.should_report());
    assert!(Error::InvalidReset.should_report());
    assert!(
        Error::Http {
            kind: HttpErrKind::Transport,
            host: "api.github.com".to_owned(),
        }
        .should_report()
    );
    assert!(!Error::NoCredentials.should_report());
    assert!(!Error::Unavailable.should_report());
}

#[test]
fn expected_auth_and_unsupported_statuses_settle_quietly() {
    for status in [401, 403, 404] {
        let error = map_http_error((HttpErrKind::Status(status), "api.github.com".to_owned()));
        assert!(matches!(error, Error::Unavailable));
        assert!(!error.should_report());
    }
    let error = map_http_error((HttpErrKind::Status(500), "api.github.com".to_owned()));
    assert!(matches!(error, Error::Http { .. }));
    assert!(error.should_report());
}

#[test]
fn credential_and_host_precedence_skips_empty_values() {
    let credentials = credentials_from(env(&[
        ("COPILOT_GITHUB_TOKEN", " "),
        ("GH_TOKEN", "gh-token"),
        ("GITHUB_TOKEN", "fallback"),
        ("COPILOT_GH_HOST", ""),
        ("GH_HOST", " HTTPS://GitHub.Example:8443/path "),
    ]))
    .unwrap();
    assert_eq!(credentials.token, "gh-token");
    assert_eq!(credentials.host.as_str(), "github.example:8443");
    assert_eq!(
        credentials.host.usage_url(),
        "https://api.github.example:8443/copilot_internal/user"
    );

    let public = credentials_from(env(&[("GITHUB_TOKEN", "token")])).unwrap();
    assert_eq!(
        public.host.usage_url(),
        "https://api.github.com/copilot_internal/user"
    );
}

#[test]
fn host_normalization_accepts_paths_ports_and_rejects_malformed_values() {
    for (raw, normalized, endpoint) in [
        (
            "github.com",
            "github.com",
            "https://api.github.com/copilot_internal/user",
        ),
        (
            "http://api.github.example/root",
            "api.github.example",
            "https://api.github.example/copilot_internal/user",
        ),
        (
            "github.example:444/path",
            "github.example:444",
            "https://api.github.example:444/copilot_internal/user",
        ),
    ] {
        let host = GitHubHost::parse(raw).unwrap();
        assert_eq!(host.as_str(), normalized);
        assert_eq!(host.usage_url(), endpoint);
    }
    for raw in [
        "https://",
        "file://github.com",
        "user@github.com",
        "github..com",
        "github.com:nope",
    ] {
        assert!(matches!(GitHubHost::parse(raw), Err(Error::InvalidHost)));
    }
}

#[test]
fn account_fingerprint_changes_with_host_or_token_without_exposing_secrets() {
    let first = credentials_from(env(&[("GH_TOKEN", "secret-one")])).unwrap();
    let second = credentials_from(env(&[("GH_TOKEN", "secret-two")])).unwrap();
    let enterprise = credentials_from(env(&[
        ("GH_TOKEN", "secret-one"),
        ("GH_HOST", "github.example"),
    ]))
    .unwrap();
    assert_ne!(first.fingerprint(), second.fingerprint());
    assert_ne!(first.fingerprint(), enterprise.fingerprint());
    assert!(!first.fingerprint().contains("secret-one"));
    assert_eq!(first.fingerprint().len(), "copilot:".len() + 64);

    let error = match credentials_from(env(&[
        ("GH_TOKEN", "top-secret-token"),
        ("GH_HOST", "https://user@github.example"),
    ])) {
        Err(error) => error,
        Ok(_) => panic!("malformed host should fail"),
    };
    assert!(!error.to_string().contains("top-secret-token"));
    assert!(!error.to_string().contains("user@github.example"));
}
