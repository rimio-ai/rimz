//! Contributor task runner — `cargo xtask <task>`; `ci` composes the full quality-gate stack.

#![deny(clippy::print_stdout)]
#![deny(clippy::print_stderr)]

mod brew;
mod build;
mod docs_links;
mod files;
mod gates;
mod hooks;
mod invariants;
mod pricing;
mod rtk;
mod runner;
mod screenshot;
mod source_files;
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
        name: "install",
        summary: "Build and install the host rimz binary.",
        runs: "cargo xtask stage-install, then atomically installs host rimz",
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
        summary: "Run clippy with warnings as errors.",
        runs: "cargo clippy --workspace --all-targets --all-features --locked -- -D warnings",
    },
    TaskInfo {
        name: "test",
        summary: "Run the workspace test suite through nextest; accepts nextest filters.",
        runs: "cargo nextest run --workspace --all-features --locked [nextest filter]...",
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
        name: "perf",
        summary: "Run the divan performance benchmarks.",
        runs: "cargo bench -p rimz --features testkit --locked",
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
        runs: "fetch LiteLLM pricing JSON plus authoritative models.dev fillers, compact them, and rewrite the ignored snapshot atomically",
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
        name: "ci",
        summary: "Run the full local CI gate stack.",
        runs: "fmt, invariants, docs-links, audits, build-plugin, lint, doctest, coverage, semver",
    },
];

#[derive(Debug, PartialEq, Eq)]
enum Action<'a> {
    Run { task: &'a str, args: &'a [String] },
    Help(Option<&'a str>),
}

fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    match parse_args(&args)? {
        Action::Run { task, args } => {
            let root = runner::workspace_root()?;
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
    matches!(task, "test" | "perf" | "screenshot")
}

fn run_task(task: &str, args: &[String], root: &Path) -> Result<()> {
    match task {
        "build" => build::build(root),
        "build-plugin" => build::build_plugin(root),
        "install" => build::install(root),
        "stage-install" => build::stage_install(root).map(|_| ()),
        "dist" => build::dist(root),
        "brew-formula" => brew::brew_formula(root),
        "hooks" => hooks::install(root),
        "fmt" => gates::fmt(root),
        "lint" => gates::lint(root),
        "test" => gates::test(root, args),
        "doctest" => gates::doctest(root),
        "deny" => gates::deny(root),
        "deps" => gates::deps(root),
        "vet" => gates::vet(root),
        "coverage" => gates::coverage(root),
        "semver" => gates::semver(root),
        "perf" => gates::perf(root, args),
        "invariants" => invariants::invariants(root),
        "docs-links" => docs_links::docs_links(root),
        "pricing-refresh" => pricing::pricing_refresh(root),
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
