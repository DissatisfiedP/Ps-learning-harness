//! OpenAI chat-completions wire (`POST …/chat/completions`, SSE).
//!
//! Tool calls stream as `choices[].delta.tool_calls[i]` fragments keyed by
//! `index`; the replay is an assistant `tool_calls` message followed by
//! `{role:"tool"}` messages. Reasoning `thinking` blocks from other wires are
//! never emitted here, so `Native` blocks with a different wire are dropped.

use serde_json::{json, Value};

use crate::sse::SseEvent;
use crate::{
    ChatRequest, Collector, ContentBlock, Decode, Flow, ProviderError, Role, StopReason, ToolCall,
    ToolSpec, Wire,
};

pub(crate) fn body(req: &ChatRequest) -> Value {
    let mut messages = Vec::with_capacity(req.messages.len() + 1);
    // WorkBuddy rejects requests whose first message is not `system` (11128).
    messages.push(json!({ "role": "system", "content": req.system }));
    messages.extend(req.messages.iter().flat_map(replay_message));
    let mut body = json!({
        "model": req.model,
        "stream": true,
        "stream_options": { "include_usage": true },
        "messages": messages,
    });
    if let Some(effort) = req.effort {
        body["reasoning_effort"] = effort.into();
    }
    if let Some(max) = req.max_tokens {
        body["max_tokens"] = max.into();
    }
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(req.tools.iter().map(replay_tool).collect());
    }
    if let Some(key) = req.cache_key {
        // Where the wire accepts it (OpenAI / OpenRouter / DeepSeek).
        body["prompt_cache_key"] = key.into();
    }
    body
}

fn replay_tool(t: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": t.name,
            "description": t.description,
            "parameters": t.input_schema,
        },
    })
}
fn replay_message(m: &crate::Message) -> Vec<Value> {
    let mut out = Vec::new();
    let mut text: Option<String> = None;
    let mut tool_calls = Vec::new();
    let mut tool_results = Vec::new();
    let mut native_text: Option<String> = None;
    let mut images = Vec::new();
    for block in &m.content {
        match block {
            ContentBlock::Text { text: t } => match text.as_mut() {
                Some(acc) => {
                    acc.push_str("\n\n");
                    acc.push_str(t);
                }
                None => text = Some(t.clone()),
            },
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => {
                tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": { "name": name, "arguments": ser::args_to_string(arguments) },
                }));
            }
            ContentBlock::ToolResult {
                call_id,
                content: c,
                is_error: _,
            } => {
                tool_results.push(json!({
                    "role": "tool",
                    "tool_call_id": call_id,
                    "content": c,
                }));
            }
            ContentBlock::Image { media_type, data } => {
                images.push(json!({
                    "type": "image_url",
                    "image_url": { "url": format!("data:{media_type};base64,{data}") },
                }));
            }
            // Only replay our own native blocks; this wire has none normally.
            ContentBlock::Native { wire, raw } if *wire == Wire::OpenaiChat => {
                native_text = Some(raw.to_string());
            }
            ContentBlock::Native { .. } => {}
        }
    }
    let role = match m.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    match m.role {
        Role::Assistant => {
            // An assistant message: text (or null) plus its tool_calls.
            let mut msg = json!({ "role": role });
            msg["content"] = text
                .or(native_text)
                .map(Value::String)
                .unwrap_or(Value::Null);
            if !tool_calls.is_empty() {
                msg["tool_calls"] = Value::Array(tool_calls);
            }
            out.push(msg);
        }
        Role::User => {
            // Tool results must directly follow the assistant's tool_calls, so
            // they come first; any user text follows as its own message.
            let body = text.or(native_text);
            let only_text = tool_results.is_empty();
            out.extend(tool_results);
            if images.is_empty() {
                if body.is_some() || only_text {
                    out.push(json!({ "role": "user", "content": body.unwrap_or_default() }));
                }
            } else {
                let mut parts = Vec::new();
                if let Some(t) = &body {
                    parts.push(json!({ "type": "text", "text": t }));
                }
                parts.extend(images);
                out.push(json!({ "role": "user", "content": parts }));
            }
        }
    }
    out
}

#[derive(Default)]
pub(crate) struct Decoder {
    /// Accumulated calls by stream index: (id, name, arguments string).
    calls: Vec<PartialCall>,
}

#[derive(Default)]
struct PartialCall {
    id: String,
    name: String,
    arguments: String,
}

impl Decode for Decoder {
    fn on_event(&mut self, ev: &SseEvent, out: &mut Collector) -> Result<Flow, ProviderError> {
        let data = ev.data.trim();
        if data.is_empty() {
            return Ok(Flow::Continue);
        }
        if data == "[DONE]" {
            self.flush(out);
            return Ok(Flow::Done);
        }
        let v: Value = serde_json::from_str(data)
            .map_err(|e| ProviderError::Protocol(format!("invalid stream chunk: {e}")))?;
        if let Some(err) = v.get("error") {
            let message = err
                .get("message")
                .and_then(Value::as_str)
                .map_or_else(|| err.to_string(), str::to_owned);
            return Err(ProviderError::Api {
                status: 200,
                message,
            });
        }
        if let Some(choice) = v.pointer("/choices/0") {
            if let Some(delta) = choice.get("delta") {
                // DeepSeek/WorkBuddy use `reasoning_content`, OpenRouter `reasoning`.
                for key in ["reasoning_content", "reasoning"] {
                    if let Some(t) = delta.get(key).and_then(Value::as_str) {
                        out.thinking(t);
                    }
                }
                if let Some(t) = delta.get("content").and_then(Value::as_str) {
                    out.text(t);
                }
                if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                    for frag in calls {
                        let idx = frag.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                        if self.calls.len() <= idx {
                            self.calls.resize_with(idx + 1, PartialCall::default);
                        }
                        let slot = &mut self.calls[idx];
                        if let Some(id) = frag.get("id").and_then(Value::as_str) {
                            slot.id = id.to_owned();
                        }
                        if let Some(f) = frag.get("function") {
                            if let Some(name) = f.get("name").and_then(Value::as_str) {
                                slot.name = name.to_owned();
                            }
                            if let Some(args) = f.get("arguments").and_then(Value::as_str) {
                                slot.arguments.push_str(args);
                            }
                        }
                    }
                    // Set ToolUse as soon as calls appear; finish_reason may be absent.
                    out.tool_use();
                }
            }
            match choice.get("finish_reason").and_then(Value::as_str) {
                None | Some("") => {}
                Some("length" | "max_tokens") => out.stop(StopReason::MaxTokens),
                Some("content_filter" | "refusal") => out.stop(StopReason::Refusal),
                Some("error") => {
                    return Err(ProviderError::Api {
                        status: 200,
                        message: "the model service reported an error mid-stream".into(),
                    })
                }
                Some("tool_calls") => out.tool_use(),
                Some(_) => {
                    if !self.calls.is_empty() {
                        out.tool_use();
                    } else {
                        out.stop(StopReason::End);
                    }
                }
            }
        }
        if let Some(usage) = v.get("usage").filter(|u| u.is_object()) {
            if let Some(n) = usage.get("prompt_tokens").and_then(Value::as_u64) {
                out.input_tokens(n);
            }
            if let Some(n) = usage.get("completion_tokens").and_then(Value::as_u64) {
                out.output_tokens(n);
            }
        }
        Ok(Flow::Continue)
    }
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

mod ser {
    use serde_json::Value;

    /// Arguments replay as a JSON-encoded string on this wire.
    pub fn args_to_string(v: &Value) -> String {
        match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        }
    }

    /// Parse streamed arguments; malformed JSON is kept as a string so the
    /// runtime can answer with an error tool result and let the model correct.
    pub fn parse_args(raw: &str) -> Value {
        serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
    }
}
