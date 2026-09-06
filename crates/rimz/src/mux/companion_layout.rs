//! Bounded column tiling for dedicated subagent tabs, shared by both backends.

use crate::ids::PaneId;

use super::SplitDirection;

pub const COMPANION_PANE_LIMIT: usize = 8;
const MAX_ROWS: usize = 4;
const MAX_COLUMNS: usize = 2;

/// Outer pane rectangle, including backend-owned frame cells where applicable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GridPane {
    pub(super) pane_id: PaneId,
    pub(super) x: u64,
    pub(super) y: u64,
    pub(super) cols: u64,
    pub(super) rows: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GridSplit {
    pub(super) pane_id: PaneId,
    pub(super) direction: SplitDirection,
}

fn columns(panes: &[GridPane], gap: u64) -> Option<Vec<Vec<&GridPane>>> {
    if panes.is_empty() || panes.len() > COMPANION_PANE_LIMIT {
        return None;
    }
    let mut ordered = panes.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|pane| (pane.x, pane.y));
    let top = ordered.first()?.y;
    let bottom = panes.iter().map(|pane| pane.y + pane.rows).max()?;
    let mut bands: Vec<Vec<&GridPane>> = Vec::new();
    for pane in ordered {
        if pane.cols == 0 || pane.rows == 0 {
            return None;
        }
        if let Some(band) = bands.last_mut()
            && band[0].x == pane.x
        {
            let previous = band.last()?;
            if previous.cols != pane.cols || previous.y + previous.rows + gap != pane.y {
                return None;
            }
            band.push(pane);
        } else {
            if let Some(previous) = bands.last()
                && previous[0].x + previous[0].cols + gap != pane.x
            {
                return None;
            }
            bands.push(vec![pane]);
        }
    }
    (bands.len() <= MAX_COLUMNS
        && bands.iter().all(|band| {
            band.len() <= MAX_ROWS
                && band[0].y == top
                && band.last().is_some_and(|pane| pane.y + pane.rows == bottom)
        }))
    .then_some(bands)
}

/// Start with two columns, then grow each column to at most four rows.
/// Unknown/manual/stacked shapes are left alone rather than retiled destructively.
pub(super) fn plan_append(panes: &[GridPane], gap: u64) -> Option<GridSplit> {
    if panes.len() >= COMPANION_PANE_LIMIT {
        return None;
    }
    let bands = columns(panes, gap)?;
    if bands.len() < MAX_COLUMNS
        && let Some(pane) = bands
            .iter()
            .filter(|band| band.len() == 1)
            .map(|band| band[0])
            .max_by_key(|pane| (pane.cols, std::cmp::Reverse(pane.x)))
    {
        return (pane.cols >= 12 + gap).then(|| GridSplit {
            pane_id: pane.pane_id.clone(),
            direction: SplitDirection::Right,
        });
    }
    let band = bands
        .iter()
        .filter(|band| band.len() < MAX_ROWS)
        .min_by_key(|band| (band.len(), band[0].x))?;
    let pane = band
        .iter()
        .max_by_key(|pane| (pane.rows, std::cmp::Reverse(pane.y)))?;
    (pane.rows >= 6 + gap).then(|| GridSplit {
        pane_id: pane.pane_id.clone(),
        direction: SplitDirection::Down,
    })
}

/// Equal heights within each column and width proportional to its pane count
/// make areas near-equal without moving processes between columns. Backends may only
/// approximate these targets when their resize steps are coarse.
pub(super) fn balance(panes: &[GridPane], gap: u64) -> Option<Vec<GridPane>> {
    let bands = columns(panes, gap)?;
    let left = panes.iter().map(|pane| pane.x).min()?;
    let top = panes.iter().map(|pane| pane.y).min()?;
    let width = panes.iter().map(|pane| pane.x + pane.cols).max()? - left;
    let height = panes.iter().map(|pane| pane.y + pane.rows).max()? - top;
    let usable_width = width.checked_sub(gap * (bands.len() - 1) as u64)?;
    let mut targets = Vec::with_capacity(panes.len());
    let mut x = left;
    let mut preceding = 0;
    for band in bands {
        let next = preceding + band.len() as u64;
        let cols = usable_width * next / panes.len() as u64
            - usable_width * preceding / panes.len() as u64;
        let usable_height = height.checked_sub(gap * (band.len() - 1) as u64)?;
        let mut y = top;
        for (index, pane) in band.iter().enumerate() {
            let rows = usable_height * (index + 1) as u64 / band.len() as u64
                - usable_height * index as u64 / band.len() as u64;
            if rows < 3 || cols < 6 {
                return None;
            }
            targets.push(GridPane {
                pane_id: pane.pane_id.clone(),
                x,
                y,
                cols,
                rows,
            });
            y += rows + gap;
        }
        x += cols + gap;
        preceding = next;
    }
    Some(targets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::MuxName;

    #[test]
    fn companion_layout_starts_with_two_columns_and_grows_to_eight() {
        for gap in [0, 1] {
            let mut panes = vec![GridPane {
                pane_id: PaneId::from_parts(MuxName::Tmux, "%1"),
                x: 50,
                y: 1,
                cols: 240,
                rows: 80,
            }];
            for count in 2..=COMPANION_PANE_LIMIT {
                let split = plan_append(&panes, gap).unwrap();
                assert_eq!(
                    split.direction,
                    if count == 2 {
                        SplitDirection::Right
                    } else {
                        SplitDirection::Down
                    }
                );
                let pane = panes
                    .iter_mut()
                    .find(|pane| pane.pane_id == split.pane_id)
                    .unwrap();
                let mut added = pane.clone();
                added.pane_id = PaneId::from_parts(MuxName::Tmux, format!("%{count}"));
                match split.direction {
                    SplitDirection::Down => {
                        pane.rows = (pane.rows - gap) / 2;
                        added.y += pane.rows + gap;
                        added.rows -= pane.rows + gap;
                    }
                    SplitDirection::Right => {
                        pane.cols = (pane.cols - gap) / 2;
                        added.x += pane.cols + gap;
                        added.cols -= pane.cols + gap;
                    }
                }
                panes.push(added);
                panes = balance(&panes, gap).unwrap();
                let cols = columns(&panes, gap).unwrap();
                assert_eq!(cols.len(), 2);
                assert_eq!(cols[0].len(), count.div_ceil(2), "left column at {count}");
                assert_eq!(cols[1].len(), count / 2, "right column at {count}");
                let areas = panes
                    .iter()
                    .map(|pane| pane.cols * pane.rows)
                    .collect::<Vec<_>>();
                assert!(areas.iter().max().unwrap() - areas.iter().min().unwrap() <= 300);
                assert!(panes.iter().all(|pane| pane.x >= 50 && pane.y >= 1));
            }
            assert!(plan_append(&panes, gap).is_none());
            let bands = columns(&panes, gap).unwrap();
            assert_eq!(bands.len(), MAX_COLUMNS);
            assert!(bands.iter().all(|band| band.len() == MAX_ROWS));
        }
    }

    #[test]
    fn companion_layout_rejects_stacks_and_unrecognised_geometry() {
        let pane = GridPane {
            pane_id: PaneId::from_parts(MuxName::Tmux, "%1"),
            x: 50,
            y: 0,
            cols: 100,
            rows: 40,
        };
        assert!(plan_append(&[pane.clone(), pane], 0).is_none());
        assert!(plan_append(&[], 0).is_none());
    }
}
