//! One home for model-id display names across CLI and sidebar surfaces.

/// Render a provider model id for people.
pub fn display_model(id: &str) -> String {
    prettify_model_slug(strip_date_suffix(id.trim()))
}

/// Prefer a versioned provider display name, then context id, launch model, or family name.
pub fn model_label_source<'a>(
    display_name: Option<&'a str>,
    context_id: Option<&'a str>,
    launch_model: Option<&'a str>,
) -> Option<&'a str> {
    display_name
        .filter(|name| name.bytes().any(|byte| byte.is_ascii_digit()))
        .or(context_id)
        .or(launch_model)
        .or(display_name)
}

/// A model's display name and optional compact context-window qualifier.
pub fn model_label_parts(raw: &str) -> (String, Option<String>) {
    let (name, window) = strip_window_qualifier(raw);
    let label = if let Some(custom) = display_factory_custom_selector(name) {
        custom
    } else if looks_like_slug(name) {
        display_model(name)
    } else {
        let spaced = name.replace('-', " ");
        spaced
            .strip_prefix("Claude ")
            .or_else(|| spaced.strip_prefix("Anthropic "))
            .map(str::to_owned)
            .unwrap_or(spaced)
    };
    (label, window)
}

fn strip_window_qualifier(raw: &str) -> (&str, Option<String>) {
    let trimmed = raw.trim_end();
    let qualifier = trimmed
        .strip_suffix(']')
        .and_then(|head| head.rsplit_once('['))
        .or_else(|| {
            trimmed
                .strip_suffix(')')
                .and_then(|head| head.rsplit_once(" ("))
        });
    let Some((name, qualifier)) = qualifier else {
        return (trimmed, None);
    };
    let magnitude = qualifier.strip_suffix(" context").unwrap_or(qualifier);
    let (digits, multiplier) = match magnitude.as_bytes().last() {
        Some(b'k' | b'K') => (&magnitude[..magnitude.len() - 1], 1_000),
        Some(b'm' | b'M') => (&magnitude[..magnitude.len() - 1], 1_000_000),
        _ => return (trimmed, None),
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return (trimmed, None);
    }
    let window = digits
        .parse::<u64>()
        .ok()
        .and_then(|count| count.checked_mul(multiplier));
    match window {
        Some(window) => (name.trim_end(), Some(window_short(window))),
        None => (trimmed, None),
    }
}

fn looks_like_slug(value: &str) -> bool {
    value.contains('-')
        && !value.contains(' ')
        && !value.contains('(')
        && !value.chars().any(|c| c.is_ascii_uppercase())
}

/// Context windows use a quiet lowercase magnitude, separate from token counts.
pub(crate) fn window_short(window: u64) -> String {
    if window >= 1_000_000 {
        format!("{}m", window / 1_000_000)
    } else if window >= 1_000 {
        format!("{}k", window / 1_000)
    } else {
        window.to_string()
    }
}

/// Structurally render Factory's legacy custom selector
/// `custom:<display-name-slug>-<zero-based-index>`. The terminal decimal is
/// protocol bookkeeping, not part of the model name. This is presentation
/// only: callers must resolve configuration separately before treating the
/// selector as a canonical pricing identity.
pub(crate) fn display_factory_custom_selector(id: &str) -> Option<String> {
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
    fn model_label_source_preserves_specific_provider_names_and_fallbacks() {
        let labels = [
            (Some("Opus"), Some("claude-opus-5"), Some("claude-opus-5")),
            (Some("GPT-5.6 Sol"), Some("auto"), Some("auto")),
            (
                Some("DeepSeek V4 Pro"),
                None,
                Some("[DeepSeek] deepseek-v4-pro"),
            ),
            (Some("Opus"), None, Some("claude-opus-5")),
            (None, Some("claude-opus-5"), Some("claude-opus-4-8")),
            (Some("Opus"), None, None),
            (None, None, None),
        ]
        .map(|(display, id, launch)| {
            model_label_source(display, id, launch).map(|raw| model_label_parts(raw).0)
        });
        assert_eq!(
            labels,
            [
                Some("Opus 5"),
                Some("GPT 5.6 Sol"),
                Some("DeepSeek V4 Pro"),
                Some("Opus 5"),
                Some("Opus 5"),
                Some("Opus"),
                None
            ]
            .map(|label| label.map(str::to_owned))
        );
    }

    #[test]
    fn display_model_names_match_human_surfaces() {
        assert_eq!(display_model("claude-opus-4-8"), "Opus 4.8");
        assert_eq!(display_model("anthropic-claude-4-8"), "Claude 4.8");
        assert_eq!(display_model("gpt-5-codex"), "GPT 5 Codex");
        assert_eq!(display_model("gpt-5.5-codex"), "GPT 5.5 Codex");
        assert_eq!(display_model("gpt-6-astra"), "GPT 6 Astra");
        assert_eq!(display_model("claude-opus-4-7-20260101"), "Opus 4.7");
        assert_eq!(display_model("gpt-5-codex-20260101"), "GPT 5 Codex");
        assert_eq!(display_model("deepseek-v4-pro"), "DeepSeek V4 Pro");
        assert_eq!(display_model("mystery-model"), "Mystery Model");
    }

    #[test]
    fn model_label_parts_unify_slug_and_display_names() {
        for (slug, display, expected) in [
            ("claude-opus-5-5", "Claude Opus 5.5", "Opus 5.5"),
            ("gpt-6-astra", "GPT-6-Astra", "GPT 6 Astra"),
            ("gpt-5.5-codex", "GPT-5.5 Codex", "GPT 5.5 Codex"),
            ("claude-opus-4-8-20260101", "Opus 4.8", "Opus 4.8"),
        ] {
            assert_eq!(model_label_parts(slug), (expected.to_owned(), None));
            assert_eq!(model_label_parts(display), (expected.to_owned(), None));
        }
        for (raw, expected) in [
            ("Opus 4.8 (1M context)", "Opus 4.8"),
            ("Opus 4.8 (1M)", "Opus 4.8"),
            ("Sonnet 4.6 (200K context)", "Sonnet 4.6"),
            ("GPT-5.5", "GPT 5.5"),
            ("Sonnet 3.5 (New)", "Sonnet 3.5 (New)"),
            ("claude-opus-4-8", "Opus 4.8"),
            ("claude-opus-4-8-20260101", "Opus 4.8"),
            ("gpt-5.5-codex", "GPT 5.5 Codex"),
            ("GPT-5.5 Codex", "GPT 5.5 Codex"),
        ] {
            assert_eq!(model_label_parts(raw).0, expected);
        }
    }

    #[test]
    fn model_label_parts_separate_window_qualifiers() {
        for raw in [
            "claude-opus-5-5[1m]",
            "Claude Opus 5.5 (1M context)",
            "Opus 5.5 (1M)",
        ] {
            assert_eq!(
                model_label_parts(raw),
                ("Opus 5.5".to_owned(), Some("1m".to_owned()))
            );
        }
        assert_eq!(
            model_label_parts("gpt-6-astra[200k]"),
            ("GPT 6 Astra".to_owned(), Some("200k".to_owned()))
        );
        assert_eq!(
            model_label_parts("Sonnet 4.6 (200K context)"),
            ("Sonnet 4.6".to_owned(), Some("200k".to_owned()))
        );
        for raw in [
            "Sonnet 3.5 (New)",
            "Opus (é)",
            "Opus [é]",
            "Opus (18446744073709551615M)",
            "Opus (1.5M)",
        ] {
            assert_eq!(model_label_parts(raw), (raw.to_owned(), None));
        }
        assert_eq!(window_short(1_050_000), "1m");
        assert_eq!(window_short(272_000), "272k");
        assert_eq!(window_short(523), "523");
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
