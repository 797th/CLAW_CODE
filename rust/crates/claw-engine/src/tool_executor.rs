//! Dispatch for model-requested tools.
//!
//! Thin by design: the tools themselves live in the `tools` crate's
//! [`GlobalToolRegistry`], and permission enforcement is the registry's
//! enforcer. This routes a call to the builtin registry, the MCP bridge, or the
//! tool-search builtin, then reports the result to the sink.

use std::sync::{Arc, Mutex, PoisonError};

use runtime::{ToolError, ToolExecutor};
use tools::{canonical_allowed_tool_name, GlobalToolRegistry};

use crate::client::AllowedToolSet;
use crate::mcp::{
    ListMcpResourcesRequest, McpToolRequest, ReadMcpResourceRequest, RuntimeMcpState,
    ToolSearchRequest,
};
use crate::sink::TurnSink;

/// Executes the tools a model asks for, reporting each result to a sink.
pub struct EngineToolExecutor {
    allowed_tools: Option<AllowedToolSet>,
    tool_registry: GlobalToolRegistry,
    mcp_state: Option<Arc<Mutex<RuntimeMcpState>>>,
    sink: Box<dyn TurnSink + Send>,
}

impl EngineToolExecutor {
    pub fn new(
        allowed_tools: Option<AllowedToolSet>,
        tool_registry: GlobalToolRegistry,
        mcp_state: Option<Arc<Mutex<RuntimeMcpState>>>,
        sink: Box<dyn TurnSink + Send>,
    ) -> Self {
        Self {
            allowed_tools,
            tool_registry,
            mcp_state,
            sink,
        }
    }

    fn execute_search_tool(&self, value: serde_json::Value) -> Result<String, ToolError> {
        let input: ToolSearchRequest = serde_json::from_value(value)
            .map_err(|error| ToolError::new(format!("invalid tool input JSON: {error}")))?;
        // Surface pending/degraded MCP servers in search results so the model
        // learns why a tool it expects is missing.
        let (pending_mcp_servers, mcp_degraded) =
            self.mcp_state.as_ref().map_or((None, None), |state| {
                let state = state.lock().unwrap_or_else(PoisonError::into_inner);
                (state.pending_servers(), state.degraded_report())
            });
        serde_json::to_string_pretty(&self.tool_registry.search(
            &input.query,
            input.max_results.unwrap_or(5),
            pending_mcp_servers,
            mcp_degraded,
        ))
        .map_err(|error| ToolError::new(error.to_string()))
    }

    fn execute_runtime_tool(
        &self,
        tool_name: &str,
        value: serde_json::Value,
    ) -> Result<String, ToolError> {
        let Some(mcp_state) = &self.mcp_state else {
            return Err(ToolError::new(format!(
                "runtime tool `{tool_name}` is unavailable without configured MCP servers"
            )));
        };
        let mut mcp_state = mcp_state.lock().unwrap_or_else(PoisonError::into_inner);

        match tool_name {
            "MCPTool" => {
                let input: McpToolRequest = serde_json::from_value(value)
                    .map_err(|error| ToolError::new(format!("invalid tool input JSON: {error}")))?;
                let qualified_name = input
                    .qualified_name
                    .or(input.tool)
                    .ok_or_else(|| ToolError::new("missing required field `qualifiedName`"))?;
                mcp_state.call_tool(&qualified_name, input.arguments)
            }
            "ListMcpResourcesTool" => {
                let input: ListMcpResourcesRequest = serde_json::from_value(value)
                    .map_err(|error| ToolError::new(format!("invalid tool input JSON: {error}")))?;
                match input.server {
                    Some(server_name) => mcp_state.list_resources_for_server(&server_name),
                    None => mcp_state.list_resources_for_all_servers(),
                }
            }
            "ReadMcpResourceTool" => {
                let input: ReadMcpResourceRequest = serde_json::from_value(value)
                    .map_err(|error| ToolError::new(format!("invalid tool input JSON: {error}")))?;
                mcp_state.read_resource(&input.server, &input.uri)
            }
            _ => mcp_state.call_tool(tool_name, Some(value)),
        }
    }
}

impl ToolExecutor for EngineToolExecutor {
    fn execute(&mut self, tool_name: &str, input: &str) -> Result<String, ToolError> {
        if self
            .allowed_tools
            .as_ref()
            .is_some_and(|allowed| !allowed.contains(&canonical_allowed_tool_name(tool_name)))
        {
            return Err(ToolError::new(format!(
                "tool `{tool_name}` is not enabled by the current --allowedTools setting"
            )));
        }
        let value = serde_json::from_str(input)
            .map_err(|error| ToolError::new(format!("invalid tool input JSON: {error}")))?;
        let result = if tool_name == "ToolSearch" {
            self.execute_search_tool(value)
        } else if self.tool_registry.has_runtime_tool(tool_name) {
            self.execute_runtime_tool(tool_name, value)
        } else {
            self.tool_registry
                .execute(tool_name, &value)
                .map_err(ToolError::new)
        };
        match result {
            Ok(output) => {
                self.sink
                    .tool_result(tool_name, &output, false)
                    .map_err(ToolError::new)?;
                Ok(output)
            }
            Err(error) => {
                self.sink
                    .tool_result(tool_name, &error.to_string(), true)
                    .map_err(ToolError::new)?;
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::EngineToolExecutor;
    use crate::sink::{SinkResult, TurnSink};
    use runtime::ToolExecutor;
    use std::collections::BTreeSet;
    use std::sync::{Arc, Mutex};
    use tools::GlobalToolRegistry;

    #[derive(Default)]
    struct CapturedResults {
        results: Vec<(String, bool)>,
    }

    struct CapturingSink(Arc<Mutex<CapturedResults>>);

    impl TurnSink for CapturingSink {
        fn tool_result(&mut self, name: &str, _output: &str, is_error: bool) -> SinkResult {
            self.0
                .lock()
                .unwrap()
                .results
                .push((name.to_string(), is_error));
            Ok(())
        }
    }

    fn executor(
        allowed: Option<BTreeSet<String>>,
    ) -> (EngineToolExecutor, Arc<Mutex<CapturedResults>>) {
        let captured = Arc::new(Mutex::new(CapturedResults::default()));
        let executor = EngineToolExecutor::new(
            allowed,
            GlobalToolRegistry::builtin(),
            None,
            Box::new(CapturingSink(Arc::clone(&captured))),
        );
        (executor, captured)
    }

    #[test]
    fn tools_outside_the_allowed_set_are_refused_before_execution() {
        let allowed = BTreeSet::from(["read_file".to_string()]);
        let (mut executor, captured) = executor(Some(allowed));

        let error = executor
            .execute("Bash", r#"{"command":"echo hi"}"#)
            .expect_err("Bash is not in the allowed set");

        assert!(
            error.to_string().contains("not enabled by the current"),
            "error should name the allowedTools gate: {error}"
        );
        assert!(
            captured.lock().unwrap().results.is_empty(),
            "a refused tool must not report a result to the sink"
        );
    }

    #[test]
    fn malformed_input_json_is_rejected_without_reaching_the_registry() {
        let (mut executor, captured) = executor(None);

        let error = executor
            .execute("read_file", "{not json")
            .expect_err("malformed JSON should not execute");

        assert!(
            error.to_string().contains("invalid tool input JSON"),
            "error should identify the parse failure: {error}"
        );
        assert!(captured.lock().unwrap().results.is_empty());
    }

    #[test]
    fn runtime_tools_without_mcp_configured_explain_the_missing_servers() {
        let (mut executor, _captured) = executor(None);

        // MCPTool only exists once servers are configured; without them the
        // model should get a reason rather than "unknown tool".
        let error = executor
            .execute("MCPTool", r#"{"qualifiedName":"srv__tool"}"#)
            .expect_err("no MCP servers are configured");

        assert!(!error.to_string().is_empty());
    }

    #[test]
    fn failed_tool_results_are_reported_to_the_sink_as_errors() {
        let (mut executor, captured) = executor(None);

        let _ = executor.execute("read_file", r#"{"path":"/nonexistent/does-not-exist"}"#);

        let results = captured.lock().unwrap();
        assert_eq!(
            results.results.len(),
            1,
            "the failure must still reach the sink so frontends can draw it"
        );
        assert!(results.results[0].1, "it should be flagged as an error");
    }
}
