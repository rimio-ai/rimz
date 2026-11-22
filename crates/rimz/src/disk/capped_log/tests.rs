use std::io::Write as _;
use std::sync::{Arc, Barrier};
use std::time::Instant;

use super::*;

fn log_with_cap(dir: &Path, cap: u64) -> CappedLog {
    let live = dir.join("log").join("daemon.log");
    CappedLog {
        previous: previous_log(&live),
        lock: dir.join("locks").join("daemon.log.trim.lock"),
        live,
        cap,
    }
}

fn seed(log: &CappedLog, content: &str) {
    std::fs::create_dir_all(log.live.parent().unwrap()).unwrap();
    std::fs::write(&log.live, content).unwrap();
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[test]
fn previous_file_sits_beside_the_live_file() {
    assert_eq!(
        previous_log(Path::new("/r/log/sidebar-host.ab12.log")),
        Path::new("/r/log/sidebar-host.ab12.1.log")
    );
    assert_eq!(
        previous_log(Path::new("/r/log/hook-drainer.log")),
        Path::new("/r/log/hook-drainer.1.log")
    );
}

#[test]
fn a_log_below_the_cap_is_never_touched() {
    let dir = tempfile::tempdir().unwrap();
    let log = log_with_cap(dir.path(), 10);
    seed(&log, "123456789");

    drop(log.open().unwrap());
    assert!(!log.trim().unwrap());

    assert_eq!(read(&log.live).as_deref(), Some("123456789"));
    assert_eq!(read(&log.previous), None);
}

#[test]
fn a_log_at_the_cap_moves_to_the_one_previous_file() {
    let dir = tempfile::tempdir().unwrap();
    let log = log_with_cap(dir.path(), 4);
    seed(&log, "aaaa");

    let mut file = log.open().unwrap();
    assert_eq!(read(&log.live).as_deref(), Some(""));
    assert_eq!(read(&log.previous).as_deref(), Some("aaaa"));

    file.write_all(b"bbbbb").unwrap();
    assert!(log.trim().unwrap());
    assert_eq!(read(&log.live).as_deref(), Some(""));
    assert_eq!(read(&log.previous).as_deref(), Some("bbbb"));
    let names: Vec<_> = std::fs::read_dir(log.live.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
}

#[test]
fn a_log_many_times_the_cap_keeps_its_newest_lines_up_to_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let lines: String = (0..100).map(|n| format!("line {n:02}\n")).collect();
    for (cap, newest) in [
        (20, "line 98\nline 99\n"),
        (24, "line 97\nline 98\nline 99\n"),
    ] {
        let log = log_with_cap(dir.path(), cap);
        seed(&log, &lines);

        assert!(log.trim().unwrap());

        assert_eq!(read(&log.previous).as_deref(), Some(newest), "cap {cap}");
        assert_eq!(read(&log.live).as_deref(), Some(""));
    }
}

#[test]
fn append_descriptors_opened_before_a_trim_keep_writing_at_the_new_end() {
    let dir = tempfile::tempdir().unwrap();
    let log = log_with_cap(dir.path(), 8);
    let mut first = log.open().unwrap();
    let mut second = log.open().unwrap();
    first.write_all(b"one\n").unwrap();
    second.write_all(b"two\n").unwrap();

    assert!(log.trim().unwrap());
    first.write_all(b"three\n").unwrap();
    second.write_all(b"four\n").unwrap();

    let bytes = std::fs::read(&log.live).unwrap();
    assert_eq!(bytes, b"three\nfour\n");
    assert_eq!(read(&log.previous).as_deref(), Some("one\ntwo\n"));
}

#[test]
fn racing_trimmers_keep_what_the_first_one_saved() {
    let dir = tempfile::tempdir().unwrap();
    let log = Arc::new(log_with_cap(dir.path(), 4));
    seed(&log, "saved");
    let start = Arc::new(Barrier::new(8));

    let trimmers: Vec<_> = (0..8)
        .map(|_| {
            let (log, start) = (Arc::clone(&log), Arc::clone(&start));
            std::thread::spawn(move || {
                start.wait();
                log.trim().unwrap()
            })
        })
        .collect();
    let trims = trimmers
        .into_iter()
        .map(|trimmer| trimmer.join().unwrap())
        .filter(|trimmed| *trimmed)
        .count();

    assert_eq!(trims, 1);
    assert_eq!(read(&log.previous).as_deref(), Some("aved"));
    assert_eq!(read(&log.live).as_deref(), Some(""));
}

#[test]
fn a_failed_copy_leaves_the_live_file_and_still_opens() {
    let dir = tempfile::tempdir().unwrap();
    let log = log_with_cap(dir.path(), 4);
    seed(&log, "kept");
    std::fs::create_dir_all(log.previous.join("blocker")).unwrap();

    assert!(log.trim().is_err());
    let mut file = log.open().unwrap();
    file.write_all(b"+").unwrap();

    assert_eq!(read(&log.live).as_deref(), Some("kept+"));
}

#[test]
fn a_process_whose_stderr_is_another_file_starts_no_check() {
    let dir = tempfile::tempdir().unwrap();
    let log = log_with_cap(dir.path(), 4);
    seed(&log, "untouched");
    let other = File::create(dir.path().join("terminal")).unwrap();

    assert!(!log.watch(other.as_fd(), Duration::from_millis(10)));
    std::thread::sleep(Duration::from_millis(50));

    let log = log_with_cap(dir.path(), 4);
    assert_eq!(read(&log.live).as_deref(), Some("untouched"));
    assert_eq!(read(&log.previous), None);
}

#[test]
fn a_process_whose_stderr_is_its_log_trims_it_on_the_interval() {
    let dir = tempfile::tempdir().unwrap();
    let log = log_with_cap(dir.path(), 4);
    let mut stderr = log.open().unwrap();

    assert!(log.watch(stderr.as_fd(), Duration::from_millis(10)));
    stderr.write_all(b"gust").unwrap();

    let log = log_with_cap(dir.path(), 4);
    let deadline = Instant::now() + Duration::from_secs(10);
    while read(&log.previous).is_none() {
        assert!(Instant::now() < deadline, "the check never trimmed the log");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(read(&log.previous).as_deref(), Some("gust"));
}
