//! M8 end-to-end proof: a scripted fake provider drives one whole teaching
//! session through the real runtime (commands, phase machine, gates, jobs,
//! delivery, planner, plugins) and the test asserts on the op log and the
//! derived `SessionState` at each step.
//!
//! The fake provider is a local HTTP server speaking the OpenAI chat SSE wire
//! (DeepSeek's endpoint shape). It picks the next canned response by which
//! agent is calling, read from the first line of the system prompt.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use harness::plugins::{Plugin, PluginContext, ToolFuture, ToolOutcome};
use harness::providers::{ProviderId, ToolSpec};
use harness::state::Phase;
use harness::{AgentEvent, Harness, Mode, ModelChoice, ModelRole, Op, PluginManifest};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

// ---------------------------------------------------------------------------
// Scripted fake provider
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum Reply {
    Text(&'static str),
    Calls(&'static str, Vec<(&'static str, &'static str, Value)>),
}

#[derive(Default)]
struct Script {
    queues: Mutex<HashMap<&'static str, VecDeque<Reply>>>,
    /// (agent, request body) in arrival order.
    requests: Mutex<Vec<(String, Value)>>,
    unexpected: Mutex<Vec<String>>,
}

impl Script {
    fn push(&self, agent: &'static str, reply: Reply) {
        self.queues
            .lock()
            .unwrap()
            .entry(agent)
            .or_default()
            .push_back(reply);
    }

    fn requests_of(&self, agent: &str) -> Vec<Value> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(a, _)| a == agent)
            .map(|(_, b)| b.clone())
            .collect()
    }
}

fn agent_of(body: &Value) -> &'static str {
    let system = body["messages"][0]["content"].as_str().unwrap_or("");
    match system.lines().next().unwrap_or("") {
        "## Voice" => "tutor",
        "## Accuracy" => "planner",
        "# Researcher" => "researcher",
        "Session title" => "title",
        "# Compaction turn" => "compaction",
        _ => "practice",
    }
}

fn sse(reply: &Reply) -> String {
    let mut out = String::new();
    let chunk = |delta: Value, finish: &str| {
        format!(
            "data: {}\n\n",
            json!({ "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }] })
        )
    };
    let (text, calls) = match reply {
        Reply::Text(t) => (*t, Vec::new()),
        Reply::Calls(t, c) => (*t, c.clone()),
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
async fn serve(script: Arc<Script>, release: Arc<Semaphore>) -> String {
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
                    .push((agent.to_owned(), body));
                let next = script
                    .queues
                    .lock()
                    .unwrap()
                    .get_mut(agent)
                    .and_then(VecDeque::pop_front);
                let response = match next {
                    Some(reply) => format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{}",
                        sse(&reply)
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
// Test-only plugins
// ---------------------------------------------------------------------------

struct EchoPlugin {
    manifest: PluginManifest,
    tool: String,
}

impl EchoPlugin {
    fn plugin(id: &str) -> Arc<dyn Plugin> {
        Arc::new(Self {
            manifest: PluginManifest {
                id: id.into(),
                title: format!("Echo ({id})"),
                tools: vec![id.into()],
                platforms: vec!["desktop".into(), "mobile".into()],
                phases: vec!["teach".into()],
                use_when: "A test needs a plugin.".into(),
                avoid_when: "Real lessons.".into(),
                cost: "none".into(),
            },
            tool: id.into(),
        })
    }
}

impl Plugin for EchoPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn tools(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: self.tool.clone(),
            description: "Echo the text back.".into(),
            input_schema: json!({ "type": "object", "properties": { "text": { "type": "string" } } }),
        }]
    }

    fn fragment(&self) -> String {
        format!("Plugin {}: call `{}` to echo.", self.manifest.id, self.tool)
    }

    fn call(&self, _name: &str, args: &Value, _ctx: PluginContext) -> ToolFuture {
        let text = args
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        Box::pin(async move {
            ToolOutcome {
                content: format!("echo: {text}"),
                is_error: false,
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn temp_dirs(name: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("lh-e2e-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    (root.join("repo"), root.join("config"))
}

/// A harness whose only configured role is Tutor (role fallback), pointed at
/// the fake provider.
async fn harness(repo: &Path, config: &Path, base: &str) -> Arc<Harness> {
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

fn ops(h: &Harness, id: &str) -> Vec<Op> {
    h.load_session(id).unwrap().ops
}

/// Tool ops (call or result) named `name`, as JSON.
fn tool_ops(h: &Harness, id: &str, name: &str) -> Vec<Value> {
    ops(h, id)
        .iter()
        .filter_map(|o| match o {
            Op::Tool(t) if t.name == name => serde_json::to_value(t).ok(),
            _ => None,
        })
        .collect()
}

fn results(h: &Harness, id: &str, name: &str) -> Vec<(String, bool)> {
    tool_ops(h, id, name)
        .into_iter()
        .filter_map(|t| {
            let r = t.get("result")?;
            Some((r["content"].as_str()?.to_owned(), r["isError"].as_bool()?))
        })
        .collect()
}

fn tool_names(body: &Value) -> Vec<String> {
    body["tools"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|t| t["function"]["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn answer(text: &str) -> Value {
    json!({ "answer": text, "thinking": "because", "confidence": 70, "dontKnow": false })
}

// ---------------------------------------------------------------------------
// The full session
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scripted_session_runs_end_to_end() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("full");
    let h = harness(&repo, &config, &base).await;
    for id in ["echo", "echo2", "echo3", "echo4"] {
        h.register_plugin(EchoPlugin::plugin(id)).unwrap();
    }
    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::default();
    let sink_events = Arc::clone(&events);
    h.set_event_sink(Some(Arc::new(move |_, ev| {
        sink_events.lock().unwrap().push(ev)
    })));

    let s = &*script;
    // 1. Goal → goal_set, then the first probe question.
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "g1",
                "goal_set",
                json!({
        "goal": "Find unknown resistors in voltage dividers", "purpose": "curiosity",
        "successLooksLike": ["solve a divider for R1"], "materials": [], "outOfScope": [],
        "preferences": [], "orientationFirst": false }),
            )],
        ),
    );
    s.push("tutor", Reply::Calls("Some questions will go past what you know.", vec![("q1", "question_ask", json!({
        "role": "probe", "strand": "ohm", "format": "choice", "prompt": "Two equal resistors across 12 V: voltage on one?",
        "options": ["3 V", "6 V", "12 V"], "answer": "6 V", "explanation": "Equal split." }))]));
    s.push("title", Reply::Text("\"Voltage dividers.\""));

    let summary = h
        .start_teaching("I want to learn voltage dividers")
        .await
        .unwrap();
    let id = summary.id.clone();
    let st = h.session_state(&id).unwrap();
    assert_eq!(st.phase, Phase::Measure);
    assert!(st.goal.is_some());
    assert_eq!(st.pending_call.as_ref().unwrap().call_id, "q1");
    assert_eq!(st.open_jobs.len(), 1, "the scope job is queued/running");
    assert_eq!(st.open_jobs[0].kind, "research");

    // 3. Two probe questions answered via `answer` (the first is graded by code).
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "q2",
                "question_ask",
                json!({
        "role": "probe", "strand": "divider", "format": "free",
        "prompt": "What sets the ratio of the two voltages?", "answer": "The resistance ratio" }),
            )],
        ),
    );
    h.answer(&id, "q1", answer("6 V")).await.unwrap();
    let q1 = results(&h, &id, "question_ask");
    let graded: Value = serde_json::from_str(&q1[0].0).unwrap();
    assert_eq!(graded["verdict"], "correct");
    assert_eq!(graded["questionId"], "q1");

    // 4. probe_done refused before the brief.
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "p1",
                "probe_done",
                json!({ "strands": [{"name":"ohm"}], "notes": "fluent" }),
            )],
        ),
    );
    s.push("tutor", Reply::Text("I'm waiting for the field brief."));
    h.answer(&id, "q2", answer("The resistances"))
        .await
        .unwrap();
    let refused = results(&h, &id, "probe_done");
    assert!(
        refused[0].1 && refused[0].0.contains("brief"),
        "{refused:?}"
    );
    assert_eq!(h.session_state(&id).unwrap().phase, Phase::Measure);

    // 2. The scope job (brief_submit) arrives; it wakes the idle tutor, which
    // now gets probe_done accepted and hands over to the planner.
    s.push("researcher", Reply::Calls("", vec![("b1", "brief_submit", json!({
        "kind": "scope", "summary": "Dividers rest on one current through series parts.",
        "strands": [{"name": "series current", "why": "root", "kind": "mechanism", "prerequisites": []}],
        "misconceptions": [], "sources": [] }))]));
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "p2",
                "probe_done",
                json!({ "strands": [{"name":"ohm","floor":"V=IR"}], "notes": "fluent" }),
            )],
        ),
    );
    s.push("tutor", Reply::Text("The plan is being built."));
    // 5. The planner: one invalid plan (a requires cycle) rejected, then a valid one.
    let nodes = json!([
        {"id": "series-current", "title": "One path, one current", "kind": "mechanism", "requires": []},
        {"id": "divider-rule", "title": "The divider rule", "kind": "mechanism", "requires": ["series-current"]}
    ]);
    let mut cyclic = nodes.clone();
    cyclic[0]["requires"] = json!(["divider-rule"]);
    let sections = json!([
        {"id": "s1", "title": "Series current", "nodeIds": ["series-current"], "mode": "mechanism",
         "style": "socratic", "plugins": ["echo", "echo2", "echo3"], "notes": "Root first.", "exit": "Explains one current."},
        {"id": "s2", "title": "Dividers", "nodeIds": ["divider-rule"], "mode": "mechanism",
         "style": "mixed", "plugins": [], "notes": "", "exit": "Solves for R1."}
    ]);
    s.push("planner", Reply::Calls("", vec![("w1", "plan_write", json!({"goal": "Dividers", "approach": "Bottom up.", "nodes": cyclic, "sections": sections}))]));
    s.push("planner", Reply::Calls("", vec![("w2", "plan_write", json!({"goal": "Dividers", "approach": "Bottom up.", "nodes": nodes, "sections": sections}))]));
    release.add_permits(1);
    h.wait_background(&id).await;

    let st = h.session_state(&id).unwrap();
    assert!(st.brief.is_some());
    assert_eq!(st.phase, Phase::AwaitApproval);
    assert_eq!(st.plan_version, 1, "only the valid plan was persisted");
    let writes = results(&h, &id, "plan_write");
    assert!(writes[0].1 && writes[0].0.contains("cycle"), "{writes:?}");
    assert!(!writes[1].1);
    let probe = results(&h, &id, "probe_done");
    assert!(!probe[1].1, "accepted after the brief: {probe:?}");
    assert!(ops(&h, &id)
        .iter()
        .any(|o| matches!(o, Op::Message(m) if m.tag.as_deref() == Some("research-brief"))));
    assert_eq!(
        h.load_session(&id).unwrap().summary.title,
        "Voltage dividers"
    );
    assert_eq!(st.title.as_deref(), Some("Voltage dividers"));

    // 6. Approve → teach section 1; 7. predict (after one out-of-section
    // question refused), then section_done refused, check, verdict_record.
    s.push("tutor", Reply::Calls("", vec![("x1", "question_ask", json!({
        "role": "check", "nodeId": "divider-rule", "format": "free", "prompt": "?", "answer": "a" }))]));
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "q3",
                "question_ask",
                json!({
        "role": "predict", "nodeId": "series-current", "format": "free",
        "prompt": "Does current get used up along a wire?", "answer": "No" }),
            )],
        ),
    );
    h.approve(&id, vec![]).await.unwrap();
    let st = h.session_state(&id).unwrap();
    assert_eq!(st.phase, Phase::Teach);
    assert_eq!(st.current_section.as_deref(), Some("s1"));
    assert_eq!(st.active_plugins, ["echo", "echo2", "echo3"]);
    assert_eq!(st.last_node_id.as_deref(), Some("series-current"));
    let out_of_section = results(&h, &id, "question_ask");
    assert!(out_of_section
        .iter()
        .any(|(c, e)| *e && c.contains("not in the current section")));

    // 8. section_done refused, then a check question.
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "d1",
                "section_done",
                json!({"sectionId": "s1", "evidence": "-", "carryOver": "-"}),
            )],
        ),
    );
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "q4",
                "question_ask",
                json!({
        "role": "check", "nodeId": "series-current", "format": "free",
        "prompt": "Current into R1 is 2 mA. Current out of R2?", "answer": "2 mA" }),
            )],
        ),
    );
    h.answer(&id, "q3", answer("No, it stays the same"))
        .await
        .unwrap();
    let done = results(&h, &id, "section_done");
    assert!(
        done[0].1 && done[0].0.contains("series-current"),
        "{done:?}"
    );

    // verdict_record, the echo plugin round trip, plugin limits, section_done
    // accepted, then (9.) a plan_change_request.
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "v1",
                "verdict_record",
                json!({"questionId": "q4", "verdict": "correct", "note": "exact"}),
            )],
        ),
    );
    s.push(
        "tutor",
        Reply::Calls("", vec![("e1", "echo", json!({"text": "hi"}))]),
    );
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![
                (
                    "r1",
                    "plugin_request",
                    json!({"plugin": "echo4", "reason": "more"}),
                ),
                (
                    "r2",
                    "plugin_request",
                    json!({"plugin": "nonexistent", "reason": "?"}),
                ),
            ],
        ),
    );
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "d2",
                "section_done",
                json!({"sectionId": "s1", "evidence": "q4 correct", "carryOver": "says 'used up'"}),
            )],
        ),
    );
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "c1",
                "plan_change_request",
                json!({"reason": "ahead", "evidence": "q4 fast and exact"}),
            )],
        ),
    );
    s.push(
        "tutor",
        Reply::Text("I'm checking the plan with the planner."),
    );
    s.push(
        "planner",
        Reply::Calls(
            "",
            vec![(
                "k1",
                "plan_keep",
                json!({"reason": "One fast answer is not enough evidence."}),
            )],
        ),
    );
    // The plan_keep notice wakes the tutor: 10. session_end.
    s.push(
        "tutor",
        Reply::Calls("Done for today.", vec![("end", "session_end", json!({}))]),
    );
    h.answer(&id, "q4", answer("2 mA")).await.unwrap();
    h.wait_background(&id).await;

    assert_eq!(
        results(&h, &id, "verdict_record"),
        [("Verdict recorded.".to_owned(), false)]
    );
    assert_eq!(results(&h, &id, "echo"), [("echo: hi".to_owned(), false)]);
    let grants = results(&h, &id, "plugin_request");
    assert!(grants.iter().all(|(_, e)| *e), "{grants:?}");
    assert!(grants[0].0.contains("limit"));
    assert!(grants[1].0.contains("unknown"));
    let done = results(&h, &id, "section_done");
    assert!(!done[1].1, "{done:?}");
    let st = h.session_state(&id).unwrap();
    assert!(st.node_passed("series-current"));
    assert_eq!(st.completed_sections, ["s1"]);
    assert!(ops(&h, &id).iter().any(|o| matches!(o, Op::PlanKeep(_))));
    assert!(ops(&h, &id).iter().any(|o| matches!(o, Op::Message(m)
        if m.tag.as_deref() == Some("plan-notice") && m.content.starts_with("[plan update] Plan kept"))));
    assert!(st.ended);
    assert_eq!(st.phase, Phase::Close);
    assert_eq!(st.plan_version, 1);

    // Requests: every agent fell back to the Tutor model (only Tutor is set);
    // the tutor never had plan_write; echo only while section s1 was active;
    // nothing was requested that the script didn't expect.
    let all = script.requests.lock().unwrap().clone();
    assert!(all.iter().all(|(_, b)| b["model"] == "fake-tutor"));
    for agent in ["planner", "researcher", "title"] {
        assert!(all.iter().any(|(a, _)| a == agent), "no {agent} request");
    }
    let tutor = script.requests_of("tutor");
    assert!(tutor
        .iter()
        .all(|b| !tool_names(b).contains(&"plan_write".to_owned())));
    let with_echo = tutor
        .iter()
        .filter(|b| tool_names(b).contains(&"echo".to_owned()))
        .count();
    assert!(with_echo >= 3, "echo offered in section s1");
    let last = tutor.last().unwrap();
    assert!(
        !tool_names(last).contains(&"echo".to_owned()),
        "echo gone in s2"
    );
    assert!(tool_names(&tutor[0]).contains(&"goal_set".to_owned()));
    assert!(!tool_names(&tutor[0]).contains(&"section_done".to_owned()));
    let planner = script.requests_of("planner");
    assert!(tool_names(&planner[0]).contains(&"plan_write".to_owned()));
    assert!(
        planner[0]["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("echo"),
        "catalog lists echo"
    );
    assert!(
        script.unexpected.lock().unwrap().is_empty(),
        "unexpected: {:?}",
        script.unexpected.lock().unwrap()
    );
    // Secrets never reach the op log.
    let raw = std::fs::read_dir(repo.join("sessions"))
        .unwrap()
        .flat_map(|y| std::fs::read_dir(y.unwrap().path()).unwrap())
        .map(|f| std::fs::read_to_string(f.unwrap().path()).unwrap())
        .collect::<String>();
    assert!(!raw.contains("sk-test-secret"));
    // The UI saw the planner run and streamed tutor text.
    let events = events.lock().unwrap();
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::TurnStart {
            agent: harness::AgentName::Planner
        }
    )));
    assert!(events.iter().any(|e| matches!(e, AgentEvent::Text { .. })));
}

// ---------------------------------------------------------------------------
// Restart with a question pending
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pending_question_survives_restart_and_resumes() {
    let script = Arc::new(Script::default());
    let base = serve(Arc::clone(&script), Arc::new(Semaphore::new(0))).await;
    let (repo, config) = temp_dirs("restart");
    let id = {
        let h = harness(&repo, &config, &base).await;
        script.push("tutor", Reply::Calls("Before we start:", vec![("c1", "choice_ask", json!({
            "question": "Which is closest?", "options": ["DC circuits", "An exam"], "recommended": "DC circuits" }))]));
        h.start_teaching("circuits").await.unwrap().id
    };
    // A new process: nothing in memory, the pending call comes from the ops.
    let h = Arc::new(Harness::open(&repo, &config).unwrap());
    let st = h.session_state(&id).unwrap();
    assert_eq!(st.pending_call.as_ref().unwrap().name, "choice_ask");
    assert!(
        h.send_learner_message(&id, "hello").await.is_err(),
        "answer first"
    );
    script.push("tutor", Reply::Text("Good: DC circuits."));
    h.answer(&id, "c1", json!({ "answer": "DC circuits" }))
        .await
        .unwrap();
    assert!(h.session_state(&id).unwrap().pending_call.is_none());
    // The resumed request replays the call and its result in wire order.
    let last = script.requests_of("tutor").pop().unwrap();
    let msgs = last["messages"].as_array().unwrap();
    let call_at = msgs
        .iter()
        .position(|m| m["tool_calls"][0]["id"] == "c1")
        .unwrap();
    assert_eq!(msgs[call_at + 1]["role"], "tool");
    assert_eq!(msgs[call_at + 1]["tool_call_id"], "c1");
    assert!(msgs[call_at + 1]["content"]
        .as_str()
        .unwrap()
        .contains("DC circuits"));
}

// ---------------------------------------------------------------------------
// Legacy sessions and Practice
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_learn_sessions_are_read_only_and_practice_still_works() {
    let script = Arc::new(Script::default());
    let base = serve(Arc::clone(&script), Arc::new(Semaphore::new(0))).await;
    let (repo, config) = temp_dirs("legacy");
    let h = harness(&repo, &config, &base).await;

    // A session written by the old single-prompt Learn path: no phase op.
    let id = ulid_like();
    let year = chrono_year(&id);
    let dir = repo.join("sessions").join(year);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{id}.jsonl")),
        format!(
            "{}\n{}\n",
            json!({"op":"session","opId":"01J00000000000000000000000","at":"2026-01-01T00:00:00.000Z","id":id,"mode":"learn","title":"old"}),
            json!({"op":"message","opId":"01J00000000000000000000001","at":"2026-01-01T00:00:01.000Z","role":"user","content":"hi"}),
        ),
    )
    .unwrap();
    let session = h.load_session(&id).unwrap();
    assert!(session.summary.read_only);
    let err = h
        .send_learner_message(&id, "again")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("read-only"), "{err}");
    assert!(h
        .list_sessions()
        .unwrap()
        .iter()
        .any(|s| s.id == id && s.read_only));

    // Practice keeps its single-prompt path and is never read-only.
    script.push("practice", Reply::Text("Here is a problem."));
    let mut seen = Vec::new();
    let reply = h
        .send(None, Mode::Practice, "give me a problem", &mut |e| {
            seen.push(serde_json::to_string(&e).unwrap())
        })
        .await
        .unwrap();
    assert_eq!(reply.content, "Here is a problem.");
    let practice = h
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|s| s.mode == Mode::Practice)
        .unwrap();
    assert!(!practice.read_only);
    // Learn turns never go through the Practice path.
    assert!(h.send(None, Mode::Learn, "x", &mut |_| {}).await.is_err());
}

// ---------------------------------------------------------------------------
// Compaction near the context limit
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_conversation_is_compacted_before_the_next_turn() {
    let script = Arc::new(Script::default());
    let base = serve(Arc::clone(&script), Arc::new(Semaphore::new(0))).await;
    let (repo, config) = temp_dirs("compact");
    let h = harness(&repo, &config, &base).await;
    // The unknown model gets the 100k-token default window; 70% is the limit.
    let old = format!("OLDMARK {}", "a".repeat(300_000));
    script.push("tutor", Reply::Text("Noted."));
    let id = h.start_teaching(&old).await.unwrap().id;
    assert!(
        !ops(&h, &id).iter().any(|o| matches!(o, Op::Compaction(_))),
        "nothing older to summarize yet"
    );

    script.push(
        "compaction",
        Reply::Text("## Session state\nThe learner pasted notes."),
    );
    script.push("tutor", Reply::Text("Continuing from the notes."));
    let recent = format!("RECENTMARK {}", "b".repeat(100_000));
    h.send_learner_message(&id, &recent).await.unwrap();

    let compaction = ops(&h, &id).into_iter().find_map(|o| match o {
        Op::Compaction(c) => Some(c),
        _ => None,
    });
    let compaction = compaction.expect("a compaction op");
    assert!(compaction.summary.contains("pasted notes"));
    let summarized = script.requests_of("compaction").pop().unwrap();
    assert!(summarized.to_string().contains("OLDMARK"));
    // The next tutor request uses the notes plus the recent turns only.
    let last = script.requests_of("tutor").pop().unwrap().to_string();
    assert!(!last.contains("OLDMARK"));
    assert!(last.contains("RECENTMARK") && last.contains("pasted notes"));
}

/// A valid ULID string for a hand-written legacy file.
fn ulid_like() -> String {
    // 2026-01-01T00:00:00Z in ms, Crockford base32, plus fixed randomness.
    let ms: u64 = 1_767_225_600_000;
    let alphabet = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut time = String::new();
    let mut v = ms;
    for _ in 0..10 {
        time.insert(0, alphabet[(v % 32) as usize] as char);
        v /= 32;
    }
    format!("{time}0000000000000000")
}

fn chrono_year(_id: &str) -> String {
    "2026".into()
}
