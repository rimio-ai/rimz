//! Stable human-friendly agent names.
//!
//! Agent cards need a short handle that is not a provider session id and does
//! not depend on the current pane. This module keeps the name generator local:
//! no dependency, deterministic fallback for old logs, and a collision check
//! supplied by the caller's current rollup.

use std::collections::BTreeSet;

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::ids::AgentSessionId;

const ADJECTIVES: &[&str] = &[
    "able", "acute", "amber", "ample", "apt", "bold", "brave", "bright", "candid", "chief",
    "civil", "clever", "cool", "crisp", "direct", "eager", "early", "easy", "exact", "fair",
    "firm", "fit", "glad", "gold", "grand", "green", "handy", "happy", "hardy", "honest", "ideal",
    "jolly", "kind", "level", "light", "lucid", "major", "merry", "mint", "modern", "neat",
    "noble", "novel", "open", "patient", "plain", "prime", "prompt", "proper", "proud", "quick",
    "real", "right", "robust", "round", "simple", "smart", "solid", "sound", "spry", "stable",
    "still", "stout", "tidy", "true", "trusty", "useful", "valid", "vast", "warm", "whole", "wise",
    "witty", "young", "zesty", "active", "choice", "dapper", "deft", "even", "expert", "polite",
    "sure", "upbeat",
];

const NOUNS: &[&str] = &[
    "arc", "atlas", "badge", "beam", "beacon", "binder", "block", "brook", "cable", "camp",
    "canyon", "cargo", "cipher", "cliff", "cloud", "cobalt", "comet", "compass", "copper", "coral",
    "dock", "drift", "echo", "fig", "flare", "forge", "frame", "frost", "garden", "gate", "glyph",
    "grain", "grove", "haven", "hazel", "hinge", "index", "iris", "isle", "jolt", "kernel",
    "keystone", "lagoon", "lane", "store", "lens", "linen", "lumen", "maple", "marker", "mesa",
    "meter", "mint", "mirror", "module", "needle", "notch", "nova", "oak", "parcel", "path",
    "pillar", "pixel", "plaza", "portal", "ridge", "river", "rivet", "route", "saddle", "signal",
    "silver", "slate", "spark", "spire", "spring", "square", "stone", "strand", "summit",
    "thicket", "thread", "tower", "trace", "valley", "vector", "vista", "wicket", "yard",
];

/// Words `rimz agents` owns as verbs or cells; no profile, command, team, or agent name may claim them.
/// TODO(reserved-words): decide whether to reserve the newer restart, resume, budget, logs, history, top, check, register, and refresh verbs.
pub const RESERVED_AGENT_WORDS: &[&str] = &[
    "exec", "focus", "fork", "list", "ls", "profiles", "show", "stop", "term", "wait",
];

pub fn mint(taken: impl IntoIterator<Item = impl AsRef<str>>) -> String {
    let seed = Uuid::now_v7().simple().to_string();
    mint_from_seed(&seed, taken)
}

pub fn mint_for_session(
    agent_id: &AgentSessionId,
    taken: impl IntoIterator<Item = impl AsRef<str>>,
) -> String {
    mint_from_seed(agent_id.as_str(), taken)
}

/// Accept one durable agent handle at every allocation, fold, and hook boundary.
pub fn valid_agent_name(name: &str) -> bool {
    basic_valid_name(name)
        && !crate::agents::known_kinds().any(|kind| {
            name == kind
                || name
                    .strip_prefix(kind)
                    .is_some_and(|tail| tail.starts_with('-'))
        })
}

fn basic_valid_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name == name.trim()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
        && name != "all"
        && !RESERVED_AGENT_WORDS.contains(&name)
}

fn mint_from_seed(seed: &str, taken: impl IntoIterator<Item = impl AsRef<str>>) -> String {
    let taken: BTreeSet<String> = taken
        .into_iter()
        .map(|value| value.as_ref().to_owned())
        .collect();
    for attempt in 0u32.. {
        let value = hash_u64(seed, attempt);
        let adjective = ADJECTIVES[(value as usize) % ADJECTIVES.len()];
        let noun = NOUNS[((value >> 16) as usize) % NOUNS.len()];
        let candidate = if attempt == 0 {
            format!("{adjective}-{noun}")
        } else {
            format!("{adjective}-{noun}-{attempt}")
        };
        if valid_agent_name(&candidate) && !taken.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!("unbounded attempts eventually append a unique suffix")
}

fn hash_u64(seed: &str, attempt: u32) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(seed.as_bytes());
    hasher.update(attempt.to_le_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_le_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_fallback_avoids_taken_names() {
        let agent_id = AgentSessionId::from("session-a");
        let first = mint_for_session(&agent_id, std::iter::empty::<&str>());
        assert_eq!(
            first,
            mint_for_session(&agent_id, std::iter::empty::<&str>())
        );
        let second = mint_for_session(&agent_id, [first.as_str()]);
        assert_ne!(first, second);
    }

    #[test]
    fn validates_cli_safe_names() {
        for (name, valid) in [
            ("amber-atlas", true),
            ("show", false),
            ("fork", false),
            // `all` is the @all fan-out keyword; the generator must never mint it.
            ("all", false),
            ("two words", false),
            ("claude", false),
            ("claude-1", false),
            ("codex-12", false),
            ("claudette-1", true),
        ] {
            assert_eq!(valid_agent_name(name), valid, "{name}");
        }
        assert!(valid_agent_name(&mint_for_session(
            &AgentSessionId::from("session-matrix"),
            std::iter::empty::<&str>(),
        )));
    }
}
