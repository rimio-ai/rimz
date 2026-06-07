//! The plugin's pure core: the stable-field manifest hash, poke policy, and
//! focus-stranding classifier. Time is injected as Unix milliseconds and no
//! `zellij-tile` type appears, so this module compiles and unit-tests on the
//! host target; `main.rs` is the thin wasm shell that projects Zellij events
//! into it.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

use serde::Serialize;

/// Floor between two `panes-changed` pokes — caps host forks under
/// pathological manifest churn. A change that lands inside the floor is
/// deferred, never dropped.
pub const POKE_FLOOR_MS: u64 = 100;

/// Follow-up after a pane-changing poke. Zellij can deliver `CommandChanged`
/// before `list-panes` has converged on the new foreground command; this second
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

/// Pane title the Zellij layouts assign to Rimz's native sidebar.
pub const SIDEBAR_PANE_TITLE: &str = "rimz-sidebar";

/// The pane fields the plugin projects. The manifest hash folds only the stable
/// subset whose change means the sidebar should refetch panes. `title` is
/// carried for focus correction but deliberately excluded from the hash: agents
/// mutate titles per output line, and hashing them would re-poke per line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PaneFields {
    pub id: u32,
    pub is_plugin: bool,
    pub is_focused: bool,
    pub is_suppressed: bool,
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
    pub terminal_command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusPatch {
    pub id: u32,
    pub is_focused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusShortcut {
    Patch(Vec<FocusPatch>),
    Ignore,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TopologyPayload {
    pub session_name: String,
    pub produced_at_ms: u64,
    pub panes: Vec<PaneFields>,
}

impl TopologyPayload {
    pub fn from_tabs(
        session_name: impl Into<String>,
        produced_at_ms: u64,
        tabs: &BTreeMap<usize, Vec<PaneFields>>,
    ) -> Self {
        Self {
            session_name: session_name.into(),
            produced_at_ms,
            panes: tabs.values().flatten().cloned().collect(),
        }
    }
}

impl PaneFields {
    fn is_live_terminal(&self) -> bool {
        !self.is_plugin && !self.is_suppressed && !self.exited && !self.is_held
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
pub fn manifest_hash(tabs: &BTreeMap<usize, Vec<PaneFields>>, _active_tab: Option<usize>) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    for (tab, panes) in tabs {
        tab.hash(&mut hasher);
        for pane in panes {
            pane.id.hash(&mut hasher);
            pane.is_plugin.hash(&mut hasher);
            pane.is_focused.hash(&mut hasher);
            pane.is_suppressed.hash(&mut hasher);
            pane.exited.hash(&mut hasher);
            pane.is_held.hash(&mut hasher);
            pane.tab_position.hash(&mut hasher);
            pane.tab_name.hash(&mut hasher);
            pane.pane_x.hash(&mut hasher);
            pane.pane_columns.hash(&mut hasher);
            pane.terminal_command.hash(&mut hasher);
        }
    }
    hasher.finish()
}

/// The focus values to publish when the only sidebar-relevant manifest change is
/// a per-pane focus move onto a pane that can render as an agent/process card.
/// Returns [`FocusShortcut::Ignore`] for focus-only moves onto sidebar/chrome so
/// selection holds its last card, and `None` for topology, command, held/exited,
/// or suppression changes so the caller falls back to an authoritative pane
/// produce.
pub fn focus_shortcut_if_only_focus_changed(
    previous: &BTreeMap<usize, Vec<PaneFields>>,
    next: &BTreeMap<usize, Vec<PaneFields>>,
) -> Option<FocusShortcut> {
    let mut changed = false;
    let mut focused_card = false;
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
            if previous.id != next.id
                || previous.is_plugin != next.is_plugin
                || previous.is_suppressed != next.is_suppressed
                || previous.exited != next.exited
                || previous.is_held != next.is_held
                || previous.tab_position != next.tab_position
                || previous.tab_name != next.tab_name
                || previous.pane_x != next.pane_x
                || previous.pane_columns != next.pane_columns
                || previous.terminal_command != next.terminal_command
            {
                return None;
            }
            changed |= previous.is_focused != next.is_focused;
            if next.is_focused && next.is_card_pane() {
                focused_card = true;
            }
            if next.is_card_pane() {
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
    if focused_card {
        Some(FocusShortcut::Patch(patch))
    } else {
        Some(FocusShortcut::Ignore)
    }
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

/// The focused sidebar pane after switching to `active_tab`, if Zellij restored
/// the tab's focus to the sidebar while a live working sibling exists. `None`
/// means the tab is already on work, has no sidebar focus, or has no live
/// working pane.
pub fn stranded_sidebar_pane(
    tabs: &BTreeMap<usize, Vec<PaneFields>>,
    active_tab: Option<usize>,
) -> Option<u32> {
    let panes = tabs.get(&active_tab?)?;
    let focused = focused_pane(tabs, active_tab)?;
    if !focused.is_sidebar() {
        return None;
    }
    panes
        .iter()
        .any(|pane| pane.is_live_terminal() && !pane.is_sidebar())
        .then_some(focused.id)
}

pub fn focused_pane_id(
    tabs: &BTreeMap<usize, Vec<PaneFields>>,
    active_tab: Option<usize>,
) -> Option<u32> {
    focused_pane(tabs, active_tab).map(|pane| pane.id)
}

fn focused_pane(
    tabs: &BTreeMap<usize, Vec<PaneFields>>,
    active_tab: Option<usize>,
) -> Option<&PaneFields> {
    tabs.get(&active_tab?)?.iter().find(|pane| pane.is_focused)
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
    /// The pending classification resolved without a stranded sidebar.
    Clear,
    /// Broadcast `focus-stranded` for this sidebar pane id.
    Broadcast(u32),
}

/// Classifies whether a tab switch restored focus to Rimz's sidebar. The plugin
/// only reports the stranded sidebar id; the renderer that owns that pane
/// decides whether and where to move focus.
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
        now_ms: u64,
    ) {
        self.on_active_tab_change_with_focus(previous_active, next_active, None, now_ms);
    }

    /// Fold an active-tab observation with the pane that was focused in the
    /// previous active tab. If the new active tab reports the same focused pane,
    /// Zellij renumbered tab positions rather than moving the user.
    pub fn on_active_tab_change_with_focus(
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
        if focused_pane_id(tabs, Some(pending.tab)) == pending.previous_focused_pane
            && pending.previous_focused_pane.is_some()
        {
            self.pending = None;
            return CorrectionAction::Clear;
        }
        match stranded_sidebar_pane(tabs, Some(pending.tab)) {
            Some(_) if now_ms < pending.deadline => CorrectionAction::Wait,
            Some(pane_id) => {
                self.pending = None;
                CorrectionAction::Broadcast(pane_id)
            }
            None => {
                self.pending = None;
                CorrectionAction::Clear
            }
        }
    }

    pub fn next_deadline(&self) -> Option<u64> {
        self.pending.map(|pending| pending.deadline)
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

    /// Accept a manifest observation without queuing a producer poke. Used for a
    /// focus-only move to sidebar/chrome: the renderer's baseline should hold
    /// its last card, and no pane truth changed.
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
