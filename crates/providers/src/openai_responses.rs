//! OpenAI Responses wire (`POST …/responses`, SSE). Used by the OpenAI API and
//! the ChatGPT (Codex) backend.
//!
//! `max_output_tokens` is never sent: the Codex backend rejects it, and the
//! model's own output limit is the right cap for a teaching turn.

use serde_json::{json, Value};

use crate::sse::SseEvent;
use crate::{ChatRequest, Collector, Decode, Flow, ProviderError, Role, StopReason};

pub(crate) fn body(req: &ChatRequest) -> Value {
    let input: Vec<Value> = req
        .messages
        .iter()
        .map(|m| {
            let kind = match m.role {
                Role::User => "input_text",
                Role::Assistant => "output_text",
            };
            json!({ "role": m.role, "content": [{ "type": kind, "text": m.content }] })
        })
        .collect();
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
    body
}

#[derive(Default)]
pub(crate) struct Decoder {
    refused: bool,
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
            "response.completed" => {
                usage(&v, out);
                out.stop(if self.refused {
                    StopReason::Refusal
                } else {
                    StopReason::End
                });
                return Ok(Flow::Done);
            }
            "response.incomplete" => {
                usage(&v, out);
                let reason = v
                    .pointer("/response/incomplete_details/reason")
                    .and_then(Value::as_str);
                out.stop(match reason {
                    Some("max_output_tokens") => StopReason::MaxTokens,
                    _ => StopReason::Refusal,
                });
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
