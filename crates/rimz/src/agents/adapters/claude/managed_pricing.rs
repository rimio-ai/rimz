//! Claude enterprise contracted-rate settings.

use std::collections::BTreeMap;
use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::UNIX_EPOCH;

use serde_json::Value;

use crate::agents::pricing::{PriceBook, Pricing};

const PER_MILLION: f64 = 1_000_000.0;
const MAX_RATE_PER_MTOK: f64 = 10_000.0;

#[derive(Clone, Debug, Default, PartialEq)]
struct ManagedPricing {
    multiplier: Option<f64>,
    overrides: BTreeMap<String, ManagedRates>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ManagedRates {
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileStamp {
    path: PathBuf,
    modified_nanos: u128,
    len: u64,
}

#[derive(Clone, Debug)]
struct LoadedPricing {
    pricing: Option<ManagedPricing>,
    fingerprint: Option<String>,
}

type ManagedPricingMemo = Option<(PathBuf, Vec<FileStamp>, Arc<LoadedPricing>)>;
type OverlayMemo = Option<(Arc<()>, String, Arc<PriceBook>)>;

static MANAGED_PRICING_MEMO: LazyLock<Mutex<ManagedPricingMemo>> =
    LazyLock::new(|| Mutex::new(None));
static OVERLAY_MEMO: LazyLock<Mutex<OverlayMemo>> = LazyLock::new(|| Mutex::new(None));

pub(super) enum ManagedPriceBook<'a> {
    Borrowed(&'a PriceBook),
    Shared(Arc<PriceBook>),
}

impl Deref for ManagedPriceBook<'_> {
    type Target = PriceBook;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Borrowed(book) => book,
            Self::Shared(book) => book,
        }
    }
}

pub(super) fn overlay(book: &PriceBook) -> ManagedPriceBook<'_> {
    let loaded = load_managed_pricing();
    let Some(pricing) = loaded.pricing.as_ref() else {
        return ManagedPriceBook::Borrowed(book);
    };
    let Some(fingerprint) = loaded.fingerprint.as_deref() else {
        return ManagedPriceBook::Borrowed(book);
    };
    let identity = book.identity();
    let mut memo = OVERLAY_MEMO.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((memo_identity, memo_fingerprint, overlaid)) = memo.as_ref()
        && Arc::ptr_eq(memo_identity, &identity)
        && memo_fingerprint == fingerprint
    {
        return ManagedPriceBook::Shared(Arc::clone(overlaid));
    }

    let overlaid = Arc::new(apply(book, pricing));
    *memo = Some((identity, fingerprint.to_owned(), Arc::clone(&overlaid)));
    ManagedPriceBook::Shared(overlaid)
}

pub(super) fn extend_fingerprint(base: Option<&str>) -> Option<String> {
    let managed = load_managed_pricing().fingerprint.clone();
    match (base, managed) {
        (Some(base), Some(managed)) => Some(format!("{base}|claude-managed:{managed}")),
        (Some(base), None) => Some(base.to_owned()),
        (None, Some(managed)) => Some(format!("claude-managed:{managed}")),
        (None, None) => None,
    }
}

pub(super) fn fingerprint_current() -> Option<String> {
    load_managed_pricing().fingerprint.clone()
}

fn load_managed_pricing() -> Arc<LoadedPricing> {
    let root = managed_settings_root();
    let files = managed_settings_files(&root);
    let stamps = file_stamps(&files);
    let mut memo = MANAGED_PRICING_MEMO
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Some((memo_root, memo_stamps, loaded)) = memo.as_ref()
        && *memo_root == root
        && *memo_stamps == stamps
    {
        return Arc::clone(loaded);
    }

    let pricing = read_managed_pricing(&files).filter(ManagedPricing::has_effect);
    let fingerprint = pricing.as_ref().map(|_| fingerprint(&stamps));
    let loaded = Arc::new(LoadedPricing {
        pricing,
        fingerprint,
    });
    *memo = Some((root, stamps, Arc::clone(&loaded)));
    loaded
}

#[cfg(target_os = "windows")]
fn managed_settings_root() -> PathBuf {
    PathBuf::from(r"C:\Program Files\ClaudeCode")
}

#[cfg(target_os = "macos")]
fn managed_settings_root() -> PathBuf {
    PathBuf::from("/Library/Application Support/ClaudeCode")
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn managed_settings_root() -> PathBuf {
    PathBuf::from("/etc/claude-code")
}

fn managed_settings_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let primary = root.join("managed-settings.json");
    if primary.is_file() {
        files.push(primary);
    }

    let mut fragments = fs::read_dir(root.join("managed-settings.d"))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
        })
        .collect::<Vec<_>>();
    fragments.sort();
    files.extend(fragments);
    files
}

fn file_stamps(files: &[PathBuf]) -> Vec<FileStamp> {
    files
        .iter()
        .filter_map(|path| {
            let metadata = fs::metadata(path).ok()?;
            let modified_nanos = metadata
                .modified()
                .ok()?
                .duration_since(UNIX_EPOCH)
                .ok()?
                .as_nanos();
            Some(FileStamp {
                path: path.clone(),
                modified_nanos,
                len: metadata.len(),
            })
        })
        .collect()
}

fn fingerprint(stamps: &[FileStamp]) -> String {
    stamps
        .iter()
        .map(|stamp| {
            format!(
                "{}:{}:{}",
                stamp.path.to_string_lossy(),
                stamp.modified_nanos,
                stamp.len
            )
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn read_managed_pricing(files: &[PathBuf]) -> Option<ManagedPricing> {
    let mut pricing = ManagedPricing::default();
    for path in files {
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        let Ok(root) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        merge_model_pricing(&mut pricing, &root);
    }
    pricing.has_effect().then_some(pricing)
}

fn merge_model_pricing(pricing: &mut ManagedPricing, root: &Value) {
    let Some(model_pricing) = root.get("modelPricing").and_then(Value::as_object) else {
        return;
    };
    if let Some(multiplier) = model_pricing
        .get("multiplier")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && *value > 0.0 && *value <= 1.0)
    {
        pricing.multiplier = Some(multiplier);
    }
    let Some(overrides) = model_pricing.get("overrides").and_then(Value::as_object) else {
        return;
    };
    for (model, value) in overrides {
        let model = model.trim();
        if model.is_empty() {
            continue;
        }
        if let Some(rates) = managed_rates(value) {
            pricing.overrides.insert(model.to_owned(), rates);
        }
    }
}

fn managed_rates(value: &Value) -> Option<ManagedRates> {
    let object = value.as_object()?;
    Some(ManagedRates {
        input: rate(object.get("input")?)?,
        output: rate(object.get("output")?)?,
        cache_read: rate(object.get("cacheRead")?)?,
        cache_write: rate(object.get("cacheWrite")?)?,
    })
}

fn rate(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .filter(|value| value.is_finite() && *value >= 0.0 && *value <= MAX_RATE_PER_MTOK)
}

impl ManagedPricing {
    fn has_effect(&self) -> bool {
        self.multiplier.is_some_and(|multiplier| multiplier != 1.0) || !self.overrides.is_empty()
    }
}

fn apply(book: &PriceBook, managed: &ManagedPricing) -> PriceBook {
    let multiplier = managed.multiplier.unwrap_or(1.0);
    let overrides = managed.overrides.iter().map(|(model, rates)| {
        let mut price = Pricing {
            input: rates.input / PER_MILLION,
            output: rates.output / PER_MILLION,
            cache_read: rates.cache_read / PER_MILLION,
            cache_create: rates.cache_write / PER_MILLION,
            cache_create_1h: Some(rates.cache_write / PER_MILLION),
            cache_read_explicit: true,
            max_input_tokens: book
                .exact_price(model)
                .and_then(|price| price.max_input_tokens),
            ..Pricing::empty()
        };
        scale_rates(&mut price, multiplier);
        (model.clone(), price)
    });
    book.derive_with_exact_overrides(
        |mut price| {
            scale_rates(&mut price, multiplier);
            price
        },
        overrides,
    )
}

fn scale_rates(price: &mut Pricing, multiplier: f64) {
    price.input *= multiplier;
    price.output *= multiplier;
    price.cache_read *= multiplier;
    price.cache_create *= multiplier;
    price.cache_create_1h = price.cache_create_1h.map(|rate| rate * multiplier);
    price.input_above_200k = price.input_above_200k.map(|rate| rate * multiplier);
    price.output_above_200k = price.output_above_200k.map(|rate| rate * multiplier);
    price.cache_create_above_200k = price.cache_create_above_200k.map(|rate| rate * multiplier);
    price.cache_read_above_200k = price.cache_read_above_200k.map(|rate| rate * multiplier);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::pricing::TokenSplit;
    use crate::agents::spending::SpendParse;
    use std::io::Write as _;

    #[test]
    fn managed_settings_merge_valid_fields_and_ignore_invalid_rows() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("managed-settings.json"),
            r#"{
                "modelPricing": {
                    "multiplier": 0.8,
                    "overrides": {
                        "claude-base": {"input": 10, "output": 20, "cacheRead": 1, "cacheWrite": 12},
                        "missing-field": {"input": 1, "output": 2, "cacheRead": 0.1},
                        "out-of-range": {"input": 1, "output": 10001, "cacheRead": 0.1, "cacheWrite": 1.2}
                    }
                }
            }"#,
        )
        .unwrap();
        let fragments = dir.path().join("managed-settings.d");
        fs::create_dir(&fragments).unwrap();
        fs::write(
            fragments.join("10-pricing.json"),
            r#"{"modelPricing":{"overrides":{"claude-base":{"input":5,"output":6,"cacheRead":0.5,"cacheWrite":7}}}}"#,
        )
        .unwrap();
        fs::write(
            fragments.join("20-discount.json"),
            r#"{"modelPricing":{"multiplier":0.5}}"#,
        )
        .unwrap();
        fs::write(fragments.join("ignored.txt"), "not json").unwrap();

        let pricing = read_managed_pricing(&managed_settings_files(dir.path())).unwrap();
        assert_eq!(pricing.multiplier, Some(0.5));
        assert_eq!(
            pricing.overrides.get("claude-base"),
            Some(&ManagedRates {
                input: 5.0,
                output: 6.0,
                cache_read: 0.5,
                cache_write: 7.0,
            })
        );
        assert!(!pricing.overrides.contains_key("missing-field"));
        assert!(!pricing.overrides.contains_key("out-of-range"));
    }

    #[test]
    fn invalid_multiplier_bounds_do_not_replace_a_valid_value() {
        let mut pricing = ManagedPricing::default();
        merge_model_pricing(
            &mut pricing,
            &serde_json::json!({"modelPricing": {"multiplier": 0.75}}),
        );
        for invalid in [0.0, -0.1, 1.1, 10_001.0] {
            merge_model_pricing(
                &mut pricing,
                &serde_json::json!({"modelPricing": {"multiplier": invalid}}),
            );
        }
        assert_eq!(pricing.multiplier, Some(0.75));
    }

    #[test]
    fn overlay_scales_list_rates_and_replaces_exact_models() {
        let book = PriceBook::from_litellm_json(
            r#"{
                "claude-base": {
                    "input_cost_per_token": 0.00001,
                    "output_cost_per_token": 0.00002,
                    "cache_read_input_token_cost": 0.000001,
                    "cache_creation_input_token_cost": 0.000012,
                    "max_input_tokens": 1000000,
                    "fast_mode_multiplier": 2
                },
                "claude-other": {
                    "input_cost_per_token": 0.000004,
                    "output_cost_per_token": 0.000008
                }
            }"#,
        );
        let managed = ManagedPricing {
            multiplier: Some(0.5),
            overrides: BTreeMap::from([(
                "claude-base".to_owned(),
                ManagedRates {
                    input: 6.0,
                    output: 12.0,
                    cache_read: 0.6,
                    cache_write: 7.5,
                },
            )]),
        };

        let overlaid = apply(&book, &managed);
        let exact = overlaid.exact_price("claude-base").unwrap();
        assert_eq!(exact.input, 3.0 / PER_MILLION);
        assert_eq!(exact.output, 6.0 / PER_MILLION);
        assert_eq!(exact.cache_read, 0.3 / PER_MILLION);
        assert_eq!(exact.cache_create, 3.75 / PER_MILLION);
        assert_eq!(exact.cache_create_1h, Some(3.75 / PER_MILLION));
        assert_eq!(exact.fast_multiplier, 1.0);
        assert_eq!(exact.max_input_tokens, Some(1_000_000));
        assert_eq!(
            overlaid.exact_price("claude-other").unwrap().input,
            0.000002
        );
        assert_eq!(
            overlaid.price("claude-base-via-gateway").unwrap().input,
            0.000005,
            "an exact override does not lend its rate through fuzzy lookup"
        );
        assert!(overlaid.exact_price("claude-unknown").is_none());
        assert_eq!(
            exact.cost_of(TokenSplit {
                cache_write_1h: 1_000_000,
                ..TokenSplit::default()
            }),
            3.75,
            "managed cacheWrite prices both cache-write durations"
        );
    }

    #[test]
    fn transcript_spend_uses_contracted_override() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut file = fs::File::create(&path).unwrap();
        writeln!(
            file,
            r#"{{"timestamp":"2026-01-01T10:00:00Z","requestId":"req-1","message":{{"id":"msg-1","model":"claude-managed","usage":{{"input_tokens":1000000,"output_tokens":1000000,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#
        )
        .unwrap();
        let managed = ManagedPricing {
            multiplier: None,
            overrides: BTreeMap::from([(
                "claude-managed".to_owned(),
                ManagedRates {
                    input: 2.0,
                    output: 4.0,
                    cache_read: 0.2,
                    cache_write: 2.5,
                },
            )]),
        };
        let prices = apply(&PriceBook::default(), &managed);

        let SpendParse { entries, .. } = super::super::spend::parse_claude_spend(&path, 0, &prices);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].cost_usd, 6.0);
        assert_eq!(
            prices
                .exact_price("claude-managed")
                .unwrap()
                .cost_of(TokenSplit::new(1_000_000, 1_000_000)),
            6.0
        );
    }
}
