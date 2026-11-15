//! End-to-end proof of scratch-pad drawings sent with an answer: the staged
//! picture is committed to the repo, the stored answer says where it
//! belongs, and the tutor's next request carries the image after the tool
//! result.

mod common;

use std::sync::Arc;

use common::*;
use serde_json::{json, Value};
use tokio::sync::Semaphore;

const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sketch_rides_with_the_answer_and_the_tutor_gets_the_picture() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("sketch");
    let h = harness(&repo, &config, &base).await;
    let id = session_awaiting_approval(
        &h,
        &script,
        &release,
        vec![Reply::Calls("", vec![("w1", "plan_write", plan_args(&[]))])],
    )
    .await;
    script.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "q1",
                "question_ask",
                json!({ "role": "check", "level": "explain", "nodeId": NODE, "format": "free",
                    "prompt": "Draw the divider.", "answer": "R1 over R2" }),
            )],
        ),
    );
    h.approve(&id, vec![]).await.unwrap();

    // A path outside the repo's attachments is refused, and the question stays open.
    let escape = json!({ "answer": "x", "sketches": [{ "in": "answer",
        "attachment": { "kind": "image", "path": "../../secret.png" } }] });
    assert!(h.answer(&id, "q1", escape).await.is_err());

    let sketch = h.stage_image(PNG, Some("Scratch pad.png")).unwrap();
    assert!(
        !repo.join(&sketch.path).exists(),
        "a staged drawing stays out of the synced repo until it is sent"
    );
    script.push("tutor", Reply::Text("I can see your drawing."));
    let payload = json!({ "answer": "", "thinking": "see the sketch", "confidence": 60,
        "sketches": [{ "in": "answer", "attachment": sketch }] });
    h.answer(&id, "q1", payload).await.unwrap();
    h.wait_background(&id).await;

    // The stored answer says where the picture belongs; the file is in the repo.
    let stored = results(&h, &id, "question_ask");
    assert_eq!(stored.len(), 1, "{stored:?}");
    let content: Value = serde_json::from_str(&stored[0].0).unwrap();
    assert_eq!(
        content["sketches"],
        json!([{ "in": "answer", "path": sketch.path }])
    );
    assert!(repo.join(&sketch.path).is_file());

    // The next request: the tool result, then the picture as a user message.
    let requests = script.requests_of("tutor");
    let messages = requests.last().unwrap()["messages"].as_array().unwrap();
    let at = messages
        .iter()
        .position(|m| {
            m["content"]
                .as_array()
                .is_some_and(|parts| parts.iter().any(|p| p["type"] == "image_url"))
        })
        .unwrap_or_else(|| panic!("no image in the request: {messages:?}"));
    assert_eq!(messages[at]["role"], "user");
    assert_eq!(messages[at - 1]["role"], "tool", "{messages:?}");
    let label = messages[at]["content"][0]["text"].as_str().unwrap();
    assert!(label.contains("with their answer"), "{label}");
}
