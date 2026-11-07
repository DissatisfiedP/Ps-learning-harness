//! Learner files end to end (build prompt W3, W4) with the scripted provider
//! from `common/mod.rs`: attachments on the first message and the read roots
//! they grant, and the workspace folder's write guard.
//!
//! The file tools' home root is `$HOME`, so this binary points `HOME` at a
//! temp directory once, before any harness exists.

mod common;

use std::path::PathBuf;
use std::sync::{Arc, Once};

use common::*;
use harness::Op;
use serde_json::{json, Value};
use tokio::sync::Semaphore;

/// A 1×1 PNG.
const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

/// Temp root shared by this binary: `<root>/home` is `$HOME`.
fn root() -> PathBuf {
    static HOME: Once = Once::new();
    let root = std::env::temp_dir().join(format!("lh-files-{}", std::process::id()));
    HOME.call_once(|| {
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::env::set_var("HOME", root.join("home"));
    });
    root
}

// ---------------------------------------------------------------------------
// W3: three attachments on the first message
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attachments_reach_the_model_and_grant_only_the_picked_outside_file() {
    let root = root();
    let script = Arc::new(Script::default());
    let base = serve(Arc::clone(&script), Arc::new(Semaphore::new(0))).await;
    let (repo, config) = temp_dirs("attach");
    let h = harness(&repo, &config, &base).await;

    std::fs::write(root.join("home/notes.md"), "home notes").unwrap();
    let outside = root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "picked file").unwrap();
    std::fs::write(outside.join("sibling.txt"), "not picked").unwrap();

    let image = h.stage_image(PNG, Some("dot.png")).unwrap();
    // A chip removed before Send leaves nothing behind.
    let removed = h.stage_image(PNG, Some("removed.png")).unwrap();
    h.unstage_image(&removed.path).unwrap();
    assert!(
        !repo.join("attachments").exists(),
        "images reach the repo only on Send"
    );
    let mention = h.stage_file(None, "~/notes.md").unwrap();
    let picked = h
        .stage_file(None, outside.join("secret.txt").to_str().unwrap())
        .unwrap();
    let secret = picked.path.clone();
    let sibling = outside.join("sibling.txt").display().to_string();

    script.push(
        "tutor",
        Reply::Calls(
            "",
            vec![
                ("f1", "file_read", json!({ "path": secret })),
                ("f2", "file_read", json!({ "path": sibling })),
                ("f3", "file_read", json!({ "path": "~/notes.md" })),
            ],
        ),
    );
    script.push("tutor", Reply::Text("I read your files."));
    let id = h
        .start_teaching_with(
            "Help me with these",
            &[image.clone(), mention.clone(), picked.clone()],
        )
        .await
        .unwrap()
        .id;

    // The first learner message op holds all three.
    let first = ops(&h, &id)
        .into_iter()
        .find_map(|o| match o {
            Op::Message(m) if m.role == harness::providers::Role::User => Some(m),
            _ => None,
        })
        .unwrap();
    assert_eq!(first.attachments, [image.clone(), mention, picked]);
    // Send moved the staged image into the repo; the removed one never came.
    assert!(repo.join(&image.path).is_file());
    assert!(!repo.join(&removed.path).exists());

    // The model got one image block and one path line per file.
    let request = &script.requests_of("tutor")[0];
    let user = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "user")
        .unwrap();
    let parts = user["content"].as_array().unwrap();
    let images = parts.iter().filter(|p| p["type"] == "image_url").count();
    assert_eq!(images, 1, "{user}");
    let text: String = parts
        .iter()
        .filter_map(|p| p["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| l.starts_with("[attached file]"))
        .collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(lines.iter().any(|l| l.contains("notes.md")));
    assert!(lines.iter().any(|l| l.contains(&secret)));

    // Only the outside file became a read root, and only that exact file.
    let reads = results(&h, &id, "file_read");
    assert_eq!((reads[0].0.trim(), reads[0].1), ("picked file", false));
    assert!(
        reads[1].1 && reads[1].0.contains("outside the directories you granted"),
        "{reads:?}"
    );
    assert_eq!((reads[2].0.trim(), reads[2].1), ("home notes", false));
    assert_eq!(h.session_state(&id).unwrap().file_grants, [secret]);
    assert!(script.unexpected.lock().unwrap().is_empty());
}

/// Practice has no file tools: a file reference is refused before anything
/// is written, and a message always needs text.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn practice_takes_images_only() {
    let root = root();
    let script = Arc::new(Script::default());
    let base = serve(Arc::clone(&script), Arc::new(Semaphore::new(0))).await;
    let (repo, config) = temp_dirs("practice");
    let h = harness(&repo, &config, &base).await;
    std::fs::write(root.join("home/practice.md"), "notes").unwrap();
    let file = h.stage_file(None, "~/practice.md").unwrap();
    let image = h.stage_image(PNG, Some("dot.png")).unwrap();

    let err = h
        .send_with(
            None,
            harness::Mode::Practice,
            "check this",
            &[file],
            &mut |_| {},
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("images only"), "{err}");
    let err = h
        .send_with(
            None,
            harness::Mode::Practice,
            "  ",
            std::slice::from_ref(&image),
            &mut |_| {},
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("empty"), "{err}");
    assert!(h.list_sessions().unwrap().is_empty());
    assert!(!repo.join(&image.path).exists());
}

// ---------------------------------------------------------------------------
// W4: workspace write guard, escapes, detach, nothing absolute in the log
// ---------------------------------------------------------------------------

fn write(path: &str, content: &str) -> Value {
    json!({ "path": path, "content": content })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workspace_writes_never_clobber_learner_edits_and_stop_at_detach() {
    let root = root();
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("workspace");
    let h = harness(&repo, &config, &base).await;
    let ws = root.join("practice-ws");
    let escape = root.join("escape-target");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::create_dir_all(&escape).unwrap();
    std::os::unix::fs::symlink(&escape, ws.join("link")).unwrap();
    let s = &*script;

    let id = session_awaiting_approval(
        &h,
        s,
        &release,
        vec![Reply::Calls("", vec![("w1", "plan_write", plan_args(&[]))])],
    )
    .await;
    assert_eq!(
        h.attach_workspace(&id, ws.to_str().unwrap()).unwrap(),
        "practice-ws"
    );

    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "a1",
                "workspace_write",
                write("task1/README.md", "# Task 1\n"),
            )],
        ),
    );
    s.push("tutor", Reply::Text("Your task is in task1/README.md."));
    h.approve(&id, vec![]).await.unwrap();
    let file = ws.join("task1/README.md");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "# Task 1\n");

    // The learner edits it in their editor; the agent must read first.
    std::fs::write(&file, "# Task 1\nmy answer\n").unwrap();
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![
                (
                    "a2",
                    "workspace_write",
                    write("task1/README.md", "overwrite"),
                ),
                ("r1", "file_read", json!({ "path": "task1/README.md" })),
                ("f1", "file_find", json!({ "pattern": "README" })),
                (
                    "a3",
                    "workspace_write",
                    write("task1/README.md", "# Task 1\nmy answer\nnext step\n"),
                ),
                ("a4", "workspace_write", write("../x.md", "escape")),
                ("a5", "workspace_write", write("link/evil.md", "escape")),
            ],
        ),
    );
    s.push("tutor", Reply::Text("Updated."));
    h.send_learner_message(&id, "I edited the file.")
        .await
        .unwrap();
    let writes = results(&h, &id, "workspace_write");
    assert!(!writes[0].1, "{writes:?}");
    assert!(
        writes[1].1 && writes[1].0.contains("call file_read first"),
        "{writes:?}"
    );
    assert!(
        !writes[2].1,
        "after file_read the write goes through: {writes:?}"
    );
    assert!(
        writes[3].1 && writes[3].0.contains("outside the workspace"),
        "{writes:?}"
    );
    assert!(
        writes[4].1 && writes[4].0.contains("outside the workspace"),
        "{writes:?}"
    );
    assert_eq!(
        results(&h, &id, "file_read")[0],
        ("# Task 1\nmy answer\n".to_owned(), false)
    );
    // Workspace hits are workspace-relative (the path file_read takes).
    assert_eq!(
        results(&h, &id, "file_find")[0],
        ("task1/README.md".to_owned(), false)
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "# Task 1\nmy answer\nnext step\n"
    );
    assert!(!root.join("x.md").exists());
    assert!(!escape.join("evil.md").exists());

    // Detach removes write access at once.
    h.detach_workspace(&id).unwrap();
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![("a6", "workspace_write", write("task1/README.md", "late"))],
        ),
    );
    s.push("tutor", Reply::Text("Detached."));
    h.send_learner_message(&id, "I detached it.").await.unwrap();
    let writes = results(&h, &id, "workspace_write");
    assert!(writes[5].1, "{writes:?}");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "# Task 1\nmy answer\nnext step\n"
    );
    let last = script.requests_of("tutor").pop().unwrap();
    assert!(!tool_names(&last).contains(&"workspace_write".to_owned()));

    // The op log names the folder but never its absolute path.
    let raw = raw_op_logs(&repo);
    assert!(raw.contains(r#""name":"practice-ws""#), "{raw}");
    assert!(!raw.contains(ws.to_str().unwrap()));
    assert!(!raw.contains(root.to_str().unwrap()));
    assert!(script.unexpected.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// PDFs: a learner document read as text, and a scan read as an image
// ---------------------------------------------------------------------------

/// A two-page PDF built with `lopdf`: page 1 has a text layer, page 2 is a
/// scan (an embedded Flate RGB image and no text at all).
fn pdf_fixture() -> Vec<u8> {
    use lopdf::{dictionary, Document, Object, Stream};
    use std::io::Write as _;

    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
    });
    let resources = dictionary! { "Font" => dictionary! { "F1" => font_id } };

    let text = "Ohm's law holds that voltage equals current times resistance";
    let content = Stream::new(
        dictionary! {},
        format!("BT /F1 14 Tf 72 720 Td ({text}) Tj ET").into_bytes(),
    );
    let content_id = doc.add_object(content);
    let first = doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        "Contents" => content_id,
        "Resources" => resources,
    });

    let raw: Vec<u8> = (0..12u8).collect();
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(&raw).unwrap();
    let image = doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Image",
            "Width" => 2, "Height" => 2,
            "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8,
            "Filter" => "FlateDecode",
        },
        enc.finish().unwrap(),
    ));
    let blank = doc.add_object(Stream::new(dictionary! {}, b"q Q".to_vec()));
    let second = doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        "Contents" => blank,
        "Resources" => dictionary! { "XObject" => dictionary! { "Im1" => image } },
    });

    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(first), Object::Reference(second)],
            "Count" => 2,
        }),
    );
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog);
    let mut out = Vec::new();
    doc.save_to(&mut out).unwrap();
    out
}

/// The researcher reads a PDF the learner keeps at home: page 1 as page-tagged
/// text, page 2 (a scan) as an image block in its own message list, so the very
/// next model call can look at it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_researcher_reads_a_pdf_and_sees_a_scan_as_an_image() {
    let root = root();
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("pdf");
    let h = harness(&repo, &config, &base).await;
    std::fs::write(root.join("home/paper.pdf"), pdf_fixture()).unwrap();

    let s = &*script;
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "g1",
                "goal_set",
                json!({ "goal": "Ohm's law", "purpose": "curiosity",
                    "successLooksLike": ["apply it"], "materials": [], "outOfScope": [],
                    "preferences": [], "orientationFirst": false }),
            )],
        ),
    );
    s.push("tutor", Reply::Text("Let's begin."));
    s.push("title", Reply::Text("Ohm's law"));
    s.push(
        "researcher",
        Reply::Calls(
            "",
            vec![
                (
                    "r1",
                    "source_read",
                    json!({ "path": "paper.pdf", "pages": "1" }),
                ),
                (
                    "r2",
                    "source_page_image",
                    json!({ "path": "paper.pdf", "page": 2 }),
                ),
            ],
        ),
    );
    s.push(
        "researcher",
        Reply::Calls(
            "",
            vec![(
                "b1",
                "brief_submit",
                json!({ "kind": "scope", "summary": "Voltage is current times resistance.",
                    "strands": [], "misconceptions": [], "sources": [] }),
            )],
        ),
    );
    s.push("tutor", Reply::Text("A brief arrived."));
    release.add_permits(2);
    let id = h
        .start_teaching("I want to understand Ohm's law")
        .await
        .unwrap()
        .id;
    h.wait_background(&id).await;

    let requests = script.requests_of("researcher");
    assert_eq!(requests.len(), 2, "{:?}", script.unexpected.lock().unwrap());
    // Nothing to look at yet on the first call.
    let first = requests[0]["messages"].to_string();
    assert!(!first.contains("image_url"), "{first}");

    // The second call carries the page-tagged text and the scan's image.
    let messages = requests[1]["messages"].as_array().unwrap().clone();
    let tool_text: String = messages
        .iter()
        .filter(|m| m["role"] == "tool")
        .filter_map(|m| m["content"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(tool_text.contains("page 1 of 2"), "{tool_text}");
    assert!(tool_text.contains("[p.1] Ohm"), "{tool_text}");
    assert!(
        tool_text.contains("voltage equals current times resistance"),
        "{tool_text}"
    );
    assert!(
        tool_text.contains("page 2 is a scan; its image is attached"),
        "{tool_text}"
    );
    let image = messages.iter().find_map(|m| {
        (m["role"] == "user")
            .then(|| m["content"].as_array())
            .flatten()
            .and_then(|parts| {
                parts
                    .iter()
                    .find(|p| p["type"] == "image_url")
                    .map(|p| p["image_url"]["url"].as_str().unwrap_or("").to_owned())
            })
    });
    let image = image.expect("the scan reaches the model as an image block");
    assert!(image.starts_with("data:image/png;base64,"), "{image}");
    assert!(script.unexpected.lock().unwrap().is_empty());
}
