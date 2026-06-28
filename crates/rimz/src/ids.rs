//! Strongly-typed identifiers.
//!
//! Every ID that travels through the ledger, the wakeup socket, or the agent
//! hook protocol is a newtype. Rimz-minted long IDs (`RequestId`, `EventId`,
//! `SidebarInstanceId`) use UUIDv7, while message IDs use a shorter
//! time-sortable token. IDs derived from external truth (`WorkspaceId`,
//! `PaneId`) keep their natural shape.

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Multiplexer backend selector.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MuxName {
    Zellij,
    Tmux,
}

impl MuxName {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Zellij => "zellij",
            Self::Tmux => "tmux",
        }
    }
}

impl fmt::Display for MuxName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown multiplexer `{0}`; expected `zellij` or `tmux`")]
pub struct UnknownMux(pub String);

impl FromStr for MuxName {
    type Err = UnknownMux;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "zellij" => Ok(Self::Zellij),
            "tmux" => Ok(Self::Tmux),
            other => Err(UnknownMux(other.to_owned())),
        }
    }
}

/// Whether a view is a Zellij tab or a tmux window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewKind {
    Tab,
    Window,
}

/// Multiplexer view identifier: Zellij tab id or tmux window id.
///
/// View ids are backend-owned opaque grouping keys. They are distinct from
/// display names: `tab_15` and a tab named "Tab #15" are unrelated values.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ViewId(String);

impl ViewId {
    pub fn new_unchecked(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ViewId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// `ws_<24 hex chars>` — SHA-256-of-canonical-project-root.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkspaceId(String);

#[derive(Debug, thiserror::Error)]
#[error("invalid workspace id `{0}`; expected `ws_` followed by 24 hex characters")]
pub struct InvalidWorkspaceId(pub String);

impl WorkspaceId {
    pub fn from_project_root(project_root: &Path) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(project_root.to_string_lossy().as_bytes());
        let hash = hex::encode(hasher.finalize());
        Self(format!("ws_{}", &hash[..24]))
    }

    /// Parse a canonical workspace identifier.
    ///
    /// ```
    /// use rimz::ids::WorkspaceId;
    /// let id = WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap();
    /// assert_eq!(id.as_str(), "ws_0123456789abcdef01234567");
    /// assert!(WorkspaceId::parse("not-a-workspace-id").is_err());
    /// ```
    pub fn parse(value: &str) -> Result<Self, InvalidWorkspaceId> {
        let Some(hex) = value.strip_prefix("ws_") else {
            return Err(InvalidWorkspaceId(value.to_owned()));
        };
        if hex.len() != 24 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(InvalidWorkspaceId(value.to_owned()));
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorkspaceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for WorkspaceId {
    type Err = InvalidWorkspaceId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// Macro: define a UUIDv7-backed newtype with a fixed prefix.
macro_rules! uuid_v7_id {
    ($name:ident, $prefix:literal, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Debug, PartialEq, Eq, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn new() -> Self {
                Self(format!("{}_{}", $prefix, Uuid::now_v7().simple()))
            }

            pub fn parse(value: &str) -> Result<Self, InvalidUuidId> {
                validate_uuid_id(value, $prefix, stringify!($name))?;
                Ok(Self(value.to_owned()))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Last 12 hex chars of the UUID portion. Used to name AF_UNIX
            /// sockets, where the platform path budget makes the full 32-char UUID
            /// wasteful. The tail is the v7 UUID's random field, so two ids minted
            /// in the same millisecond still differ — unlike the leading 48 bits,
            /// which are the shared `now_v7` timestamp and would collide for
            /// sidebars launched together, letting one `bind` steal the other's
            /// path and strand a renderer with no wakeup socket.
            pub fn short(&self) -> &str {
                // `new`/`parse` guarantee `<prefix>_<32 hex>`, so the last 12
                // chars are always hex and this slice is always in bounds.
                &self.0[self.0.len() - 12..]
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = InvalidUuidId;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::parse(s)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let raw = String::deserialize(deserializer)?;
                Self::parse(&raw).map_err(serde::de::Error::custom)
            }
        }
    };
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid {kind} `{value}`; expected `{prefix}_` followed by 32 hex characters")]
pub struct InvalidUuidId {
    kind: &'static str,
    prefix: &'static str,
    value: String,
}

fn validate_uuid_id(
    value: &str,
    prefix: &'static str,
    kind: &'static str,
) -> Result<(), InvalidUuidId> {
    let Some(rest) = value
        .strip_prefix(prefix)
        .and_then(|value| value.strip_prefix('_'))
    else {
        return Err(InvalidUuidId {
            kind,
            prefix,
            value: value.to_owned(),
        });
    };
    if rest.len() != 32
        || !rest
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, 'a'..='f'))
    {
        return Err(InvalidUuidId {
            kind,
            prefix,
            value: value.to_owned(),
        });
    }
    Ok(())
}

uuid_v7_id!(RequestId, "req", "Per-feed-item request identifier.");
uuid_v7_id!(RunId, "run", "Per-supervised-run identifier.");
uuid_v7_id!(EventId, "evt", "Per-event identifier in the event log.");
uuid_v7_id!(
    SidebarInstanceId,
    "sb",
    "Per-instance sidebar identifier; one per live sidebar process."
);

/// Per-agent queued message identifier.
///
/// `msg_<16 base32hex chars>` encodes a 48-bit millisecond timestamp plus a
/// 32-bit suffix seeded from UUIDv7 entropy and made process-monotonic. The
/// fixed big-endian base32hex form preserves enqueue order in filenames while
/// keeping command output compact.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MessageId(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid MessageId `{0}`; expected `msg_` followed by 16 lowercase base32hex characters")]
pub struct InvalidMessageId(String);

const MESSAGE_ID_PREFIX: &str = "msg_";
const MESSAGE_ID_LEN: usize = 16;
const BASE32HEX: &[u8; 32] = b"0123456789abcdefghijklmnopqrstuv";
static LAST_MESSAGE_ID: std::sync::Mutex<(u64, u32)> = std::sync::Mutex::new((0, 0));

impl MessageId {
    pub fn new() -> Self {
        let uuid = Uuid::now_v7().as_u128();
        let timestamp_ms = (uuid >> 80) as u64;
        let random_suffix = uuid as u32;
        let (timestamp_ms, suffix) = next_message_id_parts(timestamp_ms, random_suffix);
        let sortable = ((timestamp_ms as u128) << 32) | u128::from(suffix);
        let mut token = String::with_capacity(MESSAGE_ID_PREFIX.len() + MESSAGE_ID_LEN);
        token.push_str(MESSAGE_ID_PREFIX);
        for shift in (0..80).step_by(5).rev() {
            let index = ((sortable >> shift) & 0x1f) as usize;
            token.push(BASE32HEX[index] as char);
        }
        Self(token)
    }

    pub fn parse(value: &str) -> Result<Self, InvalidMessageId> {
        let Some(token) = value.strip_prefix(MESSAGE_ID_PREFIX) else {
            return Err(InvalidMessageId(value.to_owned()));
        };
        if token.len() != MESSAGE_ID_LEN
            || !token
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'v'))
        {
            return Err(InvalidMessageId(value.to_owned()));
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn next_message_id_parts(timestamp_ms: u64, random_suffix: u32) -> (u64, u32) {
    let mut last = LAST_MESSAGE_ID
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (mut next_ms, mut next_suffix) = (timestamp_ms, random_suffix);
    if next_ms < last.0 || (next_ms == last.0 && next_suffix <= last.1) {
        next_ms = last.0;
        next_suffix = last.1.wrapping_add(1);
        if next_suffix == 0 {
            next_ms = next_ms.saturating_add(1);
        }
    }
    *last = (next_ms, next_suffix);
    (next_ms, next_suffix)
}

impl Default for MessageId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for MessageId {
    type Err = InvalidMessageId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl Serialize for MessageId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for MessageId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// Resolver allowlist identifier. Caller-supplied but constrained: the
/// allowlist key, the heartbeat filename, and the audit log all use this
/// verbatim. Validation matches the allowlist schema: ASCII `[A-Za-z0-9_-]`
/// only, length 1..=[`ResolverId::MAX_LEN`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ResolverId(String);

#[derive(Debug, thiserror::Error)]
pub enum InvalidResolverId {
    #[error("resolver id is empty")]
    Empty,
    #[error("resolver id `{value}` exceeds max length {max}")]
    TooLong { value: String, max: usize },
    #[error(
        "resolver id `{value}` contains invalid character `{ch}` (allowed: ASCII alphanumeric, `-`, `_`)"
    )]
    BadChar { value: String, ch: char },
}

impl ResolverId {
    /// Allowlist schema cap (matches the per-machine `resolvers.toml`
    /// key budget; long enough for `org.team.resolver-name`).
    pub const MAX_LEN: usize = 64;

    /// Mint a resolver id from a trusted internal source (placeholder
    /// values in error reporting, fixture IDs in tests). Public callers
    /// take the validated [`FromStr`] path.
    pub fn new_unchecked(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn parse(value: &str) -> Result<Self, InvalidResolverId> {
        if value.is_empty() {
            return Err(InvalidResolverId::Empty);
        }
        if value.len() > Self::MAX_LEN {
            return Err(InvalidResolverId::TooLong {
                value: value.to_owned(),
                max: Self::MAX_LEN,
            });
        }
        if let Some(ch) = value
            .chars()
            .find(|c| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_'))
        {
            return Err(InvalidResolverId::BadChar {
                value: value.to_owned(),
                ch,
            });
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ResolverId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ResolverId {
    type Err = InvalidResolverId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl<'de> Deserialize<'de> for ResolverId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// Agent adapter kind label (`claude`, `codex`, `pi`).
///
/// An open set, deliberately: the registry
/// ([`registry::ADAPTERS`](crate::agents::registry)) is the source of truth
/// for *known* kinds — every dispatch resolves through it and an unknown kind
/// degrades gracefully (skipped probe, title-cased panel) — while ledger
/// replay and snapshot decode stay open so events from a removed adapter
/// still fold and render. CLI boundaries validate by registry lookup
/// (`find_adapter`), which is where a typo dies; internally the kind is a
/// label, so construction is unchecked.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentKind(String);

impl AgentKind {
    pub fn new_unchecked(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl PartialEq<str> for AgentKind {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for AgentKind {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<String> for AgentKind {
    fn eq(&self, other: &String) -> bool {
        self.0 == *other
    }
}

impl PartialEq<AgentKind> for String {
    fn eq(&self, other: &AgentKind) -> bool {
        *self == other.0
    }
}

impl std::ops::Deref for AgentKind {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

// Sound: `Ord`/`Eq`/`Hash` all delegate to the inner string, so a borrowed
// `&str` keys sets and maps consistently.
impl std::borrow::Borrow<str> for AgentKind {
    fn borrow(&self) -> &str {
        &self.0
    }
}

/// Agent-supplied session identifier — the `agent_id` half of the rollup key
/// `(kind, agent_id)`.
///
/// Opaque by contract: each agent mints its own shape (Claude/Pi UUIDs, Codex
/// thread ids), so the only structure Rimz can assume is "non-empty string",
/// and the adapters enforce that at observation time. The newtype exists so a
/// session id can never transpose with an [`AgentKind`] in a key or a
/// signature.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentSessionId(String);

impl AgentSessionId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A launch-time placeholder card, before a lazy agent publishes its real
    /// session id.
    pub fn is_provisional(&self) -> bool {
        self.0.starts_with("launch_")
    }
}

impl From<String> for AgentSessionId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for AgentSessionId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl fmt::Display for AgentSessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl PartialEq<str> for AgentSessionId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for AgentSessionId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<String> for AgentSessionId {
    fn eq(&self, other: &String) -> bool {
        self.0 == *other
    }
}

impl PartialEq<AgentSessionId> for String {
    fn eq(&self, other: &AgentSessionId) -> bool {
        *self == other.0
    }
}

impl std::ops::Deref for AgentSessionId {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

// Sound: `Ord`/`Eq`/`Hash` all delegate to the inner string, so a borrowed
// `&str` keys sets and maps consistently.
impl std::borrow::Borrow<str> for AgentSessionId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

/// Normalized pane identifier: `<mux>:<raw_pane_id>` (e.g. `zellij:terminal_3`).
///
/// Raw pane IDs stay inside backend adapters. This type is what travels in
/// feed items, env vars, and `rimz pane` CLI calls.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct PaneId(String);

#[derive(Debug, thiserror::Error)]
#[error("invalid pane id `{0}`; expected `<mux>:<raw_pane_id>`")]
pub struct InvalidPaneId(pub String);

impl PaneId {
    pub fn from_parts(mux: MuxName, raw: impl AsRef<str>) -> Self {
        Self(format!("{}:{}", mux.as_str(), raw.as_ref()))
    }

    /// Parse a normalized pane identifier of the form `<mux>:<raw_pane_id>`.
    ///
    /// ```
    /// use rimz::ids::{MuxName, PaneId};
    /// let zellij: PaneId = "zellij:terminal_3".parse().unwrap();
    /// assert_eq!(zellij.mux(), MuxName::Zellij);
    /// assert_eq!(zellij.raw(), "terminal_3");
    ///
    /// let tmux: PaneId = "tmux:%3".parse().unwrap();
    /// assert_eq!(tmux.mux(), MuxName::Tmux);
    /// assert_eq!(tmux.raw(), "%3");
    /// ```
    pub fn parse(value: &str) -> Result<Self, InvalidPaneId> {
        let (head, tail) = value
            .split_once(':')
            .ok_or_else(|| InvalidPaneId(value.to_owned()))?;
        if head != MuxName::Zellij.as_str() && head != MuxName::Tmux.as_str() {
            return Err(InvalidPaneId(value.to_owned()));
        }
        if tail.is_empty() {
            return Err(InvalidPaneId(value.to_owned()));
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Multiplexer prefix this pane is scoped to.
    pub fn mux(&self) -> MuxName {
        // Constructed only via `from_parts`/`parse`, both of which guarantee a
        // valid prefix — the unwrap below cannot fire on a well-formed value.
        let (head, _) = self
            .0
            .split_once(':')
            .expect("PaneId invariant: contains ':'");
        head.parse()
            .expect("PaneId invariant: prefix is a valid MuxName")
    }

    /// Mux-native pane id (e.g. `terminal_3` for Zellij, `%3` for tmux).
    pub fn raw(&self) -> &str {
        let (_, tail) = self
            .0
            .split_once(':')
            .expect("PaneId invariant: contains ':'");
        tail
    }
}

impl fmt::Display for PaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for PaneId {
    type Err = InvalidPaneId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl<'de> Deserialize<'de> for PaneId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_id_is_stable_for_same_root() {
        let a = WorkspaceId::from_project_root(Path::new("/tmp/repo"));
        let b = WorkspaceId::from_project_root(Path::new("/tmp/repo"));
        assert_eq!(a, b);
        assert!(a.as_str().starts_with("ws_"));
        assert_eq!(a.as_str().len(), 3 + 24);
    }

    #[test]
    fn workspace_id_parser_accepts_only_canonical_shape() {
        let id = WorkspaceId::parse("ws_0123456789abcdefABCDEF01").expect("valid");
        assert_eq!(id.as_str(), "ws_0123456789abcdefABCDEF01");
        assert!(WorkspaceId::parse("0123456789abcdefABCDEF01").is_err());
        assert!(WorkspaceId::parse("ws_short").is_err());
        assert!(WorkspaceId::parse("ws_0123456789abcdefABCDEFG").is_err());
    }

    #[test]
    fn request_ids_are_unique_and_prefixed() {
        let a = RequestId::new();
        let b = RequestId::new();
        assert_ne!(a, b);
        assert!(a.as_str().starts_with("req_"));
    }

    #[test]
    fn uuid_prefixed_ids_reject_non_canonical_input() {
        assert!(RequestId::parse("req_0123456789abcdef0123456789abcdef").is_ok());
        assert!(RequestId::parse("evt_0123456789abcdef0123456789abcdef").is_err());
        assert!(RequestId::parse("req_short").is_err());
        assert!(RequestId::parse("req_0123456789abcdef0123456789abcdeg").is_err());
        assert!(RequestId::parse("req_0123456789abcdef0123456789ABCDEF").is_err());
        assert!(EventId::parse("evt_0123456789abcdef0123456789abcdef").is_ok());
        assert!(SidebarInstanceId::parse("sb_0123456789abcdef0123456789abcdef").is_ok());
    }

    #[test]
    fn message_ids_are_short_time_sortable_tokens() {
        let first = MessageId::new();
        let second = MessageId::new();

        assert_ne!(first, second);
        assert!(first.as_str().starts_with("msg_"));
        assert_eq!(first.as_str().len(), "msg_".len() + 16);
        assert!(
            first.as_str()["msg_".len()..]
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'v'))
        );
        assert!(second.as_str() >= first.as_str());
        assert!(MessageId::parse(first.as_str()).is_ok());
        assert!(MessageId::parse("msg_0123456789abcdef").is_ok());
        assert!(MessageId::parse("msg_0123456789abcde").is_err());
        assert!(MessageId::parse("msg_0123456789abcdew").is_err());
        assert!(MessageId::parse("msg_0123456789ABCDEF").is_err());
    }

    #[test]
    fn uuid_prefixed_ids_reject_bad_json_values() {
        let parsed: Result<RequestId, _> =
            serde_json::from_str("\"req_0123456789abcdef0123456789abcdef\"");
        assert!(parsed.is_ok());

        let parsed: Result<RequestId, _> = serde_json::from_str("\"not-a-request-id\"");
        assert!(parsed.is_err());
    }

    #[test]
    fn short_returns_the_12_hex_tail() {
        // `short()` slices from the end, so it works across prefixes of different
        // lengths ("req" vs "sb").
        for (short, full) in [
            {
                let id = RequestId::new();
                (id.short().to_owned(), id.as_str().to_owned())
            },
            {
                let id = SidebarInstanceId::new();
                (id.short().to_owned(), id.as_str().to_owned())
            },
        ] {
            assert_eq!(short.len(), 12);
            assert!(short.chars().all(|c| c.is_ascii_hexdigit()));
            // The short id is the hex tail of the UUID portion.
            assert!(full.ends_with(&short));
        }
    }

    #[test]
    fn short_disambiguates_same_millisecond_ids() {
        // The first 12 hex of a v7 UUID are the millisecond timestamp, so two ids
        // minted together share them; `short()` must take the random tail instead
        // or their socket paths collide and one `bind` steals the other's.
        let a = SidebarInstanceId::parse("sb_019e8c565bbd708097fce9514f79da04").unwrap();
        let b = SidebarInstanceId::parse("sb_019e8c565bbd7b22854f93a905e1034c").unwrap();
        assert_eq!(
            &a.as_str()[3..15],
            &b.as_str()[3..15],
            "same-millisecond ids share the leading v7 timestamp",
        );
        assert_ne!(
            a.short(),
            b.short(),
            "the random tail disambiguates same-millisecond ids",
        );
    }

    #[test]
    fn agent_identity_newtypes_serialize_transparently() {
        // The rollup cache and snapshot JSON shapes must stay byte-identical
        // to the plain-string era — the newtypes are compile-time-only.
        let kind = AgentKind::new_unchecked("claude");
        assert_eq!(serde_json::to_string(&kind).unwrap(), r#""claude""#);
        let back: AgentKind = serde_json::from_str(r#""claude""#).unwrap();
        assert_eq!(back, kind);
        assert!(kind == "claude");

        let session = AgentSessionId::from("sess-1");
        assert_eq!(serde_json::to_string(&session).unwrap(), r#""sess-1""#);
        let back: AgentSessionId = serde_json::from_str(r#""sess-1""#).unwrap();
        assert_eq!(back, session);
        assert!(session == "sess-1");

        // Open set: an unknown kind decodes fine — replay of a removed
        // adapter's events must fold, not fail.
        let unknown: AgentKind = serde_json::from_str(r#""opencode""#).unwrap();
        assert_eq!(unknown.as_str(), "opencode");
    }

    #[test]
    fn pane_id_round_trips_parts() {
        let id = PaneId::from_parts(MuxName::Zellij, "terminal_3");
        assert_eq!(id.as_str(), "zellij:terminal_3");
        assert_eq!(id.mux(), MuxName::Zellij);
        assert_eq!(id.raw(), "terminal_3");

        let parsed: PaneId = "tmux:%5".parse().expect("valid pane id");
        assert_eq!(parsed.mux(), MuxName::Tmux);
        assert_eq!(parsed.raw(), "%5");
    }

    #[test]
    fn pane_id_rejects_unknown_mux_prefix() {
        assert!(PaneId::parse("kitty:1").is_err());
        assert!(PaneId::parse("no-colon").is_err());
        assert!(PaneId::parse("tmux:").is_err());
        assert!(PaneId::parse("zellij:").is_err());
    }

    #[test]
    fn pane_id_deserialize_rejects_unknown_mux_prefix() {
        let parsed: PaneId = serde_json::from_str(r#""tmux:%5""#).expect("valid pane id");
        assert_eq!(parsed.raw(), "%5");
        assert!(serde_json::from_str::<PaneId>(r#""not-a-pane""#).is_err());
    }
}
