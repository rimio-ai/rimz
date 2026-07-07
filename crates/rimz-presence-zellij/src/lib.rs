//! Pure core of the Zellij presence plugin.
//!
//! The [`engine`] owns the room model and returns effects for the shell to
//! execute, [`policy`] holds pure helpers and timing state machines, and
//! [`wire`] renders host-facing argv/KDL payloads. These modules stay free of
//! `zellij-tile` types and unit-test on the host target. The wasm shell in
//! `main.rs` only projects Zellij events, gathers runtime telemetry, and
//! executes effects.

pub mod engine;
pub mod policy;
pub mod wire;
