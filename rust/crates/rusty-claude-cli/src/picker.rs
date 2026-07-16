//! Interactive arrow-key list picker used by REPL commands such as `/model`.
//!
//! The navigation logic lives in [`PickerState`], which is pure and
//! unit-tested. [`select_from_list`] drives the terminal event loop on top of
//! it. Both `Esc` and `Ctrl+C` return [`PickerOutcome::Cancelled`] so the user
//! drops back into the REPL without quitting the application. (`Cmd+C` is the
//! terminal emulator's copy shortcut and cannot be intercepted.)

use std::env;
use std::io::{self, IsTerminal, Write};

use crossterm::cursor;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor};
use crossterm::terminal::{self, Clear, ClearType};
use crossterm::{queue, QueueableCommand};

/// One selectable row: a primary `label` plus an optional dimmed `detail`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerItem {
    label: String,
    detail: String,
}

impl PickerItem {
    #[must_use]
    pub fn new(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            detail: detail.into(),
        }
    }
}

/// Pure selection state for an arrow-key list picker. Navigation wraps at both
/// ends. Kept free of I/O so the logic is unit-testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerState {
    len: usize,
    selected: usize,
}

impl PickerState {
    #[must_use]
    pub fn new(len: usize, initial: usize) -> Self {
        let selected = if len == 0 { 0 } else { initial.min(len - 1) };
        Self { len, selected }
    }

    #[must_use]
    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn move_up(&mut self) {
        if self.len == 0 {
            return;
        }
        self.selected = if self.selected == 0 {
            self.len - 1
        } else {
            self.selected - 1
        };
    }

    pub fn move_down(&mut self) {
        if self.len == 0 {
            return;
        }
        self.selected = (self.selected + 1) % self.len;
    }
}

/// Result of an interactive picker session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerOutcome {
    /// The user confirmed the row at this index with `Enter`.
    Selected(usize),
    /// The user pressed `Esc` or `Ctrl+C`.
    Cancelled,
    /// The session was not attached to an interactive terminal, so no picker
    /// was shown. Callers should fall back to a non-interactive prompt.
    NotInteractive,
}

fn colors_enabled() -> bool {
    env::var_os("NO_COLOR").is_none()
}

/// Present `items` as an arrow-key selectable list under `title`, starting with
/// `initial` highlighted. Returns the chosen index, a cancellation, or
/// [`PickerOutcome::NotInteractive`] when stdin/stdout are not a terminal.
pub fn select_from_list(
    title: &str,
    items: &[PickerItem],
    initial: usize,
) -> io::Result<PickerOutcome> {
    if items.is_empty() {
        return Ok(PickerOutcome::Cancelled);
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Ok(PickerOutcome::NotInteractive);
    }

    let total_lines = u16::try_from(items.len() + 2).unwrap_or(u16::MAX);
    let mut state = PickerState::new(items.len(), initial);
    let mut stdout = io::stdout();

    terminal::enable_raw_mode()?;
    let outcome = (|| -> io::Result<PickerOutcome> {
        render(&mut stdout, title, items, &state, false)?;
        loop {
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => state.move_up(),
                KeyCode::Down | KeyCode::Char('j') => state.move_down(),
                KeyCode::Enter => {
                    clear_block(&mut stdout, total_lines)?;
                    return Ok(PickerOutcome::Selected(state.selected()));
                }
                KeyCode::Esc => {
                    clear_block(&mut stdout, total_lines)?;
                    return Ok(PickerOutcome::Cancelled);
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    clear_block(&mut stdout, total_lines)?;
                    return Ok(PickerOutcome::Cancelled);
                }
                _ => continue,
            }
            render(&mut stdout, title, items, &state, true)?;
        }
    })();

    let restore = terminal::disable_raw_mode();
    let outcome = outcome?;
    restore?;
    Ok(outcome)
}

fn render(
    out: &mut impl Write,
    title: &str,
    items: &[PickerItem],
    state: &PickerState,
    is_redraw: bool,
) -> io::Result<()> {
    let total_lines = u16::try_from(items.len() + 2).unwrap_or(u16::MAX);
    if is_redraw {
        queue!(
            out,
            cursor::MoveToColumn(0),
            cursor::MoveUp(total_lines),
            Clear(ClearType::FromCursorDown)
        )?;
    }

    let muted = Color::Rgb {
        r: 148,
        g: 163,
        b: 184,
    };
    let selected_bg = Color::Rgb {
        r: 51,
        g: 65,
        b: 85,
    };
    let selected_fg = Color::Rgb {
        r: 226,
        g: 232,
        b: 240,
    };

    if colors_enabled() {
        out.queue(SetForegroundColor(muted))?
            .queue(Print(format!("{title}\r\n")))?
            .queue(ResetColor)?;
    } else {
        out.queue(Print(format!("{title}\r\n")))?;
    }

    for (index, item) in items.iter().enumerate() {
        let is_selected = index == state.selected();
        let marker = if is_selected { "›" } else { " " };
        let line = if item.detail.is_empty() {
            format!("  {marker} {}", item.label)
        } else {
            format!("  {marker} {}  {}", item.label, item.detail)
        };
        if colors_enabled() && is_selected {
            out.queue(SetBackgroundColor(selected_bg))?
                .queue(SetForegroundColor(selected_fg))?
                .queue(Print(format!("{line}\r\n")))?
                .queue(ResetColor)?;
        } else {
            out.queue(Print(format!("{line}\r\n")))?;
        }
    }

    let hint = "  ↑/↓ move · enter select · esc cancel";
    if colors_enabled() {
        out.queue(SetForegroundColor(muted))?
            .queue(Print(format!("{hint}\r\n")))?
            .queue(ResetColor)?;
    } else {
        out.queue(Print(format!("{hint}\r\n")))?;
    }

    out.flush()
}

/// Erase the rendered block and leave the cursor at its top-left, so the caller
/// can print a clean result line where the picker used to be.
fn clear_block(out: &mut impl Write, total_lines: u16) -> io::Result<()> {
    queue!(
        out,
        cursor::MoveToColumn(0),
        cursor::MoveUp(total_lines),
        Clear(ClearType::FromCursorDown)
    )?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::{PickerOutcome, PickerState};

    #[test]
    fn new_clamps_initial_selection_into_range() {
        assert_eq!(PickerState::new(3, 10).selected(), 2);
        assert_eq!(PickerState::new(3, 1).selected(), 1);
        assert_eq!(PickerState::new(0, 5).selected(), 0);
    }

    #[test]
    fn move_down_advances_and_wraps() {
        let mut state = PickerState::new(3, 0);
        state.move_down();
        assert_eq!(state.selected(), 1);
        state.move_down();
        assert_eq!(state.selected(), 2);
        state.move_down();
        assert_eq!(state.selected(), 0);
    }

    #[test]
    fn move_up_retreats_and_wraps() {
        let mut state = PickerState::new(3, 0);
        state.move_up();
        assert_eq!(state.selected(), 2);
        state.move_up();
        assert_eq!(state.selected(), 1);
    }

    #[test]
    fn empty_list_navigation_is_a_noop() {
        let mut state = PickerState::new(0, 0);
        state.move_up();
        state.move_down();
        assert_eq!(state.selected(), 0);
    }

    #[test]
    fn outcomes_compare_by_value() {
        assert_eq!(PickerOutcome::Selected(2), PickerOutcome::Selected(2));
        assert_ne!(PickerOutcome::Selected(1), PickerOutcome::Cancelled);
    }
}
