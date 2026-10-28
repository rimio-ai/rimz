//! Read-only Zen Go usage with best-effort Console OAuth tier enrichment.

use jiff::Timestamp;
use serde_json::{Map, Value};

use crate::agents::AccountUsageSnapshot;
use crate::agents::context::{AgentRateLimits, RateLimitWindow, WindowSource};
use crate::agents::credits::{
    AccountUsageFault, AccountUsageReportable, HttpErrKind, oauth_http_get, oauth_http_get_once,
    trusted_usage_url,
};

const DEFAULT_USAGE_URL: &str = "https://opencode.ai/zen/go/v1/usage";
const URL_ENV: &str = "RIMZ_OPENCODE_GO_USAGE_URL";

#[derive(Debug, thiserror::Error)]
pub(in crate::agents) enum GoUsageErr {
    #[error("OpenCode Go usage URL override refused")]
    UntrustedUsageUrl,
    #[error("OpenCode Go usage HTTP {kind}")]
    Http { kind: HttpErrKind },
    #[error("OpenCode Go usage schema drift at {location}: {problem}")]
    Schema {
        location: String,
        problem: &'static str,
    },
}

impl AccountUsageReportable for GoUsageErr {
    fn fault(&self) -> AccountUsageFault {
        match self {
            Self::UntrustedUsageUrl => AccountUsageFault::NoCredentials,
            Self::Http { kind } if kind.is_auth_rejected() => AccountUsageFault::NoCredentials,
            _ => AccountUsageFault::Transient,
        }
    }
}

type Result<T> = std::result::Result<T, GoUsageErr>;

pub(in crate::agents) fn fetch_usage_with_token(
    access_token: &str,
    org_id: Option<&str>,
) -> Result<AccountUsageSnapshot> {
    let url = resolve_usage_url(std::env::var(URL_ENV).ok().as_deref())?;
    fetch_usage_with_url(&url, access_token.trim(), org_id)
}

fn fetch_usage_with_url(
    url: &str,
    access_token: &str,
    org_id: Option<&str>,
) -> Result<AccountUsageSnapshot> {
    let mut headers = vec![("Authorization", format!("Bearer {access_token}"))];
    if let Some(org_id) = org_id.filter(|id| !id.is_empty()) {
        headers.push(("x-opencode-org-id", org_id.to_owned()));
    }
    // Query freshness precedes I/O; expiry is checked after it.
    let observed_at = Timestamp::now();
    let body = oauth_http_get(url, &headers, "OpenCode Go usage fetch")
        .map_err(|error| GoUsageErr::Http { kind: error.kind })?;
    let mut snapshot = parse_response(&body, observed_at)?;
    snapshot.plan = fetch_plan(url, access_token, org_id);
    // The tier read is the last I/O before publication, so it can outlast
    // the running window's remaining time.
    clear_inactive_five_hour(&mut snapshot, observed_at, Timestamp::now());
    Ok(snapshot)
}

fn fetch_plan(usage_url: &str, access_token: &str, org_id: Option<&str>) -> Option<String> {
    let origin = url::Url::parse(usage_url)
        .ok()?
        .origin()
        .ascii_serialization();
    let url = format!("{origin}/console/api/go/status");
    let mut headers = vec![("Authorization", format!("Bearer {access_token}"))];
    if let Some(org_id) = org_id.filter(|id| !id.is_empty()) {
        headers.push(("x-org-id", org_id.to_owned()));
    }
    let body = oauth_http_get_once(&url, &headers, "OpenCode Go tier fetch").ok()?;
    let value: Value = serde_json::from_str(&body).ok()?;
    value
        .get("product")?
        .as_str()
        .map(str::trim)
        .filter(|product| !product.is_empty())
        .map(ToOwned::to_owned)
}

fn resolve_usage_url(override_url: Option<&str>) -> Result<String> {
    let candidate = override_url
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .unwrap_or(DEFAULT_USAGE_URL);
    if !trusted_usage_url(candidate, "opencode.ai") {
        return Err(GoUsageErr::UntrustedUsageUrl);
    }
    Ok(candidate.to_owned())
}

fn schema(location: &str, problem: &'static str) -> GoUsageErr {
    GoUsageErr::Schema {
        location: location.to_owned(),
        problem,
    }
}

fn required<'a>(value: Option<&'a Value>, location: &str) -> Result<&'a Value> {
    match value {
        None => Err(schema(location, "missing")),
        Some(Value::Null) => Err(schema(location, "null")),
        Some(value) => Ok(value),
    }
}

fn object<'a>(value: Option<&'a Value>, location: &str) -> Result<&'a Map<String, Value>> {
    required(value, location)?
        .as_object()
        .ok_or_else(|| schema(location, "wrong type"))
}

fn parse_response(body: &str, observed_at: Timestamp) -> Result<AccountUsageSnapshot> {
    let value: Value = serde_json::from_str(body).map_err(|_| schema("body", "not JSON"))?;
    let body = object(Some(&value), "body")?;
    let usage = object(body.get("usage"), "usage")?;
    let windows = [("rolling", 300), ("weekly", 10_080), ("monthly", 43_200)]
        .into_iter()
        .map(|(name, duration)| window(usage, name, duration, observed_at))
        .collect::<Result<Vec<_>>>()?;
    Ok(AccountUsageSnapshot {
        rate_limits: Some(AgentRateLimits { windows }),
        ..Default::default()
    })
}

fn window(
    usage: &Map<String, Value>,
    name: &str,
    duration_mins: u32,
    observed_at: Timestamp,
) -> Result<RateLimitWindow> {
    let location = format!("usage.{name}");
    let meter = object(usage.get(name), &location)?;
    let status_location = format!("{location}.status");
    let status = required(meter.get("status"), &status_location)?
        .as_str()
        .ok_or_else(|| schema(&status_location, "wrong type"))?;
    let limited = match status {
        "ok" => false,
        "rate-limited" => true,
        _ => return Err(schema(&status_location, "invalid value")),
    };
    let percent_location = format!("{location}.percent");
    let percent = required(meter.get("percent"), &percent_location)?
        .as_f64()
        .ok_or_else(|| schema(&percent_location, "wrong type"))?;
    let reset_location = format!("{location}.resetsAt");
    let reset = required(meter.get("resetsAt"), &reset_location)?
        .as_str()
        .ok_or_else(|| schema(&reset_location, "wrong type"))?
        .parse()
        .map_err(|_| schema(&reset_location, "invalid value"))?;
    Ok(RateLimitWindow {
        used_percentage: Some(if limited {
            100
        } else {
            percent.clamp(0.0, 100.0).floor().min(99.0) as u8
        }),
        resets_at: Some(reset),
        duration_mins: Some(duration_mins),
        observed_at: Some(observed_at),
        source: WindowSource::Authoritative,
        // Week and month follow Go's calendar and billing schedule; only the
        // five-hour clock starts at first use.
        fixed_period: duration_mins != 300,
        ..Default::default()
    })
}

fn clear_inactive_five_hour(
    snapshot: &mut AccountUsageSnapshot,
    observed_at: Timestamp,
    now: Timestamp,
) {
    let windows = snapshot
        .rate_limits
        .iter_mut()
        .flat_map(|limits| &mut limits.windows);
    for window in windows.filter(|window| window.duration_mins == Some(300)) {
        if window.resets_at.is_some_and(|reset| reset <= now) || window.not_started(observed_at) {
            window.used_percentage = Some(0);
            window.resets_at = None;
        }
    }
}

#[cfg(test)]
mod tests;
