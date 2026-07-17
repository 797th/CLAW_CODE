//! Turning provider errors into text a user can act on.
//!
//! Context-window failures get a structured report with recovery steps;
//! provider "something went wrong" wrappers get qualified with the session and
//! trace id so they can be correlated. Everything else passes through.

/// Format a provider error for display, adding recovery guidance where the
/// failure has a known remedy.
#[must_use]
pub fn format_user_visible_api_error(session_id: &str, error: &api::ApiError) -> String {
    if error.is_context_window_failure() {
        format_context_window_blocked_error(session_id, error)
    } else if error.is_generic_fatal_wrapper() {
        let mut qualifiers = vec![format!("session {session_id}")];
        if let Some(request_id) = error.request_id() {
            qualifiers.push(format!("trace {request_id}"));
        }
        format!(
            "{} ({}): {}",
            error.safe_failure_class(),
            qualifiers.join(", "),
            error
        )
    } else {
        error.to_string()
    }
}

fn format_context_window_blocked_error(session_id: &str, error: &api::ApiError) -> String {
    let mut lines = vec![
        "Context window blocked".to_string(),
        "  Failure class    context_window_blocked".to_string(),
        format!("  Session          {session_id}"),
    ];

    if let Some(request_id) = error.request_id() {
        lines.push(format!("  Trace            {request_id}"));
    }

    match error {
        api::ApiError::ContextWindowExceeded {
            model,
            estimated_input_tokens,
            requested_output_tokens,
            estimated_total_tokens,
            context_window_tokens,
        } => {
            lines.push(format!("  Model            {model}"));
            lines.push(format!(
                "  Input estimate   ~{estimated_input_tokens} tokens (heuristic)"
            ));
            lines.push(format!(
                "  Requested output {requested_output_tokens} tokens"
            ));
            lines.push(format!(
                "  Total estimate   ~{estimated_total_tokens} tokens (heuristic)"
            ));
            lines.push(format!("  Context window   {context_window_tokens} tokens"));
        }
        api::ApiError::Api { message, body, .. } => {
            let detail = message.as_deref().unwrap_or(body).trim();
            if !detail.is_empty() {
                lines.push(format!(
                    "  Detail           {}",
                    truncate_for_summary(detail, 120)
                ));
            }
        }
        api::ApiError::RetriesExhausted { last_error, .. } => {
            let detail = match last_error.as_ref() {
                api::ApiError::Api { message, body, .. } => message.as_deref().unwrap_or(body),
                other => return format_context_window_blocked_error(session_id, other),
            }
            .trim();
            if !detail.is_empty() {
                lines.push(format!(
                    "  Detail           {}",
                    truncate_for_summary(detail, 120)
                ));
            }
        }
        _ => {}
    }

    lines.push(String::new());
    lines.push("Recovery".to_string());
    lines.push("  Compact          /compact".to_string());
    lines.push(format!(
        "  Resume compact   clawcli --resume {session_id} /compact"
    ));
    lines.push("  Fresh session    /clear --confirm".to_string());
    lines.push(
        "  Reduce scope     remove large pasted context/files or ask for a smaller slice"
            .to_string(),
    );
    lines.push("  Retry            rerun after compacting or reducing the request".to_string());

    lines.join("\n")
}

fn truncate_for_summary(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::{format_user_visible_api_error, truncate_for_summary};

    #[test]
    fn context_window_failures_report_the_budget_and_recovery_steps() {
        let error = api::ApiError::ContextWindowExceeded {
            model: "gpt-4.1".to_string(),
            estimated_input_tokens: 90_000,
            requested_output_tokens: 4_096,
            estimated_total_tokens: 94_096,
            context_window_tokens: 81_920,
        };

        let rendered = format_user_visible_api_error("sess-1", &error);

        assert!(rendered.starts_with("Context window blocked"));
        assert!(rendered.contains("  Session          sess-1"));
        assert!(rendered.contains("Context window   81920 tokens"));
        assert!(
            rendered.contains("Resume compact   clawcli --resume sess-1 /compact"),
            "recovery must name the session so it can be copy-pasted: {rendered}"
        );
    }

    #[test]
    fn ordinary_errors_pass_through_without_decoration() {
        let error = api::ApiError::Auth("bad key".to_string());

        let rendered = format_user_visible_api_error("sess-1", &error);

        assert_eq!(rendered, error.to_string());
    }

    #[test]
    fn summary_truncation_marks_elision_and_counts_chars_not_bytes() {
        assert_eq!(truncate_for_summary("abc", 5), "abc");
        assert_eq!(truncate_for_summary("abcdef", 3), "abc…");
        // Multi-byte input must not panic or split a char.
        assert_eq!(truncate_for_summary("héllo wörld", 4), "héll…");
    }
}
