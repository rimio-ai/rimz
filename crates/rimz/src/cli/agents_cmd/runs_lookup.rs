use super::*;

use crate::cli::render;

pub(super) fn newest_run_for_agent(
    store: &rimz::Store,
    agent: &AgentState,
) -> Result<Option<RunRecord>> {
    newest_run_by_ref(store, agent.name.as_deref().unwrap_or(""), Some(agent))
}

pub(super) fn newest_run_by_ref(
    store: &rimz::Store,
    reference: &str,
    agent: Option<&AgentState>,
) -> Result<Option<RunRecord>> {
    let mut records = rimz::harness::run::list(store.paths())?;
    records.retain(|record| {
        if record.peer.is_some()
            && let Some(agent) = agent
        {
            return record.matches_agent(agent);
        }
        if record.run_id.as_str() == reference || record.agent_name.as_deref() == Some(reference) {
            return true;
        }
        if let Some(agent) = agent {
            return record.kind == agent.kind
                && (record.agent_id.as_ref() == Some(&agent.agent_id)
                    || record.agent_name.as_deref() == agent.name.as_deref());
        }
        false
    });
    records.sort_by_key(|record| std::cmp::Reverse(record.started_at));
    Ok(records.into_iter().next())
}

pub(super) fn print_run_line(run: &RunRecord) -> std::io::Result<()> {
    use std::io::Write;
    let status = supervised::output::status_label(run.status);
    writeln!(
        render::out(),
        "{} {} {} {}",
        render::paint(render::palette::muted(), "run:"),
        run.run_id,
        render::paint(render::status::run(run.status), status),
        run.prompt,
    )
}

pub(super) fn agent_name(agent: &AgentState) -> &str {
    agent.name.as_deref().unwrap_or(agent.agent_id.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::agents::AgentStatus;

    #[test]
    fn peer_lookup_requires_identity_when_agent_resolves() {
        let dir = tempfile::tempdir().unwrap();
        let id = rimz::ids::WorkspaceId::from_project_root(dir.path());
        let paths = rimz::disk::paths::StatePaths::under(id.clone(), dir.path()).unwrap();
        let runtime = rimz::disk::paths::RuntimePaths::under(id.clone(), dir.path()).unwrap();
        let store = rimz::Store::open(paths, runtime).unwrap();
        let mut agent = AgentState::stub("codex", "session", AgentStatus::Idle);
        agent.name = Some("peer".into());
        agent.launch_id = Some("launch".into());
        let mut record = RunRecord::new(
            id,
            agent.kind.clone(),
            PermissionMode::Auto,
            "task".into(),
            dir.path().into(),
        );
        record.agent_name = agent.name.clone();
        let mut value = serde_json::to_value(record).unwrap();
        value["peer"] = serde_json::json!({"launch_id": "old-launch"});
        let record: RunRecord = serde_json::from_value(value.clone()).unwrap();
        rimz::harness::run::create(store.paths(), &record).unwrap();
        assert!(
            newest_run_by_ref(&store, "peer", Some(&agent))
                .unwrap()
                .is_none()
        );
        assert!(
            newest_run_by_ref(&store, record.run_id.as_str(), None)
                .unwrap()
                .is_some()
        );
        value["peer"]["launch_id"] = serde_json::json!("launch");
        let mut record: RunRecord = serde_json::from_value(value).unwrap();
        record.agent_name = Some("old-name".into());
        rimz::harness::run::create(store.paths(), &record).unwrap();
        assert_eq!(
            newest_run_for_agent(&store, &agent)
                .unwrap()
                .unwrap()
                .run_id,
            record.run_id
        );
        agent.kind = rimz::ids::AgentKind::new_unchecked("claude");
        assert!(
            newest_run_by_ref(&store, "old-name", Some(&agent))
                .unwrap()
                .is_none()
        );
    }
}
