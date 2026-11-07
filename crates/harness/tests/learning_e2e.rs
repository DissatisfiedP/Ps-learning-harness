//! Slice 2 end-to-end proof (docs/learning-system.md §4): plan-card edits,
//! the planner's one question on the card, and known sections that are
//! verified instead of trusted. The scripted provider is the one in
//! `common/mod.rs`, so every path runs through the real runtime: commands,
//! gates, planner jobs, delivery and prompt composition.

mod common;

use std::sync::Arc;

use common::*;
use harness::state::Phase;
use harness::{Harness, Op};
use serde_json::{json, Value};
use tokio::sync::Semaphore;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The plan card's edits, shaped as the UI sends them: plan-level fields and
/// a `sections` array keyed by section id.
fn plan_edits() -> Value {
    json!({
        "depth": "deep",
        "sections": [{
            "id": "s1",
            "targetLevel": "analyze",
            "knownNote": "I already know KVL, just not loaded dividers.",
            "learnerNote": "Go slower on the load."
        }]
    })
}

/// [`plan_args`] with section-level fields set (targetLevel, assumedKnown,
/// knownNote, a rewritten exit).
fn plan_with(fields: Value) -> Value {
    let mut plan = plan_args(&[]);
    if let Some(f) = fields.as_object() {
        for (k, v) in f {
            plan["sections"][0][k] = v.clone();
        }
    }
    plan
}

/// Approve the plan card and check that teaching started. The caller has
/// already scripted the tutor's reply for the approve turn (replies are
/// consumed in push order, so push it before anything else the tutor must
/// say).
async fn approve_now(h: &Arc<Harness>, id: &str) {
    h.approve(id, vec![]).await.unwrap();
    assert_eq!(h.session_state(id).unwrap().phase, Phase::Teach);
}

/// A free-format check on the plan's only node, at `level`.
fn check_question(call_id: &'static str, level: &'static str) -> Reply {
    Reply::Calls(
        "",
        vec![(
            call_id,
            "question_ask",
            json!({ "role": "check", "level": level, "nodeId": NODE, "format": "free",
                "prompt": "With 12 V across 3 kΩ and 1 kΩ, what is across the 1 kΩ?",
                "answer": "3 V" }),
        )],
    )
}

/// The planner's user message in `body`: the request text of that run.
fn request_text(body: &Value) -> String {
    body["messages"]
        .as_array()
        .map(|m| {
            m.iter()
                .filter_map(|m| m["content"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn plan_jobs(h: &Harness, id: &str) -> Vec<(String, bool)> {
    ops(h, id)
        .into_iter()
        .filter_map(|o| match o {
            Op::Job(j) if j.kind == "plan" && j.state != "running" => {
                Some((j.state, j.result.is_some()))
            }
            _ => None,
        })
        .collect()
}

/// [`session_awaiting_approval`] with a goal record of the caller's choosing:
/// the goal's purpose decides whether hunts are allowed at all (§5).
async fn session_awaiting_approval_for(
    h: &Arc<Harness>,
    s: &Script,
    release: Arc<Semaphore>,
    goal: Value,
    planner: Vec<Reply>,
) -> String {
    s.push("tutor", Reply::Calls("", vec![("g1", "goal_set", goal)]));
    s.push("tutor", Reply::Text("Let's begin."));
    s.push("title", Reply::Text("Voltage dividers"));
    let id = h
        .start_teaching("I want to learn voltage dividers")
        .await
        .unwrap()
        .id;
    s.push("researcher", Reply::Calls("", vec![("b1", "brief_submit", json!({
        "kind": "scope", "summary": "Dividers rest on one current through series parts.",
        "strands": [{"name": "series current", "why": "root", "kind": "mechanism", "prerequisites": []}],
        "misconceptions": [], "sources": [] }))]));
    s.push(
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
    s.push("tutor", Reply::Text("The plan is being built."));
    for reply in planner {
        s.push("planner", reply);
    }
    release.add_permits(1);
    h.wait_background(&id).await;
    assert_eq!(
        h.session_state(&id).unwrap().phase,
        Phase::AwaitApproval,
        "unexpected: {:?}",
        s.unexpected.lock().unwrap()
    );
    id
}

// ---------------------------------------------------------------------------
// Plan edits
// ---------------------------------------------------------------------------

/// One "Send changes" is one `planEdit` op and one planner job, and the job's
/// request spells out every edited field plus the rules (§4).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn edit_plan_writes_one_op_and_one_planner_job() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("plan-edit");
    let h = harness(&repo, &config, &base).await;

    let first = plan_with(json!({ "targetLevel": "use" }));
    let id = session_awaiting_approval(
        &h,
        &script,
        &release,
        vec![Reply::Calls("", vec![("w1", "plan_write", first)])],
    )
    .await;
    script.push("tutor", Reply::Text("Let's start."));
    approve_now(&h, &id).await;
    assert_eq!(
        plan_jobs(&h, &id),
        [("queued".to_owned(), false), ("done".to_owned(), false)],
        "the first plan carries no notice (it waits for approval)"
    );

    // The planner revises at the level the learner asked for; the minor
    // revision applies with a notice and wakes the tutor.
    script.push(
        "planner",
        Reply::Calls(
            "",
            vec![(
                "w2",
                "plan_write",
                plan_with(
                    json!({ "targetLevel": "analyze", "exit": "Diagnoses a loaded divider." }),
                ),
            )],
        ),
    );
    script.push("tutor", Reply::Text("I've deepened the aim."));
    h.edit_plan(&id, plan_edits(), Some("Just the first part."))
        .await
        .unwrap();
    h.wait_background(&id).await;

    // Exactly one op, holding the diff verbatim plus the comment.
    let edits: Vec<Value> = ops(&h, &id)
        .into_iter()
        .filter_map(|o| match &o {
            Op::PlanEdit(_) => Some(serde_json::to_value(&o).unwrap()),
            _ => None,
        })
        .collect();
    assert_eq!(edits.len(), 1, "{edits:?}");
    assert_eq!(edits[0]["edits"], plan_edits());
    assert_eq!(edits[0]["comment"], "Just the first part.");
    assert_eq!(edits[0]["op"], "planEdit");

    // Exactly one more planner job (the initial one plus the edit), and the
    // revision's notice is delivered.
    assert_eq!(
        plan_jobs(&h, &id),
        [
            ("queued".to_owned(), false),
            ("done".to_owned(), false),
            ("queued".to_owned(), false),
            ("done".to_owned(), true)
        ]
    );
    let st = h.session_state(&id).unwrap();
    assert_eq!(st.plan_version, 2, "the revision was written once");
    let section = &st.plan.as_ref().unwrap().sections[0];
    assert_eq!(section.target_level, Some(harness::state::Level::Analyze));
    assert_eq!(section.exit, "Diagnoses a loaded divider.");
    // A minor revision needs no new approval.
    assert_eq!(st.phase, Phase::Teach);

    // The request text: every edited field by name, the comment, the JSON and
    // the rules from §4.
    let request = request_text(&script.requests_of("planner").pop().unwrap());
    for expected in [
        "Plan edits from the learner (plan card)",
        "- depth: \"deep\"",
        "- `s1`:",
        "targetLevel: \"analyze\"",
        "knownNote: \"I already know KVL, just not loaded dividers.\"",
        "learnerNote: \"Go slower on the load.\"",
        "Learner's comment: \"Just the first part.\"",
        "Edits as JSON:",
        "How to apply these edits",
        "`keep: false`",
        "`assumedKnown: true`",
        "needs the learner's approval",
    ] {
        assert!(
            request.contains(expected),
            "missing {expected:?} in\n{request}"
        );
    }
    // The planner's prompt carries the plan-controls block too.
    let system = script.requests_of("planner").pop().unwrap()["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        system.contains("plan_ask"),
        "plan_controls is on for the planner"
    );
    assert!(
        script.unexpected.lock().unwrap().is_empty(),
        "unexpected: {:?}",
        script.unexpected.lock().unwrap()
    );
}

/// `plan_ask` renders as a pending question, ends the planner run without a
/// notice, and the answer writes one `planAnswer` and starts a new run that
/// reaches `plan_write` (§4).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_planner_question_pauses_the_plan_and_the_answer_resumes_it() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("plan-ask");
    let h = harness(&repo, &config, &base).await;

    let first = plan_with(json!({ "targetLevel": "use" }));
    let id = session_awaiting_approval(
        &h,
        &script,
        &release,
        vec![Reply::Calls("", vec![("w1", "plan_write", first)])],
    )
    .await;
    script.push("tutor", Reply::Text("Let's start."));
    approve_now(&h, &id).await;

    // The planner asks instead of guessing. The run ends here.
    script.push(
        "planner",
        Reply::Calls(
            "",
            vec![(
                "a1",
                "plan_ask",
                json!({ "question": "Which part do you already know?",
                    "options": ["The series current", "The load effect"] }),
            )],
        ),
    );
    h.edit_plan(&id, plan_edits(), None).await.unwrap();
    h.wait_background(&id).await;

    let asked = results(&h, &id, "plan_ask");
    assert_eq!(
        asked,
        [(
            "Question sent to the learner. The plan waits for their answer.".to_owned(),
            false
        )]
    );
    let st = h.session_state(&id).unwrap();
    let pending = st.pending_plan_question.clone().unwrap();
    assert_eq!(pending.call_id, "a1");
    assert_eq!(pending.question, "Which part do you already know?");
    assert_eq!(pending.options, ["The series current", "The load effect"]);
    assert_eq!(st.plan_version, 1, "no plan was written");
    // The run ended with no delivered notice: nothing new to deliver.
    assert!(st.queued_jobs.is_empty(), "{:?}", st.queued_jobs);
    assert_eq!(
        plan_jobs(&h, &id),
        [
            ("queued".to_owned(), false),
            ("done".to_owned(), false),
            ("queued".to_owned(), false),
            ("done".to_owned(), false)
        ],
        "the asking run is done and delivered nothing"
    );
    let notices = ops(&h, &id)
        .into_iter()
        .filter(|o| matches!(o, Op::Message(m) if m.tag.as_deref() == Some("plan-notice")))
        .count();
    assert_eq!(notices, 1, "only the approval notice");

    // The answer: one op, then a fresh run that writes the plan.
    script.push(
        "planner",
        Reply::Calls(
            "",
            vec![(
                "w2",
                "plan_write",
                plan_with(
                    json!({ "targetLevel": "analyze", "exit": "Diagnoses a loaded divider." }),
                ),
            )],
        ),
    );
    script.push("tutor", Reply::Text("Deeper aim set."));
    h.answer_plan_question(&id, "The series current")
        .await
        .unwrap();
    h.wait_background(&id).await;

    let answers: Vec<Value> = ops(&h, &id)
        .into_iter()
        .filter_map(|o| match &o {
            Op::PlanAnswer(_) => Some(serde_json::to_value(&o).unwrap()),
            _ => None,
        })
        .collect();
    assert_eq!(answers.len(), 1, "{answers:?}");
    assert_eq!(answers[0]["callId"], "a1");
    assert_eq!(answers[0]["answer"], "The series current");
    assert_eq!(answers[0]["op"], "planAnswer");

    let st = h.session_state(&id).unwrap();
    assert_eq!(st.pending_plan_question, None, "the question is answered");
    assert_eq!(st.plan_version, 2);
    assert_eq!(
        st.plan.as_ref().unwrap().sections[0].target_level,
        Some(harness::state::Level::Analyze)
    );
    assert_eq!(st.phase, Phase::Teach);

    // The answering run carries the question, the options, the answer and the
    // edit it was asked about.
    let request = request_text(&script.requests_of("planner").pop().unwrap());
    for expected in [
        "The learner answered your plan question.",
        "Question: Which part do you already know?",
        "Options: The series current / The load effect",
        "Learner's answer: The series current",
        "Plan edits from the learner (plan card)",
        "targetLevel: \"analyze\"",
    ] {
        assert!(
            request.contains(expected),
            "missing {expected:?} in\n{request}"
        );
    }
    assert!(
        script.unexpected.lock().unwrap().is_empty(),
        "unexpected: {:?}",
        script.unexpected.lock().unwrap()
    );
}

// ---------------------------------------------------------------------------
// Known sections
// ---------------------------------------------------------------------------

/// An assumed-known section closes on one passing check at its target level,
/// and its teach prompt carries the learner's words (`known_section`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_known_section_closes_on_one_check_at_its_target_level() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("known-pass");
    let h = harness(&repo, &config, &base).await;

    let known = plan_with(json!({
        "targetLevel": "use",
        "assumedKnown": true,
        "knownNote": "I already know the divider rule."
    }));
    let id = session_awaiting_approval(
        &h,
        &script,
        &release,
        vec![Reply::Calls("", vec![("w1", "plan_write", known)])],
    )
    .await;
    let section = &h.session_state(&id).unwrap().plan.clone().unwrap().sections[0];
    assert!(section.assumed_known);

    // The approve turn asks the one check; there is no room for other text.
    script.push("tutor", check_question("q1", "use"));
    approve_now(&h, &id).await;
    // One check at the target level, tagged to the section's only node.
    let asked = script.requests_of("tutor").pop().unwrap();
    let system = asked["messages"][0]["content"].as_str().unwrap();
    assert!(
        system.contains("I already know the divider rule."),
        "the known note reaches teach.md"
    );
    assert!(
        system.contains("you said you know this"),
        "the known_section block is composed"
    );
    assert!(!tool_names(&asked).contains(&"plan_write".to_owned()));

    script.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "v1",
                "verdict_record",
                json!({ "questionId": "q1", "verdict": "correct", "note": "exact" }),
            )],
        ),
    );
    script.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "d1",
                "section_done",
                json!({ "sectionId": "s1", "evidence": "q1 correct at use", "carryOver": "-" }),
            )],
        ),
    );
    script.push("tutor", Reply::Text("That's the whole plan."));
    h.answer(&id, "q1", answer("3 V")).await.unwrap();
    h.wait_background(&id).await;

    let done = results(&h, &id, "section_done");
    assert_eq!(
        done,
        [(
            "That was the last section. Write the close, then call session_end.".to_owned(),
            false
        )]
    );

    let st = h.session_state(&id).unwrap();
    assert_eq!(st.completed_sections, ["s1"]);
    assert!(st.known_refuted.is_empty(), "a pass never refutes");
    assert!(!ops(&h, &id)
        .iter()
        .any(|o| matches!(o, Op::KnownRefuted(_))));
    assert_eq!(st.phase, Phase::Close);
    assert!(
        script.unexpected.lock().unwrap().is_empty(),
        "unexpected: {:?}",
        script.unexpected.lock().unwrap()
    );
}

/// A missed check appends `knownRefuted` (code's job, no planner call) and
/// the next `section_done` needs the per-node rule.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_missed_check_refutes_the_known_section() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("known-miss");
    let h = harness(&repo, &config, &base).await;

    let known = plan_with(json!({
        "targetLevel": "use",
        "assumedKnown": true,
        "knownNote": "I already know the divider rule."
    }));
    let id = session_awaiting_approval(
        &h,
        &script,
        &release,
        vec![Reply::Calls("", vec![("w1", "plan_write", known)])],
    )
    .await;

    script.push("tutor", check_question("q1", "use"));
    approve_now(&h, &id).await;

    script.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "v1",
                "verdict_record",
                json!({ "questionId": "q1", "verdict": "wrong", "note": "read the load as series" }),
            )],
        ),
    );
    script.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "d1",
                "section_done",
                json!({ "sectionId": "s1", "evidence": "they said they knew it", "carryOver": "-" }),
            )],
        ),
    );
    script.push("tutor", Reply::Text("Let me teach this one after all."));
    h.answer(&id, "q1", answer("9 V")).await.unwrap();
    h.wait_background(&id).await;

    // The refutation: one op, by code, naming the section and the question.
    let refuted: Vec<Value> = ops(&h, &id)
        .into_iter()
        .filter_map(|o| match &o {
            Op::KnownRefuted(_) => Some(serde_json::to_value(&o).unwrap()),
            _ => None,
        })
        .collect();
    assert_eq!(refuted.len(), 1, "{refuted:?}");
    assert_eq!(refuted[0]["sectionId"], "s1");
    assert_eq!(refuted[0]["questionId"], "q1");
    assert_eq!(refuted[0]["op"], "knownRefuted");

    let st = h.session_state(&id).unwrap();
    assert_eq!(st.known_refuted, ["s1"]);
    assert!(st.completed_sections.is_empty());
    assert_eq!(st.phase, Phase::Teach, "the tutor teaches it right there");
    // No planner was woken by a known-section miss.
    assert_eq!(
        plan_jobs(&h, &id),
        [("queued".to_owned(), false), ("done".to_owned(), false)]
    );

    // The fallback gate: every node needs a passing check now.
    let done = results(&h, &id, "section_done");
    assert_eq!(done.len(), 1, "{done:?}");
    assert!(done[0].1, "refused: {:?}", done[0]);
    assert!(
        done[0].0.contains("no passing check question yet") && done[0].0.contains(NODE),
        "{done:?}"
    );

    // The section is no longer taught as known: the block is gone.
    let system = script.requests_of("tutor").pop().unwrap()["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        !system.contains("you said you know this"),
        "a refuted section un-greys"
    );
    assert!(
        script.unexpected.lock().unwrap().is_empty(),
        "unexpected: {:?}",
        script.unexpected.lock().unwrap()
    );
}

// ---------------------------------------------------------------------------
// Hunts and the frontier (§5)
// ---------------------------------------------------------------------------

/// [`plan_args`] with the learner's `explore` choice and a hunt the planner
/// marked available on the only section.
fn plan_with_hunt(explore: &str) -> Value {
    let mut plan = plan_with(json!({
        "targetLevel": "use",
        "hunt": { "available": true, "reason": "Practitioners argue about loaded dividers." }
    }));
    plan["explore"] = json!(explore);
    plan
}

/// The tutor's hunt card for the only section.
fn hunt_send_call(call_id: &'static str) -> Reply {
    Reply::Calls(
        "",
        vec![(
            call_id,
            "hunt_send",
            json!({
                "sectionId": "s1",
                "question": "Find out why op-amp circuits almost always use negative feedback, and where people disagree about it.",
                "lookAt": [
                    { "kind": "forum", "venue": "EEVblog", "why": "people argue about feedback here" },
                    { "kind": "datasheet", "why": "the app notes are the settled version" }
                ],
                "bringBack": ["claims, with sources if you have them", "open questions",
                    "anything surprising", "practical tips"]
            }),
        )],
    )
}

fn hunt_ops(h: &Harness, id: &str) -> Vec<Value> {
    ops(h, id)
        .into_iter()
        .filter_map(|o| match &o {
            Op::HuntSend(_)
            | Op::HuntReturn(_)
            | Op::HuntSkip(_)
            | Op::Debrief(_)
            | Op::HuntToggle(_) => Some(serde_json::to_value(&o).unwrap()),
            _ => None,
        })
        .collect()
}

/// The `hunt` key of every `plan` op, to prove the plan record is never
/// rewritten by the hunt's lifecycle.
fn plan_hunts(h: &Harness, id: &str) -> Vec<Value> {
    ops(h, id)
        .into_iter()
        .filter_map(|o| match &o {
            Op::Plan(_) => serde_json::to_value(&o).ok(),
            _ => None,
        })
        .map(|v| v["payload"]["sections"][0]["hunt"].clone())
        .collect()
}

/// The tutor system prompt of the last request, and its tool names.
fn last_tutor(script: &Script) -> (String, Vec<String>) {
    let body = script.requests_of("tutor").pop().unwrap();
    (
        body["messages"][0]["content"].as_str().unwrap().to_owned(),
        tool_names(&body),
    )
}

/// A hunted section: the learner switches the hunt on, the tutor sends it, and
/// teaching questions stay refused until the debrief is recorded. Nothing in
/// the hunt satisfies `section_done`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hunt_runs_send_return_debrief_then_teaching() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("hunt-path");
    let h = harness(&repo, &config, &base).await;

    let id = session_awaiting_approval(
        &h,
        &script,
        &release,
        vec![Reply::Calls(
            "",
            vec![("w1", "plan_write", plan_with_hunt("suggest"))],
        )],
    )
    .await;

    // Only the learner enables it: one `huntToggle` op, no turn, no planner
    // job. Replay carries it onto the plan section (nothing is being taught
    // yet, so no lifecycle exists and the state line stays quiet).
    let tutor_calls_before = script.requests_of("tutor").len();
    h.set_hunt(&id, "s1", true).unwrap();
    let st = h.session_state(&id).unwrap();
    assert!(
        st.plan.as_ref().unwrap().sections[0]
            .hunt
            .as_ref()
            .unwrap()
            .enabled
    );
    assert_eq!(st.hunt_state(), "", "no section is being taught yet");
    assert_eq!(st.hunt, None, "nothing sent yet");
    assert_eq!(script.requests_of("tutor").len(), tutor_calls_before);
    let toggles: Vec<Value> = ops(&h, &id)
        .into_iter()
        .filter_map(|o| match &o {
            Op::HuntToggle(_) => Some(serde_json::to_value(&o).unwrap()),
            _ => None,
        })
        .collect();
    assert_eq!(toggles.len(), 1, "{toggles:?}");
    assert_eq!(toggles[0]["op"], "huntToggle");
    assert_eq!(toggles[0]["sectionId"], "s1");
    assert_eq!(toggles[0]["enabled"], true);
    assert!(
        plan_jobs(&h, &id).len() == 2,
        "the toggle starts no planner run"
    );

    // The approve turn: the tutor sends the hunt, tries to teach anyway, and
    // is refused until the debrief.
    script.push("tutor", hunt_send_call("h1"));
    script.push("tutor", check_question("q1", "use"));
    script.push(
        "tutor",
        Reply::Text("The hunt is on your desk; no deadline."),
    );
    approve_now(&h, &id).await;

    // The send: one op, the call id as its hunt id, the card's fields intact.
    let sent: Vec<Value> = hunt_ops(&h, &id)
        .into_iter()
        .filter(|o| o["op"] == "huntSend")
        .collect();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["op"], "huntSend");
    assert_eq!(sent[0]["huntId"], "h1");
    assert_eq!(sent[0]["sectionId"], "s1");
    assert!(sent[0]["question"]
        .as_str()
        .unwrap()
        .contains("negative feedback"));
    assert_eq!(sent[0]["lookAt"].as_array().unwrap().len(), 2);
    assert_eq!(sent[0]["lookAt"][0]["kind"], "forum");
    assert_eq!(sent[0]["lookAt"][0]["venue"], "EEVblog");
    assert_eq!(sent[0]["bringBack"].as_array().unwrap().len(), 4);
    assert_eq!(
        results(&h, &id, "hunt_send"),
        [(
            "Hunt sent: the learner has the card. End your turn; they come back whenever they like, and you debrief what they found before teaching.".to_owned(),
            false
        )]
    );

    // The plan record itself never carries the learner's toggle.
    assert_eq!(
        plan_hunts(&h, &id),
        [json!({ "available": true, "reason": "Practitioners argue about loaded dividers." })]
    );

    // Teaching is refused, naming the debrief.
    let refused = results(&h, &id, "question_ask");
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert!(refused[0].1, "refused: {:?}", refused[0]);
    assert!(
        refused[0].0.contains("debrief_record") && refused[0].0.contains("hunt"),
        "{refused:?}"
    );

    // `hunt_active` composed the fragment with its state, and the phase has
    // the three hunt tools. The teach-phase request is the first with them.
    let teach = script
        .requests_of("tutor")
        .into_iter()
        .find(|b| tool_names(b).contains(&"hunt_send".to_owned()))
        .expect("a teach-phase request");
    let system = teach["messages"][0]["content"].as_str().unwrap();
    assert!(
        system.contains("This section starts with a hunt"),
        "{system}"
    );
    assert!(system.contains("Hunt state: ready"), "{system}");
    assert!(system.contains("· hunt: ready"), "the state line names it");
    let tools = tool_names(&teach);
    for tool in ["hunt_send", "debrief_record", "frontier_park"] {
        assert!(
            tools.contains(&tool.to_owned()),
            "{tool} missing from {tools:?}"
        );
    }

    let st = h.session_state(&id).unwrap();
    assert_eq!(st.hunt.as_ref().unwrap().state, "out");
    assert_eq!(st.hunt_state(), "out");
    assert!(!st.hunt.as_ref().unwrap().debriefed);

    // The learner comes back. Notes, links and files all optional.
    script.push("tutor", check_question("q1", "use"));
    script.push(
        "tutor",
        Reply::Calls(
            "",
            vec![
                ("d1", "debrief_record", json!({
                    "huntId": "h1",
                    "claims": [
                        { "claim": "Feedback makes the op-amp linear", "verdict": "correct",
                          "note": "yes: the loop keeps the inputs equal" },
                        { "claim": "Put a 100 nF cap on every power pin", "verdict": "tip",
                          "note": "rule of thumb, not a law", "attribution": "hobbyist forums",
                          "scope": "digital ICs, within a few mm of the pin" },
                        { "claim": "Op-amps can't be used as comparators", "verdict": "offroute",
                          "note": "worth its own session" }
                    ]
                })),
                ("s1", "section_done", json!({ "sectionId": "s1",
                    "evidence": "they found things out", "carryOver": "-" })),
            ],
        ),
    );
    script.push("tutor", check_question("q2", "use"));
    h.hunt_return(
        &id,
        "h1",
        json!({
            "notes": "The forum says it's about linearity.",
            "links": ["https://forum.example/feedback"],
            "files": ["sketch.png"]
        }),
    )
    .await
    .unwrap();

    // One return op, and the return itself in the conversation.
    let after = hunt_ops(&h, &id);
    let returned = after.iter().find(|o| o["op"] == "huntReturn").unwrap();
    assert_eq!(returned["huntId"], "h1");
    assert_eq!(returned["notes"], "The forum says it's about linearity.");
    assert_eq!(returned["links"][0], "https://forum.example/feedback");
    assert_eq!(returned["files"][0], "sketch.png");
    let messages: Vec<String> = ops(&h, &id)
        .into_iter()
        .filter_map(|o| match o {
            Op::Message(m) if m.tag.as_deref() == Some("hunt-return") => Some(m.content),
            _ => None,
        })
        .collect();
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(
        messages[0].contains("https://forum.example/feedback"),
        "{messages:?}"
    );
    assert!(messages[0].contains("debrief_record"));

    // Before the debrief the question was still refused; after it teaching
    // continued (the second question is pending, not refused).
    let refused = results(&h, &id, "question_ask");
    assert_eq!(refused.len(), 2, "{refused:?}");
    assert!(
        refused[1].1,
        "still refused before the debrief: {:?}",
        refused[1]
    );
    assert!(refused[1].0.contains("debrief_record"), "{refused:?}");
    let st = h.session_state(&id).unwrap();
    assert_eq!(st.pending_call.as_ref().unwrap().call_id, "q2");

    // The debrief: three claims, sorted by the verdicts from §5.
    let debriefs: Vec<Value> = hunt_ops(&h, &id)
        .into_iter()
        .filter(|o| o["op"] == "debrief")
        .collect();
    assert_eq!(debriefs.len(), 1, "{debriefs:?}");
    assert_eq!(debriefs[0]["huntId"], "h1");
    assert_eq!(debriefs[0]["claims"].as_array().unwrap().len(), 3);
    assert_eq!(debriefs[0]["claims"][1]["verdict"], "tip");
    assert_eq!(st.hunt.as_ref().unwrap().state, "returned");
    assert!(st.hunt.as_ref().unwrap().debriefed);
    assert!(!st.hunt_blocks_teaching());

    // No hunt op closes a section: the exit still needs a passing check.
    let done = results(&h, &id, "section_done");
    assert_eq!(done.len(), 1, "{done:?}");
    assert!(done[0].1, "refused: {:?}", done[0]);
    assert!(
        done[0].0.contains("no passing check question yet"),
        "{done:?}"
    );

    // The returned hunt still frames the section, now with its state.
    let (system, _) = last_tutor(&script);
    assert!(system.contains("Hunt state: returned"), "{system}");
    assert!(system.contains("· hunt: returned"), "{system}");
    assert!(
        script.unexpected.lock().unwrap().is_empty(),
        "unexpected: {:?}",
        script.unexpected.lock().unwrap()
    );
}

/// Skipping the hunt switches it off and starts teaching normally: no debrief
/// is asked for, and the first teaching question goes through.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn skipping_a_hunt_starts_teaching_normally() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("hunt-skip");
    let h = harness(&repo, &config, &base).await;

    let id = session_awaiting_approval(
        &h,
        &script,
        &release,
        vec![Reply::Calls(
            "",
            vec![("w1", "plan_write", plan_with_hunt("suggest"))],
        )],
    )
    .await;
    h.set_hunt(&id, "s1", true).unwrap();

    script.push("tutor", hunt_send_call("h1"));
    script.push("tutor", Reply::Text("Here's your hunt."));
    // The skip wakes the tutor, which may teach at once.
    script.push("tutor", check_question("q1", "use"));
    approve_now(&h, &id).await;
    assert_eq!(h.session_state(&id).unwrap().hunt_state(), "out");

    h.hunt_skip(&id, "h1").await.unwrap();

    // One skip op; the hunt is off in the plan and its state line says so.
    let st = h.session_state(&id).unwrap();
    assert_eq!(st.hunt.as_ref().unwrap().state, "skipped");
    assert!(!st.hunt_active());
    assert_eq!(st.hunt_state(), "skipped");
    assert!(
        !st.plan.as_ref().unwrap().sections[0]
            .hunt
            .as_ref()
            .unwrap()
            .enabled
    );
    let skips = hunt_ops(&h, &id)
        .into_iter()
        .filter(|o| o["op"] == "huntSkip")
        .count();
    assert_eq!(skips, 1);

    // The wake delivered the skip and let the question through.
    let asked = results(&h, &id, "question_ask");
    assert!(asked.is_empty(), "nothing refused: {asked:?}");
    assert_eq!(st.pending_call.as_ref().unwrap().call_id, "q1");
    let (system, _) = last_tutor(&script);
    assert!(
        !system.contains("This section starts with a hunt"),
        "a skipped hunt drops the fragment"
    );
    assert!(system.contains("· hunt: skipped"), "{system}");
    assert!(
        script.unexpected.lock().unwrap().is_empty(),
        "unexpected: {:?}",
        script.unexpected.lock().unwrap()
    );
}

/// Every hunt keeps its own lifecycle (§5): a hunt sent and skipped in section
/// 1 stays finished while section 2's hunt runs, and its card can't be
/// returned again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hunt_in_an_earlier_section_stays_finished() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("hunt-two-sections");
    let h = harness(&repo, &config, &base).await;

    let plan = json!({
        "goal": "Dividers", "approach": "Two sections.", "explore": "suggest",
        "nodes": [
            { "id": "divider-rule", "title": "The divider rule", "kind": "mechanism", "requires": [] },
            { "id": "loaded-divider", "title": "The loaded divider", "kind": "mechanism",
              "requires": ["divider-rule"] }
        ],
        "sections": [
            { "id": "s1", "title": "Dividers", "nodeIds": ["divider-rule"], "mode": "mechanism",
              "style": "mixed", "plugins": [], "notes": "", "exit": "Solves a divider.",
              "targetLevel": "use",
              "hunt": { "available": true, "reason": "Practitioners argue about loaded dividers." } },
            { "id": "s2", "title": "Loaded dividers", "nodeIds": ["loaded-divider"],
              "mode": "mechanism", "style": "mixed", "plugins": [], "notes": "",
              "exit": "Solves a loaded divider.", "targetLevel": "use",
              "hunt": { "available": true, "reason": "Sagging outputs are argued about too." } }
        ]
    });
    let id = session_awaiting_approval(
        &h,
        &script,
        &release,
        vec![Reply::Calls("", vec![("w1", "plan_write", plan)])],
    )
    .await;
    // Both hunts on: only the learner may switch them on.
    h.set_hunt(&id, "s1", true).unwrap();
    h.set_hunt(&id, "s2", true).unwrap();

    script.push("tutor", hunt_send_call("h1"));
    script.push("tutor", Reply::Text("Here's your hunt."));
    // The skip wakes the tutor, which asks the section's check at once.
    script.push("tutor", check_question("q1", "use"));
    approve_now(&h, &id).await;
    assert_eq!(h.session_state(&id).unwrap().hunt_state(), "out");

    h.hunt_skip(&id, "h1").await.unwrap();

    // The pass closes section 1; section 2's hunt is sent in the same turn.
    script.push(
        "tutor",
        Reply::Calls(
            "",
            vec![
                (
                    "v1",
                    "verdict_record",
                    json!({ "questionId": "q1", "verdict": "correct", "note": "exact" }),
                ),
                (
                    "d1",
                    "section_done",
                    json!({ "sectionId": "s1", "evidence": "q1 correct at use", "carryOver": "-" }),
                ),
            ],
        ),
    );
    script.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "h2",
                "hunt_send",
                json!({
                    "sectionId": "s2",
                    "question": "Find out why a loaded divider's output sags.",
                    "lookAt": [{ "kind": "docs", "why": "the app notes are the settled version" }],
                    "bringBack": ["claims", "open questions"]
                }),
            )],
        ),
    );
    script.push("tutor", Reply::Text("Section 2 starts with its own hunt."));
    h.answer(&id, "q1", answer("3 V")).await.unwrap();
    h.wait_background(&id).await;

    let st = h.session_state(&id).unwrap();
    assert_eq!(st.current_section.as_deref(), Some("s2"));
    assert_eq!(st.completed_sections, ["s1"]);
    // Both hunts, in op order, each with its own state.
    let states: Vec<(&str, &str, &str)> = st
        .hunts
        .iter()
        .map(|h| (h.hunt_id.as_str(), h.section_id.as_str(), h.state.as_str()))
        .collect();
    assert_eq!(
        states,
        [("h1", "s1", "skipped"), ("h2", "s2", "out")],
        "section 2's hunt doesn't inherit section 1's state"
    );
    assert_eq!(st.hunt.as_ref().unwrap().hunt_id, "h2");
    assert_eq!(st.hunt_state(), "out");
    // A card is resolved by its id: section 1's is finished, and driving it
    // again is refused.
    let first = st.hunts.iter().find(|h| h.hunt_id == "h1").unwrap();
    assert_eq!(first.state, "skipped");
    let refused = h.hunt_return(&id, "h1", json!({})).await.unwrap_err();
    assert!(
        refused.to_string().contains("already finished"),
        "{refused}"
    );
    assert_eq!(
        h.session_state(&id).unwrap().hunts[0].state,
        "skipped",
        "a refused return changes nothing"
    );
    assert!(
        script.unexpected.lock().unwrap().is_empty(),
        "unexpected: {:?}",
        script.unexpected.lock().unwrap()
    );
}

/// A course or exam goal never gets hunts (§5): `goal_set` forces explore off,
/// and a plan that declares its own explore on is still refused, naming the
/// purpose.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_course_goal_never_gets_hunts() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("course-goal-hunts");
    let h = harness(&repo, &config, &base).await;

    let id = session_awaiting_approval_for(
        &h,
        &script,
        release,
        json!({ "goal": "Pass the electronics course", "purpose": "course", "explore": "on",
            "successLooksLike": ["solve a divider"], "materials": [], "outOfScope": [],
            "preferences": [], "orientationFirst": false }),
        vec![
            Reply::Calls("", vec![("w1", "plan_write", plan_with_hunt("on"))]),
            Reply::Calls(
                "",
                vec![(
                    "w2",
                    "plan_write",
                    plan_with(json!({ "targetLevel": "use" })),
                )],
            ),
        ],
    )
    .await;

    // Code forced explore off whatever the tutor asked for.
    let st = h.session_state(&id).unwrap();
    assert_eq!(st.goal.as_ref().unwrap()["purpose"], "course");
    assert_eq!(st.goal.as_ref().unwrap()["explore"], "off");

    // The planner's own `explore: "on"` can't bring a hunt back.
    let writes = results(&h, &id, "plan_write");
    assert_eq!(writes.len(), 2, "{writes:?}");
    assert!(writes[0].1, "refused: {:?}", writes[0]);
    assert!(
        writes[0]
            .0
            .contains("section `s1` marks a hunt available, but this is a `course` goal"),
        "{writes:?}"
    );
    assert!(!writes[1].1, "the plain plan is accepted: {writes:?}");

    let st = h.session_state(&id).unwrap();
    assert_eq!(st.phase, Phase::AwaitApproval);
    assert_eq!(st.plan_version, 1, "only the accepted plan was written");
    assert!(st.plan.as_ref().unwrap().sections[0].hunt.is_none());
    assert!(
        script.unexpected.lock().unwrap().is_empty(),
        "unexpected: {:?}",
        script.unexpected.lock().unwrap()
    );
}

/// `hunt_send` is refused while the learner hasn't switched the hunt on: the
/// planner can only make one available.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hunt_send_is_refused_until_the_learner_enables_it() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("hunt-not-enabled");
    let h = harness(&repo, &config, &base).await;

    let id = session_awaiting_approval(
        &h,
        &script,
        &release,
        vec![Reply::Calls(
            "",
            vec![("w1", "plan_write", plan_with_hunt("suggest"))],
        )],
    )
    .await;

    script.push("tutor", hunt_send_call("h1"));
    script.push("tutor", Reply::Text("Let's start teaching instead."));
    approve_now(&h, &id).await;

    let refused = results(&h, &id, "hunt_send");
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert!(refused[0].1, "refused: {:?}", refused[0]);
    assert!(
        refused[0].0.contains("only the learner can enable a hunt"),
        "{refused:?}"
    );
    let st = h.session_state(&id).unwrap();
    assert_eq!(st.hunt, None, "no hunt was sent");
    assert!(hunt_ops(&h, &id).is_empty(), "no hunt op at all");
    // Without the toggle the fragment is dropped and the state line quiet.
    let (system, tools) = last_tutor(&script);
    assert!(
        !system.contains("This section starts with a hunt"),
        "{system}"
    );
    assert!(!system.contains("· hunt:"), "{system}");
    // The hunt tools are offered anyway: the gate, not the schema, refuses.
    assert!(tools.contains(&"hunt_send".to_owned()), "{tools:?}");
    assert!(
        script.unexpected.lock().unwrap().is_empty(),
        "unexpected: {:?}",
        script.unexpected.lock().unwrap()
    );
}

/// `plan_write` may only make a hunt available, and only with explore on:
/// `hunt.enabled` is always refused, and `available` is refused at `off`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plan_write_refuses_an_enabled_hunt_and_a_hunt_with_explore_off() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("hunt-plan-refusals");
    let h = harness(&repo, &config, &base).await;

    let enabled = plan_with(json!({
        "targetLevel": "use",
        "hunt": { "available": true, "reason": "lore", "enabled": true }
    }));
    let explore_off = plan_with(json!({
        "targetLevel": "use",
        "hunt": { "available": true, "reason": "lore" }
    }));
    let id = session_awaiting_approval(
        &h,
        &script,
        &release,
        vec![
            Reply::Calls(
                "",
                vec![("w1", "plan_write", {
                    let mut p = enabled.clone();
                    p["explore"] = json!("suggest");
                    p
                })],
            ),
            Reply::Calls("", vec![("w2", "plan_write", explore_off)]),
            Reply::Calls(
                "",
                vec![(
                    "w3",
                    "plan_write",
                    plan_with(json!({ "targetLevel": "use" })),
                )],
            ),
        ],
    )
    .await;

    let writes = results(&h, &id, "plan_write");
    assert_eq!(writes.len(), 3, "{writes:?}");
    assert!(writes[0].1, "{writes:?}");
    assert!(
        writes[0].0.contains("only the learner can enable a hunt"),
        "{writes:?}"
    );
    assert!(writes[1].1, "{writes:?}");
    assert!(
        writes[1]
            .0
            .contains("hunt available while `explore` is `off`"),
        "{writes:?}"
    );
    assert!(!writes[2].1, "the third plan is accepted: {writes:?}");

    let st = h.session_state(&id).unwrap();
    assert_eq!(st.plan_version, 1, "only the accepted plan was written");
    assert!(st.plan.as_ref().unwrap().sections[0].hunt.is_none());
    assert!(
        script.unexpected.lock().unwrap().is_empty(),
        "unexpected: {:?}",
        script.unexpected.lock().unwrap()
    );
}

/// Off-route topics go to `frontier.jsonl`, the command reads them back, and
/// a frontier item opens a new session with the learner's own words.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_frontier_item_opens_a_new_session() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("frontier");
    let h = harness(&repo, &config, &base).await;

    let id = session_awaiting_approval(
        &h,
        &script,
        &release,
        vec![Reply::Calls(
            "",
            vec![(
                "w1",
                "plan_write",
                plan_with(json!({ "targetLevel": "use" })),
            )],
        )],
    )
    .await;

    assert!(h.frontier().unwrap().is_empty(), "nothing parked yet");
    script.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "f1",
                "frontier_park",
                json!({ "topic": "Loaded voltage dividers", "why": "why the output sags" }),
            )],
        ),
    );
    script.push("tutor", Reply::Text("Saved that for later."));
    approve_now(&h, &id).await;

    let parked = results(&h, &id, "frontier_park");
    assert_eq!(
        parked,
        [(
            "Parked on the frontier: Loaded voltage dividers. The learner can start a session from it later.".to_owned(),
            false
        )]
    );

    // The line is the documented record, outside the session log.
    let text = std::fs::read_to_string(repo.join("frontier.jsonl")).unwrap();
    let line: Value = serde_json::from_str(text.trim()).unwrap();
    assert_eq!(line["op"], "frontier");
    assert_eq!(line["topic"], "Loaded voltage dividers");
    assert_eq!(line["why"], "why the output sags");
    assert_eq!(line["from"]["sessionId"], id.as_str());
    assert!(line["from"].get("huntId").is_none(), "no hunt, no key");
    assert!(!line["opId"].as_str().unwrap().is_empty());
    assert!(!line["at"].as_str().unwrap().is_empty());
    // The record lives in the frontier file, not in the session log.
    assert!(
        !raw_op_logs(&repo).contains("\"op\":\"frontier\""),
        "the frontier record is not a session op"
    );

    let items = h.frontier().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].topic, "Loaded voltage dividers");
    assert_eq!(items[0].why, "why the output sags");
    assert_eq!(items[0].session_id, id);

    // Starting from it: a new Learn session whose opening words are the item.
    script.push("tutor", Reply::Text("Let's go."));
    let created = h.start_from_frontier(&items[0].id).await.unwrap();
    assert_ne!(created.id, id);
    let opening = ops(&h, &created.id)
        .into_iter()
        .find_map(|o| match o {
            Op::Message(m) if m.tag.is_none() => Some(m.content),
            _ => None,
        })
        .unwrap();
    assert!(opening.contains("Loaded voltage dividers"), "{opening}");
    assert!(opening.contains("why the output sags"), "{opening}");

    // An id that isn't on the list names itself.
    let err = h
        .start_from_frontier("nope")
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("nope"), "{err}");
    assert!(
        script.unexpected.lock().unwrap().is_empty(),
        "unexpected: {:?}",
        script.unexpected.lock().unwrap()
    );
}

// ---------------------------------------------------------------------------
// Starting point (§3)
// ---------------------------------------------------------------------------

/// A learner who starts "new to this" gets no probe questions: the gate
/// refuses one outright, `probe_done` still waits for the brief, and then it
/// accepts every strand as `unprobed`/`new` — the first check of each section
/// is the measurement. The saved device default is what `goal_set` fills in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_learner_is_not_probed_and_the_plan_is_built_from_the_brief() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("starting-point-new");
    let h = harness(&repo, &config, &base).await;
    h.set_starting_point("new").unwrap();

    script.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "g1",
                "goal_set",
                json!({ "goal": "Voltage dividers", "purpose": "curiosity",
                    "successLooksLike": ["solve a divider"], "materials": [], "outOfScope": [],
                    "preferences": [], "orientationFirst": false }),
            )],
        ),
    );
    script.push("tutor", Reply::Text("Let's start from the foundations."));
    script.push("title", Reply::Text("Voltage dividers"));
    let id = h.start_teaching("I'm new to circuits").await.unwrap().id;

    let st = h.session_state(&id).unwrap();
    assert_eq!(st.starting_point(), "new", "the saved default is used");
    assert_eq!(
        st.goal.as_ref().unwrap()["startingPoint"],
        "new",
        "goal_set filled the default in"
    );

    // Before the brief: probe_done waits for it, and a probe question is
    // refused because the learner starts new.
    script.push(
        "tutor",
        Reply::Calls(
            "",
            vec![
                ("p0", "probe_done", json!({ "strands": [] })),
                (
                    "q0",
                    "question_ask",
                    json!({ "role": "probe", "level": "use",
                    "format": "free", "prompt": "Why is the current the same?",
                    "answer": "one path", "strand": "series current" }),
                ),
            ],
        ),
    );
    script.push("tutor", Reply::Text("Waiting for the brief."));
    h.resume_turn(&id).await.unwrap();

    let probe_done = results(&h, &id, "probe_done");
    assert_eq!(probe_done.len(), 1, "{probe_done:?}");
    assert!(
        probe_done[0].1 && probe_done[0].0.contains("brief"),
        "probe_done waits for the brief: {probe_done:?}"
    );
    let probes = results(&h, &id, "question_ask");
    assert_eq!(probes.len(), 1, "{probes:?}");
    assert!(
        probes[0].1 && probes[0].0.contains("starts new"),
        "no probe question for a new learner: {probes:?}"
    );
    assert_eq!(h.session_state(&id).unwrap().phase, Phase::Measure);

    // The brief arrives; every strand is recorded as new, and the planner
    // builds the plan from the conversation.
    script.push(
        "researcher",
        Reply::Calls(
            "",
            vec![(
                "b1",
                "brief_submit",
                json!({
                    "kind": "scope", "summary": "One current through series parts.",
                    "strands": [{ "name": "series current", "why": "root",
                        "kind": "mechanism", "prerequisites": [] }],
                    "misconceptions": [], "sources": [] }),
            )],
        ),
    );
    script.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "p1",
                "probe_done",
                json!({
                    "strands": [{ "name": "series current", "confidence": "unprobed",
                        "reason": "new" }],
                    "notes": "starts from scratch" }),
            )],
        ),
    );
    script.push("tutor", Reply::Text("The plan is being built."));
    script.push(
        "planner",
        Reply::Calls(
            "",
            vec![(
                "w1",
                "plan_write",
                plan_with(json!({ "targetLevel": "use" })),
            )],
        ),
    );
    release.add_permits(1);
    h.wait_background(&id).await;

    let st = h.session_state(&id).unwrap();
    assert_eq!(
        st.phase,
        Phase::AwaitApproval,
        "unexpected: {:?}",
        script.unexpected.lock().unwrap()
    );
    assert_eq!(st.probe.len(), 1, "the strand was recorded");
    assert_eq!(st.probe[0]["confidence"], "unprobed");
    assert_eq!(st.probe[0]["reason"], "new");
    let asked = results(&h, &id, "question_ask")
        .into_iter()
        .filter(|(_, error)| !error)
        .count();
    assert_eq!(asked, 0, "no probe question was accepted");
    assert!(
        script.unexpected.lock().unwrap().is_empty(),
        "unexpected: {:?}",
        script.unexpected.lock().unwrap()
    );
}
