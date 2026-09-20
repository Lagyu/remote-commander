use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const MAX_FRAME_BYTES: usize = 512 * 1024;
pub const MAX_REQUEST_BYTES: usize = 192 * 1024;
pub const READ_LIMIT: usize = 32 * 1024;
pub const WRITE_LIMIT: usize = 64 * 1024;
pub const PROTOCOL_VERSION: &str = "2025-11-25";
pub const SCOPES: [&str; 3] = ["commander:read", "commander:write", "commander:execute"];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentResponse {
    pub id: String,
    pub result: ToolResult,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextContent {
    pub r#type: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: Vec<TextContent>,
    #[serde(rename = "isError")]
    pub is_error: bool,
    #[serde(rename = "structuredContent", skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
}

impl ToolResult {
    pub fn ok(value: Value) -> Self {
        Self {
            content: vec![TextContent {
                r#type: "text".into(),
                text: value.to_string(),
            }],
            is_error: false,
            structured_content: Some(value),
        }
    }

    pub fn error(code: &str, message: impl ToString) -> Self {
        let mut result = Self::ok(json!({"error": code, "message": message.to_string()}));
        result.is_error = true;
        result
    }
}

pub fn scope_for(name: &str) -> Option<&'static str> {
    match name {
        "list_devices"
        | "get_device_info"
        | "get_recent_activity"
        | "ping_device"
        | "get_config"
        | "list_directory"
        | "read_file"
        | "read_multiple_files"
        | "get_file_info"
        | "start_search"
        | "get_more_search_results"
        | "stop_search"
        | "list_searches" => Some(SCOPES[0]),
        "write_file" | "edit_block" | "create_directory" | "move_file" => Some(SCOPES[1]),
        "start_process"
        | "read_process_output"
        | "interact_with_process"
        | "list_sessions"
        | "force_terminate"
        | "shutdown_device" => Some(SCOPES[2]),
        _ => None,
    }
}

pub fn tool_definitions() -> Vec<Value> {
    let string = || json!({"type":"string"});
    let path = || json!({"type":"string", "description":"Path relative to the agent's configured root. Absolute paths and '..' are rejected."});
    let number = |default: u64, maximum: u64| json!({"type":"integer","minimum":0,"maximum":maximum,"default":default});
    let mut tools = Vec::new();
    let mut add = |name: &str,
                   description: &str,
                   mut properties: Value,
                   required: &[&str],
                   remote: bool| {
        let scope = scope_for(name).expect("every advertised tool has a scope");
        let mut required: Vec<Value> = required.iter().map(|x| json!(x)).collect();
        if remote {
            properties["device_id"] = json!({"type":"string","description":"An exact device ID returned by list_devices."});
            required.push(json!("device_id"));
        }
        let read = scope == SCOPES[0];
        tools.push(json!({
            "name":name,"description":description,
            "inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false},
            "annotations":{"readOnlyHint":read,"destructiveHint":!read,"idempotentHint":read,"openWorldHint":scope == SCOPES[2]},
            "securitySchemes":[{"type":"oauth2","scopes":[scope]}],
            "_meta":{"securitySchemes":[{"type":"oauth2","scopes":[scope]}]}
        }));
    };
    add(
        "list_devices",
        "List paired computers, their identifiers, and current connection status.",
        json!({}),
        &[],
        false,
    );
    add(
        "get_device_info",
        "Read the name, connection state, and pairing time of a computer.",
        json!({}),
        &[],
        true,
    );
    add(
        "get_recent_activity",
        "Read recent tool names, outcomes, and timestamps. File contents and commands are never recorded.",
        json!({}),
        &[],
        false,
    );
    add(
        "ping_device",
        "Check whether a paired device can execute a request.",
        json!({}),
        &[],
        true,
    );
    add(
        "get_config",
        "Read the agent's root, enabled capabilities, and bounded operation limits.",
        json!({}),
        &[],
        true,
    );
    add(
        "list_directory",
        "List a directory with bounded depth and entries. Does not follow symbolic links.",
        json!({"path":path(),"depth":number(1,4)}),
        &["path"],
        true,
    );
    add(
        "read_file",
        "Read UTF-8 text by line offset. Response is capped at 32 KiB; use next_offset to continue.",
        json!({"path":path(),"offset":number(0,1000000),"length":number(200,2000)}),
        &["path"],
        true,
    );
    add(
        "read_multiple_files",
        "Read up to 8 text files, with individual errors and a combined output cap.",
        json!({"paths":{"type":"array","items":path(),"minItems":1,"maxItems":8}}),
        &["paths"],
        true,
    );
    add(
        "get_file_info",
        "Read file type, byte length, and modification time within the configured root.",
        json!({"path":path()}),
        &["path"],
        true,
    );
    add(
        "write_file",
        "Write UTF-8 text atomically, or append to an existing regular file. Requires local --allow-write.",
        json!({"path":path(),"content":string(),"mode":{"type":"string","enum":["rewrite","append"],"default":"rewrite"}}),
        &["path", "content"],
        true,
    );
    add(
        "edit_block",
        "Replace exact text only when the expected occurrence count matches. Requires local --allow-write.",
        json!({"path":path(),"old_string":string(),"new_string":string(),"expected_replacements":{"type":"integer","minimum":1,"maximum":100,"default":1}}),
        &["path", "old_string", "new_string"],
        true,
    );
    add(
        "create_directory",
        "Create a directory and missing parents within the configured root. Requires --allow-write.",
        json!({"path":path()}),
        &["path"],
        true,
    );
    add(
        "move_file",
        "Move or rename a regular file without replacing an existing destination. Requires --allow-write.",
        json!({"source":path(),"destination":path()}),
        &["source", "destination"],
        true,
    );
    add(
        "start_search",
        "Search bounded file names or text for a literal substring. Returns a paginated in-memory snapshot; never follows symlinks.",
        json!({"path":path(),"pattern":string(),"search_type":{"type":"string","enum":["files","content"],"default":"files"}}),
        &["path", "pattern"],
        true,
    );
    add(
        "get_more_search_results",
        "Read another page of a search snapshot. Results expire after ten minutes.",
        json!({"search_id":string(),"offset":number(0,1000),"length":number(50,100)}),
        &["search_id"],
        true,
    );
    add(
        "stop_search",
        "Release a search snapshot.",
        json!({"search_id":string()}),
        &["search_id"],
        true,
    );
    add(
        "list_searches",
        "List retained search snapshots and result counts.",
        json!({}),
        &[],
        true,
    );
    add(
        "start_process",
        "Start a shell process with piped stdin/stdout/stderr. Requires local --allow-shell, which grants the agent OS user's full authority. Sessions have a hard lifetime and bounded output. Not a PTY.",
        json!({"command":string(),"cwd":path(),"timeout_ms":{"type":"integer","minimum":100,"maximum":300000,"default":60000}}),
        &["command"],
        true,
    );
    add(
        "read_process_output",
        "Read captured process output from a byte cursor; indicates discarded bytes and process completion.",
        json!({"session_id":string(),"cursor":number(0,9007199254740991)}),
        &["session_id"],
        true,
    );
    add(
        "interact_with_process",
        "Send text to a running process's stdin, optionally closing stdin. Not a terminal emulator.",
        json!({"session_id":string(),"input":string(),"close_stdin":{"type":"boolean","default":false}}),
        &["session_id", "input"],
        true,
    );
    add(
        "list_sessions",
        "List processes started by this agent, including completion and timeout status.",
        json!({}),
        &[],
        true,
    );
    add(
        "force_terminate",
        "Terminate a process session and its process group. Cannot target unrelated system processes.",
        json!({"session_id":string()}),
        &["session_id"],
        true,
    );
    add(
        "shutdown_device",
        "Stop this device agent and its child processes. An operator must restart it locally.",
        json!({}),
        &[],
        true,
    );
    tools
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_tools_have_consistent_scopes_and_unique_names() {
        let tools = tool_definitions();
        let names: std::collections::HashSet<_> =
            tools.iter().map(|x| x["name"].as_str().unwrap()).collect();
        assert_eq!(names.len(), tools.len());
        for tool in tools {
            assert!(scope_for(tool["name"].as_str().unwrap()).is_some());
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        }
    }
}
