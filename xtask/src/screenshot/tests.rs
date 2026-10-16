use super::*;

#[test]
fn frame_grid_measures_ansi_rows_and_zero_width_variation_selectors() {
    let ansi = "\x1b[38;2;1;2;3m☀\u{fe0e} fleet\x1b[0m\n\x1b[48;2;4;5;6mready\x1b[0m\n";
    assert_eq!(frame_grid(ansi.as_bytes()), (7, 2));
    assert_eq!(frame_grid(b"\x1b[2Kok\x1b[1 q\n\n"), (2, 2));
    assert_eq!(frame_grid(b"\x1b[31m\xff\xfe\x1b[0m\n"), (2, 1));
    assert_eq!(frame_grid(b""), (0, 0));
}

#[test]
fn frame_grid_skips_osc_hyperlinks_and_other_escapes() {
    let link = "\x1b]8;;https://github.com/o/r/pull/91\x1b\\#91\x1b]8;;\x1b\\ ok\n";
    assert_eq!(frame_grid(link.as_bytes()), (6, 1));
    assert_eq!(
        frame_grid(b"\x1b]8;;https://x.y\x07#9\x1b]8;;\x07\x1b(Bz\n"),
        (3, 1)
    );
}

#[test]
fn frame_grid_counts_wide_glyphs_as_two_columns() {
    assert_eq!(frame_grid("a你b\n".as_bytes()), (4, 1));
}

#[test]
fn freeze_frame_size_matches_the_retina_sidebar_grid() {
    assert_eq!(freeze_frame_size(54, 34), (831, 1103));
    assert_eq!(freeze_frame_size(40, 10), (618, 335));
}

#[test]
fn cascadia_font_family_requires_the_requested_face() {
    assert!(is_cascadia_font_family("Cascadia Code NF\n"));
    assert!(is_cascadia_font_family(
        "CASCADIA CODE NF,Cascadia Code NF Light\n"
    ));
    assert!(!is_cascadia_font_family("DejaVu Sans\n"));
}
