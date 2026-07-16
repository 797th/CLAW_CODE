---
name: verification-before-completion
description: Evidence before claims — run the command, capture the output, only then claim success.
---
# Verification before completion

Never claim work is complete, fixed, or passing without having just run the
verification command and looked at its real output.

## The rule

1. Run the actual command (tests, build, lint, the specific repro steps) with
   the Bash tool.
2. Read the real output — do not predict, summarize from memory, or assume
   it would have passed.
3. Only after seeing passing output should you tell the user the work is
   done, fixed, or verified.

## Why this matters

- "Should work" and "does work" are different claims. Only the second is
  backed by evidence.
- If a command fails or can't be run, say so plainly instead of describing
  what you expect would happen.
- If verification is genuinely not possible in this environment, state that
  explicitly rather than substituting a confident-sounding guess.
