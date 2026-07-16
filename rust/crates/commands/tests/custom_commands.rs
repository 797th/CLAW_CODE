//! Integration tests for user-defined markdown slash commands
//! (`.claw/commands/*.md`), parsed via `SlashCommand::parse_with_commands`.

use std::fs;
use std::path::Path;

use commands::{
    is_builtin_command_name, render_project_commands_help, resolve_custom_commands, SlashCommand,
};
use runtime::harness_assets::CommandMeta;

fn write_md(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, contents).expect("write");
}

fn command_meta(dir: &Path, name: &str, description: &str, body: &str) -> CommandMeta {
    let path = dir.join(format!("{name}.md"));
    write_md(
        &path,
        &format!("---\nname: {name}\ndescription: {description}\n---\n{body}\n"),
    );
    CommandMeta {
        name: name.to_string(),
        description: description.to_string(),
        path,
        argument_hint: None,
    }
}

#[test]
fn parses_custom_command_and_substitutes_arguments() {
    let temp = tempfile::tempdir().expect("tempdir");
    let meta = command_meta(
        temp.path(),
        "start-work",
        "Start a work session",
        "Starting work on $ARGUMENTS now.",
    );
    let registry = vec![meta];

    let parsed = SlashCommand::parse_with_commands("/start-work FOO-123", &registry)
        .expect("parse should succeed")
        .expect("should produce a command");

    assert_eq!(
        parsed,
        SlashCommand::Custom {
            name: "start-work".to_string(),
            body: "Starting work on FOO-123 now.".to_string(),
        }
    );
}

#[test]
fn substitutes_empty_string_when_no_arguments_given() {
    let temp = tempfile::tempdir().expect("tempdir");
    let meta = command_meta(
        temp.path(),
        "pre-pr",
        "Pre-PR checklist",
        "Arguments were: [$ARGUMENTS]",
    );
    let registry = vec![meta];

    let parsed = SlashCommand::parse_with_commands("/pre-pr", &registry)
        .expect("parse should succeed")
        .expect("should produce a command");

    assert_eq!(
        parsed,
        SlashCommand::Custom {
            name: "pre-pr".to_string(),
            body: "Arguments were: []".to_string(),
        }
    );
}

#[test]
fn joins_multiple_arguments_verbatim() {
    let temp = tempfile::tempdir().expect("tempdir");
    let meta = command_meta(temp.path(), "note", "Take a note", "$ARGUMENTS");
    let registry = vec![meta];

    let parsed = SlashCommand::parse_with_commands("/note fix the flaky test suite", &registry)
        .expect("parse should succeed")
        .expect("should produce a command");

    assert_eq!(
        parsed,
        SlashCommand::Custom {
            name: "note".to_string(),
            body: "fix the flaky test suite".to_string(),
        }
    );
}

#[test]
fn frontmatter_only_command_file_parses_to_empty_body_without_panicking() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("noop.md");
    // Frontmatter block with no trailing body content at all (not even a
    // blank line) — the degenerate case for `strip_frontmatter_block`.
    write_md(&path, "---\nname: noop\ndescription: Does nothing\n---\n");
    let meta = CommandMeta {
        name: "noop".to_string(),
        description: "Does nothing".to_string(),
        path,
        argument_hint: None,
    };
    let registry = vec![meta];

    let parsed = SlashCommand::parse_with_commands("/noop", &registry)
        .expect("parse should succeed")
        .expect("should produce a command");

    assert_eq!(
        parsed,
        SlashCommand::Custom {
            name: "noop".to_string(),
            body: String::new(),
        }
    );
}

#[test]
fn unknown_command_name_still_errors_as_unknown() {
    let registry: Vec<CommandMeta> = Vec::new();
    let parsed = SlashCommand::parse_with_commands("/totally-not-a-command", &registry)
        .expect("parse should succeed");

    assert_eq!(
        parsed,
        Some(SlashCommand::Unknown("totally-not-a-command".to_string()))
    );
}

#[test]
fn builtin_help_is_unaffected_by_custom_command_registry() {
    let temp = tempfile::tempdir().expect("tempdir");
    let meta = command_meta(temp.path(), "help", "Shadow help", "should never win");
    let registry = vec![meta];

    let parsed =
        SlashCommand::parse_with_commands("/help", &registry).expect("parse should succeed");

    assert_eq!(parsed, Some(SlashCommand::Help));
}

#[test]
fn conflicting_custom_command_is_skipped_with_a_warning() {
    let temp = tempfile::tempdir().expect("tempdir");
    let conflicting = command_meta(temp.path(), "help", "Shadow help", "nope");
    let unique = command_meta(temp.path(), "start-work", "Start work", "$ARGUMENTS");

    let (kept, warnings) = resolve_custom_commands(vec![conflicting, unique]);

    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].name, "start-work");
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("help"));
    assert!(is_builtin_command_name("help"));
    assert!(!is_builtin_command_name("start-work"));
}

#[test]
fn project_commands_help_section_lists_frontmatter_descriptions() {
    let temp = tempfile::tempdir().expect("tempdir");
    let meta = command_meta(temp.path(), "start-work", "Start a work session", "body");
    let rendered = render_project_commands_help(std::slice::from_ref(&meta));

    assert!(rendered.contains("Project commands"));
    assert!(rendered.contains("/start-work"));
    assert!(rendered.contains("Start a work session"));
}

#[test]
fn empty_registry_renders_no_project_commands_section() {
    assert_eq!(render_project_commands_help(&[]), "");
}
