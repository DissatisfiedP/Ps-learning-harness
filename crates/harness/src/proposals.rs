//! Edge proposals: the review surface for `extract_edges` (architecture §4.7).
//!
//! The extractor asks a model for edges between claims — `requires`
//! (prerequisite) or `contrasts` (easily confused) — and writes them to
//! `graph/edges.proposed.jsonl` with a `why` each. Nothing about that file is
//! trusted here: this module re-validates every proposal against the graph as
//! it is *now* and appends only what survives, so accepting a batch can never
//! introduce a dangling id, a duplicate or a cycle.
//!
//! The relation is part of the edge, not a label on it: an accepted proposal
//! is appended as an `edge` op, a `requires` and a `contrasts` may both exist
//! between the same pair, and only `requires` is a dependency — cycle checks
//! and duplicate detection therefore treat the two differently.
//!
//! One implementation, two callers: the `extract_edges --accept` path and the
//! Map page's review dialog. A proposal is addressed by `key()`
//! (`from\0to\0relation`), because the file predates the rows having ids.
//!
//! Accepting appends to `graph/ops.jsonl` — the graph has one writer shape
//! (`crate::graph`) — and never rewrites `nodes.jsonl` or
//! `edges.jsonl`; those are only read once, by the migration.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::knowledge::{load, KnowledgeMap, CONTRASTS, REQUIRES};
use crate::Error;

pub const PROPOSALS: &str = "graph/edges.proposed.jsonl";

/// One proposed edge, as the extractor wrote it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposedEdge {
    pub from: String,
    pub to: String,
    /// requires | contrasts
    #[serde(default = "default_relation")]
    pub relation: String,
    /// Why the model thinks so — shown verbatim in the review list, never
    /// interpreted.
    #[serde(default)]
    pub why: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

fn default_relation() -> String {
    "requires".to_owned()
}

impl ProposedEdge {
    /// Stable address: the triple, so a `requires` and a `contrasts` over the
    /// same pair are two proposals, accepted and dismissed separately.
    pub fn key(&self) -> String {
        format!("{}\u{0}{}\u{0}{}", self.from, self.to, self.relation)
    }
}

/// What one decision round did.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    /// Keys appended to the graph (as `edge` ops), in order.
    pub accepted: Vec<String>,
    /// Accepted per relation: a caller can say "2 prerequisites, 1 contrast"
    /// rather than one undifferentiated total.
    pub accepted_by_relation: BTreeMap<String, usize>,
    /// Keys the graph refused, with the reason.
    pub rejected: Vec<Rejection>,
    /// Dismissed per relation (removed from the file without being added).
    pub dismissed_by_relation: BTreeMap<String, usize>,
    /// Pairs removed from the proposal file without being added (dismissed, or
    /// rejected — a refused pair does not come back to be refused again).
    pub removed: usize,
    pub remaining: usize,
}

/// "2 prerequisites and 1 contrast" for a per-relation count; `""` when empty.
/// Prerequisites come first, then the other relations by name, so the same
/// decision always reads the same way.
pub fn phrase(counts: &BTreeMap<String, usize>) -> String {
    let mut entries: Vec<(&String, &usize)> = counts.iter().collect();
    entries.sort_by_key(|(relation, _)| (relation.as_str() != REQUIRES, relation.as_str()));
    let mut parts: Vec<String> = Vec::new();
    for (relation, n) in entries {
        let word = match relation.as_str() {
            REQUIRES => "prerequisite",
            CONTRASTS => "contrast",
            other => other,
        };
        parts.push(if *n == 1 {
            format!("1 {word}")
        } else {
            format!("{n} {word}s")
        });
    }
    parts.join(", ")
}

fn bump(counts: &mut BTreeMap<String, usize>, relation: &str) {
    *counts.entry(relation.to_owned()).or_default() += 1;
}

/// A `key()` rendered for a human: `a → b (requires)`. Anything that is not a
/// key this module made is returned unchanged.
pub fn describe(key: &str) -> String {
    match key.split('\u{0}').collect::<Vec<&str>>().as_slice() {
        [from, to, relation] => format!("{from} → {to} ({relation})"),
        _ => key.to_owned(),
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Rejection {
    pub key: String,
    pub reason: String,
}

/// Reads the proposal file. A malformed line is skipped rather than failing the
/// whole review: the file is a model's output, not the graph.
pub fn proposals(repo: &Path) -> Vec<ProposedEdge> {
    let path = repo.join(PROPOSALS);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<ProposedEdge>(l).ok())
        .filter(|p| !p.from.trim().is_empty() && !p.to.trim().is_empty())
        .collect()
}

/// Applies a decision round: `accept` and `dismiss` name proposals by `key()`.
///
/// Accepted edges that the graph refuses (unknown id, self-edge, bad relation,
/// duplicate of the same relation, `requires` cycle) are reported in
/// [`Decision::rejected`] and dropped from the file — they cannot become edges,
/// so keeping them would only ask again.
pub fn decide(repo: &Path, accept: &[String], dismiss: &[String]) -> Result<Decision, Error> {
    let map = load(repo)?;
    let mut list = proposals(repo);
    let ids: HashSet<&str> = map.nodes.iter().map(|n| n.id.as_str()).collect();
    // Dependencies only: a `contrasts`, `supports` or `attacks` edge claims no
    // order, so it never takes part in the cycle test (and may point both
    // ways).
    let mut deps: Vec<(String, String)> = map
        .prerequisites()
        .map(|e| (e.from.clone(), e.to.clone()))
        .collect();
    // Every edge the folded graph has, both relations, for the duplicate rule.
    let mut edges: HashSet<(String, String, String)> = map
        .edges
        .iter()
        .map(|e| (e.from.clone(), e.to.clone(), e.relation.clone()))
        .collect();

    let take: HashSet<&str> = accept.iter().map(String::as_str).collect();
    let drop: HashSet<&str> = dismiss.iter().map(String::as_str).collect();
    let mut out = Decision::default();
    let mut ops = Vec::new();

    for p in &list {
        let key = p.key();
        if drop.contains(key.as_str()) {
            bump(&mut out.dismissed_by_relation, &p.relation);
            continue;
        }
        if !take.contains(key.as_str()) {
            continue;
        }
        if let Some(reason) = refuse(p, &ids, &edges, &deps) {
            out.rejected.push(Rejection {
                key: key.clone(),
                reason,
            });
            continue;
        }
        edges.insert((p.from.clone(), p.to.clone(), p.relation.clone()));
        if p.relation == REQUIRES {
            // Later proposals in this same round must see it, or two
            // prerequisites could close a loop together.
            deps.push((p.from.clone(), p.to.clone()));
        }
        ops.push(crate::graph::GraphOp::edge(
            &p.from,
            &p.to,
            &p.relation,
            None,
            Some(&p.why),
            Some(crate::graph::provenance("learner", None, &crate::now_iso())),
        ));
        bump(&mut out.accepted_by_relation, &p.relation);
        out.accepted.push(key);
    }

    list.retain(|p| {
        let key = p.key();
        !(take.contains(key.as_str()) || drop.contains(key.as_str()))
    });
    out.removed = proposals(repo).len().saturating_sub(list.len());
    out.remaining = list.len();

    crate::graph::append(repo, &ops)?;
    write_proposals(repo, &list)?;
    Ok(out)
}

/// The rules a proposal must pass to enter the graph. The same ones the
/// extractor applies before it writes a proposal; this is the second gate.
///
/// `edges` is every edge the graph has (both relations, for the duplicate
/// rule); `deps` is the `requires` ones only, for the cycle rule.
fn refuse(
    p: &ProposedEdge,
    ids: &HashSet<&str>,
    edges: &HashSet<(String, String, String)>,
    deps: &[(String, String)],
) -> Option<String> {
    if p.from == p.to {
        return Some("a claim cannot be related to itself".to_owned());
    }
    if !ids.contains(p.from.as_str()) {
        return Some(format!("unknown claim `{}`", p.from));
    }
    if !ids.contains(p.to.as_str()) {
        return Some(format!("unknown claim `{}`", p.to));
    }
    if p.relation != REQUIRES && p.relation != CONTRASTS {
        return Some(format!(
            "relation `{}` is not one the graph stores",
            p.relation
        ));
    }
    if edges.contains(&(p.from.clone(), p.to.clone(), p.relation.clone())) {
        return Some(
            if p.relation == REQUIRES {
                "already a prerequisite"
            } else {
                "already recorded as a contrast"
            }
            .to_owned(),
        );
    }
    // A contrast says the two claims are easily confused, not that one comes
    // first, so it can point either way and closes no loop.
    if p.relation == REQUIRES && reaches(deps, &p.to, &p.from) {
        return Some("would close a prerequisite loop".to_owned());
    }
    None
}

/// Whether `from` can already reach `to` through the `requires` edges (the
/// cycle test). `contrasts` edges are not passed in and never order anything.
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

fn write_proposals(repo: &Path, list: &[ProposedEdge]) -> Result<(), Error> {
    let path = repo.join(PROPOSALS);
    if list.is_empty() {
        // An empty file would read as "nothing to review"; remove it instead.
        if path.is_file() {
            std::fs::remove_file(&path)?;
        }
        return Ok(());
    }
    let mut body = String::new();
    for p in list {
        body.push_str(&serde_json::to_string(p).map_err(Error::from)?);
        body.push('\n');
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, body)?;
    Ok(())
}

/// The proposals plus the graph they would join — one object for the review UI.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Review {
    pub proposals: Vec<ProposedEdge>,
    /// Titles for the ids in each proposal, so the UI never shows a bare id.
    pub titles: std::collections::BTreeMap<String, String>,
}

pub fn review(repo: &Path) -> Result<Review, Error> {
    let map: KnowledgeMap = load(repo)?;
    let proposals = proposals(repo);
    let mut titles: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for p in &proposals {
        for id in [&p.from, &p.to] {
            if titles.contains_key(id) {
                continue;
            }
            if let Some(n) = map.nodes.iter().find(|n| &n.id == id) {
                titles.insert(id.clone(), n.title.clone());
            }
        }
    }
    Ok(Review { proposals, titles })
}

/// Where the proposal file lives, for messages.
pub fn path(repo: &Path) -> PathBuf {
    repo.join(PROPOSALS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("proposals-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("graph")).unwrap();
        std::fs::write(
            dir.join("graph/topics.json"),
            r#"{"topics":[{"id":"t","title":"T","parent":null}]}"#,
        )
        .unwrap();
        let nodes = [
            r#"{"id":"a","title":"A","statement":"a","kind":"fact","home":"t"}"#,
            r#"{"id":"b","title":"B","statement":"b","kind":"fact","home":"t"}"#,
            r#"{"id":"c","title":"C","statement":"c","kind":"fact","home":"t"}"#,
        ]
        .join("\n");
        std::fs::write(dir.join("graph/nodes.jsonl"), format!("{nodes}\n")).unwrap();
        std::fs::write(
            dir.join("graph/edges.jsonl"),
            "{\"from\":\"a\",\"to\":\"b\"}\n",
        )
        .unwrap();
        dir
    }

    fn propose(repo: &Path, from: &str, to: &str, relation: &str) {
        let line = format!(
            "{{\"from\":\"{from}\",\"to\":\"{to}\",\"relation\":\"{relation}\",\"why\":\"because\"}}\n"
        );
        let path = repo.join(PROPOSALS);
        let mut text = std::fs::read_to_string(&path).unwrap_or_default();
        text.push_str(&line);
        std::fs::write(path, text).unwrap();
    }

    fn keys(repo: &Path) -> Vec<String> {
        proposals(repo).iter().map(ProposedEdge::key).collect()
    }

    #[test]
    fn a_proposal_enters_the_graph_and_leaves_the_file() {
        let dir = repo("accept");
        propose(&dir, "b", "c", "requires");
        let key = keys(&dir)[0].clone();
        let out = decide(&dir, std::slice::from_ref(&key), &[]).unwrap();
        assert_eq!(out.accepted, vec![key]);
        assert_eq!(out.remaining, 0);
        let map = load(&dir).unwrap();
        assert!(map.edges.iter().any(|e| e.from == "b" && e.to == "c"));
        assert!(proposals(&dir).is_empty());
    }

    #[test]
    fn a_cycle_is_refused_and_dropped() {
        let dir = repo("cycle");
        // a → b exists; b → a would close it.
        propose(&dir, "b", "a", "requires");
        let key = keys(&dir)[0].clone();
        let out = decide(&dir, &[key], &[]).unwrap();
        assert!(out.accepted.is_empty());
        assert_eq!(out.rejected.len(), 1);
        assert!(out.rejected[0].reason.contains("loop"));
        assert!(
            proposals(&dir).is_empty(),
            "a refused pair must not come back"
        );
    }

    #[test]
    fn an_unknown_id_a_self_edge_and_a_duplicate_are_refused() {
        let dir = repo("bad");
        propose(&dir, "b", "ghost", "requires");
        propose(&dir, "c", "c", "requires");
        propose(&dir, "a", "b", "requires");
        let all: Vec<String> = keys(&dir);
        let out = decide(&dir, &all, &[]).unwrap();
        assert!(out.accepted.is_empty());
        assert_eq!(out.rejected.len(), 3);
        let reasons: String = out.rejected.iter().map(|r| r.reason.clone()).collect();
        assert!(reasons.contains("unknown claim"));
        assert!(reasons.contains("itself"));
        assert!(reasons.contains("already a prerequisite"));
    }

    #[test]
    fn dismissing_removes_without_adding() {
        let dir = repo("dismiss");
        propose(&dir, "b", "c", "requires");
        let key = keys(&dir)[0].clone();
        let out = decide(&dir, &[], &[key]).unwrap();
        assert!(out.accepted.is_empty());
        assert_eq!(out.remaining, 0);
        assert_eq!(load(&dir).unwrap().edges.len(), 1, "the graph is untouched");
    }

    #[test]
    fn deciding_twice_changes_nothing_the_second_time() {
        let dir = repo("idempotent");
        propose(&dir, "b", "c", "requires");
        let key = keys(&dir)[0].clone();
        decide(&dir, std::slice::from_ref(&key), &[]).unwrap();
        let ops = crate::graph::OPS.to_owned();
        let before = std::fs::read_to_string(dir.join(&ops)).unwrap();
        let out = decide(&dir, &[key], &[]).unwrap();
        assert!(out.accepted.is_empty());
        assert_eq!(std::fs::read_to_string(dir.join(&ops)).unwrap(), before);
    }

    #[test]
    fn an_unknown_relation_is_refused() {
        let dir = repo("relation");
        propose(&dir, "b", "c", "mentions");
        let key = keys(&dir)[0].clone();
        let out = decide(&dir, &[key], &[]).unwrap();
        assert_eq!(out.rejected.len(), 1);
        assert!(out.rejected[0].reason.contains("relation"));
    }

    #[test]
    fn a_contrast_is_stored_as_one_and_never_becomes_a_prerequisite() {
        let dir = repo("contrast");
        // The reverse of the graph's a → b prerequisite. A contrast claims no
        // order, so it is not a loop and both directions may exist.
        propose(&dir, "b", "a", "contrasts");
        let key = keys(&dir)[0].clone();
        let out = decide(&dir, std::slice::from_ref(&key), &[]).unwrap();
        assert_eq!(out.accepted.len(), 1, "{:?}", out.rejected);
        assert_eq!(out.accepted_by_relation.get("contrasts"), Some(&1));
        assert!(!out.accepted_by_relation.contains_key("requires"));

        let text = std::fs::read_to_string(dir.join(crate::graph::OPS)).unwrap();
        assert!(text.contains(r#""op":"edge""#), "{text}");
        assert!(text.contains(r#""relation":"contrasts""#), "{text}");
        let map = load(&dir).unwrap();
        let stored = map
            .edges
            .iter()
            .find(|e| e.from == "b" && e.to == "a")
            .expect("the contrast is stored");
        assert_eq!(stored.relation, "contrasts");
        assert_eq!(
            stored.provenance.as_ref().unwrap()["type"],
            "learner",
            "the learner's acceptance is the provenance"
        );
        assert_eq!(stored.why.as_deref(), Some("because"));
        assert_eq!(map.edges.len(), 2, "the prerequisite is still there");
        // The dependency view has only the prerequisite.
        let deps: Vec<(&str, &str)> = map
            .prerequisites()
            .map(|e| (e.from.as_str(), e.to.as_str()))
            .collect();
        assert_eq!(deps, vec![("a", "b")]);
    }

    #[test]
    fn a_contrast_does_not_break_a_prerequisite_cycle() {
        let dir = repo("contrast-cycle");
        // b → c joins the store's a → b, so a → b → c is a chain.
        crate::graph::append(
            &dir,
            &[crate::graph::GraphOp::edge(
                "b", "c", "requires", None, None, None,
            )],
        )
        .unwrap();
        // c → a as a contrast: no order claimed, so it is accepted.
        propose(&dir, "c", "a", "contrasts");
        let key = keys(&dir)[0].clone();
        let out = decide(&dir, &[key], &[]).unwrap();
        assert_eq!(out.accepted.len(), 1, "{:?}", out.rejected);
        // The same pair as a prerequisite is a loop and is still refused.
        propose(&dir, "c", "a", "requires");
        let key = keys(&dir)[0].clone();
        let out = decide(&dir, &[key], &[]).unwrap();
        assert!(out.accepted.is_empty());
        assert!(
            out.rejected[0].reason.contains("loop"),
            "{:?}",
            out.rejected
        );
        assert!(proposals(&dir).is_empty());
    }

    #[test]
    fn the_same_pair_may_be_a_prerequisite_and_a_contrast_but_not_twice() {
        let dir = repo("two-relations");
        // a → b is already a prerequisite; the same pair as a contrast is a
        // different edge and may be added.
        propose(&dir, "a", "b", "contrasts");
        let key = keys(&dir)[0].clone();
        let out = decide(&dir, &[key], &[]).unwrap();
        assert_eq!(out.accepted.len(), 1, "{:?}", out.rejected);
        assert_eq!(load(&dir).unwrap().edges.len(), 2);
        // The same relation twice is a duplicate of that relation.
        propose(&dir, "a", "b", "contrasts");
        let key = keys(&dir)[0].clone();
        let out = decide(&dir, &[key], &[]).unwrap();
        assert!(out.accepted.is_empty());
        assert!(
            out.rejected[0]
                .reason
                .contains("already recorded as a contrast"),
            "{:?}",
            out.rejected
        );
        assert_eq!(load(&dir).unwrap().edges.len(), 2);
    }

    #[test]
    fn the_shared_rules_apply_to_both_relations() {
        let dir = repo("both-rules");
        propose(&dir, "a", "a", "contrasts");
        propose(&dir, "ghost", "a", "contrasts");
        let all = keys(&dir);
        let out = decide(&dir, &all, &[]).unwrap();
        assert!(out.accepted.is_empty());
        let reasons: String = out.rejected.iter().map(|r| r.reason.clone()).collect();
        assert!(reasons.contains("itself"), "{reasons}");
        assert!(reasons.contains("unknown claim"), "{reasons}");
    }

    #[test]
    fn dismissing_is_counted_per_relation() {
        let dir = repo("dismiss-counts");
        propose(&dir, "b", "c", "requires");
        propose(&dir, "c", "b", "contrasts");
        let all = keys(&dir);
        let out = decide(&dir, &[], &all).unwrap();
        assert_eq!(out.dismissed_by_relation.get("requires"), Some(&1));
        assert_eq!(out.dismissed_by_relation.get("contrasts"), Some(&1));
        assert_eq!(load(&dir).unwrap().edges.len(), 1, "nothing added");
    }

    #[test]
    fn a_summary_counts_each_relation_in_words() {
        let mut counts = BTreeMap::new();
        bump(&mut counts, "requires");
        bump(&mut counts, "requires");
        bump(&mut counts, "contrasts");
        assert_eq!(phrase(&counts), "2 prerequisites, 1 contrast");
        assert_eq!(phrase(&BTreeMap::new()), "");
        // A key reads as one line, relation included, so two edges over the
        // same pair never print the same.
        let p = ProposedEdge {
            from: "a".to_owned(),
            to: "b".to_owned(),
            relation: "contrasts".to_owned(),
            why: String::new(),
            model: None,
            at: None,
        };
        assert_eq!(describe(&p.key()), "a → b (contrasts)");
        assert_eq!(describe("nonsense"), "nonsense");
    }

    #[test]
    fn accepting_appends_an_op_and_never_rewrites_the_legacy_edge_file() {
        let dir = repo("append-only");
        // A legacy file with an old-format line, a dangling row and an
        // unsorted order: it migrates once and is then read-only.
        let legacy = "{\"from\":\"zz-ghost\",\"to\":\"a\"}\n\
                      {\"from\":\"a\",\"to\":\"c\"}\n\
                      {\"from\":\"a\",\"to\":\"b\"}\n";
        std::fs::write(dir.join("graph/edges.jsonl"), legacy).unwrap();

        propose(&dir, "b", "c", "requires");
        let key = keys(&dir)[0].clone();
        decide(&dir, &[key], &[]).unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.join("graph/edges.jsonl")).unwrap(),
            legacy,
            "the legacy file is byte-for-byte untouched"
        );
        let ops = std::fs::read_to_string(dir.join(crate::graph::OPS)).unwrap();
        assert!(ops.contains(r#""op":"edge""#), "{ops}");
        assert!(
            ops.contains(r#""from":"b","to":"c","relation":"requires""#),
            "{ops}"
        );
        // The dangling row is in the log but cannot be read as structure.
        assert!(ops.contains("zz-ghost"), "{ops}");
        assert_eq!(load(&dir).unwrap().edges.len(), 3, "the new edge plus two");
    }

    #[test]
    fn the_review_carries_titles_and_tolerates_a_missing_file() {
        let dir = repo("review");
        assert!(review(&dir).unwrap().proposals.is_empty());
        propose(&dir, "b", "c", "requires");
        let r = review(&dir).unwrap();
        assert_eq!(r.proposals.len(), 1);
        assert_eq!(r.titles.get("b").map(String::as_str), Some("B"));
        assert_eq!(r.titles.get("c").map(String::as_str), Some("C"));
    }

    #[test]
    fn a_malformed_line_is_skipped_not_fatal() {
        let dir = repo("malformed");
        std::fs::write(
            dir.join(PROPOSALS),
            "not json\n{\"from\":\"b\",\"to\":\"c\",\"relation\":\"requires\"}\n",
        )
        .unwrap();
        assert_eq!(proposals(&dir).len(), 1);
    }
}
