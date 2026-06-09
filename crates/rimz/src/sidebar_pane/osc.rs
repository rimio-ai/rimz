//! Terminal-local notification bytes.
//!
//! Desktop banners ride OSC 777. Under tmux the payload is DCS-wrapped so
//! `allow-passthrough` forwards it to the client terminal; Zellij drops these
//! notification OSCs today, so `auto` skips them there and the command channel
//! remains the portable fallback.

use crate::config::{DesktopNotificationMode, NotificationSoundMode};
use crate::ids::MuxName;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;

pub(super) fn desktop_notification_bytes(
    mux: MuxName,
    desktop: DesktopNotificationMode,
    title: &str,
    body: &str,
) -> Vec<u8> {
    desktop_payload(mux, desktop, title, body).unwrap_or_default()
}

pub(super) fn sound_notification_bytes(sound: NotificationSoundMode) -> Vec<u8> {
    match sound {
        NotificationSoundMode::Bell => vec![BEL],
        NotificationSoundMode::Off => Vec::new(),
    }
}

fn desktop_payload(
    mux: MuxName,
    mode: DesktopNotificationMode,
    title: &str,
    body: &str,
) -> Option<Vec<u8>> {
    match (mux, mode) {
        (_, DesktopNotificationMode::Off) => None,
        (MuxName::Zellij, DesktopNotificationMode::Auto) => None,
        (MuxName::Tmux, DesktopNotificationMode::Auto | DesktopNotificationMode::Osc) => {
            Some(tmux_wrap(&osc777(title, body)))
        }
        (MuxName::Zellij, DesktopNotificationMode::Osc) => Some(osc777(title, body)),
    }
}

fn osc777(title: &str, body: &str) -> Vec<u8> {
    format!("\x1b]777;notify;{};{}\x07", osc_text(title), osc_text(body)).into_bytes()
}

fn tmux_wrap(payload: &[u8]) -> Vec<u8> {
    let mut out = b"\x1bPtmux;".to_vec();
    for byte in payload {
        if *byte == ESC {
            out.push(ESC);
        }
        out.push(*byte);
    }
    out.extend_from_slice(b"\x1b\\");
    out
}

fn osc_text(value: &str) -> String {
    value
        .chars()
        .filter_map(|ch| match ch {
            '\u{1b}' | '\u{7}' => None,
            ';' => Some(':'),
            '\n' | '\r' | '\t' => Some(' '),
            ch if ch.is_control() => None,
            ch => Some(ch),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmux_auto_wraps_osc() {
        let bytes = desktop_notification_bytes(
            MuxName::Tmux,
            DesktopNotificationMode::Auto,
            "Title",
            "Body",
        );
        assert_eq!(
            bytes,
            b"\x1bPtmux;\x1b\x1b]777;notify;Title;Body\x07\x1b\\".to_vec()
        );
    }

    #[test]
    fn zellij_auto_skips_desktop() {
        let bytes = desktop_notification_bytes(
            MuxName::Zellij,
            DesktopNotificationMode::Auto,
            "Title",
            "Body",
        );
        assert!(bytes.is_empty());
    }

    #[test]
    fn bell_sound_emits_bel() {
        assert_eq!(
            sound_notification_bytes(NotificationSoundMode::Bell),
            vec![BEL]
        );
        assert!(sound_notification_bytes(NotificationSoundMode::Off).is_empty());
    }

    #[test]
    fn off_modes_emit_nothing() {
        let bytes = desktop_notification_bytes(
            MuxName::Tmux,
            DesktopNotificationMode::Off,
            "Title",
            "Body",
        );
        assert!(bytes.is_empty());
    }

    #[test]
    fn osc_text_strips_controls_and_separator_semicolons() {
        let bytes = desktop_notification_bytes(
            MuxName::Zellij,
            DesktopNotificationMode::Osc,
            "A;B\x1b",
            "line\nnext;tail",
        );
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            "\x1b]777;notify;A:B;line next:tail\x07"
        );
    }
}
