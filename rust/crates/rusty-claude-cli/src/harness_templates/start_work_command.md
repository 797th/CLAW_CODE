---
description: Begin a gated work session against a task reference.
---
Start of work. Task reference (if given): $ARGUMENTS

1. If no task reference was supplied above, ask the user for one now (an
   issue ID, ticket key, or short description of the work).
2. Ask the user for the acceptance criteria for this task, if they were not
   already provided. Do not proceed on assumed or inferred criteria.
3. Run `/workflow start <ref>` (substituting the task reference) to open the
   gated workflow for this task.
4. Read back the acceptance criteria to the user in your own words and get
   explicit confirmation that you have understood them correctly.
5. Only after the user confirms the acceptance criteria should you begin any
   implementation work.
