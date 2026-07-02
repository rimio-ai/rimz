//! Static backend facts exposed through the mux seam.

use crate::ids::{MuxName, ViewKind};

pub fn view_kind(mux: MuxName) -> ViewKind {
    match mux {
        MuxName::Zellij => ViewKind::Tab,
        MuxName::Tmux => ViewKind::Window,
    }
}

pub fn lists_full_cmdline(mux: MuxName) -> bool {
    match mux {
        MuxName::Zellij => true,
        MuxName::Tmux => false,
    }
}

pub fn wraps_osc_passthrough(mux: MuxName) -> bool {
    match mux {
        MuxName::Zellij => false,
        MuxName::Tmux => true,
    }
}

pub fn drops_desktop_osc(mux: MuxName) -> bool {
    match mux {
        MuxName::Zellij => true,
        MuxName::Tmux => false,
    }
}
