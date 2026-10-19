//! Section labels and row extraction from the last painted buffer.

use std::io::{self, Write};

use ratatui::buffer::{Buffer, Cell};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};

use super::{UiState, ansi};

pub(super) fn wrapped_line_height(line: &Line<'_>, width: u16) -> usize {
    if line.width() <= usize::from(width) && width > 0 {
        return 1;
    }
    Paragraph::new(line.clone())
        .wrap(Wrap { trim: false })
        .line_count(width)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::sidebar_pane) struct SectionLabel(pub(super) String);

impl SectionLabel {
    pub(in crate::sidebar_pane) fn as_str(&self) -> &str {
        &self.0
    }

    pub(in crate::sidebar_pane) fn matches(&self, section: &str) -> bool {
        self.as_str() == section || (section.starts_with('#') && self.as_str().ends_with(section))
    }
}

pub(in crate::sidebar_pane) struct FrameRow<'a> {
    pub(in crate::sidebar_pane) label: &'a SectionLabel,
    cells: &'a [Cell],
}

impl FrameRow<'_> {
    #[cfg(any(test, feature = "testkit"))]
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "The scenario player will consume plain rows under testkit."
        )
    )]
    pub(in crate::sidebar_pane) fn plain_text(&self) -> String {
        let mut text = String::with_capacity(self.cells.len());
        for cell in ansi::display_cells(self.cells) {
            for part in anstream::adapter::strip_str(cell.symbol()) {
                text.push_str(part);
            }
        }
        text
    }

    pub(in crate::sidebar_pane) fn write_ansi<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        ansi::write_row_ansi(writer, self.cells)
    }
}

impl UiState {
    pub(in crate::sidebar_pane) fn frame_rows<'a>(
        &'a self,
        buffer: &'a Buffer,
    ) -> impl Iterator<Item = FrameRow<'a>> {
        static EMPTY_LABEL: SectionLabel = SectionLabel(String::new());
        buffer
            .content
            .chunks(usize::from(buffer.area.width.max(1)))
            .enumerate()
            .map(|(row, cells)| FrameRow {
                label: self.sections.get(row).unwrap_or(&EMPTY_LABEL),
                cells,
            })
    }
}
