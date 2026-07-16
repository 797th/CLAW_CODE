use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use crossterm::style::{style, Color, Stylize};
use crossterm::terminal;
use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::{CmdKind, Highlighter};
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{
    Cmd, CompletionType, ConditionalEventHandler, Config, Context, EditMode, Editor, Event,
    EventContext, EventHandler, Helper, KeyCode, KeyEvent, Modifiers, Movement, RepeatCount,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadOutcome {
    Submit(String),
    Cancel,
    Exit,
}

/// Shared footer storage read by the live renderer while a model turn is in
/// flight. Keeping the string behind a mutex lets the stream callback update
/// usage without competing with the spinner thread for terminal ownership.
pub type FooterStore = Arc<Mutex<String>>;

// Shift+Tab cycles through four REPL permission/workspace modes, mirroring
// Claude Code's default/plan/auto-accept/bypass rotation. "plan" is not a
// separate runtime `PermissionMode`; it maps onto read-only enforcement (the
// agent can research and plan but not modify the workspace) while presenting a
// distinct label and color so the planning posture is visible.
const PERMISSION_MODE_CYCLE: [&str; 4] = [
    "prompt",
    "read-only",
    "workspace-write",
    "danger-full-access",
];
const PERMISSION_MODE_DISPLAY: [&str; 4] = ["ask", "plan", "workspace", "bypass"];

fn permission_mode_index(mode: &str) -> u8 {
    match mode.trim() {
        "read-only" | "plan" => 1,
        "workspace-write" | "workspace" => 2,
        "danger-full-access" | "bypass" | "full access" => 3,
        _ => 0,
    }
}

fn permission_mode_for_index(index: u8) -> &'static str {
    PERMISSION_MODE_CYCLE[usize::from(index) % PERMISSION_MODE_CYCLE.len()]
}

fn permission_display_for_index(index: u8) -> &'static str {
    PERMISSION_MODE_DISPLAY[usize::from(index) % PERMISSION_MODE_DISPLAY.len()]
}

/// Distinct footer accent color per mode so each posture is visually
/// recognizable at a glance: muted (ask), blue (plan), green (workspace),
/// red (bypass).
fn permission_mode_color(index: u8) -> Color {
    match index % 4 {
        1 => Color::Rgb { r: 96, g: 165, b: 250 },
        2 => Color::Rgb { r: 52, g: 211, b: 153 },
        3 => Color::Rgb { r: 248, g: 113, b: 113 },
        _ => Color::Rgb {
            r: 148,
            g: 163,
            b: 184,
        },
    }
}

/// Render the `· <label>` permission segment shown in the REPL footer, colored
/// for the given mode. This is the single source of truth shared by the static
/// footer (built in `main.rs`) and the live Shift+Tab redraw below, so the
/// dynamic redraw can locate and swap the exact segment string in place.
#[must_use]
pub fn styled_permission_segment(mode: &str) -> String {
    permission_segment_for_index(permission_mode_index(mode))
}

/// Render the REPL input prompt as a shaded block ("chip") rather than a bare
/// dot, so the user can clearly see where typing begins. The chip is a padded,
/// background-filled region; typed text follows it on the same row.
#[must_use]
pub fn styled_input_chip() -> String {
    if env::var_os("NO_COLOR").is_some() {
        return "> ".to_string();
    }
    let bg = Color::Rgb {
        r: 39,
        g: 45,
        b: 62,
    };
    let fg = Color::Rgb {
        r: 148,
        g: 163,
        b: 184,
    };
    // A padded, background-shaded chevron block, then a space so typed text is
    // not visually glued to the shaded region.
    format!("{} ", style(" › ").on(bg).with(fg))
}

/// Return the one-based terminal column immediately after a prompt, ignoring
/// ANSI styling sequences. This lets the parked cursor sit inside the
/// composer instead of covering the prompt marker.
fn prompt_cursor_column(prompt: &str) -> u16 {
    let mut width = 0usize;
    let mut chars = prompt.chars();
    while let Some(character) = chars.next() {
        if character == '\x1b' {
            if chars.next() == Some('[') {
                while let Some(code) = chars.next() {
                    if ('@'..='~').contains(&code) {
                        break;
                    }
                }
            }
            continue;
        }
        if character == '\n' {
            width = 0;
        } else {
            width = width.saturating_add(1);
        }
    }
    u16::try_from(width.saturating_add(1)).unwrap_or(u16::MAX)
}

fn permission_segment_for_index(index: u8) -> String {
    let label = permission_display_for_index(index);
    if env::var_os("NO_COLOR").is_some() {
        return format!("· {label}");
    }
    format!("{}", style(format!("· {label}")).with(permission_mode_color(index)))
}

fn redraw_footer_line(footer: &str) {
    let rows = terminal::size().map(|(_, rows)| rows).unwrap_or_default();
    if rows < 3 {
        return;
    }

    let mut stdout = io::stdout();
    // Save the cursor while rustyline owns the editable input row, redraw the
    // pinned footer, then restore the cursor so the current line stays intact.
    let _ = write!(
        stdout,
        "\x1b7\x1b[{rows};1H\x1b[2K{footer}\x1b8"
    );
    let _ = stdout.flush();
}

struct PermissionToggleState {
    mode: AtomicU8,
    footer: FooterStore,
}

impl PermissionToggleState {
    fn new() -> Self {
        Self {
            mode: AtomicU8::new(permission_mode_index("prompt")),
            footer: Arc::new(Mutex::new(String::new())),
        }
    }

    fn set_mode(&self, mode: &str) {
        self.mode
            .store(permission_mode_index(mode), Ordering::Relaxed);
    }

    fn current_mode(&self) -> &'static str {
        permission_mode_for_index(self.mode.load(Ordering::Relaxed))
    }

    fn set_footer(&self, footer: String) {
        if let Ok(mut shared_footer) = self.footer.lock() {
            *shared_footer = footer;
        }
    }

    fn cycle(&self) {
        let current = self.mode.load(Ordering::Relaxed);
        let next = current.wrapping_add(1) % u8::try_from(PERMISSION_MODE_CYCLE.len()).unwrap_or(1);
        self.mode.store(next, Ordering::Relaxed);

        let old_segment = permission_segment_for_index(current);
        let new_segment = permission_segment_for_index(next);
        let footer = self
            .footer
            .lock()
            .ok()
            .map(|mut footer| {
                if let Some(pos) = footer.find(&old_segment) {
                    footer.replace_range(pos..pos + old_segment.len(), &new_segment);
                }
                footer.clone()
            });
        if let Some(footer) = footer {
            if let Ok(mut shared_footer) = self.footer.lock() {
                *shared_footer = footer.clone();
            }
            redraw_footer_line(&footer);
        }
    }
}

struct PermissionToggleHandler {
    state: Arc<PermissionToggleState>,
}

impl ConditionalEventHandler for PermissionToggleHandler {
    fn handle(
        &self,
        _event: &Event,
        _repeat_count: RepeatCount,
        _positive: bool,
        _context: &EventContext,
    ) -> Option<Cmd> {
        self.state.cycle();
        Some(Cmd::Repaint)
    }
}

struct SlashCommandHelper {
    completions: Vec<String>,
    current_line: RefCell<String>,
}

impl SlashCommandHelper {
    fn new(completions: Vec<String>) -> Self {
        Self {
            completions: normalize_completions(completions),
            current_line: RefCell::new(String::new()),
        }
    }

    fn reset_current_line(&self) {
        self.current_line.borrow_mut().clear();
    }

    fn current_line(&self) -> String {
        self.current_line.borrow().clone()
    }

    fn set_current_line(&self, line: &str) {
        let mut current = self.current_line.borrow_mut();
        current.clear();
        current.push_str(line);
    }

    fn set_completions(&mut self, completions: Vec<String>) {
        self.completions = normalize_completions(completions);
    }
}

impl Completer for SlashCommandHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Self::Candidate>)> {
        if let Some((start, matches)) = complete_path_argument(line, pos) {
            return Ok((start, matches));
        }

        let Some(prefix) = slash_command_prefix(line, pos) else {
            return Ok((0, Vec::new()));
        };

        let matches = self
            .completions
            .iter()
            .filter(|candidate| candidate.starts_with(prefix))
            .map(|candidate| Pair {
                display: candidate.clone(),
                replacement: candidate.clone(),
            })
            .collect();

        Ok((0, matches))
    }
}

const PATH_COMPLETION_COMMAND_PREFIXES: &[&str] = &["/export ", "/resume "];

impl Hinter for SlashCommandHelper {
    type Hint = String;
}

impl Highlighter for SlashCommandHelper {
    fn highlight<'l>(&self, line: &'l str, _pos: usize) -> Cow<'l, str> {
        self.set_current_line(line);
        Cow::Owned(highlight_slash_command(line))
    }

    fn highlight_char(&self, line: &str, _pos: usize, _kind: CmdKind) -> bool {
        self.set_current_line(line);
        // Ask rustyline to rerender the line after every edit while it is a
        // slash command. This makes `/model`, `/login`, and partial commands
        // turn blue as soon as the slash and command token are typed.
        line.starts_with('/')
    }

    fn highlight_prompt<'b, 's: 'b, 'p: 'b>(
        &'s self,
        prompt: &'p str,
        _default: bool,
    ) -> Cow<'b, str> {
        // The prompt is already rendered as a shaded input chip by the caller
        // (see `styled_input_chip`). Preserve it verbatim so the affordance is
        // stable regardless of whether the line is a slash command.
        Cow::Borrowed(prompt)
    }
}

impl Validator for SlashCommandHelper {}
impl Helper for SlashCommandHelper {}

fn highlight_slash_command(line: &str) -> String {
    if !line.starts_with('/') {
        return line.to_string();
    }

    let command_len = line.find(char::is_whitespace).unwrap_or(line.len());
    let (command, remainder) = line.split_at(command_len);
    format!("{}{remainder}", style(command).with(Color::Blue))
}

pub struct LineEditor {
    prompt: String,
    footer: String,
    footer_state: Arc<PermissionToggleState>,
    footer_active: bool,
    working_active: bool,
    editor: Editor<SlashCommandHelper, DefaultHistory>,
}

impl LineEditor {
    #[must_use]
    pub fn new(prompt: impl Into<String>, completions: Vec<String>) -> Self {
        let config = Config::builder()
            .completion_type(CompletionType::List)
            .edit_mode(EditMode::Emacs)
            .bracketed_paste(true)
            .build();
        let mut editor = Editor::<SlashCommandHelper, DefaultHistory>::with_config(config)
            .expect("rustyline editor should initialize");
        editor.set_helper(Some(SlashCommandHelper::new(completions)));
        editor.bind_sequence(KeyEvent(KeyCode::Char('J'), Modifiers::CTRL), Cmd::Newline);
        editor.bind_sequence(KeyEvent(KeyCode::Enter, Modifiers::SHIFT), Cmd::Newline);
        // Escape cancels the current command buffer without leaving the REPL.
        // Interactive command pickers and setup prompts handle Escape in the
        // same way, so every command has a consistent cancellation gesture.
        editor.bind_sequence(
            KeyEvent(KeyCode::Esc, Modifiers::NONE),
            Cmd::Kill(Movement::WholeBuffer),
        );
        let footer_state = Arc::new(PermissionToggleState::new());
        editor.bind_sequence(
            KeyEvent(KeyCode::BackTab, Modifiers::NONE),
            EventHandler::Conditional(Box::new(PermissionToggleHandler {
                state: Arc::clone(&footer_state),
            })),
        );
        editor.bind_sequence(
            KeyEvent(KeyCode::Tab, Modifiers::SHIFT),
            EventHandler::Conditional(Box::new(PermissionToggleHandler {
                state: Arc::clone(&footer_state),
            })),
        );

        Self {
            prompt: prompt.into(),
            footer: String::new(),
            footer_state,
            footer_active: false,
            working_active: false,
            editor,
        }
    }

    pub fn push_history(&mut self, entry: impl Into<String>) {
        let entry = entry.into();
        if entry.trim().is_empty() {
            return;
        }

        let _ = self.editor.add_history_entry(entry);
    }

    pub fn set_completions(&mut self, completions: Vec<String>) {
        if let Some(helper) = self.editor.helper_mut() {
            helper.set_completions(completions);
        }
    }

    pub fn set_prompt(&mut self, prompt: impl Into<String>) {
        self.prompt = prompt.into();
    }

    pub fn set_footer(&mut self, footer: impl Into<String>) {
        let footer = footer.into();
        self.footer.clone_from(&footer);
        self.footer_state.set_footer(footer);
    }

    /// Return the shared footer storage used by the background activity
    /// renderer to repaint live usage counters.
    #[must_use]
    pub fn footer_store(&self) -> FooterStore {
        Arc::clone(&self.footer_state.footer)
    }

    pub fn set_permission_mode(&mut self, mode: &str) {
        self.footer_state.set_mode(mode);
        self.sync_footer();
    }

    pub fn permission_mode(&mut self) -> String {
        self.sync_footer();
        self.footer_state.current_mode().to_string()
    }

    fn sync_footer(&mut self) {
        if let Ok(footer) = self.footer_state.footer.lock() {
            self.footer.clone_from(&footer);
        }
    }

    pub fn read_line(&mut self) -> io::Result<ReadOutcome> {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return self.read_line_fallback();
        }

        self.prepare_footer()?;

        if let Some(helper) = self.editor.helper_mut() {
            helper.reset_current_line();
        }

        let result = match self.editor.readline(&self.prompt) {
            Ok(line) => Ok(ReadOutcome::Submit(line)),
            Err(ReadlineError::Interrupted) => {
                let has_input = !self.current_line().is_empty();
                if has_input {
                    Ok(ReadOutcome::Cancel)
                } else {
                    Ok(ReadOutcome::Exit)
                }
            }
            Err(ReadlineError::Eof) => Ok(ReadOutcome::Exit),
            Err(error) => Err(io::Error::other(error)),
        };

        self.finish_prompt_read()?;
        if matches!(result, Ok(ReadOutcome::Cancel | ReadOutcome::Exit)) {
            self.finish_interrupted_read()?;
        }
        if matches!(result, Ok(ReadOutcome::Exit)) {
            self.finish()?;
        }
        result
    }

    pub fn finish(&mut self) -> io::Result<()> {
        if !self.footer_active {
            return Ok(());
        }

        let rows = terminal::size().map(|(_, rows)| rows).unwrap_or_default();
        let mut stdout = io::stdout();
        if rows < 3 {
            write!(stdout, "\r\x1b[2K")?;
            stdout.flush()?;
            self.footer_active = false;
            self.working_active = false;
            return Ok(());
        }

        write!(stdout, "\x1b[r\x1b[{rows};1H\x1b[2K\x1b[{rows};1H")?;
        stdout.flush()?;
        self.footer_active = false;
        self.working_active = false;
        Ok(())
    }

    /// Reserve a line above the composer for the live activity indicator.
    /// The transcript scrolls above that line; the composer and model footer
    /// stay pinned at the bottom of the terminal.
    pub fn begin_working(&mut self) -> io::Result<()> {
        if self.footer.is_empty() || !self.footer_active || self.working_active {
            return Ok(());
        }

        let rows = terminal::size().map(|(_, rows)| rows).unwrap_or_default();
        if rows < 4 {
            return Ok(());
        }

        let working_row = rows - 2;
        let input_row = rows - 1;
        let input_column = prompt_cursor_column(&self.prompt);
        let mut stdout = io::stdout();

        // The submitted input currently occupies the bottom scroll row. Move
        // it into the transcript before reserving a fresh activity row.
        write!(stdout, "\x1b[1;{working_row}r\x1b[{working_row};1H\r\n")?;
        write!(
            stdout,
            "\x1b[{input_row};1H\x1b[2K{}\x1b[{rows};1H\x1b[2K{}\x1b[{working_row};1H\x1b[2K\x1b[{input_row};{input_column}G",
            self.prompt, self.footer
        )?;
        stdout.flush()?;
        self.working_active = true;
        Ok(())
    }

    fn current_line(&self) -> String {
        self.editor
            .helper()
            .map_or_else(String::new, SlashCommandHelper::current_line)
    }

    fn finish_interrupted_read(&mut self) -> io::Result<()> {
        if let Some(helper) = self.editor.helper_mut() {
            helper.reset_current_line();
        }
        let mut stdout = io::stdout();
        writeln!(stdout)
    }

    fn prepare_footer(&mut self) -> io::Result<()> {
        if self.footer.is_empty() {
            return Ok(());
        }

        let rows = terminal::size().map(|(_, rows)| rows).unwrap_or_default();
        if rows < 3 {
            self.working_active = false;
            return self.prepare_relative_footer();
        }

        let mut stdout = io::stdout();
        let input_row = rows - 1;
        write!(
            stdout,
            "\x1b[1;{input_row}r\x1b[{rows};1H\x1b[2K{}\x1b[{input_row};1H\x1b[2K",
            self.footer
        )?;
        stdout.flush().map(|()| {
            self.footer_active = true;
            self.working_active = false;
        })
    }

    fn prepare_relative_footer(&mut self) -> io::Result<()> {
        let mut stdout = io::stdout();
        writeln!(stdout)?;
        write!(stdout, "{}\x1b[1A\x1b[1G", self.footer)?;
        stdout.flush()?;
        self.footer_active = true;
        self.working_active = false;
        Ok(())
    }

    fn finish_prompt_read(&self) -> io::Result<()> {
        if self.footer.is_empty() || !self.footer_active {
            return Ok(());
        }

        let mut stdout = io::stdout();
        write!(stdout, "\r")?;
        stdout.flush()
    }

    fn read_line_fallback(&self) -> io::Result<ReadOutcome> {
        let mut stdout = io::stdout();
        write!(stdout, "{}", self.prompt)?;
        stdout.flush()?;

        let mut buffer = String::new();
        let bytes_read = io::stdin().read_line(&mut buffer)?;
        if bytes_read == 0 {
            return Ok(ReadOutcome::Exit);
        }

        while matches!(buffer.chars().last(), Some('\n' | '\r')) {
            buffer.pop();
        }

        if !self.footer.is_empty() {
            writeln!(stdout)?;
            writeln!(stdout, "{}", self.footer)?;
            stdout.flush()?;
        }
        Ok(ReadOutcome::Submit(buffer))
    }
}

impl Drop for LineEditor {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

fn slash_command_prefix(line: &str, pos: usize) -> Option<&str> {
    if pos != line.len() {
        return None;
    }

    let prefix = &line[..pos];
    if !prefix.starts_with('/') {
        return None;
    }

    Some(prefix)
}

fn complete_path_argument(line: &str, pos: usize) -> Option<(usize, Vec<Pair>)> {
    if pos != line.len() || !line.starts_with('/') {
        return None;
    }

    for command_prefix in PATH_COMPLETION_COMMAND_PREFIXES {
        let Some(path_fragment) = line.strip_prefix(command_prefix) else {
            continue;
        };

        let matches = path_completion_candidates(path_fragment)
            .into_iter()
            .map(|candidate| Pair {
                display: candidate.display,
                replacement: candidate.replacement,
            })
            .collect();
        return Some((command_prefix.len(), matches));
    }

    None
}

fn path_completion_candidates(path_fragment: &str) -> Vec<PathCompletionCandidate> {
    let Ok((search_dir, prefix, base_display)) = resolve_path_completion_context(path_fragment)
    else {
        return Vec::new();
    };

    let Ok(entries) = fs::read_dir(&search_dir) else {
        return Vec::new();
    };

    let mut candidates = BTreeSet::new();
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };

        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if !file_name.starts_with(&prefix) {
            continue;
        }

        let mut replacement = format!("{base_display}{file_name}");
        if file_type.is_dir() {
            replacement.push('/');
        }
        candidates.insert(replacement);
    }

    candidates
        .into_iter()
        .map(|replacement| PathCompletionCandidate {
            display: replacement.clone(),
            replacement,
        })
        .collect()
}

fn resolve_path_completion_context(
    path_fragment: &str,
) -> io::Result<(PathBuf, String, String)> {
    if path_fragment.is_empty() {
        return Ok((PathBuf::from("."), String::new(), String::new()));
    }

    if path_fragment.ends_with('/') {
        let search_dir = PathBuf::from(path_fragment);
        let base_display = normalize_path_completion_prefix(path_fragment);
        return Ok((search_dir, String::new(), base_display));
    }

    let path = Path::new(path_fragment);
    let prefix = path
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default();
    let search_dir = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let base_display = path_completion_base_display(&search_dir);

    Ok((search_dir, prefix, base_display))
}

fn path_completion_base_display(search_dir: &Path) -> String {
    if search_dir == Path::new(".") {
        String::new()
    } else if search_dir == Path::new("/") {
        "/".to_string()
    } else {
        format!("{}/", search_dir.display())
    }
}

fn normalize_path_completion_prefix(path_fragment: &str) -> String {
    if path_fragment == "/" {
        "/".to_string()
    } else {
        path_fragment.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PathCompletionCandidate {
    display: String,
    replacement: String,
}

fn normalize_completions(completions: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    completions
        .into_iter()
        .filter(|candidate| candidate.starts_with('/'))
        .filter(|candidate| seen.insert(candidate.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        highlight_slash_command, path_completion_candidates, permission_display_for_index,
        permission_mode_for_index, permission_mode_index, prompt_cursor_column,
        slash_command_prefix, LineEditor, SlashCommandHelper,
    };
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};
    use rustyline::completion::Completer;
    use rustyline::highlight::Highlighter;
    use rustyline::history::{DefaultHistory, History};
    use rustyline::Context;

    #[test]
    fn extracts_terminal_slash_command_prefixes_with_arguments() {
        assert_eq!(slash_command_prefix("/he", 3), Some("/he"));
        assert_eq!(slash_command_prefix("/help me", 8), Some("/help me"));
        assert_eq!(
            slash_command_prefix("/session switch ses", 19),
            Some("/session switch ses")
        );
        assert_eq!(slash_command_prefix("hello", 5), None);
        assert_eq!(slash_command_prefix("/help", 2), None);
    }

    #[test]
    fn completes_matching_slash_commands() {
        let helper = SlashCommandHelper::new(vec![
            "/help".to_string(),
            "/hello".to_string(),
            "/status".to_string(),
        ]);
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);
        let (start, matches) = helper
            .complete("/he", 3, &ctx)
            .expect("completion should work");

        assert_eq!(start, 0);
        assert_eq!(
            matches
                .into_iter()
                .map(|candidate| candidate.replacement)
                .collect::<Vec<_>>(),
            vec!["/help".to_string(), "/hello".to_string()]
        );
    }

    #[test]
    fn completes_matching_slash_command_arguments() {
        let helper = SlashCommandHelper::new(vec![
            "/model".to_string(),
            "/model opus".to_string(),
            "/model sonnet".to_string(),
            "/session switch alpha".to_string(),
        ]);
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);
        let (start, matches) = helper
            .complete("/model o", 8, &ctx)
            .expect("completion should work");

        assert_eq!(start, 0);
        assert_eq!(
            matches
                .into_iter()
                .map(|candidate| candidate.replacement)
                .collect::<Vec<_>>(),
            vec!["/model opus".to_string()]
        );
    }

    #[test]
    fn ignores_non_slash_command_completion_requests() {
        let helper = SlashCommandHelper::new(vec!["/help".to_string()]);
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);
        let (_, matches) = helper
            .complete("hello", 5, &ctx)
            .expect("completion should work");

        assert!(matches.is_empty());
    }

    #[test]
    fn tracks_current_buffer_through_highlighter() {
        let helper = SlashCommandHelper::new(Vec::new());
        let _ = helper.highlight("draft", 5);

        assert_eq!(helper.current_line(), "draft");
    }

    #[test]
    fn highlights_slash_command_token_in_blue() {
        let highlighted = highlight_slash_command("/login later");

        assert!(highlighted.contains("\u{1b}[38;5;12m/login\u{1b}[39m"));
        assert!(highlighted.ends_with(" later"));
    }

    #[test]
    fn slash_command_highlighting_refreshes_as_each_token_character_is_typed() {
        let helper = SlashCommandHelper::new(Vec::new());

        assert!(helper.highlight_char(
            "/",
            1,
            rustyline::highlight::CmdKind::Other
        ));
        assert!(helper.highlight_char(
            "/model",
            6,
            rustyline::highlight::CmdKind::Other
        ));
        assert!(!helper.highlight_char(
            "normal text",
            11,
            rustyline::highlight::CmdKind::Other
        ));
    }

    #[test]
    fn shift_tab_permission_cycle_matches_repl_labels() {
        assert_eq!(permission_mode_index("ask"), 0);
        assert_eq!(permission_mode_index("plan"), 1);
        assert_eq!(permission_mode_index("read-only"), 1);
        assert_eq!(permission_mode_index("workspace"), 2);
        assert_eq!(permission_mode_index("bypass"), 3);
        assert_eq!(permission_mode_for_index(0), "prompt");
        assert_eq!(permission_mode_for_index(1), "read-only");
        assert_eq!(permission_mode_for_index(2), "workspace-write");
        assert_eq!(permission_mode_for_index(3), "danger-full-access");
        // Cycle wraps back to the first mode.
        assert_eq!(permission_mode_for_index(4), "prompt");
    }

    #[test]
    fn shift_tab_cycle_advances_through_all_four_modes() {
        let state = super::PermissionToggleState::new();
        state.set_mode("prompt");
        assert_eq!(state.current_mode(), "prompt");
        state.cycle();
        assert_eq!(state.current_mode(), "read-only");
        state.cycle();
        assert_eq!(state.current_mode(), "workspace-write");
        state.cycle();
        assert_eq!(state.current_mode(), "danger-full-access");
        state.cycle();
        assert_eq!(state.current_mode(), "prompt");
    }

    #[test]
    fn permission_display_labels_track_index() {
        assert_eq!(permission_display_for_index(0), "ask");
        assert_eq!(permission_display_for_index(1), "plan");
        assert_eq!(permission_display_for_index(2), "workspace");
        assert_eq!(permission_display_for_index(3), "bypass");
    }

    #[test]
    fn cycle_swaps_the_colored_footer_segment_in_place() {
        let state = super::PermissionToggleState::new();
        state.set_mode("prompt");
        let footer = format!("• model {}", super::styled_permission_segment("prompt"));
        state.set_footer(footer);
        state.cycle();
        let updated = state
            .footer
            .lock()
            .expect("footer lock")
            .clone();
        // After cycling, the footer carries the plan segment, not the ask one.
        assert!(updated.contains(&super::styled_permission_segment("read-only")));
        assert!(!updated.contains(&super::styled_permission_segment("prompt")));
    }

    #[test]
    fn push_history_ignores_blank_entries() {
        let mut editor = LineEditor::new("> ", vec!["/help".to_string()]);
        editor.push_history("   ");
        editor.push_history("/help");

        assert_eq!(editor.editor.history().len(), 1);
    }

    #[test]
    fn prompt_cursor_column_ignores_ansi_styling() {
        assert_eq!(prompt_cursor_column("> "), 3);
        assert_eq!(
            prompt_cursor_column("\x1b[38;2;148;163;184m › \x1b[39m "),
            5
        );
    }

    #[test]
    fn set_completions_replaces_and_normalizes_candidates() {
        let mut editor = LineEditor::new("> ", vec!["/help".to_string()]);
        editor.set_completions(vec![
            "/model opus".to_string(),
            "/model opus".to_string(),
            "status".to_string(),
        ]);

        let helper = editor.editor.helper().expect("helper should exist");
        assert_eq!(helper.completions, vec!["/model opus".to_string()]);
    }

    #[test]
    fn completes_export_paths_from_filesystem() {
        let temp_dir = unique_test_dir("export-paths");
        fs::create_dir_all(&temp_dir).expect("temp dir should exist");
        fs::write(temp_dir.join("notes.txt"), "hi").expect("file should be created");
        fs::create_dir_all(temp_dir.join("nested")).expect("nested dir should be created");

        let helper = SlashCommandHelper::new(Vec::new());
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);
        let line = format!("/export {}/n", temp_dir.display());
        let (start, matches) = helper
            .complete(&line, line.len(), &ctx)
            .expect("completion should work");

        assert_eq!(start, "/export ".len());
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].replacement, format!("{}/nested/", temp_dir.display()));
        assert_eq!(matches[1].replacement, format!("{}/notes.txt", temp_dir.display()));

        fs::remove_dir_all(&temp_dir).expect("temp dir should be removed");
    }

    #[test]
    fn path_completion_candidates_include_trailing_slash_for_directories() {
        let temp_dir = unique_test_dir("path-candidates");
        fs::create_dir_all(temp_dir.join("alpha")).expect("dir should be created");
        fs::write(temp_dir.join("alpine.txt"), "hi").expect("file should be created");

        let candidates = path_completion_candidates(&format!("{}/al", temp_dir.display()));
        assert_eq!(
            candidates
                .into_iter()
                .map(|candidate| candidate.replacement)
                .collect::<Vec<_>>(),
            vec![
                format!("{}/alpha/", temp_dir.display()),
                format!("{}/alpine.txt", temp_dir.display()),
            ]
        );

        fs::remove_dir_all(&temp_dir).expect("temp dir should be removed");
    }

    fn unique_test_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should move forward")
            .as_nanos();
        std::env::temp_dir().join(format!("clawcli-input-{label}-{nanos}"))
    }
}
