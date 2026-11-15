//! What the learner may see of a practice session.
//!
//! [`PracticeView`] is the UI's one read of the state (`practice_state`): the
//! problems (statement once posed, the result and the revealed solution only
//! after it ended), the conversation as a feed, and which buttons are live.
//! The ops themselves hold the solution from the moment the coach commits, so
//! everything that leaves the backend as ops — `load_session` and the `op`
//! events — goes through [`learner_session`] / [`learner_op`], which remove
//! what the learner must not read yet: the plan (it names the target), the
//! committed problem, the checkers' reasons, the coach's private diagnosis,
//! tool arguments and results, and the coach's own text while it is still
//! writing a problem.

use std::collections::HashSet;

use providers::Role;
use serde::Serialize;
use serde_json::{json, Value};

use crate::knowledge::KnowledgeMap;
use crate::learner::{Format, Layer, MissClass, Verdict};
use crate::sessions::{Attachment, Op, Session, StoredMessage};
use crate::state::{Calibration, Level};

use chrono::{DateTime, Duration, Utc};

use super::state::{PracticeState, ProblemRecord, Status};
use super::{paper, tag, Criterion, Move, PracticeEvent, ProblemType, SetAnswer};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeRef {
    pub id: String,
    pub title: String,
    pub home: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reveal {
    pub solution: Vec<Move>,
    pub final_answer: String,
    pub acceptable_variants: Vec<String>,
    pub rubric: Vec<Criterion>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultView {
    pub verdict: Verdict,
    pub hint_rung: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub miss_class: Option<MissClass>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub misconception: Option<String>,
    pub rating: String,
    pub duration_sec: u32,
    pub targets: Vec<NodeRef>,
    pub hosts: Vec<NodeRef>,
    pub layer: Layer,
    pub level: Level,
    pub cue_distance: u8,
    pub is_distractor: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retest_of: Option<String>,
    pub reveal: Reveal,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marks_awarded: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProblemView {
    pub problem_id: String,
    pub index: u32,
    /// `writing` | `open` | `concluded`.
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statement: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<ProblemType>,
    /// `passed` | `none` | `skipped`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer_check: Option<String>,
    pub hints: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub posed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marks: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<ResultView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum FeedItem {
    #[serde(rename_all = "camelCase")]
    Problem { problem_id: String, at: String },
    #[serde(rename_all = "camelCase")]
    Learner {
        intent: &'static str,
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        confidence: Option<u8>,
        /// What the learner attached to a submission (images, files, folders).
        #[serde(skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<Attachment>,
        at: String,
    },
    #[serde(rename_all = "camelCase")]
    Coach {
        text: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        thinking: String,
        op_id: String,
        at: String,
    },
    #[serde(rename_all = "camelCase")]
    Concluded {
        problem_id: String,
        verdict: Verdict,
        at: String,
    },
}

/// Which buttons are live.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Can {
    pub submit: bool,
    pub hint: bool,
    pub dismiss: bool,
    pub next: bool,
    pub end: bool,
    pub submit_set: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PracticeView {
    pub session_id: String,
    /// A Practice session from before the coach: plain messages only.
    pub legacy: bool,
    pub format: &'static str,
    /// `writing` | `open` | `concluded` | `set-writing` | `set-open` |
    /// `set-review` | `ended`.
    pub status: &'static str,
    pub wrap_up: bool,
    pub set_size: Option<u32>,
    pub retests_left: usize,
    pub turn_running: bool,
    pub last_turn_error: Option<String>,
    pub picked: Vec<NodeRef>,
    pub problems: Vec<ProblemView>,
    pub feed: Vec<FeedItem>,
    pub can: Can,
    pub calibration: Option<Calibration>,
    /// An exam paper's clock, draft and score.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exam: Option<ExamView>,
}

/// Marks awarded out of marks on offer.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Score {
    pub awarded: u32,
    pub out_of: u32,
}

/// What the learner sees of an exam paper beyond the problems: the clock the
/// app enforces, the saved draft and, after the review, the score.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExamView {
    pub name: String,
    pub goal_session_id: String,
    pub budget_sec: u32,
    /// RFC 3339; `None` until the last problem is posed (the clock starts then).
    pub opened_at: Option<String>,
    pub deadline: Option<String>,
    /// The harness clock when this was read: the countdown's reference, so a
    /// phone whose clock is wrong still counts down correctly.
    pub now: String,
    pub remaining_sec: Option<i64>,
    /// The deadline has passed.
    pub expired: bool,
    /// The deadline plus the grace has passed: answers are refused.
    pub closed: bool,
    pub submitted: bool,
    /// Handed in by the app at the deadline from the saved draft.
    pub auto_submitted: bool,
    pub elapsed_sec: Option<u32>,
    pub draft: Vec<SetAnswer>,
    pub score: Option<Score>,
}

fn node_ref(map: &KnowledgeMap, id: &str) -> NodeRef {
    match map.node(id) {
        Some(n) => NodeRef {
            id: n.id.clone(),
            title: n.title.clone(),
            home: n.home.clone(),
        },
        None => NodeRef {
            id: id.to_owned(),
            title: id.to_owned(),
            home: String::new(),
        },
    }
}

fn format_name(f: Format) -> &'static str {
    match f {
        Format::Set => "set",
        Format::Exam => "exam",
        _ => "stream",
    }
}

fn problem_view(map: &KnowledgeMap, r: &ProblemRecord) -> ProblemView {
    let posed = r.posed_at.is_some();
    let problem = r.problem().filter(|_| posed);
    let result = match (&r.conclusion, r.problem()) {
        (Some((c, _)), Some(p)) => Some(ResultView {
            verdict: c.verdict,
            hint_rung: c.hint_rung,
            miss_class: c.miss_class,
            misconception: c.misconception.clone(),
            rating: c.rating.clone(),
            duration_sec: c.duration_sec,
            targets: p.targets.iter().map(|t| node_ref(map, t)).collect(),
            hosts: p.hosts.iter().map(|t| node_ref(map, t)).collect(),
            layer: p.layer,
            level: p.level,
            cue_distance: p.cue_distance,
            is_distractor: p.is_distractor,
            retest_of: r.plan.retest_of.clone(),
            reveal: Reveal {
                solution: p.solution.clone(),
                final_answer: p.final_answer.clone(),
                acceptable_variants: p.acceptable_variants.clone(),
                rubric: p.rubric.clone(),
            },
            marks_awarded: c.marks_awarded,
        }),
        _ => None,
    };
    ProblemView {
        problem_id: r.id.clone(),
        index: r.index,
        status: match r.status() {
            Status::Writing => "writing",
            Status::Open => "open",
            Status::Concluded => "concluded",
        },
        statement: problem.map(|p| p.statement.clone()),
        kind: problem.map(|p| p.kind),
        answer_check: r
            .commit
            .as_ref()
            .filter(|_| posed)
            .map(|c| c.checks.answer_check.clone()),
        hints: r.hints.len(),
        posed_at: r.posed_at.clone(),
        marks: problem.and_then(|p| p.marks),
        result,
    }
}

/// The session status word.
fn status(s: &PracticeState) -> &'static str {
    if s.ended {
        "ended"
    } else if s.is_set() {
        if !s.writing().is_empty() || s.problems.is_empty() {
            "set-writing"
        } else if !s.set_submitted {
            "set-open"
        } else {
            "set-review"
        }
    } else if s.open().is_some() {
        "open"
    } else if !s.writing().is_empty() || s.problems.is_empty() {
        "writing"
    } else {
        "concluded"
    }
}

/// The learner-visible state. `now_turn`: `(running, last error)` from this
/// process's turn table.
pub fn build(
    session_id: &str,
    ops: &[Op],
    map: &KnowledgeMap,
    turn: (bool, Option<String>),
    calibration: Option<Calibration>,
) -> PracticeView {
    build_at(session_id, ops, map, turn, calibration, Utc::now())
}

/// [`build`] with the clock an exam paper's countdown is read on.
pub fn build_at(
    session_id: &str,
    ops: &[Op],
    map: &KnowledgeMap,
    turn: (bool, Option<String>),
    calibration: Option<Calibration>,
    now: DateTime<Utc>,
) -> PracticeView {
    let s = PracticeState::replay(ops);
    let status = status(&s);
    let set = s.is_set();
    let running = turn.0;
    let live = !s.ended && !running;
    let exam = exam_view(&s, now);
    // An exam paper stops taking answers shortly after its deadline, and is
    // left by handing it in, not by ending.
    let closed = exam.as_ref().is_some_and(|e| e.closed);
    let can = Can {
        submit: live && matches!(status, "open" | "concluded" | "set-review"),
        hint: live && !set && status == "open",
        dismiss: live && !set && status == "open",
        next: live && !set && status == "concluded" && (!s.wrap_up || s.retests_left() > 0),
        end: live
            && status != "open"
            && !status.ends_with("writing")
            && !(exam.is_some() && status == "set-open"),
        submit_set: live && status == "set-open" && !closed,
    };
    PracticeView {
        session_id: session_id.to_owned(),
        legacy: !s.started,
        format: format_name(s.format()),
        status,
        wrap_up: s.wrap_up,
        set_size: s.size,
        retests_left: s.retests_left(),
        turn_running: running,
        last_turn_error: turn.1,
        picked: s.picked.iter().map(|id| node_ref(map, id)).collect(),
        problems: s.problems.iter().map(|r| problem_view(map, r)).collect(),
        feed: feed(ops, set),
        can,
        calibration,
        exam,
    }
}

/// The paper's clock and score; `None` for a stream or a set.
fn exam_view(s: &PracticeState, now: DateTime<Utc>) -> Option<ExamView> {
    let e = s.exam.as_ref()?;
    let deadline = e.deadline.as_deref().and_then(paper::parse_time);
    let remaining = deadline.map(|d| (d - now).num_seconds().max(0));
    let expired = deadline.is_some_and(|d| now >= d);
    let closed = deadline.is_some_and(|d| now > d + Duration::seconds(paper::GRACE_SEC));
    // The score exists once every problem is marked.
    let marked: Vec<&ProblemRecord> = s.problems.iter().collect();
    let score = (!marked.is_empty() && marked.iter().all(|r| r.status() == Status::Concluded))
        .then(|| {
            let mut score = Score::default();
            for r in &marked {
                score.out_of += r.problem().and_then(|p| p.marks).unwrap_or(0);
                score.awarded += r
                    .conclusion
                    .as_ref()
                    .and_then(|(c, _)| c.marks_awarded)
                    .unwrap_or(0);
            }
            score
        })
        .filter(|sc| sc.out_of > 0);
    Some(ExamView {
        name: e.name.clone(),
        goal_session_id: e.goal_session_id.clone(),
        budget_sec: e.budget_sec,
        opened_at: e.opened_at.clone(),
        deadline: e.deadline.clone(),
        now: now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        remaining_sec: remaining,
        expired,
        closed,
        submitted: s.set_submitted,
        auto_submitted: e.auto_submitted,
        elapsed_sec: e.elapsed_sec,
        draft: e.draft.clone(),
        score,
    })
}

/// The conversation as the learner sees it.
fn feed(ops: &[Op], set: bool) -> Vec<FeedItem> {
    let mut out = Vec::new();
    // Plans the coach hasn't posed yet: while any exists its text is private.
    let mut writing: HashSet<String> = HashSet::new();
    let mut pending_confidence: Option<u8> = None;
    for op in ops {
        match op {
            Op::Practice(p) => match &p.event {
                PracticeEvent::Next { problem_id, .. } => {
                    writing.insert(problem_id.clone());
                }
                PracticeEvent::Pose { problem_id } => {
                    writing.remove(problem_id);
                    out.push(FeedItem::Problem {
                        problem_id: problem_id.clone(),
                        at: p.at.clone(),
                    });
                }
                // The confidence rides on the attempt written just before the
                // submission message; the text comes from the message, which
                // also exists when no problem is open (a question after a
                // conclusion, a question in a set's review).
                PracticeEvent::Attempt { confidence, .. } if !set => {
                    pending_confidence = *confidence
                }
                PracticeEvent::SetSubmit { answers, auto, .. } => out.push(FeedItem::Learner {
                    intent: "set",
                    text: if *auto {
                        format!("Time was up: handed in {} saved answer(s).", answers.len())
                    } else {
                        format!("Handed in {} answer(s).", answers.len())
                    },
                    confidence: None,
                    attachments: Vec::new(),
                    at: p.at.clone(),
                }),
                PracticeEvent::Hint { .. } => out.push(FeedItem::Learner {
                    intent: "hint",
                    text: String::new(),
                    confidence: None,
                    attachments: Vec::new(),
                    at: p.at.clone(),
                }),
                PracticeEvent::Dismiss { .. } => out.push(FeedItem::Learner {
                    intent: "dismiss",
                    text: String::new(),
                    confidence: None,
                    attachments: Vec::new(),
                    at: p.at.clone(),
                }),
                PracticeEvent::Conclude {
                    problem_id,
                    conclusion,
                } => out.push(FeedItem::Concluded {
                    problem_id: problem_id.clone(),
                    verdict: conclusion.verdict,
                    at: p.at.clone(),
                }),
                _ => {}
            },
            Op::Message(m) if m.role == Role::User && m.tag.as_deref() == Some(tag::SUBMISSION) => {
                out.push(FeedItem::Learner {
                    intent: "submission",
                    text: submission_text(&m.content),
                    confidence: pending_confidence.take(),
                    attachments: m.attachments.clone(),
                    at: m.at.clone(),
                });
            }
            Op::Message(m)
                if m.role == Role::Assistant
                    && writing.is_empty()
                    && !m.content.trim().is_empty() =>
            {
                out.push(FeedItem::Coach {
                    text: m.content.clone(),
                    thinking: m.thinking.clone(),
                    op_id: m.op_id.clone(),
                    at: m.at.clone(),
                });
            }
            _ => {}
        }
    }
    out
}

/// The learner's words without the `[confidence: …]` line the app appends for
/// the coach (the confidence is its own field in the feed).
fn submission_text(content: &str) -> String {
    match content.rfind("\n\n[confidence: ") {
        Some(at) if content.ends_with(']') => content[..at].trim().to_owned(),
        _ => content.trim().to_owned(),
    }
}

/// A practice op as the UI may receive it in an `op` event, or `None` when
/// the learner must never be told it happened. `hide_text`: the coach is
/// still writing a problem, so its messages stay blank.
pub fn learner_op(op: &Op, hide_text: bool) -> Option<Value> {
    let mut v = serde_json::to_value(op).ok()?;
    match op {
        Op::Practice(p) => match &p.event {
            // Private always: the committed problem and what refused it.
            PracticeEvent::Commit { .. } | PracticeEvent::Reject { .. } => return None,
            // The plan names the target: the UI is only told one exists.
            PracticeEvent::Next {
                problem_id, index, ..
            } => {
                return Some(json!({
                    "op": "practice", "opId": p.op_id, "at": p.at, "event": "next",
                    "problemId": problem_id, "index": index
                }));
            }
            PracticeEvent::Conclude { .. } => {
                v["conclusion"]["note"] = Value::String(String::new());
            }
            _ => {}
        },
        Op::Tool(_) => {
            v["args"] = Value::Null;
            if let Some(r) = v.get_mut("result").filter(|r| r.is_object()) {
                r["content"] = Value::String(String::new());
            }
        }
        Op::Message(m) if m.role == Role::Assistant && hide_text => {
            v["content"] = Value::String(String::new());
            if let Some(o) = v.as_object_mut() {
                o.remove("thinking");
                o.remove("native");
            }
        }
        // What the app told the coach (the plan names the target).
        Op::Message(m) if is_app_message(m) => {
            v["content"] = Value::String(String::new());
        }
        _ => {}
    }
    Some(v)
}

/// Remove from a loaded Practice session everything the learner must not
/// read yet (see the module docs). The ops stay in order; `messages` is
/// rebuilt from the cleaned ops.
pub fn learner_session(session: &mut Session) {
    let state = PracticeState::replay(&session.ops);
    if !state.started {
        return; // a session from before the coach: plain messages
    }
    let concluded: HashSet<&str> = state
        .problems
        .iter()
        .filter(|p| p.status() == Status::Concluded)
        .map(|p| p.id.as_str())
        .collect();
    let mut writing: HashSet<String> = HashSet::new();
    let mut ops: Vec<Op> = Vec::with_capacity(session.ops.len());
    for op in std::mem::take(&mut session.ops) {
        match op {
            Op::Practice(mut p) => {
                let done = p.problem_id().is_some_and(|id| concluded.contains(id));
                match &mut p.event {
                    PracticeEvent::Next { problem_id, .. } => {
                        writing.insert(problem_id.clone());
                        if !done {
                            continue;
                        }
                    }
                    PracticeEvent::Pose { problem_id } => {
                        writing.remove(problem_id);
                    }
                    PracticeEvent::Commit { .. } | PracticeEvent::Reject { .. } if !done => {
                        continue
                    }
                    PracticeEvent::Reject { .. } => continue,
                    PracticeEvent::Conclude { conclusion, .. } => conclusion.note.clear(),
                    _ => {}
                }
                ops.push(Op::Practice(p));
            }
            Op::Tool(mut t) => {
                t.args = Value::Null;
                if let Some(r) = t.result.as_mut() {
                    r.content.clear();
                }
                ops.push(Op::Tool(t));
            }
            Op::Message(mut m) if is_app_message(&m) => {
                m.content.clear();
                ops.push(Op::Message(m));
            }
            Op::Message(mut m) if m.role == Role::Assistant && !writing.is_empty() => {
                m.content.clear();
                m.thinking.clear();
                m.native.clear();
                ops.push(Op::Message(m));
            }
            other => ops.push(other),
        }
    }
    session.messages = ops
        .iter()
        .filter_map(|o| match o {
            Op::Message(m) => Some(m.clone()),
            _ => None,
        })
        .collect::<Vec<StoredMessage>>();
    session.ops = ops;
}

/// The user messages of a practice session the app wrote for the coach alone.
pub fn is_app_message(m: &StoredMessage) -> bool {
    matches!(m.tag.as_deref(), Some(tag::NEXT | tag::END))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learner::Format;
    use crate::practice::{Checks, Conclusion, PracticeOp, Problem, ProblemPlan, ProblemType};
    use crate::sessions::{new_message, ToolOp, ToolResultValue};

    fn plan() -> ProblemPlan {
        ProblemPlan {
            target: "secret-target".into(),
            layer: Layer::Recall,
            level: Level::Use,
            cue_distance: 2,
            hosts: vec![],
            is_distractor: false,
            why: "due".into(),
            retest_of: None,
            reached: None,
            aim: None,
            plugins: vec![],
        }
    }

    fn problem() -> Problem {
        Problem {
            statement: "What is across R2?".into(),
            targets: vec!["secret-target".into()],
            hosts: vec![],
            layer: Layer::Recall,
            level: Level::Use,
            cue_distance: 2,
            is_distractor: false,
            kind: ProblemType::Standard,
            solution: vec![Move {
                text: "Split the supply".into(),
                uses_node: None,
                decision_point: true,
            }],
            final_answer: "SECRET-ANSWER".into(),
            acceptable_variants: vec![],
            rubric: vec![],
            check: None,
            surface: "lamps".into(),
            marks: None,
        }
    }

    fn practice(event: PracticeEvent) -> Op {
        Op::Practice(PracticeOp::new(event))
    }

    fn assistant(text: &str) -> Op {
        Op::Message(new_message(Role::Assistant, text.into()))
    }

    fn tool_call(name: &str) -> Op {
        Op::Tool(ToolOp {
            op_id: crate::sessions::op_id(),
            at: crate::now_iso(),
            call_id: "c1".into(),
            name: name.into(),
            args: json!({ "finalAnswer": "SECRET-ANSWER" }),
            result: Some(ToolResultValue {
                content: "rejected: solved without secret-target".into(),
                is_error: true,
            }),
            agent: None,
            plugin: None,
            platforms: None,
        })
    }

    fn session(ops: Vec<Op>) -> Session {
        let at = crate::now_iso();
        Session {
            summary: crate::sessions::SessionSummary {
                id: "s".into(),
                mode: crate::Mode::Practice,
                title: "t".into(),
                created_at: at.clone(),
                updated_at: at,
                read_only: false,
                read_only_reason: None,
                desktop_only_plugins: vec![],
                lineage: None,
            },
            messages: vec![],
            ops,
            skipped_ops: 0,
        }
    }

    fn start() -> Op {
        practice(PracticeEvent::Start {
            format: Format::Stream,
            picked: vec![],
            size: None,
            exam: None,
        })
    }

    fn conclusion() -> Conclusion {
        Conclusion {
            verdict: Verdict::Correct,
            hint_rung: None,
            miss_class: None,
            misconception: None,
            note: "PRIVATE-NOTE".into(),
            rating: "good".into(),
            duration_sec: 5,
            confidence: Some(70),
            by: "coach".into(),
            marks_awarded: None,
        }
    }

    fn writing_ops() -> Vec<Op> {
        vec![
            start(),
            practice(PracticeEvent::Next {
                problem_id: "p1".into(),
                index: 0,
                plan: plan(),
            }),
            assistant("thinking about SECRET-ANSWER"),
            tool_call("problem_commit"),
            practice(PracticeEvent::Commit {
                problem_id: "p1".into(),
                problem: problem(),
                checks: Checks::default(),
            }),
            practice(PracticeEvent::Reject {
                problem_id: "p1".into(),
                by: "checker".into(),
                reason: "solved without secret-target".into(),
            }),
            practice(PracticeEvent::Pose {
                problem_id: "p1".into(),
            }),
            assistant("Take your time."),
        ]
    }

    #[test]
    fn an_open_problem_shows_its_statement_and_nothing_the_coach_knows() {
        let mut s = session(writing_ops());
        learner_session(&mut s);
        let shown = serde_json::to_string(&s).unwrap();
        for secret in ["secret-target", "SECRET-ANSWER", "solved without"] {
            assert!(!shown.contains(secret), "{secret} leaked: {shown}");
        }
        // What the learner may read stays: the pose and the coach's words
        // after it; the writing-phase message is blank but in place.
        assert!(shown.contains("Take your time."));
        let blank = s.messages.iter().filter(|m| m.content.is_empty()).count();
        assert_eq!(blank, 1, "{:?}", s.messages);
        assert!(s.ops.iter().any(|o| matches!(
            o,
            Op::Practice(p) if matches!(p.event, PracticeEvent::Pose { .. })
        )));

        let view = build(
            "s",
            &writing_ops(),
            &KnowledgeMap::default(),
            (false, None),
            None,
        );
        assert_eq!(view.status, "open");
        let shown = serde_json::to_string(&view).unwrap();
        assert!(shown.contains("What is across R2?") && !shown.contains("SECRET-ANSWER"));
        assert!(!shown.contains("secret-target") && !shown.contains("lamps"));
    }

    #[test]
    fn a_concluded_problem_gives_up_its_plan_and_solution_but_never_the_private_note() {
        let mut ops = writing_ops();
        ops.push(practice(PracticeEvent::Attempt {
            problem_id: "p1".into(),
            text: "6 V".into(),
            confidence: Some(70),
        }));
        ops.push(practice(PracticeEvent::Conclude {
            problem_id: "p1".into(),
            conclusion: conclusion(),
        }));
        let mut s = session(ops.clone());
        learner_session(&mut s);
        let shown = serde_json::to_string(&s).unwrap();
        assert!(shown.contains("SECRET-ANSWER") && shown.contains("secret-target"));
        assert!(!shown.contains("PRIVATE-NOTE"));
        assert!(
            !shown.contains("solved without"),
            "a refusal stays private even after the problem ended"
        );

        let view = build("s", &ops, &KnowledgeMap::default(), (false, None), None);
        assert_eq!(view.status, "concluded");
        let result = view.problems[0].result.as_ref().unwrap();
        assert_eq!(result.reveal.final_answer, "SECRET-ANSWER");
        assert!(view.can.next && !view.can.hint);
    }

    #[test]
    fn the_ui_is_never_told_the_plan_or_a_commit_when_they_happen() {
        let next = practice(PracticeEvent::Next {
            problem_id: "p1".into(),
            index: 0,
            plan: plan(),
        });
        let stub = learner_op(&next, true).unwrap();
        assert_eq!(stub["event"], "next");
        assert!(!stub.to_string().contains("secret-target"));
        assert!(learner_op(
            &practice(PracticeEvent::Commit {
                problem_id: "p1".into(),
                problem: problem(),
                checks: Checks::default(),
            }),
            false
        )
        .is_none());
        let tool = learner_op(&tool_call("problem_commit"), false).unwrap();
        assert!(!tool.to_string().contains("SECRET-ANSWER") && tool["name"] == "problem_commit");
        let mut m = new_message(Role::User, "next: {\"target\":\"secret-target\"}".into());
        m.tag = Some(tag::NEXT.into());
        let told = learner_op(&Op::Message(m), false).unwrap();
        assert!(!told.to_string().contains("secret-target"));
    }

    fn submission(text: &str) -> Op {
        let mut m = new_message(Role::User, text.into());
        m.tag = Some(tag::SUBMISSION.into());
        Op::Message(m)
    }

    fn learner_texts(view: &PracticeView) -> Vec<(String, Option<u8>)> {
        view.feed
            .iter()
            .filter_map(|f| match f {
                FeedItem::Learner {
                    intent: "submission",
                    text,
                    confidence,
                    ..
                } => Some((text.clone(), *confidence)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn every_learner_submission_is_in_the_feed_even_when_no_problem_is_open() {
        let k = KnowledgeMap::default();
        // An answer to the open problem: its confidence rides on the attempt,
        // and the line the app adds for the coach is not part of the text.
        let mut ops = writing_ops();
        ops.push(practice(PracticeEvent::Attempt {
            problem_id: "p1".into(),
            text: "6 V\n\n[confidence: 70% — Fairly sure]".into(),
            confidence: Some(70),
        }));
        ops.push(submission("6 V\n\n[confidence: 70% — Fairly sure]"));
        let view = build("s", &ops, &k, (false, None), None);
        assert_eq!(learner_texts(&view), [("6 V".to_owned(), Some(70))]);

        // A question after the problem concluded writes no attempt.
        ops.push(practice(PracticeEvent::Conclude {
            problem_id: "p1".into(),
            conclusion: conclusion(),
        }));
        ops.push(submission("Why not 5 V?"));
        let view = build("s", &ops, &k, (false, None), None);
        assert_eq!(
            learner_texts(&view),
            [
                ("6 V".to_owned(), Some(70)),
                ("Why not 5 V?".to_owned(), None)
            ]
        );

        // So does a question in a set's review.
        let set_ops = vec![
            practice(PracticeEvent::Start {
                format: Format::Set,
                picked: vec![],
                size: Some(2),
                exam: None,
            }),
            submission("Can you explain problem 2?"),
        ];
        let view = build("s", &set_ops, &k, (false, None), None);
        assert_eq!(
            learner_texts(&view),
            [("Can you explain problem 2?".to_owned(), None)]
        );
    }

    #[test]
    fn buttons_follow_the_state_and_a_running_turn_disables_them_all() {
        let ops = writing_ops();
        let idle = build("s", &ops, &KnowledgeMap::default(), (false, None), None);
        assert!(
            idle.can.submit && idle.can.hint && idle.can.dismiss && !idle.can.next && !idle.can.end
        );
        let busy = build("s", &ops, &KnowledgeMap::default(), (true, None), None);
        assert!(!busy.can.submit && !busy.can.hint && !busy.can.dismiss && !busy.can.end);
        // Before a pose there is nothing to press.
        let writing = build(
            "s",
            &ops[..3],
            &KnowledgeMap::default(),
            (false, None),
            None,
        );
        assert_eq!(writing.status, "writing");
        assert!(!writing.can.submit && !writing.can.end);
        // A session with no `start` op is a legacy one.
        let legacy = build(
            "s",
            &[assistant("hi")],
            &KnowledgeMap::default(),
            (false, None),
            None,
        );
        assert!(legacy.legacy);
    }

    #[test]
    fn an_exam_paper_reports_the_clock_and_stops_taking_answers_after_the_grace() {
        use crate::practice::{ExamStart, SetAnswer};
        // Whole seconds, as the log keeps them to the millisecond.
        let opened = DateTime::from_timestamp(Utc::now().timestamp(), 0).unwrap();
        let iso = |t: DateTime<Utc>| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let mut marked = problem();
        marked.marks = Some(5);
        let mut ops = vec![
            practice(PracticeEvent::Start {
                format: Format::Exam,
                picked: vec![],
                size: Some(1),
                exam: Some(ExamStart {
                    goal_session_id: "goal".into(),
                    name: "EE101".into(),
                    budget_sec: 2700,
                }),
            }),
            practice(PracticeEvent::Next {
                problem_id: "p1".into(),
                index: 0,
                plan: plan(),
            }),
            practice(PracticeEvent::Commit {
                problem_id: "p1".into(),
                problem: marked,
                checks: Checks::default(),
            }),
            practice(PracticeEvent::Pose {
                problem_id: "p1".into(),
            }),
        ];
        let at = |ops: &[Op], secs: i64| {
            build_at(
                "s",
                ops,
                &KnowledgeMap::default(),
                (false, None),
                None,
                opened + Duration::seconds(secs),
            )
        };
        // The last problem is posed but the clock has not started.
        let before = at(&ops, 0);
        assert_eq!(before.status, "set-open");
        let exam = before.exam.as_ref().unwrap();
        assert!(exam.deadline.is_none() && exam.remaining_sec.is_none() && !exam.expired);
        assert_eq!(before.problems[0].marks, Some(5));

        ops.push(practice(PracticeEvent::ExamOpen {
            opened_at: iso(opened),
            deadline: iso(opened + Duration::seconds(2700)),
        }));
        ops.push(practice(PracticeEvent::ExamDraft {
            answers: vec![SetAnswer {
                problem_id: "p1".into(),
                text: "6 V".into(),
                confidence: Some(70),
                sketch: None,
            }],
        }));
        let mid = at(&ops, 600);
        let exam = mid.exam.as_ref().unwrap();
        assert_eq!(exam.remaining_sec, Some(2100));
        assert!(!exam.expired && !exam.closed);
        assert_eq!(exam.draft[0].text, "6 V");
        assert!(mid.can.submit_set && !mid.can.end && !mid.can.hint && !mid.can.dismiss);

        // At the deadline the paper is expired but still takes what was in
        // flight; two seconds on it takes nothing.
        let due = at(&ops, 2700);
        assert!(due.exam.as_ref().unwrap().expired && !due.exam.as_ref().unwrap().closed);
        assert!(due.can.submit_set);
        let closed = at(&ops, 2703);
        assert!(closed.exam.as_ref().unwrap().closed);
        assert!(!closed.can.submit_set);
        assert_eq!(closed.exam.as_ref().unwrap().remaining_sec, Some(0));
        let json = serde_json::to_value(&closed).unwrap();
        assert_eq!(json["exam"]["budgetSec"], 2700);
        assert_eq!(json["exam"]["goalSessionId"], "goal");

        // Handed in and marked: the score is in, and nothing is left to hand in.
        ops.push(practice(PracticeEvent::SetSubmit {
            answers: vec![],
            auto: true,
            elapsed_sec: Some(2700),
        }));
        let mut c = conclusion();
        c.marks_awarded = Some(3);
        ops.push(practice(PracticeEvent::Conclude {
            problem_id: "p1".into(),
            conclusion: c,
        }));
        let done = at(&ops, 2710);
        assert_eq!(done.status, "set-review");
        let exam = done.exam.as_ref().unwrap();
        assert!(exam.submitted && exam.auto_submitted);
        assert_eq!(exam.elapsed_sec, Some(2700));
        let score = exam.score.as_ref().unwrap();
        assert_eq!((score.awarded, score.out_of), (3, 5));
        assert_eq!(
            done.problems[0].result.as_ref().unwrap().marks_awarded,
            Some(3)
        );
        assert!(done.can.end && !done.can.submit_set);
    }
}
