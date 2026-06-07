//! Pure core of the Zellij presence plugin.
//!
//! The plugin watches Zellij's pane/tab manifests, runs one fixed `rimz sidebar
//! wake` argv when the room's stable shape changes, and broadcasts when a
//! switched-to tab restored focus to Rimz's sidebar. Everything
//! decision-shaped — what counts as a change, how bursts coalesce, when the
//! keepalive fires, and which switched tab is stranded — lives in [`policy`],
//! which is free of `zellij-tile` types and unit-tests on the host target. The
//! wasm shell in `main.rs` only projects Zellij events into the policy's inputs
//! and executes the resulting wake pokes.

pub mod policy;
