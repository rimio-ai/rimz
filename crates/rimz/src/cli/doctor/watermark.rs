use std::fs;

use anyhow::{Context, Result};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use rimz::StatePaths;

#[derive(Deserialize, Serialize)]
struct Watermark {
    cleared_at: Timestamp,
}

pub(super) fn read(paths: &StatePaths) -> Option<Timestamp> {
    serde_json::from_slice::<Watermark>(&fs::read(&paths.doctor_watermark).ok()?)
        .ok()
        .map(|watermark| watermark.cleared_at)
}

pub(super) fn stamp(paths: &StatePaths, now: Timestamp) -> Result<()> {
    let bytes = serde_json::to_vec(&Watermark { cleared_at: now })
        .context("serializing doctor history watermark")?;
    rimz::store::atomic::write_bytes_atomically(&paths.doctor_watermark, &bytes).with_context(
        || {
            format!(
                "writing doctor watermark to {}",
                paths.doctor_watermark.display()
            )
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::WorkspaceId;

    fn paths(dir: &tempfile::TempDir) -> StatePaths {
        StatePaths::under(
            WorkspaceId::parse("ws_0123456789abcdef01234567").expect("workspace id"),
            dir.path(),
        )
        .expect("state paths")
    }

    #[test]
    fn stamp_then_read_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = paths(&dir);
        let now = Timestamp::from_millisecond(1_234).expect("timestamp");

        stamp(&paths, now).expect("stamp watermark");

        assert_eq!(read(&paths), Some(now));
    }

    #[test]
    fn malformed_watermark_reads_as_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = paths(&dir);
        std::fs::create_dir_all(&paths.root).expect("state root");
        std::fs::write(&paths.doctor_watermark, b"not json").expect("broken watermark");

        assert_eq!(read(&paths), None);
    }
}
