//! When the graph builder runs over a session, and over which part of it
//! (architecture §4.7).
//!
//! A `graph` job reads one slice of the conversation: the ops after the cut
//! of the newest `done` graph job, through a cut chosen when the job is
//! queued and kept in its args (`after`, `through`). A cut is the op log's
//! own sort key (`at`, then `opId`), like a compaction's, so a sync union
//! that interleaves another device's ops cannot move it. Ops appended while
//! the job runs are after its cut and wait for the next one.
//!
//! Size is counted in **tutor tokens**: characters / 4 of what the tutor
//! wrote (its messages and the arguments of its conversation tool calls).
//! That is the teaching the graph is built from; the learner's side and the
//! app's messages ride along in the transcript but do not start a build.
//!
//! Three triggers:
//! - after every tutor turn, once [`AUTO_TOKENS`] have accumulated since the
//!   newest finished build — so a failing build waits for that much new
//!   teaching instead of retrying on every turn;
//! - at `session_end`, whatever is left;
//! - by hand (the Home list's Extract), once [`MANUAL_TOKENS`] are uncovered.
//!
//! A done build that leaves enough behind (teaching that arrived while it
//! ran) starts the next one through the same rules.

use providers::Role;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::replay::{in_transcript, is_conversation_tool};
use crate::sessions::Op;

/// Tutor tokens that start a build after a turn. One build costs a fixed
/// ~4–5k tokens of prompt, graph summary and plan on every model call of its
/// loop; a slice about that size keeps the overhead near half of what is
/// read, while ~8–10 ordinary tutor replies is a short enough lag for
/// practice between sittings.
pub const AUTO_TOKENS: usize = 4096;

/// Tutor tokens a manual build needs: a quarter of [`AUTO_TOKENS`]. Below
/// that a build is mostly overhead.
pub const MANUAL_TOKENS: usize = AUTO_TOKENS / 4;

/// What asked for a build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// A tutor turn ended, or a build finished.
    Turn,
    /// `session_end`.
    Close,
    /// The learner pressed Extract.
    Manual,
}

impl Trigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Trigger::Turn => "turn",
            Trigger::Close => "close",
            Trigger::Manual => "manual",
        }
    }
}

/// A position in the op log: the sort key of the last op a build covers.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cut {
    pub at: String,
    pub op_id: String,
}

impl Cut {
    fn of(op: &Op) -> Self {
        Cut {
            at: op.at().to_owned(),
            op_id: op.op_id().to_owned(),
        }
    }
}

/// The slice one build reads: after `after` (from the start when `None`)
/// through `through`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Chunk {
    pub after: Option<Cut>,
    pub through: Cut,
}

impl Chunk {
    /// The chunk a queued job's args carry.
    pub fn from_args(args: &Value) -> Option<Chunk> {
        Some(Chunk {
            after: match args.get("after") {
                None | Some(Value::Null) => None,
                Some(v) => Some(serde_json::from_value(v.clone()).ok()?),
            },
            through: serde_json::from_value(args.get("through")?.clone()).ok()?,
        })
    }

    /// The ops of `ops` (sorted by `at`, then `opId`) inside this chunk.
    pub fn slice<'a>(&self, ops: &'a [Op]) -> &'a [Op] {
        let start = self.after.as_ref().map_or(0, |c| position_after(ops, c));
        let end = position_after(ops, &self.through).max(start);
        &ops[start..end]
    }

    /// The newest compaction notes covering nothing past this chunk: the
    /// session's compacted context for the builder. Notes that summarise
    /// teaching after `through` are left out, so the builder never extracts
    /// what a later build will read.
    pub fn notes_before<'a>(&self, ops: &'a [Op]) -> Option<&'a str> {
        let end = (self.through.at.as_str(), self.through.op_id.as_str());
        ops.iter().rev().find_map(|op| match op {
            Op::Compaction(c) if (c.cut_at.as_str(), c.cut_op_id.as_str()) <= end => {
                Some(c.summary.as_str())
            }
            _ => None,
        })
    }
}

/// What the session's graph jobs have done so far.
#[derive(Debug, Default)]
struct Coverage {
    /// The newest cut a `done` build reached.
    covered: Option<Cut>,
    /// The newest cut any finished build (done, failed or aborted) reached.
    attempted: Option<Cut>,
    /// A build is queued or running.
    open: bool,
}

fn coverage(ops: &[Op]) -> Coverage {
    // job id → (cut, still open)
    let mut jobs: Vec<(&str, Cut, bool)> = Vec::new();
    let mut cov = Coverage::default();
    for op in ops {
        let Op::Job(j) = op else { continue };
        if j.kind != "graph" {
            continue;
        }
        if j.state == "queued" {
            // Builds queued before incremental extraction read the whole
            // transcript as it stood: they cover through their own queued op.
            let cut = j
                .args
                .as_ref()
                .and_then(Chunk::from_args)
                .map_or_else(|| Cut::of(op), |c| c.through);
            jobs.push((j.job_id.as_str(), cut, true));
            continue;
        }
        let Some(job) = jobs.iter_mut().find(|(id, _, _)| *id == j.job_id) else {
            continue;
        };
        match j.state.as_str() {
            "done" | "failed" | "aborted" => {
                job.2 = false;
                if j.state == "done" {
                    cov.covered = cov.covered.max(Some(job.1.clone()));
                }
                cov.attempted = cov.attempted.max(Some(job.1.clone()));
            }
            _ => {}
        }
    }
    cov.open = jobs.iter().any(|(_, _, open)| *open);
    cov
}

/// Index of the first op after `cut`.
fn position_after(ops: &[Op], cut: &Cut) -> usize {
    let key = (cut.at.as_str(), cut.op_id.as_str());
    ops.partition_point(|o| (o.at(), o.op_id()) <= key)
}

fn after<'a>(ops: &'a [Op], cut: Option<&Cut>) -> &'a [Op] {
    &ops[cut.map_or(0, |c| position_after(ops, c))..]
}

/// Characters / 4 of what the tutor wrote in `ops`.
fn tutor_tokens(ops: &[Op]) -> usize {
    let chars: usize = ops
        .iter()
        .map(|op| match op {
            Op::Message(m) if m.role == Role::Assistant => m.content.len(),
            Op::Tool(t) if is_conversation_tool(t) && t.result.is_none() => {
                t.args.to_string().len()
            }
            _ => 0,
        })
        .sum();
    chars / 4
}

/// Everything after `from` through the last conversation op. `None` when no
/// conversation op follows `from`.
fn next_chunk(ops: &[Op], from: Option<&Cut>) -> Option<Chunk> {
    let last = after(ops, from).iter().rev().find(|op| in_transcript(op))?;
    Some(Chunk {
        after: from.cloned(),
        through: Cut::of(last),
    })
}

/// The chunk `trigger` should build now: `Ok(None)` when it shouldn't (an
/// automatic trigger with too little new teaching, or a build already open),
/// `Err` with a sentence for the learner when a manual build is refused.
pub fn plan(ops: &[Op], trigger: Trigger) -> Result<Option<Chunk>, String> {
    let cov = coverage(ops);
    if cov.open {
        return match trigger {
            Trigger::Manual => Err("the graph builder is already running for this session".into()),
            _ => Ok(None),
        };
    }
    let uncovered = tutor_tokens(after(ops, cov.covered.as_ref()));
    let due = match trigger {
        Trigger::Turn => tutor_tokens(after(ops, cov.attempted.as_ref())) >= AUTO_TOKENS,
        Trigger::Close => uncovered > 0,
        Trigger::Manual if uncovered >= MANUAL_TOKENS => true,
        Trigger::Manual => {
            return Err(format!(
                "not enough new teaching to extract yet: about {uncovered} tutor tokens since the last extraction, {MANUAL_TOKENS} needed"
            ))
        }
    };
    Ok(due.then(|| next_chunk(ops, cov.covered.as_ref())).flatten())
}

/// What the Home list's Extract item shows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionStatus {
    /// Tutor tokens not yet read by a done build.
    pub uncovered_tokens: usize,
    /// What a manual build needs ([`MANUAL_TOKENS`]).
    pub manual_tokens: usize,
    /// What starts one after a tutor turn ([`AUTO_TOKENS`]).
    pub auto_tokens: usize,
    /// A build is queued or running.
    pub running: bool,
}

pub fn status(ops: &[Op]) -> ExtractionStatus {
    let cov = coverage(ops);
    ExtractionStatus {
        uncovered_tokens: tutor_tokens(after(ops, cov.covered.as_ref())),
        manual_tokens: MANUAL_TOKENS,
        auto_tokens: AUTO_TOKENS,
        running: cov.open,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::{new_message, CompactionOp, JobOp};
    use serde_json::json;

    /// Ops with strictly increasing `at`, like a real log.
    struct Log(Vec<Op>);

    impl Log {
        fn at(&self) -> String {
            format!(
                "2026-11-10T10:{:02}:{:02}.000Z",
                self.0.len() / 60,
                self.0.len() % 60
            )
        }

        fn tutor(&mut self, chars: usize) -> Cut {
            let mut m = new_message(Role::Assistant, "t".repeat(chars));
            m.at = self.at();
            self.0.push(Op::Message(m));
            Cut::of(self.0.last().unwrap())
        }

        fn learner(&mut self, chars: usize) {
            let mut m = new_message(Role::User, "l".repeat(chars));
            m.at = self.at();
            self.0.push(Op::Message(m));
        }

        fn job(&mut self, id: &str, state: &str, args: Option<Value>) {
            let at = self.at();
            self.0.push(Op::Job(JobOp {
                op_id: format!("{at}-{id}"),
                at,
                job_id: id.into(),
                kind: "graph".into(),
                state: state.into(),
                args,
                question: None,
                blocking: false,
                result: None,
                error: None,
            }));
        }

        fn queue(&mut self, id: &str, trigger: Trigger) -> Chunk {
            let chunk = plan(&self.0, trigger).unwrap().expect("a build is due");
            self.job(
                id,
                "queued",
                Some(json!({ "after": chunk.after, "through": chunk.through })),
            );
            chunk
        }
    }

    #[test]
    fn a_turn_builds_only_past_the_threshold_and_each_build_starts_where_the_last_ended() {
        let mut log = Log(Vec::new());
        log.tutor(AUTO_TOKENS * 4 - 8);
        assert_eq!(plan(&log.0, Trigger::Turn), Ok(None), "one token short");
        let end = log.tutor(8);
        let first = log.queue("a", Trigger::Turn);
        assert_eq!((first.after.as_ref(), &first.through), (None, &end));

        // Teaching during the build is after its cut; nothing else starts.
        log.tutor(AUTO_TOKENS * 4);
        assert_eq!(
            plan(&log.0, Trigger::Turn),
            Ok(None),
            "one open build at a time"
        );
        log.job("a", "done", None);

        let second = log.queue("b", Trigger::Turn);
        assert_eq!(second.after, Some(end.clone()));
        assert_eq!(
            tutor_tokens(second.slice(&log.0)),
            AUTO_TOKENS,
            "only the new teaching"
        );
    }

    #[test]
    fn a_failed_build_waits_for_new_teaching_but_its_slice_is_read_again() {
        let mut log = Log(Vec::new());
        log.tutor(AUTO_TOKENS * 4);
        let failed = log.queue("a", Trigger::Turn);
        log.job("a", "failed", None);
        assert_eq!(
            plan(&log.0, Trigger::Turn),
            Ok(None),
            "no retry on every turn"
        );
        assert_eq!(
            status(&log.0).uncovered_tokens,
            AUTO_TOKENS,
            "still uncovered"
        );

        log.tutor(AUTO_TOKENS * 4);
        let retry = log.queue("b", Trigger::Turn);
        assert_eq!(retry.after, None, "the failed slice is read again");
        assert!(retry.through > failed.through, "with the new teaching");
    }

    #[test]
    fn close_takes_any_remainder_and_manual_needs_a_quarter() {
        let mut log = Log(Vec::new());
        log.learner(400);
        assert_eq!(
            plan(&log.0, Trigger::Close),
            Ok(None),
            "no teaching, nothing to read"
        );
        log.tutor(MANUAL_TOKENS * 4 - 4);
        let refused = plan(&log.0, Trigger::Manual).unwrap_err();
        assert!(
            refused.contains(&format!("{MANUAL_TOKENS} needed")),
            "{refused}"
        );
        assert!(plan(&log.0, Trigger::Close).unwrap().is_some());
        log.tutor(4);
        assert!(plan(&log.0, Trigger::Manual).unwrap().is_some());
    }

    #[test]
    fn a_build_from_before_incremental_extraction_covers_up_to_its_queue() {
        let mut log = Log(Vec::new());
        log.tutor(AUTO_TOKENS * 4);
        log.job(
            "old",
            "queued",
            Some(json!({ "sessionId": "s", "reason": "close" })),
        );
        log.job("old", "done", None);
        assert_eq!(status(&log.0).uncovered_tokens, 0);
        assert_eq!(plan(&log.0, Trigger::Close), Ok(None));
    }

    #[test]
    fn the_builder_gets_the_newest_compaction_notes_that_stop_inside_its_part() {
        let mut log = Log(Vec::new());
        let early = log.tutor(400);
        let compact = |log: &mut Log, summary: &str, cut: &Cut| {
            let at = log.at();
            log.0.push(Op::Compaction(CompactionOp {
                op_id: format!("{at}-c"),
                at,
                summary: summary.into(),
                cut_at: cut.at.clone(),
                cut_op_id: cut.op_id.clone(),
            }));
        };
        let chunk = |through: &Cut| Chunk {
            after: None,
            through: through.clone(),
        };
        let middle = log.tutor(400);
        assert_eq!(
            chunk(&middle).notes_before(&log.0),
            None,
            "no compaction yet"
        );
        compact(&mut log, "notes to early", &early);
        let late = log.tutor(400);
        compact(&mut log, "notes to late", &late);
        assert_eq!(chunk(&late).notes_before(&log.0), Some("notes to late"));
        // Notes that run past a part summarise teaching it must not extract.
        assert_eq!(chunk(&middle).notes_before(&log.0), Some("notes to early"));
    }
}
