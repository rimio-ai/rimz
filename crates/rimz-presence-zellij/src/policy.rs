//! The plugin's pure decision core: the stable-field manifest hash, poke
//! policy, foreground overlay, and focus-stranding classifier. Time is injected
//! as Unix milliseconds and no `zellij-tile` type appears, so this module
//! compiles and unit-tests on the host target; `main.rs` is the thin wasm shell
//! that projects Zellij events into it.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};

/// Floor between two `panes-changed` pokes — caps host forks under
/// pathological manifest churn. A change that lands inside the floor is
/// deferred, never dropped.
pub const POKE_FLOOR_MS: u64 = 100;

/// Follow-up after a pane-changing poke. Zellij can deliver `CommandChanged`
/// before the manifest has converged on the new foreground command; this second
/// poke forces one settled frame instead of letting the stretched event-mode
/// pane TTL carry the pre-change command.
pub const SETTLE_POKE_MS: u64 = 250;

/// Settle window after a tab switch before a stale pane manifest may classify
/// the tab as stranded on its sidebar. A fresh `PaneUpdate` resolves
/// non-stranded work immediately, but broadcasts still wait for this deadline
/// because Zellij does not guarantee TabUpdate/PaneUpdate delivery order. A
/// jump whose focus-mark manifest arrives after the window can still be
/// misclassified; the window is the correction latency/risk bound.
pub const FOCUS_SETTLE_MS: u64 = 250;

/// Keepalive cadence. One host fork per minute per session keeps an
/// idle-but-healthy channel distinguishable from a dead one; the host's
/// `PRESENCE_STAMP_FRESH` (150s) allows two missed keepalives of slack.
pub const KEEPALIVE_MS: u64 = 60_000;

/// Pane title the Zellij layouts assign to RimZ's native sidebar.
pub const SIDEBAR_PANE_TITLE: &str = "rimz-sidebar";

/// The pane fields the plugin projects. The manifest hash folds only the stable
/// subset whose change means the sidebar should refetch panes. `title` and
/// `pane_command` are carried for focus correction and topology publication but
/// deliberately excluded from the hash: agents mutate titles per output line,
/// and foreground command changes already publish through `CommandChanged`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PaneFields {
    pub id: u32,
    pub is_plugin: bool,
    pub is_focused: bool,
    pub is_suppressed: bool,
    pub is_floating: bool,
    pub exited: bool,
    pub is_held: bool,
    pub tab_position: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_x: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_columns: Option<u64>,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneBaseline {
    pub command: String,
    pub cwd: Option<String>,
}

/// Stable pane fields folded before the wasm shell allocates projected
/// [`PaneFields`]. This mirrors [`manifest_hash`]'s per-pane field set:
/// title and foreground `pane_command` stay out because they churn without
/// changing the sidebar roster.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub struct RawStablePaneFields<'a> {
    pub id: u32,
    pub is_plugin: bool,
    pub is_focused: bool,
    pub is_suppressed: bool,
    pub is_floating: bool,
    pub exited: bool,
    pub is_held: bool,
    pub tab_position: u64,
    pub tab_name: Option<&'a str>,
    pub pane_x: Option<u64>,
    pub pane_columns: Option<u64>,
    pub terminal_command: Option<&'a str>,
}

impl<'a> RawStablePaneFields<'a> {
    fn from_projected(pane: &'a PaneFields) -> Self {
        Self {
            id: pane.id,
            is_plugin: pane.is_plugin,
            is_focused: pane.is_focused,
            is_suppressed: pane.is_suppressed,
            is_floating: pane.is_floating,
            exited: pane.exited,
            is_held: pane.is_held,
            tab_position: pane.tab_position,
            tab_name: pane.tab_name.as_deref(),
            pane_x: pane.pane_x,
            pane_columns: pane.pane_columns,
            terminal_command: pane.terminal_command.as_deref(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusPatch {
    pub id: u32,
    pub is_focused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TopologyPayload {
    pub session_name: String,
    pub produced_at_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub writer: Option<TopologyWriter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_pane: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clients: Option<ClientSample>,
    pub panes: Vec<PaneFields>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientSample {
    pub human_clients: u32,
    pub viewed_panes: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopologyWriter {
    pub plugin_id: u32,
    pub loaded_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<String>,
}

pub fn published_topology_payload(
    session_name: impl Into<String>,
    produced_at_ms: u64,
    writer: Option<TopologyWriter>,
    focused_pane: Option<u32>,
    clients: Option<ClientSample>,
    tabs: &BTreeMap<usize, Vec<PaneFields>>,
) -> Option<TopologyPayload> {
    if tabs.is_empty() {
        return None;
    }
    Some(TopologyPayload {
        session_name: session_name.into(),
        produced_at_ms,
        writer,
        focused_pane,
        clients,
        panes: tabs.values().flatten().cloned().collect(),
    })
}

impl PaneFields {
    pub fn from_stable(stable: &RawStablePaneFields<'_>, title: String) -> Self {
        Self {
            id: stable.id,
            is_plugin: stable.is_plugin,
            is_focused: stable.is_focused,
            is_suppressed: stable.is_suppressed,
            is_floating: stable.is_floating,
            exited: stable.exited,
            is_held: stable.is_held,
            tab_position: stable.tab_position,
            tab_name: stable.tab_name.map(str::to_owned),
            pane_x: stable.pane_x,
            pane_columns: stable.pane_columns,
            title,
            pane_command: None,
            pane_cwd: None,
            terminal_command: stable.terminal_command.map(str::to_owned),
        }
    }

    fn is_live_terminal(&self) -> bool {
        !self.is_plugin && !self.is_suppressed && !self.is_floating && !self.exited && !self.is_held
    }

    fn is_sidebar(&self) -> bool {
        self.is_live_terminal() && self.title == SIDEBAR_PANE_TITLE
    }

    pub fn is_card_pane(&self) -> bool {
        self.is_live_terminal() && !self.is_sidebar()
    }
}

/// Fold the projected manifest into one stable hash. The `BTreeMap` keying by
/// tab position makes iteration order deterministic regardless of the host
/// map's order; callers sort each tab's panes by id before inserting. The
/// active tab is deliberately excluded: tab switches are navigation, while the
/// sidebar's row roster and selection baseline change only when the per-pane
/// fields change. The value only ever compares against the previous hash in
/// this process, so no cross-version stability is needed.
pub fn manifest_hash(tabs: &BTreeMap<usize, Vec<PaneFields>>) -> u64 {
    raw_stable_hash(tabs.iter().flat_map(|(tab, panes)| {
        panes
            .iter()
            .map(move |pane| (*tab, RawStablePaneFields::from_projected(pane)))
    }))
}

/// Fold raw stable pane fields without allocating projected [`PaneFields`].
/// The caller may feed raw host order; an order-only difference costs one full
/// fold, while a title-only event stays cheap because title is absent from
/// [`RawStablePaneFields`].
pub fn raw_stable_hash<'a, I>(panes: I) -> u64
where
    I: IntoIterator<Item = (usize, RawStablePaneFields<'a>)>,
{
    let mut hasher = std::hash::DefaultHasher::new();
    for (tab, pane) in panes {
        tab.hash(&mut hasher);
        pane.hash(&mut hasher);
    }
    hasher.finish()
}

/// The focus transitions to publish when the only sidebar-relevant manifest
/// change is a per-pane focus move onto a pane that can render as an
/// agent/process card or the sidebar pane itself. `Some(patch)` is the
/// optimistic focus patch; `None` means no patch is available, so the caller
/// falls back to an authoritative pane produce. That includes topology,
/// command, held/exited, suppression changes, and focus moves onto other
/// chrome.
pub fn focus_shortcut_if_only_focus_changed(
    previous: &BTreeMap<usize, Vec<PaneFields>>,
    next: &BTreeMap<usize, Vec<PaneFields>>,
) -> Option<Vec<FocusPatch>> {
    let mut changed = false;
    let mut focused_card = false;
    let mut focused_sidebar = false;
    let mut patch = Vec::new();
    if previous.len() != next.len() {
        return None;
    }
    for (tab, previous_panes) in previous {
        let next_panes = next.get(tab)?;
        if previous_panes.len() != next_panes.len() {
            return None;
        }
        for (previous, next) in previous_panes.iter().zip(next_panes) {
            let mut previous_stable = RawStablePaneFields::from_projected(previous);
            let mut next_stable = RawStablePaneFields::from_projected(next);
            previous_stable.is_focused = false;
            next_stable.is_focused = false;
            if previous_stable != next_stable
                || previous.pane_command != next.pane_command
                || previous.pane_cwd != next.pane_cwd
            {
                return None;
            }
            changed |= previous.is_focused != next.is_focused;
            if previous.is_focused != next.is_focused && next.is_focused && next.is_card_pane() {
                focused_card = true;
            }
            if previous.is_focused != next.is_focused && next.is_focused && next.is_sidebar() {
                focused_sidebar = true;
            }
            if previous.is_focused != next.is_focused && (next.is_card_pane() || next.is_sidebar())
            {
                patch.push(FocusPatch {
                    id: next.id,
                    is_focused: next.is_focused,
                });
            }
        }
    }
    if !changed {
        return None;
    }
    (focused_card || focused_sidebar).then_some(patch)
}

/// The card panes `next` holds that `previous` does not — the genuinely new
/// panes a manifest reports, each worth one card-create poke. The first
/// manifest after plugin load has no `previous` and names every pre-existing
/// pane; those are not opens — the producer's pull already covers the room —
/// so an empty `previous` reports nothing.
pub fn opened_card_panes(
    previous: &BTreeMap<usize, Vec<PaneFields>>,
    next: &BTreeMap<usize, Vec<PaneFields>>,
) -> Vec<PaneFields> {
    if previous.is_empty() {
        return Vec::new();
    }
    let mut opened = Vec::new();
    for panes in next.values() {
        for pane in panes {
            if pane.is_card_pane()
                && !previous
                    .values()
                    .flatten()
                    .any(|old| old.id == pane.id && old.is_plugin == pane.is_plugin)
            {
                opened.push(pane.clone());
            }
        }
    }
    opened
}

/// Merge a Zellij pane manifest without letting a partial `PaneUpdate` churn
/// the detection map. Zellij can deliver a manifest that carries only part of
/// the room — a tab omitted entirely, or carried with an empty pane list on its
/// ~60s serialization blip — so carried non-empty tabs overwrite exactly while
/// both an absent tab and a present-but-empty one retain what they last held
/// until `PaneClosed` removes their panes. The first manifest after plugin load
/// is the baseline; a partial first manifest self-heals on the next manifest
/// that carries the omitted tabs. Publication uses this merged room, so partial
/// `PaneUpdate` bursts neither shrink the topology cache nor emit spurious
/// `panes-changed` pokes.
///
/// Retention is bounded by one-tab-per-pane: a pane id is unique across the
/// session, so a copy left in a tab Zellij renumbered or dropped without a
/// `PaneClosed` is stale and is evicted once the pane is reported fresh
/// elsewhere — without this a moved/renumbered tab leaks the same pane into
/// several tab positions forever.
pub fn merged_room(
    previous: &BTreeMap<usize, Vec<PaneFields>>,
    next: &BTreeMap<usize, Vec<PaneFields>>,
) -> BTreeMap<usize, Vec<PaneFields>> {
    if previous.is_empty() {
        return next
            .iter()
            .filter(|(_, panes)| !panes.is_empty())
            .map(|(tab, panes)| (*tab, panes.clone()))
            .collect();
    }
    let mut merged = previous.clone();
    for (tab, panes) in next {
        // An empty pane vector is a serialization artifact, never a real empty
        // tab: a live tab always holds at least one pane, and a tab's panes
        // leave through `PaneClosed`. Treat it like an omitted tab — retain what
        // the tab last held — so a transiently-partial manifest cannot collapse
        // an idle tab out of the published room.
        if panes.is_empty() {
            continue;
        }
        merged.insert(*tab, panes.clone());
    }
    enforce_one_tab_per_pane(&mut merged, next);
    merged
}

/// Drop every duplicate of a pane so each `(is_plugin, id)` lives under one tab.
/// The fresh manifest is authoritative for where a pane lives, so a copy in any
/// other tab is evicted; a residual duplicate among retained-only tabs keeps its
/// lowest-positioned occurrence. Tabs emptied by eviction are removed.
fn enforce_one_tab_per_pane(
    room: &mut BTreeMap<usize, Vec<PaneFields>>,
    authoritative: &BTreeMap<usize, Vec<PaneFields>>,
) {
    let mut home = BTreeMap::new();
    for (tab, panes) in authoritative {
        for pane in panes {
            home.insert((pane.is_plugin, pane.id), *tab);
        }
    }
    let mut kept = BTreeSet::new();
    for (tab, panes) in room.iter_mut() {
        panes.retain(|pane| {
            let key = (pane.is_plugin, pane.id);
            match home.get(&key) {
                Some(fresh_tab) => fresh_tab == tab,
                None => kept.insert(key),
            }
        });
    }
    room.retain(|_, panes| !panes.is_empty());
}

pub fn remove_pane_from_tabs(
    tabs: &mut BTreeMap<usize, Vec<PaneFields>>,
    is_plugin: bool,
    id: u32,
) {
    for panes in tabs.values_mut() {
        panes.retain(|pane| pane.is_plugin != is_plugin || pane.id != id);
    }
    tabs.retain(|_, panes| !panes.is_empty());
}

pub fn joined_foreground_command(command: &[String]) -> Option<String> {
    let joined = command
        .iter()
        .filter(|arg| !arg.is_empty())
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    (!joined.is_empty()).then_some(joined)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForegroundCommandUpdate {
    Remember(String),
    Forget,
}

/// Project one Zellij `CommandChanged` event into the retained foreground map.
/// A non-foreground or empty foreground event means the previous foreground
/// tenant ended; any real foreground command replaces it. Launch-chrome
/// scrubbing lives in the host wake path.
pub fn foreground_command_update(
    command: &[String],
    is_foreground: bool,
) -> ForegroundCommandUpdate {
    if !is_foreground {
        return ForegroundCommandUpdate::Forget;
    }
    match joined_foreground_command(command) {
        Some(command) => ForegroundCommandUpdate::Remember(command),
        None => ForegroundCommandUpdate::Forget,
    }
}

pub fn apply_foreground_commands(
    tabs: &mut BTreeMap<usize, Vec<PaneFields>>,
    foreground: &BTreeMap<u32, String>,
    baseline: &BTreeMap<u32, PaneBaseline>,
) {
    for pane in tabs.values_mut().flatten() {
        if pane.is_plugin {
            continue;
        }
        pane.pane_command = foreground.get(&pane.id).cloned().or_else(|| {
            baseline
                .get(&pane.id)
                .map(|baseline| baseline.command.clone())
        });
        pane.pane_cwd = baseline
            .get(&pane.id)
            .and_then(|baseline| baseline.cwd.clone());
    }
}

pub fn panes_needing_baseline(
    tabs: &BTreeMap<usize, Vec<PaneFields>>,
    foreground: &BTreeMap<u32, String>,
    baseline: &BTreeMap<u32, PaneBaseline>,
) -> Vec<u32> {
    tabs.values()
        .flatten()
        .filter(|pane| {
            pane.is_live_terminal()
                && pane.terminal_command.as_deref().is_none_or(str::is_empty)
                && !foreground.contains_key(&pane.id)
                && !baseline.contains_key(&pane.id)
        })
        .map(|pane| pane.id)
        .collect()
}

/// The focused sidebar pane after switching to `active_tab`, if Zellij restored
/// the tab's focus to the sidebar while a live working sibling exists. `None`
/// means the tab is already on work, has no sidebar focus, or has no live
/// working pane.
pub fn stranded_sidebar_pane(
    tabs: &BTreeMap<usize, Vec<PaneFields>>,
    active_tab: Option<usize>,
    session_focused_pane: Option<u32>,
) -> Option<u32> {
    let active_tab = active_tab?;
    let panes = tabs.get(&active_tab)?;
    let focused_id = resolved_focused_pane_id(tabs, Some(active_tab), session_focused_pane)?;
    let focused = panes
        .iter()
        .find(|pane| pane.id == focused_id && pane.is_live_terminal())?;
    if !focused.is_sidebar() {
        return None;
    }
    panes
        .iter()
        .any(|pane| pane.is_live_terminal() && !pane.is_sidebar())
        .then_some(focused.id)
}

pub fn resolved_focused_pane_id(
    tabs: &BTreeMap<usize, Vec<PaneFields>>,
    active_tab: Option<usize>,
    session_focused_pane: Option<u32>,
) -> Option<u32> {
    resolved_focused_pane(tabs, active_tab, session_focused_pane).map(|pane| pane.id)
}

pub fn manifest_focused_tiled(
    tabs: &BTreeMap<usize, Vec<PaneFields>>,
    active_tab: Option<usize>,
) -> Option<u32> {
    tabs.get(&active_tab?)?
        .iter()
        .find(|pane| pane.is_live_terminal() && pane.is_focused)
        .map(|pane| pane.id)
}

pub fn focus_tiled_pane(tabs: &mut BTreeMap<usize, Vec<PaneFields>>, focused: u32) {
    let Some(tab) = tabs.iter().find_map(|(tab, panes)| {
        panes
            .iter()
            .any(|pane| pane.id == focused && pane.is_live_terminal())
            .then_some(*tab)
    }) else {
        return;
    };
    if let Some(panes) = tabs.get_mut(&tab) {
        for pane in panes {
            if pane.is_live_terminal() {
                pane.is_focused = pane.id == focused;
            }
        }
    }
}

pub fn repair_contested_tab_focus(
    tabs: &mut BTreeMap<usize, Vec<PaneFields>>,
    client_viewed: &[u32],
    prior_focused_by_tab: &BTreeMap<usize, u32>,
) {
    let client_viewed: BTreeSet<u32> = client_viewed.iter().copied().collect();
    for (tab, panes) in tabs {
        let focused = panes
            .iter()
            .filter(|pane| pane.is_focused && !pane.is_floating)
            .map(|pane| pane.id)
            .collect::<Vec<_>>();
        if focused.len() <= 1 {
            continue;
        }
        let keep = focused
            .iter()
            .copied()
            .filter(|id| client_viewed.contains(id))
            .min()
            .or_else(|| {
                prior_focused_by_tab
                    .get(tab)
                    .copied()
                    .filter(|prior| focused.contains(prior))
            })
            .or_else(|| focused.iter().copied().min());
        if let Some(keep) = keep {
            for pane in panes {
                if pane.is_focused && !pane.is_floating && pane.id != keep {
                    pane.is_focused = false;
                }
            }
        }
    }
}

pub fn is_card_pane_id(tabs: &BTreeMap<usize, Vec<PaneFields>>, pane_id: u32) -> bool {
    tabs.values()
        .flatten()
        .any(|pane| pane.id == pane_id && pane.is_card_pane())
}

fn resolved_focused_pane(
    tabs: &BTreeMap<usize, Vec<PaneFields>>,
    active_tab: Option<usize>,
    session_focused_pane: Option<u32>,
) -> Option<&PaneFields> {
    let active_tab = active_tab?;
    let panes = tabs.get(&active_tab)?;
    if let Some(resolved) = session_focused_pane
        && let Some(pane) = panes
            .iter()
            .find(|pane| pane.id == resolved && pane.is_live_terminal())
    {
        return Some(pane);
    }
    panes
        .iter()
        .find(|pane| pane.is_live_terminal() && pane.is_focused)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingFocusCorrection {
    tab: usize,
    deadline: u64,
    previous_focused_pane: Option<u32>,
}

/// What the plugin shell should do for a switched-tab focus correction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorrectionAction {
    /// The pending classification is not ready, or no classification is armed.
    Wait,
    /// The pending classification resolved without a focus event to publish.
    Clear,
    /// Broadcast `focus-stranded` for this sidebar pane id.
    StrandedSidebar(u32),
    /// Broadcast `focus-changed` for a tab switch that restored work focus.
    FocusWorkingPane {
        focused: u32,
        unfocused: Option<u32>,
    },
}

/// Classifies focus transitions caused by a tab switch. Zellij keeps per-tab
/// focus marks, so a pure switch needs an explicit sidebar wake even when the
/// pane manifest did not change.
#[derive(Debug, Default)]
pub struct FocusCorrection {
    pending: Option<PendingFocusCorrection>,
}

impl FocusCorrection {
    /// Fold an active-tab observation. Loading the plugin (`None -> Some`) is a
    /// baseline, not navigation; only real tab switches arm classification.
    pub fn on_active_tab_change(
        &mut self,
        previous_active: Option<usize>,
        next_active: Option<usize>,
        previous_focused_pane: Option<u32>,
        now_ms: u64,
    ) {
        match (previous_active, next_active) {
            (_, None) => self.pending = None,
            (Some(previous), Some(next)) if previous != next => {
                self.pending = Some(PendingFocusCorrection {
                    tab: next,
                    deadline: now_ms + FOCUS_SETTLE_MS,
                    previous_focused_pane,
                });
            }
            (None, Some(_)) => {}
            (Some(_), Some(_)) => {}
        }
    }

    /// Resolve the pending classification. A fresh manifest is authoritative
    /// immediately; a stale manifest is consulted only after the settle
    /// deadline, giving cross-tab explicit jumps time to land their focus mark.
    pub fn resolve(
        &mut self,
        tabs: &BTreeMap<usize, Vec<PaneFields>>,
        active_tab: Option<usize>,
        session_focused_pane: Option<u32>,
        manifest_fresh: bool,
        now_ms: u64,
    ) -> CorrectionAction {
        let Some(pending) = self.pending else {
            return CorrectionAction::Wait;
        };
        if active_tab != Some(pending.tab) || !tabs.contains_key(&pending.tab) {
            self.pending = None;
            return CorrectionAction::Clear;
        }
        if !manifest_fresh && now_ms < pending.deadline {
            return CorrectionAction::Wait;
        }
        if resolved_focused_pane_id(tabs, Some(pending.tab), session_focused_pane)
            == pending.previous_focused_pane
            && pending.previous_focused_pane.is_some()
        {
            self.pending = None;
            return CorrectionAction::Clear;
        }
        if manifest_fresh
            && session_focused_pane.is_some_and(|focused| {
                pending.previous_focused_pane != Some(focused)
                    && resolved_focused_pane_id(tabs, Some(pending.tab), Some(focused))
                        == Some(focused)
                    && is_card_pane_id(tabs, focused)
            })
        {
            self.pending = None;
            return CorrectionAction::Clear;
        }
        match stranded_sidebar_pane(tabs, Some(pending.tab), session_focused_pane) {
            Some(_) if now_ms < pending.deadline => CorrectionAction::Wait,
            Some(pane_id) => {
                self.pending = None;
                CorrectionAction::StrandedSidebar(pane_id)
            }
            None => {
                let focused = resolved_focused_pane(tabs, Some(pending.tab), session_focused_pane)
                    .filter(|pane| pane.is_card_pane())
                    .map(|pane| pane.id);
                self.pending = None;
                match focused {
                    Some(focused) => CorrectionAction::FocusWorkingPane {
                        focused,
                        unfocused: pending.previous_focused_pane,
                    },
                    None => CorrectionAction::Clear,
                }
            }
        }
    }

    pub fn next_deadline(&self) -> Option<u64> {
        self.pending.map(|pending| pending.deadline)
    }

    pub fn pending_tab(&self) -> Option<usize> {
        self.pending.map(|pending| pending.tab)
    }
}

/// What the shell should do now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Poke {
    /// Run `rimz sidebar wake --reason panes-changed`.
    Changed,
    /// Run `rimz sidebar wake --reason alive`.
    Alive,
}

/// The poke-policy state machine. The shell feeds it manifest hashes and
/// clock readings; it answers "which pokes are due" and "when to wake next".
/// Spurious wake-ups are harmless — [`PokePolicy::due`] is idempotent between
/// deadline crossings — so the shell may consult it on every event.
#[derive(Debug)]
pub struct PokePolicy {
    last_hash: Option<u64>,
    last_optimistic_poke_by_pane: BTreeMap<u32, u64>,
    /// First change of the current duplicate burst. The first change pokes
    /// immediately; a later change inside [`POKE_FLOOR_MS`] is held until the
    /// floor lifts and then pokes once for the burst.
    pending_since: Option<u64>,
    last_changed_poke: Option<u64>,
    /// One post-change settle poke. A fresh real change before it fires
    /// supersedes the old settle and arms a new one after that change's poke.
    settle_due_at: Option<u64>,
    next_keepalive: u64,
}

impl PokePolicy {
    pub fn new(now_ms: u64) -> Self {
        Self {
            last_hash: None,
            last_optimistic_poke_by_pane: BTreeMap::new(),
            pending_since: None,
            last_changed_poke: None,
            settle_due_at: None,
            next_keepalive: now_ms + KEEPALIVE_MS,
        }
    }

    /// Fold a manifest observation. The first manifest after load is the
    /// baseline — the room did not change, the plugin just learned it — so it
    /// arms nothing.
    pub fn on_manifest(&mut self, hash: u64, now_ms: u64) {
        if self.last_hash == Some(hash) {
            return;
        }
        let baseline = self.last_hash.is_none();
        self.last_hash = Some(hash);
        if baseline {
            return;
        }
        self.queue_change(now_ms);
    }

    /// Accept a manifest observation without queuing a producer poke. Used after
    /// an optimistic direct event already reached the renderer.
    pub fn accept_manifest(&mut self, hash: u64) {
        self.last_hash = Some(hash);
    }

    /// Fold an explicit host signal that means the live pane frame should be
    /// refreshed even when no full manifest accompanies it.
    pub fn on_signal(&mut self, now_ms: u64) {
        self.queue_change(now_ms);
    }

    /// Fold an optimistic change that already published a command patch through
    /// the host CLI. It skips the immediate `panes-changed` poke and arms only
    /// the settled read that verifies the patch against Zellij's pane list.
    pub fn on_optimistic_signal(&mut self, now_ms: u64) {
        self.pending_since = None;
        self.last_changed_poke = Some(now_ms);
        self.settle_due_at = Some(now_ms + SETTLE_POKE_MS);
    }

    /// Whether a same-pane optimistic command patch may fork the host now.
    /// Repeated command churn for one pane uses the normal floored change path;
    /// distinct panes keep their own fast lane so a burst in one pane does not
    /// hide another pane's first foreground change.
    pub fn optimistic_pane_poke_allowed(&self, pane_id: u32, now_ms: u64) -> bool {
        self.last_optimistic_poke_by_pane
            .get(&pane_id)
            .is_none_or(|last| now_ms >= last.saturating_add(POKE_FLOOR_MS))
    }

    /// Record an emitted same-pane optimistic command patch and arm its settled
    /// verification read.
    pub fn accept_optimistic_pane_poke(&mut self, pane_id: u32, now_ms: u64) {
        self.last_optimistic_poke_by_pane.insert(pane_id, now_ms);
        self.on_optimistic_signal(now_ms);
    }

    pub fn forget_pane(&mut self, pane_id: u32) {
        self.last_optimistic_poke_by_pane.remove(&pane_id);
    }

    fn queue_change(&mut self, now_ms: u64) {
        if self.pending_since.is_none() {
            self.pending_since = Some(now_ms);
        }
    }

    /// The pokes due at `now_ms`, consuming them. A pending change fires
    /// immediately unless it arrived inside the duplicate floor; the keepalive
    /// fires on its own cadence regardless of change traffic.
    pub fn due(&mut self, now_ms: u64) -> Vec<Poke> {
        let mut pokes = Vec::new();
        let mut fired_pending_change = false;
        if let Some(since) = self.pending_since {
            let due_at = match self.last_changed_poke {
                Some(at) => since.max(at.saturating_add(POKE_FLOOR_MS)),
                None => since,
            };
            if now_ms >= due_at {
                self.pending_since = None;
                self.last_changed_poke = Some(now_ms);
                self.settle_due_at = Some(now_ms + SETTLE_POKE_MS);
                fired_pending_change = true;
                pokes.push(Poke::Changed);
            }
        }
        if !fired_pending_change && self.settle_due_at.is_some_and(|due_at| now_ms >= due_at) {
            self.settle_due_at = None;
            self.last_changed_poke = Some(now_ms);
            pokes.push(Poke::Changed);
        }
        if now_ms >= self.next_keepalive {
            self.next_keepalive = now_ms + KEEPALIVE_MS;
            pokes.push(Poke::Alive);
        }
        pokes
    }

    /// The next absolute instant [`PokePolicy::due`] should be consulted.
    /// Always `Some` — the keepalive deadline never disappears.
    pub fn next_wake_at(&self) -> u64 {
        let change_at = self
            .pending_since
            .map(|since| match self.last_changed_poke {
                Some(at) => since.max(at.saturating_add(POKE_FLOOR_MS)),
                None => since,
            });
        [change_at, self.settle_due_at, Some(self.next_keepalive)]
            .into_iter()
            .flatten()
            .min()
            .expect("keepalive deadline is always present")
    }
}

/// Dedupes host timers over the policy's deadlines. Zellij timers are
/// one-shot and anonymous — a fired timer does not say which deadline it was
/// armed for — so the gate tracks the earliest armed deadline and lets a
/// superseded timer's late fire read as stale instead of clearing the mark.
/// Clearing on every fire would arm a duplicate for the still-outstanding
/// deadline, and since every fire re-arms one successor, each duplicate is a
/// chain that never collapses — wakeups would grow with every event burst
/// over a session's lifetime.
#[derive(Debug, Default)]
pub struct TimerGate {
    armed_for: Option<u64>,
}

impl TimerGate {
    /// Record a host timer firing at `now_ms`. The mark clears only when the
    /// armed-for deadline has arrived; an earlier fire is a stale timer from
    /// a superseded chain, with the marked deadline still outstanding.
    pub fn on_fire(&mut self, now_ms: u64) {
        if self.armed_for.is_some_and(|at| at <= now_ms) {
            self.armed_for = None;
        }
    }

    /// Whether the shell should arm a host timer for deadline `at`: yes when
    /// nothing is armed or `at` precedes the armed deadline (the superseded
    /// later timer then fires as a harmless no-op). Marks `at` as armed when
    /// answering yes.
    pub fn should_arm(&mut self, at: u64) -> bool {
        if self.armed_for.is_none_or(|armed| at < armed) {
            self.armed_for = Some(at);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests;
