//! Run a command in a held room without owning its lifecycle.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::{RoomRecord, apply_env, sandbox_env, validate_sandbox_root};

pub(super) fn run(workspace: &Path, args: &[String]) -> Result<()> {
    let mut command = joined_command(workspace, args)?;
    let status = command.status().with_context(|| {
        format!(
            "running sandboxed command `{}`",
            command.get_program().to_string_lossy()
        )
    })?;
    if !status.success() {
        bail!(
            "sandboxed command `{}` exited with {status}",
            command.get_program().to_string_lossy()
        );
    }
    Ok(())
}

fn joined_command(workspace: &Path, args: &[String]) -> Result<Command> {
    let Some((root, args)) = args.split_first() else {
        bail!("usage: cargo xtask sandbox in <root> [--cwd <dir>] [--] <command>…");
    };
    let root = Path::new(root);
    validate_sandbox_root(root)
        .context("use the sandbox root printed by cargo xtask sandbox room --mux <backend>")?;
    let record = RoomRecord::read(root).context(
        "no readable room record; create a held room with cargo xtask sandbox room --mux <backend>",
    )?;
    room_command(workspace, root, &record, args)
}

fn room_command(
    workspace: &Path,
    root: &Path,
    record: &RoomRecord,
    mut args: &[String],
) -> Result<Command> {
    let cwd = if args.first().is_some_and(|arg| arg == "--cwd") {
        let Some(dir) = args.get(1) else {
            bail!("--cwd requires a directory");
        };
        args = &args[2..];
        Path::new(dir)
    } else {
        &record.worktree
    };
    if args.first().is_some_and(|arg| arg == "--") {
        args = &args[1..];
    }
    let Some((program, program_args)) = args.split_first() else {
        bail!("sandbox in requires a command after the root and optional --cwd <dir>");
    };
    let program = Path::new(program);
    let program = if program.is_relative() && program.components().count() > 1 {
        workspace.join(program)
    } else {
        program.to_path_buf()
    };
    let mut command = Command::new(program);
    command.args(program_args).current_dir(cwd);
    apply_env(&mut command, &sandbox_env(root), true);
    let inherited_path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(record.stub_dir.clone()).chain(std::env::split_paths(&inherited_path)),
    )
    .context("prepending the room stub directory to PATH")?;
    command.env("PATH", path);
    Ok(command)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsStr;

    use super::*;
    use crate::sandbox::session_key;

    #[test]
    fn join_refuses_roots_outside_generated_shape() {
        let err =
            joined_command(Path::new("/workspace"), &["/tmp".into(), "true".into()]).unwrap_err();
        assert!(format!("{err:#}").contains("outside /tmp/rimz-sandbox-"));
        assert!(err.to_string().contains("cargo xtask sandbox room"));
    }

    #[test]
    fn join_refuses_missing_room_record() {
        let err = joined_command(
            Path::new("/workspace"),
            &["/tmp/rimz-sandbox-000000".into(), "true".into()],
        )
        .unwrap_err();
        assert!(err.to_string().contains("no readable room record"));
        assert!(
            err.to_string()
                .contains("cargo xtask sandbox room --mux <backend>")
        );
    }

    #[test]
    fn room_record_round_trips() {
        let root = tempfile::tempdir().unwrap();
        let record = RoomRecord {
            mux: "tmux".into(),
            session: "rimz-room-test".into(),
            worktree: root.path().join("worktree"),
            repo: root.path().join("repo"),
            stub_dir: root.path().join("bin"),
        };
        RoomRecord::write(root.path(), &record).unwrap();
        let decoded = RoomRecord::read(root.path()).unwrap();
        assert_eq!(
            serde_json::to_value(&record).unwrap(),
            serde_json::to_value(decoded).unwrap()
        );
    }

    #[test]
    fn joined_command_uses_room_environment_and_worktree() {
        let root = Path::new("/tmp/rimz-sandbox-aB123z");
        let record = RoomRecord {
            mux: "tmux".into(),
            session: "rimz-room-test".into(),
            worktree: root.join("worktree"),
            repo: root.join("repo"),
            stub_dir: root.join("bin"),
        };
        let command = room_command(
            Path::new("/workspace"),
            root,
            &record,
            &[
                "--".into(),
                "target/debug/rimz".into(),
                "pane".into(),
                "list".into(),
            ],
        )
        .unwrap();
        assert_eq!(command.get_program(), "/workspace/target/debug/rimz");
        assert_eq!(command.get_current_dir(), Some(record.worktree.as_path()));
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["pane", "list"]);
        let env: BTreeMap<_, _> = command.get_envs().collect();
        let inherited_path = std::env::var_os("PATH").unwrap_or_default();
        let paths: Vec<_> = std::env::split_paths(env[OsStr::new("PATH")].unwrap()).collect();
        assert_eq!(paths[0], record.stub_dir);
        assert_eq!(
            paths[1..],
            std::env::split_paths(&inherited_path).collect::<Vec<_>>()
        );
        let sandbox = sandbox_env(root);
        for (key, path) in &sandbox {
            assert_eq!(env[OsStr::new(key)], Some(path.as_os_str()));
        }
        for (key, _) in std::env::vars_os() {
            if session_key(&key) && !sandbox.keys().any(|name| key == OsStr::new(name)) {
                assert_eq!(env[&*key], None);
            }
        }
        let command = room_command(
            Path::new("/workspace"),
            root,
            &record,
            &[
                "--cwd".into(),
                "/override".into(),
                "--".into(),
                "true".into(),
            ],
        )
        .unwrap();
        assert_eq!(command.get_current_dir(), Some(Path::new("/override")));
    }
}
