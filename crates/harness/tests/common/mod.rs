//! Shared scaffolding for the scripted end-to-end tests: a fake provider
//! speaking the OpenAI chat SSE wire (DeepSeek's endpoint shape) that picks
//! the next canned response by which agent is calling, read from the first
//! line of the system prompt, plus helpers over the op log.
#![allow(dead_code)] // each test binary uses a different subset

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use harness::providers::ProviderId;
use harness::{Harness, ModelChoice, ModelRole, Op};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

// ---------------------------------------------------------------------------
// Scripted fake provider
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub enum Reply {
    Text(&'static str),
    Calls(&'static str, Vec<(&'static str, &'static str, Value)>),
    /// Answer with the inner reply only once the gate has a permit, so a
    /// test can act while the model call (and its turn) is in flight.
    Wait(Arc<Semaphore>, Box<Reply>),
    /// A text reply built from the request body when it arrives, for text
    /// that quotes something only known at run time (e.g. an artifact handle).
    FromRequest(fn(&Value) -> String),
}

#[derive(Default)]
pub struct Script {
    pub queues: Mutex<HashMap<&'static str, VecDeque<Reply>>>,
    /// (agent, request body) in arrival order.
    pub requests: Mutex<Vec<(String, Value)>>,
    pub unexpected: Mutex<Vec<String>>,
}

impl Script {
    pub fn push(&self, agent: &'static str, reply: Reply) {
        self.queues
            .lock()
            .unwrap()
            .entry(agent)
            .or_default()
            .push_back(reply);
    }

    pub fn requests_of(&self, agent: &str) -> Vec<Value> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(a, _)| a == agent)
            .map(|(_, b)| b.clone())
            .collect()
    }
}

pub fn agent_of(body: &Value) -> &'static str {
    let system = body["messages"][0]["content"].as_str().unwrap_or("");
    match system.lines().next().unwrap_or("") {
        "## Voice" => "tutor",
        "## Accuracy" => "planner",
        "# Researcher" => "researcher",
        "# Graph builder" => "graph",
        "Session title" => "title",
        "# Compaction turn" => "compaction",
        _ => "practice",
    }
}

pub fn sse(reply: &Reply) -> String {
    match reply {
        Reply::Text(t) => events(t, &[]),
        Reply::Calls(t, c) => events(t, c),
        Reply::Wait(_, inner) => sse(inner),
        Reply::FromRequest(_) => unreachable!("serve builds FromRequest text itself"),
    }
}

fn events(text: &str, calls: &[(&'static str, &'static str, Value)]) -> String {
    let mut out = String::new();
    let chunk = |delta: Value, finish: &str| {
        format!(
            "data: {}\n\n",
            json!({ "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }] })
        )
    };
    if !text.is_empty() {
        out.push_str(&chunk(json!({ "content": text }), ""));
    }
    for (i, (id, name, args)) in calls.iter().enumerate() {
        out.push_str(&chunk(
            json!({ "tool_calls": [{ "index": i, "id": id, "type": "function",
                "function": { "name": name, "arguments": args.to_string() } }] }),
            "",
        ));
    }
    let finish = if calls.is_empty() {
        "stop"
    } else {
        "tool_calls"
    };
    out.push_str(&chunk(json!({}), finish));
    out.push_str("data: [DONE]\n\n");
    out
}

/// Serve scripted replies; researcher replies wait for a `release` permit so
/// the test controls when the scope brief arrives.
pub async fn serve(script: Arc<Script>, release: Arc<Semaphore>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let script = Arc::clone(&script);
            let release = Arc::clone(&release);
            tokio::spawn(async move {
                let mut req = Vec::new();
                let mut buf = [0u8; 8192];
                let body = loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    req.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&req).into_owned();
                    if let Some(split) = text.find("\r\n\r\n") {
                        let len = text[..split]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if req.len() >= split + 4 + len {
                            break req[split + 4..split + 4 + len].to_vec();
                        }
                    }
                };
                let body: Value = serde_json::from_slice(&body).unwrap();
                let agent = agent_of(&body);
                if agent == "researcher" {
                    release.acquire().await.unwrap().forget();
                }
                script
                    .requests
                    .lock()
                    .unwrap()
                    .push((agent.to_owned(), body.clone()));
                let next = script
                    .queues
                    .lock()
                    .unwrap()
                    .get_mut(agent)
                    .and_then(VecDeque::pop_front);
                let stream = match next {
                    Some(Reply::Wait(gate, inner)) => {
                        gate.acquire().await.unwrap().forget();
                        Some(sse(&inner))
                    }
                    Some(Reply::FromRequest(build)) => Some(events(&build(&body), &[])),
                    Some(reply) => Some(sse(&reply)),
                    None => None,
                };
                let response = match stream {
                    Some(stream) => format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{stream}"
                    ),
                    None => {
                        script.unexpected.lock().unwrap().push(agent.to_owned());
                        "HTTP/1.1 500 Internal Server Error\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}".to_owned()
                    }
                };
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    base
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub fn temp_dirs(name: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("lh-e2e-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    (root.join("repo"), root.join("config"))
}

/// A harness whose only configured role is Tutor (role fallback), pointed at
/// the fake provider.
pub async fn harness(repo: &Path, config: &Path, base: &str) -> Arc<Harness> {
    let h = Arc::new(Harness::open(repo, config).unwrap());
    h.set_base_url(ProviderId::Deepseek, Some(base)).unwrap();
    h.save_api_key(ProviderId::Deepseek, "sk-test-secret-key-0123456789")
        .unwrap();
    h.set_role_model(
        ModelRole::Tutor,
        Some(ModelChoice {
            provider: ProviderId::Deepseek,
            model: "fake-tutor".into(),
            effort: None,
        }),
    )
    .await
    .unwrap();
    h
}

pub fn ops(h: &Harness, id: &str) -> Vec<Op> {
    h.load_session(id).unwrap().ops
}

/// Tool ops (call or result) named `name`, as JSON.
pub fn tool_ops(h: &Harness, id: &str, name: &str) -> Vec<Value> {
    ops(h, id)
        .iter()
        .filter_map(|o| match o {
            Op::Tool(t) if t.name == name => serde_json::to_value(t).ok(),
            _ => None,
        })
        .collect()
}

pub fn results(h: &Harness, id: &str, name: &str) -> Vec<(String, bool)> {
    tool_ops(h, id, name)
        .into_iter()
        .filter_map(|t| {
            let r = t.get("result")?;
            Some((r["content"].as_str()?.to_owned(), r["isError"].as_bool()?))
        })
        .collect()
}

pub fn tool_names(body: &Value) -> Vec<String> {
    body["tools"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|t| t["function"]["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

pub fn answer(text: &str) -> Value {
    json!({ "answer": text, "thinking": "because", "confidence": 70, "dontKnow": false })
}

/// The node every scripted plan teaches.
pub const NODE: &str = "divider-rule";

/// A one-section plan (`s1`, node [`NODE`]) using `plugins`.
pub fn plan_args(plugins: &[&str]) -> Value {
    json!({
        "goal": "Dividers", "approach": "One section.",
        "nodes": [{"id": NODE, "title": "The divider rule", "kind": "mechanism", "requires": []}],
        "sections": [{"id": "s1", "title": "Dividers", "nodeIds": [NODE], "mode": "mechanism",
            "style": "mixed", "plugins": plugins, "notes": "", "exit": "Solves a divider."}]
    })
}

/// Drive a new Learn session to the plan card: goal_set, the scope brief
/// (released here), probe_done, then the queued `planner` replies, which
/// must end in an accepted `plan_write`. Returns the session id.
pub async fn session_awaiting_approval(
    h: &Arc<Harness>,
    s: &Script,
    release: &Semaphore,
    planner: Vec<Reply>,
) -> String {
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "g1",
                "goal_set",
                json!({ "goal": "Voltage dividers", "purpose": "curiosity",
                    "successLooksLike": ["solve a divider"], "materials": [], "outOfScope": [],
                    "preferences": [], "orientationFirst": false }),
            )],
        ),
    );
    s.push("tutor", Reply::Text("Let's begin."));
    s.push("title", Reply::Text("Voltage dividers"));
    let id = h
        .start_teaching("I want to learn voltage dividers")
        .await
        .unwrap()
        .id;
    s.push("researcher", Reply::Calls("", vec![("b1", "brief_submit", json!({
        "kind": "scope", "summary": "Dividers rest on one current through series parts.",
        "strands": [{"name": "series current", "why": "root", "kind": "mechanism", "prerequisites": []}],
        "misconceptions": [], "sources": [] }))]));
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "p1",
                "probe_done",
                json!({ "strands": [{"name": "ohm"}], "notes": "fluent" }),
            )],
        ),
    );
    s.push("tutor", Reply::Text("The plan is being built."));
    for reply in planner {
        s.push("planner", reply);
    }
    release.add_permits(1);
    h.wait_background(&id).await;
    let st = h.session_state(&id).unwrap();
    assert_eq!(
        st.phase,
        harness::state::Phase::AwaitApproval,
        "unexpected: {:?}",
        s.unexpected.lock().unwrap()
    );
    id
}

/// A 12 V source over 3 kΩ / `r2` Ω with a `Vout` probe across `r2`.
pub fn divider(r2: f64) -> Value {
    json!({ "analysis": { "dc": true }, "components": [
        { "id": "V1", "type": "V", "from": [0, 0], "to": [0, 2], "value": 12, "label": "Vin" },
        { "id": "R1", "type": "R", "from": [0, 2], "to": [2, 2], "value": 3000, "label": "R1" },
        { "id": "R2", "type": "R", "from": [2, 2], "to": [2, 0], "value": r2, "label": "R2" },
        { "id": "W0", "type": "wire", "from": [2, 0], "to": [0, 0] },
        { "id": "GND", "type": "ground", "from": [0, 0], "to": [0, 0] },
        { "id": "P1", "type": "probe", "from": [2, 2], "to": [2, 0], "label": "Vout" }
    ]})
}

/// The reference plugin's package directory in this repo.
pub fn circuit_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/circuit")
}

/// Raw text of every op log file of the repo (for "never stored" checks).
pub fn raw_op_logs(repo: &Path) -> String {
    std::fs::read_dir(repo.join("sessions"))
        .unwrap()
        .flat_map(|y| std::fs::read_dir(y.unwrap().path()).unwrap())
        .map(|f| std::fs::read_to_string(f.unwrap().path()).unwrap())
        .collect()
}

/// The `content` of the tool message answering `call_id` in a chat request.
pub fn tool_message(body: &Value, call_id: &str) -> Option<String> {
    body["messages"].as_array()?.iter().find_map(|m| {
        (m["role"] == "tool" && m["tool_call_id"] == call_id)
            .then(|| m["content"].as_str().unwrap_or_default().to_owned())
    })
}
