use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use anyhow::{Context, Result, bail};

pub(crate) struct Captured {
    pub(crate) status: ExitStatus,
    pub(crate) output: String,
}

pub(crate) fn run<I, S>(root: &Path, program: &str, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_with_env(root, program, args, &[])
}

pub(crate) fn run_with_env<I, S>(
    root: &Path,
    program: &str,
    args: I,
    envs: &[(&str, PathBuf)],
) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_with_env_and_removed(root, program, args, envs, &[])
}

pub(crate) fn run_with_env_removed<I, S>(
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
    let mut command = build_command(root, program, &args, envs, removed_envs);
    let status = command
        .status()
        .with_context(|| format!("running `{program}`"))?;
    ensure_success(program, &args, status)
}

pub(crate) fn run_captured<I, S>(
    root: &Path,
    program: &str,
    args: I,
    removed_envs: &[&str],
) -> Result<Captured>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<_> = args.into_iter().collect();
    let output = build_command(root, program, &args, &[], removed_envs)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("running `{program}`"))?;
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(Captured {
        status: output.status,
        output: combined,
    })
}

fn build_command<S: AsRef<OsStr>>(
    root: &Path,
    program: &str,
    args: &[S],
    envs: &[(&str, PathBuf)],
    removed_envs: &[&str],
) -> Command {
    let mut command = if crate::rtk::wrap_cargo(program, args) {
        let mut command = Command::new("rtk");
        command.arg(program);
        command
    } else {
        Command::new(program)
    };
    command
        .args(args.iter().map(AsRef::as_ref))
        .current_dir(root)
        .envs(envs.iter().map(|(key, value)| (*key, value)));
    if crate::sccache::should_wrap(program, args) {
        command.env("RUSTC_WRAPPER", "sccache");
        command.env("CARGO_INCREMENTAL", "0");
    }
    for key in removed_envs {
        command.env_remove(key);
    }
    command
}

pub(crate) fn ensure_success<S: AsRef<OsStr>>(
    program: &str,
    args: &[S],
    status: ExitStatus,
) -> Result<()> {
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

pub(crate) fn workspace_root() -> Result<PathBuf> {
    let mut dir = env::current_dir().context("reading current directory")?;
    loop {
        let manifest = dir.join("Cargo.toml");
        if manifest.is_file() && manifest_declares_workspace(&manifest)? {
            return Ok(dir);
        }
        if !dir.pop() {
            bail!("could not find workspace root from current directory");
        }
    }
}

fn manifest_declares_workspace(manifest: &Path) -> Result<bool> {
    let raw =
        fs::read_to_string(manifest).with_context(|| format!("reading {}", manifest.display()))?;
    let parsed = toml::from_str::<toml::Value>(&raw)
        .with_context(|| format!("parsing {}", manifest.display()))?;
    Ok(parsed.get("workspace").is_some())
}
