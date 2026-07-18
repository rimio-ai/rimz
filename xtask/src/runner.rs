use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;

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

pub(crate) fn run_with_env_and_removed<I, S>(
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

pub(crate) fn run_streamed<I, S>(
    root: &Path,
    program: &str,
    args: I,
    envs: &[(&str, PathBuf)],
    removed_envs: &[&str],
    on_line: &mut dyn FnMut(&str),
) -> Result<Captured>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<_> = args.into_iter().collect();
    let mut child = build_command(root, program, &args, envs, removed_envs)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("running `{program}`"))?;
    let stdout = child.stdout.take().context("capturing command stdout")?;
    let stderr = child.stderr.take().context("capturing command stderr")?;
    let stdout_worker = thread::spawn(move || {
        let mut output = Vec::new();
        let mut stdout = stdout;
        stdout.read_to_end(&mut output).map(|_| output)
    });

    let stderr_result = capture_lines_lossy(BufReader::new(stderr), on_line);
    let status = child.wait().context("waiting for command")?;
    let stdout = stdout_worker
        .join()
        .map_err(|_| anyhow::anyhow!("command stdout reader panicked"))?
        .context("reading command stdout")?;
    let stderr_output = stderr_result.context("reading command stderr")?;

    let mut combined = String::from_utf8_lossy(&stdout).into_owned();
    combined.push_str(&stderr_output);
    Ok(Captured {
        status,
        output: combined,
    })
}

fn capture_lines_lossy(
    mut reader: impl BufRead,
    on_line: &mut dyn FnMut(&str),
) -> std::io::Result<String> {
    let mut output = String::new();
    let mut bytes = Vec::new();
    while reader.read_until(b'\n', &mut bytes)? != 0 {
        let line = String::from_utf8_lossy(&bytes);
        on_line(line.trim_end_matches(['\r', '\n']));
        output.push_str(&line);
        bytes.clear();
    }
    Ok(output)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streamed_capture_preserves_lines_with_lossy_utf8() {
        let mut lines = Vec::new();
        let output = capture_lines_lossy(
            std::io::Cursor::new(b"first\xff line\r\nsecond line"),
            &mut |line| lines.push(line.to_owned()),
        )
        .unwrap();

        assert_eq!(lines, ["first� line", "second line"]);
        assert_eq!(output, "first� line\r\nsecond line");
    }
}
