//! Configurable sidebar navigation keymap.

use crate::config::SidebarKeys;
use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use super::input::KeyAction;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct KeyChord {
    code: KeyCode,
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
            && self.code == code
    }
}

fn parse_code(raw: &str) -> Option<KeyCode> {
    let mut chars = raw.chars();
    let first = chars.next()?;
    if chars.next().is_none() {
        return Some(KeyCode::Char(first));
    }
    match raw.to_ascii_lowercase().as_str() {
        "up" => Some(KeyCode::Up),
        "down" => Some(KeyCode::Down),
        "left" => Some(KeyCode::Left),
        "right" => Some(KeyCode::Right),
        "home" => Some(KeyCode::Home),
        "end" => Some(KeyCode::End),
        "pageup" => Some(KeyCode::PageUp),
        "pagedown" => Some(KeyCode::PageDown),
        "enter" => Some(KeyCode::Enter),
        "space" => Some(KeyCode::Char(' ')),
        _ => None,
    }
}

#[derive(Clone, Debug)]
pub struct NavKeymap {
    bindings: Vec<(KeyChord, KeyAction)>,
}

impl NavKeymap {
    pub fn from_config(keys: &SidebarKeys) -> Self {
        let mut bindings = Vec::new();
        for (spec, action) in [
            (keys.narrower.as_str(), KeyAction::WidthNarrower),
            (keys.wider.as_str(), KeyAction::WidthWider),
            (keys.up.as_str(), KeyAction::Up),
            (keys.down.as_str(), KeyAction::Down),
            (keys.top.as_str(), KeyAction::Top),
            (keys.bottom.as_str(), KeyAction::Bottom),
            (keys.worktree_up.as_str(), KeyAction::WorktreeUp),
            (keys.worktree_down.as_str(), KeyAction::WorktreeDown),
            (keys.page_up.as_str(), KeyAction::PageUp),
            (keys.page_down.as_str(), KeyAction::PageDown),
            (keys.screen_top.as_str(), KeyAction::ScreenTop),
            (keys.screen_bottom.as_str(), KeyAction::ScreenBottom),
        ] {
            for token in spec.split_whitespace() {
                match KeyChord::parse(token) {
                    Some(chord) => bindings.push((chord, action)),
                    None => tracing::warn!(
                        binding = token,
                        action = ?action,
                        "invalid sidebar motion key binding skipped",
                    ),
                }
            }
        }
        Self { bindings }
    }

    pub(super) fn action_for(&self, code: KeyCode, mods: KeyModifiers) -> Option<KeyAction> {
        self.bindings
            .iter()
            .find_map(|(chord, action)| chord.matches(code, mods).then_some(*action))
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
                code: KeyCode::Char('H'),
                ctrl: false,
                alt: false,
            })
        );
        assert_eq!(
            KeyChord::parse("h"),
            Some(KeyChord {
                code: KeyCode::Char('h'),
                ctrl: false,
                alt: false,
            })
        );
        assert_eq!(
            KeyChord::parse("ctrl+f"),
            Some(KeyChord {
                code: KeyCode::Char('f'),
                ctrl: true,
                alt: false,
            })
        );
        assert_eq!(
            KeyChord::parse("M-v"),
            Some(KeyChord {
                code: KeyCode::Char('v'),
                ctrl: false,
                alt: true,
            })
        );
        assert_eq!(
            KeyChord::parse("alt+,"),
            Some(KeyChord {
                code: KeyCode::Char(','),
                ctrl: false,
                alt: true,
            })
        );
        assert_eq!(
            KeyChord::parse("M->"),
            Some(KeyChord {
                code: KeyCode::Char('>'),
                ctrl: false,
                alt: true,
            })
        );
        assert_eq!(
            KeyChord::parse("PageDown"),
            Some(KeyChord {
                code: KeyCode::PageDown,
                ctrl: false,
                alt: false,
            })
        );
        assert_eq!(
            KeyChord::parse("space"),
            Some(KeyChord {
                code: KeyCode::Char(' '),
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
    fn configured_named_keys_match_exact_ctrl_alt_and_ignore_shift() {
        let modifiers = [
            ("", KeyModifiers::NONE),
            ("ctrl+", KeyModifiers::CONTROL),
            ("alt+", KeyModifiers::ALT),
            ("ctrl+alt+", KeyModifiers::CONTROL | KeyModifiers::ALT),
        ];
        for (name, code) in [
            ("Up", KeyCode::Up),
            ("Down", KeyCode::Down),
            ("Left", KeyCode::Left),
            ("Right", KeyCode::Right),
            ("Home", KeyCode::Home),
            ("End", KeyCode::End),
            ("PageUp", KeyCode::PageUp),
            ("PageDown", KeyCode::PageDown),
            ("Enter", KeyCode::Enter),
        ] {
            for (prefix, expected_mods) in modifiers {
                let keys = SidebarKeys {
                    narrower: format!("{prefix}{name}"),
                    up: String::new(),
                    down: String::new(),
                    page_up: String::new(),
                    page_down: String::new(),
                    ..SidebarKeys::default()
                };
                let keymap = NavKeymap::from_config(&keys);
                for (_, mods) in modifiers {
                    for shift in [KeyModifiers::NONE, KeyModifiers::SHIFT] {
                        assert_eq!(
                            keymap.action_for(code, mods | shift),
                            (mods == expected_mods).then_some(KeyAction::WidthNarrower),
                            "{prefix}{name}: {mods:?} | {shift:?}",
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn default_config_binds_all_motion_actions() {
        let keymap = NavKeymap::from_config(&SidebarKeys::default());
        let cases = [
            (
                KeyCode::Char('a'),
                KeyModifiers::NONE,
                KeyAction::WidthNarrower,
            ),
            (
                KeyCode::Char('d'),
                KeyModifiers::NONE,
                KeyAction::WidthWider,
            ),
            (KeyCode::Char('k'), KeyModifiers::NONE, KeyAction::Up),
            (KeyCode::Up, KeyModifiers::NONE, KeyAction::Up),
            (KeyCode::Char('j'), KeyModifiers::NONE, KeyAction::Down),
            (KeyCode::Down, KeyModifiers::NONE, KeyAction::Down),
            (KeyCode::Char('g'), KeyModifiers::NONE, KeyAction::Top),
            (KeyCode::Char('G'), KeyModifiers::SHIFT, KeyAction::Bottom),
            (
                KeyCode::Char('K'),
                KeyModifiers::SHIFT,
                KeyAction::WorktreeUp,
            ),
            (
                KeyCode::Char('J'),
                KeyModifiers::SHIFT,
                KeyAction::WorktreeDown,
            ),
            (KeyCode::Char('b'), KeyModifiers::CONTROL, KeyAction::PageUp),
            (KeyCode::PageUp, KeyModifiers::NONE, KeyAction::PageUp),
            (
                KeyCode::Char('f'),
                KeyModifiers::CONTROL,
                KeyAction::PageDown,
            ),
            (KeyCode::PageDown, KeyModifiers::NONE, KeyAction::PageDown),
            (
                KeyCode::Char('H'),
                KeyModifiers::SHIFT,
                KeyAction::ScreenTop,
            ),
            (
                KeyCode::Char('L'),
                KeyModifiers::SHIFT,
                KeyAction::ScreenBottom,
            ),
        ];
        for (code, mods, action) in cases {
            assert_eq!(keymap.action_for(code, mods), Some(action), "{code:?}");
        }
    }
}
