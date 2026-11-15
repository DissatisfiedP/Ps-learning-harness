//! End-to-end proof of exam mode (architecture §4.14, `docs/ui-design.md`
//! §11c): an exam goal's weighted plan is ranked by coverage priority, a timed
//! paper is written on the best-ranked ideas, the app enforces the clock
//! (no hints, answers refused after the deadline, the saved draft handed in at
//! it), the coach marks it the way the exam would, and every problem writes an
//! `exam` outcome.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use harness::graph::{append, provenance, GraphOp};
use harness::practice::SetAnswer;
use harness::state::Kind;
use harness::Harness;
use serde_json::{json, Value};
use tokio::sync::Semaphore;

const A: &str = "divider-rule";
const OHM: &str = "ohm";
const KVL: &str = "kvl";
const BUDGET_MIN: u32 = 45;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

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

fn append_outcome(repo: &std::path::Path, line: Value) {
    let path = repo.join("practice/outcomes.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    text.push_str(&format!("{line}\n"));
    std::fs::write(path, text).unwrap();
}

fn hours_ago(h: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::hours(h))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// A correct answer `hours` ago on one layer of a node.
fn seed_outcome(repo: &std::path::Path, id: &str, node: &str, layer: &str, hours: i64) {
    append_outcome(
        repo,
        json!({
            "opId": id, "at": hours_ago(hours), "sessionId": "old", "problemId": id,
            "targets": [node], "hosts": [], "layer": layer, "format": "check",
            "verdict": "correct", "confidence": 70, "hintRung": null,
            "grader": "committed", "durationSec": 60
        }),
    );
}

/// A structural miss `hours` ago: the learner believes something wrong.
fn seed_structural_miss(repo: &std::path::Path, id: &str, node: &str, hours: i64) {
    append_outcome(
        repo,
        json!({
            "opId": id, "at": hours_ago(hours), "sessionId": "old", "problemId": id,
            "targets": [node], "hosts": [], "layer": "recall", "format": "stream",
            "verdict": "wrong", "confidence": 70, "hintRung": null, "missClass": "structural",
            "misconception": "thinks voltage is shared equally between parts",
            "grader": "committed", "durationSec": 60
        }),
    );
}

fn outcomes(repo: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(repo.join("practice/outcomes.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

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

fn practice_events(repo: &std::path::Path, id: &str, event: &str) -> Vec<Value> {
    raw_ops(repo, id)
        .into_iter()
        .filter(|o| o["op"] == "practice" && o["event"] == event)
        .collect()
}

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

/// The `next:` array of an exam request, parsed.
fn next_requests(body: &Value) -> Vec<Value> {
    let all = texts(body);
    let at = all.rfind("next: ").expect("a next: request") + "next: ".len();
    let mut stream = serde_json::Deserializer::from_str(&all[at..]).into_iter::<Value>();
    stream.next().unwrap().unwrap().as_array().unwrap().clone()
}

/// Every problem id the coach's pinned state lists, in order.
fn problem_ids(body: &Value) -> Vec<String> {
    let system = body["messages"][0]["content"].as_str().unwrap();
    system
        .split("problemId `")
        .skip(1)
        .map(|rest| rest.split('`').next().unwrap().to_owned())
        .collect()
}

fn call(id: &'static str, name: &'static str, args: Value) -> Reply {
    Reply::Calls("", vec![(id, name, args)])
}

fn audit(id: &'static str) -> Reply {
    call(
        id,
        "audit_submit",
        json!({
            "techniques": [{ "name": "Ohm's law", "confidence": 0.6, "cuedBy": "the sources" }],
            "firstMove": "set up the loop", "obviousAfter": "not obvious"
        }),
    )
}

fn necessity(id: &'static str) -> Reply {
    call(
        id,
        "necessity_submit",
        json!({ "solvedWithout": false, "route": "every route needs the idea" }),
    )
}

/// An exam problem: past-paper wording, its marks and its mark scheme.
fn paper_problem(target: &str, layer: &str, marks: u64, surface: &str) -> Value {
    json!({
        "statement": format!("[{marks} marks] A 12 V source feeds two parts in a row ({surface}). Find the voltage across the second part."),
        "targets": [target], "hosts": [],
        "layer": layer, "level": "use", "cueDistance": 1, "isDistractor": false,
        "solution": [
            { "move": "Find the current through the chain", "usesNode": "ohm", "decisionPoint": false },
            { "move": "Apply the idea to the second part", "usesNode": target, "decisionPoint": true }
        ],
        "finalAnswer": format!("FINAL-{surface}"), "acceptableVariants": [],
        "rubric": [
            { "id": "m1", "criterion": "Finds the current (method mark)", "essential": true },
            { "id": "a1", "criterion": "States the voltage with its unit", "essential": false }
        ],
        "marks": marks, "surface": surface
    })
}

struct Rig {
    script: Arc<Script>,
    release: Arc<Semaphore>,
    h: Arc<Harness>,
    base: String,
    repo: std::path::PathBuf,
    config: std::path::PathBuf,
    secrets: Arc<harness::secrets::SecretStore>,
}

async fn rig(name: &str) -> Rig {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs(name);
    seed_graph(&repo);
    let secrets = Arc::new(harness::secrets::SecretStore::memory());
    let h = harness_with_secrets(&repo, &config, &base, Arc::clone(&secrets)).await;
    Rig {
        script,
        release,
        h,
        base,
        repo,
        config,
        secrets,
    }
}

fn exam_date(days: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::days(days))
        .format("%Y-%m-%d")
        .to_string()
}

/// The plan the planner writes for the exam: weights per node, one idea
/// (the divider rule) the learner holds well enough for practice alone.
fn exam_plan_args() -> Value {
    json!({
        "goal": "Pass EE101", "approach": "Practice-led, papers last.",
        "nodes": [
            { "id": OHM, "title": "Ohm's law", "kind": "mechanism", "requires": [], "weight": 0.3, "practiceOnly": false },
            { "id": A, "title": "The divider rule", "kind": "mechanism", "requires": [OHM], "weight": 0.9, "practiceOnly": true },
            { "id": KVL, "title": "Kirchhoff's voltage law", "kind": "mechanism", "requires": [OHM], "weight": 0.6, "practiceOnly": false },
            { "id": "ghost", "title": "Not in the graph yet", "kind": "fact", "requires": [], "weight": 0.2, "practiceOnly": false }
        ],
        "sections": [{ "id": "s1", "title": "Circuits", "nodeIds": [OHM, A, KVL, "ghost"], "mode": "mechanism",
            "style": "mixed", "plugins": [], "notes": "", "exit": "Solves a paper question.", "targetLevel": "use" }]
    })
}

/// An exam goal's Learn session at its plan card.
async fn exam_session(r: &Rig, days: i64) -> String {
    r.script.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "g1",
                "goal_set",
                json!({ "goal": "Pass EE101", "purpose": "exam",
                    "successLooksLike": ["pass the finals"],
                    "exam": { "name": "EE101 finals", "date": exam_date(days),
                        "syllabus": "Ohm, dividers, KVL" },
                    "materials": [], "outOfScope": [], "preferences": [], "orientationFirst": false }),
            )],
        ),
    );
    r.script.push("tutor", Reply::Text("Let's begin."));
    r.script.push("title", Reply::Text("EE101 finals"));
    let id = r.h.start_teaching("I have an exam").await.unwrap().id;
    r.script.push(
        "researcher",
        Reply::Calls(
            "",
            vec![(
                "b1",
                "brief_submit",
                json!({ "kind": "scope", "summary": "Circuits finals.",
                    "strands": [{"name": "series current", "why": "root", "kind": "mechanism", "prerequisites": []}],
                    "misconceptions": [], "sources": [] }),
            )],
        ),
    );
    r.script.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "p1",
                "probe_done",
                json!({ "strands": [{"name": "ohm"}], "notes": "fluent" }),
            )],
        ),
    );
    r.script
        .push("tutor", Reply::Text("The plan is being built."));
    r.script
        .push("planner", call("pw", "plan_write", exam_plan_args()));
    r.release.add_permits(1);
    r.h.wait_background(&id).await;
    let st = r.h.session_state(&id).unwrap();
    assert_eq!(
        st.phase,
        harness::state::Phase::AwaitApproval,
        "unexpected: {:?}",
        r.script.unexpected.lock().unwrap()
    );
    id
}

/// Queue the coach's three commits (KVL, OHM, divider rule: the coverage
/// order below) and the checker's six turns.
fn queue_paper(r: &Rig) {
    // The divider rule is the heaviest idea but was recalled an hour ago.
    seed_outcome(&r.repo, "seedA", A, "recall", 1);
    r.script.push(
        "coach",
        Reply::Calls(
            "",
            vec![
                (
                    "c1",
                    "problem_commit",
                    paper_problem(KVL, "recall", 5, "a sensor input"),
                ),
                (
                    "c2",
                    "problem_commit",
                    paper_problem(OHM, "recall", 4, "a lamp"),
                ),
                (
                    "c3",
                    "problem_commit",
                    paper_problem(A, "discrimination", 6, "a car circuit"),
                ),
            ],
        ),
    );
    for i in 0..3 {
        let (a, n) = [("a1", "n1"), ("a2", "n2"), ("a3", "n3")][i];
        r.script.push("checker", audit(a));
        r.script.push("checker", necessity(n));
    }
}

/// The review: two problems marked by the coach (the third has no answer),
/// then the overall message.
fn queue_review(r: &Rig) {
    r.script.push(
        "coach",
        Reply::Dynamic(Arc::new(|body: &Value| {
            let ids = problem_ids(body);
            (
                String::new(),
                vec![
                    (
                        "r1".to_owned(),
                        "problem_conclude".to_owned(),
                        json!({ "problemId": ids[0], "verdict": "correct", "hintRung": null,
                            "marksAwarded": 5, "note": "full marks" }),
                    ),
                    (
                        "r2".to_owned(),
                        "problem_conclude".to_owned(),
                        json!({ "problemId": ids[1], "verdict": "partial", "hintRung": null,
                            "missClass": "execution", "marksAwarded": 2,
                            "note": "method mark only" }),
                    ),
                ],
            )
        })),
    );
    r.script.push(
        "coach",
        Reply::Text("7 of 15. The first was full marks; the second kept only its method mark."),
    );
}

async fn until(what: &str, mut ready: impl FnMut() -> bool) {
    for _ in 0..300 {
        if ready() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}

fn view(h: &Harness, id: &str) -> Value {
    serde_json::to_value(h.practice_state(id).unwrap()).unwrap()
}

fn answers(view: &Value) -> Vec<SetAnswer> {
    let ids: Vec<&str> = view["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["problemId"].as_str().unwrap())
        .collect();
    vec![
        SetAnswer {
            problem_id: ids[0].into(),
            text: "6 V".into(),
            confidence: Some(90),
            sketch: None,
        },
        SetAnswer {
            problem_id: ids[1].into(),
            text: "about 4 V".into(),
            confidence: Some(50),
            sketch: None,
        },
    ]
}

// ---------------------------------------------------------------------------
// The plan: coverage order, teaching only for structural misses
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_exam_plan_ranks_by_weight_forgetting_and_unshown_layers() {
    let r = rig("exam-plan").await;
    // The divider rule is the heaviest idea but was recalled an hour ago;
    // Kirchhoff and Ohm have no record.
    seed_outcome(&r.repo, "seedA", A, "recall", 1);
    let id = exam_session(&r, 10).await;

    let plan = serde_json::to_value(r.h.exam_plan(&id).unwrap()).unwrap();
    let order: Vec<&str> = plan["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert_eq!(order, [KVL, OHM, A], "{plan:#}");
    let kvl = &plan["nodes"][0];
    assert_eq!(kvl["weight"], 0.6);
    assert_eq!(kvl["retrievability"], 0.0, "no evidence: nothing recalled");
    assert_eq!(kvl["layerGap"], 3);
    assert!((kvl["priority"].as_f64().unwrap() - 0.6 * 4.0).abs() < 1e-9);
    assert_eq!(kvl["mode"], "practice");
    let divider = &plan["nodes"][2];
    assert_eq!(divider["layerGap"], 2, "recall shown, two layers left");
    assert!(divider["retrievability"].as_f64().unwrap() > 0.95);
    assert!(divider["priority"].as_f64().unwrap() < 0.1);

    let left = plan["daysLeft"].as_i64().unwrap();
    assert!((9..=10).contains(&left), "{left}");
    assert_eq!(plan["exam"]["name"], "EE101 finals");
    assert_eq!(plan["unmapped"][0]["planNode"], "ghost");
    assert_eq!(plan["simulation"]["label"], "estimate");
    let raise = plan["simulation"]["raise"].as_array().unwrap();
    assert_eq!(raise[0]["id"], KVL, "the biggest unshown weight gains most");
    assert!(plan["simulation"]["expectedExam"].as_f64().unwrap() > 0.0);
    assert_eq!(plan["today"]["kind"], "practice");
    assert_eq!(plan["paper"]["targets"], 3);
    assert_eq!(plan["paper"]["minutesPerProblem"], 15);
    assert_eq!(plan["mix"]["teachNodes"], 0);

    // Ohm's law keeps producing a structural miss: it is taught, not put on
    // a paper, and it is the day's work.
    seed_structural_miss(&r.repo, "miss1", OHM, 2);
    let plan = serde_json::to_value(r.h.exam_plan(&id).unwrap()).unwrap();
    let by = |id: &str| {
        plan["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(by(OHM)["mode"], "teach");
    assert!(by(OHM)["reason"]
        .as_str()
        .unwrap()
        .contains("voltage is shared equally"));
    assert_eq!(by(KVL)["mode"], "practice");
    assert_eq!(plan["today"]["kind"], "teach");
    assert_eq!(plan["today"]["nodeIds"][0], OHM);
    assert_eq!(plan["mix"]["teachNodes"], 1);
    assert_eq!(
        plan["paper"]["targets"], 2,
        "taught nodes aren't on a paper"
    );

    // The Learn session of a curiosity goal has no exam plan.
    let s = Arc::clone(&r.script);
    let other = session_awaiting_approval(
        &r.h,
        &s,
        &r.release,
        vec![call("pw2", "plan_write", plan_args(&[]))],
    )
    .await;
    let err = r.h.exam_plan(&other).unwrap_err().to_string();
    assert!(err.contains("not an exam"), "{err}");
    let err = r.h.exam_plan("nope").unwrap_err().to_string();
    assert!(!err.is_empty());
}

// ---------------------------------------------------------------------------
// The paper: coverage-ordered targets, the clock, the deadline hand-in
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_paper_runs_from_coverage_targets_to_the_deadline_hand_in_to_marked_outcomes() {
    let r = rig("exam-paper").await;
    let goal = exam_session(&r, 10).await;
    queue_paper(&r);
    queue_review(&r);

    let summary = r.h.start_exam(&goal, BUDGET_MIN, Some(3)).await.unwrap();
    let id = summary.id;
    assert_eq!(summary.mode, harness::Mode::Practice);
    assert!(summary.title.contains("EE101 finals"), "{}", summary.title);

    // Coverage order: the unseen heavy idea, the unseen light one, then the
    // heaviest idea that was recalled an hour ago. Past-paper style: close
    // to the lesson, no distractors, no hosts.
    let coach = r.script.requests_of("coach");
    let next = next_requests(&coach[0]);
    let targets: Vec<&str> = next.iter().map(|n| n["target"].as_str().unwrap()).collect();
    assert_eq!(targets, [KVL, OHM, A], "{next:?}");
    assert!(next
        .iter()
        .all(|n| n["cueDistance"] == 1 && n["isDistractor"] == false));
    assert_eq!(next[2]["layer"], "discrimination");
    let system = coach[0]["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains("- Format: exam"), "{system}");
    assert!(system.contains("## Exam paper"), "{system}");
    assert!(system.contains("45 minutes for 3 problems"), "{system}");
    assert!(system.contains("No past papers"), "{system}");

    // The paper is open: the clock started when the last problem was posed.
    let v = view(&r.h, &id);
    assert_eq!(v["format"], "exam");
    assert_eq!(v["status"], "set-open");
    assert_eq!(v["exam"]["budgetSec"], BUDGET_MIN * 60);
    assert_eq!(v["exam"]["name"], "EE101 finals");
    assert_eq!(v["exam"]["goalSessionId"], goal.as_str());
    assert_eq!(v["exam"]["submitted"], false);
    assert_eq!(v["exam"]["expired"], false);
    let remaining = v["exam"]["remainingSec"].as_i64().unwrap();
    assert!((2690..=2700).contains(&remaining), "{remaining}");
    assert_eq!(v["problems"].as_array().unwrap().len(), 3);
    let marks: Vec<u64> = v["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["marks"].as_u64().unwrap())
        .collect();
    assert_eq!(marks, [5, 4, 6]);
    assert!(v["problems"][0]["statement"]
        .as_str()
        .unwrap()
        .contains("5 marks"));
    assert_eq!(v["can"]["hint"], false);
    assert_eq!(v["can"]["dismiss"], false);
    assert_eq!(v["can"]["end"], false, "a paper is left by handing it in");
    assert_eq!(v["can"]["submitSet"], true);
    assert!(v["problems"][0].get("result").is_none());
    assert_eq!(practice_events(&r.repo, &id, "examOpen").len(), 1);

    // No hints, no skipping, no single answers, no next in an exam.
    let err = r.h.practice_hint(&id).await.unwrap_err().to_string();
    assert!(err.contains("no hints"), "{err}");
    assert!(r.h.practice_dismiss(&id).await.is_err());
    assert!(r.h.practice_next(&id).await.is_err());
    let err =
        r.h.practice_submit(&id, "6 V", None, &[])
            .await
            .unwrap_err()
            .to_string();
    assert!(err.contains("whole set"), "{err}");
    assert!(
        outcomes(&r.repo).iter().all(|o| o["format"] != "exam"),
        "no verdict before the paper is in"
    );

    // The learner saves as they type; a bad save is refused, a repeat is not
    // written twice.
    let ans = answers(&v);
    let bad = vec![SetAnswer {
        confidence: Some(55),
        ..ans[0].clone()
    }];
    assert!(r
        .h
        .practice_exam_save(&id, bad)
        .unwrap_err()
        .to_string()
        .contains("10, 30, 50, 70 or 90"));
    let unknown = vec![SetAnswer {
        problem_id: "nope".into(),
        text: "x".into(),
        confidence: None,
        sketch: None,
    }];
    assert!(r.h.practice_exam_save(&id, unknown).is_err());
    r.h.practice_exam_save(&id, ans.clone()).unwrap();
    r.h.practice_exam_save(&id, ans.clone()).unwrap();
    assert_eq!(practice_events(&r.repo, &id, "examDraft").len(), 1);
    let v = view(&r.h, &id);
    assert_eq!(v["exam"]["draft"][0]["text"], "6 V");
    // A drawing in the draft leaves staging at once, so the draft and the
    // hand-in at the deadline survive a restart.
    let sketch = r
        .h
        .stage_image(
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==",
            None,
        )
        .unwrap();
    let mut drawn = ans.clone();
    drawn[0].sketch = Some(sketch.clone());
    r.h.practice_exam_save(&id, drawn).unwrap();
    assert!(r.repo.join(&sketch.path).is_file());
    let v = view(&r.h, &id);
    assert_eq!(v["exam"]["draft"][0]["sketch"]["path"], json!(sketch.path));

    // The deadline passes with nothing handed in. The app's timer hands in
    // the saved draft by itself, and answers sent now are refused.
    r.h.skew_clock(i64::from(BUDGET_MIN) * 60 + 30);
    let v = view(&r.h, &id);
    assert_eq!(v["exam"]["expired"], true);
    assert_eq!(v["exam"]["closed"], true);
    assert_eq!(v["exam"]["remainingSec"], 0);
    assert_eq!(v["can"]["submitSet"], false);
    let err =
        r.h.practice_exam_save(&id, ans.clone())
            .unwrap_err()
            .to_string();
    assert!(err.contains("time is up"), "{err}");
    until("the deadline hand-in", || {
        view(&r.h, &id)["exam"]["submitted"] == true
    })
    .await;
    until("the marked outcomes", || {
        outcomes(&r.repo)
            .iter()
            .filter(|o| o["format"] == "exam")
            .count()
            == 3
    })
    .await;
    r.h.wait_background(&id).await;
    let late =
        r.h.practice_submit_set(&id, ans.clone())
            .await
            .unwrap_err()
            .to_string();
    assert!(late.contains("already handed in"), "{late}");

    let submit = &practice_events(&r.repo, &id, "setSubmit")[0];
    assert_eq!(submit["auto"], true);
    assert_eq!(submit["elapsedSec"], BUDGET_MIN * 60);
    assert_eq!(submit["answers"].as_array().unwrap().len(), 2);

    // Outcomes: format exam, the paper's average time per problem, no hints.
    let exam_outcomes: Vec<Value> = outcomes(&r.repo)
        .into_iter()
        .filter(|o| o["format"] == "exam")
        .collect();
    assert!(exam_outcomes
        .iter()
        .all(|o| o["durationSec"] == BUDGET_MIN * 60 / 3 && o["hintRung"].is_null()));
    let of = |target: &str| {
        exam_outcomes
            .iter()
            .find(|o| o["targets"][0] == target)
            .unwrap()
            .clone()
    };
    assert_eq!(of(KVL)["verdict"], "correct");
    assert_eq!(of(KVL)["confidence"], 90);
    assert_eq!(of(OHM)["verdict"], "partial");
    assert_eq!(of(OHM)["missClass"], "execution");
    assert_eq!(
        of(A)["verdict"],
        "dontknow",
        "no answer saved for the third"
    );
    assert_eq!(of(A)["grader"], "rubric", "a mark scheme grades it");

    // The review: marks as the exam would give them, the revealed scheme and
    // the total.
    let v = view(&r.h, &id);
    assert_eq!(v["status"], "set-review");
    assert_eq!(v["exam"]["autoSubmitted"], true);
    assert_eq!(v["exam"]["score"], json!({ "awarded": 7, "outOf": 15 }));
    assert_eq!(v["problems"][0]["result"]["marksAwarded"], 5);
    assert_eq!(v["problems"][1]["result"]["marksAwarded"], 2);
    assert_eq!(v["problems"][2]["result"]["marksAwarded"], 0);
    assert_eq!(
        v["problems"][0]["result"]["reveal"]["rubric"][0]["id"],
        "m1"
    );
    assert!(v["feed"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["intent"] == "set" && f["text"].as_str().unwrap().contains("Time was up")));
    let review = r.script.requests_of("coach").pop().unwrap();
    let told = texts(&review);
    assert!(
        told.contains("handed in by the app at the deadline"),
        "{told}"
    );
    assert!(told.contains("(no answer)"), "{told}");
    assert!(r.script.unexpected.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_paper_handed_in_before_the_deadline_records_the_time_it_took() {
    let r = rig("exam-early").await;
    let goal = exam_session(&r, 3).await;
    queue_paper(&r);
    queue_review(&r);
    let id = r.h.start_exam(&goal, BUDGET_MIN, Some(3)).await.unwrap().id;
    let v = view(&r.h, &id);

    // Ten minutes in, the learner hands in.
    r.h.skew_clock(600);
    let v2 = view(&r.h, &id);
    let remaining = v2["exam"]["remainingSec"].as_i64().unwrap();
    assert!((2090..=2100).contains(&remaining), "{remaining}");
    assert_eq!(v2["can"]["submitSet"], true);
    r.h.practice_submit_set(&id, answers(&v)).await.unwrap();

    let submit = &practice_events(&r.repo, &id, "setSubmit")[0];
    assert_eq!(
        submit["auto"],
        serde_json::Value::Null,
        "handed in by the learner"
    );
    assert_eq!(submit["elapsedSec"], 600);
    let exam_outcomes: Vec<Value> = outcomes(&r.repo)
        .into_iter()
        .filter(|o| o["format"] == "exam")
        .collect();
    assert_eq!(exam_outcomes.len(), 3);
    assert!(
        exam_outcomes.iter().all(|o| o["durationSec"] == 200),
        "600 s over three problems: {exam_outcomes:?}"
    );
    // The timer found nothing left to do.
    r.h.skew_clock(3000);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(practice_events(&r.repo, &id, "setSubmit").len(), 1);
    assert_eq!(
        outcomes(&r.repo)
            .iter()
            .filter(|o| o["format"] == "exam")
            .count(),
        3
    );
    assert!(r.script.unexpected.lock().unwrap().is_empty());

    // The score: the coach's marks plus nothing for the unanswered problem.
    let v = view(&r.h, &id);
    assert_eq!(v["exam"]["score"], json!({ "awarded": 7, "outOf": 15 }));
    assert_eq!(v["exam"]["autoSubmitted"], false);
    assert_eq!(v["exam"]["elapsedSec"], 600);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn after_a_restart_the_passed_deadline_hands_in_the_saved_draft() {
    let r = rig("exam-restart").await;
    let goal = exam_session(&r, 5).await;
    queue_paper(&r);
    queue_review(&r);
    let id = r.h.start_exam(&goal, BUDGET_MIN, Some(3)).await.unwrap().id;
    let ans = answers(&view(&r.h, &id));
    r.h.practice_exam_save(&id, ans.clone()).unwrap();

    // The app is closed and reopened after the deadline: no timer survived.
    let Rig {
        h,
        base,
        repo,
        config,
        secrets,
        script,
        ..
    } = r;
    drop(h);
    let h2 = harness_with_secrets(&repo, &config, &base, secrets).await;
    h2.skew_clock(i64::from(BUDGET_MIN) * 60 + 600);
    let v = view(&h2, &id);
    assert_eq!(v["exam"]["closed"], true);
    assert_eq!(v["exam"]["submitted"], false);

    // Resuming (what the UI does on finding an expired paper) hands it in.
    h2.practice_resume(&id).await.unwrap();
    let v = view(&h2, &id);
    assert_eq!(v["exam"]["submitted"], true);
    assert_eq!(v["exam"]["autoSubmitted"], true);
    assert_eq!(v["status"], "set-review");
    assert_eq!(
        outcomes(&repo)
            .iter()
            .filter(|o| o["format"] == "exam")
            .count(),
        3
    );
    // A late hand-in is refused, with the paper already in.
    let err = h2
        .practice_submit_set(&id, ans)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("already handed in"), "{err}");
    assert!(script.unexpected.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_late_hand_in_is_refused_and_the_saved_draft_goes_in_instead() {
    let r = rig("exam-late").await;
    let goal = exam_session(&r, 5).await;
    queue_paper(&r);
    queue_review(&r);
    let id = r.h.start_exam(&goal, BUDGET_MIN, Some(3)).await.unwrap().id;
    let ans = answers(&view(&r.h, &id));
    // Only the first answer was saved; the second arrives after the deadline.
    r.h.practice_exam_save(&id, vec![ans[0].clone()]).unwrap();
    r.h.skew_clock(i64::from(BUDGET_MIN) * 60 + 10);
    let err =
        r.h.practice_submit_set(&id, ans)
            .await
            .unwrap_err()
            .to_string();
    assert!(err.contains("time is up"), "{err}");
    let submit = &practice_events(&r.repo, &id, "setSubmit")[0];
    assert_eq!(submit["auto"], true);
    assert_eq!(
        submit["answers"].as_array().unwrap().len(),
        1,
        "the late answer was not accepted: {submit}"
    );
    assert_eq!(
        outcomes(&r.repo)
            .iter()
            .filter(|o| o["format"] == "exam")
            .count(),
        3
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_paper_refuses_what_it_cannot_start() {
    let r = rig("exam-refuse").await;
    let goal = exam_session(&r, 10).await;
    let err =
        r.h.start_exam(&goal, 2, None)
            .await
            .unwrap_err()
            .to_string();
    assert!(err.contains("5 to 360 minutes"), "{err}");
    let err =
        r.h.start_exam(&goal, 400, None)
            .await
            .unwrap_err()
            .to_string();
    assert!(err.contains("5 to 360 minutes"), "{err}");
    let err =
        r.h.start_exam(&goal, 60, Some(50))
            .await
            .unwrap_err()
            .to_string();
    assert!(err.contains("2 to 10"), "{err}");
    let err =
        r.h.start_exam("nope", 60, None)
            .await
            .unwrap_err()
            .to_string();
    assert!(!err.is_empty());
    let err =
        r.h.start_practice(&[], "exam", None)
            .await
            .unwrap_err()
            .to_string();
    assert!(err.contains("start_exam"), "{err}");
    // A paper's default size comes from its budget (15 minutes a problem).
    let plan = r.h.exam_plan(&goal).unwrap();
    assert_eq!(plan.paper.default_budget_min, 60);
    assert_eq!(plan.paper.default_problems, 4);
    assert_eq!(
        r.h.list_sessions()
            .unwrap()
            .iter()
            .filter(|s| s.mode == harness::Mode::Practice)
            .count(),
        0,
        "nothing is left behind"
    );
}
