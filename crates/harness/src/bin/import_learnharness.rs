//! One-off importer: LearnHarness (AGPL-3.0) data → this harness's standalone
//! knowledge graph in `<repo>/graph/` (architecture §4.2, §4.5).
//!
//! It reads only the learner's own files (no LearnHarness source is copied):
//! - `data/graph/nodes.jsonl`, `data/graph/chains.jsonl` — propositions and
//!   dependency chains (premises → conclusion).
//! - `data/chats/*.json` — the last plan block (topic titles), and the graded
//!   quiz/probe results (per-topic correct/missed totals).
//!
//! It writes `graph/ops.jsonl` — `topic`, `node` and `edge` ops with
//! provenance `{"type":"import"}` — plus `graph/checks.jsonl` for the learner
//! evidence. It never writes the legacy `topics.json` / `nodes.jsonl` /
//! `edges.jsonl` files; those are only read once, by the migration
//! (`graph::migrate`).
//!
//! Usage: `cargo run -p harness --bin import_learnharness -- <learnharness-data-dir> <repo-dir>`
//!
//! Topic tree and each proposition's home topic are authored here from the
//! data (the plan's topic titles, plus keyword rules over name+statement).
//! Both are **[inference]**: LearnHarness has no topic tree of its own, so the
//! grouping is a judgement, not imported fact.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use harness::graph::{self, GraphOp};
use harness::state::Kind;
use serde_json::{json, Value};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let data = PathBuf::from(
        args.next()
            .ok_or("usage: <learnharness-data-dir> <repo-dir>")?,
    );
    let repo = PathBuf::from(
        args.next()
            .ok_or("usage: <learnharness-data-dir> <repo-dir>")?,
    );

    let out = repo.join("graph");
    std::fs::create_dir_all(&out)?;

    let chat_path = newest_chat(&data)?;
    let chat: Value = serde_json::from_str(&std::fs::read_to_string(&chat_path)?)?;

    let plan_topics = plan_topics(&chat);
    let topics = topic_tree(&plan_topics);
    let nodes = propositions(&data, &chat, &plan_topics)?;
    let edges = chains(&data, &nodes)?;
    let checks = checks(&chat);

    let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let mut ops: Vec<GraphOp> = Vec::new();
    for t in &topics {
        ops.push(GraphOp::topic(
            t["id"].as_str().unwrap_or_default(),
            t["title"].as_str().unwrap_or_default(),
            t["parent"].as_str(),
        ));
    }
    for n in &nodes {
        let kind =
            Kind::parse(n["kind"].as_str().unwrap_or("mechanism")).unwrap_or(Kind::Mechanism);
        ops.push(
            GraphOp::node(
                n["id"].as_str().unwrap_or_default(),
                n["title"].as_str(),
                n["statement"].as_str().unwrap_or_default(),
                kind,
                n["home"].as_str().unwrap_or_default(),
                Some(graph::provenance("import", None, &at)),
            )
            .with_evidence(
                n["correct"].as_u64().map(|c| c as u32),
                n["missed"].as_u64().map(|m| m as u32),
            ),
        );
    }
    for e in &edges {
        ops.push(GraphOp::edge(
            e["from"].as_str().unwrap_or_default(),
            e["to"].as_str().unwrap_or_default(),
            "requires",
            None,
            None,
            Some(graph::provenance("import", None, &at)),
        ));
    }
    graph::append(&repo, &ops)?;
    if !checks.is_empty() {
        write_jsonl(&out.join("checks.jsonl"), checks)?;
    }
    println!(
        "wrote {} topics, {} nodes, {} edges, {} topic checks to {}",
        topics.len(),
        nodes.len(),
        edges.len(),
        checks_len(&out),
        graph::OPS
    );
    Ok(())
}

fn checks_len(out: &Path) -> usize {
    std::fs::read_to_string(out.join("checks.jsonl"))
        .map(|t| t.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0)
}

fn write_jsonl<T: serde::Serialize>(path: &Path, rows: Vec<T>) -> std::io::Result<()> {
    let mut s = String::new();
    for row in rows {
        s.push_str(&serde_json::to_string(&row).map_err(std::io::Error::other)?);
        s.push('\n');
    }
    std::fs::write(path, s)
}

fn newest_chat(data: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let dir = data.join("chats");
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(&dir)? {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let at = std::fs::metadata(&path)?.modified()?;
        if best.as_ref().is_none_or(|(t, _)| at > *t) {
            best = Some((at, path));
        }
    }
    best.map(|(_, p)| p)
        .ok_or_else(|| "no chat file in data/chats".into())
}

/// The last plan block's node ids and titles (the session's topics).
fn plan_topics(chat: &Value) -> Vec<(String, String)> {
    let Some(messages) = chat["messages"].as_array() else {
        return Vec::new();
    };
    let block = messages
        .iter()
        .rfind(|m| m["customType"] == "plan-block")
        .map(|m| &m["details"]["block"]);
    let Some(block) = block else {
        return Vec::new();
    };
    block["nodes"]
        .as_array()
        .map(|ns| {
            ns.iter()
                .filter_map(|n| {
                    Some((
                        n["id"].as_str()?.to_owned(),
                        n["title"].as_str()?.to_owned(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `Mathematics → {Algebra, Calculus, Multivariable} → the plan's topics`.
/// The grouping is authored **[inference]** (see the module doc).
fn topic_tree(plan: &[(String, String)]) -> Vec<Value> {
    let group_of = |id: &str| -> &'static str {
        match id {
            "algebra-domains" => "algebra",
            "vectors-partials" | "multiple-vector-calculus" => "multivariable",
            _ => "calculus",
        }
    };
    let mut topics = vec![
        json!({ "id": "math", "title": "Mathematics", "parent": null }),
        json!({ "id": "algebra", "title": "Algebra & functions", "parent": "math" }),
        json!({ "id": "calculus", "title": "Calculus", "parent": "math" }),
        json!({ "id": "multivariable", "title": "Multivariable calculus", "parent": "math" }),
    ];
    for (id, title) in plan {
        topics.push(json!({ "id": id, "title": title, "parent": group_of(id) }));
    }
    topics
}

/// Home topic from the claim's name + statement. Authored **[inference]**.
fn home_of(name: &str, statement: &str) -> &'static str {
    let s = format!("{name} {statement}").to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| s.contains(n));
    if has(&["hyperbolic"]) {
        return "hyperbolic-functions";
    }
    if has(&[
        "logarith",
        "exponential",
        "exponent",
        "log_b",
        "positivity of exp",
        "repeated multiplication",
    ]) {
        return "derivative-rules";
    }
    if has(&["unit-circle", "unit circle", "radian", "trig"]) {
        return "trig-foundations";
    }
    if has(&[
        "epsilon",
        "ε",
        "δ",
        "two-sided",
        "one-sided",
        "continuity",
        "punctured",
        "1/x",
        "cancellation",
        "quantifier",
    ]) {
        return "limits-continuity";
    }
    if has(&["indeterminate", "conjugate", "0/0"]) {
        return "lhopital";
    }
    if has(&["taylor", "series", "convergence"]) {
        return "series-taylor";
    }
    if has(&[
        "definite integral",
        "integration techniques",
        "fundamental theorem",
    ]) {
        return "integral-ftc-methods";
    }
    if has(&["average change", "base e"]) {
        return "derivative-meaning";
    }
    if has(&["composition", "codomain", "domain"]) {
        return "algebra-domains";
    }
    // Claims that fell through to the group topic. Kept last so every
    // earlier classification is unchanged; **[inference]** like the rules
    // above, from the claim's own statement:
    // - the |f(x) − L| = |x − a|·|extra(x)| bound is part of an ε–δ proof;
    // - algebraic identities and member-by-member multiplication are
    //   algebra, used as tools *inside* limit and derivative work.
    if has(&["extra factor", "bounding"]) {
        return "limits-continuity";
    }
    if has(&[
        "side by side",
        "identity",
        "expression-specific",
        "multiply",
    ]) {
        return "algebra-domains";
    }
    "calculus"
}

fn kind_of(text: &str) -> &'static str {
    let s = text.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| s.contains(n));
    if has(&[
        "method",
        "prove",
        "choose",
        "bound",
        "solve",
        "compute",
        "construct",
    ]) {
        "procedure"
    } else if has(&["is called", "notation", "refers to", "means ", "term "]) {
        "fact"
    } else if has(&["argument", "counterexample", "objection", "disagree"]) {
        "argument"
    } else if has(&["recognis", "recognize", "by eye", "visual"]) {
        "perceptual"
    } else {
        "mechanism"
    }
}

/// The propositions (last revision of each id wins) with their home topic.
fn propositions(
    data: &Path,
    _chat: &Value,
    _plan: &[(String, String)],
) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(data.join("graph/nodes.jsonl"))?;
    let mut by_id: BTreeMap<String, Value> = BTreeMap::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let v: Value = serde_json::from_str(line)?;
        if let Some(id) = v["id"].as_str() {
            by_id.insert(id.to_owned(), v);
        }
    }
    let mut out = Vec::new();
    for (id, v) in by_id {
        let title = v["name"].as_str().unwrap_or_default().to_owned();
        let statement = v["statement"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_owned();
        if title.is_empty() {
            continue;
        }
        out.push(json!({
            "id": id,
            "title": title,
            "statement": statement,
            "kind": kind_of(&format!("{title} {statement}")),
            "home": home_of(&title, &statement),
            // Honest: no per-proposition check was ever recorded.
            "correct": 0,
            "missed": 0
        }));
    }
    Ok(out)
}

/// Prerequisite edges from chains (`premises[] → conclusion`), deduped.
/// A node may keep several `from` entries: that is the graph, not a tree.
fn chains(data: &Path, nodes: &[Value]) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let known: HashSet<&str> = nodes.iter().filter_map(|n| n["id"].as_str()).collect();
    let text = std::fs::read_to_string(data.join("graph/chains.jsonl"))?;
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let v: Value = serde_json::from_str(line)?;
        let Some(conclusion) = v["conclusion"].as_str() else {
            continue;
        };
        if !known.contains(conclusion) {
            continue;
        }
        for premise in v["premises"].as_array().into_iter().flatten() {
            let Some(p) = premise.as_str() else { continue };
            if p == conclusion || !known.contains(p) {
                continue;
            }
            seen.insert((p.to_owned(), conclusion.to_owned()));
        }
    }
    // The source records one chain per argument it made, and two arguments can
    // each name the other as a premise — that produced a real 2-cycle here
    // (continuity ↔ punctured neighbourhood). A prerequisite graph must be
    // acyclic (the planner's plan validation refuses one), so add edges in
    // sorted order and drop any that would close a loop, keeping the edge the
    // source recorded first. Deterministic: same input, same survivors.
    let mut kept: Vec<(String, String)> = Vec::new();
    let mut dropped = 0usize;
    for (from, to) in seen {
        if reaches(&kept, &to, &from) {
            dropped += 1;
            continue;
        }
        kept.push((from, to));
    }
    if dropped > 0 {
        eprintln!("dropped {dropped} prerequisite edge(s) that would close a loop");
    }
    Ok(kept
        .into_iter()
        .map(|(from, to)| json!({ "from": from, "to": to }))
        .collect())
}

/// Whether `from` already reaches `to` through `edges` (DFS, no allocation per
/// step). Used to keep the prerequisite graph acyclic.
fn reaches(edges: &[(String, String)], from: &str, to: &str) -> bool {
    let mut stack = vec![from];
    let mut seen: HashSet<&str> = HashSet::new();
    while let Some(at) = stack.pop() {
        if at == to {
            return true;
        }
        if !seen.insert(at) {
            continue;
        }
        for (a, b) in edges {
            if a == at {
                stack.push(b.as_str());
            }
        }
    }
    false
}

/// Real per-topic quiz totals: `toolResult` for `quiz`/`probe`, matched to the
/// `toolCall` that named the topic, counted by `details.correct`.
fn checks(chat: &Value) -> Vec<Value> {
    let Some(messages) = chat["messages"].as_array() else {
        return Vec::new();
    };
    let mut call_topic: HashMap<&str, &str> = HashMap::new();
    for m in messages.iter().filter(|m| m["role"] == "assistant") {
        for call in m["content"].as_array().into_iter().flatten() {
            if call["type"] != "toolCall" {
                continue;
            }
            let name = call["name"].as_str().unwrap_or_default();
            if name != "quiz" && name != "probe" {
                continue;
            }
            if let (Some(id), Some(topic)) =
                (call["id"].as_str(), call["arguments"]["nodeId"].as_str())
            {
                call_topic.insert(id, topic);
            }
        }
    }
    let mut totals: BTreeMap<&str, (u32, u32)> = BTreeMap::new();
    for m in messages.iter().filter(|m| m["role"] == "toolResult") {
        let name = m["toolName"].as_str().unwrap_or_default();
        if name != "quiz" && name != "probe" {
            continue;
        }
        let Some(topic) = m["toolCallId"]
            .as_str()
            .and_then(|id| call_topic.get(id).copied())
        else {
            continue;
        };
        let entry = totals.entry(topic).or_insert((0, 0));
        match m["details"]["correct"].as_bool() {
            Some(true) => entry.0 += 1,
            Some(false) => entry.1 += 1,
            None => {}
        }
    }
    totals
        .into_iter()
        .map(|(topic, (correct, missed))| json!({ "topic": topic, "correct": correct, "missed": missed }))
        .collect()
}
