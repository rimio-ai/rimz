//! Live backend suites. The mux suites self-skip when their binary is absent,
//! and the browser suite self-skips without ttyd, Chromium, and its selected
//! mux; the shared `CommandSpec` engine suite needs only coreutils.

mod command;
mod single_backend_room;
mod tmux;
mod web;
mod zellij;
