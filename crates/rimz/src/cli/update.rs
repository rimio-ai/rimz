//! `rimz update` — update through the active install origin and converge live
//! RimZ surfaces onto the new build.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use clap::Args;

use super::spinner::Spinner;
use super::{GlobalFlags, render};
use rimz::build_id::VERSION;
use rimz::update::{self, InstallOrigin};

#[derive(Debug, Args)]
pub struct UpdateArgs {
    /// Install a release tag such as `v0.3.1`, or the rolling `latest-main` build.
    #[arg(long = "version", value_name = "TAG")]
    version: Option<String>,
}

pub fn run(args: UpdateArgs, _globals: &GlobalFlags) -> Result<()> {
    let reported_exe = std::env::current_exe().context("resolving the running RimZ binary")?;
    let resolved_exe = rimz::proc::resolve_existing_or_replacement(&reported_exe)
        .context("the running RimZ binary has been removed; reinstall RimZ and retry")?;
    let canonical_exe = resolved_exe
        .canonicalize()
        .with_context(|| format!("resolving RimZ binary {}", resolved_exe.display()))?;
    let before_id = rimz::build_id::of_file(&canonical_exe)
        .with_context(|| format!("reading RimZ binary {}", canonical_exe.display()))?;
    let origin = update::detect_origin(&canonical_exe, update::cargo_bin_dir().as_deref());

    let active_exe = match origin {
        InstallOrigin::Homebrew => update_homebrew(args.version.as_deref(), &reported_exe)?,
        InstallOrigin::Cargo => {
            update_cargo(args.version.as_deref())?;
            canonical_exe.clone()
        }
        InstallOrigin::Standalone => {
            update_standalone(args.version.as_deref(), &canonical_exe, &before_id)?;
            canonical_exe.clone()
        }
    };

    let after_id = rimz::build_id::of_file(&active_exe)
        .with_context(|| format!("reading updated RimZ binary {}", active_exe.display()))?;
    if before_id != after_id {
        writeln!(render::out(), "Reloading running RimZ surfaces…")?;
        let status = Command::new(&active_exe)
            .arg("reload")
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .with_context(|| format!("starting {} reload", active_exe.display()))?;
        if !status.success() {
            bail!(
                "RimZ updated, but `{}` reload exited with {status}; rerun `rimz reload`",
                active_exe.display()
            );
        }
    }
    Ok(())
}

fn update_homebrew(version: Option<&str>, reported_exe: &Path) -> Result<PathBuf> {
    if let Some(tag) = version {
        bail!(
            "Homebrew cannot pin RimZ to `{tag}`; reinstall the standalone build with `RIMZ_VERSION={tag}` or let `brew upgrade rimz` select the release"
        );
    }
    writeln!(render::out(), "Updating RimZ with Homebrew…")?;
    run_inherited(
        Command::new("brew").args(["upgrade", "rimz"]),
        "brew upgrade rimz",
    )?;

    Ok(homebrew_prefix_binary().unwrap_or_else(|| reported_exe.to_path_buf()))
}

fn homebrew_prefix_binary() -> Option<PathBuf> {
    let output = Command::new("brew")
        .args(["--prefix", "rimz"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let prefix = String::from_utf8(output.stdout).ok()?;
    let binary = PathBuf::from(prefix.trim()).join("bin/rimz");
    binary.is_file().then_some(binary)
}

fn update_cargo(tag: Option<&str>) -> Result<()> {
    let cargo_version = tag.map(update::cargo_version_for_tag).transpose()?;
    if VERSION.contains("+g") {
        writeln!(
            render::out(),
            "Warning: Cargo will replace source build {} with the crates.io release.",
            VERSION
        )?;
    }
    writeln!(render::out(), "Updating RimZ with Cargo…")?;
    let mut command = Command::new("cargo");
    command.args(["install", "--locked", "rimz"]);
    if let Some(cargo_version) = cargo_version {
        command.args(["--version", &cargo_version]);
    }
    run_inherited(&mut command, "cargo install --locked rimz")
}

fn update_standalone(tag: Option<&str>, current_exe: &Path, before_id: &str) -> Result<()> {
    let target_tag = match tag {
        Some(tag) => tag.to_owned(),
        None => {
            let spinner = Spinner::new("checking the latest RimZ release…");
            let tag = update::resolve_latest_tag()?;
            drop(spinner);
            tag
        }
    };
    if update::is_current(VERSION, &target_tag) {
        writeln!(render::out(), "rimz {VERSION} is up to date.")?;
        return Ok(());
    }
    let archive = update::release_archive().ok_or(update::UpdateError::UnsupportedTarget)?;
    let spinner = Spinner::new(format!("downloading RimZ {target_tag}…"));
    let release = update::download_release(&target_tag, archive)?;
    spinner.set(format!("verifying {archive}…"));
    release.verify()?;
    spinner.set(format!("extracting {archive}…"));
    let staged = release.extract()?;
    spinner.set("smoke-testing staged RimZ…");
    let new_version = update::smoke_test(&staged)?;
    let staged_id = rimz::build_id::of_file(&staged).context("reading staged RimZ build id")?;
    drop(spinner);

    if staged_id == before_id {
        writeln!(render::out(), "rimz {new_version} is already up to date.")?;
        return Ok(());
    }
    update::install_over(&staged, current_exe)?;
    writeln!(
        render::out(),
        "Updated rimz {} → {} at {}",
        VERSION,
        new_version,
        current_exe.display()
    )?;
    Ok(())
}

fn run_inherited(command: &mut Command, display: &str) -> Result<()> {
    let status = command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("starting `{display}`; install its package manager and retry"))?;
    if !status.success() {
        bail!("`{display}` exited with {status}; fix the package-manager error and retry");
    }
    Ok(())
}
