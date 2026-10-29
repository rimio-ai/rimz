//! End-to-end proof that the rimzd middle-column supervisor watches the
//! per-machine config and swaps its child command in place on save.

use std::path::Path;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

use crate::common::Env;
#[cfg(target_os = "linux")]
use crate::common::builds::children;

#[test]
fn content_supervisor_restarts_child_when_config_is_saved() {
    let env = Env::new();
    let config_path = env.rimz_home().join("config.toml");
    let sentinels = env.home_root.join("daemon-content");
    std::fs::create_dir_all(&sentinels).expect("mkdir sentinels");

    let first_pid = sentinels.join("first.pid");
    let first_marker = sentinels.join("first.marker");
    let second_pid = sentinels.join("second.pid");
    let second_marker = sentinels.join("second.marker");
    write_config(&config_path, &sentinel_command(&first_pid, &first_marker));

    let child = env
        .rimz()
        .args(["daemon", "content", "--slot", "0", "--worktree-root"])
        .arg(&env.project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon content supervisor");
    let mut supervisor = ChildGuard { child };

    assert!(
        wait_until(Duration::from_secs(5), || first_marker.exists()),
        "first configured child did not start; supervisor status: {:?}",
        supervisor.child.try_wait().expect("poll supervisor")
    );
    let first_child_pid = read_pid(&first_pid);
    assert!(pid_alive(first_child_pid), "first child pid is not live");

    std::thread::sleep(Duration::from_millis(100));
    write_raw_config(&config_path, "[daemon\n");
    std::thread::sleep(Duration::from_millis(700));
    assert!(
        pid_alive(first_child_pid),
        "broken mid-edit config should keep the current child"
    );

    let missing_command = format!("rimz-missing-daemon-content-command-{}", std::process::id());
    write_config(&config_path, &missing_command);
    std::thread::sleep(Duration::from_millis(700));
    assert!(
        supervisor
            .child
            .try_wait()
            .expect("poll supervisor")
            .is_none(),
        "unspawnable reload command should keep the supervisor running"
    );
    assert!(
        pid_file_alive(&first_pid),
        "unspawnable reload command should keep the previous command running"
    );

    write_config(&config_path, &sentinel_command(&second_pid, &second_marker));

    assert!(
        wait_until(Duration::from_secs(5), || second_marker.exists()),
        "second configured child did not start; supervisor status: {:?}",
        supervisor.child.try_wait().expect("poll supervisor")
    );
    assert!(
        wait_until(Duration::from_secs(5), || !pid_alive(first_child_pid)),
        "first child pid stayed live after config reload"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn content_supervisor_follows_room_build_without_overlapping_children() {
    use crate::common::builds::{RoomBuilds, children, executable};

    let env = Env::new();
    let builds = RoomBuilds::new(&env);
    let pid_path = env.home_root.join("content.pid");
    let marker = env.home_root.join("content.ready");
    write_config(
        &env.rimz_home().join("config.toml"),
        &tree_command(&env, &pid_path, &marker, false),
    );
    let mut supervisor = content_process(&env, &builds.first, true);
    assert!(wait_until(Duration::from_secs(10), || marker.exists()));
    let first_child = read_pid(&pid_path) as u32;
    let first_grandchild = host_pids(&env.home_root.join("grandchildren.pids"))[0];
    let pid = supervisor.child.id();

    builds.record(&env, &builds.first);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(executable(pid), builds.first);
    assert_eq!(
        children(pid),
        vec![first_child],
        "same-build write restarted the child"
    );

    builds.record(&env, &builds.second);
    assert!(
        wait_until(Duration::from_secs(10), || executable(pid) == builds.second),
        "content supervisor did not exec the recorded build"
    );
    assert!(wait_until(Duration::from_secs(10), || try_read_pid(
        &pid_path
    )
    .is_some_and(|pid| pid as u32 != first_child)));
    let next_child = read_pid(&pid_path) as u32;
    assert_eq!(children(pid), vec![next_child]);
    assert!(
        !Path::new(&format!("/proc/{first_child}")).exists(),
        "old child was not reaped"
    );
    assert!(
        !rimz::proc::process_is_live(first_grandchild, None),
        "old slot grandchild {first_grandchild} survived the build move",
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        children(pid),
        vec![next_child],
        "follower execed twice for one change"
    );
    assert!(supervisor.child.try_wait().unwrap().is_none());
}

#[test]
#[cfg(target_os = "linux")]
fn content_supervisor_without_workspace_does_not_follow() {
    use crate::common::builds::{RoomBuilds, executable};

    let env = Env::new();
    let builds = RoomBuilds::new(&env);
    let pid_path = env.home_root.join("content.pid");
    let marker = env.home_root.join("content.ready");
    write_config(
        &env.rimz_home().join("config.toml"),
        &sentinel_command(&pid_path, &marker),
    );
    let supervisor = content_process(&env, &builds.first, false);
    assert!(wait_until(Duration::from_secs(10), || marker.exists()));
    let first_child = read_pid(&pid_path);
    builds.record(&env, &builds.second);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(executable(supervisor.child.id()), builds.first);
    assert_eq!(read_pid(&pid_path), first_child);
}

#[test]
#[cfg(target_os = "linux")]
fn content_supervisor_respawns_stats_on_its_running_build_after_exec_failure() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    use crate::common::builds::{RoomBuilds, children};

    let env = Env::new();
    let builds = RoomBuilds::new(&env);
    let staged = env
        .rimz_home()
        .join("builds")
        .join(rimz::build_id::of_file(&builds.first).unwrap())
        .join("rimz");
    std::fs::create_dir_all(staged.parent().unwrap()).unwrap();
    std::fs::copy(&builds.first, &staged).unwrap();
    builds.record(&env, &staged);
    let room_bin = env.store().paths().room_bin.clone();
    assert_eq!(
        std::fs::metadata(&room_bin).unwrap().ino(),
        std::fs::metadata(&staged).unwrap().ino(),
    );
    assert_ne!(
        std::fs::metadata(&builds.second).unwrap().ino(),
        std::fs::metadata(&staged).unwrap().ino(),
        "removing execute permission must not affect the running build",
    );
    let log = env.home_root.join("supervisor.stderr");
    let pty = nix::pty::openpty(None, None).unwrap();
    let mut supervisor = ChildGuard {
        child: env
            .rimz_at(&room_bin)
            .args(["daemon", "content", "--slot", "0", "--worktree-root"])
            .arg(&env.project_root)
            .args(["--workspace-id", env.workspace_id.as_str()])
            .env("RIMZ_TEST_SIDEBAR_RECORD_POLL_MS", "50")
            .stdin(pty.slave.try_clone().unwrap())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap(),
    };
    let pid = supervisor.child.id();
    assert!(wait_until(Duration::from_secs(10), || children(pid).len() == 1));
    let first_child = children(pid)[0];
    std::fs::set_permissions(&builds.second, std::fs::Permissions::from_mode(0o644)).unwrap();
    builds.record(&env, &builds.second);
    assert!(
        wait_until(Duration::from_secs(10), || {
            std::fs::read_to_string(&log)
                .unwrap_or_default()
                .contains("daemon content exec failed")
        }),
        "no failed exec observed; supervisor {:?}; {}",
        supervisor.child.try_wait().unwrap(),
        std::fs::read_to_string(&log).unwrap(),
    );
    assert!(
        wait_until(Duration::from_secs(10), || {
            let status = supervisor.child.try_wait().unwrap();
            status.is_some() || children(pid).iter().any(|child| *child != first_child)
        }),
        "supervisor never settled after the failed exec",
    );
    let status = supervisor.child.try_wait().unwrap();
    assert!(
        status.is_none(),
        "failed exec closed the supervisor: {status:?}; {}",
        std::fs::read_to_string(&log).unwrap(),
    );
    let next_children = children(pid);
    assert_eq!(next_children.len(), 1, "{next_children:?}");
    assert_eq!(
        rimz::build_id::of_file(Path::new(&format!("/proc/{}/exe", next_children[0]))).unwrap(),
        rimz::build_id::of_file(&builds.first).unwrap(),
    );
    assert!(!Path::new(&format!("/proc/{first_child}")).exists());
}

#[test]
#[cfg(target_os = "linux")]
fn off_target_content_supervisor_ignores_ordinary_record_writes() {
    use crate::common::builds::{RoomBuilds, executable};

    let env = Env::new();
    let builds = RoomBuilds::new(&env);
    let pid_path = env.home_root.join("content.pid");
    let marker = env.home_root.join("content.ready");
    write_config(
        &env.rimz_home().join("config.toml"),
        &sentinel_command(&pid_path, &marker),
    );
    let supervisor = content_process(&env, &builds.second, true);
    assert!(wait_until(Duration::from_secs(10), || marker.exists()));
    let first_child = read_pid(&pid_path);
    assert_eq!(executable(supervisor.child.id()), builds.second);

    builds.record(&env, &builds.first);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        executable(supervisor.child.id()),
        builds.second,
        "an ordinary record write moved a deliberately off-target process"
    );
    assert_eq!(read_pid(&pid_path), first_child);
}

#[cfg(target_os = "linux")]
fn content_process(env: &Env, binary: &Path, follows: bool) -> ChildGuard {
    let mut command = env.rimz_at(binary);
    command
        .args(["daemon", "content", "--slot", "0", "--worktree-root"])
        .arg(&env.project_root)
        .env("RIMZ_TEST_SIDEBAR_RECORD_POLL_MS", "50")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if follows {
        command.args(["--workspace-id", env.workspace_id.as_str()]);
    }
    ChildGuard {
        child: command.spawn().unwrap(),
    }
}

#[test]
#[cfg(target_os = "linux")]
fn daemon_host_follows_room_build_after_hangup() {
    host_build_move(false);
}

#[test]
#[cfg(target_os = "linux")]
fn daemon_host_kills_a_child_that_ignores_hangup() {
    host_build_move(true);
}

#[cfg(target_os = "linux")]
fn host_build_move(ignore_hangup: bool) {
    use crate::common::builds::{RoomBuilds, children, executable};

    let env = Env::new();
    let builds = RoomBuilds::new(&env);
    let mut host = host_tree_process(&env, &builds.first, ignore_hangup);
    let pids = env.home_root.join("host.pids");
    assert!(wait_until(Duration::from_secs(10), || !host_pids(&pids).is_empty()));
    let first_child = host_pids(&pids)[0];
    let first_grandchild = host_pids(&env.home_root.join("grandchildren.pids"))[0];
    let pid = host.child.id();
    assert_eq!(
        nix::unistd::getpgid(Some(Pid::from_raw(first_child as i32))).unwrap(),
        nix::unistd::getpgid(Some(Pid::from_raw(pid as i32))).unwrap()
    );
    builds.record(&env, &builds.first);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(host_pids(&pids), vec![first_child]);
    let move_started = Instant::now();
    builds.record(&env, &builds.second);
    assert!(
        wait_until(Duration::from_secs(10), || executable(pid) == builds.second),
        "daemon host did not stop its child and exec the recorded build"
    );
    assert!(wait_until(Duration::from_secs(10), || host_pids(&pids)
        .len()
        == 2));
    let next_child = host_pids(&pids)[1];
    assert_eq!(children(pid), vec![next_child]);
    assert!(
        !Path::new(&format!("/proc/{first_child}")).exists(),
        "old host was not reaped"
    );
    assert!(
        !rimz::proc::process_is_live(first_grandchild, None),
        "old host grandchild {first_grandchild} survived the build move",
    );
    if ignore_hangup {
        assert!(move_started.elapsed() >= Duration::from_millis(100));
    }
    if !ignore_hangup {
        assert_eq!(
            std::fs::read_to_string(env.home_root.join("host.signals")).unwrap(),
            format!("{first_child} HUP\n")
        );
    }
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        host_pids(&pids).len(),
        2,
        "host restarted twice for one record move"
    );
    assert!(host.child.try_wait().unwrap().is_none());
}

#[test]
#[cfg(target_os = "linux")]
fn daemon_host_relays_termination_and_returns_child_status() {
    for (signal, name, code) in [(Signal::SIGHUP, "HUP", 17), (Signal::SIGTERM, "TERM", 19)] {
        let env = Env::new();
        env.record(&env.project_root);
        let mut host = host_process(&env, &env.rimz_bin(), false);
        let pids = env.home_root.join("host.pids");
        assert!(wait_until(Duration::from_secs(10), || !host_pids(&pids).is_empty()));
        let child = host_pids(&pids)[0];
        assert!(wait_until(Duration::from_secs(10), || sleeper_is_running(
            child
        )));
        kill(Pid::from_raw(host.child.id() as i32), signal).unwrap();
        assert!(
            wait_until(Duration::from_secs(10), || host
                .child
                .try_wait()
                .unwrap()
                .is_some()),
            "wrapper did not exit after relaying {name}"
        );
        assert_eq!(host.child.wait().unwrap().code(), Some(code));
        assert_eq!(
            std::fs::read_to_string(env.home_root.join("host.signals")).unwrap(),
            format!("{child} {name}\n")
        );
        assert!(!Path::new(&format!("/proc/{child}")).exists());
    }
}

#[test]
#[cfg(target_os = "linux")]
fn daemon_host_relays_repeated_termination_signals() {
    let env = Env::new();
    env.record(&env.project_root);
    let signals = env.home_root.join("signals");
    let script = r#"
count=0
trap 'count=$((count + 1)); echo "$count" >> "$1"; if [ "$count" = 2 ]; then kill "$sleeper"; wait "$sleeper"; exit 23; fi' TERM
/bin/sleep 120 &
sleeper=$!
printf 'ready\n' > "$2"
while :; do wait "$sleeper"; done
"#;
    let ready = env.home_root.join("ready");
    let mut host = ChildGuard {
        child: env
            .rimz()
            .args([
                "daemon",
                "host",
                "--workspace-id",
                env.workspace_id.as_str(),
                "--",
                "sh",
                "-c",
            ])
            .arg(script)
            .arg("sh")
            .arg(&signals)
            .arg(&ready)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    };
    assert!(wait_until(Duration::from_secs(10), || ready.exists()));
    assert!(wait_until(Duration::from_secs(10), || {
        children(host.child.id())
            .into_iter()
            .any(sleeper_is_running)
    }));
    let pid = Pid::from_raw(host.child.id() as i32);
    kill(pid, Signal::SIGTERM).unwrap();
    assert!(wait_until(Duration::from_secs(10), || {
        std::fs::read_to_string(&signals).unwrap_or_default() == "1\n"
    }));
    kill(pid, Signal::SIGTERM).unwrap();
    assert!(
        wait_until(Duration::from_secs(5), || host
            .child
            .try_wait()
            .unwrap()
            .is_some()),
        "wrapper lost the second SIGTERM; child saw {:?}",
        std::fs::read_to_string(&signals),
    );
    assert_eq!(host.child.wait().unwrap().code(), Some(23));
    assert_eq!(std::fs::read_to_string(&signals).unwrap(), "1\n2\n");
}

#[cfg(target_os = "linux")]
fn sleeper_is_running(parent: u32) -> bool {
    let sleep = std::fs::canonicalize("/bin/sleep").unwrap();
    children(parent)
        .into_iter()
        .any(|pid| std::fs::read_link(format!("/proc/{pid}/exe")).is_ok_and(|exe| exe == sleep))
}

#[cfg(target_os = "linux")]
fn tree_command(env: &Env, pid_path: &Path, marker: &Path, ignore_hangup: bool) -> String {
    let sleep = env.home_root.join("unique-daemon-tree-sleep");
    std::os::unix::fs::symlink("/bin/sleep", &sleep).unwrap();
    let trap = if ignore_hangup {
        "trap '' HUP"
    } else {
        "trap 'printf \"%s HUP\\n\" \"$$\" >> \"$5\"; exit 17' HUP"
    };
    let script = format!(
        r#"{trap}
trap 'exit 0' TERM
"$3" 120 &
printf '%s\n' "$!" >> "$4"
echo $$ > "$1"
printf '%s\n' "$$" >> "$6"
touch "$2"
while :; do wait "$!"; done
"#,
    );
    [
        "sh",
        "-c",
        &script,
        "sh",
        &pid_path.display().to_string(),
        &marker.display().to_string(),
        &sleep.display().to_string(),
        &env.home_root
            .join("grandchildren.pids")
            .display()
            .to_string(),
        &env.home_root.join("host.signals").display().to_string(),
        &env.home_root.join("host.pids").display().to_string(),
    ]
    .map(shell_arg)
    .join(" ")
}

#[cfg(target_os = "linux")]
fn host_tree_process(env: &Env, binary: &Path, ignore_hangup: bool) -> ChildGuard {
    let argv = shlex::split(&tree_command(
        env,
        &env.home_root.join("host.pid"),
        &env.home_root.join("host.ready"),
        ignore_hangup,
    ))
    .unwrap();
    let mut command = env.rimz_at(binary);
    command
        .args([
            "daemon",
            "host",
            "--workspace-id",
            env.workspace_id.as_str(),
            "--",
        ])
        .args(argv)
        .env("RIMZ_TEST_SIDEBAR_RECORD_POLL_MS", "50")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Only a child that ignores SIGHUP waits out the grace, so only it gets a short one: a short
    // grace can kill a hangup child inside its trap on a loaded host.
    if ignore_hangup {
        command.env("RIMZ_TEST_DAEMON_HOST_STOP_MS", "100");
    }
    ChildGuard {
        child: command.spawn().unwrap(),
    }
}

#[cfg(target_os = "linux")]
fn host_process(env: &Env, binary: &Path, ignore_hangup: bool) -> ChildGuard {
    let hup = if ignore_hangup {
        "trap '' HUP"
    } else {
        r#"trap 'printf "%s HUP\n" "$$" >> "$2"; kill "$sleeper"; wait "$sleeper"; exit 17' HUP"#
    };
    let script = if ignore_hangup {
        "trap '' HUP; printf '%s\\n' \"$$\" >> \"$1\"; exec sleep 120".to_owned()
    } else {
        format!(
            r#"{hup}
trap 'printf "%s TERM\n" "$$" >> "$2"; kill "$sleeper"; wait "$sleeper"; exit 19' TERM
/bin/sleep 120 &
sleeper=$!
printf '%s\n' "$$" >> "$1"
wait "$sleeper"
"#
        )
    };
    ChildGuard {
        child: env
            .rimz_at(binary)
            .args([
                "daemon",
                "host",
                "--workspace-id",
                env.workspace_id.as_str(),
                "--",
                "sh",
                "-c",
            ])
            .arg(script)
            .arg("sh")
            .arg(env.home_root.join("host.pids"))
            .arg(env.home_root.join("host.signals"))
            .env("RIMZ_TEST_SIDEBAR_RECORD_POLL_MS", "50")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    }
}

#[cfg(target_os = "linux")]
fn host_pids(path: &Path) -> Vec<u32> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .split_inclusive('\n')
        .filter_map(|pid| pid.strip_suffix('\n')?.parse().ok())
        .collect()
}

struct ChildGuard {
    child: Child,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = kill(Pid::from_raw(self.child.id() as i32), Signal::SIGTERM);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

fn sentinel_command(pid_path: &Path, marker_path: &Path) -> String {
    let script = r#"echo $$ > "$1"; touch "$2"; exec sleep 60"#;
    format!(
        "sh -c {} sh {} {}",
        shell_arg(script),
        shell_arg(&pid_path.display().to_string()),
        shell_arg(&marker_path.display().to_string()),
    )
}

fn shell_arg(raw: &str) -> String {
    shlex::try_quote(raw).expect("shell quote").into_owned()
}

fn write_config(path: &Path, command: &str) {
    write_raw_config(
        path,
        &format!("[daemon]\n[[daemon.pane]]\ncommand = {command:?}\n"),
    );
}

fn write_raw_config(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("config parent")).expect("mkdir config parent");
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).expect("write config temp");
    std::fs::rename(&tmp, path).expect("publish config");
}

fn read_pid(path: &Path) -> i32 {
    std::fs::read_to_string(path)
        .expect("read pid")
        .trim()
        .parse()
        .expect("parse pid")
}

fn pid_file_alive(path: &Path) -> bool {
    try_read_pid(path).is_some_and(pid_alive)
}

fn try_read_pid(path: &Path) -> Option<i32> {
    std::fs::read_to_string(path)
        .ok()?
        .strip_suffix('\n')?
        .parse()
        .ok()
}

fn pid_alive(pid: i32) -> bool {
    matches!(kill(Pid::from_raw(pid), None), Ok(()) | Err(Errno::EPERM))
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    condition()
}
