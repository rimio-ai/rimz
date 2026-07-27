//! Read-only dynamic shell-completion candidates.

use clap::builder::StyledStr;
use clap_complete::CompletionCandidate;

use rimz::agents::AgentState;
use rimz::config::MachineConfig;
use rimz::workspace::{ResolvedWorkspace, WorkspaceResolver};
use rimz::{RuntimePaths, StatePaths, Store};

struct RoomContext {
    workspace: ResolvedWorkspace,
    store: Store,
    snapshot: rimz::SidebarSnapshot,
}

fn candidate(value: impl Into<std::ffi::OsString>, help: impl Into<String>) -> CompletionCandidate {
    CompletionCandidate::new(value).help(Some(StyledStr::from(help.into())))
}

fn room_context() -> Option<RoomContext> {
    let workspace = WorkspaceResolver::resolve_participant(".", None).ok()?;
    let paths = StatePaths::for_workspace(workspace.workspace_id.clone()).ok()?;
    let runtime = RuntimePaths::for_workspace(workspace.workspace_id.clone()).ok()?;
    room_context_from(workspace, paths, runtime)
}

fn room_context_from(
    workspace: ResolvedWorkspace,
    paths: StatePaths,
    runtime: RuntimePaths,
) -> Option<RoomContext> {
    let store = Store::open_existing(paths, runtime.clone())?;
    let snapshot = rimz::sidebar::consumer::reap_cached_daemon_sessions(
        store.snapshot_cached().ok()?,
        &runtime,
        &workspace.session_name,
    );
    Some(RoomContext {
        workspace,
        store,
        snapshot,
    })
}

pub(crate) fn handles() -> Vec<CompletionCandidate> {
    let Some(context) = room_context() else {
        return Vec::new();
    };
    handles_from_agents(&context.snapshot.agents)
}

fn handles_from_agents(agents: &[AgentState]) -> Vec<CompletionCandidate> {
    let peers: Vec<_> = agents
        .iter()
        .filter(|agent| !agent.is_provider_subagent())
        .collect();
    let unqualified: Vec<_> = peers
        .iter()
        .map(|agent| rimz::harness::target::agent_handle(agent, &peers, false))
        .collect();
    let mut candidates: Vec<_> = peers
        .iter()
        .zip(&unqualified)
        .map(|(agent, base)| {
            let collides = unqualified.iter().filter(|handle| *handle == base).count() > 1;
            candidate(
                if collides {
                    rimz::harness::target::agent_handle(agent, &peers, true)
                } else {
                    base.clone()
                },
                format!("{} · {}", agent.kind, agent.effective_status().as_str()),
            )
        })
        .collect();
    candidates.push(candidate("@all", "every live agent"));
    candidates
}

pub(crate) fn message_targets() -> Vec<CompletionCandidate> {
    handles()
}

pub(crate) fn queued_message_ids() -> Vec<CompletionCandidate> {
    let Some(context) = room_context() else {
        return Vec::new();
    };
    context
        .store
        .list_pending_messages()
        .unwrap_or_default()
        .into_iter()
        .map(|message| {
            candidate(
                message.message_id.to_string(),
                format!(
                    "{} · {}",
                    message_target(&message),
                    text_snippet(&message.text)
                ),
            )
        })
        .collect()
}

pub(crate) fn all_message_ids() -> Vec<CompletionCandidate> {
    let Some(context) = room_context() else {
        return Vec::new();
    };
    let mut messages = context.store.list_messages().unwrap_or_default();
    messages.extend(context.store.list_message_history().unwrap_or_default());
    messages.sort_by_key(|message| message.message_id.to_string());
    messages.dedup_by(|left, right| left.message_id == right.message_id);
    messages
        .into_iter()
        .map(|message| {
            candidate(
                message.message_id.to_string(),
                format!("{} · {}", message_target(&message), message.status),
            )
        })
        .collect()
}

fn message_target(message: &rimz::message::MessageRecord) -> String {
    message
        .address
        .clone()
        .or_else(|| message.agent_name.as_ref().map(|name| format!("@{name}")))
        .unwrap_or_else(|| format!("@{}", message.kind))
}

fn text_snippet(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = compact.chars();
    let snippet: String = chars.by_ref().take(48).collect();
    if chars.next().is_some() {
        format!("{snippet}...")
    } else {
        snippet
    }
}

pub(crate) fn loop_tasks() -> Vec<CompletionCandidate> {
    let project_root = WorkspaceResolver::resolve(".", None)
        .ok()
        .map(|workspace| workspace.project_root);
    rimz::harness::schedule::catalog::TaskCatalog::load_lenient(project_root.as_deref())
        .visible()
        .iter()
        .map(|(name, task)| {
            let help = task
                .schedule()
                .as_ref()
                .map(|schedule| schedule.describe())
                .unwrap_or_else(|error| format!("invalid: {error}"));
            candidate(name, help)
        })
        .collect()
}

pub(crate) fn agent_specs() -> Vec<CompletionCandidate> {
    agent_specs_from(&MachineConfig::load_lenient())
}

pub(crate) fn team_names() -> Vec<CompletionCandidate> {
    team_names_from(&MachineConfig::load_lenient())
}

fn team_names_from(config: &MachineConfig) -> Vec<CompletionCandidate> {
    config
        .agents
        .teams
        .0
        .keys()
        .map(|name| candidate(name, "team"))
        .collect()
}

fn agent_specs_from(config: &MachineConfig) -> Vec<CompletionCandidate> {
    let mut candidates = Vec::new();
    let mut suffix_bases = Vec::new();
    for kind in rimz::agents::known_kinds() {
        candidates.push(candidate(kind, "agent kind"));
        suffix_bases.push(kind.to_owned());
    }
    for (name, profile) in &config.agents.profiles.0 {
        candidates.push(candidate(name, format!("profile · {}", profile.agent)));
        suffix_bases.push(name.clone());
    }
    candidates.extend(
        config
            .agents
            .commands
            .0
            .keys()
            .map(|name| candidate(name, "launch command")),
    );
    for (team_name, team) in &config.agents.teams.0 {
        candidates.push(candidate(team_name, "team"));
        candidates.extend(team.roles.iter().map(|role| {
            candidate(
                format!("{team_name}.{}", role.role),
                format!("team role · {}", role.profile),
            )
        }));
    }
    for base in suffix_bases {
        for mode in ["auto", "ask", "plan", "yolo"] {
            candidates.push(candidate(format!("{base}-{mode}"), "permission mode").hide(true));
        }
    }
    candidates
}

pub(crate) fn agent_refs() -> Vec<CompletionCandidate> {
    let Some(context) = room_context() else {
        return Vec::new();
    };
    let mut candidates = handles_from_agents(&context.snapshot.agents);
    candidates.extend(
        rimz::harness::run::list(context.store.paths())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|run| {
                let name = run.agent_name?;
                Some(candidate(
                    name,
                    format!("run · {:?}", run.status).to_lowercase(),
                ))
            }),
    );
    candidates
}

pub(crate) fn pane_targets() -> Vec<CompletionCandidate> {
    let mut candidates = vec![candidate("sidebar", "RimZ sidebar pane")];
    let Some(context) = room_context() else {
        return candidates;
    };
    candidates.extend(handles_from_agents(&context.snapshot.agents));
    if let Some(frame) = rimz::sidebar::cache::read_snapshot_cache(
        &context.store.runtime_paths().pane_frame_path(),
        &context.workspace.session_name,
    ) {
        candidates.extend(
            frame
                .to_pane_refs()
                .into_iter()
                .map(|pane| CompletionCandidate::new(pane.pane_id.to_string())),
        );
    }
    candidates
}

pub(crate) fn worktrees() -> Vec<CompletionCandidate> {
    let Some(context) = room_context() else {
        return Vec::new();
    };
    worktrees_from(&context)
}

fn worktrees_from(context: &RoomContext) -> Vec<CompletionCandidate> {
    rimz::worktree::discover_owned(&context.workspace.project_root)
        .unwrap_or_default()
        .into_iter()
        .map(|worktree| {
            candidate(
                worktree.marker.name,
                worktree.branch.unwrap_or_else(|| "detached".to_owned()),
            )
        })
        .collect()
}

pub(crate) fn channels() -> Vec<CompletionCandidate> {
    let Some(context) = room_context() else {
        return Vec::new();
    };
    channels_from(&context)
}

fn channels_from(context: &RoomContext) -> Vec<CompletionCandidate> {
    rimz::channel::list(&context.store.paths().channels_record)
        .unwrap_or_default()
        .into_iter()
        .map(|channel| CompletionCandidate::new(channel.name))
        .collect()
}

pub(crate) fn scope_names() -> Vec<CompletionCandidate> {
    let Some(context) = room_context() else {
        return Vec::new();
    };
    let mut candidates = worktrees_from(&context);
    for channel in channels_from(&context) {
        let name = channel.get_value().to_string_lossy();
        candidates.push(CompletionCandidate::new(name.as_ref()));
        candidates.push(CompletionCandidate::new(format!("#{name}")));
    }
    candidates
}

pub(crate) fn transcript_targets() -> Vec<CompletionCandidate> {
    let Some(context) = room_context() else {
        return Vec::new();
    };
    let mut candidates = handles_from_agents(&context.snapshot.agents);
    candidates.extend(channels_from(&context).into_iter().map(|channel| {
        CompletionCandidate::new(format!("#{}", channel.get_value().to_string_lossy()))
    }));
    candidates
}

pub(crate) fn remote_aliases() -> Vec<CompletionCandidate> {
    rimz::remote::aliases::RemoteAliases::load()
        .map(|aliases| {
            aliases
                .entries()
                .iter()
                .map(|alias| candidate(&alias.name, &alias.target))
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn sessions() -> Vec<CompletionCandidate> {
    rimz::workspace::known_workspaces()
        .unwrap_or_default()
        .into_iter()
        .map(|workspace| {
            candidate(
                workspace.session_name,
                workspace.project_root.display().to_string(),
            )
        })
        .collect()
}

pub(crate) fn config_keys() -> Vec<CompletionCandidate> {
    let Ok(value) = MachineConfig::load_lenient().to_toml_value() else {
        return Vec::new();
    };
    let mut leaves = Vec::new();
    collect_config_leaves(&value, "", &mut leaves);
    leaves
        .into_iter()
        .map(|(key, value)| candidate(key, value))
        .collect()
}

fn collect_config_leaves(value: &toml::Value, prefix: &str, leaves: &mut Vec<(String, String)>) {
    if let toml::Value::Table(table) = value {
        for (key, value) in table {
            let path = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            collect_config_leaves(value, &path, leaves);
        }
    } else if !prefix.is_empty() {
        leaves.push((prefix.to_owned(), value.to_string()));
    }
}

pub(crate) fn mux_names() -> Vec<CompletionCandidate> {
    vec![
        candidate("zellij", "Zellij backend"),
        candidate("tmux", "tmux backend"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_include_live_agents_and_broadcast() {
        let mut coder =
            rimz::testkit::agent_state("codex", "sess-coder", jiff::Timestamp::UNIX_EPOCH);
        coder.role = Some("coder".to_owned());
        let candidates = handles_from_agents(&[coder]);
        let values: Vec<_> = candidates
            .iter()
            .map(|candidate| candidate.get_value().to_string_lossy().into_owned())
            .collect();
        assert_eq!(values, ["@coder", "@all"]);
    }

    #[test]
    fn agent_specs_include_configured_names_and_hide_permission_variants() {
        let config: MachineConfig = toml::from_str(
            r#"
            [agents.profiles.writer]
            agent = "claude"

            [agents.teams.forge]
            roles = [{ role = "planner", profile = "writer" }]
            "#,
        )
        .expect("config fixture");
        let candidates = agent_specs_from(&config);
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.get_value() == "writer")
        );
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.get_value() == "forge.planner")
        );
        assert!(candidates.iter().any(|candidate| {
            candidate.get_value() == "writer-yolo" && candidate.is_hide_set()
        }));
        assert_eq!(
            team_names_from(&config)[0].get_value().to_string_lossy(),
            "forge"
        );
    }

    #[test]
    fn missing_store_has_no_room_candidates() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = ResolvedWorkspace {
            workspace_id: rimz::WorkspaceId::from_project_root(dir.path()),
            project_root: dir.path().to_path_buf(),
            root_class: rimz::workspace::RootClass::Directory,
            worktree_root: dir.path().to_path_buf(),
            worktree_branch: None,
            session_name: "rimz-test".to_owned(),
            mux_hint: None,
        };
        let paths =
            StatePaths::under(workspace.workspace_id.clone(), dir.path()).expect("state paths");
        let runtime =
            RuntimePaths::under(workspace.workspace_id.clone(), dir.path()).expect("runtime paths");
        assert!(room_context_from(workspace, paths, runtime).is_none());
    }

    #[test]
    fn config_leaf_walk_emits_dotted_keys() {
        let value: toml::Value =
            toml::from_str("[theme.display]\nmax_cols = 3\n").expect("config fixture");
        let mut leaves = Vec::new();
        collect_config_leaves(&value, "", &mut leaves);
        assert_eq!(
            leaves,
            [("theme.display.max_cols".to_owned(), "3".to_owned())]
        );
    }
}
