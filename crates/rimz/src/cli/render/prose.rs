use pulldown_cmark::{Alignment, Event, LinkType, Options, Parser, Tag, TagEnd};
use unicode_width::UnicodeWidthStr;

use super::{Table, cell, paint, palette, terminal_columns};

const MAX_PROSE_WIDTH: usize = 100;
const MIN_PROSE_WIDTH: usize = 24;

/// How agent-authored text reaches a human-facing output surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Prose {
    Raw,
    Markdown,
}

impl Prose {
    /// Render markdown only when stdout will preserve terminal styling.
    pub(crate) fn for_stdout() -> Self {
        if anstream::AutoStream::choice(&std::io::stdout()) == anstream::ColorChoice::Never {
            Self::Raw
        } else {
            Self::Markdown
        }
    }

    pub(crate) fn lines(self, text: &str, width: usize) -> Vec<String> {
        self.lines_with_style(text, width, None)
    }

    pub(crate) fn lines_with_style(
        self,
        text: &str,
        width: usize,
        base_style: Option<anstyle::Style>,
    ) -> Vec<String> {
        match self {
            Self::Raw => text.lines().map(ToOwned::to_owned).collect(),
            Self::Markdown => Layout::render(text, width.max(1), base_style),
        }
    }
}

/// Body width for prose following a fixed-width caller-owned prefix.
pub(crate) fn prose_width(prefix_width: usize) -> usize {
    terminal_columns(MAX_PROSE_WIDTH)
        .min(MAX_PROSE_WIDTH)
        .saturating_sub(prefix_width)
        .max(MIN_PROSE_WIDTH)
}

pub(crate) struct StyledFragment {
    text: String,
    style: Option<anstyle::Style>,
    mentions: bool,
}

impl StyledFragment {
    pub(crate) fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: None,
            mentions: false,
        }
    }

    pub(crate) fn styled(text: impl Into<String>, style: anstyle::Style) -> Self {
        Self {
            text: text.into(),
            style: Some(style),
            mentions: false,
        }
    }

    pub(crate) fn prose(text: impl Into<String>, style: Option<anstyle::Style>) -> Self {
        Self {
            text: text.into(),
            style,
            mentions: true,
        }
    }

    pub(crate) fn paint(&self) -> String {
        if self.mentions {
            paint_mentions_with(&self.text, self.style)
        } else if let Some(style) = self.style {
            paint(style, &self.text)
        } else {
            self.text.clone()
        }
    }
}

#[derive(Clone)]
struct WrapToken {
    text: String,
    style: Option<anstyle::Style>,
    mentions: bool,
}

pub(crate) fn wrap_fragments(
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

fn fragment_tokens(fragments: Vec<StyledFragment>) -> Vec<WrapToken> {
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

pub(crate) fn paint_mentions_with(line: &str, base_style: Option<anstyle::Style>) -> String {
    let mut rendered = String::new();
    let mut index = 0;
    while index < line.len() {
        let ch = line[index..]
            .chars()
            .next()
            .expect("index stays on char boundary");
        if matches!(ch, '@' | '#') && mention_boundary(line, index) {
            let token_start = index + ch.len_utf8();
            let mut token_end = token_start;
            for (offset, token_ch) in line[token_start..].char_indices() {
                if is_mention_char(token_ch) {
                    token_end = token_start + offset + token_ch.len_utf8();
                } else {
                    break;
                }
            }
            let mut paint_end = token_end;
            while paint_end > token_start {
                let tail = line[..paint_end]
                    .chars()
                    .next_back()
                    .expect("paint_end stays on char boundary");
                if matches!(tail, '.' | ',' | ';' | ':' | '!' | '?' | ')') {
                    paint_end -= tail.len_utf8();
                } else {
                    break;
                }
            }
            if paint_end > token_start {
                push_painted(&mut rendered, base_style, &line[..index]);
                rendered.push_str(&paint(palette::cool().bold(), &line[index..paint_end]));
                push_painted(&mut rendered, base_style, &line[paint_end..token_end]);
                rendered.push_str(&paint_mentions_with(&line[token_end..], base_style));
                return rendered;
            }
        }
        index += ch.len_utf8();
    }
    push_painted(&mut rendered, base_style, line);
    rendered
}

fn push_painted(rendered: &mut String, style: Option<anstyle::Style>, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(style) = style {
        rendered.push_str(&paint(style, text));
    } else {
        rendered.push_str(text);
    }
}

fn mention_boundary(line: &str, index: usize) -> bool {
    index == 0
        || line[..index]
            .chars()
            .next_back()
            .is_some_and(|ch| ch.is_whitespace() || ch == '(')
}

fn is_mention_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '/' | '-')
}

struct Layout {
    width: usize,
    lines: Vec<String>,
    inline: Vec<StyledFragment>,
    styles: Vec<anstyle::Style>,
    lists: Vec<ListState>,
    items: Vec<ItemState>,
    quote_depth: usize,
    code: Option<String>,
    link: Option<LinkState>,
    image_depth: usize,
    table: Option<TableBuild>,
}

struct ListState {
    next: Option<u64>,
}

struct ItemState {
    indent: String,
    marker: String,
    emitted: bool,
}

struct LinkState {
    start: usize,
    destination: String,
    kind: LinkType,
}

struct TableBuild {
    alignments: Vec<Alignment>,
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    cell: String,
    heading: bool,
}

impl Layout {
    fn render(text: &str, width: usize, base_style: Option<anstyle::Style>) -> Vec<String> {
        let options =
            Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
        let mut layout = Self {
            width,
            lines: Vec::new(),
            inline: Vec::new(),
            styles: vec![base_style.unwrap_or_default()],
            lists: Vec::new(),
            items: Vec::new(),
            quote_depth: 0,
            code: None,
            link: None,
            image_depth: 0,
            table: None,
        };
        for event in Parser::new_ext(text, options) {
            layout.event(event);
        }
        layout.flush_inline();
        while layout.lines.last().is_some_and(String::is_empty) {
            layout.lines.pop();
        }
        layout.lines
    }

    fn event(&mut self, event: Event<'_>) {
        if self.table.is_some() {
            self.table_event(event);
            return;
        }
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                if let Some(code) = self.code.as_mut() {
                    code.push_str(&text);
                } else {
                    self.push_prose(&text);
                }
            }
            Event::Code(text) => self
                .inline
                .push(StyledFragment::styled(text, palette::accent())),
            Event::InlineMath(text) | Event::DisplayMath(text) => self.push_prose(&text),
            Event::SoftBreak => self.push_prose(" "),
            Event::HardBreak => self.flush_inline(),
            Event::Rule => {
                self.flush_inline();
                let rule_width = self.available_width().max(1);
                self.emit_preformatted(vec![paint(palette::rule(), &"─".repeat(rule_width))]);
                self.push_blank();
            }
            Event::TaskListMarker(checked) => {
                if let Some(item) = self.items.last_mut()
                    && !item.emitted
                {
                    item.marker = if checked { "☑ " } else { "☐ " }.to_owned();
                }
            }
            Event::FootnoteReference(label) => self.push_prose(&format!("[{label}]")),
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { .. } => {
                self.push_blank();
                self.push_style(if self.styles[0] == anstyle::Style::new() {
                    palette::header().bold()
                } else {
                    self.current_style().bold()
                });
            }
            Tag::BlockQuote(_) => {
                self.flush_inline();
                self.quote_depth += 1;
            }
            Tag::CodeBlock(_) => {
                self.flush_inline();
                self.code = Some(String::new());
            }
            Tag::HtmlBlock => {}
            Tag::List(start) => {
                self.flush_inline();
                self.lists.push(ListState { next: start });
            }
            Tag::Item => {
                let depth = self.lists.len().saturating_sub(1);
                let marker = match self.lists.last_mut().and_then(|list| list.next.as_mut()) {
                    Some(next) => {
                        let marker = format!("{next}. ");
                        *next += 1;
                        marker
                    }
                    None => "• ".to_owned(),
                };
                self.items.push(ItemState {
                    indent: "  ".repeat(depth),
                    marker,
                    emitted: false,
                });
            }
            Tag::Table(alignments) => {
                self.flush_inline();
                self.table = Some(TableBuild {
                    alignments,
                    headers: Vec::new(),
                    rows: Vec::new(),
                    row: Vec::new(),
                    cell: String::new(),
                    heading: false,
                });
            }
            Tag::Emphasis => self.push_style(self.current_style().italic()),
            Tag::Strong => self.push_style(self.current_style().bold()),
            Tag::Strikethrough => self.push_style(self.current_style().strikethrough()),
            Tag::Link {
                link_type,
                dest_url,
                ..
            } if self.image_depth == 0 => {
                self.link = Some(LinkState {
                    start: self.inline.len(),
                    destination: dest_url.into_string(),
                    kind: link_type,
                });
            }
            Tag::Image { .. } => self.image_depth += 1,
            Tag::Superscript
            | Tag::Subscript
            | Tag::FootnoteDefinition(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::TableHead
            | Tag::TableRow
            | Tag::TableCell
            | Tag::Link { .. }
            | Tag::MetadataBlock(_) => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush_inline();
                self.push_blank();
            }
            TagEnd::Heading(_) => {
                self.flush_inline();
                self.pop_style();
                self.push_blank();
            }
            TagEnd::BlockQuote(_) => {
                self.flush_inline();
                self.quote_depth = self.quote_depth.saturating_sub(1);
                if self.quote_depth == 0 {
                    self.push_blank();
                }
            }
            TagEnd::CodeBlock => {
                let code = self.code.take().unwrap_or_default();
                let lines = code
                    .split_terminator('\n')
                    .map(|line| format!("    {}", paint(palette::accent(), line)))
                    .collect::<Vec<_>>();
                self.emit_preformatted(if lines.is_empty() {
                    vec!["    ".to_owned()]
                } else {
                    lines
                });
                self.push_blank();
            }
            TagEnd::HtmlBlock => {
                self.flush_inline();
                self.push_blank();
            }
            TagEnd::List(_) => {
                self.flush_inline();
                self.lists.pop();
                if self.lists.is_empty() {
                    self.push_blank();
                }
            }
            TagEnd::Item => {
                self.flush_inline();
                self.items.pop();
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => self.pop_style(),
            TagEnd::Link => self.finish_link(),
            TagEnd::Image => self.image_depth = self.image_depth.saturating_sub(1),
            TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::FootnoteDefinition
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::Table
            | TagEnd::TableHead
            | TagEnd::TableRow
            | TagEnd::TableCell
            | TagEnd::MetadataBlock(_) => {}
        }
    }

    fn push_prose(&mut self, text: &str) {
        self.inline
            .push(StyledFragment::prose(text, self.style_option()));
    }

    fn push_style(&mut self, style: anstyle::Style) {
        self.styles.push(style);
    }

    fn pop_style(&mut self) {
        if self.styles.len() > 1 {
            self.styles.pop();
        }
    }

    fn current_style(&self) -> anstyle::Style {
        self.styles.last().copied().unwrap_or_default()
    }

    fn style_option(&self) -> Option<anstyle::Style> {
        let style = self.current_style();
        (style != anstyle::Style::new()).then_some(style)
    }

    fn finish_link(&mut self) {
        let Some(link) = self.link.take() else {
            return;
        };
        let label = self.inline[link.start..]
            .iter()
            .map(|fragment| fragment.text.as_str())
            .collect::<String>();
        if matches!(link.kind, LinkType::Autolink | LinkType::Email)
            || label.trim() == link.destination
        {
            return;
        }
        self.inline.push(StyledFragment::styled(
            format!(" ({})", link.destination),
            palette::faint(),
        ));
    }

    fn flush_inline(&mut self) {
        if self.inline.is_empty() {
            return;
        }
        let quote = self.quote_prefix();
        let quote_width = self.quote_depth * UnicodeWidthStr::width("▌ ");
        let (first_indent, hang_indent) = self.item_indents();
        let lines = wrap_fragments(
            std::mem::take(&mut self.inline),
            self.width.saturating_sub(quote_width).max(1),
            &first_indent,
            &hang_indent,
        );
        for fragments in lines {
            let body = fragments
                .iter()
                .map(StyledFragment::paint)
                .collect::<String>();
            self.lines.push(format!("{quote}{body}"));
        }
    }

    fn emit_preformatted(&mut self, lines: Vec<String>) {
        let quote = self.quote_prefix();
        let (first_indent, hang_indent) = self.item_indents();
        for (index, line) in lines.into_iter().enumerate() {
            let indent = if index == 0 {
                &first_indent
            } else {
                &hang_indent
            };
            self.lines.push(format!("{quote}{indent}{line}"));
        }
    }

    fn item_indents(&mut self) -> (String, String) {
        let Some(item) = self.items.last_mut() else {
            return (String::new(), String::new());
        };
        let prefix = format!("{}{}", item.indent, item.marker);
        let hang = " ".repeat(UnicodeWidthStr::width(prefix.as_str()));
        if item.emitted {
            (hang.clone(), hang)
        } else {
            item.emitted = true;
            (prefix, hang)
        }
    }

    fn quote_prefix(&self) -> String {
        paint(palette::muted(), "▌ ").repeat(self.quote_depth)
    }

    fn available_width(&self) -> usize {
        self.width
            .saturating_sub(self.quote_depth * UnicodeWidthStr::width("▌ "))
    }

    fn push_blank(&mut self) {
        if self.lines.last().is_some_and(|line| !line.is_empty()) {
            self.lines.push(String::new());
        }
    }

    fn table_event(&mut self, event: Event<'_>) {
        let mut finish = false;
        if let Some(table) = self.table.as_mut() {
            match event {
                Event::Start(Tag::TableHead) => table.heading = true,
                Event::Start(Tag::TableRow) => table.row.clear(),
                Event::Start(Tag::TableCell) => table.cell.clear(),
                Event::End(TagEnd::TableCell) => table.row.push(table.cell.trim().to_owned()),
                Event::End(TagEnd::TableHead) => {
                    table.headers = std::mem::take(&mut table.row);
                    table.heading = false;
                }
                Event::End(TagEnd::TableRow) => {
                    if table.heading {
                        table.headers = std::mem::take(&mut table.row);
                    } else {
                        table.rows.push(std::mem::take(&mut table.row));
                    }
                }
                Event::Text(text)
                | Event::Code(text)
                | Event::Html(text)
                | Event::InlineHtml(text)
                | Event::InlineMath(text)
                | Event::DisplayMath(text) => table.cell.push_str(&text),
                Event::SoftBreak | Event::HardBreak => table.cell.push(' '),
                Event::TaskListMarker(checked) => {
                    table.cell.push_str(if checked { "☑ " } else { "☐ " });
                }
                Event::FootnoteReference(label) => {
                    table.cell.push_str(&format!("[{label}]"));
                }
                Event::End(TagEnd::Table) => finish = true,
                Event::Rule => table.cell.push('─'),
                Event::Start(_) | Event::End(_) => {}
            }
        }
        if finish {
            self.finish_table();
        }
    }

    fn finish_table(&mut self) {
        let Some(build) = self.table.take() else {
            return;
        };
        let right = build
            .alignments
            .iter()
            .enumerate()
            .filter_map(|(index, alignment)| (*alignment == Alignment::Right).then_some(index))
            .collect::<Vec<_>>();
        let mut table = Table::new(build.headers)
            .right(&right)
            .max_width(self.available_width());
        for row in build.rows {
            table.row(row.into_iter().map(cell));
        }
        let mut rendered = Vec::new();
        table
            .render(&mut rendered)
            .expect("rendering a table into memory cannot fail");
        let rendered = String::from_utf8(rendered).expect("table rendering is utf-8");
        self.emit_preformatted(rendered.lines().map(ToOwned::to_owned).collect());
        self.push_blank();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: Vec<String>) -> String {
        anstream::adapter::strip_str(&lines.join("\n")).to_string()
    }

    #[test]
    fn raw_prose_keeps_markdown_syntax_and_lines() {
        assert_eq!(
            Prose::Raw.lines("**bold**\n\n- item", 20),
            vec!["**bold**", "", "- item"]
        );
    }

    #[test]
    fn markdown_styles_and_wraps_blocks() {
        let rendered = plain(Prose::Markdown.lines(
            "# Heading\n\nA **bold** line for @coder.\n\n- item with enough words to wrap",
            24,
        ));
        assert_eq!(
            rendered,
            "Heading\n\nA bold line for @coder.\n\n• item with enough words\n  to wrap"
        );
    }

    #[test]
    fn markdown_preserves_code_and_indents_quotes() {
        let rendered =
            plain(Prose::Markdown.lines("> quoted text\n\n```rust\nlet value = 1;\n```", 40));
        assert_eq!(rendered, "▌ quoted text\n\n    let value = 1;");
    }

    #[test]
    fn markdown_indents_nested_lists_and_spaces_loose_items() {
        let nested = plain(Prose::Markdown.lines("3. outer\n   - inner", 40));
        assert_eq!(nested, "3. outer\n  • inner");

        let loose = plain(Prose::Markdown.lines("- first\n\n- second", 40));
        assert_eq!(loose, "• first\n\n• second");
    }

    #[test]
    fn markdown_renders_links_tasks_and_tables() {
        let rendered = plain(Prose::Markdown.lines(
            "[docs](https://example.com)\n\n- [x] done\n\n| Name | Count |\n| --- | ---: |\n| one | 2 |",
            60,
        ));
        assert!(rendered.contains("docs (https://example.com)"));
        assert!(rendered.contains("☑ done"));
        assert!(rendered.contains("Name  Count\none       2"));
    }

    #[test]
    fn markdown_paints_mentions_but_not_inline_code() {
        let lines = Prose::Markdown.lines("@coder and `@reviewer`", 40);
        assert!(lines[0].contains("\u{1b}["));
        let plain = plain(lines);
        assert_eq!(plain, "@coder and @reviewer");
    }
}
