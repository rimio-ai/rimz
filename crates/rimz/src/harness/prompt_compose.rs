//! Materialize profile prompt fragments into one provider replacement value.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::agents::{PresetArgMatcher, PresetField};
use crate::ids::AgentKind;
use crate::store::RuntimePaths;

const TEXT_PROMPT_LIMIT: usize = 120 * 1024;

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
    #[error(
        "{agent} system prompt is {size} bytes, exceeding RimZ's {limit}-byte argv safety limit; shorten the prompt"
    )]
    TooLarge {
        agent: &'static str,
        size: usize,
        limit: usize,
    },
    #[error(transparent)]
    Write(#[from] crate::store::atomic::AtomicErr),
}

/// Materialize and render one complete replacement prompt.
///
/// Launch planning has already validated support and file existence. Reads can
/// still fail if a file disappears between planning and process spawn; that
/// race is a launch failure, never a silent fallback.
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
    let agent = adapter.spec().kind;
    let matcher = adapter
        .spec()
        .launch
        .preset_arg_matcher(PresetField::SystemPromptFile)
        .ok_or(PromptComposeErr::Unsupported { agent })?;

    if sources.append_system_prompt_files.is_empty() {
        let path = sources
            .system_prompt_file
            .as_deref()
            .expect("non-empty sources without fragments contain a base prompt");
        return render(matcher, path, agent);
    }

    let base = sources
        .system_prompt_file
        .as_deref()
        .ok_or(PromptComposeErr::Unsupported { agent })?;
    let mut pieces = Vec::with_capacity(1 + sources.append_system_prompt_files.len());
    pieces.push(read_prompt(base)?);
    for path in &sources.append_system_prompt_files {
        pieces.push(read_prompt(path)?);
    }
    let composed = compose(&pieces);
    let artifact = write_artifact(runtime, &composed)?;
    render(matcher, &artifact, agent)
}

pub fn validate_text_prompt_size(
    kind: &AgentKind,
    sources: &SystemPromptSources,
) -> Result<(), PromptComposeErr> {
    let adapter = crate::agents::find_definition(kind.as_str())
        .ok_or_else(|| PromptComposeErr::UnknownAdapter { kind: kind.clone() })?;
    let agent = adapter.spec().kind;
    let matcher = adapter
        .spec()
        .launch
        .preset_arg_matcher(PresetField::SystemPromptFile)
        .ok_or(PromptComposeErr::Unsupported { agent })?;
    if !matches!(matcher, PresetArgMatcher::TextFlag(_)) {
        return Ok(());
    }
    let base = sources
        .system_prompt_file
        .as_deref()
        .ok_or(PromptComposeErr::Unsupported { agent })?;
    let contents = if sources.append_system_prompt_files.is_empty() {
        read_prompt(base)?
    } else {
        let mut pieces = Vec::with_capacity(1 + sources.append_system_prompt_files.len());
        pieces.push(read_prompt(base)?);
        for path in &sources.append_system_prompt_files {
            pieces.push(read_prompt(path)?);
        }
        compose(&pieces)
    };
    ensure_text_prompt_size(agent, &contents)
}

fn render(
    matcher: PresetArgMatcher,
    path: &Path,
    agent: &'static str,
) -> Result<MaterializedSystemPrompt, PromptComposeErr> {
    let path_value = path.to_string_lossy().into_owned();
    match matcher {
        PresetArgMatcher::Flag(flags) => Ok(with_args(render_flag(flags, path_value, agent)?)),
        PresetArgMatcher::ConfigKey { flags, key } => {
            Ok(with_args(render_config_key(flags, key, path_value, agent)?))
        }
        PresetArgMatcher::EnvPathVar(key) => Ok(MaterializedSystemPrompt {
            args: Vec::new(),
            env: [(key, path_value)].into_iter().collect(),
        }),
        PresetArgMatcher::TextFlag(flags) => {
            let contents = read_prompt(path)?;
            ensure_text_prompt_size(agent, &contents)?;
            Ok(with_args(render_flag(flags, contents, agent)?))
        }
    }
}

fn ensure_text_prompt_size(agent: &'static str, contents: &str) -> Result<(), PromptComposeErr> {
    if contents.len() > TEXT_PROMPT_LIMIT {
        return Err(PromptComposeErr::TooLarge {
            agent,
            size: contents.len(),
            limit: TEXT_PROMPT_LIMIT,
        });
    }
    Ok(())
}

fn with_args(args: Vec<String>) -> MaterializedSystemPrompt {
    MaterializedSystemPrompt {
        args,
        env: BTreeMap::new(),
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
    fn fragments_materialize_once_in_order_for_all_matchers() {
        let dir = tempfile::tempdir().expect("temp dir");
        let base = dir.path().join("base.md");
        let first = dir.path().join("first.md");
        std::fs::write(&base, "base\n\n").expect("base");
        std::fs::write(&first, "first").expect("first");
        let runtime = RuntimePaths::under(
            WorkspaceId::from_project_root(dir.path()),
            &dir.path().join("runtime"),
        )
        .expect("runtime");
        let sources = SystemPromptSources {
            system_prompt_file: Some(base),
            append_system_prompt_files: vec![first],
        };

        let claude =
            materialize_system_prompt(&AgentKind::new_unchecked("claude"), &sources, &runtime)
                .expect("claude prompt");
        let artifact = PathBuf::from(&claude.args[1]);
        assert_eq!(
            std::fs::read_to_string(&artifact).expect("artifact"),
            "base\n\nfirst\n"
        );
        assert_eq!(
            materialize_system_prompt(&AgentKind::new_unchecked("codex"), &sources, &runtime)
                .expect("codex prompt")
                .args,
            [
                "-c".to_owned(),
                format!("model_instructions_file={}", artifact.display())
            ]
        );
        assert_eq!(
            materialize_system_prompt(&AgentKind::new_unchecked("qwen"), &sources, &runtime)
                .expect("qwen prompt")
                .env
                .get("QWEN_SYSTEM_MD"),
            artifact.to_str().map(str::to_owned).as_ref()
        );
        assert_eq!(
            materialize_system_prompt(&AgentKind::new_unchecked("pi"), &sources, &runtime)
                .expect("pi prompt")
                .args,
            ["--system-prompt".to_owned(), "base\n\nfirst\n".to_owned()]
        );
    }

    #[test]
    fn text_prompt_has_conservative_argv_limit() {
        let dir = tempfile::tempdir().expect("temp dir");
        let prompt = dir.path().join("prompt.md");
        std::fs::write(&prompt, "x".repeat(TEXT_PROMPT_LIMIT + 1)).expect("prompt");
        let sources = SystemPromptSources {
            system_prompt_file: Some(prompt),
            append_system_prompt_files: Vec::new(),
        };
        let err = materialize_system_prompt(
            &AgentKind::new_unchecked("pi"),
            &sources,
            &RuntimePaths::shared(),
        )
        .expect_err("oversized prompt");
        assert!(matches!(err, PromptComposeErr::TooLarge { .. }));
    }

    #[test]
    fn no_fragments_keep_the_users_path() {
        let dir = tempfile::tempdir().expect("temp dir");
        let prompt = dir.path().join("prompt.md");
        std::fs::write(&prompt, "voice").expect("prompt");
        let sources = SystemPromptSources {
            system_prompt_file: Some(prompt.clone()),
            append_system_prompt_files: Vec::new(),
        };
        let qwen = materialize_system_prompt(
            &AgentKind::new_unchecked("qwen"),
            &sources,
            &RuntimePaths::shared(),
        )
        .expect("qwen");
        assert_eq!(
            qwen.env.get("QWEN_SYSTEM_MD").map(String::as_str),
            prompt.to_str()
        );
    }
}
