//! Build-time embeds for generated data bundled into `rimz`.
//!
//! Compacts the generated pricing snapshot
//! (`pricing/litellm-pricing.json`) — or a `RIMZ_PRICING_JSON_PATH` override —
//! down to the per-token fields the binary reads, and writes
//! `$OUT_DIR/litellm-pricing.json.gz` for `include_bytes!` (see
//! `src/agents/pricing/embedded.rs`).
//!
//! The build never touches the network, so every build is hermetic for a given
//! commit and worktree state. Release packaging runs `cargo xtask
//! pricing-refresh` first, which fetches LiteLLM plus authoritative models.dev
//! fillers and rewrites the ignored snapshot; the runtime refresh
//! (`src/agents/pricing/remote.rs`) keeps prices fresh between releases. The
//! compaction here mirrors `cargo xtask pricing-refresh` — keep the two in step.
//!
//! The sidebar theme catalog is checked in under `themes/alacritty/`, compacted
//! as a sorted JSON map, and written to `$OUT_DIR/alacritty-themes.json.gz` for
//! `include_bytes!` (see `src/sidebar_pane/render/embedded_themes.rs`).

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use flate2::Compression;
use flate2::write::GzEncoder;
use serde_json::{Map, Value};

const GENERATED_SNAPSHOT: &str = "pricing/litellm-pricing.json";
const THEME_CATALOG_DIR: &str = "themes/alacritty";
const PRESENCE_PLUGIN_ENV: &str = "RIMZ_EMBED_PRESENCE_PLUGIN";
const PRESENCE_PLUGIN_VENDOR_DIR: &str = "presence";
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
    println!("cargo:rerun-if-changed={THEME_CATALOG_DIR}");
    emit_build_version();

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR set by cargo"));
    let out_path = out_dir.join("litellm-pricing.json.gz");
    let raw = resolve_raw_json();
    let compact = compact(&raw).expect("compact pricing JSON");
    let compressed = gzip(&compact).expect("gzip embedded pricing snapshot");
    fs::write(&out_path, compressed).expect("write embedded pricing snapshot");
    write_themes_embed(&out_dir);
    write_presence_plugin_embed(&out_dir);
}

fn emit_build_version() {
    let package_version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION set by cargo");
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    emit_git_rerun_paths(&manifest);
    let version =
        git_build_version(&manifest, &package_version).unwrap_or_else(|| package_version.clone());
    println!("cargo:rustc-env=RIMZ_VERSION={version}");
}

fn git_build_version(manifest: &Path, package_version: &str) -> Option<String> {
    let exact_release_tag = git_stdout(
        manifest,
        &[
            "describe",
            "--tags",
            "--exact-match",
            "--match",
            "v[0-9]*",
            "HEAD",
        ],
    )
    .is_some();
    let short = git_stdout(manifest, &["rev-parse", "--short=12", "HEAD"])?;
    if short.is_empty() {
        return None;
    }
    let dirty = !git_stdout(manifest, &["status", "--porcelain"])?.is_empty();
    if exact_release_tag && !dirty {
        Some(package_version.to_owned())
    } else {
        Some(format!(
            "{package_version}+g{short}{}",
            if dirty { ".dirty" } else { "" }
        ))
    }
}

fn emit_git_rerun_paths(manifest: &Path) {
    let Some(head) = git_path(manifest, "HEAD") else {
        return;
    };
    emit_rerun_if_exists(&head);
    if let Some(branch) = git_stdout(manifest, &["symbolic-ref", "-q", "HEAD"])
        && let Some(branch_ref) = git_path(manifest, &branch)
    {
        emit_rerun_if_exists(&branch_ref);
    }
    if let Some(packed_refs) = git_path(manifest, "packed-refs") {
        emit_rerun_if_exists(&packed_refs);
    }
}

fn git_path(manifest: &Path, path: &str) -> Option<PathBuf> {
    let raw = git_stdout(manifest, &["rev-parse", "--git-path", path])?;
    if raw.is_empty() {
        return None;
    }
    let path = PathBuf::from(raw);
    Some(if path.is_absolute() {
        path
    } else {
        manifest.join(path)
    })
}

fn emit_rerun_if_exists(path: &Path) {
    if path.exists() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

fn git_stdout(manifest: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(manifest)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn write_themes_embed(out_dir: &Path) {
    let out_path = out_dir.join("alacritty-themes.json.gz");
    let catalog = read_theme_catalog();
    let json = serde_json::to_string(&catalog).expect("serialize embedded theme catalog");
    let compressed = gzip(&json).expect("gzip embedded theme catalog");
    fs::write(&out_path, compressed).expect("write embedded theme catalog");
}

fn read_theme_catalog() -> BTreeMap<String, String> {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let dir = manifest.join(THEME_CATALOG_DIR);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return BTreeMap::new(),
        Err(err) => panic!("theme catalog dir {} is unreadable ({err})", dir.display()),
    };

    let mut catalog = BTreeMap::new();
    for entry in entries {
        let entry = entry.unwrap_or_else(|err| {
            panic!(
                "theme catalog dir {} has an unreadable entry ({err})",
                dir.display()
            )
        });
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        println!("cargo:rerun-if-changed={}", path.display());
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_else(|| panic!("theme file {} has no UTF-8 stem", path.display()))
            .to_owned();
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("theme file {} is unreadable ({err})", path.display()));
        catalog.insert(name, text);
    }
    catalog
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
        _ => {
            let manifest =
                PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
            let path = manifest
                .join(PRESENCE_PLUGIN_VENDOR_DIR)
                .join(PRESENCE_PLUGIN_OUT);
            println!("cargo:rerun-if-changed={}", path.display());
            match fs::read(&path) {
                Ok(bytes) => bytes,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                Err(err) => panic!(
                    "vendored presence plugin {} is unreadable ({err})",
                    path.display()
                ),
            }
        }
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
