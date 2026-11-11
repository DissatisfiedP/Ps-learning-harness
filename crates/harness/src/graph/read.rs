//! Read-only graph access for the live agents (architecture §4.10): the tutor
//! (measure and teach) and the planner see what earlier sessions put in the
//! learner's graph before they measure or plan, so they don't re-probe or
//! re-teach what it already shows. Three views:
//!
//! - `graph_search { query }` — [`super::search_with`], each node line carrying
//!   the learner's evidence;
//! - `graph_read { id }` — one node (statement, home, prerequisites,
//!   dependents, evidence) or one topic (path, subtopics, nodes);
//! - [`learner_state`] — the compact `## Learner state` block of the pinned
//!   session state: topic tree with counts, then the nodes matching the goal.
//!
//! Writing stays the graph builder's (`graph_submit`). An empty graph answers
//! both tools with [`EMPTY`] rather than an empty list, so the model can tell
//! "nothing recorded" from "no match".

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

use crate::knowledge::{KnowledgeMap, KnowledgeNode, KnowledgeTopic};

/// What `graph_search` and `graph_read` return when the graph holds nothing.
pub const EMPTY: &str = "The knowledge graph is empty: no earlier session has recorded anything this learner holds. Measure and plan from the conversation.";

/// Ceiling on the `## Learner state` block (about 400 tokens): the tools are
/// for depth.
pub const MAX_LEARNER_STATE_CHARS: usize = 1600;
/// Share of [`MAX_LEARNER_STATE_CHARS`] the topic tree may use; the rest is
/// for the nodes matching the goal.
const MAX_TOPIC_CHARS: usize = 800;
/// Most goal-matching nodes the block lists.
const MAX_GOAL_NODES: usize = 8;
/// Most nodes, dependents or subtopics one `graph_read` lists per heading.
const MAX_READ_LIST: usize = 40;
/// Ceiling on one `graph_read` result.
const MAX_READ_CHARS: usize = 8000;

/// Whether the graph holds no topic and no node.
pub fn is_empty(map: &KnowledgeMap) -> bool {
    map.topics.is_empty() && map.nodes.is_empty()
}

/// Execute `graph_search` or `graph_read` for the tutor or planner against
/// the graph in `repo`. `Err` is the refusal text for the model; an
/// unreadable graph is a refusal, never a failed turn.
pub fn run(repo: &Path, name: &str, args: &Value) -> Result<String, String> {
    let map = crate::knowledge::load(repo)
        .map_err(|e| format!("the knowledge graph couldn't be read: {e}"))?;
    lookup(&map, name, args)
}

/// [`run`] on an already loaded map.
pub fn lookup(map: &KnowledgeMap, name: &str, args: &Value) -> Result<String, String> {
    match name {
        "graph_search" => {
            let query = crate::tools::req_str(args, "query")?;
            if is_empty(map) {
                return Ok(EMPTY.to_owned());
            }
            Ok(super::search_with(map, query, evidence))
        }
        "graph_read" => {
            let id = crate::tools::req_str(args, "id")?;
            if is_empty(map) {
                return Ok(EMPTY.to_owned());
            }
            Ok(read(map, id.trim()))
        }
        other => Err(format!("`{other}` is not a graph read tool")),
    }
}

/// The learner's record on a node, as a suffix: ` · checks 3✓ 1✗ · session
/// 2026-10-02`. Empty when the node carries neither.
fn evidence(node: &KnowledgeNode) -> String {
    let mut out = String::new();
    if node.correct + node.missed > 0 {
        out.push_str(&format!(" · checks {}✓ {}✗", node.correct, node.missed));
    }
    if let Some(recorded) = recorded(node) {
        out.push_str(" · ");
        out.push_str(&recorded);
    }
    out
}

/// Who recorded the node and when (`session 2026-10-02`), from its provenance.
fn recorded(node: &KnowledgeNode) -> Option<String> {
    let p = node.provenance.as_ref()?;
    let kind = p.get("type").and_then(Value::as_str).unwrap_or("recorded");
    let date = p
        .get("at")
        .and_then(Value::as_str)
        .map(|at| at.get(..10).unwrap_or(at));
    Some(match date {
        Some(d) => format!("{kind} {d}"),
        None => kind.to_owned(),
    })
}

/// `Root › Child › Topic`, by title; an unknown id stands for itself.
fn topic_path(map: &KnowledgeMap, id: &str) -> String {
    let by_id: HashMap<&str, &KnowledgeTopic> =
        map.topics.iter().map(|t| (t.id.as_str(), t)).collect();
    let mut parts: Vec<&str> = Vec::new();
    let mut cur = Some(id);
    while let Some(c) = cur {
        // A parent cycle can't come out of the fold, but never loop on one.
        if parts.len() > map.topics.len() {
            break;
        }
        match by_id.get(c) {
            Some(t) => {
                parts.push(t.title.as_str());
                cur = t.parent.as_deref();
            }
            None => {
                parts.push(c);
                break;
            }
        }
    }
    parts.reverse();
    parts.join(" › ")
}

fn title_of<'a>(map: &'a KnowledgeMap, id: &str) -> &'a str {
    map.nodes
        .iter()
        .find(|n| n.id == id)
        .map_or("", |n| n.title.as_str())
}

/// Push `line` (plus a newline) unless it would take `out` past `cap`.
fn push_line(out: &mut String, line: &str, cap: usize) -> bool {
    if out.len() + line.len() + 1 > cap {
        return false;
    }
    out.push_str(line);
    out.push('\n');
    true
}

/// `graph_read`: one node or topic in full, or why there is none.
pub fn read(map: &KnowledgeMap, id: &str) -> String {
    let canon = map.canonical(id);
    if let Some(node) = map.nodes.iter().find(|n| n.id == canon) {
        return read_node(map, node, id);
    }
    if let Some(topic) = map.topics.iter().find(|t| t.id == id) {
        return read_topic(map, topic);
    }
    if map.is_tombstoned(id) {
        return format!("`{id}` was removed from the graph.");
    }
    format!("No node or topic has the id `{id}`. Find ids with graph_search.")
}

fn read_node(map: &KnowledgeMap, node: &KnowledgeNode, asked: &str) -> String {
    let mut out = format!("node {} — {}\n", node.id, node.title);
    if asked != node.id {
        out.push_str(&format!("(`{asked}` was merged into this node)\n"));
    }
    out.push_str(&format!(
        "kind: {} · home: {} ({})\n",
        node.kind.as_str(),
        topic_path(map, &node.home),
        node.home
    ));
    out.push_str(&format!("statement: {}\n", node.statement));
    if let Some(a) = &node.attribution {
        out.push_str(&format!("attribution: {a}\n"));
    }
    if let Some(s) = &node.scope {
        out.push_str(&format!("scope: {s}\n"));
    }

    let mut requires = Vec::new();
    let mut needed_by = Vec::new();
    let mut related = Vec::new();
    for e in &map.edges {
        let trivial = if e.weight == "trivial" {
            " (trivial)"
        } else {
            ""
        };
        if e.is_requires() {
            if e.to == node.id {
                requires.push(format!("- {} {}{trivial}", e.from, title_of(map, &e.from)));
            } else if e.from == node.id {
                needed_by.push(format!("- {} {}{trivial}", e.to, title_of(map, &e.to)));
            }
        } else if e.from == node.id || e.to == node.id {
            let other = if e.from == node.id { &e.to } else { &e.from };
            related.push(format!(
                "- {} {} {}",
                e.relation,
                other,
                title_of(map, other)
            ));
        }
    }
    let mut list = |heading: &str, lines: &[String], none: &str| {
        out.push_str(heading);
        if lines.is_empty() {
            out.push_str(none);
            out.push('\n');
            return;
        }
        out.push('\n');
        for line in lines.iter().take(MAX_READ_LIST) {
            out.push_str(line);
            out.push('\n');
        }
        if lines.len() > MAX_READ_LIST {
            out.push_str(&format!("… {} more\n", lines.len() - MAX_READ_LIST));
        }
    };
    list("requires (learn these first):", &requires, " none recorded");
    list("needed by:", &needed_by, " none recorded");
    if !related.is_empty() {
        list("other relations:", &related, "");
    }

    out.push_str("evidence:");
    let mut any = false;
    if node.correct + node.missed > 0 {
        out.push_str(&format!(
            " checks on this node {}✓ {}✗;",
            node.correct, node.missed
        ));
        any = true;
    }
    if let Some(c) = map.topic_checks.get(&node.home) {
        out.push_str(&format!(
            " checks in topic {} {}✓ {}✗;",
            node.home, c.correct, c.missed
        ));
        any = true;
    }
    if let Some(r) = recorded(node) {
        out.push_str(&format!(" recorded by {r}"));
        if let Some(sid) = node
            .provenance
            .as_ref()
            .and_then(|p| p.get("sessionId"))
            .and_then(Value::as_str)
        {
            out.push_str(&format!(" (session {sid})"));
        }
        any = true;
    }
    if !any {
        out.push_str(" none recorded");
    }
    out.push('\n');
    if !node.aliases.is_empty() {
        out.push_str(&format!("also known as: {}\n", node.aliases.join(", ")));
    }
    cap(out)
}

fn read_topic(map: &KnowledgeMap, topic: &KnowledgeTopic) -> String {
    let mut out = format!("topic {} — {}\n", topic.id, topic.title);
    out.push_str(&format!("path: {}\n", topic_path(map, &topic.id)));
    if let Some(c) = map.topic_checks.get(&topic.id) {
        out.push_str(&format!("checks: {}✓ {}✗\n", c.correct, c.missed));
    }
    let subtopics: Vec<&KnowledgeTopic> = map
        .topics
        .iter()
        .filter(|t| t.parent.as_deref() == Some(topic.id.as_str()))
        .collect();
    if !subtopics.is_empty() {
        out.push_str("subtopics:\n");
        for t in subtopics.iter().take(MAX_READ_LIST) {
            out.push_str(&format!(
                "- {} {} ({} nodes)\n",
                t.id,
                t.title,
                map.topic_nodes(&t.id).len()
            ));
        }
        if subtopics.len() > MAX_READ_LIST {
            out.push_str(&format!("… {} more\n", subtopics.len() - MAX_READ_LIST));
        }
    }
    let nodes = map.topic_nodes(&topic.id);
    out.push_str(&format!("nodes ({}):", nodes.len()));
    if nodes.is_empty() {
        out.push_str(" none\n");
    } else {
        out.push('\n');
        for n in nodes.iter().take(MAX_READ_LIST) {
            out.push_str(&format!("- {} {}{}\n", n.id, n.title, evidence(n)));
        }
        if nodes.len() > MAX_READ_LIST {
            out.push_str(&format!(
                "… {} more; narrow with graph_search\n",
                nodes.len() - MAX_READ_LIST
            ));
        }
    }
    cap(out)
}

/// Cut a `graph_read` result at [`MAX_READ_CHARS`], on a line boundary.
fn cap(mut out: String) -> String {
    if out.len() <= MAX_READ_CHARS {
        return out;
    }
    let mut end = MAX_READ_CHARS;
    while !out.is_char_boundary(end) {
        end -= 1;
    }
    let end = out[..end].rfind('\n').map_or(end, |i| i + 1);
    out.truncate(end);
    out.push_str("… (cut; read a narrower id)\n");
    out
}

/// The goal record's words worth matching: `goal` and `successLooksLike`,
/// lowercased, at least three letters, without the filler every goal has.
fn goal_words(goal: &Value) -> Vec<String> {
    const FILLER: &[&str] = &[
        "the",
        "and",
        "for",
        "how",
        "why",
        "can",
        "use",
        "get",
        "able",
        "about",
        "also",
        "basics",
        "being",
        "better",
        "between",
        "from",
        "good",
        "have",
        "into",
        "know",
        "learn",
        "learning",
        "more",
        "that",
        "their",
        "them",
        "then",
        "they",
        "this",
        "those",
        "understand",
        "understanding",
        "want",
        "what",
        "when",
        "where",
        "which",
        "with",
        "without",
        "work",
        "explain",
        "your",
        "you",
    ];
    let mut text = goal
        .get("goal")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if let Some(items) = goal.get("successLooksLike").and_then(Value::as_array) {
        for item in items.iter().filter_map(Value::as_str) {
            text.push(' ');
            text.push_str(item);
        }
    }
    let mut words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|w| w.chars().count() >= 3 && !FILLER.contains(&w.as_str()))
        // Substring matching does the rest: "dividers" finds "divider".
        .map(|mut w| {
            if w.ends_with('s') && w.chars().count() >= 5 {
                w.pop();
            }
            w
        })
        .collect();
    words.sort();
    words.dedup();
    words
}

/// The `## Learner state` block (architecture §4.15): the topic tree with
/// node counts, then up to [`MAX_GOAL_NODES`] nodes matching the goal record,
/// with the learner's evidence, within [`MAX_LEARNER_STATE_CHARS`]. Empty for
/// an empty graph (the block then reads "no graph yet").
pub fn learner_state(map: &KnowledgeMap, goal: Option<&Value>) -> String {
    if is_empty(map) {
        return String::new();
    }
    let mut out = format!(
        "Knowledge graph: {} topics, {} nodes. Read deeper with graph_search and graph_read.\n",
        map.topics.len(),
        map.nodes.len()
    );

    let mut counts: HashMap<&str, usize> = HashMap::new();
    for n in &map.nodes {
        *counts.entry(n.home.as_str()).or_default() += 1;
    }
    let mut children: HashMap<&str, Vec<&KnowledgeTopic>> = HashMap::new();
    for t in &map.topics {
        if let Some(parent) = &t.parent {
            children.entry(parent.as_str()).or_default().push(t);
        }
    }
    out.push_str("Topics (nodes homed there):\n");
    let topic_cap = out.len() + MAX_TOPIC_CHARS;
    let mut shown = 0usize;
    let mut stack: Vec<(&KnowledgeTopic, usize)> = map
        .topics
        .iter()
        .filter(|t| t.parent.is_none())
        .rev()
        .map(|t| (t, 0))
        .collect();
    while let Some((topic, depth)) = stack.pop() {
        // A parent cycle can't come out of the fold, but never loop on one.
        if shown > map.topics.len() {
            break;
        }
        let mut line = format!(
            "{}- {} ({}) {}",
            "  ".repeat(depth),
            topic.title,
            topic.id,
            counts.get(topic.id.as_str()).copied().unwrap_or(0)
        );
        if let Some(c) = map.topic_checks.get(&topic.id) {
            line.push_str(&format!(" · checks {}✓ {}✗", c.correct, c.missed));
        }
        if !push_line(&mut out, &line, topic_cap) {
            break;
        }
        shown += 1;
        if let Some(kids) = children.get(topic.id.as_str()) {
            stack.extend(kids.iter().rev().map(|k| (*k, depth + 1)));
        }
    }
    if shown < map.topics.len() {
        out.push_str(&format!("- … {} more topics\n", map.topics.len() - shown));
    }

    let words = goal.map(goal_words).unwrap_or_default();
    if words.is_empty() {
        return out;
    }
    let titles: HashMap<&str, String> = map
        .topics
        .iter()
        .map(|t| (t.id.as_str(), t.title.to_lowercase()))
        .collect();
    let mut matches: Vec<(usize, &KnowledgeNode)> = map
        .nodes
        .iter()
        .filter_map(|n| {
            let hay = format!(
                "{} {} {} {}",
                n.id,
                n.title,
                n.statement,
                titles.get(n.home.as_str()).map_or("", String::as_str)
            )
            .to_lowercase();
            let score = words.iter().filter(|w| hay.contains(w.as_str())).count();
            (score > 0).then_some((score, n))
        })
        .collect();
    matches.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));
    if matches.is_empty() {
        push_line(
            &mut out,
            "No node matches the goal's words; graph_search before treating it as new.",
            MAX_LEARNER_STATE_CHARS,
        );
        return out;
    }
    if !push_line(
        &mut out,
        "Nodes matching the goal:",
        MAX_LEARNER_STATE_CHARS,
    ) {
        return out;
    }
    let mut listed = 0usize;
    for (_, n) in matches.iter().take(MAX_GOAL_NODES) {
        let line = format!("- {} [{}] {}{}", n.id, n.home, n.title, evidence(n));
        if !push_line(&mut out, &line, MAX_LEARNER_STATE_CHARS) {
            break;
        }
        listed += 1;
    }
    if matches.len() > listed {
        push_line(
            &mut out,
            &format!("- … {} more matches", matches.len() - listed),
            MAX_LEARNER_STATE_CHARS,
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{fold, provenance, GraphOp};
    use crate::state::Kind;
    use serde_json::json;

    fn map() -> KnowledgeMap {
        fold(&[
            GraphOp::topic("electronics", "Electronics", None),
            GraphOp::topic("circuits", "Circuits", Some("electronics")),
            GraphOp::node(
                "ohms-law",
                Some("Ohm's law"),
                "V = IR for a resistor.",
                Kind::Fact,
                "circuits",
                Some(provenance("session", Some("s0"), "2026-10-02T10:00:00Z")),
            ),
            GraphOp::node(
                "divider-rule",
                Some("The voltage divider rule"),
                "An unloaded divider gives V_in R2/(R1+R2).",
                Kind::Mechanism,
                "circuits",
                None,
            )
            .with_evidence(Some(3), Some(1)),
            GraphOp::edge("ohms-law", "divider-rule", "requires", None, None, None),
        ])
    }

    #[test]
    fn an_empty_graph_says_so_for_both_tools_and_leaves_the_block_empty() {
        let empty = KnowledgeMap::default();
        for (name, args) in [
            ("graph_search", json!({"query": "ohm"})),
            ("graph_read", json!({"id": "ohms-law"})),
        ] {
            assert_eq!(lookup(&empty, name, &args).unwrap(), EMPTY);
        }
        assert!(lookup(&empty, "graph_search", &json!({})).is_err());
        assert_eq!(
            learner_state(&empty, Some(&json!({"goal": "dividers"}))),
            ""
        );
    }

    #[test]
    fn search_lines_carry_the_learners_evidence() {
        let text = lookup(&map(), "graph_search", &json!({"query": "divider"})).unwrap();
        assert!(text.contains("divider-rule [circuits]"), "{text}");
        assert!(text.contains("checks 3✓ 1✗"), "{text}");
    }

    #[test]
    fn read_gives_a_nodes_prerequisites_dependents_and_record() {
        let m = map();
        let node = read(&m, "divider-rule");
        assert!(
            node.contains("home: Electronics › Circuits (circuits)"),
            "{node}"
        );
        assert!(
            node.contains("requires (learn these first):\n- ohms-law Ohm's law"),
            "{node}"
        );
        assert!(node.contains("checks on this node 3✓ 1✗"), "{node}");
        let ohm = read(&m, "ohms-law");
        assert!(ohm.contains("needed by:\n- divider-rule"), "{ohm}");
        assert!(
            ohm.contains("recorded by session 2026-10-02 (session s0)"),
            "{ohm}"
        );

        let topic = read(&m, "circuits");
        assert!(topic.contains("nodes (2):"), "{topic}");
        assert!(read(&m, "ghost").contains("No node or topic"));
    }

    #[test]
    fn learner_state_lists_topics_and_the_nodes_matching_the_goal_within_its_cap() {
        let m = map();
        let goal = json!({"goal": "Analyze loaded voltage dividers", "successLooksLike": []});
        let block = learner_state(&m, Some(&goal));
        assert!(
            block.contains("- Electronics (electronics) 0\n  - Circuits (circuits) 2"),
            "{block}"
        );
        assert!(
            block.contains("Nodes matching the goal:\n- divider-rule [circuits]"),
            "{block}"
        );
        assert!(
            !block.contains("- ohms-law"),
            "ohm's law doesn't match the goal: {block}"
        );

        // A big graph still fits the budget.
        let mut ops = vec![GraphOp::topic("t", "T", None)];
        for i in 0..400 {
            ops.push(GraphOp::topic(
                &format!("t{i}"),
                &format!("Topic {i}"),
                Some("t"),
            ));
            ops.push(GraphOp::node(
                &format!("divider-{i}"),
                Some("divider claim"),
                "a divider fact",
                Kind::Fact,
                &format!("t{i}"),
                None,
            ));
        }
        let block = learner_state(&fold(&ops), Some(&goal));
        assert!(block.len() <= MAX_LEARNER_STATE_CHARS, "{}", block.len());
        assert!(block.contains("more topics"), "{block}");
        assert!(block.contains("more matches"), "{block}");
    }
}
