//! Settings asserted on one Zellij session and its last managed attach.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{
    MachineConfig, MultiplexerConfig, MuxConfig, RoomConfig, TmuxConfig, ZellijConfig,
};
use crate::disk::{atomic, lock::WorkspaceLock, paths::RuntimePaths};
use crate::ids::MuxName;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppliesWhen {
    Reload,
    RoomStart,
    Attach,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingSetting {
    pub key: String,
    pub configured: Option<Value>,
    pub recorded: Option<Value>,
    pub when: AppliesWhen,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PendingSettings {
    BeforeTracking,
    Tracked(Vec<PendingSetting>),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Record {
    version: u32,
    session: Option<BTreeMap<String, Value>>,
    client: Option<BTreeMap<String, Value>>,
}

impl Default for Record {
    fn default() -> Self {
        Self {
            version: 1,
            session: None,
            client: None,
        }
    }
}

fn classes(config: &MachineConfig, mux: MuxName) -> BTreeMap<&'static str, AppliesWhen> {
    use AppliesWhen::{Attach, Reload, RoomStart};
    let mut table = BTreeMap::new();
    macro_rules! classify {
        ($value:expr, $ty:ident, $prefix:literal, {$($field:ident: $when:expr),* $(,)?}) => {
            let $ty { $($field: _,)* } = $value;
            $(if let Some(when) = $when {
                table.insert(concat!($prefix, ".", stringify!($field)), when);
            })*
        };
    }
    classify!(&config.mux, MuxConfig, "mux", {
        default: None,
        status_bar: Some(if mux == MuxName::Tmux { Reload } else { RoomStart }),
    });
    classify!(&config.room, RoomConfig, "room", { style: Some(Reload) });
    classify!(&config.zellij, ZellijConfig, "zellij", {
        mouse_mode: Some(Attach),
        mouse_click_through: Some(Reload),
        advanced_mouse_actions: Some(Reload),
        mouse_hover_effects: Some(Reload),
        focus_follows_mouse: Some(Reload),
        pane_frames: Some(Reload),
        on_force_close: Some(RoomStart),
        scroll_buffer_size: Some(RoomStart),
        show_startup_tips: Some(RoomStart),
        show_release_notes: Some(RoomStart),
        copy_clipboard: Some(Reload),
        copy_on_select: Some(Reload),
        support_kitty_keyboard_protocol: Some(Attach),
        osc8_hyperlinks: Some(RoomStart),
        session_serialization: Some(RoomStart),
        disable_session_metadata: Some(RoomStart),
    });
    classify!(&config.tmux, TmuxConfig, "tmux", {
        mouse: Some(Reload), focus_events: Some(Reload), history_limit: Some(Reload),
        allow_passthrough: Some(Reload), set_clipboard: Some(Reload),
        extended_keys: Some(Reload), extended_keys_format: Some(Reload),
        escape_time_ms: Some(Reload), renumber_windows: Some(Reload),
        aggressive_resize: Some(Reload), pane_border_status: Some(Reload),
        pane_border_lines: Some(Reload),
    });
    table.insert("sidebar.focus_key", Reload);
    table.insert("sidebar.zoom_key", Reload);
    for key in [
        "keys.keymap",
        "keys.prefix",
        "keys.bind.normal",
        "keys.bind.prefix",
        "keys.bind.resize",
        "keys.bind.move",
    ] {
        table.insert(key, Reload);
    }
    table
}

impl Record {
    fn replace_session(&mut self, config: &MultiplexerConfig) {
        let mut session = option_values(&super::zellij::zellij_session_options(&config.zellij));
        session.insert(
            "mux.status_bar".into(),
            serde_json::json!(config.status_bar),
        );
        self.session = Some(session);
        self.client = None;
    }

    fn merge_live(&mut self, options: &[super::zellij::ZellijSessionOption]) {
        let Some(session) = &mut self.session else {
            return;
        };
        let table = classes(&MachineConfig::default(), MuxName::Zellij);
        session.extend(
            option_values(options)
                .into_iter()
                .filter(|(key, _)| table.get(key.as_str()) == Some(&AppliesWhen::Reload)),
        );
    }

    fn replace_client(&mut self, config: &ZellijConfig) {
        self.client = Some(option_values(&super::zellij::zellij_client_options(config)));
    }
}

fn option_values(options: &[super::zellij::ZellijSessionOption]) -> BTreeMap<String, Value> {
    options
        .iter()
        .copied()
        .map(super::zellij::ZellijSessionOption::setting)
        .filter(|(key, _)| !matches!(*key, "auto_layout" | "stacked_resize" | "stacked_pane_list"))
        .map(|(key, value)| (format!("zellij.{key}"), value))
        .collect()
}

fn diff(record: Option<&Record>, config: &MachineConfig, mux: MuxName) -> PendingSettings {
    if mux == MuxName::Tmux {
        return PendingSettings::Tracked(Vec::new());
    }
    let Some(record) = record else {
        return PendingSettings::BeforeTracking;
    };
    let Some(session) = &record.session else {
        return PendingSettings::BeforeTracking;
    };
    let mut desired = Record::default();
    desired.replace_session(&MultiplexerConfig::from(config));
    desired.replace_client(&config.zellij);
    let mut pending = Vec::new();
    for (key, mut when) in classes(config, mux) {
        let (old, current) = if when == AppliesWhen::Attach {
            let Some(client) = &record.client else {
                continue;
            };
            (
                client.get(key),
                desired.client.as_ref().and_then(|map| map.get(key)),
            )
        } else {
            (
                session.get(key),
                desired.session.as_ref().and_then(|map| map.get(key)),
            )
        };
        if when == AppliesWhen::Reload {
            if old.is_none() || current.is_some() {
                continue;
            }
            when = AppliesWhen::RoomStart;
        }
        if old != current {
            pending.push(PendingSetting {
                key: key.into(),
                configured: if key == "zellij.mouse_mode" {
                    config.zellij.mouse_mode.map(Value::Bool)
                } else {
                    current.cloned()
                },
                recorded: old.cloned(),
                when,
            });
        }
    }
    PendingSettings::Tracked(pending)
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SettingsWriteErr {
    #[error(transparent)]
    Lock(#[from] crate::disk::lock::LockErr),
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
}

fn read(runtime: &RuntimePaths) -> Option<Record> {
    let bytes = std::fs::read(runtime.lane_path("room-settings.json")).ok()?;
    serde_json::from_slice::<Record>(&bytes)
        .ok()
        .filter(|record| record.version == 1)
}

fn update(
    runtime: &RuntimePaths,
    change: impl FnOnce(&mut Record) -> bool,
) -> Result<(), SettingsWriteErr> {
    let _lock = WorkspaceLock::acquire(&runtime.lock_path("room-settings.lock"))?;
    let mut record = read(runtime).unwrap_or_default();
    if change(&mut record) {
        atomic::write_temp_then_rename_cache(&runtime.lane_path("room-settings.json"), &record)?;
    }
    Ok(())
}

pub(crate) fn record_birth(
    runtime: &RuntimePaths,
    config: &MultiplexerConfig,
) -> Result<(), SettingsWriteErr> {
    update(runtime, |record| {
        record.replace_session(config);
        true
    })
}

pub(crate) fn record_live(
    runtime: &RuntimePaths,
    options: &[super::zellij::ZellijSessionOption],
) -> Result<(), SettingsWriteErr> {
    update(runtime, |record| {
        if record.session.is_none() {
            return false;
        }
        record.merge_live(options);
        true
    })
}

pub(crate) fn record_attach(
    runtime: &RuntimePaths,
    config: &ZellijConfig,
) -> Result<(), SettingsWriteErr> {
    update(runtime, |record| {
        record.replace_client(config);
        true
    })
}

/// Compare a live room with machine intent; tmux has no pending settings.
pub(crate) fn pending(
    runtime: &RuntimePaths,
    config: &MachineConfig,
    mux: MuxName,
) -> PendingSettings {
    diff(read(runtime).as_ref(), config, mux)
}

#[cfg(test)]
mod tests;
