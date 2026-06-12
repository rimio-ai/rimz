//! Build-time tier-1 pricing embed.
//!
//! Compacts the generated pricing snapshot
//! (`pricing/litellm-pricing.json`) — or a `RIMZ_PRICING_JSON_PATH` override —
//! down to the per-token fields the binary reads, and writes
//! `$OUT_DIR/litellm-pricing.json.gz` for `include_bytes!` (see
//! `src/agents/pricing/embedded.rs`).
//!
//! The build never touches the network, so every build is reproducible and
//! hermetic. Release packaging runs `cargo xtask pricing-refresh` first, which
//! fetches LiteLLM plus authoritative models.dev fillers and rewrites the
//! ignored snapshot; the runtime refresh (`src/agents/pricing/remote.rs`) keeps
//! prices fresh between releases. The compaction here mirrors `cargo xtask
//! pricing-refresh` — keep the two in step.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use flate2::Compression;
use flate2::write::GzEncoder;
use serde_json::{Map, Value};

const GENERATED_SNAPSHOT: &str = "pricing/litellm-pricing.json";
const PRESENCE_PLUGIN_ENV: &str = "RIMZ_EMBED_PRESENCE_PLUGIN";
const PRESENCE_PLUGIN_OUT: &str = "rimz-presence-zellij.wasm";
const KEPT_FIELDS: [&str; 4] = [
    "input_cost_per_token",
    "output_cost_per_token",
    "cache_read_input_token_cost",
    "cache_creation_input_token_cost",
];

fn main() {
    println!("cargo:rerun-if-env-changed=RIMZ_PRICING_JSON_PATH");
    println!("cargo:rerun-if-env-changed={PRESENCE_PLUGIN_ENV}");
    println!("cargo:rerun-if-changed={GENERATED_SNAPSHOT}");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR set by cargo"));
    let out_path = out_dir.join("litellm-pricing.json.gz");
    let raw = resolve_raw_json();
    let compact = compact(&raw).expect("compact pricing JSON");
    let compressed = gzip(&compact).expect("gzip embedded pricing snapshot");
    fs::write(&out_path, compressed).expect("write embedded pricing snapshot");
    write_presence_plugin_embed(&out_dir);
}

fn write_presence_plugin_embed(out_dir: &std::path::Path) {
    let out_path = out_dir.join(PRESENCE_PLUGIN_OUT);
    let bytes = match env::var_os(PRESENCE_PLUGIN_ENV) {
        Some(path) if !path.is_empty() => {
            let path = PathBuf::from(path);
            println!("cargo:rerun-if-changed={}", path.display());
            fs::read(&path).unwrap_or_else(|err| {
                panic!(
                    "presence plugin embed source {} is unreadable ({err})",
                    path.display()
                )
            })
        }
        _ => Vec::new(),
    };
    fs::write(&out_path, bytes).expect("write embedded presence plugin");
}

/// Resolve the raw LiteLLM-shaped document: a `RIMZ_PRICING_JSON_PATH` override,
/// else the generated snapshot when present. No network — release packaging
/// refreshes the ignored snapshot before it builds. A missing snapshot embeds an
/// empty table; builtins and the runtime refresh still populate usable prices.
fn resolve_raw_json() -> String {
    if let Some(path) = env::var_os("RIMZ_PRICING_JSON_PATH") {
        let path = PathBuf::from(path);
        println!("cargo:rerun-if-changed={}", path.display());
        return fs::read_to_string(&path).expect("read RIMZ_PRICING_JSON_PATH");
    }

    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let generated = manifest.join(GENERATED_SNAPSHOT);
    match fs::read_to_string(&generated) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => "{}".to_owned(),
        Err(err) => {
            panic!(
                "generated pricing source {} is unreadable ({err})",
                generated.display()
            )
        }
    }
}

/// Filter to the per-token fields Rimz reads; require both input and output
/// costs. Sorted (`BTreeMap`), compact, and gzipped so the embedded full-model
/// table stays small.
fn compact(json: &str) -> Option<String> {
    let Value::Object(raw) = serde_json::from_str::<Value>(json).ok()? else {
        return None;
    };
    let mut out: BTreeMap<String, Value> = BTreeMap::new();
    for (model, pricing) in raw {
        let Value::Object(fields) = pricing else {
            continue;
        };
        let mut kept = Map::new();
        for field in KEPT_FIELDS {
            if let Some(value) = fields.get(field)
                && !value.is_null()
            {
                kept.insert(field.to_owned(), value.clone());
            }
        }
        if kept.contains_key("input_cost_per_token") && kept.contains_key("output_cost_per_token") {
            out.insert(model, Value::Object(kept));
        }
    }
    serde_json::to_string(&out).ok()
}

fn gzip(json: &str) -> Option<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(json.as_bytes()).ok()?;
    encoder.finish().ok()
}
