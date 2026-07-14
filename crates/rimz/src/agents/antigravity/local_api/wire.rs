//! Connect JSON schemas and conservative Antigravity quota normalization.

use std::cmp::Ordering;

use jiff::Timestamp;
use serde::Deserialize;

use super::LocalApiError;
use crate::agents::context::{AgentAccount, AgentRateLimits, RateLimitWindow, WindowSource};

const FIVE_HOURS_MINS: u32 = 300;
const WEEK_MINS: u32 = 7 * 24 * 60;

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum CodeValue {
    Integer(i64),
    String(String),
}

impl CodeValue {
    fn is_success(&self) -> bool {
        match self {
            Self::Integer(code) => *code == 0,
            Self::String(code) => {
                let code = code.trim();
                code == "0"
                    || code.eq_ignore_ascii_case("ok")
                    || code.eq_ignore_ascii_case("success")
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserStatusResponse {
    code: Option<CodeValue>,
    user_status: Option<UserStatus>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserStatus {
    email: Option<String>,
    user_tier: Option<UserTier>,
    plan_status: Option<PlanStatus>,
}

#[derive(Debug, Deserialize)]
struct UserTier {
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlanStatus {
    plan_info: Option<PlanInfo>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlanInfo {
    plan_name: Option<String>,
    plan_display_name: Option<String>,
    display_name: Option<String>,
    product_name: Option<String>,
    plan_short_name: Option<String>,
}

pub(in crate::agents::antigravity) fn parse_identity(
    body: &str,
) -> Result<AgentAccount, LocalApiError> {
    let response: UserStatusResponse =
        serde_json::from_str(body).map_err(|_| LocalApiError::InvalidResponse)?;
    validate_code(response.code.as_ref())?;
    let status = response.user_status.ok_or(LocalApiError::InvalidResponse)?;
    let account_id = trimmed(status.email);
    let plan = status
        .user_tier
        .and_then(|tier| trimmed(tier.name))
        .or_else(|| {
            let info = status.plan_status?.plan_info?;
            [
                info.plan_display_name,
                info.display_name,
                info.product_name,
                info.plan_name,
                info.plan_short_name,
            ]
            .into_iter()
            .find_map(trimmed)
        });
    if account_id.is_none() && plan.is_none() {
        return Err(LocalApiError::InvalidResponse);
    }
    Ok(AgentAccount {
        plan,
        account_id,
        metered: Some(true),
        ..AgentAccount::default()
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuotaResponse {
    code: Option<CodeValue>,
    response: Option<QuotaPayload>,
    summary: Option<QuotaPayload>,
    groups: Option<Vec<QuotaGroup>>,
}

#[derive(Debug, Deserialize)]
struct QuotaPayload {
    groups: Option<Vec<QuotaGroup>>,
}

#[derive(Debug, Deserialize)]
struct QuotaGroup {
    buckets: Option<Vec<QuotaBucket>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuotaBucket {
    bucket_id: Option<String>,
    display_name: Option<String>,
    #[serde(default)]
    disabled: bool,
    remaining_fraction: Option<f64>,
    remaining: Option<Remaining>,
    reset_time: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Remaining {
    remaining_fraction: Option<f64>,
    #[serde(rename = "case")]
    oneof_case: Option<String>,
    value: Option<f64>,
}

impl Remaining {
    fn fraction(&self) -> Option<f64> {
        self.remaining_fraction.or_else(|| {
            self.oneof_case
                .as_deref()
                .is_some_and(|case| case == "remainingFraction")
                .then_some(self.value)
                .flatten()
        })
    }
}

#[derive(Clone, Debug)]
struct UsableBucket {
    identity: String,
    remaining: f64,
    reset: Timestamp,
}

pub(in crate::agents::antigravity) fn parse_rate_limits(
    body: &str,
    observed_at: Timestamp,
) -> Result<AgentRateLimits, LocalApiError> {
    let response: QuotaResponse =
        serde_json::from_str(body).map_err(|_| LocalApiError::InvalidResponse)?;
    validate_code(response.code.as_ref())?;
    let groups = response
        .response
        .and_then(|payload| payload.groups)
        .or_else(|| response.summary.and_then(|payload| payload.groups))
        .or(response.groups)
        .ok_or(LocalApiError::InvalidResponse)?;

    let mut periods = [PeriodBuckets::default(), PeriodBuckets::default()];
    for bucket in groups
        .into_iter()
        .flat_map(|group| group.buckets.unwrap_or_default())
    {
        let bucket_id = trimmed(bucket.bucket_id);
        let display_name = trimmed(bucket.display_name);
        let Some(period) = period_from_labels(
            bucket_id.as_deref().unwrap_or_default(),
            display_name.as_deref(),
        ) else {
            continue;
        };
        let identity = bucket_id
            .or(display_name)
            .ok_or(LocalApiError::InvalidResponse)?;
        let period_buckets = &mut periods[period.index()];
        period_buckets.recognized = true;
        if bucket.disabled {
            continue;
        }
        let remaining = bucket
            .remaining_fraction
            .or_else(|| bucket.remaining.as_ref().and_then(Remaining::fraction));
        let Some(remaining) = remaining else {
            continue;
        };
        if !remaining.is_finite() || !(0.0..=1.0).contains(&remaining) {
            return Err(LocalApiError::InvalidResponse);
        }
        let reset = bucket
            .reset_time
            .as_deref()
            .and_then(|value| value.parse::<Timestamp>().ok())
            .filter(|reset| *reset > observed_at)
            .ok_or(LocalApiError::InvalidResponse)?;
        period_buckets.usable.push(UsableBucket {
            identity,
            remaining,
            reset,
        });
    }

    let mut windows = Vec::new();
    for period in [Period::FiveHours, Period::Week] {
        let buckets = &mut periods[period.index()];
        if !buckets.recognized {
            continue;
        }
        buckets.usable.sort_by(compare_buckets);
        let (used_percentage, resets_at) = buckets.usable.first().map_or((None, None), |bucket| {
            (Some(used_percentage(bucket.remaining)), Some(bucket.reset))
        });
        windows.push(RateLimitWindow {
            scope: None,
            used_percentage,
            resets_at,
            duration_mins: Some(period.duration_mins()),
            observed_at: Some(observed_at),
            source: WindowSource::Authoritative,
            lifted: false,
        });
    }
    if windows.is_empty() {
        return Err(LocalApiError::InvalidResponse);
    }
    Ok(AgentRateLimits { windows })
}

#[derive(Clone, Copy)]
enum Period {
    FiveHours,
    Week,
}

impl Period {
    const fn index(self) -> usize {
        match self {
            Self::FiveHours => 0,
            Self::Week => 1,
        }
    }

    const fn duration_mins(self) -> u32 {
        match self {
            Self::FiveHours => FIVE_HOURS_MINS,
            Self::Week => WEEK_MINS,
        }
    }
}

#[derive(Default)]
struct PeriodBuckets {
    recognized: bool,
    usable: Vec<UsableBucket>,
}

fn compare_buckets(left: &UsableBucket, right: &UsableBucket) -> Ordering {
    left.remaining
        .total_cmp(&right.remaining)
        .then_with(|| right.reset.cmp(&left.reset))
        .then_with(|| left.identity.cmp(&right.identity))
}

fn period_from_labels(bucket_id: &str, display_name: Option<&str>) -> Option<Period> {
    [Some(bucket_id), display_name]
        .into_iter()
        .flatten()
        .find_map(|label| {
            let label = label.to_ascii_lowercase().replace(['-', '_'], " ");
            if contains_token(&label, "5h")
                || label
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .windows(2)
                    .any(|words| matches!(words, ["5", "hour"] | ["five", "hour"]))
            {
                Some(Period::FiveHours)
            } else if contains_token(&label, "weekly") {
                Some(Period::Week)
            } else {
                None
            }
        })
}

fn contains_token(value: &str, token: &str) -> bool {
    value.match_indices(token).any(|(start, matched)| {
        let end = start + matched.len();
        value[..start]
            .chars()
            .next_back()
            .is_none_or(|char| !char.is_ascii_alphanumeric())
            && value[end..]
                .chars()
                .next()
                .is_none_or(|char| !char.is_ascii_alphanumeric())
    })
}

fn used_percentage(remaining: f64) -> u8 {
    if remaining == 0.0 {
        return 100;
    }
    ((1.0 - remaining) * 100.0).round().clamp(0.0, 99.0) as u8
}

fn validate_code(code: Option<&CodeValue>) -> Result<(), LocalApiError> {
    if code.is_none_or(CodeValue::is_success) {
        Ok(())
    } else {
        Err(LocalApiError::InvalidResponse)
    }
}

fn trimmed(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}
