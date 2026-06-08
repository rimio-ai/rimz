//! Native terminal sidebar renderer and runtime loop.
//!
//! This module projects the [`crate::SidebarSnapshot`] view-model into the
//! terminal frame and owns the pane-resident serve loop. It does not own ledger
//! decisions; those stay in [`crate::sidebar`] and [`crate::ledger`].

pub mod app;
mod osc;
pub mod render;
