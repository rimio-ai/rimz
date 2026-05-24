#![deny(clippy::print_stdout)]
#![deny(clippy::print_stderr)]

use std::env;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use anyhow::{Context, Result, bail};

fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let task = args.next().unwrap_or_else(|| "ci".to_owned());
    let root = workspace_root()?;
    match task.as_str() {
        "fmt" => run(&root, "cargo", ["fmt", "--all", "--", "--check"]),
        "lint" => run(
            &root,
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
        ),
        "test" => test(&root),
        "doctest" => run(&root, "cargo", ["test", "--workspace", "--doc", "--locked"]),
        "deny" => run(&root, "cargo", ["deny", "check", "-D", "warnings"]),
        "deps" => deps(&root),
        "vet" => run(&root, "cargo", ["vet"]),
        "coverage" => run(
            &root,
            "cargo",
            ["llvm-cov", "--workspace", "--all-features"],
        ),
        "semver" => run(&root, "cargo", ["semver-checks"]),
        "invariants" => invariants(&root),
        "ci" => ci(&root),
        other => bail!("unknown xtask `{other}`"),
    }
}

fn ci(root: &Path) -> Result<()> {
    for task in [
        "fmt",
        "lint",
        "test",
        "doctest",
        "deny",
        "deps",
        "vet",
        "coverage",
        "semver",
        "invariants",
    ] {
        run_self(root, task)?;
    }
    Ok(())
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
    if cargo_subcommand_available("nextest") {
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
    } else {
        run(
            root,
            "cargo",
            ["test", "--workspace", "--all-features", "--locked"],
        )
    }
}

fn run_self(root: &Path, task: &str) -> Result<()> {
    run(root, "cargo", ["xtask", task])
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

fn cargo_subcommand_available(name: &str) -> bool {
    Command::new("cargo")
        .args([name, "--version"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
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

    for needle in ["rimz::ledger::atomic", "crate::ledger::atomic"] {
        ensure_no_match(
            &files,
            needle,
            outside_sidebar,
            "sidebar crate must not import ledger writer APIs",
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
