//! Native sidebar supervisors, the shared room host, and snapshot rendering.
//!
//! [`app`] owns attachment loops, retained host inputs, fetch, last-known-good gates, health, selection, input, and reload. [`render`] projects [`crate::store::snapshot::SidebarSnapshot`] into terminal cells. Neither owns store decisions; those stay in [`crate::sidebar`] and [`crate::store`].

pub mod app;
mod attach;
pub mod host;
pub mod pets;
pub(crate) mod pixel;
pub use pixel::probe::{ZellijKittySupport, probe_zellij_kitty};
pub use pixel::{
    LiveGraphicsPacer, PixelRenderCaps, detect_pixel_render_env, encode_png,
    inline_placeholder_row, transmit_png, virtual_place, wrap_pixel_payload,
    write_synchronized_pixel_output,
};
pub mod render;
pub mod supervise;
pub mod view;
