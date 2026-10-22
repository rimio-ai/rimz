//! Installed-build identity shared by pressure relief and broker retirement.

use crate::disk::parse_cache::StampedPath;
use std::sync::Mutex;

static INSTALLED: Mutex<Option<(StampedPath, Option<String>)>> = Mutex::new(None);

pub(super) fn build() -> Option<String> {
    let path = which::which("rimz").ok()?;
    let stamped = StampedPath::of(&path);
    let mut cached = INSTALLED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((previous, build)) = &*cached
        && *previous == stamped
    {
        return build.clone();
    }
    let build = crate::build_id::of_file(&path).ok();
    if StampedPath::of(&path) != stamped {
        return None;
    }
    *cached = Some((stamped, build.clone()));
    build
}

pub(super) fn may_stop() -> bool {
    same_build(crate::build_id::current(), build().as_deref())
}

fn same_build(own: Option<&str>, installed: Option<&str>) -> bool {
    own.is_some() && own == installed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_known_installed_build_can_stop_servers() {
        let cases = [
            (Some("a"), Some("a")),
            (Some("a"), Some("b")),
            (Some("a"), None),
            (None, Some("a")),
            (None, None),
        ];
        assert_eq!(
            cases.map(|(own, installed)| same_build(own, installed)),
            [true, false, false, false, false]
        );
    }
}
