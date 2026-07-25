use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::build::{build_plugin, verify_vendored_plugin};
use crate::docs_links::docs_links;
use crate::invariants::invariants;
use crate::runner::{ensure_success, run, run_streamed, run_with_env_and_removed};
use crate::sandbox::HostSandbox;
use crate::spinner::Spinner;

const ALL_FEATURE_LINT_ARGS: &[&str] = &[
    "clippy",
    "--workspace",
    "--all-targets",
    "--all-features",
    "--locked",
    "--",
    "-D",
    "warnings",
];
const INSTALL_HOST_LINT_ARGS: &[&str] = &[
    "clippy", "-p", "rimz", "--bin", "rimz", "--locked", "--", "-D", "warnings",
];
const INSTALL_DEV_HOST_LINT_ARGS: &[&str] = &[
    "clippy",
    "-p",
    "rimz",
    "--bin",
    "rimz",
    "--features",
    "sentry",
    "--locked",
    "--",
    "-D",
    "warnings",
];
// All features enables `testkit`, so lint both installed host shapes separately
// to keep test-only references from masking dead code.
const LINT_ARG_SETS: &[&[&str]] = &[
    ALL_FEATURE_LINT_ARGS,
    INSTALL_HOST_LINT_ARGS,
    INSTALL_DEV_HOST_LINT_ARGS,
];
const GATE_TEST_ARGS: &[&str] = &[
    "nextest",
    "run",
    "--profile",
    "gate",
    "--workspace",
    "--all-features",
    "--locked",
];
const CARGO_PROGRESS_VERBS: &[&str] = &[
    "Compiling",
    "Checking",
    "Finished",
    "Building",
    "Downloading",
    "Downloaded",
    "Updating",
    "Locking",
    "Blocking",
    "Running",
];
const TRIMMED_OUTPUT_MAX_CHARS: usize = 12_000;

pub(crate) fn fmt(root: &Path) -> Result<()> {
    run(root, "cargo", ["fmt", "--all", "--", "--check"])
}

pub(crate) fn lint(root: &Path) -> Result<()> {
    for args in LINT_ARG_SETS {
        run(root, "cargo", args.iter().copied())?;
    }
    Ok(())
}

pub(crate) fn deny(root: &Path) -> Result<()> {
    let args = deny_args(deny_offline(
        std::env::var("RIMZ_DENY_OFFLINE").ok().as_deref(),
    ));
    run(root, "cargo", args)
}

fn deny_args(offline: bool) -> Vec<&'static str> {
    let mut args = vec!["deny"];
    if offline {
        // CI bakes the advisory DB into the image and prepares a local index at
        // the canonical crates.io cache path, so read both locally. Unset
        // elsewhere keeps the public-upstream fetch.
        args.push("--offline");
    }
    args.extend(["check", "-D", "warnings"]);
    args
}

fn deny_offline(raw: Option<&str>) -> bool {
    matches!(raw, Some("1") | Some("true"))
}

pub(crate) fn vet(root: &Path) -> Result<()> {
    run(root, "cargo", ["vet", "--locked"])
}

/// Report Rust API drift against the published baseline. Advisory, and out of
/// every blocking gate on purpose.
///
/// RimZ ships as a binary. The `rimz` crate publishes a `lib` target so the
/// binary, tests, and benches can link the domain modules, and crates.io
/// carries it so `cargo install rimz` works — neither makes its Rust API a
/// supported surface, and no document offers one. `cargo semver-checks` reads
/// exactly that surface: it fires on internal refactors (renaming an error
/// enum's field) while staying blind to what callers actually depend on —
/// flags, output, exit codes, config keys, and persisted formats. Gating on it
/// would price every internal rename as a major release and make the version
/// number describe the library instead of the product.
///
/// The binary's contract has its own gates: the flag-surface snapshot in
/// `crates/rimz/src/cli/surface_tests.rs`, the visible-command guard in
/// `crates/rimz/src/cli/help.rs`, and the schema-version assertions in the
/// doctor integration suite. Run this task when a release note wants the API
/// delta, not to decide whether a change may land.
pub(crate) fn semver(root: &Path) -> Result<()> {
    if workspace_version(root)? == "0.0.0" {
        return Ok(());
    }
    let output = Command::new("cargo")
        .arg("semver-checks")
        .current_dir(root)
        .output()
        .context("running `cargo`")?;
    if output.status.success() {
        return Ok(());
    }
    if semver_registry_baseline_missing(&output.stderr) {
        report_semver_baseline_missing();
        return Ok(());
    }
    let _ = std::io::stdout().write_all(&output.stdout);
    let _ = std::io::stderr().write_all(&output.stderr);
    ensure_success("cargo", &["semver-checks"], output.status)
}

fn semver_registry_baseline_missing(stderr: &[u8]) -> bool {
    String::from_utf8_lossy(stderr).contains("rimz not found in registry (crates.io)")
}

#[expect(
    clippy::print_stderr,
    reason = "xtask reports why semver checks are skipped before the first public publish"
)]
fn report_semver_baseline_missing() {
    eprintln!("cargo semver-checks skipped: rimz has no crates.io baseline yet");
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

const COVERAGE_LCOV_PATH: &str = "target/ci/coverage/lcov.info";

// `checks` ordering is performance, not taste:
//   1. The instant text gates (`fmt`, `invariants`) run first and fail fast —
//      a formatting or invariant break aborts before any compile is paid for.
//   2. The metadata-only dependency check never holds cargo's build lock, so it
//      overlaps the compile gates on its own thread.
//   3. The compile gates run sequentially on this thread: two concurrent cargo
//      builds only serialize on the target-dir lock, so parallelizing them buys
//      nothing.
//
// `deny` and `vet` stay out of `checks`: `deny` runs offline against the baked
// advisory DB and a local index at the canonical crates.io cache path, while
// `vet` fetches the crates.io index directly and bypasses a
// `[source.crates-io]` mirror. They run in their own `externals` task and a
// standalone CI job (see `externals`). `semver` is advisory and gates nothing.
type Gate = fn(&Path) -> Result<()>;

type CompactGate = fn(&Path, &mut dyn FnMut(&str)) -> Result<GateResult>;

enum GateResult {
    Pass { note: Option<String> },
    Fail { detail: String },
}

pub(crate) fn gate(root: &Path) -> Result<()> {
    let steps = [
        ("fmt", gate_fmt as CompactGate),
        ("invariants", gate_invariants),
        ("docs-links", gate_docs_links),
        ("lint", gate_lint),
        ("test", gate_test),
    ];
    let total = steps.len();
    for (index, (name, step)) in steps.into_iter().enumerate() {
        let base_label = format!("gate [{}/{}] {name}", index + 1, total);
        let spinner = Spinner::new(&base_label);
        let mut progress = |line: &str| {
            let line = line.trim();
            if !line.is_empty() {
                let line = line.chars().take(100).collect::<String>();
                spinner.set(format!("{base_label} — {line}"));
            }
        };
        let result = step(root, &mut progress);
        drop(spinner);
        match result? {
            GateResult::Pass { note } => report_gate_pass(name, note.as_deref()),
            GateResult::Fail { detail } => {
                report_gate_failure(name, &detail);
                bail!("gate failed at {name}");
            }
        }
    }
    report_gate_complete();
    Ok(())
}

fn gate_fmt(root: &Path, progress: &mut dyn FnMut(&str)) -> Result<GateResult> {
    captured_cargo_gate(root, ["fmt", "--all"], &[], &[], None, progress)
}

fn gate_invariants(root: &Path, _progress: &mut dyn FnMut(&str)) -> Result<GateResult> {
    Ok(in_process_gate(|| invariants(root)))
}

fn gate_docs_links(root: &Path, _progress: &mut dyn FnMut(&str)) -> Result<GateResult> {
    Ok(in_process_gate(|| docs_links(root)))
}

fn gate_lint(root: &Path, progress: &mut dyn FnMut(&str)) -> Result<GateResult> {
    for args in LINT_ARG_SETS {
        let result = captured_cargo_gate(root, args.iter().copied(), &[], &[], None, progress)?;
        if matches!(result, GateResult::Fail { .. }) {
            return Ok(result);
        }
    }
    Ok(GateResult::Pass { note: None })
}

fn gate_test(root: &Path, progress: &mut dyn FnMut(&str)) -> Result<GateResult> {
    let sandbox = HostSandbox::for_tests(root)?;
    let env = sandbox.command_env();
    captured_cargo_gate(
        root,
        GATE_TEST_ARGS.iter().copied(),
        &env,
        &["NO_COLOR"],
        Some(extract_test_summary),
        progress,
    )
}

fn captured_cargo_gate<I, S>(
    root: &Path,
    args: I,
    envs: &[(&str, PathBuf)],
    removed_envs: &[&str],
    note: Option<fn(&str) -> Option<String>>,
    progress: &mut dyn FnMut(&str),
) -> Result<GateResult>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let captured = run_streamed(root, "cargo", args, envs, removed_envs, progress)?;
    if captured.status.success() {
        return Ok(GateResult::Pass {
            note: note.and_then(|extract| extract(&captured.output)),
        });
    }
    Ok(GateResult::Fail {
        detail: failure_detail(&captured.output),
    })
}

fn in_process_gate(gate: impl FnOnce() -> Result<()>) -> GateResult {
    match gate() {
        Ok(()) => GateResult::Pass { note: None },
        Err(err) => GateResult::Fail {
            detail: format!("{err:#}"),
        },
    }
}

pub(crate) fn checks(root: &Path) -> Result<()> {
    let checks_start = Instant::now();
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
            report_timings("checks", checks_start.elapsed(), &timings);
            return Err(err);
        }
    }

    // `cargo machete` reads metadata and never holds the target-dir build lock,
    // so it runs directly (not via `cargo xtask`, which would reacquire the
    // lock) and overlaps the compile gates on its own thread.
    let metadata_checks: Vec<_> = [("deps", deps as Gate)]
        .into_iter()
        .map(|(name, gate)| {
            let root = root.to_path_buf();
            thread::spawn(move || timed(name, || gate(&root)))
        })
        .collect();

    // Compile gates serialize on the build lock, so run them sequentially. The
    // wasm plugin compile is the cheapest compile gate; it fails fast before
    // the host lint build is paid for.
    let mut first_err: Option<anyhow::Error> = None;
    for (name, gate) in [
        ("build-plugin", build_plugin as Gate),
        ("plugin-provenance", verify_vendored_plugin),
        ("lint", lint),
    ] {
        let (name, elapsed, result) = timed(name, || gate(root));
        timings.push((name, elapsed));
        if let Err(err) = result {
            first_err = Some(err);
            break;
        }
    }

    for metadata_check in metadata_checks {
        let (name, elapsed, result) = metadata_check
            .join()
            .expect("metadata gate thread panicked");
        timings.push((name, elapsed));
        if let Err(err) = result {
            first_err.get_or_insert(err);
        }
    }

    report_timings("checks", checks_start.elapsed(), &timings);
    first_err.map_or(Ok(()), Err)
}

// Supply-chain checks that sit outside `checks`. `deny` runs offline against the
// baked advisory DB and a local index at the canonical crates.io cache path.
// `vet` fetches the registry index to resolve its audit set and bypasses a
// `[source.crates-io]` mirror, so they run as a standalone CI job where
// transient egress failures can be retried without failing `checks`. Both run
// so a single pass reports every signal; the first error is returned.
//
// `semver` is deliberately absent: it reads the crate's Rust API, which RimZ
// does not support as a surface, and the binary's contract is gated by the CLI
// surface snapshot in the test suite instead. See `semver`.
pub(crate) fn externals(root: &Path) -> Result<()> {
    let externals_start = Instant::now();
    let mut timings: Vec<(String, Duration)> = Vec::new();
    let mut first_err: Option<anyhow::Error> = None;
    for (name, gate) in [("deny", deny as Gate), ("vet", vet)] {
        let (name, elapsed, result) = timed(name, || gate(root));
        timings.push((name, elapsed));
        if let Err(err) = result {
            first_err.get_or_insert(err);
        }
    }
    report_timings("externals", externals_start.elapsed(), &timings);
    first_err.map_or(Ok(()), Err)
}

// Local full stack: non-test gates, then the whole test suite under plain
// nextest. Instrumented coverage runs through `coverage` and scheduled
// workflows, off the PR/push hot path.
pub(crate) fn ci(root: &Path) -> Result<()> {
    checks(root)?;
    test(root, &[])
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
    reason = "xtask prints its gate timing summary to the operator's stderr"
)]
fn report_timings(label: &str, wall_clock: Duration, timings: &[(String, Duration)]) {
    let mut sorted: Vec<&(String, Duration)> = timings.iter().collect();
    sorted.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    let secs = |d: Duration| format!("{:.1}s", d.as_secs_f64());
    eprintln!("gate timings (slowest first):");
    for (name, elapsed) in sorted {
        eprintln!("  {:>8}  {name}", secs(*elapsed));
    }
    eprintln!("  {:>8}  {label} wall clock", secs(wall_clock));
}

#[expect(
    clippy::print_stderr,
    reason = "xtask prints compact gate progress to the operator's stderr"
)]
fn report_gate_pass(name: &str, note: Option<&str>) {
    if let Some(note) = note {
        eprintln!("✓ {name} ({note})");
    } else {
        eprintln!("✓ {name}");
    }
}

#[expect(
    clippy::print_stderr,
    reason = "xtask prints compact gate failures and the next action to stderr"
)]
fn report_gate_failure(name: &str, detail: &str) {
    eprintln!("gate: fail at {name}");
    eprintln!("{detail}");
    eprintln!("NEXT: fix the {name} errors above, then rerun `cargo xtask gate`");
}

#[expect(
    clippy::print_stderr,
    reason = "xtask prints compact gate completion to the operator's stderr"
)]
fn report_gate_complete() {
    eprintln!("gate: pass");
}

fn failure_detail(output: &str) -> String {
    let detail = trim_cargo_noise(output);
    if detail.is_empty() {
        "command failed without output".to_owned()
    } else {
        detail
    }
}

fn trim_cargo_noise(output: &str) -> String {
    let mut lines = Vec::new();
    let mut previous_blank = true;
    for line in output.lines().filter(|line| !is_cargo_progress(line)) {
        let line = line.trim_end();
        if line.trim().is_empty() {
            if !previous_blank {
                lines.push(String::new());
                previous_blank = true;
            }
        } else {
            lines.push(line.to_owned());
            previous_blank = false;
        }
    }
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    bound_trimmed_output(lines.join("\n"))
}

fn is_cargo_progress(line: &str) -> bool {
    let line = line.trim_start();
    CARGO_PROGRESS_VERBS
        .iter()
        .any(|verb| line.starts_with(verb))
}

fn bound_trimmed_output(output: String) -> String {
    if output.chars().count() <= TRIMMED_OUTPUT_MAX_CHARS {
        return output;
    }
    let mut truncated: String = output.chars().take(TRIMMED_OUTPUT_MAX_CHARS).collect();
    truncated.push_str("\n... output truncated ...");
    truncated
}

fn extract_test_summary(output: &str) -> Option<String> {
    output
        .lines()
        .find(|line| line.contains("tests run:"))
        .map(|line| line.trim().to_owned())
}

// cargo-machete decides "I'm running under cargo" with
// `CARGO is set AND CARGO_PKG_NAME is unset`; since xtask is itself a cargo
// crate, `CARGO_PKG_NAME=xtask` is inherited and machete treats argv[1]
// ("machete") as a path. Clear it for the spawn.
pub(crate) fn deps(root: &Path) -> Result<()> {
    run_with_env_and_removed(root, "cargo", ["machete"], &[], &["CARGO_PKG_NAME"])
}

pub(crate) fn test(root: &Path, args: &[String]) -> Result<()> {
    let sandbox = HostSandbox::for_tests(root)?;
    let env = sandbox.command_env();
    let mut cargo_args = vec![
        "nextest".to_owned(),
        "run".to_owned(),
        "--workspace".to_owned(),
        "--all-features".to_owned(),
        "--locked".to_owned(),
    ];
    cargo_args.extend(args.iter().cloned());
    run_with_env_and_removed(root, "cargo", cargo_args, &env, &["NO_COLOR"])
}

pub(crate) fn test_archive(root: &Path, args: &[String]) -> Result<()> {
    let sandbox = HostSandbox::for_tests(root)?;
    let env = sandbox.command_env();
    let archive_parent = nextest_archive_file(args)
        .and_then(|archive_file| archive_file.parent().map(Path::to_path_buf))
        .filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = archive_parent {
        fs::create_dir_all(root.join(parent)).context("creating nextest archive directory")?;
    }

    let mut cargo_args = vec![
        "nextest".to_owned(),
        "archive".to_owned(),
        "--workspace".to_owned(),
        "--all-features".to_owned(),
        "--locked".to_owned(),
    ];
    cargo_args.extend(args.iter().cloned());
    run_with_env_and_removed(root, "cargo", cargo_args, &env, &["NO_COLOR"])
}

fn nextest_archive_file(args: &[String]) -> Option<PathBuf> {
    for (index, arg) in args.iter().enumerate() {
        if let Some(path) = arg.strip_prefix("--archive-file=") {
            return Some(PathBuf::from(path));
        }
        if arg == "--archive-file" {
            return args.get(index + 1).map(PathBuf::from);
        }
    }
    None
}

// Scheduled coverage runs the suite under instrumentation and emits lcov for
// workflow artifacts. The default nextest live-server groups bound mux
// concurrency per run.
pub(crate) fn coverage(root: &Path) -> Result<()> {
    let sandbox = HostSandbox::for_tests(root)?;
    let env = sandbox.command_env();
    // Stale profraw files from an interrupted local run can poison the merge.
    run(root, "cargo", ["llvm-cov", "clean", "--workspace"])?;
    fs::create_dir_all(root.join("target/ci/coverage"))
        .context("creating coverage output directory")?;
    run_with_env_and_removed(
        root,
        "cargo",
        [
            "llvm-cov",
            "nextest",
            "--lcov",
            "--output-path",
            COVERAGE_LCOV_PATH,
            "--workspace",
            "--all-features",
            "--locked",
        ],
        &env,
        &["NO_COLOR"],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_baseline_missing_matches_first_publish_error_only() {
        assert!(semver_registry_baseline_missing(
            b"error: failed to retrieve index\nCaused by:\n    rimz not found in registry (crates.io)"
        ));
        assert!(!semver_registry_baseline_missing(
            b"error: failed to retrieve index\nCaused by:\n    registry request failed"
        ));
    }

    #[test]
    fn deny_offline_enabled_only_for_truthy_flag() {
        assert!(deny_offline(Some("1")));
        assert!(deny_offline(Some("true")));
        assert!(!deny_offline(Some("0")));
        assert!(!deny_offline(Some("")));
        assert!(!deny_offline(None));
    }

    #[test]
    fn deny_offline_is_a_global_option() {
        assert_eq!(
            deny_args(true),
            vec!["deny", "--offline", "check", "-D", "warnings"]
        );
        assert_eq!(deny_args(false), vec!["deny", "check", "-D", "warnings"]);
    }

    #[test]
    fn install_host_lints_cover_both_non_test_feature_shapes() {
        assert!(!INSTALL_HOST_LINT_ARGS.contains(&"--features"));
        assert_eq!(
            INSTALL_DEV_HOST_LINT_ARGS
                .windows(2)
                .filter(|pair| pair[0] == "--features")
                .map(|pair| pair[1])
                .collect::<Vec<_>>(),
            ["sentry"]
        );
        for args in [INSTALL_HOST_LINT_ARGS, INSTALL_DEV_HOST_LINT_ARGS] {
            assert!(!args.contains(&"--all-features"));
            assert!(
                !args
                    .iter()
                    .any(|arg| arg.split(',').any(|feature| feature == "testkit"))
            );
            assert!(args.windows(2).any(|pair| pair == ["-D", "warnings"]));
        }
    }

    #[test]
    fn trim_cargo_noise_drops_progress_and_keeps_diagnostics() {
        let output = "\
   Compiling foo v1.2.3
    Finished `dev` profile
     Running `cargo clippy`


error[E0599]: no method named `run`


warning: unused variable
  --> src/x.rs:3:1
";

        assert_eq!(
            trim_cargo_noise(output),
            "\
error[E0599]: no method named `run`

warning: unused variable
  --> src/x.rs:3:1"
        );
    }

    #[test]
    fn extract_test_summary_reads_nextest_summary_line() {
        let output = "\
some setup line
Summary [   12.3s] 2611 tests run: 2611 passed, 42 skipped
";

        assert_eq!(
            extract_test_summary(output).as_deref(),
            Some("Summary [   12.3s] 2611 tests run: 2611 passed, 42 skipped")
        );
        assert_eq!(extract_test_summary("no summary here"), None);
    }

    #[test]
    fn nextest_archive_file_reads_split_and_equals_forms() {
        let split = [
            "--archive-file".to_owned(),
            "target/ci/archive.tar.zst".to_owned(),
        ];
        let equals = ["--archive-file=archive.tar.zst".to_owned()];

        assert_eq!(
            nextest_archive_file(&split).as_deref(),
            Some(Path::new("target/ci/archive.tar.zst"))
        );
        assert_eq!(
            nextest_archive_file(&equals).as_deref(),
            Some(Path::new("archive.tar.zst"))
        );
        assert_eq!(nextest_archive_file(&[]), None);
    }
}
