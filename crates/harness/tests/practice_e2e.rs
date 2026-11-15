//! End-to-end proof of the practice backend (architecture §4.11–4.13): a
//! scripted coach and a scripted problem checker drive real sessions through
//! the scheduler, the commit gates, the checks, the learner-safe view, the
//! outcome log and a restart.

mod common;

use std::sync::{Arc, Mutex, Once};

use common::*;
use harness::graph::{append, provenance, GraphOp};
use harness::plugins::{Plugin, PluginContext, ToolFuture, ToolOutcome};
use harness::practice::SetAnswer;
use harness::providers::ToolSpec;
use harness::state::Kind;
use harness::{AgentEvent, Harness, PluginManifest};
use serde_json::{json, Value};
use tokio::sync::Semaphore;

const A: &str = "divider-rule";
const OHM: &str = "ohm";
const KVL: &str = "kvl";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Two topics: `ohm` (basics) feeds `divider-rule` and `kvl` (circuits).
fn seed_graph(repo: &std::path::Path) {
    let node = |id: &str, title: &str, home: &str| {
        GraphOp::node(
            id,
            Some(title),
            "A statement.",
            Kind::Mechanism,
            home,
            Some(provenance("import", None, "seed")),
        )
    };
    append(
        repo,
        &[
            GraphOp::topic("basics", "Basics", None),
            GraphOp::topic("circuits", "Circuits", None),
            node(OHM, "Ohm's law", "basics"),
            node(A, "The divider rule", "circuits"),
            node(KVL, "Kirchhoff's voltage law", "circuits"),
            GraphOp::edge(OHM, A, "requires", None, None, None),
            GraphOp::edge(OHM, KVL, "requires", None, None, None),
        ],
    )
    .unwrap();
}

/// A graded answer `hours` ago, appended to the outcome log.
fn seed_outcome(repo: &std::path::Path, id: &str, node: &str, layer: &str, hours: i64) {
    let at = (chrono::Utc::now() - chrono::Duration::hours(hours))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let line = json!({
        "opId": id, "at": at, "sessionId": "old", "problemId": id, "targets": [node],
        "hosts": [], "layer": layer, "format": "check", "verdict": "correct",
        "confidence": 70, "hintRung": null, "grader": "committed", "durationSec": 60
    });
    let path = repo.join("practice/outcomes.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    text.push_str(&format!("{line}\n"));
    std::fs::write(path, text).unwrap();
}

fn outcomes(repo: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(repo.join("practice/outcomes.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Every op of a session as the file holds it (the learner-facing
/// `load_session` hides the private ones).
fn raw_ops(repo: &std::path::Path, id: &str) -> Vec<Value> {
    fn find(dir: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let p = entry.path();
            if p.is_dir() {
                if let Some(f) = find(&p, name) {
                    return Some(f);
                }
            } else if p.file_name().is_some_and(|n| n == name) {
                return Some(p);
            }
        }
        None
    }
    let file = find(&repo.join("sessions"), &format!("{id}.jsonl")).expect("session file");
    std::fs::read_to_string(file)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// The result texts of a coach tool as the log holds them.
fn raw_results(repo: &std::path::Path, id: &str, tool: &str) -> Vec<String> {
    raw_ops(repo, id)
        .into_iter()
        .filter(|o| o["op"] == "tool" && o["name"] == tool && o.get("result").is_some())
        .map(|o| o["result"]["content"].as_str().unwrap().to_owned())
        .collect()
}

fn practice_events(repo: &std::path::Path, id: &str, event: &str) -> Vec<Value> {
    raw_ops(repo, id)
        .into_iter()
        .filter(|o| o["op"] == "practice" && o["event"] == event)
        .collect()
}

/// Everything the model was sent in one request, as plain text.
fn texts(body: &Value) -> String {
    let mut out = String::new();
    for m in body["messages"].as_array().into_iter().flatten() {
        match &m["content"] {
            Value::String(s) => out.push_str(s),
            Value::Array(parts) => {
                for p in parts {
                    out.push_str(p["text"].as_str().unwrap_or(""));
                }
            }
            _ => {}
        }
        out.push('\n');
    }
    out
}

/// The `next:` request the app wrote to the coach, parsed.
fn next_request(body: &Value) -> Value {
    let all = texts(body);
    let at = all.rfind("next: ").expect("a next: request");
    let rest = &all[at + "next: ".len()..];
    let mut stream = serde_json::Deserializer::from_str(rest).into_iter::<Value>();
    stream.next().unwrap().unwrap()
}

fn commit(target: &str, layer: &str, cue: u64, surface: &str, answer: &str) -> Value {
    json!({
        "statement": format!("A 12 V source feeds two parts in a row ({surface}). Find the voltage across the second part."),
        "targets": [target], "hosts": [],
        "layer": layer, "level": "use", "cueDistance": cue, "isDistractor": false,
        "solution": [
            { "move": "Find the current through the chain", "usesNode": "ohm", "decisionPoint": false },
            { "move": "Notice the supply splits in proportion", "usesNode": target, "decisionPoint": true }
        ],
        "finalAnswer": answer, "acceptableVariants": [], "surface": surface
    })
}

fn call(id: &'static str, name: &'static str, args: Value) -> Reply {
    Reply::Calls("", vec![(id, name, args)])
}

fn audit(id: &'static str, obvious: &str, technique: &str, confidence: f64) -> Reply {
    call(
        id,
        "audit_submit",
        json!({
            "techniques": [{ "name": technique, "confidence": confidence, "cuedBy": "the sources" }],
            "firstMove": "set up the loop", "obviousAfter": obvious
        }),
    )
}

fn necessity(id: &'static str, without: bool, route: &str) -> Reply {
    call(
        id,
        "necessity_submit",
        json!({ "solvedWithout": without, "route": route }),
    )
}

fn conclude(id: &'static str, args: Value) -> Reply {
    call(id, "problem_conclude", args)
}

fn collect_events(h: &Harness) -> Arc<Mutex<Vec<(String, Value)>>> {
    let events: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
    let sink = Arc::clone(&events);
    h.set_event_sink(Some(Arc::new(move |session, ev: AgentEvent| {
        sink.lock()
            .unwrap()
            .push((session.to_owned(), serde_json::to_value(&ev).unwrap()));
    })));
    events
}

/// A python3 stand-in first on PATH: it runs the check script as a shell
/// script (`echo 150`), with markers to play a missing SymPy.
fn fake_python() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("lh-fake-python-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let program = dir.join("python3");
        std::fs::write(
            &program,
            r#"#!/bin/sh
case "$2" in
  main.py)
    if grep -q NOSYMPY check.py; then echo "ModuleNotFoundError: No module named 'sympy'" >&2; exit 1; fi
    exec sh check.py ;;
  check.py)
    if grep -q NEEDSYMPY check.py; then echo "ModuleNotFoundError: No module named 'sympy'" >&2; exit 1; fi
    exec sh check.py ;;
  equivalent.py)
    in=$(cat)
    a=$(printf '%s' "$in" | sed -n 's/.*"a": *"\([^"]*\)".*/\1/p')
    b=$(printf '%s' "$in" | sed -n 's/.*"b": *"\([^"]*\)".*/\1/p')
    if [ "$a" = "$b" ]; then echo '{"result": "equal", "detail": "0"}'; else echo '{"result": "different", "detail": "x"}'; fi ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut paths = vec![dir];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
    });
}

struct Rig {
    script: Arc<Script>,
    h: Arc<Harness>,
    repo: std::path::PathBuf,
    config: std::path::PathBuf,
    secrets: Arc<harness::secrets::SecretStore>,
}

async fn rig(name: &str) -> Rig {
    fake_python();
    let script = Arc::new(Script::default());
    let base = serve(Arc::clone(&script), Arc::new(Semaphore::new(0))).await;
    let (repo, config) = temp_dirs(name);
    seed_graph(&repo);
    let secrets = Arc::new(harness::secrets::SecretStore::memory());
    let h = harness_with_secrets(&repo, &config, &base, Arc::clone(&secrets)).await;
    Rig {
        script,
        h,
        repo,
        config,
        secrets,
    }
}

fn queue(script: &Script, agent: &'static str, replies: Vec<Reply>) {
    for r in replies {
        script.push(agent, r);
    }
}

// ---------------------------------------------------------------------------
// The whole stream: selection, pose, hint, conclude, outcome, next
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stream_runs_from_selection_to_outcome_to_the_next_problem() {
    let r = rig("practice-stream").await;
    // `ohm` was fully shown an hour ago; the other two ideas have no record:
    // the scheduler picks the first of those (load-bearing ties by id).
    for (i, layer) in ["recall", "discrimination", "composition"]
        .iter()
        .enumerate()
    {
        seed_outcome(&r.repo, &format!("seed{i}"), OHM, layer, 1);
    }
    let events = collect_events(&r.h);
    queue(
        &r.script,
        "coach",
        vec![
            Reply::Calls(
                "PRIVATE-REASONING: the answer is FINAL-6V.",
                vec![(
                    "c1",
                    "problem_commit",
                    commit(A, "recall", 1, "a lamp chain", "FINAL-6V"),
                )],
            ),
            Reply::Text("Re-read what the 12 V has to cover."),
            conclude(
                "c2",
                json!({ "verdict": "correct", "hintRung": 1, "note": "needed a nudge to see the split" }),
            ),
            Reply::Text("Correct. Splitting the supply was the key step."),
            call(
                "c3",
                "problem_commit",
                commit(KVL, "recall", 1, "a sensor input", "FINAL-2V"),
            ),
        ],
    );
    queue(
        &r.script,
        "checker",
        vec![
            audit("a1", "not obvious", "Ohm's law", 0.8),
            necessity("n1", false, "every route needs the split"),
            audit("a2", "not obvious", "Ohm's law", 0.7),
            necessity("n2", false, "needs the split"),
        ],
    );

    let summary = r.h.start_practice(&[], "stream", None).await.unwrap();
    let id = summary.id;
    assert_eq!(summary.mode, harness::Mode::Practice);

    // Target selection: the app asked for the idea with no record, new, at
    // cue 1, in its own words, and told the coach the held ideas and hosts.
    let coach = r.script.requests_of("coach");
    let next = next_request(&coach[0]);
    assert_eq!(next["target"], A, "{next}");
    assert_eq!(next["layer"], "recall");
    assert_eq!(next["level"], "use");
    assert_eq!(next["cueDistance"], 1);
    assert_eq!(next["isDistractor"], false);
    let system = coach[0]["messages"][0]["content"].as_str().unwrap();
    assert!(
        system.contains("# Practice coach") && !system.contains("{{"),
        "{system}"
    );
    assert!(system.contains("## Write now") && system.contains("## Host candidates"));
    assert!(
        system.contains("`ohm` — Ohm's law"),
        "ohm is held and a host candidate: {system}"
    );
    for tool in [
        "problem_commit",
        "problem_conclude",
        "research_request",
        "graph_read",
    ] {
        assert!(tool_names(&coach[0]).contains(&tool.to_owned()), "{tool}");
    }

    // The checker read the statement blind, then was told the target.
    let checker = r.script.requests_of("checker");
    let first = texts(&checker[0]) + checker[0]["messages"][0]["content"].as_str().unwrap();
    assert!(
        first.contains("a lamp chain") && !first.to_lowercase().contains("divider"),
        "{first}"
    );
    assert!(!first.contains("Target:"));
    assert!(
        texts(&checker[1]).contains("Target: The divider rule"),
        "{}",
        texts(&checker[1])
    );

    // The problem is posed without the idea named anywhere the learner sees.
    let view = serde_json::to_value(r.h.practice_state(&id).unwrap()).unwrap();
    assert_eq!(view["status"], "open");
    assert_eq!(view["problems"][0]["status"], "open");
    assert!(view["problems"][0]["statement"]
        .as_str()
        .unwrap()
        .contains("a lamp chain"));
    assert_eq!(view["problems"][0]["answerCheck"], "none");
    assert!(view["problems"][0].get("result").is_none());
    let shown = view.to_string();
    for secret in [
        "FINAL-6V",
        "PRIVATE-REASONING",
        "divider-rule",
        "divider rule",
    ] {
        assert!(
            !shown.contains(secret),
            "{secret} leaked into the view: {shown}"
        );
    }
    assert!(view["can"]["hint"].as_bool().unwrap() && !view["can"]["next"].as_bool().unwrap());
    let session = serde_json::to_string(&r.h.load_session(&id).unwrap()).unwrap();
    for secret in [
        "FINAL-6V",
        "PRIVATE-REASONING",
        "divider-rule",
        "needs the split",
    ] {
        assert!(
            !session.contains(secret),
            "{secret} leaked into load_session"
        );
    }
    {
        // Nothing the coach wrote while writing was streamed or sent as an op.
        let seen = events.lock().unwrap();
        let all: String = seen.iter().map(|(_, e)| e.to_string()).collect();
        assert!(
            !all.contains("FINAL-6V") && !all.contains("PRIVATE-REASONING"),
            "{all}"
        );
        assert!(
            !seen.iter().any(|(_, e)| e["kind"] == "text"),
            "no text while writing"
        );
        assert!(seen
            .iter()
            .any(|(_, e)| e["kind"] == "turnStart" && e["agent"] == "coach"));
        assert!(seen.iter().any(|(_, e)| e["kind"] == "op"
            && e["op"]["op"] == "practice"
            && e["op"]["event"] == "pose"));
    }

    // A hint: the coach streams its reply now.
    r.h.practice_hint(&id).await.unwrap();
    {
        let seen = events.lock().unwrap();
        assert!(seen
            .iter()
            .any(|(_, e)| e["kind"] == "text" && e["agent"] == "coach"));
    }
    assert_eq!(r.h.practice_state(&id).unwrap().problems[0].hints, 1);

    // The answer, with a confidence; the coach concludes with the rung it gave.
    r.h.practice_submit(&id, "6 V", Some(70), &[])
        .await
        .unwrap();
    let coach = r.script.requests_of("coach");
    assert!(texts(coach.last().unwrap()).contains("[confidence: 70% — Fairly sure]"));
    let log = outcomes(&r.repo);
    let mine: Vec<&Value> = log
        .iter()
        .filter(|o| o["sessionId"] == id.as_str())
        .collect();
    assert_eq!(mine.len(), 1, "{log:?}");
    let o = mine[0];
    assert_eq!(o["targets"], json!([A]));
    assert_eq!(
        (o["layer"].as_str(), o["format"].as_str()),
        (Some("recall"), Some("stream"))
    );
    assert_eq!(
        (o["verdict"].as_str(), o["hintRung"].as_u64()),
        (Some("correct"), Some(1))
    );
    assert_eq!(
        (o["confidence"].as_u64(), o["cueDistance"].as_u64()),
        (Some(70), Some(1))
    );
    assert_eq!(o["grader"], "committed");
    assert_eq!(o["level"], "use");
    let nodes = r.h.learner_nodes().unwrap();
    assert!(nodes[A].layers.recall, "a success shows the layer");
    assert_eq!(nodes[A].reviews, 1);

    // After the conclusion the learner sees the result and the solution.
    let view = serde_json::to_value(r.h.practice_state(&id).unwrap()).unwrap();
    assert_eq!(view["status"], "concluded");
    let result = &view["problems"][0]["result"];
    assert_eq!(result["verdict"], "correct");
    assert_eq!(result["rating"], "hard", "R1 is a success with help: Hard");
    assert_eq!(result["targets"][0]["id"], A);
    assert_eq!(result["reveal"]["finalAnswer"], "FINAL-6V");
    assert!(
        result.get("note").is_none(),
        "the coach's diagnosis stays private"
    );
    assert!(view["can"]["next"].as_bool().unwrap());
    let kinds: Vec<&str> = view["feed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "problem",
            "learner",
            "coach",
            "learner",
            "concluded",
            "coach"
        ]
    );
    assert_eq!(view["feed"][1]["intent"], "hint");
    assert_eq!(view["feed"][3]["confidence"], 70);

    // The next problem: mixed, not blocked. The idea just practised waits and
    // the other one with no record comes up, at its own starting point.
    r.h.practice_next(&id).await.unwrap();
    let coach = r.script.requests_of("coach");
    let next = next_request(coach.last().unwrap());
    assert_eq!(next["target"], KVL, "{next}");
    assert_eq!(next["layer"], "recall", "{next}");
    assert_eq!(next["cueDistance"], 1, "{next}");
    assert_eq!(r.h.practice_state(&id).unwrap().problems.len(), 2);
    assert!(
        r.script.unexpected.lock().unwrap().is_empty(),
        "{:?}",
        r.script.unexpected.lock().unwrap()
    );
}

// ---------------------------------------------------------------------------
// Commit gates, checker rejection, cue cap, difficulty step down
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refusals_come_back_privately_and_a_named_target_lowers_the_cue_distance() {
    let r = rig("practice-gates").await;
    // The divider rule was recalled an hour ago: its next layer is
    // discrimination, at cue 2 with `ohm` as a host.
    seed_outcome(&r.repo, "seed0", A, "recall", 1);
    let events = collect_events(&r.h);

    let mut bad = commit(A, "discrimination", 2, "a sensor", "FINAL-4V");
    bad["targets"] = json!(["nope"]);
    bad["solution"][1]["decisionPoint"] = json!(false);
    bad["solution"][0]["decisionPoint"] = json!(false);
    queue(
        &r.script,
        "coach",
        vec![
            call("c1", "problem_commit", bad),
            call(
                "c2",
                "problem_commit",
                commit(A, "discrimination", 2, "a sensor", "FINAL-4V"),
            ),
            call(
                "c3",
                "problem_commit",
                commit(A, "discrimination", 2, "a heater", "FINAL-4V"),
            ),
            conclude(
                "c4",
                json!({ "verdict": "wrong", "hintRung": null, "missClass": "execution", "note": "slip" }),
            ),
            Reply::Text("Not quite."),
            call(
                "c5",
                "problem_commit",
                commit(A, "discrimination", 0, "a lamp", "FINAL-1V"),
            ),
        ],
    );
    queue(
        &r.script,
        "checker",
        vec![
            // First checked commit: solvable without the target.
            audit("a1", "not obvious", "Ohm's law", 0.6),
            necessity("n1", true, "read the answer off the table"),
            // Second: the checker names the target at once, but needs it.
            audit("a2", "first sentence", "The divider rule", 0.9),
            necessity("n2", false, "every route splits the supply"),
            // The retry after the miss.
            audit("a3", "not obvious", "Ohm's law", 0.6),
            necessity("n3", false, "needs the split"),
        ],
    );
    let id =
        r.h.start_practice(&[A.into()], "stream", None)
            .await
            .unwrap()
            .id;

    let first = r.script.requests_of("coach");
    let next = next_request(&first[0]);
    assert_eq!(
        (next["cueDistance"].as_u64(), next["hosts"].clone()),
        (Some(2), json!([OHM]))
    );
    assert_eq!(next["layer"], "discrimination");

    // What the model was told, and in the log, in order.
    let told = r.script.requests_of("coach");
    assert!(
        texts(&told[1]).contains("problem_commit refused: the problem has issues"),
        "{}",
        texts(&told[1])
    );
    assert!(texts(&told[1]).contains("`nope` is not a node of the knowledge graph"));
    assert!(texts(&told[1]).contains("mark at least one solution move"));
    assert!(
        texts(&told[2]).contains("the checker solved it without The divider rule"),
        "{}",
        texts(&told[2])
    );
    assert!(texts(&told[2]).contains("read the answer off the table"));
    let results = raw_results(&r.repo, &id, "problem_commit");
    assert_eq!(results.len(), 3);
    assert!(results[0].contains("has issues") && results[1].contains("solved it without"));
    assert!(
        results[2].starts_with("Problem `") && results[2].contains("posed"),
        "{}",
        results[2]
    );
    // Only the committed problem is kept; the refusals leave `reject` ops.
    assert_eq!(practice_events(&r.repo, &id, "commit").len(), 1);
    let rejects = practice_events(&r.repo, &id, "reject");
    assert_eq!(
        rejects
            .iter()
            .map(|o| o["by"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["scope", "checker"]
    );
    // None of it reached the learner.
    let session = serde_json::to_string(&r.h.load_session(&id).unwrap()).unwrap();
    assert!(!session.contains("read the answer off the table") && !session.contains("`nope`"));
    assert!(!session.contains("\"event\":\"reject\""));
    let all: String = events
        .lock()
        .unwrap()
        .iter()
        .map(|(_, e)| e.to_string())
        .collect();
    assert!(!all.contains("read the answer off the table") && !all.contains("solved it without"));

    // The checker named the target on sight: cue 2 was recorded as 1.
    let commit_op = &practice_events(&r.repo, &id, "commit")[0];
    assert_eq!(commit_op["problem"]["cueDistance"], 1);
    assert_eq!(commit_op["checks"]["cueCappedFrom"], 2);
    assert!(
        results[2].contains("recorded as 1 instead of 2"),
        "{}",
        results[2]
    );

    // A miss: the outcome carries the class and no confidence for a given-up
    // answer; the next problem steps one notch down from the recorded cue.
    r.h.practice_submit(&id, "5 V", Some(30), &[])
        .await
        .unwrap();
    let log = outcomes(&r.repo);
    let o = log.iter().find(|o| o["sessionId"] == id.as_str()).unwrap();
    assert_eq!(o["verdict"], "wrong");
    assert_eq!(o["missClass"], "execution");
    assert_eq!(o["cueDistance"], 1);
    assert_eq!(o["layer"], "discrimination");
    r.h.practice_next(&id).await.unwrap();
    let coach = r.script.requests_of("coach");
    let next = next_request(coach.last().unwrap());
    assert_eq!(next["cueDistance"], 0, "a miss steps down: {next}");
    assert_eq!(
        next["layer"], "discrimination",
        "a miss holds the layer: {next}"
    );
    assert!(r.script.unexpected.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// The answer check by code
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_check_script_that_disagrees_discards_the_problem_before_the_checker_runs() {
    let r = rig("practice-script").await;
    let mut wrong = commit(A, "recall", 1, "a lamp chain", "300 V");
    wrong["check"] = json!({ "language": "python", "code": "echo 150" });
    let mut right = commit(A, "recall", 1, "a lamp chain", "150 V");
    right["check"] = json!({ "language": "python", "code": "echo 150" });
    queue(
        &r.script,
        "coach",
        vec![
            call("c1", "problem_commit", wrong),
            call("c2", "problem_commit", right),
        ],
    );
    queue(
        &r.script,
        "checker",
        vec![
            audit("a1", "not obvious", "Ohm's law", 0.6),
            necessity("n1", false, "needs the split"),
        ],
    );
    let id =
        r.h.start_practice(&[A.into()], "stream", None)
            .await
            .unwrap()
            .id;

    let results = raw_results(&r.repo, &id, "problem_commit");
    assert!(
        results[0].contains("printed `150`") && results[0].contains("`300 V`"),
        "{}",
        results[0]
    );
    assert!(results[0].contains("nothing was posed"));
    assert!(
        results[1].contains("reproduced the committed answer"),
        "{}",
        results[1]
    );
    // The mismatch cost no model call: the checker ran only for the second.
    assert_eq!(r.script.requests_of("checker").len(), 2);
    assert_eq!(
        practice_events(&r.repo, &id, "commit")[0]["checks"]["answerCheck"],
        "passed"
    );
    assert_eq!(
        practice_events(&r.repo, &id, "reject")[0]["by"],
        "script",
        "the discarded problem is on record"
    );
    let view = serde_json::to_value(r.h.practice_state(&id).unwrap()).unwrap();
    assert_eq!(view["problems"][0]["answerCheck"], "passed");
    assert!(!view.to_string().contains("printed `150`"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_script_that_cannot_run_here_is_skipped_and_the_coach_is_told_to_grade_with_care() {
    let r = rig("practice-skip").await;
    // The stand-in plays python3 without SymPy; the script needs SymPy too.
    let mut c = commit(A, "recall", 1, "a lamp chain", "150 V");
    c["check"] = json!({ "language": "python", "code": "# NOSYMPY NEEDSYMPY\necho 150" });
    queue(&r.script, "coach", vec![call("c1", "problem_commit", c)]);
    queue(
        &r.script,
        "checker",
        vec![
            audit("a1", "not obvious", "Ohm's law", 0.6),
            necessity("n1", false, "needs it"),
        ],
    );
    let id =
        r.h.start_practice(&[A.into()], "stream", None)
            .await
            .unwrap()
            .id;
    let results = raw_results(&r.repo, &id, "problem_commit");
    assert!(
        results[0].contains("could not run here") && results[0].contains("unverified"),
        "{}",
        results[0]
    );
    let view = serde_json::to_value(r.h.practice_state(&id).unwrap()).unwrap();
    assert_eq!(view["status"], "open", "unverified, but posed");
    assert_eq!(view["problems"][0]["answerCheck"], "skipped");
}

// ---------------------------------------------------------------------------
// Dismiss, retests, the end
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dismissal_is_concluded_by_the_app_and_retested_on_a_fresh_surface_before_the_end() {
    let r = rig("practice-retest").await;
    queue(
        &r.script,
        "coach",
        vec![
            call(
                "c1",
                "problem_commit",
                commit(A, "recall", 1, "a lamp chain", "FINAL-6V"),
            ),
            // The coach talks about the skip but never concludes.
            Reply::Text("Skipped. It was about splitting a supply."),
            // The retest: first the same surface (refused), then a new one.
            call(
                "c2",
                "problem_commit",
                commit(A, "recall", 1, "A lamp chain", "FINAL-6V"),
            ),
            call(
                "c3",
                "problem_commit",
                commit(A, "recall", 1, "a phone charger", "FINAL-3V"),
            ),
            conclude(
                "c4",
                json!({ "verdict": "correct", "hintRung": null, "note": "clean" }),
            ),
            Reply::Text("Correct."),
        ],
    );
    queue(
        &r.script,
        "checker",
        vec![
            audit("a1", "not obvious", "Ohm's law", 0.6),
            necessity("n1", false, "needs it"),
            audit("a2", "not obvious", "Ohm's law", 0.6),
            necessity("n2", false, "needs it"),
        ],
    );
    let id =
        r.h.start_practice(&[A.into()], "stream", None)
            .await
            .unwrap()
            .id;

    // Dismissing with no answer: allowed; the app writes the conclusion.
    r.h.practice_dismiss(&id).await.unwrap();
    let log = outcomes(&r.repo);
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!(log[0]["verdict"], "dismissed");
    assert!(log[0].get("confidence").is_none());
    let conclusion = &practice_events(&r.repo, &id, "conclude")[0]["conclusion"];
    assert_eq!(
        (conclusion["by"].as_str(), conclusion["rating"].as_str()),
        (Some("app"), Some("again"))
    );
    let view = serde_json::to_value(r.h.practice_state(&id).unwrap()).unwrap();
    assert_eq!(view["status"], "concluded");
    assert_eq!(view["problems"][0]["result"]["verdict"], "dismissed");

    // End: the dismissed problem has not been retested, so the first press
    // starts the retests instead of ending.
    r.h.practice_end(&id, false).await.unwrap();
    let view = serde_json::to_value(r.h.practice_state(&id).unwrap()).unwrap();
    assert_eq!(
        (view["wrapUp"].as_bool(), view["retestsLeft"].as_u64()),
        (Some(true), Some(1))
    );
    assert_eq!(view["status"], "concluded");
    assert!(view["can"]["next"].as_bool().unwrap());

    r.h.practice_next(&id).await.unwrap();
    let results = raw_results(&r.repo, &id, "problem_commit");
    assert!(
        results[1].contains("this is a retest") && results[1].contains("must differ"),
        "{}",
        results[1]
    );
    let coach = r.script.requests_of("coach");
    let next = next_request(&coach[2]);
    assert_eq!(
        next["retestOf"].as_str(),
        practice_events(&r.repo, &id, "next")[0]["problemId"].as_str()
    );
    assert!(
        texts(&coach[2]).contains("this is a retest"),
        "retests say so"
    );
    assert!(coach[2]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("fresh surface"));
    r.h.practice_submit(&id, "3 V", Some(90), &[])
        .await
        .unwrap();
    let log = outcomes(&r.repo);
    assert_eq!(log.len(), 2);
    assert_eq!(log[1]["verdict"], "correct");

    // Everything is retested: ending ends, with no model call (no
    // calibration to report).
    let before = r.script.requests.lock().unwrap().len();
    r.h.practice_end(&id, false).await.unwrap();
    assert_eq!(r.script.requests.lock().unwrap().len(), before);
    let view = r.h.practice_state(&id).unwrap();
    assert_eq!(view.status, "ended");
    assert!(r.h.practice_submit(&id, "more", None, &[]).await.is_err());
    assert!(r.h.practice_next(&id).await.is_err());
    assert!(r.script.unexpected.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// The set format
// ---------------------------------------------------------------------------

/// The first `problemId` the coach's pinned state lists for the open set.
fn first_problem_id(body: &Value) -> String {
    let system = body["messages"][0]["content"].as_str().unwrap();
    let at = system
        .find("problemId `")
        .expect("a problemId in the state")
        + "problemId `".len();
    system[at..].split('`').next().unwrap().to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_set_gives_no_feedback_until_it_is_handed_in_and_is_then_reviewed_problem_by_problem() {
    let r = rig("practice-set").await;
    queue(
        &r.script,
        "coach",
        vec![
            Reply::Calls(
                "",
                vec![
                    (
                        "s1",
                        "problem_commit",
                        commit(A, "recall", 1, "a lamp chain", "FINAL-6V"),
                    ),
                    (
                        "s2",
                        "problem_commit",
                        commit(KVL, "recall", 1, "a car circuit", "FINAL-2V"),
                    ),
                    // Too early: nobody has answered the set yet.
                    (
                        "s3",
                        "problem_conclude",
                        json!({ "verdict": "correct", "hintRung": null, "note": "early" }),
                    ),
                ],
            ),
            Reply::Dynamic(Arc::new(|body: &Value| {
                let id = first_problem_id(body);
                (
                    String::new(),
                    vec![(
                        "r1".to_owned(),
                        "problem_conclude".to_owned(),
                        json!({ "problemId": id, "verdict": "correct", "hintRung": null, "note": "ok" }),
                    )],
                )
            })),
            Reply::Text("Overall: the first was right; you left the second blank."),
        ],
    );
    queue(
        &r.script,
        "checker",
        vec![
            audit("a1", "not obvious", "Ohm's law", 0.6),
            necessity("n1", false, "needs it"),
            audit("a2", "not obvious", "Ohm's law", 0.6),
            necessity("n2", false, "needs it"),
        ],
    );
    let events = collect_events(&r.h);
    let id =
        r.h.start_practice(&[A.into(), KVL.into()], "set", Some(2))
            .await
            .unwrap()
            .id;

    let view = serde_json::to_value(r.h.practice_state(&id).unwrap()).unwrap();
    assert_eq!(
        (view["format"].as_str(), view["status"].as_str()),
        (Some("set"), Some("set-open"))
    );
    assert_eq!(view["problems"].as_array().unwrap().len(), 2);
    assert!(view["problems"]
        .as_array()
        .unwrap()
        .iter()
        .all(|p| p["status"] == "open"));
    assert!(view["can"]["submitSet"].as_bool().unwrap());
    assert!(!view["can"]["hint"].as_bool().unwrap() && !view["can"]["submit"].as_bool().unwrap());
    let early = raw_results(&r.repo, &id, "problem_conclude");
    assert!(early[0].contains("hasn't handed in the set"), "{early:?}");
    assert!(
        outcomes(&r.repo).is_empty(),
        "no verdict before the set is in"
    );
    assert!(!events
        .lock()
        .unwrap()
        .iter()
        .any(|(_, e)| e["kind"] == "text"));

    // No hints, no single answers, no next in a set.
    assert!(r.h.practice_hint(&id).await.is_err());
    let err =
        r.h.practice_submit(&id, "6 V", None, &[])
            .await
            .unwrap_err()
            .to_string();
    assert!(err.contains("whole set"), "{err}");
    assert!(r.h.practice_next(&id).await.is_err());
    let ids: Vec<String> = view["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["problemId"].as_str().unwrap().to_owned())
        .collect();
    let bad =
        r.h.practice_submit_set(
            &id,
            vec![SetAnswer {
                problem_id: ids[0].clone(),
                text: "x".into(),
                confidence: Some(55),
                sketch: None,
            }],
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(bad.contains("10, 30, 50, 70 or 90"), "{bad}");

    // Hand it in: the first answered, the second left blank.
    // A drawing goes with the first answer: committed on hand-in, named in
    // the coach's listing and sent to it as an image.
    let sketch = r
        .h
        .stage_image(
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==",
            Some("Scratch pad.png"),
        )
        .unwrap();
    assert!(!r.repo.join(&sketch.path).exists());
    r.h.practice_submit_set(
        &id,
        vec![SetAnswer {
            problem_id: ids[0].clone(),
            text: "6 V".into(),
            confidence: Some(90),
            sketch: Some(sketch.clone()),
        }],
    )
    .await
    .unwrap();
    assert!(r.repo.join(&sketch.path).is_file());
    let log: Vec<Value> = outcomes(&r.repo);
    assert_eq!(log.len(), 2, "{log:?}");
    assert!(log
        .iter()
        .all(|o| o["format"] == "set" && o["durationSec"] == 0));
    let by_problem = |pid: &str| log.iter().find(|o| o["problemId"] == pid).unwrap();
    assert_eq!(by_problem(&ids[0])["verdict"], "correct");
    assert_eq!(by_problem(&ids[0])["confidence"], 90);
    assert_eq!(
        by_problem(&ids[1])["verdict"],
        "dontknow",
        "an unanswered problem is a give-up"
    );
    assert!(by_problem(&ids[1]).get("confidence").is_none());
    let view = serde_json::to_value(r.h.practice_state(&id).unwrap()).unwrap();
    assert_eq!(view["status"], "set-review");
    assert!(view["problems"]
        .as_array()
        .unwrap()
        .iter()
        .all(|p| p["status"] == "concluded"));
    assert_eq!(
        view["problems"][0]["result"]["reveal"]["finalAnswer"],
        "FINAL-6V"
    );
    assert!(
        view["can"]["submit"].as_bool().unwrap(),
        "questions about the review are fine"
    );
    assert!(events
        .lock()
        .unwrap()
        .iter()
        .any(|(_, e)| e["kind"] == "text" && e["agent"] == "coach"));
    // The review turn listed the answers, with the blank one.
    let coach = r.script.requests_of("coach");
    assert!(texts(coach.last().unwrap()).contains("(no answer)"));
    assert!(texts(coach.last().unwrap()).contains("[sketch: image 1 of this message]"));
    assert!(coach.last().unwrap().to_string().contains("image_url"));
    assert!(r.script.unexpected.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// Restart and resume; refusals at the door
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_cut_short_resumes_after_a_restart_and_the_outcome_is_written_once() {
    let r = rig("practice-resume").await;
    queue(
        &r.script,
        "coach",
        vec![call(
            "c1",
            "problem_commit",
            commit(A, "recall", 1, "a lamp chain", "FINAL-6V"),
        )],
    );
    queue(
        &r.script,
        "checker",
        vec![
            audit("a1", "not obvious", "Ohm's law", 0.6),
            necessity("n1", false, "needs it"),
        ],
    );
    let id =
        r.h.start_practice(&[A.into()], "stream", None)
            .await
            .unwrap()
            .id;

    // The model fails mid-session: the answer is saved, the turn is not.
    let err =
        r.h.practice_submit(&id, "6 V", Some(70), &[])
            .await
            .unwrap_err();
    assert!(!err.to_string().is_empty());
    assert_eq!(r.script.unexpected.lock().unwrap().len(), 1);
    let view = r.h.practice_state(&id).unwrap();
    assert!(!view.turn_running && view.last_turn_error.is_some());

    // The app restarts: nothing is running, and the learner's answer is
    // still the last thing in the conversation.
    drop(r.h);
    let base = {
        // A fresh provider endpoint serving the same script.
        serve(Arc::clone(&r.script), Arc::new(Semaphore::new(0))).await
    };
    let h = harness_with_secrets(&r.repo, &r.config, &base, Arc::clone(&r.secrets)).await;
    let view = h.practice_state(&id).unwrap();
    assert!(!view.turn_running && view.last_turn_error.is_none());
    assert_eq!(view.status, "open");
    queue(
        &r.script,
        "coach",
        vec![
            conclude(
                "c2",
                json!({ "verdict": "correct", "hintRung": null, "note": "ok" }),
            ),
            Reply::Text("Correct."),
        ],
    );
    h.practice_resume(&id).await.unwrap();
    assert_eq!(outcomes(&r.repo).len(), 1);
    assert_eq!(h.practice_state(&id).unwrap().status, "concluded");
    // The coach is now waiting for the learner: nothing to resume.
    let err = h.practice_resume(&id).await.unwrap_err().to_string();
    assert!(err.contains("nothing to resume"), "{err}");
    // The log replays the same on a third open.
    let again = h.practice_state(&id).unwrap();
    assert_eq!(again.problems[0].result.as_ref().unwrap().rating, "good");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn practice_refuses_what_it_cannot_start() {
    let r = rig("practice-refuse").await;
    let err =
        r.h.start_practice(&[], "exam", None)
            .await
            .unwrap_err()
            .to_string();
    assert!(err.contains("start_exam"), "{err}");
    let err =
        r.h.start_practice(&["nope".into()], "stream", None)
            .await
            .unwrap_err()
            .to_string();
    assert!(err.contains("`nope` is not an idea"), "{err}");
    let err =
        r.h.start_practice(&[], "set", Some(99))
            .await
            .unwrap_err()
            .to_string();
    assert!(err.contains("2 to 10"), "{err}");
    let err =
        r.h.start_practice(&[], "marathon", None)
            .await
            .unwrap_err()
            .to_string();
    assert!(err.contains("unknown practice format"), "{err}");
    assert!(
        r.h.list_sessions().unwrap().is_empty(),
        "nothing is left behind"
    );

    // An empty graph has nothing to practise.
    let script = Arc::new(Script::default());
    let base = serve(Arc::clone(&script), Arc::new(Semaphore::new(0))).await;
    let (repo, config) = temp_dirs("practice-empty");
    let h = harness(&repo, &config, &base).await;
    let err = h
        .start_practice(&[], "stream", None)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("knowledge graph is empty"), "{err}");
}

// ---------------------------------------------------------------------------
// Plugins active when the idea was taught
// ---------------------------------------------------------------------------

/// A plugin whose one tool is scoped to the tutor and to practice.
struct EchoPlugin {
    manifest: PluginManifest,
}

impl Plugin for EchoPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn tools(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "echo_echo".into(),
            description: "Echo the text back.".into(),
            input_schema: json!({ "type": "object", "properties": { "text": { "type": "string" } } }),
        }]
    }

    fn fragment(&self, agent: &str) -> String {
        format!("For {agent}: call `echo_echo` to echo.")
    }

    fn call(self: Arc<Self>, _name: &str, args: &Value, _ctx: PluginContext) -> ToolFuture {
        let text = args["text"].as_str().unwrap_or("").to_owned();
        Box::pin(async move {
            ToolOutcome {
                content: format!("echo: {text}"),
                is_error: false,
                artifact: None,
            }
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_coach_gets_the_tools_of_the_plugins_that_were_active_when_the_idea_was_taught() {
    fake_python();
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("practice-plugins");
    seed_graph(&repo);
    let h = harness(&repo, &config, &base).await;
    h.register_native_plugin(Arc::new(EchoPlugin {
        manifest: PluginManifest {
            id: "echo".into(),
            version: "0.1.0".into(),
            api: 1,
            title: "Echo".into(),
            description: "A test plugin.".into(),
            license: "MIT".into(),
            tools: vec![harness::plugins::manifest::ToolDecl {
                name: "echo_echo".into(),
                description: "Echo the text back.".into(),
                agents: vec!["tutor".into(), "practice".into()],
                input: "schemas/echo.json".into(),
                entry: "echo".into(),
                artifact: false,
            }],
            platforms: vec!["desktop".into(), "mobile".into()],
            permissions: vec![],
            use_when: "A test needs a plugin.".into(),
            avoid_when: "Real lessons.".into(),
            cost: "none".into(),
            renderer: vec![],
            view: vec![],
            prompts: Default::default(),
        },
    }))
    .unwrap();

    // A lesson on the divider rule with the plugin in its only section.
    let lesson = session_awaiting_approval(
        &h,
        &script,
        &release,
        vec![Reply::Calls(
            "",
            vec![("w1", "plan_write", plan_args(&["echo"]))],
        )],
    )
    .await;
    script.push("tutor", Reply::Text("Let's start."));
    h.approve(&lesson, vec![]).await.unwrap();

    // Practising the idea: the plan carries what teaching said (the
    // section's `explain` target, its plugin), and the coach has the tool.
    let mut c = commit(A, "recall", 1, "a lamp chain", "FINAL-6V");
    c["level"] = json!("explain");
    queue(
        &script,
        "coach",
        vec![
            call("e1", "echo_echo", json!({ "text": "hi" })),
            call("c1", "problem_commit", c),
        ],
    );
    queue(
        &script,
        "checker",
        vec![
            audit("a1", "not obvious", "Ohm's law", 0.6),
            necessity("n1", false, "needs it"),
        ],
    );
    let id = h
        .start_practice(&[A.into()], "stream", None)
        .await
        .unwrap()
        .id;

    let plan = &practice_events(&repo, &id, "next")[0]["plan"];
    assert_eq!(plan["plugins"], json!(["echo"]));
    assert_eq!(
        (plan["aim"].as_str(), plan["level"].as_str()),
        (Some("explain"), Some("explain"))
    );
    let coach = script.requests_of("coach");
    assert!(tool_names(&coach[0]).contains(&"echo_echo".to_owned()));
    let system = coach[0]["messages"][0]["content"].as_str().unwrap();
    assert!(
        system.contains("## Plugins you may use for this problem")
            && system.contains("For practice: call `echo_echo` to echo."),
        "{system}"
    );
    assert_eq!(raw_results(&repo, &id, "echo_echo"), ["echo: hi"]);
    assert_eq!(h.practice_state(&id).unwrap().status, "open");
    assert!(script.unexpected.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// Attachments: images and files on a submission
// ---------------------------------------------------------------------------

/// A 1×1 PNG.
const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_submission_can_carry_a_photo_and_a_file_and_the_coach_reads_both() {
    let r = rig("practice-attach").await;
    seed_outcome(&r.repo, "seed", OHM, "recall", 1);
    // The worksheet is outside the home folder: it becomes a read root.
    let dir = std::env::temp_dir().join(format!("lh-practice-attach-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("worksheet.txt"), "Q1. R1 = 4.7k, R2 = 10k").unwrap();
    let sheet = dir
        .join("worksheet.txt")
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    queue(
        &r.script,
        "coach",
        vec![
            call(
                "c1",
                "problem_commit",
                commit(A, "recall", 1, "a lamp chain", "FINAL-6V"),
            ),
            // After the submission: read the worksheet.
            call("f1", "file_read", json!({ "path": sheet })),
            Reply::Text("Your worksheet and photo agree: R1 is 4.7k."),
        ],
    );
    queue(
        &r.script,
        "checker",
        vec![
            audit("a1", "not obvious", "Ohm's law", 0.8),
            necessity("n1", false, "every route needs the split"),
        ],
    );
    let id =
        r.h.start_practice(&[A.into()], "stream", None)
            .await
            .unwrap()
            .id;
    let file = r.h.stage_file(Some(&id), &sheet).unwrap();
    assert_eq!(file.kind, "file");
    let photo = r.h.stage_image(PNG, Some("working.png")).unwrap();

    // A library is linked from Home, never sent from Practice; nothing is
    // recorded for the refused send.
    let ops_before = r.h.load_session(&id).unwrap().ops.len();
    let library = harness::Attachment {
        kind: "library".into(),
        path: "huawei-ict".into(),
        media_type: None,
        bytes: None,
        name: Some("Huawei ICT".into()),
    };
    let err =
        r.h.practice_submit(&id, "x", None, &[library])
            .await
            .unwrap_err()
            .to_string();
    assert!(err.contains("takes images, files and folders"), "{err}");
    assert_eq!(r.h.load_session(&id).unwrap().ops.len(), ops_before);

    r.h.practice_submit(&id, "See my working", None, &[photo.clone(), file.clone()])
        .await
        .unwrap();

    // The coach was shown the photo and told where the file is.
    let coach = r.script.requests_of("coach");
    let first = &coach[1];
    let user = first["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|m| m["role"] == "user")
        .unwrap();
    let parts = user["content"].as_array().unwrap();
    assert_eq!(
        parts.iter().filter(|p| p["type"] == "image_url").count(),
        1,
        "{user}"
    );
    let text = texts(first);
    assert!(
        text.contains(&format!("[attached file] {sheet}"))
            && text.contains("read it with file_read"),
        "{text}"
    );

    // The file was readable through its grant.
    let read = raw_results(&r.repo, &id, "file_read");
    assert!(read[0].contains("R1 = 4.7k, R2 = 10k"), "{read:?}");
    assert_eq!(
        r.h.session_state(&id).unwrap().file_grants,
        std::slice::from_ref(&sheet)
    );

    // The learner's own page shows both attachments on the submission.
    let view = serde_json::to_value(r.h.practice_state(&id).unwrap()).unwrap();
    let sent = view["feed"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["kind"] == "learner" && f["intent"] == "submission")
        .unwrap();
    let kinds: Vec<&str> = sent["attachments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["image", "file"]);
    assert!(r.script.unexpected.lock().unwrap().is_empty());
}
