//! Agent target parsing and snapshot resolution.
//!
//! Targets read like Slack: `@<agent>` mentions who, `#<worktree>` names the
//! channel. `@all` and a bare `@<kind>` fan out to every match; `@<kind>-<n>`,
//! `@<petname>`, and a session-id prefix name one agent. A pane id
//! (`tmux:%1`, `zellij:terminal_3`) is a precise, sigil-free, channel-agnostic
//! address.
//!
//! The channel narrows to one worktree. Callers pass the *current* channel
//! (the worktree the command runs in); an explicit `#name` or `--worktree name`
//! overrides it. A `None` current channel means **all channels** — it never
//! silently narrows to "only worktree-less agents", so addressing the room
//! from a bare directory workspace still reaches every agent.

use crate::feed::AgentState;
use crate::ids::PaneId;
use crate::ledger::snapshot::SidebarSnapshot;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TargetErr {
    #[error("{0}")]
    InvalidPaneId(String),
    #[error(
        "agent target `{target}` must start with `@` (try `@{target}`); pane ids like `tmux:%1` are the exception"
    )]
    MissingSigil { target: String },
    #[error("target `{target}` names channel `#{channel}` but --worktree names `{flag}`")]
    WorktreeMismatch {
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

/// A parsed target: a precise pane, or an `@`-mention scoped to a channel.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Target {
    Pane(PaneId),
    Mention {
        selector: AgentSelector,
        channel: Option<String>,
    },
}

/// Resolve a target to exactly one agent. `@all` or a kind that fans out to
/// several agents is [`TargetErr::Ambiguous`] here — pick a more specific
/// mention. Used by the single-agent commands (`show`/`focus`/`wait`/`stop`,
/// `queue clear`/`list`).
pub fn resolve_one<'a>(
    snapshot: &'a SidebarSnapshot,
    raw: &str,
    worktree_flag: Option<&str>,
    current_channel: Option<&str>,
) -> Result<&'a AgentState, TargetErr> {
    let matches = resolve_many(snapshot, raw, worktree_flag, current_channel)?;
    match matches.as_slice() {
        [agent] => Ok(agent),
        many => Err(TargetErr::Ambiguous {
            target: raw.to_owned(),
            candidates: render_candidates(many),
        }),
    }
}

/// Resolve a target to every matching agent (fan-out). Empty is an error.
/// Used by the broadcast commands (`steer`, `queue add`).
pub fn resolve_many<'a>(
    snapshot: &'a SidebarSnapshot,
    raw: &str,
    worktree_flag: Option<&str>,
    current_channel: Option<&str>,
) -> Result<Vec<&'a AgentState>, TargetErr> {
    match parse_target(raw)? {
        Target::Pane(pane) => Ok(vec![resolve_by_pane(snapshot, raw, &pane)?]),
        Target::Mention { selector, channel } => {
            let channel =
                effective_channel(raw, channel.as_deref(), worktree_flag, current_channel)?;
            let agents = live_root_agents(snapshot, channel.as_deref());
            let matches = select(&selector, &agents);
            if !matches.is_empty() {
                return Ok(matches);
            }
            Err(no_match_error(snapshot, raw, &selector, channel))
        }
    }
}

/// Require the `@` mention sigil (or a pane id). The "talk" commands
/// (`steer`, `queue`) call this so a bare `codex` is a clear miss with the
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
    // `@`-or-error lives in `require_mention`, applied only by steer/queue.
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

fn select<'a>(selector: &AgentSelector, agents: &[&'a AgentState]) -> Vec<&'a AgentState> {
    match selector {
        AgentSelector::All => agents.to_vec(),
        AgentSelector::Kind(kind) => agents
            .iter()
            .copied()
            .filter(|agent| agent.kind.as_str() == kind)
            .collect(),
        AgentSelector::KindOrdinal(kind, ordinal) => agents
            .iter()
            .copied()
            .filter(|agent| agent.kind.as_str() == kind && agent.kind_ordinal == Some(*ordinal))
            .collect(),
        AgentSelector::NameOrSession(selector) => {
            let by_name: Vec<&AgentState> = agents
                .iter()
                .copied()
                .filter(|agent| agent.name.as_deref() == Some(selector.as_str()))
                .collect();
            if !by_name.is_empty() {
                return by_name;
            }
            let by_prefix: Vec<&AgentState> = agents
                .iter()
                .copied()
                .filter(|agent| agent.agent_id.as_str().starts_with(selector.as_str()))
                .collect();
            prefer_exact_session_raw(selector, by_prefix)
        }
    }
}

fn resolve_by_pane<'a>(
    snapshot: &'a SidebarSnapshot,
    raw: &str,
    pane_id: &PaneId,
) -> Result<&'a AgentState, TargetErr> {
    let live_agents = live_root_agents(snapshot, None);
    let matches: Vec<&AgentState> = live_agents
        .iter()
        .copied()
        .filter(|agent| {
            agent
                .pane
                .as_ref()
                .is_some_and(|pane| pane.pane_id == *pane_id)
        })
        .collect();
    match matches.as_slice() {
        [agent] => Ok(agent),
        [] => Err(TargetErr::PaneUnbound {
            pane_id: pane_id.clone(),
        }),
        many => Err(TargetErr::Ambiguous {
            target: raw.to_owned(),
            candidates: render_candidates(many),
        }),
    }
}

fn live_root_agents<'a>(
    snapshot: &'a SidebarSnapshot,
    channel_filter: Option<&str>,
) -> Vec<&'a AgentState> {
    snapshot
        .agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .filter(|agent| channel_filter.is_none_or(|filter| agent_in_worktree(agent, filter)))
        .collect()
}

/// Reconcile the inline `#channel` with the `--worktree` flag (mismatch is an
/// error), then fall back to the current channel when neither is given. Returns
/// an owned channel so it can outlive the parsed target's borrow.
fn effective_channel(
    raw: &str,
    inline: Option<&str>,
    flag: Option<&str>,
    current: Option<&str>,
) -> Result<Option<String>, TargetErr> {
    let reconciled = match (inline, flag) {
        (Some(channel), Some(flag)) if channel != flag => {
            return Err(TargetErr::WorktreeMismatch {
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

fn prefer_exact_session_raw<'a>(
    selector: &str,
    candidates: Vec<&'a AgentState>,
) -> Vec<&'a AgentState> {
    let exact: Vec<&AgentState> = candidates
        .iter()
        .copied()
        .filter(|agent| agent.agent_id.as_str() == selector)
        .collect();
    if exact.is_empty() { candidates } else { exact }
}

fn agent_in_worktree(agent: &AgentState, filter: &str) -> bool {
    agent.worktree_branch.as_deref() == Some(filter)
        || agent
            .worktree_path
            .as_deref()
            .is_some_and(|path| path == filter || path.rsplit('/').next() == Some(filter))
}

/// Build the right miss for a mention that matched nothing. When a channel was
/// in play and the selector matches *elsewhere*, name those channels so the
/// fix is obvious; otherwise fall back to the generic did-you-mean miss.
fn no_match_error(
    snapshot: &SidebarSnapshot,
    raw: &str,
    selector: &AgentSelector,
    channel: Option<String>,
) -> TargetErr {
    let everywhere = live_root_agents(snapshot, None);
    if let Some(channel) = channel {
        let elsewhere = select(selector, &everywhere);
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
        suggestion: suggest_names(raw, &everywhere),
    }
}

/// The channel label for an agent: its branch, else its worktree directory
/// basename, else a placeholder.
fn agent_channel_label(agent: &AgentState) -> String {
    agent
        .worktree_branch
        .clone()
        .or_else(|| {
            agent
                .worktree_path
                .as_deref()
                .and_then(|path| path.rsplit('/').next())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "no-worktree".to_owned())
}

/// A deduplicated, quoted list of the channels a selector matches.
fn channel_list(agents: &[&AgentState]) -> String {
    let mut names: Vec<String> = agents
        .iter()
        .map(|agent| agent_channel_label(agent))
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

fn render_candidates(candidates: &[&AgentState]) -> String {
    let mut rendered = candidates
        .iter()
        .take(CANDIDATE_CAP)
        .map(|agent| {
            let name = agent.name.as_deref().unwrap_or("unnamed");
            let kind = match agent.kind_ordinal {
                Some(ordinal) => format!("{}-{}", agent.kind, ordinal),
                None => agent.kind.to_string(),
            };
            let worktree = agent_channel_label(agent);
            let pane = agent
                .pane
                .as_ref()
                .map(|pane| pane.pane_id.to_string())
                .unwrap_or_else(|| "no-pane".to_owned());
            format!("{name} {kind} {worktree} {pane}")
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
fn suggest_names(raw: &str, live_agents: &[&AgentState]) -> String {
    let selector = selector_of(raw).to_lowercase();
    if selector.is_empty() {
        return String::new();
    }
    let mut names: Vec<&str> = live_agents
        .iter()
        .filter_map(|agent| agent.name.as_deref())
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
mod tests {
    use jiff::Timestamp;

    use super::*;
    use crate::feed::{AgentStatus, PaneRef};
    use crate::ids::{AgentKind, AgentSessionId, MuxName, WorkspaceId};

    #[test]
    fn resolve_prefers_name_ordinal_kind_then_session_prefix() {
        let mut snapshot = empty_snapshot();
        let mut alpha = agent("claude", "session-alpha", Some("main"), "terminal_1");
        alpha.name = Some("lucid-atlas".to_owned());
        alpha.kind_ordinal = Some(1);
        let mut beta = agent("claude", "session-beta", Some("feature/x.y"), "terminal_2");
        beta.name = Some("bright-beacon".to_owned());
        beta.kind_ordinal = Some(2);
        snapshot.agents = vec![alpha, beta];

        assert_eq!(
            resolve_one(&snapshot, "@lucid-atlas", None, None)
                .unwrap()
                .agent_id
                .as_str(),
            "session-alpha"
        );
        assert_eq!(
            resolve_one(&snapshot, "@claude-2", None, None)
                .unwrap()
                .agent_id
                .as_str(),
            "session-beta"
        );
        assert!(matches!(
            resolve_one(&snapshot, "@claude", None, None),
            Err(TargetErr::Ambiguous { .. })
        ));
        // Compact channel form picks the branch.
        assert_eq!(
            resolve_one(&snapshot, "@claude#feature/x.y", None, None)
                .unwrap()
                .agent_id
                .as_str(),
            "session-beta"
        );
        assert_eq!(
            resolve_one(&snapshot, "@session-a", None, None)
                .unwrap()
                .agent_id
                .as_str(),
            "session-alpha"
        );
    }

    #[test]
    fn require_mention_demands_the_sigil() {
        // steer/queue enforce `@`; pane ids are exempt.
        assert!(require_mention("claude").is_err());
        assert!(require_mention("@claude").is_ok());
        assert!(require_mention("@all").is_ok());
        assert!(require_mention("tmux:%1").is_ok());
        // The removed `selector@worktree` infix is not a mention.
        assert!(require_mention("claude@main").is_err());
    }

    #[test]
    fn old_infix_no_longer_scopes_by_worktree() {
        let mut snapshot = empty_snapshot();
        let agent = agent("claude", "session-alpha", Some("main"), "terminal_1");
        snapshot.agents = vec![agent];
        // `claude@main` is just an unknown name now, not "claude in main".
        assert!(matches!(
            resolve_one(&snapshot, "@claude@main", None, None),
            Err(TargetErr::NoMatch { .. })
        ));
    }

    #[test]
    fn pane_id_bypasses_sigils() {
        let mut snapshot = empty_snapshot();
        let agent = agent("claude", "session-pane", Some("main"), "terminal_7");
        snapshot.agents = vec![agent];
        assert_eq!(
            resolve_one(&snapshot, "zellij:terminal_7", None, Some("other"))
                .unwrap()
                .agent_id
                .as_str(),
            "session-pane"
        );
    }

    #[test]
    fn at_kind_ordinal_never_falls_through_to_session_prefix() {
        let mut snapshot = empty_snapshot();
        let mut agent = agent("codex", "claude-1-session", Some("main"), "terminal_1");
        agent.name = Some("solid-lumen".to_owned());
        snapshot.agents = vec![agent];

        assert!(matches!(
            resolve_one(&snapshot, "@claude-1", None, None),
            Err(TargetErr::NoMatch { .. })
        ));
    }

    #[test]
    fn at_kind_fans_out_but_resolve_one_is_ambiguous() {
        let mut snapshot = empty_snapshot();
        let mut one = agent("claude", "session-1", Some("main"), "terminal_1");
        one.kind_ordinal = Some(1);
        let mut two = agent("claude", "session-2", Some("main"), "terminal_2");
        two.kind_ordinal = Some(2);
        let codex = agent("codex", "session-3", Some("main"), "terminal_3");
        snapshot.agents = vec![one, two, codex];

        let many = resolve_many(&snapshot, "@claude", None, None).unwrap();
        assert_eq!(many.len(), 2);
        assert!(matches!(
            resolve_one(&snapshot, "@claude", None, None),
            Err(TargetErr::Ambiguous { .. })
        ));
        // A specific ordinal stays single.
        assert_eq!(
            resolve_one(&snapshot, "@claude-2", None, None)
                .unwrap()
                .agent_id
                .as_str(),
            "session-2"
        );
    }

    #[test]
    fn at_all_fans_to_the_channel_only() {
        let mut snapshot = empty_snapshot();
        let feat_claude = agent("claude", "session-a", Some("feat"), "terminal_1");
        let feat_codex = agent("codex", "session-b", Some("feat"), "terminal_2");
        let main_claude = agent("claude", "session-c", Some("main"), "terminal_3");
        snapshot.agents = vec![feat_claude, feat_codex, main_claude];

        let ids: Vec<&str> = resolve_many(&snapshot, "@all", None, Some("feat"))
            .unwrap()
            .iter()
            .map(|agent| agent.agent_id.as_str())
            .collect();
        assert_eq!(ids, vec!["session-a", "session-b"]);
    }

    #[test]
    fn current_channel_default_applies() {
        let mut snapshot = empty_snapshot();
        let feat = agent("claude", "session-feat", Some("feat"), "terminal_1");
        let main = agent("claude", "session-main", Some("main"), "terminal_2");
        snapshot.agents = vec![feat, main];

        assert_eq!(
            resolve_one(&snapshot, "@claude", None, Some("feat"))
                .unwrap()
                .agent_id
                .as_str(),
            "session-feat"
        );
    }

    #[test]
    fn none_current_channel_means_all_channels() {
        let mut snapshot = empty_snapshot();
        let feat = agent("claude", "session-feat", Some("feat"), "terminal_1");
        let main = agent("claude", "session-main", Some("main"), "terminal_2");
        snapshot.agents = vec![feat, main];

        // No current channel must not silently narrow — both are visible.
        assert!(matches!(
            resolve_one(&snapshot, "@claude", None, None),
            Err(TargetErr::Ambiguous { .. })
        ));
    }

    #[test]
    fn zero_in_channel_but_matches_elsewhere() {
        let mut snapshot = empty_snapshot();
        let main_codex = agent("codex", "session-codex", Some("main"), "terminal_1");
        snapshot.agents = vec![main_codex];

        let err = resolve_one(&snapshot, "@codex#cli-docs", None, None).unwrap_err();
        let message = err.to_string();
        assert!(
            matches!(err, TargetErr::NoMatchInChannel { .. }),
            "expected a channel-scoped miss: {message}"
        );
        assert!(
            message.contains("`main`"),
            "names the real channel: {message}"
        );
    }

    #[test]
    fn rejects_conflicting_channels() {
        let snapshot = empty_snapshot();
        assert!(matches!(
            resolve_one(&snapshot, "@claude#main", Some("docs"), None),
            Err(TargetErr::WorktreeMismatch { .. })
        ));
    }

    #[test]
    fn splits_channel_at_first_hash() {
        let mut snapshot = empty_snapshot();
        let mut agent = agent("claude", "session-alpha", Some("feature/x.y"), "terminal_1");
        agent.name = Some("lucid-atlas".to_owned());
        snapshot.agents = vec![agent];

        assert_eq!(
            resolve_one(&snapshot, "@lucid-atlas#feature/x.y", None, None)
                .unwrap()
                .agent_id
                .as_str(),
            "session-alpha"
        );
    }

    #[test]
    fn no_match_points_to_the_list_without_dumping_the_roster() {
        let mut snapshot = empty_snapshot();
        let names = ["calm-fox", "bold-pine", "warm-dune"];
        snapshot.agents = names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let mut agent = agent(
                    "claude",
                    &format!("session-{i}"),
                    Some("main"),
                    "terminal_1",
                );
                agent.name = Some((*name).to_owned());
                agent
            })
            .collect();

        let err = resolve_one(&snapshot, "@missing-name", None, None).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("run `rimz agents list`"),
            "points at the list: {message}"
        );
        assert!(
            names.iter().all(|name| !message.contains(name)),
            "no roster names leak in: {message}"
        );
    }

    #[test]
    fn no_match_suggests_close_pet_names() {
        let mut snapshot = empty_snapshot();
        let mut close = agent("claude", "session-1", Some("main"), "terminal_1");
        close.name = Some("otter-swift".to_owned());
        let mut far = agent("claude", "session-2", Some("main"), "terminal_2");
        far.name = Some("calm-fox".to_owned());
        snapshot.agents = vec![close, far];

        let message = resolve_one(&snapshot, "@swift-otter", None, None)
            .unwrap_err()
            .to_string();
        assert!(
            message.contains("did you mean") && message.contains("otter-swift"),
            "suggests the token-sharing name: {message}"
        );
        assert!(
            !message.contains("calm-fox"),
            "skips unrelated names: {message}"
        );
    }

    fn empty_snapshot() -> SidebarSnapshot {
        SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-target-test")),
            Vec::new(),
            Vec::new(),
            Timestamp::now(),
        )
    }

    fn agent(kind: &str, id: &str, branch: Option<&str>, raw_pane: &str) -> AgentState {
        let now = Timestamp::now();
        AgentState {
            agent_id: AgentSessionId::from(id),
            kind: AgentKind::new_unchecked(kind),
            name: None,
            kind_ordinal: None,
            status: AgentStatus::Idle,
            phase: crate::agents::TurnPhase::Idle,
            pane: Some(PaneRef::from_id(PaneId::from_parts(
                MuxName::Zellij,
                raw_pane,
            ))),
            agent_pid: None,
            agent_process_start: None,
            runtime_owner: None,
            parent_agent_id: None,
            worktree_path: branch.map(|branch| format!("/repo/{branch}")),
            worktree_branch: branch.map(ToOwned::to_owned),
            task: None,
            prompt: None,
            transcript_path: None,
            recent_prompts: Vec::new(),
            model: None,
            effort: None,
            context_pct: None,
            context_window: None,
            total_tokens: None,
            cache_read_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
            todo_done: None,
            todo_total: None,
            context: None,
            subagent_description: None,
            subagent_started_at: None,
            turn_started_at: None,
            compacting_since: None,
            compaction_count: 0,
            last_seen: now,
            last_activity: now,
            registered_at: Some(now),
        }
    }
}
