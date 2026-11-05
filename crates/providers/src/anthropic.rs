//! Anthropic Messages wire (`POST …/messages`, SSE).

use serde_json::{json, Value};

use crate::sse::SseEvent;
use crate::{ChatRequest, Collector, Decode, Flow, ProviderError, StopReason};

pub(crate) fn body(req: &ChatRequest) -> Value {
    let messages: Vec<Value> = req
        .messages
        .iter()
        .map(|m| json!({ "role": m.role, "content": m.content }))
        .collect();
    json!({
        "model": req.model,
        "max_tokens": req.max_tokens,
        "stream": true,
        "system": req.system,
        "messages": messages,
    })
}

pub(crate) struct Decoder;

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
            "content_block_delta" => {
                let delta = &v["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") => out.text(delta["text"].as_str().unwrap_or_default()),
                    Some("thinking_delta") => {
                        out.thinking(delta["thinking"].as_str().unwrap_or_default())
                    }
                    _ => {}
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
                    Some(_) => out.stop(StopReason::End),
                }
            }
            "message_stop" => {
                if out.stop.is_none() {
                    out.stop(StopReason::End);
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
