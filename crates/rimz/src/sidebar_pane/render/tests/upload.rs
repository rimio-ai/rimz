//! The paste upload row directly above the footer.

use super::*;
use crate::sidebar_pane::render::chrome::upload_line;
use crate::store::snapshot::SidebarUpload;

fn running(received: u64, total: u64) -> SidebarUpload {
    SidebarUpload::Running { received, total }
}

fn failed() -> SidebarUpload {
    SidebarUpload::Failed {
        reason: "5 B is over the 4 B upload limit (paste_max)".to_owned(),
    }
}

fn with_upload(upload: SidebarUpload) -> SidebarSnapshot {
    let mut snapshot = snapshot_with(Vec::new());
    snapshot.upload = Some(upload);
    snapshot
}

fn row_text(upload: &SidebarUpload, width: usize) -> String {
    upload_line(&Theme::fixed(true), upload, width, None)
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

/// The chrome row holding `needle`, and the rows on either side of it.
fn around(lines: &[String], needle: &str) -> (String, String) {
    let row = lines
        .iter()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no {needle:?} row: {lines:?}"));
    (lines[row - 1].clone(), lines[row + 1].clone())
}

#[test]
fn the_upload_row_sits_directly_above_the_footer_in_both_placements() {
    for (name, providers) in [
        ("bare store", Vec::new()),
        ("dashboard", two_provider_panels()),
    ] {
        let short_failure = SidebarUpload::Failed {
            reason: "disk full".to_owned(),
        };
        for upload in [running(1, 2), short_failure] {
            let mut snapshot = with_upload(upload.clone());
            snapshot.providers = providers.clone();
            let (lines, _) = bottom_chrome_texts(&snapshot, None);
            let needle = match upload {
                SidebarUpload::Running { .. } => "(⌃V cancel)",
                SidebarUpload::Failed { .. } => "upload failed",
            };
            let (above, below) = around(&lines, needle);
            assert!(above.trim().is_empty(), "{name}: {lines:?}");
            assert!(below.contains("? for help"), "{name}: {lines:?}");
        }
    }
}

#[test]
fn the_upload_row_is_absent_under_an_active_alert_and_without_a_record() {
    let alert = Alert::active("unreadable", fixed_now());
    for snapshot in [with_upload(running(1, 2)), with_upload(failed())] {
        let (lines, _) = bottom_chrome_texts(&snapshot, Some(&alert));
        assert!(!lines.iter().any(|line| line.contains('↑')), "{lines:?}");
    }
    let (lines, _) = bottom_chrome_texts(&snapshot_with(Vec::new()), None);
    assert!(!lines.iter().any(|line| line.contains('↑')), "{lines:?}");
}

#[test]
fn the_text_bar_fills_to_the_received_share() {
    assert_eq!(
        [running(0, 8), running(1, 2), running(8, 8)].map(|upload| row_text(&upload, 22)),
        [
            "↑ ▱▱▱▱▱▱▱▱ (⌃V cancel)",
            "↑ ▰▰▰▰▱▱▱▱ (⌃V cancel)",
            "↑ ▰▰▰▰▰▰▰▰ (⌃V cancel)",
        ]
    );
    assert_eq!(
        row_text(&failed(), 60),
        "↑ upload failed: 5 B is over the 4 B upload limit (paste_max)"
    );
}

#[test]
fn narrow_widths_drop_the_hint_before_the_bar() {
    let upload = running(1, 2);
    assert_eq!(
        [19, 18, 17, 3, 2, 1].map(|width| row_text(&upload, width)),
        [
            "↑ ▰▰▰▱▱ (⌃V cancel)",
            "↑ ▰▰▱▱ (⌃V cancel)",
            "↑ ▰▰▰▰▰▰▰▰▱▱▱▱▱▱▱",
            "↑ ▰",
            "↑",
            "↑",
        ]
    );
}

#[test]
fn the_pixel_bar_takes_the_text_bars_cells() {
    let theme = truecolor_sidebar_theme();
    let upload = running(1, 3);
    let text = upload_line(&theme, &upload, 30, None);
    let mut pixels = MeterPixels::new(0x120000);
    pixels.begin_frame();
    let drawn = upload_line(&theme, &upload, 30, Some(&mut pixels));
    let bar_columns = |line: &Line<'_>, placeholder: bool| {
        let (mut columns, mut column) = (Vec::new(), 0);
        for span in &line.spans {
            if span.content.contains('\u{10eeee}') {
                if placeholder {
                    columns.push(column);
                }
                column += span.width();
                continue;
            }
            for ch in span.content.chars() {
                if !placeholder && matches!(ch, '▰' | '▱') {
                    columns.push(column);
                }
                column += crate::sidebar_pane::render::layout::text_width(&ch.to_string());
            }
        }
        columns
    };
    let glyphs = bar_columns(&text, false);
    let placeholders = bar_columns(&drawn, true);
    assert_eq!(glyphs.len(), 16, "{text}");
    assert_eq!(placeholders, glyphs, "{drawn}");
    assert_eq!(
        crate::sidebar_pane::render::layout::spans_width(&drawn.spans),
        crate::sidebar_pane::render::layout::spans_width(&text.spans)
    );
}

#[test]
fn the_upload_row_holds_still_between_paint_ticks() {
    let snapshot = with_upload(running(1, 3));
    let refresh_ms = snapshot.theme.display.resolved_refresh_ms();
    let half = 3_500 / u64::from(refresh_ms) / 2;
    let [rest, later] = [0, half].map(|phase| {
        snapshot_to_bytes_with_alert_and_ui(
            &snapshot,
            None,
            &ui_at_phase(phase),
            &UiState::default(),
            54,
            40,
        )
    });
    assert_eq!(rest, later, "one snapshot paints one upload row");
}

#[test]
fn upload_rows_render_above_the_footer() {
    for (name, upload) in [
        ("upload_running", running(3, 8)),
        ("upload_failed", failed()),
    ] {
        assert_snapshot(name, snapshot_to_screen(&with_upload(upload), 46, 18));
    }
}
