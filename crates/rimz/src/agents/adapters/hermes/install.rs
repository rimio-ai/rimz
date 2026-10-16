//! Non-destructive Hermes YAML hook configuration and native consent reporting.

use super::*;
use crate::agents::provider_file;
use crate::agents::version::{CliVersion, probe_cli_version};
use serde_json::{Map, Value, json};
use serde_saphyr::granit_parser::{Event, Parser, StructureStyle};
use std::collections::BTreeMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

const COMMAND: &str = "rimz hooks feed --source hermes";
const OWNER: &str = "hooks feed --source hermes";
pub(super) const MIN_VERSION: CliVersion = CliVersion::new(0, 22, 2);

pub(super) static MANAGED_INTEGRATION: HermesManagedIntegration = HermesManagedIntegration;

pub(super) struct HermesManagedIntegration;

impl ManagedIntegration for HermesManagedIntegration {
    fn install(&self, env: &BTreeMap<String, String>) -> Result<HookInstallReport> {
        install_at(&config_path(env)?, installed_version())
    }

    fn preview(&self, env: &BTreeMap<String, String>) -> Result<HookInstallPreview> {
        preview_at(&config_path(env)?, installed_version())
    }

    fn uninstall(&self, env: &BTreeMap<String, String>) -> Result<HookUninstallReport> {
        uninstall_at(&config_path(env)?)
    }

    fn installed(&self, env: &BTreeMap<String, String>) -> bool {
        config_path(env).is_ok_and(|path| installed_at(&path))
    }

    fn managed_artifacts_present(&self, env: &BTreeMap<String, String>) -> bool {
        config_path(env)
            .ok()
            .and_then(|path| read_config(&path).ok())
            .is_some_and(|(_, config)| {
                config.hooks.values().any(|entries| {
                    entries
                        .as_array()
                        .is_some_and(|entries| entries.iter().any(owned))
                })
            })
    }

    fn install_blocker(&self, env: &BTreeMap<String, String>) -> Option<String> {
        self.preview(env).err().map(|error| error.to_string())
    }

    fn wiring_input_paths(
        &self,
        _spec: &AgentSpec,
        env: &BTreeMap<String, String>,
    ) -> Vec<PathBuf> {
        config_path(env).into_iter().collect()
    }

    fn untrusted_installed_hooks(&self, env: &BTreeMap<String, String>) -> Vec<String> {
        config_path(env).map_or_else(|_| Vec::new(), |path| untrusted_at(&path))
    }
}

fn config_path(env: &BTreeMap<String, String>) -> Result<PathBuf> {
    HermesAdapter
        .config_home(env)
        .map(|home| home.join("config.yaml"))
        .ok_or_else(|| AgentErr::Install {
            agent: "hermes",
            reason: "$HOME is not set; cannot resolve ~/.hermes/config.yaml".into(),
        })
}

fn installed_version() -> Option<CliVersion> {
    probe_cli_version("hermes")?.parse().ok()
}

fn preview_at(path: &Path, version: Option<CliVersion>) -> Result<HookInstallPreview> {
    if let Some(found) = version.filter(|found| *found < MIN_VERSION) {
        return Err(AgentErr::Install {
            agent: "hermes",
            reason: format!(
                "Hermes {found} is older than {MIN_VERSION}, the oldest release RimZ supports; upgrade Hermes"
            ),
        });
    }
    let (original, mut config) = read_config(path)?;
    let wired = config.hooks.clone();
    config.remove_owned();
    for event in HERMES_HOOKS.iter().map(|hook| hook.event) {
        let entries = config
            .hooks
            .entry(event.to_owned())
            .or_insert_with(|| json!([]));
        if entries.is_null() {
            *entries = json!([]);
        }
        entries
            .as_array_mut()
            .ok_or_else(config_refusal)?
            .push(json!({"command":COMMAND,"timeout":10}));
    }
    Ok(HookInstallPreview {
        agent: "hermes",
        files: vec![HookInstallFilePreview {
            path: path.to_path_buf(),
            existed: original.is_some(),
            candidate: match &original {
                // Hermes re-emits `hooks` in its own layout; equal values keep those bytes.
                Some(text) if config.hooks == wired => text.clone(),
                _ => config.render(original.as_deref().unwrap_or_default())?,
            },
            original,
        }],
        planned_events: HERMES_HOOKS
            .iter()
            .map(|hook| hook.event.to_owned())
            .collect(),
        status_line_change: None,
        subagent_status_line_change: None,
    })
}

fn install_at(path: &Path, version: Option<CliVersion>) -> Result<HookInstallReport> {
    let preview = preview_at(path, version)?;
    let file = &preview.files[0];
    if file.original.as_deref() != Some(&file.candidate) {
        provider_file::write_bytes(path, file.candidate.as_bytes())?;
    }
    Ok(HookInstallReport {
        agent: "hermes",
        files: vec![HookInstallFileReport {
            path: path.to_path_buf(),
            existed: file.existed,
        }],
        installed_events: preview.planned_events,
    })
}

fn uninstall_at(path: &Path) -> Result<HookUninstallReport> {
    let (original, mut config) = read_config(path)?;
    let removed_events = config.remove_owned();
    if !removed_events.is_empty() {
        provider_file::write_bytes(
            path,
            config
                .render(original.as_deref().unwrap_or_default())?
                .as_bytes(),
        )?;
    }
    Ok(HookUninstallReport {
        agent: "hermes",
        files: vec![HookInstallFileReport {
            path: path.to_path_buf(),
            existed: original.is_some(),
        }],
        removed_events,
    })
}

fn installed_at(path: &Path) -> bool {
    read_config(path).is_ok_and(|(_, config)| {
        HERMES_HOOKS.iter().all(|hook| {
            config
                .hooks
                .get(hook.event)
                .and_then(Value::as_array)
                .is_some_and(|entries| entries.contains(&json!({"command":COMMAND,"timeout":10})))
        })
    })
}

fn untrusted_at(path: &Path) -> Vec<String> {
    let Ok((_, config)) = read_config(path) else {
        return Vec::new();
    };
    let allowlist = path.with_file_name("shell-hooks-allowlist.json");
    let approvals = std::fs::read_to_string(allowlist)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|mut value| {
            value
                .get_mut("approvals")
                .and_then(Value::as_array_mut)
                .map(std::mem::take)
        })
        .unwrap_or_default();
    HERMES_HOOKS
        .iter()
        .filter(|hook| {
            config
                .hooks
                .get(hook.event)
                .and_then(Value::as_array)
                .is_some_and(|entries| {
                    entries.iter().filter(|entry| owned(entry)).any(|entry| {
                        !approvals.iter().any(|approved| {
                            approved.get("event").and_then(Value::as_str) == Some(hook.event)
                                && approved.get("command") == entry.get("command")
                        })
                    })
                })
        })
        .map(|hook| hook.event.to_owned())
        .collect()
}

fn owned(entry: &Value) -> bool {
    entry
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|command| command.contains(OWNER))
}

fn config_refusal() -> AgentErr {
    AgentErr::Install {agent:"hermes",reason:"cannot safely edit this YAML; edit config.yaml in the Hermes home (default ~/.hermes) by hand: add the RimZ hooks shown by rimz hooks install hermes --dry-run (use a block-mapping root and no anchors, aliases, or tags inside hooks)".into()}
}

fn read_config(path: &Path) -> Result<(Option<String>, Config)> {
    let original = read_optional_file("hermes", path)?;
    let config = Config::parse(original.as_deref().unwrap_or_default())?;
    Ok((original, config))
}

struct Config {
    hooks: Map<String, Value>,
    range: Option<Range<usize>>,
    indent: usize,
}

impl Config {
    fn parse(text: &str) -> Result<Self> {
        let root: Value = serde_saphyr::from_str(text).map_err(|_| config_refusal())?;
        let hooks = match root.get("hooks") {
            Some(Value::Object(hooks)) => hooks.clone(),
            None | Some(Value::Null) => Map::new(),
            _ => return Err(config_refusal()),
        };
        for (event, entries) in &hooks {
            if matches!(event.as_str(), "output_spill" | "outbound") {
                continue;
            }
            if !entries.is_null() && !entries.is_array() {
                return Err(config_refusal());
            }
        }
        let mut depth = 0;
        let mut documents = 0;
        let mut key = true;
        let mut inside = false;
        let mut start = None;
        let mut end = 0;
        let mut indent = 0;
        let mut styles = Vec::new();
        for item in Parser::new_from_str(text) {
            let (event, span) = item.map_err(|_| config_refusal())?;
            let offset = |marker: serde_saphyr::granit_parser::Marker| {
                marker.byte_offset().unwrap_or_else(|| {
                    text.char_indices()
                        .nth(marker.index())
                        .map_or(text.len(), |(offset, _)| offset)
                })
            };
            let forbidden = matches!(&event, Event::Alias(_))
                || match &event {
                    Event::Scalar(_, _, anchor, tag)
                    | Event::MappingStart(_, anchor, tag)
                    | Event::SequenceStart(_, anchor, tag) => *anchor != 0 || tag.is_some(),
                    _ => false,
                };
            if inside && forbidden {
                return Err(config_refusal());
            }
            match event {
                Event::DocumentStart(..) => {
                    documents += 1;
                    if documents > 1 {
                        return Err(config_refusal());
                    }
                }
                Event::MappingStart(style, anchor, tag)
                | Event::SequenceStart(style, anchor, tag) => {
                    if depth == 0 {
                        if style != StructureStyle::Block
                            || anchor != 0
                            || tag.is_some()
                            || !root.is_object()
                        {
                            return Err(config_refusal());
                        }
                        indent = span.start.col();
                    }
                    if inside {
                        end = offset(span.end);
                    }
                    styles.push(style);
                    depth += 1;
                }
                Event::MappingEnd | Event::SequenceEnd => {
                    if inside && styles.last() == Some(&StructureStyle::Flow) {
                        end = offset(span.end);
                    }
                    styles.pop();
                    depth -= 1;
                    if depth == 1 {
                        key = true;
                        inside = false;
                    }
                }
                Event::Scalar(value, _, anchor, tag) if depth == 1 && key => {
                    inside = value == "hooks";
                    if inside {
                        if start.is_some()
                            || anchor != 0
                            || tag.is_some()
                            || span.start.line() != span.end.line()
                        {
                            return Err(config_refusal());
                        }
                        let offset = offset(span.start);
                        start = Some(text[..offset].rfind('\n').map_or(0, |index| index + 1));
                        end = offset;
                    }
                    key = false;
                }
                Event::Scalar(..) | Event::Alias(_) => {
                    if inside {
                        end = offset(span.end);
                    }
                    if depth == 1 {
                        key = true;
                        inside = false;
                    }
                }
                _ => {}
            }
        }
        if !root.is_object()
            && !text
                .lines()
                .all(|line| line.trim().is_empty() || line.trim_start().starts_with('#'))
        {
            return Err(config_refusal());
        }
        let range = start.map(|start| {
            // Consume only the value's final line, not the following comments.
            if end > start && text.as_bytes().get(end.wrapping_sub(1)) != Some(&b'\n') {
                end = text[end..]
                    .find('\n')
                    .map_or(text.len(), |index| end + index + 1);
            }
            start..end
        });
        Ok(Self {
            hooks,
            range,
            indent,
        })
    }

    fn remove_owned(&mut self) -> Vec<String> {
        let mut removed = Vec::new();
        self.hooks.retain(|event, entries| {
            if matches!(event.as_str(), "output_spill" | "outbound") {
                return true;
            }
            let Some(entries) = entries.as_array_mut() else {
                return true;
            };
            let before = entries.len();
            entries.retain(|entry| !owned(entry));
            if entries.len() == before {
                return true;
            }
            removed.push(event.clone());
            !entries.is_empty()
        });
        removed
    }

    fn render(&self, original: &str) -> Result<String> {
        let mut block = String::new();
        if !self.hooks.is_empty() {
            let indent = " ".repeat(self.indent);
            block.push_str(&format!("{indent}hooks:\n"));
            // JSON is a YAML flow mapping. It preserves all accepted values
            // without enabling a second serializer or rewriting unrelated keys.
            let json = serde_json::to_string_pretty(&self.hooks).map_err(|_| config_refusal())?;
            for line in json.lines() {
                block.push_str(&format!("{indent}  {line}\n"));
            }
        }
        if let Some(range) = &self.range {
            return Ok(format!(
                "{}{}{}",
                &original[..range.start],
                block,
                &original[range.end..]
            ));
        }
        let newline = if original.is_empty() || original.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        Ok(format!("{original}{newline}{block}"))
    }
}

#[cfg(test)]
mod tests;
