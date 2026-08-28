//! Map omp RPC JSONL stdout into Klepto `SessionEvent`s.

use crate::SessionEvent;

/// Track whether streaming deltas already delivered content for this turn.
#[derive(Debug, Default)]
pub struct StreamState {
    pub saw_text: bool,
    pub saw_thinking: bool,
    pub saw_error: bool,
}

/// Parse one JSONL line from omp RPC stdout into zero or more session events.
pub fn map_omp_line(line: &str, state: &mut StreamState) -> Vec<SessionEvent> {
    let line = line.trim();
    if line.is_empty() {
        return Vec::new();
    }
    let v: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let typ = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match typ {
        "message_update" => map_message_update(&v, state),
        "agent_end" => map_agent_end(&v, state),
        "turn_end" => map_turn_end(&v, state),
        // Legacy pi frame; omp never emits this, but keep for old journals.
        "agent_settled" => {
            *state = StreamState::default();
            vec![SessionEvent::Status {
                status: "idle".into(),
            }]
        }
        "tool_execution_start" => {
            let name = v
                .get("toolName")
                .or_else(|| v.get("tool_name"))
                .and_then(|t| t.as_str())
                .unwrap_or("tool")
                .to_string();
            let args = v
                .get("args")
                .map(|a| match a {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            vec![SessionEvent::ToolCall { name, args }]
        }
        "tool_execution_end" | "tool_execution_error" => {
            let tool = v
                .get("toolName")
                .or_else(|| v.get("tool_name"))
                .and_then(|t| t.as_str())
                .unwrap_or("tool")
                .to_string();
            let is_error = typ == "tool_execution_error"
                || v.get("isError").and_then(|b| b.as_bool()).unwrap_or(false);
            let output = v
                .get("result")
                .or_else(|| v.get("output"))
                .or_else(|| v.get("error"))
                .map(|a| match a {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            vec![SessionEvent::ToolResult {
                tool,
                exit_code: if is_error { 1 } else { 0 },
                output,
            }]
        }
        "error" => {
            let message = v
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("omp error")
                .to_string();
            vec![SessionEvent::Error { message }]
        }
        // Ignore ready/response/rpc_chunk/acks and other host protocol frames.
        "ready" | "response" | "rpc_chunk" | "prompt_result" | "extension_ui_request"
        | "host_tool_call" | "host_tool_cancel" | "host_uri_request" | "host_uri_cancel"
        | "available_commands_update" | "extension_error" => Vec::new(),
        _ => Vec::new(),
    }
}

fn map_message_update(v: &serde_json::Value, state: &mut StreamState) -> Vec<SessionEvent> {
    let ame = match v.get("assistantMessageEvent") {
        Some(a) => a,
        None => return Vec::new(),
    };
    let ev_type = ame.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let delta = ame
        .get("delta")
        .and_then(|d| d.as_str())
        .unwrap_or("")
        .to_string();
    if delta.is_empty() {
        return Vec::new();
    }
    match ev_type {
        "text_delta" => {
            state.saw_text = true;
            vec![SessionEvent::TextDelta { text: delta }]
        }
        "thinking_delta" => {
            state.saw_thinking = true;
            vec![SessionEvent::ThinkingDelta { text: delta }]
        }
        _ => Vec::new(),
    }
}

fn map_agent_end(v: &serde_json::Value, state: &mut StreamState) -> Vec<SessionEvent> {
    // omp: isTerminal === false means more work is scheduled; not turn completion.
    if v.get("isTerminal").and_then(|t| t.as_bool()) == Some(false) {
        return Vec::new();
    }

    let mut events = Vec::new();

    // Fallback: if no deltas streamed, pull final content from agent_end.
    if !state.saw_thinking {
        if let Some(thinking) = extract_thinking(v) {
            if !thinking.is_empty() {
                state.saw_thinking = true;
                events.push(SessionEvent::ThinkingDelta { text: thinking });
            }
        }
    }
    if !state.saw_text {
        if let Some(text) = extract_text(v) {
            if !text.is_empty() {
                state.saw_text = true;
                events.push(SessionEvent::TextDelta { text });
            }
        }
    }
    if !state.saw_error {
        if let Some(message) = extract_error(v) {
            state.saw_error = true;
            events.push(SessionEvent::Error { message });
        }
    }

    *state = StreamState::default();
    events.push(SessionEvent::Status {
        status: "agent_end".into(),
    });
    events.push(SessionEvent::Status {
        status: "idle".into(),
    });
    events
}

fn map_turn_end(v: &serde_json::Value, state: &mut StreamState) -> Vec<SessionEvent> {
    if state.saw_error {
        return Vec::new();
    }
    let Some(message) = extract_error(v) else {
        return Vec::new();
    };
    state.saw_error = true;
    vec![SessionEvent::Error { message }]
}

fn extract_error(v: &serde_json::Value) -> Option<String> {
    let direct = v
        .get("errorMessage")
        .or_else(|| v.get("error"))
        .and_then(error_text);
    if direct.is_some() {
        return direct;
    }
    if let Some(message) = v.get("message").and_then(|message| {
        message
            .get("errorMessage")
            .or_else(|| message.get("error"))
            .and_then(error_text)
    }) {
        return Some(message);
    }
    v.get("messages")
        .and_then(|messages| messages.as_array())
        .into_iter()
        .flatten()
        .rev()
        .find(|message| message.get("role").and_then(|role| role.as_str()) == Some("assistant"))
        .and_then(|message| {
            message
                .get("errorMessage")
                .or_else(|| message.get("error"))
                .and_then(error_text)
        })
}

fn error_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(message) if !message.trim().is_empty() => {
            Some(message.to_string())
        }
        serde_json::Value::Object(error) => error
            .get("message")
            .and_then(|message| message.as_str())
            .filter(|message| !message.trim().is_empty())
            .map(str::to_string),
        _ => None,
    }
}

fn extract_thinking(v: &serde_json::Value) -> Option<String> {
    if let Some(t) = v.get("thinking").and_then(|t| t.as_str()) {
        return Some(t.to_string());
    }
    content_blocks(v).find_map(|block| {
        if block.get("type").and_then(|t| t.as_str()) == Some("thinking") {
            block
                .get("thinking")
                .and_then(|t| t.as_str())
                .map(|s| s.to_string())
        } else {
            None
        }
    })
}

fn extract_text(v: &serde_json::Value) -> Option<String> {
    if let Some(t) = v.get("text").and_then(|t| t.as_str()) {
        return Some(t.to_string());
    }
    let parts: Vec<String> = content_blocks(v)
        .filter_map(|block| {
            if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                block
                    .get("text")
                    .and_then(|t| t.as_str())
                    .map(|s| s.to_string())
            } else {
                None
            }
        })
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(""))
    }
}

fn content_blocks(v: &serde_json::Value) -> impl Iterator<Item = &serde_json::Value> {
    v.get("messages")
        .and_then(|m| m.as_array())
        .into_iter()
        .flatten()
        .rev()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("assistant"))
        .and_then(|m| m.get("content").and_then(|c| c.as_array()))
        .into_iter()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_thinking_and_text_deltas() {
        let mut state = StreamState::default();
        let thinking = r#"{"type":"message_update","assistantMessageEvent":{"type":"thinking_delta","delta":"Hmm…"}}"#;
        let text = r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"Hello"}}"#;
        assert!(matches!(
            map_omp_line(thinking, &mut state).as_slice(),
            [SessionEvent::ThinkingDelta { text }] if text == "Hmm…"
        ));
        assert!(matches!(
            map_omp_line(text, &mut state).as_slice(),
            [SessionEvent::TextDelta { text }] if text == "Hello"
        ));
        assert!(state.saw_text && state.saw_thinking);
    }

    #[test]
    fn agent_end_fallback_extracts_content() {
        let mut state = StreamState::default();
        let line = r#"{
            "type":"agent_end",
            "messages":[{
                "role":"assistant",
                "content":[
                    {"type":"thinking","thinking":"plan"},
                    {"type":"text","text":"Why do programmers prefer dark mode?"}
                ]
            }],
            "willRetry":false
        }"#;
        let events = map_omp_line(line, &mut state);
        assert!(matches!(
            &events[0],
            SessionEvent::ThinkingDelta { text } if text == "plan"
        ));
        assert!(matches!(
            &events[1],
            SessionEvent::TextDelta { text } if text.contains("dark mode")
        ));
        assert!(matches!(
            &events[2],
            SessionEvent::Status { status } if status == "agent_end"
        ));
        assert!(matches!(
            &events[3],
            SessionEvent::Status { status } if status == "idle"
        ));
    }

    #[test]
    fn agent_end_skips_fallback_when_deltas_seen() {
        let mut state = StreamState {
            saw_text: true,
            saw_thinking: true,
            saw_error: false,
        };
        let line = r#"{"type":"agent_end","messages":[{"role":"assistant","content":[{"type":"text","text":"dup"}]}],"willRetry":false}"#;
        let events = map_omp_line(line, &mut state);
        assert_eq!(events.len(), 2);
        assert!(matches!(
            &events[0],
            SessionEvent::Status { status } if status == "agent_end"
        ));
        assert!(matches!(
            &events[1],
            SessionEvent::Status { status } if status == "idle"
        ));
    }

    #[test]
    fn non_terminal_agent_end_is_ignored() {
        let mut state = StreamState::default();
        let events = map_omp_line(
            r#"{"type":"agent_end","isTerminal":false,"messages":[]}"#,
            &mut state,
        );
        assert!(events.is_empty());
    }

    #[test]
    fn ignores_ready_and_response_frames() {
        let mut state = StreamState::default();
        assert!(
            map_omp_line(
                r#"{"type":"ready","protocolVersion":1}"#,
                &mut state
            )
            .is_empty()
        );
        assert!(
            map_omp_line(
                r#"{"id":"req_1","type":"response","command":"prompt","success":true}"#,
                &mut state
            )
            .is_empty()
        );
    }

    #[test]
    fn agent_settled_emits_idle() {
        let mut state = StreamState {
            saw_text: true,
            saw_thinking: true,
            saw_error: false,
        };
        let events = map_omp_line(r#"{"type":"agent_settled"}"#, &mut state);
        assert!(matches!(
            events.as_slice(),
            [SessionEvent::Status { status }] if status == "idle"
        ));
        assert!(!state.saw_text && !state.saw_thinking);
    }

    #[test]
    fn ignores_prompt_echo() {
        let mut state = StreamState::default();
        let events = map_omp_line(
            r#"{"type":"prompt","message":"tell me a joke","streamingBehavior":"followUp"}"#,
            &mut state,
        );
        assert!(events.is_empty());
    }

    #[test]
    fn failed_agent_end_emits_error_before_status() {
        let mut state = StreamState::default();
        let line = r#"{
            "type":"agent_end",
            "messages":[{
                "role":"assistant",
                "content":[],
                "stopReason":"error",
                "errorMessage":"404: model does not exist"
            }],
            "willRetry":false
        }"#;
        let events = map_omp_line(line, &mut state);
        assert!(matches!(
            &events[0],
            SessionEvent::Error { message } if message.contains("404")
        ));
        assert!(matches!(
            &events[1],
            SessionEvent::Status { status } if status == "agent_end"
        ));
    }

    #[test]
    fn turn_end_error_is_not_duplicated_by_agent_end() {
        let mut state = StreamState::default();
        let turn = r#"{
            "type":"turn_end",
            "message":{
                "role":"assistant",
                "stopReason":"error",
                "errorMessage":"provider unavailable"
            }
        }"#;
        assert!(matches!(
            map_omp_line(turn, &mut state).as_slice(),
            [SessionEvent::Error { message }] if message == "provider unavailable"
        ));
        let end = r#"{"type":"agent_end","errorMessage":"provider unavailable"}"#;
        let events = map_omp_line(end, &mut state);
        assert!(matches!(
            &events[0],
            SessionEvent::Status { status } if status == "agent_end"
        ));
        assert!(matches!(
            &events[1],
            SessionEvent::Status { status } if status == "idle"
        ));
    }
}
