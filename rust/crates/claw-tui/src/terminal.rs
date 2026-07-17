use std::io::{self, Stdout};
use std::panic::{self, PanicHookInfo};
use std::sync::{Arc, Mutex};

use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{cursor, event};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

type PanicHook = Box<dyn Fn(&PanicHookInfo<'_>) + Send + Sync + 'static>;

pub type TuiTerminal = Terminal<CrosstermBackend<Stdout>>;

/// Owns raw mode, the alternate screen, and the panic hook for one TUI run.
/// Dropping it is the final safety net against leaving the user's shell in a
/// hidden-cursor/raw-input state.
pub struct TerminalGuard {
    previous_hook: Arc<Mutex<Option<PanicHook>>>,
    restored: bool,
}

impl TerminalGuard {
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(
            stdout,
            EnterAlternateScreen,
            event::EnableMouseCapture,
            cursor::Hide
        ) {
            let _ = disable_raw_mode();
            return Err(error);
        }

        let previous_hook = Arc::new(Mutex::new(Some(panic::take_hook())));
        let panic_hook_state = Arc::clone(&previous_hook);
        panic::set_hook(Box::new(move |panic_info| {
            restore_terminal();
            if let Ok(mut previous) = panic_hook_state.lock() {
                if let Some(hook) = previous.take() {
                    hook(panic_info);
                }
            }
        }));

        Ok(Self {
            previous_hook,
            restored: false,
        })
    }

    pub fn restore(&mut self) {
        if self.restored {
            return;
        }
        self.restored = true;
        restore_terminal();

        let current_hook = panic::take_hook();
        if let Ok(mut previous) = self.previous_hook.lock() {
            if let Some(previous_hook) = previous.take() {
                panic::set_hook(previous_hook);
                return;
            }
        }
        panic::set_hook(current_hook);
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

pub fn open_terminal() -> io::Result<(TuiTerminal, TerminalGuard)> {
    let guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    match Terminal::new(backend) {
        Ok(terminal) => Ok((terminal, guard)),
        Err(error) => {
            drop(guard);
            Err(error)
        }
    }
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let mut stdout = io::stdout();
    let _ = execute!(
        stdout,
        cursor::Show,
        event::DisableMouseCapture,
        LeaveAlternateScreen
    );
}
