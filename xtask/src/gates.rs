use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::build::build_plugin;
use crate::docs_links::docs_links;
use crate::invariants::invariants;
use crate::runner::{ensure_success, run, run_with_env_removed};

pub(crate) fn fmt(root: &Path) -> Result<()> {
    run(root, "cargo", ["fmt", "--all", "--", "--check"])
}

pub(crate) fn lint(root: &Path) -> Result<()> {
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

pub(crate) fn doctest(root: &Path) -> Result<()> {
    run(
        root,
        "cargo",
        ["test", "--workspace", "--doc", "--all-features", "--locked"],
    )
}

pub(crate) fn deny(root: &Path) -> Result<()> {
    run(root, "cargo", ["deny", "check", "-D", "warnings"])
}

pub(crate) fn vet(root: &Path) -> Result<()> {
    run(root, "cargo", ["vet"])
}

pub(crate) fn semver(root: &Path) -> Result<()> {
    if workspace_version(root)? == "0.0.0" {
        return Ok(());
    }
    run(root, "cargo", ["semver-checks"])
}

pub(crate) fn perf(root: &Path, args: &[String]) -> Result<()> {
    let mut cargo_args = vec![
        "bench".to_owned(),
        "-p".to_owned(),
        "rimz".to_owned(),
        "--features".to_owned(),
        "testkit".to_owned(),
        "--locked".to_owned(),
    ];
    cargo_args.extend(args.iter().cloned());
    run(root, "cargo", cargo_args)
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

// Gate ordering is performance, not taste:
//   1. The instant text gates (`fmt`, `invariants`) run first and fail fast —
//      a formatting or invariant break aborts before any compile is paid for.
//   2. The metadata-only audits (`deny`, `deps`, `vet`) never hold cargo's
//      build lock, so they overlap the compile gates on their own threads.
//   3. The compile gates run sequentially on this thread: two concurrent cargo
//      builds only serialize on the target-dir lock, so parallelizing them buys
//      nothing. `coverage` is the single instrumented test run (no separate
//      uninstrumented `test` pass); `lint` and doctests precede it so cheaper
//      compile failures land before the expensive instrumented build.
type Gate = fn(&Path) -> Result<()>;

pub(crate) fn ci(root: &Path) -> Result<()> {
    let ci_start = Instant::now();
    let mut timings: Vec<(String, Duration)> = Vec::new();

    // Instant text gates first — a formatting, invariant, or doc-link break
    // aborts before any compile is paid for.
    for (name, gate) in [
        ("fmt", fmt as Gate),
        ("invariants", invariants),
        ("docs-links", docs_links),
    ] {
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
    // `lint` and doctests precede `coverage` so cheaper failures land before
    // the expensive instrumented test build. Keep doctests before
    // `cargo llvm-cov`: the coverage run owns and may rewrite target artifacts
    // that rustdoc otherwise reuses by fingerprint.
    let mut first_err: Option<anyhow::Error> = None;
    for (name, gate) in [
        // The wasm plugin compile is the cheapest compile gate; it fails fast
        // before the host lint/coverage builds are paid for.
        ("build-plugin", build_plugin as Gate),
        ("lint", lint),
        ("doctest", doctest),
        ("coverage", coverage),
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
pub(crate) fn deps(root: &Path) -> Result<()> {
    let status = Command::new("cargo")
        .arg("machete")
        .current_dir(root)
        .env_remove("CARGO_PKG_NAME")
        .status()
        .context("running `cargo`")?;
    ensure_success("cargo", &["machete"], status)
}

pub(crate) fn test(root: &Path, args: &[String]) -> Result<()> {
    let mut cargo_args = vec![
        "nextest".to_owned(),
        "run".to_owned(),
        "--workspace".to_owned(),
        "--all-features".to_owned(),
        "--locked".to_owned(),
    ];
    cargo_args.extend(args.iter().cloned());
    run_with_env_removed(root, "cargo", cargo_args, &["NO_COLOR"])
}

// Coverage is the *only* test run in `ci`: `llvm-cov nextest` runs the suite
// under instrumentation, so there is no separate uninstrumented `test` pass to
// build and execute the workspace a second time. `-P ci` pins the live-Zellij
// cap to one server per run so overlapping coverage jobs on the shared runner
// stay inside the safe server envelope (see .config/nextest.toml).
pub(crate) fn coverage(root: &Path) -> Result<()> {
    // Stale profraw files from an interrupted local run can poison the merge.
    run(root, "cargo", ["llvm-cov", "clean", "--workspace"])?;
    run_with_env_removed(
        root,
        "cargo",
        [
            "llvm-cov",
            "nextest",
            "--profile",
            "ci",
            "--workspace",
            "--all-features",
            "--locked",
        ],
        &["NO_COLOR"],
    )
}
