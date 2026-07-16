---
description: Wrap up a gated work session — commit, then advance the workflow to closure.
---
End of work. Extra context (if given): $ARGUMENTS

1. Check for uncommitted changes (e.g. `git status`). If there are any,
   summarize them for the user and prompt them to commit (or commit yourself
   if the user has asked you to handle commits directly).
2. Do not leave uncommitted work behind — confirm the working tree is clean
   before proceeding.
3. Run `/workflow advance` to move the gated workflow to closure.
4. Report the final state to the user, including any gate that is still
   outstanding.
