use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use crossterm::style::{style, Color, Stylize};
use crossterm::terminal;
use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::{CmdKind, Highlighter};
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{
    Cmd, CompletionType, Config, Context, EditMode, Editor, Helper, KeyCode, KeyEvent, Modifiers,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadOutcome {
    Submit(String),
    Cancel,
    Exit,
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
        false
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
    footer_active: bool,
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

        Self {
            prompt: prompt.into(),
            footer: String::new(),
            footer_active: false,
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
        self.footer = footer.into();
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
            return Ok(());
        }

        write!(stdout, "\x1b[r\x1b[{rows};1H\x1b[2K\x1b[{rows};1H")?;
        stdout.flush()?;
        self.footer_active = false;
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
            return self.prepare_relative_footer();
        }

        let mut stdout = io::stdout();
        let input_row = rows - 1;
        write!(
            stdout,
            "\x1b[1;{input_row}r\x1b[{rows};1H\x1b[2K{}\x1b[{input_row};1H\x1b[2K",
            self.footer
        )?;
        stdout.flush().map(|()| self.footer_active = true)
    }

    fn prepare_relative_footer(&mut self) -> io::Result<()> {
        let mut stdout = io::stdout();
        writeln!(stdout)?;
        write!(stdout, "{}\x1b[1A\x1b[1G", self.footer)?;
        stdout.flush()?;
        self.footer_active = true;
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
        highlight_slash_command, path_completion_candidates, slash_command_prefix, LineEditor,
        SlashCommandHelper,
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
    fn push_history_ignores_blank_entries() {
        let mut editor = LineEditor::new("> ", vec!["/help".to_string()]);
        editor.push_history("   ");
        editor.push_history("/help");

        assert_eq!(editor.editor.history().len(), 1);
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
