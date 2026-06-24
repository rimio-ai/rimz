//! Native pane-resident sidebar process: the serve loop ([`app`]) and the
//! snapshot renderer ([`render`]).
//!
//! `app` owns the fixed-timestep serve loop, two-speed fetch, last-known-good
//! gate, health/give-up, selection, input codec, reload-in-place, and producer
//! election. `render` projects [`crate::SidebarSnapshot`] into the terminal
//! frame. Neither owns ledger decisions; those stay in [`crate::sidebar`] and
//! [`crate::ledger`].

pub mod app;
mod osc;
pub mod pets;
pub mod render;
