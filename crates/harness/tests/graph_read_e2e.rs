//! End-to-end proof that the measuring tutor and the planner read the
//! learner's knowledge graph before the plan exists: both are offered
//! `graph_search` and `graph_read` (never `graph_submit`), the calls return
//! graph nodes, and the pinned `## Learner state` shows the graph — the topic
//! tree, then the nodes matching the goal.

mod common;

use std::sync::Arc;

use common::*;
use harness::graph::{append, provenance, GraphOp};
use harness::state::Kind;
use serde_json::json;
use tokio::sync::Semaphore;

/// What an earlier session left: one topic, two claims, one prerequisite.
fn seed_graph(repo: &std::path::Path) {
    append(
        repo,
        &[
            GraphOp::topic("circuits", "Circuits", None),
            GraphOp::node(
                "series-current",
                Some("Series current"),
                "In a series circuit the same current flows through every element.",
                Kind::Mechanism,
                "circuits",
                Some(provenance(
                    "session",
                    Some("earlier"),
                    "2026-10-02T10:00:00Z",
                )),
            ),
            GraphOp::node(
                NODE,
                Some("The divider rule"),
                "The output of an unloaded two-resistor divider is V_in R2/(R1+R2).",
                Kind::Mechanism,
                "circuits",
                Some(provenance(
                    "session",
                    Some("earlier"),
                    "2026-10-02T10:00:00Z",
                )),
            ),
            GraphOp::edge("series-current", NODE, "requires", None, None, None),
        ],
    )
    .unwrap();
}

fn system_prompt(request: &serde_json::Value) -> String {
    request["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_measuring_tutor_and_the_planner_read_the_graph_before_planning() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("graph-read");
    let h = harness(&repo, &config, &base).await;
    seed_graph(&repo);

    // The measurer searches before goal_set; the planner reads a node
    // before writing the plan.
    script.push(
        "tutor",
        Reply::Calls(
            "",
            vec![("gs1", "graph_search", json!({"query": "divider"}))],
        ),
    );
    let id = session_awaiting_approval(
        &h,
        &script,
        &release,
        vec![
            Reply::Calls("", vec![("gr1", "graph_read", json!({"id": NODE}))]),
            Reply::Calls("", vec![("w1", "plan_write", plan_args(&[]))]),
        ],
    )
    .await;

    // Measure: offered the reads, never the write, and the learner state
    // shows the graph's topics.
    let tutor = script.requests_of("tutor");
    let names = tool_names(&tutor[0]);
    assert!(names.iter().any(|n| n == "graph_search"), "{names:?}");
    assert!(names.iter().any(|n| n == "graph_read"), "{names:?}");
    assert!(!names.iter().any(|n| n == "graph_submit"), "{names:?}");
    let measure = system_prompt(&tutor[0]);
    assert!(
        measure.contains("## Learner state\nKnowledge graph: 1 topics, 2 nodes"),
        "{measure}"
    );
    assert!(measure.contains("- Circuits (circuits) 2"), "{measure}");

    // The search came back with the node and the session that recorded it.
    let searched = results(&h, &id, "graph_search");
    assert_eq!(searched.len(), 1, "{searched:?}");
    assert!(!searched[0].1, "{searched:?}");
    assert!(
        searched[0]
            .0
            .contains("divider-rule [circuits] The divider rule"),
        "{}",
        searched[0].0
    );
    assert!(
        searched[0].0.contains("session 2026-10-02"),
        "{}",
        searched[0].0
    );
    assert!(tool_message(&tutor[1], "gs1")
        .unwrap()
        .contains("divider-rule [circuits]"));

    // Plan: the planner has the reads, sees the goal's nodes in its state,
    // and graph_read returns the node with its prerequisite.
    let planner = script.requests_of("planner");
    let names = tool_names(&planner[0]);
    assert!(names.iter().any(|n| n == "graph_search"), "{names:?}");
    assert!(names.iter().any(|n| n == "graph_read"), "{names:?}");
    assert!(!names.iter().any(|n| n == "graph_submit"), "{names:?}");
    let plan_prompt = system_prompt(&planner[0]);
    assert!(
        plan_prompt
            .contains("Nodes matching the goal:\n- divider-rule [circuits] The divider rule"),
        "{plan_prompt}"
    );
    let read = results(&h, &id, "graph_read");
    assert_eq!(read.len(), 1, "{read:?}");
    assert!(!read[0].1, "{read:?}");
    assert!(
        read[0]
            .0
            .contains("requires (learn these first):\n- series-current Series current"),
        "{}",
        read[0].0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_empty_graph_says_so_and_the_learner_state_reads_no_graph_yet() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("graph-read-empty");
    let h = harness(&repo, &config, &base).await;

    script.push(
        "tutor",
        Reply::Calls(
            "",
            vec![("gs1", "graph_search", json!({"query": "divider"}))],
        ),
    );
    script.push("tutor", Reply::Text("What would you like to learn?"));
    let id = h.start_teaching("Hello").await.unwrap().id;
    h.wait_background(&id).await;

    let tutor = script.requests_of("tutor");
    assert!(
        system_prompt(&tutor[0]).contains("## Learner state\nno graph yet"),
        "{}",
        system_prompt(&tutor[0])
    );
    let searched = results(&h, &id, "graph_search");
    assert_eq!(searched.len(), 1, "{searched:?}");
    assert!(!searched[0].1, "{searched:?}");
    assert!(
        searched[0].0.starts_with("The knowledge graph is empty"),
        "{}",
        searched[0].0
    );
}
