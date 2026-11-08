//! Model provider clients.
//!
//! A provider is identified by [`ProviderId`]; it resolves a credential into an
//! [`Endpoint`] (URL + headers + wire format). Streaming is wire-specific, not
//! provider-specific: every provider speaks one of the three [`Wire`]s.

mod anthropic;
mod catalog;
pub mod claude_oauth;
pub mod codex_oauth;
pub mod models;
mod openai_chat;
mod openai_responses;
mod sse;
pub mod workbuddy;

use futures_util::StreamExt;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use sse::{SseDecoder, SseEvent};

pub use catalog::{Credential, CredentialKind, ProviderId, ProviderSpec};
pub use models::{Catalog, CatalogModel};
pub use reqwest::Client;

// ---- Core types ---------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Wire {
    /// `POST {base}/chat/completions` (OpenAI, OpenRouter, DeepSeek, Z.AI, WorkBuddy).
    OpenaiChat,
    /// `POST {base}/responses` (ChatGPT/Codex backend).
    OpenaiResponses,
    /// `POST {base}/messages` (Anthropic).
    AnthropicMessages,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// A function tool the model may call. `input_schema` is a JSON Schema object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// A call the assistant made. `arguments` is parsed JSON, or `Value::String(raw)`
/// when the streamed arguments did not parse (answered as an error tool result).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Message content is a sequence of typed blocks so provider-native blocks
/// (Anthropic `thinking` with a signature, Responses `reasoning` items) can be
/// replayed unchanged across tool loops.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    /// Assistant message: a call the model made.
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    /// User/assistant message: the result of executing a call.
    ToolResult {
        call_id: String,
        content: String,
        is_error: bool,
    },
    /// A user-supplied image, base64-encoded. Built when the request is
    /// assembled from an attachment reference; bytes never enter the op log.
    Image {
        media_type: String,
        data: String,
    },
    /// Opaque provider block that must be replayed verbatim to the same wire.
    /// Dropped when the next request targets a different wire.
    Native {
        wire: Wire,
        raw: serde_json::Value,
    },
}

impl ContentBlock {
    /// Convenience for the common single-text message.
    pub fn text(s: impl Into<String>) -> Self {
        ContentBlock::Text { text: s.into() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn text(role: Role, s: impl Into<String>) -> Self {
        Message {
            role,
            content: vec![ContentBlock::text(s)],
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChatRequest<'a> {
    pub model: &'a str,
    pub system: &'a str,
    pub messages: &'a [Message],
    pub max_tokens: u32,
    /// Reasoning effort (`low`/`medium`/`high`/…); sent only when set.
    pub effort: Option<&'a str>,
    /// Tools offered to the model. Empty means none are sent.
    pub tools: &'a [ToolSpec],
    /// Session id used for prefix caching where the wire accepts it.
    pub cache_key: Option<&'a str>,
}

/// A fully resolved request target.
#[derive(Debug, Clone)]
pub struct Endpoint {
    pub wire: Wire,
    pub url: String,
    pub headers: HeaderMap,
    /// Subscription (Claude Code) requests must prepend the "You are Claude
    /// Code" identity to `system[0]`; API-key requests must not.
    pub claude_code: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamEvent<'a> {
    Text(&'a str),
    Thinking(&'a str),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StopReason {
    End,
    MaxTokens,
    Refusal,
    /// The model wants to call tools; `Completion::tool_calls` holds them.
    ToolUse,
}

/// How a turn ended. The text itself arrives only through [`StreamEvent`]s,
/// so callers own the single copy of it.
#[derive(Debug, Clone, PartialEq)]
pub struct Completion {
    pub stop: StopReason,
    pub usage: Usage,
    pub tool_calls: Vec<ToolCall>,
    /// Opaque native blocks (e.g. Anthropic thinking signatures, Responses
    /// reasoning items) that must be replayed with the assistant message.
    pub native: Vec<ContentBlock>,
}

/// Capabilities a catalog may assert. `None` means "unverified" (unknown),
/// never "missing"; the UI words it that way.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
}

/// Model entry shown in the picker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub efforts: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<String>,
    /// Billing multiplier label as the provider reports it (WorkBuddy credits).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_label: Option<String>,
    /// Catalog-asserted capabilities. Unset fields are "unverified".
    #[serde(default, skip_serializing_if = "Capabilities::is_empty")]
    pub capabilities: Capabilities,
}

impl Capabilities {
    fn is_empty(&self) -> bool {
        self.tool_calls.is_none() && self.vision.is_none()
    }
}
impl ModelInfo {
    pub fn named(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            name: id.to_owned(),
            description: None,
            context_window: None,
            max_output_tokens: None,
            efforts: Vec::new(),
            default_effort: None,
            cost_label: None,
            capabilities: Capabilities::default(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("{0} is not connected")]
    NotConnected(&'static str),
    #[error("invalid configuration: {0}")]
    Config(String),
    #[error("not authorized: {0}")]
    Unauthorized(String),
    #[error("rate limited: {0}")]
    RateLimited(String),
    #[error("request rejected ({status}): {message}")]
    Api { status: u16, message: String },
    #[error("network error: {0}")]
    Transport(String),
    #[error("the connection closed before the model finished")]
    StreamCut,
    #[error("unexpected response: {0}")]
    Protocol(String),
}

impl ProviderError {
    pub(crate) fn transport(e: reqwest::Error) -> Self {
        Self::Transport(e.without_url().to_string())
    }
}

/// Streams one model turn, forwarding text and reasoning deltas to `on_event`
/// as they arrive.
pub async fn stream_chat(
    client: &Client,
    endpoint: &Endpoint,
    req: &ChatRequest<'_>,
    on_event: &mut (dyn FnMut(StreamEvent<'_>) + Send),
) -> Result<Completion, ProviderError> {
    match endpoint.wire {
        Wire::OpenaiChat => {
            let body = openai_chat::body(req);
            drive(
                client,
                endpoint,
                &body,
                openai_chat::Decoder::default(),
                on_event,
            )
            .await
        }
        Wire::OpenaiResponses => {
            let body = openai_responses::body(req);
            drive(
                client,
                endpoint,
                &body,
                openai_responses::Decoder::default(),
                on_event,
            )
            .await
        }
        Wire::AnthropicMessages => {
            let body = anthropic::body(req, endpoint.claude_code);
            drive(
                client,
                endpoint,
                &body,
                anthropic::Decoder::default(),
                on_event,
            )
            .await
        }
    }
}
/// Forwards deltas and records how the turn ends. Wire decoders accumulate
/// tool calls (and opaque native blocks) here; `finish` drains them.
pub(crate) struct Collector<'f> {
    on_event: &'f mut (dyn FnMut(StreamEvent<'_>) + Send),
    usage: Usage,
    stop: Option<StopReason>,
    pub tool_calls: Vec<ToolCall>,
    pub native: Vec<ContentBlock>,
}

impl Collector<'_> {
    pub fn text(&mut self, delta: &str) {
        if !delta.is_empty() {
            (self.on_event)(StreamEvent::Text(delta));
        }
    }
    pub fn thinking(&mut self, delta: &str) {
        if !delta.is_empty() {
            (self.on_event)(StreamEvent::Thinking(delta));
        }
    }
    pub fn input_tokens(&mut self, n: u64) {
        self.usage.input_tokens = saturate(n);
    }
    pub fn output_tokens(&mut self, n: u64) {
        self.usage.output_tokens = saturate(n);
    }
    pub fn stop(&mut self, reason: StopReason) {
        self.stop = Some(reason);
    }
    pub fn has_stop(&self) -> bool {
        self.stop.is_some()
    }
    /// Force the stop reason to ToolUse when the wire reported tool calls.
    pub fn tool_use(&mut self) {
        self.stop = Some(StopReason::ToolUse);
    }
    pub fn push_native(&mut self, block: ContentBlock) {
        self.native.push(block);
    }
}
fn saturate(n: u64) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}
pub(crate) enum Flow {
    Continue,
    Done,
}

pub(crate) trait Decode {
    fn on_event(&mut self, ev: &SseEvent, out: &mut Collector) -> Result<Flow, ProviderError>;
}

async fn drive(
    client: &Client,
    endpoint: &Endpoint,
    body: &serde_json::Value,
    mut decoder: impl Decode,
    on_event: &mut (dyn FnMut(StreamEvent<'_>) + Send),
) -> Result<Completion, ProviderError> {
    let resp = client
        .post(&endpoint.url)
        .headers(endpoint.headers.clone())
        .json(body)
        .send()
        .await
        .map_err(ProviderError::transport)?;
    let status = resp.status();
    let is_json = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    if !status.is_success() || is_json {
        // Gateways (WorkBuddy) reject with a JSON envelope instead of a stream.
        let text = resp.text().await.map_err(ProviderError::transport)?;
        return Err(error_from_body(status.as_u16(), &text));
    }

    let mut out = Collector {
        on_event,
        usage: Usage::default(),
        stop: None,
        tool_calls: Vec::new(),
        native: Vec::new(),
    };
    let mut sse = SseDecoder::default();
    let mut events = Vec::new();
    let mut stream = resp.bytes_stream();
    let mut done = false;
    while !done {
        match stream.next().await {
            Some(chunk) => sse.push(&chunk.map_err(ProviderError::transport)?, &mut events),
            None => {
                sse.finish(&mut events);
                done = true;
            }
        }
        for ev in events.drain(..) {
            if let Flow::Done = decoder.on_event(&ev, &mut out)? {
                return finish(out);
            }
        }
    }
    finish(out)
}

/// A stream that ends without a terminal event or a stop reason was cut
/// mid-turn; reporting it as a normal stop would silently truncate answers.
fn finish(out: Collector) -> Result<Completion, ProviderError> {
    let stop = out.stop.ok_or(ProviderError::StreamCut)?;
    Ok(Completion {
        stop,
        usage: out.usage,
        tool_calls: out.tool_calls,
        native: out.native,
    })
}

/// Maps an error response to a readable error. Understands OpenAI/Anthropic
/// (`error.message`) and WorkBuddy (`code`/`msg`/`displayMsg`) envelopes and
/// gateway HTML pages.
pub(crate) fn error_from_body(status: u16, body: &str) -> ProviderError {
    let message = error_message(body);
    match status {
        401 | 403 => ProviderError::Unauthorized(message),
        429 => ProviderError::RateLimited(message),
        _ => ProviderError::Api { status, message },
    }
}

fn error_message(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(m) = v.pointer("/error/message").and_then(|m| m.as_str()) {
            return m.to_owned();
        }
        if let Some(code) = v.get("code").and_then(|c| c.as_i64()) {
            let msg = v.get("msg").and_then(|m| m.as_str()).unwrap_or_default();
            return match v.pointer("/displayMsg/en").and_then(|m| m.as_str()) {
                Some(display) if !display.is_empty() => format!("{display} ({code}: {msg})"),
                _ => format!("{msg} ({code})"),
            };
        }
        if let Some(m) = v.get("message").and_then(|m| m.as_str()) {
            return m.to_owned();
        }
    }
    if let Some(title) = body
        .split_once("<title>")
        .and_then(|(_, rest)| rest.split_once("</title>"))
    {
        return title.0.trim().to_owned();
    }
    let trimmed = body.trim();
    match trimmed.char_indices().nth(300) {
        Some((i, _)) => format!("{}…", &trimmed[..i]),
        None if trimmed.is_empty() => "empty response".to_owned(),
        None => trimmed.to_owned(),
    }
}

/// Decodes a JWT payload without verifying it (display and routing only).
pub(crate) fn jwt_claims(token: &str) -> Option<serde_json::Value> {
    use base64::Engine;
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn image_blocks_serialize_on_every_wire() {
        let messages = [Message {
            role: Role::User,
            content: vec![
                ContentBlock::text("what is this?"),
                ContentBlock::Image {
                    media_type: "image/png".into(),
                    data: "QUJD".into(),
                },
            ],
        }];
        let req = ChatRequest {
            model: "m",
            system: "s",
            messages: &messages,
            max_tokens: 8,
            effort: None,
            tools: &[],
            cache_key: None,
        };

        let chat = openai_chat::body(&req);
        assert_eq!(
            chat["messages"][1],
            json!({ "role": "user", "content": [
                { "type": "text", "text": "what is this?" },
                { "type": "image_url", "image_url": { "url": "data:image/png;base64,QUJD" } }
            ] })
        );

        let responses = openai_responses::body(&req);
        assert_eq!(
            responses["input"][0],
            json!({ "type": "message", "role": "user", "content": [
                { "type": "input_text", "text": "what is this?" },
                { "type": "input_image", "image_url": "data:image/png;base64,QUJD" }
            ] })
        );

        let anthropic = anthropic::body(&req, false);
        assert_eq!(anthropic["system"].as_array().unwrap().len(), 1);
        assert_eq!(anthropic["messages"][0]["content"][1]["type"], "image");
        assert_eq!(
            anthropic["messages"][0]["content"][1]["source"],
            json!({ "type": "base64", "media_type": "image/png", "data": "QUJD" })
        );
        // Subscription requests prepend the Claude Code identity.
        let oauth = anthropic::body(&req, true);
        assert_eq!(oauth["system"][0]["text"], claude_oauth::CLAUDE_CODE_PREFIX);
        assert_eq!(oauth["system"][1]["text"], "s");
    }

    #[test]
    fn workbuddy_envelope_prefers_display_message() {
        let body = r#"{"code":11158,"msg":"model [gpt-5.5] is not allowed for user type [personal_free_user]","displayMsg":{"en":"Upgrade to use this model"}}"#;
        let err = error_from_body(400, body);
        assert_eq!(
            err.to_string(),
            "request rejected (400): Upgrade to use this model (11158: model [gpt-5.5] is not allowed for user type [personal_free_user])"
        );
    }

    #[test]
    fn gateway_html_reduces_to_title_and_status_classifies() {
        let body = "<html><head><title>401 Authorization Required</title></head></html>";
        assert!(matches!(
            error_from_body(401, body),
            ProviderError::Unauthorized(m) if m == "401 Authorization Required"
        ));
        assert!(matches!(
            error_from_body(429, r#"{"error":{"message":"slow down"}}"#),
            ProviderError::RateLimited(m) if m == "slow down"
        ));
    }
}
