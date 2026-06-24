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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_capability_values_match_adapters() {
        assert_eq!(view_kind(MuxName::Zellij), ViewKind::Tab);
        assert_eq!(view_kind(MuxName::Tmux), ViewKind::Window);

        assert!(lists_full_cmdline(MuxName::Zellij));
        assert!(!lists_full_cmdline(MuxName::Tmux));

        assert!(!wraps_osc_passthrough(MuxName::Zellij));
        assert!(wraps_osc_passthrough(MuxName::Tmux));

        assert!(drops_desktop_osc(MuxName::Zellij));
        assert!(!drops_desktop_osc(MuxName::Tmux));
    }
}
