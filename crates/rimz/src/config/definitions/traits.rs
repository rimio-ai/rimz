//! Trait fragments substituted into definition crafts.

use std::collections::BTreeMap;
use std::path::Path;

use crate::packs::ClaimResolution;

use super::{DefinitionErr, SkillCatalog};

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
        // Trait names address one file in the trait tree, never an arbitrary path.
        if name.is_empty() || matches!(name.as_str(), "." | "..") || name.contains(['/', '\\']) {
            return Err(DefinitionErr::new(
                source,
                format!("invalid trait name '{name}'"),
            ));
        }
        let claim = paths.get(name).ok_or_else(|| {
            DefinitionErr::new(
                source,
                format!(
                    "cannot read trait '{}': no such trait",
                    home.join("traits").join(format!("{name}.md")).display()
                ),
            )
        })?;
        let path = match claim {
            ClaimResolution::Home(path) => path,
            ClaimResolution::Pack(claim) => &claim.path,
            ClaimResolution::Conflict { message, .. } => {
                return Err(DefinitionErr::new(source, message));
            }
        };
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
