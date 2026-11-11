//! Ports one LearnHarness (AGPL-3.0) chat file into a session op log under
//! `<repo>/sessions/`, and rebuilds its **plan from this harness's knowledge
//! graph** instead of from LearnHarness's topic list.
//!
//! It reads only the learner's own files; no LearnHarness code is copied.
//!
//! Usage:
//! `cargo run -p harness --bin port_learnharness_session -- <chat.json> <repo-dir>`
//!
//! ## What is transported, what is re-derived
//!
//! Transported verbatim (the learner's real history):
//! - every learner message and every tutor text block, in order;
//! - every graded question: prompt, options, explanation, the tutor's
//!   committed answer, and the learner's answer with reasoning + confidence +
//!   correctness → our `question_ask` call/result pair. LearnHarness's
//!   `probe`/`quiz` role and format are kept (`probe`, `predict`, `check`;
//!   choice vs free), so the cards read the same as they did there;
//! - the free-form `ask_user_question` exchanges: the tutor's question and the
//!   learner's answer are ordinary conversation, so they become messages;
//! - compaction markers → `compaction` ops (the transcript keeps its break);
//! - the last plan-block's node statuses → section progress (`visited` →
//!   `section_done`, in the order teaching reached each topic).
//!
//! Re-derived here (the reason this bin exists):
//! - **Plan nodes are knowledge propositions**, not session topics: one node
//!   per graph node that the plan's topics own, `requires` taken from the
//!   graph's `requires` edges (the `graph/ops.jsonl` fold; a `contrasts` edge
//!   orders nothing and is not a dependency). The plan is therefore a
//!   *view of the knowledge graph*, and its diagram shows cross-topic
//!   prerequisite edges that a topic-level tree cannot have.
//! - **Plan sections are knowledge topics** (which have propositions, so a
//!   leaf with no recorded claim is not a section) ordered by the topic
//!   dependency the edges imply, tie-broken by the source plan's order.
//! - `goal` is the learner's own first sentence; `approach` is the tutor's
//!   plan paragraph; both are excerpts of the source, never new prose.
//! - Section `exit` is left empty: the source states no per-topic exit
//!   criterion. **[inference]** — the plan's shape is knowledge-based, the
//!   learner never authored this exact document.
//!
//! Teaching progress per *proposition* is deliberately not invented: the
//! source's questions are tagged at topic level, so evidence stays at topic
//! level (see `graph/checks.jsonl`). Only section-level progress is recorded.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use harness::state::{Kind, Plan, PlanNode, Section};
use harness::{
    ApproveOp, CompactionOp, Header, KnowledgeMap, KnowledgeNode, Mode, Op, PhaseOp, RecordOp,
    StoredMessage, ToolOp, ToolResultValue,
};
use providers::Role;
use serde_json::{json, Value};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let chat_path = PathBuf::from(args.next().ok_or("usage: <chat.json> <repo-dir>")?);
    let repo = PathBuf::from(args.next().ok_or("usage: <chat.json> <repo-dir>")?);

    let chat: Value = serde_json::from_str(&std::fs::read_to_string(&chat_path)?)?;
    let graph = harness::knowledge::load(&repo)?;
    if graph.nodes.is_empty() {
        return Err(format!(
            "{} has no knowledge graph yet: run import_learnharness first",
            repo.display()
        )
        .into());
    }
    let plan = build_plan(&chat, &graph);

    let session_id = session_id(&chat)?;
    let at = chat["createdAt"].as_str().unwrap_or_default().to_owned();
    let path = session_path(&repo, &session_id, &at)?;

    let progress = source_progress(&chat);
    let port = Port {
        plan: &plan,
        progress: &progress,
    };
    let ops = port.ops(&chat, &session_id)?;

    std::fs::create_dir_all(path.parent().expect("session dir"))?;
    let mut text = String::new();
    for op in &ops {
        text.push_str(&serde_json::to_string(op)?);
        text.push('\n');
    }
    std::fs::write(&path, text)?;
    println!(
        "wrote {} ops ({} messages, {} questions, {} sections, {} nodes) to {}",
        ops.len(),
        ops.iter().filter(|o| matches!(o, Op::Message(_))).count(),
        ops.iter().filter(|o| matches!(o, Op::Tool(_))).count() / 2,
        plan.sections.len(),
        plan.nodes.len(),
        path.display()
    );
    Ok(())
}

// ---------------------------------------------------------------- plan ----

/// Where each topic-node of the source plan stood when the chat was exported.
#[derive(Default)]
struct Progress {
    /// Topic ids the source marked `visited`, in the order teaching reached
    /// them (source plan order as a tie-break).
    visited: Vec<String>,
    /// Topic order of the source plan (its first `plan-block`).
    order: Vec<String>,
}

fn source_progress(chat: &Value) -> Progress {
    let messages = chat["messages"].as_array().cloned().unwrap_or_default();
    let blocks: Vec<&Value> = messages
        .iter()
        .filter(|m| m["customType"] == "plan-block")
        .map(|m| &m["details"]["block"])
        .collect();
    let mut p = Progress::default();
    let Some(last) = blocks.last() else { return p };
    let nodes = last["nodes"].as_array().cloned().unwrap_or_default();
    for n in &nodes {
        if let Some(id) = n["id"].as_str() {
            if p.order.is_empty() || !p.order.contains(&id.to_owned()) {
                p.order.push(id.to_owned());
            }
        }
    }
    for n in &nodes {
        if n["status"] == "visited" {
            if let Some(id) = n["id"].as_str() {
                p.visited.push(id.to_owned());
            }
        }
    }
    p
}

/// The plan: knowledge propositions as nodes, knowledge topics as sections.
fn build_plan(chat: &Value, graph: &KnowledgeMap) -> Plan {
    let progress = source_progress(chat);

    // Sections: topics that own at least one proposition, in the dependency
    // order the knowledge edges imply (a section precedes the sections its
    // propositions lead to), tie-broken by the source plan's own order.
    let mut nodes_by_home: BTreeMap<&str, Vec<&KnowledgeNode>> = BTreeMap::new();
    for n in &graph.nodes {
        nodes_by_home.entry(n.home.as_str()).or_default().push(n);
    }
    let topic_ids: HashSet<&str> = graph.topics.iter().map(|t| t.id.as_str()).collect();
    let section_ids: Vec<&str> = graph
        .topics
        .iter()
        .filter(|t| nodes_by_home.contains_key(t.id.as_str()))
        .map(|t| t.id.as_str())
        .collect();
    let rank = |id: &str| {
        progress
            .order
            .iter()
            .position(|o| o == id)
            .unwrap_or(usize::MAX)
    };
    let ordered = topo_sections(&section_ids, graph, &rank);

    let mut sections = Vec::new();
    for id in &ordered {
        if !topic_ids.contains(id.as_str()) {
            continue;
        }
        let title = graph
            .topics
            .iter()
            .find(|t| t.id == *id)
            .map(|t| t.title.clone())
            .unwrap_or_else(|| (*id).to_owned());
        let home_nodes = &nodes_by_home[id.as_str()];
        let mode = dominant_kind(home_nodes);
        sections.push(Section {
            id: (*id).to_owned(),
            title,
            node_ids: home_nodes.iter().map(|n| n.id.clone()).collect(),
            mode,
            style: "mixed".to_owned(),
            plugins: Vec::new(),
            notes: String::new(),
            exit: String::new(),
            desktop_only: false,
            needs_install: Vec::new(),
            target_level: None,
            assumed_known: false,
            known_note: String::new(),
            learner_note: String::new(),
            hunt: None,
            revision: false,
        });
    }

    // Nodes: the propositions those sections hold; `requires` = the knowledge
    // edges inside that set (`from` is a prerequisite of `to`).
    let plan_ids: HashSet<&str> = sections
        .iter()
        .flat_map(|s| s.node_ids.iter().map(String::as_str))
        .collect();
    let mut requires: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for e in graph.prerequisites() {
        if plan_ids.contains(e.from.as_str()) && plan_ids.contains(e.to.as_str()) {
            requires
                .entry(e.to.as_str())
                .or_default()
                .push(e.from.clone());
        }
    }
    let mut nodes = Vec::new();
    for s in &sections {
        for id in &s.node_ids {
            let Some(n) = graph.nodes.iter().find(|n| &n.id == id) else {
                continue;
            };
            nodes.push(PlanNode {
                id: n.id.clone(),
                title: n.title.clone(),
                kind: n.kind,
                requires: requires.get(n.id.as_str()).cloned().unwrap_or_default(),
                graph_node: Some(n.id.clone()),
                weight: None,
                practice_only: false,
            });
        }
    }

    Plan {
        version: 0,
        goal: goal_text(chat),
        approach: approach_text(chat),
        depth: String::new(),
        explore: String::new(),
        minutes_per_sitting: None,
        style: String::new(),
        nodes,
        sections,
        tracks: Vec::new(),
    }
}

/// Kahn over the topic graph induced by the prerequisite (`requires`) edges,
/// tie-broken by the source plan's order then by id, so the result is
/// deterministic.
fn topo_sections(ids: &[&str], graph: &KnowledgeMap, rank: &impl Fn(&str) -> usize) -> Vec<String> {
    let known: HashSet<&str> = ids.iter().copied().collect();
    let home_of: HashMap<&str, &str> = graph
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n.home.as_str()))
        .collect();
    let mut deps: HashMap<&str, HashSet<&str>> =
        ids.iter().map(|id| (*id, HashSet::new())).collect();
    for e in graph.prerequisites() {
        let (Some(from), Some(to)) = (home_of.get(e.from.as_str()), home_of.get(e.to.as_str()))
        else {
            continue;
        };
        if from != to && known.contains(from) && known.contains(to) {
            deps.entry(to).or_default().insert(from);
        }
    }
    let mut out: Vec<String> = Vec::new();
    let mut left: HashSet<&str> = known.clone();
    while !left.is_empty() {
        let mut ready: Vec<&str> = left
            .iter()
            .copied()
            .filter(|id| deps[id].iter().all(|d| !left.contains(d)))
            .collect();
        if ready.is_empty() {
            // A cycle of topics (possible only if the edges disagree with the
            // grouping): keep the remainder in source order rather than fail.
            ready = left.iter().copied().collect();
        }
        ready.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.cmp(b)));
        let next = ready[0];
        left.remove(next);
        out.push(next.to_owned());
    }
    out
}

fn dominant_kind(nodes: &[&KnowledgeNode]) -> Kind {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for n in nodes {
        *counts.entry(n.kind.as_str()).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by_key(|(k, c)| (*c, std::cmp::Reverse(*k)))
        .and_then(|(k, _)| Kind::parse(k))
        .unwrap_or(Kind::Mechanism)
}

/// The learner's own first sentence — the goal they stated.
fn goal_text(chat: &Value) -> String {
    let messages = chat["messages"].as_array().cloned().unwrap_or_default();
    let first = messages.iter().find(|m| m["role"] == "user");
    let text = first
        .and_then(|m| m["content"].as_str())
        .unwrap_or_default()
        .trim()
        .to_owned();
    if text.is_empty() {
        return String::new();
    }
    first_sentence(&text)
}

fn first_sentence(text: &str) -> String {
    let flat = text.replace('\n', " ");
    let mut out = String::new();
    for (i, ch) in flat.char_indices() {
        out.push(ch);
        if matches!(ch, '.' | '!' | '?') && i + 1 < flat.len() && flat[i + 1..].starts_with(' ') {
            break;
        }
        if out.chars().count() >= 240 {
            break;
        }
    }
    out.trim().to_owned()
}

/// The tutor's own plan paragraph: the text block that explains the plan.
fn approach_text(chat: &Value) -> String {
    let messages = chat["messages"].as_array().cloned().unwrap_or_default();
    let mut found: Option<String> = None;
    for m in &messages {
        if m["role"] != "assistant" {
            continue;
        }
        for part in m["content"].as_array().cloned().unwrap_or_default() {
            if part["type"] != "text" {
                continue;
            }
            let Some(t) = part["text"].as_str() else {
                continue;
            };
            if t.contains("reasoning path")
                || t.contains("What I’ll build from")
                || t.contains("What I'll build from")
            {
                found = Some(t.to_owned());
            }
        }
    }
    let Some(text) = found else {
        return String::new();
    };
    // Excerpt: whole paragraphs up to ~900 characters, never mid-sentence.
    // Markdown headings are dropped — the card supplies its own structure.
    let mut out = String::new();
    for para in text.split("\n\n") {
        let mut para = para.trim();
        while let Some(rest) = para.strip_prefix('#') {
            // "## Heading\nbody" → keep the body only.
            para = rest.trim_start_matches('#').trim_start();
            if let Some((_, body)) = para.split_once('\n') {
                para = body.trim();
            } else {
                para = "";
                break;
            }
        }
        if para.is_empty() {
            continue;
        }
        if !out.is_empty() && out.chars().count() + para.chars().count() > 900 {
            break;
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(para);
        if out.chars().count() > 900 {
            break;
        }
    }
    out
}

// ------------------------------------------------------------- session ----

struct Port<'a> {
    plan: &'a Plan,
    progress: &'a Progress,
}

/// Where the walk through the source has got to: the open section, the sections
/// already closed, and whether the plan/approval ops were written.
#[derive(Default)]
struct Walk {
    current: Option<String>,
    done: HashSet<String>,
    plan_written: bool,
    approved: bool,
}

impl Port<'_> {
    fn ops(&self, chat: &Value, session_id: &str) -> Result<Vec<Op>, Box<dyn std::error::Error>> {
        let at = chat["createdAt"].as_str().unwrap_or_default().to_owned();
        let mut w = Writer::new();
        let plan_payload = serde_json::to_value(self.plan)?;
        let title = chat["title"]
            .as_str()
            .unwrap_or("Imported session")
            .to_owned();

        w.emit(&at, |op_id, at| {
            Op::Session(Header {
                op_id,
                at,
                id: session_id.to_owned(),
                mode: Mode::Learn,
                title,
                fork: None,
                merge: None,
            })
        });
        w.emit(&at, |op_id, at| {
            Op::Phase(PhaseOp {
                op_id,
                at,
                phase: harness::state::Phase::Measure,
                section_id: None,
            })
        });

        let messages = chat["messages"].as_array().cloned().unwrap_or_default();
        // toolCallId -> our question args, so a result can complete the pair.
        let mut asked: HashMap<String, Value> = HashMap::new();
        let mut walk = Walk::default();

        for m in &messages {
            let when = msg_at(m, &at);
            match m["role"].as_str().unwrap_or_default() {
                "user" => {
                    let text = m["content"].as_str().unwrap_or_default().trim().to_owned();
                    if text.is_empty() {
                        continue;
                    }
                    w.emit(&when, |op_id, at| {
                        Op::Message(StoredMessage {
                            op_id,
                            at,
                            role: Role::User,
                            content: text,
                            ..blank_message()
                        })
                    });
                }
                "assistant" => {
                    let mut text = String::new();
                    for part in m["content"].as_array().cloned().unwrap_or_default() {
                        match part["type"].as_str().unwrap_or_default() {
                            "text" => {
                                let t = part["text"].as_str().unwrap_or_default().trim();
                                if t.is_empty() {
                                    continue;
                                }
                                if !text.is_empty() {
                                    text.push_str("\n\n");
                                }
                                text.push_str(t);
                            }
                            "toolCall" => {
                                let name = part["name"].as_str().unwrap_or_default();
                                let call_id = part["id"].as_str().unwrap_or_default().to_owned();
                                let args = part["arguments"].clone();
                                match name {
                                    "probe" | "quiz" => {
                                        let (role, auto) = question_role(name, &args);
                                        asked.insert(
                                            call_id.clone(),
                                            json!({ "role": role, "auto": auto }),
                                        );
                                        self.flush_text(&mut w, &when, &mut text, m);
                                        w.emit(&when, |op_id, at| {
                                            Op::Tool(ToolOp {
                                                op_id,
                                                at,
                                                call_id: call_id.clone(),
                                                name: "question_ask".to_owned(),
                                                args: question_args(role, &args),
                                                result: None,
                                                agent: None,
                                                plugin: None,
                                                platforms: None,
                                            })
                                        });
                                        // Teaching position: the source tags each
                                        // teach question with its topic. Opening a
                                        // new section here is what the plan panel and
                                        // the section openers follow.
                                        if let Some(topic) = args["nodeId"].as_str() {
                                            self.enter_section(&mut w, &when, topic, &mut walk);
                                        }
                                    }
                                    "ask_user_question" => {
                                        // A free-form exchange: the tutor's question and
                                        // the learner's answer are conversation, so they
                                        // become messages rather than a tool card.
                                        self.flush_text(&mut w, &when, &mut text, m);
                                        let q =
                                            args["question"].as_str().unwrap_or_default().trim();
                                        if !q.is_empty() {
                                            let mut body = q.to_owned();
                                            if let Some(d) = args["details"].as_str().map(str::trim)
                                            {
                                                if !d.is_empty() {
                                                    body.push_str("\n\n");
                                                    body.push_str(d);
                                                }
                                            }
                                            w.emit(&when, |op_id, at| {
                                                Op::Message(StoredMessage {
                                                    op_id,
                                                    at,
                                                    role: Role::Assistant,
                                                    content: body,
                                                    model: m["model"].as_str().map(str::to_owned),
                                                    ..blank_message()
                                                })
                                            });
                                        }
                                    }
                                    "plan_init" | "probe_done" | "pdf" | "ls" | "find" | "demo"
                                    | "list_sessions" => {
                                        // The tutor's own tooling: no learner-visible
                                        // content, and LearnHarness-only by name.
                                    }
                                    _ => {}
                                }
                            }
                            _ => {}
                        }
                    }
                    self.flush_text(&mut w, &when, &mut text, m);
                }
                "toolResult" => {
                    let name = m["toolName"].as_str().unwrap_or_default();
                    let call_id = m["toolCallId"].as_str().unwrap_or_default().to_owned();
                    match name {
                        "probe" | "quiz" => {
                            if !asked.contains_key(&call_id) {
                                continue;
                            }
                            let content = m["content"]
                                .as_array()
                                .and_then(|a| a.first())
                                .and_then(|p| p["text"].as_str())
                                .unwrap_or_default()
                                .to_owned();
                            let result =
                                answer_result(&content, m["isError"].as_bool().unwrap_or(false));
                            w.emit(&when, |op_id, at| {
                                Op::Tool(ToolOp {
                                    op_id,
                                    at,
                                    call_id,
                                    name: "question_ask".to_owned(),
                                    args: Value::Null,
                                    result: Some(result),
                                    agent: None,
                                    plugin: None,
                                    platforms: None,
                                })
                            });
                        }
                        "ask_user_question" => {
                            let text = m["content"]
                                .as_array()
                                .and_then(|a| a.first())
                                .and_then(|p| p["text"].as_str())
                                .unwrap_or_default()
                                .to_owned();
                            let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                            let answer = value["text"]
                                .as_str()
                                .or_else(|| value["other"].as_str())
                                .unwrap_or_default()
                                .trim()
                                .to_owned();
                            if answer.is_empty() {
                                continue;
                            }
                            w.emit(&when, |op_id, at| {
                                Op::Message(StoredMessage {
                                    op_id,
                                    at,
                                    role: Role::User,
                                    content: answer,
                                    ..blank_message()
                                })
                            });
                        }
                        "plan_init" if !walk.plan_written => {
                            walk.plan_written = true;
                            w.emit(&when, |op_id, at| {
                                Op::Phase(PhaseOp {
                                    op_id,
                                    at,
                                    phase: harness::state::Phase::Plan,
                                    section_id: None,
                                })
                            });
                            w.emit(&when, |op_id, at| {
                                Op::Plan(RecordOp {
                                    op_id,
                                    at,
                                    payload: plan_payload.clone(),
                                })
                            });
                            w.emit(&when, |op_id, at| {
                                Op::Phase(PhaseOp {
                                    op_id,
                                    at,
                                    phase: harness::state::Phase::AwaitApproval,
                                    section_id: None,
                                })
                            });
                        }
                        _ => {}
                    }
                }
                "custom" if m["customType"] == "compaction" => {
                    let summary = m["content"].as_str().unwrap_or_default().trim().to_owned();
                    if summary.is_empty() {
                        continue;
                    }
                    let (cut_at, cut_op_id) = w.tail();
                    w.emit(&when, |op_id, at| {
                        Op::Compaction(CompactionOp {
                            op_id,
                            at,
                            summary,
                            cut_at,
                            cut_op_id,
                        })
                    });
                }
                _ => {}
            }
        }

        if !walk.plan_written {
            w.emit(&at, |op_id, at| {
                Op::Phase(PhaseOp {
                    op_id,
                    at,
                    phase: harness::state::Phase::Plan,
                    section_id: None,
                })
            });
            w.emit(&at, |op_id, at| {
                Op::Plan(RecordOp {
                    op_id,
                    at,
                    payload: plan_payload.clone(),
                })
            });
        }
        // The source's own closing position: a current topic with no
        // propositions behind it (or none at all) leaves no section current,
        // and its visited topics are the closed ones.
        for id in &self.progress.visited {
            if self.plan.sections.iter().any(|s| &s.id == id) && walk.done.insert(id.clone()) {
                self.push_section_done(&mut w, &at, id);
            }
        }
        w.emit(&at, |op_id, at| {
            Op::Phase(PhaseOp {
                op_id,
                at,
                phase: harness::state::Phase::Teach,
                section_id: None,
            })
        });
        Ok(w.ops)
    }

    fn flush_text(&self, w: &mut Writer, at: &str, text: &mut String, m: &Value) {
        let body = text.trim().to_owned();
        text.clear();
        if body.is_empty() {
            return;
        }
        w.emit(at, |op_id, at| {
            Op::Message(StoredMessage {
                op_id,
                at,
                role: Role::Assistant,
                content: body,
                model: m["model"].as_str().map(str::to_owned),
                ..blank_message()
            })
        });
    }

    /// Records the move into `topic`'s section, closing the previous one when
    /// the source has already left it for good.
    fn enter_section(&self, w: &mut Writer, at: &str, topic: &str, walk: &mut Walk) {
        if !self.plan.sections.iter().any(|s| s.id == topic) {
            return;
        }
        if walk.current.as_deref() == Some(topic) {
            return;
        }
        if let Some(prev) = walk.current.clone() {
            if !walk.done.contains(&prev) && self.progress.visited.iter().any(|v| v == &prev) {
                walk.done.insert(prev.clone());
                self.push_section_done(w, at, &prev);
            }
        }
        walk.current = Some(topic.to_owned());
        if !walk.plan_written || !walk.approved {
            walk.plan_written = true;
            walk.approved = true;
            w.emit(at, |op_id, at| {
                Op::Approve(ApproveOp {
                    op_id,
                    at,
                    plan_version: 1,
                    removed_plugins: Vec::new(),
                })
            });
        }
        w.emit(at, |op_id, at| {
            Op::Phase(PhaseOp {
                op_id,
                at,
                phase: harness::state::Phase::Teach,
                section_id: Some(topic.to_owned()),
            })
        });
    }

    fn push_section_done(&self, w: &mut Writer, at: &str, section: &str) {
        let call_id = format!("port-section-{section}");
        w.emit(at, |op_id, at| {
            Op::Tool(ToolOp {
                op_id,
                at,
                call_id: call_id.clone(),
                name: "section_done".to_owned(),
                args: json!({ "sectionId": section, "evidence": "", "carryOver": "" }),
                result: None,
                agent: None,
                plugin: None,
                platforms: None,
            })
        });
        w.emit(at, |op_id, at| {
            Op::Tool(ToolOp {
                op_id,
                at,
                call_id,
                name: "section_done".to_owned(),
                args: Value::Null,
                result: Some(ToolResultValue {
                    content: "ok".to_owned(),
                    is_error: false,
                }),
                agent: None,
                plugin: None,
                platforms: None,
            })
        });
    }
}

/// Message timestamps are epoch milliseconds in the source; the op log is
/// RFC 3339. Unparseable/missing → the chat's creation time.
fn msg_at(m: &Value, fallback: &str) -> String {
    let millis = m["timestamp"]
        .as_i64()
        .or_else(|| m["timestamp"].as_str().and_then(|s| s.parse::<i64>().ok()));
    millis
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_else(|| fallback.to_owned())
}

fn blank_message() -> StoredMessage {
    StoredMessage {
        op_id: String::new(),
        at: String::new(),
        role: Role::Assistant,
        content: String::new(),
        thinking: String::new(),
        model: None,
        ending: None,
        tag: None,
        job_id: None,
        native: Vec::new(),
        attachments: Vec::new(),
    }
}

/// LearnHarness's question role → ours, and whether the app graded it.
fn question_role(name: &str, args: &Value) -> (&'static str, bool) {
    if name == "probe" {
        return ("probe", true);
    }
    match args["type"].as_str().unwrap_or_default() {
        "predict" => ("predict", true),
        _ => ("check", true),
    }
}

/// The learner-facing card: prompt, options, the tutor's committed answer,
/// the explanation. `nodeId` is deliberately absent — the source only tags
/// the *topic*, and no proposition-level evidence was ever recorded.
fn question_args(role: &str, args: &Value) -> Value {
    let options: Vec<String> = args["options"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| o["label"].as_str().map(str::to_owned))
        .collect();
    let committed = args["correctAnswer"]
        .as_str()
        .and_then(|v| {
            args["options"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|o| o["value"].as_str() == Some(v))
                .and_then(|o| o["label"].as_str())
                .map(str::to_owned)
        })
        .or_else(|| args["correctAnswer"].as_str().map(str::to_owned))
        .unwrap_or_default();
    json!({
        "role": role,
        "format": if options.is_empty() { "free" } else { "choice" },
        "prompt": args["question"].as_str().unwrap_or_default(),
        "options": options,
        "answer": committed,
        "explanation": args["explanation"].as_str().unwrap_or_default()
    })
}

/// The learner's answer as our result payload: what they picked, their
/// reasoning, their confidence and the source's own correctness verdict.
fn answer_result(content: &str, is_error: bool) -> ToolResultValue {
    if is_error {
        return ToolResultValue {
            content: content.to_owned(),
            is_error: true,
        };
    }
    let value: Value = serde_json::from_str(content).unwrap_or(Value::Null);
    let labels: Vec<String> = value["answers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a["label"].as_str().map(str::to_owned))
        .collect();
    let thinking = value["thinking"]
        .as_str()
        .or_else(|| value["thinkingWhileReading"].as_str())
        .unwrap_or_default();
    let mut payload = json!({
        "answer": labels.join(", "),
        "thinking": thinking,
    });
    if let Some(c) = value["confidence"].as_i64() {
        payload["confidence"] = json!(c);
    }
    if let Some(correct) = value["correct"].as_bool() {
        payload["verdict"] = json!(if correct { "correct" } else { "wrong" });
    }
    ToolResultValue {
        content: payload.to_string(),
        is_error: false,
    }
}

struct Writer {
    ops: Vec<Op>,
    seq: u64,
    /// Newest `at` handed out: the log is ordered by (`at`, opId), so the
    /// writer never lets a timestamp move backwards. Learner messages typed
    /// while a question is pending carry an earlier timestamp than the
    /// question's own result in the source; clamping keeps the reading order
    /// the source actually had instead of reordering the transcript.
    last_at: String,
}

impl Writer {
    fn new() -> Self {
        Self {
            ops: Vec::new(),
            seq: 0,
            last_at: String::new(),
        }
    }

    fn next_id(&mut self) -> String {
        self.seq += 1;
        format!("p{:06}", self.seq)
    }

    /// Builds an op from a fresh op id and the clamped timestamp. One place
    /// hands out both, so the log is monotonic by construction.
    fn emit(&mut self, at: &str, make: impl FnOnce(String, String) -> Op) {
        let op_id = self.next_id();
        let at = if self.last_at.is_empty() || at >= self.last_at.as_str() {
            at.to_owned()
        } else {
            self.last_at.clone()
        };
        self.last_at.clone_from(&at);
        self.ops.push(make(op_id, at));
    }

    fn tail(&self) -> (String, String) {
        self.ops
            .last()
            .map(|o| (o.at().to_owned(), o.op_id().to_owned()))
            .unwrap_or_default()
    }
}

// -------------------------------------------------------------- on disk ----

/// A stable ULID for the ported session: the source chat's creation time in
/// the timestamp half, a fixed random half so re-running is idempotent.
/// Session ids must parse as ULIDs — `Store` derives the year directory from
/// them, which is why the file cannot simply be named after the chat.
fn session_id(chat: &Value) -> Result<String, Box<dyn std::error::Error>> {
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let created = chat["createdAt"].as_str().unwrap_or_default();
    let millis = chrono::DateTime::parse_from_rfc3339(created)?
        .timestamp_millis()
        .max(0) as u64;
    let mut id = String::new();
    let mut ts = millis & ((1 << 48) - 1);
    for i in (0..10).rev() {
        let shift = i * 5;
        let digit = (ts >> shift) & 0x1f;
        ts &= !(0x1f << shift);
        id.push(ALPHABET[digit as usize] as char);
    }
    // Deterministic tail (80 bits of SHA-256 of the chat id) so re-running the
    // port overwrites the same file instead of piling up duplicates.
    use sha2::{Digest, Sha256};
    let seed = chat["id"].as_str().unwrap_or("session");
    let digest = Sha256::digest(seed.as_bytes());
    let mut bits: u128 = 0;
    for b in digest.iter().take(10) {
        bits = (bits << 8) | u128::from(*b);
    }
    for i in (0..16).rev() {
        let digit = (bits >> (i * 5)) & 0x1f;
        id.push(ALPHABET[digit as usize] as char);
    }
    ulid::Ulid::from_string(&id)?;
    Ok(id)
}

fn session_path(
    repo: &Path,
    id: &str,
    created: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let year = chrono::DateTime::parse_from_rfc3339(created)
        .map(|d| d.format("%Y").to_string())
        .map_err(|e| format!("chat createdAt is not RFC 3339: {e}"))?;
    Ok(repo.join("sessions").join(year).join(format!("{id}.jsonl")))
}
