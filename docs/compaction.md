# Context compaction

Claw compacts a session before sending a request when the estimated request
context reaches the configured threshold. The default threshold is 100,000
input tokens and can be overridden with
`CLAUDE_CODE_AUTO_COMPACT_INPUT_TOKENS`. Set the variable to `0` to disable
automatic compaction.

The compactor follows four rules:

1. Keep the newest context by token budget, with a preferred recent-message
   floor. Automatic compaction targets 75% of the trigger threshold.
2. Cut only at safe message boundaries. A tool call and its result are treated
   as one protocol unit, so compaction cannot create an orphaned `tool` record.
3. Bound the material used to build a checkpoint. Tool results are summarized
   with a short preview, the timeline keeps the beginning and newest entries,
   and the structured summary has a fixed token ceiling. If a retained tool
   result alone is too large, its context copy keeps the head and tail with a
   rerun marker.
4. Persist cumulative checkpoint state. Each session checkpoint records token
   counts, the first retained message index, goals, constraints, progress,
   decisions, next steps, and read/modified file lists. Older session files
   without these fields continue to load with empty defaults.

Context-window errors still have a retry path in the CLI. The retry progressively
lowers the target budget, re-evaluates the compacted request, and stops when a
pass makes no change instead of retrying the same oversized payload.
