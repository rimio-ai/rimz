use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use jiff::Timestamp;

use crate::config::{CardDensityMode, ContextMeterConfig};
use crate::store::snapshot::SidebarWorktreeGroup;

use super::sections::{CardExpansion, RowCtx};
use super::theme::Theme;

#[derive(Clone, Debug, Default)]
pub(in crate::sidebar_pane) struct CardHeights {
    epoch: Option<HeightEpoch>,
    heights: HashMap<String, (CardKey, usize)>,
    #[cfg(test)]
    pub(super) painted: Vec<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CardKey {
    pub(super) expansion: CardExpansion,
    pub(super) selected: bool,
}

#[derive(Clone, Debug)]
struct HeightEpoch {
    // Holding the allocation forces make_mut to detach even a local unread edit.
    groups: Arc<Vec<SidebarWorktreeGroup>>,
    theme: Rc<Theme>,
    inputs: HeightInputs,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct HeightInputs {
    now: Timestamp,
    width: usize,
    bands: ContextMeterConfig,
    tool_repeat_warn_after: u32,
    stalled_after_secs: u32,
    card_density: CardDensityMode,
    stale_after_secs: u64,
    recent_subagent_secs: u64,
    max_recent_subagents: usize,
}

impl CardHeights {
    pub(super) fn prepare(
        &mut self,
        groups: &Arc<Vec<SidebarWorktreeGroup>>,
        theme: Rc<Theme>,
        ctx: &RowCtx<'_>,
    ) -> bool {
        let RowCtx {
            theme: _, // The owned Rc is compared by identity below.
            now,
            width,
            tier: _, // Derived from width.
            bands,
            tool_repeat_warn_after,
            stalled_after_secs,
            card_density,
            stale_after_secs,
            entry_cursor: _, // Content-only; the mark replaces indent cells.
            recent_subagent_secs,
            max_recent_subagents,
            // These resolve into CardKey, not the epoch.
            selected_index: _,
            held_expansions: _,
            delegation_overrides: _,
            delegation_history: _,
            lead_unread: _, // Derived from the owned groups.
            // Content-only; test builds re-measure every hit to guard this premise.
            animation_phase: _,
            money_phase: _,
            cost_rolls: _,
        } = *ctx;
        let inputs = HeightInputs {
            now,
            width,
            bands: bands.clone(),
            tool_repeat_warn_after,
            stalled_after_secs,
            card_density,
            stale_after_secs,
            recent_subagent_secs,
            max_recent_subagents,
        };
        if self.epoch.as_ref().is_some_and(|epoch| {
            Arc::ptr_eq(&epoch.groups, groups)
                && Rc::ptr_eq(&epoch.theme, &theme)
                && epoch.inputs == inputs
        }) {
            return false;
        }
        self.heights.clear();
        self.epoch = Some(HeightEpoch {
            groups: Arc::clone(groups),
            theme,
            inputs,
        });
        true
    }

    pub(super) fn get(&self, id: &str, key: CardKey) -> Option<usize> {
        self.heights
            .get(id)
            .filter(|(cached, _)| *cached == key)
            .map(|(_, height)| *height)
    }

    pub(super) fn insert(&mut self, id: &str, key: CardKey, height: usize) {
        self.heights.insert(id.to_owned(), (key, height));
    }
}
