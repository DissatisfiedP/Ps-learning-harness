//! Branch sessions end to end (architecture §4.26): a plan split into two
//! root tracks and a merge track. Approval keeps the first track in the
//! session and forks the second into a new session that starts with a copy
//! of the pre-plan conversation; ending both branches creates exactly one
//! merge session, which opens with a revision. Bad track graphs are refused.
//! Same scripted provider and real runtime as the other e2e tests.

mod common;

use std::sync::Arc;

use common::*;
use harness::state::Phase;
use harness::{Harness, Op};
use serde_json::{json, Value};
use tokio::sync::Semaphore;

/// Two independent strands (`dc`, `ac`) that merge into `filters`.
fn branching_plan() -> Value {
    json!({
        "goal": "Filters", "approach": "Two strands, then filters.",
        "nodes": [
            {"id": "ohm", "title": "Ohm's law", "kind": "mechanism", "requires": []},
            {"id": "sine", "title": "Sine waves", "kind": "mechanism", "requires": []},
            {"id": "rc", "title": "RC filter", "kind": "mechanism", "requires": ["ohm", "sine"]}
        ],
        "sections": [
            {"id": "s-dc", "title": "DC", "nodeIds": ["ohm"], "mode": "mechanism",
             "style": "mixed", "plugins": [], "notes": "", "exit": "Uses Ohm's law."},
            {"id": "s-ac", "title": "AC", "nodeIds": ["sine"], "mode": "mechanism",
             "style": "mixed", "plugins": [], "notes": "", "exit": "Reads a sine."},
            {"id": "s-rc", "title": "Filters", "nodeIds": ["rc"], "mode": "mechanism",
             "style": "mixed", "plugins": [], "notes": "", "exit": "Designs an RC filter."}
        ],
        "tracks": [
            {"id": "dc", "title": "DC circuits", "sectionIds": ["s-dc"], "after": []},
            {"id": "ac", "title": "AC signals", "sectionIds": ["s-ac"], "after": []},
            {"id": "filters", "title": "Filters", "sectionIds": ["s-rc"], "after": ["dc", "ac"]}
        ]
    })
}

fn check(call_id: &'static str, node: &str) -> Reply {
    Reply::Calls(
        "",
        vec![(
            call_id,
            "question_ask",
            json!({ "role": "check", "level": "use", "nodeId": node, "format": "free",
                "prompt": "Apply it.", "answer": "a" }),
        )],
    )
}

fn call(id: &'static str, name: &'static str, args: Value) -> Reply {
    Reply::Calls("", vec![(id, name, args)])
}

/// The scripted rest of a branch after its check `q` was asked: verdict,
/// `section_done`, `session_end` (and the graph build that closing starts).
/// `ids` are the four call ids.
fn finish_branch(s: &Script, ids: [&'static str; 4], q: &str, section: &str) {
    let [verdict, done, end, graph] = ids;
    s.push(
        "tutor",
        call(
            verdict,
            "verdict_record",
            json!({ "questionId": q, "verdict": "correct", "note": "ok" }),
        ),
    );
    s.push(
        "tutor",
        call(
            done,
            "section_done",
            json!({ "sectionId": section, "evidence": format!("{q} correct"), "carryOver": format!("carry from {section}") }),
        ),
    );
    s.push("tutor", call(end, "session_end", json!({})));
    s.push("graph", call(graph, "graph_submit", json!({})));
}

fn branch_children(h: &Harness, id: &str) -> Vec<(String, String)> {
    ops(h, id)
        .into_iter()
        .filter_map(|o| match o {
            Op::Branch(b) => Some(
                b.children
                    .into_iter()
                    .map(|c| (c.session_id, c.track))
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .flatten()
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_branching_plan_forks_at_approval_and_merges_once_when_both_branches_end() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("branches-fork-merge");
    let h = harness(&repo, &config, &base).await;

    let origin = session_awaiting_approval(
        &h,
        &script,
        &release,
        vec![call("w1", "plan_write", branching_plan())],
    )
    .await;
    assert_eq!(
        h.list_sessions().unwrap().len(),
        1,
        "nothing forks before approval"
    );

    // Approve: the origin keeps `dc` and asks its first check; `ac` becomes a
    // new session that does not run.
    script.push("tutor", check("q1", "ohm"));
    let tutor_calls = script.requests_of("tutor").len();
    h.approve(&origin, vec![]).await.unwrap();
    h.wait_background(&origin).await;
    assert_eq!(
        script.requests_of("tutor").len(),
        tutor_calls + 1,
        "only the origin ran"
    );

    let children = branch_children(&h, &origin);
    assert_eq!(children.len(), 1, "{children:?}");
    let (fork, track) = children[0].clone();
    assert_eq!(track, "ac");

    let o = h.session_state(&origin).unwrap();
    assert_eq!(o.phase, Phase::Teach);
    assert_eq!(o.current_section.as_deref(), Some("s-dc"));
    let ob = o.branch.clone().unwrap();
    assert_eq!((ob.kind.as_str(), ob.track.as_str()), ("origin", "dc"));
    assert_eq!(ob.origin, origin);

    // The fork: a new file with the origin's pre-plan ops copied, then its
    // own approval on its track.
    let f = h.session_state(&fork).unwrap();
    assert_eq!(f.phase, Phase::Teach);
    assert_eq!(f.current_section.as_deref(), Some("s-ac"));
    let fb = f.branch.clone().unwrap();
    assert_eq!(
        (fb.kind.as_str(), fb.track.as_str(), fb.origin.as_str()),
        ("fork", "ac", origin.as_str())
    );
    assert_eq!(f.goal, o.goal, "the goal record is copied");
    assert_eq!(f.probe, o.probe, "the probe map is copied");
    assert_eq!(f.plan.as_ref().unwrap().tracks.len(), 3);
    assert!(f.approved_version.is_some());
    let fork_session = h.load_session(&fork).unwrap();
    assert!(
        !fork_session.summary.read_only,
        "a fork is a runtime session"
    );
    assert!(
        fork_session
            .messages
            .iter()
            .any(|m| m.content == "I want to learn voltage dividers"),
        "the measure transcript is copied"
    );
    let origin_ops = ops(&h, &origin);
    let goal_op = |ops: &[Op]| {
        ops.iter()
            .find_map(|o| match o {
                Op::Goal(g) => Some(g.op_id.clone()),
                _ => None,
            })
            .unwrap()
    };
    assert_eq!(
        goal_op(&origin_ops),
        goal_op(&fork_session.ops),
        "copied ops keep their ids"
    );
    assert_eq!(
        h.graph_status(&fork).unwrap().uncovered_tokens,
        0,
        "the copied conversation is never extracted a second time"
    );

    // The list shows the lineage: the fork is ready, never started.
    let list = h.list_sessions().unwrap();
    assert_eq!(list.len(), 2);
    let fork_row = list.iter().find(|s| s.id == fork).unwrap();
    let lineage = fork_row.lineage.as_ref().unwrap();
    assert_eq!(
        (lineage.kind.as_str(), lineage.index, lineage.count),
        ("fork", 2, 3)
    );
    assert!(!lineage.started && !lineage.ended);
    assert_eq!(lineage.feeds.as_ref().unwrap().track, "filters");
    assert_eq!(lineage.waiting_on, ["DC circuits"]);
    let origin_row = list.iter().find(|s| s.id == origin).unwrap();
    assert_eq!(origin_row.lineage.as_ref().unwrap().kind, "origin");

    // The origin teaches its track, hands off and ends: the merge waits.
    finish_branch(&script, ["v1", "d1", "e1", "g1"], "q1", "s-dc");
    h.answer(&origin, "q1", answer("a")).await.unwrap();
    h.wait_background(&origin).await;
    let done = results(&h, &origin, "section_done");
    assert!(done[0].0.contains("hand-off"), "{done:?}");
    let ended = results(&h, &origin, "session_end");
    assert!(
        ended[0]
            .0
            .contains("once these branches end too: AC signals"),
        "{ended:?}"
    );
    assert_eq!(
        h.list_sessions().unwrap().len(),
        2,
        "no merge while `ac` runs"
    );

    // The learner opens the fork and starts it: its prompt has the branch
    // block and the copied conversation.
    script.push("tutor", check("q2", "sine"));
    h.resume_turn(&fork).await.unwrap();
    let body = script.requests_of("tutor").pop().unwrap();
    let system = body["messages"][0]["content"].as_str().unwrap();
    assert!(
        system.contains("one branch of a split plan"),
        "the branch block is composed"
    );
    assert!(
        system.contains("- s-dc [mechanism] nodes: ohm · other session\n"),
        "{system}"
    );
    assert!(
        system.contains("This session (fork) teaches track ac only"),
        "{system}"
    );
    assert!(body
        .to_string()
        .contains("I want to learn voltage dividers"));

    finish_branch(&script, ["v2", "d2", "e2", "g2"], "q2", "s-ac");
    h.answer(&fork, "q2", answer("a")).await.unwrap();
    h.wait_background(&fork).await;
    let ended = results(&h, &fork, "session_end");
    assert!(ended[0].0.contains("merge session is ready"), "{ended:?}");

    // Exactly one merge session, also after listing again.
    let merges = |h: &Harness| -> Vec<String> {
        h.list_sessions()
            .unwrap()
            .into_iter()
            .filter(|s| s.lineage.as_ref().is_some_and(|l| l.kind == "merge"))
            .map(|s| s.id)
            .collect()
    };
    let merge = merges(&h);
    assert_eq!(merge.len(), 1);
    assert_eq!(merges(&h), merge, "listing again creates nothing");
    let merge = merge[0].clone();

    let m = h.session_state(&merge).unwrap();
    assert_eq!(m.phase, Phase::Teach);
    let mb = m.branch.clone().unwrap();
    assert_eq!((mb.kind.as_str(), mb.track.as_str()), ("merge", "filters"));
    assert_eq!(m.completed_sections, ["s-dc", "s-ac"]);
    let current = m.current_section().unwrap().clone();
    assert!(
        current.revision,
        "the merge starts with the revision: {current:?}"
    );
    assert_eq!(current.node_ids, ["ohm", "sine"]);
    let track = m.own_track().unwrap();
    assert_eq!(track.section_ids, [current.id.clone(), "s-rc".to_owned()]);

    // Its first new content is the revision context: what each branch did.
    let merge_session = h.load_session(&merge).unwrap();
    let notice = merge_session
        .messages
        .iter()
        .find(|m| m.tag.as_deref() == Some("branch-notice"))
        .unwrap();
    assert!(notice.content.starts_with("[branches]"));
    assert!(notice.content.contains("revision"));
    assert!(
        notice.content.contains("**DC circuits**") && notice.content.contains("**AC signals**")
    );
    assert!(notice.content.contains("carry from s-ac"));
    assert!(merge_session
        .messages
        .iter()
        .any(|m| m.content == "I want to learn voltage dividers"));
    let row = h
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|s| s.id == merge)
        .unwrap();
    let l = row.lineage.unwrap();
    assert!(!l.started && !l.ended);
    assert_eq!(l.inputs.len(), 2);
    assert!(l.inputs.iter().all(|i| i.ended));
    assert_eq!(h.graph_status(&merge).unwrap().uncovered_tokens, 0);

    // Starting it asks the revision check on a merged node.
    script.push("tutor", check("q3", "sine"));
    h.resume_turn(&merge).await.unwrap();
    assert_eq!(
        h.session_state(&merge)
            .unwrap()
            .pending_call
            .unwrap()
            .call_id,
        "q3"
    );
    assert!(
        script.unexpected.lock().unwrap().is_empty(),
        "unexpected: {:?}",
        script.unexpected.lock().unwrap()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bad_track_graphs_are_refused_and_a_plan_without_tracks_never_forks() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("branches-refusals");
    let h = harness(&repo, &config, &base).await;

    let mut unknown = branching_plan();
    unknown["tracks"][2]["after"] = json!(["dc", "zz"]);
    unknown["tracks"][1]["sectionIds"] = json!([]);
    let mut crossing = branching_plan();
    // `sine` builds on `ohm`, but `ac` runs in parallel with `dc`.
    crossing["nodes"][1]["requires"] = json!(["ohm"]);
    let mut cycle = branching_plan();
    cycle["tracks"][0]["after"] = json!(["filters", "ac"]);

    let id = session_awaiting_approval(
        &h,
        &script,
        &release,
        vec![
            call("w1", "plan_write", unknown),
            call("w2", "plan_write", crossing),
            call("w3", "plan_write", cycle),
            call("w4", "plan_write", plan_args(&[])),
        ],
    )
    .await;
    let writes = results(&h, &id, "plan_write");
    assert_eq!(writes.len(), 4, "{writes:?}");
    assert!(
        writes[0].1 && writes[0].0.contains("after unknown track `zz`"),
        "{writes:?}"
    );
    assert!(
        writes[0].0.contains("section `s-ac` is in no track"),
        "{writes:?}"
    );
    assert!(
        writes[1].1 && writes[1].0.contains("requires `ohm` on track `dc`"),
        "{writes:?}"
    );
    assert!(writes[2].1 && writes[2].0.contains("cycle"), "{writes:?}");
    assert!(
        !writes[3].1,
        "the plan without tracks is accepted: {writes:?}"
    );

    script.push("tutor", Reply::Text("Let's start."));
    h.approve(&id, vec![]).await.unwrap();
    h.wait_background(&id).await;
    let st = h.session_state(&id).unwrap();
    assert!(st.branch.is_none());
    assert_eq!(st.current_section.as_deref(), Some("s1"));
    let list = h.list_sessions().unwrap();
    assert_eq!(list.len(), 1);
    assert!(list[0].lineage.is_none());
    assert!(!ops(&h, &id).iter().any(|o| matches!(o, Op::Branch(_))));
    assert!(
        script.unexpected.lock().unwrap().is_empty(),
        "unexpected: {:?}",
        script.unexpected.lock().unwrap()
    );
}
