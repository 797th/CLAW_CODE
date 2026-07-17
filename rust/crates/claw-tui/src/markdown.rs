use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::Theme;

/// Render the subset of Markdown most visible in a streaming assistant reply.
/// The output is owned `Line`s, so it can be retained in the transcript and
/// redrawn without touching stdout.
#[must_use]
pub fn render_markdown(markdown: &str, theme: Theme) -> Vec<Line<'static>> {
    let parser = Parser::new_ext(markdown, Options::all());
    let mut renderer = MarkdownRenderer::new(theme);
    for event in parser {
        renderer.event(event);
    }
    renderer.finish()
}

struct MarkdownRenderer {
    theme: Theme,
    lines: Vec<Line<'static>>,
    current: Vec<Span<'static>>,
    style: Style,
    code_block: bool,
    quote_depth: usize,
    list_depth: usize,
    list_markers: Vec<Option<u64>>,
    list_counters: Vec<u64>,
    code_language: Option<String>,
    table_depth: usize,
    table_header: bool,
    table_cell_index: usize,
}

impl MarkdownRenderer {
    fn new(theme: Theme) -> Self {
        Self {
            theme,
            lines: Vec::new(),
            current: Vec::new(),
            style: theme.base(),
            code_block: false,
            quote_depth: 0,
            list_depth: 0,
            list_markers: Vec::new(),
            list_counters: Vec::new(),
            code_language: None,
            table_depth: 0,
            table_header: false,
            table_cell_index: 0,
        }
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.text(text.as_ref()),
            Event::Code(text) => self.push_span(Span::styled(
                text.into_string(),
                Style::default()
                    .fg(self.theme.code)
                    .add_modifier(Modifier::BOLD),
            )),
            Event::SoftBreak | Event::HardBreak => self.break_line(),
            Event::Rule => {
                self.separate_block();
                self.lines.push(Line::from(Span::styled(
                    "  ─────────────────────────────────────────",
                    self.theme.border(),
                )));
            }
            Event::Html(text) | Event::InlineHtml(text) => self.text(text.as_ref()),
            Event::FootnoteReference(name) => {
                self.push_span(Span::styled(format!("[^{name}]"), self.theme.muted()))
            }
            Event::TaskListMarker(checked) => self.push_span(Span::styled(
                if checked { "☑ " } else { "☐ " },
                Style::default().fg(self.theme.accent),
            )),
            Event::InlineMath(text) | Event::DisplayMath(text) => {
                self.push_span(Span::styled(
                    text.into_string(),
                    self.theme.emphasis_style(),
                ));
            }
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Heading { level, .. } => {
                self.separate_block();
                let color = match level {
                    pulldown_cmark::HeadingLevel::H1 | pulldown_cmark::HeadingLevel::H2 => {
                        self.theme.heading
                    }
                    _ => self.theme.link,
                };
                self.style = self.theme.base().fg(color).add_modifier(Modifier::BOLD);
            }
            Tag::Paragraph => {
                if !self.current.is_empty() {
                    self.flush_line();
                }
                if self.table_depth == 0 && self.list_depth == 0 && self.quote_depth == 0 {
                    self.separate_block();
                }
                self.style = self.theme.base();
            }
            Tag::BlockQuote(_) => {
                if self.quote_depth == 0 {
                    self.separate_block();
                } else {
                    self.flush_line();
                }
                self.quote_depth += 1;
                self.push_span(Span::styled("│ ", self.theme.muted()));
                self.style = self.theme.base().fg(self.theme.quote);
            }
            Tag::CodeBlock(kind) => {
                self.separate_block();
                self.code_block = true;
                self.code_language = match kind {
                    CodeBlockKind::Fenced(language) if !language.is_empty() => {
                        Some(language.into_string())
                    }
                    _ => None,
                };
                if let Some(language) = &self.code_language {
                    self.lines.push(Line::from(Span::styled(
                        format!("  ┌─ {language}"),
                        self.theme.border(),
                    )));
                } else {
                    self.lines
                        .push(Line::from(Span::styled("  ┌─ code", self.theme.border())));
                }
            }
            Tag::List(start) => {
                if self.list_depth == 0 {
                    self.separate_block();
                } else {
                    self.flush_line();
                }
                self.list_depth += 1;
                self.list_markers.push(start);
                self.list_counters.push(start.unwrap_or(0));
            }
            Tag::Table(_) => {
                self.separate_block();
                self.table_depth += 1;
            }
            Tag::TableHead => {
                self.table_header = true;
            }
            Tag::TableRow => {
                self.flush_line();
                self.table_cell_index = 0;
                self.push_span(Span::styled("  │ ", self.theme.border()));
            }
            Tag::TableCell => {
                if self.table_cell_index > 0 {
                    self.push_span(Span::styled(" │ ", self.theme.border()));
                }
                self.table_cell_index += 1;
                self.style = if self.table_header {
                    self.theme
                        .base()
                        .fg(self.theme.heading)
                        .add_modifier(Modifier::BOLD)
                } else {
                    self.theme.base()
                };
            }
            Tag::Item => {
                self.flush_line();
                let indent = "  ".repeat(self.list_depth.saturating_sub(1));
                let marker = if self.list_markers.last().is_some_and(Option::is_some) {
                    let counter = self.list_counters.last_mut().expect("list counter");
                    let marker = format!("{indent}{}. ", *counter);
                    *counter = counter.saturating_add(1);
                    marker
                } else {
                    format!("{indent}• ")
                };
                self.push_span(Span::styled(marker, Style::default().fg(self.theme.accent)));
            }
            Tag::Emphasis => {
                self.style = self
                    .theme
                    .base()
                    .fg(self.theme.emphasis)
                    .add_modifier(Modifier::ITALIC);
            }
            Tag::Strong => {
                self.style = self
                    .theme
                    .base()
                    .fg(self.theme.strong)
                    .add_modifier(Modifier::BOLD);
            }
            Tag::Strikethrough => {
                self.style = self
                    .theme
                    .base()
                    .fg(self.theme.muted)
                    .add_modifier(Modifier::CROSSED_OUT);
            }
            Tag::Link { .. } => {
                self.style = self
                    .theme
                    .base()
                    .fg(self.theme.link)
                    .add_modifier(Modifier::UNDERLINED);
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(_) | TagEnd::Paragraph => {
                self.flush_line();
                self.style = self.theme.base();
            }
            TagEnd::BlockQuote(_) => {
                self.flush_line();
                self.quote_depth = self.quote_depth.saturating_sub(1);
                self.style = self.theme.base();
            }
            TagEnd::CodeBlock => {
                self.flush_line();
                while self.lines.last().is_some_and(|line| line.spans.is_empty()) {
                    self.lines.pop();
                }
                self.lines.push(Line::from(Span::styled(
                    "  └─────────────────────────────────────────",
                    self.theme.border(),
                )));
                self.code_block = false;
                self.code_language = None;
                self.style = self.theme.base();
            }
            TagEnd::List(_) => {
                self.flush_line();
                self.list_depth = self.list_depth.saturating_sub(1);
                self.list_markers.pop();
                self.list_counters.pop();
            }
            TagEnd::Item => self.flush_line(),
            TagEnd::TableHead => {
                self.table_header = false;
                self.lines.push(Line::from(Span::styled(
                    "  ├────────────────────────────────────────",
                    self.theme.border(),
                )));
            }
            TagEnd::TableRow => {
                self.push_span(Span::styled(" │", self.theme.border()));
                self.flush_line();
                self.table_cell_index = 0;
            }
            TagEnd::TableCell => {
                self.style = self.theme.base();
            }
            TagEnd::Table => {
                self.separate_block();
                self.table_depth = self.table_depth.saturating_sub(1);
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link => {
                self.style = if self.quote_depth > 0 {
                    self.theme.base().fg(self.theme.quote)
                } else {
                    self.theme.base()
                };
            }
            _ => {}
        }
    }

    fn text(&mut self, text: &str) {
        for (index, part) in text.split('\n').enumerate() {
            if index > 0 {
                self.break_line();
            }
            if part.is_empty() {
                continue;
            }
            let mut style = self.style;
            if self.code_block {
                style = Style::default().fg(self.theme.code);
                if self.current.is_empty() {
                    self.push_span(Span::styled("  │ ", self.theme.border()));
                }
            } else if self.quote_depth > 0 && self.current.is_empty() {
                self.push_span(Span::styled("│ ", self.theme.muted()));
            }
            self.push_span(Span::styled(part.to_string(), style));
        }
    }

    fn push_span(&mut self, span: Span<'static>) {
        self.current.push(span);
    }

    fn flush_line(&mut self) {
        if !self.current.is_empty() {
            self.lines
                .push(Line::from(std::mem::take(&mut self.current)));
        }
    }

    fn break_line(&mut self) {
        if self.current.is_empty() {
            self.lines.push(Line::default());
        } else {
            self.flush_line();
        }
    }

    fn separate_block(&mut self) {
        self.flush_line();
        if !self.lines.is_empty() && !self.lines.last().is_some_and(|line| line.spans.is_empty()) {
            self.lines.push(Line::default());
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.flush_line();
        while self.lines.last().is_some_and(|line| line.spans.is_empty()) {
            self.lines.pop();
        }
        self.lines
    }
}

trait ThemeStyleExt {
    fn emphasis_style(self) -> Style;
}

impl ThemeStyleExt for Theme {
    fn emphasis_style(self) -> Style {
        self.base().fg(self.emphasis).add_modifier(Modifier::ITALIC)
    }
}

#[cfg(test)]
mod tests {
    use super::render_markdown;
    use crate::theme::Theme;

    #[test]
    fn preserves_markdown_content_and_design_accents() {
        let lines = render_markdown(
            "# Heading\n\nText with **important** and `code`.\n\n> quoted",
            Theme::default(),
        );
        let rendered = lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("Heading"));
        assert!(rendered.contains("important"));
        assert!(rendered.contains("code"));
        assert!(rendered.contains("quoted"));
    }

    #[test]
    fn preserves_block_spacing_without_a_trailing_blank_line() {
        let lines = render_markdown("First paragraph.\n\nSecond paragraph.", Theme::default());

        assert_eq!(
            lines.iter().filter(|line| line.spans.is_empty()).count(),
            1,
            "separate paragraphs need one readable blank line"
        );
        assert!(lines.first().is_some_and(|line| !line.spans.is_empty()));
        assert!(lines.last().is_some_and(|line| !line.spans.is_empty()));
    }

    #[test]
    fn empty_markdown_does_not_create_a_phantom_row() {
        assert!(render_markdown("", Theme::default()).is_empty());
    }

    #[test]
    fn tables_keep_cell_boundaries_readable() {
        let lines = render_markdown(
            "| Area | Finding |\n| --- | --- |\n| API | Missing docs |\n| TUI | Hard to scan |",
            Theme::default(),
        );
        let rendered = lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("Area"));
        assert!(rendered.contains("Finding"));
        assert!(
            rendered.contains("│"),
            "table cells need visible boundaries"
        );
        assert!(rendered.contains("API"));
        assert!(rendered.contains("Hard to scan"));
    }

    #[test]
    fn ordered_lists_keep_their_numbers() {
        let lines = render_markdown("15. first item\n16. second item", Theme::default());
        let rendered = lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("15. first item"));
        assert!(rendered.contains("16. second item"));
    }
}
