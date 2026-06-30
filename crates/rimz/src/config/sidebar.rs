use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};

pub const DEFAULT_AFK_AFTER_SECS: u32 = 15 * 60;

/// Sidebar behavior preferences.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct SidebarConfig {
    /// The cockpit and provider stats headline window: the current session
    /// burst, trailing 24 hours, or the local calendar day.
    #[serde(default)]
    pub spend_window: crate::agents::spending::SpendWindowMode,
    /// Preferred comparison target for the worktree header's git stats (the
    /// `+/-` diff, the `⇡`/`⇣` commit delta, and the `≡`/`✓` landed markers).
    /// Tried
    /// first in the trunk ladder, per repo: a repo where the branch doesn't
    /// resolve falls back to the `main` → `master` → remote-default detection,
    /// so one machine-wide value never breaks other projects. Unset means
    /// detection alone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trunk: Option<String>,
    /// The global multiplexer chord that focuses the sidebar from any pane — a
    /// toggle, so pressing it again returns to your last working pane. Rimz
    /// registers it room-scoped at session birth (tmux as a `bind-key`, Zellij
    /// through the presence plugin), so it never touches your global config.
    /// Default `Alt+p`; set empty or `off` to register nothing and leave your
    /// keybinds untouched.
    pub focus_key: String,
    /// Seconds of input idle before the footer shows the AFK badge. tmux
    /// reports per-client input idle, so this drives `zᶻ idle · Nm`; Zellij
    /// reports attach state only, so it shows `zᶻ away` on full detach
    /// regardless of this value. 15 minutes by default.
    pub afk_after_secs: NonZeroU32,
}

impl Default for SidebarConfig {
    fn default() -> Self {
        Self {
            spend_window: Default::default(),
            trunk: None,
            focus_key: default_focus_key(),
            afk_after_secs: NonZeroU32::new(DEFAULT_AFK_AFTER_SECS)
                .expect("non-zero default afk window"),
        }
    }
}

impl SidebarConfig {
    /// The focus-sidebar chord to register and display, or `None` when the user
    /// disabled it (empty / `off` / `none`).
    pub fn focus_key_label(&self) -> Option<&str> {
        let key = self.focus_key.trim();
        if key.is_empty() || key.eq_ignore_ascii_case("off") || key.eq_ignore_ascii_case("none") {
            None
        } else {
            Some(key)
        }
    }

    pub fn afk_after_ms(&self) -> u64 {
        u64::from(self.afk_after_secs.get()) * 1_000
    }
}

/// The shipped default focus-sidebar chord: `Alt+p`, a toggle that reaches the
/// sidebar and returns to the last pane. `Alt` survives the terminal and
/// Zellij's locked mode; the user can rebind or disable it.
pub fn default_focus_key() -> String {
    "Alt+p".to_owned()
}
