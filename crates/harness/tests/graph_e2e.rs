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
        system.contains("Voltage dividers"),
        "the plan or goal: {system}"
    );
    assert!(system.contains("Learner:"), "the transcript: {system}");
    let offered = tool_names(&graph_requests[0]);
    assert_eq!(offered, vec!["graph_search", "graph_submit"]);

    // Building again does not duplicate the structure. Re-sending the same
    // edges is refused as duplicates (the graph already holds them); the same
    // claims as upserts are accepted and change nothing.
    script.push(
        "graph",
        Reply::Calls("", vec![("s3", "graph_submit", submission())]),
    );
    script.push(
        "graph",
        Reply::Calls(
            "",
            vec![(
                "s4",
                "graph_submit",
                json!({
                    "topics": [{"topicId": "circuits", "title": "Circuits", "parent": null}],
                    "nodes": [
                        {"nodeId": "series-current", "title": "Series current",
                         "statement": "In a series circuit the same current flows through every element.",
                         "kind": "mechanism", "home": "circuits"},
                        {"nodeId": "divider-rule", "title": "The divider rule",
                         "statement": "The output of an unloaded two-resistor divider is V_in R2/(R1+R2).",
                         "kind": "mechanism", "home": "circuits"}
                    ]
                }),
            )],
        ),
    );
    h.graph_build(&id).unwrap();
    h.wait_background(&id).await;
    let again = h.knowledge_map().unwrap();
    assert_eq!(again.nodes.len(), 2, "the same ids, no copies");
    assert_eq!(again.edges.len(), 2);
    assert_eq!(again.topics.len(), 1);
    assert_eq!(again.chains.len(), 1);
    let rebuild = results(&h, &id, "graph_submit");
    assert!(
        rebuild
            .iter()
            .any(|(text, error)| *error && text.contains("already exists")),
        "a duplicate edge is refused: {rebuild:?}"
    );

    // And a third build: still two nodes and two edges.
    script.push(
        "graph",
        Reply::Calls(
            "",
            vec![(
                "s5",
                "graph_submit",
                json!({"nodes": [
                    {"nodeId": "divider-rule", "statement": "The output of an unloaded two-resistor divider is V_in R2/(R1+R2).",
                     "kind": "mechanism", "home": "circuits"}
                ]}),
            )],
        ),
    );
    h.graph_build(&id).unwrap();
    h.wait_background(&id).await;
    let third = h.knowledge_map().unwrap();
    assert_eq!(third.nodes.len(), 2);
    assert_eq!(third.edges.len(), 2);
    assert_eq!(script.requests_of("graph").len(), 5);

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
