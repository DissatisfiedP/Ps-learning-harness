//! M1 tool-calling wire tests: SSE fixtures with text + two split tool calls,
//! malformed arguments, and request-body replay shapes / native blocks.

use providers::{
    stream_chat, ChatRequest, Client, ContentBlock, Credential, Message, ProviderId, Role,
    StopReason, StreamEvent, ToolSpec, Wire,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Serves one SSE response and returns the base URL plus a handle yielding the
/// raw HTTP request (headers + body).
async fn serve(chunks: Vec<String>) -> (String, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n";
    let handle = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut req = Vec::new();
        let mut buf = [0u8; 4096];
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
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        String::from_utf8(req).unwrap()
    });
    (base, handle)
}

fn body_of(raw: &str) -> serde_json::Value {
    serde_json::from_str(raw.split("\r\n\r\n").nth(1).unwrap()).unwrap()
}

fn ping_tool() -> ToolSpec {
    ToolSpec {
        name: "ping".into(),
        description: "echo a value".into(),
        input_schema: serde_json::json!({ "type": "object", "properties": { "x": {"type":"integer"} } }),
    }
}

/// Runs with the given messages/tools and returns (completion, seen events,
/// raw request body).
async fn run_full(
    id: ProviderId,
    base: &str,
    messages: &[Message],
    tools: &[ToolSpec],
) -> (providers::Completion, Vec<String>, serde_json::Value) {
    let req = ChatRequest {
        model: "m",
        system: "sys",
        messages,
        max_tokens: Some(512),
        effort: None,
        tools,
        cache_key: Some("sess-1"),
    };
    let cred = Credential::ApiKey { key: "k".into() };
    let ep = id.endpoint(&cred, Some(base), "conv").unwrap();
    let mut seen = Vec::new();
    let done = stream_chat(&Client::new(), &ep, &req, &mut |ev| match ev {
        StreamEvent::Text(t) => seen.push(format!("T:{t}")),
        StreamEvent::Thinking(t) => seen.push(format!("R:{t}")),
    })
    .await
    .unwrap();
    // The raw request is served by the same one-shot listener; we can't get it
    // here, so callers use `serve_with_body`.
    (done, seen, serde_json::Value::Null)
}

// ---------------------------------------------------------------------------
// OpenAI chat: split arguments across chunks, two calls, text delta.
// ---------------------------------------------------------------------------

const CHAT_SPLIT: &[&str] = &[
    // A text delta first.
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Working \"},\"finish_reason\":\"\"}]}\n\n",
    // Call 0: id + name + first half of arguments.
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"type\":\"function\",\"function\":{\"name\":\"ping\",\"arguments\":\"{\\\"x\\\":1\"}}]},\"finish_reason\":\"\"}]}\n\n",
    // Call 1: id + name (split from its arguments).
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call_b\",\"type\":\"function\",\"function\":{\"name\":\"ping\"}}]},\"finish_reason\":\"\"}]}\n\n",
    // Call 0 second half + Call 1 arguments (split across the boundary).
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\",\\\"y\\\":2}\"}},{\"index\":1,\"function\":{\"arguments\":\"{\\\"x\\\":3\"}}]},\"finish_reason\":\"\"}]}\n\n",
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":1,\"function\":{\"arguments\":\",\\\"y\\\":4}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
    "data: [DONE]\n\n",
];

#[tokio::test]
async fn chat_two_split_tool_calls_accumulate_by_index() {
    let chunks = CHAT_SPLIT.iter().map(|s| s.to_string()).collect();
    let (base, _raw) = serve(chunks).await;
    let (done, seen, _) = run_full(ProviderId::Deepseek, &base, &messages(), &[ping_tool()]).await;
    assert_eq!(done.stop, StopReason::ToolUse);
    assert_eq!(seen, ["T:Working "]);
    assert_eq!(done.tool_calls.len(), 2);
    assert_eq!(done.tool_calls[0].id, "call_a");
    assert_eq!(done.tool_calls[0].name, "ping");
    assert_eq!(
        done.tool_calls[0].arguments,
        serde_json::json!({"x":1,"y":2})
    );
    assert_eq!(done.tool_calls[1].id, "call_b");
    assert_eq!(
        done.tool_calls[1].arguments,
        serde_json::json!({"x":3,"y":4})
    );
}

#[tokio::test]
async fn chat_malformed_arguments_become_string_value() {
    let frames = [
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c\",\"type\":\"function\",\"function\":{\"name\":\"ping\",\"arguments\":\"{not json\"}}]},\"finish_reason\":\"\"}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    ];
    let (base, _raw) = serve(frames.iter().map(|s| s.to_string()).collect()).await;
    let (done, _, _) = run_full(ProviderId::Openrouter, &base, &messages(), &[ping_tool()]).await;
    assert_eq!(done.stop, StopReason::ToolUse);
    assert_eq!(done.tool_calls.len(), 1);
    assert_eq!(
        done.tool_calls[0].arguments,
        serde_json::Value::String("{not json".into())
    );
}

/// Messages for replay-shape assertions: a tool call, its result, and a
/// reasoning native block from a *different* wire (must be dropped).
#[tokio::test]
async fn chat_replay_shape_and_cross_wire_native_drop() {
    let messages = vec![
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "Let me check.".into(),
                },
                ContentBlock::ToolCall {
                    id: "call_a".into(),
                    name: "ping".into(),
                    arguments: serde_json::json!({"x":1}),
                },
                ContentBlock::Native {
                    wire: Wire::AnthropicMessages,
                    raw: serde_json::json!({"type":"thinking","signature":"sig"}),
                },
            ],
        },
        Message {
            role: Role::User,
            content: vec![
                ContentBlock::ToolResult {
                    call_id: "call_a".into(),
                    content: "pong".into(),
                    is_error: false,
                },
                // e.g. a delivered job result in the same user turn
                ContentBlock::Text {
                    text: "[research brief] done".into(),
                },
            ],
        },
    ];
    // A plain stop stream.
    let frames = [
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    ];
    let (base, raw) = serve(frames.iter().map(|s| s.to_string()).collect()).await;
    let (_done, _, _) = run_full(ProviderId::Deepseek, &base, &messages, &[ping_tool()]).await;
    let body = body_of(&raw.await.unwrap());
    let msgs = body["messages"].as_array().unwrap();
    // system + assistant + tool result + the user's text after it.
    assert_eq!(msgs.len(), 4);
    assert_eq!(msgs[0]["role"], "system");
    // assistant: text + tool_calls, no anthropic native block.
    assert_eq!(msgs[1]["role"], "assistant");
    assert_eq!(msgs[1]["content"], "Let me check.");
    assert_eq!(msgs[1]["tool_calls"][0]["id"], "call_a");
    assert_eq!(msgs[1]["tool_calls"][0]["function"]["name"], "ping");
    assert_eq!(
        msgs[1]["tool_calls"][0]["function"]["arguments"],
        "{\"x\":1}"
    );
    // The cross-wire Native block must NOT appear anywhere in the request.
    assert!(!body.to_string().contains("thinking"));
    assert!(!body.to_string().contains("signature"));
    // tool result is its own {role:"tool"} message keyed by tool_call_id.
    assert_eq!(msgs[2]["role"], "tool");
    assert_eq!(msgs[2]["tool_call_id"], "call_a");
    assert_eq!(msgs[2]["content"], "pong");
    // User text follows the tool message; it never splits call from result.
    assert_eq!(msgs[3]["role"], "user");
    assert_eq!(msgs[3]["content"], "[research brief] done");
    // prompt_cache_key is set.
    assert_eq!(body["prompt_cache_key"], "sess-1");
    // tools are in the function/name shape.
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["function"]["name"], "ping");
}

// ---------------------------------------------------------------------------
// OpenAI Responses: split arguments, reasoning native, replay.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn responses_two_split_tool_calls_and_reasoning_native() {
    let events = [
        // Text + a reasoning item (kept as Native).
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Planning \"}\n\n",
        "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"reasoning\",\"id\":\"rs_1\",\"encrypted_content\":\"enc\",\"summary\":[]}}\n\n",
        // Call A added, then arguments split.
        "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"call_id\":\"call_a\",\"name\":\"ping\"}}\n\n",
        "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"call_a\",\"delta\":\"{\\\"x\\\":1\"}\n\n",
        "data: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"call_a\",\"arguments\":\"{\\\"x\\\":1,\\\"y\\\":2}\"}\n\n",
        // Call B added, arguments split across delta + done.
        "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"call_id\":\"call_b\",\"name\":\"ping\"}}\n\n",
        "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"call_b\",\"delta\":\"{\\\"x\\\":3\"}\n\n",
        "data: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"call_b\",\"arguments\":\"{\\\"x\\\":3,\\\"y\\\":4}\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":7}}}\n\n",
    ];
    let (base, _raw) = serve(events.iter().map(|s| s.to_string()).collect()).await;
    let (done, seen, _) = run_full(ProviderId::Openai, &base, &messages(), &[ping_tool()]).await;
    assert_eq!(done.stop, StopReason::ToolUse);
    assert_eq!(seen, ["T:Planning "]);
    assert_eq!(done.tool_calls.len(), 2);
    assert_eq!(
        done.tool_calls[0].arguments,
        serde_json::json!({"x":1,"y":2})
    );
    assert_eq!(
        done.tool_calls[1].arguments,
        serde_json::json!({"x":3,"y":4})
    );
    // The reasoning item is replayable unchanged.
    assert_eq!(done.native.len(), 1);
    match &done.native[0] {
        ContentBlock::Native { wire, raw } => {
            assert_eq!(*wire, Wire::OpenaiResponses);
            assert_eq!(raw["type"], "reasoning");
            assert_eq!(raw["encrypted_content"], "enc");
        }
        other => panic!("unexpected native block: {other:?}"),
    }
}

#[tokio::test]
async fn responses_malformed_arguments_become_string_value() {
    let events = [
        "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"call_id\":\"c\",\"name\":\"ping\"}}\n\n",
        "data: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"c\",\"arguments\":\"{oops\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{}}}\n\n",
    ];
    let (base, _raw) = serve(events.iter().map(|s| s.to_string()).collect()).await;
    let (done, _, _) = run_full(ProviderId::Openai, &base, &messages(), &[ping_tool()]).await;
    assert_eq!(done.stop, StopReason::ToolUse);
    assert_eq!(
        done.tool_calls[0].arguments,
        serde_json::Value::String("{oops".into())
    );
}

#[tokio::test]
async fn responses_replay_shape_and_native_reasoning() {
    let messages = vec![
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::ToolCall {
                    id: "call_a".into(),
                    name: "ping".into(),
                    arguments: serde_json::json!({"x":1}),
                },
                ContentBlock::Native {
                    wire: Wire::OpenaiResponses,
                    raw: serde_json::json!({"type":"reasoning","id":"rs_1","encrypted_content":"enc"}),
                },
            ],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                call_id: "call_a".into(),
                content: "pong".into(),
                is_error: true,
            }],
        },
    ];
    let events = ["data: {\"type\":\"response.completed\",\"response\":{\"usage\":{}}}\n\n"];
    let (base, raw) = serve(events.iter().map(|s| s.to_string()).collect()).await;
    let (_done, _, _) = run_full(ProviderId::Openai, &base, &messages, &[ping_tool()]).await;
    let body = body_of(&raw.await.unwrap());
    let input = body["input"].as_array().unwrap();
    // function_call + reasoning + function_call_output.
    let kinds: Vec<&str> = input.iter().map(|i| i["type"].as_str().unwrap()).collect();
    assert!(kinds.contains(&"function_call"), "{kinds:?}");
    assert!(kinds.contains(&"function_call_output"), "{kinds:?}");
    assert!(kinds.contains(&"reasoning"), "{kinds:?}");
    let fc = input.iter().find(|i| i["type"] == "function_call").unwrap();
    assert_eq!(fc["call_id"], "call_a");
    assert_eq!(fc["arguments"], "{\"x\":1}");
    let out = input
        .iter()
        .find(|i| i["type"] == "function_call_output")
        .unwrap();
    assert_eq!(out["call_id"], "call_a");
    assert_eq!(out["output"], "pong");
    // A native reasoning item from another wire would be dropped; here it is
    // replayed verbatim (encrypted_content present).
    let reasoning = input.iter().find(|i| i["type"] == "reasoning").unwrap();
    assert_eq!(reasoning["encrypted_content"], "enc");
    // tools use top-level name/parameters.
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["name"], "ping");
}

// ---------------------------------------------------------------------------
// Anthropic: split input_json_delta, thinking native, replay, cache_control.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn anthropic_two_split_tool_calls_and_thinking_native() {
    let events = [
        (
            "message_start",
            r#"{"type":"message_start","message":{"usage":{"input_tokens":10}}}"#,
        ),
        (
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Thinking "}}"#,
        ),
        (
            "content_block_start",
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"thinking","thinking":"hmm"}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"signature_delta","signature":"sig-xyz"}}"#,
        ),
        (
            "content_block_start",
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_a","name":"ping","input":{}}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"x\":1"}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":",\"y\":2}"}}"#,
        ),
        (
            "content_block_stop",
            r#"{"type":"content_block_stop","index":2}"#,
        ),
        (
            "content_block_start",
            r#"{"type":"content_block_start","index":3,"content_block":{"type":"tool_use","id":"toolu_b","name":"ping","input":{}}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":3,"delta":{"type":"input_json_delta","partial_json":"{\"x\":3"}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":3,"delta":{"type":"input_json_delta","partial_json":",\"y\":4}"}}"#,
        ),
        (
            "content_block_stop",
            r#"{"type":"content_block_stop","index":3}"#,
        ),
        (
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}"#,
        ),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ];
    let body = events
        .map(|(e, d)| format!("event: {e}\ndata: {d}\n\n"))
        .concat();
    let (base, _raw) = serve(vec![body]).await;
    let (done, seen, _) = run_full(ProviderId::Anthropic, &base, &messages(), &[ping_tool()]).await;
    assert_eq!(done.stop, StopReason::ToolUse);
    assert_eq!(seen, ["T:Thinking "]);
    assert_eq!(done.tool_calls.len(), 2);
    assert_eq!(done.tool_calls[0].id, "toolu_a");
    assert_eq!(
        done.tool_calls[0].arguments,
        serde_json::json!({"x":1,"y":2})
    );
    assert_eq!(
        done.tool_calls[1].arguments,
        serde_json::json!({"x":3,"y":4})
    );
    // The thinking block carries its signature.
    assert_eq!(done.native.len(), 1);
    match &done.native[0] {
        ContentBlock::Native { wire, raw } => {
            assert_eq!(*wire, Wire::AnthropicMessages);
            assert_eq!(raw["type"], "thinking");
            assert_eq!(raw["signature"], "sig-xyz");
        }
        other => panic!("unexpected native: {other:?}"),
    }
}

#[tokio::test]
async fn anthropic_malformed_arguments_become_string_value() {
    let events = [
        (
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t","name":"ping","input":{}}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{bad json"}}"#,
        ),
        (
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        ),
        (
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":1}}"#,
        ),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ];
    let body = events
        .map(|(e, d)| format!("event: {e}\ndata: {d}\n\n"))
        .concat();
    let (base, _raw) = serve(vec![body]).await;
    let (done, _, _) = run_full(ProviderId::Anthropic, &base, &messages(), &[ping_tool()]).await;
    assert_eq!(done.stop, StopReason::ToolUse);
    assert_eq!(
        done.tool_calls[0].arguments,
        serde_json::Value::String("{bad json".into())
    );
}

#[tokio::test]
async fn anthropic_replay_shape_native_and_cache_control() {
    let messages = vec![
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::ToolCall {
                    id: "toolu_a".into(),
                    name: "ping".into(),
                    arguments: serde_json::json!({"x":1}),
                },
                ContentBlock::Native {
                    wire: Wire::AnthropicMessages,
                    raw: serde_json::json!({"type":"thinking","thinking":"hmm","signature":"sig"}),
                },
            ],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                call_id: "toolu_a".into(),
                content: "pong".into(),
                is_error: true,
            }],
        },
    ];
    let events = [
        (
            "message_start",
            r#"{"type":"message_start","message":{"usage":{"input_tokens":1}}}"#,
        ),
        (
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
        ),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ];
    let body = events
        .map(|(e, d)| format!("event: {e}\ndata: {d}\n\n"))
        .concat();
    let (base, raw) = serve(vec![body]).await;
    let (_done, _, _) = run_full(ProviderId::Anthropic, &base, &messages, &[ping_tool()]).await;
    let body = body_of(&raw.await.unwrap());
    // cache_control on the last system block.
    assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    let msgs = body["messages"].as_array().unwrap();
    // assistant: tool_use + thinking native; user: tool_result with is_error.
    let a = &msgs[0];
    assert_eq!(a["role"], "assistant");
    let types: Vec<&str> = a["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["type"].as_str().unwrap())
        .collect();
    assert!(types.contains(&"tool_use"), "{types:?}");
    assert!(types.contains(&"thinking"), "{types:?}");
    let tu = a["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["type"] == "tool_use")
        .unwrap();
    assert_eq!(tu["id"], "toolu_a");
    assert_eq!(tu["input"], serde_json::json!({"x":1}));
    let tr = msgs[1]["content"].as_array().unwrap();
    assert_eq!(tr[0]["type"], "tool_result");
    assert_eq!(tr[0]["tool_use_id"], "toolu_a");
    assert_eq!(tr[0]["is_error"], true);
    // tools use name + input_schema.
    assert_eq!(body["tools"][0]["name"], "ping");
    assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
    // cache_control on the last message's last block.
    let last = msgs.last().unwrap()["content"].as_array().unwrap();
    assert_eq!(last.last().unwrap()["cache_control"]["type"], "ephemeral");
}

fn messages() -> Vec<Message> {
    vec![Message::text(Role::User, "teach me limits")]
}
