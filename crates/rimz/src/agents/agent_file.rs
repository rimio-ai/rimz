//! Provider agent files: a fresh launch's composed system prompt, launch reminder, tools, and
//! MCP servers written into a file the provider selects by name. The adapter renders the file
//! ([`LaunchCapability::agent_file`](super::capabilities::LaunchCapability::agent_file)); this
//! module publishes it and prunes the old ones beside it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// A file older than this is pruned when a launch publishes beside it. A startup retry
/// replays argv without publishing, and a provider can reread the file after `/clear`.
const PRUNE_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// What an adapter renders an agent file from.
pub struct AgentFileInput<'a> {
    pub profile: Option<&'a str>,
    pub prompt: &'a str,
    pub reminder: &'a str,
    /// The definition's tools as canonical base names; `None` when it lists none.
    pub tools: Option<&'a [String]>,
    pub native_subagents: bool,
    /// Whether the provider keeps its own question tool: false where the launch reaches
    /// `rimz-ask`.
    pub native_ask: bool,
    /// The launch env, where the launched provider resolves its home.
    pub env: &'a BTreeMap<String, String>,
    /// RimZ's MCP server when the launch hands it over, and the `mcps` list over the user's
    /// own servers, which a provider whose agent file names its servers applies there.
    pub mcp: super::capabilities::McpLaunch<'a>,
}

/// A planned agent file: where it goes, the name the provider selects it by, and its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentFile {
    pub path: PathBuf,
    pub name: String,
    pub bytes: Vec<u8>,
    /// Each mark of the launch's `mcps` list the file cannot apply.
    pub unapplied: Vec<super::capabilities::UnappliedMark>,
}

/// Why the provider could not read a planned agent file, and the command that fixes it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{reason}; run `{fix}`")]
pub struct AgentFileRefusal {
    pub reason: String,
    pub fix: String,
}

/// Write the file (a fresh mtime even when its bytes are unchanged), then remove
/// `rimz-*.md` files beside it older than seven days.
pub fn publish_agent_file(file: &AgentFile) -> crate::disk::atomic::Result<()> {
    // A planned agent file always has a parent: it is joined under the provider's dir.
    let dir = file.path.parent().expect("agent file has a parent dir");
    std::fs::create_dir_all(dir).map_err(|source| crate::disk::atomic::AtomicErr::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    super::provider_file::write_bytes(&file.path, &file.bytes)?;
    prune(dir, SystemTime::now());
    Ok(())
}

fn prune(dir: &Path, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with("rimz-") || !name.ends_with(".md") {
            continue;
        }
        let stale = entry.metadata().is_ok_and(|metadata| {
            metadata.is_file()
                && metadata.modified().is_ok_and(|modified| {
                    now.duration_since(modified).unwrap_or_default() > PRUNE_AGE
                })
        });
        if stale && let Err(error) = std::fs::remove_file(entry.path()) {
            tracing::debug!(%error, path = %entry.path().display(), "agent file prune skipped");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aged(path: &Path, age: Duration) {
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::now() - age).unwrap();
    }

    #[test]
    fn publish_writes_through_a_linked_dir_and_prunes_only_old_rimz_files() {
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("real");
        std::fs::create_dir_all(real.join("agents")).unwrap();
        let plugin = root.path().join("plugin");
        std::os::unix::fs::symlink(&real, &plugin).unwrap();
        let old = real.join("agents/rimz-old-000000000000.md");
        let young = real.join("agents/rimz-young-000000000000.md");
        let foreign = real.join("agents/mine.md");
        let old_dir = real.join("agents/rimz-dir.md");
        for path in [&old, &young, &foreign] {
            std::fs::write(path, "x").unwrap();
        }
        std::fs::create_dir(&old_dir).unwrap();
        let week = Duration::from_secs(8 * 24 * 60 * 60);
        aged(&old, week);
        aged(&young, Duration::from_secs(6 * 24 * 60 * 60));
        aged(&foreign, week);
        let file = AgentFile {
            path: plugin.join("agents/rimz-new-111111111111.md"),
            name: "rimz-new-111111111111".to_owned(),
            bytes: b"---\nname: rimz-new-111111111111\n---\nBody.\n".to_vec(),
            unapplied: Vec::new(),
        };
        publish_agent_file(&file).unwrap();
        let written = real.join("agents/rimz-new-111111111111.md");
        assert_eq!(std::fs::read(&written).unwrap(), file.bytes);
        assert!(!old.exists());
        assert!(young.exists() && foreign.exists() && old_dir.is_dir());
        aged(&written, Duration::from_secs(60));
        publish_agent_file(&file).unwrap();
        assert_eq!(std::fs::read(&written).unwrap(), file.bytes);
        let mtime = std::fs::metadata(&written).unwrap().modified().unwrap();
        assert!(SystemTime::now().duration_since(mtime).unwrap() < Duration::from_secs(30));
    }

    #[test]
    fn publish_creates_the_agents_dir() {
        let root = tempfile::tempdir().unwrap();
        let file = AgentFile {
            path: root.path().join("plugin/agents/rimz-a-000000000000.md"),
            name: "rimz-a-000000000000".to_owned(),
            bytes: b"x".to_vec(),
            unapplied: Vec::new(),
        };
        publish_agent_file(&file).unwrap();
        assert_eq!(std::fs::read(&file.path).unwrap(), b"x");
    }
}
