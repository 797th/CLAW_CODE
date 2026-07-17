use ratatui::style::{Color, Modifier, Style};

/// Claw Code's visual signature: cool cyan and indigo accents, warm orange
/// emphasis, mint code, and slate structure. Keep this small and explicit so
/// the full-screen renderer can evolve independently from the inline renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub background: Color,
    pub surface: Color,
    pub panel: Color,
    pub text: Color,
    pub muted: Color,
    pub border: Color,
    pub accent: Color,
    pub heading: Color,
    pub emphasis: Color,
    pub strong: Color,
    pub code: Color,
    pub link: Color,
    pub quote: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            background: Color::Rgb(15, 23, 42),
            surface: Color::Rgb(20, 29, 50),
            panel: Color::Rgb(30, 41, 59),
            text: Color::Rgb(226, 232, 240),
            muted: Color::Rgb(148, 163, 184),
            border: Color::Rgb(71, 85, 105),
            accent: Color::Rgb(129, 140, 248),
            heading: Color::Rgb(103, 232, 249),
            emphasis: Color::Rgb(196, 181, 253),
            strong: Color::Rgb(253, 186, 116),
            code: Color::Rgb(134, 239, 172),
            link: Color::Rgb(147, 197, 253),
            quote: Color::Rgb(148, 163, 184),
            success: Color::Rgb(74, 222, 128),
            warning: Color::Rgb(250, 204, 21),
            error: Color::Rgb(248, 113, 113),
        }
    }
}

impl Theme {
    #[must_use]
    pub fn base(self) -> Style {
        Style::default().fg(self.text).bg(self.background)
    }

    #[must_use]
    pub fn border(self) -> Style {
        Style::default().fg(self.border)
    }

    #[must_use]
    pub fn title(self) -> Style {
        Style::default()
            .fg(self.heading)
            .add_modifier(Modifier::BOLD)
    }

    #[must_use]
    pub fn muted(self) -> Style {
        Style::default().fg(self.muted)
    }

    #[must_use]
    pub fn prompt(self) -> Style {
        Style::default()
            .fg(self.accent)
            .add_modifier(Modifier::BOLD)
    }

    #[must_use]
    pub fn caret(self) -> Style {
        Style::default().fg(Color::White).bg(Color::White)
    }
}
