//! Pure core of the Zellij presence plugin.
//!
//! The plugin is signal-only: it watches Zellij's pane/tab manifests and runs
//! one fixed `rimz sidebar wake` argv when the room's stable shape changes,
//! plus a keepalive on a fixed cadence. Everything decision-shaped — what
//! counts as a change, how bursts coalesce, when the keepalive fires — lives
//! in [`policy`], which is free of `zellij-tile` types and unit-tests on the
//! host target. The wasm shell in `main.rs` only projects Zellij events into
//! the policy's inputs and executes the pokes it returns.

pub mod policy;
