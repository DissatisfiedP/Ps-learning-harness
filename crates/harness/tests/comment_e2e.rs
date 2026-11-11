//! The learner's comment interrupts the tutor (ui-design §6.3, "Comment"): an
//! open question is set aside, the comment is answered first, and a
//! how-to-teach instruction recorded with `preference_add` reaches every later
//! turn through the goal's preferences.

mod common;

use std::sync::Arc;

use common::*;
use harness::state::Phase;
use serde_json::json;
use tokio::sync::Semaphore;

const COMMENT: &str = "Make up your own questions, don't copy them from the reference PDF.";
const PREFERENCE: &str = "Write original questions; never copy them from the reference PDF.";

#[tokio::test]
async fn a_comment_sets_the_open_question_aside_and_its_instruction_stands() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("comment");
    let h = harness(&repo, &config, &base).await;
    let id = session_awaiting_approval(
        &h,
        &script,
        &release,
        vec![Reply::Calls("", vec![("w1", "plan_write", plan_args(&[]))])],
    )
    .await;

    // Teaching opens with a question; the learner comments instead of answering.
    script.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "q1",
                "question_ask",
                json!({ "role": "check", "level": "use", "nodeId": NODE, "format": "free",
                    "prompt": "With 12 V across 3 kΩ and 1 kΩ, what is across the 1 kΩ?",
                    "answer": "3 V" }),
            )],
        ),
    );
    h.approve(&id, vec![]).await.unwrap();
    assert!(h.session_state(&id).unwrap().pending_call.is_some());
    let err = h.send_learner_message(&id, "hello").await.unwrap_err();
    assert!(err.to_string().contains("answer the open question first"));

    script.push(
        "tutor",
        Reply::Calls(
            "",
            vec![("p1", "preference_add", json!({ "instruction": PREFERENCE }))],
        ),
    );
    script.push(
        "tutor",
        Reply::Text("Understood: my own questions from here on."),
    );
    h.comment(&id, COMMENT).await.unwrap();
    h.wait_background(&id).await;

    let st = h.session_state(&id).unwrap();
    assert_eq!(st.phase, Phase::Teach);
    assert!(st.pending_call.is_none(), "the question was set aside");
    let preferences = st.goal.as_ref().unwrap()["preferences"].clone();
    assert_eq!(preferences, json!([PREFERENCE]));
    assert_eq!(
        results(&h, &id, "preference_add"),
        [("Recorded. Follow it from now on.".to_owned(), false)]
    );

    // The tutor's turn on the comment: the question's result says it was set
    // aside, and the comment is the learner's latest message.
    let tutor = script.requests_of("tutor");
    let on_comment = &tutor[tutor.len() - 2];
    let set_aside = tool_message(on_comment, "q1").expect("q1 has a result");
    assert!(set_aside.contains("\"setAside\":true"), "{set_aside}");
    let last_user = on_comment["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|m| m["role"] == "user")
        .unwrap();
    assert!(last_user["content"]
        .to_string()
        .contains("Make up your own questions"));

    // The instruction stands: the next turn's pinned state carries it.
    script.push("tutor", Reply::Text("Here is a question I wrote."));
    h.send_learner_message(&id, "ok, go on").await.unwrap();
    let next = script.requests_of("tutor").pop().unwrap();
    let system = next["messages"][0]["content"].as_str().unwrap();
    assert!(
        system.contains(PREFERENCE),
        "the preference is in the session state"
    );
}

#[tokio::test]
async fn a_comment_on_a_waiting_plan_becomes_a_change_request() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("comment-plan");
    let h = harness(&repo, &config, &base).await;
    let id = session_awaiting_approval(
        &h,
        &script,
        &release,
        vec![Reply::Calls("", vec![("w1", "plan_write", plan_args(&[]))])],
    )
    .await;

    script.push(
        "planner",
        Reply::Calls(
            "",
            vec![("k1", "plan_keep", json!({ "reason": "Already covered." }))],
        ),
    );
    h.comment(&id, "Can we skip the theory?").await.unwrap();
    h.wait_background(&id).await;

    let planner = script.requests_of("planner").pop().unwrap();
    assert!(planner.to_string().contains("Can we skip the theory?"));
    assert_eq!(h.session_state(&id).unwrap().phase, Phase::AwaitApproval);
}
