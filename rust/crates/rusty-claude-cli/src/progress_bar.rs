use crossterm::cursor::MoveToColumn;
use crossterm::style::{Print, ResetColor, SetForegroundColor};
use crossterm::terminal::{Clear, ClearType};
use crossterm::{execute, queue};
use std::io::{self, Write};
use std::time::Duration;

use crate::render::ColorTheme;

/// A progress bar that fills from left to right as compaction rounds complete.
///
/// Example:
/// ```text
/// Compacting session [██████░░░░] 2/4 (preserving 2 recent messages)...
/// ```
pub struct ProgressBar {
    /// Total number of steps (e.g. max compaction rounds).
    total: usize,
    /// How many steps have completed so far.
    current: usize,
    /// Label prefix (e.g. "Compacting session").
    label: String,
    /// Detail suffix shown in parens (e.g. "preserving 4 recent messages").
    detail: String,
    /// Bar width in characters (the actual block bars, not the brackets).
    bar_width: usize,
}

impl ProgressBar {
    /// Create a new progress bar that advances `total` steps.
    ///
    /// `detail` is the suffix shown in parentheses after the bar.
    /// It should be updated by the caller as compaction rounds advance.
    pub fn new(label: impl Into<String>, detail: impl Into<String>, total: usize) -> Self {
        Self {
            total: total.max(1),
            current: 0,
            label: label.into(),
            detail: detail.into(),
            bar_width: 20,
        }
    }

    /// Update the current step and detail. Call this once per compaction round.
    ///
    /// Returns a `ProgressBar` that can be `finished` or `failed`.
    pub fn advance(&mut self, detail: impl Into<String>) {
        self.current = self.current.saturating_add(1);
        self.detail = detail.into();
    }

    /// Render the progress bar to `out`. Call this after updating `advance` or `set_detail`.
    pub fn render(&self, out: &mut impl Write, theme: &ColorTheme) -> io::Result<()> {
        let filled = self.current;
        let remaining = self.total.saturating_sub(filled);
        let filled_width = self.bar_width * filled / self.total;
        let empty_width = self.bar_width.saturating_sub(filled_width);

        // Build the bar string.
        let filled_chars = "█".repeat(filled_width);
        let empty_chars = "░".repeat(empty_width);
        let bar = format!("[{}{}]", filled_chars, empty_chars);

        // Build the full line.
        let line = format!(
            "  {} {} ({}/{}) {}",
            self.label, bar, self.current, self.total, self.detail
        );

        execute!(
            out,
            MoveToColumn(0),
            Clear(ClearType::CurrentLine),
            Print(&line)
        )?;
        out.flush()
    }

    /// Finish the progress bar with a checkmark.
    pub fn finish(&self, out: &mut impl Write, theme: &ColorTheme) -> io::Result<()> {
        execute!(
            out,
            MoveToColumn(0),
            Clear(ClearType::CurrentLine),
            SetForegroundColor(theme.spinner_done),
            Print(format!("  ✔ {} finished\n", self.label)),
            ResetColor
        )?;
        out.flush()
    }

    /// Fail the progress bar with an X.
    pub fn fail(&self, out: &mut impl Write, theme: &ColorTheme) -> io::Result<()> {
        execute!(
            out,
            MoveToColumn(0),
            Clear(ClearType::CurrentLine),
            SetForegroundColor(theme.spinner_failed),
            Print(format!("  ✘ {} failed\n", self.label)),
            ResetColor
        )?;
        out.flush()
    }

    /// Clear the progress bar line.
    pub fn clear(&self, out: &mut impl Write) -> io::Result<()> {
        execute!(out, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
        out.flush()
    }
}
