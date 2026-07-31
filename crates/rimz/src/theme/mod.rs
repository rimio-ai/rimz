//! Renderer-neutral interface theme resolution.
//!
//! Schemes become raw palettes, semantic slots resolve at the requested color
//! depth, glyph sets resolve from one built-in catalog plus configured
//! overrides, and renderer edges convert [`Tone`] into their terminal carrier.
//! Provider identity, setup probes, and shared value formats live here so human
//! surfaces use one vocabulary without coupling the core to ratatui or anstyle.

mod glyphs;
mod identity;
pub(crate) mod oklab;
mod palette;
mod provider;
mod raw;
pub mod scheme;
mod tone;

pub mod fmt;

#[cfg(test)]
pub(crate) use glyphs::unicode_glyph;
pub(crate) use glyphs::{GlyphSet, GlyphSetKind};
pub use glyphs::{
    agent_status_glyph_role, nerd_font_probe_glyphs, nerd_font_probe_gradient,
    strip_status_glyph_suffix, theme_glyphs,
};
pub use identity::Identity;
pub(crate) use palette::HEAT_RAMP_WARM_START;
pub use palette::{Palette, ramp_tone};
pub use provider::{
    BrandColor, ResolvedProviderIdentity, provider_title_case, resolve_provider_brand,
    resolve_provider_identity,
};
pub use tone::Tone;
