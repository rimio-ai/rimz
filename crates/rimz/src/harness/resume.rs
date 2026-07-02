//! Resume-on-rebirth planning: turn the durable agent rollup into the tabs a
//! reborn session re-seeds.
//!
//! When the CLI admits agent recovery for a reborn room — a machine reboot by
//! default — the agents' processes are gone, but the ledger remembers them.
//! This module reads that memory (the audit rollup, which keeps the
//! dead-process agents the runtime projection would expel) and plans one
//! `#channel` tab per worktree, with one resume pane per prior root agent, so
//! the next birth can recover where the user left off instead of empty.
//!
//! Pure over its inputs: the caller supplies the rollup, the set of cleanly
//! ended sessions, and a worktree-exists predicate, so every filtering rule is
//! unit-tested without a multiplexer or the filesystem. The launcher
//! ([`crate::mux::MuxBackend`]) seeds the resulting [`ResumeTab`]s at birth and
//! stays ignorant of agents and the ledger.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use crate::agents::AgentState;
use crate::agents::find_adapter;
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::mux::ResumeTab;

/// The default ceiling on agents auto-resumed into one reborn session, so a
/// long-lived workspace cannot fork-bomb a fleet of agent processes on birth.
/// Anything past it is reported, never silently dropped.
pub const DEFAULT_RESUME_MAX: usize = 8;

/// Why a candidate agent was not resumed — surfaced in the start report so a
/// skipped agent stays visible rather than silently lost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeSkipReason {
    /// The agent's kind has no resume CLI ([`crate::agents::AgentAdapter::resume_command`]).
    NoResumeSupport,
    /// Dropped to stay within the resume cap.
    OverCap,
}

/// A candidate that the planner deliberately did not resume, with the reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumeSkip {
    pub label: String,
    pub reason: ResumeSkipReason,
}

/// What a reborn session should re-seed, and what it deliberately left out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResumePlan {
    /// The tabs to seed, ordered by their freshest pane activity (the lead is
    /// the focus target). Panes inside each tab are freshest-first.
    pub tabs: Vec<ResumeTab>,
    /// Candidates not resumed, each with its reason — the start report names them.
    pub skipped: Vec<ResumeSkip>,
    /// Candidates whose worktree disappeared; the caller records these as
    /// durable end traces so they leave the next resume candidate set.
    pub tombstone: Vec<(AgentKind, AgentSessionId)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ResumeTabIdentity {
    Channel(String),
    Cwd(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PlannedResumeTab {
    identity: ResumeTabIdentity,
    tab: ResumeTab,
}

impl ResumePlan {
    /// Whether there is nothing to seed — the birth is exactly the bare working
    /// room.
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }
}

/// Plan the resume seeds for one reborn session from the durable agent rollup.
///
/// `agents` is the audit rollup (dead-process agents intact); `ended` is the
/// `(kind, agent_id)` set the user closed cleanly from
/// [`crate::RuntimeProjection::ended`]; `max` caps the auto-launched panes;
/// `worktree_exists` decides whether a candidate's worktree
/// is still on disk (production passes `|p| p.is_dir()`); `rimz_bin` is the
/// `rimz` executable each pane's wrapper argv names (production passes
/// `std::env::current_exe()`).
///
/// A candidate qualifies when it is a root agent (subagents ride their parent),
/// was bound to a pane in the incarnation that died, still carries a session id
/// and a worktree, and was not cleanly ended. The rollup is workspace-scoped and
/// a `session.rebirth` boundary clears every pane stamp recorded before it, so a
/// surviving (non-`None`) pane stamp means the agent was live in the incarnation
/// the rebirth replaces — exactly the set to bring back. One pane hosts one
/// agent: a relaunch that re-used a pane id collapses to its newest stamp —
/// the same rule the live sidebar binds by (`stamped_agent_for_pane`, in
/// `ledger::snapshot::panes`) — so resume never doubles a pane, while two
/// concurrent agents in one worktree (distinct panes) share one `#channel` tab.
pub fn plan_resume(
    agents: &[AgentState],
    ended: &BTreeSet<(AgentKind, AgentSessionId)>,
    max: usize,
    worktree_exists: impl Fn(&Path) -> bool,
    rimz_bin: &Path,
) -> ResumePlan {
    // Root agents that were bound to a pane in the dead incarnation, still
    // identified, and not cleanly ended. A subagent is paneless and rides its
    // parent, so it is filtered out here and never resumed standalone.
    let mut candidates: Vec<&AgentState> = agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .filter(|agent| !agent.agent_id.is_empty())
        .filter(|agent| {
            agent
                .worktree_path
                .as_deref()
                .is_some_and(|path| !path.is_empty())
        })
        .filter(|agent| agent.pane.is_some())
        .filter(|agent| !ended.contains(&(agent.kind.clone(), agent.agent_id.clone())))
        .collect();

    // Most-recently-active first (deterministic on ties), so the newest session
    // wins supersession, the lead pane is the focus target, and the cap keeps
    // the freshest agents.
    candidates.sort_by(|a, b| {
        b.last_activity
            .cmp(&a.last_activity)
            .then_with(|| a.agent_id.cmp(&b.agent_id))
    });

    let mut seen: HashSet<PaneId> = HashSet::new();
    let mut plan = ResumePlan::default();
    let mut tabs: Vec<PlannedResumeTab> = Vec::new();
    for agent in candidates {
        // `pane` is `Some` and `worktree_path` is `Some(non-empty)` by the
        // filters above. The pane is the unit of identity: an older relaunch
        // that re-used this pane id is superseded by the newest stamp (the
        // candidates are newest-first, so the first one seen for a pane wins),
        // mirroring the live binding's `stamped_agent_for_pane`. Distinct panes
        // — including two same-kind agents in one worktree — each get a seed.
        let pane_id = agent
            .pane
            .as_ref()
            .expect("candidates are filtered to a stamped pane")
            .pane_id
            .clone();
        if !seen.insert(pane_id) {
            continue;
        }
        let worktree = agent.worktree_path.clone().unwrap_or_default();
        let cwd = PathBuf::from(&worktree);
        let label = build_label(
            &agent.kind,
            agent.channel.as_deref(),
            agent.worktree_branch.as_deref(),
            &cwd,
        );
        if !worktree_exists(&cwd) {
            plan.tombstone
                .push((agent.kind.clone(), agent.agent_id.clone()));
            continue;
        }
        let supports_resume = find_adapter(&agent.kind)
            .is_some_and(|adapter| adapter.resume_command(&agent.agent_id, &cwd).is_some());
        if !supports_resume {
            plan.skipped.push(ResumeSkip {
                label,
                reason: ResumeSkipReason::NoResumeSupport,
            });
            continue;
        }
        let seeded = tabs.iter().map(|tab| tab.tab.panes.len()).sum::<usize>();
        if seeded >= max {
            plan.skipped.push(ResumeSkip {
                label,
                reason: ResumeSkipReason::OverCap,
            });
            continue;
        }
        // The pane runs the supervised exec wrapper, not the agent CLI
        // directly: every agent launch funnels through `rimz agents exec`,
        // which replays the durable launch identity, applies trusted
        // `[[agents]]` env and the adapter's launch pins before spawning the
        // resume argv.
        let command = resume_command(rimz_bin, agent);
        let tab_label = channel_label(agent.channel.as_deref(), &cwd);
        let identity = resume_tab_identity(agent.channel.as_deref(), &cwd);
        if let Some(tab) = tabs.iter_mut().find(|tab| tab.identity == identity) {
            tab.tab.panes.push(command);
        } else {
            tabs.push(PlannedResumeTab {
                identity,
                tab: ResumeTab {
                    label: tab_label,
                    cwd,
                    panes: vec![command],
                },
            });
        }
    }
    disambiguate_resume_tab_labels(&mut tabs);
    plan.tabs = tabs.into_iter().map(|planned| planned.tab).collect();
    plan
}

fn resume_tab_identity(channel: Option<&str>, cwd: &Path) -> ResumeTabIdentity {
    match channel.filter(|channel| !channel.is_empty()) {
        Some(channel) => ResumeTabIdentity::Channel(channel.to_owned()),
        None => ResumeTabIdentity::Cwd(cwd.to_path_buf()),
    }
}

fn disambiguate_resume_tab_labels(tabs: &mut [PlannedResumeTab]) {
    let mut label_counts = BTreeMap::new();
    for planned in tabs.iter() {
        *label_counts.entry(planned.tab.label.clone()).or_insert(0) += 1;
    }
    let relabel: BTreeSet<usize> = tabs
        .iter()
        .enumerate()
        .filter(|(_, planned)| label_counts[&planned.tab.label] > 1)
        .filter(|(_, planned)| matches!(planned.identity, ResumeTabIdentity::Cwd(_)))
        .map(|(index, _)| index)
        .collect();
    if relabel.is_empty() {
        return;
    }

    let mut used: HashSet<String> = tabs
        .iter()
        .enumerate()
        .filter(|(index, _)| !relabel.contains(index))
        .map(|(_, planned)| planned.tab.label.clone())
        .collect();
    for index in relabel {
        let base = parent_prefixed_label(&tabs[index].tab.cwd)
            .unwrap_or_else(|| tabs[index].tab.label.clone());
        tabs[index].tab.label = unique_label(&base, &mut used);
    }
}

fn parent_prefixed_label(cwd: &Path) -> Option<String> {
    let child = cwd.file_name()?.to_string_lossy();
    let parent = cwd.parent()?.file_name()?.to_string_lossy();
    Some(format!("#{parent}/{child}"))
}

fn unique_label(base: &str, used: &mut HashSet<String>) -> String {
    if used.insert(base.to_owned()) {
        return base.to_owned();
    }
    for ordinal in 2.. {
        let candidate = format!("{base}-{ordinal}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("unbounded ordinal search always yields a fresh label")
}

fn resume_command(rimz_bin: &Path, agent: &AgentState) -> Vec<String> {
    let mut command = vec![
        rimz_bin.to_string_lossy().into_owned(),
        "agents".to_owned(),
        "exec".to_owned(),
        agent.kind.as_str().to_owned(),
        "--resume".to_owned(),
        agent.agent_id.as_str().to_owned(),
        "--close-pane-on-exit".to_owned(),
    ];
    if let Some(name) = agent.name.as_deref() {
        command.extend(["--agent-name".to_owned(), name.to_owned()]);
    }
    if let Some(profile) = agent.profile.as_deref() {
        command.extend(["--agent-profile".to_owned(), profile.to_owned()]);
    }
    if let Some(role) = agent.role.as_deref() {
        command.extend(["--agent-role".to_owned(), role.to_owned()]);
    }
    if let Some(team) = agent.team.as_deref() {
        command.extend(["--agent-team".to_owned(), team.to_owned()]);
    }
    if let Some(channel) = agent.channel.as_deref() {
        command.extend(["--agent-channel".to_owned(), channel.to_owned()]);
    }
    command
}

/// A short, view-safe label for a resumed agent: `kind:branch`, falling back to
/// the worktree directory name, then `kind:agent`. Used in skip reports and
/// legacy per-agent tab title fallbacks.
pub fn build_label(
    kind: &str,
    channel: Option<&str>,
    branch: Option<&str>,
    worktree: &Path,
) -> String {
    format!("{kind}:{}", channel_short(channel, branch, worktree))
}

/// A short, view-safe channel name: branch, then worktree directory, then
/// `agent`.
pub fn channel_short(channel: Option<&str>, branch: Option<&str>, worktree: &Path) -> String {
    channel
        .filter(|channel| !channel.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            branch
                .filter(|branch| !branch.is_empty())
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            worktree
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "agent".to_owned())
}

/// A channel tab label from the worktree directory name, matching live
/// worktree-launch tabs. A main-repo non-worktree agent falls back to
/// `#<repo-name>` rather than the live `kind:repo` title because resume groups by
/// cwd.
pub fn channel_label(channel: Option<&str>, worktree: &Path) -> String {
    format!("#{}", channel_short(channel, None, worktree))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentStatus;
    use crate::agents::TurnPhase;
    use crate::ids::{MuxName, PaneId};
    use crate::pane::PaneRef;
    use jiff::Timestamp;

    fn pane(raw: &str) -> PaneRef {
        PaneRef {
            pane_id: PaneId::from_parts(MuxName::Zellij, raw),
            session_name: String::new(),
            view_id: None,
            view_kind: None,
            view_name: None,
            is_focused: false,
            is_floating: false,
            command: None,
            spawn_command: None,
            cwd: None,
            pane_pid: None,
            pane_process_start: None,
            hosted_agent_kind: None,
            hosted_agent_process_start: None,
            resumed_session_id: None,
            elevated_agent: None,
            first_seen_at_ms: None,
        }
    }

    /// A root agent bound to a pane, active `secs_ago` seconds back.
    fn agent(
        kind: &str,
        id: &str,
        worktree: &str,
        branch: Option<&str>,
        secs_ago: i64,
    ) -> AgentState {
        let when = Timestamp::now() - std::time::Duration::from_secs(secs_ago.max(0) as u64);
        AgentState {
            agent_id: id.into(),
            kind: AgentKind::new_unchecked(kind),
            name: None,
            kind_ordinal: None,
            profile: None,
            role: None,
            team: None,
            channel: None,
            status: AgentStatus::Idle,
            phase: TurnPhase::Idle,
            pane: Some(pane(&format!("terminal_{id}"))),
            agent_pid: None,
            agent_process_start: None,
            runtime_owner: None,
            parent_agent_id: None,
            worktree_path: Some(worktree.to_owned()),
            worktree_branch: branch.map(ToOwned::to_owned),
            task: None,
            prompt: None,
            description: None,
            transcript_path: None,
            origin: None,
            recent_prompts: Vec::new(),
            model: None,
            effort: None,
            context_pct: None,
            context_window: None,
            total_tokens: None,
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
            context: None,
            subagent_description: None,
            subagent_started_at: None,
            turn_started_at: None,
            compacting_since: None,
            compaction_count: 0,
            last_compact_command_tokens: None,
            last_seen: when,
            last_activity: when,
            registered_at: Some(when),
        }
    }

    /// As [`agent`], but stamped on an explicit pane id so a test can model two
    /// sessions sharing one pane (a relaunch in place) rather than the default
    /// one-pane-per-id.
    fn agent_on_pane(
        kind: &str,
        id: &str,
        worktree: &str,
        branch: Option<&str>,
        secs_ago: i64,
        pane_raw: &str,
    ) -> AgentState {
        let mut agent = agent(kind, id, worktree, branch, secs_ago);
        agent.pane = Some(pane(pane_raw));
        agent
    }

    fn no_ended() -> BTreeSet<(AgentKind, AgentSessionId)> {
        BTreeSet::new()
    }

    fn exec_resume(kind: &str, id: &str) -> Vec<String> {
        vec![
            "/bin/rimz".to_owned(),
            "agents".to_owned(),
            "exec".to_owned(),
            kind.to_owned(),
            "--resume".to_owned(),
            id.to_owned(),
            "--close-pane-on-exit".to_owned(),
        ]
    }

    #[test]
    fn resumes_root_agents_most_recent_first() {
        let agents = vec![
            agent("codex", "c1", "/code/query-engine", Some("main"), 30),
            agent(
                "claude",
                "a1",
                "/code/qe-feature",
                Some("feature-migration"),
                5,
            ),
        ];
        let plan = plan_resume(
            &agents,
            &no_ended(),
            DEFAULT_RESUME_MAX,
            |_| true,
            Path::new("/bin/rimz"),
        );
        assert!(plan.skipped.is_empty());
        assert_eq!(plan.tabs.len(), 2);
        // Most-recently-active leads (the focus target).
        assert_eq!(plan.tabs[0].label, "#qe-feature");
        // Wrapper argv: the pane funnels through `rimz agents exec`, which
        // injects launch env before spawning the adapter's resume argv.
        assert_eq!(plan.tabs[0].panes, vec![exec_resume("claude", "a1")]);
        assert_eq!(plan.tabs[0].cwd, PathBuf::from("/code/qe-feature"));
        assert_eq!(plan.tabs[1].label, "#query-engine");
        assert_eq!(plan.tabs[1].panes, vec![exec_resume("codex", "c1")]);
    }

    #[test]
    fn disambiguates_reborn_tabs_with_the_same_basename() {
        let agents = vec![
            agent("claude", "a1", "/work/repoA/main", None, 5),
            agent("codex", "c1", "/work/repoB/main", None, 9),
        ];
        let plan = plan_resume(
            &agents,
            &no_ended(),
            DEFAULT_RESUME_MAX,
            |_| true,
            Path::new("/bin/rimz"),
        );

        assert_eq!(plan.tabs.len(), 2);
        assert_eq!(plan.tabs[0].cwd, PathBuf::from("/work/repoA/main"));
        assert_eq!(plan.tabs[0].label, "#repoA/main");
        assert_eq!(plan.tabs[0].panes, vec![exec_resume("claude", "a1")]);
        assert_eq!(plan.tabs[1].cwd, PathBuf::from("/work/repoB/main"));
        assert_eq!(plan.tabs[1].label, "#repoB/main");
        assert_eq!(plan.tabs[1].panes, vec![exec_resume("codex", "c1")]);
    }

    #[test]
    fn resume_command_replays_launch_identity() {
        // A reborn agent re-stamps its durable launch identity, so it answers
        // to `@<profile>` and `@<role>` again after a mux rebirth.
        let mut agent = agent("claude", "a1", "/code/qe", Some("main"), 1);
        agent.name = Some("swift-otter".to_owned());
        agent.profile = Some("claude-planner".to_owned());
        agent.role = Some("planner".to_owned());
        agent.team = Some("pcr".to_owned());
        assert_eq!(
            resume_command(Path::new("/bin/rimz"), &agent),
            vec![
                "/bin/rimz",
                "agents",
                "exec",
                "claude",
                "--resume",
                "a1",
                "--close-pane-on-exit",
                "--agent-name",
                "swift-otter",
                "--agent-profile",
                "claude-planner",
                "--agent-role",
                "planner",
                "--agent-team",
                "pcr",
            ]
        );
    }

    #[test]
    fn skips_subagents() {
        let mut child = agent("claude", "kid", "/code/query-engine", Some("main"), 1);
        child.parent_agent_id = Some("parent".into());
        let plan = plan_resume(
            &[child],
            &no_ended(),
            DEFAULT_RESUME_MAX,
            |_| true,
            Path::new("/bin/rimz"),
        );
        assert!(plan.is_empty());
        assert!(plan.skipped.is_empty());
    }

    #[test]
    fn skips_paneless_agents() {
        // A `None` pane is both a subagent/ghost with no presence and the shape
        // a rebirth boundary leaves behind for an agent that was not live in the
        // dying incarnation — neither is resumed.
        let mut paneless = agent("claude", "a1", "/code/query-engine", Some("main"), 1);
        paneless.pane = None;
        let plan = plan_resume(
            &[paneless],
            &no_ended(),
            DEFAULT_RESUME_MAX,
            |_| true,
            Path::new("/bin/rimz"),
        );
        assert!(plan.is_empty());
    }

    #[test]
    fn skips_cleanly_ended_sessions() {
        let agents = vec![agent("claude", "a1", "/code/query-engine", Some("main"), 1)];
        let ended: BTreeSet<(AgentKind, AgentSessionId)> =
            [(AgentKind::new_unchecked("claude"), "a1".into())]
                .into_iter()
                .collect();
        let plan = plan_resume(
            &agents,
            &ended,
            DEFAULT_RESUME_MAX,
            |_| true,
            Path::new("/bin/rimz"),
        );
        assert!(plan.is_empty());
    }

    #[test]
    fn tombstones_a_missing_worktree() {
        let agents = vec![agent("claude", "a1", "/code/gone", Some("dead-branch"), 1)];
        let plan = plan_resume(
            &agents,
            &no_ended(),
            DEFAULT_RESUME_MAX,
            |_| false,
            Path::new("/bin/rimz"),
        );
        assert!(plan.tabs.is_empty());
        assert!(plan.skipped.is_empty());
        assert_eq!(
            plan.tombstone,
            vec![(AgentKind::new_unchecked("claude"), "a1".into())]
        );
    }

    #[test]
    fn dedups_a_relaunched_agent_keeping_the_newest() {
        // A relaunch in place re-uses the same pane id; the older stamp is
        // superseded by the newest, exactly as the live sidebar binds the pane.
        let agents = vec![
            agent_on_pane(
                "claude",
                "old",
                "/code/query-engine",
                Some("main"),
                60,
                "terminal_4",
            ),
            agent_on_pane(
                "claude",
                "new",
                "/code/query-engine",
                Some("main"),
                2,
                "terminal_4",
            ),
        ];
        let plan = plan_resume(
            &agents,
            &no_ended(),
            DEFAULT_RESUME_MAX,
            |_| true,
            Path::new("/bin/rimz"),
        );
        assert_eq!(plan.tabs.len(), 1);
        assert_eq!(plan.tabs[0].panes, vec![exec_resume("claude", "new")]);
        // The superseded relaunch is dropped silently, not reported as a skip.
        assert!(plan.skipped.is_empty());
    }

    #[test]
    fn collapses_a_relaunch_that_changed_branch_on_one_pane() {
        // Same pane, a branch checkout between the two sessions. The pane is the
        // identity, so the differing branch must not leak a second resume pane —
        // the `(kind, worktree, branch)` key used to double this.
        let agents = vec![
            agent_on_pane(
                "claude",
                "old",
                "/code/query-engine",
                Some("main"),
                60,
                "terminal_4",
            ),
            agent_on_pane(
                "claude",
                "new",
                "/code/query-engine",
                Some("feature"),
                2,
                "terminal_4",
            ),
        ];
        let plan = plan_resume(
            &agents,
            &no_ended(),
            DEFAULT_RESUME_MAX,
            |_| true,
            Path::new("/bin/rimz"),
        );
        assert_eq!(plan.tabs.len(), 1);
        assert_eq!(plan.tabs[0].panes, vec![exec_resume("claude", "new")]);
    }

    #[test]
    fn keeps_two_kinds_in_one_worktree() {
        let agents = vec![
            agent("claude", "a1", "/code/query-engine", Some("main"), 5),
            agent("codex", "c1", "/code/query-engine", Some("main"), 5),
        ];
        let plan = plan_resume(
            &agents,
            &no_ended(),
            DEFAULT_RESUME_MAX,
            |_| true,
            Path::new("/bin/rimz"),
        );
        assert_eq!(plan.tabs.len(), 1);
        assert_eq!(plan.tabs[0].label, "#query-engine");
        assert_eq!(plan.tabs[0].panes.len(), 2);
    }

    #[test]
    fn keeps_two_same_kind_agents_in_one_worktree() {
        // Two Claude sessions running side by side in one worktree — distinct
        // panes, so each is its own live agent. The `(kind, worktree, branch)`
        // key used to collapse them to one; pane identity keeps both.
        let agents = vec![
            agent_on_pane(
                "claude",
                "a1",
                "/code/query-engine",
                Some("main"),
                5,
                "terminal_4",
            ),
            agent_on_pane(
                "claude",
                "a2",
                "/code/query-engine",
                Some("main"),
                9,
                "terminal_5",
            ),
        ];
        let plan = plan_resume(
            &agents,
            &no_ended(),
            DEFAULT_RESUME_MAX,
            |_| true,
            Path::new("/bin/rimz"),
        );
        assert_eq!(plan.tabs.len(), 1);
        // Freshest leads within the tab; both sessions are resumed.
        assert_eq!(
            plan.tabs[0].panes,
            vec![exec_resume("claude", "a1"), exec_resume("claude", "a2")]
        );
    }

    #[test]
    fn caps_and_reports_the_overflow() {
        let agents = vec![
            agent("claude", "a1", "/code/wt-1", Some("b1"), 5),
            agent("claude", "a2", "/code/wt-2", Some("b2"), 10),
        ];
        let plan = plan_resume(&agents, &no_ended(), 1, |_| true, Path::new("/bin/rimz"));
        assert_eq!(plan.tabs.len(), 1);
        // The freshest survives the cap; the older overflows.
        assert_eq!(plan.tabs[0].panes, vec![exec_resume("claude", "a1")]);
        assert_eq!(plan.skipped.len(), 1);
        assert_eq!(plan.skipped[0].reason, ResumeSkipReason::OverCap);
    }

    #[test]
    fn empty_when_no_agents() {
        let plan = plan_resume(
            &[],
            &no_ended(),
            DEFAULT_RESUME_MAX,
            |_| true,
            Path::new("/bin/rimz"),
        );
        assert!(plan.is_empty());
        assert!(plan.skipped.is_empty());
    }

    #[test]
    fn labels_fall_back_to_the_worktree_dir_without_a_branch() {
        let agents = vec![agent("codex", "c1", "/code/query-engine", None, 1)];
        let plan = plan_resume(
            &agents,
            &no_ended(),
            DEFAULT_RESUME_MAX,
            |_| true,
            Path::new("/bin/rimz"),
        );
        assert_eq!(plan.tabs[0].label, "#query-engine");
        assert_eq!(
            build_label("codex", None, None, Path::new("/code/query-engine")),
            "codex:query-engine"
        );
    }

    #[test]
    fn named_channel_groups_by_explicit_channel_and_replays_identity() {
        let mut design = agent("codex", "c1", "/code/query-engine", Some("main"), 1);
        design.channel = Some("design".to_owned());
        let plan = plan_resume(
            &[design],
            &no_ended(),
            DEFAULT_RESUME_MAX,
            |_| true,
            Path::new("/bin/rimz"),
        );

        assert_eq!(plan.tabs[0].label, "#design");
        assert_eq!(
            build_label(
                "codex",
                Some("design"),
                Some("main"),
                Path::new("/code/query-engine")
            ),
            "codex:design"
        );
        assert!(
            plan.tabs[0].panes[0].windows(2).any(|pair| {
                pair[0].as_str() == "--agent-channel" && pair[1].as_str() == "design"
            }),
            "resume argv re-stamps the named channel: {:?}",
            plan.tabs[0].panes[0]
        );
    }
}
