//! Two real executable generations for room-record followers.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::Env;

pub(crate) struct RoomBuilds {
    _binaries: tempfile::TempDir,
    pub first: PathBuf,
    pub second: PathBuf,
}

impl RoomBuilds {
    pub fn new(env: &Env) -> Self {
        env.record(&env.project_root);
        let binary = env.rimz_bin();
        let binaries = tempfile::tempdir_in(binary.parent().unwrap()).unwrap();
        let first = binaries.path().join("rimz");
        std::fs::hard_link(binary, &first).unwrap();
        let builds = env.rimz_home().join("builds");
        std::fs::create_dir_all(&builds).unwrap();
        let patched = builds.join("second-rimz");
        let note = env.project_root.join("build-id.note");
        let dump = Command::new("objcopy")
            .args([
                "--only-section=.note.gnu.build-id",
                "--output-target=binary",
            ])
            .arg(&first)
            .arg(&note)
            .output()
            .unwrap();
        assert!(dump.status.success(), "{dump:?}");
        let mut bytes = std::fs::read(&note).unwrap();
        assert_eq!(&bytes[12..16], b"GNU\0");
        bytes[16] ^= 0x80;
        std::fs::write(&note, bytes).unwrap();
        let patch = Command::new("objcopy")
            .arg("--update-section")
            .arg(format!(".note.gnu.build-id={}", note.display()))
            .arg(&first)
            .arg(&patched)
            .output()
            .unwrap();
        assert!(patch.status.success(), "{patch:?}");
        let second_id = rimz::build_id::of_file(&patched).unwrap();
        assert_ne!(rimz::build_id::of_file(&first).unwrap(), second_id);
        let second = builds.join(second_id).join("rimz");
        std::fs::create_dir_all(second.parent().unwrap()).unwrap();
        std::fs::rename(patched, &second).unwrap();
        let builds = Self {
            _binaries: binaries,
            first,
            second,
        };
        builds.record(env, &builds.first);
        builds
    }

    pub fn record(&self, env: &Env, binary: &Path) {
        env.store()
            .record_room_bin(
                &env.resolve_workspace(&env.project_root),
                binary.to_path_buf(),
                rimz::build_id::of_file(binary).unwrap(),
            )
            .unwrap();
    }
}

pub(crate) fn executable(pid: u32) -> PathBuf {
    std::fs::read_link(format!("/proc/{pid}/exe")).unwrap()
}

pub(crate) fn children(pid: u32) -> Vec<u32> {
    std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
        .unwrap()
        .split_whitespace()
        .map(|pid| pid.parse().unwrap())
        .collect()
}
