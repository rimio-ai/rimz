//! Renderer-neutral interface theme resolution.
//!
//! Schemes become raw palettes, semantic slots resolve at the requested color
//! depth, and renderer edges convert [`Tone`] into their terminal carrier.
//! Provider identity and shared value formats live here so human surfaces use
//! one vocabulary without coupling the core to ratatui or anstyle.

mod identity;
pub(crate) mod oklab;
mod palette;
mod provider;
mod raw;
pub mod scheme;
mod tone;

pub mod fmt;

pub use identity::Identity;
pub(crate) use palette::HEAT_RAMP_WARM_START;
pub use palette::{Palette, ramp_tone};
pub use provider::{
    BrandColor, ResolvedProviderIdentity, provider_title_case, resolve_provider_identity,
};
pub use tone::Tone;
