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
    code_language: Option<String>,
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
            code_language: None,
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
            Event::SoftBreak | Event::HardBreak => self.flush_line(),
            Event::Rule => {
                self.flush_line();
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
                self.flush_line();
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
                self.style = self.theme.base();
            }
            Tag::BlockQuote(_) => {
                self.flush_line();
                self.quote_depth += 1;
                self.push_span(Span::styled("│ ", self.theme.muted()));
                self.style = self.theme.base().fg(self.theme.quote);
            }
            Tag::CodeBlock(kind) => {
                self.flush_line();
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
            Tag::List(_) => {
                self.flush_line();
                self.list_depth += 1;
            }
            Tag::Item => {
                self.flush_line();
                self.push_span(Span::styled(
                    format!("{}• ", "  ".repeat(self.list_depth.saturating_sub(1))),
                    Style::default().fg(self.theme.accent),
                ));
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
            }
            TagEnd::Item => self.flush_line(),
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
                self.flush_line();
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
        if self.current.is_empty() {
            if self.lines.last().is_some_and(|line| line.spans.is_empty()) {
                return;
            }
            self.lines.push(Line::default());
        } else {
            self.lines
                .push(Line::from(std::mem::take(&mut self.current)));
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        if !self.current.is_empty() || self.lines.is_empty() {
            self.flush_line();
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
}
