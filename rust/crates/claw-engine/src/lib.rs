//! Shared agent engine for every Claw frontend.
//!
//! The line REPL and the full-screen frontend must run the *same* turn: the
//! same provider streaming, the same tool dispatch, the same permission gates.
//! Those pieces used to live inside the CLI binary, which made them
//! unreachable from the frontend crate and left it running a mock. They live
//! here instead, with rendering pushed behind [`sink::TurnSink`] so neither
//! frontend is privileged.

pub mod client;
pub mod error;
pub mod mcp;
pub mod setup;
pub mod sink;
pub mod tool_executor;

pub use client::{
    convert_messages, convert_messages_with_mode, filter_tool_specs, AllowedToolSet, EngineClient,
    NEEDS_LOGIN_HINT,
};
pub use error::format_user_visible_api_error;
pub use mcp::{
    ListMcpResourcesRequest, McpToolRequest, ReadMcpResourceRequest, RuntimeMcpState,
    ToolSearchRequest,
};
pub use setup::{
    build_runtime_mcp_state, build_runtime_plugin_state, permission_policy, RuntimePluginState,
};
pub use sink::{NullSink, SinkResult, TurnSink};
pub use tool_executor::EngineToolExecutor;
