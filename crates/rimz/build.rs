//! Build-time tier-1 pricing embed.
//!
//! Compacts the checked-in LiteLLM pricing snapshot
//! (`pricing/litellm-pricing.json`) — or a `RIMZ_PRICING_JSON_PATH` override —
//! down to the model prefixes and per-token fields the binary reads, and writes
//! `$OUT_DIR/litellm-pricing.json` for `include_str!` (see
//! `src/agents/pricing/embedded.rs`).
//!
//! The build never touches the network, so every build is reproducible and
//! hermetic. The vendored snapshot is the embed source, kept current by `cargo
//! xtask pricing-refresh` (which fetches upstream and rewrites a reviewable,
//! committed snapshot); the runtime refresh (`src/agents/pricing/remote.rs`)
//! keeps prices fresh between releases. The compaction here mirrors `cargo xtask
//! pricing-refresh` — keep the two in step.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;

use serde_json::{Map, Value};

const VENDORED: &str = "pricing/litellm-pricing.json";
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
    println!("cargo:rerun-if-changed={VENDORED}");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR set by cargo"));
    let out_path = out_dir.join("litellm-pricing.json");
    let raw = resolve_raw_json();
    let compact = compact(&raw).expect("compact pricing JSON");
    fs::write(&out_path, compact).expect("write embedded pricing snapshot");
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

/// Resolve the raw LiteLLM document: a `RIMZ_PRICING_JSON_PATH` override, else
/// the checked-in vendored snapshot. No network — the snapshot is the source of
/// truth, refreshed deliberately by `cargo xtask pricing-refresh`.
fn resolve_raw_json() -> String {
    if let Some(path) = env::var_os("RIMZ_PRICING_JSON_PATH") {
        let path = PathBuf::from(path);
        println!("cargo:rerun-if-changed={}", path.display());
        return fs::read_to_string(&path).expect("read RIMZ_PRICING_JSON_PATH");
    }

    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let vendored = manifest.join(VENDORED);
    fs::read_to_string(&vendored).unwrap_or_else(|err| {
        panic!(
            "no embedded pricing source: {} is unreadable ({err}) — run \
             `cargo xtask pricing-refresh` or set RIMZ_PRICING_JSON_PATH",
            vendored.display()
        )
    })
}

/// Filter to the kept model prefixes and per-token fields; require both input
/// and output costs. Sorted (`BTreeMap`) and compact so the embed is small and
/// the vendored snapshot diffs stably.
fn compact(json: &str) -> Option<String> {
    let Value::Object(raw) = serde_json::from_str::<Value>(json).ok()? else {
        return None;
    };
    let mut out: BTreeMap<String, Value> = BTreeMap::new();
    for (model, pricing) in raw {
        if !is_kept_model(&model) {
            continue;
        }
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

fn is_kept_model(model: &str) -> bool {
    model.starts_with("gpt-")
        || model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("o4")
        || model.starts_with("codex")
        || model.starts_with("claude-")
}
