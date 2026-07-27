//! Materialize profile prompt fragments into one provider replacement value.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::agents::{PresetArgMatcher, PresetField};
use crate::ids::AgentKind;
use crate::store::RuntimePaths;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemPromptSources {
    pub system_prompt_file: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub append_system_prompt_files: Vec<PathBuf>,
}

impl SystemPromptSources {
    pub fn from_cell(cell: &crate::harness::spec::AgentCell) -> Self {
        Self {
            system_prompt_file: cell.system_prompt_file.clone(),
            append_system_prompt_files: cell.append_system_prompt_files.clone(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.system_prompt_file.is_none() && self.append_system_prompt_files.is_empty()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MaterializedSystemPrompt {
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, thiserror::Error)]
pub enum PromptComposeErr {
    #[error("unknown agent kind `{kind}`")]
    UnknownAdapter { kind: AgentKind },
    #[error(
        "{agent} does not support system prompt replacement; remove the prompt fields or put provider-specific flags in `args`"
    )]
    Unsupported { agent: &'static str },
    #[error("cannot read prompt file `{}`: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Write(#[from] crate::store::atomic::AtomicErr),
}

/// Render the prompt replacement argv for one hidden exec request.
///
/// Launch planning has already validated support and file existence. Reads can
/// still fail here if a file disappears between planning and process spawn;
/// that race is a launch failure, never a silent fallback.
pub fn materialize_system_prompt(
    kind: &AgentKind,
    sources: &SystemPromptSources,
    runtime: &RuntimePaths,
) -> Result<MaterializedSystemPrompt, PromptComposeErr> {
    if sources.is_empty() {
        return Ok(MaterializedSystemPrompt::default());
    }
    let adapter = crate::agents::find_definition(kind.as_str())
        .ok_or_else(|| PromptComposeErr::UnknownAdapter { kind: kind.clone() })?;
    let matcher = adapter
        .spec()
        .launch
        .preset_arg_matcher(PresetField::SystemPromptFile)
        .ok_or(PromptComposeErr::Unsupported {
            agent: adapter.spec().kind,
        })?;

    if sources.append_system_prompt_files.is_empty() {
        let path = sources
            .system_prompt_file
            .as_deref()
            .expect("non-empty sources without fragments contain a base prompt");
        return match matcher {
            PresetArgMatcher::Flag(flags) => Ok(MaterializedSystemPrompt {
                args: render_flag(
                    flags,
                    path.to_string_lossy().into_owned(),
                    adapter.spec().kind,
                )?,
                env: BTreeMap::new(),
            }),
            PresetArgMatcher::ConfigKey { flags, key } => Ok(MaterializedSystemPrompt {
                args: render_config_key(
                    flags,
                    key,
                    path.to_string_lossy().into_owned(),
                    adapter.spec().kind,
                )?,
                env: BTreeMap::new(),
            }),
            PresetArgMatcher::EnvPathVar(key) => {
                let artifact = write_artifact(runtime, &read_prompt(path)?)?;
                Ok(env_path(key, artifact))
            }
        };
    }

    let mut pieces = Vec::with_capacity(
        usize::from(sources.system_prompt_file.is_some())
            + sources.append_system_prompt_files.len(),
    );
    if let Some(path) = sources.system_prompt_file.as_deref() {
        pieces.push(read_prompt(path)?);
    }
    for path in &sources.append_system_prompt_files {
        pieces.push(read_prompt(path)?);
    }
    let composed = compose(&pieces);
    match matcher {
        PresetArgMatcher::Flag(flags) => {
            let path = write_artifact(runtime, &composed)?;
            Ok(MaterializedSystemPrompt {
                args: render_flag(
                    flags,
                    path.to_string_lossy().into_owned(),
                    adapter.spec().kind,
                )?,
                env: BTreeMap::new(),
            })
        }
        PresetArgMatcher::ConfigKey { flags, key } => {
            let path = write_artifact(runtime, &composed)?;
            Ok(MaterializedSystemPrompt {
                args: render_config_key(
                    flags,
                    key,
                    path.to_string_lossy().into_owned(),
                    adapter.spec().kind,
                )?,
                env: BTreeMap::new(),
            })
        }
        PresetArgMatcher::EnvPathVar(key) => {
            let path = write_artifact(runtime, &composed)?;
            Ok(env_path(key, path))
        }
    }
}

fn read_prompt(path: &Path) -> Result<String, PromptComposeErr> {
    std::fs::read_to_string(path).map_err(|source| PromptComposeErr::Read {
        path: path.to_path_buf(),
        source,
    })
}

fn compose(pieces: &[String]) -> String {
    pieces
        .iter()
        .map(|piece| format!("{}\n", piece.trim_end_matches(['\r', '\n'])))
        .collect::<Vec<_>>()
        .join("\n")
}

fn write_artifact(runtime: &RuntimePaths, contents: &str) -> Result<PathBuf, PromptComposeErr> {
    let digest = hex::encode(Sha256::digest(contents.as_bytes()));
    let path = runtime
        .system_prompt_dir()
        .join(format!("sys.{}.md", &digest[..32]));
    crate::store::atomic::write_cache_bytes_atomically(&path, contents.as_bytes())?;
    Ok(path)
}

fn env_path(key: String, path: PathBuf) -> MaterializedSystemPrompt {
    MaterializedSystemPrompt {
        args: Vec::new(),
        env: [(key, path.to_string_lossy().into_owned())]
            .into_iter()
            .collect(),
    }
}

fn render_flag(
    flags: Vec<String>,
    value: String,
    agent: &'static str,
) -> Result<Vec<String>, PromptComposeErr> {
    let flag = flags
        .into_iter()
        .next()
        .ok_or(PromptComposeErr::Unsupported { agent })?;
    Ok(vec![flag, value])
}

fn render_config_key(
    flags: Vec<String>,
    key: String,
    value: String,
    agent: &'static str,
) -> Result<Vec<String>, PromptComposeErr> {
    let flag = flags
        .into_iter()
        .next()
        .ok_or(PromptComposeErr::Unsupported { agent })?;
    Ok(vec![flag, format!("{key}={value}")])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::WorkspaceId;

    #[test]
    fn composition_normalizes_boundaries_to_one_blank_line() {
        assert_eq!(
            compose(&[
                "base\r\n\r\n".to_owned(),
                "first".to_owned(),
                "second\n".to_owned()
            ]),
            "base\n\nfirst\n\nsecond\n"
        );
    }

    #[test]
    fn fragments_materialize_once_in_order_for_path_adapters() {
        let dir = tempfile::tempdir().expect("temp dir");
        let base = dir.path().join("base.md");
        let first = dir.path().join("first.md");
        let second = dir.path().join("second.md");
        std::fs::write(&base, "base\n\n").expect("base");
        std::fs::write(&first, "first").expect("first");
        std::fs::write(&second, "second\n").expect("second");
        let runtime = RuntimePaths::under(
            WorkspaceId::from_project_root(dir.path()),
            &dir.path().join("runtime"),
        )
        .expect("runtime");
        let sources = SystemPromptSources {
            system_prompt_file: Some(base),
            append_system_prompt_files: vec![first, second],
        };

        let claude =
            materialize_system_prompt(&AgentKind::new_unchecked("claude"), &sources, &runtime)
                .expect("claude prompt");
        assert_eq!(claude.args[0], "--system-prompt-file");
        let artifact = PathBuf::from(&claude.args[1]);
        assert_eq!(
            std::fs::read_to_string(&artifact).expect("artifact"),
            "base\n\nfirst\n\nsecond\n"
        );
        assert_eq!(
            materialize_system_prompt(&AgentKind::new_unchecked("claude"), &sources, &runtime,)
                .expect("stable prompt")
                .args[1],
            claude.args[1]
        );

        let codex =
            materialize_system_prompt(&AgentKind::new_unchecked("codex"), &sources, &runtime)
                .expect("codex prompt");
        assert_eq!(
            codex.args,
            vec![
                "-c".to_owned(),
                format!("model_instructions_file={}", artifact.display())
            ]
        );
        let qwen = materialize_system_prompt(&AgentKind::new_unchecked("qwen"), &sources, &runtime)
            .expect("qwen prompt");
        assert!(qwen.args.is_empty());
        assert_eq!(
            qwen.env.get("QWEN_SYSTEM_MD").map(String::as_str),
            artifact.to_str()
        );
    }

    #[test]
    fn env_adapter_receives_artifact_and_single_path_adapter_keeps_source() {
        let dir = tempfile::tempdir().expect("temp dir");
        let prompt = dir.path().join("prompt.md");
        std::fs::write(&prompt, "voice").expect("prompt");
        let runtime = RuntimePaths::under(
            WorkspaceId::from_project_root(dir.path()),
            &dir.path().join("runtime"),
        )
        .expect("runtime");
        let sources = SystemPromptSources {
            system_prompt_file: Some(prompt.clone()),
            append_system_prompt_files: Vec::new(),
        };

        assert_eq!(
            materialize_system_prompt(&AgentKind::new_unchecked("claude"), &sources, &runtime)
                .expect("claude")
                .args,
            [
                "--system-prompt-file".to_owned(),
                prompt.to_string_lossy().into_owned()
            ]
        );
        let qwen = materialize_system_prompt(&AgentKind::new_unchecked("qwen"), &sources, &runtime)
            .expect("qwen");
        assert!(qwen.args.is_empty());
        let artifact = qwen.env.get("QWEN_SYSTEM_MD").expect("qwen env path");
        assert_eq!(
            std::fs::read_to_string(artifact).expect("qwen artifact"),
            "voice"
        );
    }

    #[test]
    fn adapter_without_replacement_fails_typed() {
        let sources = SystemPromptSources {
            system_prompt_file: Some(PathBuf::from("/tmp/prompt.md")),
            append_system_prompt_files: Vec::new(),
        };
        let runtime = RuntimePaths::shared();
        let err = materialize_system_prompt(&AgentKind::new_unchecked("droid"), &sources, &runtime)
            .expect_err("droid has append only");
        assert!(
            err.to_string()
                .contains("does not support system prompt replacement")
        );
    }
}
