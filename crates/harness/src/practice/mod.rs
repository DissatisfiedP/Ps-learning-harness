//! Practice (architecture §4.11–4.13): the coach, the problem checker, the
//! scheduler and the learner-facing view.
//!
//! A practice session is a [`Mode::Practice`](crate::Mode) session driven by a
//! tool-loop agent, the **coach** (model role Tutor, `prompts/coach/base.md`).
//! The app asks for each problem (`Next` op, a user message the learner never
//! sees), the coach writes it privately and commits it with `problem_commit`,
//! and code runs the checks before the learner sees a word:
//!
//! 1. the answer script, when the coach wrote one ([`answer`]), because it
//!    costs no model call;
//! 2. the problem checker job ([`checker`], role Planner): turn 1 reads the
//!    statement blind, turn 2 is told the target.
//!
//! A failure goes back to the coach as the tool result and nothing is posed.
//! The learner then works with hints; `problem_conclude` ends the problem,
//! and code turns that verdict into a [`learner::Outcome`](crate::learner)
//! (FSRS rating, layers and misconceptions all follow from it).
//!
//! What is stored is a handful of ops in the session log: one
//! [`PracticeOp`] per fact (`start`, `next`, `commit`, `reject`, `pose`,
//! `hint`, `attempt`, `dismiss`, `conclude`, `setSubmit`, `wrap`, `end`),
//! beside the conversation's own `message` and `tool` ops. [`state`] replays
//! them; [`view`] turns the replay into what the learner may see, and strips
//! the solution from every op the UI is given until its problem has ended.
//!
//! Modules:
//! - [`state`]: replay, [`state::PracticeState`];
//! - [`scheduler`]: target, layer, level, cue distance, hosts, distractors
//!   (pure, over the knowledge map and the learner model);
//! - [`commit`]: `problem_commit` validation;
//! - [`answer`]: the answer check by code;
//! - [`checker`]: the two-turn problem checker;
//! - [`coach`]: prompt composition, the turn loop and the coach's tools;
//! - [`view`]: `PracticeView` and the learner-safe op filter;
//! - [`api`]: the learner's commands.

pub mod answer;
mod api;
pub mod checker;
mod coach;
pub mod commit;
mod paper;
pub mod scheduler;
pub mod state;
pub mod view;

use serde::{Deserialize, Serialize};

use crate::learner::{Format, Layer, MissClass, Verdict};
use crate::state::Level;

pub use state::PracticeState;
pub use view::PracticeView;

/// Fewest and most problems in a set.
pub const SET_SIZE: std::ops::RangeInclusive<u32> = 2..=10;
/// Set length when the learner doesn't choose.
pub const DEFAULT_SET_SIZE: u32 = 5;
/// The furthest cue distance from the lesson (§4.11).
pub const MAX_CUE: u8 = 3;
/// `ProblemPlan::why` of an exam paper's problems: past-paper style, marks
/// and a mark scheme (`commit`).
pub const WHY_EXAM: &str = "exam";
/// What one exam problem may be worth.
pub const MARKS: std::ops::RangeInclusive<u32> = 1..=40;

/// What the scheduler decided one problem must be: the coach's `next:`
/// request (`coach/base.md`), frozen in the log so a restart and every later
/// reader sees the same directive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProblemPlan {
    /// The graph node the problem must need (or, for a distractor, must look
    /// as if it needs).
    pub target: String,
    pub layer: Layer,
    /// The level the problem demands (`explain` / `use` / `analyze` /
    /// `design`).
    pub level: Level,
    pub cue_distance: u8,
    #[serde(default)]
    pub hosts: Vec<String>,
    #[serde(default)]
    pub is_distractor: bool,
    /// Why the scheduler chose the target: `picked`, `misconception`, `due`,
    /// `weak`, `new` or `retest`.
    pub why: String,
    /// The earlier problem this one retests (a fresh surface).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retest_of: Option<String>,
    /// The highest level the learner reached on the target before this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reached: Option<Level>,
    /// The level the learner's plan targeted for the node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aim: Option<Level>,
    /// Plugins active when the target was taught (their `practice` tools and
    /// fragments go to the coach for this problem).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProblemType {
    Standard,
    Critique,
    Design,
}

/// One solution move; `decision_point` marks where a new idea has to come to
/// mind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Move {
    #[serde(rename = "move")]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uses_node: Option<String>,
    pub decision_point: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Criterion {
    pub id: String,
    pub criterion: String,
    pub essential: bool,
}

/// The coach's answer script (`check { language: "python", code }`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckScript {
    pub language: String,
    pub code: String,
}

/// A committed problem: everything the coach wrote, private until the
/// problem ends (the statement is shown at the pose).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Problem {
    pub statement: String,
    pub targets: Vec<String>,
    #[serde(default)]
    pub hosts: Vec<String>,
    pub layer: Layer,
    pub level: Level,
    /// After the checker's cap.
    pub cue_distance: u8,
    pub is_distractor: bool,
    #[serde(rename = "type")]
    pub kind: ProblemType,
    pub solution: Vec<Move>,
    #[serde(default)]
    pub final_answer: String,
    #[serde(default)]
    pub acceptable_variants: Vec<String>,
    #[serde(default)]
    pub rubric: Vec<Criterion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<CheckScript>,
    /// The world the problem is set in; a retest uses another.
    pub surface: String,
    /// Exam papers only: what the problem is worth (past-paper marks). The
    /// rubric is the mark scheme, one criterion per mark-bearing step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marks: Option<u32>,
}

/// What the checks found out, kept beside the committed problem.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Checks {
    /// The checker's turn-1 answer (`audit_submit`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit: Option<serde_json::Value>,
    /// The checker's turn-2 answer (`necessity_submit`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub necessity: Option<serde_json::Value>,
    /// The cue distance the coach committed, when the checker lowered it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cue_capped_from: Option<u8>,
    /// `passed` (the script reproduced `finalAnswer`), `none` (no script) or
    /// `skipped` (the script couldn't run on this device).
    pub answer_check: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer_check_note: Option<String>,
}

/// How a problem ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conclusion {
    pub verdict: Verdict,
    /// Highest hint rung given (0–5), `None` when no hint was given.
    #[serde(default)]
    pub hint_rung: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub miss_class: Option<MissClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub misconception: Option<String>,
    #[serde(default)]
    pub note: String,
    /// The FSRS rating the outcome gave each target: again | hard | good | easy.
    pub rating: String,
    pub duration_sec: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<u8>,
    /// `coach`, or `app` when code concluded a dismissal the coach didn't.
    pub by: String,
    /// Exam papers only: the marks the coach awarded, at most the problem's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marks_awarded: Option<u32>,
}

/// One answer of a submitted set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetAnswer {
    pub problem_id: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<u8>,
}

/// What an exam paper was started on: the exam goal's session (its plan
/// carries the coverage weights) and the time budget the app enforces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExamStart {
    pub goal_session_id: String,
    pub name: String,
    pub budget_sec: u32,
}

/// One fact about a practice session.
// `Commit` carries the whole committed problem; events are few per session.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum PracticeEvent {
    /// The session began: its format, the ideas the learner picked (empty
    /// when the scheduler chooses) and a set's length.
    #[serde(rename_all = "camelCase")]
    Start {
        format: Format,
        #[serde(default)]
        picked: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<u32>,
        /// An exam paper's goal session and time budget.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exam: Option<ExamStart>,
    },
    /// The scheduler chose what the next problem must be.
    #[serde(rename_all = "camelCase")]
    Next {
        problem_id: String,
        index: u32,
        plan: ProblemPlan,
    },
    /// The coach committed a problem that passed every check. Private until
    /// its `conclude`.
    #[serde(rename_all = "camelCase")]
    Commit {
        problem_id: String,
        problem: Problem,
        checks: Checks,
    },
    /// A check refused a commit; `by` is `checker`, `script` or `scope`.
    /// Private always (the reason can name the target).
    #[serde(rename_all = "camelCase")]
    Reject {
        problem_id: String,
        by: String,
        reason: String,
    },
    /// The learner may see the statement now.
    #[serde(rename_all = "camelCase")]
    Pose { problem_id: String },
    /// The learner asked for a hint.
    #[serde(rename_all = "camelCase")]
    Hint { problem_id: String },
    /// The learner submitted something (working, a question or an answer).
    #[serde(rename_all = "camelCase")]
    Attempt {
        problem_id: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        confidence: Option<u8>,
    },
    /// The learner skipped the problem.
    #[serde(rename_all = "camelCase")]
    Dismiss { problem_id: String },
    /// The problem ended; its outcome is in `practice/outcomes.jsonl`.
    #[serde(rename_all = "camelCase")]
    Conclude {
        problem_id: String,
        conclusion: Conclusion,
    },
    /// The learner handed in a whole set.
    #[serde(rename_all = "camelCase")]
    SetSubmit {
        answers: Vec<SetAnswer>,
        /// An exam paper the app handed in at its deadline from the last
        /// saved draft.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        auto: bool,
        /// An exam paper: seconds from the paper opening to the hand-in, at
        /// most the budget.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        elapsed_sec: Option<u32>,
    },
    /// An exam paper opened: the last problem was posed and the clock
    /// started. Times are RFC 3339 on the harness clock.
    #[serde(rename_all = "camelCase")]
    ExamOpen { opened_at: String, deadline: String },
    /// The learner's unsent answers, saved while the paper is open so the
    /// deadline can hand them in. The latest draft wins.
    #[serde(rename_all = "camelCase")]
    ExamDraft { answers: Vec<SetAnswer> },
    /// The learner asked to end: from now the problems are retests.
    #[serde(rename_all = "camelCase")]
    Wrap {},
    /// The session is over.
    #[serde(rename_all = "camelCase")]
    End {
        #[serde(default)]
        skipped_retests: bool,
    },
}

/// The op that carries a [`PracticeEvent`] in the session log:
/// `{ "op": "practice", "opId", "at", "event": "...", … }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PracticeOp {
    pub op_id: String,
    pub at: String,
    #[serde(flatten)]
    pub event: PracticeEvent,
}

impl PracticeOp {
    pub fn new(event: PracticeEvent) -> Self {
        Self {
            op_id: crate::sessions::op_id(),
            at: crate::now_iso(),
            event,
        }
    }

    /// The problem an event is about, when it has one.
    pub fn problem_id(&self) -> Option<&str> {
        match &self.event {
            PracticeEvent::Next { problem_id, .. }
            | PracticeEvent::Commit { problem_id, .. }
            | PracticeEvent::Reject { problem_id, .. }
            | PracticeEvent::Pose { problem_id }
            | PracticeEvent::Hint { problem_id }
            | PracticeEvent::Attempt { problem_id, .. }
            | PracticeEvent::Dismiss { problem_id }
            | PracticeEvent::Conclude { problem_id, .. } => Some(problem_id),
            _ => None,
        }
    }
}

/// The tags of the user messages the app writes into a practice conversation.
pub mod tag {
    /// The app asks the coach for the next problem(s): never shown.
    pub const NEXT: &str = "practice-next";
    /// The learner's submission.
    pub const SUBMISSION: &str = "practice-submission";
    pub const HINT: &str = "practice-hint";
    pub const DISMISS: &str = "practice-dismiss";
    /// A whole set handed in.
    pub const SET: &str = "practice-set";
    /// The app asks for the closing turn: never shown.
    pub const END: &str = "practice-end";
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::Op;

    #[test]
    fn a_practice_op_is_one_flat_json_line_that_reads_back_the_same() {
        let op = Op::Practice(PracticeOp::new(PracticeEvent::Attempt {
            problem_id: "p1".into(),
            text: "6 V".into(),
            confidence: Some(70),
        }));
        let line = serde_json::to_value(&op).unwrap();
        assert_eq!(line["op"], "practice");
        assert_eq!(line["event"], "attempt");
        assert_eq!(
            (line["problemId"].as_str(), line["confidence"].as_u64()),
            (Some("p1"), Some(70))
        );
        let back: Op = serde_json::from_value(line).unwrap();
        assert_eq!(
            serde_json::to_value(&back).unwrap(),
            serde_json::to_value(&op).unwrap()
        );

        // An event kind from a newer build is skipped like any unknown op,
        // not a failure of the whole session.
        let newer = serde_json::json!({ "op": "practice", "opId": "x", "at": "2026-11-14T10:00:00Z", "event": "teleport" });
        assert!(serde_json::from_value::<Op>(newer).is_err());
    }

    #[test]
    fn a_conclusion_without_optional_fields_reads_from_an_older_line() {
        let line = serde_json::json!({
            "op": "practice", "opId": "x", "at": "2026-11-14T10:00:00Z", "event": "conclude",
            "problemId": "p1",
            "conclusion": { "verdict": "wrong", "rating": "again", "durationSec": 3, "by": "coach" }
        });
        let Op::Practice(p) = serde_json::from_value::<Op>(line).unwrap() else {
            panic!("not a practice op");
        };
        assert_eq!(p.problem_id(), Some("p1"));
    }
}
