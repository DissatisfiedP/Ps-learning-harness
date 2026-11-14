//! End-to-end proof of the learner model (architecture §4.8): a graded
//! teaching question becomes an `Outcome` on the graph node it measures, and
//! what the log says reaches the agents' view of that node (`graph_read`, the
//! pinned `## Learner state`) on the next turn.

mod common;

use std::sync::Arc;

use common::*;
use harness::graph::{append, provenance, GraphOp};
use harness::state::Kind;
use serde_json::{json, Value};
use tokio::sync::Semaphore;

/// The graph already holds the plan's one node, by the plan node's id.
fn seed_graph(repo: &std::path::Path) {
    append(
        repo,
        &[
            GraphOp::topic("circuits", "Circuits", None),
            GraphOp::node(
                NODE,
                Some("The divider rule"),
                "Vout = Vin · R2 / (R1 + R2).",
                Kind::Mechanism,
                "circuits",
                Some(provenance("import", None, "seed")),
            ),
        ],
    )
    .unwrap();
}

fn outcomes(repo: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(repo.join("practice/outcomes.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn system_prompt(request: &Value) -> String {
    request["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn check(call_id: &'static str) -> Reply {
    Reply::Calls(
        "",
        vec![(
            call_id,
            "question_ask",
            json!({ "role": "check", "level": "explain", "nodeId": NODE, "format": "free",
                "prompt": "What is across the 1 kΩ?", "answer": "3 V" }),
        )],
    )
}

fn verdict(call_id: &'static str, question: &'static str, v: &str, extra: Value) -> Reply {
    let mut args = json!({ "questionId": question, "verdict": v, "note": "n" });
    if let (Some(a), Some(e)) = (args.as_object_mut(), extra.as_object()) {
        a.extend(e.clone());
    }
    Reply::Calls("", vec![(call_id, "verdict_record", args)])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_graded_check_becomes_evidence_and_the_next_turn_sees_it() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("learner");
    seed_graph(&repo);
    let h = harness(&repo, &config, &base).await;
    let id = session_awaiting_approval(
        &h,
        &script,
        &release,
        vec![Reply::Calls("", vec![("w1", "plan_write", plan_args(&[]))])],
    )
    .await;

    // Teach: a check, answered wrongly with a stated misconception.
    script.push("tutor", check("q1"));
    h.approve(&id, vec![]).await.unwrap();
    script.push(
        "tutor",
        verdict(
            "v1",
            "q1",
            "wrong",
            json!({ "misconception": "treats the two resistors as in parallel" }),
        ),
    );
    script.push("tutor", Reply::Text("Not quite: they're in series."));
    h.answer(&id, "q1", answer("6 V")).await.unwrap();
    h.wait_background(&id).await;

    let log = outcomes(&repo);
    assert_eq!(log.len(), 1, "{log:?}");
    let o = &log[0];
    assert_eq!(o["targets"], json!([NODE]));
    assert_eq!(o["verdict"], "wrong");
    assert_eq!(o["layer"], "recall");
    assert_eq!(o["format"], "check");
    assert_eq!(o["sessionId"], id);
    assert_eq!(o["missClass"], "structural");
    assert_eq!(
        o["misconception"],
        "treats the two resistors as in parallel"
    );

    // The model derived from that log says the node isn't held and carries
    // the open misconception.
    let views = h.learner_nodes().unwrap();
    let view = &views[NODE];
    assert!(
        !view.layers.recall && view.misconceptions.len() == 1,
        "{view:?}"
    );

    // The next tutor turn sees it, both in the pinned state and from the
    // graph tools (the probe skips held nodes; this one isn't).
    script.push(
        "tutor",
        Reply::Calls("", vec![("g1", "graph_read", json!({ "id": NODE }))]),
    );
    script.push("tutor", Reply::Text("Let's repair that."));
    h.send_learner_message(&id, "ok").await.unwrap();
    h.wait_background(&id).await;
    let tutor = script.requests_of("tutor");
    let pinned = system_prompt(tutor.iter().rev().nth(1).unwrap());
    assert!(
        pinned.contains("not yet recalled")
            && pinned.contains("misconception")
            && pinned.contains("treats the two resistors as in parallel"),
        "{pinned}"
    );
    let read = results(&h, &id, "graph_read");
    assert!(
        read[0].0.contains("learner model: not held") && read[0].0.contains("open misconception"),
        "{read:?}"
    );

    // A learner who answers "I don't know" is evidence too, once.
    script.push("tutor", check("q2"));
    script.push("tutor", verdict("v2", "q2", "wrong", json!({})));
    script.push("tutor", Reply::Text("Noted."));
    h.send_learner_message(&id, "ask me again").await.unwrap();
    h.answer(
        &id,
        "q2",
        json!({ "answer": "", "dontKnow": true, "thinking": "no idea" }),
    )
    .await
    .unwrap();
    h.wait_background(&id).await;
    let log = outcomes(&repo);
    let dont_know: Vec<_> = log.iter().filter(|o| o["verdict"] == "dontknow").collect();
    assert_eq!(
        dont_know.len(),
        1,
        "recorded when sent, not again by the verdict: {log:?}"
    );
    assert_eq!(log.len(), 2, "{log:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_question_that_measures_no_graph_node_records_nothing() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("learner-nograph");
    // No graph yet: the plan node isn't a graph node, so there is nothing to
    // attach evidence to.
    let h = harness(&repo, &config, &base).await;
    let id = session_awaiting_approval(
        &h,
        &script,
        &release,
        vec![Reply::Calls("", vec![("w1", "plan_write", plan_args(&[]))])],
    )
    .await;
    script.push("tutor", check("q1"));
    h.approve(&id, vec![]).await.unwrap();
    script.push("tutor", verdict("v1", "q1", "correct", json!({})));
    script.push("tutor", Reply::Text("Right."));
    h.answer(&id, "q1", answer("3 V")).await.unwrap();
    h.wait_background(&id).await;
    assert!(outcomes(&repo).is_empty());
}
