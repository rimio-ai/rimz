//! The plugin's pure core: the stable-field manifest hash and the poke-policy
//! state machine. Time is injected as Unix milliseconds and no `zellij-tile`
//! type appears, so this module compiles and unit-tests on the host target;
//! `lib.rs` is the thin wasm shell that projects Zellij events into it.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

/// One poke is debounced this long after the first change of a burst, so a
/// split (which fans out as several manifest events) collapses to one poke.
pub const DEBOUNCE_MS: u64 = 200;

/// Floor between two `panes-changed` pokes — caps host forks under
/// pathological manifest churn. A change that lands inside the floor is
/// deferred, never dropped.
pub const POKE_FLOOR_MS: u64 = 500;

/// Keepalive cadence. One host fork per minute per session keeps an
/// idle-but-healthy channel distinguishable from a dead one; the host's
/// `PRESENCE_STAMP_FRESH` (150s) allows two missed keepalives of slack.
pub const KEEPALIVE_MS: u64 = 60_000;

/// The pane fields the hash folds — the stable subset whose change means the
/// sidebar should refetch panes. `title` is deliberately absent: agents
/// mutate titles per output line, and hashing them would re-poke per line.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PaneFields {
    pub id: u32,
    pub is_plugin: bool,
    pub is_focused: bool,
    pub exited: bool,
    pub terminal_command: Option<String>,
}

/// Fold the projected manifest into one stable hash. The `BTreeMap` keying by
/// tab position makes iteration order deterministic regardless of the host
/// map's order; callers sort each tab's panes by id before inserting. The
/// value only ever compares against the previous hash in this process, so no
/// cross-version stability is needed.
pub fn manifest_hash(tabs: &BTreeMap<usize, Vec<PaneFields>>, active_tab: Option<usize>) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    active_tab.hash(&mut hasher);
    for (tab, panes) in tabs {
        tab.hash(&mut hasher);
        panes.hash(&mut hasher);
    }
    hasher.finish()
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
    /// First change of the current burst, the debounce anchor. Deliberately
    /// not refreshed by later changes in the burst: the poke fires at most
    /// `DEBOUNCE_MS` after the first change even under continuous churn.
    pending_since: Option<u64>,
    last_changed_poke: Option<u64>,
    next_keepalive: u64,
}

impl PokePolicy {
    pub fn new(now_ms: u64) -> Self {
        Self {
            last_hash: None,
            pending_since: None,
            last_changed_poke: None,
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
        if self.pending_since.is_none() {
            self.pending_since = Some(now_ms);
        }
    }

    /// The pokes due at `now_ms`, consuming them. A pending change fires once
    /// its debounce has elapsed and the rate floor allows; the keepalive fires
    /// on its own cadence regardless of change traffic.
    pub fn due(&mut self, now_ms: u64) -> Vec<Poke> {
        let mut pokes = Vec::new();
        if let Some(since) = self.pending_since {
            let debounced = now_ms >= since.saturating_add(DEBOUNCE_MS);
            let floored = self
                .last_changed_poke
                .is_none_or(|at| now_ms >= at.saturating_add(POKE_FLOOR_MS));
            if debounced && floored {
                self.pending_since = None;
                self.last_changed_poke = Some(now_ms);
                pokes.push(Poke::Changed);
            }
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
        let change_at = self.pending_since.map(|since| {
            let debounce_at = since.saturating_add(DEBOUNCE_MS);
            match self.last_changed_poke {
                Some(at) => debounce_at.max(at.saturating_add(POKE_FLOOR_MS)),
                None => debounce_at,
            }
        });
        match change_at {
            Some(at) => at.min(self.next_keepalive),
            None => self.next_keepalive,
        }
    }
}

#[cfg(test)]
mod tests;
