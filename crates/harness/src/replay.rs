//! Derived state and model messages, both rebuilt from the op log (build
//! prompt M4: "never store derived state").
//!
//! [`replay_state`] folds ops into a [`SessionState`]. [`conversation`] turns
//! the conversation ops (learner/tutor messages, the tutor's tool calls and
//! results, delivered job results) into provider messages, applying the latest
//! compaction. The planner's own tool loop is not part of the conversation;
//! its effects reach the tutor as `plan` ops and `[plan update]` messages.

use std::collections::{HashMap, HashSet};

use providers::{ContentBlock, Message, Role};
use serde_json::Value;

use crate::sessions::{Op, ToolOp};
use crate::state::{CheckVerdict, Grant, OpenJob, PendingCall, Plan, QueuedResult, SessionState};

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
                                if let Some(verdict) = v.get("verdict").and_then(Value::as_str) {
                                    record_verdict(&mut s, &calls, &t.call_id, verdict);
                                }
                            }
                        }
                        "verdict_record" => {
                            let qid = args.get("questionId").and_then(Value::as_str);
                            let verdict = args.get("verdict").and_then(Value::as_str);
                            if let (Some(qid), Some(verdict)) = (qid, verdict) {
                                record_verdict(&mut s, &calls, qid, verdict);
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
                    "done" | "failed" if !j.blocking => {
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
            Op::PlanRequest(_) | Op::PlanKeep(_) | Op::Compaction(_) | Op::Session(_) => {}
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
    s.active_plugins = active_plugins(&s);
    s
}

fn record_verdict(
    s: &mut SessionState,
    calls: &HashMap<String, (String, Value)>,
    question_id: &str,
    verdict: &str,
) {
    let Some((name, args)) = calls.get(question_id) else {
        return;
    };
    // Only check questions count towards a section's exit.
    if name != "question_ask" || args.get("role").and_then(Value::as_str) != Some("check") {
        return;
    }
    let Some(node_id) = args.get("nodeId").and_then(Value::as_str) else {
        return;
    };
    s.checks
        .entry(node_id.to_owned())
        .or_default()
        .push(CheckVerdict {
            node_id: node_id.to_owned(),
            question_id: question_id.to_owned(),
            verdict: verdict.to_owned(),
        });
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
pub(crate) fn conversation(ops: &[Op]) -> Vec<Message> {
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
            Op::Message(m) => {
                if !m.content.trim().is_empty() {
                    b.user_text(m.content.clone());
                }
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

/// Plain-text transcript of the conversation ops in `ops` for the compaction
/// prompt's `{{turns_to_summarize}}`.
pub(crate) fn transcript(ops: &[Op]) -> String {
    const ITEM_CAP: usize = 4000;
    let mut out = String::new();
    for op in ops {
        let line = match op {
            Op::Message(m) if m.content.trim().is_empty() => continue,
            Op::Message(m) => {
                let who = match (m.role, m.tag.as_deref()) {
                    (Role::Assistant, _) => "Tutor",
                    (Role::User, Some(_)) => "App",
                    (Role::User, None) => "Learner",
                };
                format!("{who}: {}", clip(&m.content, ITEM_CAP))
            }
            Op::Tool(t) if is_conversation_tool(t) => match &t.result {
                None => format!(
                    "[tutor called {} {}]",
                    t.name,
                    clip(&t.args.to_string(), ITEM_CAP)
                ),
                Some(r) => format!(
                    "[{} result{}: {}]",
                    t.name,
                    if r.is_error { " (error)" } else { "" },
                    clip(&r.content, ITEM_CAP)
                ),
            },
            _ => continue,
        };
        out.push_str(&line);
        out.push_str("\n\n");
    }
    out
}

fn clip(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::{new_message, op_id, CompactionOp, ToolResultValue};

    fn call(id: &str, name: &str, args: Value) -> Op {
        Op::Tool(ToolOp {
            op_id: op_id(),
            at: crate::now_iso(),
            call_id: id.into(),
            name: name.into(),
            args,
            result: None,
            agent: None,
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
}
