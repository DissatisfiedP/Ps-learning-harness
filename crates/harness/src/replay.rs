//! Derived state and model messages, both rebuilt from the op log (build
//! prompt M4: "never store derived state").
//!
//! [`replay_state`] folds ops into a [`SessionState`]. [`conversation`] turns
//! the conversation ops (learner/tutor messages, the tutor's tool calls and
//! results, delivered job results) into provider messages, applying the latest
//! compaction. The planner's own tool loop is not part of the conversation;
//! its effects reach the tutor as `plan` ops and `[plan update]` messages.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use providers::{ContentBlock, Message, Role};
use serde_json::Value;

use crate::sessions::{HuntSendOp, Op, ToolOp};
use crate::state::{
    BranchState, Calibration, CalibrationBucket, CheckVerdict, Grant, HuntState, Level, OpenJob,
    PendingCall, Plan, PlanQuestion, QueuedResult, SessionState,
};

/// Text shown to a model for a call whose result is missing: the learner
/// hasn't answered yet, or the app stopped mid-call.
pub(crate) const PENDING_RESULT: &str = "(waiting for the learner's answer)";
const MISSING_RESULT: &str = "(no result: the call was interrupted)";

/// Whether a tool op belongs to the conversation (the tutor's loop).
pub(crate) fn is_conversation_tool(t: &ToolOp) -> bool {
    t.agent.as_deref().is_none_or(|a| a == "tutor")
}

/// Derive the full session state from ops sorted by `at`.
pub fn replay_state(ops: &[Op]) -> SessionState {
    let mut s = SessionState {
        platform: "desktop".into(),
        learner_state: "no graph yet".into(),
        ..Default::default()
    };
    // callId -> (name, args) of conversation calls.
    let mut calls: HashMap<String, (String, Value)> = HashMap::new();
    // callId -> the confidence the learner gave, for calibration (§7).
    let mut confidences: HashMap<String, u32> = HashMap::new();
    // callId -> whether the graded answer was correct, for calibration (§7):
    // every graded question, including probe answers, which carry no node.
    let mut graded: HashMap<String, bool> = HashMap::new();
    let mut jobs: Vec<OpenJob> = Vec::new();
    let mut done: Vec<QueuedResult> = Vec::new();
    let mut delivered: HashSet<String> = HashSet::new();

    for op in ops {
        match op {
            Op::Phase(p) => {
                s.phase = p.phase;
                s.current_section.clone_from(&p.section_id);
            }
            Op::Title(t) => s.title = Some(t.title.clone()),
            Op::Goal(g) => s.goal = Some(g.payload.clone()),
            Op::Brief(b) => s.brief = Some(b.payload.clone()),
            Op::Probe(p) => {
                s.probe = p
                    .payload
                    .get("strands")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
            }
            Op::Plan(p) => {
                if let Ok(mut plan) = serde_json::from_value::<Plan>(p.payload.clone()) {
                    s.plan_version += 1;
                    plan.version = s.plan_version;
                    s.plan = Some(plan);
                }
            }
            Op::Approve(_) => s.approved_version = Some(s.plan_version),
            Op::PluginGrant(g) => {
                let grant = Grant {
                    section_id: g.section_id.clone(),
                    plugin: g.plugin.clone(),
                    by: g.by.clone(),
                };
                if !s.grants.contains(&grant) {
                    s.grants.push(grant);
                }
            }
            Op::Tool(t) if !is_conversation_tool(t) => {}
            Op::Tool(t) => match &t.result {
                None => {
                    if matches!(t.name.as_str(), "question_ask" | "choice_ask") {
                        s.pending_call = Some(PendingCall {
                            call_id: t.call_id.clone(),
                            name: t.name.clone(),
                            args: t.args.clone(),
                        });
                        if s.phase == crate::state::Phase::Teach {
                            if let Some(n) = t.args.get("nodeId").and_then(Value::as_str) {
                                s.last_node_id = Some(n.to_owned());
                            }
                        }
                    }
                    calls.insert(t.call_id.clone(), (t.name.clone(), t.args.clone()));
                }
                Some(result) => {
                    if s.pending_call
                        .as_ref()
                        .is_some_and(|p| p.call_id == t.call_id)
                    {
                        s.pending_call = None;
                    }
                    if result.is_error {
                        continue;
                    }
                    let Some((name, args)) = calls.get(&t.call_id).cloned() else {
                        continue;
                    };
                    match name.as_str() {
                        // A choice question graded by code carries its verdict
                        // in the result.
                        "question_ask" => {
                            if let Ok(v) = serde_json::from_str::<Value>(&result.content) {
                                if let Some(c) = v.get("confidence").and_then(Value::as_u64) {
                                    confidences.insert(t.call_id.clone(), c as u32);
                                }
                                if let Some(verdict) = v.get("verdict").and_then(Value::as_str) {
                                    if let Some(correct) =
                                        record_verdict(&mut s, &calls, &t.call_id, verdict)
                                    {
                                        graded.entry(t.call_id.clone()).or_insert(correct);
                                    }
                                }
                            }
                        }
                        "verdict_record" => {
                            let qid = args.get("questionId").and_then(Value::as_str);
                            let verdict = args.get("verdict").and_then(Value::as_str);
                            if let (Some(qid), Some(verdict)) = (qid, verdict) {
                                if let Some(correct) = record_verdict(&mut s, &calls, qid, verdict)
                                {
                                    graded.entry(qid.to_owned()).or_insert(correct);
                                }
                            }
                        }
                        "section_done" => {
                            if let Some(id) = args.get("sectionId").and_then(Value::as_str) {
                                if !s.completed_sections.iter().any(|c| c == id) {
                                    s.completed_sections.push(id.to_owned());
                                }
                            }
                        }
                        "session_end" => s.ended = true,
                        _ => {}
                    }
                }
            },
            Op::Job(j) => {
                jobs.retain(|o| o.job_id != j.job_id);
                match j.state.as_str() {
                    "queued" | "running" => {
                        let previous_args = j.args.clone();
                        jobs.push(OpenJob {
                            job_id: j.job_id.clone(),
                            kind: j.kind.clone(),
                            state: j.state.clone(),
                            question: j.question.clone().unwrap_or_default(),
                            blocking: j.blocking,
                            args: previous_args,
                        });
                    }
                    // A graph build is not conversation material: its result
                    // is the counts it appended, and delivering it would wake
                    // the tutor for nothing (architecture §4.7).
                    "done" | "failed" if !j.blocking && j.kind != "graph" => {
                        let result = match (&j.result, &j.error) {
                            (_, Some(e)) if j.state == "failed" => {
                                Some(serde_json::json!({ "error": e }))
                            }
                            (Some(r), _) => Some(r.clone()),
                            _ => None,
                        };
                        if let Some(result) = result {
                            done.push(QueuedResult {
                                job_id: j.job_id.clone(),
                                kind: j.kind.clone(),
                                question: j.question.clone().unwrap_or_default(),
                                result,
                            });
                        }
                    }
                    _ => {}
                }
            }
            Op::Message(m) => {
                if let Some(job) = &m.job_id {
                    delivered.insert(job.clone());
                }
            }
            // A forked or merge session's header names its track (§4.26); a
            // merge also starts with the sections its inputs completed.
            Op::Session(h) => {
                if let Some(f) = &h.fork {
                    s.branch = Some(BranchState {
                        kind: "fork".into(),
                        origin: f.parent.clone(),
                        track: f.track_id.clone(),
                    });
                }
                if let Some(m) = &h.merge {
                    s.branch = Some(BranchState {
                        kind: "merge".into(),
                        origin: m.origin.clone(),
                        track: m.track_id.clone(),
                    });
                    for id in &m.completed {
                        if !s.completed_sections.contains(id) {
                            s.completed_sections.push(id.clone());
                        }
                    }
                }
            }
            // The origin keeps the first root track once it forked the rest.
            Op::Branch(b) => {
                s.branch = Some(BranchState {
                    kind: "origin".into(),
                    origin: String::new(),
                    track: b.track.clone(),
                });
            }
            Op::PlanRequest(_) | Op::PlanKeep(_) | Op::Compaction(_) => {}
            Op::PlanEdit(_) | Op::PlanAsk(_) | Op::PlanAnswer(_) => {}
            // Hunts are derived whole at the end (§5): toggles land on the
            // plan's sections and the lifecycle needs every hunt op at once.
            Op::HuntToggle(_)
            | Op::HuntSend(_)
            | Op::HuntReturn(_)
            | Op::HuntSkip(_)
            | Op::Debrief(_) => {}
            Op::KnownRefuted(k) => {
                if !s.known_refuted.contains(&k.section_id) {
                    s.known_refuted.push(k.section_id.clone());
                }
            }
            Op::Workspace(w) => {
                s.workspace = w.attached.then(|| w.name.clone());
            }
            Op::WorkspaceWrite(w) => {
                s.workspace_writes.insert(w.path.clone(), w.sha256.clone());
            }
            Op::Artifact(_) => {}
            Op::FileGrant(g) => {
                if !s.file_grants.contains(&g.path) {
                    s.file_grants.push(g.path.clone());
                }
            }
            // The view summary is rendered into the conversation below; the
            // mobile lock is read by `mobile.rs`, never by replay.
            Op::ViewState(_) | Op::MobileUnlock(_) => {}
        }
    }
    // A `running` op carries no args; keep the ones from `queued`.
    for job in &mut jobs {
        if job.args.is_none() {
            job.args = ops.iter().find_map(|o| match o {
                Op::Job(j) if j.job_id == job.job_id && j.args.is_some() => j.args.clone(),
                _ => None,
            });
        }
    }
    s.open_jobs = jobs;
    s.queued_jobs = done
        .into_iter()
        .filter(|r| !delivered.contains(&r.job_id))
        .collect();
    s.calibration = calibration(&graded, &confidences);
    s.pending_plan_question = pending_plan_question(ops);
    apply_hunts(ops, &mut s);
    s.active_plugins = active_plugins(&s);
    s
}

// ---------------------------------------------------------------------------
// Hunts (§5)
// ---------------------------------------------------------------------------

/// Apply the learner's hunt toggles onto the plan's sections and derive the
/// current section's hunt lifecycle. Both halves come from the same ops, so
/// gates, prompts and the UI read one value: `Section.hunt.enabled` is the
/// learner's choice, never the planner's.
fn apply_hunts(ops: &[Op], s: &mut SessionState) {
    // sectionId -> the learner's latest toggle.
    let mut toggles: HashMap<&str, bool> = HashMap::new();
    for op in ops {
        if let Op::HuntToggle(t) = op {
            toggles.insert(t.section_id.as_str(), t.enabled);
        }
    }
    // A skip is the learner's final word on that hunt (§5: always skippable),
    // so it switches the section's hunt off whatever the last toggle said.
    let skipped_hunts: HashSet<&str> = ops
        .iter()
        .filter_map(|o| match o {
            Op::HuntSkip(k) => Some(k.hunt_id.as_str()),
            _ => None,
        })
        .collect();
    let skipped_sections: HashSet<&str> = ops
        .iter()
        .filter_map(|o| match o {
            Op::HuntSend(h) if skipped_hunts.contains(h.hunt_id.as_str()) => {
                Some(h.section_id.as_str())
            }
            _ => None,
        })
        .collect();

    if let Some(plan) = &mut s.plan {
        for section in &mut plan.sections {
            let Some(hunt) = &mut section.hunt else {
                continue;
            };
            if let Some(&enabled) = toggles.get(section.id.as_str()) {
                hunt.enabled = enabled;
            }
            if skipped_sections.contains(section.id.as_str()) {
                hunt.enabled = false;
            }
        }
    }

    // Every hunt keeps its own lifecycle: the flow's cards are looked up by
    // `huntId`, so a card from an earlier section shows its own finished
    // state when teaching has moved on. The current section's is the one the
    // gates and the prompt read; a later send for a section wins over an
    // earlier one, another section's hunt never leaks in.
    s.hunts = hunt_states(ops);
    let current = s.current_section.clone();
    s.hunt = current.as_deref().and_then(|section_id| {
        s.hunts
            .iter()
            .rev()
            .find(|h| h.section_id == section_id)
            .cloned()
    });
}

/// Every hunt the tutor sent (§5), in op order, one record per `hunt_send`
/// call id.
fn hunt_states(ops: &[Op]) -> Vec<HuntState> {
    let mut states: Vec<HuntState> = Vec::new();
    for op in ops {
        let Op::HuntSend(send) = op else {
            continue;
        };
        if states.iter().any(|s| s.hunt_id == send.hunt_id) {
            continue;
        }
        states.push(hunt_state(ops, send));
    }
    states
}

/// One hunt's lifecycle, from the ops that name its id.
fn hunt_state(ops: &[Op], send: &HuntSendOp) -> HuntState {
    let hunt_id = send.hunt_id.as_str();
    let returned = ops.iter().rev().find_map(|o| match o {
        Op::HuntReturn(r) if r.hunt_id == hunt_id => Some(r),
        _ => None,
    });
    let skipped = ops
        .iter()
        .any(|o| matches!(o, Op::HuntSkip(k) if k.hunt_id == hunt_id));
    let debriefed = ops
        .iter()
        .any(|o| matches!(o, Op::Debrief(d) if d.hunt_id == hunt_id));
    // The learner has the hunt once its `hunt_send` call completed: the
    // result arrives in the same turn, after the card is on screen.
    let told = ops.iter().any(|o| {
        matches!(o, Op::Tool(t) if t.call_id == hunt_id
            && t.result.as_ref().is_some_and(|r| !r.is_error))
    });
    let state = if skipped {
        "skipped"
    } else if returned.is_some() {
        "returned"
    } else if told {
        "out"
    } else {
        "ready"
    };
    HuntState {
        hunt_id: hunt_id.to_owned(),
        section_id: send.section_id.clone(),
        question: send.question.clone(),
        look_at: send.look_at.clone(),
        bring_back: send.bring_back.clone(),
        state: state.to_owned(),
        returned: returned.map(|r| {
            serde_json::json!({
                "notes": r.notes,
                "links": r.links,
                "files": r.files,
            })
        }),
        debriefed,
    }
}

/// The planner's latest `planAsk` op that no `planAnswer` has answered
/// (docs/learning-system.md §4). The answer is matched by its call id.
fn pending_plan_question(ops: &[Op]) -> Option<PlanQuestion> {
    let answered: HashSet<&str> = ops
        .iter()
        .filter_map(|o| match o {
            Op::PlanAnswer(a) => Some(a.call_id.as_str()),
            _ => None,
        })
        .collect();
    ops.iter().rev().find_map(|o| match o {
        Op::PlanAsk(a) if !answered.contains(a.call_id.as_str()) => Some(PlanQuestion {
            call_id: a.call_id.clone(),
            question: a.question.clone(),
            options: a.options.clone(),
        }),
        _ => None,
    })
}

/// Fold one graded answer into the derived state (§2 and §7). Returns whether
/// it was correct when the graded question is one this knows how to read:
/// **every** `question_ask` answer counts towards calibration, including
/// probe answers, which carry no node; only `check` questions tagged to a real
/// node count towards a section's exit, the per-node learner model and the
/// level gate.
fn record_verdict(
    s: &mut SessionState,
    calls: &HashMap<String, (String, Value)>,
    question_id: &str,
    verdict: &str,
) -> Option<bool> {
    let (name, args) = calls.get(question_id)?;
    if name != "question_ask" {
        return None;
    }
    let correct = verdict == "correct";
    // Exit evidence: a check question, on a node of the section.
    let node_id = args
        .get("role")
        .and_then(Value::as_str)
        .filter(|role| *role == "check")
        .and(args.get("nodeId"))
        .and_then(Value::as_str);
    if let Some(node_id) = node_id {
        // The level is copied here, from the graded question's own args (§2).
        let level = args
            .get("level")
            .and_then(Value::as_str)
            .and_then(Level::parse);
        s.checks
            .entry(node_id.to_owned())
            .or_default()
            .push(CheckVerdict {
                node_id: node_id.to_owned(),
                question_id: question_id.to_owned(),
                verdict: verdict.to_owned(),
                level,
            });
        // The learner model reports the highest level passed per node; never
        // stored (§2).
        if correct {
            if let Some(level) = level {
                let best = s.node_levels.entry(node_id.to_owned()).or_insert(level);
                if level > *best {
                    *best = level;
                }
            }
        }
    }
    Some(correct)
}

// ---------------------------------------------------------------------------
// Calibration (§7)
// ---------------------------------------------------------------------------

/// The five confidence buttons (ui-design §7.2.4), in ascending order.
const CALIBRATION_BUCKETS: [(u32, &str); 5] = [
    (10, "Guessing"),
    (30, "Unsure"),
    (50, "Even odds"),
    (70, "Fairly sure"),
    (90, "Certain"),
];
/// Below this many graded answers, calibration says nothing at all.
const CALIBRATION_MIN_GRADED: usize = 20;
/// A bucket is only reported once it has this many answers.
const CALIBRATION_MIN_BUCKET: u32 = 5;
/// A bucket is only named when its accuracy is this many points off.
const CALIBRATION_GAP: u32 = 15;

/// The calibration block: per confidence bucket, the share of correct
/// answers, and observational notes. `None` below the thresholds. Only
/// answers with both a confidence and a recorded verdict count — a probe
/// answer counts the same as a check (§7, "the confidence the learner already
/// gives on every answer").
fn calibration(
    graded: &HashMap<String, bool>,
    confidences: &HashMap<String, u32>,
) -> Option<Calibration> {
    // A question graded twice counts once: the first verdict stands, which
    // the map's keys already do.
    let mut answers: Vec<(bool, u32)> = graded
        .iter()
        .filter_map(|(question_id, correct)| {
            let confidence = *confidences.get(question_id)?;
            let bucket = CALIBRATION_BUCKETS
                .iter()
                .find(|(value, _)| *value == confidence)?
                .0;
            Some((*correct, bucket))
        })
        .collect();
    if answers.len() < CALIBRATION_MIN_GRADED {
        return None;
    }
    answers.sort_by_key(|(_, bucket)| *bucket);

    let mut buckets = Vec::new();
    let mut notes = Vec::new();
    for (confidence, label) in CALIBRATION_BUCKETS {
        let answers: Vec<bool> = answers
            .iter()
            .filter(|(_, bucket)| *bucket == confidence)
            .map(|(correct, _)| *correct)
            .collect();
        let total = answers.len() as u32;
        if total < CALIBRATION_MIN_BUCKET {
            continue;
        }
        let correct = answers.iter().filter(|correct| **correct).count() as u32;
        let pct = crate::state::percent(correct, total);
        if pct + CALIBRATION_GAP <= confidence {
            notes.push(format!(
                "When you picked {label}, you were right {correct} of {total} times ({pct}%): your confidence is ahead of your results."
            ));
        } else if pct >= confidence + CALIBRATION_GAP {
            notes.push(format!(
                "When you picked {label}, you were right {correct} of {total} times ({pct}%): your results are ahead of your confidence."
            ));
        }
        buckets.push(CalibrationBucket {
            confidence,
            label: label.to_owned(),
            correct,
            total,
        });
    }
    if notes.is_empty() {
        notes.push("Confidence matches results.".to_owned());
    }
    Some(Calibration {
        graded: answers.len() as u32,
        buckets,
        notes,
    })
}

/// Plugins active in the current section: plan grants still listed by the
/// current plan, plus mid-section `plugin_request` grants.
fn active_plugins(s: &SessionState) -> Vec<String> {
    let Some(section) = s.current_section() else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for g in &s.grants {
        if g.section_id != section.id || out.contains(&g.plugin) {
            continue;
        }
        if g.by != "plan" || section.plugins.contains(&g.plugin) {
            out.push(g.plugin.clone());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Conversation messages
// ---------------------------------------------------------------------------

/// Accumulates provider messages with the merge rules every wire accepts:
/// an assistant message holds its text, native blocks and tool calls; tool
/// results go into one user message right after; other user text is its own
/// message.
#[derive(Default)]
struct Builder {
    out: Vec<Message>,
    /// Calls emitted whose result hasn't been emitted yet, in order.
    open: Vec<String>,
    pending: Option<String>,
}

impl Builder {
    fn assistant(&mut self, block: ContentBlock) {
        if !matches!(block, ContentBlock::ToolCall { .. }) {
            // New text after calls means those calls never got results.
            self.close_open();
        }
        match self.out.last_mut() {
            Some(m) if m.role == Role::Assistant && self.open.is_empty() => m.content.push(block),
            Some(m)
                if m.role == Role::Assistant && matches!(block, ContentBlock::ToolCall { .. }) =>
            {
                m.content.push(block)
            }
            _ => self.out.push(Message {
                role: Role::Assistant,
                content: vec![block],
            }),
        }
        if let Some(ContentBlock::ToolCall { id, .. }) =
            self.out.last().and_then(|m| m.content.last())
        {
            self.open.push(id.clone());
        }
    }

    fn result(&mut self, call_id: &str, content: String, is_error: bool) {
        let Some(pos) = self.open.iter().position(|c| c == call_id) else {
            return; // its call was compacted away or never sent
        };
        self.open.remove(pos);
        let block = ContentBlock::ToolResult {
            call_id: call_id.to_owned(),
            content,
            is_error,
        };
        match self.out.last_mut() {
            Some(m)
                if m.role == Role::User
                    && m.content
                        .iter()
                        .all(|b| matches!(b, ContentBlock::ToolResult { .. })) =>
            {
                m.content.push(block)
            }
            _ => self.out.push(Message {
                role: Role::User,
                content: vec![block],
            }),
        }
    }

    fn user_text(&mut self, text: String) {
        self.close_open();
        self.out.push(Message::text(Role::User, text));
    }

    fn user_content(&mut self, content: Vec<ContentBlock>) {
        self.close_open();
        self.out.push(Message {
            role: Role::User,
            content,
        });
    }

    /// Answer every open call so the wire sees a result for each call.
    fn close_open(&mut self) {
        for id in std::mem::take(&mut self.open) {
            let text = if self.pending.as_deref() == Some(id.as_str()) {
                PENDING_RESULT
            } else {
                MISSING_RESULT
            };
            self.open.push(id.clone());
            self.result(&id, text.to_owned(), false);
        }
    }

    fn finish(mut self) -> Vec<Message> {
        self.close_open();
        self.out
    }
}

/// The ops after the latest compaction cut, plus its summary.
pub(crate) fn after_cut(ops: &[Op]) -> (Option<&str>, &[Op]) {
    let latest = ops.iter().rev().find_map(|o| match o {
        Op::Compaction(c) => Some(c),
        _ => None,
    });
    match latest {
        None => (None, ops),
        Some(c) => {
            let key = (c.cut_at.as_str(), c.cut_op_id.as_str());
            let start = ops.partition_point(|o| (o.at(), o.op_id()) <= key);
            (Some(c.summary.as_str()), &ops[start..])
        }
    }
}

/// Provider messages for the conversation (tutor; also the planner's context).
#[cfg(test)]
pub(crate) fn conversation(ops: &[Op]) -> Vec<Message> {
    conversation_with(ops, None, true, true)
}

/// As [`conversation`], resolving image attachments from `repo` and dropping
/// them when the model is known to lack vision (W2/W3). `file_tools` says
/// whether the agent can read attached files (Practice can't).
pub(crate) fn conversation_with(
    ops: &[Op],
    repo: Option<&Path>,
    vision: bool,
    file_tools: bool,
) -> Vec<Message> {
    let pending = replay_pending(ops);
    let (summary, tail) = after_cut(ops);
    let mut b = Builder {
        pending,
        ..Default::default()
    };
    if let Some(summary) = summary {
        b.user_text(format!(
            "[notes on the earlier part of this session]\n\n{summary}"
        ));
    }
    // Latest view-state summary per artifact since the last learner message;
    // rendered as tagged lines on the next one (W7).
    let mut view_states: BTreeMap<&str, &str> = Default::default();
    for op in tail {
        match op {
            Op::Message(m) if m.role == Role::Assistant => {
                for n in &m.native {
                    b.assistant(n.clone());
                }
                if !m.content.trim().is_empty() {
                    b.assistant(ContentBlock::Text {
                        text: m.content.clone(),
                    });
                }
            }
            Op::ViewState(v) => {
                view_states.insert(&v.artifact_id, &v.summary);
            }
            Op::Message(m) => {
                let mut blocks: Vec<ContentBlock> = Vec::new();
                let mut text = m.content.clone();
                if m.tag.is_none() && m.job_id.is_none() {
                    // Sessions from before viewState ops were kept apart
                    // already carry the lines in their content.
                    if !view_states.is_empty() && !m.content.contains("[view state ") {
                        if !text.trim().is_empty() {
                            text.push_str("\n\n");
                        }
                        let lines: Vec<String> = view_states
                            .iter()
                            .map(|(id, s)| format!("[view state {id}] {s}"))
                            .collect();
                        text.push_str(&lines.join("\n"));
                    }
                    view_states.clear();
                }
                if !text.trim().is_empty() {
                    blocks.push(ContentBlock::text(text));
                }
                let mut lines = Vec::new();
                for a in &m.attachments {
                    if a.kind == "image" {
                        match repo.and_then(|r| crate::attachments::image_block(r, a)) {
                            Some((media_type, data)) if vision => {
                                blocks.push(ContentBlock::Image { media_type, data });
                            }
                            _ => lines.push(format!(
                                "[image attached] {} (not sent to this model)",
                                a.name.as_deref().unwrap_or(&a.path)
                            )),
                        }
                    } else {
                        lines.push(crate::attachments::reference_line(a, file_tools));
                    }
                }
                // An attachments-only message from an older build still
                // replays: its blocks are just the images and these lines.
                if !lines.is_empty() {
                    blocks.push(ContentBlock::text(lines.join("\n")));
                }
                b.user_content(blocks);
            }
            Op::Tool(t) if is_conversation_tool(t) => match &t.result {
                None => b.assistant(ContentBlock::ToolCall {
                    id: t.call_id.clone(),
                    name: t.name.clone(),
                    arguments: t.args.clone(),
                }),
                Some(r) => b.result(&t.call_id, r.content.clone(), r.is_error),
            },
            _ => {}
        }
    }
    b.finish()
}

fn replay_pending(ops: &[Op]) -> Option<String> {
    let mut pending = None;
    for op in ops {
        if let Op::Tool(t) = op {
            if !is_conversation_tool(t) {
                continue;
            }
            match &t.result {
                None if matches!(t.name.as_str(), "question_ask" | "choice_ask") => {
                    pending = Some(t.call_id.clone());
                }
                Some(_) if pending.as_deref() == Some(t.call_id.as_str()) => pending = None,
                _ => {}
            }
        }
    }
    pending
}

/// Rough prompt size in tokens (characters / 4), used for the compaction
/// threshold.
pub(crate) fn estimate_tokens(system: &str, messages: &[Message]) -> usize {
    let mut chars = system.len();
    for m in messages {
        for b in &m.content {
            chars += match b {
                ContentBlock::Text { text } => text.len(),
                ContentBlock::ToolCall { arguments, .. } => arguments.to_string().len() + 32,
                ContentBlock::ToolResult { content, .. } => content.len() + 32,
                ContentBlock::Native { raw, .. } => raw.to_string().len(),
                ContentBlock::Image { data, .. } => data.len() / 4,
            };
        }
    }
    chars / 4
}

/// Where a compaction should cut: the newest learner-side message op that
/// leaves at least `keep_tokens` of recent conversation verbatim after it, at
/// a point where no tool call is waiting for its result. Returns the index
/// (into `ops`) of the last op the summary covers.
pub(crate) fn compaction_cut(ops: &[Op], keep_tokens: usize) -> Option<usize> {
    let (_, tail) = after_cut(ops);
    let offset = ops.len() - tail.len();
    let mut open: HashSet<&str> = HashSet::new();
    // Candidate cut points: before a user message with no open calls.
    let mut candidates: Vec<usize> = Vec::new();
    let mut size_after: Vec<usize> = vec![0; tail.len() + 1];
    for (i, op) in tail.iter().enumerate().rev() {
        size_after[i] = size_after[i + 1] + op_chars(op) / 4;
    }
    for (i, op) in tail.iter().enumerate() {
        match op {
            // Only cut where the summary would cover some conversation.
            Op::Message(m)
                if m.role == Role::User && open.is_empty() && size_after[0] > size_after[i] =>
            {
                candidates.push(i);
            }
            Op::Tool(t) if is_conversation_tool(t) => {
                if t.result.is_none() {
                    open.insert(t.call_id.as_str());
                } else {
                    open.remove(t.call_id.as_str());
                }
            }
            _ => {}
        }
    }
    candidates
        .into_iter()
        .rev()
        .find(|&i| size_after[i] >= keep_tokens)
        .map(|i| offset + i - 1)
}

fn op_chars(op: &Op) -> usize {
    match op {
        Op::Message(m) => m.content.len(),
        Op::Tool(t) if is_conversation_tool(t) => {
            t.args.to_string().len() + t.result.as_ref().map_or(0, |r| r.content.len())
        }
        _ => 0,
    }
}

/// Plain-text transcript of the conversation ops in `ops`, every item whole,
/// for the compaction prompt's `{{turns_to_summarize}}`.
pub(crate) fn transcript(ops: &[Op]) -> String {
    let mut out = String::new();
    for item in transcript_items(ops) {
        out.push_str(&item.label);
        out.push_str(": ");
        out.push_str(&item.text);
        out.push_str("\n\n");
    }
    out
}

/// One conversation op as the transcript shows it: who wrote it (or which
/// tool call or result it is) and its full text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TranscriptItem {
    pub label: String,
    pub text: String,
}

/// The conversation ops of `ops` in order: non-empty messages and the
/// tutor's tool calls and results. Nothing is clipped.
pub(crate) fn transcript_items(ops: &[Op]) -> Vec<TranscriptItem> {
    ops.iter()
        .filter(|op| in_transcript(op))
        .filter_map(|op| {
            let (label, text) = match op {
                Op::Message(m) => {
                    let who = match (m.role, m.tag.as_deref()) {
                        (Role::Assistant, _) => "Tutor",
                        (Role::User, Some(_)) => "App",
                        (Role::User, None) => "Learner",
                    };
                    (who.to_owned(), m.content.clone())
                }
                Op::Tool(t) => match &t.result {
                    None => (format!("Tutor called {}", t.name), t.args.to_string()),
                    Some(r) => (
                        format!(
                            "{} result{}",
                            t.name,
                            if r.is_error { " (error)" } else { "" }
                        ),
                        r.content.clone(),
                    ),
                },
                _ => return None,
            };
            Some(TranscriptItem { label, text })
        })
        .collect()
}

/// Whether `op` is a [`transcript_items`] entry: a non-empty message or a
/// conversation tool call or result.
pub(crate) fn in_transcript(op: &Op) -> bool {
    match op {
        Op::Message(m) => !m.content.trim().is_empty(),
        Op::Tool(t) => is_conversation_tool(t),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::{
        new_message, op_id, CompactionOp, KnownRefutedOp, PlanAnswerOp, PlanAskOp, ToolResultValue,
    };

    fn call(id: &str, name: &str, args: Value) -> Op {
        Op::Tool(ToolOp {
            op_id: op_id(),
            at: crate::now_iso(),
            call_id: id.into(),
            name: name.into(),
            args,
            result: None,
            agent: None,
            plugin: None,
            platforms: None,
        })
    }

    fn result(id: &str, name: &str, content: &str, is_error: bool) -> Op {
        Op::Tool(ToolOp {
            op_id: op_id(),
            at: crate::now_iso(),
            call_id: id.into(),
            name: name.into(),
            args: Value::Null,
            result: Some(ToolResultValue {
                content: content.into(),
                is_error,
            }),
            agent: None,
            plugin: None,
            platforms: None,
        })
    }

    fn text(role: Role, s: &str) -> Op {
        Op::Message(new_message(role, s.into()))
    }

    #[test]
    fn calls_and_results_group_into_alternating_messages() {
        let ops = vec![
            text(Role::User, "hi"),
            text(Role::Assistant, "two calls"),
            call("a", "goal_set", serde_json::json!({})),
            call("b", "file_find", serde_json::json!({})),
            result("a", "goal_set", "ok", false),
            result("b", "file_find", "none", true),
            text(Role::User, "[research brief] …"),
        ];
        let msgs = conversation(&ops);
        let roles: Vec<Role> = msgs.iter().map(|m| m.role).collect();
        assert_eq!(roles, [Role::User, Role::Assistant, Role::User, Role::User]);
        assert_eq!(msgs[1].content.len(), 3); // text + two calls
        assert_eq!(msgs[2].content.len(), 2); // both results, then the brief alone
    }

    #[test]
    fn a_pending_question_gets_a_placeholder_result_and_is_in_state() {
        let ops = vec![
            text(Role::User, "hi"),
            call("q1", "question_ask", serde_json::json!({"role":"probe"})),
        ];
        let msgs = conversation(&ops);
        let last = msgs.last().unwrap();
        assert!(matches!(&last.content[0],
            ContentBlock::ToolResult { content, .. } if content == PENDING_RESULT));
        assert_eq!(replay_state(&ops).pending_call.unwrap().call_id, "q1");
    }

    fn view(artifact: &str, summary: &str) -> Op {
        Op::ViewState(crate::sessions::ViewStateOp {
            op_id: op_id(),
            at: crate::now_iso(),
            artifact_id: artifact.into(),
            summary: summary.into(),
        })
    }

    fn user_text(m: &Message) -> String {
        m.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn view_states_render_once_on_the_next_learner_message_only() {
        let ops = vec![
            view("B", "b old"),
            view("A", "a"),
            view("B", "b new"),
            text(Role::User, "moved it"),
            text(Role::Assistant, "ok"),
            text(Role::User, "next"),
        ];
        let msgs = conversation(&ops);
        assert_eq!(
            user_text(&msgs[0]),
            "moved it\n\n[view state A] a\n[view state B] b new"
        );
        assert_eq!(user_text(&msgs[2]), "next");
    }

    #[test]
    fn old_sessions_with_view_lines_in_content_are_not_doubled() {
        let old = "moved it\n\n[view state A] a";
        let ops = vec![view("A", "a"), text(Role::User, old)];
        assert_eq!(user_text(&conversation(&ops)[0]), old);
    }

    #[test]
    fn image_blocks_are_sent_only_to_a_model_that_may_see_them() {
        let repo = std::env::temp_dir().join(format!("lh-replay-vision-{}", ulid::Ulid::new()));
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
        let staging = repo.join("staging");
        let image = crate::attachments::store_image(&staging, png, Some("dot.png")).unwrap();
        crate::attachments::commit(&repo, &staging, std::slice::from_ref(&image)).unwrap();
        let mut m = new_message(Role::User, "look".into());
        m.attachments = vec![image];
        let ops = vec![Op::Message(m)];
        let images = |vision: bool| {
            conversation_with(&ops, Some(&repo), vision, true)[0]
                .content
                .iter()
                .filter(|b| matches!(b, ContentBlock::Image { .. }))
                .count()
        };
        assert_eq!(images(true), 1);
        assert_eq!(images(false), 0);
        let blind = format!(
            "{:?}",
            conversation_with(&ops, Some(&repo), false, true)[0].content
        );
        assert!(
            blind.contains("[image attached] dot.png (not sent to this model)"),
            "{blind}"
        );
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn attached_files_are_offered_to_read_only_when_the_agent_has_file_tools() {
        let mut m = new_message(Role::User, "".into());
        m.attachments = vec![crate::sessions::Attachment {
            kind: "file".into(),
            path: "/home/me/notes.md".into(),
            media_type: None,
            bytes: Some(5),
            name: Some("notes.md".into()),
        }];
        let ops = vec![Op::Message(m)];
        // Practice (no file tools): the path line only.
        let practice = conversation_with(&ops, None, true, false);
        assert_eq!(
            user_text(&practice[0]),
            "[attached file] /home/me/notes.md, 5 B"
        );
        // The tutor and planner are told how to read it.
        let tutor = user_text(&conversation(&ops)[0]);
        assert!(
            tutor.ends_with("— read it with file_read or file_find"),
            "{tutor}"
        );
    }

    #[test]
    fn only_correct_check_verdicts_count_towards_a_node() {
        let ops = vec![
            call(
                "p",
                "question_ask",
                serde_json::json!({"role":"predict","nodeId":"n1"}),
            ),
            result("p", "question_ask", r#"{"verdict":"correct"}"#, false),
            call(
                "c",
                "question_ask",
                serde_json::json!({"role":"check","nodeId":"n1"}),
            ),
            result("c", "question_ask", r#"{"answer":"x"}"#, false),
            call(
                "v0",
                "verdict_record",
                serde_json::json!({"questionId":"c","verdict":"wrong"}),
            ),
            result("v0", "verdict_record", "ok", false),
        ];
        let s = replay_state(&ops);
        assert!(!s.node_passed("n1"));
        let mut more = ops.clone();
        more.push(call(
            "v1",
            "verdict_record",
            serde_json::json!({"questionId":"c","verdict":"correct"}),
        ));
        more.push(result("v1", "verdict_record", "refused", true));
        assert!(
            !replay_state(&more).node_passed("n1"),
            "refused verdicts don't count"
        );
        more.push(call(
            "v2",
            "verdict_record",
            serde_json::json!({"questionId":"c","verdict":"correct"}),
        ));
        more.push(result("v2", "verdict_record", "ok", false));
        assert!(replay_state(&more).node_passed("n1"));
    }

    #[test]
    fn compaction_replaces_covered_ops_and_never_splits_a_call() {
        let mut ops = vec![text(Role::User, "first")];
        for i in 0..10 {
            ops.push(text(Role::Assistant, &"x".repeat(400)));
            ops.push(call(&format!("c{i}"), "file_find", serde_json::json!({})));
            ops.push(result(&format!("c{i}"), "file_find", "r", false));
            ops.push(text(Role::User, &format!("learner {i}")));
        }
        let cut = compaction_cut(&ops, 300).unwrap();
        assert!(matches!(&ops[cut + 1], Op::Message(m) if m.role == Role::User));
        let covered = &ops[cut];
        ops.push(Op::Compaction(CompactionOp {
            op_id: op_id(),
            at: crate::now_iso(),
            summary: "NOTES".into(),
            cut_at: covered.at().to_owned(),
            cut_op_id: covered.op_id().to_owned(),
        }));
        let msgs = conversation(&ops);
        assert!(matches!(&msgs[0].content[0],
            ContentBlock::Text { text } if text.contains("NOTES")));
        assert!(
            estimate_tokens("", &msgs) < estimate_tokens("", &conversation(&ops[..ops.len() - 1]))
        );
        // Every result in the compacted conversation follows its call.
        let mut seen = HashSet::new();
        for m in &msgs {
            for b in &m.content {
                match b {
                    ContentBlock::ToolCall { id, .. } => {
                        seen.insert(id.clone());
                    }
                    ContentBlock::ToolResult { call_id, .. } => assert!(seen.contains(call_id)),
                    _ => {}
                }
            }
        }
    }

    /// One graded check: the question, the learner's answer with a
    /// confidence, the tutor's verdict.
    fn graded(
        id: &str,
        node: &str,
        level: Option<&str>,
        confidence: u32,
        verdict: &str,
    ) -> Vec<Op> {
        let mut args = serde_json::json!({ "role": "check", "nodeId": node });
        if let Some(level) = level {
            args["level"] = serde_json::json!(level);
        }
        vec![
            call(id, "question_ask", args),
            result(
                id,
                "question_ask",
                &format!(r#"{{"answer":"x","confidence":{confidence}}}"#),
                false,
            ),
            call(
                &format!("v-{id}"),
                "verdict_record",
                serde_json::json!({ "questionId": id, "verdict": verdict }),
            ),
            result(&format!("v-{id}"), "verdict_record", "ok", false),
        ]
    }

    #[test]
    fn node_levels_report_the_highest_level_with_a_passing_check() {
        let mut ops = Vec::new();
        ops.extend(graded("q1", "n1", Some("analyze"), 70, "wrong"));
        ops.extend(graded("q2", "n1", Some("explain"), 70, "correct"));
        ops.extend(graded("q3", "n1", Some("design"), 70, "wrong"));
        ops.extend(graded("q4", "n1", Some("analyze"), 70, "correct"));
        ops.extend(graded("q5", "n2", Some("use"), 70, "partial"));
        let s = replay_state(&ops);
        assert_eq!(s.node_level("n1"), Some(Level::Analyze));
        assert_eq!(s.node_level("n2"), None, "a partial pass is not a pass");
        assert_eq!(s.checks["n1"].len(), 4);
        assert_eq!(s.checks["n1"][1].level, Some(Level::Explain));
    }

    fn plan_ask(call_id: &str, question: &str) -> Op {
        Op::PlanAsk(PlanAskOp {
            op_id: op_id(),
            at: crate::now_iso(),
            call_id: call_id.into(),
            question: question.into(),
            options: vec!["Yes".into(), "No".into()],
        })
    }

    fn plan_answer(call_id: &str, answer: &str) -> Op {
        Op::PlanAnswer(PlanAnswerOp {
            op_id: op_id(),
            at: crate::now_iso(),
            call_id: call_id.into(),
            answer: answer.into(),
        })
    }

    #[test]
    fn a_plan_ask_stays_pending_until_its_answer() {
        assert_eq!(replay_state(&[]).pending_plan_question, None);

        let asked = vec![plan_ask("a1", "Which sections?")];
        let q = replay_state(&asked).pending_plan_question.unwrap();
        assert_eq!(q.call_id, "a1");
        assert_eq!(q.question, "Which sections?");
        assert_eq!(q.options, ["Yes", "No"]);

        // An answer for another call id leaves it pending.
        let mut other = asked.clone();
        other.push(plan_answer("a2", "Yes"));
        assert!(replay_state(&other).pending_plan_question.is_some());

        let mut answered = asked;
        answered.push(plan_answer("a1", "Just the first one"));
        assert_eq!(replay_state(&answered).pending_plan_question, None);

        // The latest open ask is the one shown.
        let mut twice = vec![plan_ask("a1", "First?"), plan_ask("a2", "Second?")];
        assert_eq!(
            replay_state(&twice).pending_plan_question.unwrap().question,
            "Second?"
        );
        twice.push(plan_answer("a2", "Ok"));
        assert_eq!(
            replay_state(&twice).pending_plan_question.unwrap().question,
            "First?"
        );
    }

    #[test]
    fn only_refuted_known_sections_are_listed() {
        assert!(replay_state(&[]).known_refuted.is_empty());
        let ops = vec![
            Op::KnownRefuted(KnownRefutedOp {
                op_id: op_id(),
                at: crate::now_iso(),
                section_id: "s1".into(),
                question_id: "q1".into(),
            }),
            // Seen twice (a second missed check): listed once.
            Op::KnownRefuted(KnownRefutedOp {
                op_id: op_id(),
                at: crate::now_iso(),
                section_id: "s1".into(),
                question_id: "q2".into(),
            }),
            Op::KnownRefuted(KnownRefutedOp {
                op_id: op_id(),
                at: crate::now_iso(),
                section_id: "s3".into(),
                question_id: "q3".into(),
            }),
        ];
        let s = replay_state(&ops);
        assert_eq!(s.known_refuted, ["s1", "s3"]);
        // An un-refuted section is not in the list.
        assert!(!s.known_refuted.iter().any(|id| id == "s2"));
    }

    #[test]
    fn older_logs_without_levels_derive_cleanly() {
        let mut ops = graded("q1", "n1", None, 70, "correct");
        ops.extend(graded("q2", "n1", None, 90, "correct"));
        let s = replay_state(&ops);
        assert!(s.node_passed("n1"));
        assert!(s.checks["n1"].iter().all(|c| c.level.is_none()));
        assert!(s.node_levels.is_empty());
        // Nothing to show: no section can pass "at" any level.
        assert!(!s.section_passed_at(
            &crate::state::Section {
                id: "s1".into(),
                title: "S".into(),
                node_ids: vec!["n1".into()],
                mode: crate::state::Kind::Mechanism,
                style: "socratic".into(),
                plugins: vec![],
                notes: String::new(),
                exit: String::new(),
                desktop_only: false,
                needs_install: vec![],
                target_level: Some(Level::Explain),
                assumed_known: false,
                known_note: String::new(),
                learner_note: String::new(),
                hunt: None,
                revision: false,
            },
            Level::Explain,
        ));
    }

    #[test]
    fn calibration_needs_twenty_graded_answers_with_a_confidence() {
        let mut ops = Vec::new();
        for i in 0..19 {
            ops.extend(graded(&format!("q{i}"), "n1", Some("use"), 70, "correct"));
        }
        assert!(replay_state(&ops).calibration.is_none());

        // One more graded answer reaches the threshold.
        ops.extend(graded("q19", "n1", Some("use"), 70, "correct"));
        let c = replay_state(&ops).calibration.unwrap();
        assert_eq!(c.graded, 20);
        assert_eq!(c.buckets.len(), 1);
        assert_eq!(c.buckets[0].label, "Fairly sure");
        assert_eq!(c.buckets[0].correct, 20);
        // Answers without a confidence don't count at all.
        assert!(
            c.notes[0].contains("results are ahead of your confidence"),
            "{:?}",
            c.notes
        );
    }

    #[test]
    fn calibration_reports_buckets_with_five_answers_and_names_only_wide_gaps() {
        let mut ops = Vec::new();
        // Fairly sure: 15 answers, 9 correct (60%): 10 points off, so silent.
        for i in 0..15 {
            let verdict = if i < 9 { "correct" } else { "wrong" };
            ops.extend(graded(&format!("a{i}"), "n1", Some("use"), 70, verdict));
        }
        // Certain: 5 answers, 2 correct (40%): 50 points off, named.
        for i in 0..5 {
            let verdict = if i < 2 { "correct" } else { "wrong" };
            ops.extend(graded(&format!("b{i}"), "n1", Some("use"), 90, verdict));
        }
        // Guessing: 3 answers, below the per-bucket threshold.
        for i in 0..3 {
            ops.extend(graded(&format!("c{i}"), "n1", Some("use"), 10, "wrong"));
        }
        // A probe answer with a confidence but no verdict never counts.
        ops.push(call(
            "p1",
            "question_ask",
            serde_json::json!({ "role": "probe", "level": "use" }),
        ));
        ops.push(result(
            "p1",
            "question_ask",
            r#"{"answer":"x","confidence":10}"#,
            false,
        ));

        let c = replay_state(&ops).calibration.unwrap();
        assert_eq!(c.graded, 23);
        let labels: Vec<&str> = c.buckets.iter().map(|b| b.label.as_str()).collect();
        assert_eq!(labels, ["Fairly sure", "Certain"]);
        assert_eq!(c.buckets[0].correct, 9);
        assert_eq!(c.buckets[0].total, 15);
        assert_eq!(c.buckets[1].correct, 2);
        assert_eq!(c.buckets[1].total, 5);
        assert_eq!(
            c.notes,
            ["When you picked Certain, you were right 2 of 5 times (40%): your confidence is ahead of your results."]
        );

        // Within 15 points everywhere: one neutral sentence.
        let mut calm = Vec::new();
        for i in 0..20 {
            let verdict = if i < 9 { "correct" } else { "wrong" };
            calm.extend(graded(&format!("d{i}"), "n1", Some("use"), 50, verdict));
        }
        let c = replay_state(&calm).calibration.unwrap();
        assert_eq!(c.notes, ["Confidence matches results."]);
        assert_eq!(c.buckets[0].correct, 9);
        assert_eq!(c.buckets[0].total, 20);
    }

    /// §7 counts the confidence the learner gives on **every** graded answer:
    /// a probe answer has no node, but its verdict and confidence reach the
    /// buckets. "I don't know" carries no confidence, so it never does.
    #[test]
    fn a_probe_answer_counts_towards_calibration() {
        let mut ops = Vec::new();
        for i in 0..19 {
            ops.extend(graded(&format!("q{i}"), "n1", Some("use"), 70, "wrong"));
        }
        assert!(replay_state(&ops).calibration.is_none());

        // One graded probe answer with a confidence reaches the threshold.
        ops.push(call(
            "p1",
            "question_ask",
            serde_json::json!({ "role": "probe", "level": "use" }),
        ));
        ops.push(result(
            "p1",
            "question_ask",
            r#"{"answer":"x","confidence":70}"#,
            false,
        ));
        ops.push(call(
            "vp1",
            "verdict_record",
            serde_json::json!({ "questionId": "p1", "verdict": "correct" }),
        ));
        ops.push(result("vp1", "verdict_record", "ok", false));

        let s = replay_state(&ops);
        let c = s.calibration.clone().unwrap();
        assert_eq!(c.graded, 20);
        assert_eq!(c.buckets.len(), 1);
        assert_eq!(c.buckets[0].total, 20);
        assert_eq!(c.buckets[0].correct, 1, "the probe's verdict counts");
        // It is not a node check: nothing lands in a node's exit evidence.
        assert_eq!(
            s.checks.keys().map(String::as_str).collect::<Vec<_>>(),
            ["n1"]
        );
        assert_eq!(s.checks["n1"].len(), 19);
        assert!(s.node_levels.is_empty(), "a probe pass sets no node level");
        assert!(!s.node_passed(""), "a probe is never a node");

        // "I don't know" carries no confidence: its verdict adds no answer.
        let mut dontknow = ops;
        dontknow.push(call(
            "p2",
            "question_ask",
            serde_json::json!({ "role": "probe", "level": "use" }),
        ));
        dontknow.push(result(
            "p2",
            "question_ask",
            r#"{"answer":"","dontKnow":true}"#,
            false,
        ));
        dontknow.push(call(
            "vp2",
            "verdict_record",
            serde_json::json!({ "questionId": "p2", "verdict": "wrong" }),
        ));
        dontknow.push(result("vp2", "verdict_record", "ok", false));
        let c = replay_state(&dontknow).calibration.unwrap();
        assert_eq!(
            c.graded, 20,
            "a don't-know answer contributes no confidence"
        );
        assert_eq!(c.buckets[0].total, 20);
    }

    // ---- hunts (§5) --------------------------------------------------------

    fn hunt_section(id: &str, available: bool) -> crate::state::Section {
        crate::state::Section {
            id: id.into(),
            title: format!("Section {id}"),
            node_ids: vec!["n1".into()],
            mode: crate::state::Kind::Mechanism,
            style: "socratic".into(),
            plugins: vec![],
            notes: String::new(),
            exit: String::new(),
            desktop_only: false,
            needs_install: vec![],
            target_level: Some(Level::Use),
            assumed_known: false,
            known_note: String::new(),
            learner_note: String::new(),
            hunt: available.then(|| crate::state::Hunt {
                available: true,
                reason: "practitioner lore".into(),
                enabled: false,
            }),
            revision: false,
        }
    }

    fn plan_with_sections(sections: Vec<crate::state::Section>) -> Op {
        Op::Plan(crate::sessions::record(serde_json::json!({
            "goal": "Dividers",
            "approach": "One route.",
            "nodes": [
                { "id": "n1", "title": "The divider rule", "kind": "mechanism" },
                { "id": "n2", "title": "Series current", "kind": "mechanism" }
            ],
            "sections": sections.iter().map(|s| serde_json::json!({
                "id": s.id,
                "title": s.title,
                "nodeIds": s.node_ids,
                "mode": "mechanism",
                "style": s.style,
                "plugins": [],
                "notes": "",
                "exit": s.exit,
                "targetLevel": "use",
                "hunt": s.hunt,
            })).collect::<Vec<_>>(),
        })))
    }

    fn in_section(section: &str) -> Op {
        crate::agent::phase_op(crate::state::Phase::Teach, Some(section.into()))
    }

    fn hunt_sent(hunt_id: &str, section: &str) -> Op {
        Op::HuntSend(crate::sessions::HuntSendOp {
            op_id: op_id(),
            at: crate::now_iso(),
            hunt_id: hunt_id.into(),
            section_id: section.into(),
            question: "Why negative feedback?".into(),
            look_at: serde_json::json!([{ "kind": "forum", "why": "arguments" }]),
            bring_back: vec!["claims".into()],
        })
    }

    fn hunt_toggled(section: &str, enabled: bool) -> Op {
        Op::HuntToggle(crate::sessions::HuntToggleOp {
            op_id: op_id(),
            at: crate::now_iso(),
            section_id: section.into(),
            enabled,
        })
    }

    /// A log with the plan, the current section and the learner's toggle, so
    /// each test only adds the hunt ops it is about.
    fn hunt_log(sections: Vec<&str>, current: &str, enable: bool) -> Vec<Op> {
        let mut ops = vec![
            plan_with_sections(sections.iter().map(|id| hunt_section(id, true)).collect()),
            in_section(current),
        ];
        if enable {
            ops.push(hunt_toggled(current, true));
        }
        ops
    }

    #[test]
    fn a_hunt_moves_ready_out_returned_and_debriefed() {
        let base = hunt_log(vec!["s1"], "s1", true);
        let state_of = |ops: &[Op]| replay_state(ops);

        // The learner switched it on; nothing is sent yet, and the section
        // records that: `ready`.
        let s = state_of(&base);
        assert!(s.hunt_active());
        assert_eq!(s.hunt_state(), "ready");
        assert_eq!(s.hunt, None, "no hunt record before the tutor sends one");
        assert!(s.hunt_blocks_teaching(), "teaching waits for the hunt");

        // The sent call without its result: the learner hasn't been told.
        let mut ops = base.clone();
        ops.push(hunt_sent("h1", "s1"));
        let s = state_of(&ops);
        assert_eq!(s.hunt_state(), "ready");
        assert_eq!(s.hunt.as_ref().unwrap().hunt_id, "h1");
        assert_eq!(s.hunt.as_ref().unwrap().question, "Why negative feedback?");

        // The result lands in the same turn: the card is on screen, `out`.
        ops.push(result("h1", "hunt_send", "Hunt sent.", false));
        let s = state_of(&ops);
        assert_eq!(s.hunt_state(), "out");
        assert!(!s.hunt.as_ref().unwrap().debriefed);
        assert!(s.hunt_blocks_teaching());

        // The learner comes back with notes and a link.
        ops.push(Op::HuntReturn(crate::sessions::HuntReturnOp {
            op_id: op_id(),
            at: crate::now_iso(),
            hunt_id: "h1".into(),
            notes: "the forum says it's about linearity".into(),
            links: vec!["https://forum.example/x".into()],
            files: vec![],
        }));
        let s = state_of(&ops);
        let hunt = s.hunt.as_ref().unwrap();
        assert_eq!(hunt.state, "returned");
        assert_eq!(
            hunt.returned.as_ref().unwrap()["notes"],
            "the forum says it's about linearity"
        );
        assert_eq!(
            hunt.returned.as_ref().unwrap()["links"][0],
            "https://forum.example/x"
        );
        assert!(!hunt.debriefed);
        assert!(s.hunt_blocks_teaching(), "the debrief comes first");

        // The debrief: recorded, and teaching may start.
        ops.push(Op::Debrief(crate::sessions::DebriefOp {
            op_id: op_id(),
            at: crate::now_iso(),
            hunt_id: "h1".into(),
            claims: serde_json::json!([{ "claim": "x", "verdict": "correct", "note": "yes" }]),
        }));
        let s = state_of(&ops);
        assert_eq!(s.hunt.as_ref().unwrap().state, "returned");
        assert!(s.hunt.as_ref().unwrap().debriefed);
        assert!(!s.hunt_blocks_teaching(), "the debrief unblocks teaching");
        assert!(s.hunt_active(), "the hunt still frames the section");
    }

    #[test]
    fn a_skip_switches_the_hunt_off_and_unblocks_teaching() {
        let mut ops = hunt_log(vec!["s1"], "s1", true);
        ops.push(hunt_sent("h1", "s1"));
        ops.push(result("h1", "hunt_send", "Hunt sent.", false));
        ops.push(Op::HuntSkip(crate::sessions::HuntSkipOp {
            op_id: op_id(),
            at: crate::now_iso(),
            hunt_id: "h1".into(),
        }));
        let s = replay_state(&ops);
        assert_eq!(s.hunt.as_ref().unwrap().state, "skipped");
        assert!(!s.hunt.as_ref().unwrap().debriefed);
        assert!(
            !s.hunt_active(),
            "a skip switches the hunt off whatever the toggle said"
        );
        assert!(
            !s.hunt_blocks_teaching(),
            "skipping costs the section nothing"
        );
        // The plan section carries the derived value, so gates and prompts
        // read one place.
        assert!(
            !s.plan.as_ref().unwrap().sections[0]
                .hunt
                .as_ref()
                .unwrap()
                .enabled
        );
    }

    #[test]
    fn toggles_are_last_write_wins_per_section() {
        let mut ops = hunt_log(vec!["s1"], "s1", false);
        assert!(
            !replay_state(&ops).hunt_active(),
            "plan_write can't enable it"
        );
        ops.push(hunt_toggled("s1", true));
        assert!(replay_state(&ops).hunt_active());
        ops.push(hunt_toggled("s1", false));
        assert!(!replay_state(&ops).hunt_active());
        assert_eq!(replay_state(&ops).hunt_state(), "", "nothing to show");
        ops.push(hunt_toggled("s1", true));
        // Another section's toggle never enables this one.
        ops.push(hunt_toggled("s2", false));
        assert!(replay_state(&ops).hunt_active());
    }

    #[test]
    fn the_current_sections_hunt_wins_over_an_earlier_one() {
        let mut ops = hunt_log(vec!["s1", "s2"], "s1", true);
        ops.push(hunt_sent("h1", "s1"));
        ops.push(result("h1", "hunt_send", "Hunt sent.", false));
        ops.push(Op::HuntReturn(crate::sessions::HuntReturnOp {
            op_id: op_id(),
            at: crate::now_iso(),
            hunt_id: "h1".into(),
            notes: "found it".into(),
            links: vec![],
            files: vec![],
        }));
        let first = replay_state(&ops).hunt.unwrap();
        assert_eq!(
            (first.hunt_id.as_str(), first.state.as_str()),
            ("h1", "returned")
        );

        // The second section's hunt is the one the learner is looking at.
        ops.push(in_section("s2"));
        ops.push(hunt_toggled("s2", true));
        assert_eq!(
            replay_state(&ops).hunt,
            None,
            "a new section starts with no hunt record"
        );
        assert_eq!(replay_state(&ops).hunt_state(), "ready");
        ops.push(hunt_sent("h2", "s2"));
        ops.push(result("h2", "hunt_send", "Hunt sent.", false));
        let second = replay_state(&ops).hunt.unwrap();
        assert_eq!(
            (second.hunt_id.as_str(), second.section_id.as_str()),
            ("h2", "s2")
        );
        assert_eq!(second.state, "out");
        // Going back to the first section shows its own, earlier record: the
        // log is the whole truth, section by section.
        ops.push(in_section("s1"));
        assert_eq!(replay_state(&ops).hunt.unwrap().hunt_id, "h1");
    }

    /// Every hunt keeps its own lifecycle across sections (§5): the flow
    /// resolves a card by `huntId`, so an earlier section's card shows its own
    /// finished state and can't pass itself off as live.
    #[test]
    fn every_hunt_keeps_its_own_state_across_sections() {
        let mut ops = hunt_log(vec!["s1", "s2"], "s1", true);
        ops.push(hunt_sent("h1", "s1"));
        ops.push(result("h1", "hunt_send", "Hunt sent.", false));
        let s = replay_state(&ops);
        assert_eq!(s.hunts.len(), 1);
        assert_eq!(s.hunts[0].state, "out");
        assert_eq!(s.hunt.as_ref().unwrap().hunt_id, "h1");

        // The learner skips it, the section closes, and section 2's hunt
        // starts: the first record does not follow.
        ops.push(Op::HuntSkip(crate::sessions::HuntSkipOp {
            op_id: op_id(),
            at: crate::now_iso(),
            hunt_id: "h1".into(),
        }));
        ops.push(in_section("s2"));
        ops.push(hunt_toggled("s2", true));
        ops.push(hunt_sent("h2", "s2"));
        ops.push(result("h2", "hunt_send", "Hunt sent.", false));

        let s = replay_state(&ops);
        let states: Vec<(&str, &str, &str)> = s
            .hunts
            .iter()
            .map(|h| (h.hunt_id.as_str(), h.section_id.as_str(), h.state.as_str()))
            .collect();
        assert_eq!(
            states,
            [("h1", "s1", "skipped"), ("h2", "s2", "out")],
            "every hunt keeps its own state, in op order"
        );
        assert_eq!(
            s.hunt.as_ref().unwrap().hunt_id,
            "h2",
            "the current section's hunt is `hunt`"
        );
        assert_eq!(s.hunt_state(), "out");
        // Looking a card up by its id finds section 1's finished hunt, not
        // the live one.
        let card = s.hunts.iter().find(|h| h.hunt_id == "h1").unwrap();
        assert_eq!(card.state, "skipped");
        // Only section 2's live hunt blocks teaching.
        assert!(s.hunt_blocks_teaching());
        // Going back to section 1 shows its own record again.
        ops.push(in_section("s1"));
        assert_eq!(replay_state(&ops).hunt.unwrap().hunt_id, "h1");
    }

    #[test]
    fn older_logs_without_a_hunt_derive_no_hunt_at_all() {
        let ops = vec![
            plan_with_sections(vec![hunt_section("s1", false)]),
            in_section("s1"),
        ];
        let s = replay_state(&ops);
        assert_eq!(s.hunt, None);
        assert_eq!(s.hunt_state(), "");
        assert!(!s.hunt_active());
        assert!(!s.hunt_blocks_teaching());
        // A plan written before hunts existed has no such key at all.
        let old = Op::Plan(crate::sessions::record(serde_json::json!({
            "goal": "Dividers", "approach": "a",
            "nodes": [{ "id": "n1", "title": "One", "kind": "mechanism" }],
            "sections": [{ "id": "s1", "title": "S", "nodeIds": ["n1"],
                "mode": "mechanism", "style": "socratic", "plugins": [],
                "notes": "", "exit": "" }]
        })));
        let s = replay_state(&[old, in_section("s1")]);
        assert!(s.plan.as_ref().unwrap().sections[0].hunt.is_none());
        assert_eq!(s.hunt, None);
        // Without a plan there is no section to hunt in, so the state line
        // stays silent even though the log names a hunt.
        let ops = hunt_log(vec!["s1"], "s1", false)
            .into_iter()
            .filter(|o| !matches!(o, Op::Plan(_)))
            .collect::<Vec<_>>();
        assert_eq!(replay_state(&ops).hunt_state(), "");
    }
}
