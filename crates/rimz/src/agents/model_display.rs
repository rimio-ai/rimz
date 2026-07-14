//! One home for model-id display names across CLI and sidebar surfaces.

/// Render a provider model id for people.
pub fn display_model(id: &str) -> String {
    prettify_model_slug(strip_date_suffix(id.trim()))
}

/// Structurally render Factory's legacy custom selector
/// `custom:<display-name-slug>-<zero-based-index>`. The terminal decimal is
/// protocol bookkeeping, not part of the model name. This is presentation
/// only: callers must resolve configuration separately before treating the
/// selector as a canonical pricing identity.
pub fn display_factory_custom_selector(id: &str) -> Option<String> {
    let selector = id.trim().strip_prefix("custom:")?;
    let (display_slug, index) = selector.rsplit_once('-')?;
    if display_slug.is_empty()
        || index.is_empty()
        || !index.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let display = prettify_model_slug(display_slug);
    (!display.is_empty()).then_some(display)
}

/// Drop a trailing `-YYYYMMDD` 8-digit date stamp, leaving the base model id.
fn strip_date_suffix(id: &str) -> &str {
    match id.rsplit_once('-') {
        Some((base, tail)) if tail.len() == 8 && tail.bytes().all(|b| b.is_ascii_digit()) => base,
        _ => id,
    }
}

/// Prettify a raw model slug into a display name: drop a leading vendor token
/// so the family name leads, join split version digits with a dot (`4-8` →
/// `4.8`), and title-case the words (acronyms like `gpt` upper-cased), so
/// `claude-opus-4-8` reads `Opus 4.8` and `gpt-5.5-codex` reads `GPT 5.5 Codex`.
fn prettify_model_slug(slug: &str) -> String {
    let segments: Vec<&str> = slug.split('-').filter(|seg| !seg.is_empty()).collect();
    // A leading vendor prefix is redundant with the brand emblem and product
    // header, so the family name leads; a single-segment product keeps its name.
    let start = usize::from(segments.len() > 1 && matches!(segments[0], "claude" | "anthropic"));
    let mut words: Vec<String> = Vec::new();
    for segment in &segments[start..] {
        let is_int = segment.chars().all(|c| c.is_ascii_digit());
        let prev_is_version = words
            .last()
            .is_some_and(|prev| prev.chars().all(|c| c.is_ascii_digit() || c == '.'));
        if is_int && prev_is_version {
            // A split `major-minor`: glue onto the running version (`4` then `8`).
            let version = words.last_mut().expect("prev_is_version implies a word");
            version.push('.');
            version.push_str(segment);
        } else {
            words.push(title_word(segment));
        }
    }
    words.join(" ")
}

/// Title-case one slug segment: known acronyms upper-case, a version-like
/// segment (digits and dots) passes through, every other word capitalizes its
/// first letter.
fn title_word(word: &str) -> String {
    match word {
        "gpt" => "GPT".to_owned(),
        "codex" => "Codex".to_owned(),
        "deepseek" => "DeepSeek".to_owned(),
        _ if word.chars().all(|c| c.is_ascii_digit() || c == '.') => word.to_owned(),
        _ => {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_model_names_match_human_surfaces() {
        assert_eq!(display_model("claude-opus-4-8"), "Opus 4.8");
        assert_eq!(display_model("anthropic-claude-4-8"), "Claude 4.8");
        assert_eq!(display_model("gpt-5-codex"), "GPT 5 Codex");
        assert_eq!(display_model("gpt-5.5-codex"), "GPT 5.5 Codex");
        assert_eq!(display_model("claude-opus-4-7-20260101"), "Opus 4.7");
        assert_eq!(display_model("gpt-5-codex-20260101"), "GPT 5 Codex");
        assert_eq!(display_model("deepseek-v4-pro"), "DeepSeek V4 Pro");
        assert_eq!(display_model("mystery-model"), "Mystery Model");
    }

    #[test]
    fn factory_custom_selector_drops_only_the_terminal_index() {
        assert_eq!(
            display_factory_custom_selector("custom:DeepSeek-V4-Pro-0").as_deref(),
            Some("DeepSeek V4 Pro")
        );
        assert_eq!(
            display_factory_custom_selector("custom:Model-2-12").as_deref(),
            Some("Model 2")
        );
        assert!(display_factory_custom_selector("custom:Model-x").is_none());
        assert!(display_factory_custom_selector("gpt-5").is_none());
    }
}
