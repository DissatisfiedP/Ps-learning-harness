//! End-to-end wire tests against a one-shot local HTTP server.

use providers::{
    stream_chat, ChatRequest, Client, Credential, Message, ProviderError, ProviderId, Role,
    StopReason, StreamEvent,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Serves one response (written in the given chunks) and returns the base URL
/// plus a handle yielding the raw request.
async fn serve(
    status: &str,
    content_type: &str,
    chunks: Vec<String>,
) -> (String, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let head =
        format!("HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\nconnection: close\r\n\r\n");
    let handle = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut req = Vec::new();
        let mut buf = [0u8; 4096];
        // Read headers + body (Content-Length) before answering.
        loop {
            let n = sock.read(&mut buf).await.unwrap();
            req.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&req);
            if let Some(split) = text.find("\r\n\r\n") {
                let len = text[..split]
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if req.len() >= split + 4 + len {
                    break;
                }
            }
        }
        sock.write_all(head.as_bytes()).await.unwrap();
        for c in chunks {
            sock.write_all(c.as_bytes()).await.unwrap();
            sock.flush().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        String::from_utf8(req).unwrap()
    });
    (base, handle)
}

fn messages() -> Vec<Message> {
    vec![Message {
        role: Role::User,
        content: "teach me limits".into(),
    }]
}

async fn run(
    id: ProviderId,
    base: &str,
) -> (Result<providers::Completion, ProviderError>, Vec<String>) {
    let msgs = messages();
    let req = ChatRequest {
        model: "m",
        system: "be a tutor",
        messages: &msgs,
        max_tokens: 512,
        effort: Some("high"),
    };
    let cred = Credential::ApiKey { key: "k".into() };
    let ep = id.endpoint(&cred, Some(base), "conv").unwrap();
    let mut seen = Vec::new();
    let result = stream_chat(&Client::new(), &ep, &req, &mut |ev| match ev {
        StreamEvent::Text(t) => seen.push(format!("T:{t}")),
        StreamEvent::Thinking(t) => seen.push(format!("R:{t}")),
    })
    .await;
    (result, seen)
}

#[tokio::test]
async fn chat_completions_stream_collects_reasoning_text_and_usage() {
    let frames = [
        r#"data: {"choices":[{"index":0,"delta":{"role":"assistant","reasoning_content":"think"},"finish_reason":""}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{"content":"Hel"},"finish_reason":""}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{"content":"lo"},"finish_reason":"stop"}]}"#,
        r#"data: {"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":3}}"#,
        "data: [DONE]",
    ];
    // Split one frame across two TCP writes, plus a heartbeat comment.
    let joined = frames.map(|f| format!("{f}\n\n")).concat();
    let mid = joined.len() / 2;
    let chunks = vec![
        ": heartbeat\n\n".to_owned(),
        joined[..mid].to_owned(),
        joined[mid..].to_owned(),
    ];
    let (base, req) = serve("200 OK", "text/event-stream", chunks).await;

    let (result, seen) = run(ProviderId::Deepseek, &base).await;
    let done = result.unwrap();
    assert_eq!(done.stop, StopReason::End);
    assert_eq!((done.usage.input_tokens, done.usage.output_tokens), (12, 3));
    assert_eq!(seen, ["R:think", "T:Hel", "T:lo"]);

    let raw = req.await.unwrap();
    assert!(raw.starts_with("POST /chat/completions"));
    let body: serde_json::Value =
        serde_json::from_str(raw.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["reasoning_effort"], "high");
    assert_eq!(body["stream"], true);
}

#[tokio::test]
async fn stream_without_terminal_event_is_reported_as_cut() {
    let chunks = vec!["data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n".to_owned()];
    let (base, _req) = serve("200 OK", "text/event-stream", chunks).await;
    let (result, seen) = run(ProviderId::Openrouter, &base).await;
    assert!(matches!(result, Err(ProviderError::StreamCut)));
    assert_eq!(seen, ["T:partial"]);
}

#[tokio::test]
async fn json_error_envelope_on_400_becomes_api_error() {
    let body = r#"{"code":11102,"msg":"model [m] service info not found","displayMsg":{"en":"Model unavailable. Please switch models"}}"#;
    let (base, _req) = serve("400 Bad Request", "application/json", vec![body.to_owned()]).await;
    let (result, _) = run(ProviderId::Zai, &base).await;
    match result {
        Err(ProviderError::Api {
            status: 400,
            message,
        }) => {
            assert!(
                message.starts_with("Model unavailable. Please switch models"),
                "{message}"
            )
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn anthropic_stream_maps_blocks_and_stop_reason() {
    let events = [
        (
            "message_start",
            r#"{"type":"message_start","message":{"usage":{"input_tokens":20}}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"A limit"}}"#,
        ),
        (
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":9}}"#,
        ),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ];
    let body = events
        .map(|(e, d)| format!("event: {e}\ndata: {d}\n\n"))
        .concat();
    let (base, req) = serve("200 OK", "text/event-stream", vec![body]).await;
    let (result, seen) = run(ProviderId::Anthropic, &base).await;
    let done = result.unwrap();
    assert_eq!(done.stop, StopReason::MaxTokens);
    assert_eq!((done.usage.input_tokens, done.usage.output_tokens), (20, 9));
    assert_eq!(seen, ["R:hmm", "T:A limit"]);
    let raw = req.await.unwrap();
    assert!(raw.starts_with("POST /messages"));
    assert!(raw.to_ascii_lowercase().contains("x-api-key: k"));
}

#[tokio::test]
async fn responses_stream_handles_incomplete_and_reasoning_summary() {
    let events = [
        r#"{"type":"response.reasoning_summary_text.delta","delta":"plan"}"#,
        r#"{"type":"response.output_text.delta","delta":"Step 1"}"#,
        r#"{"type":"response.incomplete","response":{"incomplete_details":{"reason":"max_output_tokens"},"usage":{"input_tokens":5,"output_tokens":7}}}"#,
    ];
    let body = events.map(|d| format!("data: {d}\n\n")).concat();
    let (base, req) = serve("200 OK", "text/event-stream", vec![body]).await;
    let (result, seen) = run(ProviderId::Openai, &base).await;
    let done = result.unwrap();
    assert_eq!(done.stop, StopReason::MaxTokens);
    assert_eq!(seen, ["R:plan", "T:Step 1"]);
    let raw = req.await.unwrap();
    let body: serde_json::Value =
        serde_json::from_str(raw.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["instructions"], "be a tutor");
    assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
    assert!(body.get("max_output_tokens").is_none());
}
