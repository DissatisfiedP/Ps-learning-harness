//! The frontier list (docs/learning-system.md §5): topics the learner found
//! interesting but off-route, parked by `frontier_park` and kept in
//! `<repo>/frontier.jsonl`.
//!
//! The file is append-only and lives outside any session, because a frontier
//! item outlives the session that found it: the learner can start a new
//! session from any item. Each line is one record:
//!
//! ```text
//! { "op": "frontier", "opId": "…", "at": "…", "topic": "…", "why": "…",
//!   "from": { "sessionId": "…", "huntId": "…" } }
//! ```
//!
//! Reading never fails: the file is a record of what the tutor parked, and a
//! line that doesn't parse is skipped with a warning rather than taking the
//! whole list (and the learner's way back into it) down.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Error;

pub const FRONTIER: &str = "frontier.jsonl";

/// One parked topic, as the command returns it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrontierItem {
    /// The record's `opId`: the id `start_from_frontier` takes.
    pub id: String,
    pub topic: String,
    pub why: String,
    pub at: String,
    pub session_id: String,
}

/// Where the list lives, for messages.
pub fn path(repo: &Path) -> PathBuf {
    repo.join(FRONTIER)
}

/// The line written for one item, in the documented field order.
#[derive(Serialize)]
struct Line<'a> {
    op: &'static str,
    #[serde(rename = "opId")]
    op_id: &'a str,
    at: &'a str,
    topic: &'a str,
    why: &'a str,
    from: From<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct From<'a> {
    session_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    hunt_id: Option<&'a str>,
}

/// What a line must hold to be read back. Extra keys are ignored.
#[derive(Deserialize)]
struct RawLine {
    #[serde(default)]
    op: String,
    #[serde(rename = "opId")]
    op_id: String,
    #[serde(default)]
    at: String,
    #[serde(default)]
    topic: String,
    #[serde(default)]
    why: String,
    #[serde(default)]
    from: Option<RawFrom>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawFrom {
    #[serde(default)]
    session_id: String,
}

/// Appends one item (created on first use) and returns it. `hunt_id` is the
/// hunt the topic came from, when it came from one.
pub fn append(
    repo: &Path,
    session_id: &str,
    hunt_id: Option<&str>,
    topic: &str,
    why: &str,
) -> Result<FrontierItem, Error> {
    let id = crate::sessions::op_id();
    let at = crate::now_iso();
    let line = Line {
        op: "frontier",
        op_id: &id,
        at: &at,
        topic,
        why,
        from: From {
            session_id,
            hunt_id,
        },
    };
    let mut text = serde_json::to_string(&line)?;
    text.push('\n');
    let path = path(repo);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    file.write_all(text.as_bytes())?;
    Ok(FrontierItem {
        id,
        topic: topic.to_owned(),
        why: why.to_owned(),
        at,
        session_id: session_id.to_owned(),
    })
}

/// Reads the list back in append order. A missing file is an empty list; a
/// corrupt line (or one that isn't a frontier record, or names no topic) is
/// skipped with a warning, never an error.
pub fn items(repo: &Path) -> Vec<FrontierItem> {
    let path = path(repo);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let raw: RawLine = match serde_json::from_str(line) {
            Ok(raw) => raw,
            Err(e) => {
                tracing::warn!(
                    "frontier: skipping line {} of {}: {e}",
                    i + 1,
                    path.display()
                );
                continue;
            }
        };
        if raw.op != "frontier" || raw.topic.trim().is_empty() || raw.op_id.is_empty() {
            tracing::warn!(
                "frontier: skipping line {} of {}: not a frontier record",
                i + 1,
                path.display()
            );
            continue;
        }
        out.push(FrontierItem {
            id: raw.op_id,
            topic: raw.topic,
            why: raw.why,
            at: raw.at,
            session_id: raw.from.map(|f| f.session_id).unwrap_or_default(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lh-frontier-{name}-{}-{}",
            std::process::id(),
            ulid::Ulid::new()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_item_is_written_then_read_back() {
        let repo = temp_repo("append");
        let first = append(
            &repo,
            "s-1",
            Some("h-1"),
            "Loaded dividers",
            "the load matters",
        )
        .unwrap();
        let second = append(&repo, "s-1", None, "Op-amp feedback", "why negative?").unwrap();
        assert_ne!(first.id, second.id);

        // The line is the documented shape, one per item, append-only.
        let text = std::fs::read_to_string(path(&repo)).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(v["op"], "frontier");
        assert_eq!(v["opId"], first.id.as_str());
        assert_eq!(v["topic"], "Loaded dividers");
        assert_eq!(v["why"], "the load matters");
        assert_eq!(v["from"]["sessionId"], "s-1");
        assert_eq!(v["from"]["huntId"], "h-1");
        assert_eq!(
            lines[0],
            format!(
                "{{\"op\":\"frontier\",\"opId\":\"{}\",\"at\":\"{}\",\"topic\":\"Loaded dividers\",\"why\":\"the load matters\",\"from\":{{\"sessionId\":\"s-1\",\"huntId\":\"h-1\"}}}}",
                first.id, first.at
            )
        );
        let second_line: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second_line["from"]["sessionId"], "s-1");
        assert!(
            second_line["from"].get("huntId").is_none(),
            "no hunt, no key"
        );

        let items = items(&repo);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], first);
        assert_eq!(items[1], second);
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn a_malformed_line_is_skipped_not_fatal() {
        let repo = temp_repo("corrupt");
        let kept = append(
            &repo,
            "s-2",
            None,
            "Transformer design",
            "want to build one",
        )
        .unwrap();
        let mut text = std::fs::read_to_string(path(&repo)).unwrap();
        text.push_str("{ not json\n");
        text.push_str("{\"op\":\"other\",\"opId\":\"x\",\"topic\":\"nope\"}\n");
        text.push('\n');
        std::fs::write(path(&repo), text).unwrap();

        let items = items(&repo);
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].id, kept.id);
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn an_empty_or_missing_file_reads_as_no_items() {
        let repo = temp_repo("empty");
        assert!(items(&repo).is_empty(), "no file yet");
        std::fs::write(path(&repo), "").unwrap();
        assert!(items(&repo).is_empty(), "an empty file");
        std::fs::write(path(&repo), "\n  \n").unwrap();
        assert!(items(&repo).is_empty(), "blank lines are not records");
        let _ = std::fs::remove_dir_all(repo);
    }
}
