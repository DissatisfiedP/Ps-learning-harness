//! The learner's practice commands (`docs/ui-design.md` §11b). Each takes
//! the session's turn lock without waiting (a second command while a coach
//! turn runs is refused, not queued), writes what the learner did as ops,
//! asks the scheduler for what comes next, and runs the coach.

use std::sync::Arc;

use chrono::Utc;
use providers::Role;
use serde_json::{json, Value};

use crate::events::AgentEvent;
use crate::knowledge::KnowledgeMap;
use crate::learner::{self, Format, LearnerModel};
use crate::replay::replay_state;
use crate::sessions::{self, Attachment, Mode, Op, SessionSummary};
use crate::{lock, Error, Harness};

use super::coach::{calibration_of, conclusion_from};
use super::scheduler::{self, Context, Taught};
use super::state::{PracticeState, ProblemRecord, Status};
use super::view::{self, PracticeView};
use super::{tag, PracticeEvent, ProblemPlan, SetAnswer, DEFAULT_SET_SIZE, SET_SIZE};

/// How many recent Learn sessions are read for what taught each idea.
const TAUGHT_SESSIONS: usize = 40;

/// The five confidence buttons (`ui-design` §7.2.4).
const CONFIDENCE: [(u8, &str); 5] = [
    (10, "Guessing"),
    (30, "Unsure"),
    (50, "Even odds"),
    (70, "Fairly sure"),
    (90, "Certain"),
];

pub(super) fn check_confidence(confidence: Option<u8>) -> Result<(), Error> {
    match confidence {
        Some(c) if !CONFIDENCE.iter().any(|(v, _)| *v == c) => Err(Error::Invalid(
            "confidence is one of 10, 30, 50, 70 or 90 (the five buttons)".into(),
        )),
        _ => Ok(()),
    }
}

fn confidence_label(c: u8) -> &'static str {
    CONFIDENCE
        .iter()
        .find(|(v, _)| *v == c)
        .map_or("", |(_, l)| *l)
}

/// What the learner wrote, as the coach reads it.
fn submission_text(text: &str, confidence: Option<u8>) -> String {
    match confidence {
        Some(c) => format!(
            "{}\n\n[confidence: {c}% — {}]",
            text.trim(),
            confidence_label(c)
        ),
        None => text.trim().to_owned(),
    }
}

fn app_message(tag: &str, text: String) -> Op {
    let mut m = sessions::new_message(Role::User, text);
    m.tag = Some(tag.into());
    Op::Message(m)
}

impl Harness {
    /// Learn sessions' teaching index: the plan's target level and active
    /// plugins per graph node, and the highest level a check showed.
    /// Newest sessions first, so the latest plan's aim wins.
    pub(crate) fn taught_index(&self, map: &KnowledgeMap) -> Taught {
        let mut taught = Taught::default();
        let Ok(list) = self.store.list() else {
            return taught;
        };
        let live = |id: &str| {
            let id = map.canonical(id);
            (map.node(&id).is_some() && !map.is_tombstoned(&id)).then_some(id)
        };
        let sessions = list
            .into_iter()
            .filter(|s| s.mode == Mode::Learn && !s.read_only)
            .take(TAUGHT_SESSIONS);
        for s in sessions {
            let Ok(ops) = self.store.ops(&s.id) else {
                continue;
            };
            let state = replay_state(&ops);
            let Some(plan) = &state.plan else { continue };
            let graph_of = |plan_node: &str| -> Option<String> {
                let node = plan.nodes.iter().find(|n| n.id == plan_node)?;
                live(node.graph_node.as_deref().unwrap_or(&node.id))
            };
            for section in &plan.sections {
                let plugins: Vec<String> = state
                    .grants
                    .iter()
                    .filter(|g| g.section_id == section.id)
                    .map(|g| g.plugin.clone())
                    .collect();
                for node_id in &section.node_ids {
                    let Some(id) = graph_of(node_id) else {
                        continue;
                    };
                    if let Some(level) = section.target_level {
                        taught.aim.entry(id.clone()).or_insert(level);
                    }
                    let list = taught.plugins.entry(id).or_default();
                    for p in &plugins {
                        if !list.contains(p) {
                            list.push(p.clone());
                        }
                    }
                }
            }
            for (node_id, level) in &state.node_levels {
                if let Some(id) = graph_of(node_id) {
                    let e = taught.reached.entry(id).or_insert(*level);
                    *e = (*e).max(*level);
                }
            }
        }
        // Only plugins this device can run, at most as many as a section has.
        let registry = self.plugin_registry();
        for list in taught.plugins.values_mut() {
            list.retain(|p| registry.grantable(p));
            list.truncate(crate::tools::MAX_PLUGINS_PER_SECTION);
        }
        taught.plugins.retain(|_, l| !l.is_empty());
        taught
    }

    /// The scheduler's plans for what the session needs next: one problem for
    /// a stream, the whole set for a set.
    fn schedule(&self, session_id: &str, s: &PracticeState) -> Result<Vec<ProblemPlan>, Error> {
        let map = crate::knowledge::load(&self.repo)?;
        let model: LearnerModel = learner::load_with(&self.repo, &map)?;
        let taught = self.taught_index(&map);
        let ctx = Context {
            map: &map,
            model: &model,
            taught: &taught,
            state: s,
            session_id,
            now: Utc::now(),
        };
        let plans = if let Some(exam) = &s.exam {
            // A paper: the coverage ranking of the exam's plan decides what
            // each problem is on.
            let plan = self.exam_plan(&exam.goal_session_id)?;
            let targets = crate::exam::paper_targets(
                &plan.nodes,
                s.size.unwrap_or(DEFAULT_SET_SIZE) as usize,
            );
            scheduler::plan_exam(&ctx, &targets)
        } else if s.is_set() {
            scheduler::plan_set(&ctx, s.size.unwrap_or(DEFAULT_SET_SIZE))
        } else {
            scheduler::plan_next(&ctx, &scheduler::trail(s)).map(|p| vec![p])
        };
        plans.map_err(Error::Invalid)
    }

    /// Plan, record and ask for the next problem(s).
    pub(super) fn queue_plans(&self, session_id: &str, s: &PracticeState) -> Result<(), Error> {
        let plans = self.schedule(session_id, s)?;
        let base = s.problems.len() as u32;
        let mut asked: Vec<Value> = Vec::new();
        for (i, plan) in plans.into_iter().enumerate() {
            let problem_id = ulid::Ulid::new().to_string();
            asked.push(super::coach::plan_json(&problem_id, &plan, &plan.hosts));
            self.practice_event(
                session_id,
                PracticeEvent::Next {
                    problem_id,
                    index: base + i as u32,
                    plan,
                },
            )?;
        }
        let text = if let Some(exam) = &s.exam {
            format!(
                "[exam paper] Write all {} problems of the exam paper now, privately, in the style of past papers, and commit each with problem_commit (pass its `problemId`, its `marks` and its mark scheme as the `rubric`). The paper is timed at {} minutes by the app. Give no hints and no verdicts until the learner hands it in.\n\nnext: {}",
                asked.len(),
                exam.budget_sec / 60,
                Value::Array(asked)
            )
        } else if s.is_set() {
            format!(
                "[set] Write all {} problems of the set now, privately, and commit each with problem_commit (pass its `problemId`). Give no hints and no verdicts until the learner hands in the whole set.\n\nnext: {}",
                asked.len(),
                Value::Array(asked)
            )
        } else if s.wrap_up {
            format!(
                "[next problem] The learner is wrapping up: this is a retest, on a fresh surface. Write it privately and commit it with problem_commit.\n\nnext: {}",
                asked[0]
            )
        } else {
            format!(
                "[next problem] Write the next problem now, privately, and commit it with problem_commit.\n\nnext: {}",
                asked[0]
            )
        };
        self.practice_append(session_id, app_message(tag::NEXT, text), true)
    }

    /// The Practice session's ops and replay, or why it can't take a command.
    pub(super) fn practice_session(
        &self,
        session_id: &str,
    ) -> Result<(Arc<Vec<Op>>, PracticeState), Error> {
        let ops = self.store.ops(session_id)?;
        let mode = ops.iter().find_map(|o| match o {
            Op::Session(h) => Some(h.mode),
            _ => None,
        });
        if mode != Some(Mode::Practice) {
            return Err(Error::Invalid("this is not a Practice session".into()));
        }
        let s = PracticeState::replay(&ops);
        if !s.started {
            return Err(Error::Invalid(
                "this Practice session is from before the coach and is read-only; start a new one"
                    .into(),
            ));
        }
        Ok((ops, s))
    }

    /// The learner-visible state of a Practice session.
    pub fn practice_state(&self, session_id: &str) -> Result<PracticeView, Error> {
        let ops = self.store.ops(session_id)?;
        let mode = ops.iter().find_map(|o| match o {
            Op::Session(h) => Some(h.mode),
            _ => None,
        });
        if mode != Some(Mode::Practice) {
            return Err(Error::Invalid("this is not a Practice session".into()));
        }
        let map = crate::knowledge::load(&self.repo).unwrap_or_default();
        let turn = lock(&self.turns)
            .get(session_id)
            .map_or((false, None), |t| (t.running, t.error.clone()));
        let calibration = learner::read_outcomes(&self.repo)
            .ok()
            .and_then(|(o, _)| calibration_of(&o));
        Ok(view::build_at(
            session_id,
            &ops,
            &map,
            turn,
            calibration,
            self.clock(),
        ))
    }

    /// Start a Practice session on the ideas the learner picked (none: the
    /// scheduler picks what is due or weak) and run the coach until the first
    /// problem is posed.
    pub async fn start_practice(
        self: &Arc<Self>,
        targets: &[String],
        format: &str,
        size: Option<u32>,
    ) -> Result<SessionSummary, Error> {
        let format = match format.trim() {
            "" | "stream" => Format::Stream,
            "set" => Format::Set,
            "exam" => {
                return Err(Error::Invalid(
                    "an exam paper starts from the exam goal's session (start_exam), with a time budget".into(),
                ))
            }
            other => {
                return Err(Error::Invalid(format!(
                    "unknown practice format `{other}` (stream or set)"
                )))
            }
        };
        let size = match (format, size) {
            (Format::Set, s) => {
                let n = s.unwrap_or(DEFAULT_SET_SIZE);
                if !SET_SIZE.contains(&n) {
                    return Err(Error::Invalid(format!(
                        "a set has {} to {} problems",
                        SET_SIZE.start(),
                        SET_SIZE.end()
                    )));
                }
                Some(n)
            }
            _ => None,
        };
        self.model_choice()?;
        let map = crate::knowledge::load(&self.repo)?;
        if map.nodes.iter().all(|n| map.is_tombstoned(&n.id)) {
            return Err(Error::Invalid(
                "the knowledge graph is empty: practice needs ideas to practise, so learn something first".into(),
            ));
        }
        let mut picked: Vec<String> = Vec::new();
        for id in targets {
            let canonical = map.canonical(id);
            if map.node(&canonical).is_none() || map.is_tombstoned(&canonical) {
                return Err(Error::Invalid(format!(
                    "`{id}` is not an idea in the knowledge graph"
                )));
            }
            if !picked.contains(&canonical) {
                picked.push(canonical);
            }
        }
        let title = if picked.is_empty() {
            "Practice: what is due".to_owned()
        } else {
            let names: Vec<String> = picked
                .iter()
                .take(3)
                .filter_map(|id| map.node(id).map(|n| n.title.clone()))
                .collect();
            format!(
                "Practice: {}{}",
                names.join(", "),
                if picked.len() > 3 { ", …" } else { "" }
            )
        };
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
                format,
                picked,
                size,
                exam: None,
            },
        )?;
        let (_, s) = self.practice_session(&id)?;
        if let Err(e) = self.queue_plans(&id, &s) {
            // Nothing was asked of the coach: leave no empty session behind.
            let _ = self.store.delete(&id);
            return Err(e);
        }
        self.coach_turn(&id).await?;
        self.store.summary(&id)
    }

    pub(super) fn try_turn(
        &self,
        session_id: &str,
    ) -> Result<tokio::sync::OwnedMutexGuard<()>, Error> {
        self.turn_lock(session_id)
            .try_lock_owned()
            .map_err(|_| Error::Invalid("a coach turn is already running in this session".into()))
    }

    /// One submission: working, a question or a claimed answer.
    pub async fn practice_submit(
        self: &Arc<Self>,
        session_id: &str,
        text: &str,
        confidence: Option<u8>,
        attachments: &[Attachment],
    ) -> Result<(), Error> {
        check_confidence(confidence)?;
        if text.trim().is_empty() && attachments.is_empty() {
            return Err(Error::Invalid("the submission is empty".into()));
        }
        if attachments.iter().any(|a| a.kind != "image") {
            return Err(Error::Invalid(
                "Practice can't read files or folders; attach images only".into(),
            ));
        }
        self.check_vision(attachments)?;
        let _turn = self.try_turn(session_id)?;
        let (_, s) = self.practice_session(session_id)?;
        if s.ended {
            return Err(Error::Invalid("this practice session has ended".into()));
        }
        let open = s.open().map(|r| r.id.clone());
        if s.is_set() && !s.set_submitted {
            return Err(Error::Invalid(
                "hand in the whole set first (practice_submit_set)".into(),
            ));
        }
        if open.is_none() && !s.writing().is_empty() {
            return Err(Error::Invalid("the problem isn't ready yet".into()));
        }
        let body = submission_text(text, confidence);
        if let Some(problem_id) = open.filter(|_| !s.is_set()) {
            self.practice_event(
                session_id,
                PracticeEvent::Attempt {
                    problem_id,
                    text: body.clone(),
                    confidence,
                },
            )?;
        }
        self.commit_attachments(attachments)?;
        let mut m = sessions::new_message(Role::User, body);
        m.tag = Some(tag::SUBMISSION.into());
        m.attachments = attachments.to_vec();
        self.practice_append(session_id, Op::Message(m), false)?;
        self.coach_turn(session_id).await
    }

    /// The Hint button (stream).
    pub async fn practice_hint(self: &Arc<Self>, session_id: &str) -> Result<(), Error> {
        let _turn = self.try_turn(session_id)?;
        let (_, s) = self.practice_session(session_id)?;
        if s.exam.is_some() {
            return Err(Error::Invalid("an exam paper has no hints".into()));
        }
        let Some(open) = s.open().filter(|_| !s.is_set()) else {
            return Err(Error::Invalid(
                "there is no open problem to give a hint on (a set has no hints)".into(),
            ));
        };
        let n = open.hints.len() + 1;
        self.practice_event(
            session_id,
            PracticeEvent::Hint {
                problem_id: open.id.clone(),
            },
        )?;
        self.practice_append(
            session_id,
            app_message(
                tag::HINT,
                format!(
                    "[hint requested] The learner pressed Hint (request {n} on this problem). Find the first point where they fail and give the next rung of the ladder, no further."
                ),
            ),
            false,
        )?;
        self.coach_turn(session_id).await
    }

    /// Skip the open problem (stream): ends as `dismissed` even when the coach
    /// doesn't conclude it.
    pub async fn practice_dismiss(self: &Arc<Self>, session_id: &str) -> Result<(), Error> {
        let _turn = self.try_turn(session_id)?;
        let (_, s) = self.practice_session(session_id)?;
        let Some(open) = s.open().filter(|_| !s.is_set()) else {
            return Err(Error::Invalid("there is no open problem to skip".into()));
        };
        let id = open.id.clone();
        self.practice_event(
            session_id,
            PracticeEvent::Dismiss {
                problem_id: id.clone(),
            },
        )?;
        self.practice_append(
            session_id,
            app_message(
                tag::DISMISS,
                "[dismissed] The learner skipped this problem. Conclude it as `dismissed`, then say in a sentence or two what it was about."
                    .into(),
            ),
            false,
        )?;
        let turn = self.coach_turn(session_id).await;
        // The skip is the learner's act: it counts whether or not the coach
        // concluded it.
        self.conclude_dismissed(session_id, &id)?;
        turn
    }

    fn conclude_dismissed(&self, session_id: &str, problem_id: &str) -> Result<(), Error> {
        let s = PracticeState::replay(&self.store.ops(session_id)?);
        let Some(r) = s.problem(problem_id) else {
            return Ok(());
        };
        if r.status() != Status::Open {
            return Ok(());
        }
        let c = conclusion_from(
            &json!({ "verdict": "dismissed", "hintRung": null, "note": "dismissed by the learner" }),
            r,
            "app",
        )
        .map_err(Error::Invalid)?;
        self.finish_problem(session_id, &s, r, c)?;
        Ok(())
    }

    /// The next problem (stream), after the current one concluded.
    pub async fn practice_next(self: &Arc<Self>, session_id: &str) -> Result<(), Error> {
        let _turn = self.try_turn(session_id)?;
        let (_, s) = self.practice_session(session_id)?;
        if s.is_set() {
            return Err(Error::Invalid(
                "a set ends with its review; start another set or end the session".into(),
            ));
        }
        if s.ended {
            return Err(Error::Invalid("this practice session has ended".into()));
        }
        if s.open().is_some() {
            return Err(Error::Invalid(
                "finish or skip the open problem first".into(),
            ));
        }
        if !s.writing().is_empty() {
            return Err(Error::Invalid(
                "the next problem is already being written (resume the turn if it stopped)".into(),
            ));
        }
        if s.wrap_up && s.retests_left() == 0 {
            return Err(Error::Invalid(
                "every miss has been retested: end the session".into(),
            ));
        }
        self.queue_plans(session_id, &s)?;
        self.coach_turn(session_id).await
    }

    /// A whole set (or exam paper) handed in: starts the review. A paper
    /// whose deadline has passed takes no more answers: the app hands in what
    /// the learner saved.
    pub async fn practice_submit_set(
        self: &Arc<Self>,
        session_id: &str,
        answers: Vec<SetAnswer>,
    ) -> Result<(), Error> {
        for a in &answers {
            check_confidence(a.confidence)?;
        }
        if let Ok((_, s)) = self.practice_session(session_id) {
            if self.paper_closed(&s) {
                self.exam_auto_submit(session_id).await?;
                return Err(Error::Invalid(
                    "time is up: the paper was handed in at the deadline with your last saved answers; answers sent after it are not accepted".into(),
                ));
            }
        }
        let _turn = self.try_turn(session_id)?;
        let (_, s) = self.practice_session(session_id)?;
        if !s.is_set() {
            return Err(Error::Invalid(
                "only a set is handed in whole; submit answers one at a time".into(),
            ));
        }
        if s.ended || s.set_submitted {
            return Err(Error::Invalid("this set was already handed in".into()));
        }
        if s.problems.is_empty() || !s.writing().is_empty() {
            return Err(Error::Invalid("the set isn't ready yet".into()));
        }
        let elapsed = self.paper_elapsed(&s);
        self.submit_set_locked(session_id, &s, answers, false, elapsed)
            .await
    }

    /// The hand-in of a set or paper, with the session's turn lock held.
    /// `auto`: the app handed in the saved draft at the deadline.
    pub(super) async fn submit_set_locked(
        self: &Arc<Self>,
        session_id: &str,
        s: &PracticeState,
        answers: Vec<SetAnswer>,
        auto: bool,
        elapsed_sec: Option<u32>,
    ) -> Result<(), Error> {
        let mut seen: Vec<&str> = Vec::new();
        for a in &answers {
            if s.problem(&a.problem_id).is_none() {
                return Err(Error::Invalid(format!(
                    "no problem `{}` in this set",
                    a.problem_id
                )));
            }
            if seen.contains(&a.problem_id.as_str()) {
                return Err(Error::Invalid(format!(
                    "problem `{}` is answered twice",
                    a.problem_id
                )));
            }
            seen.push(&a.problem_id);
        }
        let mut lines: Vec<String> = Vec::new();
        for r in &s.problems {
            let n = r.index + 1;
            match answers
                .iter()
                .find(|a| a.problem_id == r.id && !a.text.trim().is_empty())
            {
                Some(a) => {
                    self.practice_event(
                        session_id,
                        PracticeEvent::Attempt {
                            problem_id: r.id.clone(),
                            text: a.text.trim().to_owned(),
                            confidence: a.confidence,
                        },
                    )?;
                    lines.push(format!(
                        "Problem {n} (`{}`): {}",
                        r.id,
                        submission_text(&a.text, a.confidence)
                    ));
                }
                None => lines.push(format!("Problem {n} (`{}`): (no answer)", r.id)),
            }
        }
        self.practice_event(
            session_id,
            PracticeEvent::SetSubmit {
                answers: answers.clone(),
                auto,
                elapsed_sec,
            },
        )?;
        let (header, task) = if s.exam.is_some() {
            (
                if auto {
                    "[exam paper handed in by the app at the deadline, from the learner's last saved answers]"
                } else {
                    "[exam paper handed in]"
                },
                "Mark it now the way the exam would: for each problem call problem_conclude with its `problemId`, the verdict and `marksAwarded` under the mark scheme (an unanswered one is `dontknow`, 0 marks), then give your overall review in one message: the total marks, and for each problem the first break and miss class.",
            )
        } else {
            (
                "[set handed in]",
                "Review it now: for each problem call problem_conclude with its `problemId` (an unanswered one is `dontknow`), then give your overall review in one message: verdict, first break and miss class for each.",
            )
        };
        self.practice_append(
            session_id,
            app_message(tag::SET, format!("{header} {task}\n\n{}", lines.join("\n"))),
            false,
        )?;
        let turn = self.coach_turn(session_id).await;
        // A problem with no answer is a give-up whatever the coach wrote.
        let s = PracticeState::replay(&self.store.ops(session_id)?);
        for r in s
            .problems
            .iter()
            .filter(|r| r.status() == Status::Open && r.attempts.is_empty())
        {
            let c = conclusion_from(
                &json!({ "verdict": "dontknow", "hintRung": null, "missClass": "cue", "note": "no answer in the set" }),
                &ProblemRecord {
                    attempts: vec![super::state::Attempt {
                        text: String::new(),
                        confidence: None,
                        at: r.posed_at.clone().unwrap_or_default(),
                    }],
                    ..r.clone()
                },
                "app",
            )
            .map_err(Error::Invalid)?;
            self.finish_problem(session_id, &s, r, c)?;
        }
        turn
    }

    /// End practice. With misses, R3+ hints or dismissals not yet retested,
    /// the first call starts the retests (`wrapUp`); `skip_retests`, a second
    /// call, or a set (no retests) ends the session.
    pub async fn practice_end(
        self: &Arc<Self>,
        session_id: &str,
        skip_retests: bool,
    ) -> Result<(), Error> {
        let _turn = self.try_turn(session_id)?;
        let (_, s) = self.practice_session(session_id)?;
        if s.ended {
            return Err(Error::Invalid(
                "this practice session has already ended".into(),
            ));
        }
        if s.open().is_some() {
            return Err(Error::Invalid(
                "finish or skip the open problem before ending".into(),
            ));
        }
        if !s.is_set() && !s.wrap_up && !skip_retests && !s.retests_due().is_empty() {
            return self.practice_event(session_id, PracticeEvent::Wrap {});
        }
        self.practice_event(
            session_id,
            PracticeEvent::End {
                skipped_retests: skip_retests || s.retests_left() > 0,
            },
        )?;
        // One closing line, only when there is calibration to report.
        let calibrated = learner::read_outcomes(&self.repo)
            .ok()
            .and_then(|(o, _)| calibration_of(&o))
            .is_some();
        if calibrated {
            self.practice_append(
                session_id,
                app_message(
                    tag::END,
                    "[end] The learner ended the session. Close with the one observational calibration line from the session state, in the confidence words; nothing else.".into(),
                ),
                true,
            )?;
            return self.coach_turn(session_id).await;
        }
        Ok(())
    }

    /// Run the coach again: after a failed turn, or one an app restart cut
    /// short.
    pub async fn practice_resume(self: &Arc<Self>, session_id: &str) -> Result<(), Error> {
        // A paper whose deadline passed while nobody was looking (a restart
        // lost its timer) is handed in now.
        let (_, s) = self.practice_session(session_id)?;
        if s.exam.is_some() && !s.set_submitted && !s.ended && !s.problems.is_empty() {
            self.exam_auto_submit(session_id).await?;
            if PracticeState::replay(&self.store.ops(session_id)?).set_submitted {
                return Ok(());
            }
        }
        let _turn = self.try_turn(session_id)?;
        let (ops, s) = self.practice_session(session_id)?;
        if s.ended {
            return Err(Error::Invalid("this practice session has ended".into()));
        }
        if s.writing().is_empty() && !owes_turn(&ops) {
            return Err(Error::Invalid(
                "nothing to resume: the coach is waiting for you".into(),
            ));
        }
        drop(ops);
        self.coach_turn(session_id).await
    }
}

/// The last thing in the conversation is the learner's or a tool's, not the
/// coach's words: the coach owes a reply.
fn owes_turn(ops: &[Op]) -> bool {
    ops.iter()
        .rev()
        .find_map(|o| match o {
            Op::Message(m) => Some(m.role == Role::User),
            Op::Tool(t) if crate::replay::is_conversation_tool(t) => Some(true),
            _ => None,
        })
        .unwrap_or(false)
}
