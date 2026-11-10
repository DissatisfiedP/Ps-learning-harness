//! Anthropic Messages wire (`POST …/messages`, SSE).
//!
//! Tool calls stream as `content_block_start` of type `tool_use` followed by
//! `input_json_delta.partial_json` fragments. Replay puts `tool_use` blocks in
//! the assistant message and `tool_result` blocks in the next user message.
//! `thinking`/`redacted_thinking` blocks (with signatures) are kept as opaque
//! `Native` blocks and replayed unchanged.

use serde_json::{json, Value};

use crate::sse::SseEvent;
use crate::{
    ChatRequest, Collector, ContentBlock, Decode, Flow, ProviderError, Role, StopReason, ToolCall,
    ToolSpec, Wire,
};

/// `max_tokens` for a model whose limit the caller doesn't know. The field is
/// required here, and this is the smallest output limit of any Claude model
/// the API still serves (Haiku/Sonnet/Opus 4.5; everything newer allows
/// 128000), so no live model refuses it.
pub(crate) const FALLBACK_MAX_TOKENS: u32 = 64_000;

pub(crate) fn body(req: &ChatRequest, claude_code: bool) -> Value {
    // Roles must alternate: merge consecutive same-role messages (e.g. tool
    // results followed by a delivered job message), keeping block order.
    let mut messages: Vec<Value> = Vec::with_capacity(req.messages.len());
    for m in req.messages.iter().map(replay_message) {
        match messages.last_mut() {
            Some(prev) if prev["role"] == m["role"] => {
                if let (Some(dst), Some(src)) =
                    (prev["content"].as_array_mut(), m["content"].as_array())
                {
                    dst.extend(src.iter().cloned());
                }
            }
            _ => messages.push(m),
        }
    }
    // Subscription (Claude Code) requests must carry the CLI identity as
    // system[0]. The agent's own system prompt follows as a second block.
    let mut system: Vec<Value> = Vec::with_capacity(2);
    if claude_code {
        system.push(json!({ "type": "text", "text": crate::claude_oauth::CLAUDE_CODE_PREFIX }));
    }
    system.push(json!({
        "type": "text",
        "text": req.system,
        "cache_control": { "type": "ephemeral" },
    }));
    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens.unwrap_or(FALLBACK_MAX_TOKENS),
        "stream": true,
        "system": system,
        "messages": messages,
    });
    if let Some(effort) = req.effort {
        // Effort also sets thinking depth on adaptive-thinking models, and
        // thinking counts toward `max_tokens`.
        body["output_config"] = json!({ "effort": effort });
    }
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(req.tools.iter().map(replay_tool).collect());
    }
    // Anthropic caches by prefix; mark the last (most recent) message too.
    if let Some(last) = body["messages"].as_array_mut().and_then(|a| a.last_mut()) {
        if let Some(content) = last.get_mut("content") {
            if let Some(arr) = content.as_array_mut() {
                if let Some(block) = arr.last_mut() {
                    block["cache_control"] = json!({ "type": "ephemeral" });
                }
            } else if content.is_string() {
                // A plain string becomes a cached text block.
                let text = content.as_str().unwrap_or_default().to_owned();
                *content = json!([{
                    "type": "text",
                    "text": text,
                    "cache_control": { "type": "ephemeral" },
                }]);
            }
        }
    }
    body
}

fn replay_tool(t: &ToolSpec) -> Value {
    json!({ "name": t.name, "description": t.description, "input_schema": t.input_schema })
}

/// Build one Anthropic message from typed content blocks.
fn replay_message(m: &crate::Message) -> Value {
    let content: Vec<Value> = m
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(json!({ "type": "text", "text": text })),
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => {
                // `input` must be an object. Arguments that never parsed (a
                // call cut off at the output cap) are kept as a string; the
                // tool result already told the model so, and sending the
                // string would get the whole request rejected with a 400.
                let input = if arguments.is_object() {
                    arguments.clone()
                } else {
                    json!({})
                };
                Some(json!({ "type": "tool_use", "id": id, "name": name, "input": input }))
            }
            ContentBlock::ToolResult {
                call_id,
                content,
                is_error,
            } => {
                let mut b = json!({
                    "type": "tool_result",
                    "tool_use_id": call_id,
                    "content": content,
                });
                if *is_error {
                    b["is_error"] = true.into();
                }
                Some(b)
            }
            ContentBlock::Image { media_type, data } => Some(json!({
                "type": "image",
                "source": { "type": "base64", "media_type": media_type, "data": data },
            })),
            ContentBlock::Native { wire, raw } if *wire == Wire::AnthropicMessages => {
                Some(raw.clone())
            }
            ContentBlock::Native { .. } => None,
        })
        .collect();
    let role = if m.role == Role::User {
        "user"
    } else {
        "assistant"
    };
    // A tool result belongs in a user message; a message mixing both roles is
    // not expected, but role is taken from the message itself.
    json!({ "role": role, "content": content })
}

#[derive(Default)]
pub(crate) struct Decoder {
    /// Accumulated tool_use blocks by content-block index.
    calls: Vec<(usize, PartialCall)>,
    /// Native reasoning blocks captured from content_block_start.
    native: Vec<Value>,
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
        match v.get("type").and_then(Value::as_str).unwrap_or(&ev.event) {
            "message_start" => {
                if let Some(n) = v
                    .pointer("/message/usage/input_tokens")
                    .and_then(Value::as_u64)
                {
                    out.input_tokens(n);
                }
            }
            "content_block_start" => {
                let index = v.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let block = v.get("content_block").unwrap_or(&Value::Null);
                match block.get("type").and_then(Value::as_str) {
                    Some("tool_use") => {
                        let id = block
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        let name = block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        // Streaming sends `input: {}` as a placeholder; the
                        // real JSON arrives in input_json_delta fragments. A
                        // non-empty object (a server that sends the whole
                        // input up front) is kept as the arguments.
                        let args = match block.get("input") {
                            Some(Value::Object(o)) if o.is_empty() => String::new(),
                            Some(Value::String(s)) => s.clone(),
                            Some(v) if !v.is_null() => v.to_string(),
                            _ => String::new(),
                        };
                        self.calls.push((
                            index,
                            PartialCall {
                                id,
                                name,
                                arguments: args,
                            },
                        ));
                        out.tool_use();
                    }
                    Some("thinking") | Some("redacted_thinking") => {
                        self.native.push(block.clone());
                    }
                    _ => {}
                }
            }
            "content_block_delta" => {
                let delta = &v["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") => out.text(delta["text"].as_str().unwrap_or_default()),
                    Some("thinking_delta") => {
                        out.thinking(delta["thinking"].as_str().unwrap_or_default())
                    }
                    Some("signature_delta") => {
                        // A signature arrives after the thinking block; attach
                        // it to the last native thinking block.
                        if let Some(last) = self.native.last_mut() {
                            last["signature"] = delta["signature"].clone();
                        }
                    }
                    Some("input_json_delta") => {
                        let index = v.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                        let partial = delta["partial_json"].as_str().unwrap_or_default();
                        if let Some((_, c)) = self.calls.iter_mut().find(|(i, _)| *i == index) {
                            c.arguments.push_str(partial);
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let index = v.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                if let Some(pos) = self.calls.iter().position(|(i, _)| *i == index) {
                    let (_, c) = self.calls.remove(pos);
                    out.tool_calls.push(ToolCall {
                        id: c.id,
                        name: c.name,
                        arguments: ser::parse_args(&c.arguments),
                    });
                }
            }
            "message_delta" => {
                if let Some(n) = v.pointer("/usage/output_tokens").and_then(Value::as_u64) {
                    out.output_tokens(n);
                }
                match v.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    None => {}
                    Some("max_tokens") => out.stop(StopReason::MaxTokens),
                    Some("refusal") => out.stop(StopReason::Refusal),
                    Some("tool_use") => out.tool_use(),
                    Some(_) => {
                        if !out.tool_calls.is_empty() || !self.calls.is_empty() {
                            out.tool_use();
                        } else {
                            out.stop(StopReason::End);
                        }
                    }
                }
            }
            "message_stop" => {
                self.flush(out);
                if !out.has_stop() {
                    out.stop(if out.tool_calls.is_empty() {
                        StopReason::End
                    } else {
                        StopReason::ToolUse
                    });
                }
                return Ok(Flow::Done);
            }
            "error" => {
                let message = v
                    .pointer("/error/message")
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

impl Decoder {
    fn flush(&mut self, out: &mut Collector) {
        for (_, c) in self.calls.drain(..) {
            out.tool_calls.push(ToolCall {
                id: c.id,
                name: c.name,
                arguments: ser::parse_args(&c.arguments),
            });
        }
        for raw in self.native.drain(..) {
            out.push_native(ContentBlock::Native {
                wire: Wire::AnthropicMessages,
                raw,
            });
        }
    }
}

mod ser {
    use serde_json::Value;

    /// Parse streamed arguments; malformed JSON is kept as a string.
    pub fn parse_args(raw: &str) -> Value {
        if raw.is_empty() {
            return Value::Object(Default::default());
        }
        serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
    }
}
