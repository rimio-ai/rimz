//! Experimental Alibaba Coding Plan API-key quota probe.

use jiff::Timestamp;
use serde::Serialize;
use serde_json::{Map, Value};

use super::selection::{AlibabaRegion, SelectedProvider, Selection};
use crate::agents::context::{AgentRateLimits, RateLimitWindow, WindowSource};
use crate::agents::credits::oauth_http_post_json;
use crate::agents::{AccountUsageProbe, AccountUsageSnapshot, HttpErrKind};

const ACTION: &str = "zeldaEasy.broadscope-bailian.codingPlan.queryCodingPlanInstanceInfoV2";
const PRODUCT: &str = "broadscope-bailian";
const API: &str = "queryCodingPlanInstanceInfoV2";
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36";

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("Alibaba Coding Plan API key rejected")]
    AuthRejected,
    #[error("Alibaba Coding Plan API-key quota is unavailable for this region")]
    Unsupported,
    #[error("Alibaba Coding Plan quota request throttled")]
    Throttled,
    #[error("Alibaba Coding Plan quota HTTP {kind} (host {host})")]
    Http { kind: HttpErrKind, host: String },
    #[error("Alibaba Coding Plan quota schema drift ({shape})")]
    Schema { shape: String },
}

pub(crate) fn probe(selection: Selection) -> AccountUsageProbe {
    let identity = selection.account_usage_identity();
    let SelectedProvider::Alibaba(region) = selection.provider else {
        return AccountUsageProbe::Unsupported;
    };
    match fetch(region, &selection.credential) {
        Ok(snapshot) => AccountUsageProbe::Found { identity, snapshot },
        Err(Error::AuthRejected) => {
            tracing::debug!(provider = "qwen", "Alibaba Coding Plan API key rejected");
            AccountUsageProbe::NoCredentials(identity)
        }
        Err(Error::Unsupported | Error::Throttled) => AccountUsageProbe::Unsupported,
        Err(error) => {
            tracing::warn!(
                tags.operation = "oauth_usage",
                tags.provider = "qwen",
                error = &error as &dyn std::error::Error,
                "OAuth account usage fetch failed",
            );
            AccountUsageProbe::Failed(identity)
        }
    }
}

fn fetch(region: AlibabaRegion, api_key: &str) -> Result<AccountUsageSnapshot, Error> {
    let request = RequestMetadata::for_region(region);
    let headers = request.headers(api_key);
    let body = RequestBody {
        query: QueryRequest {
            commodity_code: request.commodity_code,
        },
    };
    let response = oauth_http_post_json(
        &request.url(),
        &headers,
        &body,
        "Qwen Alibaba Coding Plan quota fetch",
    )
    .map_err(|(kind, host)| match kind {
        kind if kind.is_auth_rejected() => Error::AuthRejected,
        HttpErrKind::Status(404) => Error::Unsupported,
        HttpErrKind::Status(429) => Error::Throttled,
        kind => Error::Http { kind, host },
    })?;
    parse_response(&response, region)
}

struct RequestMetadata {
    host: &'static str,
    region_id: &'static str,
    commodity_code: &'static str,
    referer: &'static str,
}

impl RequestMetadata {
    fn for_region(region: AlibabaRegion) -> Self {
        match region {
            AlibabaRegion::International => Self {
                host: "modelstudio.console.alibabacloud.com",
                region_id: "ap-southeast-1",
                commodity_code: "sfm_codingplan_public_intl",
                referer: "https://modelstudio.console.alibabacloud.com/ap-southeast-1/?tab=globalset#/efm/coding_plan",
            },
            AlibabaRegion::China => Self {
                host: "bailian.console.aliyun.com",
                region_id: "cn-beijing",
                commodity_code: "sfm_codingplan_public_cn",
                referer: "https://bailian.console.aliyun.com/cn-beijing/?tab=model#/efm/coding_plan",
            },
        }
    }

    fn url(&self) -> String {
        format!(
            "https://{}/data/api.json?action={ACTION}&product={PRODUCT}&api={API}&currentRegionId={}",
            self.host, self.region_id
        )
    }

    fn headers(&self, api_key: &str) -> Vec<(&'static str, String)> {
        vec![
            ("Accept", "application/json".to_owned()),
            ("Authorization", format!("Bearer {api_key}")),
            ("x-api-key", api_key.to_owned()),
            ("X-DashScope-API-Key", api_key.to_owned()),
            ("Origin", format!("https://{}", self.host)),
            ("Referer", self.referer.to_owned()),
            ("User-Agent", USER_AGENT.to_owned()),
        ]
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestBody<'a> {
    #[serde(rename = "queryCodingPlanInstanceInfoRequest")]
    query: QueryRequest<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct QueryRequest<'a> {
    commodity_code: &'a str,
}

pub(crate) fn parse_response(
    body: &str,
    _region: AlibabaRegion,
) -> Result<AccountUsageSnapshot, Error> {
    let root: Value = serde_json::from_str(body).map_err(|_| Error::Schema {
        shape: "invalid-json".to_owned(),
    })?;
    let root_object = root.as_object().ok_or_else(|| Error::Schema {
        shape: "non-object".to_owned(),
    })?;
    classify_response_error(root_object)?;
    let payload = known_payload(root_object)?.ok_or_else(|| Error::Schema {
        shape: safe_shape(root_object),
    })?;
    classify_response_error(&payload)?;

    let instances = value_for(
        &payload,
        &["codingPlanInstanceInfos", "coding_plan_instance_infos"],
    )
    .and_then(Value::as_array);
    let selected = match instances {
        Some(instances) => select_instance(instances)?,
        None => None,
    };
    let plan = selected.and_then(plan_name).or_else(|| plan_name(&payload));
    let active = selected.is_some_and(explicitly_active)
        || (instances.is_none() && explicitly_active(&payload));
    let quota = selected.and_then(quota_info).or_else(|| {
        instances
            .is_none_or(|items| items.len() <= 1)
            .then(|| quota_info(&payload))
            .flatten()
    });

    let now = Timestamp::now();
    let mut windows = Vec::new();
    if let Some(quota) = quota {
        push_window(
            &mut windows,
            quota,
            &["per5HourUsedQuota", "perFiveHourUsedQuota"],
            &["per5HourTotalQuota", "perFiveHourTotalQuota"],
            &[
                "per5HourQuotaNextRefreshTime",
                "perFiveHourQuotaNextRefreshTime",
            ],
            300,
            now,
        );
        push_window(
            &mut windows,
            quota,
            &["perWeekUsedQuota"],
            &["perWeekTotalQuota"],
            &["perWeekQuotaNextRefreshTime"],
            10_080,
            now,
        );
        push_window(
            &mut windows,
            quota,
            &["perBillMonthUsedQuota", "perMonthUsedQuota"],
            &["perBillMonthTotalQuota", "perMonthTotalQuota"],
            &[
                "perBillMonthQuotaNextRefreshTime",
                "perMonthQuotaNextRefreshTime",
            ],
            43_200,
            now,
        );
    }
    if windows.is_empty() && !(active && plan.is_some()) {
        return Err(Error::Schema {
            shape: safe_shape(&payload),
        });
    }
    Ok(AccountUsageSnapshot {
        rate_limits: (!windows.is_empty()).then_some(AgentRateLimits { windows }),
        plan,
        ..Default::default()
    })
}

fn known_payload(root: &Map<String, Value>) -> Result<Option<Map<String, Value>>, Error> {
    if has_usage_shape(root) {
        return Ok(Some(root.clone()));
    }
    let Some(data) = root.get("data").and_then(parse_object_value) else {
        return Ok(None);
    };
    classify_response_error(&data)?;
    if has_usage_shape(&data) {
        return Ok(Some(data));
    }
    let Some(success) = data
        .get("successResponse")
        .or_else(|| data.get("success_response"))
        .and_then(parse_object_value)
    else {
        return Ok(None);
    };
    classify_response_error(&success)?;
    if has_usage_shape(&success) {
        return Ok(Some(success));
    }
    let Some(body) = success.get("body").and_then(parse_object_value) else {
        return Ok(None);
    };
    classify_response_error(&body)?;
    Ok(has_usage_shape(&body).then_some(body))
}

fn parse_object_value(value: &Value) -> Option<Map<String, Value>> {
    if let Some(value) = value.as_object() {
        return Some(value.clone());
    }
    let parsed: Value = serde_json::from_str(value.as_str()?).ok()?;
    Some(parsed.as_object()?.clone())
}

fn has_usage_shape(object: &Map<String, Value>) -> bool {
    [
        "codingPlanInstanceInfos",
        "coding_plan_instance_infos",
        "codingPlanQuotaInfo",
        "coding_plan_quota_info",
    ]
    .iter()
    .any(|key| object.contains_key(*key))
}

fn select_instance(instances: &[Value]) -> Result<Option<&Map<String, Value>>, Error> {
    let objects = instances
        .iter()
        .filter_map(Value::as_object)
        .collect::<Vec<_>>();
    let active = objects
        .iter()
        .copied()
        .filter(|object| explicitly_active(object))
        .collect::<Vec<_>>();
    match (objects.as_slice(), active.as_slice()) {
        (_, [selected, ..]) => Ok(Some(selected)),
        ([only], []) if !explicitly_inactive(only) => Ok(Some(only)),
        ([], []) => Ok(None),
        _ => Err(Error::Schema {
            shape: format!("instances={};active={}", objects.len(), active.len()),
        }),
    }
}

fn explicitly_active(object: &Map<String, Value>) -> bool {
    value_for(object, &["status", "instanceStatus"])
        .and_then(Value::as_str)
        .is_some_and(|status| matches!(status.to_ascii_uppercase().as_str(), "ACTIVE" | "VALID"))
        || value_for(object, &["isActive", "active"]).and_then(Value::as_bool) == Some(true)
}

fn explicitly_inactive(object: &Map<String, Value>) -> bool {
    value_for(object, &["status", "instanceStatus"])
        .and_then(Value::as_str)
        .is_some_and(|status| {
            matches!(
                status.to_ascii_uppercase().as_str(),
                "EXPIRED" | "INVALID" | "INACTIVE" | "DISABLED" | "TERMINATED" | "STOPPED"
            )
        })
        || value_for(object, &["isActive", "active"]).and_then(Value::as_bool) == Some(false)
}

fn plan_name(object: &Map<String, Value>) -> Option<String> {
    value_for(object, &["planName", "instanceName", "packageName"])
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn quota_info(object: &Map<String, Value>) -> Option<&Map<String, Value>> {
    value_for(object, &["codingPlanQuotaInfo", "coding_plan_quota_info"])
        .and_then(Value::as_object)
        .or_else(|| object.contains_key("per5HourTotalQuota").then_some(object))
}

fn push_window(
    windows: &mut Vec<RateLimitWindow>,
    quota: &Map<String, Value>,
    used_keys: &[&str],
    total_keys: &[&str],
    reset_keys: &[&str],
    duration_mins: u32,
    now: Timestamp,
) {
    let Some(total) = value_for(quota, total_keys).and_then(number) else {
        return;
    };
    if !total.is_finite() || total <= 0.0 {
        return;
    }
    let used_percentage = value_for(quota, used_keys)
        .and_then(number)
        .and_then(|used| {
            used.is_finite()
                .then_some(((used.max(0.0).min(total) / total) * 100.0).round() as u8)
        });
    windows.push(RateLimitWindow {
        used_percentage,
        resets_at: value_for(quota, reset_keys).and_then(timestamp),
        duration_mins: Some(duration_mins),
        observed_at: Some(now),
        source: WindowSource::Authoritative,
        ..Default::default()
    });
}

fn value_for<'a>(object: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|key| object.get(*key))
}

fn number(value: &Value) -> Option<f64> {
    let number = value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse().ok())?;
    number.is_finite().then_some(number)
}

fn timestamp(value: &Value) -> Option<Timestamp> {
    if let Some(number) = number(value) {
        let seconds = if number >= 1_000_000_000_000.0 {
            (number / 1000.0) as i64
        } else {
            number as i64
        };
        return Timestamp::from_second(seconds).ok();
    }
    value.as_str()?.parse().ok()
}

fn classify_response_error(object: &Map<String, Value>) -> Result<(), Error> {
    let status = value_for(object, &["statusCode", "status_code", "code"]);
    if let Some(code) = status.and_then(number)
        && code != 0.0
        && code != 200.0
    {
        return Err(match code as u16 {
            401 | 403 => Error::AuthRejected,
            404 => Error::Unsupported,
            429 => Error::Throttled,
            _ => Error::Schema {
                shape: format!("provider-status={code}"),
            },
        });
    }
    let code = status
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let message = value_for(object, &["message", "msg", "statusMessage"])
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if code.contains("unauthor")
        || code.contains("invalidcredential")
        || message.contains("unauthorized")
        || message.contains("invalid api key")
    {
        return Err(Error::AuthRejected);
    }
    if code.contains("throttl")
        || code.contains("toomanyrequest")
        || message.contains("too many requests")
        || message.contains("throttl")
    {
        return Err(Error::Throttled);
    }
    if code.contains("needlogin")
        || code.contains("login")
        || message.contains("console session")
        || message.contains("api key mode may be unavailable")
        || message.contains("login")
        || message.contains("log in")
    {
        return Err(Error::Unsupported);
    }
    if !code.is_empty() && code != "success" && code != "ok" && code != "0" && code != "200" {
        return Err(Error::Schema {
            shape: "provider-code".to_owned(),
        });
    }
    Ok(())
}

fn safe_shape(object: &Map<String, Value>) -> String {
    format!("object-fields={}", object.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn region_request_metadata_is_fixed() {
        for (region, host, region_id, commodity, referer) in [
            (
                AlibabaRegion::International,
                "modelstudio.console.alibabacloud.com",
                "ap-southeast-1",
                "sfm_codingplan_public_intl",
                "https://modelstudio.console.alibabacloud.com/ap-southeast-1/?tab=globalset#/efm/coding_plan",
            ),
            (
                AlibabaRegion::China,
                "bailian.console.aliyun.com",
                "cn-beijing",
                "sfm_codingplan_public_cn",
                "https://bailian.console.aliyun.com/cn-beijing/?tab=model#/efm/coding_plan",
            ),
        ] {
            let request = RequestMetadata::for_region(region);
            assert_eq!(request.host, host);
            assert_eq!(
                request.url(),
                format!(
                    "https://{host}/data/api.json?action={ACTION}&product={PRODUCT}&api={API}&currentRegionId={region_id}"
                )
            );
            assert_eq!(request.commodity_code, commodity);
            assert_eq!(request.referer, referer);
            let body = RequestBody {
                query: QueryRequest {
                    commodity_code: request.commodity_code,
                },
            };
            assert_eq!(
                serde_json::to_value(body).unwrap(),
                json!({"queryCodingPlanInstanceInfoRequest":{"commodityCode":commodity}})
            );
            let headers = request.headers("sentinel-secret");
            let header = |name| {
                headers
                    .iter()
                    .find_map(|(header, value)| (*header == name).then_some(value.as_str()))
            };
            assert_eq!(header("Accept"), Some("application/json"));
            assert_eq!(header("Authorization"), Some("Bearer sentinel-secret"));
            assert_eq!(header("x-api-key"), Some("sentinel-secret"));
            assert_eq!(header("X-DashScope-API-Key"), Some("sentinel-secret"));
            assert_eq!(header("Origin"), Some(format!("https://{host}").as_str()));
            assert_eq!(header("Referer"), Some(referer));
            assert_eq!(header("User-Agent"), Some(USER_AGENT));
        }
    }

    #[test]
    fn parses_active_instance_and_all_windows() {
        let body = json!({
            "data": {
                "codingPlanInstanceInfos": [
                    {"status":"EXPIRED","planName":"Old"},
                    {"status":"ACTIVE","planName":"Pro","codingPlanQuotaInfo":{
                        "per5HourUsedQuota":"25","per5HourTotalQuota":"100",
                        "per5HourQuotaNextRefreshTime":"2030-01-01T00:00:00Z",
                        "perWeekUsedQuota":50,"perWeekTotalQuota":200,
                        "perWeekQuotaNextRefreshTime":1893456000000_i64,
                        "perBillMonthUsedQuota":3,"perBillMonthTotalQuota":10,
                        "perBillMonthQuotaNextRefreshTime":1893456000
                    }}
                ]
            }
        });
        let snapshot = parse_response(&body.to_string(), AlibabaRegion::International).unwrap();
        assert_eq!(snapshot.plan.as_deref(), Some("Pro"));
        let windows = snapshot.rate_limits.unwrap().windows;
        assert_eq!(
            windows
                .iter()
                .map(|window| window.duration_mins)
                .collect::<Vec<_>>(),
            [Some(300), Some(10_080), Some(43_200)]
        );
        assert_eq!(windows[0].used_percentage, Some(25));
        assert_eq!(windows[2].used_percentage, Some(30));
    }

    #[test]
    fn parses_known_nested_string_envelope_and_plan_only_success() {
        let payload = json!({
            "codingPlanInstanceInfos":[{"status":"VALID","planName":"Starter"}]
        });
        let body = json!({
            "data": {"successResponse": {"body": payload.to_string()}}
        });
        let snapshot = parse_response(&body.to_string(), AlibabaRegion::China).unwrap();
        assert_eq!(snapshot.plan.as_deref(), Some("Starter"));
        assert!(snapshot.rate_limits.is_none());
    }

    #[test]
    fn rejects_login_and_unknown_success_without_secrets() {
        assert!(matches!(
            parse_response(
                r#"{"data":{"code":"ConsoleNeedLogin"}}"#,
                AlibabaRegion::China
            ),
            Err(Error::Unsupported)
        ));
        let error = parse_response(
            r#"{"sentinel-secret":"body","data":{"unexpected":true}}"#,
            AlibabaRegion::International,
        )
        .unwrap_err();
        assert!(!error.to_string().contains("body"));
        assert!(!error.to_string().contains("sentinel-secret"));
    }

    #[test]
    fn classifies_provider_auth_unavailable_and_throttle_responses() {
        for (body, expected) in [
            (r#"{"statusCode":403}"#, "auth"),
            (r#"{"statusCode":404}"#, "unsupported"),
            (r#"{"statusCode":"Throttling.User"}"#, "throttled"),
            (
                r#"{"message":"API key mode may be unavailable for this account"}"#,
                "unsupported",
            ),
        ] {
            let error = parse_response(body, AlibabaRegion::International).unwrap_err();
            assert!(
                matches!(
                    (expected, error),
                    ("auth", Error::AuthRejected)
                        | ("unsupported", Error::Unsupported)
                        | ("throttled", Error::Throttled)
                ),
                "wrong classification for {body}"
            );
        }
    }
}
