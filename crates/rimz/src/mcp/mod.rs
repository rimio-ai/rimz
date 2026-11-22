//! RimZ's MCP server: `rimz mcp serve`, which a Claude, Codex, or Antigravity
//! launch starts over stdio, the catalog of the tools it serves, and their
//! wording. The server is a thin front: each tool call but `guide`'s runs one
//! `rimz` one-shot with an argv array built from the call's arguments, so a
//! tool behaves exactly as its command does, and `guide` answers from the
//! wording read at the server's start. It opens no store, loads no config, and
//! resolves no room; the one-shots do.

pub mod catalog;
pub mod launch_env;
mod server;
pub mod wording;

pub use server::{ServeErr, serve};
