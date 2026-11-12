//! The knowledge graph as an append-only op log (architecture §4.2–§4.7).
//!
//! Structure lives in `graph/ops.jsonl`: one JSON object per line, every op
//! carrying `op`, `opId` (ULID) and `at` (ISO-8601). Folding the log, ordered
//! by `(at, opId)`, gives the read model in [`crate::knowledge`]. Nothing here
//! rewrites a line: a correction is another op.
//!
//! A store made by the older files (`topics.json`, `nodes.jsonl`,
//! `edges.jsonl`) is migrated once, on the first read, into `ops.jsonl`; the
//! legacy files are then left exactly as they were and never written again.
//! `graph/checks.jsonl` is learner evidence, not structure, and keeps its own
//! reader in [`crate::knowledge`].
//!
//! A malformed op line is skipped and counted, and a chain whose premises or
//! conclusion no longer exist (or were tombstoned) is dropped and logged: the
//! log is model output plus accepted review, so one bad line must never make
//! the whole map unreadable.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::knowledge::{
    KnowledgeChain, KnowledgeEdge, KnowledgeMap, KnowledgeNode, KnowledgeTopic, KnowledgeVenue,
    ProposedSplit, SplitGroup, CONTRASTS, REQUIRES,
};
use crate::state::Kind;
use crate::Error;

pub mod history;
pub mod read;
pub mod validate;

pub use validate::{check, Batch, Submit};

/// The op log: the only structure source.
pub const OPS: &str = "graph/ops.jsonl";
/// Topic-split proposals awaiting a learner decision.
pub const SPLITS: &str = "graph/splits.proposed.json";
/// Legacy files, read once by [`migrate`] and never written again.
const LEGACY_TOPICS: &str = "graph/topics.json";
const LEGACY_NODES: &str = "graph/nodes.jsonl";
const LEGACY_EDGES: &str = "graph/edges.jsonl";

/// A topic with more nodes than this is a split candidate.
pub const MAX_TOPIC_NODES: usize = 30;
/// A component smaller than this is not worth its own topic.
pub const MIN_SPLIT_GROUP: usize = 5;
/// Most nodes [`summary`] lists for the builder.
pub const MAX_SUMMARY_NODES: usize = 200;
/// Cap on the rendered topic tree + node list.
pub const MAX_SUMMARY_CHARS: usize = 6000;
/// Most candidates one `graph_search` returns.
pub const MAX_SEARCH_HITS: usize = 20;

/// Where an op's claim came from: `session`, `import`, `hunt`, `learner` or
/// `builder`.
pub const PROVENANCE_TYPES: [&str; 5] = ["session", "import", "hunt", "learner", "builder"];

/// The relations an edge may carry. Only `requires` orders anything.
pub const RELATIONS: [&str; 4] = [REQUIRES, CONTRASTS, "supports", "attacks"];

/// Where a topic's material lives (`docs/learning-system.md` §6): asking works
/// at every level, discussion and reference venues unlock with their entry
/// level.
pub const VENUE_KINDS: [&str; 3] = ["ask", "discussion", "reference"];

/// A venue kind as stored: trimmed and lowercased, so a hand-written `Ask` is
/// still an `ask`.
pub fn normal_venue_kind(s: &str) -> String {
    s.trim().to_lowercase()
}

/// The read model's venue order: ask, then discussion, then reference; by name
/// within a kind. Sorting the own list means every per-topic view (built by
/// filtering it) already carries that order.
fn venue_order(a: &KnowledgeVenue, b: &KnowledgeVenue) -> std::cmp::Ordering {
    let rank = |kind: &str| VENUE_KINDS.iter().position(|k| *k == kind).unwrap_or(3);
    rank(&a.kind)
        .cmp(&rank(&b.kind))
        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        .then_with(|| a.topic_id.cmp(&b.topic_id))
}

/// Most venues [`summary`] lists for the builder.
const MAX_SUMMARY_VENUES: usize = 400;

/// `load-bearing` | `trivial`; meaningful only for `requires`.
pub const WEIGHTS: [&str; 2] = ["load-bearing", "trivial"];

/// The default weight of an edge written before the field existed, or named
/// without one.
pub fn load_bearing() -> String {
    "load-bearing".to_owned()
}

/// A relation as stored: trimmed, blank read as the prerequisite the older
/// format meant.
fn normal_relation(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() {
        REQUIRES.to_owned()
    } else {
        s.to_owned()
    }
}

fn normal_weight(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() {
        load_bearing()
    } else {
        s.to_owned()
    }
}

/// `{"type": …}` on every op that carries a claim, so a reader can tell a
/// session's propositions from an import's or the learner's own.
pub fn provenance(kind: &str, session_id: Option<&str>, at: &str) -> Value {
    let mut v = serde_json::Map::new();
    v.insert("type".into(), Value::String(kind.to_owned()));
    if let Some(sid) = session_id {
        v.insert("sessionId".into(), Value::String(sid.to_owned()));
    }
    v.insert("at".into(), Value::String(at.to_owned()));
    Value::Object(v)
}

/// One line of `graph/ops.jsonl`.
///
/// Serde's internally tagged enum gives exactly the documented shape:
/// `{"op":"node","opId":…,"at":…,"nodeId":…}`. Optional payload fields are
/// omitted rather than written as `null`, so an upsert that only renames a
/// title does not clear what it didn't name.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum GraphOp {
    /// Upsert a topic. `parent` absent keeps the topic's current parent.
    Topic {
        op_id: String,
        at: String,
        topic_id: String,
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
    },
    /// Upsert a proposition.
    Node {
        op_id: String,
        at: String,
        node_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        statement: String,
        kind: Kind,
        home: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provenance: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attribution: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        correct: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        missed: Option<u32>,
    },
    /// Move a node. `by: "learner"` outranks any later `by: "builder"`.
    /// `provenance` says who moved it and when (`{"type": "learner", "at"}`).
    Place {
        op_id: String,
        at: String,
        node_id: String,
        home: String,
        by: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provenance: Option<Value>,
    },
    /// Upsert the edge addressed by `(from, to, relation)`.
    Edge {
        op_id: String,
        at: String,
        from: String,
        to: String,
        relation: String,
        #[serde(default = "load_bearing")]
        weight: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        why: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provenance: Option<Value>,
    },
    /// The two ids are one proposition; fold into the earlier one.
    SameAs {
        op_id: String,
        at: String,
        a: String,
        b: String,
    },
    /// A multi-premise argument: the conclusion follows from the premises
    /// together.
    Chain {
        op_id: String,
        at: String,
        chain_id: String,
        premises: Vec<String>,
        conclusion: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        statement: String,
    },
    /// The claim was wrong or merged away; readers ignore it and its edges.
    Tombstone {
        op_id: String,
        at: String,
        node_id: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        why: String,
    },
    /// An accepted topic split: create the child topics and move the topic's
    /// own nodes into them. Accepting is the learner's act, so those moves
    /// rank as learner placements; `provenance` is `{"type": "learner"}`.
    Split {
        op_id: String,
        at: String,
        topic: String,
        groups: Vec<SplitGroup>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provenance: Option<Value>,
    },
    /// Where a topic's material lives: a forum to ask, a place people discuss,
    /// a reference to read. Deduped by `(topicId, name)`, last writer wins.
    Venue {
        op_id: String,
        at: String,
        topic_id: String,
        kind: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        entry_level: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        note: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checked: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provenance: Option<Value>,
    },
}

fn stamp() -> (String, String) {
    (crate::sessions::op_id(), crate::now_iso())
}

impl GraphOp {
    pub fn at(&self) -> &str {
        match self {
            GraphOp::Topic { at, .. }
            | GraphOp::Node { at, .. }
            | GraphOp::Place { at, .. }
            | GraphOp::Edge { at, .. }
            | GraphOp::SameAs { at, .. }
            | GraphOp::Chain { at, .. }
            | GraphOp::Tombstone { at, .. }
            | GraphOp::Split { at, .. }
            | GraphOp::Venue { at, .. } => at,
        }
    }

    pub fn op_id(&self) -> &str {
        match self {
            GraphOp::Topic { op_id, .. }
            | GraphOp::Node { op_id, .. }
            | GraphOp::Place { op_id, .. }
            | GraphOp::Edge { op_id, .. }
            | GraphOp::SameAs { op_id, .. }
            | GraphOp::Chain { op_id, .. }
            | GraphOp::Tombstone { op_id, .. }
            | GraphOp::Split { op_id, .. }
            | GraphOp::Venue { op_id, .. } => op_id,
        }
    }

    pub fn topic(topic_id: &str, title: &str, parent: Option<&str>) -> Self {
        let (op_id, at) = stamp();
        GraphOp::Topic {
            op_id,
            at,
            topic_id: topic_id.trim().to_owned(),
            title: title.trim().to_owned(),
            parent: parent
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(str::to_owned),
        }
    }

    pub fn node(
        node_id: &str,
        title: Option<&str>,
        statement: &str,
        kind: Kind,
        home: &str,
        proven: Option<Value>,
    ) -> Self {
        let (op_id, at) = stamp();
        GraphOp::Node {
            op_id,
            at,
            node_id: node_id.trim().to_owned(),
            title: title
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_owned),
            statement: statement.trim().to_owned(),
            kind,
            home: home.trim().to_owned(),
            provenance: proven,
            attribution: None,
            scope: None,
            correct: None,
            missed: None,
        }
    }

    /// `kind: heuristic` needs both of these; validation refuses a heuristic
    /// without them.
    pub fn described(mut self, attribution: Option<&str>, scope: Option<&str>) -> Self {
        if let GraphOp::Node {
            attribution: a,
            scope: s,
            ..
        } = &mut self
        {
            *a = attribution
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(str::to_owned);
            *s = scope
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(str::to_owned);
        }
        self
    }

    pub fn with_evidence(mut self, correct: Option<u32>, missed: Option<u32>) -> Self {
        if let GraphOp::Node {
            correct: c,
            missed: m,
            ..
        } = &mut self
        {
            *c = correct;
            *m = missed;
        }
        self
    }

    /// A placement by `by` (`learner` or `builder`), stamped with a
    /// provenance of that type.
    pub fn place(node_id: &str, home: &str, by: &str) -> Self {
        let (op_id, at) = stamp();
        GraphOp::Place {
            provenance: Some(provenance(by, None, &at)),
            op_id,
            at,
            node_id: node_id.trim().to_owned(),
            home: home.trim().to_owned(),
            by: by.to_owned(),
        }
    }

    pub fn edge(
        from: &str,
        to: &str,
        relation: &str,
        weight: Option<&str>,
        why: Option<&str>,
        proven: Option<Value>,
    ) -> Self {
        let (op_id, at) = stamp();
        GraphOp::Edge {
            op_id,
            at,
            from: from.trim().to_owned(),
            to: to.trim().to_owned(),
            relation: normal_relation(relation),
            weight: normal_weight(weight.unwrap_or("")),
            why: why
                .map(str::trim)
                .filter(|w| !w.is_empty())
                .map(str::to_owned),
            provenance: proven,
        }
    }

    pub fn same_as(a: &str, b: &str) -> Self {
        let (op_id, at) = stamp();
        GraphOp::SameAs {
            op_id,
            at,
            a: a.trim().to_owned(),
            b: b.trim().to_owned(),
        }
    }

    pub fn chain(chain_id: &str, premises: &[String], conclusion: &str, statement: &str) -> Self {
        let (op_id, at) = stamp();
        GraphOp::Chain {
            op_id,
            at,
            chain_id: chain_id.trim().to_owned(),
            premises: premises
                .iter()
                .map(|p| p.trim().to_owned())
                .filter(|p| !p.is_empty())
                .collect(),
            conclusion: conclusion.trim().to_owned(),
            statement: statement.trim().to_owned(),
        }
    }

    pub fn tombstone(node_id: &str, why: &str) -> Self {
        let (op_id, at) = stamp();
        GraphOp::Tombstone {
            op_id,
            at,
            node_id: node_id.trim().to_owned(),
            why: why.trim().to_owned(),
        }
    }

    /// The split the learner accepted.
    pub fn split(topic: &str, groups: Vec<SplitGroup>) -> Self {
        let (op_id, at) = stamp();
        GraphOp::Split {
            provenance: Some(provenance("learner", None, &at)),
            op_id,
            at,
            topic: topic.trim().to_owned(),
            groups,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn venue(
        topic_id: &str,
        kind: &str,
        name: &str,
        url: Option<&str>,
        entry_level: &str,
        note: &str,
        checked: Option<&str>,
        proven: Option<Value>,
    ) -> Self {
        let (op_id, at) = stamp();
        GraphOp::Venue {
            op_id,
            at,
            topic_id: topic_id.trim().to_owned(),
            kind: normal_venue_kind(kind),
            name: name.trim().to_owned(),
            url: url
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .map(str::to_owned),
            entry_level: entry_level.trim().to_owned(),
            note: note.trim().to_owned(),
            checked: checked
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .map(str::to_owned),
            provenance: proven,
        }
    }
}

// ---------------------------------------------------------------------------
// Reading and writing the log
// ---------------------------------------------------------------------------

/// Every op in `graph/ops.jsonl`, and how many lines were skipped as
/// malformed. Migrates a legacy store first (see [`migrate`]); a graph
/// directory that doesn't exist is an empty log, never an error.
pub fn read_ops(repo: &Path) -> Result<(Vec<GraphOp>, usize), Error> {
    migrate(repo)?;
    let path = repo.join(OPS);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok((Vec::new(), 0));
    };
    let mut ops = Vec::new();
    let mut skipped = 0usize;
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<GraphOp>(line) {
            Ok(op) => ops.push(op),
            Err(e) => {
                skipped += 1;
                tracing::warn!("graph/ops.jsonl:{}: skipping a malformed op: {e}", i + 1);
            }
        }
    }
    Ok((ops, skipped))
}

/// Append ops to the log (created on first use). Append-only: an existing
/// line is never rewritten.
pub fn append(repo: &Path, ops: &[GraphOp]) -> Result<(), Error> {
    if ops.is_empty() {
        return Ok(());
    }
    // A legacy store must be folded in before the first line is added, or the
    // log would shadow the files it was supposed to replace.
    migrate(repo)?;
    let path = repo.join(OPS);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut body = String::new();
    for op in ops {
        body.push_str(&serde_json::to_string(op)?);
        body.push('\n');
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    file.write_all(body.as_bytes())?;
    Ok(())
}

/// The one-time migration: when `graph/ops.jsonl` is absent and any legacy
/// file exists, synthesize the ops it describes, write the log, and leave the
/// legacy files untouched. Idempotent — a second call finds the log and does
/// nothing.
pub fn migrate(repo: &Path) -> Result<bool, Error> {
    let dir = repo.join("graph");
    let ops_path = dir.join("ops.jsonl");
    if ops_path.is_file() || !dir.is_dir() {
        return Ok(false);
    }
    let topics_path = repo.join(LEGACY_TOPICS);
    let nodes_path = repo.join(LEGACY_NODES);
    let edges_path = repo.join(LEGACY_EDGES);
    if !topics_path.is_file() && !nodes_path.is_file() && !edges_path.is_file() {
        return Ok(false);
    }

    let mut ops = Vec::new();
    if topics_path.is_file() {
        let text = std::fs::read_to_string(&topics_path)?;
        let file: TopicsFile = serde_json::from_str(&text)
            .map_err(|e| Error::Corrupt(format!("{LEGACY_TOPICS}: {e}")))?;
        for t in file.topics {
            ops.push(GraphOp::topic(&t.id, &t.title, t.parent.as_deref()));
        }
    }
    for n in read_legacy::<KnowledgeNode>(&nodes_path, LEGACY_NODES)? {
        ops.push(
            GraphOp::node(
                &n.id,
                Some(&n.title),
                &n.statement,
                n.kind,
                &n.home,
                Some(provenance("import", None, &crate::now_iso())),
            )
            .with_evidence(Some(n.correct), Some(n.missed)),
        );
    }
    for e in read_legacy::<KnowledgeEdge>(&edges_path, LEGACY_EDGES)? {
        ops.push(GraphOp::edge(
            &e.from,
            &e.to,
            &e.relation,
            Some(&e.weight),
            e.why.as_deref(),
            Some(provenance("import", None, &crate::now_iso())),
        ));
    }
    if ops.is_empty() {
        return Ok(false);
    }
    let mut body = String::new();
    for op in &ops {
        body.push_str(&serde_json::to_string(op)?);
        body.push('\n');
    }
    // Write through a temp file so a reader never sees half a log; a log that
    // appeared meanwhile wins (the other writer folded the same legacy files).
    let tmp = dir.join("ops.jsonl.tmp");
    std::fs::write(&tmp, body)?;
    if ops_path.exists() {
        let _ = std::fs::remove_file(&tmp);
        return Ok(false);
    }
    std::fs::rename(&tmp, &ops_path)?;
    tracing::info!(
        ops = ops.len(),
        "graph: migrated the legacy topics/nodes/edges store into graph/ops.jsonl"
    );
    Ok(true)
}

#[derive(Deserialize)]
struct TopicsFile {
    #[serde(default)]
    topics: Vec<KnowledgeTopic>,
}

fn read_legacy<T: for<'de> Deserialize<'de>>(path: &Path, label: &str) -> Result<Vec<T>, Error> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        out.push(
            serde_json::from_str(line)
                .map_err(|e| Error::Corrupt(format!("{label}:{}: {e}", i + 1)))?,
        );
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Fold
// ---------------------------------------------------------------------------

struct NodeRow {
    node: KnowledgeNode,
    order: usize,
    learner_home: bool,
    tombstoned: bool,
}

fn find_alias(alias: &HashMap<String, String>, id: &str) -> String {
    let mut current = id;
    for _ in 0..64 {
        match alias.get(current) {
            Some(next) => current = next,
            None => break,
        }
    }
    current.to_owned()
}

/// Fold the log into the read model (ordered by `(at, opId)`), with the
/// derived index built. Chains that no longer validate are dropped and logged.
pub fn fold(ops: &[GraphOp]) -> KnowledgeMap {
    let mut sorted: Vec<&GraphOp> = ops.iter().collect();
    sorted.sort_by(|a, b| (a.at(), a.op_id()).cmp(&(b.at(), b.op_id())));

    let mut topics: Vec<KnowledgeTopic> = Vec::new();
    let mut topic_at: HashMap<String, usize> = HashMap::new();
    let mut rows: Vec<NodeRow> = Vec::new();
    let mut node_at: HashMap<String, usize> = HashMap::new();
    let mut edges: Vec<KnowledgeEdge> = Vec::new();
    let mut edge_at: HashMap<(String, String, String), usize> = HashMap::new();
    let mut chains: Vec<KnowledgeChain> = Vec::new();
    let mut chain_at: HashMap<String, usize> = HashMap::new();
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut dead: HashSet<String> = HashSet::new();
    // Venues are folded after the topics are final: a venue op may name a
    // topic the same log creates further down.
    let mut venue_rows: Vec<VenueRow> = Vec::new();

    for op in sorted {
        match op {
            GraphOp::Topic {
                topic_id,
                title,
                parent,
                ..
            } => match topic_at.get(topic_id) {
                Some(&i) => {
                    if !title.trim().is_empty() {
                        topics[i].title = title.trim().to_owned();
                    }
                    if parent.is_some() {
                        topics[i].parent = parent.clone();
                    }
                }
                None => {
                    topic_at.insert(topic_id.clone(), topics.len());
                    topics.push(KnowledgeTopic {
                        id: topic_id.clone(),
                        title: title.trim().to_owned(),
                        parent: parent.clone(),
                    });
                }
            },
            GraphOp::Node {
                node_id,
                title,
                statement,
                kind,
                home,
                provenance,
                attribution,
                scope,
                correct,
                missed,
                ..
            } => {
                // A node op is the claim living again after a tombstone.
                dead.remove(node_id);
                match node_at.get(node_id) {
                    Some(&i) => {
                        let row = &mut rows[i];
                        if let Some(t) = title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                            row.node.title = t.to_owned();
                        }
                        row.node.statement = statement.trim().to_owned();
                        row.node.kind = *kind;
                        if !home.trim().is_empty() && !row.learner_home {
                            row.node.home = home.trim().to_owned();
                        }
                        if provenance.is_some() {
                            row.node.provenance = provenance.clone();
                        }
                        if attribution.is_some() {
                            row.node.attribution = attribution.clone();
                        }
                        if scope.is_some() {
                            row.node.scope = scope.clone();
                        }
                        if let Some(c) = correct {
                            row.node.correct = *c;
                        }
                        if let Some(m) = missed {
                            row.node.missed = *m;
                        }
                        // A node op is the claim living again after a tombstone.
                        row.tombstoned = false;
                    }
                    None => {
                        let title = title
                            .as_deref()
                            .map(str::trim)
                            .filter(|t| !t.is_empty())
                            .map(str::to_owned)
                            .unwrap_or_else(|| crate::agent::clip(statement.trim(), 80));
                        let node = KnowledgeNode {
                            id: node_id.clone(),
                            title,
                            statement: statement.trim().to_owned(),
                            kind: *kind,
                            home: home.trim().to_owned(),
                            correct: correct.unwrap_or(0),
                            missed: missed.unwrap_or(0),
                            provenance: provenance.clone(),
                            attribution: attribution.clone(),
                            scope: scope.clone(),
                            aliases: Vec::new(),
                        };
                        node_at.insert(node_id.clone(), rows.len());
                        rows.push(NodeRow {
                            node,
                            order: rows.len(),
                            learner_home: false,
                            tombstoned: false,
                        });
                    }
                }
            }
            GraphOp::Place {
                node_id, home, by, ..
            } => {
                let Some(&i) = node_at.get(node_id) else {
                    continue;
                };
                let row = &mut rows[i];
                if by == "learner" {
                    row.learner_home = true;
                    row.node.home = home.trim().to_owned();
                } else if !row.learner_home {
                    row.node.home = home.trim().to_owned();
                }
            }
            GraphOp::Edge {
                from,
                to,
                relation,
                weight,
                why,
                provenance,
                ..
            } => {
                let relation = normal_relation(relation);
                let weight = normal_weight(weight);
                let key = (from.clone(), to.clone(), relation.clone());
                match edge_at.get(&key) {
                    Some(&i) => {
                        edges[i].weight = weight;
                        if why.is_some() {
                            edges[i].why = why.clone();
                        }
                        if provenance.is_some() {
                            edges[i].provenance = provenance.clone();
                        }
                    }
                    None => {
                        edge_at.insert(key, edges.len());
                        edges.push(KnowledgeEdge {
                            from: from.clone(),
                            to: to.clone(),
                            relation,
                            weight,
                            why: why.clone(),
                            provenance: provenance.clone(),
                        });
                    }
                }
            }
            GraphOp::SameAs { a, b, .. } => pairs.push((a.clone(), b.clone())),
            GraphOp::Chain {
                chain_id,
                premises,
                conclusion,
                statement,
                ..
            } => {
                if chain_id.trim().is_empty() {
                    continue;
                }
                let chain = KnowledgeChain {
                    chain_id: chain_id.clone(),
                    premises: premises.clone(),
                    conclusion: conclusion.clone(),
                    statement: statement.clone(),
                };
                match chain_at.get(chain_id) {
                    Some(&i) => chains[i] = chain,
                    None => {
                        chain_at.insert(chain_id.clone(), chains.len());
                        chains.push(chain);
                    }
                }
            }
            GraphOp::Tombstone { node_id, .. } => {
                dead.insert(node_id.clone());
                if let Some(&i) = node_at.get(node_id) {
                    rows[i].tombstoned = true;
                }
            }
            GraphOp::Split { topic, groups, .. } => {
                if !topic_at.contains_key(topic) {
                    continue;
                }
                for group in groups {
                    let title = group.title.trim();
                    if title.is_empty() || group.node_ids.is_empty() {
                        continue;
                    }
                    let id = unique_topic_id(title, &topic_at);
                    topic_at.insert(id.clone(), topics.len());
                    topics.push(KnowledgeTopic {
                        id: id.clone(),
                        title: title.to_owned(),
                        parent: Some(topic.clone()),
                    });
                    // Only the topic's own nodes move: one the learner placed
                    // elsewhere since the proposal stays where they put it.
                    // The moves are the learner's (they accepted the split),
                    // so a later builder upsert cannot undo them.
                    for node_id in &group.node_ids {
                        if let Some(&i) = node_at.get(node_id) {
                            if rows[i].node.home == *topic {
                                rows[i].node.home = id.clone();
                                rows[i].learner_home = true;
                            }
                        }
                    }
                }
            }
            GraphOp::Venue {
                topic_id,
                kind,
                name,
                url,
                entry_level,
                note,
                checked,
                provenance,
                ..
            } => venue_rows.push(VenueRow {
                topic_id: topic_id.trim().to_owned(),
                kind: normal_venue_kind(kind),
                name: name.trim().to_owned(),
                url: url.clone(),
                entry_level: entry_level.trim().to_owned(),
                note: note.trim().to_owned(),
                checked: checked.clone(),
                provenance: provenance.clone(),
            }),
        }
    }

    // `sameAs`: fold into the earlier node, remap everything, keep the folded
    // id as an alias of the survivor.
    let mut alias: HashMap<String, String> = HashMap::new();
    let order_of = |id: &str| {
        node_at
            .get(id)
            .map(|i| rows[*i].order)
            .unwrap_or(usize::MAX)
    };
    for (a, b) in &pairs {
        let ra = find_alias(&alias, a);
        let rb = find_alias(&alias, b);
        if ra == rb {
            continue;
        }
        let (win, lose) = if order_of(&ra) <= order_of(&rb) {
            (ra, rb)
        } else {
            (rb, ra)
        };
        for v in alias.values_mut() {
            if *v == lose {
                *v = win.clone();
            }
        }
        alias.insert(lose, win);
    }

    let mut nodes: Vec<KnowledgeNode> = Vec::new();
    let mut survivor_at: HashMap<String, usize> = HashMap::new();
    let mut tombstones: HashSet<String> = dead.clone();
    for row in &rows {
        let canon = find_alias(&alias, &row.node.id);
        if row.tombstoned {
            tombstones.insert(canon);
            continue;
        }
        match survivor_at.get(&canon) {
            Some(&i) => {
                let mut folded = vec![row.node.id.clone()];
                folded.extend(row.node.aliases.iter().cloned());
                for id in folded {
                    if id != canon && !nodes[i].aliases.contains(&id) {
                        nodes[i].aliases.push(id);
                    }
                }
            }
            None => {
                survivor_at.insert(canon.clone(), nodes.len());
                let mut node = row.node.clone();
                node.id = canon;
                nodes.push(node);
            }
        }
    }
    nodes.retain(|n| !tombstones.contains(&n.id));

    // The index is derived from what survived: aliases, tombstones, homes.
    let live: HashSet<String> = nodes.iter().map(|n| n.id.clone()).collect();
    let mut resolved: HashMap<String, String> = HashMap::new();
    for n in &nodes {
        resolved.insert(n.id.clone(), n.id.clone());
        for a in &n.aliases {
            resolved.insert(a.clone(), n.id.clone());
        }
    }

    let mut final_edges: Vec<KnowledgeEdge> = Vec::new();
    let mut seen: HashSet<(String, String, String)> = HashSet::new();
    for e in edges {
        let from = find_alias(&alias, &e.from);
        let to = find_alias(&alias, &e.to);
        if from == to || !live.contains(&from) || !live.contains(&to) {
            continue;
        }
        if !seen.insert((from.clone(), to.clone(), e.relation.clone())) {
            continue;
        }
        final_edges.push(KnowledgeEdge {
            from,
            to,
            relation: e.relation,
            weight: e.weight,
            why: e.why,
            provenance: e.provenance,
        });
    }

    let mut final_chains: Vec<KnowledgeChain> = Vec::new();
    for c in chains {
        let premises: Vec<String> = c.premises.iter().map(|p| find_alias(&alias, p)).collect();
        let conclusion = find_alias(&alias, &c.conclusion);
        let problem = if premises.is_empty() {
            Some("no premises".to_owned())
        } else if premises.contains(&conclusion) {
            Some("its conclusion is one of its premises".to_owned())
        } else if !premises.iter().all(|p| live.contains(p)) {
            Some(format!(
                "a premise no longer exists or was tombstoned: {}",
                premises
                    .iter()
                    .filter(|p| !live.contains(*p))
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        } else if !live.contains(&conclusion) {
            Some(format!("its conclusion `{conclusion}` no longer exists"))
        } else {
            None
        };
        if let Some(why) = problem {
            tracing::warn!(
                chain = %c.chain_id,
                "graph: dropped chain `{}`: {why}",
                c.chain_id
            );
            continue;
        }
        final_chains.push(KnowledgeChain {
            chain_id: c.chain_id,
            premises,
            conclusion,
            statement: c.statement,
        });
    }

    let (venues, topic_venues) = fold_venues(&topics, venue_rows);
    let index = build_index(&topics, &nodes, &final_edges, resolved, tombstones);
    KnowledgeMap {
        topics,
        nodes,
        edges: final_edges,
        chains: final_chains,
        splits_proposed: Vec::new(),
        topic_checks: Default::default(),
        venues,
        topic_venues,
        index,
    }
}

/// One venue op as read, before the fold decides which line survives for a
/// `(topicId, name)`.
struct VenueRow {
    topic_id: String,
    kind: String,
    name: String,
    url: Option<String>,
    entry_level: String,
    note: String,
    checked: Option<String>,
    provenance: Option<Value>,
}

/// Fold the venue ops into the topic tree: the own list (deduped by
/// `(topicId, name)`, last writer wins, in read-model order) and a view per
/// topic that also holds its descendants' venues, so `Mathematics` shows what
/// was recorded under `Calculus`. A venue naming a topic that doesn't exist, a
/// blank name or entry level, and a kind outside [`VENUE_KINDS`] are dropped
/// and logged: the log is model output, and one bad line must not make the map
/// unreadable.
fn fold_venues(
    topics: &[KnowledgeTopic],
    rows: Vec<VenueRow>,
) -> (Vec<KnowledgeVenue>, BTreeMap<String, Vec<KnowledgeVenue>>) {
    let mut own: Vec<KnowledgeVenue> = Vec::new();
    let mut at: HashMap<(String, String), usize> = HashMap::new();
    for row in rows {
        let why = if !topics.iter().any(|t| t.id == row.topic_id) {
            Some(format!("unknown topic `{}`", row.topic_id))
        } else if row.name.is_empty() {
            Some("an empty name".to_owned())
        } else if row.entry_level.is_empty() {
            Some("an empty entry level".to_owned())
        } else if !VENUE_KINDS.contains(&row.kind.as_str()) {
            Some(format!(
                "kind `{}` (must be {})",
                row.kind,
                VENUE_KINDS.join(", ")
            ))
        } else {
            None
        };
        if let Some(why) = why {
            tracing::warn!(venue = %row.name, "graph: dropped venue: {why}");
            continue;
        }
        let venue = KnowledgeVenue {
            topic_id: row.topic_id.clone(),
            kind: row.kind,
            name: row.name,
            url: row.url,
            entry_level: row.entry_level,
            note: row.note,
            checked: row.checked,
            provenance: row.provenance,
        };
        let key = (row.topic_id, venue.name.to_lowercase());
        match at.get(&key) {
            Some(&i) => own[i] = venue,
            None => {
                at.insert(key, own.len());
                own.push(venue);
            }
        }
    }
    own.sort_by(venue_order);

    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    for t in topics {
        if let Some(parent) = &t.parent {
            children.entry(parent.as_str()).or_default().push(&t.id);
        }
    }
    let mut view: BTreeMap<String, Vec<KnowledgeVenue>> = BTreeMap::new();
    if !own.is_empty() {
        for t in topics {
            let mut subtree: HashSet<&str> = HashSet::new();
            let mut stack = vec![t.id.as_str()];
            while let Some(id) = stack.pop() {
                if !subtree.insert(id) {
                    continue;
                }
                for kid in children.get(id).into_iter().flatten() {
                    stack.push(kid);
                }
            }
            let list: Vec<KnowledgeVenue> = own
                .iter()
                .filter(|v| subtree.contains(v.topic_id.as_str()))
                .cloned()
                .collect();
            if !list.is_empty() {
                view.insert(t.id.clone(), list);
            }
        }
    }
    (own, view)
}

fn unique_topic_id(title: &str, taken: &HashMap<String, usize>) -> String {
    let base = slug(title);
    let mut id = base.clone();
    let mut n = 2;
    while taken.contains_key(&id) {
        id = format!("{base}-{n}");
        n += 1;
    }
    id
}

/// A lowercase slug for a split's child topic id.
fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_owned();
    if out.is_empty() {
        "part".to_owned()
    } else {
        out.chars().take(40).collect()
    }
}

/// Everything derived from the folded map, rebuilt at load and never written.
#[derive(Debug, Clone, Default)]
pub struct GraphIndex {
    /// node id → index in `nodes`.
    pub by_id: HashMap<String, usize>,
    /// topic id → ids of its child topics.
    pub children: HashMap<String, Vec<String>>,
    /// topic id → ids of the nodes homed there.
    pub topic_nodes: HashMap<String, Vec<String>>,
    /// node id → indexes of the edges touching it (either end).
    pub edges_at: HashMap<String, Vec<usize>>,
    /// `requires` adjacency: from → its targets.
    pub requires: HashMap<String, Vec<String>>,
    /// any id → the node it folds into (itself for a survivor).
    pub alias: HashMap<String, String>,
    /// node ids tombstoned by the log.
    pub tombstoned: HashSet<String>,
}

pub fn build_index(
    topics: &[KnowledgeTopic],
    nodes: &[KnowledgeNode],
    edges: &[KnowledgeEdge],
    alias: HashMap<String, String>,
    tombstoned: HashSet<String>,
) -> GraphIndex {
    let mut index = GraphIndex {
        alias,
        tombstoned,
        ..Default::default()
    };
    for (i, n) in nodes.iter().enumerate() {
        index.by_id.insert(n.id.clone(), i);
        index
            .topic_nodes
            .entry(n.home.clone())
            .or_default()
            .push(n.id.clone());
        index
            .alias
            .entry(n.id.clone())
            .or_insert_with(|| n.id.clone());
    }
    for t in topics {
        index.children.entry(t.id.clone()).or_default();
        if let Some(parent) = &t.parent {
            index
                .children
                .entry(parent.clone())
                .or_default()
                .push(t.id.clone());
        }
    }
    for (i, e) in edges.iter().enumerate() {
        index.edges_at.entry(e.from.clone()).or_default().push(i);
        index.edges_at.entry(e.to.clone()).or_default().push(i);
        if e.is_requires() {
            index
                .requires
                .entry(e.from.clone())
                .or_default()
                .push(e.to.clone());
        }
    }
    index
}

// ---------------------------------------------------------------------------
// Builder input: the graph as text
// ---------------------------------------------------------------------------

/// The topic tree plus up to [`MAX_SUMMARY_NODES`] node ids, homes and titles,
/// capped at [`MAX_SUMMARY_CHARS`]. This is what `{{graph_summary}}` holds.
pub fn summary(map: &KnowledgeMap) -> String {
    let mut out = String::from("Topics (indented by parent):\n");
    let mut children: HashMap<&str, Vec<&KnowledgeTopic>> = HashMap::new();
    for t in &map.topics {
        if let Some(parent) = &t.parent {
            children.entry(parent.as_str()).or_default().push(t);
        }
    }
    fn render(
        topic: &KnowledgeTopic,
        children: &HashMap<&str, Vec<&KnowledgeTopic>>,
        depth: usize,
        out: &mut String,
    ) {
        if out.len() > MAX_SUMMARY_CHARS {
            return;
        }
        let indent = "  ".repeat(depth);
        out.push_str(&format!("{indent}- {} ({})\n", topic.title, topic.id));
        if let Some(kids) = children.get(topic.id.as_str()) {
            for kid in kids {
                render(kid, children, depth + 1, out);
            }
        }
    }
    for topic in map.topics.iter().filter(|t| t.parent.is_none()) {
        render(topic, &children, 0, &mut out);
    }

    out.push_str("\nNodes (id [home] title):\n");
    for (shown, node) in map.nodes.iter().enumerate() {
        if shown >= MAX_SUMMARY_NODES || out.len() > MAX_SUMMARY_CHARS {
            out.push_str(&format!(
                "… {} more nodes not listed\n",
                map.nodes.len().saturating_sub(shown)
            ));
            break;
        }
        out.push_str(&format!("- {} [{}] {}\n", node.id, node.home, node.title));
    }

    if !map.venues.is_empty() {
        out.push_str("\nVenues already recorded (topic → kind name):\n");
        for (shown, venue) in map.venues.iter().enumerate() {
            if shown >= MAX_SUMMARY_VENUES || out.len() > MAX_SUMMARY_CHARS {
                out.push_str(&format!(
                    "… {} more venues not listed\n",
                    map.venues.len().saturating_sub(shown)
                ));
                break;
            }
            out.push_str(&format!(
                "- {} → {} {}\n",
                venue.topic_id, venue.kind, venue.name
            ));
        }
    }
    out
}

/// `graph_search`: up to [`MAX_SEARCH_HITS`] nodes and topics whose id, title
/// or statement contains `query`, best match first.
pub fn search(map: &KnowledgeMap, query: &str) -> String {
    search_with(map, query, |_| String::new())
}

/// As [`search`], with `annotate(node)` appended to each node's line (the
/// tutor's and planner's search adds the learner's evidence, [`read`]).
pub fn search_with(
    map: &KnowledgeMap,
    query: &str,
    annotate: impl Fn(&KnowledgeNode) -> String,
) -> String {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return "graph_search needs a non-empty `query`.".to_owned();
    }
    let mut hits: Vec<(u8, String, String)> = Vec::new();
    for topic in &map.topics {
        let title = topic.title.to_lowercase();
        let id = topic.id.to_lowercase();
        let score = if id == needle {
            0
        } else if id.contains(&needle) || title.contains(&needle) {
            1
        } else {
            continue;
        };
        hits.push((
            score,
            format!("topic {}", topic.id),
            format!(
                "{} {} (parent: {})",
                topic.id,
                topic.title,
                topic.parent.as_deref().unwrap_or("none")
            ),
        ));
    }
    for node in &map.nodes {
        let title = node.title.to_lowercase();
        let id = node.id.to_lowercase();
        let statement = node.statement.to_lowercase();
        let score = if id == needle {
            0
        } else if title.contains(&needle) {
            2
        } else if id.contains(&needle) {
            3
        } else if statement.contains(&needle) {
            4
        } else {
            continue;
        };
        hits.push((
            score,
            format!("node {}", node.id),
            format!(
                "{} [{}] {} — {}{}",
                node.id,
                node.home,
                node.title,
                node.statement,
                annotate(node)
            ),
        ));
    }
    hits.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    hits.dedup_by(|a, b| a.1 == b.1);
    if hits.is_empty() {
        return format!("No node or topic matches `{}`.", query.trim());
    }
    let mut out = String::new();
    for (i, (_, _, line)) in hits.iter().take(MAX_SEARCH_HITS).enumerate() {
        out.push_str(&format!("{}. {line}\n", i + 1));
    }
    out
}

// ---------------------------------------------------------------------------
// Topic splitter
// ---------------------------------------------------------------------------

/// A topic the learner dismissed a split of, with the node count it had then:
/// it is not proposed again until the count changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DismissedSplit {
    pub topic: String,
    pub nodes: usize,
}

/// `graph/splits.proposed.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitFile {
    #[serde(default)]
    pub proposals: Vec<ProposedSplit>,
    #[serde(default)]
    pub dismissed: Vec<DismissedSplit>,
}

/// The proposal file, or an empty one when absent or unreadable.
pub fn read_split_file(repo: &Path) -> SplitFile {
    let path = repo.join(SPLITS);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return SplitFile::default();
    };
    match serde_json::from_str(&text) {
        Ok(file) => file,
        Err(e) => {
            tracing::warn!("graph/splits.proposed.json: {e}; reading it as empty");
            SplitFile::default()
        }
    }
}

pub fn write_split_file(repo: &Path, file: &SplitFile) -> Result<(), Error> {
    let path = repo.join(SPLITS);
    if file.proposals.is_empty() && file.dismissed.is_empty() {
        if path.is_file() {
            std::fs::remove_file(&path)?;
        }
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(file)? + "\n")?;
    Ok(())
}

/// Recompute the split proposals: for every topic with more than
/// [`MAX_TOPIC_NODES`] nodes, the connected components of its internal
/// `requires` graph; when at least two components hold at least
/// [`MIN_SPLIT_GROUP`] nodes, the topic is worth splitting.
pub fn propose_splits(map: &KnowledgeMap, dismissed: &[DismissedSplit]) -> Vec<ProposedSplit> {
    let mut proposals = Vec::new();
    for topic in &map.topics {
        let node_ids: Vec<&str> = map
            .nodes
            .iter()
            .filter(|n| n.home == topic.id)
            .map(|n| n.id.as_str())
            .collect();
        if node_ids.len() <= MAX_TOPIC_NODES {
            continue;
        }
        if dismissed
            .iter()
            .any(|d| d.topic == topic.id && d.nodes == node_ids.len())
        {
            continue;
        }
        let inside: HashSet<&str> = node_ids.iter().copied().collect();
        let mut parent: HashMap<&str, &str> = node_ids.iter().map(|id| (*id, *id)).collect();
        fn root<'a>(parent: &mut HashMap<&'a str, &'a str>, id: &'a str) -> &'a str {
            let mut cur = id;
            while let Some(&up) = parent.get(cur) {
                if up == cur {
                    break;
                }
                cur = up;
            }
            cur
        }
        for edge in map.prerequisites() {
            if inside.contains(edge.from.as_str()) && inside.contains(edge.to.as_str()) {
                let a = root(&mut parent, &edge.from);
                let b = root(&mut parent, &edge.to);
                if a != b {
                    parent.insert(a, b);
                }
            }
        }
        let mut components: HashMap<&str, Vec<&str>> = HashMap::new();
        for id in &node_ids {
            let r = root(&mut parent, id);
            components.entry(r).or_default().push(id);
        }
        let mut groups: Vec<Vec<&str>> = components
            .into_values()
            .filter(|c| c.len() >= MIN_SPLIT_GROUP)
            .collect();
        if groups.len() < 2 {
            continue;
        }
        // Stable order: biggest component first, ties by first node id.
        groups.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a[0].cmp(b[0])));
        let total = node_ids.len();
        let sizes: String = groups
            .iter()
            .map(|g| g.len().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let mut split_groups = Vec::new();
        for (i, component) in groups.iter().enumerate() {
            // A placeholder name the learner can recognise: the largest node's
            // title, suffixed with the part's number.
            let largest = component
                .iter()
                .filter_map(|id| map.index.by_id.get(*id).map(|i| &map.nodes[*i]))
                .max_by_key(|n| n.title.len())
                .map(|n| n.title.clone())
                .unwrap_or_else(|| format!("Part {}", i + 1));
            split_groups.push(SplitGroup {
                title: format!("{largest} ({})", i + 1),
                node_ids: component.iter().map(|id| (*id).to_owned()).collect(),
                why: Some(format!(
                    "{} of the topic's {total} nodes, connected by their prerequisites (components: {sizes})",
                    component.len()
                )),
            });
        }
        proposals.push(ProposedSplit {
            topic: topic.id.clone(),
            groups: split_groups,
        });
    }
    proposals
}

/// Recompute the proposals and store them. Returns them.
pub fn splits(repo: &Path) -> Result<Vec<ProposedSplit>, Error> {
    let map = crate::knowledge::load(repo)?;
    let mut file = read_split_file(repo);
    let previous = std::mem::take(&mut file.proposals);
    let mut fresh = propose_splits(&map, &file.dismissed);
    // A recompute never throws away names the builder chose: when the same
    // partition comes back, its titles and reasons are kept.
    for proposal in &mut fresh {
        if let Some(old) = previous
            .iter()
            .find(|p| p.topic == proposal.topic && same_partition(p, proposal))
        {
            proposal.groups = old.groups.clone();
        }
    }
    file.proposals = fresh;
    write_split_file(repo, &file)?;
    Ok(file.proposals)
}

/// The same nodes in the same groups (titles and reasons may differ).
fn same_partition(a: &ProposedSplit, b: &ProposedSplit) -> bool {
    if a.groups.len() != b.groups.len() {
        return false;
    }
    let mut left: Vec<Vec<String>> = a
        .groups
        .iter()
        .map(|g| {
            let mut ids = g.node_ids.clone();
            ids.sort();
            ids
        })
        .collect();
    let mut right: Vec<Vec<String>> = b
        .groups
        .iter()
        .map(|g| {
            let mut ids = g.node_ids.clone();
            ids.sort();
            ids
        })
        .collect();
    left.sort();
    right.sort();
    left == right
}

/// Merge the builder's named splits into the proposal file (a named proposal
/// replaces the placeholder for the same topic). Nothing becomes structure
/// until the learner accepts it.
pub fn record_splits(repo: &Path, splits: Vec<ProposedSplit>) -> Result<(), Error> {
    if splits.is_empty() {
        return Ok(());
    }
    let mut file = read_split_file(repo);
    for proposal in splits {
        file.proposals.retain(|p| p.topic != proposal.topic);
        file.proposals.push(proposal);
    }
    write_split_file(repo, &file)
}

/// What `decide_splits` did.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitDecision {
    /// Topics whose split was accepted (a `split` op each).
    pub accepted: Vec<String>,
    /// Topics whose proposal was dismissed.
    pub dismissed: Vec<String>,
    /// Proposals left in the file.
    pub remaining: usize,
}

/// Accept or dismiss proposed splits. Accepting appends a `split` op (with
/// learner provenance); the proposal file is rewritten either way. `titles`
/// renames an accepted proposal's groups, by topic and group index: the
/// names are suggestions until the learner accepts them, and a blank or
/// missing entry keeps the proposed name.
pub fn decide_splits(
    repo: &Path,
    accept: &[String],
    dismiss: &[String],
    titles: &HashMap<String, Vec<String>>,
) -> Result<SplitDecision, Error> {
    let map = crate::knowledge::load(repo)?;
    let mut file = read_split_file(repo);
    let mut out = SplitDecision::default();
    let mut ops = Vec::new();
    let mut unknown = Vec::new();

    for topic in accept {
        match file.proposals.iter().position(|p| &p.topic == topic) {
            Some(i) => {
                let mut proposal = file.proposals.remove(i);
                let names = titles.get(topic).map(Vec::as_slice).unwrap_or(&[]);
                for (group, name) in proposal.groups.iter_mut().zip(names) {
                    if !name.trim().is_empty() {
                        group.title = name.trim().to_owned();
                    }
                }
                ops.push(GraphOp::split(&proposal.topic, proposal.groups));
                out.accepted.push(topic.clone());
            }
            None => unknown.push(topic.clone()),
        }
    }
    for topic in dismiss {
        match file.proposals.iter().position(|p| &p.topic == topic) {
            Some(i) => {
                let proposal = file.proposals.remove(i);
                let nodes = map
                    .nodes
                    .iter()
                    .filter(|n| n.home == proposal.topic)
                    .count();
                file.dismissed.retain(|d| &d.topic != topic);
                file.dismissed.push(DismissedSplit {
                    topic: topic.clone(),
                    nodes,
                });
                out.dismissed.push(topic.clone());
            }
            None if !unknown.contains(topic) => unknown.push(topic.clone()),
            None => {}
        }
    }
    if !unknown.is_empty() {
        return Err(Error::Invalid(format!(
            "no split is proposed for: {} (run graph_splits first)",
            unknown.join(", ")
        )));
    }

    append(repo, &ops)?;
    out.remaining = file.proposals.len();
    write_split_file(repo, &file)?;
    Ok(out)
}

/// What `place_node` did: the idea (its canonical id) and the two homes.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Placed {
    pub node_id: String,
    pub from: String,
    pub to: String,
}

/// The learner moved an idea to another topic (arch §4.5 "learner-
/// overridable"): one `place` op `by: "learner"` with learner provenance,
/// which outranks every later builder placement or node upsert. An alias
/// moves the node it folds into. Refused for an unknown or tombstoned idea,
/// an unknown topic, or the topic the idea already lives in.
pub fn place_node(repo: &Path, node_id: &str, home: &str) -> Result<Placed, Error> {
    let (node_id, home) = (node_id.trim(), home.trim());
    let map = crate::knowledge::load(repo)?;
    if map.is_tombstoned(node_id) {
        return Err(Error::Invalid(format!(
            "idea `{node_id}` was removed from the map"
        )));
    }
    let Some(node) = map.node(node_id) else {
        return Err(Error::Invalid(format!(
            "there is no idea `{node_id}` on the map"
        )));
    };
    if !map.topics.iter().any(|t| t.id == home) {
        return Err(Error::Invalid(format!(
            "there is no topic `{home}` on the map"
        )));
    }
    if node.home == home {
        return Err(Error::Invalid(format!(
            "`{}` is already in that topic",
            node.title
        )));
    }
    let placed = Placed {
        node_id: node.id.clone(),
        from: node.home.clone(),
        to: home.to_owned(),
    };
    append(repo, &[GraphOp::place(&node.id, home, "learner")])?;
    Ok(placed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Kind;
    use std::path::PathBuf;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lh-fold-{name}-{}", ulid::Ulid::new()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("graph")).unwrap();
        dir
    }

    fn base() -> Vec<GraphOp> {
        vec![
            GraphOp::topic("t", "T", None),
            GraphOp::topic("t2", "T2", Some("t")),
            GraphOp::node("a", Some("A"), "a holds", Kind::Fact, "t", None),
            GraphOp::node("b", Some("B"), "b holds", Kind::Mechanism, "t", None),
            GraphOp::node("c", Some("C"), "c holds", Kind::Fact, "t", None),
        ]
    }

    #[test]
    fn an_upsert_replaces_what_it_names_and_keeps_what_it_does_not() {
        let mut ops = base();
        ops.push(GraphOp::topic("t2", "Algebra", None));
        ops.push(GraphOp::node(
            "a",
            None,
            "a, restated",
            Kind::Procedure,
            "t2",
            None,
        ));
        let map = fold(&ops);
        let t2 = map.topics.iter().find(|t| t.id == "t2").unwrap();
        assert_eq!(t2.title, "Algebra");
        let a = map.node("a").unwrap();
        assert_eq!(a.title, "A", "an unnamed title is kept");
        assert_eq!(a.statement, "a, restated");
        assert_eq!(a.kind, Kind::Procedure);
        assert_eq!(a.home, "t2");
    }

    #[test]
    fn a_learner_place_wins_over_any_later_builder_place() {
        let mut ops = base();
        ops.push(GraphOp::place("a", "t2", "learner"));
        ops.push(GraphOp::place("a", "t", "builder"));
        // Even a later node upsert cannot undo the learner's move.
        ops.push(GraphOp::node(
            "a",
            Some("A"),
            "a holds",
            Kind::Fact,
            "t",
            None,
        ));
        let map = fold(&ops);
        assert_eq!(map.node("a").unwrap().home, "t2");

        // Builder moves stand until a learner move replaces them.
        let mut ops = base();
        ops.push(GraphOp::place("b", "t2", "builder"));
        assert_eq!(fold(&ops).node("b").unwrap().home, "t2");
        ops.push(GraphOp::place("b", "t", "learner"));
        assert_eq!(fold(&ops).node("b").unwrap().home, "t");
    }

    #[test]
    fn same_as_folds_into_the_earlier_node_remaps_edges_and_keeps_an_alias() {
        let mut ops = base();
        ops.push(GraphOp::edge("b", "c", "requires", None, None, None));
        ops.push(GraphOp::edge("c", "a", "contrasts", None, None, None));
        // `b → a` would become a self-edge once b folds into a.
        ops.push(GraphOp::edge("b", "a", "requires", None, None, None));
        ops.push(GraphOp::same_as("b", "a"));
        let map = fold(&ops);
        assert_eq!(map.nodes.len(), 2, "b folded into a");
        let a = map.node("a").unwrap();
        assert_eq!(a.aliases, vec!["b".to_owned()]);
        assert_eq!(map.canonical("b"), "a");
        let pairs: Vec<(&str, &str, &str)> = map
            .edges
            .iter()
            .map(|e| (e.from.as_str(), e.to.as_str(), e.relation.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![("a", "c", "requires"), ("c", "a", "contrasts")],
            "edges follow the merge and the resulting self-edge is dropped"
        );
        // The fold goes to whichever node came first, in either argument order.
        let mut ops = base();
        ops.push(GraphOp::same_as("c", "b"));
        let map = fold(&ops);
        assert_eq!(map.node("b").unwrap().aliases, vec!["c".to_owned()]);
        assert_eq!(map.canonical("c"), "b");
    }

    #[test]
    fn a_tombstone_hides_the_node_and_its_edges_until_a_later_node_op() {
        let mut ops = base();
        ops.push(GraphOp::edge("a", "b", "requires", None, None, None));
        ops.push(GraphOp::tombstone("a", "wrong"));
        let map = fold(&ops);
        assert_eq!(map.nodes.len(), 2);
        assert!(map.is_tombstoned("a"));
        assert!(map.edges.is_empty(), "its edges go with it");
        assert!(!map.index.tombstoned.is_empty());

        // A later node op is the claim living again.
        ops.push(GraphOp::node(
            "a",
            Some("A"),
            "a holds",
            Kind::Fact,
            "t",
            None,
        ));
        let map = fold(&ops);
        assert_eq!(map.nodes.len(), 3);
        assert!(!map.is_tombstoned("a"));
        assert_eq!(map.edges.len(), 1);
    }

    #[test]
    fn a_chain_is_revalidated_at_load_and_dropped_when_it_cannot_hold() {
        let mut ops = base();
        ops.push(GraphOp::chain(
            "k1",
            &["a".into(), "b".into()],
            "c",
            "a and b give c",
        ));
        assert_eq!(fold(&ops).chains.len(), 1);

        // One premise tombstoned: the chain no longer holds.
        let mut dead = ops.clone();
        dead.push(GraphOp::tombstone("b", "wrong"));
        let map = fold(&dead);
        assert!(map.chains.is_empty(), "the chain is dropped, not fatal");

        // A conclusion among its premises, or no premises at all.
        let mut bad = base();
        bad.push(GraphOp::chain(
            "k2",
            &["a".into(), "c".into()],
            "c",
            "circular",
        ));
        bad.push(GraphOp::chain("k3", &[], "c", "from nothing"));
        assert!(fold(&bad).chains.is_empty());
        // An empty chain id cannot be addressed, so it is ignored.
        let mut anon = base();
        anon.push(GraphOp::chain("", &["a".into()], "c", ""));
        assert!(fold(&anon).chains.is_empty());
    }

    #[test]
    fn a_split_creates_child_topics_and_places_its_nodes() {
        let mut ops = base();
        ops.push(GraphOp::split(
            "t",
            vec![
                SplitGroup {
                    title: "Limits".to_owned(),
                    node_ids: vec!["a".to_owned(), "b".to_owned()],
                    why: None,
                },
                SplitGroup {
                    title: "Continuity".to_owned(),
                    node_ids: vec!["c".to_owned()],
                    why: None,
                },
            ],
        ));
        let map = fold(&ops);
        assert_eq!(map.topics.len(), 4);
        let limits = map.topics.iter().find(|t| t.id == "limits").unwrap();
        assert_eq!(limits.parent.as_deref(), Some("t"));
        assert_eq!(map.node("a").unwrap().home, "limits");
        assert_eq!(map.node("c").unwrap().home, "continuity");
        // A second split with a colliding title gets a distinct id.
        ops.push(GraphOp::split(
            "t",
            vec![SplitGroup {
                title: "Limits".to_owned(),
                node_ids: vec!["c".to_owned()],
                why: None,
            }],
        ));
        let map = fold(&ops);
        assert_eq!(map.topics.iter().filter(|t| t.title == "Limits").count(), 2);
    }

    #[test]
    fn a_malformed_or_unknown_op_does_not_lose_the_rest() {
        let repo = temp("mixed");
        append(&repo, &base()).unwrap();
        let mut text = std::fs::read_to_string(repo.join(OPS)).unwrap();
        text.push_str("{\"op\":\"nonsense\"}\n");
        std::fs::write(repo.join(OPS), text).unwrap();
        let (ops, skipped) = read_ops(&repo).unwrap();
        assert_eq!(skipped, 1);
        assert_eq!(ops.len(), base().len());
        // A split of a topic that doesn't exist is ignored, not fatal.
        let map = fold(&[
            GraphOp::node("x", None, "x", Kind::Fact, "t", None),
            GraphOp::split(
                "nope",
                vec![SplitGroup {
                    title: "Part".to_owned(),
                    node_ids: vec!["x".to_owned()],
                    why: None,
                }],
            ),
        ]);
        assert_eq!(map.topics.len(), 0);
        assert_eq!(map.node("x").unwrap().home, "t");
    }

    #[test]
    fn the_summary_caps_the_node_list_and_search_finds_both_kinds() {
        let mut ops = base();
        for i in 0..(MAX_SUMMARY_NODES + 5) {
            ops.push(GraphOp::node(
                &format!("filler-{i}"),
                Some("Filler"),
                "nothing to do with the query",
                Kind::Fact,
                "t",
                None,
            ));
        }
        let map = fold(&ops);
        let text = summary(&map);
        assert!(text.contains("T (t)"), "{text}");
        assert!(text.contains("T2 (t2)"), "{text}");
        assert!(text.contains("more nodes not listed"), "{text}");
        assert_eq!(
            text.lines()
                .filter(|l| l.starts_with("- ") && l.contains(" ["))
                .count(),
            MAX_SUMMARY_NODES
        );

        let found = search(&map, "B");
        assert!(found.contains("b [t] B"), "{found}");
        let found = search(&map, "T2");
        assert!(found.contains("t2 T2 (parent: t)"), "{found}");
        assert!(search(&map, "no such thing").starts_with("No node or topic"));
        assert!(search(&map, "  ").contains("non-empty"));
    }

    /// A topic of `n` nodes with two prerequisite components.
    fn big_topic(repo: &Path, n: usize) {
        let mut ops = vec![GraphOp::topic("big", "Big", None)];
        for i in 0..n {
            ops.push(GraphOp::node(
                &format!("n{i}"),
                Some(&format!("Node {i}")),
                "claim",
                Kind::Fact,
                "big",
                None,
            ));
        }
        for start in [0usize, n / 2] {
            for i in start..(start + n / 2 - 1) {
                ops.push(GraphOp::edge(
                    &format!("n{i}"),
                    &format!("n{}", i + 1),
                    "requires",
                    None,
                    None,
                    None,
                ));
            }
        }
        append(repo, &ops).unwrap();
    }

    #[test]
    fn the_splitter_proposes_components_over_the_threshold_and_respects_a_dismissal() {
        let repo = temp("split");
        big_topic(&repo, 34);
        let proposals = splits(&repo).unwrap();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].topic, "big");
        assert_eq!(proposals[0].groups.len(), 2);
        assert_eq!(proposals[0].groups[0].node_ids.len(), 17);
        assert!(
            proposals[0].groups[0]
                .why
                .as_deref()
                .unwrap_or("")
                .contains("17"),
            "{:?}",
            proposals[0].groups[0]
        );
        assert!(repo.join(SPLITS).is_file());
        assert_eq!(read_split_file(&repo).proposals.len(), 1);

        // Dismissing it stops it being proposed while the node count holds.
        let out = decide_splits(&repo, &[], &["big".to_owned()], &HashMap::new()).unwrap();
        assert_eq!(out.dismissed, ["big"]);
        assert!(splits(&repo).unwrap().is_empty());
        assert_eq!(read_split_file(&repo).dismissed[0].nodes, 34);

        // One more node and it is worth proposing again.
        append(
            &repo,
            &[GraphOp::node(
                "extra",
                Some("Extra"),
                "c",
                Kind::Fact,
                "big",
                None,
            )],
        )
        .unwrap();
        assert_eq!(splits(&repo).unwrap().len(), 1);

        // A small or single-component topic is never proposed.
        let repo = temp("small");
        big_topic(&repo, 30);
        assert!(splits(&repo).unwrap().is_empty());
    }

    #[test]
    fn accepting_a_split_appends_the_op_and_creates_the_topics() {
        let repo = temp("decide");
        big_topic(&repo, 34);
        let proposals = splits(&repo).unwrap();
        assert_eq!(proposals.len(), 1);
        let out = decide_splits(&repo, &["big".to_owned()], &[], &HashMap::new()).unwrap();
        assert_eq!(out.accepted, ["big"]);
        assert_eq!(out.remaining, 0);
        let map = crate::knowledge::load(&repo).unwrap();
        let children: Vec<&KnowledgeTopic> = map
            .topics
            .iter()
            .filter(|t| t.parent.as_deref() == Some("big"))
            .collect();
        assert_eq!(children.len(), 2, "both groups became topics");
        assert_eq!(
            map.nodes.iter().filter(|n| n.home == "big").count(),
            0,
            "every node moved into a child topic"
        );
        let ops = std::fs::read_to_string(repo.join(OPS)).unwrap();
        assert!(ops.contains(r#""op":"split""#), "{ops}");

        // Deciding a topic with no proposal is a readable error.
        let err = decide_splits(&repo, &["big".to_owned()], &[], &HashMap::new()).unwrap_err();
        assert!(err.to_string().contains("no split is proposed"), "{err}");
    }

    #[test]
    fn an_accepted_split_takes_the_learners_names_and_holds_against_the_builder() {
        let repo = temp("split-rank");
        big_topic(&repo, 34);
        let proposal = splits(&repo).unwrap().remove(0);
        let moved_away = proposal.groups[1].node_ids[0].clone();
        // The learner moves one of the proposal's nodes elsewhere first.
        append(&repo, &[GraphOp::topic("other", "Other", None)]).unwrap();
        place_node(&repo, &moved_away, "other").unwrap();

        let titles = HashMap::from([(
            "big".to_owned(),
            vec!["Sequences".to_owned(), "  ".to_owned()],
        )]);
        decide_splits(&repo, &["big".to_owned()], &[], &titles).unwrap();
        let map = crate::knowledge::load(&repo).unwrap();
        let child = |title: &str| {
            map.topics
                .iter()
                .find(|t| t.title == title)
                .map(|t| t.id.clone())
        };
        let first = child("Sequences").expect("the learner's name");
        let second = child(&proposal.groups[1].title).expect("a blank name keeps the proposal's");
        let first_node = &proposal.groups[0].node_ids[0];
        assert_eq!(map.node(first_node).unwrap().home, first);
        assert_eq!(
            map.node(&moved_away).unwrap().home,
            "other",
            "a node the learner placed elsewhere stays"
        );
        assert_eq!(
            map.node(&proposal.groups[1].node_ids[1]).unwrap().home,
            second
        );
        let ops = std::fs::read_to_string(repo.join(OPS)).unwrap();
        let split = ops.lines().find(|l| l.contains(r#""op":"split""#)).unwrap();
        assert!(split.contains(r#""type":"learner""#), "{split}");

        // The builder re-submitting the node with its old home can't undo it.
        append(
            &repo,
            &[GraphOp::node(
                first_node,
                None,
                "claim",
                Kind::Fact,
                "big",
                None,
            )],
        )
        .unwrap();
        assert_eq!(
            crate::knowledge::load(&repo)
                .unwrap()
                .node(first_node)
                .unwrap()
                .home,
            first
        );
    }

    #[test]
    fn the_learner_places_an_idea_and_the_builder_cannot_move_it_back() {
        let repo = temp("place");
        append(&repo, &base()).unwrap();
        append(
            &repo,
            &[
                GraphOp::node("a2", Some("A again"), "a holds", Kind::Fact, "t", None),
                GraphOp::same_as("a", "a2"),
            ],
        )
        .unwrap();

        // An alias moves the idea it folds into.
        let placed = place_node(&repo, "a2", "t2").unwrap();
        assert_eq!(
            (
                placed.node_id.as_str(),
                placed.from.as_str(),
                placed.to.as_str()
            ),
            ("a", "t", "t2")
        );
        let ops = std::fs::read_to_string(repo.join(OPS)).unwrap();
        let line = ops.lines().last().unwrap();
        let op: Value = serde_json::from_str(line).unwrap();
        assert_eq!(op["op"], "place");
        assert_eq!(op["by"], "learner");
        assert_eq!(op["provenance"]["type"], "learner");
        append(
            &repo,
            &[
                GraphOp::node("a", None, "a holds", Kind::Fact, "t", None),
                GraphOp::place("a", "t", "builder"),
            ],
        )
        .unwrap();
        assert_eq!(
            crate::knowledge::load(&repo)
                .unwrap()
                .node("a")
                .unwrap()
                .home,
            "t2"
        );

        let err = |id: &str, home: &str| place_node(&repo, id, home).unwrap_err().to_string();
        assert!(err("ghost", "t").contains("no idea `ghost`"));
        assert!(err("a", "nowhere").contains("no topic `nowhere`"));
        assert!(err("a", "t2").contains("already in that topic"));
        append(&repo, &[GraphOp::tombstone("b", "wrong")]).unwrap();
        assert!(err("b", "t2").contains("removed"));
    }

    #[test]
    fn the_builder_can_name_the_parts_it_proposes() {
        let repo = temp("record");
        big_topic(&repo, 34);
        let mut proposals = splits(&repo).unwrap();
        assert_eq!(proposals.len(), 1);
        // The builder's names replace the placeholders for the same partition.
        let mut named = proposals.remove(0);
        for (i, group) in named.groups.iter_mut().enumerate() {
            group.title = format!("Chapter {}", i + 1);
        }
        record_splits(&repo, vec![named]).unwrap();
        assert_eq!(
            read_split_file(&repo).proposals[0].groups[0].title,
            "Chapter 1"
        );
        // A recompute keeps them: the partition is unchanged.
        let again = splits(&repo).unwrap();
        assert_eq!(again[0].groups[0].title, "Chapter 1");

        // A changed partition falls back to the placeholder names: a third
        // cluster big enough to stand alone joins the proposal.
        let mut extra = Vec::new();
        for i in 0..MIN_SPLIT_GROUP {
            extra.push(GraphOp::node(
                &format!("x{i}"),
                Some("Extra"),
                "c",
                Kind::Fact,
                "big",
                None,
            ));
        }
        for i in 0..(MIN_SPLIT_GROUP - 1) {
            extra.push(GraphOp::edge(
                &format!("x{i}"),
                &format!("x{}", i + 1),
                "requires",
                None,
                None,
                None,
            ));
        }
        append(&repo, &extra).unwrap();
        let changed = splits(&repo).unwrap();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].groups.len(), 3);
        assert_ne!(changed[0].groups[0].title, "Chapter 1");
    }

    // ---- venues: where the field lives, per topic ------------------------

    /// A venue op with an explicit stamp, so "last writer wins" is decided by
    /// the test and not by two constructors landing in the same millisecond.
    #[allow(clippy::too_many_arguments)]
    fn venue_op(
        seq: u32,
        topic: &str,
        kind: &str,
        name: &str,
        url: Option<&str>,
        entry: &str,
        note: &str,
    ) -> GraphOp {
        GraphOp::Venue {
            op_id: format!("01J000000000000000000000{seq:02}"),
            at: format!("2026-11-07T00:00:{seq:02}.000Z"),
            topic_id: topic.to_owned(),
            kind: kind.to_owned(),
            name: name.to_owned(),
            url: url.map(str::to_owned),
            entry_level: entry.to_owned(),
            note: note.to_owned(),
            checked: Some("2026-11-07".to_owned()),
            provenance: Some(provenance(
                "builder",
                Some("s1"),
                "2026-11-07T00:00:00.000Z",
            )),
        }
    }

    fn venue_topics() -> Vec<GraphOp> {
        vec![
            GraphOp::topic("math", "Mathematics", None),
            GraphOp::topic("calc", "Calculus", Some("math")),
            GraphOp::topic("limits", "Limits", Some("calc")),
            GraphOp::topic("algebra", "Algebra", Some("math")),
        ]
    }

    #[test]
    fn venue_ops_dedupe_by_topic_and_name_with_the_last_writer_winning() {
        let mut ops = venue_topics();
        ops.push(venue_op(
            1,
            "calc",
            "ask",
            "Math StackExchange",
            Some("https://math.stackexchange.com/"),
            "anyone",
            "ask anything",
        ));
        // The same venue, restated: one row, the later note and kind win.
        ops.push(venue_op(
            2,
            "calc",
            "reference",
            "math stackexchange",
            Some("https://math.stackexchange.com/"),
            "anyone",
            "also good for reading old answers",
        ));
        // The same name on another topic is a different venue.
        ops.push(venue_op(
            3,
            "algebra",
            "ask",
            "Math StackExchange",
            None,
            "anyone",
            "same site, other topic",
        ));
        let map = fold(&ops);
        assert_eq!(map.venues.len(), 2, "{:?}", map.venues);
        let calc = map
            .venues
            .iter()
            .find(|v| v.topic_id == "calc")
            .expect("the calc row");
        assert_eq!(calc.note, "also good for reading old answers");
        assert_eq!(calc.kind, "reference");
        assert_eq!(
            calc.name, "math stackexchange",
            "the last writer's spelling"
        );

        // A venue is not a node: it changes nothing about the ideas.
        assert_eq!(map.nodes.len(), 0);
        assert!(map.topics.len() == 4);
    }

    #[test]
    fn a_venue_is_visible_on_every_ancestor_topic() {
        let mut ops = venue_topics();
        ops.push(venue_op(
            1,
            "limits",
            "discussion",
            "MathOverflow",
            Some("https://mathoverflow.net/"),
            "one variable calculus",
            "research-level discussion",
        ));
        let map = fold(&ops);
        for topic in ["limits", "calc", "math"] {
            let list = map.topic_venues.get(topic).unwrap_or_else(|| {
                panic!("`{topic}` should show the venue: {:?}", map.topic_venues)
            });
            assert_eq!(list.len(), 1, "{topic}");
            assert_eq!(list[0].name, "MathOverflow");
        }
        assert!(
            !map.topic_venues.contains_key("algebra"),
            "a sibling does not inherit it"
        );
    }

    #[test]
    fn venues_come_in_ask_discussion_reference_order_then_by_name() {
        let mut ops = venue_topics();
        ops.push(venue_op(
            1,
            "math",
            "reference",
            "Zeta notes",
            None,
            "anyone",
            "",
        ));
        ops.push(venue_op(
            2,
            "calc",
            "ask",
            "Stack Exchange",
            None,
            "anyone",
            "",
        ));
        ops.push(venue_op(
            3,
            "calc",
            "discussion",
            "MathOverflow",
            None,
            "anyone",
            "",
        ));
        ops.push(venue_op(
            4,
            "math",
            "ask",
            "Ask a mathematician",
            None,
            "anyone",
            "",
        ));
        let map = fold(&ops);
        let kinds: Vec<&str> = map.venues.iter().map(|v| v.kind.as_str()).collect();
        assert_eq!(kinds, vec!["ask", "ask", "discussion", "reference"]);
        let names: Vec<&str> = map.venues.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "Ask a mathematician",
                "Stack Exchange",
                "MathOverflow",
                "Zeta notes"
            ]
        );
        // The per-topic view keeps the same order.
        let view: Vec<&str> = map.topic_venues["math"]
            .iter()
            .map(|v| v.name.as_str())
            .collect();
        assert_eq!(
            view,
            vec![
                "Ask a mathematician",
                "Stack Exchange",
                "MathOverflow",
                "Zeta notes"
            ]
        );
    }

    #[test]
    fn a_venue_naming_an_unknown_topic_or_a_bad_kind_is_dropped() {
        let mut ops = venue_topics();
        ops.push(venue_op(1, "ghost", "ask", "Nowhere", None, "anyone", ""));
        ops.push(venue_op(
            2,
            "math",
            "forum",
            "Odd place",
            None,
            "anyone",
            "",
        ));
        ops.push(venue_op(3, "math", "ask", "Unleveled", None, "", ""));
        ops.push(venue_op(4, "math", "ask", "Fine", None, "anyone", ""));
        let map = fold(&ops);
        assert_eq!(map.venues.len(), 1, "{:?}", map.venues);
        assert_eq!(map.venues[0].name, "Fine");
        assert_eq!(map.topic_venues["math"].len(), 1);
    }

    #[test]
    fn an_empty_graph_has_no_venues_at_all() {
        let map = fold(&[]);
        assert!(map.venues.is_empty());
        assert!(map.topic_venues.is_empty());
    }
}
