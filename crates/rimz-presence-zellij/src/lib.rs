//! Pure core of the Zellij presence plugin.
//!
//! The plugin watches Zellij's pane/tab manifests, asks [`policy`] which pokes
//! are due, and asks [`wire`] to render the host-facing argv/KDL payloads. The
//! decision core and wire seam stay free of `zellij-tile` types and unit-test on
//! the host target. The wasm shell in `main.rs` only projects Zellij events,
//! gathers runtime telemetry, and executes the resulting outputs.

pub mod policy;
pub mod wire;
