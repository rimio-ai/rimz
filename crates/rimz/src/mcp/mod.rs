//! RimZ's MCP server: `rimz mcp serve`, which a Claude, Codex, or Antigravity
//! launch starts over stdio, and the catalog of the tools it serves. The server is a thin
//! front: each tool call runs one `rimz` one-shot with an argv array built
//! from the call's arguments, so a tool behaves exactly as its command does.
//! It opens no store, loads no config, and resolves no room; the one-shots do.

pub mod catalog;
mod server;

pub use server::{ServeErr, serve};
