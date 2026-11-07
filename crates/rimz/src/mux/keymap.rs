//! Best-effort installation of the resolved machine keymap at room entry and reload.

use std::path::Path;

use crate::config::{SidebarConfig, keys::KeyChord, keys::KeyMode};

pub(crate) fn register_room_keys(
    backend: &dyn super::MuxBackend,
    config: &crate::config::MachineConfig,
    rimz_bin: &Path,
) {
    let keymap = config.room_keymap();
    if !keymap.modes.contains_key(&KeyMode::Prefix) {
        for (name, key) in [
            ("focus_key", &config.sidebar.focus_key),
            ("zoom_key", &config.sidebar.zoom_key),
        ] {
            if let Some(label) = SidebarConfig::key_label(key)
                && KeyChord::parse(label).is_none_or(|key| !key.is_modified())
            {
                tracing::warn!(
                    key = name,
                    value = label,
                    "ignoring invalid [sidebar] key; expected e.g. Alt+p"
                );
            }
        }
    }
    if let Err(err) = backend.apply_room_keymap(&keymap, rimz_bin) {
        tracing::debug!(error = %err, "registering room keymap failed");
    }
}
