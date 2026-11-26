//! RimZ's MCP server: `rimz mcp serve`, which a Claude, Codex, Antigravity, or
//! OpenCode launch starts over stdio, the catalog of the tools it serves, and their
//! wording. Each tool call but `guide`'s runs in the server's own process,
//! through the runner `serve` is handed, and answers with data or text;
//! `guide` answers from the wording read at the server's start. This module
//! holds the protocol, the catalog and the wording, and reaches the rest of
//! RimZ only through that runner, so it opens no store, loads no config, and
//! resolves no room itself.

pub mod catalog;
pub mod launch_env;
mod runner;
mod server;
pub mod wording;

pub use runner::{Answer, Failure, Runner};
pub use server::{ServeErr, serve};
