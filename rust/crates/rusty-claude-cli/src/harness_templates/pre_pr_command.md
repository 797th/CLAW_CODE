---
description: Run the test suite, record the gate, and summarize the diff before opening a PR.
---
Pre-PR check. Extra context (if given): $ARGUMENTS

1. Determine this project's test/verification command (check CLAUDE.md or
   repo conventions) and run it with the Bash tool. Capture the real output —
   do not summarize from memory.
2. Once the run completes, record the result with:
   `/workflow gate test_run <one-line summary of pass/fail and counts>`
3. Run a diff of the current changes (e.g. `git diff`) and summarize what
   changed, file by file, for the user.
4. If the test run failed, stop here and report the failure — do not proceed
   to opening a PR until it is green.
