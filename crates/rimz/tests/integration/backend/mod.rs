//! Live multiplexer backend suites. The mux suites self-skip when their
//! binary is absent, so the matrix stays green on machines without tmux or
//! Zellij; the shared `CommandSpec` engine suite needs only coreutils, so it
//! never skips.

mod command;
mod tmux;
mod zellij;
