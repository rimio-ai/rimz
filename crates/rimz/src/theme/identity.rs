//! External-identity tones — the fixed colors the sidebar pins independent of
//! any scheme: the Claude brand clay, reused as the live-agent and
//! working-spinner accent, and the dollar green of money figures. These never
//! derive from the imported palette; only depth quantization applies
//! downstream.
//!
//! Per-agent provider brand colors live with each agent's definition
//! ([`SidebarProviderPanel::color_rgb`](crate::SidebarProviderPanel)), not
//! here — identity holds only the tones the sidebar itself owns.

use super::oklab::Rgb;

/// A fixed-hue tone with an external meaning, resolved at the active depth like
/// any other but never retuned by the scheme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Identity {
    /// Claude brand clay (`#d97757`) — the live-agent glyph and working spinners.
    Claude,
    /// Dollar green (`#85bb65`) for money figures.
    Money,
}

impl Identity {
    pub const fn base_rgb(self) -> Rgb {
        match self {
            Self::Claude => (0xd9, 0x77, 0x57),
            Self::Money => (0x85, 0xbb, 0x65),
        }
    }
}
