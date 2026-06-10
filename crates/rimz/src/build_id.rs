//! Build identity of the running executable.
//!
//! Durable sidebar artifacts are written by whichever process holds the role
//! at the time, and across an upgrade old and new builds overlap inside one
//! workspace. Stamping each published pane frame and diagnostic record with
//! the writer's build id turns that overlap into recorded evidence.

use std::sync::OnceLock;

use sha2::{Digest, Sha256};

/// Hex digest prefix of the digest of the running executable's bytes.
const BUILD_ID_BYTES: usize = 6;

/// Build id of this process, computed once from the executable's bytes;
/// `None` when the binary cannot be read (for example replaced mid-upgrade
/// before the re-exec lands).
pub fn current() -> Option<&'static str> {
    static BUILD_ID: OnceLock<Option<String>> = OnceLock::new();
    BUILD_ID.get_or_init(compute).as_deref()
}

fn compute() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let bytes = std::fs::read(exe).ok()?;
    let digest = Sha256::digest(&bytes);
    Some(hex::encode(&digest[..BUILD_ID_BYTES]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_id_is_stable_lowercase_hex() {
        let first = current().expect("the test binary is readable");
        let second = current().expect("the second call serves the cached id");

        assert_eq!(first, second);
        assert_eq!(first.len(), BUILD_ID_BYTES * 2);
        assert!(
            first
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }
}
