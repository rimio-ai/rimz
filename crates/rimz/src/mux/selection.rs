//! Backend selection precedence per `DESIGN.md`:
//!
//! 1. explicit `--mux <name>` flag
//! 2. environment auto-detect (`ZELLIJ` / `TMUX`)
//! 3. installed binary (Zellij preferred when both are present)

use super::MuxErr;
use crate::ids::MuxName;

pub type Result<T> = std::result::Result<T, MuxErr>;

pub fn auto_detect_backend(explicit: Option<MuxName>) -> Result<MuxName> {
    if let Some(mux) = explicit {
        return Ok(mux);
    }
    if std::env::var_os("ZELLIJ").is_some() || std::env::var_os("ZELLIJ_PANE_ID").is_some() {
        return Ok(MuxName::Zellij);
    }
    if std::env::var_os("TMUX").is_some() || std::env::var_os("TMUX_PANE").is_some() {
        return Ok(MuxName::Tmux);
    }
    if which::which("zellij").is_ok() {
        return Ok(MuxName::Zellij);
    }
    if which::which("tmux").is_ok() {
        return Ok(MuxName::Tmux);
    }
    Err(MuxErr::NoMuxFound)
}
