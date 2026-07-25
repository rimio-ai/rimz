//! One room-runtime sidebar width target shared by every renderer.

use std::fs;
use std::num::NonZeroU16;

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::ids::MuxName;
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

/// Read the current target share without resolving it against view geometry.
pub fn load(runtime: &RuntimePaths) -> Option<WidthPermille> {
    load_file(runtime).map(|file| file.permille)
}

/// Read the target only when the user has pinned it.
pub fn pinned(runtime: &RuntimePaths) -> Option<WidthPermille> {
    load_file(runtime)
        .filter(|file| file.pinned)
        .map(|file| file.permille)
}

/// Resolve the room target against the current backend geometry.
pub fn resolve(
    runtime: &RuntimePaths,
    width: SidebarWidth,
    mux: MuxName,
    view_cols: Option<u16>,
) -> crate::mux::SidebarTarget {
    let stored = load_file(runtime);
    let pinned = stored.is_some_and(|file| file.pinned);
    let view_cols = view_cols.and_then(NonZeroU16::new);
    let permille = if pinned {
        stored
            .map(|file| file.permille)
            .unwrap_or_else(|| WidthPermille::from_percent(width.percent.resolve(None)))
    } else if let Some(view_cols) = view_cols {
        let target_cols =
            u16::try_from(width.target_cols(u64::from(view_cols.get()))).unwrap_or(u16::MAX);
        WidthPermille::from_cols(
            NonZeroU16::new(target_cols).unwrap_or(NonZeroU16::MIN),
            view_cols,
        )
    } else {
        WidthPermille::from_percent(width.percent.resolve(None))
    };
    let permille = permille.snap_to_rung(mux);
    let resolved = WidthTargetFile { permille, pinned };
    if stored != Some(resolved)
        && let Err(err) = write_and_broadcast(runtime, resolved)
    {
        warn!(error = %err, "sidebar width target resolve write failed");
    }
    crate::mux::SidebarTarget {
        cols: view_cols.map_or(width.max_cols, |view_cols| permille.cols(view_cols)),
        percent: permille.to_percent_rounded(),
    }
}

/// Pin a user-selected room target as a backend-native share of the view.
pub fn pin(
    runtime: &RuntimePaths,
    cols: NonZeroU16,
    mux: MuxName,
    view_cols: u16,
) -> atomic::Result<WidthPermille> {
    let view_cols = NonZeroU16::new(view_cols).ok_or_else(|| atomic::AtomicErr::Io {
        path: runtime.sidebar_width_path(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "sidebar width target needs nonzero view geometry",
        ),
    })?;
    let permille = WidthPermille::from_cols(cols, view_cols).snap_to_rung(mux);
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
    if let Err(err) = crate::store::wakeup::broadcast_sidebar_event(
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
    fn resolve_tracks_unpinned_geometry_and_recovers_invalid_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = runtime(dir.path());
        assert_eq!(load(&runtime), None);

        let width = SidebarWidth::default();
        assert_eq!(
            resolve(&runtime, width, MuxName::Zellij, Some(200)).cols,
            NonZeroU16::new(50).expect("nonzero"),
        );
        assert_eq!(load(&runtime), Some(WidthPermille::from_percent(25)));
        assert_eq!(pinned(&runtime), None);
        assert_eq!(
            resolve(&runtime, width, MuxName::Tmux, Some(300)).cols,
            NonZeroU16::new(72).expect("nonzero"),
        );
        assert_ne!(load(&runtime), Some(WidthPermille::from_percent(25)));

        fs::write(runtime.sidebar_width_path(), b"not json").expect("garbage file");
        assert_eq!(
            resolve(&runtime, width, MuxName::Tmux, Some(120)).cols,
            NonZeroU16::new(30).expect("nonzero"),
        );

        fs::write(runtime.sidebar_width_path(), br#"{"cols":90}"#).expect("old record");
        assert_eq!(
            resolve(&runtime, width, MuxName::Tmux, Some(120)).cols,
            NonZeroU16::new(30).expect("nonzero"),
        );
    }

    #[test]
    fn pin_snaps_and_resolve_scales_the_pin_with_the_view() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = runtime(dir.path());
        let cols = NonZeroU16::new(81).expect("nonzero");
        let snapped = pin(&runtime, cols, MuxName::Zellij, 200).expect("pin width target");
        assert_eq!(snapped, WidthPermille::from_percent(40));
        assert_eq!(pinned(&runtime), Some(snapped));
        assert_eq!(
            resolve(
                &runtime,
                SidebarWidth::default(),
                MuxName::Zellij,
                Some(200),
            )
            .cols,
            NonZeroU16::new(80).expect("nonzero"),
        );
        assert_eq!(
            resolve(
                &runtime,
                SidebarWidth::default(),
                MuxName::Zellij,
                Some(300),
            )
            .cols,
            NonZeroU16::new(120).expect("nonzero"),
            "a pin scales and is not clamped by max_cols",
        );
    }

    #[test]
    fn max_cols_clamps_unpinned_policy_but_not_a_pin() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = runtime(dir.path());
        let width = SidebarWidth::default();
        assert_eq!(
            resolve(&runtime, width, MuxName::Tmux, Some(400)).cols,
            width.max_cols,
        );

        pin(
            &runtime,
            NonZeroU16::new(100).expect("nonzero"),
            MuxName::Tmux,
            200,
        )
        .expect("pin width target");
        assert_eq!(
            resolve(&runtime, width, MuxName::Tmux, Some(400)).cols,
            NonZeroU16::new(200).expect("nonzero"),
        );
    }

    #[test]
    fn unknown_geometry_keeps_the_configured_percentage_spelling() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = runtime(dir.path());
        let mut theme = crate::config::ThemeConfig::default();
        theme.display.width_percent = Some(40);
        let target = resolve(
            &runtime,
            SidebarWidth::from_config(&theme),
            MuxName::Zellij,
            None,
        );
        assert_eq!(target.cols, theme.display.max_cols);
        assert_eq!(target.percent, 40);

        clear(&runtime).expect("clear explicit target");
        theme.display.width_percent = None;
        theme.pets.enabled = true;
        let target = resolve(
            &runtime,
            SidebarWidth::from_config(&theme),
            MuxName::Zellij,
            None,
        );
        assert_eq!(target.cols, theme.display.max_cols);
        assert_eq!(target.percent, 30);
    }

    #[test]
    fn clear_removes_a_target_and_accepts_a_missing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = runtime(dir.path());
        pin(
            &runtime,
            NonZeroU16::new(81).expect("nonzero"),
            MuxName::Tmux,
            200,
        )
        .expect("pin target");
        clear(&runtime).expect("clear target");
        assert_eq!(load(&runtime), None);
        clear(&runtime).expect("clear missing target");
    }
}
