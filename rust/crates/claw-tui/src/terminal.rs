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

/// Mouse capture routes drags to the app, which stops the terminal's own
/// click-drag selection — so text in the transcript cannot be highlighted or
/// copied. Selection is worth more than wheel scrolling (keys cover that), so
/// capture is opt-in via `CLAW_MOUSE_CAPTURE=1`.
pub fn mouse_capture_requested() -> bool {
    std::env::var("CLAW_MOUSE_CAPTURE")
        .map(|value| matches!(value.trim(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

impl TerminalGuard {
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, cursor::Hide) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        if mouse_capture_requested() {
            if let Err(error) = execute!(stdout, event::EnableMouseCapture) {
                let _ = execute!(stdout, cursor::Show, LeaveAlternateScreen);
                let _ = disable_raw_mode();
                return Err(error);
            }
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
    // Disabling capture that was never enabled is a no-op, so this stays
    // unconditional and still cleans up after `CLAW_MOUSE_CAPTURE=1` runs.
    let _ = execute!(
        stdout,
        cursor::Show,
        event::DisableMouseCapture,
        LeaveAlternateScreen
    );
}

#[cfg(test)]
mod tests {
    use super::mouse_capture_requested;

    /// Serializes the env mutation below against other tests in this binary.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn mouse_capture_stays_off_unless_explicitly_requested() {
        let _guard = env_lock();

        std::env::remove_var("CLAW_MOUSE_CAPTURE");
        assert!(
            !mouse_capture_requested(),
            "default must leave selection/copy working"
        );

        for value in ["0", "false", "", "no"] {
            std::env::set_var("CLAW_MOUSE_CAPTURE", value);
            assert!(
                !mouse_capture_requested(),
                "{value:?} should not enable capture"
            );
        }

        for value in ["1", "true", "yes", " 1 "] {
            std::env::set_var("CLAW_MOUSE_CAPTURE", value);
            assert!(mouse_capture_requested(), "{value:?} should enable capture");
        }

        std::env::remove_var("CLAW_MOUSE_CAPTURE");
    }
}
