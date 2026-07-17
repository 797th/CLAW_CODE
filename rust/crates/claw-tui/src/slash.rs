//! Slash-command parsing and command-surface metadata for the TUI.
//!
//! The names mirror the command registry in `CLAW_CODE/rust/crates/commands`.
//! Keeping parsing here means a slash command is never accidentally sent as a
//! normal model prompt while the full-screen frontend owns the interactive surface.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommand {
    pub name: String,
    pub args: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandSpec {
    pub name: &'static str,
    pub summary: &'static str,
}

const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "help",
        summary: "Show available slash commands",
    },
    CommandSpec {
        name: "status",
        summary: "Show current session status",
    },
    CommandSpec {
        name: "sandbox",
        summary: "Show sandbox isolation status",
    },
    CommandSpec {
        name: "compact",
        summary: "Compact local session history",
    },
    CommandSpec {
        name: "model",
        summary: "Show, switch, add, or remove a model",
    },
    CommandSpec {
        name: "permissions",
        summary: "Show or switch the active permission mode",
    },
    CommandSpec {
        name: "clear",
        summary: "Start a fresh local session",
    },
    CommandSpec {
        name: "cost",
        summary: "Show cumulative token usage",
    },
    CommandSpec {
        name: "resume",
        summary: "Load a saved session",
    },
    CommandSpec {
        name: "config",
        summary: "Inspect merged configuration",
    },
    CommandSpec {
        name: "mcp",
        summary: "Inspect configured MCP servers",
    },
    CommandSpec {
        name: "memory",
        summary: "Inspect loaded instruction memory",
    },
    CommandSpec {
        name: "dream",
        summary: "Consolidate project memory",
    },
    CommandSpec {
        name: "init",
        summary: "Create starter project instructions",
    },
    CommandSpec {
        name: "diff",
        summary: "Show the current git diff",
    },
    CommandSpec {
        name: "version",
        summary: "Show CLI version information",
    },
    CommandSpec {
        name: "bughunter",
        summary: "Inspect the codebase for bugs",
    },
    CommandSpec {
        name: "commit",
        summary: "Generate and create a git commit",
    },
    CommandSpec {
        name: "pr",
        summary: "Draft or create a pull request",
    },
    CommandSpec {
        name: "issue",
        summary: "Draft or create a GitHub issue",
    },
    CommandSpec {
        name: "ultraplan",
        summary: "Run a deep planning turn",
    },
    CommandSpec {
        name: "teleport",
        summary: "Jump to a file or symbol",
    },
    CommandSpec {
        name: "debug-tool-call",
        summary: "Replay the last tool call",
    },
    CommandSpec {
        name: "export",
        summary: "Export the current conversation",
    },
    CommandSpec {
        name: "session",
        summary: "List or switch saved sessions",
    },
    CommandSpec {
        name: "plugin",
        summary: "Inspect or reload plugins",
    },
    CommandSpec {
        name: "agents",
        summary: "Inspect configured agents",
    },
    CommandSpec {
        name: "skills",
        summary: "List or invoke skills",
    },
    CommandSpec {
        name: "doctor",
        summary: "Run configuration diagnostics",
    },
    CommandSpec {
        name: "setup",
        summary: "Open provider setup",
    },
    CommandSpec {
        name: "history",
        summary: "Show prompt history",
    },
    CommandSpec {
        name: "stats",
        summary: "Show session statistics",
    },
    CommandSpec {
        name: "login",
        summary: "Configure provider credentials",
    },
    CommandSpec {
        name: "logout",
        summary: "Remove stored provider credentials",
    },
    CommandSpec {
        name: "exit",
        summary: "Exit the TUI",
    },
    CommandSpec {
        name: "quit",
        summary: "Exit the TUI",
    },
    CommandSpec {
        name: "plan",
        summary: "Run or inspect planning mode",
    },
    CommandSpec {
        name: "review",
        summary: "Review a scope or change",
    },
    CommandSpec {
        name: "tasks",
        summary: "Inspect background tasks",
    },
    CommandSpec {
        name: "theme",
        summary: "Select a color theme",
    },
    CommandSpec {
        name: "voice",
        summary: "Configure voice mode",
    },
    CommandSpec {
        name: "usage",
        summary: "Show token usage",
    },
    CommandSpec {
        name: "rename",
        summary: "Rename the current session",
    },
    CommandSpec {
        name: "copy",
        summary: "Copy conversation content",
    },
    CommandSpec {
        name: "hooks",
        summary: "Inspect configured hooks",
    },
    CommandSpec {
        name: "context",
        summary: "Inspect workspace context",
    },
    CommandSpec {
        name: "color",
        summary: "Configure terminal colors",
    },
    CommandSpec {
        name: "effort",
        summary: "Set reasoning effort",
    },
    CommandSpec {
        name: "branch",
        summary: "Set or inspect the branch",
    },
    CommandSpec {
        name: "rewind",
        summary: "Rewind recent conversation",
    },
    CommandSpec {
        name: "ide",
        summary: "Configure IDE integration",
    },
    CommandSpec {
        name: "tag",
        summary: "Tag the current session",
    },
    CommandSpec {
        name: "output-style",
        summary: "Select an output style",
    },
    CommandSpec {
        name: "add-dir",
        summary: "Add a trusted workspace directory",
    },
    CommandSpec {
        name: "team",
        summary: "Inspect team workflows",
    },
    CommandSpec {
        name: "workflow",
        summary: "Inspect workflow gates",
    },
];

// The command crate contains a larger built-in surface than the handlers that
// are currently rendered locally. Keep these names recognized and on the
// command path so they are never mistaken for ordinary model prompts while
// the runtime bridge is being completed.
const EXTRA_COMMANDS: &[&str] = &[
    "vim",
    "upgrade",
    "share",
    "feedback",
    "files",
    "fast",
    "summary",
    "desktop",
    "brief",
    "advisor",
    "stickers",
    "insights",
    "thinkback",
    "release-notes",
    "security-review",
    "keybindings",
    "privacy-settings",
    "allowed-tools",
    "api-key",
    "approve",
    "deny",
    "undo",
    "stop",
    "retry",
    "paste",
    "screenshot",
    "image",
    "terminal-setup",
    "search",
    "listen",
    "speak",
    "language",
    "profile",
    "max-tokens",
    "temperature",
    "system-prompt",
    "tool-details",
    "format",
    "pin",
    "unpin",
    "bookmarks",
    "workspace",
    "tokens",
    "cache",
    "providers",
    "notifications",
    "changelog",
    "test",
    "lint",
    "build",
    "run",
    "git",
    "stash",
    "blame",
    "log",
    "cron",
    "benchmark",
    "migrate",
    "reset",
    "telemetry",
    "env",
    "project",
    "templates",
    "explain",
    "refactor",
    "docs",
    "fix",
    "perf",
    "chat",
    "focus",
    "unfocus",
    "web",
    "map",
    "symbols",
    "references",
    "definition",
    "hover",
    "diagnostics",
    "autofix",
    "multi",
    "macro",
    "alias",
    "parallel",
    "agent",
    "subagent",
    "reasoning",
    "budget",
    "rate-limit",
    "metrics",
];

#[must_use]
pub fn command_specs() -> Vec<CommandSpec> {
    let mut specs = COMMANDS.to_vec();
    specs.extend(EXTRA_COMMANDS.iter().map(|name| CommandSpec {
        name,
        summary: "Built-in command handled by the Claw Code runtime",
    }));
    specs
}

#[must_use]
pub fn parse(input: &str) -> Option<Result<SlashCommand, String>> {
    let trimmed = input.trim();
    if !trimmed.starts_with('/') {
        return None;
    }

    let without_prefix = trimmed.trim_start_matches('/');
    let mut parts = without_prefix.splitn(2, char::is_whitespace);
    let raw_name = parts.next().unwrap_or_default().to_ascii_lowercase();
    let name = canonical_name(&raw_name).to_string();
    let args = parts.next().unwrap_or_default().trim().to_string();
    if name.is_empty() {
        return Some(Err("Slash command name is missing. Use /help.".to_string()));
    }
    if !command_specs().iter().any(|command| command.name == name) {
        return Some(Err(format!(
            "Unknown slash command `/{name}`. Use /help to list commands."
        )));
    }
    Some(Ok(SlashCommand { name, args }))
}

fn canonical_name(name: &str) -> &str {
    match name {
        "plugins" | "marketplace" => "plugin",
        "skill" => "skills",
        "cwd" => "workspace",
        "yes" | "y" => "approve",
        "no" | "n" => "deny",
        name => name,
    }
}

#[must_use]
pub fn is_model_turn(name: &str) -> bool {
    matches!(
        name,
        "bughunter"
            | "commit"
            | "pr"
            | "issue"
            | "ultraplan"
            | "teleport"
            | "debug-tool-call"
            | "dream"
            | "plan"
            | "review"
            | "skills"
            | "team"
            | "workflow"
    )
}

#[cfg(test)]
mod tests {
    use super::{is_model_turn, parse};

    #[test]
    fn parses_known_commands_without_turning_them_into_prompts() {
        assert_eq!(
            parse("/model add mini openai/gpt-4.1-mini"),
            Some(Ok(super::SlashCommand {
                name: "model".to_string(),
                args: "add mini openai/gpt-4.1-mini".to_string(),
            }))
        );
        assert!(is_model_turn("commit"));
        assert!(!is_model_turn("status"));
        assert_eq!(parse("/plugins"), parse("/plugin"));
        assert_eq!(parse("/cwd"), parse("/workspace"));
        assert!(parse("/diagnostics src/main.rs").is_some_and(|result| result.is_ok()));
    }

    #[test]
    fn rejects_unknown_commands() {
        let result = parse("/not-a-command").expect("slash input");
        assert!(result.is_err());
    }
}
