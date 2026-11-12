//! What a `graph_submit` must get right (architecture §4.3, §4.6).
//!
//! The code owns these rules, not the prompt: a refused submit returns every
//! problem at once, in the style of `plan_write`, and the builder run carries
//! on so the model can fix them all in one retry. Nothing here trusts the
//! submission — ids, enums, loops and partitions are all re-derived from the
//! graph as it is now plus what the same submission already adds.

use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use serde_json::Value;

use super::{provenance, GraphOp, PROVENANCE_TYPES, RELATIONS, VENUE_KINDS, WEIGHTS};
use crate::knowledge::{KnowledgeMap, ProposedSplit, SplitGroup, REQUIRES};
use crate::state::Kind;

/// A parsed `graph_submit` payload.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Submit {
    #[serde(default)]
    pub topics: Vec<SubmitTopic>,
    #[serde(default)]
    pub nodes: Vec<SubmitNode>,
    #[serde(default)]
    pub edges: Vec<SubmitEdge>,
    #[serde(default)]
    pub same_as: Vec<SubmitSameAs>,
    #[serde(default)]
    pub chains: Vec<SubmitChain>,
    #[serde(default)]
    pub splits: Vec<SubmitSplit>,
    #[serde(default)]
    pub venues: Vec<SubmitVenue>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitTopic {
    #[serde(default)]
    pub topic_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub parent: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitNode {
    #[serde(default)]
    pub node_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub statement: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub home: String,
    #[serde(default)]
    pub provenance: Option<Value>,
    #[serde(default)]
    pub attribution: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitEdge {
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub to: String,
    #[serde(default)]
    pub relation: Option<String>,
    #[serde(default)]
    pub weight: Option<String>,
    #[serde(default)]
    pub why: Option<String>,
    #[serde(default)]
    pub provenance: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitSameAs {
    #[serde(default)]
    pub a: String,
    #[serde(default)]
    pub b: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitChain {
    #[serde(default)]
    pub chain_id: Option<String>,
    #[serde(default)]
    pub premises: Vec<String>,
    #[serde(default)]
    pub conclusion: String,
    #[serde(default)]
    pub statement: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitSplit {
    #[serde(default)]
    pub topic: String,
    #[serde(default)]
    pub groups: Vec<SplitGroup>,
}

/// One place where a topic's material lives (`docs/learning-system.md` §6).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitVenue {
    #[serde(default)]
    pub topic_id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub entry_level: String,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub checked: Option<String>,
    #[serde(default)]
    pub provenance: Option<Value>,
}

/// A submission that passed: the ops to append and the splits to propose.
#[derive(Debug, Clone, Default)]
pub struct Batch {
    pub ops: Vec<GraphOp>,
    /// Named splits, for `graph/splits.proposed.json` — never structure until
    /// the learner accepts them.
    pub splits: Vec<ProposedSplit>,
}

impl Batch {
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty() && self.splits.is_empty()
    }
}

const KINDS: &str = "mechanism, fact, procedure, perceptual, argument or heuristic";

/// Check a `graph_submit` payload against the graph. On failure every problem
/// is returned (sorted, deduplicated); on success the ops it describes are
/// built, stamped with `provenance` and ready to append.
pub fn check(
    args: &Value,
    map: &KnowledgeMap,
    session_id: Option<&str>,
    at: &str,
) -> Result<Batch, Vec<String>> {
    let submit: Submit = serde_json::from_value(args.clone()).map_err(|e| {
        vec![format!(
            "graph_submit arguments don't match the schema: {e}"
        )]
    })?;
    let default_provenance = provenance("builder", session_id, at);

    let mut problems: Vec<String> = Vec::new();
    let mut topic_ids: HashSet<String> = map.topics.iter().map(|t| t.id.clone()).collect();
    let mut node_ids: HashSet<String> = map.nodes.iter().map(|n| n.id.clone()).collect();
    for n in &map.nodes {
        for a in &n.aliases {
            node_ids.insert(a.clone());
        }
    }
    let mut edges: HashSet<(String, String, String)> = map
        .edges
        .iter()
        .map(|e| (e.from.clone(), e.to.clone(), e.relation.clone()))
        .collect();
    let mut deps: Vec<(String, String)> = map
        .prerequisites()
        .map(|e| (e.from.clone(), e.to.clone()))
        .collect();

    // ---- topics ---------------------------------------------------------
    let mut ops: Vec<GraphOp> = Vec::new();
    let mut submitted_topics: HashSet<String> = HashSet::new();
    for t in &submit.topics {
        let id = t.topic_id.trim();
        if id.is_empty() {
            problems.push("a submitted topic has an empty `topicId`".to_owned());
            continue;
        }
        if !submitted_topics.insert(id.to_owned()) {
            problems.push(format!("topic `{id}` is submitted twice"));
        }
        if t.title.trim().is_empty() {
            problems.push(format!("topic `{id}` has an empty `title`"));
        }
        topic_ids.insert(id.to_owned());
    }
    for t in &submit.topics {
        let id = t.topic_id.trim();
        let parent = t.parent.as_deref().map(str::trim).filter(|p| !p.is_empty());
        if let Some(parent) = parent {
            if parent == id {
                problems.push(format!("topic `{id}` is its own parent"));
            } else if !topic_ids.contains(parent) {
                problems.push(format!("topic `{id}` has unknown parent `{parent}`"));
            }
        }
        if !id.is_empty() && !t.title.trim().is_empty() {
            ops.push(GraphOp::topic(id, t.title.trim(), parent));
        }
    }

    // ---- nodes ----------------------------------------------------------
    let mut homes: HashMap<String, Vec<String>> = HashMap::new();
    for n in &map.nodes {
        homes.entry(n.home.clone()).or_default().push(n.id.clone());
    }
    let mut submitted_nodes: HashSet<String> = HashSet::new();
    for n in &submit.nodes {
        let id = n.node_id.trim();
        if id.is_empty() {
            problems.push("a submitted node has an empty `nodeId`".to_owned());
            continue;
        }
        if !submitted_nodes.insert(id.to_owned()) {
            problems.push(format!("node `{id}` is submitted twice"));
        }
        if n.statement.trim().is_empty() {
            problems.push(format!("node `{id}` has an empty `statement`"));
        }
        let Some(kind) = Kind::parse(n.kind.trim()) else {
            problems.push(format!(
                "node `{id}` has kind `{}` (must be {KINDS})",
                n.kind.trim()
            ));
            continue;
        };
        if kind == Kind::Heuristic {
            if n.attribution
                .as_deref()
                .map(str::trim)
                .filter(|a| !a.is_empty())
                .is_none()
            {
                problems.push(format!("node `{id}` is a heuristic without `attribution`"));
            }
            if n.scope
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .is_none()
            {
                problems.push(format!("node `{id}` is a heuristic without `scope`"));
            }
        }
        let home = n.home.trim();
        if home.is_empty() {
            problems.push(format!("node `{id}` has an empty `home`"));
        } else if !topic_ids.contains(home) {
            problems.push(format!("node `{id}` has unknown home `{home}`"));
        } else {
            homes
                .entry(home.to_owned())
                .or_default()
                .push(id.to_owned());
        }
        if let Some(p) = &n.provenance {
            if let Some(why) = provenance_problem(p) {
                problems.push(format!("node `{id}`: {why}"));
            }
        }
        node_ids.insert(id.to_owned());
        let proven = stamped(n.provenance.as_ref(), &default_provenance);
        ops.push(
            GraphOp::node(
                id,
                n.title.as_deref(),
                &n.statement,
                kind,
                home,
                Some(proven),
            )
            .described(n.attribution.as_deref(), n.scope.as_deref()),
        );
    }

    // ---- edges ----------------------------------------------------------
    for e in &submit.edges {
        let (from, to) = (e.from.trim(), e.to.trim());
        let mut ok = true;
        if from.is_empty() || to.is_empty() {
            problems.push("an edge has an empty `from` or `to`".to_owned());
            ok = false;
        } else if from == to {
            problems.push(format!("edge `{from}` → `{to}` is a self-edge"));
            ok = false;
        } else {
            for end in [from, to] {
                if map.is_tombstoned(end) {
                    problems.push(format!("edge names tombstoned node `{end}`"));
                    ok = false;
                } else if !node_ids.contains(end) {
                    problems.push(format!("edge names unknown node `{end}`"));
                    ok = false;
                }
            }
        }
        let relation = e
            .relation
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .unwrap_or(REQUIRES)
            .to_owned();
        if !RELATIONS.contains(&relation.as_str()) {
            problems.push(format!(
                "edge `{from}` → `{to}` has relation `{relation}` (must be {})",
                RELATIONS.join(", ")
            ));
            ok = false;
        }
        let weight = e
            .weight
            .as_deref()
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .unwrap_or("load-bearing")
            .to_owned();
        if !WEIGHTS.contains(&weight.as_str()) {
            problems.push(format!(
                "edge `{from}` → `{to}` has weight `{weight}` (must be {})",
                WEIGHTS.join(" or ")
            ));
            ok = false;
        }
        if let Some(p) = &e.provenance {
            if let Some(why) = provenance_problem(p) {
                problems.push(format!("edge `{from}` → `{to}`: {why}"));
            }
        }
        if !ok {
            continue;
        }
        let key = (from.to_owned(), to.to_owned(), relation.clone());
        if !edges.insert(key) {
            problems.push(format!(
                "edge `{from}` → `{to}` ({relation}) is submitted twice or already exists"
            ));
            continue;
        }
        // Only `requires` orders anything, so only it can close a loop.
        if relation == REQUIRES {
            if reaches(&deps, to, from) {
                problems.push(format!(
                    "edge `{from}` → `{to}` would close a prerequisite loop"
                ));
                continue;
            }
            deps.push((from.to_owned(), to.to_owned()));
        }
        let proven = stamped(e.provenance.as_ref(), &default_provenance);
        ops.push(GraphOp::edge(
            from,
            to,
            &relation,
            Some(&weight),
            e.why.as_deref(),
            Some(proven),
        ));
    }

    // ---- sameAs ---------------------------------------------------------
    for s in &submit.same_as {
        let (a, b) = (s.a.trim(), s.b.trim());
        if a.is_empty() || b.is_empty() {
            problems.push("a `sameAs` pair has an empty id".to_owned());
            continue;
        }
        if a == b {
            problems.push(format!("`sameAs` names the same id twice (`{a}`)"));
            continue;
        }
        for id in [a, b] {
            if !node_ids.contains(id) {
                problems.push(format!("`sameAs` names unknown node `{id}`"));
            }
        }
        ops.push(GraphOp::same_as(a, b));
    }

    // ---- chains ---------------------------------------------------------
    for c in &submit.chains {
        let premises: Vec<&str> = c
            .premises
            .iter()
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
            .collect();
        let conclusion = c.conclusion.trim();
        let chain = if c.chain_id.as_deref().unwrap_or("").trim().is_empty() {
            "a chain".to_owned()
        } else {
            format!("chain `{}`", c.chain_id.as_deref().unwrap_or("").trim())
        };
        if premises.is_empty() {
            problems.push(format!("{chain} has no premises"));
        }
        if conclusion.is_empty() {
            problems.push(format!("{chain} has no conclusion"));
        }
        if premises.contains(&conclusion) {
            problems.push(format!("{chain} counts its conclusion among its premises"));
        }
        for id in premises.iter().copied().chain(std::iter::once(conclusion)) {
            if map.is_tombstoned(id) {
                problems.push(format!("{chain} names tombstoned node `{id}`"));
            } else if !node_ids.contains(id) {
                problems.push(format!("{chain} names unknown node `{id}`"));
            }
        }
        let chain_id = c
            .chain_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(crate::sessions::op_id);
        ops.push(GraphOp::chain(
            &chain_id,
            &premises.iter().map(|p| (*p).to_owned()).collect::<Vec<_>>(),
            conclusion,
            c.statement.as_deref().unwrap_or(""),
        ));
    }

    // ---- splits ---------------------------------------------------------
    let mut splits: Vec<ProposedSplit> = Vec::new();
    for s in &submit.splits {
        let topic = s.topic.trim();
        if !topic_ids.contains(topic) {
            problems.push(format!("split of unknown topic `{topic}`"));
            continue;
        }
        if s.groups.is_empty() {
            problems.push(format!("split of `{topic}` has no groups"));
            continue;
        }
        let members: Vec<String> = homes.get(topic).cloned().unwrap_or_default();
        if members.len() <= super::MAX_TOPIC_NODES {
            problems.push(format!(
                "split of `{topic}`: it has {} nodes, and a split is only worth it over {}",
                members.len(),
                super::MAX_TOPIC_NODES
            ));
        }
        let mut placed: HashMap<String, usize> = HashMap::new();
        let mut groups: Vec<SplitGroup> = Vec::new();
        for (i, g) in s.groups.iter().enumerate() {
            let title = g.title.trim();
            if title.is_empty() {
                problems.push(format!("split of `{topic}`: group {} has no title", i + 1));
            }
            if g.node_ids.is_empty() {
                problems.push(format!("split of `{topic}`: group `{title}` is empty"));
            }
            let mut ids = Vec::new();
            for raw in &g.node_ids {
                let id = raw.trim();
                if !members.iter().any(|m| m == id) {
                    problems.push(format!(
                        "split of `{topic}`: group `{title}` names `{id}`, which is not homed in `{topic}`"
                    ));
                    continue;
                }
                *placed.entry(id.to_owned()).or_insert(0) += 1;
                ids.push(id.to_owned());
            }
            groups.push(SplitGroup {
                title: title.to_owned(),
                node_ids: ids,
                why: g.why.clone(),
            });
        }
        for (id, count) in &placed {
            if *count > 1 {
                problems.push(format!("split of `{topic}` puts `{id}` in {count} groups"));
            }
        }
        let missing: Vec<&String> = members
            .iter()
            .filter(|m| !placed.contains_key(*m))
            .collect();
        if !missing.is_empty() {
            problems.push(format!(
                "split of `{topic}` leaves {} node(s) out: {}",
                missing.len(),
                missing
                    .iter()
                    .map(|m| m.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        splits.push(ProposedSplit {
            topic: topic.to_owned(),
            groups,
        });
    }

    // ---- venues ---------------------------------------------------------
    // Where the field lives attaches to a topic, not a session: a venue names
    // the topic whose material it serves, and the read model aggregates it up
    // the tree (`docs/learning-system.md` §6).
    let mut submitted_venues: HashSet<(String, String)> = HashSet::new();
    for v in &submit.venues {
        let (topic, name) = (v.topic_id.trim(), v.name.trim());
        let label = if name.is_empty() {
            "a submitted venue".to_owned()
        } else if topic.is_empty() {
            format!("venue `{name}`")
        } else {
            format!("venue `{name}` on `{topic}`")
        };
        let mut ok = true;
        if name.is_empty() {
            problems.push("a submitted venue has an empty `name`".to_owned());
            ok = false;
        }
        if topic.is_empty() {
            problems.push(format!("{label} has an empty `topicId`"));
            ok = false;
        } else if !topic_ids.contains(topic) {
            problems.push(format!("{label} names unknown topic `{topic}`"));
            ok = false;
        }
        if v.entry_level.trim().is_empty() {
            problems.push(format!("{label} has an empty `entryLevel`"));
            ok = false;
        }
        let kind = super::normal_venue_kind(&v.kind);
        if !VENUE_KINDS.contains(&kind.as_str()) {
            problems.push(format!(
                "{label} has kind `{}` (must be {})",
                v.kind.trim(),
                VENUE_KINDS.join(", ")
            ));
            ok = false;
        }
        if let Some(p) = &v.provenance {
            if let Some(why) = provenance_problem(p) {
                problems.push(format!("{label}: {why}"));
            }
        }
        if !ok {
            continue;
        }
        if !submitted_venues.insert((topic.to_owned(), name.to_lowercase())) {
            problems.push(format!("{label} is submitted twice"));
            continue;
        }
        let proven = v
            .provenance
            .clone()
            .unwrap_or_else(|| default_provenance.clone());
        ops.push(GraphOp::venue(
            topic,
            &kind,
            name,
            v.url.as_deref(),
            &v.entry_level,
            v.note.as_deref().unwrap_or(""),
            v.checked.as_deref(),
            Some(proven),
        ));
    }

    if problems.is_empty() {
        return Ok(Batch { ops, splits });
    }
    problems.sort();
    problems.dedup();
    Err(problems)
}

fn provenance_problem(p: &Value) -> Option<String> {
    let kind = p.get("type").and_then(Value::as_str)?;
    if PROVENANCE_TYPES.contains(&kind) {
        None
    } else {
        Some(format!(
            "provenance type `{kind}` (must be {})",
            PROVENANCE_TYPES.join(", ")
        ))
    }
}

/// The model's provenance with what the run knows filled in: a submission
/// that says only `{"type": "hunt"}` still names the session it came from and
/// when, so the Map can link the claim back to that session. Fields the model
/// set are kept; no provenance at all is the run's own (`builder`).
fn stamped(submitted: Option<&Value>, default: &Value) -> Value {
    let Some(Value::Object(fields)) = submitted else {
        return default.clone();
    };
    let mut out = fields.clone();
    if let Value::Object(base) = default {
        for key in ["sessionId", "at"] {
            if let Some(v) = base.get(key) {
                if !out.contains_key(key) {
                    out.insert(key.to_owned(), v.clone());
                }
            }
        }
    }
    Value::Object(out)
}

/// Whether `from` can already reach `to` through `requires` edges — the cycle
/// test. `contrasts`, `supports` and `attacks` claim no order and are never
/// passed in.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::{KnowledgeEdge, KnowledgeNode, KnowledgeTopic};
    use serde_json::json;

    const AT: &str = "2026-11-07T00:00:00.000Z";

    fn node(id: &str, home: &str) -> KnowledgeNode {
        KnowledgeNode {
            id: id.to_owned(),
            title: id.to_owned(),
            statement: format!("statement of {id}"),
            kind: Kind::Fact,
            home: home.to_owned(),
            correct: 0,
            missed: 0,
            provenance: None,
            attribution: None,
            scope: None,
            aliases: Vec::new(),
        }
    }

    /// A graph with one topic `t` and nodes `a` and `b`, built the way `load`
    /// builds one (index included).
    fn map() -> KnowledgeMap {
        let topics = vec![KnowledgeTopic {
            id: "t".to_owned(),
            title: "T".to_owned(),
            parent: None,
        }];
        let nodes = vec![node("a", "t"), node("b", "t")];
        let edges = vec![KnowledgeEdge {
            from: "a".to_owned(),
            to: "b".to_owned(),
            relation: REQUIRES.to_owned(),
            weight: "load-bearing".to_owned(),
            why: None,
            provenance: None,
        }];
        let index = super::super::build_index(
            &topics,
            &nodes,
            &edges,
            nodes.iter().map(|n| (n.id.clone(), n.id.clone())).collect(),
            HashSet::new(),
        );
        KnowledgeMap {
            topics,
            nodes,
            edges,
            chains: Vec::new(),
            splits_proposed: Vec::new(),
            topic_checks: Default::default(),
            index,
            ..Default::default()
        }
    }

    fn problems(args: Value) -> Vec<String> {
        check(&args, &map(), None, AT).unwrap_err()
    }

    fn accepted(args: Value) -> Batch {
        check(&args, &map(), Some("s1"), AT).unwrap()
    }

    #[test]
    fn an_empty_submission_is_accepted_and_adds_nothing() {
        assert!(accepted(json!({})).is_empty());
        assert!(accepted(json!({"topics":[],"nodes":[],"edges":[]})).is_empty());
    }

    #[test]
    fn a_good_submission_carries_the_builder_provenance() {
        let batch = accepted(json!({
            "topics": [{"topicId": "t2", "title": "Two", "parent": "t"}],
            "nodes": [{"nodeId": "c", "statement": "c holds", "kind": "mechanism", "home": "t2"}],
            "edges": [{"from": "b", "to": "c", "weight": "trivial", "why": "uses it"}],
            "chains": [{"premises": ["a", "b"], "conclusion": "c", "statement": "because"}],
        }));
        assert_eq!(batch.ops.len(), 4);
        let json = serde_json::to_string(&batch.ops).unwrap();
        assert!(json.contains(r#""op":"topic""#), "{json}");
        assert!(json.contains(r#""op":"node""#), "{json}");
        assert!(json.contains(r#""op":"edge""#), "{json}");
        assert!(json.contains(r#""op":"chain""#), "{json}");
        assert!(json.contains(r#""type":"builder""#), "{json}");
        assert!(json.contains(r#""sessionId":"s1""#), "{json}");
        // A submission with one bad id lands nothing: the same shape plus an
        // unknown `sameAs` is refused as a whole.
        let refused = check(
            &json!({
                "topics": [{"topicId": "t2", "title": "Two", "parent": "t"}],
                "nodes": [{"nodeId": "c", "statement": "c holds", "kind": "mechanism", "home": "t2"}],
                "sameAs": [{"a": "c", "b": "c2"}],
            }),
            &map(),
            Some("s1"),
            AT,
        );
        assert!(refused.is_err());
    }

    #[test]
    fn a_submitted_provenance_keeps_its_fields_and_gains_the_session() {
        let batch = accepted(json!({
            "nodes": [{"nodeId": "c", "statement": "c holds", "kind": "fact", "home": "t",
                "provenance": {"type": "hunt", "huntId": "h1", "quote": "c holds, says the forum"}}],
        }));
        let GraphOp::Node {
            provenance: Some(p),
            ..
        } = &batch.ops[0]
        else {
            panic!("{:?}", batch.ops);
        };
        assert_eq!(p["type"], "hunt", "the model's type wins");
        assert_eq!(p["quote"], "c holds, says the forum");
        assert_eq!(p["sessionId"], "s1", "the run's session fills the gap");
        assert_eq!(p["at"], AT);
    }

    #[test]
    fn unknown_ids_are_refused() {
        let p = problems(
            json!({"nodes": [{"nodeId": "c", "statement": "s", "kind": "fact", "home": "nope"}]}),
        );
        assert!(p.iter().any(|x| x.contains("unknown home `nope`")), "{p:?}");
        let p = problems(json!({"topics": [{"topicId": "t2", "title": "T2", "parent": "nope"}]}));
        assert!(
            p.iter().any(|x| x.contains("unknown parent `nope`")),
            "{p:?}"
        );
        let p = problems(json!({"edges": [{"from": "a", "to": "ghost"}]}));
        assert!(
            p.iter().any(|x| x.contains("unknown node `ghost`")),
            "{p:?}"
        );
        let p = problems(json!({"chains": [{"premises": ["ghost"], "conclusion": "a"}]}));
        assert!(
            p.iter().any(|x| x.contains("unknown node `ghost`")),
            "{p:?}"
        );
        let p = problems(
            json!({"splits": [{"topic": "nope", "groups": [{"title": "x", "nodeIds": ["a"]}]}]}),
        );
        assert!(
            p.iter().any(|x| x.contains("unknown topic `nope`")),
            "{p:?}"
        );
        let p = problems(
            json!({"venues": [{"topicId": "nope", "kind": "ask", "name": "A place", "entryLevel": "anyone"}]}),
        );
        assert!(
            p.iter().any(|x| x.contains("unknown topic `nope`")),
            "{p:?}"
        );
    }

    #[test]
    fn venues_are_checked_for_topic_kind_and_the_two_required_fields() {
        // A venue attaches to a topic that exists (or is submitted here).
        let batch = accepted(json!({
            "topics": [{"topicId": "t2", "title": "Two", "parent": "t"}],
            "venues": [
                {"topicId": "t2", "kind": "ask", "name": "Math StackExchange",
                 "url": "https://math.stackexchange.com/", "entryLevel": "anyone",
                 "note": "ask anything", "checked": "2026-11-07"},
                {"topicId": "t", "kind": "discussion", "name": "MathOverflow",
                 "entryLevel": "one variable calculus"},
            ],
        }));
        assert_eq!(batch.ops.len(), 3);
        let json = serde_json::to_string(&batch.ops).unwrap();
        assert!(json.contains(r#""op":"venue""#), "{json}");
        assert!(
            json.contains(r#""type":"builder""#) && json.contains(r#""sessionId":"s1""#),
            "{json}"
        );

        // An unknown kind, an empty name and an empty entry level all come
        // back at once, alongside the unknown-topic problem.
        let p = problems(json!({"venues": [
            {"topicId": "ghost", "kind": "ask", "name": "A", "entryLevel": "anyone"},
            {"topicId": "t", "kind": "forum", "name": "B", "entryLevel": "anyone"},
            {"topicId": "t", "kind": "ask", "name": "", "entryLevel": "anyone"},
            {"topicId": "t", "kind": "ask", "name": "C", "entryLevel": ""},
        ]}));
        assert!(
            p.iter().any(|x| x.contains("unknown topic `ghost`")),
            "{p:?}"
        );
        assert!(p.iter().any(|x| x.contains("kind `forum`")), "{p:?}");
        assert!(p.iter().any(|x| x.contains("empty `name`")), "{p:?}");
        assert!(p.iter().any(|x| x.contains("empty `entryLevel`")), "{p:?}");

        // One `(topicId, name)` twice in one submission is ambiguous, so it is
        // refused rather than silently deduped.
        let p = problems(json!({"venues": [
            {"topicId": "t", "kind": "ask", "name": "Same", "entryLevel": "anyone"},
            {"topicId": "t", "kind": "discussion", "name": "same", "entryLevel": "anyone"},
        ]}));
        assert!(p.iter().any(|x| x.contains("submitted twice")), "{p:?}");
    }

    #[test]
    fn bad_enums_and_an_undescribed_heuristic_are_refused() {
        let p = problems(
            json!({"nodes": [{"nodeId": "c", "statement": "s", "kind": "vibe", "home": "t"}]}),
        );
        assert!(p.iter().any(|x| x.contains("kind `vibe`")), "{p:?}");
        let p = problems(json!({"edges": [{"from": "b", "to": "a", "relation": "mentions"}]}));
        assert!(p.iter().any(|x| x.contains("relation `mentions`")), "{p:?}");
        let p = problems(json!({"edges": [{"from": "b", "to": "a", "weight": "heavy"}]}));
        assert!(p.iter().any(|x| x.contains("weight `heavy`")), "{p:?}");
        // A heuristic needs both fields, and both problems come back at once.
        let p = problems(
            json!({"nodes": [{"nodeId": "h", "statement": "bypass the pin", "kind": "heuristic", "home": "t"}]}),
        );
        assert_eq!(p.len(), 2, "{p:?}");
        assert!(
            p.iter().any(|x| x.contains("without `attribution`")),
            "{p:?}"
        );
        assert!(p.iter().any(|x| x.contains("without `scope`")), "{p:?}");
        // With both, it is accepted.
        let batch = accepted(
            json!({"nodes": [{"nodeId": "h", "statement": "bypass the pin", "kind": "heuristic", "home": "t",
                              "attribution": "design guides", "scope": "digital boards"}]}),
        );
        assert_eq!(batch.ops.len(), 1);
        assert!(serde_json::to_string(&batch.ops)
            .unwrap()
            .contains(r#""kind":"heuristic""#));
    }

    #[test]
    fn a_self_edge_and_a_duplicate_are_refused() {
        let p = problems(json!({"edges": [{"from": "a", "to": "a"}]}));
        assert!(p.iter().any(|x| x.contains("self-edge")), "{p:?}");
        let p = problems(json!({"edges": [{"from": "a", "to": "b"}]}));
        assert!(p.iter().any(|x| x.contains("already exists")), "{p:?}");
        let p = problems(json!({"edges": [{"from": "b", "to": "a"}, {"from": "b", "to": "a"}]}));
        assert!(p.iter().any(|x| x.contains("twice")), "{p:?}");
    }

    #[test]
    fn a_requires_loop_is_refused_and_the_other_relations_are_not() {
        // a → b exists, so b → a would loop.
        let p = problems(json!({"edges": [{"from": "b", "to": "a"}]}));
        assert!(
            p.iter().any(|x| x.contains("close a prerequisite loop")),
            "{p:?}"
        );
        // The same pair as a contrast, a support or an attack is fine.
        let batch = accepted(json!({"edges": [
            {"from": "b", "to": "a", "relation": "contrasts"},
            {"from": "b", "to": "a", "relation": "supports"},
            {"from": "b", "to": "a", "relation": "attacks"}
        ]}));
        assert_eq!(batch.ops.len(), 3);
        // Two proposals that would loop together: the second is refused.
        let p = problems(json!({
            "nodes": [{"nodeId": "c", "statement": "s", "kind": "fact", "home": "t"}],
            "edges": [{"from": "b", "to": "c"}, {"from": "c", "to": "a"}]
        }));
        assert!(p.iter().any(|x| x.contains("c` → `a")), "{p:?}");
    }

    #[test]
    fn a_chain_needs_premises_and_must_not_contain_its_conclusion() {
        let p = problems(json!({"chains": [{"premises": [], "conclusion": "a"}]}));
        assert!(p.iter().any(|x| x.contains("no premises")), "{p:?}");
        let p = problems(json!({"chains": [{"premises": ["a", "b"], "conclusion": "a"}]}));
        assert!(
            p.iter()
                .any(|x| x.contains("conclusion among its premises")),
            "{p:?}"
        );
    }

    #[test]
    fn a_tombstoned_node_cannot_join_an_edge_or_a_chain() {
        let mut m = map();
        m.index.tombstoned.insert("b".to_owned());
        m.nodes.retain(|n| n.id != "b");
        let p = check(&json!({"edges": [{"from": "a", "to": "b"}]}), &m, None, AT).unwrap_err();
        assert!(p.iter().any(|x| x.contains("tombstoned")), "{p:?}");
        let p = check(
            &json!({"chains": [{"premises": ["b"], "conclusion": "a"}]}),
            &m,
            None,
            AT,
        )
        .unwrap_err();
        assert!(p.iter().any(|x| x.contains("tombstoned")), "{p:?}");
    }

    #[test]
    fn a_bad_provenance_type_is_refused() {
        let p = problems(json!({
            "nodes": [{"nodeId": "c", "statement": "s", "kind": "fact", "home": "t",
                       "provenance": {"type": "guessed"}}]
        }));
        assert!(
            p.iter().any(|x| x.contains("provenance type `guessed`")),
            "{p:?}"
        );
    }

    #[test]
    fn an_empty_id_or_statement_is_refused() {
        let p = problems(
            json!({"nodes": [{"nodeId": " ", "statement": "s", "kind": "fact", "home": "t"}]}),
        );
        assert!(p.iter().any(|x| x.contains("empty `nodeId`")), "{p:?}");
        let p = problems(
            json!({"nodes": [{"nodeId": "c", "statement": "  ", "kind": "fact", "home": "t"}]}),
        );
        assert!(p.iter().any(|x| x.contains("empty `statement`")), "{p:?}");
        let p = problems(json!({"topics": [{"topicId": "", "title": "T"}]}));
        assert!(p.iter().any(|x| x.contains("empty `topicId`")), "{p:?}");
    }

    /// A topic `big` with `n` nodes, half of them chained.
    fn big_map(n: usize) -> KnowledgeMap {
        let topics = vec![KnowledgeTopic {
            id: "big".to_owned(),
            title: "Big".to_owned(),
            parent: None,
        }];
        let nodes: Vec<KnowledgeNode> = (0..n).map(|i| node(&format!("n{i}"), "big")).collect();
        let mut edges = Vec::new();
        // Two components: n0..n(n/2-1) and n(n/2)..n(n-1), each a chain.
        for start in [0usize, n / 2] {
            for i in start..(start + n / 2 - 1) {
                edges.push(KnowledgeEdge {
                    from: format!("n{i}"),
                    to: format!("n{}", i + 1),
                    relation: REQUIRES.to_owned(),
                    weight: "load-bearing".to_owned(),
                    why: None,
                    provenance: None,
                });
            }
        }
        let index = super::super::build_index(
            &topics,
            &nodes,
            &edges,
            nodes.iter().map(|n| (n.id.clone(), n.id.clone())).collect(),
            HashSet::new(),
        );
        KnowledgeMap {
            topics,
            nodes,
            edges,
            chains: Vec::new(),
            splits_proposed: Vec::new(),
            topic_checks: Default::default(),
            index,
            ..Default::default()
        }
    }

    fn groups_of(n: usize) -> Value {
        let half = n / 2;
        json!([
            {"title": "One", "nodeIds": (0..half).map(|i| format!("n{i}")).collect::<Vec<_>>()},
            {"title": "Two", "nodeIds": (half..n).map(|i| format!("n{i}")).collect::<Vec<_>>()}
        ])
    }

    #[test]
    fn a_split_must_partition_the_topic() {
        let m = big_map(34);
        let groups = groups_of(34);
        assert!(check(
            &json!({"splits": [{"topic": "big", "groups": groups}]}),
            &m,
            None,
            AT
        )
        .is_ok());

        // A node left out.
        let mut partial = groups.clone();
        partial[1]["nodeIds"].as_array_mut().unwrap().pop();
        let p = check(
            &json!({"splits": [{"topic": "big", "groups": partial}]}),
            &m,
            None,
            AT,
        )
        .unwrap_err();
        assert!(
            p.iter().any(|x| x.contains("leaves 1 node(s) out")),
            "{p:?}"
        );

        // A node in two groups.
        let mut twice = groups.clone();
        twice[0]["nodeIds"]
            .as_array_mut()
            .unwrap()
            .push(json!("n17"));
        let p = check(
            &json!({"splits": [{"topic": "big", "groups": twice}]}),
            &m,
            None,
            AT,
        )
        .unwrap_err();
        assert!(p.iter().any(|x| x.contains("in 2 groups")), "{p:?}");

        // An empty group.
        let mut empty = groups.clone();
        empty
            .as_array_mut()
            .unwrap()
            .push(json!({"title": "Three", "nodeIds": []}));
        let p = check(
            &json!({"splits": [{"topic": "big", "groups": empty}]}),
            &m,
            None,
            AT,
        )
        .unwrap_err();
        assert!(p.iter().any(|x| x.contains("is empty")), "{p:?}");

        // A node from another topic.
        let mut foreign = groups.clone();
        foreign[0]["nodeIds"]
            .as_array_mut()
            .unwrap()
            .push(json!("elsewhere"));
        let p = check(
            &json!({"splits": [{"topic": "big", "groups": foreign}]}),
            &m,
            None,
            AT,
        )
        .unwrap_err();
        assert!(p.iter().any(|x| x.contains("not homed in `big`")), "{p:?}");
    }

    #[test]
    fn a_small_topic_is_never_split() {
        let m = big_map(20);
        let groups = groups_of(20);
        let p = check(
            &json!({"splits": [{"topic": "big", "groups": groups}]}),
            &m,
            None,
            AT,
        )
        .unwrap_err();
        assert!(p.iter().any(|x| x.contains("over 30")), "{p:?}");
    }
}
