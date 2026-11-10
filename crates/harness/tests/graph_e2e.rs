//! End-to-end proof of the graph core (architecture §4.2–§4.7): a session is
//! closed, the graph builder runs on the scripted provider, a refused
//! `graph_submit` is fixed in the next call, the ops land in
//! `graph/ops.jsonl`, and `knowledge_map` reads them back — with the builder's
//! text never entering the conversation.

mod common;

use std::sync::Arc;

use common::*;
use harness::state::Phase;
use harness::Op;
use serde_json::{json, Value};
use tokio::sync::Semaphore;

/// The graph the builder submits in the test: two claims, one prerequisite,
/// one contrast (which must never order anything) and one chain.
fn submission() -> Value {
    json!({
        "topics": [{"topicId": "circuits", "title": "Circuits", "parent": null}],
        "nodes": [
            {"nodeId": "series-current", "title": "Series current",
             "statement": "In a series circuit the same current flows through every element.",
             "kind": "mechanism", "home": "circuits"},
            {"nodeId": "divider-rule", "title": "The divider rule",
             "statement": "The output of an unloaded two-resistor divider is V_in R2/(R1+R2).",
             "kind": "mechanism", "home": "circuits"}
        ],
        "edges": [
            {"from": "series-current", "to": "divider-rule", "relation": "requires",
             "weight": "load-bearing", "why": "the rule follows from one shared current"},
            {"from": "divider-rule", "to": "series-current", "relation": "contrasts",
             "why": "easily read as two separate currents"}
        ],
        "chains": [{
            "chainId": "divider",
            "premises": ["series-current"],
            "conclusion": "divider-rule",
            "statement": "One path means one current, so the shares follow the resistances."
        }]
    })
}

fn graph_jobs(h: &Arc<harness::Harness>, id: &str) -> Vec<(String, Option<Value>)> {
    ops(h, id)
        .into_iter()
        .filter_map(|o| match o {
            Op::Job(j) if j.kind == "graph" => Some((j.state, j.result)),
            _ => None,
        })
        .collect()
}

/// A tutor reply of about `tokens` tutor tokens (characters / 4), opening
/// with `marker` so a builder's transcript can be checked for it.
fn teaching(marker: &str, tokens: usize) -> &'static str {
    let mut text = format!("{marker}. ");
    while text.len() < tokens * 4 {
        text.push_str("In a series circuit every element carries the same current. ");
    }
    text.leak()
}

/// The `reason` of each queued graph job, in order.
fn graph_reasons(h: &Arc<harness::Harness>, id: &str) -> Vec<String> {
    ops(h, id)
        .into_iter()
        .filter_map(|o| match o {
            Op::Job(j) if j.kind == "graph" && j.state == "queued" => {
                Some(j.args?["reason"].as_str()?.to_owned())
            }
            _ => None,
        })
        .collect()
}

/// The message list of a builder request's system prompt.
fn builder_messages(request: &Value) -> String {
    let system = request["messages"][0]["content"].as_str().unwrap();
    let start = system.find("The session's messages").unwrap();
    let end = system.find("## Tools").unwrap();
    system[start..end].to_owned()
}

/// Sessions run over days: the graph is built from the teaching as it
/// accumulates, each build reads only what the last one didn't, and a manual
/// build needs a quarter of what starts one automatically.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn teaching_is_extracted_in_parts_as_the_session_goes() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("graph-parts");
    let h = harness(&repo, &config, &base).await;
    let id = session_awaiting_approval(
        &h,
        &script,
        &release,
        vec![Reply::Calls("", vec![("w1", "plan_write", plan_args(&[]))])],
    )
    .await;
    assert!(
        graph_reasons(&h, &id).is_empty(),
        "short turns build nothing"
    );

    // One long teaching turn crosses the automatic threshold.
    let first_part = teaching("FIRST PART", 4200);
    script.push("tutor", Reply::Text(first_part));
    script.push(
        "graph",
        Reply::Calls("", vec![("s1", "graph_submit", submission())]),
    );
    h.approve(&id, vec![]).await.unwrap();
    h.wait_background(&id).await;
    assert_eq!(graph_reasons(&h, &id), vec!["turn"]);
    let first = builder_messages(&script.requests_of("graph")[0]);
    assert!(first.contains("from the start of the session"), "{first}");
    assert!(
        first.contains(&format!(
            "Tutor — {} characters",
            first_part.chars().count()
        )),
        "listed whole, not clipped: {first}"
    );
    assert!(
        !first.contains("FIRST PART"),
        "listed, not inlined: {first}"
    );
    assert_eq!(h.knowledge_map().unwrap().nodes.len(), 2);

    // Nothing new: a manual build is refused and says what it needs.
    let refused = h.graph_build(&id).unwrap_err().to_string();
    assert!(refused.contains("1024 needed"), "{refused}");

    // A shorter stretch stays below the automatic threshold but is enough by
    // hand.
    script.push("tutor", Reply::Text(teaching("SECOND PART", 1500)));
    h.send_learner_message(&id, "Go on.").await.unwrap();
    h.wait_background(&id).await;
    assert_eq!(graph_reasons(&h, &id), vec!["turn"], "below the threshold");
    let status = h.graph_status(&id).unwrap();
    assert!(
        status.uncovered_tokens >= 1500 && !status.running,
        "{status:?}"
    );

    // The builder reads a message by number; restating the same claims
    // upserts them: no copies.
    script.push(
        "graph",
        Reply::Calls(
            "",
            vec![
                ("r1", "message_read", json!({"from": 1})),
                ("r2", "message_read", json!({"index": 3})),
            ],
        ),
    );
    script.push(
        "graph",
        Reply::Calls(
            "",
            vec![(
                "s2",
                "graph_submit",
                json!({"nodes": [
                    {"nodeId": "divider-rule", "title": "The divider rule",
                     "statement": "The output of an unloaded two-resistor divider is V_in R2/(R1+R2).",
                     "kind": "mechanism", "home": "circuits"}
                ]}),
            )],
        ),
    );
    h.graph_build(&id).unwrap();
    h.wait_background(&id).await;
    assert_eq!(graph_reasons(&h, &id), vec!["turn", "manual"]);
    let requests = script.requests_of("graph");
    let second = builder_messages(&requests[1]);
    assert!(
        second.contains("earlier runs already extracted"),
        "{second}"
    );
    assert!(
        second.contains("1. Learner — 6 characters") && second.contains("2. Tutor — "),
        "only the new part: {second}"
    );
    assert!(!second.contains("3. "), "only the new part: {second}");
    let read = tool_message(&requests[2], "r1").unwrap();
    assert!(
        read.starts_with("1. Learner:\nGo on.\n\n2. Tutor:\nSECOND PART"),
        "the whole list in one read: {read}"
    );
    let out_of_range = tool_message(&requests[2], "r2").unwrap();
    assert!(out_of_range.contains("1 to 2"), "{out_of_range}");
    // The log records which messages were read, not a second copy of them.
    let logged = results(&h, &id, "message_read");
    assert!(
        logged.iter().any(|(text, _)| text == "read messages 1–2"),
        "{logged:?}"
    );
    assert_eq!(h.knowledge_map().unwrap().nodes.len(), 2, "the same ids");
    assert_eq!(h.graph_status(&id).unwrap().uncovered_tokens, 0);
    assert!(
        script.unexpected.lock().unwrap().is_empty(),
        "{:?}",
        script.unexpected.lock().unwrap()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn close_builds_the_graph_refuses_once_then_lands_ops() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("graph-core");
    let h = harness(&repo, &config, &base).await;

    let id = session_awaiting_approval(
        &h,
        &script,
        &release,
        vec![Reply::Calls("", vec![("w1", "plan_write", plan_args(&[]))])],
    )
    .await;

    // The approve turn closes the session; `session_end` starts the builder.
    script.push(
        "tutor",
        Reply::Calls(
            "That's the plan done for today.",
            vec![("end", "session_end", json!({}))],
        ),
    );
    // First submission: an edge to a claim that doesn't exist — refused.
    script.push(
        "graph",
        Reply::Calls(
            "",
            vec![(
                "s1",
                "graph_submit",
                json!({
                    "topics": [{"topicId": "circuits", "title": "Circuits", "parent": null}],
                    "nodes": [{"nodeId": "series-current", "statement": "same current",
                               "kind": "mechanism", "home": "circuits"}],
                    "edges": [{"from": "series-current", "to": "ghost"}]
                }),
            )],
        ),
    );
    // Second: fixed, and accepted.
    script.push(
        "graph",
        Reply::Calls("", vec![("s2", "graph_submit", submission())]),
    );

    h.approve(&id, vec![]).await.unwrap();
    h.wait_background(&id).await;

    // The refused submit came back with the problem, and the next one passed.
    let submits = results(&h, &id, "graph_submit");
    assert_eq!(submits.len(), 2, "{submits:?}");
    assert!(submits[0].1, "the first submission is refused");
    assert!(
        submits[0].0.contains("unknown node `ghost`"),
        "{}",
        submits[0].0
    );
    assert!(!submits[1].1, "the fixed submission is accepted");
    assert!(submits[1].0.contains("2 node(s)"), "{}", submits[1].0);

    // The job ends `done`, with the counts it appended (after queued/running).
    let jobs = graph_jobs(&h, &id);
    let end: Vec<&(String, Option<Value>)> = jobs
        .iter()
        .filter(|(state, _)| state == "done" || state == "failed" || state == "aborted")
        .collect();
    assert_eq!(end.len(), 1, "{jobs:?}");
    assert_eq!(end[0].0, "done", "{jobs:?}");
    assert_eq!(end[0].1.as_ref().unwrap()["nodes"], 2);

    // Ops on disk, with the builder's provenance and the session's id.
    let raw = std::fs::read_to_string(repo.join("graph/ops.jsonl")).unwrap();
    assert!(raw.contains(r#""op":"node""#), "{raw}");
    assert!(raw.contains(r#""type":"builder""#), "{raw}");
    assert!(raw.contains(&format!(r#""sessionId":"{id}""#)), "{raw}");
    assert!(raw.contains(r#""op":"chain""#), "{raw}");

    // The read model: both nodes, both edges, the chain, and only the
    // prerequisite in the dependency view.
    let map = h.knowledge_map().unwrap();
    assert_eq!(map.topics.len(), 1);
    assert_eq!(map.nodes.len(), 2);
    assert_eq!(map.edges.len(), 2);
    assert_eq!(map.chains.len(), 1);
    assert!(
        map.nodes
            .iter()
            .all(|n| n.provenance.as_ref().unwrap()["type"] == "builder"),
        "every node carries its provenance"
    );
    let deps: Vec<(&str, &str)> = map
        .prerequisites()
        .map(|e| (e.from.as_str(), e.to.as_str()))
        .collect();
    assert_eq!(deps, vec![("series-current", "divider-rule")]);
    assert!(
        !deps
            .iter()
            .any(|(a, b)| (*a, *b) == ("divider-rule", "series-current")),
        "the contrast never orders anything"
    );

    // Nothing was delivered into the conversation, and the tutor was not woken.
    let st = h.session_state(&id).unwrap();
    assert!(st.queued_jobs.is_empty(), "{:?}", st.queued_jobs);
    assert!(st.ended);
    assert_eq!(st.phase, Phase::Close);
    let tutor_requests = script.requests_of("tutor").len();
    assert!(
        !ops(&h, &id).iter().any(|o| matches!(o, Op::Message(m)
            if m.content.contains("Submitted:") || m.content.contains("graph"))),
        "the builder's text stays out of the conversation"
    );

    // The builder saw the graph, the plan and the transcript.
    let graph_requests = script.requests_of("graph");
    assert_eq!(graph_requests.len(), 2);
    let system = graph_requests[0]["messages"][0]["content"]
        .as_str()
        .unwrap();
    assert!(system.starts_with("# Graph builder"), "{system}");
    assert!(system.contains("Nodes (id [home] title)"), "{system}");
    assert!(
        system.contains(r#""goal": "Dividers""#),
        "the plan: {system}"
    );
    assert!(
        system.contains("1. Learner — "),
        "the message list: {system}"
    );
    let offered = tool_names(&graph_requests[0]);
    assert_eq!(
        offered,
        vec!["message_read", "graph_search", "graph_submit"]
    );

    // The close read everything: a manual build has nothing left to read.
    let refused = h.graph_build(&id).unwrap_err().to_string();
    assert!(refused.contains("not enough new teaching"), "{refused}");
    assert_eq!(h.graph_status(&id).unwrap().uncovered_tokens, 0);
    assert_eq!(script.requests_of("graph").len(), 2);

    // Reaching the model again would mean something woke the tutor.
    assert_eq!(script.requests_of("tutor").len(), tutor_requests);
    assert!(
        script.unexpected.lock().unwrap().is_empty(),
        "{:?}",
        script.unexpected.lock().unwrap()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graph_build_refuses_while_one_is_open_for_the_session() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("graph-busy");
    let h = harness(&repo, &config, &base).await;
    let id = session_awaiting_approval(
        &h,
        &script,
        &release,
        vec![Reply::Calls("", vec![("w1", "plan_write", plan_args(&[]))])],
    )
    .await;
    // Enough teaching for a manual build, below the automatic threshold.
    script.push("tutor", Reply::Text(teaching("TEACHING", 1500)));
    h.approve(&id, vec![]).await.unwrap();
    h.wait_background(&id).await;

    // Hold the builder's model call so the job stays open.
    let gate = Arc::new(Semaphore::new(0));
    script.push(
        "graph",
        Reply::Wait(
            Arc::clone(&gate),
            Box::new(Reply::Calls("", vec![("g1", "graph_submit", json!({}))])),
        ),
    );
    h.graph_build(&id).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while script.requests_of("graph").is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the builder reached the model");

    let refused = h.graph_build(&id).unwrap_err().to_string();
    assert!(refused.contains("already running"), "{refused}");

    gate.add_permits(1);
    h.wait_background(&id).await;
    assert_eq!(graph_jobs(&h, &id).last().unwrap().0, "done");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_graph_run_is_recorded_and_blocks_nothing() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("graph-fail");
    let h = harness(&repo, &config, &base).await;
    let id = session_awaiting_approval(
        &h,
        &script,
        &release,
        vec![Reply::Calls("", vec![("w1", "plan_write", plan_args(&[]))])],
    )
    .await;

    // Closing starts the builder, and nothing answers it: the provider fails.
    script.push(
        "tutor",
        Reply::Calls("Done.", vec![("end", "session_end", json!({}))]),
    );
    h.approve(&id, vec![]).await.unwrap();
    h.wait_background(&id).await;

    let jobs = graph_jobs(&h, &id);
    let end = jobs
        .iter()
        .find(|(state, _)| state == "failed" || state == "done" || state == "aborted")
        .unwrap_or_else(|| panic!("no terminal graph job: {jobs:?}"));
    assert_eq!(end.0, "failed");
    // The failure is recorded with a reason and changes no structure.
    let op = ops(&h, &id)
        .into_iter()
        .find_map(|o| match o {
            Op::Job(j) if j.kind == "graph" && j.state == "failed" => Some(j),
            _ => None,
        })
        .expect("the failed job op");
    assert!(!op.error.as_deref().unwrap_or("").is_empty());
    assert!(h.knowledge_map().unwrap().nodes.is_empty());
    // The close still stands, and nothing was delivered.
    let st = h.session_state(&id).unwrap();
    assert!(st.ended);
    assert!(st.queued_jobs.is_empty());
}

/// Where the field lives belongs to topics (`docs/learning-system.md` §6): the
/// builder submits the session's venues against the topics they serve, the read
/// model aggregates them up the tree, and a later build updates one instead of
/// duplicating it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_graph_job_lands_venues_on_their_topics() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("graph-venues");
    let h = harness(&repo, &config, &base).await;
    let id = session_awaiting_approval(
        &h,
        &script,
        &release,
        vec![Reply::Calls("", vec![("w1", "plan_write", plan_args(&[]))])],
    )
    .await;
    script.push("tutor", Reply::Text(teaching("CALCULUS", 1500)));
    h.approve(&id, vec![]).await.unwrap();
    h.wait_background(&id).await;

    script.push(
        "graph",
        Reply::Calls(
            "",
            vec![(
                "v1",
                "graph_submit",
                json!({
                    "topics": [
                        {"topicId": "math", "title": "Mathematics", "parent": null},
                        {"topicId": "calc", "title": "Calculus", "parent": "math"}
                    ],
                    "nodes": [{"nodeId": "limit", "title": "Limit",
                               "statement": "A limit is the value a function approaches.",
                               "kind": "fact", "home": "calc"}],
                    "venues": [
                        {"topicId": "calc", "kind": "ask", "name": "Math StackExchange",
                         "url": "https://math.stackexchange.com/", "entryLevel": "anyone",
                         "note": "ask anything, at any level", "checked": "2026-11-07"},
                        {"topicId": "math", "kind": "reference", "name": "SEP",
                         "url": "https://plato.stanford.edu/", "entryLevel": "anyone",
                         "note": "the philosophy of mathematics, written carefully"}
                    ]
                }),
            )],
        ),
    );
    h.graph_build(&id).unwrap();
    h.wait_background(&id).await;

    let map = h.knowledge_map().unwrap();
    assert_eq!(map.venues.len(), 2, "{:?}", map.venues);
    let result = graph_jobs(&h, &id)
        .into_iter()
        .find(|(state, _)| state == "done")
        .map(|(_, result)| result)
        .expect("a done graph job");
    assert_eq!(result.as_ref().unwrap()["venues"], 2, "{result:?}");

    // The read model puts each venue on its own topic, and a venue under
    // `Calculus` is on `Mathematics` too.
    let own: Vec<&str> = map
        .topic_venues
        .get("calc")
        .expect("calculus has its venue")
        .iter()
        .map(|v| v.name.as_str())
        .collect();
    assert_eq!(own, vec!["Math StackExchange"]);
    let all: Vec<&str> = map.topic_venues["math"]
        .iter()
        .map(|v| v.name.as_str())
        .collect();
    assert_eq!(
        all,
        vec!["Math StackExchange", "SEP"],
        "own plus descendant, ask before reference"
    );
    assert!(
        !map.topic_venues.contains_key("limit"),
        "nodes are not topics"
    );

    // The wire shape the Map reads is camelCase, with the fields the UI uses.
    let wire = serde_json::to_string(&map).unwrap();
    assert!(wire.contains(r#""topicId":"calc""#), "{wire}");
    assert!(wire.contains(r#""entryLevel":"anyone""#), "{wire}");
    assert!(wire.contains(r#""checked":"2026-11-07""#), "{wire}");
    assert!(wire.contains("\"topicVenues\""), "{wire}");

    // A later build that restates the same venue replaces it, keyed by
    // (topic, name), and leaves the other venue alone.
    script.push("tutor", Reply::Text(teaching("MORE CALCULUS", 1500)));
    h.send_learner_message(&id, "Where else can I ask?")
        .await
        .unwrap();
    h.wait_background(&id).await;
    script.push(
        "graph",
        Reply::Calls(
            "",
            vec![(
                "v2",
                "graph_submit",
                json!({"venues": [{
                    "topicId": "calc", "kind": "ask", "name": "math stackexchange",
                    "url": "https://math.stackexchange.com/", "entryLevel": "anyone",
                    "note": "better for reading old answers", "checked": "2026-12-01"
                }]}),
            )],
        ),
    );
    h.graph_build(&id).unwrap();
    h.wait_background(&id).await;
    let again = h.knowledge_map().unwrap();
    assert_eq!(again.venues.len(), 2, "the same venue, updated");
    let calc = again
        .venues
        .iter()
        .find(|v| v.topic_id == "calc")
        .expect("the calculus venue");
    assert_eq!(calc.note, "better for reading old answers");
    assert_eq!(calc.checked.as_deref(), Some("2026-12-01"));
    assert_eq!(again.topic_venues["math"].len(), 2);

    // Nothing venue-related reached the conversation.
    assert!(
        !ops(&h, &id).iter().any(|o| matches!(o, Op::Message(m)
            if m.content.contains("Math StackExchange") || m.content.contains("venue"))),
        "the builder's text stays out of the conversation"
    );
}
