//! Strongly-typed identifiers.
//!
//! Every ID that travels through the store, the host's room socket, or the agent
//! hook protocol is a newtype. RimZ-minted long IDs (`RunId`, `EventId`,
//! `SidebarInstanceId`) use UUIDv7, while message IDs use a shorter
//! time-sortable token. IDs derived from external truth (`WorkspaceId`,
//! `PaneId`) keep their natural shape. Shared classification enums (`MuxName`,
//! `ViewKind`, `LinkTier`, `HoldKind`) travel beside them.

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Multiplexer backend selector.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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

    /// The other backend. A path's room is single-backend; this names the
    /// rival to probe for a live session before a new-room birth.
    pub const fn other(self) -> Self {
        match self {
            Self::Zellij => Self::Tmux,
            Self::Tmux => Self::Zellij,
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

/// Backend-native identity for one attached multiplexer client.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "mux", content = "id", rename_all = "snake_case")]
pub enum MuxClientId {
    Tmux(String),
    Zellij(u32),
}

/// Whether a view is a Zellij tab or a tmux window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewKind {
    Tab,
    Window,
}

/// Link-health tier for notifications, diagnostics, and CLI health output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkTier {
    Good,
    Degraded,
    Bad,
}

/// Why an automated start is held by the throttle.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HoldKind {
    Capacity,
    CheckFailed,
    Queue,
    MachineCap,
    AutomationCap,
    Stagger,
    WorkflowLimit,
    StepLimit,
    Pressure,
    Memory,
    Disk,
    Store,
}

impl HoldKind {
    pub(crate) fn deserialize_optional<'de, D>(deserializer: D) -> Result<Option<Self>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Option::<String>::deserialize(deserializer)?;
        Ok(value.and_then(|value| {
            Self::deserialize(serde::de::value::StrDeserializer::<D::Error>::new(&value)).ok()
        }))
    }

    pub const fn needs_fix(self) -> bool {
        matches!(self, Self::CheckFailed | Self::Disk | Self::Store)
    }
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
    /// Hash an absolute, canonical project root into its workspace identity.
    ///
    /// The caller resolves the root first — [`crate::workspace`] is the one
    /// path that does so. An empty or relative root would hash to a single
    /// shared identity and collapse unrelated projects into one store, so
    /// debug builds assert the precondition rather than mint that collision.
    pub fn from_project_root(project_root: &Path) -> Self {
        debug_assert!(
            project_root.is_absolute(),
            "workspace identity needs an absolute project root, got `{}`",
            project_root.display()
        );
        let mut hasher = Sha256::new();
        hasher.update(project_root.to_string_lossy().as_bytes());
        let hash = hex::encode(hasher.finalize());
        Self(format!("ws_{}", &hash[..24]))
    }

    /// Parse a canonical workspace identifier.
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

    /// The 24 hex digits after `ws_`.
    pub(crate) fn hex(&self) -> &str {
        &self.0[3..]
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

/// Minimum hex digits a workspace directory name carries.
pub(crate) const WORKSPACE_DIR_HEX_MIN: usize = 4;
const WORKSPACE_ID_HEX_LEN: usize = 24;

/// A workspace directory name, `<slug>-<hex>`: a basename slug plus an
/// even-length prefix (at least four digits) of its [`WorkspaceId`] hex.
///
/// The name is a location, never an identity: two workspaces may share a
/// prefix, and `workspace.json` inside the directory names the full id.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct WorkspaceDirName {
    name: String,
    hex_start: usize,
}

impl WorkspaceDirName {
    /// Parse `<slug>-<hex>`, split at the last `-`.
    pub fn parse(value: &str) -> Option<Self> {
        let (slug, hex) = value.rsplit_once('-')?;
        let hex_ok = hex.len() >= WORKSPACE_DIR_HEX_MIN
            && hex.len() <= WORKSPACE_ID_HEX_LEN
            && hex.len() % 2 == 0
            && hex.chars().all(|c| c.is_ascii_hexdigit());
        let slug_ok = !slug.is_empty()
            && !slug.starts_with('-')
            && slug
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        (hex_ok && slug_ok).then(|| Self {
            name: value.to_owned(),
            hex_start: slug.len() + 1,
        })
    }

    /// Name `workspace_id` with `slug` and the first `hex_len` id hex digits,
    /// clamped to an even length between the minimum and the full id.
    pub(crate) fn mint(slug: &str, workspace_id: &WorkspaceId, hex_len: usize) -> Self {
        let hex_len = hex_len.clamp(WORKSPACE_DIR_HEX_MIN, WORKSPACE_ID_HEX_LEN) & !1;
        let slug = if slug.is_empty() { "root" } else { slug };
        Self {
            name: format!("{slug}-{}", &workspace_id.hex()[..hex_len]),
            hex_start: slug.len() + 1,
        }
    }

    /// The name for a workspace located without its project root: the full id
    /// hex under the `ws` slug, so it can never shadow a minted neighbour.
    pub(crate) fn fallback(workspace_id: &WorkspaceId) -> Self {
        Self::mint("ws", workspace_id, WORKSPACE_ID_HEX_LEN)
    }

    /// The slug for a project root's basename, at most `max` characters:
    /// ASCII alphanumerics and `_` kept, every other run collapsed to one `-`.
    pub(crate) fn basename_slug(project_root: &Path, max: usize) -> String {
        let basename = project_root
            .file_name()
            .map(|name| name.to_string_lossy())
            .unwrap_or_default();
        let mut slug = String::new();
        for c in basename.chars() {
            let c = if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '-'
            };
            if !(c == '-' && slug.ends_with('-')) {
                slug.push(c);
            }
        }
        let slug: String = slug.trim_matches('-').chars().take(max).collect();
        let slug = slug.trim_matches('-');
        if slug.is_empty() {
            "root".to_owned()
        } else {
            slug.to_owned()
        }
    }

    /// Whether this name's hex is a prefix of `workspace_id`, making the
    /// directory a candidate location for it.
    pub(crate) fn may_name(&self, workspace_id: &WorkspaceId) -> bool {
        workspace_id.hex().starts_with(self.hex())
    }

    pub fn as_str(&self) -> &str {
        &self.name
    }

    pub(crate) fn hex(&self) -> &str {
        &self.name[self.hex_start..]
    }
}

impl fmt::Display for WorkspaceDirName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

/// A configured team in one room lane, rendered as `team#lane`.
///
/// A seat with no lane name at all is the `external` instance, which the
/// string form spells `team#external`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TeamInstanceId {
    team: String,
    lane: Option<LaneName>,
}

#[derive(Debug, thiserror::Error)]
#[error("invalid team instance `{0}`; expected team#channel")]
pub struct InvalidTeamInstanceId(String);

impl TeamInstanceId {
    /// How the string form spells the instance of a seat without a lane.
    const EXTERNAL: &'static str = "external";

    /// `team`'s instance in `lane`, or its `external` instance without one.
    /// A lane named `external` shares the string form of the lane-less
    /// instance, so it is that instance.
    pub fn new(team: &str, lane: Option<LaneName>) -> Result<Self, InvalidTeamInstanceId> {
        if team.is_empty() || team.contains('#') || team.chars().any(char::is_whitespace) {
            let lane = lane.as_ref().map_or(Self::EXTERNAL, LaneName::as_str);
            return Err(InvalidTeamInstanceId(format!("{team}#{lane}")));
        }
        Ok(Self {
            team: team.to_owned(),
            lane: lane.filter(|lane| lane.as_str() != Self::EXTERNAL),
        })
    }

    pub fn team(&self) -> &str {
        &self.team
    }

    pub fn lane(&self) -> Option<&LaneName> {
        self.lane.as_ref()
    }

    /// The part after `#`: the lane name, or `external`.
    pub fn lane_text(&self) -> &str {
        self.lane.as_ref().map_or(Self::EXTERNAL, LaneName::as_str)
    }

    /// Decode a stored instance; a lane part that is no lane name is read
    /// through [`LaneName::dashed`], as an older binary may have written one.
    fn decode(value: &str, lenient: bool) -> Result<Self, InvalidTeamInstanceId> {
        let invalid = || InvalidTeamInstanceId(value.to_owned());
        let (team, lane) = value.split_once('#').ok_or_else(invalid)?;
        if lane.is_empty() {
            return Err(invalid());
        }
        let lane = match lane {
            Self::EXTERNAL => None,
            lane if lenient => Some(LaneName::dashed(lane)),
            lane => Some(lane.parse().map_err(|_| invalid())?),
        };
        Self::new(team, lane).map_err(|_| invalid())
    }

    /// Serialize a cohort's lane stored apart from its team, spelled as the
    /// part after `#`. Pair it with [`TeamInstanceId::deserialize_lane`].
    pub(crate) fn serialize_lane<S: Serializer>(
        lane: &Option<LaneName>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(lane.as_ref().map_or(Self::EXTERNAL, LaneName::as_str))
    }

    /// Decode a cohort lane stored apart from its team without failing the
    /// record: `external` and an empty string are no lane, and any other text
    /// is read through [`LaneName::dashed`].
    pub(crate) fn deserialize_lane<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<LaneName>, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Ok((!raw.is_empty() && raw != Self::EXTERNAL).then(|| LaneName::dashed(&raw)))
    }
}

impl FromStr for TeamInstanceId {
    type Err = InvalidTeamInstanceId;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::decode(value, false)
    }
}

impl TryFrom<String> for TeamInstanceId {
    type Error = InvalidTeamInstanceId;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::decode(&value, true)
    }
}

impl From<TeamInstanceId> for String {
    fn from(value: TeamInstanceId) -> Self {
        value.to_string()
    }
}

impl fmt::Display for TeamInstanceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.team, self.lane_text())
    }
}

/// A lane's name: any non-empty text without `#` or whitespace.
///
/// Every lane a launch names is flat ([`LaneName::is_flat`]), but the grammar
/// stays wide: a record an older binary wrote may carry a name outside the flat
/// set (it decodes through [`LaneName::dashed`]), and a checkout's own lane
/// takes its directory's name, which may hold `.`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct LaneName(String);

/// A lane or lane name that does not decode.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct InvalidLane(String);

impl LaneName {
    /// Trunk's own lane's name.
    pub const TRUNK: &'static str = "main";

    /// Trunk's own lane's name, as a lane name.
    pub fn trunk() -> Self {
        Self(Self::TRUNK.to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `raw` made a lane name: each whitespace character and `#` becomes `-`,
    /// and empty text becomes `-`.
    pub fn dashed(raw: &str) -> Self {
        let dashed = raw.replace(|ch: char| ch.is_whitespace() || ch == '#', "-");
        Self(if dashed.is_empty() {
            "-".to_owned()
        } else {
            dashed
        })
    }

    /// Whether `raw` is a flat name, as a typed `--channel` must be: non-empty
    /// ASCII letters, digits, `_`, or `-`.
    pub fn is_flat(raw: &str) -> bool {
        !raw.is_empty()
            && raw
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    }
}

impl FromStr for LaneName {
    type Err = InvalidLane;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() || value.contains('#') || value.chars().any(char::is_whitespace) {
            return Err(InvalidLane(format!(
                "invalid lane name `{value}`; expected non-empty text without `#` or whitespace"
            )));
        }
        Ok(Self(value.to_owned()))
    }
}

impl TryFrom<String> for LaneName {
    type Error = InvalidLane;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<LaneName> for String {
    fn from(value: LaneName) -> Self {
        value.0
    }
}

impl fmt::Display for LaneName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a lane's home checkout is to the room.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneHome {
    /// The project checkout itself.
    Trunk,
    /// A RimZ-managed worktree of the project.
    Worktree,
    /// Any other directory.
    Foreign,
}

/// An agent's lane: a home checkout and an optional name.
///
/// Without a name it is the checkout's own lane; with one it is a named lane
/// homed there. A fresh launch decides it once, the launch record stores it
/// whole, and every relaunch copies it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "LaneWire", into = "LaneWire")]
pub struct Lane {
    home: LaneHome,
    path: String,
    name: Option<LaneName>,
}

#[derive(Serialize, Deserialize)]
struct LaneWire {
    home: LaneHome,
    path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<LaneName>,
}

impl TryFrom<LaneWire> for Lane {
    type Error = InvalidLane;

    fn try_from(wire: LaneWire) -> Result<Self, Self::Error> {
        if !Path::new(&wire.path).is_absolute() {
            return Err(InvalidLane(format!(
                "lane path `{}` is not absolute",
                wire.path
            )));
        }
        Ok(Self {
            home: wire.home,
            path: wire.path,
            name: wire.name,
        })
    }
}

impl From<Lane> for LaneWire {
    fn from(lane: Lane) -> Self {
        Self {
            home: lane.home,
            path: lane.path,
            name: lane.name,
        }
    }
}

impl Lane {
    /// The own lane of the checkout at `path`.
    pub(crate) fn own(home: LaneHome, path: impl Into<String>) -> Self {
        Self {
            home,
            path: path.into(),
            name: None,
        }
    }

    /// The named lane `name` homed at this lane's checkout.
    pub(crate) fn named(self, name: LaneName) -> Self {
        Self {
            name: Some(name),
            ..self
        }
    }

    pub fn home(&self) -> LaneHome {
        self.home
    }

    /// The home checkout's absolute path.
    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn name(&self) -> Option<&LaneName> {
        self.name.as_ref()
    }

    /// The home checkout's own lane.
    pub fn own_lane(&self) -> Self {
        Self::own(self.home, self.path.clone())
    }

    /// Trunk's own lane.
    pub fn is_trunk(&self) -> bool {
        self.home == LaneHome::Trunk && self.name.is_none()
    }

    /// The name a launch pins and titles the lane with: `RIMZ_CHANNEL`, a tab
    /// and a zone title, the row's `channel` projection, and a resume identity.
    /// A named lane and a worktree's own lane give their name; trunk's and a
    /// foreign checkout's own lanes give none. [`Lane::routing_name`] stays the
    /// lane's name everywhere a lane is printed or matched.
    pub fn pin_name(&self) -> Option<LaneName> {
        match (&self.name, self.home) {
            (Some(name), _) => Some(name.clone()),
            (None, LaneHome::Worktree) => Some(self.routing_name()),
            (None, LaneHome::Trunk | LaneHome::Foreign) => None,
        }
    }

    /// The lane's one name, which every surface, comparison, key, and record
    /// reads: the name, else `main` for trunk's own lane, else the home
    /// directory's basename made a lane name.
    pub fn routing_name(&self) -> LaneName {
        match (&self.name, self.home) {
            (Some(name), _) => name.clone(),
            (None, LaneHome::Trunk) => LaneName::trunk(),
            (None, LaneHome::Worktree | LaneHome::Foreign) => {
                LaneName::dashed(self.basename().unwrap_or_default())
            }
        }
    }

    /// The lane a lane name read back at this lane's checkout means.
    ///
    /// A named lane stays as it is. No stamp, an empty one, or the checkout's
    /// own lane's name is the own lane; any other stamp is a named lane of that
    /// name homed here.
    pub fn adopt(self, stamp: Option<&str>) -> Self {
        if self.name.is_some() {
            return self;
        }
        let Some(stamp) = stamp.filter(|stamp| !stamp.is_empty()) else {
            return self;
        };
        let owned = stamp == self.routing_name().as_str();
        match stamp.parse::<LaneName>() {
            Ok(name) if !owned => self.named(name),
            _ => self,
        }
    }

    fn basename(&self) -> Option<&str> {
        Path::new(&self.path)
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
    }
}

/// Decode an optional lane, reading one that does not decode as absent so the
/// record around it still decodes.
pub(crate) fn deserialize_lane_lenient<'de, D>(deserializer: D) -> Result<Option<Lane>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| serde_json::from_value(value).ok()))
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
            /// concurrent requests binding their own sockets.
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

uuid_v7_id!(RunId, "run", "Per-supervised-run identifier.");
uuid_v7_id!(WorkflowRunId, "wf", "Per-workflow-run identifier.");

impl PartialOrd for WorkflowRunId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for WorkflowRunId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

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
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MessageId(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid MessageId `{0}`; expected `msg_` followed by 16 lowercase base32hex characters")]
pub struct InvalidMessageId(String);

const MESSAGE_ID_PREFIX: &str = "msg_";
const MESSAGE_ID_LEN: usize = 16;
const BASE32HEX: &[u8; 32] = b"0123456789abcdefghijklmnopqrstuv";
static LAST_SHORT_ID: std::sync::Mutex<(u64, u32)> = std::sync::Mutex::new((0, 0));

impl MessageId {
    pub fn new() -> Self {
        Self(new_short_id(MESSAGE_ID_PREFIX))
    }

    pub(crate) fn successor(&self) -> Self {
        // Constructors validate the 80-bit token; 48-bit milliseconds cannot wrap in the supported wall-clock range.
        let value = u128::from_str_radix(&self.0[MESSAGE_ID_PREFIX.len()..], 32)
            .expect("MessageId contains 16 base32hex digits");
        Self(encode_short_id(MESSAGE_ID_PREFIX, value + 1))
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

/// Per-blocking-prompt identifier.
///
/// `ask_<16 base32hex chars>` has the same compact, time-sortable shape as a
/// [`MessageId`]. It is minted when hook ingestion observes a blocking prompt
/// and remains attached to that prompt until the lifecycle reducer clears it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AskId(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid AskId `{0}`; expected `ask_` followed by 16 lowercase base32hex characters")]
pub struct InvalidAskId(String);

const ASK_ID_PREFIX: &str = "ask_";

impl AskId {
    pub fn new() -> Self {
        Self(new_short_id(ASK_ID_PREFIX))
    }

    pub fn parse(value: &str) -> Result<Self, InvalidAskId> {
        let Some(token) = value.strip_prefix(ASK_ID_PREFIX) else {
            return Err(InvalidAskId(value.to_owned()));
        };
        if token.len() != MESSAGE_ID_LEN
            || !token
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'v'))
        {
            return Err(InvalidAskId(value.to_owned()));
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for AskId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for AskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for AskId {
    type Err = InvalidAskId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl Serialize for AskId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for AskId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

fn new_short_id(prefix: &str) -> String {
    let uuid = Uuid::now_v7().as_u128();
    let timestamp_ms = (uuid >> 80) as u64;
    let random_suffix = uuid as u32;
    let (timestamp_ms, suffix) = next_short_id_parts(timestamp_ms, random_suffix);
    let sortable = ((timestamp_ms as u128) << 32) | u128::from(suffix);
    encode_short_id(prefix, sortable)
}

fn encode_short_id(prefix: &str, sortable: u128) -> String {
    let mut token = String::with_capacity(prefix.len() + MESSAGE_ID_LEN);
    token.push_str(prefix);
    for shift in (0..80).step_by(5).rev() {
        let index = ((sortable >> shift) & 0x1f) as usize;
        token.push(BASE32HEX[index] as char);
    }
    token
}

fn next_short_id_parts(timestamp_ms: u64, random_suffix: u32) -> (u64, u32) {
    let mut last = LAST_SHORT_ID
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

/// Agent adapter kind label (`claude`, `codex`, `pi`).
///
/// An open set, deliberately: the registry
/// ([`registry::all_definitions`](crate::agents::registry::all_definitions)) is the source of truth
/// for *known* kinds — every dispatch resolves through it and an unknown kind
/// degrades gracefully (skipped probe, title-cased panel) — while store
/// replay and snapshot decode stay open so events from a removed adapter
/// still fold and render. CLI boundaries validate by registry lookup
/// (`find_definition`), which is where a typo dies; internally the kind is a
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

/// Name of a provider login — the user-facing "account" of a provider kind.
///
/// A login is a standalone provider home: `default` is the provider's own
/// native resolution and stores no path, every other name is a directory the
/// user declared under `[accounts.<kind>.<name>]`. The grammar is deliberately
/// narrow because the name appears in file names (`budget.account.<key>.json`)
/// and in cache keys.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct LoginName(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "invalid account name `{0}`; expected 1-32 characters of `a-z`, `0-9`, `_` or `-`, starting with a letter or digit"
)]
pub struct InvalidLoginName(pub String);

impl LoginName {
    /// The provider's native home: selected when nothing else is, never declared.
    pub const DEFAULT: &'static str = "default";

    pub fn default_login() -> Self {
        Self(Self::DEFAULT.to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_default(&self) -> bool {
        self.0 == Self::DEFAULT
    }
}

impl Default for LoginName {
    fn default() -> Self {
        Self::default_login()
    }
}

impl FromStr for LoginName {
    type Err = InvalidLoginName;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let valid = (1..=32).contains(&s.len())
            && s.bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
            && s.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
            });
        if !valid {
            return Err(InvalidLoginName(s.to_owned()));
        }
        Ok(Self(s.to_owned()))
    }
}

impl TryFrom<String> for LoginName {
    type Error = InvalidLoginName;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<LoginName> for String {
    fn from(value: LoginName) -> Self {
        value.0
    }
}

impl fmt::Display for LoginName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// Sound: `Ord`/`Eq`/`Hash` all delegate to the inner string, so a borrowed
// `&str` keys sets and maps consistently.
impl std::borrow::Borrow<str> for LoginName {
    fn borrow(&self) -> &str {
        &self.0
    }
}

/// A provider login across every kind: `<kind>@<name>`, e.g. `claude@work`.
///
/// This is the key user-scoped caches, the budget ledger and published spending
/// windows are partitioned by, so the encoding is stable and round-trips.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct LoginKey {
    pub kind: AgentKind,
    pub name: LoginName,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvalidLoginKey {
    #[error("invalid account key `{0}`; expected `<kind>@<name>`")]
    Shape(String),
    #[error(transparent)]
    Name(#[from] InvalidLoginName),
}

impl LoginKey {
    pub(crate) fn new(kind: AgentKind, name: LoginName) -> Self {
        Self { kind, name }
    }

    pub fn default_for(kind: AgentKind) -> Self {
        Self::new(kind, LoginName::default_login())
    }
}

impl FromStr for LoginKey {
    type Err = InvalidLoginKey;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (kind, name) = s
            .split_once('@')
            .ok_or_else(|| InvalidLoginKey::Shape(s.to_owned()))?;
        if kind.is_empty() {
            return Err(InvalidLoginKey::Shape(s.to_owned()));
        }
        Ok(Self {
            kind: AgentKind::new_unchecked(kind),
            name: name.parse()?,
        })
    }
}

impl TryFrom<String> for LoginKey {
    type Error = InvalidLoginKey;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<LoginKey> for String {
    fn from(value: LoginKey) -> Self {
        value.to_string()
    }
}

impl fmt::Display for LoginKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.kind, self.name)
    }
}

/// A room's defaults for future launches. A kind absent from the map defaults to `default`; existing sessions keep their own stamps.
pub type RoomLogins = std::collections::BTreeMap<AgentKind, LoginName>;

/// Agent-supplied session identifier — the `agent_id` half of the rollup key
/// `(kind, agent_id)`.
///
/// Opaque by contract: each agent mints its own shape (Claude/Pi UUIDs, Codex
/// thread ids), so the only structure RimZ can assume is "non-empty string",
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
/// env vars and `rimz pane` CLI calls.
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

    /// The pane's creation ordinal: the monotonic integer the mux assigns each pane
    /// (`zellij:terminal_176` -> 176, `tmux:%3` -> 3), ascending in birth order. It is
    /// the calm tiebreak — one signal both agents in a tab share, and the order the
    /// mux itself lays panes out, so the sidebar tracks the pane order until the
    /// panes are reordered. It replaces the former `pane_process_start`/`registered_at`
    /// spawn key, which read a different clock for each agent (a derived process
    /// start for one, hook registration for the other) and inverted co-launched
    /// panes whenever the two sources disagreed.
    pub fn creation_ordinal(&self) -> Option<u64> {
        let raw = self.raw();
        let digits_start = raw
            .as_bytes()
            .iter()
            .rposition(|byte| !byte.is_ascii_digit())
            .map_or(0, |last_non_digit| last_non_digit + 1);
        raw.get(digits_start..)
            .filter(|tail| !tail.is_empty())
            .and_then(|tail| tail.parse::<u64>().ok())
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

/// Focus-intent token minted by RimZ.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FocusNonce(Uuid);

impl FocusNonce {
    pub(crate) fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl std::fmt::Display for FocusNonce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Decode a record's optional lane name without failing the record: an empty
/// string is absent, and any other text an older binary stored is read through
/// [`LaneName::dashed`]. Pair it with `#[serde(default)]`.
pub(crate) fn lenient_lane_name<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<LaneName>, D::Error> {
    let raw = Option::<String>::deserialize(deserializer)?;
    Ok(raw
        .filter(|raw| !raw.is_empty())
        .map(|raw| LaneName::dashed(&raw)))
}

/// The worktree directory name a branch-style request maps to: `/` joins as `-`.
pub(crate) fn dashed_name(raw: &str) -> String {
    raw.replace('/', "-")
}

#[cfg(test)]
mod tests;
