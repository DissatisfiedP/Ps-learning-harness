//! The knowledge graph read model: a topic tree with proposition nodes, typed
//! edges, argument chains and split proposals, folded from the data repo's
//! append-only op log (`docs/graph-build.md`, architecture §4.2, §4.5).
//!
//! On disk (plain files, git-friendly, arch §4.17):
//! - `graph/ops.jsonl` — the structure, one op per line, never rewritten.
//!   [`crate::graph`] owns the op shapes, the fold and the one-time migration
//!   from the older `topics.json` / `nodes.jsonl` / `edges.jsonl` files.
//! - `graph/splits.proposed.json` — topic-split proposals awaiting a decision.
//! - `graph/checks.jsonl` `{ topic, correct, missed }` — learner evidence per
//!   topic, not part of the graph structure; it keeps its own reader here.
//!
//! Edges carry a `relation`: `requires` (`from` is a prerequisite of `to`),
//! `contrasts` (the two claims are easily confused), `supports` and `attacks`
//! (arguments). Only `requires` orders anything: cycle checks, layering and
//! "what must come first" read [`KnowledgeMap::prerequisites`].

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::graph::{self, GraphIndex};
use crate::state::Kind;
use crate::Error;

/// One topic in the single-home tree (`Mathematics › Calculus › …`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeTopic {
    pub id: String,
    pub title: String,
    pub parent: Option<String>,
}

/// One proposition: a self-contained claim with a description, living under
/// exactly one topic.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeNode {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub statement: String,
    pub kind: Kind,
    pub home: String,
    #[serde(default)]
    pub correct: u32,
    #[serde(default)]
    pub missed: u32,
    /// Where the claim came from (`session`, `import`, `hunt`, `learner`,
    /// `builder`), as the op recorded it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Value>,
    /// Whose rule of thumb a `heuristic` is (required for that kind).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<String>,
    /// Where a `heuristic` applies, and where it doesn't (required for that
    /// kind).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Ids folded into this node by a `sameAs` op.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
}

/// `from` and `to`, and what the edge claims about them:
///
/// - [`REQUIRES`] — `from` is a prerequisite of `to`; the only relation that
///   orders ideas, is cycle-checked and is drawn as an arrow.
/// - [`CONTRASTS`] — the claims are easily confused; no order implied.
/// - [`SUPPORTS`] / [`ATTACKS`] — the claims bear on each other as arguments.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeEdge {
    pub from: String,
    pub to: String,
    #[serde(default = "requires")]
    pub relation: String,
    /// `load-bearing` (a learner without `from` cannot follow `to`) or
    /// `trivial`; only meaningful for `requires`.
    #[serde(default = "load_bearing")]
    pub weight: String,
    /// Why the edge holds, in the words of whoever proposed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Value>,
}

/// `from` is a prerequisite of `to`.
pub const REQUIRES: &str = "requires";

/// The two claims are easily confused; no order implied.
pub const CONTRASTS: &str = "contrasts";

fn requires() -> String {
    REQUIRES.to_owned()
}

fn load_bearing() -> String {
    graph::load_bearing()
}

impl KnowledgeEdge {
    /// Whether this edge orders the two claims (`requires`). Everything that
    /// reads the graph as a dependency must filter on this.
    pub fn is_requires(&self) -> bool {
        self.relation == REQUIRES
    }
}

/// One argument: `premises` together give `conclusion`. A chain whose ids
/// stopped existing is dropped at load and logged, never fatal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeChain {
    pub chain_id: String,
    pub premises: Vec<String>,
    pub conclusion: String,
    #[serde(default)]
    pub statement: String,
}

/// One group of an accepted (or proposed) topic split.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitGroup {
    pub title: String,
    pub node_ids: Vec<String>,
    /// Why these nodes belong together; shown on the review card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

/// A topic the splitter thinks is too big, with the groups it would become.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposedSplit {
    pub topic: String,
    pub groups: Vec<SplitGroup>,
}

/// Where a topic's material lives: a place to ask, a place people discuss, a
/// reference to read. Attached to a topic, so the guide grows across sessions
/// (`docs/learning-system.md` §6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeVenue {
    /// The topic whose material the venue serves.
    pub topic_id: String,
    /// `ask` | `discussion` | `reference` (`docs/learning-system.md` §6).
    pub kind: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// What the learner must hold for the venue to be useful, e.g. "op-amp
    /// basics".
    pub entry_level: String,
    #[serde(default)]
    pub note: String,
    /// The date someone confirmed the venue is active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<serde_json::Value>,
}

/// Real check totals for one topic (learner evidence, kept separate from the
/// graph's structure).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TopicChecks {
    pub topic: String,
    pub correct: u32,
    pub missed: u32,
}

/// What the Map page draws: the topic tree, its ideas, the edges between them
/// (which may cross topics), the chains, the split proposals, the venues where
/// the field lives and the per-topic check totals.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeMap {
    pub topics: Vec<KnowledgeTopic>,
    pub nodes: Vec<KnowledgeNode>,
    /// Every edge, every relation, with `relation` on the wire.
    pub edges: Vec<KnowledgeEdge>,
    /// Chains whose premises and conclusion still exist.
    #[serde(default)]
    pub chains: Vec<KnowledgeChain>,
    /// Topic-split proposals awaiting a decision.
    #[serde(default)]
    pub splits_proposed: Vec<ProposedSplit>,
    /// Where the field lives, one entry per `(topic, name)`: ask first, then
    /// discussion, then reference; by name within a kind.
    #[serde(default)]
    pub venues: Vec<KnowledgeVenue>,
    /// Per topic, its own venues plus its descendants' (a venue under
    /// `Mathematics › Calculus` is on `Mathematics` too), in the same order,
    /// and only for topics that have any.
    #[serde(default)]
    pub topic_venues: BTreeMap<String, Vec<KnowledgeVenue>>,
    pub topic_checks: BTreeMap<String, Checks>,
    /// Derived at load: node by id, children by topic, edges by end, requires
    /// adjacency, alias → canonical. Never written to disk and never sent to
    /// the UI.
    #[serde(skip)]
    pub index: GraphIndex,
}

impl KnowledgeMap {
    /// The dependency edges only: what must come first. `contrasts`, `supports`
    /// and `attacks` are stored and listed for review but never order
    /// anything, so they are not here.
    pub fn prerequisites(&self) -> impl Iterator<Item = &KnowledgeEdge> {
        self.edges.iter().filter(|e| e.is_requires())
    }

    /// The node an id folds into (`sameAs`), or the id itself.
    pub fn canonical(&self, id: &str) -> String {
        self.index
            .alias
            .get(id)
            .cloned()
            .unwrap_or_else(|| id.to_owned())
    }

    /// Whether the log tombstoned this claim (or the claim it folds into).
    pub fn is_tombstoned(&self, id: &str) -> bool {
        self.index.tombstoned.contains(&self.canonical(id))
    }

    pub fn node(&self, id: &str) -> Option<&KnowledgeNode> {
        let id = self.canonical(id);
        self.index.by_id.get(&id).map(|i| &self.nodes[*i])
    }

    /// The nodes homed in a topic, in map order.
    pub fn topic_nodes(&self, topic: &str) -> Vec<&KnowledgeNode> {
        match self.index.topic_nodes.get(topic) {
            Some(ids) => ids
                .iter()
                .filter_map(|id| self.index.by_id.get(id).map(|i| &self.nodes[*i]))
                .collect(),
            None => self.nodes.iter().filter(|n| n.home == topic).collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Checks {
    pub correct: u32,
    pub missed: u32,
}

/// Reads the graph from `<repo>/graph/`. A missing or empty directory yields an
/// empty map rather than an error: a fresh install has no graph yet. A legacy
/// store (files from before the op log) is migrated once, on the first read.
pub fn load(repo: &Path) -> Result<KnowledgeMap, Error> {
    let dir = repo.join("graph");
    if !dir.is_dir() {
        return Ok(KnowledgeMap::default());
    }
    let (ops, skipped) = graph::read_ops(repo)?;
    if skipped > 0 {
        tracing::warn!(
            skipped,
            "graph/ops.jsonl: {skipped} malformed line(s) skipped; the map is the rest"
        );
    }
    let mut map = graph::fold(&ops);
    for c in read_lines::<TopicChecks>(&dir.join("checks.jsonl"), "graph/checks.jsonl")? {
        map.topic_checks.insert(
            c.topic.clone(),
            Checks {
                correct: c.correct,
                missed: c.missed,
            },
        );
    }
    map.splits_proposed = graph::read_split_file(repo).proposals;
    Ok(map)
}

fn read_lines<T: for<'de> Deserialize<'de>>(path: &Path, label: &str) -> Result<Vec<T>, Error> {
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Kind;
    use std::fs;
    use std::path::PathBuf;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lh-graph-{name}-{}", ulid::Ulid::new()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn legacy(repo: &Path) {
        let dir = repo.join("graph");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("topics.json"),
            r#"{"topics":[
                {"id":"math","title":"Mathematics","parent":null},
                {"id":"alg","title":"Algebra","parent":"math"},
                {"id":"lim","title":"Limits","parent":"math"}]}"#,
        )
        .unwrap();
        fs::write(
            dir.join("nodes.jsonl"),
            concat!(
                r#"{"id":"a","title":"Composition","statement":"f∘g is f(g(x)).","kind":"mechanism","home":"alg"}"#,
                "\n",
                r#"{"id":"b","title":"Epsilon–delta","statement":"For all e there is d.","kind":"mechanism","home":"lim","correct":2,"missed":1}"#,
                "\n",
                r#"{"id":"c","title":"Continuity","statement":"limit equals value","kind":"fact","home":"lim"}"#,
                "\n",
            ),
        )
        .unwrap();
        fs::write(
            dir.join("edges.jsonl"),
            "{\"from\":\"a\",\"to\":\"c\"}\n\
             {\"from\":\"b\",\"to\":\"c\"}\n\
             {\"from\":\"ghost\",\"to\":\"c\"}\n",
        )
        .unwrap();
        fs::write(
            dir.join("checks.jsonl"),
            r#"{"topic":"lim","correct":23,"missed":11}"#,
        )
        .unwrap();
    }

    #[test]
    fn a_missing_graph_directory_is_an_empty_map() {
        let repo = temp("empty");
        let map = load(&repo).unwrap();
        assert!(map.topics.is_empty() && map.nodes.is_empty() && map.edges.is_empty());
        assert!(map.chains.is_empty() && map.splits_proposed.is_empty());
        assert!(map.index.by_id.is_empty());
        let _ = fs::remove_dir_all(repo);
    }

    #[test]
    fn a_legacy_store_is_migrated_once_and_left_untouched() {
        let repo = temp("migrate");
        legacy(&repo);
        let before = (
            fs::read_to_string(repo.join("graph/topics.json")).unwrap(),
            fs::read_to_string(repo.join("graph/nodes.jsonl")).unwrap(),
            fs::read_to_string(repo.join("graph/edges.jsonl")).unwrap(),
        );
        let ops_path = repo.join("graph/ops.jsonl");
        assert!(!ops_path.exists());

        let map = load(&repo).unwrap();
        assert_eq!(map.topics.len(), 3);
        assert_eq!(map.nodes.len(), 3);
        assert_eq!(map.edges.len(), 2, "the dangling edge is dropped");
        assert_eq!(map.topic_checks["lim"].correct, 23);
        assert!(map.nodes.iter().all(|n| n.provenance.is_some()));
        assert_eq!(map.nodes[0].provenance.as_ref().unwrap()["type"], "import");
        assert!(ops_path.is_file(), "the log was written");
        // The legacy files are byte-for-byte what they were.
        assert_eq!(
            fs::read_to_string(repo.join("graph/topics.json")).unwrap(),
            before.0
        );
        assert_eq!(
            fs::read_to_string(repo.join("graph/nodes.jsonl")).unwrap(),
            before.1
        );
        assert_eq!(
            fs::read_to_string(repo.join("graph/edges.jsonl")).unwrap(),
            before.2
        );

        // A new edge appended after migration shows up; the legacy files still
        // don't change, and a second load migrates nothing.
        let bytes = fs::read_to_string(&ops_path).unwrap();
        graph::append(
            &repo,
            &[graph::GraphOp::edge(
                "c",
                "a",
                "contrasts",
                None,
                Some("easily confused"),
                None,
            )],
        )
        .unwrap();
        let again = load(&repo).unwrap();
        assert_eq!(again.edges.len(), 3);
        assert!(fs::read_to_string(repo.join("graph/topics.json")).unwrap() == before.0);
        assert!(fs::read_to_string(&ops_path).unwrap().starts_with(&bytes));
    }

    #[test]
    fn an_edge_line_written_before_the_relation_existed_reads_as_a_prerequisite() {
        let repo = temp("legacy");
        let dir = repo.join("graph");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("topics.json"),
            r#"{"topics":[{"id":"t","title":"T","parent":null}]}"#,
        )
        .unwrap();
        fs::write(
            dir.join("nodes.jsonl"),
            concat!(
                r#"{"id":"a","title":"A","statement":"a","kind":"fact","home":"t"}"#,
                "\n",
                r#"{"id":"b","title":"B","statement":"b","kind":"fact","home":"t"}"#,
                "\n",
            ),
        )
        .unwrap();
        fs::write(
            dir.join("edges.jsonl"),
            "{\"from\":\"a\",\"to\":\"b\"}\n{\"from\":\"b\",\"to\":\"a\",\"relation\":\"contrasts\"}\n",
        )
        .unwrap();
        let map = load(&repo).unwrap();
        assert_eq!(map.edges.len(), 2);
        let deps: Vec<(&str, &str)> = map
            .prerequisites()
            .map(|e| (e.from.as_str(), e.to.as_str()))
            .collect();
        assert_eq!(deps, vec![("a", "b")]);
        assert!(map.edges.iter().all(|e| e.weight == "load-bearing"));
        let json = serde_json::to_string(&map).unwrap();
        assert!(json.contains(r#""relation":"contrasts""#), "{json}");
        let _ = fs::remove_dir_all(repo);
    }

    #[test]
    fn the_op_log_is_read_and_a_malformed_line_is_skipped() {
        let repo = temp("ops");
        let dir = repo.join("graph");
        fs::create_dir_all(&dir).unwrap();
        let ops = [
            graph::GraphOp::topic("t", "T", None),
            graph::GraphOp::node("a", Some("A"), "a", Kind::Fact, "t", None),
            graph::GraphOp::node("b", Some("B"), "b", Kind::Mechanism, "t", None),
            graph::GraphOp::edge("a", "b", "requires", Some("trivial"), Some("uses it"), None),
        ];
        graph::append(&repo, &ops).unwrap();
        let mut text = fs::read_to_string(dir.join("ops.jsonl")).unwrap();
        text.push_str("{\"op\":\"node\",\"opId\":\n");
        text.push_str("not json at all\n");
        fs::write(dir.join("ops.jsonl"), text).unwrap();

        let map = load(&repo).unwrap();
        assert_eq!(map.topics.len(), 1);
        assert_eq!(map.nodes.len(), 2);
        assert_eq!(map.edges.len(), 1);
        assert_eq!(map.edges[0].weight, "trivial");
        assert_eq!(map.edges[0].why.as_deref(), Some("uses it"));
        let _ = fs::remove_dir_all(repo);
    }

    #[test]
    fn the_derived_index_maps_ids_children_edges_and_requires() {
        let repo = temp("index");
        let ops = [
            graph::GraphOp::topic("math", "Mathematics", None),
            graph::GraphOp::topic("alg", "Algebra", Some("math")),
            graph::GraphOp::node("a", None, "a", Kind::Fact, "alg", None),
            graph::GraphOp::node("b", None, "b", Kind::Fact, "math", None),
            graph::GraphOp::edge("a", "b", "requires", None, None, None),
            graph::GraphOp::edge("b", "a", "contrasts", None, None, None),
        ];
        graph::append(&repo, &ops).unwrap();
        let map = load(&repo).unwrap();
        assert_eq!(map.index.by_id.len(), 2);
        assert_eq!(map.index.children["math"], vec!["alg".to_owned()]);
        assert_eq!(map.index.edges_at["a"].len(), 2);
        assert_eq!(map.index.requires["a"], vec!["b".to_owned()]);
        assert!(
            !map.index.requires.contains_key("b"),
            "a contrast orders nothing"
        );
        assert_eq!(map.canonical("a"), "a");
        assert_eq!(map.topic_nodes("alg").len(), 1);
        // Nothing derived is written down.
        let json = serde_json::to_string(&map).unwrap();
        assert!(!json.contains("index"), "{json}");
        let _ = fs::remove_dir_all(repo);
    }

    #[test]
    fn split_proposals_are_read_from_their_file() {
        let repo = temp("splits");
        let dir = repo.join("graph");
        fs::create_dir_all(&dir).unwrap();
        graph::write_split_file(
            &repo,
            &graph::SplitFile {
                proposals: vec![ProposedSplit {
                    topic: "t".to_owned(),
                    groups: vec![SplitGroup {
                        title: "One".to_owned(),
                        node_ids: vec!["a".to_owned()],
                        why: Some("small".to_owned()),
                    }],
                }],
                dismissed: Vec::new(),
            },
        )
        .unwrap();
        let map = load(&repo).unwrap();
        assert_eq!(map.splits_proposed.len(), 1);
        assert_eq!(map.splits_proposed[0].groups[0].title, "One");
        let json = serde_json::to_string(&map).unwrap();
        assert!(json.contains("splitsProposed"), "{json}");
        let _ = fs::remove_dir_all(repo);
    }
}
