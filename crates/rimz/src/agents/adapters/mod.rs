//! Private built-in and process-plugin integration implementations.
//!
//! One directory per agent kind, each the single place that agent's protocol is
//! normalized: native payloads, parsers, installer, probes, and the capability
//! implementations behind them. Everything above this module consumes neutral
//! results. See
//! [docs/internals/agents/adapter.md](../../../../../docs/internals/agents/adapter.md).

// Provider modules historically lived directly below `agents` and use
// `super::` for neutral domain types. Keep that source vocabulary while the
// compiler enforces the private adapters boundary.
use super::*;

pub(super) mod amp;
pub(super) mod antigravity;
pub(super) mod claude;
pub(super) mod codex;
pub(super) mod copilot;
pub(super) mod cursor;
pub(super) mod droid;
pub(super) mod grok;
pub(super) mod kimi;
pub(super) mod kiro;
pub(super) mod opencode;
pub(super) mod pi;
pub(super) mod plugin;
pub(super) mod qwen;
