use std::collections::BTreeSet;

use crate::session::{CompactionDetails, ContentBlock, ConversationMessage, MessageRole, Session};

const COMPACT_CONTINUATION_PREAMBLE: &str =
    "This session is being continued from a previous conversation that ran out of context. The summary below covers the earlier portion of the conversation.\n\n";
const COMPACT_RECENT_MESSAGES_NOTE: &str = "Recent messages are preserved verbatim.";
const COMPACT_DIRECT_RESUME_INSTRUCTION: &str = "Continue the conversation from where it left off without asking the user any further questions. Resume directly — do not acknowledge the summary, do not recap what was happening, and do not preface with continuation text.";
/// The amount of recent context retained by automatic compaction before it
/// starts reducing the tail to satisfy a hard target.
pub const DEFAULT_COMPACTION_KEEP_RECENT_TOKENS: usize = 20_000;
const DEFAULT_COMPACTION_MAX_SUMMARY_TOKENS: usize = 8_192;
const DEFAULT_TOOL_RESULT_SUMMARY_CHARS: usize = 2_000;
const DEFAULT_TOOL_RESULT_CONTEXT_CHARS: usize = 12_000;
const MAX_SUMMARY_CHARS: usize = DEFAULT_COMPACTION_MAX_SUMMARY_TOKENS * 4;
const MAX_TIMELINE_ITEMS: usize = 48;
const MAX_SUMMARY_ITEMS_PER_SECTION: usize = 12;

/// Thresholds controlling when and how a session is compacted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionConfig {
    pub preserve_recent_messages: usize,
    pub max_estimated_tokens: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            preserve_recent_messages: 4,
            max_estimated_tokens: 10_000,
        }
    }
}

/// Result of compacting a session into a summary plus preserved tail messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionResult {
    pub summary: String,
    pub formatted_summary: String,
    pub compacted_session: Session,
    pub removed_message_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CompactionBudget {
    keep_recent_tokens: usize,
    max_summary_tokens: usize,
    max_tool_result_summary_chars: usize,
    max_tool_result_context_chars: usize,
}

impl Default for CompactionBudget {
    fn default() -> Self {
        Self {
            keep_recent_tokens: DEFAULT_COMPACTION_KEEP_RECENT_TOKENS,
            max_summary_tokens: DEFAULT_COMPACTION_MAX_SUMMARY_TOKENS,
            max_tool_result_summary_chars: DEFAULT_TOOL_RESULT_SUMMARY_CHARS,
            max_tool_result_context_chars: DEFAULT_TOOL_RESULT_CONTEXT_CHARS,
        }
    }
}

fn unchanged_compaction_result(session: &Session) -> CompactionResult {
    CompactionResult {
        summary: String::new(),
        formatted_summary: String::new(),
        compacted_session: session.clone(),
        removed_message_count: 0,
    }
}

/// Roughly estimates the token footprint of the current session transcript.
#[must_use]
pub fn estimate_session_tokens(session: &Session) -> usize {
    session.messages.iter().map(estimate_message_tokens).sum()
}

/// Returns `true` when the session exceeds the configured compaction budget.
#[must_use]
pub fn should_compact(session: &Session, config: CompactionConfig) -> bool {
    let start = compacted_summary_prefix_len(session);
    let compactable = &session.messages[start..];

    compactable.len() > config.preserve_recent_messages
        && compactable
            .iter()
            .map(estimate_message_tokens)
            .sum::<usize>()
            >= config.max_estimated_tokens
}

/// Normalizes a compaction summary into user-facing continuation text.
#[must_use]
pub fn format_compact_summary(summary: &str) -> String {
    let without_analysis = strip_tag_block(summary, "analysis");
    let formatted = if let Some(content) = extract_tag_block(&without_analysis, "summary") {
        without_analysis.replace(
            &format!("<summary>{content}</summary>"),
            &format!("Summary:\n{}", content.trim()),
        )
    } else {
        without_analysis
    };

    collapse_blank_lines(&formatted).trim().to_string()
}

/// Builds the synthetic system message used after session compaction.
#[must_use]
pub fn get_compact_continuation_message(
    summary: &str,
    suppress_follow_up_questions: bool,
    recent_messages_preserved: bool,
) -> String {
    let mut base = format!(
        "{COMPACT_CONTINUATION_PREAMBLE}{}",
        format_compact_summary(summary)
    );

    if recent_messages_preserved {
        base.push_str("\n\n");
        base.push_str(COMPACT_RECENT_MESSAGES_NOTE);
    }

    if suppress_follow_up_questions {
        base.push('\n');
        base.push_str(COMPACT_DIRECT_RESUME_INSTRUCTION);
    }

    base
}

/// Compacts a session by summarizing older messages and preserving the recent tail.
#[must_use]
pub fn compact_session(session: &Session, config: CompactionConfig) -> CompactionResult {
    compact_session_with_budget(session, config, CompactionBudget::default())
}

fn compact_session_with_budget(
    session: &Session,
    config: CompactionConfig,
    budget: CompactionBudget,
) -> CompactionResult {
    if !should_compact(session, config) {
        return unchanged_compaction_result(session);
    }

    let prefix_len = compacted_summary_prefix_len(session);
    let raw_keep_from = if config.preserve_recent_messages == 0 {
        session.messages.len()
    } else {
        session
            .messages
            .len()
            .saturating_sub(config.preserve_recent_messages)
    };
    let keep_from = safe_boundary_at_or_before(session, prefix_len, raw_keep_from);
    compact_at_boundary(session, keep_from, budget)
}

/// Compacts a session until its estimated token footprint falls below a target.
///
/// The helper starts from the preferred recent-message retention count and then
/// progressively relaxes that retention until the compacted session estimate
/// fits inside `target_estimated_tokens` or no further compaction is possible.
#[must_use]
pub fn compact_session_to_target(
    session: &Session,
    preferred_preserve_recent_messages: usize,
    target_estimated_tokens: usize,
) -> CompactionResult {
    let target_estimated_tokens = target_estimated_tokens.max(1);
    if estimate_session_tokens(session) <= target_estimated_tokens {
        return unchanged_compaction_result(session);
    }

    let prefix_len = compacted_summary_prefix_len(session);
    let compactable_len = session.messages.len().saturating_sub(prefix_len);
    if compactable_len <= 1 {
        return unchanged_compaction_result(session);
    }

    let budget = CompactionBudget {
        keep_recent_tokens: target_estimated_tokens
            .saturating_mul(3)
            .saturating_div(4)
            .min(DEFAULT_COMPACTION_KEEP_RECENT_TOKENS),
        max_summary_tokens: target_estimated_tokens
            .saturating_div(3)
            .clamp(256, DEFAULT_COMPACTION_MAX_SUMMARY_TOKENS),
        ..CompactionBudget::default()
    };
    let preferred_count = preferred_preserve_recent_messages.min(compactable_len);
    let count_boundary = safe_boundary_at_or_before(
        session,
        prefix_len,
        session.messages.len().saturating_sub(preferred_count),
    );
    let token_boundary =
        safe_boundary_for_recent_tokens(session, prefix_len, budget.keep_recent_tokens);
    // Keep the preferred number of messages when it fits inside the normal
    // recent-token window. A single large tool result is allowed to force a
    // more aggressive cut in later candidate passes.
    let preferred_boundary = count_boundary.min(token_boundary);
    let cut_points = valid_cut_points(session, prefix_len);
    let start = cut_points
        .iter()
        .position(|point| *point >= preferred_boundary)
        .unwrap_or(cut_points.len().saturating_sub(1));

    // Evaluating every possible cut can become quadratic for a long session
    // because each candidate has to build a summary. Sample the valid cuts
    // and always include the most aggressive cut as a guaranteed fallback.
    let mut candidate_indices = Vec::new();
    let remaining = cut_points.len().saturating_sub(start);
    let stride = (remaining / 64).max(1);
    let mut index = start;
    while index < cut_points.len() && candidate_indices.len() < 64 {
        candidate_indices.push(index);
        index = index.saturating_add(stride);
    }
    if cut_points.last().is_some_and(|point| {
        candidate_indices
            .last()
            .is_none_or(|last| cut_points[*last] != *point)
    }) {
        candidate_indices.push(cut_points.len() - 1);
    }

    let mut best_result: Option<CompactionResult> = None;
    let mut best_estimate = usize::MAX;
    for candidate_index in candidate_indices {
        let keep_from = cut_points[candidate_index];
        if keep_from <= prefix_len {
            continue;
        }
        let candidate = fit_compacted_candidate(
            compact_at_boundary(session, keep_from, budget),
            target_estimated_tokens,
            budget,
        );
        let candidate_estimate = estimate_session_tokens(&candidate.compacted_session);
        if candidate_estimate < best_estimate {
            best_estimate = candidate_estimate;
            best_result = Some(candidate.clone());
        }
        if candidate_estimate <= target_estimated_tokens {
            return candidate;
        }
    }

    best_result.unwrap_or_else(|| unchanged_compaction_result(session))
}

fn compact_at_boundary(
    session: &Session,
    keep_from: usize,
    budget: CompactionBudget,
) -> CompactionResult {
    let prefix_len = compacted_summary_prefix_len(session);
    if keep_from <= prefix_len || keep_from > session.messages.len() {
        return unchanged_compaction_result(session);
    }

    let existing_summary = session
        .messages
        .first()
        .and_then(extract_existing_compacted_summary);
    let removed = &session.messages[prefix_len..keep_from];
    if removed.is_empty() {
        return unchanged_compaction_result(session);
    }
    let preserved = session.messages[keep_from..].to_vec();
    let summary = merge_compact_summaries(
        existing_summary.as_deref(),
        &summarize_messages(removed, budget),
        budget.max_summary_tokens,
    );
    let formatted_summary = format_compact_summary(&summary);
    let continuation = get_compact_continuation_message(&summary, true, !preserved.is_empty());

    let mut compacted_messages = vec![ConversationMessage {
        role: MessageRole::System,
        blocks: vec![ContentBlock::Text { text: continuation }],
        usage: None,
    }];
    compacted_messages.extend(preserved);

    let tokens_before = estimate_session_tokens(session);
    let mut compacted_session = session.clone();
    compacted_session.messages = compacted_messages;
    let tokens_after = estimate_session_tokens(&compacted_session);
    let details = compaction_details_from_messages(removed);
    compacted_session.record_compaction_with_details(
        summary.clone(),
        removed.len(),
        tokens_before,
        tokens_after,
        keep_from,
        details,
    );

    CompactionResult {
        summary,
        formatted_summary,
        compacted_session,
        removed_message_count: removed.len(),
    }
}

fn fit_compacted_candidate(
    mut candidate: CompactionResult,
    target_estimated_tokens: usize,
    budget: CompactionBudget,
) -> CompactionResult {
    if estimate_session_tokens(&candidate.compacted_session) <= target_estimated_tokens {
        return candidate;
    }

    let (truncated_session, changed) = truncate_preserved_tool_results(
        &candidate.compacted_session,
        budget.max_tool_result_context_chars,
    );
    if changed {
        candidate.compacted_session = truncated_session;
        let tokens_after = estimate_session_tokens(&candidate.compacted_session);
        if let Some(compaction) = candidate.compacted_session.compaction.as_mut() {
            compaction.tokens_after = tokens_after;
        }
    }
    candidate
}

fn truncate_preserved_tool_results(session: &Session, max_chars: usize) -> (Session, bool) {
    let mut compacted_session = session.clone();
    let mut changed = false;
    for message in &mut compacted_session.messages {
        for block in &mut message.blocks {
            if let ContentBlock::ToolResult { output, .. } = block {
                if output.chars().count() > max_chars {
                    *output = truncate_for_context(output, max_chars);
                    changed = true;
                }
            }
        }
    }
    (compacted_session, changed)
}

fn truncate_for_context(content: &str, max_chars: usize) -> String {
    if content.chars().count() <= max_chars {
        return content.to_string();
    }
    let marker = "\n… [tool result truncated for context; rerun the tool if needed] …\n";
    let available = max_chars.saturating_sub(marker.chars().count());
    let head = available.saturating_mul(2) / 3;
    let tail = available.saturating_sub(head);
    let head_text = content.chars().take(head).collect::<String>();
    let tail_text = content
        .chars()
        .rev()
        .take(tail)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("{head_text}{marker}{tail_text}")
}

fn valid_cut_points(session: &Session, prefix_len: usize) -> Vec<usize> {
    let mut points = session
        .messages
        .iter()
        .enumerate()
        .skip(prefix_len.saturating_add(1))
        .filter_map(|(index, message)| {
            (!message
                .blocks
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolResult { .. })))
            .then_some(index)
        })
        .collect::<Vec<_>>();
    points.push(session.messages.len());
    points
}

fn safe_boundary_at_or_before(session: &Session, prefix_len: usize, raw: usize) -> usize {
    let raw = raw.clamp(prefix_len + 1, session.messages.len());
    valid_cut_points(session, prefix_len)
        .into_iter()
        .filter(|point| *point > prefix_len && *point <= raw)
        .next_back()
        .unwrap_or(prefix_len + 1)
}

fn safe_boundary_for_recent_tokens(
    session: &Session,
    prefix_len: usize,
    keep_recent_tokens: usize,
) -> usize {
    if keep_recent_tokens == 0 {
        return session.messages.len();
    }

    let mut accumulated: usize = 0;
    for index in (prefix_len..session.messages.len()).rev() {
        accumulated = accumulated.saturating_add(estimate_message_tokens(&session.messages[index]));
        if accumulated >= keep_recent_tokens {
            let first_valid_after = valid_cut_points(session, prefix_len)
                .into_iter()
                .find(|point| *point >= index);
            return first_valid_after.unwrap_or(session.messages.len());
        }
    }
    prefix_len + 1
}

fn compacted_summary_prefix_len(session: &Session) -> usize {
    usize::from(
        session
            .messages
            .first()
            .and_then(extract_existing_compacted_summary)
            .is_some(),
    )
}

#[derive(Debug, Clone, Default)]
struct SummaryFields {
    goals: Vec<String>,
    constraints: Vec<String>,
    progress_done: Vec<String>,
    progress_in_progress: Vec<String>,
    progress_blocked: Vec<String>,
    decisions: Vec<String>,
    next_steps: Vec<String>,
    critical_context: Vec<String>,
}

fn summarize_messages(messages: &[ConversationMessage], budget: CompactionBudget) -> String {
    let user_messages = messages
        .iter()
        .filter(|message| message.role == MessageRole::User)
        .count();
    let assistant_messages = messages
        .iter()
        .filter(|message| message.role == MessageRole::Assistant)
        .count();
    let tool_messages = messages
        .iter()
        .filter(|message| message.role == MessageRole::Tool)
        .count();

    let mut tool_names = messages
        .iter()
        .flat_map(|message| message.blocks.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolUse { name, .. } => Some(name.as_str()),
            ContentBlock::ToolResult { tool_name, .. } => Some(tool_name.as_str()),
            ContentBlock::Text { .. } | ContentBlock::Thinking { .. } => None,
        })
        .collect::<Vec<_>>();
    tool_names.sort_unstable();
    tool_names.dedup();

    let mut fields = summary_fields_from_messages(messages);
    fields.critical_context.insert(
        0,
        format!(
            "Scope: {} earlier messages compacted (user={}, assistant={}, tool={}).",
            messages.len(),
            user_messages,
            assistant_messages,
            tool_messages
        ),
    );
    if !tool_names.is_empty() {
        fields
            .critical_context
            .push(format!("Tools mentioned: {}.", tool_names.join(", ")));
    }
    let key_files = collect_key_files(messages);
    if !key_files.is_empty() {
        fields
            .critical_context
            .push(format!("Key files referenced: {}.", key_files.join(", ")));
    }
    let (read_files, modified_files) = collect_file_operations(messages);
    if !read_files.is_empty() {
        fields
            .critical_context
            .push(format!("Read files: {}.", read_files.join(", ")));
    }
    if !modified_files.is_empty() {
        fields
            .critical_context
            .push(format!("Modified files: {}.", modified_files.join(", ")));
    }

    let timeline = timeline_messages(messages, budget.max_tool_result_summary_chars);
    if !timeline.is_empty() {
        fields
            .critical_context
            .push(format!("Key timeline: {}", timeline.join(" | ")));
    }

    render_summary_fields(&fields, budget.max_summary_tokens)
}

fn merge_compact_summaries(
    existing_summary: Option<&str>,
    new_summary: &str,
    max_summary_tokens: usize,
) -> String {
    let Some(existing_summary) = existing_summary else {
        return cap_summary(new_summary, max_summary_tokens);
    };

    let mut merged = parse_summary_fields(existing_summary);
    let new_fields = parse_summary_fields(new_summary);
    merge_summary_fields(&mut merged, &new_fields);
    if !new_fields.critical_context.is_empty() {
        merged
            .critical_context
            .push("Newly compacted context:".to_string());
        merged
            .critical_context
            .extend(new_fields.critical_context.iter().cloned());
    }
    render_summary_fields(&merged, max_summary_tokens)
}

fn summary_fields_from_messages(messages: &[ConversationMessage]) -> SummaryFields {
    let mut fields = SummaryFields::default();
    let user_texts = messages
        .iter()
        .filter(|message| message.role == MessageRole::User)
        .filter_map(first_text_block)
        .map(|text| truncate_summary(text, 240))
        .collect::<Vec<_>>();
    fields.goals = take_recent(user_texts, MAX_SUMMARY_ITEMS_PER_SECTION);

    for message in messages {
        let Some(text) = first_text_block(message) else {
            continue;
        };
        let text = truncate_summary(text, 240);
        let lowered = text.to_ascii_lowercase();
        if message.role == MessageRole::User
            && contains_any(
                &lowered,
                &["must ", "should ", "need to", "required", "never ", "only "],
            )
        {
            push_unique(
                &mut fields.constraints,
                text.clone(),
                MAX_SUMMARY_ITEMS_PER_SECTION,
            );
        }
        if message.role == MessageRole::Assistant
            && contains_any(
                &lowered,
                &[
                    "fixed ",
                    "implemented",
                    "completed",
                    "finished",
                    "updated ",
                    "added ",
                    "removed ",
                    "passed",
                    "built ",
                    "merged",
                ],
            )
        {
            push_unique(
                &mut fields.progress_done,
                text.clone(),
                MAX_SUMMARY_ITEMS_PER_SECTION,
            );
        }
        if contains_any(
            &lowered,
            &["error", "failed", "failure", "blocked", "unable"],
        ) {
            push_unique(
                &mut fields.progress_blocked,
                text.clone(),
                MAX_SUMMARY_ITEMS_PER_SECTION,
            );
        }
        if contains_any(
            &lowered,
            &[
                "decided",
                "decision",
                "choose ",
                "chose ",
                "we'll use",
                "using ",
            ],
        ) {
            push_unique(
                &mut fields.decisions,
                text.clone(),
                MAX_SUMMARY_ITEMS_PER_SECTION,
            );
        }
    }

    if let Some(current) = infer_current_work(messages) {
        fields.progress_in_progress.push(current);
    }
    fields.next_steps = infer_pending_work(messages);
    fields
}

fn compaction_details_from_messages(messages: &[ConversationMessage]) -> CompactionDetails {
    let fields = summary_fields_from_messages(messages);
    let (read_files, modified_files) = collect_file_operations(messages);
    let mut details = CompactionDetails {
        goals: fields.goals,
        constraints: fields.constraints,
        progress_done: fields.progress_done,
        progress_in_progress: fields.progress_in_progress,
        progress_blocked: fields.progress_blocked,
        decisions: fields.decisions,
        next_steps: fields.next_steps,
        read_files,
        modified_files,
    };
    // Plain-text paths are useful when a tool uses a custom name that cannot
    // be classified as read/write/edit. Keep them as read context rather than
    // silently dropping them from the cumulative file index.
    for file in collect_key_files(messages) {
        push_unique(
            &mut details.read_files,
            file,
            MAX_SUMMARY_ITEMS_PER_SECTION * 4,
        );
    }
    details
}

fn render_summary_fields(fields: &SummaryFields, max_summary_tokens: usize) -> String {
    let mut lines = vec![
        "<summary>".to_string(),
        "Conversation summary:".to_string(),
        "## Goal".to_string(),
    ];
    append_items(&mut lines, &fields.goals, "(none)");
    lines.push("## Constraints & Preferences".to_string());
    append_items(&mut lines, &fields.constraints, "(none)");
    lines.push("## Progress".to_string());
    lines.push("### Done".to_string());
    append_items(&mut lines, &fields.progress_done, "(none)");
    lines.push("### In Progress".to_string());
    append_items(&mut lines, &fields.progress_in_progress, "(none)");
    lines.push("### Blocked".to_string());
    append_items(&mut lines, &fields.progress_blocked, "(none)");
    lines.push("## Key Decisions".to_string());
    append_items(&mut lines, &fields.decisions, "(none)");
    lines.push("## Next Steps".to_string());
    if fields.next_steps.is_empty() {
        lines.push("1. (none)".to_string());
    } else {
        lines.extend(
            fields
                .next_steps
                .iter()
                .enumerate()
                .map(|(index, item)| format!("{}. {item}", index + 1)),
        );
    }
    lines.push("## Critical Context".to_string());
    append_items(&mut lines, &fields.critical_context, "(none)");
    lines.push("</summary>".to_string());
    cap_summary(&lines.join("\n"), max_summary_tokens)
}

fn append_items(lines: &mut Vec<String>, items: &[String], empty: &str) {
    if items.is_empty() {
        lines.push(format!("- {empty}"));
    } else {
        lines.extend(items.iter().map(|item| format!("- {item}")));
    }
}

fn parse_summary_fields(summary: &str) -> SummaryFields {
    let mut fields = SummaryFields::default();
    let mut section = "";
    for raw_line in format_compact_summary(summary).lines() {
        let line = raw_line.trim();
        if line.is_empty() || line == "Summary:" || line == "Conversation summary:" {
            continue;
        }
        if let Some(heading) = line.strip_prefix("## ") {
            section = heading;
            continue;
        }
        if let Some(heading) = line.strip_prefix("### ") {
            section = heading;
            continue;
        }
        let value = line
            .strip_prefix("- ")
            .or_else(|| line.split_once(". ").map(|(_, value)| value))
            .unwrap_or(line)
            .trim();
        if value == "(none)" {
            continue;
        }
        match section {
            "Goal" => push_unique(
                &mut fields.goals,
                value.to_string(),
                MAX_SUMMARY_ITEMS_PER_SECTION,
            ),
            "Constraints & Preferences" => push_unique(
                &mut fields.constraints,
                value.to_string(),
                MAX_SUMMARY_ITEMS_PER_SECTION,
            ),
            "Done" => push_unique(
                &mut fields.progress_done,
                value.to_string(),
                MAX_SUMMARY_ITEMS_PER_SECTION,
            ),
            "In Progress" => push_unique(
                &mut fields.progress_in_progress,
                value.to_string(),
                MAX_SUMMARY_ITEMS_PER_SECTION,
            ),
            "Blocked" => push_unique(
                &mut fields.progress_blocked,
                value.to_string(),
                MAX_SUMMARY_ITEMS_PER_SECTION,
            ),
            "Key Decisions" => push_unique(
                &mut fields.decisions,
                value.to_string(),
                MAX_SUMMARY_ITEMS_PER_SECTION,
            ),
            "Next Steps" => push_unique(
                &mut fields.next_steps,
                value.to_string(),
                MAX_SUMMARY_ITEMS_PER_SECTION,
            ),
            "Critical Context" | _ => push_unique(
                &mut fields.critical_context,
                value.to_string(),
                MAX_SUMMARY_ITEMS_PER_SECTION * 4,
            ),
        }
    }
    fields
}

fn merge_summary_fields(target: &mut SummaryFields, incoming: &SummaryFields) {
    merge_summary_items(
        &mut target.goals,
        &incoming.goals,
        MAX_SUMMARY_ITEMS_PER_SECTION,
    );
    merge_summary_items(
        &mut target.constraints,
        &incoming.constraints,
        MAX_SUMMARY_ITEMS_PER_SECTION,
    );
    merge_summary_items(
        &mut target.progress_done,
        &incoming.progress_done,
        MAX_SUMMARY_ITEMS_PER_SECTION,
    );
    merge_summary_items(
        &mut target.progress_in_progress,
        &incoming.progress_in_progress,
        MAX_SUMMARY_ITEMS_PER_SECTION,
    );
    merge_summary_items(
        &mut target.progress_blocked,
        &incoming.progress_blocked,
        MAX_SUMMARY_ITEMS_PER_SECTION,
    );
    merge_summary_items(
        &mut target.decisions,
        &incoming.decisions,
        MAX_SUMMARY_ITEMS_PER_SECTION,
    );
    merge_summary_items(
        &mut target.next_steps,
        &incoming.next_steps,
        MAX_SUMMARY_ITEMS_PER_SECTION,
    );
}

fn merge_summary_items(target: &mut Vec<String>, incoming: &[String], limit: usize) {
    for item in incoming {
        push_unique(target, item.clone(), limit);
    }
}

fn push_unique(items: &mut Vec<String>, item: String, limit: usize) {
    let item = item.trim().to_string();
    if !item.is_empty() && !items.iter().any(|existing| existing == &item) {
        items.push(item);
    }
    if items.len() > limit {
        let remove_count = items.len() - limit;
        items.drain(..remove_count);
    }
}

fn take_recent(mut items: Vec<String>, limit: usize) -> Vec<String> {
    if items.len() > limit {
        let remove_count = items.len() - limit;
        items.drain(..remove_count);
    }
    items
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

fn timeline_messages(messages: &[ConversationMessage], tool_result_chars: usize) -> Vec<String> {
    let mut timeline = messages
        .iter()
        .map(|message| {
            let role = match message.role {
                MessageRole::System => "system",
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::Tool => "tool",
            };
            let content = message
                .blocks
                .iter()
                .map(|block| summarize_block(block, tool_result_chars))
                .collect::<Vec<_>>()
                .join(" | ");
            format!("{role}: {content}")
        })
        .collect::<Vec<_>>();
    if timeline.len() > MAX_TIMELINE_ITEMS {
        let keep_head = 8;
        let tail_start = timeline.len() - (MAX_TIMELINE_ITEMS - keep_head);
        let mut compacted = timeline[..keep_head].to_vec();
        compacted.push("… [middle of timeline omitted] …".to_string());
        compacted.extend(timeline.drain(tail_start..));
        timeline = compacted;
    }
    timeline
}

fn cap_summary(summary: &str, max_summary_tokens: usize) -> String {
    let max_chars = max_summary_tokens
        .saturating_mul(4)
        .clamp(512, MAX_SUMMARY_CHARS);
    if summary.chars().count() <= max_chars {
        return summary.to_string();
    }
    let marker = "\n- Additional historical detail omitted to stay within the compaction budget.\n";
    let closing = "</summary>";
    let available = max_chars
        .saturating_sub(marker.chars().count())
        .saturating_sub(closing.chars().count());
    let head = summary.chars().take(available).collect::<String>();
    format!("{head}{marker}{closing}")
}

fn summarize_block(block: &ContentBlock, tool_result_chars: usize) -> String {
    let raw = match block {
        ContentBlock::Text { text } => text.clone(),
        ContentBlock::Thinking { thinking, .. } => {
            format!("thinking ({} chars)", thinking.chars().count())
        }
        ContentBlock::ToolUse { name, input, .. } => format!("tool_use {name}({input})"),
        ContentBlock::ToolResult {
            tool_name,
            output,
            is_error,
            ..
        } => format!(
            "tool_result {tool_name}: {}{output}",
            if *is_error { "error " } else { "" }
        ),
    };
    let max_chars = if matches!(block, ContentBlock::ToolResult { .. }) {
        tool_result_chars
    } else {
        160
    };
    truncate_summary(&raw, max_chars)
}

fn infer_pending_work(messages: &[ConversationMessage]) -> Vec<String> {
    messages
        .iter()
        .rev()
        .filter_map(first_text_block)
        .filter(|text| {
            let lowered = text.to_ascii_lowercase();
            lowered.contains("todo")
                || lowered.contains("next")
                || lowered.contains("pending")
                || lowered.contains("follow up")
                || lowered.contains("remaining")
        })
        .take(3)
        .map(|text| truncate_summary(text, 160))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn collect_key_files(messages: &[ConversationMessage]) -> Vec<String> {
    let mut files = messages
        .iter()
        .flat_map(|message| message.blocks.iter())
        .map(|block| match block {
            ContentBlock::Text { text } => text.as_str(),
            ContentBlock::ToolUse { input, .. } => input.as_str(),
            ContentBlock::ToolResult { output, .. } => output.as_str(),
            ContentBlock::Thinking { thinking, .. } => thinking.as_str(),
        })
        .flat_map(extract_file_candidates)
        .collect::<Vec<_>>();
    files.sort();
    files.dedup();
    files.into_iter().take(8).collect()
}

fn collect_file_operations(messages: &[ConversationMessage]) -> (Vec<String>, Vec<String>) {
    let mut read_files = BTreeSet::new();
    let mut modified_files = BTreeSet::new();
    for message in messages {
        for block in &message.blocks {
            let (tool_name, path) = match block {
                ContentBlock::ToolUse { name, input, .. } => {
                    (name.as_str(), extract_path_from_json(input))
                }
                ContentBlock::ToolResult {
                    tool_name, output, ..
                } => (tool_name.as_str(), extract_path_from_json(output)),
                ContentBlock::Text { .. } | ContentBlock::Thinking { .. } => continue,
            };
            let Some(path) = path else {
                continue;
            };
            match tool_name {
                "read_file" | "Read" => {
                    read_files.insert(path);
                }
                "write_file" | "Write" | "edit_file" | "Edit" => {
                    modified_files.insert(path);
                }
                _ => {}
            }
        }
    }
    (
        read_files
            .into_iter()
            .take(MAX_SUMMARY_ITEMS_PER_SECTION * 4)
            .collect(),
        modified_files
            .into_iter()
            .take(MAX_SUMMARY_ITEMS_PER_SECTION * 4)
            .collect(),
    )
}

fn extract_path_from_json(content: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(content).ok()?;
    value
        .get("path")
        .or_else(|| value.get("file_path"))
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
}

fn infer_current_work(messages: &[ConversationMessage]) -> Option<String> {
    messages
        .iter()
        .rev()
        .filter_map(first_text_block)
        .find(|text| !text.trim().is_empty())
        .map(|text| truncate_summary(text, 200))
}

fn first_text_block(message: &ConversationMessage) -> Option<&str> {
    message.blocks.iter().find_map(|block| match block {
        ContentBlock::Text { text } if !text.trim().is_empty() => Some(text.as_str()),
        ContentBlock::ToolUse { .. }
        | ContentBlock::ToolResult { .. }
        | ContentBlock::Thinking { .. }
        | ContentBlock::Text { .. } => None,
    })
}

fn has_interesting_extension(candidate: &str) -> bool {
    std::path::Path::new(candidate)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["rs", "ts", "tsx", "js", "json", "md"]
                .iter()
                .any(|expected| extension.eq_ignore_ascii_case(expected))
        })
}

fn extract_file_candidates(content: &str) -> Vec<String> {
    content
        .split_whitespace()
        .filter_map(|token| {
            let candidate = token.trim_matches(|char: char| {
                matches!(char, ',' | '.' | ':' | ';' | ')' | '(' | '"' | '\'' | '`')
            });
            if candidate.contains('/') && has_interesting_extension(candidate) {
                Some(candidate.to_string())
            } else {
                None
            }
        })
        .collect()
}

fn truncate_summary(content: &str, max_chars: usize) -> String {
    if content.chars().count() <= max_chars {
        return content.to_string();
    }
    let mut truncated = content.chars().take(max_chars).collect::<String>();
    truncated.push('…');
    truncated
}

fn estimate_message_tokens(message: &ConversationMessage) -> usize {
    message
        .blocks
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => text.len() / 4 + 1,
            ContentBlock::ToolUse { name, input, .. } => (name.len() + input.len()) / 4 + 1,
            ContentBlock::ToolResult {
                tool_name, output, ..
            } => (tool_name.len() + output.len()) / 4 + 1,
            ContentBlock::Thinking {
                thinking,
                signature,
            } => thinking.len() / 4 + signature.as_ref().map_or(0, |value| value.len() / 4 + 1),
        })
        .sum()
}

fn extract_tag_block(content: &str, tag: &str) -> Option<String> {
    let start = format!("<{tag}>");
    let end = format!("</{tag}>");
    let start_index = content.find(&start)? + start.len();
    let end_index = content[start_index..].find(&end)? + start_index;
    Some(content[start_index..end_index].to_string())
}

fn strip_tag_block(content: &str, tag: &str) -> String {
    let start = format!("<{tag}>");
    let end = format!("</{tag}>");
    if let (Some(start_index), Some(end_index_rel)) = (content.find(&start), content.find(&end)) {
        let end_index = end_index_rel + end.len();
        let mut stripped = String::new();
        stripped.push_str(&content[..start_index]);
        stripped.push_str(&content[end_index..]);
        stripped
    } else {
        content.to_string()
    }
}

fn collapse_blank_lines(content: &str) -> String {
    let mut result = String::new();
    let mut last_blank = false;
    for line in content.lines() {
        let is_blank = line.trim().is_empty();
        if is_blank && last_blank {
            continue;
        }
        result.push_str(line);
        result.push('\n');
        last_blank = is_blank;
    }
    result
}

fn extract_existing_compacted_summary(message: &ConversationMessage) -> Option<String> {
    if message.role != MessageRole::System {
        return None;
    }

    let text = first_text_block(message)?;
    let summary = text.strip_prefix(COMPACT_CONTINUATION_PREAMBLE)?;
    let summary = summary
        .split_once(&format!("\n\n{COMPACT_RECENT_MESSAGES_NOTE}"))
        .map_or(summary, |(value, _)| value);
    let summary = summary
        .split_once(&format!("\n{COMPACT_DIRECT_RESUME_INSTRUCTION}"))
        .map_or(summary, |(value, _)| value);
    Some(summary.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        collect_key_files, compact_session, compact_session_to_target, estimate_session_tokens,
        format_compact_summary, get_compact_continuation_message, infer_pending_work,
        should_compact, CompactionConfig,
    };
    use crate::session::{ContentBlock, ConversationMessage, MessageRole, Session};

    #[test]
    fn formats_compact_summary_like_upstream() {
        let summary = "<analysis>scratch</analysis>\n<summary>Kept work</summary>";
        assert_eq!(format_compact_summary(summary), "Summary:\nKept work");
    }

    #[test]
    fn leaves_small_sessions_unchanged() {
        let mut session = Session::new();
        session.messages = vec![ConversationMessage::user_text("hello")];

        let result = compact_session(&session, CompactionConfig::default());
        assert_eq!(result.removed_message_count, 0);
        assert_eq!(result.compacted_session, session);
        assert!(result.summary.is_empty());
        assert!(result.formatted_summary.is_empty());
    }

    #[test]
    fn compacts_older_messages_into_a_system_summary() {
        let mut session = Session::new();
        session.messages = vec![
            ConversationMessage::user_text("one ".repeat(200)),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "two ".repeat(200),
            }]),
            ConversationMessage::tool_result("1", "bash", "ok ".repeat(200), false),
            ConversationMessage {
                role: MessageRole::Assistant,
                blocks: vec![ContentBlock::Text {
                    text: "recent".to_string(),
                }],
                usage: None,
            },
        ];

        let result = compact_session(
            &session,
            CompactionConfig {
                preserve_recent_messages: 2,
                max_estimated_tokens: 1,
            },
        );

        // With the tool-use/tool-result boundary fix, the compaction preserves
        // one extra message to avoid an orphaned tool result at the boundary.
        // messages[1] (assistant) must be kept along with messages[2] (tool result).
        assert!(
            result.removed_message_count <= 2,
            "expected at most 2 removed, got {}",
            result.removed_message_count
        );
        assert_eq!(
            result.compacted_session.messages[0].role,
            MessageRole::System
        );
        assert!(matches!(
            &result.compacted_session.messages[0].blocks[0],
            ContentBlock::Text { text } if text.contains("Summary:")
        ));
        assert!(result.formatted_summary.contains("Scope:"));
        assert!(result.formatted_summary.contains("Key timeline:"));
        assert!(should_compact(
            &session,
            CompactionConfig {
                preserve_recent_messages: 2,
                max_estimated_tokens: 1,
            }
        ));
        // Note: with the tool-use/tool-result boundary guard the compacted session
        // may preserve one extra message at the boundary, so token reduction is
        // not guaranteed for small sessions. The invariant that matters is that
        // the removed_message_count is non-zero (something was compacted).
        assert!(
            result.removed_message_count > 0,
            "compaction must remove at least one message"
        );
    }

    #[test]
    fn keeps_previous_compacted_context_when_compacting_again() {
        let mut initial_session = Session::new();
        initial_session.messages = vec![
            ConversationMessage::user_text("Investigate rust/crates/runtime/src/compact.rs"),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "I will inspect the compact flow.".to_string(),
            }]),
            ConversationMessage::user_text("Also update rust/crates/runtime/src/conversation.rs"),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "Next: preserve prior summary context during auto compact.".to_string(),
            }]),
        ];
        let config = CompactionConfig {
            preserve_recent_messages: 2,
            max_estimated_tokens: 1,
        };

        let first = compact_session(&initial_session, config);
        let mut follow_up_messages = first.compacted_session.messages.clone();
        follow_up_messages.extend([
            ConversationMessage::user_text("Please add regression tests for compaction."),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "Working on regression coverage now.".to_string(),
            }]),
        ]);

        let mut second_session = Session::new();
        second_session.messages = follow_up_messages;
        let second = compact_session(&second_session, config);

        // "Previously compacted context:" header is intentionally flattened
        // (no re-nesting) to avoid summary inflation on repeated compaction.
        assert!(!second
            .formatted_summary
            .contains("Previously compacted context:"));
        assert!(second
            .formatted_summary
            .contains("Scope: 2 earlier messages compacted"));
        assert!(second
            .formatted_summary
            .contains("Newly compacted context:"));
        assert!(second
            .formatted_summary
            .contains("Also update rust/crates/runtime/src/conversation.rs"));
        assert!(matches!(
            &second.compacted_session.messages[0].blocks[0],
            ContentBlock::Text { text }
                if !text.contains("Previously compacted context:")
                    && text.contains("Newly compacted context:")
        ));
        assert!(matches!(
            &second.compacted_session.messages[1].blocks[0],
            ContentBlock::Text { text } if text.contains("Please add regression tests for compaction.")
        ));
    }

    #[test]
    fn ignores_existing_compacted_summary_when_deciding_to_recompact() {
        let summary = "<summary>Conversation summary:\n- Scope: earlier work preserved.\n- Key timeline:\n  - user: large preserved context\n</summary>";
        let mut session = Session::new();
        session.messages = vec![
            ConversationMessage {
                role: MessageRole::System,
                blocks: vec![ContentBlock::Text {
                    text: get_compact_continuation_message(summary, true, true),
                }],
                usage: None,
            },
            ConversationMessage::user_text("tiny"),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "recent".to_string(),
            }]),
        ];

        assert!(!should_compact(
            &session,
            CompactionConfig {
                preserve_recent_messages: 2,
                max_estimated_tokens: 1,
            }
        ));
    }

    #[test]
    fn truncates_long_blocks_in_summary() {
        let summary = super::summarize_block(
            &ContentBlock::Text {
                text: "x".repeat(400),
            },
            160,
        );
        assert!(summary.ends_with('…'));
        assert!(summary.chars().count() <= 161);
    }

    #[test]
    fn emits_structured_summary_sections_and_limits_tool_history() {
        let mut session = Session::new();
        session.messages = vec![
            ConversationMessage::user_text("Goal: update rust/crates/runtime/src/compact.rs"),
            ConversationMessage::assistant(vec![ContentBlock::ToolUse {
                id: "read-1".to_string(),
                name: "read_file".to_string(),
                input: r#"{"path":"rust/crates/runtime/src/compact.rs"}"#.to_string(),
            }]),
            ConversationMessage::tool_result(
                "read-1",
                "read_file",
                "large output ".repeat(10_000),
                false,
            ),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "Next: implement the compaction tests.".to_string(),
            }]),
        ];

        let result = compact_session(
            &session,
            CompactionConfig {
                preserve_recent_messages: 1,
                max_estimated_tokens: 1,
            },
        );

        assert!(result.summary.contains("## Goal"));
        assert!(result.summary.contains("## Constraints & Preferences"));
        assert!(result.summary.contains("## Progress"));
        assert!(result.summary.contains("## Key Decisions"));
        assert!(result.summary.contains("## Next Steps"));
        assert!(result.summary.contains("## Critical Context"));
        assert!(result.summary.len() < 40_000);
        let details = result
            .compacted_session
            .compaction
            .expect("compaction details should be recorded");
        assert!(details
            .details
            .read_files
            .contains(&"rust/crates/runtime/src/compact.rs".to_string()));
    }

    #[test]
    fn truncates_a_retained_tool_result_when_target_requires_it() {
        let mut session = Session::new();
        session.messages = vec![
            ConversationMessage::user_text("Inspect the repository"),
            ConversationMessage::assistant(vec![ContentBlock::ToolUse {
                id: "read-1".to_string(),
                name: "read_file".to_string(),
                input: r#"{"path":"src/main.rs"}"#.to_string(),
            }]),
            ConversationMessage::tool_result(
                "read-1",
                "read_file",
                "result ".repeat(20_000),
                false,
            ),
        ];

        let result = compact_session_to_target(&session, 2, 5_000);
        let output = result
            .compacted_session
            .messages
            .iter()
            .flat_map(|message| message.blocks.iter())
            .find_map(|block| match block {
                ContentBlock::ToolResult { output, .. } => Some(output),
                _ => None,
            })
            .expect("tool result should remain in the preferred recent tail");
        assert!(output.contains("truncated for context"));
        assert!(output.chars().count() <= 12_000);
    }

    #[test]
    fn extracts_key_files_from_message_content() {
        let files = collect_key_files(&[ConversationMessage::user_text(
            "Update rust/crates/runtime/src/compact.rs and rust/crates/rusty-claude-cli/src/main.rs next.",
        )]);
        assert!(files.contains(&"rust/crates/runtime/src/compact.rs".to_string()));
        assert!(files.contains(&"rust/crates/rusty-claude-cli/src/main.rs".to_string()));
    }

    #[test]
    fn compacts_to_requested_token_target_when_possible() {
        let mut session = Session::new();
        session.messages = vec![
            ConversationMessage::user_text("alpha ".repeat(140)),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "beta ".repeat(140),
            }]),
            ConversationMessage::user_text("gamma ".repeat(140)),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "delta ".repeat(140),
            }]),
            ConversationMessage::user_text("epsilon ".repeat(140)),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "zeta ".repeat(140),
            }]),
        ];

        let target = estimate_session_tokens(&session) / 2;
        let result = compact_session_to_target(&session, 4, target);

        assert!(result.removed_message_count > 0);
        assert!(estimate_session_tokens(&result.compacted_session) <= target);
    }

    #[test]
    fn keeps_best_effort_result_when_target_is_too_small() {
        let mut session = Session::new();
        session.messages = vec![
            ConversationMessage::user_text("alpha ".repeat(120)),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "beta ".repeat(120),
            }]),
            ConversationMessage::user_text("gamma ".repeat(120)),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "delta ".repeat(120),
            }]),
        ];

        let result = compact_session_to_target(&session, 3, 1);

        assert!(result.removed_message_count > 0);
        assert_eq!(
            result.compacted_session.messages[0].role,
            MessageRole::System
        );
    }

    /// Regression: compaction must not split an assistant(ToolUse) /
    /// user(ToolResult) pair at the boundary. An orphaned tool-result message
    /// without the preceding assistant `tool_calls` causes a 400 on the
    /// OpenAI-compat path (gaebal-gajae repro 2026-04-09).
    #[test]
    fn compaction_does_not_split_tool_use_tool_result_pair() {
        use crate::session::{ContentBlock, Session};

        let tool_id = "call_abc";
        let mut session = Session::default();
        // Turn 1: user prompt
        session
            .push_message(ConversationMessage::user_text("Search for files"))
            .unwrap();
        // Turn 2: assistant calls a tool
        session
            .push_message(ConversationMessage::assistant(vec![
                ContentBlock::ToolUse {
                    id: tool_id.to_string(),
                    name: "search".to_string(),
                    input: "{\"q\":\"*.rs\"}".to_string(),
                },
            ]))
            .unwrap();
        // Turn 3: tool result
        session
            .push_message(ConversationMessage::tool_result(
                tool_id,
                "search",
                "found 5 files",
                false,
            ))
            .unwrap();
        // Turn 4: assistant final response
        session
            .push_message(ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "Done.".to_string(),
            }]))
            .unwrap();

        // Compact preserving only 1 recent message — without the fix this
        // would cut the boundary so that the tool result (turn 3) is first,
        // without its preceding assistant tool_calls (turn 2).
        let config = CompactionConfig {
            preserve_recent_messages: 1,
            ..CompactionConfig::default()
        };
        let result = compact_session(&session, config);
        // After compaction, no two consecutive messages should have the pattern
        // tool_result immediately following a non-assistant message (i.e. an
        // orphaned tool result without a preceding assistant ToolUse).
        let messages = &result.compacted_session.messages;
        for i in 1..messages.len() {
            let curr_is_tool_result = messages[i]
                .blocks
                .first()
                .is_some_and(|b| matches!(b, ContentBlock::ToolResult { .. }));
            if curr_is_tool_result {
                let prev_has_tool_use = messages[i - 1]
                    .blocks
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
                assert!(
                    prev_has_tool_use,
                    "message[{}] is a ToolResult but message[{}] has no ToolUse: {:?}",
                    i,
                    i - 1,
                    &messages[i - 1].blocks
                );
            }
        }
    }

    #[test]
    fn infers_pending_work_from_recent_messages() {
        let pending = infer_pending_work(&[
            ConversationMessage::user_text("done"),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "Next: update tests and follow up on remaining CLI polish.".to_string(),
            }]),
        ]);
        assert_eq!(pending.len(), 1);
        assert!(pending[0].contains("Next: update tests"));
    }
}
