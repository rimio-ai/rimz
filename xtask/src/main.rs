//! Contributor task runner — `cargo xtask <task>`; `ci` composes the full quality-gate stack.

#![deny(clippy::print_stdout)]
#![deny(clippy::print_stderr)]

use std::collections::BTreeMap;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::process::{Command, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};

const PRESENCE_PLUGIN_TARGET: &str = "wasm32-wasip1";

struct TaskInfo {
    name: &'static str,
    summary: &'static str,
    runs: &'static str,
}

const TASKS: &[TaskInfo] = &[
    TaskInfo {
        name: "build",
        summary: "Build rimz and the Zellij presence plugin.",
        runs: "cargo build --workspace --all-features --locked",
    },
    TaskInfo {
        name: "build-plugin",
        summary: "Build the Zellij presence plugin wasm artifact.",
        runs: "cargo build -p rimz-presence-zellij --target wasm32-wasip1 --release --locked",
    },
    TaskInfo {
        name: "install",
        summary: "Build and install the rimz binary.",
        runs: "cargo xtask stage-install, then atomically installs rimz",
    },
    TaskInfo {
        name: "stage-install",
        summary: "Build release artifacts into target/xtask/install/bin.",
        runs: "build-plugin, then cargo build -p rimz --bin rimz --release --locked",
    },
    TaskInfo {
        name: "fmt",
        summary: "Check Rust formatting.",
        runs: "cargo fmt --all -- --check",
    },
    TaskInfo {
        name: "lint",
        summary: "Run clippy with warnings as errors.",
        runs: "cargo clippy --workspace --all-targets --all-features --locked -- -D warnings",
    },
    TaskInfo {
        name: "test",
        summary: "Run the workspace test suite through nextest.",
        runs: "cargo nextest run --workspace --all-features --locked",
    },
    TaskInfo {
        name: "doctest",
        summary: "Run workspace doctests.",
        runs: "cargo test --workspace --doc --all-features --locked",
    },
    TaskInfo {
        name: "deny",
        summary: "Run cargo-deny policy checks.",
        runs: "cargo deny check -D warnings",
    },
    TaskInfo {
        name: "deps",
        summary: "Run the unused dependency check.",
        runs: "cargo machete",
    },
    TaskInfo {
        name: "vet",
        summary: "Run cargo-vet supply-chain checks.",
        runs: "cargo vet",
    },
    TaskInfo {
        name: "coverage",
        summary: "Run the instrumented nextest coverage suite.",
        runs: "cargo llvm-cov nextest --workspace --all-features --locked",
    },
    TaskInfo {
        name: "semver",
        summary: "Run semver checks for published versions.",
        runs: "cargo semver-checks",
    },
    TaskInfo {
        name: "invariants",
        summary: "Run repository architecture invariants.",
        runs: "grep-style invariants implemented in xtask",
    },
    TaskInfo {
        name: "pricing-refresh",
        summary: "Refresh the vendored LiteLLM pricing snapshot.",
        runs: "fetch LiteLLM pricing JSON, compact it, and rewrite the vendored snapshot",
    },
    TaskInfo {
        name: "ci",
        summary: "Run the full local CI gate stack.",
        runs: "fmt, invariants, audits, build-plugin, lint, coverage, doctest, semver",
    },
];

#[derive(Debug, PartialEq, Eq)]
enum Action<'a> {
    Run(&'a str),
    Help(Option<&'a str>),
}

fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    match parse_args(&args)? {
        Action::Run(task) => {
            let root = workspace_root()?;
            run_task(task, &root)
        }
        Action::Help(None) => {
            print_xtask_help();
            Ok(())
        }
        Action::Help(Some(task)) => print_task_help(task),
    }
}

fn parse_args(args: &[String]) -> Result<Action<'_>> {
    let Some(first) = args.first().map(String::as_str) else {
        return Ok(Action::Run("ci"));
    };

    if is_help_flag(first) {
        if args.len() == 1 {
            return Ok(Action::Help(None));
        }
        bail!("root help takes no arguments");
    }

    if first == "help" {
        return match args {
            [_] => Ok(Action::Help(None)),
            [_, task] => Ok(Action::Help(Some(task.as_str()))),
            _ => bail!("help takes at most one task name"),
        };
    }

    if args.iter().skip(1).any(|arg| is_help_flag(arg)) {
        if args.len() == 2 {
            return Ok(Action::Help(Some(first)));
        }
        bail!("xtask `{first}` help takes no other arguments");
    }

    if args.len() > 1 {
        bail!("xtask `{first}` takes no arguments; run `cargo xtask {first} --help`");
    }

    Ok(Action::Run(first))
}

fn is_help_flag(arg: &str) -> bool {
    matches!(arg, "-h" | "--help")
}

fn run_task(task: &str, root: &Path) -> Result<()> {
    match task {
        "build" => build(root),
        "build-plugin" => build_plugin(root),
        "install" => install(root),
        "stage-install" => stage_install(root).map(|_| ()),
        "fmt" => fmt(root),
        "lint" => lint(root),
        "test" => test(root),
        "doctest" => doctest(root),
        "deny" => deny(root),
        "deps" => deps(root),
        "vet" => vet(root),
        "coverage" => coverage(root),
        "semver" => semver(root),
        "invariants" => invariants(root),
        "pricing-refresh" => pricing_refresh(root),
        "ci" => ci(root),
        other => bail!("unknown xtask `{other}`"),
    }
}

fn task_info(name: &str) -> Option<&'static TaskInfo> {
    TASKS.iter().find(|task| task.name == name)
}

#[expect(
    clippy::print_stdout,
    reason = "xtask help text is the command's stdout contract"
)]
fn print_xtask_help() {
    println!("Contributor task runner");
    println!();
    println!("Usage:");
    println!("  cargo xtask              # run ci");
    println!("  cargo xtask <task>");
    println!("  cargo xtask <task> --help");
    println!();
    println!("Tasks:");
    for task in TASKS {
        println!("  {:<15} {}", task.name, task.summary);
    }
}

#[expect(
    clippy::print_stdout,
    reason = "xtask help text is the command's stdout contract"
)]
fn print_task_help(task: &str) -> Result<()> {
    let Some(info) = task_info(task) else {
        bail!("unknown xtask `{task}`");
    };
    println!("cargo xtask {}", info.name);
    println!();
    println!("{}", info.summary);
    println!();
    println!("Runs:");
    println!("  {}", info.runs);
    Ok(())
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
    build_plugin(root)?;
    let envs = presence_plugin_embed_env(root);
    run_with_env(
        root,
        "cargo",
        ["build", "--workspace", "--all-features", "--locked"],
        &envs,
    )
}

/// Build the Zellij presence plugin for its real target. The host-target
/// workspace build only compiles the crate's pure policy core and a stub bin;
/// this produces the `.wasm` Zellij actually loads.
fn build_plugin(root: &Path) -> Result<()> {
    ensure_rust_target(root, PRESENCE_PLUGIN_TARGET)?;
    run(
        root,
        "cargo",
        [
            "build",
            "-p",
            "rimz-presence-zellij",
            "--target",
            PRESENCE_PLUGIN_TARGET,
            "--release",
            "--locked",
        ],
    )
}

/// The built presence-plugin artifact, honoring a `CARGO_TARGET_DIR` override.
fn plugin_artifact(root: &Path) -> PathBuf {
    target_dir(root)
        .join(PRESENCE_PLUGIN_TARGET)
        .join("release")
        .join("rimz-presence-zellij.wasm")
}

fn ensure_rust_target(root: &Path, target: &str) -> Result<()> {
    if rustup_target_installed(root, target)? || rust_target_std_available(root, target)? {
        return Ok(());
    }

    let status = Command::new("rustup")
        .args(["target", "add", target])
        .current_dir(root)
        .status()
        .with_context(|| {
            format!(
                "Rust target `{target}` is missing and `rustup` could not be run; install it with `rustup target add {target}`"
            )
        })?;
    if !status.success() {
        bail!("Rust target `{target}` is missing and `rustup target add {target}` failed");
    }

    if rustup_target_installed(root, target)? || rust_target_std_available(root, target)? {
        return Ok(());
    }
    bail!("Rust target `{target}` is still unavailable after `rustup target add {target}`");
}

fn rustup_target_installed(root: &Path, target: &str) -> Result<bool> {
    let output = match Command::new("rustup")
        .args(["target", "list", "--installed"])
        .current_dir(root)
        .output()
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err).context("checking installed rustup targets"),
    };
    if !output.status.success() {
        return Ok(false);
    }

    let installed =
        String::from_utf8(output.stdout).context("reading installed rustup targets output")?;
    Ok(target_list_contains(&installed, target))
}

fn target_list_contains(installed: &str, target: &str) -> bool {
    installed.lines().any(|line| line.trim() == target)
}

fn rust_target_std_available(root: &Path, target: &str) -> Result<bool> {
    let output = Command::new("rustc")
        .args(["--print", "target-libdir", "--target", target])
        .current_dir(root)
        .output()
        .with_context(|| format!("checking Rust target `{target}`"))?;
    if !output.status.success() {
        return Ok(false);
    }

    let target_libdir =
        String::from_utf8(output.stdout).context("reading rustc target libdir output")?;
    let target_libdir = PathBuf::from(target_libdir.trim());
    let entries = match fs::read_dir(&target_libdir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(err).with_context(|| format!("reading {}", target_libdir.display()));
        }
    };
    for entry in entries {
        let name = entry
            .with_context(|| format!("reading {}", target_libdir.display()))?
            .file_name();
        if let Some(name) = name.to_str()
            && (name == "libcore.rlib" || (name.starts_with("libcore-") && name.ends_with(".rlib")))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn target_dir(root: &Path) -> PathBuf {
    let dir = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"));
    if dir.is_absolute() {
        dir
    } else {
        root.join(dir)
    }
}

fn release_artifact(root: &Path, bin: &str) -> PathBuf {
    let mut artifact = target_dir(root).join("release").join(bin);
    if !env::consts::EXE_EXTENSION.is_empty() {
        artifact.set_extension(env::consts::EXE_EXTENSION);
    }
    artifact
}

fn stage_bin_dir(root: &Path) -> PathBuf {
    target_dir(root).join("xtask").join("install").join("bin")
}

/// Where a user install lands binaries — the same ladder `cargo install` walks.
fn cargo_install_bin_dir() -> PathBuf {
    if let Some(install_root) = env::var_os("CARGO_INSTALL_ROOT") {
        return PathBuf::from(install_root).join("bin");
    }
    if let Some(cargo_home) = env::var_os("CARGO_HOME") {
        return PathBuf::from(cargo_home).join("bin");
    }
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".cargo")
        .join("bin")
}

fn install(root: &Path) -> Result<()> {
    let stage = stage_install(root)?;
    let dest_dir = cargo_install_bin_dir();
    fs::create_dir_all(&dest_dir).with_context(|| format!("creating {}", dest_dir.display()))?;
    for artifact in INSTALL_ARTIFACTS {
        copy_atomically(&stage.join(artifact), &dest_dir.join(artifact))?;
    }
    Ok(())
}

const INSTALL_ARTIFACTS: [&str; 1] = ["rimz"];

fn stage_install(root: &Path) -> Result<PathBuf> {
    build_plugin(root)?;
    let envs = presence_plugin_embed_env(root);
    run_with_env(
        root,
        "cargo",
        [
            "build",
            "-p",
            "rimz",
            "--bin",
            "rimz",
            "--release",
            "--locked",
        ],
        &envs,
    )?;
    let stage = stage_bin_dir(root);
    fs::create_dir_all(&stage).with_context(|| format!("creating {}", stage.display()))?;
    copy_atomically(&release_artifact(root, "rimz"), &stage.join("rimz"))?;
    Ok(stage)
}

fn presence_plugin_embed_env(root: &Path) -> Vec<(&'static str, PathBuf)> {
    vec![("RIMZ_EMBED_PRESENCE_PLUGIN", plugin_artifact(root))]
}

fn copy_atomically(source: &Path, dest: &Path) -> Result<()> {
    let parent = dest
        .parent()
        .with_context(|| format!("{} has no parent directory", dest.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let file_name = dest
        .file_name()
        .with_context(|| format!("{} has no file name", dest.display()))?
        .to_string_lossy();
    let staged = parent.join(format!(".{file_name}.tmp.{}", process::id()));
    match fs::remove_file(&staged) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err).with_context(|| format!("removing {}", staged.display()));
        }
    }
    fs::copy(source, &staged)
        .with_context(|| format!("staging {} to {}", source.display(), staged.display()))?;
    fs::rename(&staged, dest).with_context(|| format!("installing {}", dest.display()))?;
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
        // The wasm plugin compile is the cheapest compile gate; it fails fast
        // before the host lint/coverage builds are paid for.
        ("build-plugin", build_plugin as Gate),
        ("lint", lint),
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
    run_with_env_removed(
        root,
        "cargo",
        [
            "nextest",
            "run",
            "--workspace",
            "--all-features",
            "--locked",
        ],
        &["NO_COLOR"],
    )
}

// Coverage is the *only* test run in `ci`: `llvm-cov nextest` runs the suite
// under instrumentation, so there is no separate uninstrumented `test` pass to
// build and execute the workspace a second time.
fn coverage(root: &Path) -> Result<()> {
    run_with_env_removed(
        root,
        "cargo",
        [
            "llvm-cov",
            "nextest",
            "--workspace",
            "--all-features",
            "--locked",
        ],
        &["NO_COLOR"],
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
    run_with_env(root, program, args, &[])
}

fn run_with_env<I, S>(root: &Path, program: &str, args: I, envs: &[(&str, PathBuf)]) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_with_env_and_removed(root, program, args, envs, &[])
}

fn run_with_env_removed<I, S>(
    root: &Path,
    program: &str,
    args: I,
    removed_envs: &[&str],
) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_with_env_and_removed(root, program, args, &[], removed_envs)
}

fn run_with_env_and_removed<I, S>(
    root: &Path,
    program: &str,
    args: I,
    envs: &[(&str, PathBuf)],
    removed_envs: &[&str],
) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<_> = args.into_iter().collect();
    let mut command = Command::new(program);
    command
        .args(args.iter().map(AsRef::as_ref))
        .current_dir(root)
        .envs(envs.iter().map(|(key, value)| (*key, value)));
    for key in removed_envs {
        command.env_remove(key);
    }
    let status = command
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
    let outside_sidebar_renderer = |path: &Path| {
        !path.starts_with(root.join("crates/rimz/src/sidebar_renderer"))
            || path.starts_with(root.join("xtask"))
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
            outside_sidebar_renderer,
            "sidebar renderer must not import ledger writer APIs",
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
    // Both the path form (`…::atomic`, catching `crate::ledger::atomic` and
    // `rimz::ledger::atomic` alike) and the usage form (`atomic::…`, catching
    // a grouped `use crate::ledger::{atomic, …}` through its call sites —
    // an unused grouped import already fails the lint gate).
    for needle in [
        concat!("::", "atomic"),
        concat!("atomic", "::"),
        concat!("::", "bridge"),
        concat!("bridge", "::"),
        concat!("::", "broker"),
        concat!("broker", "::"),
    ] {
        ensure_no_match(
            &files,
            needle,
            &outside_spend_parsers,
            "adapter spend parsers are read-only: no ledger writes, bridge, or broker imports",
        )?;
    }

    // The sidebar library tree (the consumer read in `snapshot.rs` and the
    // produce pipeline in `produce/`) is read-only on ledger truth: the rollup
    // arrives through the cursor fold and every write is a cache-class runtime
    // file. Ledger writers, the feed store, the decision bridge, and the Codex
    // broker belong outside it.
    let outside_sidebar_library = {
        let sidebar_root = root.join("crates/rimz/src/sidebar");
        move |path: &Path| !path.starts_with(&sidebar_root)
    };
    for needle in [
        concat!("ledger", "::", "writer"),
        concat!("feed_", "store"),
        concat!("::", "bridge"),
        concat!("bridge", "::"),
        concat!("::", "broker"),
        concat!("broker", "::"),
    ] {
        ensure_no_match(
            &files,
            needle,
            &outside_sidebar_library,
            "crates/rimz/src/sidebar is read-only on the ledger: no writer, feed-store, bridge, or broker imports",
        )?;
    }
    ensure_snapshot_json_writes_stay_in_produce(root, &files)?;
    ensure_sidebar_enrich_folds_before_live_panes(root)?;
    ensure_card_admission_predicate(root)?;

    // Durability barriers live in one file: every fsync syscall goes through
    // `ledger/atomic.rs`, so the write-class contract is auditable in one
    // place and its testkit counter observes every sync.
    for needle in [concat!(".sync_", "all("), concat!(".sync_", "data(")] {
        ensure_no_match(
            &files,
            needle,
            |path: &Path| {
                path.ends_with("crates/rimz/src/ledger/atomic.rs") || is_docs_or_xtask(path)
            },
            "fsync syscalls live in ledger/atomic.rs alone — route through its helpers",
        )?;
    }

    // Participant surfaces — hook entrypoints, event/feed publishers, the
    // statusline sidecars, pane helpers, and the sidebar renderer — resolve
    // identity through the session pin (`resolve_participant`); the
    // create-mode resolver would re-derive identity from cwd and split-brain
    // an agent working inside a nested repo.
    let outside_participants = {
        let cli_root = root.join("crates/rimz/src/cli");
        move |path: &Path| {
            let participant_cli = path.starts_with(&cli_root)
                && matches!(
                    path.file_name().and_then(OsStr::to_str),
                    Some(
                        "hooks.rs"
                            | "event.rs"
                            | "statusline.rs"
                            | "feed.rs"
                            | "pane.rs"
                            | "sidebar.rs"
                    )
                );
            !participant_cli
        }
    };
    ensure_no_match(
        &files,
        concat!("WorkspaceResolver::", "resolve("),
        &outside_participants,
        "participant surfaces resolve identity through the session pin — use resolve_participant",
    )?;

    ensure_no_core_pane_auto_use(root, &files)?;
    ensure_inline_tests_stay_small(&files)?;
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

fn ensure_sidebar_enrich_folds_before_live_panes(root: &Path) -> Result<()> {
    let path = root.join("crates/rimz/src/sidebar/enrich.rs");
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let Some(live_fold) = text.find("with_live_panes(") else {
        bail!("sidebar enrich spine must fold a live pane frame through with_live_panes");
    };
    let after_live = &text[live_fold..];
    let mut violations = Vec::new();
    for needle in [
        ".with_project_root(",
        ".with_worktree_roots(",
        ".with_root_class(",
        ".with_agent_context(",
        ".with_subagent_context(",
        ".with_agent_activity(",
        ".drop_dead_agents_with(",
        ".drop_dead_daemon_sessions(",
        ".reap_stale_sessions(",
    ] {
        if let Some(offset) = after_live.find(needle) {
            let line = text[..live_fold + offset]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count()
                + 1;
            violations.push(format!("{}:{}: {}", path.display(), line, needle));
        }
    }
    if violations.is_empty() {
        return Ok(());
    }
    bail!(
        "sidebar enrich rollup/context/liveness folds must stay before with_live_panes\n{}",
        violations.join("\n")
    );
}

fn ensure_card_admission_predicate(root: &Path) -> Result<()> {
    let path = root.join("crates/rimz/src/ledger/snapshot/view.rs");
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let Some(live_fold) = text.find("pub fn with_live_panes(") else {
        bail!("sidebar live-pane fold must stay in view.rs");
    };
    let after = &text[live_fold..];
    let Some(groups_fold) = after.find("self.worktree_groups =") else {
        bail!("with_live_panes must build worktree groups after card admission");
    };
    let body = &after[..groups_fold];
    if !body.contains("pane_admits_card(pane, exclude).admits()") {
        bail!("with_live_panes must filter rows through pane_admits_card");
    }
    let mut violations = Vec::new();
    for needle in [
        "command_is_sidebar_chrome",
        "pane_is_host",
        "pane.pane_id !=",
        "pane.pane_id ==",
    ] {
        if let Some(offset) = body.find(needle) {
            let line = text[..live_fold + offset]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count()
                + 1;
            violations.push(format!("{}:{}: {}", path.display(), line, needle));
        }
    }
    if violations.is_empty() {
        return Ok(());
    }
    bail!(
        "with_live_panes card-admission filtering must stay behind pane_admits_card\n{}",
        violations.join("\n")
    );
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

fn ensure_snapshot_json_writes_stay_in_produce(root: &Path, files: &[PathBuf]) -> Result<()> {
    let producer_root = root.join("crates/rimz/src/sidebar/produce");
    let source_root = root.join("crates/rimz/src");
    let snapshot_file = concat!("snapshot", ".json");
    let write_helper = concat!("write_temp_then_", "rename_cache");
    let mut violations = Vec::new();

    for path in files {
        if path.extension().and_then(OsStr::to_str) != Some("rs")
            || !path.starts_with(&source_root)
            || path.starts_with(&producer_root)
            || path.file_name().and_then(OsStr::to_str) == Some("tests.rs")
        {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for block in function_blocks(&text) {
            if block.body.contains(snapshot_file)
                && (block.body.contains(write_helper)
                    || block.body.contains("std::fs::write")
                    || block.body.contains("fs::write"))
            {
                violations.push(format!(
                    "{}:{}: {}",
                    path.display(),
                    block.line,
                    block.signature.trim()
                ));
            }
        }
    }

    if violations.is_empty() {
        return Ok(());
    }
    bail!(
        "published pane-frame writes belong in sidebar::produce; realtime events must not patch snapshot.json\n{}",
        violations.join("\n")
    );
}

struct FunctionBlock<'a> {
    line: usize,
    signature: &'a str,
    body: String,
}

fn function_blocks(text: &str) -> Vec<FunctionBlock<'_>> {
    let mut blocks = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut idx = 0;
    while idx < lines.len() {
        let line = lines[idx];
        if !line.contains("fn ") {
            idx += 1;
            continue;
        }

        let start = idx;
        let mut body = String::new();
        let mut depth = 0_i32;
        let mut saw_open = false;
        while idx < lines.len() {
            let current = lines[idx];
            body.push_str(current);
            body.push('\n');
            for ch in current.chars() {
                match ch {
                    '{' => {
                        depth += 1;
                        saw_open = true;
                    }
                    '}' if saw_open => depth -= 1,
                    _ => {}
                }
            }
            idx += 1;
            if saw_open && depth <= 0 {
                break;
            }
        }
        blocks.push(FunctionBlock {
            line: start + 1,
            signature: line,
            body,
        });
    }
    blocks
}

/// An inline `mod tests { … }` past this many lines moves to a sibling
/// `tests.rs` (`#[cfg(test)] mod tests;`) per
/// docs/contributing/rust-conventions.md#tests.
const INLINE_TESTS_MAX_LINES: usize = 500;

fn ensure_inline_tests_stay_small(files: &[PathBuf]) -> Result<()> {
    let mut violations = Vec::new();
    for path in files {
        if path.extension().and_then(OsStr::to_str) != Some("rs") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        // Exact-line match so needles in strings never self-trip; the house
        // shape keeps the tests module last, so its span runs to EOF.
        let Some(start) = lines.iter().position(|line| *line == "mod tests {") else {
            continue;
        };
        let span = lines.len() - start;
        if span > INLINE_TESTS_MAX_LINES {
            violations.push(format!(
                "{}:{}: inline tests module spans {span} lines",
                path.display(),
                start + 1,
            ));
        }
    }
    if violations.is_empty() {
        return Ok(());
    }
    bail!(
        "inline tests modules past {INLINE_TESTS_MAX_LINES} lines move to a sibling tests.rs — see docs/contributing/rust-conventions.md#tests\n{}",
        violations.join("\n")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn no_args_default_to_ci() {
        assert_eq!(parse_args(&args(&[])).unwrap(), Action::Run("ci"));
    }

    #[test]
    fn root_help_is_first_class() {
        assert_eq!(parse_args(&args(&["--help"])).unwrap(), Action::Help(None));
        assert_eq!(parse_args(&args(&["-h"])).unwrap(), Action::Help(None));
        assert_eq!(parse_args(&args(&["help"])).unwrap(), Action::Help(None));
    }

    #[test]
    fn task_help_does_not_run_the_task() {
        assert_eq!(
            parse_args(&args(&["test", "--help"])).unwrap(),
            Action::Help(Some("test")),
        );
        assert_eq!(
            parse_args(&args(&["help", "test"])).unwrap(),
            Action::Help(Some("test")),
        );
    }

    #[test]
    fn unexpected_task_args_fail_instead_of_being_ignored() {
        let err = parse_args(&args(&["test", "--package", "rimz"]))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("xtask `test` takes no arguments"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rustup_target_list_match_is_exact() {
        let installed = "wasm32-unknown-unknown\nwasm32-wasip1\n";

        assert!(target_list_contains(installed, "wasm32-wasip1"));
        assert!(!target_list_contains(installed, "wasm32-wasi"));
    }
}
