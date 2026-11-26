//! The kind bases RimZ ships, one Markdown file per provider in `builtin/`, compiled in. A
//! kind that takes a system prompt and has no home or pack base takes its own file, or, without
//! one, the grant and body of `claude.md`; a kind whose prompt appends to the provider's own
//! takes the grant alone. Each loads through the checks a home base passes
//! (`super::BaseLoad::rows`) and reaches a launch only under a definition
//! (`super::DefinitionSource::built_in_base`), never as a profile row.

use std::borrow::Cow;

use crate::config::PromptSource;

const FILES: [(&str, &str); 5] = [
    ("antigravity", include_str!("builtin/antigravity.md")),
    ("claude", include_str!("builtin/claude.md")),
    ("codex", include_str!("builtin/codex.md")),
    ("opencode", include_str!("builtin/opencode.md")),
    ("pi", include_str!("builtin/pi.md")),
];

/// The grant every built-in base carries in its frontmatter.
const GRANT: &str = "capabilities: [bash, read, write, ask, web, rimz]";

/// The built-in base text for `kind`, or `None` for a kind that takes no system prompt.
pub(super) fn text(kind: &str) -> Option<Cow<'static, str>> {
    if !crate::agents::find_definition(kind)
        .is_some_and(|definition| definition.spec().launch.takes_system_prompt())
    {
        return None;
    }
    if let Some((_, text)) = FILES.iter().find(|(name, _)| *name == kind) {
        return Some(Cow::Borrowed(text));
    }
    let body = if crate::agents::system_prompt_appends(kind) {
        ""
    } else {
        // The embedded files parse, as the suite's test over every built-in base proves.
        FILES
            .iter()
            .find(|(name, _)| *name == "claude")
            .and_then(|(_, text)| {
                super::frontmatter::split(&PromptSource::built_in_origin("claude"), text).ok()
            })
            .map_or("", |(_, body)| body)
    };
    Some(Cow::Owned(format!(
        "---\ndescription: The built-in kind base for {kind}.\n{GRANT}\n---\n{body}"
    )))
}
