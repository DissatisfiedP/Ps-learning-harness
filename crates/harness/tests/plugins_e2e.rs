//! Plugin runtime end to end (build prompt W5–W9) with the scripted provider
//! from `common/mod.rs` and the real `plugins/circuit` package: a planned
//! section draws a divider, artifacts outlive their plugin, answer mode
//! grades through the plugin's view, and registry installs feed the planner.

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use harness::{Harness, Op};
use serde_json::{json, Value};
use tokio::sync::Semaphore;

/// Artifact ops of a session, as JSON, in log order.
fn artifacts(h: &Harness, id: &str) -> Vec<Value> {
    ops(h, id)
        .iter()
        .filter_map(|o| match o {
            Op::Artifact(a) => serde_json::to_value(a).ok(),
            _ => None,
        })
        .collect()
}

/// A folder plugin whose artifact tool returns a hostile SVG, with a view
/// that is *not* an answer surface.
fn sketch_plugin(root: &Path) -> PathBuf {
    let dir = root.join("sketch");
    for sub in ["schemas", "src", "views"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }
    std::fs::write(
        dir.join("plugin.toml"),
        r#"id = "sketch"
version = "0.2.0"
api = 1
title = "Sketch"
description = "Draws a box."
license = "MIT"
platforms = ["desktop", "mobile"]
permissions = []
use_when = "A box helps."
avoid_when = "Never in lessons."
cost = "instant"

[[tool]]
name = "sketch_draw"
description = "Draw a box."
agents = ["tutor"]
input = "schemas/in.json"
entry = "draw"
artifact = true

[[renderer]]
entry = "render"

[[view]]
name = "sketch"
html = "views/sketch.html"
calls = []
answer = false
"#,
    )
    .unwrap();
    std::fs::write(dir.join("schemas/in.json"), r#"{"type":"object"}"#).unwrap();
    std::fs::write(dir.join("views/sketch.html"), "<p>box</p>").unwrap();
    std::fs::write(
        dir.join("src/main.luau"),
        r#"local svg = '<svg xmlns="http://www.w3.org/2000/svg" onload="alert(1)"><script>alert(2)</script><rect width="4" height="4" onclick="steal()"/></svg>'
return {
  draw = function(args)
    return { text = "drew a box", artifact = { kind = "svg", svg = svg, data = { size = 4 } } }
  end,
  render = function(data) return svg end,
}
"#,
    )
    .unwrap();
    dir
}

fn probe(v: &Value) -> f64 {
    v["result"]["probes"]["Vout"].as_f64().unwrap()
}

// ---------------------------------------------------------------------------
// W5 + W6: planner selects circuit, tutor draws, artifacts replay without it
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_planned_circuit_draws_and_its_artifacts_replay_without_the_plugin() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("plugin-draw");
    let h = harness(&repo, &config, &base).await;
    h.add_folder_plugin(circuit_dir().to_str().unwrap())
        .unwrap();
    let sketch = sketch_plugin(repo.parent().unwrap());
    h.add_folder_plugin(sketch.to_str().unwrap()).unwrap();
    let s = &*script;

    let id = session_awaiting_approval(
        &h,
        s,
        &release,
        vec![Reply::Calls(
            "",
            vec![("w1", "plan_write", plan_args(&["circuit", "sketch"]))],
        )],
    )
    .await;
    // The planner chose from a catalog that listed circuit.
    let planner = script.requests_of("planner");
    assert!(planner[0]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("`circuit`"));

    // One teach turn, three artifact calls (two circuits, one hostile SVG).
    s.push(
        "tutor",
        Reply::Calls(
            "Here is the circuit.",
            vec![
                ("d1", "circuit_draw", divider(1000.0)),
                ("d2", "circuit_draw", divider(3000.0)),
                ("k1", "sketch_draw", json!({})),
            ],
        ),
    );
    s.push("tutor", Reply::FromRequest(place_first_divider));
    h.approve(&id, vec![]).await.unwrap();
    let st = h.session_state(&id).unwrap();
    assert_eq!(st.active_plugins, ["circuit", "sketch"]);

    let made = artifacts(&h, &id);
    assert_eq!(made.len(), 3, "{made:?}");
    let tools: Vec<&str> = made.iter().map(|a| a["tool"].as_str().unwrap()).collect();
    assert_eq!(tools, ["circuit_draw", "circuit_draw", "sketch_draw"]);
    let info = h.plugins().into_iter().find(|p| p.id == "circuit").unwrap();
    let first = &made[0];
    assert_eq!(first["pluginId"], "circuit");
    assert_eq!(first["pluginVersion"], info.version);
    assert_eq!(first["packageHash"], info.package_hash);
    assert!(!info.package_hash.is_empty());
    assert_eq!(first["input"], divider(1000.0));
    assert!((probe(&first["data"]) - 3.0).abs() < 1e-6);
    assert!((probe(&made[1]["data"]) - 6.0).abs() < 1e-6);
    let svg = first["fallbackSvg"].as_str().unwrap();
    assert!(svg.starts_with("<svg") && svg.contains("Vout"), "{svg}");

    // The tutor got the solved value and the placement handle per call.
    let draws = results(&h, &id, "circuit_draw");
    assert_eq!(draws.len(), 2);
    for ((text, is_error), artifact) in draws.iter().zip(&made) {
        assert!(!is_error, "{text}");
        let handle = artifact["artifactId"].as_str().unwrap();
        assert!(text.contains(&format!("](artifact:{handle})")), "{text}");
    }
    assert!(draws[0].0.contains("Vout = 3 V"), "{}", draws[0].0);
    assert!(draws[1].0.contains("Vout = 6 V"), "{}", draws[1].0);
    let next = script.requests_of("tutor").pop().unwrap();
    assert!(tool_message(&next, "d1").unwrap().contains("Vout = 3 V"));

    // Hostile SVG from a plugin is stripped in core before it is stored.
    let hostile = made[2]["fallbackSvg"].as_str().unwrap();
    for bad in ["script", "alert", "onload", "onclick", "steal"] {
        assert!(!hostile.contains(bad), "{bad} survived: {hostile}");
    }
    assert!(hostile.contains("<rect"), "{hostile}");

    // Remove one plugin, disable the other, restart: the artifacts replay
    // from the ops alone.
    h.remove_plugin("circuit").unwrap();
    h.set_plugin_enabled("sketch", false).unwrap();
    drop(h);
    let h = Harness::open(&repo, &config).unwrap();
    assert!(h.plugins().iter().all(|p| p.id != "circuit"));
    assert!(h.plugins().iter().all(|p| p.id != "sketch" || !p.enabled));
    assert_eq!(artifacts(&h, &id), made);
    assert!(script.unexpected.lock().unwrap().is_empty());

    // Placement (W6 acceptance, core half; `scripts/ui-check` renders the
    // same op order): the closing reply carries the first circuit's handle
    // mid-paragraph and no other handle, and every artifact op precedes it,
    // so the UI places d1 in the sentence and appends the rest in order.
    let log = ops(&h, &id);
    let at = |pred: &dyn Fn(&Op) -> bool| log.iter().position(pred).unwrap();
    let reply = at(&|o| matches!(o, Op::Message(m) if m.content.contains("](artifact:")));
    let Op::Message(closing) = &log[reply] else {
        unreachable!()
    };
    let handle = |a: &Value| format!("](artifact:{})", a["artifactId"].as_str().unwrap());
    let placed = closing
        .content
        .lines()
        .find(|l| l.contains(&handle(&made[0])))
        .unwrap_or_else(|| panic!("{}", closing.content));
    let (before, after) = placed.split_once("![").unwrap();
    assert!(
        !before.trim().is_empty() && !after.split_once(')').unwrap().1.trim().is_empty(),
        "{placed}"
    );
    for other in &made[1..] {
        assert!(
            !closing.content.contains(&handle(other)),
            "{}",
            closing.content
        );
    }
    let artifact_at: Vec<usize> = made
        .iter()
        .map(|a| at(&|o| matches!(o, Op::Artifact(x) if a["artifactId"] == x.artifact_id.as_str())))
        .collect();
    assert!(
        artifact_at.windows(2).all(|w| w[0] < w[1]) && artifact_at[2] < reply,
        "artifact ops {artifact_at:?} must precede the reply at {reply}"
    );
    assert!(!log[reply + 1..].iter().any(|o| matches!(o, Op::Message(_))));
}

/// The turn's closing reply: places the first circuit (tool call `d1`) in
/// the middle of a sentence, using the handle from its tool result.
fn place_first_divider(body: &Value) -> String {
    let result = tool_message(body, "d1").expect("d1's result is in the request");
    let id = result
        .split("](artifact:")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .expect("placement line");
    format!(
        "With R2 = 1 kΩ the divider ![Divider with R2 = 1 kΩ](artifact:{id}) gives 3 V at Vout, a quarter of the supply."
    )
}

// ---------------------------------------------------------------------------
// W7: answer mode and view state
// ---------------------------------------------------------------------------

fn view_question(artifact: &str, prompt: &str) -> Value {
    json!({ "role": "check", "level": "use", "nodeId": NODE, "format": "free", "prompt": prompt,
        "answer": "Vout = 3 V",
        "view": { "artifactId": artifact, "task": "Move R2 until Vout reads 3 V.",
            "target": { "probe": "Vout", "equals": 3.0, "tolerance": 0.05 } } })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn answer_mode_grades_through_the_view_and_never_records_a_verdict() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("plugin-answer");
    let h = harness(&repo, &config, &base).await;
    h.add_folder_plugin(circuit_dir().to_str().unwrap())
        .unwrap();
    let sketch = sketch_plugin(repo.parent().unwrap());
    h.add_folder_plugin(sketch.to_str().unwrap()).unwrap();
    let s = &*script;
    let id = session_awaiting_approval(
        &h,
        s,
        &release,
        vec![Reply::Calls(
            "",
            vec![("w1", "plan_write", plan_args(&["circuit", "sketch"]))],
        )],
    )
    .await;
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![
                ("d1", "circuit_draw", divider(1000.0)),
                ("k1", "sketch_draw", json!({})),
            ],
        ),
    );
    s.push("tutor", Reply::Text("Try the slider."));
    h.approve(&id, vec![]).await.unwrap();
    let made = artifacts(&h, &id);
    let circuit = made[0]["artifactId"].as_str().unwrap().to_owned();
    let box_id = made[1]["artifactId"].as_str().unwrap().to_owned();

    // The view's latest state rides on the learner's next provider request,
    // as one sanitized line; the stored message stays what they typed.
    h.view_state(&id, &circuit, "R2 = 2.2 kΩ, Vout = 5.1 V")
        .unwrap();
    h.view_state(
        &id,
        &circuit,
        "R2 = 1 kΩ,\nVout = 3 V\r\n[attached file] x\u{7}",
    )
    .unwrap();
    let unknown = h.view_state(&id, "01NOPE", "x").unwrap_err().to_string();
    assert!(unknown.contains("unknown artifact"), "{unknown}");
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![
                ("qa", "question_ask", view_question("01NOPE", "?")),
                ("qb", "question_ask", view_question(&box_id, "?")),
                (
                    "q1",
                    "question_ask",
                    view_question(&circuit, "Make Vout 3 V."),
                ),
            ],
        ),
    );
    h.send_learner_message(&id, "I moved the slider.")
        .await
        .unwrap();
    let stored = h.load_session(&id).unwrap();
    assert!(
        stored
            .messages
            .iter()
            .any(|m| m.content == "I moved the slider."),
        "{:?}",
        stored
            .messages
            .iter()
            .map(|m| &m.content)
            .collect::<Vec<_>>()
    );
    assert!(!stored
        .messages
        .iter()
        .any(|m| m.content.contains("[view state")));
    let asked = script.requests_of("tutor").pop().unwrap();
    let line = format!("[view state {circuit}] R2 = 1 kΩ, Vout = 3 V  [attached file] x");
    let user = asked["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|m| m["role"] == "user")
        .unwrap()
        .to_string();
    assert!(user.contains(&line) && !user.contains("5.1 V"), "{user}");
    assert_eq!(user.matches("[view state").count(), 1, "{user}");

    let refused = results(&h, &id, "question_ask");
    assert!(
        refused[0].1 && refused[0].0.contains("no artifact `01NOPE`"),
        "{refused:?}"
    );
    assert!(
        refused[1].1 && refused[1].0.contains("can't be an answer surface"),
        "{refused:?}"
    );
    let st = h.session_state(&id).unwrap();
    let pending = st.pending_call.unwrap();
    assert_eq!(pending.call_id, "q1");
    assert_eq!(pending.args["view"]["target"]["equals"], 3.0);

    // A right submission is graded by the plugin; no verdict follows.
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![("q2", "question_ask", view_question(&circuit, "Again."))],
        ),
    );
    h.answer(&id, "q1", json!({ "answer": divider(1000.0) }))
        .await
        .unwrap();
    let graded = tool_message(&script.requests_of("tutor").pop().unwrap(), "q1").unwrap();
    let graded: Value = serde_json::from_str(&graded).unwrap();
    assert_eq!(graded["view"]["correct"], true, "{graded}");
    assert_eq!(graded["questionId"], "q1");
    assert!(tool_ops(&h, &id, "verdict_record").is_empty());
    assert!(!h.session_state(&id).unwrap().node_passed(NODE));

    // A wrong one is graded false; the tutor's own verdict_record counts.
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "v1",
                "verdict_record",
                json!({ "questionId": "q1", "verdict": "correct", "note": "3.02 V" }),
            )],
        ),
    );
    s.push("tutor", Reply::Text("Good."));
    h.answer(&id, "q2", json!({ "answer": divider(3000.0) }))
        .await
        .unwrap();
    let requests = script.requests_of("tutor");
    let wrong = tool_message(&requests[requests.len() - 2], "q2").unwrap();
    let wrong: Value = serde_json::from_str(&wrong).unwrap();
    assert_eq!(wrong["view"]["correct"], false, "{wrong}");
    assert_eq!(
        results(&h, &id, "verdict_record"),
        [("Verdict recorded.".to_owned(), false)]
    );
    assert!(h.session_state(&id).unwrap().node_passed(NODE));

    // A disabled plugin serves no view, no bridge call and no grading; the
    // question stays open until it is enabled again.
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![("q3", "question_ask", view_question(&circuit, "Once more."))],
        ),
    );
    h.send_learner_message(&id, "Another one?").await.unwrap();
    assert_eq!(
        h.session_state(&id).unwrap().pending_call.unwrap().call_id,
        "q3"
    );
    h.set_plugin_enabled("circuit", false).unwrap();
    let err = h.plugin_view("circuit").unwrap_err().to_string();
    assert!(err.contains("plugin `circuit` is disabled"), "{err}");
    let err = h
        .plugin_view_call("circuit", "solve", divider(1000.0))
        .unwrap_err()
        .to_string();
    assert!(err.contains("plugin `circuit` is disabled"), "{err}");
    let submit = json!({ "answer": divider(1000.0) });
    let err = h
        .answer(&id, "q3", submit.clone())
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("the plugin that made this view is disabled"),
        "{err}"
    );
    assert_eq!(
        h.session_state(&id).unwrap().pending_call.unwrap().call_id,
        "q3"
    );
    h.set_plugin_enabled("circuit", true).unwrap();
    s.push("tutor", Reply::Text("Right."));
    h.answer(&id, "q3", submit).await.unwrap();

    // ... and can't be named as a question's answer surface.
    h.set_plugin_enabled("circuit", false).unwrap();
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![("q4", "question_ask", view_question(&circuit, "Last."))],
        ),
    );
    s.push("tutor", Reply::Text("We'll do it in words."));
    h.send_learner_message(&id, "More?").await.unwrap();
    let (refusal, is_error) = results(&h, &id, "question_ask").pop().unwrap();
    assert!(
        is_error && refusal.contains(&format!("the plugin that made `{circuit}` is disabled")),
        "{refusal}"
    );

    // ... nor once its section is over (W7: an active plugin only).
    h.set_plugin_enabled("circuit", true).unwrap();
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![
                (
                    "e1",
                    "section_done",
                    json!({ "sectionId": "s1", "evidence": "3.02 V" }),
                ),
                ("q5", "question_ask", view_question(&circuit, "After.")),
            ],
        ),
    );
    s.push("tutor", Reply::Text("Done."));
    h.send_learner_message(&id, "Finished?").await.unwrap();
    assert!(!results(&h, &id, "section_done").pop().unwrap().1);
    let (refusal, is_error) = results(&h, &id, "question_ask").pop().unwrap();
    assert!(
        is_error && refusal.contains("isn't active in this section"),
        "{refusal}"
    );
    assert!(script.unexpected.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn view_bridge_calls_are_allow_listed_and_solve_fits_the_budget() {
    let (repo, config) = temp_dirs("plugin-bridge");
    let h = Harness::open(&repo, &config).unwrap();
    h.add_folder_plugin(circuit_dir().to_str().unwrap())
        .unwrap();
    for r2 in [1000.0, 3000.0] {
        let started = Instant::now();
        let out = h.plugin_view_call("circuit", "solve", divider(r2)).unwrap();
        let took = started.elapsed();
        assert!(took < Duration::from_millis(200), "solve took {took:?}");
        let want = 12.0 * r2 / (3000.0 + r2);
        assert!(
            (out["probes"]["Vout"].as_f64().unwrap() - want).abs() < 1e-6,
            "{out}"
        );
    }
    for function in ["grade", "circuit_draw"] {
        let err = h
            .plugin_view_call("circuit", function, divider(1000.0))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("not one of the view's callable functions"),
            "{err}"
        );
    }
    assert!(h.plugin_view_call("nope", "solve", json!({})).is_err());
}

/// A folder plugin `tally` whose view function `count` returns `n`.
fn tally_plugin(root: &Path, description: &str, n: u32) -> PathBuf {
    let dir = root.join("tally");
    for sub in ["schemas", "src", "views"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }
    std::fs::write(
        dir.join("plugin.toml"),
        format!(
            r#"id = "tally"
version = "0.1.0"
api = 1
title = "Tally"
description = "{description}"
license = "MIT"
platforms = ["desktop", "mobile"]
permissions = []
use_when = "Counting."
avoid_when = "Never."
cost = "instant"

[[tool]]
name = "tally_count"
description = "Count."
agents = ["tutor"]
input = "schemas/in.json"
entry = "count"

[[view]]
name = "tally"
html = "views/tally.html"
calls = ["count"]
"#
        ),
    )
    .unwrap();
    std::fs::write(dir.join("schemas/in.json"), r#"{"type":"object"}"#).unwrap();
    std::fs::write(dir.join("views/tally.html"), "<p>tally</p>").unwrap();
    std::fs::write(
        dir.join("src/main.luau"),
        format!("return {{ count = function() return {n} end }}\n"),
    )
    .unwrap();
    dir
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_folder_plugin_reloads_when_its_files_change() {
    let (repo, config) = temp_dirs("plugin-folder");
    let h = Harness::open(&repo, &config).unwrap();
    let root = repo.parent().unwrap().to_owned();
    let dir = tally_plugin(&root, "Counts things.", 1);
    h.add_folder_plugin(dir.to_str().unwrap()).unwrap();
    assert_eq!(h.plugin_view_call("tally", "count", json!({})).unwrap(), 1);
    let first_hash = h.plugins()[0].package_hash.clone();

    // An edit on disk is picked up by the next use: manifest, code, hash.
    tally_plugin(&root, "Counts things twice over.", 22);
    assert_eq!(h.plugin_view_call("tally", "count", json!({})).unwrap(), 22);
    let info = &h.plugins()[0];
    assert_eq!(info.description, "Counts things twice over.");
    assert_ne!(info.package_hash, first_hash);
    assert!(info.load_error.is_none());

    // A broken edit keeps the last good version and says why.
    std::fs::write(
        dir.join("plugin.toml"),
        "id = \"tally\"\nversion = \"oops\"\n",
    )
    .unwrap();
    assert_eq!(h.plugin_view_call("tally", "count", json!({})).unwrap(), 22);
    let info = &h.plugins()[0];
    assert_eq!(info.description, "Counts things twice over.");
    let err = info.load_error.clone().expect("the error is reported");
    assert!(err.contains("plugin.toml"), "{err}");

    // Fixing it clears the error.
    tally_plugin(&root, "Counts again.", 333);
    assert_eq!(
        h.plugin_view_call("tally", "count", json!({})).unwrap(),
        333
    );
    assert!(h.plugins()[0].load_error.is_none());
}

// ---------------------------------------------------------------------------
// W8 + W9: local registry, lock, planner search, install-later grants
// ---------------------------------------------------------------------------

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// `circuit-<version>.tar.gz` in `dir` (the repo package with its manifest
/// version set to `version`, then passed through `edit`); returns its
/// `file://` URL and sha256.
fn circuit_archive(dir: &Path, version: &str, edit: fn(String) -> String) -> (String, String) {
    let package = dir.join(format!("src-{version}"));
    copy_tree(&circuit_dir(), &package);
    let manifest = std::fs::read_to_string(package.join("plugin.toml")).unwrap();
    let manifest = manifest.replacen(
        "version = \"0.1.0\"",
        &format!("version = \"{version}\""),
        1,
    );
    std::fs::write(package.join("plugin.toml"), edit(manifest)).unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    {
        let mut tar = tar::Builder::new(&mut gz);
        tar.append_dir_all("circuit", &package).unwrap();
        tar.finish().unwrap();
    }
    let bytes = gz.finish().unwrap();
    let archive = dir.join(format!("circuit-{version}.tar.gz"));
    std::fs::File::create(&archive)
        .unwrap()
        .write_all(&bytes)
        .unwrap();
    (format!("file://{}", archive.display()), sha256_hex(&bytes))
}

/// A registry directory whose `index.json` lists circuit 0.1.0 and 0.2.0
/// (correct hashes), 0.1.1 (the 0.1.0 archive under a wrong hash) and 0.0.9
/// (correct hash, invalid manifest). Returns the `file://` index URL.
fn local_registry(root: &Path) -> String {
    let dir = root.join("registry");
    std::fs::create_dir_all(&dir).unwrap();
    let (url, sha) = circuit_archive(&dir, "0.1.0", |m| m);
    let (url2, sha2) = circuit_archive(&dir, "0.2.0", |m| m);
    let (broken, broken_sha) = circuit_archive(&dir, "0.0.9", |m| {
        m.replacen("permissions = []", "permissions = [\"net\"]", 1)
    });
    let index = json!({ "registry": 1, "name": "Test", "plugins": [{
        "id": "circuit", "title": "Circuits",
        "description": "Draw, solve and explore DC and AC circuits.",
        "use_when": "Voltages and currents in a circuit.", "avoid_when": "Words only.",
        "platforms": ["desktop", "mobile"], "permissions": [],
        "versions": [
            { "version": "0.0.9", "api": 1, "url": broken, "sha256": broken_sha },
            { "version": "0.1.0", "api": 1, "url": url, "sha256": sha },
            { "version": "0.1.1", "api": 1, "url": url, "sha256": "0".repeat(64) },
            { "version": "0.2.0", "api": 1, "url": url2, "sha256": sha2 }
        ] }] });
    let path = dir.join("index.json");
    std::fs::write(&path, index.to_string()).unwrap();
    format!("file://{}", path.display())
}

/// Add circuit `version` (a correct archive) to the registry's index.json,
/// as a registry does after a release.
fn publish(root: &Path, version: &str) {
    let dir = root.join("registry");
    let (url, sha) = circuit_archive(&dir, version, |m| m);
    let path = dir.join("index.json");
    let mut index: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    index["plugins"][0]["versions"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "version": version, "api": 1, "url": url, "sha256": sha }));
    std::fs::write(&path, index.to_string()).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_planner_proposes_an_uninstalled_plugin_and_install_grants_it_later() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("plugin-registry");
    let h = harness(&repo, &config, &base).await;
    let registry = local_registry(repo.parent().unwrap());
    h.add_registry("Test", &registry).unwrap();
    let fetched = h.refresh_registries().await;
    assert!(fetched[0].ok && fetched[0].plugins == 1, "{fetched:?}");
    let s = &*script;

    // The planner searches the cached index, then plans with circuit.
    let id = session_awaiting_approval(
        &h,
        s,
        &release,
        vec![
            Reply::Calls(
                "",
                vec![("ps", "plugin_search", json!({ "query": "Circuit" }))],
            ),
            Reply::Calls("", vec![("w1", "plan_write", plan_args(&["circuit"]))]),
        ],
    )
    .await;
    let found = results(&h, &id, "plugin_search");
    let hits: Value = serde_json::from_str(&found[0].0).unwrap();
    assert_eq!(hits[0]["id"], "circuit", "{hits}");
    assert_eq!(hits[0]["version"], "0.2.0", "the highest listed version");
    assert_eq!(hits[0]["registry"], registry);
    assert_eq!(hits[0]["permissions"], json!([]));
    let plan = ops(&h, &id)
        .into_iter()
        .find_map(|o| match o {
            Op::Plan(p) => serde_json::to_value(p).ok(),
            _ => None,
        })
        .unwrap();
    let raw = plan.to_string();
    assert!(raw.contains(r#""needsInstall":["circuit"]"#), "{raw}");
    assert!(raw.contains(r#""plugins":["circuit"]"#), "{raw}");
    let st = h.session_state(&id).unwrap();
    assert_eq!(st.plan.unwrap().sections[0].needs_install, ["circuit"]);

    // Approving installs nothing: the section starts without it.
    s.push("tutor", Reply::Text("We'll work it out by hand."));
    h.approve(&id, vec![]).await.unwrap();
    let st = h.session_state(&id).unwrap();
    assert!(st.active_plugins.is_empty());
    assert!(h.plugins().is_empty());
    assert!(ops(&h, &id).iter().any(|o| matches!(o, Op::Message(m)
        if m.tag.as_deref() == Some("plan-notice")
            && m.content.contains("Left out until installed: Dividers (circuit)"))));
    let first_teach = script.requests_of("tutor").pop().unwrap();
    assert!(!tool_names(&first_teach).contains(&"circuit_draw".to_owned()));

    // A hash mismatch is refused and leaves nothing behind.
    let err = h
        .install_plugin(&registry, "circuit", "0.1.1")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("sha256") && err.contains("refusing to install"),
        "{err}"
    );
    assert!(h.plugin_lock().unwrap().is_empty());
    // So is a package that fails validation after unpacking.
    let err = h
        .install_plugin(&registry, "circuit", "0.0.9")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("permissions must be empty"), "{err}");
    assert!(!config.join("plugins").join("circuit").exists());
    assert!(h.plugin_lock().unwrap().is_empty() && h.plugins().is_empty());

    // Install writes the lock; then plugin_request grants it mid-section.
    let info = h
        .install_plugin(&registry, "circuit", "0.1.0")
        .await
        .unwrap();
    assert_eq!(
        (info.id.as_str(), info.version.as_str()),
        ("circuit", "0.1.0")
    );
    let lock = h.plugin_lock().unwrap();
    assert_eq!(lock.len(), 1);
    assert_eq!(lock[0].id, "circuit");
    let locked = std::fs::read_to_string(repo.join("plugins.lock")).unwrap();
    assert!(locked.contains(&registry), "{locked}");
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "r1",
                "plugin_request",
                json!({ "plugin": "circuit", "reason": "draw it" }),
            )],
        ),
    );
    s.push("tutor", Reply::Text("Now I can draw it."));
    h.send_learner_message(&id, "Can you draw it?")
        .await
        .unwrap();
    let granted = results(&h, &id, "plugin_request");
    assert!(
        !granted[0].1 && granted[0].0.starts_with("Granted"),
        "{granted:?}"
    );
    assert_eq!(h.session_state(&id).unwrap().active_plugins, ["circuit"]);
    let after = script.requests_of("tutor").pop().unwrap();
    assert!(tool_names(&after).contains(&"circuit_draw".to_owned()));

    // No update while circuit is active in a session's current section (W8),
    // whether a turn is running or not.
    let gate = Arc::new(Semaphore::new(0));
    s.push(
        "tutor",
        Reply::Wait(
            Arc::clone(&gate),
            Box::new(Reply::Text("Thinking it over.")),
        ),
    );
    let before = script.requests_of("tutor").len();
    let turn = {
        let (h, id) = (Arc::clone(&h), id.clone());
        tokio::spawn(async move { h.send_learner_message(&id, "Go on.").await })
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        while script.requests_of("tutor").len() == before {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the turn reached the model");
    let refused = |err: String| {
        assert!(err.contains("is in use in the current section"), "{err}");
    };
    refused(
        h.install_plugin(&registry, "circuit", "0.2.0")
            .await
            .unwrap_err()
            .to_string(),
    );
    assert_eq!(h.plugins()[0].version, "0.1.0");
    gate.add_permits(1);
    turn.await.unwrap().unwrap();
    refused(
        h.install_plugin(&registry, "circuit", "0.2.0")
            .await
            .unwrap_err()
            .to_string(),
    );
    assert_eq!(h.plugins()[0].version, "0.1.0");

    // Once the session leaves the section, updates are allowed again.
    s.push(
        "tutor",
        Reply::Calls(
            "That's all for today.",
            vec![("end", "session_end", json!({}))],
        ),
    );
    // Closing starts the graph builder; an empty submission ends it at once.
    s.push(
        "graph",
        Reply::Calls("", vec![("g1", "graph_submit", json!({}))]),
    );
    h.send_learner_message(&id, "Let's stop here.")
        .await
        .unwrap();
    h.wait_background(&id).await;
    assert!(h.session_state(&id).unwrap().ended);

    // A version published after the index was cached is fetched on install.
    publish(repo.parent().unwrap(), "0.3.0");

    // Two installed versions side by side: a restart loads the locked one.
    let version_after_restart = |h: Arc<Harness>| {
        drop(h);
        let h = Arc::new(Harness::open(&repo, &config).unwrap());
        let info = h.plugins().into_iter().find(|p| p.id == "circuit").unwrap();
        (h, info.version)
    };
    h.install_plugin(&registry, "circuit", "0.3.0")
        .await
        .unwrap();
    let (h, version) = version_after_restart(h);
    assert_eq!(version, "0.3.0");
    h.install_plugin(&registry, "circuit", "0.1.0")
        .await
        .unwrap();
    let (h, version) = version_after_restart(h);
    assert_eq!(version, "0.1.0");
    assert_eq!(h.plugin_lock().unwrap()[0].version, "0.1.0");

    // Removal updates the lock; a deleted copy is reported from the lock.
    h.remove_plugin("circuit").unwrap();
    assert!(h.plugin_lock().unwrap().is_empty());
    assert!(!std::fs::read_to_string(repo.join("plugins.lock"))
        .unwrap()
        .contains("circuit"));
    h.install_plugin(&registry, "circuit", "0.1.0")
        .await
        .unwrap();
    std::fs::remove_dir_all(config.join("plugins").join("circuit")).unwrap();
    drop(h);
    let h = Harness::open(&repo, &config).unwrap();
    assert!(h.plugins().is_empty());
    let missing = h.missing_locked().unwrap();
    assert_eq!(missing.len(), 1);
    assert_eq!(
        (missing[0].id.as_str(), missing[0].version.as_str()),
        ("circuit", "0.1.0")
    );
    assert!(script.unexpected.lock().unwrap().is_empty());
}

/// The UI command returns only bridge metadata; the inlined view document is
/// served separately by the `pluginview` scheme, never over the command
/// channel (W7 clean cutover, 2026-11-07).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_view_returns_metadata_and_document_is_served_separately() {
    let (repo, config) = temp_dirs("view-payload");
    let h = Harness::open(&repo, &config).unwrap();
    let root = repo.parent().unwrap().to_owned();
    let dir = tally_plugin(&root, "Counts things.", 1);
    h.add_folder_plugin(dir.to_str().unwrap()).unwrap();

    let meta = h.plugin_view("tally").unwrap();
    assert_eq!(meta.name, "tally");
    assert_eq!(meta.calls, ["count"]);
    assert!(!meta.answer);
    let as_json = serde_json::to_value(&meta).unwrap();
    assert!(
        as_json.get("html").is_none(),
        "html leaked into the UI payload: {as_json}"
    );

    // The document still exists, but only through the scheme path.
    let doc = String::from_utf8(h.view_document("tally").unwrap()).unwrap();
    assert!(doc.contains("<p>tally</p>"), "{doc}");
}
