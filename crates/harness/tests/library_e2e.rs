//! Reference libraries end to end (architecture §4.27) with the scripted
//! provider from `common/mod.rs`: a folder imported as a library, linked to
//! a new Learn session from Home, listed in the tutor's prompt, and searched
//! and read through the tutor's library tools.

mod common;

use std::sync::Arc;

use common::*;
use harness::{Attachment, ImportOptions, Op};
use serde_json::json;
use tokio::sync::Semaphore;

fn library_chip(slug: &str, name: &str) -> Attachment {
    Attachment {
        kind: "library".into(),
        path: slug.into(),
        media_type: None,
        bytes: None,
        name: Some(name.into()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_linked_library_reaches_the_tutor_prompt_and_tools() {
    let script = Arc::new(Script::default());
    let base = serve(Arc::clone(&script), Arc::new(Semaphore::new(0))).await;
    let (repo, config) = temp_dirs("library");
    let h = harness(&repo, &config, &base).await;

    let source = repo.parent().unwrap().join("cloud-refs");
    std::fs::create_dir_all(source.join("past-papers")).unwrap();
    std::fs::create_dir_all(source.join(".git")).unwrap();
    std::fs::write(
        source.join("past-papers/bank.md"),
        "# Bank\n\n1. Which service stores objects?\n(A) ECS\n(B) OBS\nAnswer: B\n",
    )
    .unwrap();
    std::fs::write(
        source.join("outline.md"),
        "# Exam outline\n\nStorage: 20%\n",
    )
    .unwrap();
    std::fs::write(source.join(".git/HEAD"), "ref: refs/heads/main").unwrap();

    let mut events = 0;
    let report = h
        .library_import(
            source.to_str().unwrap(),
            ImportOptions {
                description: Some("Cloud track exam material".into()),
                ..ImportOptions::default()
            },
            &mut |_| events += 1,
        )
        .unwrap();
    let slug = report.library.slug.clone();
    assert_eq!((slug.as_str(), report.added), ("cloud-refs", 2));
    assert!(events >= 4, "scan, one per file, done: {events}");
    assert!(repo
        .join("library/cloud-refs/text/past-papers/bank.md.md")
        .is_file());
    assert!(!repo.join("library/cloud-refs/text/.git").exists());
    // Picking the same folder again refreshes the same library.
    let again = h
        .library_import(
            source.to_str().unwrap(),
            ImportOptions::default(),
            &mut |_| {},
        )
        .unwrap();
    assert_eq!(
        (again.library.slug.as_str(), again.unchanged),
        ("cloud-refs", 2)
    );
    assert_eq!(h.libraries().unwrap().len(), 1);

    // A chip naming a library that doesn't exist is refused before a
    // session is created.
    let sessions = h.list_sessions().unwrap().len();
    assert!(h
        .start_teaching_with("Help", &[library_chip("gone", "Gone")])
        .await
        .is_err());
    assert_eq!(h.list_sessions().unwrap().len(), sessions);

    script.push(
        "tutor",
        Reply::Calls(
            "",
            vec![
                ("s1", "library_search", json!({ "query": "stores objects" })),
                (
                    "r1",
                    "library_read",
                    json!({ "path": "cloud-refs/outline.md", "from": 3 }),
                ),
                ("l1", "library_list", json!({ "kind": "question-bank" })),
            ],
        ),
    );
    script.push("tutor", Reply::Text("Let's start from your outline."));
    let id = h
        .start_teaching_with(
            "Get me ready for the cloud exam",
            &[library_chip(&slug, "cloud-refs")],
        )
        .await
        .unwrap()
        .id;

    // The link is stored with the first message, as a reference, not a copy.
    let first = ops(&h, &id)
        .into_iter()
        .find_map(|o| match o {
            Op::Message(m) if m.role == harness::providers::Role::User => Some(m),
            _ => None,
        })
        .unwrap();
    assert_eq!(first.attachments, [library_chip(&slug, "cloud-refs")]);

    let request = &script.requests_of("tutor")[0];
    let system = request["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains("## Reference libraries"), "{system}");
    assert!(
        system.contains("(`cloud-refs`, linked to this session) — Cloud track exam material"),
        "{system}"
    );
    assert!(
        system.contains("write your own original questions"),
        "{system}"
    );
    assert!(
        !system.contains("align the plan"),
        "planner-only line leaked"
    );
    let tools = tool_names(request);
    for t in ["library_list", "library_search", "library_read"] {
        assert!(tools.iter().any(|n| n == t), "{tools:?}");
    }
    let user = request["messages"].to_string();
    assert!(
        user.contains("[linked library] cloud-refs (`cloud-refs`)"),
        "{user}"
    );

    let search = results(&h, &id, "library_search");
    assert!(!search[0].1, "{search:?}");
    assert!(
        search[0]
            .0
            .contains("cloud-refs/past-papers/bank.md:3 (question-bank)"),
        "{search:?}"
    );
    let read = results(&h, &id, "library_read");
    assert!(!read[0].1 && read[0].0.contains("Storage: 20%"), "{read:?}");
    let list = results(&h, &id, "library_list");
    assert!(
        list[0].0.contains("`cloud-refs/past-papers/bank.md`"),
        "{list:?}"
    );
    assert!(!list[0].0.contains("outline.md"), "{list:?}");
    assert!(script.unexpected.lock().unwrap().is_empty());

    // Removing it takes the text out of the repo and the copies off the
    // device; the source folder stays.
    h.library_remove(&slug).unwrap();
    assert!(!repo.join("library/cloud-refs").exists());
    assert!(!config.join("library/cloud-refs").exists());
    assert!(source.join("past-papers/bank.md").is_file());
    assert!(h.libraries().unwrap().is_empty());
}
