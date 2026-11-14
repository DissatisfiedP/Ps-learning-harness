//! Replay of a practice session's ops into [`PracticeState`]: what was
//! planned, committed, posed, asked and concluded, in order. Derived every
//! time, never stored.

use crate::learner::{Format, Verdict};
use crate::sessions::Op;

use super::{Checks, Conclusion, PracticeEvent, Problem, ProblemPlan, SetAnswer};

/// A learner's submission to one problem.
#[derive(Debug, Clone, PartialEq)]
pub struct Attempt {
    pub text: String,
    pub confidence: Option<u8>,
    pub at: String,
}

/// A check that refused a commit.
#[derive(Debug, Clone, PartialEq)]
pub struct Rejection {
    pub by: String,
    pub reason: String,
    pub at: String,
}

/// What the coach committed.
#[derive(Debug, Clone, PartialEq)]
pub struct Committed {
    pub problem: Problem,
    pub checks: Checks,
    pub at: String,
}

/// Where a problem stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Planned; the coach hasn't got a problem past the checks yet.
    Writing,
    /// Posed to the learner, not concluded.
    Open,
    Concluded,
}

/// One problem, from the scheduler's plan to its conclusion.
#[derive(Debug, Clone, PartialEq)]
pub struct ProblemRecord {
    pub id: String,
    pub index: u32,
    pub plan: ProblemPlan,
    pub planned_at: String,
    pub commit: Option<Committed>,
    pub rejections: Vec<Rejection>,
    pub posed_at: Option<String>,
    /// When each hint was asked for.
    pub hints: Vec<String>,
    pub attempts: Vec<Attempt>,
    pub dismissed_at: Option<String>,
    pub conclusion: Option<(Conclusion, String)>,
}

impl ProblemRecord {
    pub fn status(&self) -> Status {
        if self.conclusion.is_some() {
            Status::Concluded
        } else if self.posed_at.is_some() {
            Status::Open
        } else {
            Status::Writing
        }
    }

    pub fn problem(&self) -> Option<&Problem> {
        self.commit.as_ref().map(|c| &c.problem)
    }

    /// The learner's latest submission.
    pub fn last_attempt(&self) -> Option<&Attempt> {
        self.attempts.last()
    }

    /// The confidence on the latest submission that carried one.
    pub fn confidence(&self) -> Option<u8> {
        self.attempts.iter().rev().find_map(|a| a.confidence)
    }

    /// The problem ended without full credit: a miss, or a success that cost
    /// R3 or more (the idea was surrendered). The retest list holds these.
    pub fn needs_retest(&self) -> bool {
        match &self.conclusion {
            Some((c, _)) => c.verdict != Verdict::Correct || c.hint_rung.is_some_and(|r| r >= 3),
            None => false,
        }
    }

    pub fn is_miss(&self) -> bool {
        self.conclusion
            .as_ref()
            .is_some_and(|(c, _)| c.verdict != Verdict::Correct && c.verdict != Verdict::Partial)
    }
}

/// The replayed session.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PracticeState {
    /// A `start` op exists (sessions from before the coach have none).
    pub started: bool,
    pub format: Option<Format>,
    /// Graph nodes the learner picked on the Map.
    pub picked: Vec<String>,
    pub size: Option<u32>,
    pub wrap_up: bool,
    pub ended: bool,
    pub set_submitted: bool,
    pub problems: Vec<ProblemRecord>,
    /// An exam paper's clock and draft.
    pub exam: Option<ExamState>,
}

/// An exam paper's clock: when it opened, when it closes, what was saved.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExamState {
    pub goal_session_id: String,
    pub name: String,
    pub budget_sec: u32,
    /// RFC 3339, harness clock; `None` until the last problem is posed.
    pub opened_at: Option<String>,
    pub deadline: Option<String>,
    /// The latest saved answers.
    pub draft: Vec<SetAnswer>,
    /// Seconds from opening to the hand-in.
    pub elapsed_sec: Option<u32>,
    pub auto_submitted: bool,
}

impl PracticeState {
    pub fn replay(ops: &[Op]) -> Self {
        let mut s = PracticeState::default();
        for op in ops {
            let Op::Practice(p) = op else { continue };
            match &p.event {
                PracticeEvent::Start {
                    format,
                    picked,
                    size,
                    exam,
                } => {
                    s.started = true;
                    s.format = Some(*format);
                    s.picked.clone_from(picked);
                    s.size = *size;
                    s.exam = exam.as_ref().map(|e| ExamState {
                        goal_session_id: e.goal_session_id.clone(),
                        name: e.name.clone(),
                        budget_sec: e.budget_sec,
                        ..ExamState::default()
                    });
                }
                PracticeEvent::Next {
                    problem_id,
                    index,
                    plan,
                } => s.problems.push(ProblemRecord {
                    id: problem_id.clone(),
                    index: *index,
                    plan: plan.clone(),
                    planned_at: p.at.clone(),
                    commit: None,
                    rejections: Vec::new(),
                    posed_at: None,
                    hints: Vec::new(),
                    attempts: Vec::new(),
                    dismissed_at: None,
                    conclusion: None,
                }),
                PracticeEvent::Commit {
                    problem_id,
                    problem,
                    checks,
                } => {
                    if let Some(r) = s.problem_mut(problem_id) {
                        r.commit = Some(Committed {
                            problem: problem.clone(),
                            checks: checks.clone(),
                            at: p.at.clone(),
                        });
                    }
                }
                PracticeEvent::Reject {
                    problem_id,
                    by,
                    reason,
                } => {
                    if let Some(r) = s.problem_mut(problem_id) {
                        r.rejections.push(Rejection {
                            by: by.clone(),
                            reason: reason.clone(),
                            at: p.at.clone(),
                        });
                    }
                }
                PracticeEvent::Pose { problem_id } => {
                    if let Some(r) = s.problem_mut(problem_id) {
                        r.posed_at.get_or_insert_with(|| p.at.clone());
                    }
                }
                PracticeEvent::Hint { problem_id } => {
                    if let Some(r) = s.problem_mut(problem_id) {
                        r.hints.push(p.at.clone());
                    }
                }
                PracticeEvent::Attempt {
                    problem_id,
                    text,
                    confidence,
                } => {
                    if let Some(r) = s.problem_mut(problem_id) {
                        r.attempts.push(Attempt {
                            text: text.clone(),
                            confidence: *confidence,
                            at: p.at.clone(),
                        });
                    }
                }
                PracticeEvent::Dismiss { problem_id } => {
                    if let Some(r) = s.problem_mut(problem_id) {
                        r.dismissed_at.get_or_insert_with(|| p.at.clone());
                    }
                }
                PracticeEvent::Conclude {
                    problem_id,
                    conclusion,
                } => {
                    if let Some(r) = s.problem_mut(problem_id) {
                        if r.conclusion.is_none() {
                            r.conclusion = Some((conclusion.clone(), p.at.clone()));
                        }
                    }
                }
                PracticeEvent::SetSubmit {
                    auto, elapsed_sec, ..
                } => {
                    s.set_submitted = true;
                    if let Some(e) = &mut s.exam {
                        e.auto_submitted = *auto;
                        e.elapsed_sec = *elapsed_sec;
                    }
                }
                PracticeEvent::ExamOpen {
                    opened_at,
                    deadline,
                } => {
                    if let Some(e) = &mut s.exam {
                        e.opened_at.get_or_insert_with(|| opened_at.clone());
                        e.deadline.get_or_insert_with(|| deadline.clone());
                    }
                }
                PracticeEvent::ExamDraft { answers } => {
                    if let Some(e) = &mut s.exam {
                        e.draft.clone_from(answers);
                    }
                }
                PracticeEvent::Wrap {} => s.wrap_up = true,
                PracticeEvent::End { .. } => s.ended = true,
            }
        }
        s
    }

    /// The set answers of the latest `setSubmit` op, if any.
    pub fn set_answers(ops: &[Op]) -> Option<Vec<SetAnswer>> {
        ops.iter().rev().find_map(|o| match o {
            Op::Practice(p) => match &p.event {
                PracticeEvent::SetSubmit { answers, .. } => Some(answers.clone()),
                _ => None,
            },
            _ => None,
        })
    }

    pub fn format(&self) -> Format {
        self.format.unwrap_or(Format::Stream)
    }

    pub fn is_set(&self) -> bool {
        matches!(self.format, Some(Format::Set | Format::Exam))
    }

    pub fn problem(&self, id: &str) -> Option<&ProblemRecord> {
        self.problems.iter().find(|p| p.id == id)
    }

    fn problem_mut(&mut self, id: &str) -> Option<&mut ProblemRecord> {
        self.problems.iter_mut().find(|p| p.id == id)
    }

    /// The problem the learner is working on (stream: the one posed and not
    /// concluded).
    pub fn open(&self) -> Option<&ProblemRecord> {
        self.problems
            .iter()
            .rev()
            .find(|p| p.status() == Status::Open)
    }

    /// Planned problems the coach hasn't posed yet, in order.
    pub fn writing(&self) -> Vec<&ProblemRecord> {
        self.problems
            .iter()
            .filter(|p| p.status() == Status::Writing)
            .collect()
    }

    pub fn concluded(&self) -> Vec<&ProblemRecord> {
        self.problems
            .iter()
            .filter(|p| p.status() == Status::Concluded)
            .collect()
    }

    /// The latest problem, whatever its status.
    pub fn latest(&self) -> Option<&ProblemRecord> {
        self.problems.last()
    }

    /// Concluded problems that must be retested at the end (misses, R3+ and
    /// dismissed) and haven't had their retest planned yet. An original whose
    /// retest was planned (even if that retest was missed too) is done: a
    /// problem is retested once.
    pub fn retests_due(&self) -> Vec<&ProblemRecord> {
        self.problems
            .iter()
            .filter(|p| p.plan.retest_of.is_none() && p.needs_retest())
            .filter(|p| {
                !self
                    .problems
                    .iter()
                    .any(|r| r.plan.retest_of.as_deref() == Some(p.id.as_str()))
            })
            .collect()
    }

    /// Retests planned but not yet concluded count as still to do.
    pub fn retests_left(&self) -> usize {
        self.retests_due().len()
            + self
                .problems
                .iter()
                .filter(|p| p.plan.retest_of.is_some() && p.status() != Status::Concluded)
                .count()
    }

    /// Concluded problems on `target` (its primary target), oldest first.
    pub fn history(&self, target: &str) -> Vec<&ProblemRecord> {
        self.problems
            .iter()
            .filter(|p| p.plan.target == target && p.status() == Status::Concluded)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learner::{Layer, MissClass};
    use crate::practice::PracticeOp;
    use crate::state::Level;

    fn plan(target: &str) -> ProblemPlan {
        ProblemPlan {
            target: target.into(),
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

    fn op(event: PracticeEvent) -> Op {
        Op::Practice(PracticeOp::new(event))
    }

    fn conclusion(verdict: Verdict, rung: Option<u8>) -> Conclusion {
        Conclusion {
            verdict,
            hint_rung: rung,
            miss_class: (verdict == Verdict::Wrong).then_some(MissClass::Execution),
            misconception: None,
            note: String::new(),
            rating: "again".into(),
            duration_sec: 30,
            confidence: None,
            by: "coach".into(),
            marks_awarded: None,
        }
    }

    fn concluded(id: &str, retest_of: Option<&str>, verdict: Verdict, rung: Option<u8>) -> Vec<Op> {
        let mut p = plan("t");
        p.retest_of = retest_of.map(str::to_owned);
        vec![
            op(PracticeEvent::Next {
                problem_id: id.into(),
                index: 0,
                plan: p,
            }),
            op(PracticeEvent::Pose {
                problem_id: id.into(),
            }),
            op(PracticeEvent::Conclude {
                problem_id: id.into(),
                conclusion: conclusion(verdict, rung),
            }),
        ]
    }

    #[test]
    fn the_retest_list_holds_misses_r3_and_dismissals_once_each() {
        let mut ops = vec![op(PracticeEvent::Start {
            format: Format::Stream,
            picked: vec![],
            size: None,
            exam: None,
        })];
        ops.extend(concluded("a", None, Verdict::Wrong, None));
        ops.extend(concluded("b", None, Verdict::Correct, Some(3)));
        ops.extend(concluded("c", None, Verdict::Correct, Some(2)));
        ops.extend(concluded("d", None, Verdict::Dismissed, None));
        ops.extend(concluded("e", None, Verdict::Correct, None));
        let s = PracticeState::replay(&ops);
        let due: Vec<&str> = s.retests_due().iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            due,
            ["a", "b", "d"],
            "R1-R2 successes and clean ones are not retested"
        );

        // Planning a's retest takes it off the list, even if it is missed too.
        ops.extend(concluded("a2", Some("a"), Verdict::Wrong, None));
        let s = PracticeState::replay(&ops);
        let due: Vec<&str> = s.retests_due().iter().map(|p| p.id.as_str()).collect();
        assert_eq!(due, ["b", "d"]);
        assert_eq!(s.retests_left(), 2);
    }

    #[test]
    fn a_retest_that_was_planned_but_not_concluded_still_counts_as_left() {
        let mut ops = concluded("a", None, Verdict::Wrong, None);
        ops.push(op(PracticeEvent::Next {
            problem_id: "a2".into(),
            index: 1,
            plan: ProblemPlan {
                retest_of: Some("a".into()),
                ..plan("t")
            },
        }));
        let s = PracticeState::replay(&ops);
        assert!(s.retests_due().is_empty());
        assert_eq!(s.retests_left(), 1);
        assert_eq!(s.writing().len(), 1);
    }

    #[test]
    fn a_problem_is_open_from_its_pose_until_its_conclusion_and_a_second_conclusion_is_ignored() {
        let mut ops = vec![op(PracticeEvent::Next {
            problem_id: "a".into(),
            index: 0,
            plan: plan("t"),
        })];
        assert_eq!(
            PracticeState::replay(&ops).latest().unwrap().status(),
            Status::Writing
        );
        ops.push(op(PracticeEvent::Pose {
            problem_id: "a".into(),
        }));
        let s = PracticeState::replay(&ops);
        assert_eq!(s.open().unwrap().id, "a");
        ops.push(op(PracticeEvent::Conclude {
            problem_id: "a".into(),
            conclusion: conclusion(Verdict::Correct, None),
        }));
        ops.push(op(PracticeEvent::Conclude {
            problem_id: "a".into(),
            conclusion: conclusion(Verdict::Wrong, None),
        }));
        let s = PracticeState::replay(&ops);
        assert!(s.open().is_none());
        assert_eq!(
            s.problem("a")
                .unwrap()
                .conclusion
                .as_ref()
                .unwrap()
                .0
                .verdict,
            Verdict::Correct
        );
    }
}
