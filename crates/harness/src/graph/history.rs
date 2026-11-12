//! Where an idea came from (architecture §4.4): every op in `graph/ops.jsonl`
//! that created, updated, moved, split or merged it, oldest first, with the
//! provenance each op recorded. The read model keeps only the latest
//! provenance per node; the Map's card shows the whole record, so the learner
//! can trace a claim back to the session (or import, hunt or their own move)
//! that put it there.
//!
//! An idea's identity is its canonical id plus every id folded into it by a
//! `sameAs`, so a merged duplicate's own origin stays visible.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use super::{fold, read_ops, GraphOp};
use crate::Error;

/// One step in an idea's record.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEvent {
    pub at: String,
    /// `created` | `updated` | `placed` | `split` | `merged`.
    pub kind: String,
    /// The id the op named: the idea's own, or an alias folded into it.
    pub node_id: String,
    /// What the op recorded: `{ type, sessionId?, huntId?, quote?, at? }`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Value>,
    /// `created`/`updated`/`placed`: the topic the op put the idea in;
    /// `split`: the topic it was split out of.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub home: Option<String>,
    /// `placed`: `learner` or `builder`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    /// `split`: the title of the group (new topic) the idea went into.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// `merged`: the other id of the `sameAs` pair, and its title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub other: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub other_title: Option<String>,
    /// The title of the session in `provenance.sessionId`, filled in by the
    /// caller that can read the session list (`Harness::node_provenance`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_title: Option<String>,
}

/// An idea's record, as `node_provenance` returns it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeHistory {
    pub node_id: String,
    pub title: String,
    pub aliases: Vec<String>,
    pub events: Vec<HistoryEvent>,
}

impl HistoryEvent {
    fn new(at: &str, kind: &str, node_id: &str) -> Self {
        HistoryEvent {
            at: at.to_owned(),
            kind: kind.to_owned(),
            node_id: node_id.to_owned(),
            provenance: None,
            home: None,
            by: None,
            group: None,
            other: None,
            other_title: None,
            session_title: None,
        }
    }
}

/// The parts of a provenance that say where a claim came from; two updates
/// with the same source are one point, not two.
fn source_key(p: &Value) -> (String, String, String, String) {
    let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
    (s("type"), s("sessionId"), s("huntId"), s("quote"))
}

/// The record of the idea `id` (or of the idea an alias folds into).
pub fn node_history(repo: &Path, id: &str) -> Result<NodeHistory, Error> {
    let (ops, _) = read_ops(repo)?;
    history(&ops, id.trim())
}

fn history(ops: &[GraphOp], id: &str) -> Result<NodeHistory, Error> {
    let map = fold(ops);
    let Some(node) = map.node(id) else {
        return Err(Error::Invalid(format!(
            "there is no idea `{id}` on the map"
        )));
    };
    let ids: HashSet<&str> = std::iter::once(node.id.as_str())
        .chain(node.aliases.iter().map(String::as_str))
        .collect();

    let mut sorted: Vec<&GraphOp> = ops.iter().collect();
    sorted.sort_by(|a, b| (a.at(), a.op_id()).cmp(&(b.at(), b.op_id())));

    // Titles of every node id, for naming the other side of a merge.
    let mut titles: HashMap<&str, &str> = HashMap::new();
    for op in &sorted {
        if let GraphOp::Node {
            node_id,
            title: Some(t),
            ..
        } = op
        {
            if !t.trim().is_empty() {
                titles.insert(node_id.as_str(), t.trim());
            }
        }
    }

    let mut events: Vec<HistoryEvent> = Vec::new();
    let mut sources: HashMap<&str, Vec<(String, String, String, String)>> = HashMap::new();
    // Each id's home as the fold sees it, so a split that left the idea where
    // the learner had put it is not reported as a move.
    let mut homes: HashMap<&str, (String, bool)> = HashMap::new();
    for op in sorted {
        match op {
            GraphOp::Node {
                at,
                node_id,
                home,
                provenance,
                ..
            } if ids.contains(node_id.as_str()) => {
                let seen = sources.entry(node_id.as_str()).or_default();
                let first = !homes.contains_key(node_id.as_str());
                let entry = homes
                    .entry(node_id.as_str())
                    .or_insert_with(|| (home.trim().to_owned(), false));
                if !entry.1 && !home.trim().is_empty() {
                    entry.0 = home.trim().to_owned();
                }
                let key = provenance.as_ref().map(source_key);
                if !first && key.as_ref().is_none_or(|k| seen.contains(k)) {
                    continue;
                }
                if let Some(k) = key {
                    seen.push(k);
                }
                let mut e =
                    HistoryEvent::new(at, if first { "created" } else { "updated" }, node_id);
                e.provenance = provenance.clone();
                e.home = Some(home.trim().to_owned()).filter(|h| !h.is_empty());
                events.push(e);
            }
            GraphOp::Place {
                at,
                node_id,
                home,
                by,
                provenance,
                ..
            } if ids.contains(node_id.as_str()) => {
                if let Some(entry) = homes.get_mut(node_id.as_str()) {
                    if by == "learner" || !entry.1 {
                        *entry = (home.trim().to_owned(), entry.1 || by == "learner");
                    }
                }
                let mut e = HistoryEvent::new(at, "placed", node_id);
                e.provenance = provenance.clone();
                e.home = Some(home.trim().to_owned());
                e.by = Some(by.clone());
                events.push(e);
            }
            GraphOp::Split {
                at,
                topic,
                groups,
                provenance,
                ..
            } => {
                for group in groups {
                    for node_id in &group.node_ids {
                        if !ids.contains(node_id.as_str()) {
                            continue;
                        }
                        let Some(entry) = homes.get_mut(node_id.as_str()) else {
                            continue;
                        };
                        if entry.0 != *topic || group.title.trim().is_empty() {
                            continue;
                        }
                        // The new topic's id is decided by the fold; the
                        // record names it by title.
                        *entry = (String::new(), true);
                        let mut e = HistoryEvent::new(at, "split", node_id);
                        e.provenance = provenance.clone();
                        e.home = Some(topic.clone());
                        e.group = Some(group.title.trim().to_owned());
                        events.push(e);
                    }
                }
            }
            GraphOp::SameAs { at, a, b, .. }
                if ids.contains(a.as_str()) && ids.contains(b.as_str()) =>
            {
                // Name the side that is not this idea's own id.
                let (here, other) = if *b == node.id { (b, a) } else { (a, b) };
                let mut e = HistoryEvent::new(at, "merged", here);
                e.other = Some(other.clone());
                e.other_title = titles.get(other.as_str()).map(|t| (*t).to_owned());
                events.push(e);
            }
            _ => {}
        }
    }
    Ok(NodeHistory {
        node_id: node.id.clone(),
        title: node.title.clone(),
        aliases: node.aliases.clone(),
        events,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::provenance;
    use crate::knowledge::SplitGroup;
    use crate::state::Kind;

    fn stamped(mut op: GraphOp, when: &str) -> GraphOp {
        match &mut op {
            GraphOp::Topic { at, .. }
            | GraphOp::Node { at, .. }
            | GraphOp::Place { at, .. }
            | GraphOp::Edge { at, .. }
            | GraphOp::SameAs { at, .. }
            | GraphOp::Chain { at, .. }
            | GraphOp::Tombstone { at, .. }
            | GraphOp::Split { at, .. }
            | GraphOp::Venue { at, .. } => *at = when.to_owned(),
        }
        op
    }

    fn log() -> Vec<GraphOp> {
        let session = |s: &str| Some(provenance("session", Some(s), "x"));
        vec![
            stamped(
                GraphOp::topic("t", "Calculus", None),
                "2026-01-01T00:00:00Z",
            ),
            stamped(
                GraphOp::topic("u", "Limits", Some("t")),
                "2026-01-01T00:00:01Z",
            ),
            stamped(
                GraphOp::node(
                    "a",
                    Some("Chain rule"),
                    "d/dx f(g(x))",
                    Kind::Procedure,
                    "t",
                    Some(provenance("import", None, "x")),
                ),
                "2026-01-02T00:00:00Z",
            ),
            stamped(
                GraphOp::node(
                    "b",
                    Some("Chain rule (copy)"),
                    "same",
                    Kind::Procedure,
                    "t",
                    session("s1"),
                ),
                "2026-01-03T00:00:00Z",
            ),
            // The same session upserting again adds no point; a new session does.
            stamped(
                GraphOp::node(
                    "a",
                    None,
                    "d/dx f(g(x)) again",
                    Kind::Procedure,
                    "t",
                    session("s1"),
                ),
                "2026-01-04T00:00:00Z",
            ),
            stamped(
                GraphOp::node(
                    "a",
                    None,
                    "d/dx f(g(x)) again",
                    Kind::Procedure,
                    "t",
                    session("s1"),
                ),
                "2026-01-04T00:00:01Z",
            ),
            stamped(GraphOp::same_as("b", "a"), "2026-01-05T00:00:00Z"),
            stamped(GraphOp::place("a", "u", "learner"), "2026-01-06T00:00:00Z"),
            // The learner moved it out of `t`, so this split doesn't move it.
            stamped(
                GraphOp::split(
                    "t",
                    vec![SplitGroup {
                        title: "Rules".into(),
                        node_ids: vec!["a".into()],
                        why: None,
                    }],
                ),
                "2026-01-07T00:00:00Z",
            ),
        ]
    }

    #[test]
    fn the_record_lists_every_source_merge_and_move_in_order() {
        let h = history(&log(), "b").unwrap();
        assert_eq!(
            h.node_id, "a",
            "an alias resolves to the idea it folds into"
        );
        assert_eq!(h.aliases, ["b"]);
        let kinds: Vec<(&str, &str)> = h
            .events
            .iter()
            .map(|e| (e.kind.as_str(), e.node_id.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [
                ("created", "a"),
                ("created", "b"),
                ("updated", "a"),
                ("merged", "a"),
                ("placed", "a")
            ]
        );
        assert_eq!(h.events[0].provenance.as_ref().unwrap()["type"], "import");
        assert_eq!(h.events[2].provenance.as_ref().unwrap()["sessionId"], "s1");
        assert_eq!(h.events[3].other.as_deref(), Some("b"));
        assert_eq!(
            h.events[3].other_title.as_deref(),
            Some("Chain rule (copy)")
        );
        let placed = &h.events[4];
        assert_eq!(
            (placed.home.as_deref(), placed.by.as_deref()),
            (Some("u"), Some("learner"))
        );
        assert_eq!(placed.provenance.as_ref().unwrap()["type"], "learner");
    }

    #[test]
    fn a_split_that_moved_the_idea_is_on_its_record() {
        let mut ops = log();
        ops.truncate(7); // no learner move
        ops.push(stamped(
            GraphOp::split(
                "t",
                vec![SplitGroup {
                    title: "Rules".into(),
                    node_ids: vec!["a".into()],
                    why: None,
                }],
            ),
            "2026-01-07T00:00:00Z",
        ));
        let h = history(&ops, "a").unwrap();
        let split = h.events.last().unwrap();
        assert_eq!(split.kind, "split");
        assert_eq!(
            (split.home.as_deref(), split.group.as_deref()),
            (Some("t"), Some("Rules"))
        );
        assert!(history(&ops, "nope").is_err());
    }
}
