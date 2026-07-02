use super::chat::paint_mentions_with;
use super::*;

pub(super) const MAX_CARD_WIDTH: usize = 100;
pub(super) const MIN_CARD_CONTENT_WIDTH: usize = 24;

pub(super) struct StyledFragment {
    text: String,
    style: Option<anstyle::Style>,
    mentions: bool,
}

impl StyledFragment {
    pub(super) fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: None,
            mentions: false,
        }
    }

    pub(super) fn styled(text: impl Into<String>, style: anstyle::Style) -> Self {
        Self {
            text: text.into(),
            style: Some(style),
            mentions: false,
        }
    }

    pub(super) fn prose(text: impl Into<String>, style: Option<anstyle::Style>) -> Self {
        Self {
            text: text.into(),
            style,
            mentions: true,
        }
    }
}

#[derive(Clone)]
pub(super) struct WrapToken {
    text: String,
    style: Option<anstyle::Style>,
    mentions: bool,
}

pub(super) fn card_content_width() -> usize {
    let terminal = render::terminal_columns(MAX_CARD_WIDTH).min(MAX_CARD_WIDTH);
    let prefix_width = UnicodeWidthStr::width(format!("{BODY_INDENT}▌ ").as_str());
    terminal
        .saturating_sub(prefix_width)
        .max(MIN_CARD_CONTENT_WIDTH)
}

pub(super) fn write_wrapped_spine_fragments(
    out: &mut impl Write,
    answered: bool,
    fragments: Vec<StyledFragment>,
    hang_indent: &str,
) -> Result<()> {
    write_wrapped_spine_fragments_with_first_indent(out, answered, fragments, "", hang_indent)
}

pub(super) fn write_wrapped_spine_fragments_with_first_indent(
    out: &mut impl Write,
    answered: bool,
    fragments: Vec<StyledFragment>,
    first_indent: &str,
    hang_indent: &str,
) -> Result<()> {
    for line in wrap_fragments(fragments, card_content_width(), first_indent, hang_indent) {
        write_spine_fragments(out, answered, &line)?;
    }
    Ok(())
}

pub(super) fn wrap_fragments(
    fragments: Vec<StyledFragment>,
    width: usize,
    first_indent: &str,
    hang_indent: &str,
) -> Vec<Vec<StyledFragment>> {
    let tokens = fragment_tokens(fragments);
    if tokens.is_empty() {
        return vec![if first_indent.is_empty() {
            Vec::new()
        } else {
            vec![StyledFragment::plain(first_indent)]
        }];
    }

    let mut lines = Vec::new();
    let first_width = UnicodeWidthStr::width(first_indent);
    let mut current = if first_indent.is_empty() {
        Vec::new()
    } else {
        vec![StyledFragment::plain(first_indent)]
    };
    let mut current_width = first_width;
    let mut has_word = false;
    let hang_width = UnicodeWidthStr::width(hang_indent);

    for token in tokens {
        let token_width = UnicodeWidthStr::width(token.text.as_str());
        let separator_width = usize::from(has_word);
        if has_word && current_width + separator_width + token_width > width {
            lines.push(current);
            current = Vec::new();
            current_width = 0;
            has_word = false;
            if !hang_indent.is_empty() {
                current.push(StyledFragment::plain(hang_indent));
                current_width = hang_width;
            }
        }
        if has_word {
            current.push(StyledFragment::plain(" "));
            current_width += 1;
        }
        current.push(StyledFragment {
            text: token.text,
            style: token.style,
            mentions: token.mentions,
        });
        current_width += token_width;
        has_word = true;
    }
    lines.push(current);
    lines
}

pub(super) fn fragment_tokens(fragments: Vec<StyledFragment>) -> Vec<WrapToken> {
    fragments
        .into_iter()
        .flat_map(|fragment| {
            fragment
                .text
                .split_whitespace()
                .map(|word| WrapToken {
                    text: word.to_owned(),
                    style: fragment.style,
                    mentions: fragment.mentions,
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

pub(super) fn write_spine_fragments(
    out: &mut impl Write,
    answered: bool,
    fragments: &[StyledFragment],
) -> Result<()> {
    let style = if answered {
        render::palette::FAINT
    } else {
        render::palette::WARN
    };
    write!(out, "{BODY_INDENT}{}", render::paint(style, "▌ "))?;
    for fragment in fragments {
        if fragment.mentions {
            write!(
                out,
                "{}",
                paint_mentions_with(&fragment.text, fragment.style)
            )?;
        } else if let Some(style) = fragment.style {
            write!(out, "{}", render::paint(style, &fragment.text))?;
        } else {
            write!(out, "{}", fragment.text)?;
        }
    }
    writeln!(out)?;
    Ok(())
}

pub(super) fn write_spine_blank(out: &mut impl Write, answered: bool) -> Result<()> {
    let style = if answered {
        render::palette::FAINT
    } else {
        render::palette::WARN
    };
    writeln!(out, "{BODY_INDENT}{}", render::paint(style, "▌"))?;
    Ok(())
}
