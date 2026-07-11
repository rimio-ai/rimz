//! Room-runtime sidebar width selected by the renderer.

use std::fs;
use std::num::NonZeroU16;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::store::{RuntimePaths, atomic};

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
struct WidthOverrideFile {
    cols: NonZeroU16,
}

pub fn load(runtime: &RuntimePaths) -> Option<NonZeroU16> {
    let path = runtime.sidebar_width_path();
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            debug!(path = %path.display(), error = %err, "sidebar width override unreadable");
            return None;
        }
    };
    match serde_json::from_slice::<WidthOverrideFile>(&bytes) {
        Ok(file) => Some(file.cols),
        Err(err) => {
            debug!(path = %path.display(), error = %err, "sidebar width override invalid");
            None
        }
    }
}

pub fn write(runtime: &RuntimePaths, cols: NonZeroU16) -> atomic::Result<()> {
    atomic::write_temp_then_rename_cache(&runtime.sidebar_width_path(), &WidthOverrideFile { cols })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::WorkspaceId;

    fn runtime(dir: &std::path::Path) -> RuntimePaths {
        RuntimePaths::under(
            WorkspaceId::parse("ws_0123456789abcdef01234567").expect("workspace id"),
            dir,
        )
        .expect("runtime paths")
    }

    #[test]
    fn round_trip_missing_and_invalid_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = runtime(dir.path());
        assert_eq!(load(&runtime), None);

        let cols = NonZeroU16::new(81).expect("nonzero");
        write(&runtime, cols).expect("write override");
        assert_eq!(load(&runtime), Some(cols));

        fs::write(runtime.sidebar_width_path(), b"not json").expect("garbage file");
        assert_eq!(load(&runtime), None);
    }
}
