//! Typed hit geometry emitted with each painted frame.

use std::ops::Range;

use ratatui::text::Line;

use crate::sidebar_pane::view::BodyFilter;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HitTarget {
    Row(usize),
    ProviderTab(String),
    BodyFilter(BodyFilter),
    ToggleGroup(String),
    UnreadBanner,
}

/// Half-open display-cell region.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HitRegion {
    pub(crate) rows: Range<usize>,
    pub(crate) columns: Range<u16>,
    pub(crate) target: HitTarget,
}

impl HitRegion {
    pub(crate) fn line(line: usize, columns: Range<u16>, target: HitTarget) -> Self {
        Self {
            rows: line..line.saturating_add(1),
            columns,
            target,
        }
    }

    #[cfg(test)]
    pub(crate) fn whole_line(line: usize, target: HitTarget) -> Self {
        Self::line(line, 0..u16::MAX, target)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FrameInteractions {
    row_by_line: Vec<Option<usize>>,
    regions: Vec<HitRegion>,
}

impl FrameInteractions {
    #[cfg(test)]
    pub(crate) fn from_parts(row_by_line: Vec<Option<usize>>, regions: Vec<HitRegion>) -> Self {
        Self {
            row_by_line,
            regions,
        }
    }

    pub(crate) fn target_at(&self, column: u16, row: u16) -> Option<HitTarget> {
        let row = usize::from(row);
        self.regions
            .iter()
            .filter(|region| region.rows.contains(&row) && region.columns.contains(&column))
            .min_by_key(|region| target_precedence(&region.target))
            .map(|region| region.target.clone())
            .or_else(|| {
                self.row_by_line
                    .get(row)
                    .copied()
                    .flatten()
                    .map(HitTarget::Row)
            })
    }

    pub(crate) fn visible_row_span(&self) -> Option<(usize, usize)> {
        let mut rows = self.row_by_line.iter().flatten().copied();
        let first = rows.next()?;
        Some((first, rows.fold(first, |_, row| row)))
    }

    pub(crate) fn row_map(&self) -> &[Option<usize>] {
        &self.row_by_line
    }

    #[cfg(test)]
    pub(crate) fn row_at_line(&self, line: usize) -> Option<usize> {
        self.row_by_line.get(line).copied().flatten()
    }

    #[cfg(test)]
    pub(crate) fn line_for_row(&self, ordinal: usize) -> Option<usize> {
        self.row_by_line
            .iter()
            .position(|row| *row == Some(ordinal))
    }

    #[cfg(test)]
    pub(crate) fn line_count(&self) -> usize {
        self.row_by_line.len()
    }

    #[cfg(test)]
    pub(crate) fn line_for_target(&self, target: &HitTarget) -> Option<(u16, u16)> {
        self.regions.iter().find_map(|region| {
            (&region.target == target).then(|| {
                (
                    region.columns.start,
                    u16::try_from(region.rows.start).unwrap_or(u16::MAX),
                )
            })
        })
    }

    #[cfg(test)]
    pub(crate) fn regions(&self) -> &[HitRegion] {
        &self.regions
    }

    fn translate(&mut self, rows: usize, columns: u16) {
        for region in &mut self.regions {
            region.rows =
                region.rows.start.saturating_add(rows)..region.rows.end.saturating_add(rows);
            region.columns = region.columns.start.saturating_add(columns)
                ..region.columns.end.saturating_add(columns);
        }
    }

    fn windowed(mut self, start: usize, len: usize) -> Self {
        let end = start.saturating_add(len).min(self.row_by_line.len());
        self.row_by_line.truncate(end);
        self.row_by_line.drain(..start.min(end));
        let regions = self
            .regions
            .into_iter()
            .filter_map(|region| {
                let clipped_start = region.rows.start.max(start);
                let clipped_end = region.rows.end.min(end);
                (clipped_start < clipped_end).then(|| HitRegion {
                    rows: clipped_start - start..clipped_end - start,
                    columns: region.columns,
                    target: region.target,
                })
            })
            .collect();
        Self {
            row_by_line: self.row_by_line,
            regions,
        }
    }

    fn append(&mut self, other: Self) {
        let row_offset = self.row_by_line.len();
        let mut other = other;
        other.translate(row_offset, 0);
        self.row_by_line.extend(other.row_by_line);
        self.regions.extend(other.regions);
    }
}

fn target_precedence(target: &HitTarget) -> u8 {
    match target {
        HitTarget::ProviderTab(_) => 0,
        HitTarget::BodyFilter(_) => 1,
        HitTarget::UnreadBanner => 2,
        HitTarget::ToggleGroup(_) => 3,
        HitTarget::Row(_) => 4,
    }
}

/// Lines and their geometry, transformed together during three-zone compose.
#[derive(Default)]
pub(crate) struct RenderedBlock {
    pub(crate) lines: Vec<Line<'static>>,
    pub(crate) interactions: FrameInteractions,
}

impl RenderedBlock {
    pub(crate) fn from_parts(
        lines: Vec<Line<'static>>,
        row_by_line: Vec<Option<usize>>,
        regions: Vec<HitRegion>,
    ) -> Self {
        debug_assert_eq!(lines.len(), row_by_line.len());
        let mut block = Self {
            lines: Vec::with_capacity(lines.len()),
            interactions: FrameInteractions::default(),
        };
        for (line, row) in lines.into_iter().zip(row_by_line) {
            match row {
                Some(ordinal) => block.push_row(line, ordinal),
                None => block.push_inert(line),
            }
        }
        block.interactions.regions = regions;
        block.assert_shape();
        block
    }

    pub(crate) fn push(&mut self, line: Line<'static>, ordinal: Option<usize>) {
        self.lines.push(line);
        self.interactions.row_by_line.push(ordinal);
        self.assert_shape();
    }

    pub(crate) fn push_row(&mut self, line: Line<'static>, ordinal: usize) {
        self.push(line, Some(ordinal));
    }

    pub(crate) fn push_inert(&mut self, line: Line<'static>) {
        self.push(line, None);
    }

    pub(crate) fn push_with_regions(
        &mut self,
        line: Line<'static>,
        ordinal: Option<usize>,
        regions: impl IntoIterator<Item = (Range<u16>, HitTarget)>,
    ) {
        let row = self.lines.len();
        self.push(line, ordinal);
        self.interactions.regions.extend(
            regions
                .into_iter()
                .map(|(columns, target)| HitRegion::line(row, columns, target)),
        );
        self.assert_shape();
    }

    pub(crate) fn push_target(&mut self, line: Line<'static>, target: HitTarget) {
        self.push_with_regions(line, None, [(0..u16::MAX, target)]);
    }

    pub(crate) fn extend_inert(&mut self, lines: impl IntoIterator<Item = Line<'static>>) {
        for line in lines {
            self.push_inert(line);
        }
    }

    pub(crate) fn map_lines(&mut self, mut map: impl FnMut(Line<'static>) -> Line<'static>) {
        for line in &mut self.lines {
            *line = map(std::mem::take(line));
        }
        self.assert_shape();
    }

    pub(crate) fn translate_columns(&mut self, columns: u16) {
        self.interactions.translate(0, columns);
    }

    pub(crate) fn append(&mut self, other: Self) {
        self.interactions.append(other.interactions);
        self.lines.extend(other.lines);
        self.assert_shape();
    }

    pub(crate) fn window(mut self, start: usize, len: usize) -> Self {
        let end = start.saturating_add(len).min(self.lines.len());
        self.lines.truncate(end);
        self.lines.drain(..start.min(end));
        let block = Self {
            lines: self.lines,
            interactions: self.interactions.windowed(start, len),
        };
        block.assert_shape();
        block
    }

    #[cfg(test)]
    pub(crate) fn offset_row_ordinals(&mut self, offset: usize) {
        for ordinal in self.interactions.row_by_line.iter_mut().flatten() {
            *ordinal += offset;
        }
    }

    fn assert_shape(&self) {
        debug_assert_eq!(self.lines.len(), self.interactions.row_by_line.len());
        debug_assert!(
            self.interactions
                .regions
                .iter()
                .all(|region| region.rows.end <= self.lines.len())
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentStatus;

    #[test]
    fn target_precedence_is_explicit_before_row_fallback() {
        let regions = vec![
            HitRegion::whole_line(0, HitTarget::ToggleGroup("group".to_owned())),
            HitRegion::whole_line(0, HitTarget::UnreadBanner),
            HitRegion::line(
                0,
                0..4,
                HitTarget::BodyFilter(BodyFilter::Status(AgentStatus::Failed)),
            ),
            HitRegion::line(0, 0..4, HitTarget::ProviderTab("codex".to_owned())),
        ];
        let interactions = FrameInteractions::from_parts(vec![Some(9)], regions);

        assert_eq!(
            interactions.target_at(0, 0),
            Some(HitTarget::ProviderTab("codex".to_owned()))
        );
        assert_eq!(
            FrameInteractions::from_parts(
                vec![Some(9)],
                vec![
                    HitRegion::whole_line(0, HitTarget::ToggleGroup("group".to_owned())),
                    HitRegion::whole_line(0, HitTarget::UnreadBanner),
                ],
            )
            .target_at(20, 0),
            Some(HitTarget::UnreadBanner)
        );
        assert_eq!(
            FrameInteractions::from_parts(vec![Some(9)], Vec::new()).target_at(20, 0),
            Some(HitTarget::Row(9))
        );
    }

    #[test]
    fn append_and_window_transform_lines_and_targets_together() {
        let mut top = RenderedBlock::from_parts(
            vec![Line::from("top")],
            vec![None],
            vec![HitRegion::line(
                0,
                2..5,
                HitTarget::ProviderTab("top".to_owned()),
            )],
        );
        let body = RenderedBlock::from_parts(
            vec![Line::from("row-3"), Line::from("row-4")],
            vec![Some(3), Some(4)],
            vec![HitRegion {
                rows: 0..2,
                columns: 0..u16::MAX,
                target: HitTarget::ToggleGroup("body".to_owned()),
            }],
        );
        top.append(body);

        assert_eq!(
            top.interactions.target_at(2, 0),
            Some(HitTarget::ProviderTab("top".to_owned()))
        );
        assert_eq!(
            top.interactions.target_at(0, 1),
            Some(HitTarget::ToggleGroup("body".to_owned()))
        );
        assert_eq!(top.interactions.visible_row_span(), Some((3, 4)));

        let window = top.window(2, 1);
        assert_eq!(window.lines, vec![Line::from("row-4")]);
        assert_eq!(
            window.interactions.target_at(0, 0),
            Some(HitTarget::ToggleGroup("body".to_owned()))
        );
        assert_eq!(window.interactions.visible_row_span(), Some((4, 4)));
        assert_eq!(
            window.interactions.target_at(0, 1),
            None,
            "clipped target does not leak past window"
        );
    }
}
