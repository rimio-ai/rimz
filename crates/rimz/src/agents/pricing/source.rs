//! Upstream pricing fetch and projection.
//!
//! LiteLLM and models.dev publish different shapes. This module fetches both,
//! projects them into the compact LiteLLM-shaped document [`embedded::parse`]
//! already reads, and owns the authoritative provider and prefix allowlists.
//! The hidden `rimz pricing-refresh` helper and the runtime refresh use the same
//! projection, so release snapshots and post-release cache updates cannot drift.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value};

use super::PriceBook;

/// LiteLLM's canonical pricing document (`main`).
const LITELLM_URL: &str = "https://raw.githubusercontent.com/BerriAI/litellm/refs/heads/main/model_prices_and_context_window.json";
/// models.dev's aggregate model catalogue.
const MODELS_DEV_URL: &str = "https://models.dev/api.json";
/// Per-document budget for the weekly runtime refresh. Generous enough for a
/// slow link to pull both documents (1.6MB and 3.1MB today), because a refresh
/// that times out leaves the table untouched for another hour.
const RUNTIME_TIMEOUT_SECS: u64 = 15;
const REFRESH_TIMEOUT_SECS: u64 = 30;
const MAX_BYTES: u64 = 64 * 1024 * 1024;
const MIN_LITELLM_MODELS: usize = 1_000;

const KEPT_FIELDS: [&str; 11] = [
    "input_cost_per_token",
    "output_cost_per_token",
    "cache_read_input_token_cost",
    "cache_creation_input_token_cost",
    "input_cost_per_token_above_200k_tokens",
    "output_cost_per_token_above_200k_tokens",
    "cache_read_input_token_cost_above_200k_tokens",
    "cache_creation_input_token_cost_above_200k_tokens",
    "long_context_threshold",
    "max_input_tokens",
    "provider_specific_entry",
];

/// Official models.dev catalogues, in collision precedence order.
const MODELS_DEV_PROVIDERS: [&str; 8] = [
    "anthropic",
    "openai",
    "google",
    "xai",
    "zai",
    "zhipuai",
    "alibaba",
    "moonshotai",
];

/// Provider-namespaced prefixes whose LiteLLM rows may gain a bare alias.
///
/// Exact bare rows are installed before aliases, so they always win, and an
/// undated alias prefers a direct dated row over a namespaced one — LiteLLM
/// files Bedrock's Anthropic catalogue under `anthropic.`, and a few of those
/// rows carry a markup over the direct row. Regional and gateway prefixes do
/// not begin with one of these complete tokens and therefore remain
/// addressable only by their full upstream key.
const OFFICIAL_PREFIXES: [&str; 18] = [
    "anthropic.",
    "anthropic/",
    "openai.",
    "openai/",
    "google.",
    "google/",
    "gemini/",
    "xai.",
    "xai/",
    "zai.",
    "zai/",
    "zhipuai.",
    "zhipuai/",
    "alibaba.",
    "alibaba/",
    "moonshotai.",
    "moonshotai/",
    "moonshot/",
];

/// Models that carried hardcoded request-tier rates before the source
/// projection learned models.dev's context tiers.
const LONG_CONTEXT_CANARIES: [&str; 10] = [
    "claude-sonnet-4",
    "gpt-5.4",
    "gpt-5.4-pro",
    "gpt-5.5",
    "gpt-5.5-pro",
    "gpt-5.6",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
    "grok-4.5",
];

#[derive(Debug, thiserror::Error)]
pub enum SourceErr {
    #[error("reading {variable} at {}: {source}", path.display())]
    Override {
        variable: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("fetching {name}: {detail}")]
    Fetch { name: &'static str, detail: String },
    #[error("parsing {name} JSON: {source}")]
    Json {
        name: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("{0} JSON is not an object")]
    Shape(&'static str),
    #[error("serializing pricing snapshot: {0}")]
    Serialize(#[source] serde_json::Error),
    #[error("writing pricing snapshot: {0}")]
    Write(#[from] crate::store::atomic::AtomicErr),
    #[error("pricing coverage check failed: {0}")]
    Coverage(String),
}

pub type Result<T> = std::result::Result<T, SourceErr>;

/// Coverage facts produced by one source projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefreshReport {
    pub model_count: usize,
    pub litellm_model_count: usize,
    pub provider_model_counts: BTreeMap<&'static str, usize>,
}

/// Resolve both upstream documents and project them into one snapshot.
///
/// `out` names the destination to write atomically; `None` validates coverage
/// and leaves the filesystem alone.
pub fn refresh(out: Option<&Path>) -> Result<RefreshReport> {
    let check = out.is_none();
    let litellm_override = env::var_os("RIMZ_PRICING_JSON_PATH");
    let litellm = match litellm_override.as_ref() {
        Some(path) => read_override("RIMZ_PRICING_JSON_PATH", path)?,
        None => fetch_required("LiteLLM", LITELLM_URL)?,
    };
    let models_dev = match env::var_os("RIMZ_PRICING_MODELS_DEV_JSON_PATH") {
        Some(path) => Some(read_override("RIMZ_PRICING_MODELS_DEV_JSON_PATH", &path)?),
        // A LiteLLM-only override projects a partial table on purpose, which
        // coverage would then report as eight provider renames. Say what is
        // actually missing instead.
        None if litellm_override.is_some() && check => {
            return Err(SourceErr::Coverage(
                "RIMZ_PRICING_JSON_PATH is set without RIMZ_PRICING_MODELS_DEV_JSON_PATH; \
                 --check needs both documents"
                    .to_owned(),
            ));
        }
        None if litellm_override.is_some() => None,
        None => Some(fetch_required("models.dev", MODELS_DEV_URL)?),
    };

    let (snapshot, report) = project_sources(&litellm, models_dev.as_deref())?;
    match out {
        Some(out) => {
            let mut bytes = serde_json::to_vec(&snapshot).map_err(SourceErr::Serialize)?;
            bytes.push(b'\n');
            crate::store::atomic::write_bytes_atomically(out, &bytes)?;
        }
        None => check_coverage(&snapshot, &report)?,
    }
    Ok(report)
}

/// `true` when runtime network fetches are disabled.
pub(super) fn offline() -> bool {
    env::var_os("RIMZ_PRICING_OFFLINE").is_some()
}

/// Fetch and project LiteLLM for the runtime cache, returning `None` on any
/// network or shape failure so the spending pass keeps its current book.
pub(super) fn fetch_litellm() -> Option<String> {
    fetch_runtime("LiteLLM", LITELLM_URL)
}

/// Fetch models.dev for the runtime projection.
pub(super) fn fetch_models_dev() -> Option<String> {
    fetch_runtime("models.dev", MODELS_DEV_URL)
}

fn read_override(variable: &'static str, path: &std::ffi::OsStr) -> Result<String> {
    let path = PathBuf::from(path);
    fs::read_to_string(&path).map_err(|source| SourceErr::Override {
        variable,
        path,
        source,
    })
}

fn fetch_required(name: &'static str, url: &'static str) -> Result<String> {
    fetch(name, url, REFRESH_TIMEOUT_SECS)
}

fn fetch_runtime(name: &'static str, url: &'static str) -> Option<String> {
    if offline() {
        return None;
    }
    match fetch(name, url, RUNTIME_TIMEOUT_SECS) {
        Ok(body) => Some(body),
        Err(err) => {
            tracing::debug!(url, error = %err, "pricing fetch failed");
            None
        }
    }
}

fn fetch(name: &'static str, url: &'static str, timeout_secs: u64) -> Result<String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(timeout_secs)))
        .build()
        .new_agent();
    let mut response = agent.get(url).call().map_err(|err| SourceErr::Fetch {
        name,
        detail: err.to_string(),
    })?;
    response
        .body_mut()
        .with_config()
        .limit(MAX_BYTES)
        .read_to_string()
        .map_err(|err| SourceErr::Fetch {
            name,
            detail: err.to_string(),
        })
}

pub(super) fn project_sources(
    litellm: &str,
    models_dev: Option<&str>,
) -> Result<(BTreeMap<String, Value>, RefreshReport)> {
    let mut snapshot = compact_litellm(litellm)?;
    let litellm_model_count = snapshot.len();
    let mut provider_model_counts = MODELS_DEV_PROVIDERS
        .into_iter()
        .map(|provider| (provider, 0))
        .collect::<BTreeMap<_, _>>();

    if let Some(models_dev) = models_dev {
        let projection = compact_models_dev(models_dev)?;
        provider_model_counts = projection.provider_model_counts;
        merge_missing_fields(&mut snapshot, projection.models);
    }

    let report = RefreshReport {
        model_count: snapshot.len(),
        litellm_model_count,
        provider_model_counts,
    };
    Ok((snapshot, report))
}

fn compact_litellm(json: &str) -> Result<BTreeMap<String, Value>> {
    let Value::Object(raw) =
        serde_json::from_str::<Value>(json).map_err(|source| SourceErr::Json {
            name: "LiteLLM",
            source,
        })?
    else {
        return Err(SourceErr::Shape("LiteLLM"));
    };

    let mut out = BTreeMap::new();
    let mut aliases = Vec::new();
    for (model, pricing) in raw {
        let Value::Object(fields) = pricing else {
            continue;
        };
        let Some(compact) = compact_litellm_fields(&fields) else {
            continue;
        };
        if let Some(alias) = official_alias(&model) {
            aliases.push((alias, compact.clone()));
        }
        out.insert(model, Value::Object(compact));
    }

    let direct_undated = direct_undated_rows(&out);
    for (alias, fields) in aliases {
        let undated = strip_date_suffix(&alias).map(str::to_owned);
        out.entry(alias)
            .or_insert_with(|| Value::Object(fields.clone()));
        // A direct dated row states the model's own price; the namespaced row
        // may state a marked-up resale of it, so the direct fields win here.
        if let Some(undated) = undated {
            let fields = direct_undated.get(&undated).unwrap_or(&fields).clone();
            out.entry(undated).or_insert(Value::Object(fields));
        }
    }
    Ok(out)
}

/// Undated bases published directly, keyed to the newest dated row's fields.
///
/// Sorted iteration reaches the newest date last, so it takes the entry.
fn direct_undated_rows(rows: &BTreeMap<String, Value>) -> BTreeMap<String, Map<String, Value>> {
    let mut out = BTreeMap::new();
    for (model, fields) in rows {
        if is_namespaced(model) {
            continue;
        }
        let (Some(base), Some(fields)) = (strip_date_suffix(model), fields.as_object()) else {
            continue;
        };
        out.insert(base.to_owned(), fields.clone());
    }
    out
}

fn compact_litellm_fields(fields: &Map<String, Value>) -> Option<Map<String, Value>> {
    let mut kept = Map::new();
    for field in KEPT_FIELDS {
        if field == "provider_specific_entry" {
            continue;
        }
        if let Some(value) = fields.get(field)
            && !value.is_null()
        {
            kept.insert(field.to_owned(), value.clone());
        }
    }
    if let Some(provider_specific_entry) = compact_provider_specific_entry(fields) {
        kept.insert(
            "provider_specific_entry".to_owned(),
            provider_specific_entry,
        );
    }

    // LiteLLM names OpenAI's request-selected tier after its 272k boundary.
    // Normalize it into the fields the shared embedded parser already reads.
    for (source, target) in [
        (
            "input_cost_per_token_above_272k_tokens",
            "input_cost_per_token_above_200k_tokens",
        ),
        (
            "output_cost_per_token_above_272k_tokens",
            "output_cost_per_token_above_200k_tokens",
        ),
        (
            "cache_read_input_token_cost_above_272k_tokens",
            "cache_read_input_token_cost_above_200k_tokens",
        ),
        (
            "cache_creation_input_token_cost_above_272k_tokens",
            "cache_creation_input_token_cost_above_200k_tokens",
        ),
    ] {
        if !kept.contains_key(target)
            && let Some(value) = fields.get(source)
            && !value.is_null()
        {
            kept.insert(target.to_owned(), value.clone());
            kept.insert(
                "long_context_threshold".to_owned(),
                Value::from(272_000_u64),
            );
        }
    }

    (kept.contains_key("input_cost_per_token") && kept.contains_key("output_cost_per_token"))
        .then_some(kept)
}

fn compact_provider_specific_entry(fields: &Map<String, Value>) -> Option<Value> {
    let fast = fields
        .get("provider_specific_entry")
        .and_then(Value::as_object)
        .and_then(|entry| entry.get("fast"))
        .filter(|value| !value.is_null())?;
    let mut out = Map::new();
    out.insert("fast".to_owned(), fast.clone());
    Some(Value::Object(out))
}

/// The bare model id an official-prefixed LiteLLM key stands for.
///
/// An alias has to keep a version or date token of its own: `claude-v2:1`
/// spends its only version on the Bedrock revision suffix, and the bare
/// `claude` it would leave behind is a boundary-prefix of every Claude id, so
/// it would price the whole family at one 2023 row and hide unknown models
/// from the chase.
fn is_namespaced(model: &str) -> bool {
    OFFICIAL_PREFIXES
        .iter()
        .any(|prefix| model.starts_with(prefix))
}

fn official_alias(model: &str) -> Option<String> {
    let bare = OFFICIAL_PREFIXES
        .iter()
        .find_map(|prefix| model.strip_prefix(prefix))?;
    let stripped = strip_revision_suffix(bare);
    let consumed_the_only_version =
        stripped.len() < bare.len() && !stripped.bytes().any(|byte| byte.is_ascii_digit());
    (!consumed_the_only_version).then(|| stripped.to_owned())
}

fn strip_revision_suffix(model: &str) -> &str {
    let Some((base, revision)) = model.rsplit_once("-v") else {
        return model;
    };
    let Some((version, patch)) = revision.split_once(':') else {
        return model;
    };
    if !version.is_empty()
        && version.bytes().all(|byte| byte.is_ascii_digit())
        && !patch.is_empty()
        && patch.bytes().all(|byte| byte.is_ascii_digit())
    {
        base
    } else {
        model
    }
}

fn strip_date_suffix(model: &str) -> Option<&str> {
    let (base, date) = model.rsplit_once('-')?;
    (date.len() == 8 && date.bytes().all(|byte| byte.is_ascii_digit())).then_some(base)
}

struct ModelsDevProjection {
    models: BTreeMap<String, Value>,
    provider_model_counts: BTreeMap<&'static str, usize>,
}

fn compact_models_dev(json: &str) -> Result<ModelsDevProjection> {
    let Value::Object(providers) =
        serde_json::from_str::<Value>(json).map_err(|source| SourceErr::Json {
            name: "models.dev",
            source,
        })?
    else {
        return Err(SourceErr::Shape("models.dev"));
    };

    let mut out = BTreeMap::new();
    let mut provider_model_counts = BTreeMap::new();
    for provider_id in MODELS_DEV_PROVIDERS {
        let mut count = 0;
        let models = providers
            .get(provider_id)
            .and_then(|provider| provider.get("models"))
            .and_then(Value::as_object);
        if let Some(models) = models {
            for (model, details) in models {
                let Some(fields) = compact_models_dev_fields(details) else {
                    continue;
                };
                count += 1;
                out.entry(model.clone()).or_insert(Value::Object(fields));
            }
        }
        provider_model_counts.insert(provider_id, count);
    }

    Ok(ModelsDevProjection {
        models: out,
        provider_model_counts,
    })
}

fn compact_models_dev_fields(details: &Value) -> Option<Map<String, Value>> {
    let cost = details.get("cost")?.as_object()?;
    let per_million =
        |fields: &Map<String, Value>, key: &str| fields.get(key).and_then(Value::as_f64);
    let input = per_million(cost, "input")?;
    let output = per_million(cost, "output")?;

    let mut kept = Map::new();
    kept.insert("input_cost_per_token".to_owned(), per_token_value(input)?);
    kept.insert("output_cost_per_token".to_owned(), per_token_value(output)?);
    if let Some(cache_read) = per_million(cost, "cache_read").and_then(per_token_value) {
        kept.insert("cache_read_input_token_cost".to_owned(), cache_read);
    }
    if let Some(cache_write) = per_million(cost, "cache_write").and_then(per_token_value) {
        kept.insert("cache_creation_input_token_cost".to_owned(), cache_write);
    }
    if let Some(context) = details
        .get("limit")
        .and_then(Value::as_object)
        .and_then(|limit| limit.get("context"))
        .and_then(Value::as_u64)
        .filter(|context| *context > 0)
    {
        kept.insert("max_input_tokens".to_owned(), Value::from(context));
    }

    if let Some((tier, threshold)) = context_tier(cost) {
        insert_long_context_fields(&mut kept, tier, threshold);
    } else if let Some(tier) = cost.get("context_over_200k").and_then(Value::as_object) {
        insert_long_context_fields(&mut kept, tier, 200_000);
    }
    Some(kept)
}

fn context_tier(cost: &Map<String, Value>) -> Option<(&Map<String, Value>, u64)> {
    cost.get("tiers")?
        .as_array()?
        .iter()
        .filter_map(Value::as_object)
        .find_map(|tier| {
            let selector = tier.get("tier")?.as_object()?;
            (selector.get("type")?.as_str()? == "context")
                .then(|| selector.get("size")?.as_u64())
                .flatten()
                .filter(|threshold| *threshold > 0)
                .map(|threshold| (tier, threshold))
        })
}

fn insert_long_context_fields(
    kept: &mut Map<String, Value>,
    tier: &Map<String, Value>,
    threshold: u64,
) {
    for (source, target) in [
        ("input", "input_cost_per_token_above_200k_tokens"),
        ("output", "output_cost_per_token_above_200k_tokens"),
        (
            "cache_read",
            "cache_read_input_token_cost_above_200k_tokens",
        ),
        (
            "cache_write",
            "cache_creation_input_token_cost_above_200k_tokens",
        ),
    ] {
        if let Some(value) = tier
            .get(source)
            .and_then(Value::as_f64)
            .and_then(per_token_value)
        {
            kept.insert(target.to_owned(), value);
        }
    }
    kept.insert("long_context_threshold".to_owned(), Value::from(threshold));
}

fn per_token_value(per_million: f64) -> Option<Value> {
    serde_json::Number::from_f64(per_million / 1e6).map(Value::Number)
}

fn merge_missing_fields(
    snapshot: &mut BTreeMap<String, Value>,
    models_dev: BTreeMap<String, Value>,
) {
    for (model, models_dev_fields) in models_dev {
        let Some(existing) = snapshot.get_mut(&model) else {
            snapshot.insert(model, models_dev_fields);
            continue;
        };
        let (Some(existing), Some(models_dev_fields)) =
            (existing.as_object_mut(), models_dev_fields.as_object())
        else {
            continue;
        };
        for (field, value) in models_dev_fields {
            existing
                .entry(field.clone())
                .or_insert_with(|| value.clone());
        }
    }
}

fn check_coverage(snapshot: &BTreeMap<String, Value>, report: &RefreshReport) -> Result<()> {
    let mut failures = Vec::new();
    if report.litellm_model_count < MIN_LITELLM_MODELS {
        failures.push(format!(
            "LiteLLM yielded {} priced models, expected at least {MIN_LITELLM_MODELS}",
            report.litellm_model_count
        ));
    }
    for provider in MODELS_DEV_PROVIDERS {
        if report
            .provider_model_counts
            .get(provider)
            .copied()
            .unwrap_or(0)
            == 0
        {
            failures.push(format!(
                "models.dev provider `{provider}` is missing or has no priced models"
            ));
        }
    }

    let json = serde_json::to_string(snapshot).map_err(SourceErr::Serialize)?;
    let book = PriceBook::from_litellm_json(&json);
    for definition in super::super::registry::BUILTINS {
        if let Some(model) = definition.spec().default_model
            && book.price(model).is_none()
        {
            failures.push(format!(
                "{} default model `{model}` has no price",
                definition.spec().kind
            ));
        }
    }
    for model in LONG_CONTEXT_CANARIES {
        let Some(price) = book.price(model) else {
            failures.push(format!("long-context canary `{model}` has no price"));
            continue;
        };
        if price.input_above_200k.is_none() || price.output_above_200k.is_none() {
            failures.push(format!(
                "long-context canary `{model}` has no input/output tier"
            ));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(SourceErr::Coverage(failures.join("; ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LITELLM_FIXTURE: &str = include_str!("tests/fixtures/litellm.json");
    const MODELS_DEV_FIXTURE: &str = include_str!("tests/fixtures/models-dev.json");

    fn fields<'a>(table: &'a BTreeMap<String, Value>, model: &str) -> &'a Map<String, Value> {
        table.get(model).and_then(Value::as_object).unwrap()
    }

    fn price_field(fields: &Map<String, Value>, name: &str) -> f64 {
        fields.get(name).and_then(Value::as_f64).unwrap()
    }

    #[test]
    fn litellm_aliases_only_official_prefixes_and_strips_bedrock_revisions() {
        let table = compact_litellm(LITELLM_FIXTURE).unwrap();

        assert!(table.contains_key("claude-3-5-haiku-20241022"));
        assert!(table.contains_key("claude-3-5-haiku"));
        assert!(table.contains_key("claude-sonnet-4"));
        assert!(table.contains_key("grok-4.5"));
        assert!(table.contains_key("glm-4.6"));
        for upstream in [
            "eu.anthropic.regional-claude-v1:0",
            "vertex_ai/gateway-vertex",
            "azure_ai/gateway-azure",
            "openrouter/gateway-openrouter",
            "bedrock/gateway-bedrock",
            "baseten/gateway-baseten",
            "deepinfra/gateway-deepinfra",
            "vercel_ai_gateway/gateway-vercel",
        ] {
            assert_eq!(official_alias(upstream), None, "{upstream}");
        }
        assert_eq!(
            price_field(
                fields(&table, "claude-3-5-haiku-20241022"),
                "input_cost_per_token"
            ),
            0.8e-6
        );
    }

    #[test]
    fn litellm_refuses_an_alias_that_spends_its_only_version_on_the_revision() {
        let table = compact_litellm(LITELLM_FIXTURE).unwrap();

        assert_eq!(official_alias("anthropic.claude-v2:1"), None);
        assert!(
            !table.contains_key("claude"),
            "a bare `claude` row is a boundary-prefix of every Claude id"
        );
        assert!(table.contains_key("anthropic.claude-v2:1"));
    }

    #[test]
    fn undated_alias_takes_the_direct_row_over_the_bedrock_one() {
        let table = compact_litellm(LITELLM_FIXTURE).unwrap();

        // `anthropic.` is Bedrock's namespace and its 3.7 Sonnet row is 20% up
        // on the direct one, so the undated alias has to read the direct row.
        let alias = fields(&table, "claude-3-7-sonnet");
        assert_eq!(price_field(alias, "input_cost_per_token"), 3e-6);
        assert_eq!(price_field(alias, "output_cost_per_token"), 15e-6);
        assert_eq!(price_field(alias, "cache_read_input_token_cost"), 0.3e-6);
        assert_eq!(
            price_field(alias, "cache_creation_input_token_cost"),
            3.75e-6
        );

        // The dated Bedrock id stays reachable at its own price.
        assert_eq!(
            price_field(
                fields(&table, "claude-3-7-sonnet-20240620"),
                "input_cost_per_token"
            ),
            3.6e-6
        );
    }

    #[test]
    fn models_dev_projection_honors_precedence_tiers_and_context_capacity() {
        let projection = compact_models_dev(MODELS_DEV_FIXTURE).unwrap();
        let collision = fields(&projection.models, "collision");
        assert_eq!(
            price_field(collision, "input_cost_per_token"),
            1.0e-6,
            "zai wins before zhipuai"
        );

        let tiered = fields(&projection.models, "gpt-5.6-sol");
        assert_eq!(
            tiered.get("long_context_threshold").and_then(Value::as_u64),
            Some(272_000)
        );
        assert_eq!(
            tiered.get("max_input_tokens").and_then(Value::as_u64),
            Some(1_050_000)
        );
        assert_eq!(
            price_field(tiered, "input_cost_per_token_above_200k_tokens"),
            10e-6
        );
        assert_eq!(
            price_field(tiered, "cache_creation_input_token_cost_above_200k_tokens"),
            12.5e-6
        );
        assert!(
            projection
                .provider_model_counts
                .values()
                .all(|count| *count > 0)
        );
    }

    #[test]
    fn source_projection_preserves_removed_builtin_family_rates() {
        let (snapshot, _) = project_sources(LITELLM_FIXTURE, Some(MODELS_DEV_FIXTURE)).unwrap();
        let json = serde_json::to_string(&snapshot).unwrap();
        let book = PriceBook::from_litellm_json(&json);

        // Base and cache rates every deleted builtin published, per family.
        for (model, input, output, cache_create, cache_read) in [
            ("gpt-5", 1.25e-6, 10e-6, 1.25e-6, 0.125e-6),
            ("gpt-5.5", 5e-6, 30e-6, 5e-6, 0.5e-6),
            ("gpt-5.6-sol", 5e-6, 30e-6, 6.25e-6, 0.5e-6),
            ("gpt-5.6-terra", 2.5e-6, 15e-6, 3.125e-6, 0.25e-6),
            ("gpt-5.6-luna", 1e-6, 6e-6, 1.25e-6, 0.1e-6),
            ("glm-4.5", 0.6e-6, 2.2e-6, 0.0, 0.11e-6),
            ("glm-4.6", 0.6e-6, 2.2e-6, 0.0, 0.11e-6),
            ("glm-4.7", 0.6e-6, 2.2e-6, 0.0, 0.11e-6),
            ("glm-5", 1e-6, 3.2e-6, 0.0, 0.2e-6),
            ("glm-5-turbo", 1.2e-6, 4e-6, 0.0, 0.24e-6),
            ("glm-5.1", 1.4e-6, 4.4e-6, 0.0, 0.26e-6),
            // Qwen reports no cache writes, so its create rate stays the
            // ccusage default and only the read rate is ever billed.
            ("qwen3-coder-plus", 1e-6, 5e-6, 1.25e-6, 0.2e-6),
            ("qwen3-coder-flash", 0.3e-6, 1.5e-6, 0.375e-6, 0.06e-6),
            ("moonshot/kimi-k2.5", 0.6e-6, 3e-6, 0.75e-6, 0.1e-6),
            ("moonshot/kimi-k2.6", 0.95e-6, 4e-6, 1.1875e-6, 0.16e-6),
        ] {
            let price = book.price(model).unwrap_or_else(|| panic!("{model} price"));
            assert!((price.input - input).abs() < 1e-18, "{model} input");
            assert!((price.output - output).abs() < 1e-18, "{model} output");
            assert!(
                (price.cache_create - cache_create).abs() < 1e-18,
                "{model} cache create: {} wanted {cache_create}",
                price.cache_create
            );
            assert!(
                (price.cache_read - cache_read).abs() < 1e-18,
                "{model} cache read: {} wanted {cache_read}",
                price.cache_read
            );
        }

        // The Qwen read rate above comes from a declared ratio, so it has to
        // count as explicit — an implicit rate bills the cached slice at full
        // input in the Codex-shaped spend paths.
        assert!(book.price("qwen3-coder-plus").unwrap().cache_read_explicit);
        assert!(book.price("qwen3-coder-flash").unwrap().cache_read_explicit);

        let sol = book.price("gpt-5.6-sol").unwrap();
        assert_eq!(sol.long_context_threshold, Some(272_000));
        assert_eq!(sol.input_above_200k, Some(10e-6));
        assert_eq!(sol.output_above_200k, Some(45e-6));
        let grok = book.price("grok-4.5").unwrap();
        assert_eq!(grok.long_context_threshold, Some(200_000));
        assert_eq!(grok.input_above_200k, Some(4e-6));
        assert_eq!(grok.output_above_200k, Some(12e-6));
        assert_eq!(grok.max_input_tokens, Some(500_000));
    }
}
