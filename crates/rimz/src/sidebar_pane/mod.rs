//! Native pane-resident sidebar process: the serve loop ([`app`]) and the
//! snapshot renderer ([`render`]).
//!
//! `app` owns the fixed-timestep serve loop, two-speed fetch, last-known-good
//! gate, health/give-up, selection, input codec, reload-in-place, and producer
//! election. `render` projects [`crate::SidebarSnapshot`] into the terminal
//! frame. Neither owns store decisions; those stay in [`crate::sidebar`] and
//! [`crate::store`].

pub mod app;
pub mod pets;
pub(crate) mod pixel;
pub use pixel::{
    PixelRenderCaps, detect_pixel_render_env, encode_png, inline_placeholder_row,
    transmit_png_chunks, virtual_place, wrap_pixel_payload, write_synchronized_pixel_output,
};
pub mod render;
pub mod supervise;
pub mod view;
