//! Configurable sidebar navigation keymap.

use crate::config::SidebarKeys;
use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use super::input::{
    KEY_BOTTOM, KEY_DOWN, KEY_PAGE_DOWN, KEY_PAGE_UP, KEY_SCREEN_BOTTOM, KEY_SCREEN_TOP, KEY_TOP,
    KEY_UP, KEY_WIDTH_NARROWER, KEY_WIDTH_WIDER, KEY_WORKTREE_DOWN, KEY_WORKTREE_UP,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChordCode {
    Char(char),
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Enter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct KeyChord {
    code: ChordCode,
    ctrl: bool,
    alt: bool,
}

impl KeyChord {
    fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        let parts = raw.split(['+', '-']).collect::<Vec<_>>();
        let (base, modifiers) = parts.split_last()?;
        if base.trim().is_empty() || modifiers.iter().any(|part| part.trim().is_empty()) {
            return None;
        }
        let mut ctrl = false;
        let mut alt = false;
        for modifier in modifiers {
            match modifier.trim().to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "c" => ctrl = true,
                "alt" | "meta" | "m" => alt = true,
                _ => return None,
            }
        }
        Some(Self {
            code: parse_code(base.trim())?,
            ctrl,
            alt,
        })
    }

    fn matches(&self, code: KeyCode, mods: KeyModifiers) -> bool {
        self.ctrl == mods.contains(KeyModifiers::CONTROL)
            && self.alt == mods.contains(KeyModifiers::ALT)
            && match (self.code, code) {
                (ChordCode::Char(expected), KeyCode::Char(actual)) => expected == actual,
                (ChordCode::Up, KeyCode::Up) => true,
                (ChordCode::Down, KeyCode::Down) => true,
                (ChordCode::Left, KeyCode::Left) => true,
                (ChordCode::Right, KeyCode::Right) => true,
                (ChordCode::Home, KeyCode::Home) => true,
                (ChordCode::End, KeyCode::End) => true,
                (ChordCode::PageUp, KeyCode::PageUp) => true,
                (ChordCode::PageDown, KeyCode::PageDown) => true,
                (ChordCode::Enter, KeyCode::Enter) => true,
                _ => false,
            }
    }
}

fn parse_code(raw: &str) -> Option<ChordCode> {
    let mut chars = raw.chars();
    let first = chars.next()?;
    if chars.next().is_none() {
        return Some(ChordCode::Char(first));
    }
    match raw.to_ascii_lowercase().as_str() {
        "up" => Some(ChordCode::Up),
        "down" => Some(ChordCode::Down),
        "left" => Some(ChordCode::Left),
        "right" => Some(ChordCode::Right),
        "home" => Some(ChordCode::Home),
        "end" => Some(ChordCode::End),
        "pageup" => Some(ChordCode::PageUp),
        "pagedown" => Some(ChordCode::PageDown),
        "enter" => Some(ChordCode::Enter),
        "space" => Some(ChordCode::Char(' ')),
        _ => None,
    }
}

#[derive(Clone, Debug)]
pub struct NavKeymap {
    bindings: Vec<(KeyChord, &'static str)>,
}

impl NavKeymap {
    pub fn from_config(keys: &SidebarKeys) -> Self {
        let mut bindings = Vec::new();
        for (spec, wire) in [
            (keys.narrower.as_str(), KEY_WIDTH_NARROWER),
            (keys.wider.as_str(), KEY_WIDTH_WIDER),
            (keys.up.as_str(), KEY_UP),
            (keys.down.as_str(), KEY_DOWN),
            (keys.top.as_str(), KEY_TOP),
            (keys.bottom.as_str(), KEY_BOTTOM),
            (keys.worktree_up.as_str(), KEY_WORKTREE_UP),
            (keys.worktree_down.as_str(), KEY_WORKTREE_DOWN),
            (keys.page_up.as_str(), KEY_PAGE_UP),
            (keys.page_down.as_str(), KEY_PAGE_DOWN),
            (keys.screen_top.as_str(), KEY_SCREEN_TOP),
            (keys.screen_bottom.as_str(), KEY_SCREEN_BOTTOM),
        ] {
            for token in spec.split_whitespace() {
                match KeyChord::parse(token) {
                    Some(chord) => bindings.push((chord, wire)),
                    None => tracing::warn!(
                        binding = token,
                        action = wire,
                        "invalid sidebar motion key binding skipped",
                    ),
                }
            }
        }
        Self { bindings }
    }

    pub fn wire_for(&self, code: KeyCode, mods: KeyModifiers) -> Option<&'static str> {
        self.bindings
            .iter()
            .find_map(|(chord, wire)| chord.matches(code, mods).then_some(*wire))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_modified_named_and_case_sensitive_chords() {
        assert_eq!(
            KeyChord::parse("H"),
            Some(KeyChord {
                code: ChordCode::Char('H'),
                ctrl: false,
                alt: false,
            })
        );
        assert_eq!(
            KeyChord::parse("h"),
            Some(KeyChord {
                code: ChordCode::Char('h'),
                ctrl: false,
                alt: false,
            })
        );
        assert_eq!(
            KeyChord::parse("ctrl+f"),
            Some(KeyChord {
                code: ChordCode::Char('f'),
                ctrl: true,
                alt: false,
            })
        );
        assert_eq!(
            KeyChord::parse("M-v"),
            Some(KeyChord {
                code: ChordCode::Char('v'),
                ctrl: false,
                alt: true,
            })
        );
        assert_eq!(
            KeyChord::parse("alt+,"),
            Some(KeyChord {
                code: ChordCode::Char(','),
                ctrl: false,
                alt: true,
            })
        );
        assert_eq!(
            KeyChord::parse("M->"),
            Some(KeyChord {
                code: ChordCode::Char('>'),
                ctrl: false,
                alt: true,
            })
        );
        assert_eq!(
            KeyChord::parse("PageDown"),
            Some(KeyChord {
                code: ChordCode::PageDown,
                ctrl: false,
                alt: false,
            })
        );
        assert_eq!(
            KeyChord::parse("space"),
            Some(KeyChord {
                code: ChordCode::Char(' '),
                ctrl: false,
                alt: false,
            })
        );
    }

    #[test]
    fn rejects_invalid_chords() {
        for raw in ["", "ctrl", "ctrl+", "super+x", "ctrl+shift+x", "notakey"] {
            assert_eq!(KeyChord::parse(raw), None, "{raw}");
        }
    }

    #[test]
    fn matches_exact_ctrl_alt_and_ignores_shift() {
        let ctrl_f = KeyChord::parse("ctrl+f").unwrap();
        assert!(ctrl_f.matches(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert!(ctrl_f.matches(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        ));
        assert!(!ctrl_f.matches(KeyCode::Char('f'), KeyModifiers::NONE));
        assert!(!ctrl_f.matches(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL | KeyModifiers::ALT
        ));

        let h = KeyChord::parse("H").unwrap();
        assert!(h.matches(KeyCode::Char('H'), KeyModifiers::SHIFT));
        assert!(!h.matches(KeyCode::Char('h'), KeyModifiers::SHIFT));
    }

    #[test]
    fn default_config_binds_all_motion_actions() {
        let keymap = NavKeymap::from_config(&SidebarKeys::default());
        let cases = [
            (KeyCode::Char('a'), KeyModifiers::NONE, KEY_WIDTH_NARROWER),
            (KeyCode::Char('d'), KeyModifiers::NONE, KEY_WIDTH_WIDER),
            (KeyCode::Char('k'), KeyModifiers::NONE, KEY_UP),
            (KeyCode::Up, KeyModifiers::NONE, KEY_UP),
            (KeyCode::Char('j'), KeyModifiers::NONE, KEY_DOWN),
            (KeyCode::Down, KeyModifiers::NONE, KEY_DOWN),
            (KeyCode::Char('g'), KeyModifiers::NONE, KEY_TOP),
            (KeyCode::Char('G'), KeyModifiers::SHIFT, KEY_BOTTOM),
            (KeyCode::Char('K'), KeyModifiers::SHIFT, KEY_WORKTREE_UP),
            (KeyCode::Char('J'), KeyModifiers::SHIFT, KEY_WORKTREE_DOWN),
            (KeyCode::Char('b'), KeyModifiers::CONTROL, KEY_PAGE_UP),
            (KeyCode::PageUp, KeyModifiers::NONE, KEY_PAGE_UP),
            (KeyCode::Char('f'), KeyModifiers::CONTROL, KEY_PAGE_DOWN),
            (KeyCode::PageDown, KeyModifiers::NONE, KEY_PAGE_DOWN),
            (KeyCode::Char('H'), KeyModifiers::SHIFT, KEY_SCREEN_TOP),
            (KeyCode::Char('L'), KeyModifiers::SHIFT, KEY_SCREEN_BOTTOM),
        ];
        for (code, mods, wire) in cases {
            assert_eq!(keymap.wire_for(code, mods), Some(wire), "{code:?}");
        }
    }

    #[test]
    fn configurable_width_bindings_shadow_fixed_actions() {
        let keys = SidebarKeys {
            narrower: "q".to_owned(),
            wider: "x".to_owned(),
            ..SidebarKeys::default()
        };
        let keymap = NavKeymap::from_config(&keys);

        assert_eq!(
            keymap.wire_for(KeyCode::Char('q'), KeyModifiers::NONE),
            Some(KEY_WIDTH_NARROWER),
        );
        assert_eq!(
            keymap.wire_for(KeyCode::Char('x'), KeyModifiers::NONE),
            Some(KEY_WIDTH_WIDER),
        );
    }
}
