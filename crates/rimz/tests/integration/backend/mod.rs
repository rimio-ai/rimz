//! Live multiplexer backend suites. Each self-skips when its mux binary is
//! absent, so the matrix stays green on machines without tmux or Zellij.

mod tmux;
mod zellij;
