//! Run a command in a held room without owning its lifecycle.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::{Panes, RoomRecord, apply_env, sandbox_env, session_key, validate_sandbox_root};

#[derive(Debug, PartialEq)]
enum Identity<'a> {
    Environment(&'a str),
    Ancestor(&'a str),
}

#[derive(Debug)]
struct Options<'a> {
    cwd: Option<&'a Path>,
    identity: Option<Identity<'a>>,
    command: &'a [String],
}

fn parse_options(mut args: &[String]) -> Result<Options<'_>> {
    let mut cwd = None;
    let mut identity = None;
    while let Some(flag) = args.first() {
        match flag.as_str() {
            "--cwd" => {
                let dir = args
                    .get(1)
                    .filter(|dir| !dir.starts_with("--"))
                    .context("--cwd requires a directory")?;
                cwd = Some(Path::new(dir));
                args = &args[2..];
            }
            "--as" | "--as-ancestor" => {
                if identity.is_some() {
                    bail!("use only one of --as and --as-ancestor");
                }
                let handle = args
                    .get(1)
                    .filter(|handle| handle.starts_with('@') && handle.len() > 1)
                    .with_context(|| format!("{flag} requires an @handle (or @role#channel)"))?;
                identity = Some(if flag == "--as" {
                    Identity::Environment(handle)
                } else {
                    Identity::Ancestor(handle)
                });
                args = &args[2..];
            }
            "--" => {
                args = &args[1..];
                break;
            }
            _ => break,
        }
    }
    if args.is_empty() {
        bail!("sandbox in requires a command after the root and optional --cwd/--as/--as-ancestor");
    }
    Ok(Options {
        cwd,
        identity,
        command: args,
    })
}

fn match_handle<'a>(handle: &str, candidates: &'a [&str]) -> Result<&'a str> {
    let matches: Vec<_> = candidates
        .iter()
        .copied()
        .filter(|candidate| {
            *candidate == handle
                || (!handle.contains('#') && candidate.split('#').next() == Some(handle))
        })
        .collect();
    match matches.as_slice() {
        [matched] => Ok(matched),
        [] => bail!(
            "unknown agent {handle}; choose a room handle: {}; for an exited agent, {SEAT_FIX}",
            candidates.join(", ")
        ),
        _ => bail!(
            "ambiguous agent {handle}: {}; use @role#channel",
            matches.join(", ")
        ),
    }
}

fn agent_environment(command: &mut Command, root: &Path, pid: u32, environ: &[u8]) {
    for entry in environ.split(|byte| *byte == 0) {
        let Some(separator) = entry.iter().position(|byte| *byte == b'=') else {
            continue;
        };
        let key = OsStr::from_bytes(&entry[..separator]);
        if session_key(key) {
            command.env(key, OsStr::from_bytes(&entry[separator + 1..]));
        }
    }
    command
        .envs(sandbox_env(root))
        .env("RIMZ_AGENT_PID", pid.to_string());
}

fn request_environment(command: &Command) -> BTreeMap<OsString, OsString> {
    let mut environment: BTreeMap<_, _> = std::env::vars_os().collect();
    for (key, value) in command.get_envs() {
        if let Some(value) = value {
            environment.insert(key.into(), value.into());
        } else {
            environment.remove(key);
        }
    }
    environment
}

// Linux's O_NONBLOCK. Identity modes refuse other platforms before opening /proc or a FIFO.
const NONBLOCK: i32 = 0o4000;
const SEAT_FIX: &str =
    "hold a fresh room with this build: cargo xtask sandbox room --mux <backend>";

fn run_in_seat(root: &Path, pid: u32, command: &Command) -> Result<()> {
    let seat = root.join("tmp/agent-seats").join(pid.to_string());
    let mut requests = OpenOptions::new()
        .write(true)
        .custom_flags(NONBLOCK)
        .open(seat.join("requests"))
        .with_context(|| format!("agent pid {pid} has no serving seat; {SEAT_FIX}"))?;
    let request = tempfile::Builder::new()
        .prefix("request-")
        .tempdir_in(&seat)?;
    let dir = request.path();
    super::room::output(
        "creating request streams",
        Command::new("mkfifo").args([dir.join("stdin"), dir.join("stdout"), dir.join("stderr")]),
    )?;
    let mut stdout = OpenOptions::new()
        .read(true)
        .custom_flags(NONBLOCK)
        .open(dir.join("stdout"))?;
    let mut stderr = OpenOptions::new()
        .read(true)
        .custom_flags(NONBLOCK)
        .open(dir.join("stderr"))?;
    let mut script = b"exec".to_vec();
    for (redirect, name) in [(" <", "stdin"), (" >", "stdout"), (" 2>", "stderr")] {
        script.extend_from_slice(redirect.as_bytes());
        shell_word(&mut script, dir.join(name).as_os_str());
    }
    script.extend_from_slice(b"\nif cd ");
    let cwd = command
        .get_current_dir()
        .context("joined command has no cwd")?;
    let cwd = std::env::current_dir()?.join(cwd);
    shell_word(&mut script, cwd.as_os_str());
    script.extend_from_slice(b"; then\n  env -i --");
    for (key, value) in request_environment(command) {
        let mut assignment = key;
        assignment.push("=");
        assignment.push(value);
        script.push(b' ');
        shell_word(&mut script, &assignment);
    }
    for argument in std::iter::once(command.get_program()).chain(command.get_args()) {
        script.push(b' ');
        shell_word(&mut script, argument);
    }
    script.extend_from_slice(b"\nelse\n  false\nfi\nresult=$?\n");
    writeln!(
        script,
        "printf '%s\\n' \"$result\" > {}/status.tmp\nmv {}/status.tmp {}/status",
        super::room::quote(dir.as_os_str()),
        super::room::quote(dir.as_os_str()),
        super::room::quote(dir.as_os_str())
    )?;
    std::fs::write(dir.join("run"), script)?;
    requests
        .write_all(format!("{}\n", dir.display()).as_bytes())
        .with_context(|| format!("sending seat request; {SEAT_FIX}"))?;
    drop(requests);

    let input = dir.join("stdin");
    // Stdin may remain open after the command exits. Do not join a thread blocked on the caller.
    std::thread::spawn(move || {
        if let Ok(mut writer) = OpenOptions::new().write(true).open(input) {
            let _ = std::io::copy(&mut std::io::stdin().lock(), &mut writer);
        }
    });
    let mut out = std::io::stdout().lock();
    let mut err = std::io::stderr().lock();
    loop {
        let status = std::fs::read_to_string(dir.join("status"));
        let copied =
            relay_available(&mut stdout, &mut out)? + relay_available(&mut stderr, &mut err)?;
        match status {
            Ok(status) if copied == 0 => {
                let status: i32 = status.trim().parse().context("reading seat exit status")?;
                if status != 0 {
                    bail!(
                        "sandboxed command `{}` exited with exit status: {status}",
                        command.get_program().to_string_lossy()
                    );
                }
                return Ok(());
            }
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
            _ => {}
        }
        if !Path::new("/proc").join(pid.to_string()).exists() {
            bail!("agent pid {pid} exited while serving the command; {SEAT_FIX}");
        }
        if copied == 0 {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn relay_available(reader: &mut File, writer: &mut impl Write) -> Result<usize> {
    let mut buffer = [0; 8192];
    match reader.read(&mut buffer) {
        Ok(count) => {
            writer.write_all(&buffer[..count])?;
            writer.flush()?;
            Ok(count)
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ) =>
        {
            Ok(0)
        }
        Err(error) => Err(error.into()),
    }
}

fn shell_word(script: &mut Vec<u8>, value: &OsStr) {
    // Unlike the card's display strings, request words must preserve non-UTF-8 environment bytes.
    script.push(b'\'');
    for &byte in value.as_bytes() {
        if byte == b'\'' {
            script.extend_from_slice(b"'\\''");
        } else {
            script.push(byte);
        }
    }
    script.push(b'\'');
}

pub(super) fn run(workspace: &Path, args: &[String]) -> Result<()> {
    let (mut command, ancestor) = joined_command(workspace, args)?;
    if let Some(pid) = ancestor {
        return run_in_seat(Path::new(&args[0]), pid, &command);
    }
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

fn joined_command(workspace: &Path, args: &[String]) -> Result<(Command, Option<u32>)> {
    let Some((root, args)) = args.split_first() else {
        bail!(
            "usage: cargo xtask sandbox in <root> [--cwd <dir>] [--as <@handle> | --as-ancestor <@handle>] [--] <command>…"
        );
    };
    let root = Path::new(root);
    validate_sandbox_root(root)
        .context("use the sandbox root printed by cargo xtask sandbox room --mux <backend>")?;
    let record = RoomRecord::read(root).context(
        "no readable room record; create a held room with cargo xtask sandbox room --mux <backend>",
    )?;
    let options = parse_options(args)?;
    let mut command = room_command(workspace, root, &record, &options)?;
    let Some(identity) = options.identity else {
        return Ok((command, None));
    };
    if !cfg!(target_os = "linux") {
        bail!("--as and --as-ancestor require Linux /proc; run the held room on Linux");
    }
    let handle = match identity {
        Identity::Environment(handle) | Identity::Ancestor(handle) => handle,
    };
    let (pid, environ) = agent_process(workspace, root, &record, handle)?;
    match identity {
        Identity::Environment(_) => {
            agent_environment(&mut command, root, pid, &environ);
            Ok((command, None))
        }
        Identity::Ancestor(_) => Ok((command, Some(pid))),
    }
}

fn agent_process(
    workspace: &Path,
    root: &Path,
    record: &RoomRecord,
    handle: &str,
) -> Result<(u32, Vec<u8>)> {
    let args = vec![
        record.binary.to_string_lossy().into_owned(),
        format!("--{}", record.mux),
        "pane".into(),
        "list".into(),
        "--json".into(),
    ];
    let mut command = room_command(workspace, root, record, &parse_options(&args)?)?;
    let panes: Panes =
        serde_json::from_slice(&super::room::output("listing room agents", &mut command)?)?;
    let candidates: Vec<_> = panes
        .tabs
        .iter()
        .flat_map(|tab| &tab.panes)
        .filter_map(|pane| pane.agent.as_ref().map(|agent| agent.handle.as_str()))
        .collect();
    let matched = match_handle(handle, &candidates)?;
    let pane = panes
        .tabs
        .iter()
        .flat_map(|tab| &tab.panes)
        .find(|pane| {
            pane.agent
                .as_ref()
                .is_some_and(|agent| agent.handle == matched)
        })
        .context("matched room handle has no pane")?;
    let mut pending = std::collections::VecDeque::from_iter(pane.pid);
    while let Some(pid) = pending.pop_front() {
        let process = Path::new("/proc").join(pid.to_string());
        if let Ok(environ) = std::fs::read(process.join("environ"))
            && environ
                .split(|byte| *byte == 0)
                .any(|entry| entry.starts_with(b"RIMZ_AGENT_KIND="))
        {
            return Ok((pid, environ));
        }
        let Ok(tasks) = std::fs::read_dir(process.join("task")) else {
            continue;
        };
        for task in tasks.flatten() {
            if let Ok(children) = std::fs::read_to_string(task.path().join("children")) {
                pending.extend(
                    children
                        .split_whitespace()
                        .filter_map(|child| child.parse::<u32>().ok()),
                );
            }
        }
    }
    bail!(
        "no agent process under pane {} (pid {:?}); {SEAT_FIX}",
        pane.pane_id,
        pane.pid
    )
}

fn room_command(
    workspace: &Path,
    root: &Path,
    record: &RoomRecord,
    options: &Options<'_>,
) -> Result<Command> {
    let cwd = options.cwd.unwrap_or(&record.worktree);
    let (program, program_args) = options
        .command
        .split_first()
        .context("sandbox in requires a command")?;
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
        [record.stub_dir.as_path()]
            .into_iter()
            .chain(record.binary.parent())
            .map(Path::to_path_buf)
            .chain(std::env::split_paths(&inherited_path)),
    )
    .context("prepending the room stub and binary directories to PATH")?;
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
    fn identity_flags_accept_either_order() {
        for flag in ["--as", "--as-ancestor"] {
            for args in [
                vec!["--cwd", "/override", flag, "@coder", "--", "true"],
                vec![flag, "@coder", "--cwd", "/override", "true"],
            ] {
                let args: Vec<String> = args.into_iter().map(String::from).collect();
                let options = parse_options(&args).unwrap();
                assert_eq!(options.cwd, Some(Path::new("/override")));
                assert_eq!(options.command, ["true"]);
                assert_eq!(
                    options.identity,
                    Some(if flag == "--as" {
                        Identity::Environment("@coder")
                    } else {
                        Identity::Ancestor("@coder")
                    })
                );
            }
        }
    }

    #[test]
    fn identity_flags_refuse_conflicts_and_missing_handles() {
        for (args, message) in [
            (
                vec!["--as", "@coder", "--as-ancestor", "@coder", "true"],
                "only one",
            ),
            (
                vec!["--as-ancestor", "@coder", "--as", "@coder", "true"],
                "only one",
            ),
            (vec!["--as"], "requires an @handle"),
            (vec!["--as-ancestor", "--", "true"], "requires an @handle"),
            (vec!["--as", "coder", "true"], "requires an @handle"),
        ] {
            let args: Vec<String> = args.into_iter().map(String::from).collect();
            assert!(
                parse_options(&args)
                    .unwrap_err()
                    .to_string()
                    .contains(message)
            );
        }
    }

    #[test]
    fn identity_flags_stop_at_command_or_separator() {
        for args in [
            vec!["--", "echo", "--as", "@coder"],
            vec!["echo", "--as-ancestor", "@coder"],
        ] {
            let args: Vec<String> = args.into_iter().map(String::from).collect();
            let options = parse_options(&args).unwrap();
            assert_eq!(options.identity, None);
            assert_eq!(options.command[0], "echo");
            assert_eq!(options.command.len(), 3);
        }
    }

    #[test]
    fn handles_match_bare_and_qualified_addresses() {
        let candidates = ["@coder#probe", "@reviewer#probe"];
        assert_eq!(match_handle("@coder", &candidates).unwrap(), "@coder#probe");
        assert_eq!(
            match_handle("@reviewer#probe", &candidates).unwrap(),
            "@reviewer#probe"
        );
    }

    #[test]
    fn handles_report_ambiguous_and_unknown_candidates() {
        let candidates = ["@coder#one", "@coder#two"];
        let ambiguous = match_handle("@coder", &candidates).unwrap_err().to_string();
        assert!(ambiguous.contains("@role#channel"));
        let unknown = match_handle("@missing", &candidates)
            .unwrap_err()
            .to_string();
        assert!(unknown.contains("hold a fresh room with this build"));
        for candidate in candidates {
            assert!(ambiguous.contains(candidate));
            assert!(unknown.contains(candidate));
        }
    }

    #[test]
    fn agent_environment_overlays_only_session_keys_and_keeps_roots() {
        let root = Path::new("/tmp/rimz-sandbox-test");
        let mut command = Command::new("true");
        apply_env(&mut command, &sandbox_env(root), true);
        agent_environment(&mut command, root, 42, b"RIMZ_AGENT_KIND=claude\0RIMZ_AGENT_PID=99\0RIMZ_ROOM_ID=room\0TMUX_PANE=%4\0RIMZ_HOME=/wrong\0TMUX_TMPDIR=/wrong\0ZELLIJ_CONFIG_DIR=/wrong\0HOME=/wrong\0PATH=/wrong\0SECRET=private\0RIMZ_AGENT_NAME=nonutf8-\xff\0");
        let env: BTreeMap<_, _> = command.get_envs().collect();
        for (key, value) in [
            ("RIMZ_AGENT_KIND", "claude"),
            ("RIMZ_AGENT_PID", "42"),
            ("RIMZ_ROOM_ID", "room"),
            ("TMUX_PANE", "%4"),
        ] {
            assert_eq!(env[OsStr::new(key)], Some(OsStr::new(value)));
        }
        for (key, value) in sandbox_env(root) {
            assert_eq!(env[OsStr::new(key)], Some(value.as_os_str()));
        }
        assert!(!env.contains_key(OsStr::new("SECRET")));
        assert!(!env.contains_key(OsStr::new("PATH")));
        assert_eq!(
            env[OsStr::new("RIMZ_AGENT_NAME")].unwrap().as_bytes(),
            b"nonutf8-\xff"
        );
    }

    #[test]
    fn ancestor_request_environment_is_plain_join_environment() {
        let root = Path::new("/tmp/rimz-sandbox-test");
        let record = RoomRecord {
            mux: "tmux".into(),
            session: "room".into(),
            worktree: root.join("worktree"),
            repo: root.join("repo"),
            stub_dir: root.join("bin"),
            binary: "/dev/rimz".into(),
        };
        let command = room_command(
            Path::new("/workspace"),
            root,
            &record,
            &parse_options(&["true".into()]).unwrap(),
        )
        .unwrap();
        let mut expected: BTreeMap<_, _> = std::env::vars_os().collect();
        expected.retain(|key, _| !session_key(key));
        expected.extend(
            sandbox_env(root)
                .into_iter()
                .map(|(key, value)| (key.into(), value.into_os_string())),
        );
        expected.insert(
            "PATH".into(),
            command
                .get_envs()
                .find(|(key, _)| *key == "PATH")
                .unwrap()
                .1
                .unwrap()
                .into(),
        );
        assert_eq!(request_environment(&command), expected);
    }

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
            binary: "/dev/rimz".into(),
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
            binary: "/dev/rimz".into(),
        };
        let command = room_command(
            Path::new("/workspace"),
            root,
            &record,
            &parse_options(&[
                "--".into(),
                "target/debug/rimz".into(),
                "pane".into(),
                "list".into(),
            ])
            .unwrap(),
        )
        .unwrap();
        assert_eq!(command.get_program(), "/workspace/target/debug/rimz");
        assert_eq!(command.get_current_dir(), Some(record.worktree.as_path()));
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["pane", "list"]);
        let env: BTreeMap<_, _> = command.get_envs().collect();
        let inherited_path = std::env::var_os("PATH").unwrap_or_default();
        let paths: Vec<_> = std::env::split_paths(env[OsStr::new("PATH")].unwrap()).collect();
        assert_eq!(paths[0], record.stub_dir);
        assert_eq!(paths[1], Path::new("/dev"));
        assert_eq!(
            paths[2..],
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
        for flag in ["--as", "--as-ancestor"] {
            let args = [flag.into(), "@coder".into(), "true".into()];
            let mut identified = room_command(
                Path::new("/workspace"),
                root,
                &record,
                &parse_options(&args).unwrap(),
            )
            .unwrap();
            if flag == "--as" {
                agent_environment(
                    &mut identified,
                    root,
                    42,
                    b"RIMZ_AGENT_KIND=claude\0TMUX_PANE=%4\0",
                );
            }
            let actual = request_environment(&identified);
            let mut expected = request_environment(&command);
            if flag == "--as" {
                expected.extend([
                    ("RIMZ_AGENT_KIND".into(), "claude".into()),
                    ("TMUX_PANE".into(), "%4".into()),
                    ("RIMZ_AGENT_PID".into(), "42".into()),
                ]);
            }
            assert_eq!(actual, expected);
            assert_eq!(identified.get_current_dir(), command.get_current_dir());
        }
        let command = room_command(
            Path::new("/workspace"),
            root,
            &record,
            &parse_options(&[
                "--cwd".into(),
                "/override".into(),
                "--".into(),
                "true".into(),
            ])
            .unwrap(),
        )
        .unwrap();
        assert_eq!(command.get_current_dir(), Some(Path::new("/override")));
    }
}
