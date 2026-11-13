//! Trait fragments substituted into definition crafts, and the consensus
//! pieces a team composes after its consensus.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::{PromptSource, Team};
use crate::harness::team_prompt::{SHIPPED_REFLECT, reflect_copy_path};
use crate::packs::ClaimResolution;

use super::{DefinitionErr, SkillCatalog};

/// The trait claims one definitions load resolved, kept for the project TOML
/// teams compiled after it, which name consensus pieces from the same tree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConsensusPieces {
    home: PathBuf,
    claims: BTreeMap<String, ClaimResolution>,
}

impl ConsensusPieces {
    pub(super) fn new(home: &Path, claims: &BTreeMap<String, ClaimResolution>) -> Self {
        Self {
            home: home.to_owned(),
            claims: claims.clone(),
        }
    }

    /// Put the pieces `team` lists ahead of its other team prompt files.
    pub(in crate::config) fn compose(&self, name: &str, team: &mut Team) -> Result<(), String> {
        let pieces = consensus(
            &self.home,
            &self.claims,
            name,
            &team.consensus,
            &team.pipeline_stages(),
            |path| std::fs::read_to_string(path).map_err(|error| error.to_string()),
        )?;
        team.append_system_prompt_files.splice(0..0, pieces.sources);
        Ok(())
    }
}

pub(super) struct Pieces {
    pub sources: Vec<PromptSource>,
    /// Whether `reflect` resolved to [`SHIPPED_REFLECT`], whose wording names a
    /// `Reflect` stage and the `rimz-reflect` skill.
    pub shipped_reflect: bool,
}

/// Resolve the trait names a team lists in `consensus`, in order and each once.
/// `reflect` with no home or pack claim is the piece RimZ ships. Errors are the
/// whole refusal, naming team `name`.
pub(super) fn consensus(
    home: &Path,
    claims: &BTreeMap<String, ClaimResolution>,
    name: &str,
    names: &[String],
    stages: &[String],
    read: impl Fn(&Path) -> Result<String, String>,
) -> Result<Pieces, String> {
    let fail = |message: String| format!("team '{name}' consensus: {message}");
    let mut seen = Vec::new();
    let mut pieces = Pieces {
        sources: Vec::new(),
        shipped_reflect: false,
    };
    for piece in names {
        if seen.contains(piece) {
            continue;
        }
        seen.push(piece.clone());
        if piece == "reflect" && !claims.contains_key(piece) {
            pieces.shipped_reflect = true;
            pieces.sources.push(PromptSource::Text {
                origin: reflect_copy_path(home),
                text: SHIPPED_REFLECT.trim().to_owned(),
            });
            continue;
        }
        let path = claimed(home, claims, piece).map_err(fail)?;
        let text = read(path)
            .map_err(|error| fail(format!("cannot read trait '{}': {error}", path.display())))?;
        pieces.sources.push(PromptSource::Text {
            origin: path.to_owned(),
            text: text.trim().to_owned(),
        });
    }
    if pieces.shipped_reflect && !stages.iter().any(|stage| stage == "Reflect") {
        return Err(format!(
            "team '{name}' lists the shipped `reflect` consensus piece but declares no `Reflect` \
             stage; add a `Reflect` stage, or drop `reflect` from `consensus`"
        ));
    }
    Ok(pieces)
}

/// The file a trait name resolves to, or the refusal in the trait wording.
fn claimed<'a>(
    home: &Path,
    paths: &'a BTreeMap<String, ClaimResolution>,
    name: &str,
) -> Result<&'a Path, String> {
    // Trait names address one file in the trait tree, never an arbitrary path.
    if name.is_empty() || matches!(name, "." | "..") || name.contains(['/', '\\']) {
        return Err(format!("invalid trait name '{name}'"));
    }
    match paths.get(name) {
        None => Err(format!(
            "cannot read trait '{}': no such trait",
            home.join("traits").join(format!("{name}.md")).display()
        )),
        Some(ClaimResolution::Home(path)) => Ok(path),
        Some(ClaimResolution::Pack(claim)) => Ok(&claim.path),
        Some(ClaimResolution::Conflict { message, .. }) => Err(message.clone()),
    }
}

pub(super) fn render(
    home: &Path,
    paths: &BTreeMap<String, ClaimResolution>,
    source: &Path,
    body: &str,
    names: &[String],
    catalog: &SkillCatalog<'_>,
) -> Result<String, DefinitionErr> {
    if !names.is_empty() && !body.contains("${traits}") {
        return Err(DefinitionErr::new(
            source,
            format!("names traits {names:?} but its body has no `${{traits}}` token"),
        ));
    }
    let mut seen = Vec::new();
    let mut fragments = Vec::new();
    for name in names {
        if seen.contains(name) {
            continue;
        }
        let path = claimed(home, paths, name).map_err(|error| DefinitionErr::new(source, error))?;
        let text = catalog.read_text(path).map_err(|error| {
            DefinitionErr::new(
                source,
                format!("cannot read trait '{}': {error}", path.display()),
            )
        })?;
        seen.push(name.clone());
        fragments.push(text.trim().to_owned());
    }
    let traits = fragments.join("\n\n");
    // `build` substitutes the first token in place and folds every later one,
    // with the newlines around it, into one blank line.
    let Some((head, tail)) = body.split_once("${traits}") else {
        return Ok(body.trim().to_owned());
    };
    let mut rendered = format!("{head}{traits}");
    let mut rest = tail;
    while let Some((before, after)) = rest.split_once("${traits}") {
        rendered.push_str(before.trim_end_matches('\n'));
        rendered.push_str("\n\n");
        rest = after.trim_start_matches('\n');
    }
    rendered.push_str(rest);
    Ok(rendered.trim().to_owned())
}
