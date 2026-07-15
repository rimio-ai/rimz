use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::files::write_atomically;

const CONTENTS_URL: &str =
    "https://api.github.com/repos/mbadolato/iTerm2-Color-Schemes/contents/alacritty?ref=master";
const COMMIT_URL: &str =
    "https://api.github.com/repos/mbadolato/iTerm2-Color-Schemes/commits/master";
const LICENSE_URL: &str =
    "https://raw.githubusercontent.com/mbadolato/iTerm2-Color-Schemes/master/LICENSE";
const CATALOG_DIR: &str = "crates/rimz/themes/alacritty";
const THEMES_DIR: &str = "crates/rimz/themes";

struct ThemeSnapshot {
    themes: BTreeMap<String, String>,
    license: String,
    provenance: String,
}

pub(crate) fn theme_refresh(root: &Path) -> Result<()> {
    let snapshot = match env::var_os("RIMZ_THEMES_DIR") {
        Some(path) if !path.is_empty() => load_local_snapshot(PathBuf::from(path))?,
        _ => load_remote_snapshot()?,
    };
    if snapshot.themes.is_empty() {
        bail!("theme refresh found no Alacritty TOML files");
    }

    let catalog_dir = root.join(CATALOG_DIR);
    fs::create_dir_all(&catalog_dir)
        .with_context(|| format!("creating {}", catalog_dir.display()))?;
    let mut seen = BTreeSet::new();
    for (name, text) in snapshot.themes {
        let dest = catalog_dir.join(&name);
        write_atomically(&dest, text.as_bytes())?;
        seen.insert(name);
    }
    remove_stale_theme_files(&catalog_dir, &seen)?;

    let themes_dir = root.join(THEMES_DIR);
    write_atomically(&themes_dir.join("LICENSE"), snapshot.license.as_bytes())?;
    write_atomically(
        &themes_dir.join("README.md"),
        readme(&snapshot.provenance, seen.len()).as_bytes(),
    )?;
    Ok(())
}

fn load_remote_snapshot() -> Result<ThemeSnapshot> {
    let agent = http_agent();
    let contents = fetch_url(&agent, CONTENTS_URL).context("fetching Alacritty theme listing")?;
    let entries = parse_remote_theme_entries(&contents)?;
    let mut themes = BTreeMap::new();
    for (name, url) in entries {
        let text = fetch_url(&agent, &url).with_context(|| format!("fetching {name}"))?;
        themes.insert(name, text);
    }
    Ok(ThemeSnapshot {
        themes,
        license: fetch_url(&agent, LICENSE_URL).context("fetching iTerm2-Color-Schemes license")?,
        provenance: remote_provenance(&agent)?,
    })
}

fn parse_remote_theme_entries(json: &str) -> Result<BTreeMap<String, String>> {
    let Value::Array(entries) =
        serde_json::from_str::<Value>(json).context("parsing listing JSON")?
    else {
        bail!("GitHub contents response is not an array");
    };
    let mut out = BTreeMap::new();
    for entry in entries {
        let Value::Object(entry) = entry else {
            continue;
        };
        if entry.get("type").and_then(Value::as_str) != Some("file") {
            continue;
        }
        let Some(name) = entry.get("name").and_then(Value::as_str) else {
            continue;
        };
        if !name.ends_with(".toml") {
            continue;
        }
        let Some(url) = entry.get("download_url").and_then(Value::as_str) else {
            continue;
        };
        out.insert(name.to_owned(), url.to_owned());
    }
    Ok(out)
}

fn load_local_snapshot(path: PathBuf) -> Result<ThemeSnapshot> {
    let source = if path.join("alacritty").is_dir() {
        path.join("alacritty")
    } else {
        path.clone()
    };
    let repo = if source.file_name().and_then(|name| name.to_str()) == Some("alacritty") {
        source.parent().unwrap_or(&source).to_path_buf()
    } else {
        source.clone()
    };
    let mut themes = BTreeMap::new();
    for entry in fs::read_dir(&source).with_context(|| format!("reading {}", source.display()))? {
        let entry = entry.with_context(|| format!("reading {}", source.display()))?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .with_context(|| format!("{} has no UTF-8 file name", path.display()))?
            .to_owned();
        themes.insert(
            name,
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?,
        );
    }
    Ok(ThemeSnapshot {
        themes,
        license: fs::read_to_string(repo.join("LICENSE"))
            .with_context(|| format!("reading {}", repo.join("LICENSE").display()))?,
        provenance: local_provenance(&repo),
    })
}

fn local_provenance(repo: &Path) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "HEAD"])
        .output();
    if let Ok(output) = output
        && output.status.success()
    {
        let sha = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !sha.is_empty() {
            return format!("local checkout at {sha}");
        }
    }
    "local checkout at unknown revision".to_owned()
}

fn remote_provenance(agent: &ureq::Agent) -> Result<String> {
    let raw = fetch_url(agent, COMMIT_URL).context("fetching theme catalog revision")?;
    let value: Value = serde_json::from_str(&raw).context("parsing commit JSON")?;
    let sha = value
        .get("sha")
        .and_then(Value::as_str)
        .context("commit JSON is missing sha")?;
    let date = value
        .get("commit")
        .and_then(|commit| commit.get("committer"))
        .and_then(|committer| committer.get("date"))
        .and_then(Value::as_str)
        .unwrap_or("unknown date");
    Ok(format!("master {sha} ({date})"))
}

fn remove_stale_theme_files(dir: &Path, seen: &BTreeSet<String>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry.with_context(|| format!("reading {}", dir.display()))?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !seen.contains(name) {
            fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        }
    }
    Ok(())
}

fn readme(provenance: &str, count: usize) -> String {
    format!(
        "# Bundled Alacritty Themes\n\nThis directory vendors {count} Alacritty TOML themes from [mbadolato/iTerm2-Color-Schemes](https://github.com/mbadolato/iTerm2-Color-Schemes) for RimZ sidebar theme selection.\n\nSource revision: {provenance}.\n\nRefresh with `cargo xtask theme-refresh`. Set `RIMZ_THEMES_DIR=/path/to/iTerm2-Color-Schemes` to refresh from a local checkout without network access.\n\nThe theme files are data embedded into the RimZ binary at build time; they are not linked Rust dependencies.\n"
    )
}

fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(60)))
        .build()
        .new_agent()
}

fn fetch_url(agent: &ureq::Agent, url: &str) -> Result<String> {
    let mut last_error = None;
    for attempt in 1..=3 {
        match fetch_url_once(agent, url) {
            Ok(text) => return Ok(text),
            Err(err) => {
                last_error = Some(err);
                if attempt < 3 {
                    thread::sleep(Duration::from_millis(250 * attempt));
                }
            }
        }
    }
    Err(last_error.expect("fetch attempted at least once"))
}

fn fetch_url_once(agent: &ureq::Agent, url: &str) -> Result<String> {
    let mut response = agent.get(url).call().context("HTTP GET")?;
    if response.status().as_u16() != 200 {
        bail!("fetch returned HTTP {}", response.status().as_u16());
    }
    response
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_to_string()
        .context("reading response body")
}
