//! Agent-address parsing, rendering, and live pane binding.
//!
//! The address grammar is `@<handle>#<channel>` (the canonical handle is the
//! inverse of the parser). Pane binding joins resolved panes to exact lifecycle
//! sessions, provisional launch cards, or sessionless lazy targets.
//!
//! Handles read like Slack. A role handle names a team member (`@coder`). A
//! *type handle* names a profile to fill — `@<kind>` (`@codex`) or `@<profile>`
//! (`@planner`) — and matches every such agent in the channel; the same handles
//! can also create one (see [`create_mention`]). An
//! *instance handle* names exactly one running agent — an explicit `--name`,
//! `@<kind>-<n>`, `@<petname>`, or a session-id prefix. `@all` is the broadcast
//! handle, and a pane id (`tmux:%1`, `zellij:terminal_3`) is a precise,
//! sigil-free, channel-agnostic address. The renderer prefers a unique role,
//! then an explicit name, then a non-kind profile, then the kind, then an
//! ordinal, then the petname, so a handle always round-trips to its agent.
//!
//! The channel is the workspace segment the room groups by — an explicit named
//! lane, else a worktree name, else an in-place named team stamped at launch as
//! `<dir>/<team>`, else a directory basename. Callers pass the *current*
//! channel; an explicit `#name`, `--channel name`, or `--worktree name` overrides it. A
//! `None` current channel means **all
//! channels** — it never silently narrows to "only worktree-less agents", so
//! addressing the room from a bare directory workspace still reaches every
//! agent.

use std::collections::BTreeMap;
use std::path::Path;

use crate::agents::AgentState;
use crate::ids::PaneId;
use crate::message::{MessageRecord, MessageSender, identity_handle};
use crate::store::snapshot::{PaneAgent, SidebarSnapshot};

pub use crate::store::snapshot::compose_channel;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TargetErr {
    #[error("{0}")]
    InvalidPaneId(String),
    #[error(
        "agent target `{target}` must start with `@` (try `@{target}`); pane ids like `tmux:%1` are the exception"
    )]
    MissingSigil { target: String },
    #[error("target `{target}` names channel `#{channel}` but channel flag names `{flag}`")]
    ChannelMismatch {
        target: String,
        channel: String,
        flag: String,
    },
    #[error(
        "no agent matches target `{target}`{suggestion}; run `rimz agents list` to see live agents"
    )]
    NoMatch { target: String, suggestion: String },
    #[error("no agent matches `{target}` in channel `#{channel}`; it is running in {elsewhere}")]
    NoMatchInChannel {
        target: String,
        channel: String,
        elsewhere: String,
    },
    #[error("target `{target}` matched multiple agents: {candidates}")]
    Ambiguous { target: String, candidates: String },
    #[error("pane `{pane_id}` is not bound to a known agent")]
    PaneUnbound { pane_id: PaneId },
}

/// A parsed agent mention selector — its arity (one or many) is intrinsic.
#[derive(Clone, Debug, PartialEq, Eq)]
enum AgentSelector {
    /// `@all` — every agent in the channel.
    All,
    /// `@<kind>` — every agent of that kind in the channel.
    Kind(String),
    /// `@<kind>-<n>` — the nth agent of that kind.
    KindOrdinal(String, u32),
    /// `@<petname>` or a session-id prefix — name beats prefix.
    NameOrSession(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectorTier {
    All,
    Role,
    Kind,
    KindOrdinal,
    ExplicitName,
    Profile,
    Name,
    SessionPrefix,
}

/// A parsed target: a precise pane, or an `@`-mention scoped to a channel.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Target {
    Pane(PaneId),
    Mention {
        selector: AgentSelector,
        channel: Option<String>,
    },
}

/// The shared accessor surface over the two resolution sources: rollup sessions
/// (`&AgentState`, used by management and parked message records) and the live
/// agent panes the producer bound (`&PaneAgent`, used by `message --steer` and
/// send-now messages). One matcher set serves both; each command chooses the source it
/// resolves over.
trait Candidate<'a>: Copy {
    fn kind(self) -> &'a str;
    fn kind_ordinal(self) -> Option<u32>;
    fn name(self) -> Option<&'a str>;
    fn name_explicit(self) -> bool;
    /// The `[agents.profiles]` profile this agent launched as, when it has one. A
    /// profile is a *type* handle — `@planner` may name several agents — so the
    /// name/profile matcher returns every profile match and lets arity decide.
    fn profile(self) -> Option<&'a str>;
    fn role(self) -> Option<&'a str>;
    fn channel(self) -> Option<&'a str>;
    fn session_id(self) -> Option<&'a str>;
    fn worktree_path(self) -> Option<&'a str>;
    fn pane_id(self) -> Option<&'a PaneId>;

    /// The channel label: stamped lane, else worktree-directory basename, else a
    /// placeholder.
    fn channel_label(self) -> String {
        compose_channel(
            self.channel(),
            self.worktree_path()
                .and_then(|path| path.rsplit('/').next()),
        )
        .unwrap_or_else(|| "no-worktree".to_owned())
    }

    fn in_worktree(self, filter: &str) -> bool {
        let branch_style_filter = filter
            .contains('/')
            .then(|| crate::worktree::dashed_name(filter));
        if let Some(channel) = self.channel().filter(|channel| !channel.is_empty()) {
            return channel == filter || branch_style_filter.as_deref() == Some(channel);
        }
        let channel = compose_channel(
            self.channel(),
            self.worktree_path()
                .and_then(|path| path.rsplit('/').next()),
        );
        channel.as_deref() == Some(filter)
            || self
                .worktree_path()
                .is_some_and(|path| path == filter || path.rsplit('/').next() == Some(filter))
            || branch_style_filter.as_deref().is_some_and(|filter| {
                channel.as_deref() == Some(filter)
                    || self
                        .worktree_path()
                        .and_then(|path| path.rsplit('/').next())
                        == Some(filter)
            })
    }
}

impl<'a> Candidate<'a> for &'a AgentState {
    fn kind(self) -> &'a str {
        self.kind.as_str()
    }
    fn kind_ordinal(self) -> Option<u32> {
        self.kind_ordinal
    }
    fn name(self) -> Option<&'a str> {
        self.name.as_deref()
    }
    fn name_explicit(self) -> bool {
        self.name_explicit
    }
    fn profile(self) -> Option<&'a str> {
        self.profile.as_deref()
    }
    fn role(self) -> Option<&'a str> {
        self.role.as_deref()
    }
    fn channel(self) -> Option<&'a str> {
        self.channel.as_deref()
    }
    fn session_id(self) -> Option<&'a str> {
        (!self.agent_id.is_provisional()).then(|| self.agent_id.as_str())
    }
    fn worktree_path(self) -> Option<&'a str> {
        self.worktree_path.as_deref()
    }
    fn pane_id(self) -> Option<&'a PaneId> {
        self.pane.as_ref().map(|pane| &pane.pane_id)
    }
}

impl<'a> Candidate<'a> for &'a PaneAgent {
    fn kind(self) -> &'a str {
        self.kind.as_str()
    }
    fn kind_ordinal(self) -> Option<u32> {
        self.kind_ordinal
    }
    fn name(self) -> Option<&'a str> {
        self.name.as_deref()
    }
    fn name_explicit(self) -> bool {
        self.name_explicit
    }
    fn profile(self) -> Option<&'a str> {
        self.profile.as_deref()
    }
    fn role(self) -> Option<&'a str> {
        self.role.as_deref()
    }
    fn channel(self) -> Option<&'a str> {
        self.channel.as_deref()
    }
    fn session_id(self) -> Option<&'a str> {
        self.agent_id.as_ref().map(|id| id.as_str())
    }
    fn worktree_path(self) -> Option<&'a str> {
        self.worktree_path.as_deref()
    }
    fn pane_id(self) -> Option<&'a PaneId> {
        Some(&self.pane_id)
    }
}

/// Resolve a target to exactly one rollup agent. `@all` or a kind that fans out
/// to several agents is [`TargetErr::Ambiguous`] here — pick a more specific
/// mention. Used by the single-agent management commands (`show`/`focus`/`wait`/
/// `stop`, `message clear`/`list`).
pub fn resolve_one<'a>(
    snapshot: &'a SidebarSnapshot,
    raw: &str,
    worktree_flag: Option<&str>,
    current_channel: Option<&str>,
) -> Result<&'a AgentState, TargetErr> {
    let candidates = addressable_agents(snapshot);
    let matches = resolve_mentions(raw, worktree_flag, current_channel, &candidates)?;
    match matches.as_slice() {
        [one] => Ok(one),
        many => Err(TargetErr::Ambiguous {
            target: raw.to_owned(),
            candidates: render_candidates(many),
        }),
    }
}

/// Resolve a target to every matching rollup agent (fan-out). Empty is an error.
/// Used by management fan-out reads and the durable-identity side of parked
/// messages; `message --steer` and send-now messages use [`resolve_targets`].
pub fn resolve_many<'a>(
    snapshot: &'a SidebarSnapshot,
    raw: &str,
    worktree_flag: Option<&str>,
    current_channel: Option<&str>,
) -> Result<Vec<&'a AgentState>, TargetErr> {
    let candidates = addressable_agents(snapshot);
    resolve_agents(raw, worktree_flag, current_channel, &candidates)
}

/// Resolve a target to every matching agent from a caller-supplied durable
/// candidate set. Message dispatch uses this over audit-scope rollups when the
/// live projection missed a receiver.
pub fn resolve_agents<'a>(
    raw: &str,
    worktree_flag: Option<&str>,
    current_channel: Option<&str>,
    candidates: &[&'a AgentState],
) -> Result<Vec<&'a AgentState>, TargetErr> {
    resolve_mentions(raw, worktree_flag, current_channel, candidates)
}

/// Resolve a target to exactly one agent from a caller-supplied durable
/// candidate set.
pub fn resolve_agent<'a>(
    raw: &str,
    worktree_flag: Option<&str>,
    current_channel: Option<&str>,
    candidates: &[&'a AgentState],
) -> Result<&'a AgentState, TargetErr> {
    let matches = resolve_agents(raw, worktree_flag, current_channel, candidates)?;
    match matches.as_slice() {
        [one] => Ok(one),
        many => Err(TargetErr::Ambiguous {
            target: raw.to_owned(),
            candidates: render_candidates(many),
        }),
    }
}

/// Resolve a live message target to every matching live agent pane: bound sessions
/// and lazy (sessionless) panes alike, each addressed by the pane the producer
/// bound this fold — so a daemon-routed session reaches its pane and a just
/// started agent is reachable before its first turn. Empty is an error.
pub fn resolve_targets<'a>(
    snapshot: &'a SidebarSnapshot,
    raw: &str,
    worktree_flag: Option<&str>,
    current_channel: Option<&str>,
) -> Result<Vec<&'a PaneAgent>, TargetErr> {
    let candidates: Vec<&PaneAgent> = snapshot.agent_panes.iter().collect();
    resolve_mentions(raw, worktree_flag, current_channel, &candidates)
}

/// How one live pane relates to lifecycle state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneBindingKind {
    Exact,
    Provisional,
    Lazy,
}

/// One resolved live pane and any lifecycle card it represents.
///
/// `agent` includes a provisional launch card. `exact_agent` is present only
/// after the pane carries a registered lifecycle session and is safe for
/// Waiting, gate, and context decisions.
#[derive(Clone, Copy, Debug)]
pub struct PaneBinding<'snapshot, 'pane> {
    pub pane: &'pane PaneAgent,
    pub agent: Option<&'snapshot AgentState>,
    pub exact_agent: Option<&'snapshot AgentState>,
    pub kind: PaneBindingKind,
}

impl PaneBinding<'_, '_> {
    pub fn matches_agent(&self, agent: &AgentState) -> bool {
        self.agent
            .is_some_and(|bound| bound.card_ref().matches(agent.card_ref()))
    }
}

/// Bind one pane to exact lifecycle state, a same-channel provisional launch,
/// or a sessionless lazy target. A pinned pane filters this result in place;
/// it never redirects to another pane.
pub fn pane_binding<'snapshot, 'pane>(
    snapshot: &'snapshot SidebarSnapshot,
    pane: &'pane PaneAgent,
    pinned: Option<&PaneId>,
) -> Option<PaneBinding<'snapshot, 'pane>> {
    if pinned.is_some_and(|pinned| pinned != &pane.pane_id) {
        return None;
    }
    if let Some(agent_id) = pane.agent_id.as_ref() {
        let agent = snapshot
            .agents
            .iter()
            .find(|agent| agent.kind == pane.kind && &agent.agent_id == agent_id)?;
        return Some(PaneBinding {
            pane,
            agent: Some(agent),
            exact_agent: Some(agent),
            kind: PaneBindingKind::Exact,
        });
    }
    if let Some(agent) = snapshot.agents.iter().find(|agent| {
        !agent.is_provider_subagent()
            && agent.kind == pane.kind
            && agent.agent_id.is_provisional()
            && agent_channel(agent) == pane.channel()
    }) {
        return Some(PaneBinding {
            pane,
            agent: Some(agent),
            exact_agent: None,
            kind: PaneBindingKind::Provisional,
        });
    }
    Some(PaneBinding {
        pane,
        agent: None,
        exact_agent: None,
        kind: PaneBindingKind::Lazy,
    })
}

/// Find the exact or provisional live pane associated with one logical card.
pub fn bind_agent<'a>(
    snapshot: &'a SidebarSnapshot,
    agent: &AgentState,
    pinned: Option<&PaneId>,
) -> Option<PaneBinding<'a, 'a>> {
    snapshot
        .agent_panes
        .iter()
        .filter_map(|pane| pane_binding(snapshot, pane, pinned))
        .find(|binding| binding.matches_agent(agent))
}

/// The full agent sessions that can be named in this snapshot. Provider-native
/// subagents remain display-only; pane-backed launched children are peers for
/// messaging and pane operations even though they render under a parent card.
///
/// One physical agent instance contributes at most one addressable row. Handle
/// rendering uses this same set so [`agent_handle`] stays the inverse of
/// [`parse_target`]. A pure rollup has no `agent_panes`, cannot observe
/// shadowing, and therefore yields every root.
pub fn addressable_agents(snapshot: &SidebarSnapshot) -> Vec<&AgentState> {
    snapshot
        .agents
        .iter()
        .filter(|agent| !agent.is_provider_subagent())
        .filter(|agent| !shadowed_by_pane_owner(snapshot, agent))
        .collect()
}

/// Whether a co-resident historical root has yielded its physical agent
/// instance to the pane's bound owner. One physical instance contributes at
/// most one recipient; shadowed sessions remain audit records but are not
/// addressable, including by exact session id.
pub(crate) fn shadowed_by_pane_owner(snapshot: &SidebarSnapshot, agent: &AgentState) -> bool {
    let Some(stamped_pane) = agent.pane.as_ref().map(|pane| &pane.pane_id) else {
        return false;
    };
    snapshot.agent_panes.iter().any(|pane| {
        &pane.pane_id == stamped_pane
            && pane
                .agent_id
                .as_ref()
                .is_some_and(|owner_id| pane.kind != agent.kind || owner_id != &agent.agent_id)
    })
}

/// The shared mention/pane resolution over any candidate source. `candidates`
/// are the roots in every channel; the channel narrows them for the match while
/// the unfiltered set seeds the channel-aware miss.
fn resolve_mentions<'a, C: Candidate<'a>>(
    raw: &str,
    worktree_flag: Option<&str>,
    current_channel: Option<&str>,
    candidates: &[C],
) -> Result<Vec<C>, TargetErr> {
    match parse_target(raw)? {
        Target::Pane(pane) => resolve_by_pane(raw, &pane, candidates).map(|one| vec![one]),
        Target::Mention { selector, channel } => {
            let channel =
                effective_channel(raw, channel.as_deref(), worktree_flag, current_channel)?;
            // A full session id is a pinned instance address, so it resolves
            // across channels like a pane id. Short prefixes still use the
            // channel-scoped selector path below.
            let exact_session = exact_session_match(&selector, candidates);
            match exact_session.as_slice() {
                [one] => return Ok(vec![*one]),
                [] => {}
                many => {
                    return Err(TargetErr::Ambiguous {
                        target: raw.to_owned(),
                        candidates: render_candidates(many),
                    });
                }
            }
            let in_channel: Vec<C> = candidates
                .iter()
                .copied()
                .filter(|candidate| {
                    channel
                        .as_deref()
                        .is_none_or(|filter| candidate.in_worktree(filter))
                })
                .collect();
            let matches = select(&selector, &in_channel);
            if !matches.is_empty() {
                return Ok(matches);
            }
            Err(no_match_error(candidates, raw, &selector, channel))
        }
    }
}

/// Require the `@` mention sigil (or a pane id). The `message` command calls
/// this so a bare `codex` is a clear miss with the
/// fix, keeping Slack muscle memory. The management commands resolve leniently
/// so a run id or bare pet name still works.
pub fn require_mention(raw: &str) -> Result<(), TargetErr> {
    if raw.contains(':') || raw.starts_with('@') {
        return Ok(());
    }
    Err(TargetErr::MissingSigil {
        target: raw.to_owned(),
    })
}

/// Whether `raw` is the broadcast handle `@all` — the explicit "everyone in the
/// channel" address. A broadcast opts into fan-out on its own, so it needs no
/// `--all`; a pane id is never a broadcast.
pub fn is_broadcast(raw: &str) -> bool {
    !raw.contains(':') && selector_of(raw) == "all"
}

/// Prefix a group send with the addressed selector so receivers read it as a
/// group message, not a private one. The marker drops any `#channel` suffix.
pub fn group_prefixed(raw: &str, text: &str) -> String {
    format!("@{}, {text}", selector_of(raw))
}

/// The selector and resolved channel a `--create` launch needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateMention {
    /// The bare handle (`codex`, `planner`, `swift-otter`, `claude-2`). The
    /// caller decides whether it names a launchable *type* (a kind or an profile)
    /// or a specific instance that must already exist.
    pub selector: String,
    /// The channel to launch into: an inline `#name`, the `--worktree` flag, or
    /// the current channel. `None` only outside any channel.
    pub channel: Option<String>,
}

/// Resolve `raw` into the selector and channel a `--create` launch would use, or
/// `None` when the target cannot create a fresh agent: a precise pane address or
/// the `@all` broadcast. An inline `#channel`/`--worktree` mismatch still errors,
/// so create-on-miss reuses the same channel reconciliation as delivery.
pub fn create_mention(
    raw: &str,
    worktree_flag: Option<&str>,
    current_channel: Option<&str>,
) -> Result<Option<CreateMention>, TargetErr> {
    match parse_target(raw)? {
        Target::Pane(_)
        | Target::Mention {
            selector: AgentSelector::All,
            ..
        } => Ok(None),
        Target::Mention { channel, .. } => {
            let channel =
                effective_channel(raw, channel.as_deref(), worktree_flag, current_channel)?;
            Ok(Some(CreateMention {
                selector: selector_of(raw).to_owned(),
                channel,
            }))
        }
    }
}

/// Parse the target grammar into the bare selector and any inline `#channel`.
/// Callers that keep their own match policy use this without opting into the
/// resolver's candidate selection.
pub fn parse_selector(raw: &str) -> Result<(String, Option<String>), TargetErr> {
    match parse_target(raw)? {
        Target::Pane(_) => Ok((raw.to_owned(), None)),
        Target::Mention { selector, channel } => Ok((selector_text(&selector), channel)),
    }
}

/// Reconcile an inline target channel with the caller's channel flag and
/// fallback channel. This exposes the resolver's channel grammar without its
/// candidate selection tiers.
pub fn reconcile_channel(
    raw: &str,
    inline: Option<&str>,
    flag: Option<&str>,
    current: Option<&str>,
) -> Result<Option<String>, TargetErr> {
    effective_channel(raw, inline, flag, current)
}

fn parse_target(raw: &str) -> Result<Target, TargetErr> {
    if raw.contains(':') {
        let pane = PaneId::parse(raw).map_err(|err| TargetErr::InvalidPaneId(err.to_string()))?;
        return Ok(Target::Pane(pane));
    }
    let (agent_part, channel) = match raw.split_once('#') {
        Some((agent, chan)) if !chan.is_empty() => (agent, Some(chan.to_owned())),
        _ => (raw, None),
    };
    // The `@` sigil is optional at the resolver — strip it when present. Strict
    // `@`-or-error lives in `require_mention`, applied only by message.
    let selector = agent_part.strip_prefix('@').unwrap_or(agent_part);
    if selector.is_empty() {
        return Err(TargetErr::NoMatch {
            target: raw.to_owned(),
            suggestion: String::new(),
        });
    }
    Ok(Target::Mention {
        selector: classify_selector(selector),
        channel,
    })
}

fn selector_text(selector: &AgentSelector) -> String {
    match selector {
        AgentSelector::All => "all".to_owned(),
        AgentSelector::Kind(kind) | AgentSelector::NameOrSession(kind) => kind.clone(),
        AgentSelector::KindOrdinal(kind, ordinal) => format!("{kind}-{ordinal}"),
    }
}

fn classify_selector(selector: &str) -> AgentSelector {
    if selector == "all" {
        return AgentSelector::All;
    }
    if let Some((kind, ordinal)) = parse_ordinal_selector(selector) {
        return AgentSelector::KindOrdinal(kind.to_owned(), ordinal);
    }
    if crate::agents::known_kinds().any(|kind| kind == selector) {
        return AgentSelector::Kind(selector.to_owned());
    }
    AgentSelector::NameOrSession(selector.to_owned())
}

fn select<'a, C: Candidate<'a>>(selector: &AgentSelector, candidates: &[C]) -> Vec<C> {
    let tier = winning_tier(selector, candidates);
    let mut matches = candidates
        .iter()
        .copied()
        .filter(|candidate| tier_matches(tier, selector, *candidate))
        .collect::<Vec<_>>();
    if let (SelectorTier::SessionPrefix, AgentSelector::NameOrSession(selector)) = (tier, selector)
    {
        matches = prefer_exact_session(selector, matches);
    }
    matches
}

fn winning_tier<'a, C: Candidate<'a>>(selector: &AgentSelector, candidates: &[C]) -> SelectorTier {
    match selector {
        AgentSelector::All => SelectorTier::All,
        AgentSelector::Kind(kind) => {
            if candidates
                .iter()
                .copied()
                .any(|candidate| candidate.role() == Some(kind.as_str()))
            {
                SelectorTier::Role
            } else {
                SelectorTier::Kind
            }
        }
        AgentSelector::KindOrdinal(..) => SelectorTier::KindOrdinal,
        AgentSelector::NameOrSession(_) => [
            SelectorTier::Role,
            SelectorTier::ExplicitName,
            SelectorTier::Profile,
            SelectorTier::Name,
            SelectorTier::SessionPrefix,
        ]
        .into_iter()
        .find(|tier| {
            candidates
                .iter()
                .copied()
                .any(|candidate| tier_matches(*tier, selector, candidate))
        })
        .unwrap_or(SelectorTier::SessionPrefix),
    }
}

fn tier_matches<'a, C: Candidate<'a>>(
    tier: SelectorTier,
    selector: &AgentSelector,
    candidate: C,
) -> bool {
    match (tier, selector) {
        (SelectorTier::All, AgentSelector::All) => true,
        (SelectorTier::Role, AgentSelector::Kind(value))
        | (SelectorTier::Role, AgentSelector::NameOrSession(value)) => {
            candidate.role() == Some(value.as_str())
        }
        (SelectorTier::Kind, AgentSelector::Kind(value)) => candidate.kind() == value,
        (SelectorTier::KindOrdinal, AgentSelector::KindOrdinal(kind, ordinal)) => {
            candidate.kind() == kind && candidate.kind_ordinal() == Some(*ordinal)
        }
        (SelectorTier::ExplicitName, AgentSelector::NameOrSession(value)) => {
            candidate.name_explicit() && candidate.name() == Some(value.as_str())
        }
        (SelectorTier::Profile, AgentSelector::NameOrSession(value)) => {
            candidate.profile() == Some(value.as_str())
        }
        (SelectorTier::Name, AgentSelector::NameOrSession(value)) => {
            candidate.name() == Some(value.as_str())
        }
        (SelectorTier::SessionPrefix, AgentSelector::NameOrSession(value)) => candidate
            .session_id()
            .is_some_and(|id| id.starts_with(value.as_str())),
        _ => false,
    }
}

fn exact_session_match<'a, C: Candidate<'a>>(selector: &AgentSelector, candidates: &[C]) -> Vec<C> {
    let AgentSelector::NameOrSession(selector) = selector else {
        return Vec::new();
    };
    candidates
        .iter()
        .copied()
        .filter(|candidate| candidate.session_id() == Some(selector.as_str()))
        .collect()
}

fn resolve_by_pane<'a, C: Candidate<'a>>(
    raw: &str,
    pane_id: &PaneId,
    candidates: &[C],
) -> Result<C, TargetErr> {
    let matches: Vec<C> = candidates
        .iter()
        .copied()
        .filter(|candidate| candidate.pane_id() == Some(pane_id))
        .collect();
    match matches.as_slice() {
        [one] => Ok(*one),
        [] => Err(TargetErr::PaneUnbound {
            pane_id: pane_id.clone(),
        }),
        many => Err(TargetErr::Ambiguous {
            target: raw.to_owned(),
            candidates: render_candidates(many),
        }),
    }
}

/// Reconcile the inline `#channel` with the channel flag (`--channel` or
/// `--worktree`; mismatch is an error), then fall back to the current channel
/// when neither is given. Returns an owned channel so it can outlive the parsed
/// target's borrow.
fn effective_channel(
    raw: &str,
    inline: Option<&str>,
    flag: Option<&str>,
    current: Option<&str>,
) -> Result<Option<String>, TargetErr> {
    let reconciled = match (inline, flag) {
        (Some(channel), Some(flag)) if channel != flag => {
            return Err(TargetErr::ChannelMismatch {
                target: raw.to_owned(),
                channel: channel.to_owned(),
                flag: flag.to_owned(),
            });
        }
        (Some(channel), _) => Some(channel.to_owned()),
        (None, Some(flag)) => Some(flag.to_owned()),
        (None, None) => None,
    };
    Ok(reconciled.or_else(|| current.map(ToOwned::to_owned)))
}

fn parse_ordinal_selector(selector: &str) -> Option<(&str, u32)> {
    let (kind, raw_ordinal) = selector.rsplit_once('-')?;
    if !crate::agents::known_kinds().any(|known| known == kind) {
        return None;
    }
    let ordinal = raw_ordinal.parse::<u32>().ok()?;
    (ordinal > 0).then_some((kind, ordinal))
}

fn prefer_exact_session<'a, C: Candidate<'a>>(selector: &str, candidates: Vec<C>) -> Vec<C> {
    let exact: Vec<C> = candidates
        .iter()
        .copied()
        .filter(|candidate| candidate.session_id() == Some(selector))
        .collect();
    if exact.is_empty() { candidates } else { exact }
}

/// Whether `agent` lives in the channel `filter` names — explicit channel,
/// directory channel, worktree path, or that path's basename. A
/// display-side wrapper over the resolver's [`Candidate::in_worktree`], so
/// channel membership keeps one definition.
pub fn agent_in_worktree(agent: &AgentState, filter: &str) -> bool {
    agent.in_worktree(filter)
}

/// Build the right miss for a mention that matched nothing. When a channel was
/// in play and the selector matches *elsewhere*, name those channels so the
/// fix is obvious; otherwise fall back to the generic did-you-mean miss.
fn no_match_error<'a, C: Candidate<'a>>(
    everywhere: &[C],
    raw: &str,
    selector: &AgentSelector,
    channel: Option<String>,
) -> TargetErr {
    if let Some(channel) = channel {
        let elsewhere = select(selector, everywhere);
        if !elsewhere.is_empty() {
            return TargetErr::NoMatchInChannel {
                target: raw.to_owned(),
                channel,
                elsewhere: channel_list(&elsewhere),
            };
        }
    }
    TargetErr::NoMatch {
        target: raw.to_owned(),
        suggestion: suggest_names(raw, everywhere),
    }
}

pub fn path_basename(path: &str) -> Option<&str> {
    path.rsplit('/').next().filter(|value| !value.is_empty())
}

/// The room channel a launch runs in: an explicit named lane wins, else a
/// separate worktree's directory name, else an in-place `<dir>/<team>` stamped
/// here, else `None`. This is the launch-side peer of the CLI's current-channel
/// resolution, so stamped agent identity and human commands agree.
pub fn resolve_room_channel(
    project_root: &Path,
    cwd: &Path,
    team: Option<&str>,
    explicit: Option<&str>,
) -> Option<String> {
    if let Some(channel) = explicit.filter(|channel| !channel.is_empty()) {
        return Some(channel.to_owned());
    }
    if cwd != project_root {
        return cwd
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
    }
    let team = team.filter(|team| !team.is_empty())?;
    match project_root.file_name().and_then(|name| name.to_str()) {
        Some(dir) if !dir.is_empty() => Some(format!("{dir}/{team}")),
        _ => Some(team.to_owned()),
    }
}

/// The agent's channel — the lane it cooperates in: stamped lane, else
/// worktree directory basename.
/// `None` when the agent runs outside any channel context.
/// The display-side `Option` peer of the resolver's [`Candidate::channel_label`].
pub fn agent_channel(agent: &AgentState) -> Option<String> {
    compose_channel(
        agent.channel.as_deref(),
        agent
            .worktree_path
            .as_deref()
            .and_then(|path| path.rsplit('/').next()),
    )
}

/// One live launch of a configured team in a lane.
#[derive(Clone, Debug)]
pub struct TeamCohort<'a> {
    pub team: &'a str,
    pub channel: String,
    pub members: Vec<&'a AgentState>,
}

/// Group live root team members by team and lane.
///
/// Agents launched outside a named lane share the `external` fallback, matching
/// the teams catalogue.
pub fn team_cohorts(agents: &[AgentState]) -> Vec<TeamCohort<'_>> {
    let mut grouped: BTreeMap<(&str, String), Vec<&AgentState>> = BTreeMap::new();
    for agent in agents
        .iter()
        .filter(|agent| !agent.is_provider_subagent() && agent.ended_at.is_none())
    {
        let Some(team) = agent.team.as_deref().filter(|team| !team.is_empty()) else {
            continue;
        };
        let channel = agent_channel(agent).unwrap_or_else(|| "external".to_owned());
        grouped.entry((team, channel)).or_default().push(agent);
    }
    grouped
        .into_iter()
        .map(|((team, channel), members)| TeamCohort {
            team,
            channel,
            members,
        })
        .collect()
}

/// Pane-backed descendants stamped with `parent` as their display parent.
///
/// Ended children remain in this projection so callers can list and wait on
/// completed supervised work. Lifecycle commands filter to live rows.
pub fn launched_children<'a>(agents: &'a [AgentState], parent: &AgentState) -> Vec<&'a AgentState> {
    let mut children = agents
        .iter()
        .filter(|agent| agent.is_launched_child_of(parent))
        .collect::<Vec<_>>();
    children.sort_by_key(|agent| (agent.registered_at, agent.agent_id.clone()));
    children
}

/// The team whose members occupy this lane, from the durable rollup.
///
/// A team launch stamps its lane three ways — an explicit channel name, a
/// worktree basename, or `<dir>/<team>` in place — so the channel string does
/// not name the team. The stamped `team` on the lane's agents does. `None` when
/// the lane holds no team members, or holds members of more than one team.
pub fn channel_team<'a>(agents: &'a [AgentState], channel: &str) -> Option<&'a str> {
    let mut found: Option<&str> = None;
    for agent in agents {
        let Some(team) = agent.team.as_deref().filter(|team| !team.is_empty()) else {
            continue;
        };
        if agent_channel(agent).as_deref() != Some(channel) {
            continue;
        }
        match found {
            None => found = Some(team),
            Some(existing) if existing == team => {}
            Some(_) => return None,
        }
    }
    found
}

/// The lane a message is delivered into: the bound session's channel when known,
/// else the live pane's channel, else the lane the address resolved within.
///
/// A freshly launched pane may not have captured its channel yet; the addressed
/// scope keeps same-channel hand-offs from rendering a spurious `#channel` on
/// the structured message header.
pub fn recipient_channel(
    target: &PaneAgent,
    bound: Option<&AgentState>,
    scope_channel: Option<&str>,
) -> Option<String> {
    bound
        .and_then(agent_channel)
        .or_else(|| target.channel())
        .or_else(|| scope_channel.map(ToOwned::to_owned))
}

/// The canonical rendered address of an agent — the inverse of [`parse_target`].
///
/// Returns the shortest mention that names exactly this agent among `peers`: a
/// role first, then an explicit name, then `@<kind>` when it is the only one of
/// its kind in scope, else a disambiguator. With `include_channel`, a channelled
/// agent appends `#<channel>` for ungrouped output and disambiguates within that
/// channel (an `@<kind>-<ordinal>`, with the petname as the fallback when no
/// ordinal is set). A grouped handle
/// (`include_channel = false`) reads under its channel's section header, so it
/// scopes the same way.
///
/// A channel-less agent in ungrouped output has no `#<channel>` suffix to scope
/// it, so it must distinguish itself from *every* same-kind agent: the channel
/// ordinal cannot (it repeats across channels), so the handle falls to the
/// globally-unique petname, then a session-id selector. This keeps the handle a
/// round-tripping address even for an agent running outside any worktree.
pub fn agent_handle(agent: &AgentState, peers: &[&AgentState], include_channel: bool) -> String {
    let channel = agent_channel(agent);
    let suffix = include_channel && channel.is_some();
    // Channel context — the scope a bare `@<kind>` resolves within — comes from
    // the `#<channel>` suffix or, in grouped output, the section header. Only an
    // ungrouped channel-less handle has neither, so it must scope globally.
    let scoped = suffix || !include_channel;
    let base = handle_base(agent, peers, scoped);
    match channel {
        Some(channel) if suffix => format!("{base}#{channel}"),
        _ => base,
    }
}

/// The sender class named by a structured message header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderKind {
    Agent,
    User,
}

/// Split a delivered prompt into its structured header and body.
///
/// The inverse of [`message_header`]: the handle keeps its leading `@` and any
/// `#channel` suffix. System and `--no-from` text carry no header.
pub fn parse_message_header(text: &str) -> Option<(HeaderKind, String, String)> {
    let (kind, rest) = text.split_once('\n')?;
    let kind = match kind {
        "Type: AGENT_MESSAGE" => HeaderKind::Agent,
        "Type: USER_MESSAGE" => HeaderKind::User,
        _ => return None,
    };
    let (from, rest) = rest.split_once('\n')?;
    let handle = from.strip_prefix("From: @")?;
    if handle.is_empty() || handle.chars().any(char::is_whitespace) {
        return None;
    }
    let body = rest.strip_prefix("Content:\n")?;
    Some((kind, format!("@{handle}"), body.to_owned()))
}

/// Split a batched pane paste into prompt sections. A blank-line boundary starts
/// a new section only when the following first line names a message type.
pub fn split_batched_prompt(text: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut cursor = 0;
    while let Some(relative) = text[cursor..].find("\n\n") {
        let boundary = cursor + relative;
        let mut next_start = boundary + 2;
        while text[next_start..].starts_with('\n') {
            next_start += 1;
        }
        let first_line = text[next_start..].lines().next().unwrap_or_default();
        if matches!(first_line, "Type: AGENT_MESSAGE" | "Type: USER_MESSAGE") {
            segments.push(&text[start..boundary]);
            start = next_start;
        }
        cursor = next_start;
    }
    if segments.is_empty() {
        vec![text]
    } else {
        segments.push(&text[start..]);
        segments
    }
}

/// Align one submitted pane paste with the records written as its batch.
///
/// Record text supplies the otherwise ambiguous boundaries between adjacent
/// messages. Agent- and human-authored records consume their rendered headers;
/// system records stay verbatim. Interior record whitespace matches verbatim;
/// only the paste's outer first/last whitespace follows hook normalization.
pub fn align_submitted_prompt<'a>(
    prompt: &'a str,
    records: &[&MessageRecord],
) -> Option<Vec<&'a str>> {
    if records.is_empty() {
        return None;
    }
    let mut remaining = prompt.trim();
    let mut segments = Vec::with_capacity(records.len());
    for (index, record) in records.iter().enumerate() {
        let segment = remaining;
        let mut body = segment;
        match record.sender {
            MessageSender::Agent { .. } => {
                let attributed = body.strip_prefix("Type: AGENT_MESSAGE\nFrom: @")?;
                let (handle, text) = attributed.split_once("\nContent:\n")?;
                if handle.is_empty() || handle.chars().any(char::is_whitespace) {
                    return None;
                }
                body = text;
            }
            MessageSender::Human => {
                body = body.strip_prefix("Type: USER_MESSAGE\nFrom: @user\nContent:\n")?;
            }
            MessageSender::System => {}
        }
        let first = index == 0;
        let last = index + 1 == records.len();
        if first {
            body = body.trim_start();
        }
        let expected = match (first, last) {
            (true, true) => record.text.trim(),
            (true, false) => record.text.trim_start(),
            (false, true) => record.text.trim_end(),
            (false, false) => record.text.as_str(),
        };
        if expected.is_empty() {
            return None;
        }
        let after_text = body.strip_prefix(expected)?;
        if last {
            if !after_text.is_empty() {
                return None;
            }
            segments.push(segment.trim());
            continue;
        }
        let after_join = after_text.strip_prefix("\n\n")?;
        let segment_end = segment.len() - after_text.len();
        segments.push(segment[..segment_end].trim());
        remaining = after_join;
    }
    Some(segments)
}

/// The structured header for a human- or agent-authored message.
///
/// Agent-authored text uses the shortest live handle when the sender is visible
/// in the snapshot and falls back to the launch environment identity.
/// System-authored text stays verbatim.
pub fn message_header(
    sender: &MessageSender,
    peers: &[&AgentState],
    target_channel: Option<&str>,
) -> Option<String> {
    if matches!(sender, MessageSender::Human) {
        return Some("Type: USER_MESSAGE\nFrom: @user\nContent:\n".to_owned());
    }
    let MessageSender::Agent {
        kind,
        name,
        profile,
        role,
        channel,
    } = sender
    else {
        return None;
    };
    if let Some(sender_name) = name.as_deref()
        && let Some(agent) = peers
            .iter()
            .copied()
            .find(|agent| agent.name.as_deref() == Some(sender_name))
    {
        let include_channel = agent_channel(agent).as_deref() != target_channel;
        return Some(format!(
            "Type: AGENT_MESSAGE\nFrom: {}\nContent:\n",
            agent_handle(agent, peers, include_channel)
        ));
    }
    let include_channel = channel.as_deref() != target_channel;
    let mut handle = identity_handle(kind, profile.as_deref(), role.as_deref());
    if include_channel && let Some(channel) = channel.as_deref().filter(|value| !value.is_empty()) {
        handle.push('#');
        handle.push_str(channel);
    }
    Some(format!("Type: AGENT_MESSAGE\nFrom: {handle}\nContent:\n"))
}

fn handle_base(agent: &AgentState, peers: &[&AgentState], scoped: bool) -> String {
    let channel = agent_channel(agent);
    let scoped_peers = peers
        .iter()
        .filter(|peer| !scoped || agent_channel(peer) == channel)
        .copied()
        .collect::<Vec<_>>();
    // Restart renders from an unchanged snapshot clone. Canonicalize that
    // clone first, then keep selector comparison pointer-exact below.
    let target_peer = scoped_peers
        .iter()
        .copied()
        .find(|peer| std::ptr::eq(*peer, agent))
        .or_else(|| scoped_peers.iter().copied().find(|peer| *peer == agent));
    if target_peer.is_none() {
        return absent_agent_handle_base(agent, &scoped_peers, scoped);
    }
    let resolves = |text: &str| {
        let selector = classify_selector(text);
        let exact = exact_session_match(&selector, peers);
        let matches = if exact.is_empty() {
            select(&selector, &scoped_peers)
        } else {
            exact
        };
        matches.len() == 1
            && matches.first().is_some_and(|matched| {
                target_peer.is_some_and(|target| std::ptr::eq(*matched, target))
            })
    };
    let ordinal = scoped
        .then(|| {
            agent
                .kind_ordinal
                .map(|ordinal| format!("{}-{ordinal}", agent.kind))
        })
        .flatten();
    let candidates = [
        agent.role.as_deref(),
        agent.name.as_deref().filter(|_| agent.name_explicit),
        agent.profile.as_deref(),
        Some(agent.kind.as_str()),
        ordinal.as_deref(),
        agent.name.as_deref(),
        Some(agent.agent_id.as_str()),
    ];
    for candidate in candidates.into_iter().flatten() {
        if resolves(candidate) {
            return format!("@{candidate}");
        }
    }
    format!("@{}", agent.agent_id)
}

/// Best-effort identity for a durable agent outside the live peer snapshot.
/// Condition records keep this historical label even though it cannot resolve
/// back through that live snapshot until the agent returns.
fn absent_agent_handle_base(
    agent: &AgentState,
    scoped_peers: &[&AgentState],
    scoped: bool,
) -> String {
    if let Some(role) = agent.role.as_deref()
        && scoped_peers
            .iter()
            .filter(|peer| peer.role.as_deref() == Some(role))
            .count()
            <= 1
    {
        return format!("@{role}");
    }
    if agent.name_explicit
        && let Some(name) = agent.name.as_deref()
    {
        return format!("@{name}");
    }
    if let Some(profile) = agent.profile.as_deref() {
        let profile_rivals = scoped_peers
            .iter()
            .filter(|peer| peer.profile.as_deref() == Some(profile))
            .count();
        let shadowed_by_name = scoped_peers
            .iter()
            .any(|peer| peer.name_explicit && peer.name.as_deref() == Some(profile));
        let known_kind = crate::agents::known_kinds().any(|kind| kind == profile);
        if profile_rivals <= 1 && !shadowed_by_name && !known_kind {
            return format!("@{profile}");
        }
    }
    if scoped_peers
        .iter()
        .filter(|peer| peer.kind == agent.kind)
        .count()
        <= 1
    {
        return format!("@{}", agent.kind);
    }
    if scoped && let Some(ordinal) = agent.kind_ordinal {
        return format!("@{}-{ordinal}", agent.kind);
    }
    match agent.name.as_deref() {
        Some(name) => format!("@{name}"),
        None => format!("@{}", agent.agent_id),
    }
}

/// A deduplicated, quoted list of the channels a selector matches.
fn channel_list<'a, C: Candidate<'a>>(candidates: &[C]) -> String {
    let mut names: Vec<String> = candidates
        .iter()
        .map(|candidate| candidate.channel_label())
        .collect();
    names.sort_unstable();
    names.dedup();
    names
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// How many ambiguous candidates to spell out before collapsing the tail into
/// a `(+K more)` count — enough to disambiguate a real clash, never a fleet dump.
const CANDIDATE_CAP: usize = 8;

fn render_candidates<'a, C: Candidate<'a>>(candidates: &[C]) -> String {
    let mut rendered = candidates
        .iter()
        .take(CANDIDATE_CAP)
        .map(|candidate| {
            let name = candidate.name().unwrap_or_else(|| {
                // A bound session with no pet name reads `unnamed`; a lazy pane
                // with no session reads `unbound` so the miss shows it has none.
                if candidate.session_id().is_some() {
                    "unnamed"
                } else {
                    "unbound"
                }
            });
            let kind = match candidate.kind_ordinal() {
                Some(ordinal) => format!("{}-{}", candidate.kind(), ordinal),
                None => candidate.kind().to_owned(),
            };
            let worktree = candidate.channel_label();
            let pane = candidate
                .pane_id()
                .map(ToString::to_string)
                .unwrap_or_else(|| "no-pane".to_owned());
            match candidate.role().or_else(|| candidate.profile()) {
                Some(selector) => format!("{selector} {name} {kind} {worktree} {pane}"),
                None => format!("{name} {kind} {worktree} {pane}"),
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let extra = candidates.len().saturating_sub(CANDIDATE_CAP);
    if extra > 0 {
        rendered.push_str(&format!(" (+{extra} more)"));
    }
    rendered
}

/// The bare selector behind a target, for did-you-mean: drop the leading `@`
/// sigil and any trailing `#channel`.
fn selector_of(raw: &str) -> &str {
    let without_channel = raw.split('#').next().unwrap_or(raw);
    without_channel.strip_prefix('@').unwrap_or(without_channel)
}

/// A short "did you mean" suffix for a target miss: live agent names close to
/// the selector by prefix, substring, or a shared name token (case-insensitive),
/// capped at three. Empty when nothing is close, so the error stays a bare
/// pointer to `rimz agents list`.
fn suggest_names<'a, C: Candidate<'a>>(raw: &str, candidates: &[C]) -> String {
    let selector = selector_of(raw).to_lowercase();
    if selector.is_empty() {
        return String::new();
    }
    let mut names: Vec<&str> = candidates
        .iter()
        .filter_map(|candidate| candidate.name())
        .filter(|name| {
            let lower = name.to_lowercase();
            lower.contains(&selector)
                || selector.contains(&lower)
                || shares_token(&lower, &selector)
        })
        .collect();
    names.sort_unstable();
    names.dedup();
    names.truncate(3);
    if names.is_empty() {
        return String::new();
    }
    let joined = names
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(" (did you mean {joined}?)")
}

/// Whether two pet names share a meaningful `-`-delimited token, so
/// `swift-otter` suggests `otter-swift`. Tokens under three chars are too noisy
/// to match on.
fn shares_token(a: &str, b: &str) -> bool {
    a.split('-')
        .filter(|token| token.len() >= 3)
        .any(|token| b.split('-').any(|other| other == token))
}

#[cfg(test)]
mod tests;
