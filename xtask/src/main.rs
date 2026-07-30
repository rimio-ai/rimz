//! Contributor task runner — `cargo xtask <task>`; CI can archive nextest binaries once and run them without recompilation.

#![deny(clippy::print_stdout)]
#![deny(clippy::print_stderr)]

mod atlas;
mod brew;
mod build;
mod deadline;
mod docs_links;
mod files;
mod gates;
mod hooks;
mod invariants;
mod pricing;
mod rtk;
mod runner;
mod sandbox;
mod sccache;
mod screenshot;
mod source_files;
mod spinner;
mod theme;

#[cfg(test)]
mod tests;

use std::env;
use std::path::Path;

use anyhow::{Result, bail};

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
        name: "plugin-refresh",
        summary: "Rebuild and vendor the Zellij presence plugin wasm for crates.io.",
        runs: "build-plugin, then copy the wasm and write its source, blob, and rustc provenance into crates/rimz/presence/",
    },
    TaskInfo {
        name: "install",
        summary: "Build and install the host rimz binary.",
        runs: "cargo xtask stage-install, then atomically installs host rimz to ~/.cargo/bin",
    },
    TaskInfo {
        name: "install-dev",
        summary: "Build and install host rimz with off-box reporting (sentry) for dev.",
        runs: "build-plugin, host rimz with --profile profiling --features sentry and profiling RUSTFLAGS, atomically installs to ~/.cargo/bin, then best-effort uploads debug files with sentry-cli",
    },
    TaskInfo {
        name: "profile-build",
        summary: "Build an optimized host rimz for perf/samply profiling.",
        runs: "build-plugin, then host rimz with --profile profiling and profiling RUSTFLAGS; writes target/profiling/rimz",
    },
    TaskInfo {
        name: "stage-install",
        summary: "Build host install artifacts.",
        runs: "build-plugin, host rimz release",
    },
    TaskInfo {
        name: "dist",
        summary: "Build packaged release archives into target/dist.",
        runs: "build-plugin, host release binary when non-Darwin, cargo zigbuild for both apple-darwin targets, tar.gz + SHA256SUMS",
    },
    TaskInfo {
        name: "brew-formula",
        summary: "Render the Homebrew tap formula from the dist checksums.",
        runs: "read target/dist/SHA256SUMS plus the RIMZ_BREW_* env, write rimz.rb atomically",
    },
    TaskInfo {
        name: "hooks",
        summary: "Install the tracked git hooks (pre-commit fmt gate).",
        runs: "git config core.hooksPath .githooks",
    },
    TaskInfo {
        name: "fmt",
        summary: "Check Rust formatting.",
        runs: "cargo fmt --all -- --check",
    },
    TaskInfo {
        name: "lint",
        summary: "Run all-feature and install-host clippy with warnings as errors.",
        runs: "cargo clippy for all workspace targets and both host install feature shapes without testkit; every pass denies warnings",
    },
    TaskInfo {
        name: "check",
        summary: "Run a fast structural compile check across the workspace.",
        runs: "cargo check --workspace --all-targets --all-features --locked",
    },
    TaskInfo {
        name: "test",
        summary: "Run nextest filters, batch exact --name selections, or discover tests with --list.",
        runs: "cargo nextest run --workspace --all-features --locked [--name <test>]... [nextest filter]...; --list uses cargo nextest list",
    },
    TaskInfo {
        name: "test-archive",
        summary: "Compile and archive workspace nextest binaries.",
        runs: "cargo nextest archive --workspace --all-features --locked --archive-file <path>",
    },
    TaskInfo {
        name: "sandbox",
        summary: "Run a command with disposable HOME, XDG, tmux, and Zellij roots.",
        runs: "the supplied command with host state isolated under a temporary directory; tears down sandbox mux servers on exit",
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
        runs: "scheduled instrumented suite: cargo llvm-cov nextest --lcov --output-path target/ci/coverage/lcov.info",
    },
    TaskInfo {
        name: "semver",
        summary: "Report Rust API drift against the published crate (advisory; gates nothing).",
        runs: "cargo semver-checks",
    },
    TaskInfo {
        name: "externals",
        summary: "Run supply-chain checks that reach crates.io directly.",
        runs: "cargo deny check, then cargo vet",
    },
    TaskInfo {
        name: "perf",
        summary: "Run the divan performance benchmarks.",
        runs: "cargo bench -p rimz --features testkit --locked",
    },
    TaskInfo {
        name: "atlas",
        summary: "Measure and ratchet architecture refactor programs.",
        runs: atlas::USAGE,
    },
    TaskInfo {
        name: "invariants",
        summary: "Run repository architecture invariants.",
        runs: "grep-style invariants implemented in xtask",
    },
    TaskInfo {
        name: "docs-links",
        summary: "Check documentation links and #anchors resolve.",
        runs: "resolve every relative markdown link target and #anchor against the working tree",
    },
    TaskInfo {
        name: "pricing-refresh",
        summary: "Refresh the generated pricing snapshot.",
        runs: "cargo run -p rimz -- pricing-refresh, optionally with --check",
    },
    TaskInfo {
        name: "theme-refresh",
        summary: "Refresh the bundled Alacritty theme catalog.",
        runs: "fetch the iTerm2-Color-Schemes Alacritty catalog, rewrite vendored TOML plus attribution atomically",
    },
    TaskInfo {
        name: "screenshot",
        summary: "Render sidebar ANSI captures to PNG with freeze.",
        runs: "list, live, pane <id>, or state <empty|fleet|provider>",
    },
    TaskInfo {
        name: "gate",
        summary: "Run the fast pre-PR gate stack; --check verifies formatting instead of applying it.",
        runs: "fmt --all (fix, or check-only under --check), invariants, docs-links, all-feature + install-host lint, test (nextest -P gate)",
    },
    TaskInfo {
        name: "checks",
        summary: "Run the non-test CI gate stack.",
        runs: "fmt, invariants, docs-links, deps, build-plugin, plugin-provenance, lint",
    },
    TaskInfo {
        name: "ci",
        summary: "Run the full local CI gate stack.",
        runs: "checks, then plain nextest over the workspace",
    },
];

#[derive(Debug, PartialEq, Eq)]
enum Action<'a> {
    Run { task: &'a str, args: &'a [String] },
    Help(Option<&'a str>),
}

fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    if sandbox::run_reaper_mode(&args)? {
        return Ok(());
    }
    match parse_args(&args)? {
        Action::Run { task, args } => {
            let root = runner::workspace_root()?;
            deadline::arm(task)?;
            run_task(task, args, &root)
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
        return Ok(Action::Run {
            task: "ci",
            args: &[],
        });
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
        if task_accepts_args(first) {
            return Ok(Action::Run {
                task: first,
                args: &args[1..],
            });
        }
        bail!("xtask `{first}` help takes no other arguments");
    }

    if args.len() > 1 && !task_accepts_args(first) {
        bail!("xtask `{first}` takes no arguments; run `cargo xtask {first} --help`");
    }

    Ok(Action::Run {
        task: first,
        args: &args[1..],
    })
}

pub(crate) fn is_help_flag(arg: &str) -> bool {
    matches!(arg, "-h" | "--help")
}

fn task_accepts_args(task: &str) -> bool {
    matches!(
        task,
        "test"
            | "test-archive"
            | "sandbox"
            | "perf"
            | "atlas"
            | "pricing-refresh"
            | "screenshot"
            | "gate"
    )
}

/// Verification tasks that produce no output of their own when they pass. Run
/// standalone, an empty success is indistinguishable from a crash, so each one
/// gets the same `✓ <task>` line the composite gate stack prints per step.
const QUIET_PASS_TASKS: &[&str] = &[
    "fmt",
    "lint",
    "invariants",
    "docs-links",
    "deps",
    "deny",
    "vet",
];

fn run_task(task: &str, args: &[String], root: &Path) -> Result<()> {
    let result = dispatch(task, args, root);
    if result.is_ok() && QUIET_PASS_TASKS.contains(&task) {
        gates::report_task_pass(task);
    }
    result
}

fn dispatch(task: &str, args: &[String], root: &Path) -> Result<()> {
    match task {
        "build" => build::build(root),
        "build-plugin" => build::build_plugin(root),
        "plugin-refresh" => build::plugin_refresh(root),
        "install" => build::install(root),
        "install-dev" => build::install_dev(root),
        "profile-build" => build::profile_build(root),
        "stage-install" => build::stage_install(root).map(|_| ()),
        "dist" => build::dist(root),
        "brew-formula" => brew::brew_formula(root),
        "hooks" => hooks::install(root),
        "fmt" => gates::fmt(root),
        "lint" => gates::lint(root),
        "check" => gates::check(root),
        "test" => gates::test(root, args),
        "test-archive" => gates::test_archive(root, args),
        "sandbox" => sandbox::run(root, args),
        "deny" => gates::deny(root),
        "deps" => gates::deps(root),
        "vet" => gates::vet(root),
        "coverage" => gates::coverage(root),
        "semver" => gates::semver(root),
        "externals" => gates::externals(root),
        "perf" => gates::perf(root, args),
        "atlas" => atlas::atlas(root, args),
        "invariants" => invariants::invariants(root),
        "docs-links" => docs_links::docs_links(root),
        "gate" => gates::gate(root, args),
        "checks" => gates::checks(root),
        "pricing-refresh" => pricing::pricing_refresh(root, args),
        "theme-refresh" => theme::theme_refresh(root),
        "screenshot" => screenshot::screenshot(root, args),
        "ci" => gates::ci(root),
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
    println!("Every task runs under a wall-clock budget (15m; 45m for full");
    println!("compile-and-suite passes). Set RIMZ_XTASK_TIMEOUT=45m or =off to");
    println!("change it for one run.");
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
