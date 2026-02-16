//! Model Context Protocol (MCP) server implementation.
//!
//! Implements JSON-RPC 2.0 over stdin/stdout for tool discovery and invocation
//! by external AI agents.

use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::io::{self, BufRead, Write};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<serde_json::Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerCapabilities {
    pub tools: Option<ToolsCapability>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsCapability {
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallResult {
    pub content: Vec<ToolContent>,
    #[serde(rename = "isError", default)]
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ToolContent {
    #[serde(rename = "text")]
    Text { text: String },
}

type ToolHandler = dyn Fn(&serde_json::Value) -> ToolCallResult;

pub struct McpServer {
    tools: Vec<ToolDefinition>,
    tool_handlers: HashMap<String, Box<ToolHandler>>,
    initialized: bool,
}

impl Default for McpServer {
    fn default() -> Self {
        Self::new()
    }
}

impl McpServer {
    pub fn new() -> Self {
        Self {
            tools: Vec::new(),
            tool_handlers: HashMap::new(),
            initialized: false,
        }
    }

    /// Register a tool with its handler
    pub fn register_tool(&mut self, def: ToolDefinition, handler: Box<ToolHandler>) {
        self.tools.retain(|existing| existing.name != def.name);
        self.tool_handlers.insert(def.name.clone(), handler);
        self.tools.push(def);
    }

    /// Register all built-in Othala tools
    pub fn register_builtin_tools(&mut self) {
        self.register_tool(
            ToolDefinition {
                name: "list_tasks".to_string(),
                description: "List all Othala tasks with their current state".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "state": {
                            "type": "string",
                            "description": "Filter by state (chatting, ready, submitting, etc.)"
                        },
                        "label": {
                            "type": "string",
                            "description": "Filter by label"
                        }
                    }
                }),
            },
            Box::new(|params| ToolCallResult {
                content: vec![ToolContent::Text {
                    text: format!("Tasks listed with filter: {params:?}"),
                }],
                is_error: false,
            }),
        );

        self.register_tool(
            ToolDefinition {
                name: "create_task".to_string(),
                description: "Create a new Othala task".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["repo", "title"],
                    "properties": {
                        "repo": { "type": "string", "description": "Repository ID" },
                        "title": { "type": "string", "description": "Task title" },
                        "model": { "type": "string", "description": "Preferred model" },
                        "priority": { "type": "string", "description": "Task priority" }
                    }
                }),
            },
            Box::new(|params| ToolCallResult {
                content: vec![ToolContent::Text {
                    text: format!("Task created with params: {params:?}"),
                }],
                is_error: false,
            }),
        );

        self.register_tool(
            ToolDefinition {
                name: "get_task".to_string(),
                description: "Get task details by task ID".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["task_id"],
                    "properties": {
                        "task_id": { "type": "string", "description": "Task ID" }
                    }
                }),
            },
            Box::new(|params| ToolCallResult {
                content: vec![ToolContent::Text {
                    text: format!("Task details requested: {params:?}"),
                }],
                is_error: false,
            }),
        );

        self.register_tool(
            ToolDefinition {
                name: "stop_task".to_string(),
                description: "Stop a running task".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["task_id"],
                    "properties": {
                        "task_id": { "type": "string", "description": "Task ID" }
                    }
                }),
            },
            Box::new(|params| ToolCallResult {
                content: vec![ToolContent::Text {
                    text: format!("Task stop requested: {params:?}"),
                }],
                is_error: false,
            }),
        );

        self.register_tool(
            ToolDefinition {
                name: "resume_task".to_string(),
                description: "Resume a stopped task".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["task_id"],
                    "properties": {
                        "task_id": { "type": "string", "description": "Task ID" }
                    }
                }),
            },
            Box::new(|params| ToolCallResult {
                content: vec![ToolContent::Text {
                    text: format!("Task resume requested: {params:?}"),
                }],
                is_error: false,
            }),
        );

        self.register_tool(
            ToolDefinition {
                name: "delete_task".to_string(),
                description: "Delete a task by ID".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["task_id"],
                    "properties": {
                        "task_id": { "type": "string", "description": "Task ID" }
                    }
                }),
            },
            Box::new(|params| ToolCallResult {
                content: vec![ToolContent::Text {
                    text: format!("Task deletion requested: {params:?}"),
                }],
                is_error: false,
            }),
        );

        self.register_tool(
            ToolDefinition {
                name: "list_events".to_string(),
                description: "List task or global events".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "task_id": { "type": "string", "description": "Optional task ID" },
                        "limit": { "type": "integer", "description": "Maximum number of events" }
                    }
                }),
            },
            Box::new(|params| ToolCallResult {
                content: vec![ToolContent::Text {
                    text: format!("Events listed with params: {params:?}"),
                }],
                is_error: false,
            }),
        );

        self.register_tool(
            ToolDefinition {
                name: "get_stats".to_string(),
                description: "Get aggregate task and event statistics".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "json": { "type": "boolean", "description": "Return stats in JSON format" }
                    }
                }),
            },
            Box::new(|params| ToolCallResult {
                content: vec![ToolContent::Text {
                    text: format!("Stats requested with params: {params:?}"),
                }],
                is_error: false,
            }),
        );

        self.register_tool(
            ToolDefinition {
                name: "list_sessions".to_string(),
                description: "List all known sessions".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "json": { "type": "boolean", "description": "Return sessions in JSON format" }
                    }
                }),
            },
            Box::new(|params| ToolCallResult {
                content: vec![ToolContent::Text {
                    text: format!("Sessions listed with params: {params:?}"),
                }],
                is_error: false,
            }),
        );

        self.register_tool(
            ToolDefinition {
                name: "list_skills".to_string(),
                description: "List available Othala skills".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "tag": { "type": "string", "description": "Optional tag filter" }
                    }
                }),
            },
            Box::new(|params| ToolCallResult {
                content: vec![ToolContent::Text {
                    text: format!("Skills listed with params: {params:?}"),
                }],
                is_error: false,
            }),
        );

        self.register_tool(
            ToolDefinition {
                name: "search_tasks".to_string(),
                description: "Search tasks by query string".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["query"],
                    "properties": {
                        "query": { "type": "string", "description": "Search query" },
                        "label": { "type": "string", "description": "Optional label filter" },
                        "state": { "type": "string", "description": "Optional state filter" }
                    }
                }),
            },
            Box::new(|params| ToolCallResult {
                content: vec![ToolContent::Text {
                    text: format!("Task search requested: {params:?}"),
                }],
                is_error: false,
            }),
        );

        self.register_tool(
            ToolDefinition {
                name: "get_health".to_string(),
                description: "Get Othala health status".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {}
                }),
            },
            Box::new(|_| ToolCallResult {
                content: vec![ToolContent::Text {
                    text: "Othala MCP server is healthy".to_string(),
                }],
                is_error: false,
            }),
        );
    }

    /// Handle a single JSON-RPC request and return response
    pub fn handle_request(&mut self, request: &JsonRpcRequest) -> JsonRpcResponse {
        if request.jsonrpc != "2.0" {
            return Self::error_response(
                request.id.clone(),
                INVALID_REQUEST,
                "Invalid JSON-RPC version",
                None,
            );
        }

        let params = request
            .params
            .clone()
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

        match request.method.as_str() {
            "initialize" => self.handle_initialize(request.id.clone(), &params),
            "initialized" => {
                self.initialized = true;
                Self::success_response(request.id.clone(), serde_json::Value::Null)
            }
            "tools/list" => {
                if !self.initialized {
                    return Self::error_response(
                        request.id.clone(),
                        INVALID_REQUEST,
                        "Server not initialized",
                        None,
                    );
                }
                self.handle_tools_list(request.id.clone())
            }
            "tools/call" => {
                if !self.initialized {
                    return Self::error_response(
                        request.id.clone(),
                        INVALID_REQUEST,
                        "Server not initialized",
                        None,
                    );
                }
                self.handle_tools_call(request.id.clone(), &params)
            }
            _ => Self::error_response(
                request.id.clone(),
                METHOD_NOT_FOUND,
                "Method not found",
                None,
            ),
        }
    }

    /// Handle `initialize` method
    fn handle_initialize(
        &mut self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> JsonRpcResponse {
        if !params.is_object() {
            return Self::error_response(
                id,
                INVALID_PARAMS,
                "initialize params must be an object",
                None,
            );
        }

        self.initialized = true;

        let server_info = ServerInfo {
            name: "othala".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        };
        let capabilities = ServerCapabilities {
            tools: Some(ToolsCapability { list_changed: false }),
        };

        Self::success_response(
            id,
            json!({
                "serverInfo": server_info,
                "capabilities": capabilities
            }),
        )
    }

    /// Handle `tools/list` method
    fn handle_tools_list(&self, id: Option<serde_json::Value>) -> JsonRpcResponse {
        Self::success_response(
            id,
            json!({
                "tools": self.tools
            }),
        )
    }

    /// Handle `tools/call` method
    fn handle_tools_call(
        &self,
        id: Option<serde_json::Value>,
        params: &serde_json::Value,
    ) -> JsonRpcResponse {
        let Some(params_obj) = params.as_object() else {
            return Self::error_response(id, INVALID_PARAMS, "tools/call params must be an object", None);
        };

        let Some(name) = params_obj.get("name").and_then(serde_json::Value::as_str) else {
            return Self::error_response(id, INVALID_PARAMS, "tools/call missing string field 'name'", None);
        };

        let arguments = params_obj
            .get("arguments")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        let Some(handler) = self.tool_handlers.get(name) else {
            return Self::error_response(
                id,
                METHOD_NOT_FOUND,
                "Tool not found",
                Some(json!({ "name": name })),
            );
        };

        let tool_result = handler(&arguments);
        match serde_json::to_value(tool_result) {
            Ok(result) => Self::success_response(id, result),
            Err(err) => Self::error_response(
                id,
                INTERNAL_ERROR,
                "Failed to serialize tool result",
                Some(json!({ "reason": err.to_string() })),
            ),
        }
    }

    /// Run the MCP server loop reading from stdin and writing to stdout
    pub fn run_stdio(&mut self) -> io::Result<()> {
        let stdin = io::stdin();
        let stdout = io::stdout();
        let mut out = stdout.lock();

        for line in stdin.lock().lines() {
            let line = line?;
            if let Some(response) = self.process_line(&line) {
                out.write_all(response.as_bytes())?;
                out.write_all(b"\n")?;
                out.flush()?;
            }
        }

        Ok(())
    }

    /// Process a single line of input and return the response string
    pub fn process_line(&mut self, line: &str) -> Option<String> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }

        let parsed_value = match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(value) => value,
            Err(err) => {
                let response = Self::error_response(
                    None,
                    PARSE_ERROR,
                    "Parse error",
                    Some(json!({ "reason": err.to_string() })),
                );
                return serde_json::to_string(&response).ok();
            }
        };

        let request = match serde_json::from_value::<JsonRpcRequest>(parsed_value) {
            Ok(request) => request,
            Err(err) => {
                let response = Self::error_response(
                    None,
                    INVALID_REQUEST,
                    "Invalid request",
                    Some(json!({ "reason": err.to_string() })),
                );
                return serde_json::to_string(&response).ok();
            }
        };

        let is_notification = request.id.is_none();
        let response = self.handle_request(&request);

        if is_notification {
            None
        } else {
            serde_json::to_string(&response).ok()
        }
    }

    fn success_response(id: Option<serde_json::Value>, result: serde_json::Value) -> JsonRpcResponse {
        JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error_response(
        id: Option<serde_json::Value>,
        code: i64,
        message: &str,
        data: Option<serde_json::Value>,
    ) -> JsonRpcResponse {
        JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.to_string(),
                data,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_response(raw: &str) -> JsonRpcResponse {
        serde_json::from_str(raw).expect("parse response")
    }

    fn init_server(server: &mut McpServer) {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: "initialize".to_string(),
            params: Some(json!({})),
        };
        let response = server.handle_request(&request);
        assert!(response.error.is_none());
    }

    #[test]
    fn parse_valid_json_rpc_request() {
        let request: JsonRpcRequest = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        )
        .expect("valid request parses");
        assert_eq!(request.method, "initialize");
        assert_eq!(request.jsonrpc, "2.0");
        assert_eq!(request.id, Some(json!(1)));
    }

    #[test]
    fn parse_invalid_json_returns_parse_error() {
        let mut server = McpServer::new();
        let response_raw = server
            .process_line("{not json")
            .expect("parse errors return response");
        let response = parse_response(&response_raw);
        let error = response.error.expect("has parse error");
        assert_eq!(error.code, PARSE_ERROR);
    }

    #[test]
    fn invalid_request_payload_returns_invalid_request_error() {
        let mut server = McpServer::new();
        let response_raw = server
            .process_line(r#"{"jsonrpc":"2.0","id":1}"#)
            .expect("invalid request returns response");
        let response = parse_response(&response_raw);
        let error = response.error.expect("has invalid request error");
        assert_eq!(error.code, INVALID_REQUEST);
    }

    #[test]
    fn initialize_returns_server_info_and_capabilities() {
        let mut server = McpServer::new();
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: "initialize".to_string(),
            params: Some(json!({})),
        };
        let response = server.handle_request(&request);
        assert!(response.error.is_none());
        let result = response.result.expect("initialize returns result");
        assert_eq!(result["serverInfo"]["name"], json!("othala"));
        assert_eq!(result["capabilities"]["tools"]["listChanged"], json!(false));
    }

    #[test]
    fn tools_list_returns_all_registered_tools() {
        let mut server = McpServer::new();
        server.register_builtin_tools();
        init_server(&mut server);

        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "tools/list".to_string(),
            params: None,
        };
        let response = server.handle_request(&request);
        assert!(response.error.is_none());
        let tools = response
            .result
            .expect("tools list result")["tools"]
            .as_array()
            .cloned()
            .expect("tools is array");
        assert_eq!(tools.len(), 12);
    }

    #[test]
    fn tools_call_with_valid_tool_name() {
        let mut server = McpServer::new();
        server.register_builtin_tools();
        init_server(&mut server);

        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(3)),
            method: "tools/call".to_string(),
            params: Some(json!({
                "name": "get_health",
                "arguments": {}
            })),
        };

        let response = server.handle_request(&request);
        assert!(response.error.is_none());
        let result = response.result.expect("tool call result");
        assert_eq!(result["isError"], json!(false));
        assert_eq!(result["content"][0]["type"], json!("text"));
    }

    #[test]
    fn tools_call_with_unknown_tool_returns_error() {
        let mut server = McpServer::new();
        server.register_builtin_tools();
        init_server(&mut server);

        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(4)),
            method: "tools/call".to_string(),
            params: Some(json!({
                "name": "unknown_tool",
                "arguments": {}
            })),
        };

        let response = server.handle_request(&request);
        let error = response.error.expect("has error");
        assert_eq!(error.code, METHOD_NOT_FOUND);
    }

    #[test]
    fn method_not_found_for_unknown_methods() {
        let mut server = McpServer::new();
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(5)),
            method: "unknown/method".to_string(),
            params: None,
        };
        let response = server.handle_request(&request);
        let error = response.error.expect("unknown method error");
        assert_eq!(error.code, METHOD_NOT_FOUND);
    }

    #[test]
    fn invalid_params_error_for_tools_call_missing_name() {
        let mut server = McpServer::new();
        server.register_builtin_tools();
        init_server(&mut server);

        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(6)),
            method: "tools/call".to_string(),
            params: Some(json!({ "arguments": {} })),
        };

        let response = server.handle_request(&request);
        let error = response.error.expect("invalid params error");
        assert_eq!(error.code, INVALID_PARAMS);
    }

    #[test]
    fn register_tool_adds_to_tool_list() {
        let mut server = McpServer::new();
        server.register_tool(
            ToolDefinition {
                name: "test_tool".to_string(),
                description: "test".to_string(),
                input_schema: json!({"type": "object"}),
            },
            Box::new(|_| ToolCallResult {
                content: vec![ToolContent::Text {
                    text: "ok".to_string(),
                }],
                is_error: false,
            }),
        );

        init_server(&mut server);

        let list_response = server.handle_request(&JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(7)),
            method: "tools/list".to_string(),
            params: None,
        });

        let tools = list_response.result.expect("list result")["tools"]
            .as_array()
            .expect("tools array")
            .clone();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], json!("test_tool"));
    }

    #[test]
    fn process_line_with_valid_json() {
        let mut server = McpServer::new();
        let response_raw = server
            .process_line(r#"{"jsonrpc":"2.0","id":10,"method":"initialize","params":{}}"#)
            .expect("response returned");
        let response = parse_response(&response_raw);
        assert_eq!(response.id, Some(json!(10)));
        assert!(response.error.is_none());
    }

    #[test]
    fn process_line_with_invalid_json() {
        let mut server = McpServer::new();
        let response_raw = server.process_line("[").expect("response returned");
        let response = parse_response(&response_raw);
        let error = response.error.expect("parse error response");
        assert_eq!(error.code, PARSE_ERROR);
    }

    #[test]
    fn builtin_tools_are_registered() {
        let mut server = McpServer::new();
        server.register_builtin_tools();
        init_server(&mut server);

        let response = server.handle_request(&JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(11)),
            method: "tools/list".to_string(),
            params: None,
        });

        let tools = response.result.expect("list result")["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .filter_map(|tool| tool.get("name").and_then(serde_json::Value::as_str))
            .map(ToString::to_string)
            .collect::<Vec<_>>();

        assert!(tools.contains(&"list_tasks".to_string()));
        assert!(tools.contains(&"create_task".to_string()));
        assert!(tools.contains(&"get_health".to_string()));
        assert_eq!(tools.len(), 12);
    }

    #[test]
    fn tools_methods_require_initialization() {
        let mut server = McpServer::new();
        server.register_builtin_tools();

        let response = server.handle_request(&JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(12)),
            method: "tools/list".to_string(),
            params: None,
        });

        let error = response.error.expect("requires initialization");
        assert_eq!(error.code, INVALID_REQUEST);
    }

    #[test]
    fn notifications_return_no_output_from_process_line() {
        let mut server = McpServer::new();
        let no_output = server.process_line(r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#);
        assert!(no_output.is_none());
    }
}
