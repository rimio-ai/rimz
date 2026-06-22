use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use anyhow::{Context, Result, bail};

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
    let mut command = if crate::rtk::wrap_cargo(program, &args) {
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
    for key in removed_envs {
        command.env_remove(key);
    }
    let status = command
        .status()
        .with_context(|| format!("running `{program}`"))?;
    ensure_success(program, &args, status)
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
