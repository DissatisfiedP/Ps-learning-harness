//! The coach (`prompts/coach/base.md`): composition, the turn loop and the
//! tools that gate a problem's life.
//!
//! The loop is the tutor's (`agent.rs`): compose the system prompt and tools
//! for the current state, stream one model call, persist every tool call
//! before running it, persist the result, repeat (compacting a long
//! conversation first). What differs:
//!
//! - **Private writing.** While a planned problem has not been posed, nothing
//!   the coach says is streamed, and its messages and tool ops are persisted
//!   but blanked in everything the UI is given ([`super::view`]).
//! - **A pose ends the turn.** After `problem_commit` succeeds the learner
//!   reads the statement from the app; the coach gets no extra call to repeat
//!   it. (A set's turn ends when its last problem is posed.)
//! - **The open problem is pinned.** The committed solution is part of the
//!   `{{session_state}}` block every turn, so compaction can drop the commit
//!   call without the coach losing its own answer key.
//! - **Gates in code.** A conclusion that skips the learner's attempt,
//!   forgets the hints they asked for, or leaves out a miss's class is
//!   refused with the reason; a commit passes the checks in [`super::commit`],
//!   [`super::answer`] and [`super::checker`] before anything is posed.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use providers::{Role, StopReason, ToolCall, ToolSpec};
use serde_json::Value;
use tokio::sync::oneshot;

use crate::agent::{clip, done, ensure_user_last, refuse, result_op, tool_op, Exec};
use crate::events::{AgentEvent, AgentName};
use crate::knowledge::KnowledgeMap;
use crate::learner::{self, Format, LearnerModel, Outcome, Verdict};
use crate::prompts::{Composer, TemplateValues};
use crate::replay;
use crate::sessions::{self, Op};
use crate::state::{Calibration, SessionState};
use crate::tools::{self, opt_str};
use crate::{lock, now_iso, Error, Harness, ModelRole};

use super::answer::{self, Check};
use super::checker::{self, Judgement};
use super::commit::{self, Expect};
use super::scheduler;
use super::state::{PracticeState, ProblemRecord, Status};
use super::{tag, Checks, Conclusion, PracticeEvent, PracticeOp, Problem};

/// Most model calls in one coach turn (a set commits several problems).
const MAX_COACH_LOOP: usize = 24;
/// How often a coach that answered without committing is told to.
const MAX_NUDGES: usize = 2;

// ---------------------------------------------------------------------------
// The pinned session state
// ---------------------------------------------------------------------------

fn node_line(map: &KnowledgeMap, id: &str) -> String {
    match map.node(id) {
        Some(n) => format!("`{}` — {} ({})", n.id, n.title, n.home),
        None => format!("`{id}`"),
    }
}

fn layers_text(model: &LearnerModel, id: &str) -> String {
    match model.node(id) {
        None => "no answers yet".into(),
        Some(n) => {
            let l = n.layers;
            format!(
                "recall {}, discrimination {}, composition {}",
                if l.recall { "shown" } else { "not shown" },
                if l.discrimination {
                    "shown"
                } else {
                    "not shown"
                },
                if l.composition { "shown" } else { "not shown" },
            )
        }
    }
}

fn level_text(l: Option<crate::state::Level>) -> &'static str {
    l.map_or("none", crate::state::Level::as_str)
}

pub(super) fn plan_json(id: &str, plan: &super::ProblemPlan, hosts: &[String]) -> Value {
    let mut v = serde_json::json!({
        "problemId": id,
        "target": plan.target,
        "layer": plan.layer,
        "level": plan.level,
        "cueDistance": plan.cue_distance,
        "hosts": hosts,
        "isDistractor": plan.is_distractor,
    });
    if let Some(r) = &plan.retest_of {
        v["retestOf"] = Value::String(r.clone());
    }
    v
}

fn open_problem_text(r: &ProblemRecord) -> Option<String> {
    let p = r.problem()?;
    let mut s = format!(
        "problemId `{}` ({}, {} hint request(s), {} submission(s) so far)\n",
        r.id,
        match p.kind {
            super::ProblemType::Standard => "standard",
            super::ProblemType::Critique => "critique",
            super::ProblemType::Design => "design",
        },
        r.hints.len(),
        r.attempts.len(),
    );
    s.push_str(&format!(
        "Statement shown to the learner:\n> {}\n",
        p.statement.replace('\n', "\n> ")
    ));
    s.push_str("Your committed solution:\n");
    for (i, m) in p.solution.iter().enumerate() {
        s.push_str(&format!(
            "{}. {}{}{}\n",
            i + 1,
            m.text,
            m.uses_node
                .as_ref()
                .map_or(String::new(), |n| format!(" [uses `{n}`]")),
            if m.decision_point {
                " ← decision point"
            } else {
                ""
            }
        ));
    }
    if !p.final_answer.is_empty() {
        s.push_str(&format!("Final answer: {}\n", p.final_answer));
    }
    if !p.acceptable_variants.is_empty() {
        s.push_str(&format!(
            "Also accepted: {}\n",
            p.acceptable_variants.join("; ")
        ));
    }
    for c in &p.rubric {
        s.push_str(&format!(
            "Rubric `{}` ({}): {}\n",
            c.id,
            if c.essential { "essential" } else { "quality" },
            c.criterion
        ));
    }
    Some(s)
}

/// The `{{session_state}}` block (see `coach/base.md`).
pub(crate) fn coach_state(
    s: &PracticeState,
    map: &KnowledgeMap,
    model: &LearnerModel,
    now: DateTime<Utc>,
    calibration: Option<&Calibration>,
) -> String {
    let mut out = String::new();
    let format = match s.format() {
        Format::Set => format!("set of {}", s.size.unwrap_or(0)),
        Format::Exam => "exam".to_owned(),
        _ => "stream".to_owned(),
    };
    out.push_str(&format!("- Format: {format}\n"));
    if s.wrap_up {
        out.push_str("- The learner is wrapping up: the problems now are retests, each on a fresh surface.\n");
    }
    if s.ended {
        out.push_str("- The session has ended.\n");
    }

    // Targets: the learner's picks and every idea a problem was planned on.
    let mut targets: Vec<String> = s.picked.iter().map(|id| map.canonical(id)).collect();
    for p in &s.problems {
        let t = map.canonical(&p.plan.target);
        if !targets.contains(&t) {
            targets.push(t);
        }
    }
    out.push_str(if s.picked.is_empty() {
        "\n## Targets (chosen by the scheduler)\n"
    } else {
        "\n## Targets (picked by the learner, plus what the scheduler added)\n"
    });
    if targets.is_empty() {
        out.push_str("(none yet)\n");
    }
    for id in &targets {
        let plan = s
            .problems
            .iter()
            .rev()
            .find(|p| &map.canonical(&p.plan.target) == id)
            .map(|p| &p.plan);
        out.push_str(&format!(
            "- {} — {}",
            node_line(map, id),
            layers_text(model, id)
        ));
        if let Some(n) = model.node(id) {
            out.push_str(&format!("; R {:.2}", n.retrievability(now)));
            if let Some(m) = n.open_misconceptions().last() {
                out.push_str(&format!("; open misconception: {}", clip(&m.text, 120)));
            }
        }
        if let Some(p) = plan {
            out.push_str(&format!(
                "; level reached {}, plan targets {}",
                level_text(p.reached),
                level_text(p.aim)
            ));
        }
        out.push('\n');
    }

    // What the app asks for now.
    let writing = s.writing();
    if !writing.is_empty() {
        out.push_str("\n## Write now (the app's `next:` requests)\n");
        for r in &writing {
            let hosts: Vec<String> = r.plan.hosts.clone();
            out.push_str(&format!(
                "- next: {} — why chosen: {}",
                plan_json(&r.id, &r.plan, &hosts),
                r.plan.why
            ));
            if let Some(orig) = r.plan.retest_of.as_deref().and_then(|o| s.problem(o)) {
                let surface = orig.problem().map_or("", |p| p.surface.as_str());
                out.push_str(&format!(
                    "; a retest of problem `{}`: use a fresh surface, not \"{surface}\"",
                    orig.id
                ));
            }
            out.push('\n');
            for rej in r.rejections.iter().rev().take(2) {
                out.push_str(&format!(
                    "  - earlier attempt refused by the {}: {}\n",
                    rej.by,
                    clip(&rej.reason, 300)
                ));
            }
        }
        let mut seen: HashSet<String> = HashSet::new();
        out.push_str("\n## Host candidates\n");
        for r in &writing {
            let t = map.canonical(&r.plan.target);
            if !seen.insert(t.clone()) {
                continue;
            }
            let hosts = scheduler::host_candidates(map, model, &t, now);
            if hosts.is_empty() {
                out.push_str(&format!("- for `{t}`: none in other topics\n"));
            } else {
                let list: Vec<String> = hosts.iter().map(|h| node_line(map, h)).collect();
                out.push_str(&format!("- for `{t}`: {}\n", list.join("; ")));
            }
        }
    }

    // The open problem(s), with the answer key.
    if s.is_set() && s.set_submitted {
        out.push_str("\n## The set (submitted: review it now)\n");
        for r in &s.problems {
            if r.status() == Status::Concluded {
                continue;
            }
            if let Some(text) = open_problem_text(r) {
                out.push_str(&text);
                match r.attempts.last() {
                    Some(a) => out.push_str(&format!(
                        "Learner's answer{}: {}\n\n",
                        a.confidence
                            .map_or(String::new(), |c| format!(" (confidence {c}%)")),
                        a.text
                    )),
                    None => out.push_str("Learner's answer: (none: conclude it as dontknow)\n\n"),
                }
            }
        }
    } else if s.is_set() {
        let posed: Vec<&ProblemRecord> =
            s.problems.iter().filter(|p| p.posed_at.is_some()).collect();
        if !posed.is_empty() {
            out.push_str("\n## Posed so far (the learner is waiting for the whole set; no hints, no verdicts)\n");
            for r in posed {
                out.push_str(&format!(
                    "- `{}`: {}\n",
                    r.id,
                    r.problem().map_or("", |p| p.surface.as_str())
                ));
            }
        }
    } else if let Some(r) = s.open() {
        out.push_str("\n## Open problem (private: the learner has not seen the solution)\n");
        if let Some(text) = open_problem_text(r) {
            out.push_str(&text);
        }
    }

    // Held nodes: the only ones a problem may use.
    let held = scheduler::held_nodes(map, model, now);
    out.push_str("\n## Held nodes (the problem may use only these, its targets and what its statement gives)\n");
    if held.is_empty() {
        out.push_str("(none yet: lean on the statement and the targets' prerequisites)\n");
    }
    for id in held.iter().take(40) {
        out.push_str(&format!("- {}\n", node_line(map, id)));
    }
    if held.len() > 40 {
        out.push_str(&format!(
            "… and {} more (graph_search finds them)\n",
            held.len() - 40
        ));
    }

    // Surfaces used, outcomes, the retest list.
    let surfaces: Vec<&str> = s
        .problems
        .iter()
        .filter_map(|p| p.problem().map(|q| q.surface.as_str()))
        .collect();
    out.push_str("\n## Surfaces used\n");
    out.push_str(&if surfaces.is_empty() {
        "(none yet)\n".to_owned()
    } else {
        format!("{}\n", surfaces.join("; "))
    });
    out.push_str("\n## Outcomes so far\n");
    let concluded = s.concluded();
    if concluded.is_empty() {
        out.push_str("(none yet)\n");
    }
    for r in concluded {
        let Some((c, _)) = &r.conclusion else {
            continue;
        };
        let p = r.problem();
        out.push_str(&format!(
            "- `{}` target `{}`{}: {}{}{}{}\n",
            r.id,
            r.plan.target,
            p.map_or(String::new(), |p| format!(
                " (cue {}, {}{})",
                p.cue_distance,
                p.level.as_str(),
                if p.is_distractor { ", distractor" } else { "" }
            )),
            serde_json::to_value(c.verdict)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default(),
            c.hint_rung
                .map_or(", no hints".to_owned(), |h| format!(", hints up to R{h}")),
            c.miss_class
                .and_then(|m| serde_json::to_value(m).ok())
                .and_then(|v| v.as_str().map(str::to_owned))
                .map_or(String::new(), |m| format!(", miss class {m}")),
            c.misconception
                .as_deref()
                .map_or(String::new(), |m| format!(" — misconception: {m}")),
        ));
    }
    let due = s.retests_due();
    if !due.is_empty() {
        out.push_str("\n## To retest before the session ends (misses, R3+ hints, dismissed)\n");
        for r in due {
            out.push_str(&format!(
                "- `{}` target `{}` (surface \"{}\")\n",
                r.id,
                r.plan.target,
                r.problem().map_or("", |p| p.surface.as_str())
            ));
        }
    }
    if let Some(c) = calibration {
        out.push_str("\n## Calibration\n");
        for b in &c.buckets {
            out.push_str(&format!(
                "- {}: {}/{} correct ({}%)\n",
                b.label,
                b.correct,
                b.total,
                b.percent()
            ));
        }
        for note in &c.notes {
            out.push_str(note);
            out.push('\n');
        }
    }
    out
}

/// The learner's calibration over every graded answer with a confidence.
pub(crate) fn calibration_of(outcomes: &[Outcome]) -> Option<Calibration> {
    let mut graded = HashMap::new();
    let mut confidences = HashMap::new();
    for o in outcomes {
        if let Some(c) = o.confidence {
            graded.insert(o.op_id.clone(), o.verdict == Verdict::Correct);
            confidences.insert(o.op_id.clone(), u32::from(c));
        }
    }
    replay::calibration(&graded, &confidences)
}

// ---------------------------------------------------------------------------
// Outcomes
// ---------------------------------------------------------------------------

fn parse_at(at: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(at)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Seconds from the pose to the learner's last submission (the dismissal, for
/// a skipped problem); 0 when unknown (a set is answered together).
fn duration_sec(r: &ProblemRecord, format: Format) -> u32 {
    if !matches!(format, Format::Stream) {
        return 0;
    }
    let end = r
        .dismissed_at
        .as_deref()
        .or_else(|| r.last_attempt().map(|a| a.at.as_str()));
    match (
        r.posed_at.as_deref().and_then(parse_at),
        end.and_then(parse_at),
    ) {
        (Some(a), Some(b)) => (b - a).num_seconds().clamp(0, i64::from(u32::MAX)) as u32,
        _ => 0,
    }
}

/// The outcome a concluded problem appends to `practice/outcomes.jsonl`.
pub(crate) fn outcome_for(
    session_id: &str,
    format: Format,
    r: &ProblemRecord,
    p: &Problem,
    c: &Conclusion,
    at: &str,
) -> Outcome {
    let no_answer = matches!(c.verdict, Verdict::Dontknow | Verdict::Dismissed);
    Outcome {
        op_id: r.id.clone(),
        at: at.to_owned(),
        session_id: session_id.to_owned(),
        problem_id: r.id.clone(),
        targets: p.targets.clone(),
        hosts: p.hosts.clone(),
        layer: p.layer,
        format,
        cue_distance: Some(p.cue_distance),
        verdict: c.verdict,
        confidence: if no_answer { None } else { r.confidence() },
        hint_rung: c.hint_rung,
        miss_class: c.miss_class,
        misconception: c.misconception.clone(),
        grader: if p.rubric.is_empty() {
            learner::Grader::Committed
        } else {
            learner::Grader::Rubric
        },
        duration_sec: duration_sec(r, format),
        level: Some(p.level),
    }
}

// ---------------------------------------------------------------------------
// The coach's turn
// ---------------------------------------------------------------------------

/// What a coach tool call did, beyond its result text.
struct Ran {
    exec: Exec,
    /// A problem was posed to the learner by this call.
    posed: bool,
}

impl Ran {
    fn plain(exec: Exec) -> Self {
        Self { exec, posed: false }
    }
}

impl Harness {
    /// Persist a practice op and tell the UI what it may know of it.
    pub(crate) fn practice_append(
        &self,
        session_id: &str,
        op: Op,
        hide_text: bool,
    ) -> Result<(), Error> {
        self.store.append_op(session_id, &op)?;
        if let Some(v) = super::view::learner_op(&op, hide_text) {
            self.emit(session_id, AgentEvent::Op { op: v });
        }
        Ok(())
    }

    pub(crate) fn practice_event(
        &self,
        session_id: &str,
        event: PracticeEvent,
    ) -> Result<(), Error> {
        self.practice_append(session_id, Op::Practice(PracticeOp::new(event)), false)
    }

    /// The pinned `{{session_state}}`, computed from the graph, the log and
    /// the ops.
    fn coach_state_text(&self, s: &PracticeState) -> String {
        let map = crate::knowledge::load(&self.repo).unwrap_or_else(|e| {
            tracing::warn!("practice: the graph couldn't be read: {e}");
            KnowledgeMap::default()
        });
        let (model, outcomes) = match learner::read_outcomes(&self.repo) {
            Ok((outcomes, _)) => (
                LearnerModel::from_outcomes(&outcomes, |id| map.canonical(id)),
                outcomes,
            ),
            Err(e) => {
                tracing::warn!("practice: the outcomes couldn't be read: {e}");
                (LearnerModel::default(), Vec::new())
            }
        };
        let mut text = coach_state(
            s,
            &map,
            &model,
            Utc::now(),
            calibration_of(&outcomes).as_ref(),
        );
        if s.exam.is_some() {
            text.push_str(&super::paper::coach_block(
                s,
                &crate::exam::papers(&self.repo),
            ));
        }
        text
    }

    /// The coach's system prompt and tools for the current state.
    pub(crate) fn compose_coach(
        &self,
        ops: &[Op],
        s: &PracticeState,
    ) -> Result<(String, Vec<ToolSpec>), Error> {
        let calibrated = {
            let state = self.coach_state_text(s);
            (state.contains("\n## Calibration\n"), state)
        };
        let mut flags: HashSet<String> = HashSet::new();
        flags.insert("mermaid".into());
        if calibrated.0 {
            flags.insert("calibration".into());
        }
        let mut values = TemplateValues::new();
        values.set("session_state", calibrated.1);
        let composer = Composer::new(&self.repo);
        let mut system = composer.compose_coach(&values, &flags)?;

        // The plugins active when the target was taught (frozen in the plan).
        let ids = plugins_in_play(s);
        let (fragments, plugin_tools) = {
            let registry = self.plugin_registry();
            (
                registry.fragments(&ids, "practice"),
                registry.tool_specs(&ids, "practice"),
            )
        };
        if !fragments.is_empty() {
            system.push_str("\n\n## Plugins you may use for this problem\n\n");
            system.push_str(&fragments);
        }
        system.push_str(&self.library_prompt(ops, "coach")?);
        let mut tools = tools::coach_tools();
        tools.extend(plugin_tools);
        Ok((system, tools))
    }

    /// One coach turn, with events. The caller holds the session's turn lock.
    pub(crate) async fn coach_turn(self: &Arc<Self>, session_id: &str) -> Result<(), Error> {
        self.emit(
            session_id,
            AgentEvent::TurnStart {
                agent: AgentName::Coach,
            },
        );
        *lock(&self.turns).entry(session_id.to_owned()).or_default() = crate::TurnStatus {
            running: true,
            error: None,
        };
        let started = std::time::Instant::now();
        let result = self.coach_loop(session_id).await;
        let error = result.as_ref().err().map(|e| self.redact(&e.to_string()));
        *lock(&self.turns).entry(session_id.to_owned()).or_default() = crate::TurnStatus {
            running: false,
            error: error.clone(),
        };
        tracing::debug!(
            session = session_id,
            ms = started.elapsed().as_millis() as u64,
            ok = result.is_ok(),
            "coach turn"
        );
        self.emit(
            session_id,
            AgentEvent::TurnEnd {
                agent: AgentName::Coach,
                error,
            },
        );
        result
    }

    async fn coach_loop(self: &Arc<Self>, session_id: &str) -> Result<(), Error> {
        self.deliver_queued(session_id)?;
        let mut compacted = false;
        let mut nudges = 0;
        for _ in 0..MAX_COACH_LOOP {
            let ops = self.store.ops(session_id)?;
            let s = PracticeState::replay(&ops);
            // Until the plan is posed, nothing the coach writes is the
            // learner's to read.
            let hide = !s.writing().is_empty();
            let (system, tools) = self.compose_coach(&ops, &s)?;
            let mut messages = replay::conversation_with(
                &ops,
                Some(&self.repo),
                self.role_vision(ModelRole::Tutor) != Some(false),
                true,
            );
            ensure_user_last(&mut messages);
            let window = self.context_window(ModelRole::Tutor);
            if !compacted && replay::estimate_tokens("", &messages) > window / 5 {
                compacted = true;
                if self.compact(session_id, &ops, window).await? {
                    continue;
                }
            }
            drop(ops);

            let (stop_tx, stop_rx) = oneshot::channel();
            lock(&self.running).insert(session_id.to_owned(), stop_tx);
            let reply = self
                .call_role(
                    ModelRole::Tutor,
                    session_id,
                    session_id,
                    &system,
                    &messages,
                    &tools,
                    (!hide).then_some(AgentName::Coach),
                    Some(stop_rx),
                )
                .await;
            lock(&self.running).remove(session_id);
            let reply = reply?;
            self.persist_coach_reply(session_id, &reply, hide)?;
            if reply.interrupted {
                return Ok(());
            }
            let calls = reply.completion.tool_calls.clone();
            if calls.is_empty() || reply.completion.stop == StopReason::Refusal {
                // A coach that talks while a problem is still owed is told to
                // commit it, twice; then the turn fails readably.
                if hide && reply.completion.stop != StopReason::Refusal {
                    nudges += 1;
                    if nudges > MAX_NUDGES {
                        return Err(Error::Invalid(
                            "the coach didn't commit a problem; try again".into(),
                        ));
                    }
                    let mut m = sessions::new_message(
                        Role::User,
                        "Commit the problem with problem_commit (revise it if the last attempt was refused).".into(),
                    );
                    m.tag = Some(tag::NEXT.into());
                    self.practice_append(session_id, Op::Message(m), false)?;
                    continue;
                }
                return Ok(());
            }
            let mut posed = false;
            for call in &calls {
                self.practice_append(session_id, tool_op(call, None), hide)?;
                let ran = self.coach_tool(session_id, call, &tools).await?;
                let Exec::Done { content, is_error } = &ran.exec else {
                    continue;
                };
                self.practice_append(session_id, result_op(call, content, *is_error, None), hide)?;
                posed |= ran.posed;
            }
            // A pose ends the turn; so does the last pose of a set.
            if posed {
                let s = PracticeState::replay(&self.store.ops(session_id)?);
                if s.writing().is_empty() {
                    return Ok(());
                }
            }
        }
        let s = PracticeState::replay(&self.store.ops(session_id)?);
        if s.writing().is_empty() {
            Ok(())
        } else {
            Err(Error::Invalid(
                "the coach used its whole turn without posing the problem; try again".into(),
            ))
        }
    }

    /// Persist the coach's reply as an assistant message, blank in the UI's
    /// copy while it is still writing.
    fn persist_coach_reply(
        &self,
        session_id: &str,
        reply: &crate::agent::Reply,
        hide: bool,
    ) -> Result<(), Error> {
        let has_calls = !reply.completion.tool_calls.is_empty();
        if reply.text.is_empty()
            && reply.thinking.is_empty()
            && reply.completion.native.is_empty()
            && !has_calls
            && !reply.interrupted
        {
            return Ok(());
        }
        let mut m = sessions::new_message(Role::Assistant, reply.text.clone());
        m.thinking.clone_from(&reply.thinking);
        m.native = crate::agent::capped_native(&reply.completion.native, session_id);
        m.model = Some(reply.model.clone());
        m.ending = if reply.interrupted {
            Some(crate::Ending::Interrupted)
        } else {
            match reply.completion.stop {
                StopReason::MaxTokens => Some(crate::Ending::MaxTokens),
                StopReason::Refusal => Some(crate::Ending::Refusal),
                StopReason::End | StopReason::ToolUse => None,
            }
        };
        self.practice_append(session_id, Op::Message(m), hide)
    }

    async fn coach_tool(
        self: &Arc<Self>,
        session_id: &str,
        call: &ToolCall,
        offered: &[ToolSpec],
    ) -> Result<Ran, Error> {
        let name = call.name.as_str();
        let args = &call.arguments;
        if !offered.iter().any(|t| t.name == name) {
            return Ok(Ran::plain(refuse(name, "unknown tool")));
        }
        if let Value::String(raw) = args {
            return Ok(Ran::plain(refuse(
                name,
                format!(
                    "the arguments were not valid JSON ({}); send one JSON object",
                    clip(raw, 200)
                ),
            )));
        }
        if !args.is_object() {
            return Ok(Ran::plain(refuse(
                name,
                "the arguments must be a JSON object",
            )));
        }
        let state = SessionState::default();
        Ok(match name {
            "problem_commit" => self.problem_commit(session_id, args).await?,
            "problem_conclude" => Ran::plain(self.problem_conclude(session_id, args)?),
            "research_request" => Ran::plain(self.research_request(session_id, args)?),
            "file_find" | "file_read" => {
                Ran::plain(self.file_tool(session_id, &state, name, args).await)
            }
            "library_list" | "library_search" | "library_read" => {
                Ran::plain(self.library_tool(name, args).await)
            }
            "graph_search" | "graph_read" => {
                Ran::plain(match crate::graph::read::run(&self.repo, name, args) {
                    Ok(text) => done(text),
                    Err(why) => refuse(name, why),
                })
            }
            _ => {
                let s = PracticeState::replay(&self.store.ops(session_id)?);
                let state = SessionState {
                    active_plugins: plugins_in_play(&s),
                    platform: self.plugin_registry().platform().to_owned(),
                    ..Default::default()
                };
                Ran::plain(self.plugin_call(session_id, &state, name, args).await)
            }
        })
    }

    // ---- problem_commit ----------------------------------------------------

    async fn problem_commit(
        self: &Arc<Self>,
        session_id: &str,
        args: &Value,
    ) -> Result<Ran, Error> {
        let name = "problem_commit";
        let ops = self.store.ops(session_id)?;
        let s = PracticeState::replay(&ops);
        drop(ops);
        let writing = s.writing();
        let record = match opt_str(args, "problemId") {
            Some(id) => match s.problem(id) {
                Some(r) if r.status() == Status::Writing => r,
                Some(_) => {
                    return Ok(Ran::plain(refuse(
                        name,
                        format!("problem `{id}` is already posed"),
                    )))
                }
                None => {
                    return Ok(Ran::plain(refuse(
                        name,
                        format!("no problem `{id}` is waiting to be written"),
                    )))
                }
            },
            None => match writing.first() {
                Some(r) => r,
                None => {
                    return Ok(Ran::plain(refuse(
                        name,
                        "no problem is waiting to be written; the app asks for the next one",
                    )))
                }
            },
        };
        let id = record.id.clone();
        let plan = record.plan.clone();
        let original_surface = plan
            .retest_of
            .as_deref()
            .and_then(|o| s.problem(o))
            .and_then(|o| o.problem())
            .map(|p| p.surface.clone());

        let map = crate::knowledge::load(&self.repo)?;
        let model = learner::load_with(&self.repo, &map)?;
        let held: HashSet<String> = scheduler::held_nodes(&map, &model, Utc::now())
            .into_iter()
            .collect();
        let reject = |by: &str, reason: String| -> Result<Ran, Error> {
            self.practice_event(
                session_id,
                PracticeEvent::Reject {
                    problem_id: id.clone(),
                    by: by.into(),
                    reason: reason.clone(),
                },
            )?;
            Ok(Ran::plain(refuse(name, reason)))
        };

        // 1. the commit itself
        let mut problem = match commit::validate(
            args,
            &Expect {
                plan: &plan,
                map: &map,
                held: &held,
                original_surface: original_surface.as_deref(),
            },
        ) {
            Ok(p) => p,
            Err(problems) => {
                return reject(
                    "scope",
                    format!(
                    "the problem has issues; fix all of them and call problem_commit again:\n- {}",
                    problems.join("\n- ")
                ),
                )
            }
        };

        // 2. the answer script (no model call, so it goes first)
        let mut checks = Checks {
            answer_check: "none".into(),
            ..Default::default()
        };
        if let Some(script) = problem.check.clone() {
            let final_answer = problem.final_answer.clone();
            let result =
                tokio::task::spawn_blocking(move || answer::check_answer(&script, &final_answer))
                    .await
                    .map_err(|e| Error::Invalid(format!("internal error ({e})")))?;
            match result {
                Check::Passed => checks.answer_check = "passed".into(),
                Check::Mismatch { script } => {
                    return reject(
                        "script",
                        format!(
                            "Discarded: your check script printed `{script}` but your committed finalAnswer is `{}`. One of them is wrong. Fix the problem, the answer or the script, and commit again; nothing was posed.",
                            problem.final_answer
                        ),
                    )
                }
                Check::ScriptFailed(why) => {
                    return reject(
                        "script",
                        format!("Your check script failed ({why}). Fix it or leave `check` out; nothing was posed."),
                    )
                }
                Check::Skipped(why) => {
                    checks.answer_check = "skipped".into();
                    checks.answer_check_note = Some(why);
                }
            }
        }

        // 3. the problem checker (blind, then told the target)
        let target = map.canonical(&plan.target);
        let node = map.node(&target);
        let title = node.map_or_else(|| target.clone(), |n| n.title.clone());
        let mut names = vec![title.clone()];
        if let Some(n) = node {
            names.extend(n.aliases.iter().cloned());
        }
        let equivalents = node
            .map(|n| {
                let mut parts = Vec::new();
                if !n.statement.trim().is_empty() {
                    parts.push(n.statement.trim().to_owned());
                }
                parts.extend(n.aliases.iter().cloned());
                parts.join("; ")
            })
            .filter(|e| !e.is_empty())
            .unwrap_or_else(|| "(none recorded)".into());
        let held_summary = {
            let titles: Vec<String> = held
                .iter()
                .filter_map(|h| map.node(h).map(|n| n.title.clone()))
                .collect();
            if titles.is_empty() {
                "nothing recorded yet".to_owned()
            } else {
                titles.join(", ")
            }
        };
        let report = self
            .run_checker(
                session_id,
                &checker::Request {
                    statement: problem.statement.clone(),
                    target_name: title.clone(),
                    target_equivalents: equivalents,
                    held_summary,
                    is_distractor: plan.is_distractor,
                    cue_distance: problem.cue_distance,
                },
            )
            .await;
        let report = match report {
            Ok(r) => r,
            Err(e) => {
                return Ok(Ran::plain(refuse(
                    name,
                    format!(
                        "the problem checker could not run ({}); nothing was posed. Call problem_commit again.",
                        self.redact(&e.to_string())
                    ),
                )))
            }
        };
        match checker::judge(
            &report.audit,
            &report.necessity,
            &names,
            &title,
            plan.is_distractor,
            problem.cue_distance,
        ) {
            Judgement::Reject(reason) => return reject("checker", reason),
            Judgement::Accept { cue_cap } => {
                if let Some(cap) = cue_cap {
                    checks.cue_capped_from = Some(problem.cue_distance);
                    problem.cue_distance = cap;
                }
            }
        }
        checks.audit = Some(report.audit);
        checks.necessity = Some(report.necessity);

        // 4. commit, then pose
        let mut notes: Vec<String> = Vec::new();
        if let Some(from) = checks.cue_capped_from {
            notes.push(format!(
                "The checker could name the target from the wording alone, so the cue distance was recorded as 1 instead of {from}."
            ));
        }
        match checks.answer_check.as_str() {
            "skipped" => notes.push(format!(
                "Your check script could not run here ({}), so the answer is unverified: grade with extra care.",
                checks.answer_check_note.as_deref().unwrap_or("python3 or SymPy is missing")
            )),
            "passed" => notes.push("Your check script reproduced the committed answer.".into()),
            _ => {}
        }
        self.practice_event(
            session_id,
            PracticeEvent::Commit {
                problem_id: id.clone(),
                problem,
                checks,
            },
        )?;
        self.practice_event(
            session_id,
            PracticeEvent::Pose {
                problem_id: id.clone(),
            },
        )?;
        let mut after = PracticeState::replay(&self.store.ops(session_id)?);
        if after.writing().is_empty() {
            if let Some(exam) = after.exam.as_ref().filter(|e| e.opened_at.is_none()) {
                // The paper is complete: the clock starts now.
                let deadline = self.open_paper(session_id, exam.budget_sec)?;
                after = PracticeState::replay(&self.store.ops(session_id)?);
                self.arm_paper_timer(session_id, deadline);
            }
        }
        let tail = if after.writing().is_empty() {
            if after.is_set() {
                "The whole set is posed. The learner answers it all before you say anything about it."
            } else {
                "The learner sees the statement and starts working. Your turn is over."
            }
        } else {
            "Posed. Commit the next problem of the set."
        };
        let mut text = format!("Problem `{id}` posed. {tail}");
        for n in notes {
            text.push(' ');
            text.push_str(&n);
        }
        Ok(Ran {
            exec: done(text),
            posed: true,
        })
    }

    // ---- problem_conclude --------------------------------------------------

    fn problem_conclude(&self, session_id: &str, args: &Value) -> Result<Exec, Error> {
        let name = "problem_conclude";
        let ops = self.store.ops(session_id)?;
        let s = PracticeState::replay(&ops);
        drop(ops);
        let record = if s.is_set() {
            if !s.set_submitted {
                return Ok(refuse(
                    name,
                    "the learner hasn't handed in the set: no verdicts and no feedback until they do",
                ));
            }
            let Some(id) = opt_str(args, "problemId") else {
                return Ok(refuse(name, "in a set review, give the `problemId`"));
            };
            match s.problem(id) {
                Some(r) if r.status() == Status::Open => r,
                Some(_) => return Ok(refuse(name, format!("problem `{id}` is already concluded"))),
                None => return Ok(refuse(name, format!("no problem `{id}`"))),
            }
        } else {
            match s.open() {
                Some(r) => r,
                None => {
                    return Ok(refuse(
                        name,
                        "no problem is open (it was already concluded, or none is posed)",
                    ))
                }
            }
        };
        let conclusion = match conclusion_from(args, record, "coach") {
            Ok(c) => c,
            Err(why) => return Ok(refuse(name, why)),
        };
        let reply = self.finish_problem(session_id, &s, record, conclusion)?;
        Ok(done(reply))
    }

    /// Append the outcome and the `conclude` op of a problem; the text the
    /// coach reads back.
    pub(crate) fn finish_problem(
        &self,
        session_id: &str,
        s: &PracticeState,
        record: &ProblemRecord,
        mut conclusion: Conclusion,
    ) -> Result<String, Error> {
        let Some(problem) = record.problem() else {
            return Err(Error::Invalid(format!(
                "problem {} has no committed problem",
                record.id
            )));
        };
        let at = now_iso();
        let mut outcome = outcome_for(session_id, s.format(), record, problem, &conclusion, &at);
        if let Some(sec) = super::paper::seconds_per_problem(s) {
            outcome.duration_sec = sec;
        }
        conclusion.rating = learner::rating(&outcome).as_str().to_owned();
        conclusion.duration_sec = outcome.duration_sec;
        conclusion.confidence = outcome.confidence;
        // The outcome first: its `opId` is the problem's, so a retry after a
        // crash between the two appends writes nothing twice.
        learner::append_outcome(&self.repo, &outcome)?;
        let verdict = conclusion.verdict;
        let rating = conclusion.rating.clone();
        self.practice_event(
            session_id,
            PracticeEvent::Conclude {
                problem_id: record.id.clone(),
                conclusion,
            },
        )?;
        let after = if s.is_set() {
            "The learner now sees the committed solution for this problem. Review each remaining problem the same way, then give your overall review in one message.".to_owned()
        } else {
            "The learner now sees your committed solution. Add one or two sentences on their work, then end your turn; the next problem comes when they press Next.".to_owned()
        };
        Ok(format!(
            "Concluded `{}`: {} (FSRS rating {rating}). {after}",
            record.id,
            serde_json::to_value(verdict)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default()
        ))
    }
}

/// Plugins the coach may use now: those of the problems still to do (or, with
/// none left, of the latest one, for questions about it).
fn plugins_in_play(s: &PracticeState) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    let active: Vec<&ProblemRecord> = s
        .problems
        .iter()
        .filter(|p| p.status() != Status::Concluded)
        .collect();
    let pick: Vec<&ProblemRecord> = if active.is_empty() {
        s.problems.last().into_iter().collect()
    } else {
        active
    };
    for p in pick {
        for id in &p.plan.plugins {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
    }
    ids.truncate(tools::MAX_PLUGINS_PER_SECTION);
    ids
}

/// Parse and gate a `problem_conclude` call against the problem's history.
/// `by`: who concludes (`coach` | `app`).
pub(crate) fn conclusion_from(
    args: &Value,
    r: &ProblemRecord,
    by: &str,
) -> Result<Conclusion, String> {
    let verdict = args
        .get("verdict")
        .and_then(Value::as_str)
        .and_then(Verdict::parse)
        .ok_or("`verdict` must be correct, partial, wrong, dontknow or dismissed")?;
    let hint_rung = match args.get("hintRung") {
        None | Some(Value::Null) => None,
        Some(v) => match v.as_u64() {
            Some(n) if n <= 5 => Some(n as u8),
            _ => return Err("`hintRung` is 0 to 5, or null when you gave no hint".into()),
        },
    };
    let miss_class = match args.get("missClass") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            serde_json::from_value::<learner::MissClass>(v.clone()).map_err(|_| {
                "`missClass` is cue, discrimination, execution, careless or structural"
            })?,
        ),
    };
    let misconception = args
        .get("misconception")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_owned);
    let note = args
        .get("note")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();

    // The learner skipped it: only a dismissal ends it, and a dismissal needs
    // the skip.
    if r.dismissed_at.is_some() && verdict != Verdict::Dismissed {
        return Err("the learner dismissed this problem: the verdict is `dismissed`".into());
    }
    if verdict == Verdict::Dismissed && r.dismissed_at.is_none() {
        return Err(
            "`dismissed` is for the Skip button; when the learner gives up in words, the verdict is `dontknow`"
                .into(),
        );
    }
    if r.attempts.is_empty() && verdict != Verdict::Dismissed {
        return Err(
            "the learner hasn't tried this yet: conclude after an answer, a give-up or a dismissal, never on a hunch about how it will go"
                .into(),
        );
    }
    if !r.hints.is_empty() && hint_rung.is_none() {
        return Err(format!(
            "the learner asked for {} hint(s): report the highest rung you gave in `hintRung` (0 to 5)",
            r.hints.len()
        ));
    }
    let miss = matches!(
        verdict,
        Verdict::Wrong | Verdict::Partial | Verdict::Dontknow
    );
    if miss && miss_class.is_none() {
        return Err("classify the miss: `missClass` is cue, discrimination, execution, careless or structural".into());
    }
    if verdict == Verdict::Correct && (miss_class.is_some() || misconception.is_some()) {
        return Err("a correct answer carries no `missClass` or `misconception`".into());
    }
    if let Some(m) = &misconception {
        if miss_class != Some(learner::MissClass::Structural) {
            return Err("`misconception` goes with `missClass: structural`".into());
        }
        if m.chars().count() > learner::MAX_MISCONCEPTION_CHARS {
            return Err(format!(
                "`misconception` is a short note: at most {} characters",
                learner::MAX_MISCONCEPTION_CHARS
            ));
        }
    }
    if miss_class == Some(learner::MissClass::Structural) && misconception.is_none() {
        return Err("a structural miss needs a `misconception` note: what the learner believes that is wrong".into());
    }
    // An exam problem is marked: the coach awards marks, consistent with the
    // verdict; a conclusion by the app (no answer) is worth nothing.
    let marks_awarded = match r.problem().and_then(|p| p.marks) {
        None => None,
        Some(_) if by == "app" => Some(0),
        Some(out_of) => {
            let awarded = args
                .get("marksAwarded")
                .and_then(Value::as_u64)
                .and_then(|m| u32::try_from(m).ok())
                .filter(|m| *m <= out_of)
                .ok_or_else(|| {
                    format!(
                        "this exam problem is worth {out_of} mark(s): give `marksAwarded` (0 to {out_of}) as the mark scheme would"
                    )
                })?;
            match verdict {
                Verdict::Correct if awarded != out_of => {
                    return Err(format!(
                        "a correct answer earns all {out_of} mark(s); give `partial` when marks were lost"
                    ))
                }
                Verdict::Partial | Verdict::Wrong if awarded == out_of => {
                    return Err(format!(
                        "{awarded} of {out_of} marks is a full-mark answer: the verdict is `correct`"
                    ))
                }
                Verdict::Dontknow | Verdict::Dismissed if awarded != 0 => {
                    return Err("an answer given up on or skipped earns no marks".into())
                }
                _ => {}
            }
            Some(awarded)
        }
    };
    Ok(Conclusion {
        verdict,
        hint_rung,
        miss_class,
        misconception,
        note,
        rating: String::new(),
        duration_sec: 0,
        confidence: None,
        by: by.to_owned(),
        marks_awarded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learner::Layer;
    use crate::practice::state::{Attempt, Committed};
    use crate::practice::{Move, ProblemPlan, ProblemType};
    use crate::state::Level;
    use serde_json::json;

    fn record() -> ProblemRecord {
        ProblemRecord {
            id: "p1".into(),
            index: 0,
            plan: ProblemPlan {
                target: "deriv".into(),
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
            },
            planned_at: "2026-11-14T10:00:00Z".into(),
            commit: Some(Committed {
                problem: Problem {
                    statement: "s".into(),
                    targets: vec!["deriv".into()],
                    hosts: vec![],
                    layer: Layer::Recall,
                    level: Level::Use,
                    cue_distance: 2,
                    is_distractor: false,
                    kind: ProblemType::Standard,
                    solution: vec![Move {
                        text: "m".into(),
                        uses_node: None,
                        decision_point: true,
                    }],
                    final_answer: "a".into(),
                    acceptable_variants: vec![],
                    rubric: vec![],
                    check: None,
                    surface: "x".into(),
                    marks: None,
                },
                checks: Checks::default(),
                at: "2026-11-14T10:00:01Z".into(),
            }),
            rejections: vec![],
            posed_at: Some("2026-11-14T10:00:02Z".into()),
            hints: vec![],
            attempts: vec![],
            dismissed_at: None,
            conclusion: None,
        }
    }

    fn tried(mut r: ProblemRecord) -> ProblemRecord {
        r.attempts.push(Attempt {
            text: "t = 2".into(),
            confidence: Some(70),
            at: "2026-11-14T10:00:42Z".into(),
        });
        r
    }

    fn why(args: Value, r: &ProblemRecord) -> String {
        conclusion_from(&args, r, "coach").unwrap_err()
    }

    #[test]
    fn an_exam_conclusion_awards_marks_that_agree_with_the_verdict() {
        let mut r = tried(record());
        if let Some(c) = r.commit.as_mut() {
            c.problem.marks = Some(5);
        }
        let v = |verdict: &str, marks: Option<u64>| {
            let mut args = json!({ "verdict": verdict, "hintRung": null, "note": "n" });
            if matches!(verdict, "partial" | "wrong" | "dontknow") {
                args["missClass"] = json!("execution");
            }
            if let Some(m) = marks {
                args["marksAwarded"] = json!(m);
            }
            conclusion_from(&args, &r, "coach")
        };
        assert!(v("correct", None).unwrap_err().contains("worth 5 mark(s)"));
        assert!(v("correct", Some(6)).unwrap_err().contains("0 to 5"));
        assert!(v("correct", Some(3)).unwrap_err().contains("earns all 5"));
        assert!(v("partial", Some(5)).unwrap_err().contains("full-mark"));
        assert!(v("wrong", Some(5)).unwrap_err().contains("full-mark"));
        assert_eq!(v("correct", Some(5)).unwrap().marks_awarded, Some(5));
        assert_eq!(v("partial", Some(2)).unwrap().marks_awarded, Some(2));
        assert_eq!(v("wrong", Some(0)).unwrap().marks_awarded, Some(0));
        assert!(v("dontknow", Some(1)).is_err_and(|e| e.contains("no marks")));
        // The app concludes what nobody answered, worth nothing.
        let c = conclusion_from(
            &json!({ "verdict": "dontknow", "hintRung": null, "missClass": "cue", "note": "n" }),
            &r,
            "app",
        )
        .unwrap();
        assert_eq!(c.marks_awarded, Some(0));
        // A problem outside a paper carries no marks, and `marksAwarded` is ignored.
        let plain = tried(record());
        let c = conclusion_from(
            &json!({ "verdict": "correct", "hintRung": null, "marksAwarded": 3, "note": "n" }),
            &plain,
            "coach",
        )
        .unwrap();
        assert_eq!(c.marks_awarded, None);
    }

    #[test]
    fn a_conclusion_before_any_attempt_is_refused_unless_it_is_a_dismissal() {
        let mut r = record();
        assert!(why(
            json!({ "verdict": "wrong", "hintRung": null, "note": "n" }),
            &r
        )
        .contains("hasn't tried"));
        r.dismissed_at = Some("2026-11-14T10:00:10Z".into());
        let c = conclusion_from(
            &json!({ "verdict": "dismissed", "hintRung": null, "note": "n" }),
            &r,
            "coach",
        )
        .unwrap();
        assert_eq!(c.verdict, Verdict::Dismissed);
        assert!(why(
            json!({ "verdict": "wrong", "hintRung": null, "note": "n" }),
            &r
        )
        .contains("dismissed"));
        // `dismissed` needs the skip button.
        assert!(why(
            json!({ "verdict": "dismissed", "hintRung": null, "note": "n" }),
            &tried(record())
        )
        .contains("Skip"));
    }

    #[test]
    fn the_hints_the_learner_asked_for_must_be_reported() {
        let mut r = tried(record());
        r.hints.push("2026-11-14T10:00:20Z".into());
        assert!(why(
            json!({ "verdict": "correct", "hintRung": null, "note": "n" }),
            &r
        )
        .contains("asked for 1 hint"));
        let c = conclusion_from(
            &json!({ "verdict": "correct", "hintRung": 2, "note": "n" }),
            &r,
            "coach",
        )
        .unwrap();
        assert_eq!(c.hint_rung, Some(2));
        assert!(why(
            json!({ "verdict": "correct", "hintRung": 6, "note": "n" }),
            &r
        )
        .contains("0 to 5"));
    }

    #[test]
    fn a_miss_needs_its_class_and_a_structural_one_its_misconception() {
        let r = tried(record());
        assert!(why(
            json!({ "verdict": "wrong", "hintRung": null, "note": "n" }),
            &r
        )
        .contains("classify the miss"));
        assert!(why(
            json!({ "verdict": "wrong", "hintRung": null, "missClass": "structural", "note": "n" }),
            &r
        )
        .contains("misconception"));
        assert!(why(json!({ "verdict": "wrong", "hintRung": null, "missClass": "careless", "misconception": "x", "note": "n" }), &r).contains("structural"));
        assert!(why(
            json!({ "verdict": "correct", "hintRung": null, "missClass": "careless", "note": "n" }),
            &r
        )
        .contains("correct answer carries no"));
        let ok = conclusion_from(&json!({ "verdict": "wrong", "hintRung": null, "missClass": "structural", "misconception": "adds exponents", "note": "n" }), &r, "coach").unwrap();
        assert_eq!(ok.misconception.as_deref(), Some("adds exponents"));
        assert!(conclusion_from(
            &json!({ "verdict": "correct", "hintRung": null, "note": "n" }),
            &r,
            "coach"
        )
        .is_ok());
    }

    #[test]
    fn the_outcome_carries_the_rating_inputs_of_the_problem() {
        let mut r = tried(record());
        r.hints.push("2026-11-14T10:00:20Z".into());
        let c = conclusion_from(
            &json!({ "verdict": "correct", "hintRung": 3, "note": "n" }),
            &r,
            "coach",
        )
        .unwrap();
        let p = r.problem().unwrap();
        let o = outcome_for("s1", Format::Stream, &r, p, &c, "2026-11-14T10:01:00Z");
        assert_eq!(o.targets, ["deriv"]);
        assert_eq!(o.cue_distance, Some(2));
        assert_eq!(o.hint_rung, Some(3));
        assert_eq!(o.confidence, Some(70));
        assert_eq!(o.duration_sec, 40, "pose to the last submission");
        assert_eq!(o.level, Some(Level::Use));
        assert_eq!(
            learner::rating(&o),
            learner::Rating::Again,
            "R3+ surrenders the idea"
        );

        // A give-up carries no confidence even if an earlier submission did.
        let c = conclusion_from(
            &json!({ "verdict": "dontknow", "hintRung": null, "missClass": "cue", "note": "n" }),
            &r,
            "coach",
        );
        let c = c.unwrap_err();
        assert!(c.contains("hint"), "{c}");
        let mut r2 = tried(record());
        r2.hints.clear();
        let c = conclusion_from(
            &json!({ "verdict": "dontknow", "hintRung": null, "missClass": "cue", "note": "n" }),
            &r2,
            "coach",
        )
        .unwrap();
        let o = outcome_for(
            "s1",
            Format::Stream,
            &r2,
            r2.problem().unwrap(),
            &c,
            "2026-11-14T10:01:00Z",
        );
        assert_eq!(o.confidence, None);
        // A set has no per-problem timing.
        let o = outcome_for(
            "s1",
            Format::Set,
            &r2,
            r2.problem().unwrap(),
            &c,
            "2026-11-14T10:01:00Z",
        );
        assert_eq!(o.duration_sec, 0);
    }

    #[test]
    fn the_pinned_state_holds_the_answer_key_of_the_open_problem() {
        let s = PracticeState {
            started: true,
            format: Some(Format::Stream),
            problems: vec![tried(record())],
            ..Default::default()
        };
        let map = KnowledgeMap::default();
        let text = coach_state(&s, &map, &LearnerModel::default(), Utc::now(), None);
        assert!(text.contains("## Open problem (private"), "{text}");
        assert!(
            text.contains("Final answer: a") && text.contains("decision point"),
            "{text}"
        );
        assert!(text.contains("- Format: stream"));
    }
}
