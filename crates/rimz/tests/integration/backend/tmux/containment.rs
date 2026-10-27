use std::path::Path;
use std::process::Stdio;

use crate::common::Env;

#[test]
fn env_drop_stops_an_unguarded_real_tmux_server() {
    require_tmux!();
    let env = Env::new();
    let socket = rimz::mux::tmux::managed_server_socket_path_under(&env.runtime_root);
    std::fs::create_dir_all(socket.parent().expect("tmux socket parent"))
        .expect("create tmux socket parent");
    let output = env
        .rimz_at(Path::new("tmux"))
        .arg("-S")
        .arg(&socket)
        .args(["new-session", "-d", "-s", "containment", "sleep", "600"])
        .stdin(Stdio::null())
        .output()
        .expect("start unguarded tmux server");
    assert!(
        output.status.success(),
        "start tmux server: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = env
        .rimz_at(Path::new("tmux"))
        .arg("-S")
        .arg(&socket)
        .args(["display-message", "-p", "#{pid}"])
        .output()
        .expect("read tmux server pid");
    let server_pid = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .expect("tmux server pid");
    #[cfg(target_os = "linux")]
    let server_start = rimz::proc::process_start_token(server_pid);
    #[cfg(not(target_os = "linux"))]
    let _ = server_pid;

    drop(env);

    assert!(!socket.exists(), "managed tmux socket removed");
    // The reap stops matching the server once its environment is unreadable, which is before
    // it exits, and the exited server stays in /proc until its adopter collects it.
    #[cfg(target_os = "linux")]
    {
        use std::time::{Duration, Instant};

        let deadline = Instant::now() + Duration::from_secs(12);
        while rimz::proc::process_is_live(server_pid, server_start.as_deref()) {
            assert!(
                Instant::now() < deadline,
                "tmux server did not exit within 12s of its fixture's drop"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
