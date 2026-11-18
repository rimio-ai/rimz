//! The host's `rimz remote upload` receiver: one JSON header, a ready line,
//! the files' bytes, and a result line naming where they landed.

use super::*;

use std::io::BufRead as _;

/// A receiver running against the fixture's room.
struct Receiver {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
    stdout: std::io::BufReader<std::process::ChildStdout>,
}

impl Receiver {
    fn start(env: &Env) -> Self {
        let dir = env.project_root.to_string_lossy().into_owned();
        let mut child = env
            .rimz()
            .args(["remote", "upload", "--dir", &dir])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn remote upload");
        let stdin = child.stdin.take();
        let stdout = std::io::BufReader::new(child.stdout.take().expect("stdout"));
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn send_header(&mut self, limit: u64, files: &[(&str, u64)]) -> serde_json::Value {
        let files = files
            .iter()
            .map(|(name, size)| serde_json::json!({"name": name, "size": size}))
            .collect::<Vec<_>>();
        let header = serde_json::json!({"v": "rimz.upload.v1", "limit": limit, "files": files});
        self.write(format!("{header}\n").as_bytes());
        self.reply()
    }

    fn write(&mut self, bytes: &[u8]) {
        let stdin = self.stdin.as_mut().expect("receiver stdin open");
        stdin.write_all(bytes).expect("write to receiver");
        stdin.flush().expect("flush receiver stdin");
    }

    fn reply(&mut self) -> serde_json::Value {
        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .expect("read receiver reply");
        serde_json::from_str(&line).unwrap_or_else(|err| panic!("reply {line:?}: {err}"))
    }

    fn close(&mut self) {
        drop(self.stdin.take());
    }

    fn wait(mut self) -> Output {
        self.close();
        let deadline = Instant::now() + COMMAND_TIMEOUT;
        while self.child.try_wait().expect("poll receiver").is_none() {
            assert!(Instant::now() < deadline, "receiver did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut stderr = Vec::new();
        if let Some(mut pipe) = self.child.stderr.take() {
            pipe.read_to_end(&mut stderr).expect("read receiver stderr");
        }
        Output {
            status: self.child.wait().expect("receiver status"),
            stdout: Vec::new(),
            stderr,
        }
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn room(env: &Env) -> rimz::StatePaths {
    env.record(&env.project_root);
    env.state_path_for(&env.project_root)
}

fn upload_units(state: &rimz::StatePaths) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(&state.tmp_dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("_upload-"))
        })
        .collect()
}

fn lane(env: &Env) -> Option<rimz::remote::upload::UploadLane> {
    let bytes = std::fs::read(rimz::remote::upload::lane_path(&env.runtime_paths())).ok()?;
    Some(serde_json::from_slice(&bytes).expect("upload lane json"))
}

fn reply_paths(reply: &serde_json::Value) -> Vec<PathBuf> {
    reply["paths"]
        .as_array()
        .unwrap_or_else(|| panic!("result names paths: {reply}"))
        .iter()
        .map(|path| PathBuf::from(path.as_str().expect("path string")))
        .collect()
}

#[test]
fn upload_receiver_publishes_files_and_replies_their_paths() {
    let env = Env::new();
    let state = room(&env);
    let mut receiver = Receiver::start(&env);
    assert_eq!(
        receiver.send_header(1_000, &[("shot.png", 3), ("notes.txt", 5)]),
        serde_json::json!({"ok": true})
    );
    receiver.write(b"PNGhello");
    let result = receiver.reply();
    assert_eq!(result["ok"], true, "{result}");
    let paths = reply_paths(&result);
    let units = upload_units(&state);
    assert_eq!(units.len(), 1, "one temp unit per transfer: {units:?}");
    assert_eq!(
        paths,
        vec![units[0].join("shot.png"), units[0].join("notes.txt")]
    );
    assert_eq!(std::fs::read(&paths[0]).unwrap(), b"PNG");
    assert_eq!(std::fs::read(&paths[1]).unwrap(), b"hello");
    let out = receiver.wait();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(lane(&env).is_none(), "a finished transfer removes its row");
}

#[test]
fn upload_receiver_refuses_a_transfer_over_the_limit_before_reading_a_byte() {
    let env = Env::new();
    let state = room(&env);
    let mut receiver = Receiver::start(&env);
    let reply = receiver.send_header(4, &[("big.bin", 3), ("more.bin", 2)]);
    assert_eq!(reply.get("ok"), None, "{reply}");
    assert_eq!(
        reply["error"], "5 B is over the 4 B upload limit (paste_max)",
        "{reply}"
    );
    let record = lane(&env).expect("a refused transfer leaves a failed row");
    assert_eq!(record.state, rimz::remote::upload::UploadState::Failed);
    assert_eq!(
        record.reason.as_deref(),
        Some("5 B is over the 4 B upload limit (paste_max)")
    );
    assert!(upload_units(&state).is_empty(), "nothing was written");
}

#[test]
fn upload_receiver_drops_a_short_transfer_without_a_trace() {
    let env = Env::new();
    let state = room(&env);
    let mut receiver = Receiver::start(&env);
    assert_eq!(receiver.send_header(100, &[("a.png", 3)])["ok"], true);
    receiver.write(b"P");
    let out = receiver.wait();
    assert!(!out.status.success(), "a cut transfer is not a success");
    assert!(
        upload_units(&state).is_empty(),
        "{:?}",
        upload_units(&state)
    );
    assert!(lane(&env).is_none(), "a cut transfer leaves no row");
}

#[test]
fn upload_receiver_sanitizes_names_and_keeps_duplicates() {
    let env = Env::new();
    let state = room(&env);
    let mut receiver = Receiver::start(&env);
    let names = [
        ("../up.png", 1),
        ("a/b.png", 1),
        ("\u{1b}[31mred.png", 1),
        ("..", 1),
        (".", 1),
        ("same.png", 1),
        ("same.png", 1),
    ];
    assert_eq!(receiver.send_header(100, &names)["ok"], true);
    receiver.write(b"1234567");
    let paths = reply_paths(&receiver.reply());
    let unit = upload_units(&state).pop().expect("the transfer's unit");
    let names = paths
        .iter()
        .map(|path| {
            assert_eq!(path.parent(), Some(unit.as_path()), "{path:?}");
            path.file_name().unwrap().to_string_lossy().into_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "up.png",
            "b.png",
            "_[31mred.png",
            "file",
            "file-2",
            "same.png",
            "same-2.png"
        ]
    );
    for (path, byte) in paths.iter().zip(b"1234567") {
        assert_eq!(std::fs::read(path).unwrap(), [*byte], "{path:?}");
    }
}

#[test]
fn upload_receiver_keeps_a_newer_transfers_row() {
    let env = Env::new();
    room(&env);
    let mut receiver = Receiver::start(&env);
    assert_eq!(receiver.send_header(100, &[("a.png", 3)])["ok"], true);
    let own = lane(&env).expect("a running transfer publishes its row");
    assert_eq!(own.state, rimz::remote::upload::UploadState::Running);
    assert_eq!((own.received, own.total), (0, 3));
    let newer = rimz::remote::upload::UploadLane::running(
        "newer".to_owned(),
        0,
        9,
        rimz::utils::time::unix_now_ms(),
    );
    rimz::disk::atomic::write_temp_then_rename_cache(
        &rimz::remote::upload::lane_path(&env.runtime_paths()),
        &newer,
    )
    .expect("seed a newer transfer's row");
    receiver.write(b"abc");
    assert_eq!(receiver.reply()["ok"], true);
    assert!(receiver.wait().status.success());
    assert_eq!(lane(&env), Some(newer), "only the owner removes its row");
}
