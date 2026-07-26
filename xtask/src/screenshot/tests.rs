use super::*;

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
