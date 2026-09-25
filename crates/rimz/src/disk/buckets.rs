//! Shared date windows for append-only audit logs.

use jiff::Timestamp;
use std::io;
use std::path::{Path, PathBuf};

pub(crate) fn bucket_file_name(at: Timestamp, file_days: u32) -> String {
    const SECONDS_PER_DAY: i64 = 86_400;
    let days = at.as_second().div_euclid(SECONDS_PER_DAY);
    let window = i64::from(file_days.max(1));
    let start_days = days.div_euclid(window) * window;
    let start = Timestamp::from_second(start_days * SECONDS_PER_DAY)
        .expect("day-aligned unix timestamp is valid");
    format!("{}.jsonl", start.strftime("%Y-%m-%d"))
}

pub(crate) fn bucket_files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut files = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }
    Ok(files)
}
