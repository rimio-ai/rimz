//! Pure lexical path helpers; no filesystem access.

use std::path::{Path, PathBuf};

/// Fold `.` and `..` components without touching the filesystem.
///
/// This is purely lexical, so it preserves relativeness and can return an
/// empty path — `.` folds to `""`. Callers that need an absolute result
/// absolutize first and check the result is non-empty.
pub fn normalize_path_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}
