//! The plugin's pure core: the stable-field manifest hash and the poke-policy
//! state machine. Time is injected as Unix milliseconds and no `zellij-tile`
//! type appears, so this module compiles and unit-tests on the host target;
//! `main.rs` is the thin wasm shell that projects Zellij events into it.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

/// Floor between two `panes-changed` pokes — caps host forks under
/// pathological manifest churn. A change that lands inside the floor is
/// deferred, never dropped.
pub const POKE_FLOOR_MS: u64 = 100;

/// Follow-up after a pane-changing poke. Zellij can deliver `CommandChanged`
/// before `list-panes` has converged on the new foreground command; this second
/// poke forces one settled frame instead of letting the stretched event-mode
/// pane TTL carry the pre-change command.
pub const SETTLE_POKE_MS: u64 = 250;

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneFields {
    pub id: u32,
    pub is_plugin: bool,
    pub is_focused: bool,
    pub is_suppressed: bool,
    pub exited: bool,
    pub is_held: bool,
    pub title: String,
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

impl PaneFields {
    fn is_live_terminal(&self) -> bool {
        !self.is_plugin && !self.is_suppressed && !self.exited && !self.is_held
    }

    fn is_sidebar(&self) -> bool {
        self.is_live_terminal()
            && (self.title == SIDEBAR_PANE_TITLE
                || self.terminal_command.as_deref() == Some(SIDEBAR_PANE_TITLE))
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

/// The terminal pane that should take focus after switching to `active_tab`, if
/// Zellij restored the tab's focus to the sidebar. `None` means the tab is
/// already on work, has no sidebar focus, or has no live working pane.
pub fn switched_tab_focus_target(
    tabs: &BTreeMap<usize, Vec<PaneFields>>,
    active_tab: Option<usize>,
) -> Option<u32> {
    let panes = tabs.get(&active_tab?)?;
    let focused = panes.iter().find(|pane| pane.is_focused)?;
    if !focused.is_sidebar() {
        return None;
    }
    panes
        .iter()
        .find(|pane| pane.is_live_terminal() && !pane.is_sidebar())
        .map(|pane| pane.id)
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
