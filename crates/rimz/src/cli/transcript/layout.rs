use super::*;
pub(super) use crate::cli::render::prose::StyledFragment;
use crate::cli::render::prose::wrap_fragments;

pub(super) const MAX_CARD_WIDTH: usize = 100;
pub(super) const MIN_CARD_CONTENT_WIDTH: usize = 24;

pub(super) fn card_content_width() -> usize {
    let terminal = render::terminal_columns(MAX_CARD_WIDTH).min(MAX_CARD_WIDTH);
    let prefix_width = UnicodeWidthStr::width("│ ");
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

pub(super) fn write_spine_fragments(
    out: &mut impl Write,
    answered: bool,
    fragments: &[StyledFragment],
) -> Result<()> {
    let style = if answered {
        render::palette::faint()
    } else {
        render::palette::warn()
    };
    write!(out, "{}", render::paint(style, "│ "))?;
    for fragment in fragments {
        write!(out, "{}", fragment.paint())?;
    }
    writeln!(out)?;
    Ok(())
}

pub(super) fn write_spine_blank(out: &mut impl Write, answered: bool) -> Result<()> {
    let style = if answered {
        render::palette::faint()
    } else {
        render::palette::warn()
    };
    writeln!(out, "{}", render::paint(style, "│"))?;
    Ok(())
}
