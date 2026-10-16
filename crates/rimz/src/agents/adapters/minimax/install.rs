//! Whole-directory native MiniMax plugin, published with the manifest last.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{HOOKS, MiniMaxAdapter};
use crate::agents::capabilities::LaunchCapability;
use crate::agents::{
    AgentErr, AgentSpec, HookInstallFilePreview, HookInstallFileReport, HookInstallPreview,
    HookInstallReport, HookUninstallReport, ManagedIntegration, Result, read_optional_file,
};

const AGENT: &str = "minimax";
const COMMAND: &str = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source minimax";
const MANIFEST: &str = ".minimax-plugin/plugin.json";

// Embedded one-pixel PNG; the native manifest requires a real raster icon.
const ICON: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x04\0\0\0\xb5\x1c\x0c\x02\0\0\0\x0bIDAT\x78\xda\x63\xfc\xff\x1f\0\x03\x03\x02\0\xef\xa2\xa7\x5b\0\0\0\0IEND\xae\x42\x60\x82";

pub(super) static INTEGRATION: MiniMaxIntegration = MiniMaxIntegration;

pub(super) struct MiniMaxIntegration;

fn package(env: &BTreeMap<String, String>) -> Result<PathBuf> {
    MiniMaxAdapter
        .config_home(env)
        .map(|home| home.join("plugins/rimz"))
        .ok_or_else(|| AgentErr::Install {
            agent: AGENT,
            reason: "$HOME is not set; cannot resolve MiniMax Code's data directory".into(),
        })
}

fn event_names() -> Vec<String> {
    HOOKS.iter().map(|hook| hook.event.to_owned()).collect()
}

fn files() -> [(&'static str, Vec<u8>); 3] {
    let hooks: BTreeMap<_, _> = HOOKS.iter().map(|hook| (
        hook.event,
        json!([{"hooks":[{"type":"command", "command":COMMAND, "timeout":hook.timeout_secs.unwrap_or(10)}]}]),
    )).collect();
    let manifest = json!({
        "schemaVersion":1,
        "name":"rimz",
        "version":"1.0.0",
        "description":"RimZ session and attention hooks",
        "author":"RimZ",
        "icon":"icon.png",
        "category":"Other",
        "exampleQueries":[],
        "apps":[],
        "mcpServers":[],
        "skills":[],
        "hooks":["hooks/rimz.json"]
    });
    // These JSON values contain only string keys and serializable primitives.
    [
        ("icon.png", ICON.to_vec()),
        (
            "hooks/rimz.json",
            serde_json::to_vec_pretty(&json!({"hooks":hooks})).expect("embedded hooks serialize"),
        ),
        (
            MANIFEST,
            serde_json::to_vec_pretty(&manifest).expect("embedded manifest serializes"),
        ),
    ]
}

fn check_package(path: &Path) -> Result<()> {
    for directory in [
        path.to_path_buf(),
        path.join("hooks"),
        path.join(".minimax-plugin"),
    ] {
        match std::fs::symlink_metadata(&directory) {
            Ok(metadata) if !metadata.is_dir() => {
                return Err(AgentErr::Install {
                    agent: AGENT,
                    reason: format!(
                        "{} must be a real directory, not a symlink or file",
                        directory.display()
                    ),
                });
            }
            Ok(_) => {}
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(AgentErr::InstallIo {
                    agent: AGENT,
                    path: directory,
                    source,
                });
            }
        }
    }
    let owned = if let Some(manifest) = read_optional_file(AGENT, &path.join(MANIFEST))? {
        serde_json::from_str::<Value>(&manifest)
            .is_ok_and(|value| value["name"] == "rimz" && value["author"] == "RimZ")
    } else {
        let mut owned = true;
        for (relative, filename) in [
            ("", "icon.png"),
            ("hooks", "rimz.json"),
            (".minimax-plugin", "plugin.json"),
        ] {
            let directory = path.join(relative);
            let io_err = |source| AgentErr::InstallIo {
                agent: AGENT,
                path: directory.clone(),
                source,
            };
            let entries = match std::fs::read_dir(&directory) {
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
                result => result.map_err(&io_err)?,
            };
            for entry in entries {
                let entry = entry.map_err(&io_err)?;
                let name = entry.file_name();
                let kind = entry.file_type().map_err(&io_err)?;
                let expected_directory =
                    relative.is_empty() && (name == "hooks" || name == ".minimax-plugin");
                owned &= if expected_directory {
                    kind.is_dir()
                } else {
                    name == filename && kind.is_file()
                };
            }
        }
        owned
    };
    if !owned {
        return Err(AgentErr::Install {
            agent: AGENT,
            reason: format!(
                "{} is not a RimZ-owned plugin; move it aside before installing",
                path.display()
            ),
        });
    }
    Ok(())
}

impl ManagedIntegration for MiniMaxIntegration {
    fn install(&self, env: &BTreeMap<String, String>) -> Result<HookInstallReport> {
        let path = package(env)?;
        check_package(&path)?;
        let mut report = Vec::new();
        for (relative, candidate) in files() {
            let file = path.join(relative);
            let original = crate::agents::settings_json::read_optional_bytes(AGENT, &file)?;
            if original.as_deref() != Some(candidate.as_slice()) {
                crate::agents::provider_file::write_bytes(&file, &candidate)?;
            }
            report.push(HookInstallFileReport {
                path: file,
                existed: original.is_some(),
            });
        }
        Ok(HookInstallReport {
            agent: AGENT,
            files: report,
            installed_events: event_names(),
        })
    }

    fn preview(&self, env: &BTreeMap<String, String>) -> Result<HookInstallPreview> {
        let path = package(env)?;
        check_package(&path)?;
        let mut previews = Vec::new();
        for (relative, candidate) in files().into_iter().skip(1) {
            let file = path.join(relative);
            let original = read_optional_file(AGENT, &file)?;
            previews.push(HookInstallFilePreview {
                path: file,
                existed: original.is_some(),
                original,
                // Only the two JSON files have textual diffs; the manifest names the icon.
                candidate: String::from_utf8(candidate).expect("serialized JSON is UTF-8"),
            });
        }
        Ok(HookInstallPreview {
            agent: AGENT,
            files: previews,
            planned_events: event_names(),
            status_line_change: None,
            subagent_status_line_change: None,
        })
    }

    fn uninstall(&self, env: &BTreeMap<String, String>) -> Result<HookUninstallReport> {
        let path = package(env)?;
        check_package(&path)?;
        let existed = path.exists();
        if existed {
            std::fs::remove_dir_all(&path).map_err(|source| AgentErr::InstallIo {
                agent: AGENT,
                path: path.clone(),
                source,
            })?;
        }
        Ok(HookUninstallReport {
            agent: AGENT,
            files: vec![HookInstallFileReport { path, existed }],
            removed_events: if existed { event_names() } else { Vec::new() },
        })
    }

    fn installed(&self, env: &BTreeMap<String, String>) -> bool {
        package(env).is_ok_and(|path| {
            check_package(&path).is_ok()
                && files().iter().all(|(relative, bytes)| {
                    let file = path.join(relative);
                    std::fs::symlink_metadata(&file).is_ok_and(|metadata| metadata.is_file())
                        && std::fs::read(file).is_ok_and(|content| content == *bytes)
                })
        })
    }

    fn managed_artifacts_present(&self, env: &BTreeMap<String, String>) -> bool {
        package(env).is_ok_and(|path| path.exists())
    }

    fn upgrade_available(&self, env: &BTreeMap<String, String>) -> bool {
        self.managed_artifacts_present(env) && !self.installed(env)
    }

    fn wiring_input_paths(
        &self,
        _spec: &AgentSpec,
        env: &BTreeMap<String, String>,
    ) -> Vec<PathBuf> {
        package(env)
            .map(|path| {
                files()
                    .iter()
                    .map(|(relative, _)| path.join(relative))
                    .collect()
            })
            .unwrap_or_default()
    }
}
