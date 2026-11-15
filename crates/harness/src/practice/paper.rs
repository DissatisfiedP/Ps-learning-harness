//! Exam papers (architecture §4.14, `docs/ui-design.md` §11c): the timed
//! set. A paper is a set whose clock the app keeps.
//!
//! - **Start** ([`Harness::start_exam`]): the exam goal's plan is ranked
//!   ([`crate::exam`]) and the paper is written on its best practice nodes,
//!   one problem per target, each with marks and a mark scheme.
//! - **Open.** When the coach has posed the last problem the app writes an
//!   `examOpen` op: the clock starts, the deadline is `opened + budget`.
//! - **Save** ([`Harness::practice_exam_save`]): the UI saves the learner's
//!   answers as they type; the latest draft is what the deadline hands in.
//! - **Hand in** (`practice_submit_set`): accepted until the deadline plus
//!   [`GRACE_SEC`] (the time an answer takes to cross the app's bridge).
//! - **Deadline.** A timer armed when the paper opens hands in the saved
//!   draft ([`Harness::exam_auto_submit`]); the same hand-in runs lazily when
//!   a late command or `practice_resume` finds the deadline passed (the timer
//!   does not survive a restart). Answers sent after the grace are refused.
//! - **No hints**: a paper is a set, so `practice_hint`, `practice_dismiss`
//!   and `practice_next` are refused.
//!
//! The paper is timed on [`Harness::clock`] (the system clock, plus a skew
//! only tests set), never on the learner's device.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};

use crate::events::AgentEvent;
use crate::exam;
use crate::learner::Format;
use crate::sessions::{Mode, SessionSummary};
use crate::{Error, Harness};

use super::state::PracticeState;
use super::{ExamStart, PracticeEvent, SetAnswer, SET_SIZE};

/// Seconds after the deadline in which an answer that left the learner's
/// screen on time is still accepted.
pub const GRACE_SEC: i64 = 2;

pub fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

fn iso(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// A paper's average time per problem, in seconds: what an exam outcome's
/// `durationSec` holds (the problems are answered together). `None` outside a
/// paper, before the hand-in, or for an empty paper.
pub fn seconds_per_problem(s: &PracticeState) -> Option<u32> {
    let elapsed = s.exam.as_ref()?.elapsed_sec?;
    let n = u32::try_from(s.problems.len()).ok().filter(|n| *n > 0)?;
    Some(elapsed / n)
}

/// The deadline of an open paper, if it has one.
fn deadline_of(s: &PracticeState) -> Option<DateTime<Utc>> {
    s.exam.as_ref()?.deadline.as_deref().and_then(parse_time)
}

impl Harness {
    /// The clock exam papers are timed on.
    pub(crate) fn clock(&self) -> DateTime<Utc> {
        Utc::now()
            + Duration::seconds(
                self.clock_skew_secs
                    .load(std::sync::atomic::Ordering::Relaxed),
            )
    }

    /// Move the clock exam papers are timed on forward by `secs`: how tests
    /// reach a deadline without waiting for it. Not for the app.
    #[doc(hidden)]
    pub fn skew_clock(&self, secs: i64) {
        self.clock_skew_secs
            .fetch_add(secs, std::sync::atomic::Ordering::Relaxed);
    }

    /// Start the clock: the last problem of the paper was just posed.
    pub(crate) fn open_paper(
        &self,
        session_id: &str,
        budget_sec: u32,
    ) -> Result<DateTime<Utc>, Error> {
        let opened = self.clock();
        let deadline = opened + Duration::seconds(i64::from(budget_sec));
        self.practice_event(
            session_id,
            PracticeEvent::ExamOpen {
                opened_at: iso(opened),
                deadline: iso(deadline),
            },
        )?;
        Ok(deadline)
    }

    /// Hand in the saved draft once the deadline (and its grace) has passed.
    /// Holds only a weak reference: a closed app cancels the timer.
    pub(crate) fn arm_paper_timer(self: &Arc<Self>, session_id: &str, deadline: DateTime<Utc>) {
        let weak = Arc::downgrade(self);
        let id = session_id.to_owned();
        tokio::spawn(async move {
            loop {
                let Some(h) = weak.upgrade() else { return };
                let left = deadline + Duration::seconds(GRACE_SEC) - h.clock();
                drop(h);
                if left <= Duration::zero() {
                    break;
                }
                // Polled, so a moved clock is noticed within a second.
                let step = left
                    .to_std()
                    .unwrap_or_default()
                    .min(std::time::Duration::from_secs(1));
                tokio::time::sleep(step).await;
            }
            let Some(h) = weak.upgrade() else { return };
            if let Err(e) = h.exam_auto_submit(&id).await {
                tracing::warn!(session = %id, error = %e, "exam paper: the deadline hand-in failed");
            }
        });
    }

    /// Whether the paper's deadline and grace have passed with nothing
    /// handed in.
    pub(crate) fn paper_closed(&self, s: &PracticeState) -> bool {
        !s.set_submitted
            && !s.ended
            && deadline_of(s).is_some_and(|d| self.clock() > d + Duration::seconds(GRACE_SEC))
    }

    /// Hand in what the learner saved, when the deadline has passed: the
    /// app's auto-submit. Waits for any running turn; a paper already handed
    /// in (or not yet due) is left alone.
    pub(crate) async fn exam_auto_submit(self: &Arc<Self>, session_id: &str) -> Result<(), Error> {
        let _turn = self.turn_lock(session_id).lock_owned().await;
        let (_, s) = self.practice_session(session_id)?;
        let Some(exam) = &s.exam else { return Ok(()) };
        if s.ended
            || s.set_submitted
            || s.problems.is_empty()
            || !s.writing().is_empty()
            || !deadline_of(&s).is_some_and(|d| self.clock() >= d)
        {
            return Ok(());
        }
        let draft = exam.draft.clone();
        let budget = exam.budget_sec;
        self.submit_set_locked(session_id, &s, draft, true, Some(budget))
            .await
    }

    /// Start an exam paper on the exam goal's session: the plan is ranked,
    /// the paper written on its best practice nodes within `budget_min`
    /// minutes, and the coach runs until the whole paper is posed (when the
    /// clock starts).
    pub async fn start_exam(
        self: &Arc<Self>,
        goal_session_id: &str,
        budget_min: u32,
        size: Option<u32>,
    ) -> Result<SessionSummary, Error> {
        if !exam::BUDGET_MIN.contains(&budget_min) {
            return Err(Error::Invalid(format!(
                "a paper's time budget is {} to {} minutes",
                exam::BUDGET_MIN.start(),
                exam::BUDGET_MIN.end()
            )));
        }
        let size = size.unwrap_or_else(|| exam::paper_size(budget_min));
        if !SET_SIZE.contains(&size) {
            return Err(Error::Invalid(format!(
                "a paper has {} to {} problems",
                SET_SIZE.start(),
                SET_SIZE.end()
            )));
        }
        self.model_choice()?;
        let plan = self.exam_plan(goal_session_id)?;
        if exam::paper_targets(&plan.nodes, 1).is_empty() {
            return Err(Error::Invalid(
                "no idea of this exam's plan can go on a paper yet: teach the plan's ideas first, or weight some of them".into(),
            ));
        }
        let title = format!("Exam paper: {}", plan.exam.name);
        let summary = self.store.create(Mode::Practice, &title)?;
        self.emit(
            &summary.id,
            AgentEvent::Session {
                session: Box::new(summary.clone()),
            },
        );
        let guard = self.turn_lock(&summary.id);
        let _turn = guard.lock().await;
        let id = summary.id.clone();
        self.practice_event(
            &id,
            PracticeEvent::Start {
                format: Format::Exam,
                picked: Vec::new(),
                size: Some(size),
                exam: Some(ExamStart {
                    goal_session_id: goal_session_id.to_owned(),
                    name: plan.exam.name.clone(),
                    budget_sec: budget_min * 60,
                }),
            },
        )?;
        let (_, s) = self.practice_session(&id)?;
        if let Err(e) = self.queue_plans(&id, &s) {
            let _ = self.store.delete(&id);
            return Err(e);
        }
        self.coach_turn(&id).await?;
        self.store.summary(&id)
    }

    /// Save the answers typed so far; the deadline hands in the latest save.
    /// Only while the paper is open and on the clock.
    pub fn practice_exam_save(
        &self,
        session_id: &str,
        answers: Vec<SetAnswer>,
    ) -> Result<(), Error> {
        for a in &answers {
            super::api::check_confidence(a.confidence)?;
        }
        let (_, s) = self.practice_session(session_id)?;
        let Some(exam) = &s.exam else {
            return Err(Error::Invalid("only an exam paper keeps a draft".into()));
        };
        if s.set_submitted || s.ended {
            return Err(Error::Invalid("this paper was already handed in".into()));
        }
        if exam.deadline.is_none() {
            return Err(Error::Invalid("the paper isn't open yet".into()));
        }
        if self.paper_closed(&s) {
            return Err(Error::Invalid(
                "time is up: answers saved after the deadline are not accepted".into(),
            ));
        }
        for a in &answers {
            if s.problem(&a.problem_id).is_none() {
                return Err(Error::Invalid(format!(
                    "no problem `{}` in this paper",
                    a.problem_id
                )));
            }
        }
        // A drawing in the draft is moved out of staging now, so the draft
        // (and the hand-in at the deadline) survive a restart.
        let mut images = Vec::new();
        for sketch in answers.iter().filter_map(|a| a.sketch.as_ref()) {
            crate::attachments::check_sketch_image(sketch)?;
            images.push(sketch.clone());
        }
        self.commit_attachments(&images)?;
        if exam.draft == answers {
            return Ok(());
        }
        self.practice_event(session_id, PracticeEvent::ExamDraft { answers })
    }

    /// The seconds a hand-in at `now` took, at most the budget.
    pub(crate) fn paper_elapsed(&self, s: &PracticeState) -> Option<u32> {
        let exam = s.exam.as_ref()?;
        let opened = exam.opened_at.as_deref().and_then(parse_time)?;
        let secs = (self.clock() - opened).num_seconds().max(0);
        Some(u32::try_from(secs).unwrap_or(u32::MAX).min(exam.budget_sec))
    }
}

/// The paper's problem list as the coach reads it in its pinned state.
pub(crate) fn coach_block(s: &PracticeState, papers: &[exam::Paper]) -> String {
    let Some(e) = &s.exam else {
        return String::new();
    };
    let n = s.size.unwrap_or(s.problems.len() as u32).max(1);
    let minutes = e.budget_sec / 60;
    let mut out = format!(
        "\n## Exam paper\n- Exam: {}. Time budget: {minutes} minutes for {n} problems, timed by the app: the clock starts when the whole paper is posed, and what the learner saved is handed in at the deadline.\n- Marks: give each problem its `marks`, about {} on average (one mark is about a minute), and write its mark scheme as the `rubric`: one criterion per mark-bearing step, `essential` for the method marks.\n- No hints, no feedback and no verdicts until the paper is handed in.\n",
        e.name,
        (minutes / n).max(1),
    );
    out.push_str("- Past papers in the learner's libraries (write in their style, with their command words and mark allocation; `library_read` shows a paper):\n");
    for line in exam::paper_lines(papers).lines() {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("- Review: grade each problem the way the exam would. When a paper above lists a mark scheme, read it with `library_read` and mark by it; otherwise mark by your own rubric. `problem_conclude` carries `marksAwarded` (a correct answer earns all the marks); then give the overall review with the total.\n");
    out
}
