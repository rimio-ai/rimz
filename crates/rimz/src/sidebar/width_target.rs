//! One room-runtime sidebar width target shared by every renderer.

use std::fs;
use std::num::NonZeroU16;

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::mux::{SidebarWidth, WidthPermille};
use crate::store::{RuntimePaths, atomic};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct WidthTargetFile {
    permille: WidthPermille,
    #[serde(default)]
    pinned: bool,
}

fn load_file(runtime: &RuntimePaths) -> Option<WidthTargetFile> {
    let path = runtime.sidebar_width_path();
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            debug!(path = %path.display(), error = %err, "sidebar width target unreadable");
            return None;
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(file) => Some(file),
        Err(err) => {
            debug!(path = %path.display(), error = %err, "sidebar width target invalid");
            None
        }
    }
}

#[cfg(test)]
pub(crate) fn pinned(runtime: &RuntimePaths) -> Option<WidthPermille> {
    load_file(runtime)
        .filter(|file| file.pinned)
        .map(|file| file.permille)
}

/// Resolve the room target against the current backend geometry without changing it.
pub fn resolve(
    runtime: &RuntimePaths,
    width: SidebarWidth,
    view_cols: Option<u16>,
) -> crate::mux::SidebarTarget {
    let stored = load_file(runtime);
    let view_cols = view_cols.and_then(NonZeroU16::new);
    if view_cols.is_none()
        && let Some(file) = stored
    {
        return crate::mux::SidebarTarget {
            share: file.permille,
            max_cols: width.max_cols,
            pinned: file.pinned,
        };
    }
    let (permille, pinned) = match (stored, view_cols) {
        (Some(file), Some(_)) if file.pinned => (file.permille, true),
        (_, Some(view_cols)) => {
            let target_cols =
                u16::try_from(width.target_cols(u64::from(view_cols.get()))).unwrap_or(u16::MAX);
            (
                WidthPermille::from_cols(
                    NonZeroU16::new(target_cols).unwrap_or(NonZeroU16::MIN),
                    view_cols,
                ),
                false,
            )
        }
        _ => (
            WidthPermille::from_percent(width.percent.resolve(None)),
            false,
        ),
    };
    crate::mux::SidebarTarget {
        share: permille,
        max_cols: width.max_cols,
        pinned,
    }
}

/// Adopt the target derived from a proven viewport as the room-wide target.
pub(crate) fn adopt(
    runtime: &RuntimePaths,
    width: SidebarWidth,
    view_cols: NonZeroU16,
) -> crate::mux::SidebarTarget {
    let target = resolve(runtime, width, Some(view_cols.get()));
    let resolved = WidthTargetFile {
        permille: target.share,
        pinned: target.pinned,
    };
    if load_file(runtime) != Some(resolved)
        && let Err(err) = write_and_broadcast(runtime, resolved)
    {
        warn!(error = %err, "sidebar width target adopt write failed");
    }
    target
}

/// Pin a user-selected room target as its exact measured share of the view.
pub fn pin(
    runtime: &RuntimePaths,
    cols: NonZeroU16,
    view_cols: u16,
) -> atomic::Result<WidthPermille> {
    let view_cols = NonZeroU16::new(view_cols).ok_or_else(|| atomic::AtomicErr::Io {
        path: runtime.sidebar_width_path(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "sidebar width target needs nonzero view geometry",
        ),
    })?;
    let permille = WidthPermille::from_cols(cols, view_cols);
    write_and_broadcast(
        runtime,
        WidthTargetFile {
            permille,
            pinned: true,
        },
    )?;
    Ok(permille)
}

fn write_and_broadcast(runtime: &RuntimePaths, file: WidthTargetFile) -> atomic::Result<()> {
    atomic::write_temp_then_rename_cache(&runtime.sidebar_width_path(), &file)?;
    if let Err(err) = crate::sidebar::wakeup::broadcast(
        runtime,
        None,
        crate::sidebar::events::SidebarEvent::WidthTargetChanged,
    ) {
        debug!(error = %err, "sidebar width target broadcast failed");
    }
    Ok(())
}

/// Drop the room-runtime target so the next birth starts from config
/// defaults. Idempotent: a missing file is success.
pub fn clear(runtime: &RuntimePaths) -> std::io::Result<()> {
    match fs::remove_file(runtime.sidebar_width_path()) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
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
    fn adopt_tracks_unpinned_geometry_and_recovers_invalid_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = runtime(dir.path());
        assert_eq!(load_file(&runtime), None);

        let width = SidebarWidth::default();
        assert_eq!(
            adopt(&runtime, width, NonZeroU16::new(200).unwrap()).cols(Some(200)),
            NonZeroU16::new(50).expect("nonzero"),
        );
        assert_eq!(
            load_file(&runtime).map(|file| file.permille),
            Some(WidthPermille::from_percent(25))
        );
        assert_eq!(pinned(&runtime), None);
        assert_eq!(
            adopt(&runtime, width, NonZeroU16::new(300).unwrap()).cols(Some(300)),
            NonZeroU16::new(72).expect("nonzero"),
        );
        assert_ne!(
            load_file(&runtime).map(|file| file.permille),
            Some(WidthPermille::from_percent(25))
        );

        fs::write(runtime.sidebar_width_path(), b"not json").expect("garbage file");
        assert_eq!(
            adopt(&runtime, width, NonZeroU16::new(120).unwrap()).cols(Some(120)),
            NonZeroU16::new(30).expect("nonzero"),
        );

        fs::write(runtime.sidebar_width_path(), br#"{"cols":90}"#).expect("old record");
        assert_eq!(
            adopt(&runtime, width, NonZeroU16::new(120).unwrap()).cols(Some(120)),
            NonZeroU16::new(30).expect("nonzero"),
        );
    }

    #[test]
    fn fixed_percent_resolves_up_to_the_next_column() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = runtime(dir.path());
        let mut theme = crate::config::ThemeConfig::default();
        theme.display.width_percent = Some(30);
        let width = SidebarWidth::from_config(&theme);

        assert_eq!(
            resolve(&runtime, width, Some(213)).cols(Some(213)),
            NonZeroU16::new(64).expect("nonzero"),
        );
    }

    #[test]
    fn pin_preserves_the_measured_width_and_scales_with_the_view() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = runtime(dir.path());
        let cols = NonZeroU16::new(81).expect("nonzero");
        let share = pin(&runtime, cols, 200).expect("pin width target");
        assert_eq!(share, WidthPermille::try_from(405).expect("valid share"));
        assert_eq!(pinned(&runtime), Some(share));
        assert_eq!(
            resolve(&runtime, SidebarWidth::default(), Some(200)).cols(Some(200)),
            cols,
        );
        assert_eq!(
            resolve(&runtime, SidebarWidth::default(), Some(300)).cols(Some(300)),
            NonZeroU16::new(122).expect("nonzero"),
            "a pin scales and is not clamped by max_cols",
        );
    }

    #[test]
    fn max_cols_clamps_unpinned_policy_but_not_a_pin() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = runtime(dir.path());
        let width = SidebarWidth::default();
        assert_eq!(
            resolve(&runtime, width, Some(400)).cols(Some(400)),
            width.max_cols,
        );

        pin(&runtime, NonZeroU16::new(100).expect("nonzero"), 200).expect("pin width target");
        assert_eq!(
            resolve(&runtime, width, Some(400)).cols(Some(400)),
            NonZeroU16::new(200).expect("nonzero"),
        );
    }

    #[test]
    fn zellij_default_keeps_the_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = runtime(dir.path());
        let width = SidebarWidth::default();

        assert_eq!(
            resolve(&runtime, width, Some(400)).cols(Some(400)),
            width.max_cols,
        );
    }

    #[test]
    fn geometry_free_resolve_keeps_the_last_established_auto_share() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = runtime(dir.path());
        let width = SidebarWidth::default();

        let established = adopt(&runtime, width, NonZeroU16::new(250).unwrap());
        assert_eq!(established.cols(Some(250)), NonZeroU16::new(72).unwrap());
        let without_geometry = resolve(&runtime, width, None);
        assert_eq!(without_geometry.share, established.share);
        assert_eq!(
            without_geometry.cols(Some(250)),
            NonZeroU16::new(72).unwrap(),
            "a blind narrow Auto fallback must not replace the established share",
        );
    }

    #[test]
    fn unknown_geometry_keeps_the_configured_percentage_spelling() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = runtime(dir.path());
        let mut theme = crate::config::ThemeConfig::default();
        theme.display.width_percent = Some(40);
        let target = resolve(&runtime, SidebarWidth::from_config(&theme), None);
        assert_eq!(target.cols(None), theme.display.max_cols);
        assert_eq!(target.percent(), 40);
        assert_eq!(
            load_file(&runtime),
            None,
            "a blind fallback is not persisted"
        );

        clear(&runtime).expect("clear explicit target");
        theme.display.width_percent = None;
        theme.pets.enabled = true;
        let target = resolve(&runtime, SidebarWidth::from_config(&theme), None);
        assert_eq!(target.cols(None), theme.display.max_cols);
        assert_eq!(target.percent(), 30);
    }

    #[test]
    fn clear_removes_a_target_and_accepts_a_missing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = runtime(dir.path());
        pin(&runtime, NonZeroU16::new(81).expect("nonzero"), 200).expect("pin target");
        clear(&runtime).expect("clear target");
        assert_eq!(load_file(&runtime), None);
        clear(&runtime).expect("clear missing target");
    }
}
