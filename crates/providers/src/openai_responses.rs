//! OpenAI Responses wire (`POST …/responses`, SSE). Used by the OpenAI API and
//! the ChatGPT (Codex) backend.
//!
//! Tool calls stream as `response.output_item.added` (`function_call` with a
//! `call_id` and `name`) followed by `response.function_call_arguments.delta`.
//! Replay uses `function_call` and `function_call_output` input items keyed by
//! `call_id`. `reasoning` items are kept as opaque `Native` blocks.

use serde_json::{json, Value};

use crate::sse::SseEvent;
use crate::{
    ChatRequest, Collector, ContentBlock, Decode, Flow, ProviderError, Role, StopReason, ToolCall,
    ToolSpec, Wire,
};

pub(crate) fn body(req: &ChatRequest) -> Value {
    let input: Vec<Value> = req.messages.iter().flat_map(replay_message).collect();
    let mut body = json!({
        "model": req.model,
        "stream": true,
        "store": false,
        "instructions": req.system,
        "input": input,
    });
    if let Some(effort) = req.effort {
        body["reasoning"] = json!({ "effort": effort, "summary": "auto" });
    }
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(req.tools.iter().map(replay_tool).collect());
    }
    if let Some(key) = req.cache_key {
        body["prompt_cache_key"] = key.into();
    }
    body
}

fn replay_tool(t: &ToolSpec) -> Value {
    json!({ "type": "function", "name": t.name, "description": t.description, "parameters": t.input_schema })
}

/// Each message expands into one or more Responses `input` items. Text and
/// native reasoning items go into a `message` item; tool calls and results
/// become `function_call` / `function_call_output` items.
fn replay_message(m: &crate::Message) -> Vec<Value> {
    let mut out = Vec::new();
    let mut text_parts: Vec<Value> = Vec::new();
    for block in &m.content {
        match block {
            ContentBlock::Text { text } => {
                text_parts.push(json!({
                    "type": if m.role == Role::User { "input_text" } else { "output_text" },
                    "text": text,
                }));
            }
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => {
                out.push(json!({
                    "type": "function_call",
                    "call_id": id,
                    "name": name,
                    "arguments": ser::args_to_string(arguments),
                }));
            }
            ContentBlock::ToolResult {
                call_id,
                content,
                is_error: _,
            } => {
                out.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": content,
                }));
            }
            ContentBlock::Native { wire, raw } if *wire == Wire::OpenaiResponses => {
                // Replay the whole reasoning/output item verbatim.
                out.push(raw.clone());
            }
            ContentBlock::Native { .. } => {}
        }
    }
    if !text_parts.is_empty() {
        let role = if m.role == Role::User {
            "user"
        } else {
            "assistant"
        };
        out.insert(
            0,
            json!({ "type": "message", "role": role, "content": text_parts }),
        );
    }
    out
}

#[derive(Default)]
pub(crate) struct Decoder {
    refused: bool,
    /// Function calls keyed by call_id: (name, arguments string).
    calls: Vec<PartialCall>,
}

struct PartialCall {
    id: String,
    name: String,
    arguments: String,
}

impl Decode for Decoder {
    fn on_event(&mut self, ev: &SseEvent, out: &mut Collector) -> Result<Flow, ProviderError> {
        if ev.data.trim().is_empty() {
            return Ok(Flow::Continue);
        }
        let v: Value = serde_json::from_str(&ev.data)
            .map_err(|e| ProviderError::Protocol(format!("invalid stream event: {e}")))?;
        let delta = || v.get("delta").and_then(Value::as_str).unwrap_or_default();
        match v.get("type").and_then(Value::as_str).unwrap_or(&ev.event) {
            "response.output_text.delta" => out.text(delta()),
            "response.refusal.delta" => {
                self.refused = true;
                out.text(delta());
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                out.thinking(delta())
            }
            "response.output_item.added" => {
                let item = v.get("item").unwrap_or(&Value::Null);
                match item.get("type").and_then(Value::as_str) {
                    Some("function_call") => {
                        let id = item
                            .get("call_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        let name = item
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        self.calls.push(PartialCall {
                            id,
                            name,
                            arguments: String::new(),
                        });
                        out.tool_use();
                    }
                    Some("reasoning") => {
                        // Keep the full item as a native block (encrypted
                        // content / summary) and replay it unchanged next time.
                        out.push_native(ContentBlock::Native {
                            wire: Wire::OpenaiResponses,
                            raw: item.clone(),
                        });
                    }
                    _ => {}
                }
            }
            "response.function_call_arguments.delta" => {
                let id = v
                    .get("item_id")
                    .or_else(|| v.get("call_id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if let Some(c) = find_call(&mut self.calls, id) {
                    c.arguments.push_str(delta());
                }
            }
            "response.function_call_arguments.done" => {
                let id = v
                    .get("item_id")
                    .or_else(|| v.get("call_id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let args = v
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if let Some(c) = find_call(&mut self.calls, id) {
                    c.arguments = args;
                }
            }
            "response.completed" => {
                usage(&v, out);
                self.flush(out);
                if !out.tool_calls.is_empty() {
                    out.tool_use();
                } else {
                    out.stop(if self.refused {
                        StopReason::Refusal
                    } else {
                        StopReason::End
                    });
                }
                return Ok(Flow::Done);
            }
            "response.incomplete" => {
                usage(&v, out);
                self.flush(out);
                let reason = v
                    .pointer("/response/incomplete_details/reason")
                    .and_then(Value::as_str);
                if !out.tool_calls.is_empty() {
                    out.tool_use();
                } else {
                    out.stop(match reason {
                        Some("max_output_tokens") => StopReason::MaxTokens,
                        _ => StopReason::Refusal,
                    });
                }
                return Ok(Flow::Done);
            }
            "response.failed" | "error" => {
                let message = v
                    .pointer("/response/error/message")
                    .or_else(|| v.get("message"))
                    .or_else(|| v.pointer("/error/message"))
                    .and_then(Value::as_str)
                    .unwrap_or("the model service reported an error")
                    .to_owned();
                return Err(ProviderError::Api {
                    status: 200,
                    message,
                });
            }
            _ => {}
        }
        Ok(Flow::Continue)
    }
}

fn find_call<'a>(calls: &'a mut [PartialCall], id: &str) -> Option<&'a mut PartialCall> {
    if id.is_empty() {
        return calls.last_mut();
    }
    calls.iter_mut().find(|c| c.id == id)
}

impl Decoder {
    fn flush(&mut self, out: &mut Collector) {
        for c in self.calls.drain(..) {
            out.tool_calls.push(ToolCall {
                id: c.id,
                name: c.name,
                arguments: ser::parse_args(&c.arguments),
            });
        }
    }
}

fn usage(v: &Value, out: &mut Collector) {
    if let Some(u) = v.pointer("/response/usage") {
        if let Some(n) = u.get("input_tokens").and_then(Value::as_u64) {
            out.input_tokens(n);
        }
        if let Some(n) = u.get("output_tokens").and_then(Value::as_u64) {
            out.output_tokens(n);
        }
    }
}

mod ser {
    use serde_json::Value;

    /// Replay arguments as a JSON-encoded string on this wire.
    pub fn args_to_string(v: &Value) -> String {
        match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        }
    }

    /// Parse streamed arguments; malformed JSON is kept as a string.
    pub fn parse_args(raw: &str) -> Value {
        serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
    }
}
