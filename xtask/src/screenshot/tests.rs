use serde_json::{Value, json};

use super::*;

fn pane_list(panes: Value) -> Vec<u8> {
    serde_json::to_vec(&panes).unwrap()
}

#[test]
fn live_sidebar_selection_falls_back_to_the_only_sidebar() {
    let panes = pane_list(json!([
        {
            "pane_id": "zellij:terminal_1",
            "view_id": "tab_1",
            "command": "rimz-sidebar"
        },
        {
            "pane_id": "zellij:terminal_2",
            "view_id": "tab_2",
            "command": "zsh"
        }
    ]));

    assert_eq!(
        select_live_sidebar_pane(&panes).unwrap(),
        "zellij:terminal_1"
    );
}

#[test]
fn live_sidebar_selection_bails_when_ambiguous() {
    let panes = pane_list(json!([
        {
            "pane_id": "zellij:terminal_1",
            "view_id": "tab_1",
            "command": "rimz-sidebar"
        },
        {
            "pane_id": "zellij:terminal_2",
            "view_id": "tab_2",
            "command": "rimz-sidebar"
        }
    ]));

    let err = select_live_sidebar_pane(&panes).unwrap_err().to_string();
    assert!(
        err.contains("multiple rimz-sidebar panes matched"),
        "unexpected error: {err}"
    );
}

#[test]
fn live_sidebar_selection_bails_when_no_sidebar_exists() {
    let panes = pane_list(json!([
        {
            "pane_id": "zellij:terminal_1",
            "view_id": "tab_1",
            "command": "zsh"
        }
    ]));

    let err = select_live_sidebar_pane(&panes).unwrap_err().to_string();
    assert!(
        err.contains("no rimz-sidebar pane found"),
        "unexpected error: {err}"
    );
}

#[test]
fn sprite_remap_swaps_the_glyph_ghostty_draws_but_the_font_lacks() {
    // U+1FB87 is the selected-worktree right lane seal Ghostty paints from its sprite
    // renderer; the font carries no outline, so it must become U+2595 before freeze.
    let ansi = "\u{1FB87}\x1b[34m\u{258E}row\u{2590}\x1b[0m".as_bytes();
    let out = String::from_utf8(remap_sprite_glyphs(ansi)).unwrap();

    // the unsupported seal becomes the font-present right-eighth block
    assert!(!out.contains('\u{1FB87}'), "seal glyph should be remapped");
    assert!(out.starts_with('\u{2595}'));
    // glyphs the font already has and the SGR styling are left untouched
    assert_eq!(out, "\u{2595}\x1b[34m\u{258E}row\u{2590}\x1b[0m");
}
