//! End-to-end proof that the rimzd middle-column supervisor watches the
//! per-machine config and swaps its child command in place on save.

use std::path::Path;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

use crate::common::Env;

#[test]
fn content_supervisor_restarts_child_when_config_is_saved() {
    let env = Env::new();
    let config_path = env.config_root().join("rimz").join("config.toml");
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
