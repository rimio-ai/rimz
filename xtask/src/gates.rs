use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::atlas::conform_ratchet;
use crate::build::{build_plugin, verify_vendored_plugin};
use crate::docs_links::docs_links;
use crate::invariants::invariants;
use crate::runner::{Captured, ensure_success, run, run_streamed, run_with_env_and_removed};
use crate::sandbox::{AllowedSkips, HostSandbox};
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
const CHECK_ARGS: &[&str] = &[
    "check",
    "--workspace",
    "--all-targets",
    "--all-features",
    "--locked",
];
const DOC_ARGS: &[&str] = &[
    "doc",
    "--no-deps",
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
const NEXTEST_PROGRESS_PREFIXES: &[&str] = &["PASS [", "START [", "SLOW [", "TRY [", "LEAK ["];
const TRIMMED_OUTPUT_MAX_CHARS: usize = 12_000;
const COULD_NOT_COMPILE: &str = "error: could not compile `";

pub(crate) fn fmt(root: &Path) -> Result<()> {
    run(root, "cargo", ["fmt", "--all", "--", "--check"])
}

pub(crate) fn lint(root: &Path) -> Result<()> {
    lint_arg_sets(root, LINT_ARG_SETS)
}

/// The per-commit iteration signal: one all-feature clippy pass. The two
/// install-host passes each re-check the whole `rimz` crate under another
/// feature set, so they stay with `gate` and the `checks` composite.
pub(crate) fn lint_all_features(root: &Path) -> Result<()> {
    lint_arg_sets(root, &[ALL_FEATURE_LINT_ARGS])
}

fn lint_arg_sets(root: &Path, arg_sets: &[&[&str]]) -> Result<()> {
    for args in arg_sets {
        let captured = capture_cargo_task(root, "lint", args.iter().copied(), &[], &[])?;
        if !captured.status.success() {
            report_task_failure(
                "lint",
                &failure_detail(&captured.output),
                "cargo xtask lint",
            );
            bail!("lint failed");
        }
    }
    Ok(())
}

pub(crate) fn doc(root: &Path) -> Result<()> {
    run_with_env_and_removed(
        root,
        "cargo",
        DOC_ARGS.iter().copied(),
        &[("RUSTDOCFLAGS", "-D warnings".into())],
        &["CARGO_ENCODED_RUSTDOCFLAGS"],
    )
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
    cargo_args.extend(perf_bench_args(args));
    run(root, "cargo", cargo_args)
}

/// `--no-run` is a `cargo bench` flag, but callers write it after the `--`
/// separator (`cargo xtask perf -- --no-run`), where cargo would hand it to the
/// divan binary, which rejects it. Hoist it ahead of the separator.
fn perf_bench_args(args: &[String]) -> Vec<String> {
    let (mut cargo_flags, bench_args) = match args.iter().position(|arg| arg == "--") {
        Some(separator) => (args[..separator].to_vec(), &args[separator + 1..]),
        None => (args.to_vec(), &[][..]),
    };
    let (no_run, bench_args): (Vec<&String>, Vec<&String>) =
        bench_args.iter().partition(|arg| *arg == "--no-run");
    if !no_run.is_empty() && !cargo_flags.iter().any(|arg| arg == "--no-run") {
        cargo_flags.push("--no-run".to_owned());
    }
    if !bench_args.is_empty() {
        cargo_flags.push("--".to_owned());
        cargo_flags.extend(bench_args.into_iter().cloned());
    }
    cargo_flags
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

type CompactGate<'a> = &'a dyn Fn(&Path, &mut dyn FnMut(&str)) -> Result<GateResult>;

#[derive(Debug, PartialEq, Eq)]
enum GateResult {
    Pass { note: Option<String> },
    Fail { detail: String },
}

impl GateResult {
    /// Rides a test run's self-skip report under the pass note, the way the
    /// `FLAKY` recap does, or after a failure's detail.
    fn with_skip_report(self, report: Option<String>) -> Self {
        let Some(report) = report else {
            return self;
        };
        match self {
            Self::Pass { note: None } => Self::Pass { note: Some(report) },
            Self::Pass { note: Some(note) } => Self::Pass {
                note: Some(format!("{note}\n{report}")),
            },
            Self::Fail { detail } => Self::Fail {
                detail: format!("{detail}\n{report}"),
            },
        }
    }

    /// Under `--deny-skips`, a self-skip the allow-list does not cover fails
    /// the run even when nextest passed; `denied` is that refusal.
    fn denying_skips(self, denied: Option<String>) -> Self {
        let Some(denied) = denied else {
            return self;
        };
        let detail = match self {
            Self::Pass { note: None } => denied,
            Self::Pass { note: Some(shown) } | Self::Fail { detail: shown } => {
                format!("{shown}\n{denied}")
            }
        };
        Self::Fail { detail }
    }
}

/// How the gate treats formatting. `Fix` is the authoring default: the gate
/// formats the tree so an edit never costs a second pass. `Check` verifies
/// instead of writing, for a caller reviewing a tree it must not modify.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FmtMode {
    Fix,
    Check,
}

/// `keep_going` trades fail-fast for one complete report: every step runs, and
/// the lint and test steps run past their own first failure.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct GateOptions {
    fmt: FmtMode,
    keep_going: bool,
}

pub(crate) fn gate(root: &Path, args: &[String]) -> Result<()> {
    let options = parse_gate_options(args)?;
    warn_if_behind_trunk(root);
    let invocation = gate_invocation(options);
    let fmt_step = match options.fmt {
        FmtMode::Fix => gate_fmt_fix,
        FmtMode::Check => gate_fmt_check,
    };
    let lint_step =
        |root: &Path, progress: &mut dyn FnMut(&str)| gate_lint(root, options.keep_going, progress);
    let test_step =
        |root: &Path, progress: &mut dyn FnMut(&str)| gate_test(root, options.keep_going, progress);
    let steps: [(&str, CompactGate); 7] = [
        ("fmt", &fmt_step),
        ("invariants", &gate_invariants),
        ("conform", &gate_conform),
        ("docs-links", &gate_docs_links),
        ("lint", &lint_step),
        ("doc", &gate_doc),
        ("test", &test_step),
    ];
    let total = steps.len();
    let mut failed = Vec::new();
    let mut first_compile_failure = None;
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
        let detail = match result? {
            GateResult::Pass { note } => {
                report_gate_pass(name, note.as_deref());
                continue;
            }
            GateResult::Fail { detail } => detail,
        };
        if !options.keep_going {
            report_gate_failure(name, &detail, &invocation);
            bail!("gate failed at {name}");
        }
        match compile_cascade_source(&detail, first_compile_failure) {
            Some(earlier) => report_gate_blocked(name, earlier),
            None => report_gate_failure(name, &detail, &invocation),
        }
        if could_not_compile(&detail) {
            first_compile_failure.get_or_insert(name);
        }
        failed.push(name);
    }
    report_gate_complete(&failed);
    if !failed.is_empty() {
        bail!("gate failed at {}", failed.join(", "));
    }
    Ok(())
}

#[expect(
    clippy::print_stderr,
    reason = "xtask prints the stale-base advisory to the operator's stderr"
)]
fn warn_if_behind_trunk(root: &Path) {
    // Advisory only: use the fetched ref without adding network access to the gate.
    let Ok(output) = Command::new("git")
        .current_dir(root)
        .args(["rev-list", "--count", "HEAD..origin/main"])
        .output()
    else {
        return;
    };
    if !output.status.success() {
        return;
    }
    if let Ok(behind) = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u64>()
        && behind > 0
    {
        eprintln!("warning: branch is {behind} commits behind origin/main; rebase first");
    }
}

fn parse_gate_options(args: &[String]) -> Result<GateOptions> {
    let mut options = GateOptions {
        fmt: FmtMode::Fix,
        keep_going: false,
    };
    for arg in args {
        match arg.as_str() {
            "--check" if options.fmt == FmtMode::Fix => options.fmt = FmtMode::Check,
            "--keep-going" if !options.keep_going => options.keep_going = true,
            _ => bail!(
                "cargo xtask gate takes `--check` and `--keep-going`, each at most once; run `cargo xtask gate --help`"
            ),
        }
    }
    Ok(options)
}

fn gate_invocation(options: GateOptions) -> String {
    let mut invocation = "cargo xtask gate".to_owned();
    if options.fmt == FmtMode::Check {
        invocation.push_str(" --check");
    }
    if options.keep_going {
        invocation.push_str(" --keep-going");
    }
    invocation
}

/// Under `--keep-going`, a later step that fails because the workspace does
/// not compile repeats errors an earlier step already reported; this names
/// that earlier step so one pointer line replaces them. Rustdoc's own failures
/// say `could not document`, so a doc failure keeps its detail.
fn compile_cascade_source<'a>(
    detail: &str,
    first_compile_failure: Option<&'a str>,
) -> Option<&'a str> {
    first_compile_failure.filter(|_| could_not_compile(detail))
}

fn could_not_compile(output: &str) -> bool {
    output
        .lines()
        .any(|line| line.trim_start().starts_with(COULD_NOT_COMPILE))
}

fn gate_fmt_fix(root: &Path, progress: &mut dyn FnMut(&str)) -> Result<GateResult> {
    captured_cargo_gate(root, ["fmt", "--all"], &[], &[], None, progress)
}

fn gate_fmt_check(root: &Path, progress: &mut dyn FnMut(&str)) -> Result<GateResult> {
    captured_cargo_gate(
        root,
        ["fmt", "--all", "--", "--check"],
        &[],
        &[],
        None,
        progress,
    )
}

fn gate_invariants(root: &Path, _progress: &mut dyn FnMut(&str)) -> Result<GateResult> {
    Ok(in_process_gate(|| invariants(root)))
}

fn gate_conform(root: &Path, _progress: &mut dyn FnMut(&str)) -> Result<GateResult> {
    Ok(in_process_gate(|| conform_ratchet(root)))
}

fn gate_docs_links(root: &Path, _progress: &mut dyn FnMut(&str)) -> Result<GateResult> {
    Ok(in_process_gate(|| docs_links(root)))
}

/// Under `keep_going`, every lint set runs and cargo's own `--keep-going`
/// stops one crate's compile failure from hiding its workspace siblings'.
fn gate_lint(root: &Path, keep_going: bool, progress: &mut dyn FnMut(&str)) -> Result<GateResult> {
    let mut failures = Vec::new();
    for args in LINT_ARG_SETS {
        let mut args = args.to_vec();
        if keep_going {
            let cargo_flags_end = args.iter().position(|arg| *arg == "--");
            args.insert(cargo_flags_end.unwrap_or(args.len()), "--keep-going");
        }
        let result = captured_cargo_gate(root, args.iter().copied(), &[], &[], None, progress)?;
        let GateResult::Fail { detail } = result else {
            continue;
        };
        if !keep_going {
            return Ok(GateResult::Fail { detail });
        }
        failures.push(format!("== cargo {} ==\n{detail}", args.join(" ")));
    }
    if failures.is_empty() {
        return Ok(GateResult::Pass { note: None });
    }
    Ok(GateResult::Fail {
        detail: failures.join("\n\n"),
    })
}

fn gate_doc(root: &Path, progress: &mut dyn FnMut(&str)) -> Result<GateResult> {
    captured_cargo_gate(
        root,
        DOC_ARGS.iter().copied(),
        &[("RUSTDOCFLAGS", "-D warnings".into())],
        &["CARGO_ENCODED_RUSTDOCFLAGS"],
        None,
        progress,
    )
}

fn gate_test(root: &Path, keep_going: bool, progress: &mut dyn FnMut(&str)) -> Result<GateResult> {
    let sandbox = HostSandbox::for_tests(root)?;
    let env = sandbox.command_env();
    let removed = sandbox.removed_test_env();
    let no_fail_fast = keep_going.then_some("--no-fail-fast");
    let captured = run_streamed(
        root,
        "cargo",
        GATE_TEST_ARGS.iter().copied().chain(no_fail_fast),
        &env,
        &str_refs(&removed),
        progress,
    )?;
    Ok(captured_test_result(
        &captured,
        sandbox.skip_report(),
        None,
        &std::env::temp_dir(),
    ))
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
    let captured = capture_cargo(root, args, envs, removed_envs, progress)?;
    Ok(captured_result(&captured, note))
}

fn captured_result(captured: &Captured, note: Option<fn(&str) -> Option<String>>) -> GateResult {
    if captured.status.success() {
        return GateResult::Pass {
            note: note.and_then(|extract| extract(&captured.output)),
        };
    }
    GateResult::Fail {
        detail: failure_detail(&captured.output),
    }
}

fn captured_test_result(
    captured: &Captured,
    skip_report: Option<String>,
    denied_skips: Option<String>,
    output_dir: &Path,
) -> GateResult {
    let mut result = captured_result(captured, Some(extract_test_summary))
        .with_skip_report(skip_report)
        .denying_skips(denied_skips);
    if captured.status.success() {
        return result;
    }
    let saved = (|| -> Result<PathBuf> {
        let mut file = tempfile::Builder::new()
            .prefix("rimz-xtask-test-")
            .suffix(".output")
            .tempfile_in(std::path::absolute(output_dir)?)?;
        file.write_all(captured.output.as_bytes())?;
        let (_, path) = file.keep()?;
        Ok(path)
    })();
    if let GateResult::Fail { detail } = &mut result {
        let line = match saved {
            Ok(path) => format!(
                "{} ({} lines, {} KB)",
                path.display(),
                captured.output.lines().count(),
                captured.output.len().div_ceil(1024),
            ),
            Err(err) => format!("not saved ({err})"),
        };
        detail.push_str(&format!("\nfull test output: {line}"));
    }
    result
}

fn capture_cargo_task<I, S>(
    root: &Path,
    label: &str,
    args: I,
    envs: &[(&str, PathBuf)],
    removed_envs: &[&str],
) -> Result<Captured>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let spinner = Spinner::new(label);
    let mut progress = |line: &str| {
        let line = line.trim();
        if !line.is_empty() {
            let line = line.chars().take(100).collect::<String>();
            spinner.set(format!("{label} — {line}"));
        }
    };
    let captured = capture_cargo(root, args, envs, removed_envs, &mut progress);
    drop(spinner);
    captured
}

fn capture_cargo<I, S>(
    root: &Path,
    args: I,
    envs: &[(&str, PathBuf)],
    removed_envs: &[&str],
    progress: &mut dyn FnMut(&str),
) -> Result<Captured>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    run_streamed(root, "cargo", args, envs, removed_envs, progress).map_err(|error| {
        match error.downcast_ref::<crate::runner::BudgetOverrun>() {
            Some(timeout) => capture_timeout_error(timeout),
            None => error,
        }
    })
}

fn finish_cargo_task(name: &str, result: GateResult, invocation: &str) -> Result<()> {
    match result {
        GateResult::Pass { note } => {
            report_gate_pass(name, note.as_deref());
            Ok(())
        }
        GateResult::Fail { detail } => {
            report_task_failure(name, &detail, invocation);
            bail!("{name} failed");
        }
    }
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

    // Instant text gates first — a formatting, invariant, conform, or doc-link break
    // aborts before any compile is paid for.
    for (name, gate) in [
        ("fmt", fmt as Gate),
        ("invariants", invariants),
        ("conform", conform_ratchet),
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
        ("doc", doc),
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

/// Banner for a verification task run on its own. Several of them (`invariants`,
/// `docs-links`, `fmt`) pass in total silence, which reads the same as a crash
/// to a caller who only sees the terminal; the banner makes the pass explicit
/// and matches what the composite gate stack prints per step.
pub(crate) fn report_task_pass(name: &str) {
    report_gate_pass(name, None);
}

/// A note's first line rides in the pass line; any further lines (the nextest
/// `FLAKY` recap) follow it, indented.
#[expect(
    clippy::print_stderr,
    reason = "xtask prints compact gate progress to the operator's stderr"
)]
fn report_gate_pass(name: &str, note: Option<&str>) {
    let Some(note) = note else {
        eprintln!("✓ {name}");
        return;
    };
    let mut lines = note.lines();
    eprintln!("✓ {name} ({})", lines.next().unwrap_or_default());
    for line in lines {
        eprintln!("  {line}");
    }
}

#[expect(
    clippy::print_stderr,
    reason = "xtask prints compact gate progress to the operator's stderr"
)]
fn report_skips(report: Option<String>) {
    if let Some(report) = report {
        eprintln!("{report}");
    }
}

fn report_gate_failure(name: &str, detail: &str, invocation: &str) {
    report_failure("gate", name, detail, invocation);
}

/// A step blocked by an earlier step's compile errors has nothing of its own
/// to fix, so it carries no `NEXT:` hint; the earlier step's hint covers it.
#[expect(
    clippy::print_stderr,
    reason = "xtask prints compact failures and the next action to stderr"
)]
fn report_gate_blocked(name: &str, earlier: &str) {
    eprintln!("gate: fail at {name}");
    eprintln!("blocked by compile errors (see {earlier})");
}

fn report_task_failure(name: &str, detail: &str, invocation: &str) {
    report_failure("xtask", name, detail, invocation);
}

#[expect(
    clippy::print_stderr,
    reason = "xtask prints compact failures and the next action to stderr"
)]
fn report_failure(prefix: &str, name: &str, detail: &str, invocation: &str) {
    eprintln!("{prefix}: fail at {name}");
    eprintln!("{detail}");
    if compiler_died_without_diagnostic(detail) {
        eprintln!(
            "NEXT: rustc was killed (likely host out of memory); retry when other builds finish or set CARGO_BUILD_JOBS=2, then rerun `{invocation}`"
        );
    } else {
        eprintln!("NEXT: fix the {name} errors above, then rerun `{invocation}`");
    }
}

/// Cargo appends `due to N previous errors` to its `could not compile` line
/// whenever rustc emitted an error-level diagnostic. The bare line means the
/// compiler exited without one: killed by a signal (reported by cargo as
/// `signal: 9`, by sccache as text or as a silent exit 2), which on a busy host
/// is the out-of-memory killer. Any real diagnostic keeps the fix-it hint.
fn compiler_died_without_diagnostic(output: &str) -> bool {
    let mut compile_failures = output
        .lines()
        .filter(|line| line.trim_start().starts_with(COULD_NOT_COMPILE))
        .peekable();
    compile_failures.peek().is_some() && compile_failures.all(|line| !line.contains(" due to "))
}

#[expect(
    clippy::print_stderr,
    reason = "xtask prints compact gate completion to the operator's stderr"
)]
fn report_gate_complete(failed: &[&str]) {
    if failed.is_empty() {
        eprintln!("gate: pass");
    } else {
        eprintln!("gate: fail ({})", failed.join(", "));
    }
}

fn failure_detail(output: &str) -> String {
    let detail = trim_cargo_noise(output);
    if detail.is_empty() {
        "command failed without output".to_owned()
    } else {
        detail
    }
}

fn capture_timeout_error(timeout: &crate::runner::BudgetOverrun) -> anyhow::Error {
    let detail = trim_cargo_noise(&timeout.output);
    let detail = if detail.is_empty() {
        "step printed nothing beyond progress lines"
    } else {
        &detail
    };
    anyhow::anyhow!(
        "{}\nCaptured output before timeout:\n{detail}\n{}",
        timeout.summary,
        timeout.next_step
    )
}

fn trim_cargo_noise(output: &str) -> String {
    let mut lines = Vec::new();
    let mut previous_blank = true;
    for line in output
        .lines()
        .filter(|line| !is_cargo_progress(line) && !is_nextest_progress(line))
    {
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

fn is_nextest_progress(line: &str) -> bool {
    let line = line.trim_start();
    NEXTEST_PROGRESS_PREFIXES
        .iter()
        .any(|prefix| line.starts_with(prefix))
}

fn bound_trimmed_output(output: String) -> String {
    if output.chars().count() <= TRIMMED_OUTPUT_MAX_CHARS {
        return output;
    }
    let head_chars = TRIMMED_OUTPUT_MAX_CHARS / 3;
    let tail_chars = TRIMMED_OUTPUT_MAX_CHARS - head_chars;
    let head: String = output.chars().take(head_chars).collect();
    let mut tail: Vec<char> = output.chars().rev().take(tail_chars).collect();
    tail.reverse();
    format!(
        "{head}\n... output truncated; showing final diagnostics ...\n{}",
        tail.into_iter().collect::<String>()
    )
}

/// The nextest summary line, then the `FLAKY` recap lines nextest prints under
/// it for each test that passed only on retry, so a passing run's log still
/// names every flaky test in full. Lines are matched and reported without
/// color, since CI forces `CARGO_TERM_COLOR=always`.
fn extract_test_summary(output: &str) -> Option<String> {
    let mut lines = output
        .lines()
        .map(|line| strip_ansi_csi(line).trim().to_owned())
        .skip_while(|line| {
            !(line.contains("tests run:")
                || line.contains("test run:")
                || line.starts_with("cargo nextest:"))
        });
    let summary = lines.next()?;
    let recap = lines.filter(|line| line.starts_with("FLAKY "));
    Some(
        std::iter::once(summary)
            .chain(recap)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// Drops ANSI CSI sequences (`ESC [ params final-byte`), the shape of the
/// color codes nextest and cargo emit.
fn strip_ansi_csi(line: &str) -> String {
    let mut plain = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            plain.push(ch);
            continue;
        }
        if chars.next() == Some('[') {
            let _final_byte = chars.by_ref().find(|c| ('@'..='~').contains(c));
        }
    }
    plain
}

// cargo-machete decides "I'm running under cargo" with
// `CARGO is set AND CARGO_PKG_NAME is unset`; since xtask is itself a cargo
// crate, `CARGO_PKG_NAME=xtask` is inherited and machete treats argv[1]
// ("machete") as a path. Clear it for the spawn.
pub(crate) fn deps(root: &Path) -> Result<()> {
    run_with_env_and_removed(root, "cargo", ["machete"], &[], &["CARGO_PKG_NAME"])
}

pub(crate) fn check(root: &Path) -> Result<()> {
    let captured = capture_cargo_task(root, "check", CHECK_ARGS.iter().copied(), &[], &[])?;
    finish_cargo_task(
        "check",
        captured_result(&captured, None),
        "cargo xtask check",
    )
}

pub(crate) fn test(root: &Path, args: &[String]) -> Result<()> {
    let command = parse_test_command(args)?;
    let sandbox = HostSandbox::for_tests(root)?;
    let env = sandbox.command_env();
    let removed = sandbox.removed_test_env();
    let removed = str_refs(&removed);
    let invocation = test_invocation(args);
    let allowed_skips = command
        .deny_skips
        .then(|| AllowedSkips::load(root))
        .transpose()?;
    let denied_skips = || {
        allowed_skips
            .as_ref()
            .and_then(|allowed| sandbox.denied_skips(allowed))
    };

    if command.list {
        let mut cargo_args = nextest_args("list");
        cargo_args.extend(command.forwarded);
        let captured = capture_cargo_task(root, "test list", cargo_args, &env, &removed)?;
        if !captured.status.success() {
            report_task_failure("test list", &failure_detail(&captured.output), &invocation);
            bail!("test list failed");
        }
        std::io::stdout()
            .write_all(captured.stdout.as_bytes())
            .context("writing nextest list output")?;
        return Ok(());
    }

    if command.names.is_empty() {
        let mut cargo_args = nextest_args("run");
        let streams_output = requests_no_capture(&command.forwarded);
        cargo_args.extend(command.forwarded);
        if streams_output {
            return run_streaming_tests(
                root,
                cargo_args,
                &sandbox,
                &denied_skips,
                &invocation,
                args,
            );
        }
        let captured = capture_cargo_task(root, "test", cargo_args, &env, &removed)?;
        if nextest_matched_no_tests(captured.status.code(), &captured.output) {
            report_zero_test_match(args);
            bail!("no tests matched");
        }
        let result = captured_test_result(
            &captured,
            sandbox.skip_report(),
            denied_skips(),
            &std::env::temp_dir(),
        );
        return finish_cargo_task("test", result, &invocation);
    }

    run_named_tests(root, command, &sandbox, &denied_skips, &invocation)
}

fn str_refs(keys: &[String]) -> Vec<&str> {
    keys.iter().map(String::as_str).collect()
}

#[derive(Debug, PartialEq, Eq)]
struct TestCommand {
    names: Vec<String>,
    forwarded: Vec<String>,
    list: bool,
    deny_skips: bool,
}

fn parse_test_command(args: &[String]) -> Result<TestCommand> {
    let mut names = Vec::new();
    let mut forwarded = Vec::new();
    let mut list = false;
    let mut deny_skips = false;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            forwarded.extend(args[index..].iter().cloned());
            break;
        }
        if arg == "--list" {
            if list {
                bail!("cargo xtask test accepts `--list` once");
            }
            list = true;
            index += 1;
            continue;
        }
        if arg == "--deny-skips" {
            deny_skips = true;
            index += 1;
            continue;
        }
        if arg == "--name" {
            let Some(name) = args.get(index + 1).filter(|name| name.as_str() != "--") else {
                bail!("cargo xtask test `--name` requires a test name");
            };
            if name.is_empty() {
                bail!("cargo xtask test `--name` cannot be empty");
            }
            names.push(name.clone());
            index += 2;
            continue;
        }
        if let Some(name) = arg.strip_prefix("--name=") {
            if name.is_empty() {
                bail!("cargo xtask test `--name` cannot be empty");
            }
            names.push(name.to_owned());
            index += 1;
            continue;
        }
        forwarded.push(arg.clone());
        index += 1;
    }
    if list && !names.is_empty() {
        bail!("cargo xtask test does not combine `--list` with `--name`");
    }
    Ok(TestCommand {
        names,
        forwarded,
        list,
        deny_skips,
    })
}

fn nextest_args(subcommand: &str) -> Vec<String> {
    [
        "nextest",
        subcommand,
        "--workspace",
        "--all-features",
        "--locked",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn run_named_tests(
    root: &Path,
    command: TestCommand,
    sandbox: &HostSandbox,
    denied_skips: &dyn Fn() -> Option<String>,
    invocation: &str,
) -> Result<()> {
    let env = &sandbox.command_env();
    let removed = sandbox.removed_test_env();
    let removed = &str_refs(&removed);
    let filterset = exact_name_filterset(&command.names);
    let mut list_args = nextest_args("list");
    list_args.extend(nextest_profile_args(&command.forwarded));
    list_args.extend([
        "-E".to_owned(),
        filterset.clone(),
        "--message-format".to_owned(),
        "json".to_owned(),
    ]);
    let listed = capture_cargo_task(root, "test discovery", list_args, env, removed)?;
    if !listed.status.success() {
        report_task_failure(
            "test discovery",
            &failure_detail(&listed.output),
            invocation,
        );
        bail!("test discovery failed");
    }

    let listed_names = parse_nextest_list(&listed.stdout)?;
    let matches = match_requested_names(&command.names, &listed_names);
    if matches.matched_tests == 0 {
        report_test_selection(&command.names, &matches);
        bail!("none of the requested test names matched");
    }

    let mut run_args = nextest_args("run");
    run_args.extend(["-E".to_owned(), filterset]);
    let streams_output = requests_no_capture(&command.forwarded);
    run_args.extend(command.forwarded);
    if streams_output {
        report_test_selection(&command.names, &matches);
        run_streaming_tests(
            root,
            run_args,
            sandbox,
            denied_skips,
            invocation,
            &command.names,
        )?;
        if !matches.unmatched.is_empty() {
            bail!("some requested test names matched no tests");
        }
        return Ok(());
    }
    let captured = capture_cargo_task(root, "test", run_args, env, removed)?;
    report_test_selection(&command.names, &matches);
    let result = captured_test_result(
        &captured,
        sandbox.skip_report(),
        denied_skips(),
        &std::env::temp_dir(),
    );
    finish_cargo_task("test", result, invocation)?;
    if !matches.unmatched.is_empty() {
        bail!("some requested test names matched no tests");
    }
    Ok(())
}

/// `--no-capture` only means anything when the test's own output reaches the
/// operator, so a run that asks for it bypasses the spinner and inherits the
/// terminal. Both spellings appear in the wild, and libtest's sits after `--`.
fn requests_no_capture(args: &[String]) -> bool {
    args.iter()
        .any(|arg| matches!(arg.as_str(), "--no-capture" | "--nocapture"))
}

/// Run the tests on the operator's stdio. Nextest prints its own summary here,
/// so the gate line is the exit classification alone, after the self-skip
/// report the captured paths ride under their pass line.
fn run_streaming_tests(
    root: &Path,
    cargo_args: Vec<String>,
    sandbox: &HostSandbox,
    denied_skips: &dyn Fn() -> Option<String>,
    invocation: &str,
    requested: &[String],
) -> Result<()> {
    let env = sandbox.command_env();
    let removed = sandbox.removed_test_env();
    let status =
        crate::runner::run_inherited(root, "cargo", cargo_args, &env, &str_refs(&removed))?;
    report_skips(sandbox.skip_report());
    if nextest_matched_no_tests(status.code(), "") {
        report_zero_test_match(requested);
        bail!("no tests matched");
    }
    let failures: Vec<String> = (!status.success())
        .then(|| "test output streamed above".to_owned())
        .into_iter()
        .chain(denied_skips())
        .collect();
    if !failures.is_empty() {
        report_task_failure("test", &failures.join("\n"), invocation);
        bail!("test failed");
    }
    Ok(())
}

fn exact_name_filterset(names: &[String]) -> String {
    names
        .iter()
        .map(|name| format!("test(/(^|::){}$/)", escape_test_name(name)))
        .collect::<Vec<_>>()
        .join(" | ")
}

fn escape_test_name(name: &str) -> String {
    let mut escaped = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | ':') || !ch.is_ascii() {
            escaped.push(ch);
        } else {
            escaped.push('\\');
            escaped.push(ch);
        }
    }
    escaped
}

fn nextest_profile_args(args: &[String]) -> Vec<String> {
    let mut profiles = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            break;
        }
        if matches!(arg.as_str(), "-P" | "--profile") {
            profiles.push(arg.clone());
            if let Some(value) = args.get(index + 1) {
                profiles.push(value.clone());
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if arg.starts_with("--profile=") || (arg.starts_with("-P") && arg.len() > 2) {
            profiles.push(arg.clone());
        }
        index += 1;
    }
    profiles
}

#[derive(Deserialize)]
struct NextestList {
    #[serde(rename = "rust-suites")]
    rust_suites: BTreeMap<String, ListedSuite>,
}

#[derive(Deserialize)]
struct ListedSuite {
    #[serde(rename = "binary-path")]
    binary_path: PathBuf,
    cwd: PathBuf,
    testcases: BTreeMap<String, ListedTest>,
}

#[derive(Deserialize)]
struct ListedTest {
    #[serde(rename = "filter-match")]
    filter_match: ListedFilterMatch,
}

#[derive(Deserialize)]
struct ListedFilterMatch {
    status: String,
}

/// Every test the filterset matched, each under its suite's binary id.
fn listed_matches(output: &str) -> Result<Vec<(String, LocatedTest)>> {
    let listing: NextestList =
        serde_json::from_str(output).context("parsing `cargo nextest list` JSON")?;
    let mut matches = Vec::new();
    for (binary_id, suite) in listing.rust_suites {
        for (name, test) in suite.testcases {
            if test.filter_match.status == "matches" {
                let located = LocatedTest {
                    name,
                    binary: suite.binary_path.clone(),
                    cwd: suite.cwd.clone(),
                };
                matches.push((binary_id.clone(), located));
            }
        }
    }
    Ok(matches)
}

fn parse_nextest_list(output: &str) -> Result<Vec<String>> {
    Ok(listed_matches(output)?
        .into_iter()
        .map(|(_, test)| test.name)
        .collect())
}

/// One test resolved to the executable that holds it.
#[derive(Debug)]
pub(crate) struct LocatedTest {
    pub(crate) name: String,
    pub(crate) binary: PathBuf,
    pub(crate) cwd: PathBuf,
}

/// Build the workspace's test binaries and resolve `requested`, a name as
/// `cargo xtask test --name` takes it, to the one test it names.
pub(crate) fn locate_test(
    root: &Path,
    requested: &str,
    sandbox: &HostSandbox,
) -> Result<LocatedTest> {
    let env = sandbox.command_env();
    let removed = sandbox.removed_test_env();
    let mut list_args = nextest_args("list");
    list_args.extend([
        "-E".to_owned(),
        exact_name_filterset(&[requested.to_owned()]),
        "--message-format".to_owned(),
        "json".to_owned(),
    ]);
    let listed = capture_cargo_task(root, "test discovery", list_args, &env, &str_refs(&removed))?;
    if !listed.status.success() {
        report_task_failure(
            "test discovery",
            &failure_detail(&listed.output),
            &format!("cargo xtask stress {requested}"),
        );
        bail!("test discovery failed");
    }
    sole_listed_test(&listed.stdout, requested)
}

fn sole_listed_test(output: &str, requested: &str) -> Result<LocatedTest> {
    let mut matches = listed_matches(output)?;
    let fix = format!(
        "NEXT: pass one test's exact name, as `cargo xtask test --list {requested}` prints it"
    );
    match matches.len() {
        0 => bail!("no test is named `{requested}`\n{fix}"),
        1 => Ok(matches.remove(0).1),
        several => {
            let listed: String = matches
                .iter()
                .map(|(binary_id, test)| format!("\n  {binary_id} {}", test.name))
                .collect();
            bail!("`{requested}` names {several} tests; stress runs one:{listed}\n{fix}")
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct RequestedMatches {
    matched_requests: usize,
    matched_tests: usize,
    unmatched: Vec<String>,
}

fn match_requested_names(requested: &[String], listed: &[String]) -> RequestedMatches {
    let mut unmatched = Vec::new();
    let mut matched_requests = 0;
    for requested_name in requested {
        if listed
            .iter()
            .any(|listed_name| test_name_matches_request(listed_name, requested_name))
        {
            matched_requests += 1;
        } else {
            unmatched.push(requested_name.clone());
        }
    }
    RequestedMatches {
        matched_requests,
        matched_tests: listed.len(),
        unmatched,
    }
}

fn test_name_matches_request(listed: &str, requested: &str) -> bool {
    listed == requested
        || listed
            .strip_suffix(requested)
            .is_some_and(|prefix| prefix.ends_with("::"))
}

fn test_invocation(args: &[String]) -> String {
    if args.is_empty() {
        "cargo xtask test".to_owned()
    } else {
        format!("cargo xtask test {}", args.join(" "))
    }
}

fn nextest_matched_no_tests(status_code: Option<i32>, output: &str) -> bool {
    status_code == Some(4) || output.contains("error: no tests to run")
}

#[expect(
    clippy::print_stderr,
    reason = "xtask reports exact-name selection results to the operator"
)]
fn report_test_selection(requested: &[String], matches: &RequestedMatches) {
    eprintln!(
        "test selection: {} requested, {} matched name(s), {} matched test(s)",
        requested.len(),
        matches.matched_requests,
        matches.matched_tests
    );
    if !matches.unmatched.is_empty() {
        eprintln!("unmatched: {}", matches.unmatched.join(", "));
        eprintln!(
            "NEXT: inspect available names with `cargo xtask test --list {}`",
            matches.unmatched.join(" ")
        );
    }
}

#[expect(
    clippy::print_stderr,
    reason = "xtask explains empty nextest selections and gives a discovery command"
)]
fn report_zero_test_match(args: &[String]) {
    let rendered = if args.is_empty() {
        "<none>".to_owned()
    } else {
        args.join(" ")
    };
    eprintln!("xtask: no tests matched nextest arguments: {rendered}");
    eprintln!("The filter or active profile excluded every workspace test.");
    eprintln!("NEXT: inspect available names with `cargo xtask test --list {rendered}`");
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
    let removed = sandbox.removed_test_env();
    run_with_env_and_removed(root, "cargo", cargo_args, &env, &str_refs(&removed))
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
    let result = run_with_env_and_removed(
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
        &str_refs(&sandbox.removed_test_env()),
    );
    report_skips(sandbox.skip_report());
    result
}

#[cfg(test)]
mod tests;
