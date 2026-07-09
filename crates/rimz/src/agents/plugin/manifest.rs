//! Agent plugin manifest schema and validation.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::protocol::CANONICAL_EVENTS;

pub(super) const PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) struct PluginManifest {
    pub protocol: u32,
    pub kind: String,
    pub display_name: String,
    pub process_names: Vec<String>,
    /// Canonical events the agent-side shim emits. Descriptor coverage is
    /// derived from this list, so the published matrix cannot drift.
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default)]
    pub brand: BrandManifest,
    #[serde(default)]
    pub capabilities: CapabilityManifest,
    #[serde(default)]
    pub tools: ToolManifest,
    pub launch: Option<LaunchManifest>,
    pub transcripts: Option<TranscriptManifest>,
    #[serde(default)]
    pub probes: ProbeManifest,
    pub setup_doc: PathBuf,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub(super) struct BrandManifest {
    pub emblem: Option<String>,
    pub color: u8,
    pub color_rgb: [u8; 3],
}

impl Default for BrandManifest {
    fn default() -> Self {
        Self {
            emblem: None,
            color: 141,
            color_rgb: [175, 135, 255],
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub(super) struct CapabilityManifest {
    pub native_ask_ui: bool,
    pub subagents: bool,
    pub background_tasks: bool,
    pub registers_lazily: bool,
    pub context_usage: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub(super) struct ToolManifest {
    pub mutating: Vec<String>,
    pub editing: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) struct LaunchManifest {
    pub bin: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub model_flag: Option<String>,
    pub effort_flag: Option<String>,
    pub resume: Option<Vec<String>>,
    pub compact_command: Option<String>,
    #[serde(default)]
    pub permission_args: PermissionArgs,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub(super) struct PermissionArgs {
    pub ask: Vec<String>,
    pub auto: Vec<String>,
    pub yolo: Vec<String>,
    pub plan: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum TranscriptThreadKey {
    #[default]
    PerFile,
    SessionDir,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) struct TranscriptManifest {
    #[serde(default)]
    pub globs: Vec<String>,
    #[serde(default)]
    pub thread_key: TranscriptThreadKey,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub(super) struct ProbeManifest {
    pub spend: Option<Vec<String>>,
    pub account: Option<Vec<String>>,
    pub version: Option<Vec<String>>,
}

impl PluginManifest {
    pub(super) fn parse(path: &Path, text: &str) -> Result<Self, String> {
        let manifest: Self = toml::from_str(text).map_err(|err| format!("invalid TOML: {err}"))?;
        manifest.validate(path)?;
        Ok(manifest)
    }

    fn validate(&self, path: &Path) -> Result<(), String> {
        if self.protocol != PROTOCOL_VERSION {
            return Err(format!(
                "unsupported protocol {}; expected {PROTOCOL_VERSION}",
                self.protocol
            ));
        }
        if !valid_kind(&self.kind) {
            return Err(
                "kind must match [a-z0-9-]+ and start and end with a letter or digit".into(),
            );
        }
        if super::super::ADAPTERS
            .iter()
            .any(|adapter| adapter.descriptor().kind == self.kind)
        {
            return Err(format!(
                "kind `{}` collides with a built-in agent",
                self.kind
            ));
        }
        let directory_kind = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str());
        if directory_kind != Some(self.kind.as_str()) {
            return Err(format!(
                "kind `{}` must match its agents.d directory name",
                self.kind
            ));
        }
        if self.display_name.trim().is_empty() {
            return Err("display-name must not be empty".into());
        }
        if self.process_names.is_empty()
            || self.process_names.iter().any(|name| name.trim().is_empty())
        {
            return Err("process-names must contain at least one non-empty name".into());
        }
        unique("process-names", &self.process_names)?;
        unique("events", &self.events)?;
        if !self.events.iter().any(|event| event == "session_start") {
            return Err(
                "events must include `session_start` so the plugin can register a session".into(),
            );
        }
        if let Some(event) = self
            .events
            .iter()
            .find(|event| !CANONICAL_EVENTS.contains(&event.as_str()))
        {
            return Err(format!("unknown protocol-1 event `{event}`"));
        }
        unique("tools.mutating", &self.tools.mutating)?;
        unique("tools.editing", &self.tools.editing)?;
        if let Some(tool) = self
            .tools
            .editing
            .iter()
            .find(|tool| !self.tools.mutating.contains(tool))
        {
            return Err(format!(
                "editing tool `{tool}` must also appear in tools.mutating"
            ));
        }
        if let Some(launch) = &self.launch {
            if launch.bin.trim().is_empty() {
                return Err("launch.bin must not be empty".into());
            }
            if let Some(resume) = &launch.resume
                && (resume.is_empty() || !resume.iter().any(|arg| arg.contains("{session_id}")))
            {
                return Err("launch.resume must contain a {session_id} placeholder".into());
            }
            if launch
                .compact_command
                .as_deref()
                .is_some_and(|command| command.trim().is_empty())
            {
                return Err("launch.compact-command must not be empty".into());
            }
        }
        for (name, command) in [
            ("spend", self.probes.spend.as_ref()),
            ("account", self.probes.account.as_ref()),
            ("version", self.probes.version.as_ref()),
        ] {
            if command.is_some_and(|argv| argv.is_empty() || argv[0].trim().is_empty()) {
                return Err(format!("probes.{name} must contain an executable"));
            }
        }
        let plugin_dir = path.parent().unwrap_or_else(|| Path::new("."));
        let setup_doc = resolve_path(plugin_dir, &self.setup_doc);
        if !setup_doc.is_file() {
            return Err(format!(
                "setup-doc `{}` does not exist; add the hook wiring guide or fix the path",
                setup_doc.display()
            ));
        }
        Ok(())
    }
}

pub fn valid_kind(kind: &str) -> bool {
    !kind.is_empty()
        && kind
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && kind
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && kind
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
}

pub(super) fn resolve_path(plugin_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        plugin_dir.join(path)
    }
}

fn unique(label: &str, values: &[String]) -> Result<(), String> {
    let mut seen = HashSet::new();
    if let Some(duplicate) = values.iter().find(|value| !seen.insert(value.as_str())) {
        return Err(format!("{label} contains duplicate `{duplicate}`"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn manifest(kind: &str) -> String {
        format!(
            r#"protocol = 1
kind = "{kind}"
display-name = "Test"
process-names = ["test"]
events = ["session_start", "turn_start", "turn_end"]
setup-doc = "README.md"

[tools]
mutating = ["write"]
editing = ["write"]
"#
        )
    }

    fn parse(text: &str) -> Result<PluginManifest, String> {
        let root = TempDir::new().unwrap();
        let dir = root.path().join("testbot");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("README.md"), "setup").unwrap();
        PluginManifest::parse(&dir.join("agent.toml"), text)
    }

    #[test]
    fn validates_identity_tools_events_and_resume() {
        assert!(parse(&manifest("testbot")).is_ok());
        assert!(
            parse(&manifest("Bad_kind"))
                .unwrap_err()
                .contains("kind must")
        );
        assert!(parse(&manifest("claude")).unwrap_err().contains("built-in"));

        let bad_tools =
            manifest("testbot").replace("editing = [\"write\"]", "editing = [\"edit\"]");
        assert!(parse(&bad_tools).unwrap_err().contains("must also appear"));

        let bad_event = manifest("testbot").replace("turn_end", "future_event");
        assert!(
            parse(&bad_event)
                .unwrap_err()
                .contains("unknown protocol-1 event")
        );

        let bad_resume = format!(
            "{}\n[launch]\nbin = \"test\"\nresume = [\"test\", \"--resume\"]\n",
            manifest("testbot")
        );
        assert!(parse(&bad_resume).unwrap_err().contains("{session_id}"));
    }

    #[test]
    fn malformed_toml_is_structured_error() {
        let err = parse("kind = [").unwrap_err();
        assert!(err.contains("invalid TOML"));
    }
}
