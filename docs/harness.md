# Claw harness

Claw's harness is a small, project-local convention for making reliable agent
work repeatable. It is deliberately additive: an ordinary Claw project still
works without a `.claw/` directory and without enabling workflow gates.

## The three layers

The harness has three cooperating layers:

1. **Hooks** run automatically at lifecycle boundaries and around tool calls.
   They are useful for guardrails, audit messages, and stop-the-line checks.
2. **Commands** are user-invoked workflow instructions. Markdown files under
   `.claw/commands/` become project slash commands, such as `/start-work` or
   `/pre-pr`.
3. **Skills** are model-invoked expertise packs. Markdown files under
   `.claw/skills/` describe reusable procedures that the `Skill` tool can load.

Agents in `.claw/agents/` provide an optional role layer on top of these three
surfaces. An agent can declare `role: gate` to participate in quality review;
otherwise its role defaults to `implementer`.

Initialize the starter pack with:

```bash
clawcli init --harness
```

The command is write-if-absent. It does not overwrite an existing asset or an
existing `hooks.SessionStart` entry.

## `.claw/` layout

```text
.claw/
├── agents/
│   └── qas.md
├── commands/
│   ├── start-work.md
│   ├── pre-pr.md
│   └── end-work.md
├── skills/
│   └── <skill-name>/SKILL.md
├── settings.json
├── settings.local.json       # local-only settings
└── sessions/                  # created when a session is saved
```

Project assets are discovered from the current directory and its ancestors.
Project assets take precedence over the same-named user assets under
`$CLAW_CONFIG_HOME`. A malformed asset is skipped and reported by
`clawcli doctor`; discovery does not make the whole CLI fail.

## Asset frontmatter

Every asset should start with a simple YAML-style frontmatter block. Values are
single-line strings:

```markdown
---
name: pre-pr
description: Run verification before opening a pull request.
argument-hint: [scope]
---

The markdown body is the command or skill instructions.
```

The shared fields are `name` and `description`. `argument-hint` is accepted for
commands. Agents may additionally use `role: implementer`, `role: reviewer`,
or `role: gate`. A command or agent can omit `name` when its filename provides
an unambiguous name; a skill directory's `SKILL.md` uses the directory name.

Run `clawcli doctor` to see discovered counts and each frontmatter warning.

## Hook JSON protocol

Configure hooks in `.claw.json` under `hooks`. Each supported event maps to an
array of command strings or object entries with a matcher and nested command
hooks:

```json
{
  "hooks": {
    "SessionStart": ["echo 'Workflow harness active'"],
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          { "type": "command", "command": "scripts/audit-bash.sh" }
        ]
      }
    ]
  }
}
```

The hook process receives a JSON object on standard input. Fields are included
when applicable:

```json
{
  "hook_event_name": "PreToolUse",
  "tool_name": "Bash",
  "tool_input": { "command": "cargo test" },
  "prompt": "",
  "cwd": "/path/to/project"
}
```

Exit status `0` allows the event. Exit status `2` blocks it, using stderr as
the reason. A valid JSON response takes precedence over exit-code inference:

```json
{"decision":"block","reason":"Tests must run first"}
```

or:

```json
{"additionalContext":"Workflow is in verification; record test evidence."}
```

For lifecycle events, plain stdout is treated as additional context. This lets
a `SessionStart` hook print workflow guidance without blocking the session.

## Workflow gates

The workflow state machine uses the phases `Idle`, `Spec`, `Implement`,
`Verify`, `Review`, and `Done`. Use `/workflow start`, `/workflow gate`,
`/workflow advance`, and `/workflow status` to manage the state.

| Transition | Required evidence | Meaning |
| --- | --- | --- |
| `Spec → Implement` | Acceptance criteria | Stop-the-line: do not invent requirements. |
| `Implement → Verify` | None | Begin verification. |
| `Verify → Review` | Passing `test_run` evidence | QAS gate: tests must be run and recorded. |
| `Review → Done` | `review` evidence | Review is complete; human merge authority remains external. |

Set `workflow_gates` in `.claw.json` or another settings file:

```json
{
  "workflow_gates": "advisory"
}
```

The supported modes are:

- `off` — do not consult workflow gates (the default and backwards-compatible
  behavior).
- `advisory` — report gate failures as warnings, but allow progress.
- `enforced` — block the gated action until its evidence is present.

`clawcli doctor` reports the active mode in its `Harness` check, along with
skill, command, and agent counts.

## Attribution

The workflow model and stop-the-line/QAS terminology are derived from
[Safe Agentic Workflow (SAW)](https://github.com/bybren-llc/safe-agentic-workflow),
an MIT-licensed project by J. Scott Graham / ByBren LLC. Claw's implementation
is a smaller, native Rust adaptation and is not a bundled copy of SAW.
