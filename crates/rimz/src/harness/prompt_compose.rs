//! Materialize profile prompt fragments into one provider replacement value.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::agents::{PresetArgMatcher, PresetField};
use crate::disk::paths::RuntimePaths;
use crate::ids::AgentKind;

pub(super) const TEXT_PROMPT_LIMIT: usize = 120 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SystemPromptSources {
    pub system_prompt_file: Option<PathBuf>,
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

pub struct SystemPromptPlan {
    pub sources: SystemPromptSources,
    pub composed: Option<String>,
    pub artifact: Option<PathBuf>,
    pub materialized: MaterializedSystemPrompt,
}

#[derive(Debug, thiserror::Error)]
pub enum PromptComposeErr {
    #[error("unknown agent kind `{kind}`")]
    UnknownAdapter { kind: AgentKind },
    #[error(
        "{agent} does not support system prompt replacement; remove the prompt fields or put provider-specific flags in `args`"
    )]
    Unsupported { agent: &'static str },
    #[error(
        "{agent} append-system-prompt-files requires a base system-prompt-file; add one to the profile, role, or launch command"
    )]
    MissingBase { agent: &'static str },
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
    Write(#[from] crate::disk::atomic::AtomicErr),
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
    let plan = plan_system_prompt(kind, sources, runtime)?;
    apply_system_prompt(&plan)?;
    Ok(plan.materialized)
}

pub fn plan_system_prompt(
    kind: &AgentKind,
    sources: &SystemPromptSources,
    runtime: &RuntimePaths,
) -> Result<SystemPromptPlan, PromptComposeErr> {
    if sources.is_empty() {
        return Ok(SystemPromptPlan {
            sources: sources.clone(),
            composed: None,
            artifact: None,
            materialized: MaterializedSystemPrompt::default(),
        });
    }
    let adapter = crate::agents::find_definition(kind.as_str())
        .ok_or_else(|| PromptComposeErr::UnknownAdapter { kind: kind.clone() })?;
    let agent = adapter.spec().kind;
    let matcher = adapter
        .spec()
        .launch
        .preset_arg_matcher(PresetField::SystemPromptFile)
        .ok_or(PromptComposeErr::Unsupported { agent })?;

    let composed = read_composed(sources, agent)?;
    let artifact = (!sources.append_system_prompt_files.is_empty())
        .then(|| prompt_artifact_path(runtime, "sys", &composed));
    let path = artifact
        .as_deref()
        .or(sources.system_prompt_file.as_deref())
        .expect("read_composed requires a base prompt");
    let materialized = render(matcher, path, &composed, agent)?;
    Ok(SystemPromptPlan {
        sources: sources.clone(),
        composed: Some(composed),
        artifact,
        materialized,
    })
}

pub fn apply_system_prompt(plan: &SystemPromptPlan) -> Result<(), PromptComposeErr> {
    if let Some(path) = &plan.artifact {
        let contents = plan
            .composed
            .as_deref()
            .expect("a planned artifact always has composed contents");
        crate::disk::atomic::write_cache_bytes_atomically(path, contents.as_bytes())?;
    }
    Ok(())
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
    let contents = read_composed(sources, agent)?;
    ensure_text_prompt_size(agent, &contents)
}

fn render(
    matcher: PresetArgMatcher,
    path: &Path,
    contents: &str,
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
            ensure_text_prompt_size(agent, contents)?;
            Ok(with_args(render_flag(flags, contents.to_owned(), agent)?))
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

fn read_composed(
    sources: &SystemPromptSources,
    agent: &'static str,
) -> Result<String, PromptComposeErr> {
    let base = sources
        .system_prompt_file
        .as_deref()
        .ok_or(PromptComposeErr::MissingBase { agent })?;
    if sources.append_system_prompt_files.is_empty() {
        return read_prompt(base);
    }
    let mut pieces = Vec::with_capacity(1 + sources.append_system_prompt_files.len());
    pieces.push(read_prompt(base)?);
    for path in &sources.append_system_prompt_files {
        pieces.push(read_prompt(path)?);
    }
    Ok(compose(&pieces))
}

fn compose(pieces: &[String]) -> String {
    pieces
        .iter()
        .map(|piece| format!("{}\n", piece.trim_end_matches(['\r', '\n'])))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn write_prompt_artifact(
    runtime: &RuntimePaths,
    prefix: &str,
    contents: &str,
) -> Result<PathBuf, crate::disk::atomic::AtomicErr> {
    let path = prompt_artifact_path(runtime, prefix, contents);
    crate::disk::atomic::write_cache_bytes_atomically(&path, contents.as_bytes())?;
    Ok(path)
}

pub(super) fn prompt_artifact_path(
    runtime: &RuntimePaths,
    prefix: &str,
    contents: &str,
) -> PathBuf {
    let digest = hex::encode(Sha256::digest(contents.as_bytes()));
    runtime
        .prompt_dir()
        .join(format!("{prefix}.{}.md", &digest[..32]))
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

pub fn retry_prompt(base: &str, failure_tail: Option<&str>) -> String {
    let failure = failure_tail.map_or_else(
        || "A previous attempt at this task failed (exit 1), but no terminal output was captured."
            .to_owned(),
        |tail| {
            format!(
                "A previous attempt at this task failed (exit 1). The tail of its terminal output:\n{tail}"
            )
        },
    );
    format!("{base}\n\n<previous-attempt-failure>\n{failure}\n</previous-attempt-failure>")
}

pub fn verify_reprompt(cmd: &str, code_label: &str, output: &str) -> String {
    let tail = crate::proc::tail_output(output.as_bytes(), 4 * 1024);
    format!(
        "Verification failed — the task is not done yet. Fix the underlying problem in this same session until the verify command passes.\n\n--- verify `{cmd}` exited {code_label} ---\n{tail}"
    )
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

        let claude = plan_system_prompt(&AgentKind::new_unchecked("claude"), &sources, &runtime)
            .expect("claude prompt");
        let pi = plan_system_prompt(&AgentKind::new_unchecked("pi"), &sources, &runtime)
            .expect("pi prompt");
        let artifact = claude.artifact.as_ref().expect("planned artifact");
        assert!(!runtime.prompt_dir().exists());
        assert_eq!(claude.composed.as_deref(), Some("base\n\nfirst\n"));
        assert_eq!(pi.materialized.args, ["--system-prompt", "base\n\nfirst\n"]);
        assert_eq!(claude.materialized.args[1], artifact.to_string_lossy());
        apply_system_prompt(&claude).expect("apply prompt");
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

    #[test]
    fn fragment_without_base_names_the_missing_requirement() {
        let dir = tempfile::tempdir().expect("temp dir");
        let fragment = dir.path().join("fragment.md");
        std::fs::write(&fragment, "voice").expect("fragment");
        let err = materialize_system_prompt(
            &AgentKind::new_unchecked("claude"),
            &SystemPromptSources {
                system_prompt_file: None,
                append_system_prompt_files: vec![fragment],
            },
            &RuntimePaths::shared(),
        )
        .expect_err("base is required");
        assert!(matches!(
            err,
            PromptComposeErr::MissingBase { agent: "claude" }
        ));
    }

    #[test]
    fn retry_prompt_includes_latest_failure_without_nesting() {
        let first = retry_prompt("fix it", Some("first failure"));
        let second = retry_prompt("fix it", Some("error: broken\nlast line"));

        assert!(first.contains("first failure"));
        assert!(!second.contains("first failure"));
        assert!(second.contains("The tail of its terminal output:\nerror: broken\nlast line"));
        assert_eq!(second.matches("<previous-attempt-failure>").count(), 1);
    }

    #[test]
    fn retry_prompt_explains_missing_output() {
        assert!(retry_prompt("fix it", None).contains("no terminal output was captured"));
    }

    #[test]
    fn verify_reprompt_formats_status_and_caps_output_tail() {
        let output = format!("old{}latest", "x".repeat(4 * 1024));
        let prompt = verify_reprompt("cargo xtask test auth", "1", &output);

        assert!(prompt.starts_with("Verification failed — the task is not done yet."));
        assert!(prompt.contains("--- verify `cargo xtask test auth` exited 1 ---"));
        assert!(!prompt.contains("old"));
        assert!(prompt.ends_with("latest"));
    }
}
