//! Read-only temp-unit inventory and the shared owner-set hold ladder.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::agents::AgentState;
use crate::store::snapshot::AgentRollup;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TmpHold {
    Live,
    Open,
    Ended(jiff::Timestamp),
    Unowned,
}

#[derive(Clone, Debug)]
pub struct TmpUnit {
    pub handle: String,
    pub path: PathBuf,
    pub modified: SystemTime,
    pub hold: TmpHold,
}

impl TmpUnit {
    pub fn past_grace(&self, now: SystemTime, grace: Duration) -> bool {
        let expired = |at| now.duration_since(at).is_ok_and(|age| age > grace);
        expired(self.modified)
            && match self.hold {
                TmpHold::Unowned => true,
                TmpHold::Ended(ended) => expired(ended.into()),
                TmpHold::Live | TmpHold::Open => false,
            }
    }
}

pub fn tmp_inventory(
    paths: &crate::StatePaths,
    agents: &AgentRollup,
    temp_owner: &impl Fn(&AgentState, &AgentRollup) -> Option<String>,
) -> super::Result<Vec<TmpUnit>> {
    let mut holds = BTreeMap::<String, TmpHold>::new();
    for agent in agents.iter().filter(|agent| !agent.is_provider_subagent()) {
        let Some(handle) = temp_owner(agent, agents) else {
            continue;
        };
        let hold = if agent
            .runtime_owner
            .as_ref()
            .is_some_and(crate::store::runtime::owner_is_live)
        {
            TmpHold::Live
        } else {
            agent.ended_at.map_or(TmpHold::Open, TmpHold::Ended)
        };
        holds
            .entry(handle)
            .and_modify(|current| {
                *current = match (*current, hold) {
                    (TmpHold::Live, _) | (_, TmpHold::Live) => TmpHold::Live,
                    (TmpHold::Open, _) | (_, TmpHold::Open) => TmpHold::Open,
                    (TmpHold::Ended(a), TmpHold::Ended(b)) => TmpHold::Ended(a.max(b)),
                    (TmpHold::Unowned, hold) | (hold, TmpHold::Unowned) => hold,
                };
            })
            .or_insert(hold);
    }
    let unnamed = paths.temp_unit_dir(None);
    let mut units = Vec::new();
    for entry in super::read_dir_if_exists(&paths.tmp_dir)?
        .into_iter()
        .flatten()
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(path = %paths.tmp_dir.display(), %error, "tmp inventory skipped unreadable entry");
                continue;
            }
        };
        let path = entry.path();
        if path == unnamed {
            continue;
        }
        let Some(handle) = entry.file_name().to_str().map(str::to_owned) else {
            tracing::warn!(path = %path.display(), "tmp inventory skipped non-UTF-8 handle");
            continue;
        };
        match unit_modified(&path) {
            Ok(Some(modified)) => units.push(TmpUnit {
                hold: holds.get(&handle).copied().unwrap_or(TmpHold::Unowned),
                handle,
                path,
                modified,
            }),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "tmp inventory skipped unreadable unit")
            }
        }
    }
    Ok(units)
}

fn unit_modified(path: &Path) -> std::io::Result<Option<SystemTime>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        return Ok(None);
    }
    let mut modified = metadata.modified()?;
    for child in fs::read_dir(path)? {
        modified = modified.max(fs::symlink_metadata(child?.path())?.modified()?);
    }
    Ok(Some(modified))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn inventory_uses_all_owners_and_excludes_native_subagents_and_unnamed() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::StatePaths::under(
            crate::WorkspaceId::from_project_root(temp.path()),
            temp.path(),
        )
        .unwrap();
        for name in [
            Some("ended"),
            Some("open"),
            Some("live"),
            Some("orphan"),
            Some("native"),
            None,
        ] {
            fs::create_dir_all(paths.temp_unit_dir(name)).unwrap();
        }
        let ended_dir = paths.temp_unit_dir(Some("ended"));
        let note = ended_dir.join("note");
        fs::write(&note, b"fresh").unwrap();
        fs::File::open(&ended_dir)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH)
            .unwrap();
        let old = jiff::Timestamp::from_second(1_700_000_000).unwrap();
        let newer = jiff::Timestamp::from_second(1_700_000_001).unwrap();
        let row = |name: &str, ended| {
            let mut row = crate::testkit::agent_state("claude", name, old);
            row.name = Some(name.to_owned());
            row.ended_at = ended;
            row
        };
        let mut child = row("child", Some(newer));
        child.parent_agent_id = Some("ended".into());
        child.launch_depth = Some(1);
        let mut live = row("live", Some(old));
        live.runtime_owner = Some(crate::store::runtime::current_process_owner(
            crate::pane::RuntimeOwnerKind::Agent,
            "live",
        ));
        let mut native = row("native", None);
        native.parent_agent_id = Some("ended".into());
        let rows = vec![
            row("ended", Some(old)),
            row("open", None),
            live,
            native,
            child,
        ]
        .into();
        let inventory = tmp_inventory(&paths, &rows, &|row, _| {
            Some(if row.name.as_deref() == Some("child") {
                "ended".into()
            } else {
                row.name.clone().unwrap()
            })
        })
        .unwrap();
        assert_eq!(inventory.len(), 5);
        for (name, expected) in [
            ("ended", TmpHold::Ended(newer)),
            ("open", TmpHold::Open),
            ("live", TmpHold::Live),
            ("orphan", TmpHold::Unowned),
            ("native", TmpHold::Unowned),
        ] {
            let unit = inventory.iter().find(|unit| unit.handle == name).unwrap();
            assert_eq!(unit.hold, expected);
            assert_eq!(unit.path, paths.temp_unit_dir(Some(name)));
            if name == "ended" {
                assert_eq!(
                    unit.modified,
                    fs::metadata(&note).unwrap().modified().unwrap()
                );
            }
        }
    }

    #[test]
    fn grace_requires_both_clocks_and_never_expires_holds() {
        let now = SystemTime::now();
        let grace = Duration::from_secs(86_400);
        let old = now - Duration::from_secs(25 * 3_600);
        let recent = now - Duration::from_secs(2 * 3_600);
        let mut unit = TmpUnit {
            handle: "fox".into(),
            path: PathBuf::new(),
            modified: old,
            hold: TmpHold::Unowned,
        };
        assert!(unit.past_grace(now, grace));
        unit.hold = TmpHold::Ended(old.try_into().unwrap());
        assert!(unit.past_grace(now, grace));
        assert!(!unit.past_grace(now, Duration::from_secs(7 * 86_400)));
        unit.hold = TmpHold::Ended(recent.try_into().unwrap());
        assert!(!unit.past_grace(now, grace));
        unit.hold = TmpHold::Ended(old.try_into().unwrap());
        for modified in [recent, now - grace, now + grace] {
            unit.modified = modified;
            assert!(!unit.past_grace(now, grace));
        }
        unit.modified = old;
        unit.hold = TmpHold::Ended((now - grace).try_into().unwrap());
        assert!(!unit.past_grace(now, grace));
        for hold in [TmpHold::Live, TmpHold::Open] {
            unit.hold = hold;
            assert!(!unit.past_grace(now, grace));
        }
    }
}
