use std::{
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
};

use serde_json::Value;

use crate::{
    protocol::{
        CallToolParams, InitializeResult, JsonRpcRequest, JsonRpcResponse, ListToolsResult,
        INVALID_PARAMS, INVALID_REQUEST, MCP_PROTOCOL_VERSION, METHOD_NOT_FOUND, PARSE_ERROR,
        ServerCapabilities, ServerInfo, ToolsCapability,
    },
    tools::{dispatch_tool, tool_definitions},
};

pub struct McpServer {
    default_project_path: PathBuf,
}

impl McpServer {
    pub fn new(default_project_path: impl Into<PathBuf>) -> Self {
        Self {
            default_project_path: default_project_path.into(),
        }
    }

    pub fn default_project_path(&self) -> &Path {
        &self.default_project_path
    }

    /// Runs the MCP server over standard input and standard output (`stdio` transport).
    pub fn run_stdio(&mut self) -> io::Result<()> {
        let stdin = io::stdin();
        let stdout = io::stdout();
        self.serve(stdin.lock(), stdout.lock())
    }

    /// Processes newline-delimited JSON-RPC 2.0 messages from `reader` and writes responses to `writer`.
    pub fn serve<R: BufRead, W: Write>(&mut self, reader: R, mut writer: W) -> io::Result<()> {
        for line_result in reader.lines() {
            let line = line_result?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            if let Some(response) = self.handle_raw_message(trimmed) {
                let serialized = serde_json::to_string(&response)
                    .expect("JsonRpcResponse serialization cannot fail");
                writeln!(writer, "{serialized}")?;
                writer.flush()?;
            }
        }
        Ok(())
    }

    /// Handles a single JSON-RPC message string, returning `Some(JsonRpcResponse)` for requests
    /// and `None` for notifications (messages without an `id`).
    pub fn handle_raw_message(&mut self, raw: &str) -> Option<JsonRpcResponse> {
        let request: JsonRpcRequest = match serde_json::from_str(raw) {
            Ok(req) => req,
            Err(err) => {
                return Some(JsonRpcResponse::error(
                    Value::Null,
                    PARSE_ERROR,
                    format!("Invalid JSON: {err}"),
                ));
            }
        };

        self.handle_request(request)
    }

    pub fn handle_request(&mut self, request: JsonRpcRequest) -> Option<JsonRpcResponse> {
        // JSON-RPC notifications have no `id` and must not receive a response.
        let id = request.id?;

        if request.jsonrpc != "2.0" {
            return Some(JsonRpcResponse::error(
                id,
                INVALID_REQUEST,
                "jsonrpc version must be '2.0'",
            ));
        }

        match request.method.as_str() {
            "initialize" => {
                let result = InitializeResult {
                    protocol_version: MCP_PROTOCOL_VERSION.to_owned(),
                    capabilities: ServerCapabilities {
                        tools: ToolsCapability {
                            list_changed: Some(false),
                        },
                    },
                    server_info: ServerInfo {
                        name: "gfcut-mcp".to_owned(),
                        version: env!("CARGO_PKG_VERSION").to_owned(),
                    },
                };
                Some(JsonRpcResponse::success(id, result))
            }

            "ping" => Some(JsonRpcResponse::success(id, serde_json::json!({}))),

            "tools/list" => {
                let result = ListToolsResult {
                    tools: tool_definitions(),
                };
                Some(JsonRpcResponse::success(id, result))
            }

            "tools/call" => {
                let Some(params_val) = request.params else {
                    return Some(JsonRpcResponse::error(
                        id,
                        INVALID_PARAMS,
                        "missing params for tools/call",
                    ));
                };

                let params: CallToolParams = match serde_json::from_value(params_val) {
                    Ok(p) => p,
                    Err(err) => {
                        return Some(JsonRpcResponse::error(
                            id,
                            INVALID_PARAMS,
                            format!("invalid tools/call params: {err}"),
                        ));
                    }
                };

                let tool_result =
                    dispatch_tool(&self.default_project_path, &params.name, params.arguments);
                Some(JsonRpcResponse::success(id, tool_result))
            }

            other => Some(JsonRpcResponse::error(
                id,
                METHOD_NOT_FOUND,
                format!("method not found: {other}"),
            )),
        }
    }
}
