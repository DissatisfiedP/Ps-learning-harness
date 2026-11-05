//! OpenAI chat-completions wire (`POST …/chat/completions`, SSE).

use serde_json::{json, Value};

use crate::sse::SseEvent;
use crate::{ChatRequest, Collector, Decode, Flow, ProviderError, StopReason};

pub(crate) fn body(req: &ChatRequest) -> Value {
    let mut messages = Vec::with_capacity(req.messages.len() + 1);
    // WorkBuddy rejects requests whose first message is not `system` (11128).
    messages.push(json!({ "role": "system", "content": req.system }));
    messages.extend(
        req.messages
            .iter()
            .map(|m| json!({ "role": m.role, "content": m.content })),
    );
    let mut body = json!({
        "model": req.model,
        "stream": true,
        "stream_options": { "include_usage": true },
        "max_tokens": req.max_tokens,
        "messages": messages,
    });
    if let Some(effort) = req.effort {
        body["reasoning_effort"] = effort.into();
    }
    body
}

pub(crate) struct Decoder;

impl Decode for Decoder {
    fn on_event(&mut self, ev: &SseEvent, out: &mut Collector) -> Result<Flow, ProviderError> {
        let data = ev.data.trim();
        if data.is_empty() {
            return Ok(Flow::Continue);
        }
        if data == "[DONE]" {
            if out.stop.is_none() {
                out.stop(StopReason::End);
            }
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
                Some(_) => out.stop(StopReason::End),
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
