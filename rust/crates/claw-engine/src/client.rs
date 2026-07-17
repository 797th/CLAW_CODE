//! Provider streaming for a turn, rendered through a [`TurnSink`].
//!
//! This is the `runtime::ApiClient` every frontend runs. It owns provider
//! dispatch, the streaming loop, the post-tool stall retry, and the
//! non-streaming fallback. It draws nothing itself: each semantic event goes to
//! the sink, so the same loop serves the line REPL and the full-screen
//! frontend.

use std::collections::BTreeSet;
use std::time::Duration;

use api::{
    detect_provider_kind, resolve_startup_auth_source, AnthropicClient, AuthSource,
    ContentBlockDelta, InputContentBlock, InputMessage, MessageRequest, MessageResponse,
    OpenAiCompatClient, OpenAiCompatConfig, OutputContentBlock, PromptCache, ProviderClient,
    ProviderKind, StreamEvent as ApiStreamEvent, ToolChoice, ToolDefinition,
    ToolResultContentBlock,
};
use runtime::{
    ApiClient, ApiRequest, AssistantEvent, ContentBlock, ConversationMessage, MessageRole,
    PromptCacheEvent, RuntimeError,
};
use tools::GlobalToolRegistry;

use crate::error::format_user_visible_api_error;
use crate::sink::TurnSink;

/// Tool names the caller restricted this session to, if any.
pub type AllowedToolSet = BTreeSet<String>;

/// Frontends boot credential-free; this is the single source of truth for the
/// "not connected" guidance so the banner, the message path, and `/status`
/// stay in sync.
pub const NEEDS_LOGIN_HINT: &str =
    "No API credentials configured. Run /login to connect a provider.";

/// After tool execution some providers accept the continuation request but
/// never send a first event. Drop the stalled connection and re-send once.
const POST_TOOL_STALL_TIMEOUT: Duration = Duration::from_secs(60);

/// Streams provider responses for one session.
pub struct EngineClient {
    runtime: tokio::runtime::Runtime,
    client: ProviderClient,
    /// Built without real credentials: fail fast with a `/login` hint instead
    /// of sending a request with empty auth.
    needs_credentials: bool,
    session_id: String,
    model: String,
    enable_tools: bool,
    allowed_tools: Option<AllowedToolSet>,
    tool_registry: GlobalToolRegistry,
    reasoning_effort: Option<String>,
    sink: Box<dyn TurnSink + Send>,
}

impl EngineClient {
    /// Dispatch to the right provider for `model` and build a client for it.
    ///
    /// Model-name routing (`openai/`, `gpt-`, `grok`, `qwen/`) wins over
    /// env-var presence, so `detect_provider_kind` decides. Anthropic is built
    /// directly rather than via `ProviderClient::from_model` so `read_base_url`
    /// applies — the mock-server test harness depends on `ANTHROPIC_BASE_URL`
    /// pointing at its fake endpoint — and so a session-scoped prompt cache can
    /// be attached. The prompt cache is Anthropic-only.
    pub fn new(
        session_id: &str,
        model: String,
        enable_tools: bool,
        allowed_tools: Option<AllowedToolSet>,
        tool_registry: GlobalToolRegistry,
        sink: Box<dyn TurnSink + Send>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let resolved_model = api::resolve_model_alias(&model);
        let provider_kind = detect_provider_kind(&resolved_model);
        let has_credentials = credentials_present_for_kind(provider_kind);
        let client = match provider_kind {
            ProviderKind::Anthropic => {
                let inner =
                    AnthropicClient::from_auth(resolve_auth_source().unwrap_or(AuthSource::None))
                        .with_base_url(api::read_base_url())
                        .with_prompt_cache(PromptCache::new(session_id));
                ProviderClient::Anthropic(inner)
            }
            ProviderKind::Xai | ProviderKind::OpenAi | ProviderKind::NvidiaNim => {
                // `from_model_with_anthropic_auth` reads the matching API-key
                // and base-URL env vars internally, covering OpenAI,
                // OpenRouter, xAI, DashScope, NVIDIA NIM, Ollama, and any other
                // OpenAI-compat endpoint. It errors when credentials are
                // missing, so the credential-free boot path uses a placeholder
                // that fails at request time with the `/login` hint instead.
                if has_credentials {
                    ProviderClient::from_model_with_anthropic_auth(&resolved_model, None)?
                } else {
                    ProviderClient::OpenAi(OpenAiCompatClient::new(
                        String::new(),
                        OpenAiCompatConfig::openai(),
                    ))
                }
            }
        };
        Ok(Self {
            runtime: tokio::runtime::Runtime::new()?,
            client,
            needs_credentials: !has_credentials,
            session_id: session_id.to_string(),
            model,
            enable_tools,
            allowed_tools,
            tool_registry,
            reasoning_effort: None,
            sink,
        })
    }

    pub fn set_reasoning_effort(&mut self, effort: Option<String>) {
        self.reasoning_effort = effort;
    }

    /// True when this client was built without real credentials. Frontends
    /// surface a "run /login" hint instead of a ready state while this is set;
    /// `/login` rebuilds the runtime and clears it.
    #[must_use]
    pub const fn needs_credentials(&self) -> bool {
        self.needs_credentials
    }

    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }
}

/// Whether real credentials exist for a provider *kind*, independent of any
/// model name. Frontends boot credential-free and prompt for `/login`, so this
/// decides between a real client and a placeholder.
fn credentials_present_for_kind(kind: ProviderKind) -> bool {
    match kind {
        ProviderKind::Anthropic => resolve_startup_auth_source(|| Ok(None)).is_ok(),
        ProviderKind::Xai => api::has_api_key("XAI_API_KEY"),
        ProviderKind::NvidiaNim => api::has_api_key("NVIDIA_API_KEY"),
        ProviderKind::OpenAi => api::has_api_key("OPENAI_API_KEY"),
    }
}

#[allow(clippy::result_large_err)]
fn resolve_auth_source() -> Result<AuthSource, api::ApiError> {
    resolve_startup_auth_source(|| Ok(None))
}

impl ApiClient for EngineClient {
    fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        if self.needs_credentials {
            return Err(RuntimeError::new(NEEDS_LOGIN_HINT));
        }
        self.sink.request_start().map_err(RuntimeError::new)?;

        let is_post_tool = request_ends_with_tool_result(&request);
        let message_request = MessageRequest {
            model: self.model.clone(),
            max_tokens: api::max_tokens_for_model(&self.model),
            messages: convert_messages(&request.messages),
            system: (!request.system_prompt.is_empty()).then(|| request.system_prompt.join("\n\n")),
            tools: self
                .enable_tools
                .then(|| self.tool_registry.definitions(self.allowed_tools.as_ref())),
            tool_choice: self.enable_tools.then_some(ToolChoice::Auto),
            stream: true,
            reasoning_effort: self.reasoning_effort.clone(),
            ..Default::default()
        };

        // `block_on` needs `&mut` for the sink while `self.runtime` is borrowed,
        // so split the borrows explicitly.
        let Self {
            runtime,
            client,
            session_id,
            sink,
            ..
        } = self;

        runtime.block_on(async {
            let max_attempts: usize = if is_post_tool { 2 } else { 1 };
            for attempt in 1..=max_attempts {
                let result = consume_stream(
                    client,
                    session_id,
                    sink.as_mut(),
                    &message_request,
                    is_post_tool && attempt == 1,
                )
                .await;
                match result {
                    Ok(events) => return Ok(events),
                    // A stalled post-tool continuation is retried once; every
                    // other failure is final.
                    Err(error) if attempt < max_attempts && is_stall_error(&error) => continue,
                    Err(error) => return Err(error),
                }
            }
            unreachable!("loop returns on the final attempt")
        })
    }
}

fn is_stall_error(error: &RuntimeError) -> bool {
    error.to_string().contains("post-tool stall")
}

/// Consume one streaming response, emitting each event to `sink`.
#[allow(clippy::too_many_lines)]
async fn consume_stream(
    client: &ProviderClient,
    session_id: &str,
    sink: &mut (dyn TurnSink + Send),
    message_request: &MessageRequest,
    apply_stall_timeout: bool,
) -> Result<Vec<AssistantEvent>, RuntimeError> {
    let mut stream = client
        .stream_message(message_request)
        .await
        .map_err(|error| RuntimeError::new(format_user_visible_api_error(session_id, &error)))?;

    let mut events = Vec::new();
    let mut pending_tool: Option<(String, String, String)> = None;
    // Accumulated so the session can persist reasoning: providers deliver it
    // as deltas that must be rejoined into one Thinking block.
    let mut pending_thinking: Option<(String, Option<String>)> = None;
    let mut saw_stop = false;
    let mut received_any_event = false;

    loop {
        let next = if apply_stall_timeout && !received_any_event {
            match tokio::time::timeout(POST_TOOL_STALL_TIMEOUT, stream.next_event()).await {
                Ok(inner) => inner.map_err(|error| {
                    RuntimeError::new(format_user_visible_api_error(session_id, &error))
                })?,
                Err(_elapsed) => {
                    return Err(RuntimeError::new(
                        "post-tool stall: model did not respond within timeout",
                    ));
                }
            }
        } else {
            stream.next_event().await.map_err(|error| {
                RuntimeError::new(format_user_visible_api_error(session_id, &error))
            })?
        };

        let Some(event) = next else {
            break;
        };
        received_any_event = true;

        match event {
            ApiStreamEvent::MessageStart(start) => {
                for block in start.message.content {
                    push_output_block(block, sink, &mut events, &mut pending_tool, true)?;
                }
            }
            ApiStreamEvent::ContentBlockStart(start) => {
                if let OutputContentBlock::Thinking {
                    thinking,
                    signature,
                } = &start.content_block
                {
                    pending_thinking = Some((thinking.clone(), signature.clone()));
                }
                push_output_block(
                    start.content_block,
                    sink,
                    &mut events,
                    &mut pending_tool,
                    true,
                )?;
            }
            ApiStreamEvent::ContentBlockDelta(delta) => match delta.delta {
                ContentBlockDelta::TextDelta { text } => {
                    if !text.is_empty() {
                        sink.text_delta(&text).map_err(RuntimeError::new)?;
                        events.push(AssistantEvent::TextDelta(text));
                    }
                }
                ContentBlockDelta::InputJsonDelta { partial_json } => {
                    if let Some((_, _, input)) = &mut pending_tool {
                        input.push_str(&partial_json);
                    }
                }
                ContentBlockDelta::ThinkingDelta { thinking } => {
                    sink.thinking_delta(&thinking).map_err(RuntimeError::new)?;
                    if let Some((text, _)) = &mut pending_thinking {
                        text.push_str(&thinking);
                    }
                }
                ContentBlockDelta::SignatureDelta { signature } => {
                    if let Some((_, pending_signature)) = &mut pending_thinking {
                        pending_signature
                            .get_or_insert_with(String::new)
                            .push_str(&signature);
                    }
                }
            },
            ApiStreamEvent::ContentBlockStop(_) => {
                sink.block_stop().map_err(RuntimeError::new)?;
                if let Some((thinking, signature)) = pending_thinking.take() {
                    events.push(AssistantEvent::Thinking {
                        thinking,
                        signature,
                    });
                }
                // Emitted only now: the tool's input JSON arrives as deltas and
                // is not complete until the block closes.
                if let Some((id, name, input)) = pending_tool.take() {
                    sink.tool_call(&name, &input).map_err(RuntimeError::new)?;
                    events.push(AssistantEvent::ToolUse { id, name, input });
                }
            }
            ApiStreamEvent::MessageDelta(delta) => {
                let usage = delta.usage.token_usage();
                sink.usage(usage).map_err(RuntimeError::new)?;
                events.push(AssistantEvent::Usage(usage));
            }
            ApiStreamEvent::MessageStop(_) => {
                saw_stop = true;
                sink.message_stop().map_err(RuntimeError::new)?;
                events.push(AssistantEvent::MessageStop);
            }
        }
    }

    sink.turn_end().map_err(RuntimeError::new)?;
    push_prompt_cache_record(client, &mut events);

    // Some endpoints close the stream without an explicit stop event; treat
    // real output as an implicit stop rather than replaying the request.
    if !saw_stop
        && events.iter().any(|event| {
            matches!(event, AssistantEvent::TextDelta(text) if !text.is_empty())
                || matches!(event, AssistantEvent::ToolUse { .. })
        })
    {
        events.push(AssistantEvent::MessageStop);
    }

    if events
        .iter()
        .any(|event| matches!(event, AssistantEvent::MessageStop))
    {
        return Ok(events);
    }

    // The stream produced nothing usable: fall back to a non-streaming call.
    let response = client
        .send_message(&MessageRequest {
            stream: false,
            ..message_request.clone()
        })
        .await
        .map_err(|error| RuntimeError::new(format_user_visible_api_error(session_id, &error)))?;
    let mut events = response_to_events(response, sink)?;
    push_prompt_cache_record(client, &mut events);
    Ok(events)
}

/// Returns `true` when the conversation ends with a tool-result message,
/// meaning the model is expected to continue after tool execution.
fn request_ends_with_tool_result(request: &ApiRequest) -> bool {
    request
        .messages
        .last()
        .is_some_and(|message| message.role == MessageRole::Tool)
}

fn push_output_block(
    block: OutputContentBlock,
    sink: &mut (dyn TurnSink + Send),
    events: &mut Vec<AssistantEvent>,
    pending_tool: &mut Option<(String, String, String)>,
    streaming_tool_input: bool,
) -> Result<(), RuntimeError> {
    match block {
        OutputContentBlock::Text { text } => {
            if !text.is_empty() {
                sink.text_block(&text).map_err(RuntimeError::new)?;
                events.push(AssistantEvent::TextDelta(text));
            }
        }
        OutputContentBlock::ToolUse { id, name, input } => {
            // While streaming, `content_block_start` carries an empty input
            // (`{}`) and the real input arrives via `input_json_delta`. In
            // non-streaming responses an empty object is legitimate.
            let initial_input = if streaming_tool_input
                && input.is_object()
                && input.as_object().is_some_and(serde_json::Map::is_empty)
            {
                String::new()
            } else {
                input.to_string()
            };
            *pending_tool = Some((id, name, initial_input));
        }
        OutputContentBlock::Thinking {
            thinking,
            signature,
        } => {
            sink.thinking_block(&thinking, signature.as_deref())
                .map_err(RuntimeError::new)?;
            events.push(AssistantEvent::Thinking {
                thinking,
                signature,
            });
        }
        OutputContentBlock::RedactedThinking { .. } => {
            sink.redacted_thinking().map_err(RuntimeError::new)?;
        }
    }
    Ok(())
}

fn response_to_events(
    response: MessageResponse,
    sink: &mut (dyn TurnSink + Send),
) -> Result<Vec<AssistantEvent>, RuntimeError> {
    let mut events = Vec::new();
    let mut pending_tool = None;

    for block in response.content {
        push_output_block(block, sink, &mut events, &mut pending_tool, false)?;
        sink.block_stop().map_err(RuntimeError::new)?;
        if let Some((id, name, input)) = pending_tool.take() {
            sink.tool_call(&name, &input).map_err(RuntimeError::new)?;
            events.push(AssistantEvent::ToolUse { id, name, input });
        }
    }

    let usage = response.usage.token_usage();
    sink.usage(usage).map_err(RuntimeError::new)?;
    events.push(AssistantEvent::Usage(usage));
    events.push(AssistantEvent::MessageStop);
    sink.turn_end().map_err(RuntimeError::new)?;
    Ok(events)
}

fn push_prompt_cache_record(client: &ProviderClient, events: &mut Vec<AssistantEvent>) {
    // `take_last_prompt_cache_record` passes through to the Anthropic variant
    // and returns `None` for OpenAI-compat / xAI, which have no prompt cache.
    // So this stays a no-op on those providers without extra branching.
    if let Some(record) = client.take_last_prompt_cache_record() {
        if let Some(event) = prompt_cache_record_to_runtime_event(record) {
            events.push(AssistantEvent::PromptCache(event));
        }
    }
}

fn prompt_cache_record_to_runtime_event(
    record: api::PromptCacheRecord,
) -> Option<PromptCacheEvent> {
    let cache_break = record.cache_break?;
    Some(PromptCacheEvent {
        unexpected: cache_break.unexpected,
        reason: cache_break.reason,
        previous_cache_read_input_tokens: cache_break.previous_cache_read_input_tokens,
        current_cache_read_input_tokens: cache_break.current_cache_read_input_tokens,
        token_drop: cache_break.token_drop,
    })
}

#[must_use]
pub fn convert_messages(messages: &[ConversationMessage]) -> Vec<InputMessage> {
    convert_messages_with_mode(messages, runtime::caveman_enabled())
}

/// Convert session messages to provider input blocks, optionally applying
/// Caveman compression. Exposed so callers can test both modes explicitly.
#[must_use]
pub fn convert_messages_with_mode(
    messages: &[ConversationMessage],
    caveman: bool,
) -> Vec<InputMessage> {
    messages
        .iter()
        .filter_map(|message| {
            let role = match message.role {
                MessageRole::System | MessageRole::User | MessageRole::Tool => "user",
                MessageRole::Assistant => "assistant",
            };
            let content = message
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(InputContentBlock::Text {
                        text: if caveman {
                            runtime::compress_caveman(text)
                        } else {
                            text.clone()
                        },
                    }),
                    ContentBlock::Thinking {
                        thinking,
                        signature,
                    } => {
                        // Keep signed thinking exact: provider signatures can
                        // cover the thinking text. Compress unsigned reasoning
                        // before sending it back as reasoning_content.
                        Some(InputContentBlock::Thinking {
                            thinking: if caveman && signature.is_none() {
                                runtime::compress_caveman(thinking)
                            } else {
                                thinking.clone()
                            },
                            signature: signature.clone(),
                        })
                    }
                    ContentBlock::ToolUse { id, name, input } => Some(InputContentBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: serde_json::from_str(input)
                            .unwrap_or_else(|_| serde_json::json!({ "raw": input })),
                    }),
                    ContentBlock::ToolResult {
                        tool_use_id,
                        output,
                        is_error,
                        ..
                    } => Some(InputContentBlock::ToolResult {
                        tool_use_id: tool_use_id.clone(),
                        content: vec![ToolResultContentBlock::Text {
                            text: if caveman {
                                runtime::compress_caveman(output)
                            } else {
                                output.clone()
                            },
                        }],
                        is_error: *is_error,
                    }),
                })
                .collect::<Vec<_>>();
            (!content.is_empty()).then(|| InputMessage {
                role: role.to_string(),
                content,
            })
        })
        .collect()
}

/// Tool definitions offered to the model, honoring `--allowedTools`.
#[must_use]
pub fn filter_tool_specs(
    tool_registry: &GlobalToolRegistry,
    allowed_tools: Option<&AllowedToolSet>,
) -> Vec<ToolDefinition> {
    tool_registry.definitions(allowed_tools)
}

#[cfg(test)]
mod tests {
    use super::{convert_messages_with_mode, request_ends_with_tool_result};
    use runtime::{ApiRequest, ContentBlock, ConversationMessage, MessageRole};

    fn text_message(role: MessageRole, text: &str) -> ConversationMessage {
        ConversationMessage {
            role,
            blocks: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
            usage: None,
        }
    }

    #[test]
    fn post_tool_continuation_is_detected_from_the_last_message_role() {
        let request = ApiRequest {
            system_prompt: Vec::new(),
            messages: vec![
                text_message(MessageRole::User, "run it"),
                text_message(MessageRole::Tool, "tool output"),
            ],
        };
        assert!(request_ends_with_tool_result(&request));

        let request = ApiRequest {
            system_prompt: Vec::new(),
            messages: vec![text_message(MessageRole::User, "hi")],
        };
        assert!(!request_ends_with_tool_result(&request));
    }

    #[test]
    fn tool_and_system_messages_are_sent_as_user_turns() {
        let messages = vec![
            text_message(MessageRole::System, "sys"),
            text_message(MessageRole::Tool, "tool"),
            text_message(MessageRole::Assistant, "reply"),
        ];

        let converted = convert_messages_with_mode(&messages, false);

        let roles: Vec<&str> = converted.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, vec!["user", "user", "assistant"]);
    }

    #[test]
    fn empty_messages_are_dropped_rather_than_sent_as_blank_turns() {
        let messages = vec![ConversationMessage {
            role: MessageRole::User,
            blocks: Vec::new(),
            usage: None,
        }];

        assert!(convert_messages_with_mode(&messages, false).is_empty());
    }

    #[test]
    fn signed_thinking_survives_caveman_compression_unchanged() {
        // Provider signatures can cover the thinking text: compressing signed
        // reasoning would invalidate the signature.
        let signed = "I should carefully consider all of the available options here";
        let messages = vec![ConversationMessage {
            role: MessageRole::Assistant,
            blocks: vec![ContentBlock::Thinking {
                thinking: signed.to_string(),
                signature: Some("sig-abc".to_string()),
            }],
            usage: None,
        }];

        let converted = convert_messages_with_mode(&messages, true);

        match &converted[0].content[0] {
            api::InputContentBlock::Thinking {
                thinking,
                signature,
            } => {
                assert_eq!(thinking, signed, "signed thinking must not be compressed");
                assert_eq!(signature.as_deref(), Some("sig-abc"));
            }
            other => panic!("expected a thinking block, got {other:?}"),
        }
    }
}
