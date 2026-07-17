use std::io;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, Event as CrosstermEvent, KeyCode, KeyEvent, KeyModifiers, MouseEventKind,
};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::markdown::render_markdown;
use crate::terminal::open_terminal;
use crate::theme::Theme;

const TICK: Duration = Duration::from_millis(75);
const CARET_GLYPH: &str = "█";
const PERMISSION_MODES: [&str; 4] = ["ask", "plan", "workspace", "bypass"];

fn configured_model() -> String {
    crate::config::configured_model()
        .map(|model| resolve_model_alias_with_config(&model))
        .unwrap_or_default()
}

fn resolve_model_alias_with_config(model: &str) -> String {
    let aliases = crate::config::load_aliases();
    let mut resolved = model.trim().to_string();
    for _ in 0..8 {
        let Some(next) = aliases.get(&resolved).cloned() else {
            break;
        };
        if next == resolved {
            break;
        }
        resolved = next.trim().to_string();
    }
    resolved
}

fn model_label(model: &str) -> String {
    if model.trim().is_empty() {
        "no model".to_string()
    } else {
        model.to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageKind {
    User,
    Assistant,
    Thinking,
    Tool,
    Command,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Message {
    kind: MessageKind,
    title: String,
    body: String,
}

impl Message {
    fn new(kind: MessageKind, title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            kind,
            title: title.into(),
            body: body.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Todo {
    label: String,
    done: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StreamEvent {
    ThinkingStart,
    ThinkingDelta(String),
    ThinkingEnd,
    ToolStart {
        name: String,
        detail: String,
    },
    ToolOutput(String),
    AssistantStart,
    TextDelta(String),
    Usage {
        input_tokens: u32,
        output_tokens: u32,
        cost_cents: u32,
    },
    Done {
        input_tokens: u32,
        output_tokens: u32,
        cost_cents: u32,
    },
}

#[derive(Debug, Clone)]
struct Status {
    model: String,
    mode_index: usize,
    branch: &'static str,
    input_tokens: u32,
    output_tokens: u32,
    cost_cents: u32,
    streaming: bool,
    started_at: Instant,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            model: configured_model(),
            mode_index: 0,
            branch: "main",
            input_tokens: 0,
            output_tokens: 0,
            cost_cents: 0,
            streaming: false,
            started_at: Instant::now(),
        }
    }
}

impl Status {
    fn mode(&self) -> &'static str {
        PERMISSION_MODES[self.mode_index % PERMISSION_MODES.len()]
    }

    fn mode_style(&self, theme: Theme) -> Style {
        let color = match self.mode() {
            "plan" => Color::Rgb(96, 165, 250),
            "workspace" => theme.success,
            "bypass" => theme.error,
            _ => theme.muted,
        };
        Style::default().fg(color)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelChoiceAction {
    Switch,
    Custom,
    AddAlias,
    RemoveAlias,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelChoice {
    label: String,
    value: String,
    detail: String,
    action: ModelChoiceAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum InputAction {
    SwitchModel,
    AddAliasName,
    AddAliasModel { alias: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InputDialog {
    title: String,
    label: String,
    value: String,
    cursor: usize,
    secret: bool,
    action: InputAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoginMode {
    Endpoint,
    Provider,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoginField {
    Provider,
    ApiKey,
    BaseUrl,
    Model,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LoginDialog {
    mode: LoginMode,
    field: LoginField,
    provider: String,
    api_key: String,
    base_url: String,
    model: String,
    value: String,
    cursor: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Overlay {
    ModelPicker {
        title: String,
        items: Vec<ModelChoice>,
        selected: usize,
    },
    Input(InputDialog),
    Login(LoginDialog),
    Notice {
        title: String,
        body: String,
    },
}

const PROVIDER_CHOICES: [(&str, &str); 5] = [
    ("1", "anthropic"),
    ("2", "xai"),
    ("3", "openai"),
    ("4", "dashscope"),
    ("5", "openai"),
];

fn provider_base_url(provider: &str) -> &'static str {
    match provider {
        "anthropic-compatible" => "https://api.anthropic.com",
        "xai" => "https://api.x.ai/v1",
        "openai" | "openai-compatible" => "https://api.openai.com/v1",
        "dashscope" => "https://dashscope.aliyuncs.com/compatible-mode/v1",
        _ => "https://api.anthropic.com",
    }
}

fn normalize_provider(value: &str) -> Option<&'static str> {
    let value = value.trim().to_ascii_lowercase();
    PROVIDER_CHOICES
        .iter()
        .find(|(number, name)| {
            *number == value || *name == value || (value == "custom" && *number == "5")
        })
        .map(|(_, name)| *name)
}

fn normalize_endpoint_provider(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "openai" | "openai-compatible" | "openai custom" => Some("openai-compatible"),
        "2" | "anthropic" | "anthropic-compatible" => Some("anthropic-compatible"),
        _ => None,
    }
}

fn login_dialog(mode: LoginMode) -> LoginDialog {
    let current = crate::config::load_provider();
    let provider = match mode {
        LoginMode::Endpoint => current
            .kind
            .as_deref()
            .and_then(normalize_endpoint_provider)
            .unwrap_or("openai-compatible")
            .to_string(),
        LoginMode::Provider => current
            .kind
            .as_deref()
            .and_then(|kind| match kind {
                "openai-compatible" => Some("openai"),
                "anthropic-compatible" => Some("anthropic"),
                _ => normalize_provider(kind),
            })
            .unwrap_or("anthropic")
            .to_string(),
    };
    let base_url = current
        .base_url
        .unwrap_or_else(|| provider_base_url(&provider).to_string());
    let model = current.model.unwrap_or_default();
    LoginDialog {
        mode,
        field: LoginField::Provider,
        provider: provider.clone(),
        api_key: current.api_key.unwrap_or_default(),
        base_url,
        model,
        value: provider.clone(),
        cursor: provider.chars().count(),
    }
}

pub struct App {
    theme: Theme,
    messages: Vec<Message>,
    todos: Vec<Todo>,
    input: String,
    cursor: usize,
    scroll: u16,
    follow_output: bool,
    should_quit: bool,
    stream_rx: Option<Receiver<StreamEvent>>,
    active_thinking: Option<usize>,
    active_assistant: Option<usize>,
    active_tool: Option<usize>,
    history: Vec<String>,
    history_index: Option<usize>,
    command_menu_selected: usize,
    overlay: Option<Overlay>,
    status: Status,
}

pub fn run() -> io::Result<()> {
    let (mut terminal, mut guard) = open_terminal()?;
    let mut app = App::demo();
    let result = run_loop(&mut terminal, &mut app);
    guard.restore();
    result
}

fn run_loop(terminal: &mut crate::terminal::TuiTerminal, app: &mut App) -> io::Result<()> {
    loop {
        app.poll_stream();
        terminal.hide_cursor()?;
        terminal.draw(|frame| app.draw(frame))?;

        if event::poll(TICK)? {
            app.handle_event(event::read()?);
        }
        if app.should_quit {
            return Ok(());
        }
    }
}

impl App {
    fn demo() -> Self {
        let mut app = Self::empty();
        app.messages.push(Message::new(
            MessageKind::User,
            "You",
            "Please inspect `src/auth/token.rs`, verify expired bearer-token behavior, and keep the public API unchanged.",
        ));
        app.todos = vec![
            Todo {
                label: "Trace request validation".to_string(),
                done: false,
            },
            Todo {
                label: "Preserve error contract".to_string(),
                done: false,
            },
            Todo {
                label: "Add regression test".to_string(),
                done: false,
            },
        ];
        if app.model_available() {
            app.start_stream("inspect auth middleware");
        } else {
            app.messages.clear();
            app.todos.clear();
        }
        app
    }

    fn empty() -> Self {
        Self {
            theme: Theme::default(),
            messages: Vec::new(),
            todos: Vec::new(),
            input: String::new(),
            cursor: 0,
            scroll: 0,
            follow_output: true,
            should_quit: false,
            stream_rx: None,
            active_thinking: None,
            active_assistant: None,
            active_tool: None,
            history: Vec::new(),
            history_index: None,
            command_menu_selected: 0,
            overlay: None,
            status: Status::default(),
        }
    }

    fn start_stream(&mut self, prompt: &str) {
        if !self.model_available() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.stream_rx = Some(rx);
        self.active_thinking = None;
        self.active_assistant = None;
        self.active_tool = None;
        self.status.streaming = true;
        self.status.started_at = Instant::now();
        self.status.input_tokens = 0;
        self.status.output_tokens = 0;
        self.status.cost_cents = 0;
        self.follow_output = true;
        spawn_mock_stream(prompt.to_string(), tx);
    }

    fn poll_stream(&mut self) {
        let Some(rx) = self.stream_rx.take() else {
            return;
        };
        let mut connected = true;
        loop {
            match rx.try_recv() {
                Ok(event) => self.apply_stream_event(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    connected = false;
                    break;
                }
            }
        }
        if connected {
            self.stream_rx = Some(rx);
        }
    }

    fn apply_stream_event(&mut self, event: StreamEvent) {
        match event {
            StreamEvent::ThinkingStart => {
                let index = self.messages.len();
                self.messages
                    .push(Message::new(MessageKind::Thinking, "Reasoning", ""));
                self.active_thinking = Some(index);
            }
            StreamEvent::ThinkingDelta(delta) => {
                self.append_to(self.active_thinking, &delta);
            }
            StreamEvent::ThinkingEnd => {
                self.active_thinking = None;
            }
            StreamEvent::ToolStart { name, detail } => {
                let index = self.messages.len();
                self.messages.push(Message::new(
                    MessageKind::Tool,
                    format!("{name}  ·  running"),
                    detail,
                ));
                self.active_tool = Some(index);
            }
            StreamEvent::ToolOutput(output) => {
                self.append_to(self.active_tool, &format!("\n{output}"));
            }
            StreamEvent::AssistantStart => {
                let index = self.messages.len();
                self.messages
                    .push(Message::new(MessageKind::Assistant, "Claw", ""));
                self.active_assistant = Some(index);
            }
            StreamEvent::TextDelta(delta) => {
                if self.active_assistant.is_none() {
                    let index = self.messages.len();
                    self.messages
                        .push(Message::new(MessageKind::Assistant, "Claw", ""));
                    self.active_assistant = Some(index);
                }
                self.append_to(self.active_assistant, &delta);
            }
            StreamEvent::Usage {
                input_tokens,
                output_tokens,
                cost_cents,
            } => self.update_usage(input_tokens, output_tokens, cost_cents),
            StreamEvent::Done {
                input_tokens,
                output_tokens,
                cost_cents,
            } => {
                self.status.streaming = false;
                self.update_usage(input_tokens, output_tokens, cost_cents);
                self.active_thinking = None;
                self.active_assistant = None;
                self.active_tool = None;
                for todo in self.todos.iter_mut().take(2) {
                    todo.done = true;
                }
            }
        }
    }

    fn update_usage(&mut self, input_tokens: u32, output_tokens: u32, cost_cents: u32) {
        self.status.input_tokens = input_tokens;
        self.status.output_tokens = output_tokens;
        self.status.cost_cents = cost_cents;
    }

    fn append_to(&mut self, index: Option<usize>, text: &str) {
        if let Some(index) = index {
            if let Some(message) = self.messages.get_mut(index) {
                message.body.push_str(text);
            }
        }
    }

    fn handle_event(&mut self, event: CrosstermEvent) {
        match event {
            CrosstermEvent::Key(key) => self.handle_key(key),
            CrosstermEvent::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollUp => self.scroll_up(3),
                MouseEventKind::ScrollDown => self.scroll_down(3),
                _ => {}
            },
            _ => {}
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != event::KeyEventKind::Press {
            return;
        }
        if self.overlay.is_some() {
            self.handle_overlay_key(key);
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('q'))
        {
            self.should_quit = true;
            return;
        }

        let suggestions = self.command_suggestions();
        if !suggestions.is_empty() {
            match key.code {
                KeyCode::Up => {
                    self.command_menu_selected = self.command_menu_selected.saturating_sub(1);
                    return;
                }
                KeyCode::Down => {
                    self.command_menu_selected =
                        (self.command_menu_selected + 1).min(suggestions.len().saturating_sub(1));
                    return;
                }
                KeyCode::Tab if key.modifiers.is_empty() => {
                    self.complete_command(&suggestions);
                    return;
                }
                KeyCode::Enter if key.modifiers.is_empty() && !self.command_is_exact() => {
                    self.complete_command(&suggestions);
                    return;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::BackTab => self.cycle_mode(),
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => self.cycle_mode(),
            KeyCode::Esc => self.should_quit = true,
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.insert_char('\n');
            }
            KeyCode::Enter => self.submit_input(),
            KeyCode::Char(character) => self.insert_char(character),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.input.chars().count()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.input.chars().count(),
            KeyCode::Up => self.history_previous(),
            KeyCode::Down => self.history_next(),
            KeyCode::PageUp => self.scroll_up(8),
            KeyCode::PageDown => self.scroll_down(8),
            _ => {}
        }
    }

    fn handle_overlay_key(&mut self, key: KeyEvent) {
        let Some(mut overlay) = self.overlay.take() else {
            return;
        };

        match &mut overlay {
            Overlay::Notice { .. } => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
                    return;
                }
            }
            Overlay::ModelPicker {
                items, selected, ..
            } => match key.code {
                KeyCode::Esc => return,
                KeyCode::Up => {
                    *selected = selected.saturating_sub(1);
                }
                KeyCode::Down => {
                    *selected = (*selected + 1).min(items.len().saturating_sub(1));
                }
                KeyCode::Enter => {
                    if let Some(choice) = items.get(*selected).cloned() {
                        self.activate_model_choice(choice);
                    }
                    return;
                }
                _ => {}
            },
            Overlay::Input(input) => {
                if key.code == KeyCode::Esc {
                    return;
                }
                if key.code == KeyCode::Enter {
                    self.submit_input_dialog(input.clone());
                    return;
                }
                edit_text(&mut input.value, &mut input.cursor, key);
            }
            Overlay::Login(login) => {
                if key.code == KeyCode::Esc {
                    return;
                }
                if key.code == KeyCode::Enter {
                    if self.submit_login_field(login) {
                        return;
                    }
                } else if login.field == LoginField::Provider
                    && key.modifiers.is_empty()
                    && key.code == KeyCode::Char('1')
                {
                    login.value = "1".to_string();
                    login.cursor = 1;
                } else if login.field == LoginField::Provider
                    && key.modifiers.is_empty()
                    && key.code == KeyCode::Char('2')
                {
                    login.value = "2".to_string();
                    login.cursor = 1;
                } else if login.field == LoginField::Provider
                    && login.mode == LoginMode::Provider
                    && key.modifiers.is_empty()
                    && matches!(
                        key.code,
                        KeyCode::Char('3') | KeyCode::Char('4') | KeyCode::Char('5')
                    )
                {
                    if let KeyCode::Char(choice) = key.code {
                        login.value = choice.to_string();
                        login.cursor = 1;
                    }
                } else {
                    edit_text(&mut login.value, &mut login.cursor, key);
                }
            }
        }

        self.overlay = Some(overlay);
    }

    fn cycle_mode(&mut self) {
        self.status.mode_index = (self.status.mode_index + 1) % PERMISSION_MODES.len();
    }

    fn model_available(&self) -> bool {
        !self.status.model.trim().is_empty()
    }

    fn command_suggestions(&self) -> Vec<crate::slash::CommandSpec> {
        let Some(command) = self.input.strip_prefix('/') else {
            return Vec::new();
        };
        if command.chars().any(char::is_whitespace) {
            return Vec::new();
        }
        let query = command.to_ascii_lowercase();
        crate::slash::command_specs()
            .into_iter()
            .filter(|spec| spec.name.starts_with(&query))
            .collect()
    }

    fn command_is_exact(&self) -> bool {
        let Some(command) = self.input.strip_prefix('/') else {
            return false;
        };
        crate::slash::command_specs()
            .into_iter()
            .any(|spec| spec.name == command)
    }

    fn complete_command(&mut self, suggestions: &[crate::slash::CommandSpec]) {
        let Some(spec) = suggestions.get(
            self.command_menu_selected
                .min(suggestions.len().saturating_sub(1)),
        ) else {
            return;
        };
        self.input = format!("/{}", spec.name);
        self.cursor = self.input.chars().count();
        self.command_menu_selected = 0;
    }

    fn submit_input(&mut self) {
        let prompt = self.input.trim().to_string();
        if prompt.is_empty() || self.status.streaming {
            return;
        }
        let parsed = crate::slash::parse(&prompt);
        let requires_model = match &parsed {
            None => true,
            Some(Ok(command)) => {
                crate::slash::is_model_turn(&command.name)
                    && !(command.name == "skills" && command.args.trim().is_empty())
            }
            Some(Err(_)) => false,
        };
        if requires_model && !self.model_available() {
            self.input.clear();
            self.cursor = 0;
            self.command_menu_selected = 0;
            return;
        }
        self.history.push(prompt.clone());
        self.history_index = None;
        self.command_menu_selected = 0;
        self.input.clear();
        self.cursor = 0;
        self.messages
            .push(Message::new(MessageKind::User, "You", prompt.clone()));
        if let Some(parsed) = parsed {
            match parsed {
                Ok(command) => self.handle_slash_command(command),
                Err(error) => self.add_command_message("Command error", error),
            }
            return;
        }
        self.start_stream(&prompt);
    }

    fn handle_slash_command(&mut self, command: crate::slash::SlashCommand) {
        let name = command.name;
        let args = command.args;
        match name.as_str() {
            "help" => self.show_help(),
            "status" => self.show_status(),
            "cost" | "stats" | "usage" => self.show_cost(),
            "version" => self.add_command_message(
                "/version",
                "CLAW_CODE TUI standalone demo. Runtime command handlers remain compatible with the parent CLI.",
            ),
            "model" => self.handle_model_command(&args),
            "login" => self.open_login_dialog(LoginMode::Endpoint),
            "setup" => self.open_login_dialog(LoginMode::Provider),
            "logout" => self.logout(),
            "permissions" => self.handle_permissions_command(&args),
            "clear" => {
                self.messages.clear();
                self.add_command_message("/clear", "Session transcript cleared.");
            }
            "exit" | "quit" => self.should_quit = true,
            _ if crate::slash::is_model_turn(&name)
                && !(name == "skills" && args.trim().is_empty()) =>
            {
                let prompt = if args.trim().is_empty() {
                    format!("/{name}")
                } else {
                    format!("/{name} {args}")
                };
                self.start_stream(&prompt);
            }
            _ => self.add_command_message(
                format!("/{name}"),
                format!(
                    "Recognized /{name}. It stayed on the command path and was not sent as a model prompt.\n\n{}\n\nThis standalone demo will use the production handler when the runtime bridge is connected.",
                    command_summary(&name)
                ),
            ),
        }
    }

    fn show_help(&mut self) {
        let body = crate::slash::command_specs()
            .iter()
            .map(|spec| format!("/{} — {}", spec.name, spec.summary))
            .collect::<Vec<_>>()
            .join("\n");
        self.add_command_message("/help", body);
    }

    fn show_status(&mut self) {
        let provider = crate::config::load_provider();
        let connection = if provider
            .api_key
            .as_deref()
            .is_some_and(|key| !key.is_empty())
        {
            format!(
                "connected ({})",
                provider.kind.as_deref().unwrap_or("provider")
            )
        } else {
            "credential-free demo; use /login".to_string()
        };
        self.add_command_message(
            "/status",
            format!(
                "Model: {}\nMode: {}\nBranch: {}\nConnection: {}\nTokens: {} in / {} out\nCost: \x24{}.{:02}",
                model_label(&self.status.model),
                self.status.mode(),
                self.status.branch,
                connection,
                self.status.input_tokens,
                self.status.output_tokens,
                self.status.cost_cents / 100,
                self.status.cost_cents % 100,
            ),
        );
    }

    fn show_cost(&mut self) {
        self.add_command_message(
            "/cost",
            format!(
                "Input tokens: {}\nOutput tokens: {}\nEstimated cost: \x24{}.{:02}",
                self.status.input_tokens,
                self.status.output_tokens,
                self.status.cost_cents / 100,
                self.status.cost_cents % 100,
            ),
        );
    }

    fn handle_permissions_command(&mut self, args: &str) {
        let mode = args.trim().to_ascii_lowercase();
        if mode.is_empty() {
            self.add_command_message(
                "/permissions",
                format!(
                    "Current mode: {}\nShift+Tab cycles ask → plan → workspace → bypass.",
                    self.status.mode()
                ),
            );
            return;
        }
        let Some(index) = (match mode.as_str() {
            "ask" | "prompt" => Some(0),
            "plan" | "read-only" => Some(1),
            "workspace" | "workspace-write" => Some(2),
            "bypass" | "danger-full-access" => Some(3),
            _ => None,
        }) else {
            self.add_command_message("/permissions", "Use ask, plan, workspace, or bypass.");
            return;
        };
        self.status.mode_index = index;
        self.add_command_message(
            "/permissions",
            format!("Permission mode changed to {}.", self.status.mode()),
        );
    }

    fn handle_model_command(&mut self, args: &str) {
        let mut parts = args.split_whitespace();
        match parts.next() {
            None | Some("list") if parts.next().is_none() => self.open_model_picker(),
            Some("add") => {
                let alias = parts.next().map(str::to_string);
                let model = parts.next().map(str::to_string);
                if parts.next().is_some() {
                    self.add_command_message(
                        "/model",
                        "Usage: /model add [alias] [provider/model]",
                    );
                } else if let (Some(alias), Some(model)) = (alias.as_deref(), model.as_deref()) {
                    self.add_model_alias(alias, model);
                } else {
                    self.open_model_add_dialog(alias.as_deref());
                }
            }
            Some("remove") => {
                let alias = parts.next().map(str::to_string);
                if parts.next().is_some() {
                    self.add_command_message("/model", "Usage: /model remove [alias]");
                } else if let Some(alias) = alias {
                    self.remove_model_alias(&alias);
                } else {
                    self.open_remove_alias_picker();
                }
            }
            Some(model) if parts.next().is_none() => self.switch_model(model),
            _ => self.add_command_message(
                "/model",
                "Usage: /model [model]\n       /model add [alias] [provider/model]\n       /model remove [alias]",
            ),
        }
    }

    fn open_model_picker(&mut self) {
        let aliases = crate::config::load_aliases();
        let mut items = Vec::new();
        if !self.status.model.trim().is_empty() {
            items.push(ModelChoice {
                label: "current".to_string(),
                value: self.status.model.clone(),
                detail: "active configured model".to_string(),
                action: ModelChoiceAction::Switch,
            });
        }
        items.extend(aliases.into_iter().map(|(label, value)| ModelChoice {
            label,
            value: resolve_model_alias_with_config(&value),
            detail: format!("alias → {value}"),
            action: ModelChoiceAction::Switch,
        }));
        items.push(ModelChoice {
            label: "custom…".to_string(),
            value: String::new(),
            detail: "type any provider/model".to_string(),
            action: ModelChoiceAction::Custom,
        });
        items.push(ModelChoice {
            label: "add alias…".to_string(),
            value: String::new(),
            detail: "persist a named model".to_string(),
            action: ModelChoiceAction::AddAlias,
        });
        let selected = items
            .iter()
            .position(|item| item.value == self.status.model)
            .unwrap_or(0);
        self.overlay = Some(Overlay::ModelPicker {
            title: format!("Select model · current {}", model_label(&self.status.model)),
            items,
            selected,
        });
    }

    fn open_remove_alias_picker(&mut self) {
        let aliases = crate::config::load_aliases();
        if aliases.is_empty() {
            self.show_notice("Remove model alias", "No user-defined aliases exist.");
            return;
        }
        let items = aliases
            .into_iter()
            .map(|(label, value)| ModelChoice {
                label,
                value: value.clone(),
                detail: format!("remove alias → {value}"),
                action: ModelChoiceAction::RemoveAlias,
            })
            .collect();
        self.overlay = Some(Overlay::ModelPicker {
            title: "Remove model alias".to_string(),
            items,
            selected: 0,
        });
    }

    fn activate_model_choice(&mut self, choice: ModelChoice) {
        match choice.action {
            ModelChoiceAction::Switch => self.switch_model(&choice.value),
            ModelChoiceAction::Custom => {
                self.overlay = Some(Overlay::Input(InputDialog {
                    title: "Switch model".to_string(),
                    label: "Provider/model".to_string(),
                    value: String::new(),
                    cursor: 0,
                    secret: false,
                    action: InputAction::SwitchModel,
                }));
            }
            ModelChoiceAction::AddAlias => self.open_model_add_dialog(None),
            ModelChoiceAction::RemoveAlias => self.remove_model_alias(&choice.label),
        }
    }

    fn open_model_add_dialog(&mut self, alias: Option<&str>) {
        if let Some(alias) = alias {
            self.overlay = Some(Overlay::Input(InputDialog {
                title: "Add model alias".to_string(),
                label: format!("Model for alias {alias}"),
                value: String::new(),
                cursor: 0,
                secret: false,
                action: InputAction::AddAliasModel {
                    alias: alias.to_string(),
                },
            }));
        } else {
            self.overlay = Some(Overlay::Input(InputDialog {
                title: "Add model alias".to_string(),
                label: "Alias (e.g. mini)".to_string(),
                value: String::new(),
                cursor: 0,
                secret: false,
                action: InputAction::AddAliasName,
            }));
        }
    }

    fn open_login_dialog(&mut self, mode: LoginMode) {
        self.overlay = Some(Overlay::Login(login_dialog(mode)));
    }

    fn submit_input_dialog(&mut self, dialog: InputDialog) {
        let value = dialog.value.trim().to_string();
        match dialog.action {
            InputAction::SwitchModel => {
                if value.is_empty() {
                    self.show_notice("Switch model", "Model unchanged.");
                } else {
                    self.switch_model(&value);
                }
            }
            InputAction::AddAliasName => {
                if value.is_empty() {
                    self.show_notice("Add model alias", "Alias cannot be empty.");
                } else if reserved_model_alias(&value) {
                    self.show_notice(
                        "Add model alias",
                        format!("{value} is reserved; choose another alias."),
                    );
                } else {
                    self.open_model_add_dialog(Some(&value));
                }
            }
            InputAction::AddAliasModel { alias } => {
                if value.is_empty() {
                    self.show_notice("Add model alias", "Model cannot be empty.");
                } else {
                    self.add_model_alias(&alias, &value);
                }
            }
        }
    }

    fn add_model_alias(&mut self, alias: &str, model: &str) {
        if alias.trim().is_empty() || model.trim().is_empty() {
            self.show_notice("Add model alias", "Alias and model are required.");
            return;
        }
        if reserved_model_alias(alias) {
            self.show_notice(
                "Add model alias",
                format!("{alias} is reserved; choose another alias."),
            );
            return;
        }
        let resolved = resolve_model_alias_with_config(model);
        match crate::config::save_alias(alias, &resolved) {
            Ok(path) => self.add_command_message(
                "/model add",
                format!(
                    "Model alias added.\n\n{alias} resolves to {resolved}.\nSaved to {}.",
                    path.display()
                ),
            ),
            Err(error) => self.show_notice("Add model alias", error.to_string()),
        }
    }

    fn remove_model_alias(&mut self, alias: &str) {
        match crate::config::remove_alias(alias) {
            Ok(true) => {
                self.add_command_message("/model remove", format!("Model alias {alias} removed."))
            }
            Ok(false) => self.show_notice(
                "Remove model alias",
                format!("Model alias {alias} was not found."),
            ),
            Err(error) => self.show_notice("Remove model alias", error.to_string()),
        }
    }

    fn switch_model(&mut self, model: &str) {
        let resolved = resolve_model_alias_with_config(model);
        if resolved.is_empty() {
            self.show_notice("Switch model", "Model cannot be empty.");
            return;
        }
        let previous = self.status.model.clone();
        self.status.model = resolved.clone();
        self.add_command_message(
            "/model",
            format!("Model switched from {previous} to {resolved}."),
        );
    }

    fn submit_login_field(&mut self, dialog: &mut LoginDialog) -> bool {
        match dialog.field {
            LoginField::Provider => {
                let provider = match dialog.mode {
                    LoginMode::Endpoint => normalize_endpoint_provider(&dialog.value),
                    LoginMode::Provider => normalize_provider(&dialog.value),
                };
                let Some(provider) = provider else {
                    self.show_notice(
                        "Login",
                        match dialog.mode {
                            LoginMode::Endpoint => {
                                "Connection must be 1/2 or openai-compatible/anthropic-compatible."
                            }
                            LoginMode::Provider => {
                                "Provider must be 1/2/3/4/5 or anthropic, xai, openai, dashscope, custom."
                            }
                        },
                    );
                    return true;
                };
                dialog.provider = provider.to_string();
                dialog.field = LoginField::ApiKey;
                dialog.value.clear();
                dialog.cursor = 0;
            }
            LoginField::ApiKey => {
                if !dialog.value.trim().is_empty() {
                    dialog.api_key = dialog.value.trim().to_string();
                }
                dialog.field = LoginField::BaseUrl;
                dialog.value.clone_from(&dialog.base_url);
                dialog.cursor = dialog.value.chars().count();
            }
            LoginField::BaseUrl => {
                if dialog.value.trim().is_empty() {
                    dialog.base_url = provider_base_url(&dialog.provider).to_string();
                } else {
                    dialog.base_url = dialog.value.trim().to_string();
                }
                dialog.field = LoginField::Model;
                dialog.value.clone_from(&dialog.model);
                dialog.cursor = dialog.value.chars().count();
            }
            LoginField::Model => {
                if dialog.value.trim().is_empty() {
                    self.show_notice(
                        "Login",
                        "Model is required. Enter the provider model name before saving.",
                    );
                    return true;
                }
                dialog.model = dialog.value.trim().to_string();
                let result = match dialog.mode {
                    LoginMode::Endpoint => crate::config::save_login(
                        &dialog.provider,
                        &dialog.api_key,
                        &dialog.base_url,
                        &dialog.model,
                    ),
                    LoginMode::Provider => crate::config::save_provider(
                        &dialog.provider,
                        &dialog.api_key,
                        Some(&dialog.base_url),
                        Some(&dialog.model),
                    ),
                };
                match result {
                    Ok(path) => {
                        self.status.model = resolve_model_alias_with_config(&dialog.model);
                        let command_name = if dialog.mode == LoginMode::Endpoint {
                            "/login"
                        } else {
                            "/setup"
                        };
                        self.add_command_message(
                            command_name,
                            format!(
                                "Saved {} provider settings for {}.\n\nCredentials saved to {} with owner-only permissions.",
                                dialog.provider, self.status.model, path.display()
                            ),
                        );
                    }
                    Err(error) => self.show_notice("Login", error.to_string()),
                }
                return true;
            }
        }
        false
    }

    fn logout(&mut self) {
        match crate::config::clear_provider() {
            Ok(()) => self.add_command_message(
                "/logout",
                "Stored provider credentials and model settings removed.",
            ),
            Err(error) => self.show_notice("Logout", error.to_string()),
        }
    }

    fn add_command_message(&mut self, title: impl Into<String>, body: impl Into<String>) {
        self.messages
            .push(Message::new(MessageKind::Command, title, body));
        self.follow_output = true;
    }

    fn show_notice(&mut self, title: impl Into<String>, body: impl Into<String>) {
        self.overlay = Some(Overlay::Notice {
            title: title.into(),
            body: body.into(),
        });
    }

    fn insert_char(&mut self, character: char) {
        let byte = self
            .input
            .char_indices()
            .nth(self.cursor)
            .map_or(self.input.len(), |(byte, _)| byte);
        self.input.insert(byte, character);
        self.cursor += 1;
        self.command_menu_selected = 0;
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = self
            .input
            .char_indices()
            .nth(self.cursor - 1)
            .map(|(byte, _)| byte);
        let end = self
            .input
            .char_indices()
            .nth(self.cursor)
            .map_or(self.input.len(), |(byte, _)| byte);
        if let Some(start) = start {
            self.input.replace_range(start..end, "");
            self.cursor -= 1;
            self.command_menu_selected = 0;
        }
    }

    fn history_previous(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let index = self
            .history_index
            .unwrap_or(self.history.len())
            .saturating_sub(1);
        self.history_index = Some(index);
        self.input = self.history[index].clone();
        self.cursor = self.input.chars().count();
        self.command_menu_selected = 0;
    }

    fn history_next(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 >= self.history.len() {
            self.history_index = None;
            self.input.clear();
            self.cursor = 0;
            self.command_menu_selected = 0;
        } else {
            self.history_index = Some(index + 1);
            self.input = self.history[index + 1].clone();
            self.cursor = self.input.chars().count();
            self.command_menu_selected = 0;
        }
    }

    fn scroll_up(&mut self, amount: u16) {
        self.follow_output = false;
        self.scroll = self.scroll.saturating_sub(amount);
    }

    fn scroll_down(&mut self, amount: u16) {
        self.follow_output = false;
        self.scroll = self.scroll.saturating_add(amount);
    }

    fn draw(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();
        frame.render_widget(Block::default().style(self.theme.base()), area);

        let input_height = self.input.lines().count().max(1).saturating_add(2).min(6) as u16;
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(5),
                Constraint::Length(input_height),
                Constraint::Length(1),
            ])
            .split(area);

        if area.width >= 108 {
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(75), Constraint::Percentage(25)])
                .split(vertical[0]);
            self.draw_transcript(frame, columns[0]);
            self.draw_sidebar(frame, columns[1]);
        } else {
            self.draw_transcript(frame, vertical[0]);
        }
        self.draw_input(frame, vertical[1]);
        self.draw_command_menu(frame, vertical[1]);
        self.draw_status(frame, vertical[2]);
        self.draw_overlay(frame);
    }

    fn draw_transcript(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let lines = self.transcript_lines();
        let block = Block::default()
            .title(Line::from(vec![
                Span::styled(" CLAW_CODE ", self.theme.title()),
                Span::styled("/", self.theme.muted()),
                Span::styled(" transcript ", self.theme.muted()),
            ]))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(self.theme.border())
            .style(self.theme.base());
        let inner = block.inner(area);
        let mut paragraph = Paragraph::new(Text::from(lines))
            .block(block)
            .wrap(Wrap { trim: false });
        let max_scroll = paragraph
            .line_count(inner.width)
            .saturating_sub(2)
            .saturating_sub(inner.height as usize)
            .min(u16::MAX as usize) as u16;
        if self.follow_output {
            self.scroll = max_scroll;
        } else {
            self.scroll = self.scroll.min(max_scroll);
            if self.scroll >= max_scroll {
                self.follow_output = true;
            }
        }
        paragraph = paragraph.scroll((self.scroll, 0));
        frame.render_widget(paragraph, area);
    }

    fn transcript_lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for message in &self.messages {
            let (icon, color) = match message.kind {
                MessageKind::User => ('›', self.theme.accent),
                MessageKind::Assistant => ('◆', self.theme.heading),
                MessageKind::Thinking => ('◐', self.theme.emphasis),
                MessageKind::Tool => ('⚙', self.theme.strong),
                MessageKind::Command => ('⌘', self.theme.link),
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {icon} "), Style::default().fg(color)),
                Span::styled(
                    message.title.clone(),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
            ]));
            for line in render_markdown(&message.body, self.theme) {
                let mut spans = vec![Span::raw("   ")];
                spans.extend(line.spans);
                lines.push(Line::from(spans));
            }
            lines.push(Line::default());
        }
        if self.status.streaming {
            lines.push(Line::from(Span::styled(
                format!(
                    " ◐ thinking  {}s",
                    self.status.started_at.elapsed().as_secs()
                ),
                Style::default().fg(self.theme.accent),
            )));
        }
        lines
    }

    fn draw_input(&self, frame: &mut Frame<'_>, area: Rect) {
        let block = Block::default()
            .title(Span::styled(" Prompt ", self.theme.title()))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(if self.status.streaming {
                self.theme.muted()
            } else {
                self.theme.prompt()
            })
            .style(self.theme.base());
        let (cursor_line, cursor_column) = self.input_cursor_position();
        let mut lines = Vec::new();
        if self.input.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("  ", self.theme.base()),
                Span::styled(CARET_GLYPH, self.theme.caret()),
                Span::styled(
                    "Ask Claw anything…",
                    self.theme.muted().add_modifier(Modifier::ITALIC),
                ),
            ]));
        } else {
            for (index, line) in self.input.split('\n').enumerate() {
                let mut spans = vec![Span::styled("  ", self.theme.base())];
                if index == cursor_line {
                    let before = line.chars().take(cursor_column).collect::<String>();
                    let after = line.chars().skip(cursor_column).collect::<String>();
                    spans.push(Span::styled(before, self.theme.base()));
                    spans.push(Span::styled(CARET_GLYPH, self.theme.caret()));
                    spans.push(Span::styled(after, self.theme.base()));
                } else {
                    spans.push(Span::styled(line.to_string(), self.theme.base()));
                }
                lines.push(Line::from(spans));
            }
        }
        let inner = block.inner(area);
        let input_scroll = cursor_line.saturating_sub(inner.height.saturating_sub(1) as usize);
        let paragraph = Paragraph::new(Text::from(lines))
            .block(block)
            .style(self.theme.base())
            .scroll((input_scroll.min(u16::MAX as usize) as u16, 0));
        frame.render_widget(paragraph, area);
    }

    fn draw_command_menu(&self, frame: &mut Frame<'_>, input_area: Rect) {
        let suggestions = self.command_suggestions();
        if suggestions.is_empty() || input_area.y < 3 || input_area.width < 24 {
            return;
        }

        let visible_count = suggestions
            .len()
            .min(8)
            .min(input_area.y.saturating_sub(2) as usize);
        if visible_count == 0 {
            return;
        }

        let selected = self
            .command_menu_selected
            .min(suggestions.len().saturating_sub(1));
        let start = selected
            .saturating_sub(visible_count.saturating_sub(1))
            .min(suggestions.len().saturating_sub(visible_count));
        let lines = suggestions[start..start + visible_count]
            .iter()
            .enumerate()
            .map(|(offset, spec)| {
                let index = start + offset;
                let marker = if index == selected { "▸ " } else { "  " };
                let style = if index == selected {
                    self.theme.prompt()
                } else {
                    self.theme.base()
                };
                Line::from(vec![
                    Span::styled(marker, self.theme.accent),
                    Span::styled(format!("/{:<18}", spec.name), style),
                    Span::styled(spec.summary, self.theme.muted()),
                ])
            })
            .collect::<Vec<_>>();
        let height = visible_count.saturating_add(2) as u16;
        let width = input_area.width.saturating_sub(2);
        let popup = Rect::new(
            input_area.x.saturating_add(1),
            input_area.y.saturating_sub(height),
            width,
            height,
        );
        frame.render_widget(Clear, popup);
        let block = Block::default()
            .title(Span::styled(
                " Commands · Tab complete ",
                self.theme.title(),
            ))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(self.theme.accent)
            .style(self.theme.base());
        frame.render_widget(Paragraph::new(Text::from(lines)).block(block), popup);
    }

    fn input_cursor_position(&self) -> (usize, usize) {
        let mut remaining = self.cursor;
        for (line_index, line) in self.input.split('\n').enumerate() {
            let line_length = line.chars().count();
            if remaining <= line_length {
                return (line_index, remaining);
            }
            remaining = remaining.saturating_sub(line_length + 1);
        }
        (self.input.split('\n').count().saturating_sub(1), remaining)
    }

    fn draw_status(&self, frame: &mut Frame<'_>, area: Rect) {
        let state = if self.status.streaming {
            Span::styled("◐ streaming", Style::default().fg(self.theme.accent))
        } else {
            Span::styled("● ready", Style::default().fg(self.theme.success))
        };
        let narrow = area.width < 108;
        let model_limit = if narrow { 24 } else { 32 };
        let model = truncate_model(&model_label(&self.status.model), model_limit);
        let mode = if narrow {
            format!("  {}", self.status.mode())
        } else {
            format!("  mode:{}", self.status.mode())
        };
        let branch = if narrow {
            format!("  {}", self.status.branch)
        } else {
            format!("  branch:{}", self.status.branch)
        };
        let usage = if narrow {
            format!(
                "  {}i/{}o ${}.{:02}",
                self.status.input_tokens,
                self.status.output_tokens,
                self.status.cost_cents / 100,
                self.status.cost_cents % 100
            )
        } else {
            format!(
                "  in:{} out:{}  ${}.{:02}",
                self.status.input_tokens,
                self.status.output_tokens,
                self.status.cost_cents / 100,
                self.status.cost_cents % 100
            )
        };
        let line = Line::from(vec![
            Span::styled("  ", self.theme.muted()),
            state,
            Span::styled(format!("  {model}"), Style::default().fg(self.theme.text)),
            Span::styled(mode, self.status.mode_style(self.theme)),
            Span::styled(branch, self.theme.muted()),
            Span::styled(usage, self.theme.muted()),
        ]);
        let mut line = line;
        if area.width >= 108 {
            line.spans.push(Span::styled(
                "   Enter send · ⇧Tab mode · Esc quit",
                self.theme.muted(),
            ));
        } else {
            line.spans
                .push(Span::styled("   ⇧Tab mode", self.theme.muted()));
        }
        frame.render_widget(
            Paragraph::new(line)
                .alignment(Alignment::Left)
                .style(self.theme.base()),
            area,
        );
    }

    fn draw_overlay(&self, frame: &mut Frame<'_>) {
        let Some(overlay) = &self.overlay else {
            return;
        };
        let area = frame.area();

        match overlay {
            Overlay::Notice { title, body } => {
                let body_lines = render_markdown(body, self.theme);
                let desired_height = body_lines.len().saturating_add(4).min(18) as u16;
                let popup = centered_rect(area, 88, desired_height.max(5));
                frame.render_widget(Clear, popup);
                let block = Block::default()
                    .title(Span::styled(format!(" {title} "), self.theme.title()))
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(self.theme.accent)
                    .style(self.theme.base());
                frame.render_widget(
                    Paragraph::new(Text::from(body_lines))
                        .block(block)
                        .wrap(Wrap { trim: false }),
                    popup,
                );
            }
            Overlay::ModelPicker {
                title,
                items,
                selected,
            } => {
                let lines = items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        let marker = if index == *selected { "▸ " } else { "  " };
                        let label_style = if index == *selected {
                            self.theme.prompt()
                        } else {
                            self.theme.base()
                        };
                        Line::from(vec![
                            Span::styled(marker, self.theme.accent),
                            Span::styled(format!("{:<16}", item.label), label_style),
                            Span::styled(item.detail.clone(), self.theme.muted()),
                        ])
                    })
                    .collect::<Vec<_>>();
                let desired_height = lines.len().saturating_add(4).min(18) as u16;
                let popup = centered_rect(area, 88, desired_height.max(5));
                frame.render_widget(Clear, popup);
                let block = Block::default()
                    .title(Span::styled(format!(" {title} "), self.theme.title()))
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(self.theme.accent)
                    .style(self.theme.base());
                let inner = block.inner(popup);
                let visible_height = inner.height.max(1) as usize;
                let scroll = selected
                    .saturating_sub(visible_height.saturating_sub(1))
                    .min(u16::MAX as usize) as u16;
                frame.render_widget(
                    Paragraph::new(Text::from(lines))
                        .block(block)
                        .scroll((scroll, 0)),
                    popup,
                );
            }
            Overlay::Input(dialog) => {
                let popup = centered_rect(area, 78, 8);
                frame.render_widget(Clear, popup);
                let block = Block::default()
                    .title(Span::styled(
                        format!(" {} ", dialog.title),
                        self.theme.title(),
                    ))
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(self.theme.accent)
                    .style(self.theme.base());
                let display = if dialog.secret {
                    "•".repeat(dialog.value.chars().count())
                } else {
                    dialog.value.clone()
                };
                let value_before = display.chars().take(dialog.cursor).collect::<String>();
                let value_after = display.chars().skip(dialog.cursor).collect::<String>();
                let lines = vec![
                    Line::from(vec![
                        Span::styled(
                            format!("{}: ", dialog.label),
                            Style::default().fg(self.theme.heading),
                        ),
                        Span::styled(value_before, self.theme.text),
                        Span::styled(CARET_GLYPH, self.theme.caret()),
                        Span::styled(value_after, self.theme.text),
                    ]),
                    Line::default(),
                    Line::from(Span::styled(
                        "Enter confirm · Esc cancel",
                        self.theme.muted(),
                    )),
                ];
                frame.render_widget(Paragraph::new(Text::from(lines)).block(block), popup);
            }
            Overlay::Login(dialog) => {
                let popup = centered_rect(area, 88, 12);
                frame.render_widget(Clear, popup);
                let block = Block::default()
                    .title(Span::styled(
                        if dialog.mode == LoginMode::Endpoint {
                            " Login / connection setup "
                        } else {
                            " Setup / provider configuration "
                        },
                        self.theme.title(),
                    ))
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(self.theme.accent)
                    .style(self.theme.base());
                let (label, raw_value, hint) = match dialog.field {
                    LoginField::Provider if dialog.mode == LoginMode::Endpoint => (
                        "Connection",
                        dialog.value.clone(),
                        "1 OpenAI-compatible · 2 Anthropic-compatible",
                    ),
                    LoginField::Provider => (
                        "Provider",
                        dialog.value.clone(),
                        "1 anthropic · 2 xai · 3 openai · 4 dashscope · 5 custom",
                    ),
                    LoginField::ApiKey => (
                        "API key",
                        if dialog.value.is_empty() {
                            if dialog.api_key.is_empty() {
                                String::new()
                            } else {
                                "•".repeat(dialog.api_key.chars().count())
                            }
                        } else {
                            "•".repeat(dialog.value.chars().count())
                        },
                        "Enter keeps the saved key when the field is empty",
                    ),
                    LoginField::BaseUrl => (
                        "Base URL",
                        dialog.value.clone(),
                        "Enter accepts the URL; edit it for an OpenAI-compatible endpoint",
                    ),
                    LoginField::Model => (
                        "Model",
                        dialog.value.clone(),
                        "Enter saves provider settings; a model name is required",
                    ),
                };
                let value_before = raw_value.chars().take(dialog.cursor).collect::<String>();
                let value_after = raw_value.chars().skip(dialog.cursor).collect::<String>();
                let lines = vec![
                    Line::from(Span::styled(
                        format!(
                            "Step: {}",
                            match dialog.field {
                                LoginField::Provider if dialog.mode == LoginMode::Endpoint => {
                                    "connection"
                                }
                                LoginField::Provider => "provider",
                                LoginField::ApiKey => "API key",
                                LoginField::BaseUrl => "base URL",
                                LoginField::Model => "model",
                            }
                        ),
                        self.theme.muted(),
                    )),
                    Line::from(vec![
                        Span::styled(
                            format!("{label}: "),
                            Style::default().fg(self.theme.heading),
                        ),
                        Span::styled(value_before, self.theme.text),
                        Span::styled(CARET_GLYPH, self.theme.caret()),
                        Span::styled(value_after, self.theme.text),
                    ]),
                    Line::from(Span::styled(hint, self.theme.muted())),
                    Line::from(Span::styled("Enter next · Esc cancel", self.theme.muted())),
                ];
                frame.render_widget(Paragraph::new(Text::from(lines)).block(block), popup);
            }
        }
    }

    fn draw_sidebar(&self, frame: &mut Frame<'_>, area: Rect) {
        let todos = self
            .todos
            .iter()
            .map(|todo| {
                let marker = if todo.done { "✓" } else { "·" };
                let color = if todo.done {
                    self.theme.success
                } else {
                    self.theme.muted
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!(" {marker} "), Style::default().fg(color)),
                    Span::styled(todo.label.clone(), Style::default().fg(color)),
                ]))
            })
            .collect::<Vec<_>>();
        let block = Block::default()
            .title(Span::styled(" Context ", self.theme.title()))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(self.theme.border())
            .style(self.theme.base());
        let list = List::new(todos).block(block).style(self.theme.base());
        frame.render_widget(list, area);
    }
}

fn edit_text(value: &mut String, cursor: &mut usize, key: KeyEvent) {
    match key.code {
        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            value.clear();
            *cursor = 0;
        }
        KeyCode::Char(character) => {
            let byte = value
                .char_indices()
                .nth(*cursor)
                .map_or(value.len(), |(byte, _)| byte);
            value.insert(byte, character);
            *cursor += 1;
        }
        KeyCode::Backspace if *cursor > 0 => {
            let start = value.char_indices().nth(*cursor - 1).map(|(byte, _)| byte);
            let end = value
                .char_indices()
                .nth(*cursor)
                .map_or(value.len(), |(byte, _)| byte);
            if let Some(start) = start {
                value.replace_range(start..end, "");
                *cursor -= 1;
            }
        }
        KeyCode::Delete if *cursor < value.chars().count() => {
            let start = value
                .char_indices()
                .nth(*cursor)
                .map(|(byte, _)| byte)
                .unwrap_or(value.len());
            let end = value
                .char_indices()
                .nth(*cursor + 1)
                .map_or(value.len(), |(byte, _)| byte);
            value.replace_range(start..end, "");
        }
        KeyCode::Left => *cursor = (*cursor).saturating_sub(1),
        KeyCode::Right => *cursor = (*cursor + 1).min(value.chars().count()),
        KeyCode::Home => *cursor = 0,
        KeyCode::End => *cursor = value.chars().count(),
        _ => {}
    }
}

fn reserved_model_alias(alias: &str) -> bool {
    matches!(
        alias.trim().to_ascii_lowercase().as_str(),
        "add" | "remove" | "list"
    )
}

fn command_summary(name: &str) -> &'static str {
    crate::slash::command_specs()
        .iter()
        .find(|spec| spec.name == name)
        .map_or(
            "The command is recognized by the Claw Code command surface.",
            |spec| spec.summary,
        )
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x.saturating_add(area.width.saturating_sub(width) / 2),
        area.y
            .saturating_add(area.height.saturating_sub(height) / 2),
        width,
        height,
    )
}

fn truncate_model(model: &str, limit: usize) -> String {
    let chars = model.chars().collect::<Vec<_>>();
    if chars.len() <= limit {
        return model.to_string();
    }
    let keep = limit.saturating_sub(1);
    chars.into_iter().take(keep).chain(['…']).collect()
}

fn spawn_mock_stream(prompt: String, tx: Sender<StreamEvent>) {
    thread::spawn(move || {
        for (index, event) in mock_events(&prompt).into_iter().enumerate() {
            let delay = match event {
                StreamEvent::TextDelta(_) => Duration::from_millis(115),
                StreamEvent::ThinkingDelta(_) => Duration::from_millis(160),
                StreamEvent::ToolOutput(_) => Duration::from_millis(260),
                _ => Duration::from_millis(if index == 0 { 220 } else { 100 }),
            };
            thread::sleep(delay);
            if tx.send(event).is_err() {
                break;
            }
        }
    });
}

fn mock_events(prompt: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::ThinkingStart,
        StreamEvent::Usage {
            input_tokens: 512,
            output_tokens: 0,
            cost_cents: 1,
        },
        StreamEvent::ThinkingDelta(format!(
            "Trace `{prompt}`. Preserve constraints and exact technical values."
        )),
        StreamEvent::ThinkingEnd,
        StreamEvent::ToolStart {
            name: "read_file".to_string(),
            detail: "src/auth/token.rs".to_string(),
        },
        StreamEvent::ToolOutput("✓ 48 lines · auth middleware loaded".to_string()),
        StreamEvent::Usage {
            input_tokens: 1_284,
            output_tokens: 12,
            cost_cents: 1,
        },
        StreamEvent::AssistantStart,
        StreamEvent::TextDelta("**Auth middleware inspected.** ".to_string()),
        StreamEvent::Usage {
            input_tokens: 1_284,
            output_tokens: 32,
            cost_cents: 2,
        },
        StreamEvent::TextDelta(
            "Expired bearer tokens are rejected before handler dispatch. ".to_string(),
        ),
        StreamEvent::Usage {
            input_tokens: 1_284,
            output_tokens: 64,
            cost_cents: 3,
        },
        StreamEvent::TextDelta(
            "Public API unchanged. Next: add regression coverage for exact `401` behavior."
                .to_string(),
        ),
        StreamEvent::Done {
            input_tokens: 1_284,
            output_tokens: 96,
            cost_cents: 4,
        },
    ]
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};

    use super::{
        mock_events, model_label, resolve_model_alias_with_config, App, LoginMode, Message,
        MessageKind, Overlay, StreamEvent,
    };

    #[test]
    fn mock_stream_contains_thinking_tool_and_answer_phases() {
        let events = mock_events("test request");
        assert!(matches!(events[0], StreamEvent::ThinkingStart));
        assert!(events
            .iter()
            .any(|event| matches!(event, StreamEvent::ToolStart { .. })));
        assert!(events
            .iter()
            .any(|event| matches!(event, StreamEvent::TextDelta(_))));
        assert!(events
            .iter()
            .any(|event| matches!(event, StreamEvent::Usage { .. })));
        assert!(matches!(events.last(), Some(StreamEvent::Done { .. })));
    }

    #[test]
    fn app_accumulates_streamed_answer_without_terminal_access() {
        let mut app = App::empty();
        app.apply_stream_event(StreamEvent::AssistantStart);
        app.apply_stream_event(StreamEvent::TextDelta("hello ".to_string()));
        app.apply_stream_event(StreamEvent::TextDelta("world".to_string()));
        app.apply_stream_event(StreamEvent::Done {
            input_tokens: 3,
            output_tokens: 2,
            cost_cents: 1,
        });

        assert_eq!(app.messages.len(), 1);
        assert_eq!(app.messages[0].kind, MessageKind::Assistant);
        assert_eq!(app.messages[0].body, "hello world");
        assert!(!app.status.streaming);
        assert_eq!(app.status.output_tokens, 2);
    }

    #[test]
    fn usage_updates_while_a_turn_is_streaming() {
        let mut app = App::empty();
        app.status.streaming = true;
        app.apply_stream_event(StreamEvent::Usage {
            input_tokens: 128,
            output_tokens: 17,
            cost_cents: 2,
        });

        assert_eq!(app.status.input_tokens, 128);
        assert_eq!(app.status.output_tokens, 17);
        assert_eq!(app.status.cost_cents, 2);
    }

    #[test]
    fn shift_tab_cycles_permission_modes() {
        let mut app = App::empty();
        assert_eq!(app.status.mode(), "ask");

        for expected in ["plan", "workspace", "bypass", "ask"] {
            app.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
            assert_eq!(app.status.mode(), expected);
        }
    }

    #[test]
    fn slash_commands_use_transcript_and_interactive_overlays() {
        let mut app = App::empty();

        app.input = "/status".to_string();
        app.cursor = app.input.chars().count();
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app
            .messages
            .iter()
            .any(|message| message.kind == MessageKind::Command));
        assert!(!app.status.streaming);

        app.input = "/model".to_string();
        app.cursor = app.input.chars().count();
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(&app.overlay, Some(Overlay::ModelPicker { .. })));

        app.overlay = None;
        app.input = "/login".to_string();
        app.cursor = app.input.chars().count();
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            &app.overlay,
            Some(Overlay::Login(dialog)) if dialog.mode == LoginMode::Endpoint
        ));
    }

    #[test]
    fn unconfigured_model_has_no_fallback_value() {
        assert_eq!(model_label(""), "no model");
        assert_eq!(
            resolve_model_alias_with_config("provider/custom"),
            "provider/custom"
        );
        assert_eq!(resolve_model_alias_with_config("sonnet"), "sonnet");
    }

    #[test]
    fn slash_prefix_completes_from_the_live_command_registry() {
        let mut app = App::empty();
        app.input = "/mo".to_string();
        app.cursor = app.input.chars().count();
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.input, "/model");

        app.input = "/per".to_string();
        app.cursor = app.input.chars().count();
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| app.draw(frame))
            .expect("draw commands");
        let visible = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(visible.contains("Commands"));
        assert!(visible.contains("/permissions"));
    }

    #[test]
    fn no_model_does_not_emit_a_turn() {
        let mut app = App::empty();
        app.status.model.clear();
        app.input = "hello without login".to_string();
        app.cursor = app.input.chars().count();

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(!app.status.streaming);
        assert!(app.stream_rx.is_none());
        assert!(app.messages.is_empty());
        assert!(app.input.is_empty());

        app.input = "/login".to_string();
        app.cursor = app.input.chars().count();
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.overlay, Some(Overlay::Login(_))));
    }

    #[test]
    fn input_cursor_editing_keeps_utf8_boundaries() {
        let mut app = App::empty();
        app.insert_char('你');
        app.insert_char('a');
        app.backspace();
        assert_eq!(app.input, "你");
        assert_eq!(app.cursor, 1);
    }

    #[test]
    fn long_transcript_follows_the_latest_response_line() {
        let mut app = App::empty();
        let body = (0..80)
            .map(|index| format!("response line {index}"))
            .chain(["**tail-marker**".to_string()])
            .collect::<Vec<_>>()
            .join("\n");
        app.messages
            .push(Message::new(MessageKind::Assistant, "Claw", body));

        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| app.draw(frame))
            .expect("draw transcript");

        let visible = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(visible.contains("tail-marker"));
    }

    #[test]
    fn prompt_always_contains_a_visible_caret() {
        let mut app = App::empty();
        app.status.model.clear();
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal.draw(|frame| app.draw(frame)).expect("draw prompt");

        let visible = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(visible.contains('█'));
        assert!(!visible.contains('›'));
        assert!(visible.contains("no model"));
    }

    #[test]
    fn narrow_status_keeps_model_usage_and_cost_visible() {
        let mut app = App::empty();
        app.status.model = "provider/a-model-name-that-is-long".to_string();
        app.status.input_tokens = 1_284;
        app.status.output_tokens = 96;
        app.status.cost_cents = 4;
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal.draw(|frame| app.draw(frame)).expect("draw status");

        let visible = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(visible.contains("1284i/96o $0.04"));
        assert!(visible.contains("provider/a-model"));
    }
}
