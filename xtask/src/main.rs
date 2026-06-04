#![deny(clippy::print_stdout)]
#![deny(clippy::print_stderr)]

use std::collections::BTreeMap;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};

fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let task = args.next().unwrap_or_else(|| "ci".to_owned());
    let root = workspace_root()?;
    match task.as_str() {
        "build" => build(&root),
        "install" => install(&root),
        "fmt" => fmt(&root),
        "lint" => lint(&root),
        "test" => test(&root),
        "doctest" => doctest(&root),
        "deny" => deny(&root),
        "deps" => deps(&root),
        "vet" => vet(&root),
        "coverage" => coverage(&root),
        "semver" => semver(&root),
        "invariants" => invariants(&root),
        "pricing-refresh" => pricing_refresh(&root),
        "ci" => ci(&root),
        other => bail!("unknown xtask `{other}`"),
    }
}

fn fmt(root: &Path) -> Result<()> {
    run(root, "cargo", ["fmt", "--all", "--", "--check"])
}

fn lint(root: &Path) -> Result<()> {
    run(
        root,
        "cargo",
        [
            "clippy",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--locked",
            "--",
            "-D",
            "warnings",
        ],
    )
}

fn doctest(root: &Path) -> Result<()> {
    run(
        root,
        "cargo",
        ["test", "--workspace", "--doc", "--all-features", "--locked"],
    )
}

fn deny(root: &Path) -> Result<()> {
    run(root, "cargo", ["deny", "check", "-D", "warnings"])
}

fn vet(root: &Path) -> Result<()> {
    run(root, "cargo", ["vet"])
}

fn semver(root: &Path) -> Result<()> {
    if workspace_version(root)? == "0.0.0" {
        return Ok(());
    }
    run(root, "cargo", ["semver-checks"])
}

fn workspace_version(root: &Path) -> Result<String> {
    let manifest =
        fs::read_to_string(root.join("Cargo.toml")).context("reading workspace manifest")?;
    let manifest: toml::Value = toml::from_str(&manifest).context("parsing workspace manifest")?;
    manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("package"))
        .and_then(|package| package.get("version"))
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .context("workspace.package.version missing from Cargo.toml")
}

fn build(root: &Path) -> Result<()> {
    run(
        root,
        "cargo",
        ["build", "--workspace", "--all-features", "--locked"],
    )
}

fn install(root: &Path) -> Result<()> {
    for (path, bin) in [
        ("crates/rimz", "rimz"),
        ("crates/rimz-sidebar", "rimz-sidebar"),
    ] {
        run(
            root,
            "cargo",
            [
                "install", "--path", path, "--bin", bin, "--locked", "--force",
            ],
        )?;
    }
    Ok(())
}

// Gate ordering is performance, not taste:
//   1. The instant text gates (`fmt`, `invariants`) run first and fail fast —
//      a formatting or invariant break aborts before any compile is paid for.
//   2. The metadata-only audits (`deny`, `deps`, `vet`) never hold cargo's
//      build lock, so they overlap the compile gates on their own threads.
//   3. The compile gates run sequentially on this thread: two concurrent cargo
//      builds only serialize on the target-dir lock, so parallelizing them buys
//      nothing. `coverage` is the single instrumented test run (no separate
//      uninstrumented `test` pass); `lint` precedes it so a clippy break fails
//      before the expensive instrumented build.
type Gate = fn(&Path) -> Result<()>;

fn ci(root: &Path) -> Result<()> {
    let ci_start = Instant::now();
    let mut timings: Vec<(String, Duration)> = Vec::new();

    // Instant text gates first — a formatting or invariant break aborts before
    // any compile is paid for.
    for (name, gate) in [("fmt", fmt as Gate), ("invariants", invariants)] {
        let (name, elapsed, result) = timed(name, || gate(root));
        timings.push((name, elapsed));
        if let Err(err) = result {
            report_timings(ci_start.elapsed(), &timings);
            return Err(err);
        }
    }

    // Audits read `cargo metadata` and never hold the target-dir build lock, so
    // they run directly (not via `cargo xtask`, which would reacquire the lock)
    // and overlap the compile gates on their own threads.
    let audits: Vec<_> = [("deny", deny as Gate), ("deps", deps), ("vet", vet)]
        .into_iter()
        .map(|(name, gate)| {
            let root = root.to_path_buf();
            thread::spawn(move || timed(name, || gate(&root)))
        })
        .collect();

    // Compile gates serialize on the build lock, so run them sequentially.
    // `lint` precedes `coverage` so a clippy break fails before the expensive
    // instrumented test build; `coverage` is the single instrumented test run.
    let mut first_err: Option<anyhow::Error> = None;
    for (name, gate) in [
        ("lint", lint as Gate),
        ("coverage", coverage),
        ("doctest", doctest),
        ("semver", semver),
    ] {
        let (name, elapsed, result) = timed(name, || gate(root));
        timings.push((name, elapsed));
        if let Err(err) = result {
            first_err = Some(err);
            break;
        }
    }

    for audit in audits {
        let (name, elapsed, result) = audit.join().expect("audit gate thread panicked");
        timings.push((name, elapsed));
        if let Err(err) = result {
            first_err.get_or_insert(err);
        }
    }

    report_timings(ci_start.elapsed(), &timings);
    first_err.map_or(Ok(()), Err)
}

/// Time one gate, returning its name, wall-clock duration, and outcome so the
/// caller can both report timings and surface failures.
fn timed(name: &str, gate: impl FnOnce() -> Result<()>) -> (String, Duration, Result<()>) {
    let start = Instant::now();
    let result = gate();
    (name.to_owned(), start.elapsed(), result)
}

#[expect(
    clippy::print_stderr,
    reason = "xtask prints its CI gate timing summary to the operator's stderr"
)]
fn report_timings(wall_clock: Duration, timings: &[(String, Duration)]) {
    let mut sorted: Vec<&(String, Duration)> = timings.iter().collect();
    sorted.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    let secs = |d: Duration| format!("{:.1}s", d.as_secs_f64());
    eprintln!("gate timings (slowest first):");
    for (name, elapsed) in sorted {
        eprintln!("  {:>8}  {name}", secs(*elapsed));
    }
    eprintln!("  {:>8}  ci wall clock", secs(wall_clock));
}

// cargo-machete decides "I'm running under cargo" with
// `CARGO is set AND CARGO_PKG_NAME is unset`; since xtask is itself a cargo
// crate, `CARGO_PKG_NAME=xtask` is inherited and machete treats argv[1]
// ("machete") as a path. Clear it for the spawn.
fn deps(root: &Path) -> Result<()> {
    let status = Command::new("cargo")
        .arg("machete")
        .current_dir(root)
        .env_remove("CARGO_PKG_NAME")
        .status()
        .context("running `cargo`")?;
    ensure_success("cargo", &["machete"], status)
}

fn test(root: &Path) -> Result<()> {
    run(
        root,
        "cargo",
        [
            "nextest",
            "run",
            "--workspace",
            "--all-features",
            "--locked",
        ],
    )
}

// Coverage is the *only* test run in `ci`: `llvm-cov nextest` runs the suite
// under instrumentation, so there is no separate uninstrumented `test` pass to
// build and execute the workspace a second time.
fn coverage(root: &Path) -> Result<()> {
    run(
        root,
        "cargo",
        [
            "llvm-cov",
            "nextest",
            "--workspace",
            "--all-features",
            "--locked",
        ],
    )
}

// ── Pricing snapshot refresh ────────────────────────────────────────────────

const LITELLM_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
const VENDORED_SNAPSHOT: &str = "crates/rimz/pricing/litellm-pricing.json";
const KEPT_FIELDS: [&str; 4] = [
    "input_cost_per_token",
    "output_cost_per_token",
    "cache_read_input_token_cost",
    "cache_creation_input_token_cost",
];

/// Regenerate the checked-in LiteLLM pricing snapshot that `crates/rimz/build.rs`
/// embeds as the tier-1 table (and falls back to for offline builds). Fetches
/// upstream, compacts to the kept prefixes/fields, and writes a sorted,
/// pretty-printed JSON so the diff is reviewable. `RIMZ_PRICING_JSON_PATH`
/// overrides the network fetch with a local raw document.
///
/// The compaction mirrors `crates/rimz/build.rs::compact`; keep the two in step.
fn pricing_refresh(root: &Path) -> Result<()> {
    let raw = if let Some(path) = env::var_os("RIMZ_PRICING_JSON_PATH") {
        fs::read_to_string(&path).context("reading RIMZ_PRICING_JSON_PATH")?
    } else {
        fetch_litellm().context("fetching LiteLLM pricing JSON")?
    };
    let snapshot = compact_pretty(&raw).context("compacting pricing JSON")?;
    let dest = root.join(VENDORED_SNAPSHOT);
    fs::write(&dest, snapshot).with_context(|| format!("writing {}", dest.display()))?;
    Ok(())
}

fn fetch_litellm() -> Result<String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .new_agent();
    let mut response = agent.get(LITELLM_URL).call().context("HTTP GET")?;
    if response.status().as_u16() != 200 {
        bail!("LiteLLM fetch returned HTTP {}", response.status().as_u16());
    }
    response
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_to_string()
        .context("reading response body")
}

fn compact_pretty(json: &str) -> Result<String> {
    let Value::Object(raw) = serde_json::from_str::<Value>(json).context("parsing JSON")? else {
        bail!("pricing JSON is not an object");
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
    let mut pretty = serde_json::to_string_pretty(&out).context("serializing snapshot")?;
    pretty.push('\n');
    Ok(pretty)
}

fn is_kept_model(model: &str) -> bool {
    model.starts_with("gpt-")
        || model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("o4")
        || model.starts_with("codex")
        || model.starts_with("claude-")
}

fn run<I, S>(root: &Path, program: &str, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<_> = args.into_iter().collect();
    let status = Command::new(program)
        .args(args.iter().map(AsRef::as_ref))
        .current_dir(root)
        .status()
        .with_context(|| format!("running `{program}`"))?;
    ensure_success(program, &args, status)
}

fn ensure_success<S: AsRef<OsStr>>(program: &str, args: &[S], status: ExitStatus) -> Result<()> {
    if status.success() {
        return Ok(());
    }
    let rendered_args = args
        .iter()
        .map(|arg| arg.as_ref().to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    bail!("command failed: {program} {rendered_args}");
}

fn workspace_root() -> Result<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map(Path::to_path_buf)
        .context("xtask manifest has no workspace parent")
}

/// Architectural-invariant greps. Defense in depth: these are shallow string
/// matches, so an aliased import or a macro-generated name will bypass.
/// Treat them as a low-cost trip-wire that pairs with code review and the
/// type system; do not rely on them as the sole enforcement of any rule.
///
/// Each entry is `(needle, allow_predicate, message)`. The needle is split
/// across `concat!` calls so this file does not itself trip its own greps.
fn invariants(root: &Path) -> Result<()> {
    let files = tracked_text_files(root)?;
    let is_docs_or_xtask = |path: &Path| {
        path.starts_with(root.join("docs"))
            || path.starts_with(root.join("xtask"))
            || path.extension().and_then(OsStr::to_str) == Some("md")
    };
    let outside_sidebar = |path: &Path| {
        !path.starts_with(root.join("crates/rimz-sidebar")) || path.starts_with(root.join("xtask"))
    };

    let banned_imports: &[(&str, &str)] = &[
        (
            concat!("chrono", "::"),
            "workspace crates must use jiff, not chrono",
        ),
        (
            concat!("bytes", "::"),
            "workspace crates must not import bytes",
        ),
        (
            concat!("tokio_util", "::"),
            "workspace crates must not import tokio_util",
        ),
    ];
    for (needle, message) in banned_imports {
        ensure_no_match(&files, needle, is_docs_or_xtask, message)?;
    }

    ensure_no_match(
        &files,
        concat!("Stdio", "::", "inherit"),
        |path| {
            path.starts_with(root.join("xtask"))
                || path.extension().and_then(OsStr::to_str) == Some("md")
        },
        "hook subprocess paths must not inherit stdio",
    )?;

    for needle in [
        "rimz::ledger::atomic",
        "crate::ledger::atomic",
        "rimz::ledger::writer",
        "crate::ledger::writer",
    ] {
        ensure_no_match(
            &files,
            needle,
            outside_sidebar,
            "sidebar crate must not import ledger writer APIs",
        )?;
    }

    // Adapter spend parsers are the read-only, sidebar-safe cost surface
    // (`crates/rimz/src/agents/<name>/spend.rs` + the shared `transcript_fs`):
    // they must stay free of ledger-write, bridge, and broker imports so the
    // spending walk can never write durable state or block on a socket.
    let outside_spend_parsers = {
        let agents_root = root.join("crates/rimz/src/agents");
        move |path: &Path| {
            !(path.starts_with(&agents_root)
                && matches!(
                    path.file_name().and_then(OsStr::to_str),
                    Some("spend.rs" | "transcript_fs.rs")
                ))
        }
    };
    for needle in [
        concat!("ledger", "::", "atomic"),
        concat!("crate", "::", "bridge"),
        concat!("::", "broker"),
    ] {
        ensure_no_match(
            &files,
            needle,
            &outside_spend_parsers,
            "adapter spend parsers are read-only: no ledger writes, bridge, or broker imports",
        )?;
    }

    ensure_no_core_pane_auto_use(root, &files)?;
    Ok(())
}

fn tracked_text_files(root: &Path) -> Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .args(["ls-files"])
        .current_dir(root)
        .output()
        .context("running `git ls-files`")?;
    if !output.status.success() {
        bail!("git ls-files failed");
    }
    let files: Vec<_> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|path| {
            path.ends_with(".rs")
                || path.ends_with(".toml")
                || path.ends_with(".md")
                || path.ends_with(".json")
        })
        .map(|path| root.join(path))
        .collect();
    if files.is_empty() {
        return walk_text_files(root);
    }
    Ok(files)
}

fn walk_text_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    walk_text_files_inner(root, root, &mut files)?;
    Ok(files)
}

fn walk_text_files_inner(root: &Path, dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.starts_with(root.join(".git")) || path.starts_with(root.join("target")) {
            continue;
        }
        if path.is_dir() {
            walk_text_files_inner(root, &path, files)?;
        } else if matches!(
            path.extension().and_then(OsStr::to_str),
            Some("rs" | "toml" | "md" | "json")
        ) {
            files.push(path);
        }
    }
    Ok(())
}

fn ensure_no_match(
    files: &[PathBuf],
    needle: &str,
    allow: impl Fn(&Path) -> bool,
    message: &str,
) -> Result<()> {
    let mut violations = Vec::new();
    for path in files {
        if allow(path) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for (idx, line) in text.lines().enumerate() {
            if line.contains(needle) {
                violations.push(format!("{}:{}: {}", path.display(), idx + 1, line.trim()));
            }
        }
    }
    if violations.is_empty() {
        return Ok(());
    }
    bail!("{message}\n{}", violations.join("\n"));
}

fn ensure_no_core_pane_auto_use(root: &Path, files: &[PathBuf]) -> Result<()> {
    let allowed_prefixes = [
        root.join("crates/rimz/src/cli/pane.rs"),
        root.join("crates/rimz/src/mux"),
        root.join("crates/rimz/tests"),
        root.join("docs"),
        root.join("xtask"),
    ];
    for needle in [concat!("capture", "_pane("), concat!("send", "_keys(")] {
        ensure_no_match(
            files,
            needle,
            |path| {
                allowed_prefixes
                    .iter()
                    .any(|prefix| path.starts_with(prefix))
            },
            "core paths must not auto-use pane capture/send primitives",
        )?;
    }
    Ok(())
}
