//! Branch sessions (architecture §4.26): a plan whose `tracks` split into
//! parallel strands, each taught in its own session, that later merge back
//! into one.
//!
//! - **Fork at approval.** Approving a plan with tracks keeps the first root
//!   track in the session the plan was made in (the *origin*: no duplicate of
//!   the conversation the learner is in) and creates one new session per
//!   other root track. A fork is a new file whose ops start with a copy of the
//!   origin's ops as they stood at approval (measure transcript, goal, brief,
//!   probe and plan: the *cut*), then its own approval and a `teach` phase op
//!   on its track's first section.
//! - **Merge when the last input ends.** Once every session of a merge
//!   track's `after` tracks has called `session_end`, the merge session is
//!   created: the same copied ops, a `[branches]` summary of each input, a
//!   merged plan whose merge track starts with a code-written revision
//!   section, and its approval.
//! - **Created, not run.** New sessions appear on Home; the tutor runs when
//!   the learner opens one and presses Start (`resume_turn`).
//!
//! The copied ops are marked as already extracted (a finished `graph` job
//! covering the cut): the session they came from builds the graph from them.
//!
//! Sync (docs/sync-build.md): session files are append-only and merged by a
//! union deduplicated by `opId`; nothing here rewrites a file. Fork and merge
//! ids are derived from the origin id and the track id, and every op a fork
//! or merge is created with gets an id and an `at` derived from data every
//! device shares (the cut; the inputs' `session_end` calls). Two devices that
//! create the same merge before they sync write the same file, and its union
//! keeps one copy of each op. Residual risk: the `[branches]` summary is built
//! from what each device has of the inputs; if one device lacks input ops the
//! other has, the two lines with that one `opId` differ, the union keeps the
//! local one, and the devices agree after the next sync round trip. A merge
//! is also only created on a device that has every input's file: an input
//! deleted by the learner keeps its merge waiting.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};

use providers::Role;
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use ulid::Ulid;

use crate::events::AgentEvent;
use crate::replay::{is_conversation_tool, replay_state};
use crate::sessions::{
    self, ApproveOp, BranchChild, BranchOp, ForkLink, Header, JobOp, MergeLink, Mode, Op, PhaseOp,
    RemovedPlugin, SessionSummary, TitleOp,
};
use crate::state::{Kind, Level, Phase, Plan, Section, SessionState, Track};
use crate::{lock, now_iso, Error, Harness};

/// Tag of the app's `[branches]` messages (the UI shows them as notices).
pub(crate) const NOTICE_TAG: &str = "branch-notice";
const NOTICE_PREFIX: &str = "[branches]";
/// Key nodes a merge's revision re-checks per input branch, and in total.
const REVISION_PER_INPUT: usize = 2;
const REVISION_MAX: usize = 6;

// ---------------------------------------------------------------------------
// Lineage, as the session list shows it
// ---------------------------------------------------------------------------

/// Where a session sits in a branching plan (`SessionSummary::lineage`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Lineage {
    /// `origin` | `fork` | `merge` (see [`crate::state::BranchState`]).
    pub kind: String,
    /// The origin session's id, and its title.
    pub origin: String,
    pub origin_title: String,
    pub track: String,
    pub track_title: String,
    /// 1-based position of the track in the plan, and the number of tracks.
    pub index: usize,
    pub count: usize,
    /// A merge session's inputs, in the track's `after` order.
    pub inputs: Vec<LineageInput>,
    /// The merge this session's track feeds, if any.
    pub feeds: Option<LineageFeed>,
    /// This session called `session_end`.
    pub ended: bool,
    /// The tutor wrote something since the session was created (a fork or
    /// merge that is still `false` is ready and never ran).
    pub started: bool,
    /// Titles of the other tracks the merge in `feeds` still waits for
    /// (sessions not ended, or not on this device). Empty once it exists.
    pub waiting_on: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LineageInput {
    /// `None` while that session isn't on this device.
    pub session_id: Option<String>,
    pub track: String,
    pub track_title: String,
    pub ended: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LineageFeed {
    pub track: String,
    pub track_title: String,
    /// The merge session, once it exists.
    pub session_id: Option<String>,
}

impl Lineage {
    /// What a header alone says: forks and merges name their origin and
    /// track; the rest is filled by [`Harness::apply_lineage`].
    pub(crate) fn from_header(h: &Header) -> Option<Lineage> {
        if let Some(f) = &h.fork {
            return Some(Lineage {
                kind: "fork".into(),
                origin: f.parent.clone(),
                track: f.track_id.clone(),
                ..Lineage::default()
            });
        }
        h.merge.as_ref().map(|m| Lineage {
            kind: "merge".into(),
            origin: m.origin.clone(),
            track: m.track_id.clone(),
            ..Lineage::default()
        })
    }
}

// ---------------------------------------------------------------------------
// Ids and timestamps every device derives alike
// ---------------------------------------------------------------------------

/// A ULID with time `ms` and the rest derived from `seed`. `index` fills the
/// top random bits, so ops built in one millisecond sort in build order.
fn derived_ulid(ms: u64, index: u16, seed: &str) -> Ulid {
    let digest = Sha256::digest(seed.as_bytes());
    let mut low = [0u8; 8];
    low.copy_from_slice(&digest[..8]);
    Ulid::from_parts(
        ms,
        (u128::from(index) << 64) | u128::from(u64::from_be_bytes(low)),
    )
}

fn origin_ms(origin: &str) -> u64 {
    Ulid::from_string(origin).map_or(0, |u| u.timestamp_ms())
}

/// The id of the session forked for root track `track` of `origin`.
pub(crate) fn fork_id(origin: &str, track: &str) -> String {
    derived_ulid(origin_ms(origin), 0, &format!("fork\0{origin}\0{track}")).to_string()
}

/// The id of the merge session for merge track `track` of `origin`.
pub(crate) fn merge_id(origin: &str, track: &str) -> String {
    derived_ulid(origin_ms(origin), 0, &format!("merge\0{origin}\0{track}")).to_string()
}

/// The session that teaches `track`: the origin for the first root track, a
/// fork for the other roots, a merge session for a merge track.
pub(crate) fn track_session(origin: &str, plan: &Plan, track: &str) -> String {
    let is_root = plan.track(track).is_some_and(|t| t.after.is_empty());
    if !is_root {
        merge_id(origin, track)
    } else if plan.root_tracks().first().is_some_and(|r| r.id == track) {
        origin.to_owned()
    } else {
        fork_id(origin, track)
    }
}

fn iso(ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(i64::try_from(ms).unwrap_or(i64::MAX))
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn ms_of(at: &str) -> u64 {
    chrono::DateTime::parse_from_rfc3339(at)
        .map(|d| u64::try_from(d.timestamp_millis()).unwrap_or(0))
        .unwrap_or(0)
}

/// Ids and one `at` for the ops a new session is created with.
struct Stamp {
    ms: u64,
    at: String,
    seed: String,
    next: u16,
}

impl Stamp {
    fn new(ms: u64, session: &str) -> Self {
        Stamp {
            ms,
            at: iso(ms),
            seed: format!("{session}\0created"),
            next: 0,
        }
    }

    fn op_id(&mut self) -> String {
        let id = derived_ulid(self.ms, self.next, &self.seed).to_string();
        self.next += 1;
        id
    }

    fn message(&mut self, tag: &str, text: String) -> Op {
        let mut m = sessions::new_message(Role::User, text);
        m.op_id = self.op_id();
        m.at = self.at.clone();
        m.tag = Some(tag.to_owned());
        Op::Message(m)
    }

    fn title(&mut self, title: &str) -> Op {
        Op::Title(TitleOp {
            op_id: self.op_id(),
            at: self.at.clone(),
            title: title.to_owned(),
        })
    }

    fn phase(&mut self, section: Option<String>) -> Op {
        Op::Phase(PhaseOp {
            op_id: self.op_id(),
            at: self.at.clone(),
            phase: if section.is_some() {
                Phase::Teach
            } else {
                Phase::Close
            },
            section_id: section,
        })
    }

    fn job(&mut self, job_id: &str, kind: &str, state: &str) -> JobOp {
        JobOp {
            op_id: self.op_id(),
            at: self.at.clone(),
            job_id: job_id.to_owned(),
            kind: kind.to_owned(),
            state: state.to_owned(),
            args: None,
            question: None,
            blocking: false,
            result: None,
            error: None,
        }
    }

    /// Jobs still open in the copied ops belong to the session they ran in;
    /// closing them here keeps this session's recovery from running them a
    /// second time.
    fn close_copied_jobs(&mut self, copied: &SessionState, out: &mut Vec<Op>) {
        for job in &copied.open_jobs {
            let mut op = self.job(&job.job_id, &job.kind, "aborted");
            op.blocking = job.blocking;
            op.error = Some("left with the session this one was copied from".into());
            out.push(Op::Job(op));
        }
    }

    /// A finished graph build covering everything through `through`: the
    /// copied ops are extracted by the session they came from (and a merge's
    /// summary by the branches it summarises), never again here.
    fn cover_copied(&mut self, session: &str, through: (&str, &str), out: &mut Vec<Op>) {
        let job_id = self.op_id();
        let mut queued = self.job(&job_id, "graph", "queued");
        queued.args = Some(json!({
            "sessionId": session,
            "reason": "copy",
            "after": null,
            "through": { "at": through.0, "opId": through.1 },
        }));
        queued.question = Some("already extracted in the session it was copied from".into());
        let mut done = self.job(&job_id, "graph", "done");
        done.result = Some(json!({ "copied": true }));
        out.push(Op::Job(queued));
        out.push(Op::Job(done));
    }
}

/// The ops of `ops` through the cut, without the header: what every session
/// of one branching plan starts with.
fn copied_prefix(ops: &[Op], at: &str, op_id: &str) -> Vec<Op> {
    ops.iter()
        .filter(|o| !matches!(o, Op::Session(_)) && (o.at(), o.op_id()) <= (at, op_id))
        .cloned()
        .collect()
}

fn section_title(plan: &Plan, id: &str) -> String {
    plan.sections
        .iter()
        .find(|s| s.id == id)
        .map_or_else(|| id.to_owned(), |s| s.title.clone())
}

fn track_title(plan: &Plan, id: &str) -> String {
    plan.track(id)
        .map_or_else(|| id.to_owned(), |t| t.title.clone())
}

fn node_title(plan: &Plan, id: &str) -> String {
    plan.nodes
        .iter()
        .find(|n| n.id == id)
        .map_or_else(|| id.to_owned(), |n| n.title.clone())
}

fn joined(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_owned()
    } else {
        items.join("; ")
    }
}

// ---------------------------------------------------------------------------
// What one session of a family says about itself
// ---------------------------------------------------------------------------

/// One family session read in full (cached by file size and mtime).
pub(crate) struct Facts {
    ops: Vec<Op>,
    header: Header,
    state: SessionState,
    /// The copied cut: the header's for a fork or merge, the `branch` op's
    /// for the origin.
    cut: Option<(String, String)>,
    /// `at` of the latest `session_end` call (written before it runs, so it
    /// exists while `session_end` executes).
    end_at: Option<String>,
    /// The tutor wrote a message after the cut.
    started: bool,
}

fn facts_of(ops: Vec<Op>) -> Option<Facts> {
    let header = ops.iter().find_map(|o| match o {
        Op::Session(h) => Some(h.clone()),
        _ => None,
    })?;
    let state = replay_state(&ops);
    let cut = header
        .fork
        .as_ref()
        .map(|f| (f.up_to_at.clone(), f.up_to_op_id.clone()))
        .or_else(|| {
            header
                .merge
                .as_ref()
                .map(|m| (m.up_to_at.clone(), m.up_to_op_id.clone()))
        })
        .or_else(|| {
            ops.iter().rev().find_map(|o| match o {
                Op::Branch(b) => Some((b.up_to_at.clone(), b.up_to_op_id.clone())),
                _ => None,
            })
        });
    let end_at = ops.iter().rev().find_map(|o| match o {
        Op::Tool(t) if t.name == "session_end" && t.result.is_none() && is_conversation_tool(t) => {
            Some(t.at.clone())
        }
        _ => None,
    });
    let started = cut.as_ref().is_none_or(|(at, id)| {
        ops.iter().any(|o| {
            matches!(o, Op::Message(m) if m.role == Role::Assistant
                && !m.content.trim().is_empty()
                && (m.at.as_str(), m.op_id.as_str()) > (at.as_str(), id.as_str()))
        })
    });
    Some(Facts {
        ops,
        header,
        state,
        cut,
        end_at,
        started,
    })
}

/// Family sessions read in full, keyed by file path, with the file stamp
/// they were read at. Process-wide: a path names one repo's file.
static FACTS: LazyLock<Mutex<FactsCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Path → (file length and mtime, the facts read at that stamp).
type FactsCache = HashMap<PathBuf, ((u64, u64), Arc<Facts>)>;

// ---------------------------------------------------------------------------
// Merged plan
// ---------------------------------------------------------------------------

/// The plan a merge session teaches from.
struct Merged {
    plan: Plan,
    /// Every section the inputs completed.
    completed: Vec<String>,
    /// The revision section, when the inputs completed anything.
    revision: Option<Section>,
    /// Input sections left unfinished, moved to the merge track after the
    /// revision.
    leftovers: Vec<String>,
}

/// Each input's own version of its track (and of the tracks merged into it
/// before) replaces the first input's copy: a branch's plan revisions only
/// touch its own track. Unfinished input sections move to the merge track,
/// right after a revision section on the key nodes of each input; a nested
/// merge's unfinished revision is dropped (this one replaces it).
fn merge_plan(m: &Track, inputs: &[(&str, &SessionState)]) -> Option<Merged> {
    let mut plan = inputs.first()?.1.plan.clone()?;
    let mut completed: Vec<String> = Vec::new();
    for (track, st) in inputs {
        let Some(p) = &st.plan else { continue };
        for c in &st.completed_sections {
            if !completed.contains(c) {
                completed.push(c.clone());
            }
        }
        for u in p.upstream(track) {
            let Some(src) = p.track(&u) else { continue };
            let old: Vec<String> = plan
                .track(&u)
                .map(|t| t.section_ids.clone())
                .unwrap_or_default();
            plan.sections.retain(|s| !old.contains(&s.id));
            if let Some(dst) = plan.tracks.iter_mut().find(|t| t.id == u) {
                dst.section_ids.clone_from(&src.section_ids);
            }
            for sid in &src.section_ids {
                let Some(section) = p.sections.iter().find(|s| &s.id == sid) else {
                    continue;
                };
                plan.sections.retain(|s| &s.id != sid);
                plan.sections.push(section.clone());
                for n in &section.node_ids {
                    if let Some(node) = p.nodes.iter().find(|x| &x.id == n) {
                        plan.nodes.retain(|x| &x.id != n);
                        plan.nodes.push(node.clone());
                    }
                }
            }
        }
    }

    let upstream: Vec<String> = m.after.iter().flat_map(|t| plan.upstream(t)).collect();
    let mut leftovers = Vec::new();
    let mut dropped = Vec::new();
    for u in &upstream {
        for sid in plan
            .track(u)
            .map(|t| t.section_ids.clone())
            .unwrap_or_default()
        {
            if completed.contains(&sid) {
                continue;
            }
            if plan.sections.iter().any(|s| s.id == sid && s.revision) {
                dropped.push(sid);
            } else {
                leftovers.push(sid);
            }
        }
    }
    for t in &mut plan.tracks {
        t.section_ids
            .retain(|s| !leftovers.contains(s) && !dropped.contains(s));
    }
    plan.sections.retain(|s| !dropped.contains(&s.id));

    let nodes = key_nodes(inputs);
    let revision = (!nodes.is_empty()).then(|| {
        let mut id = format!("revise-{}", m.id);
        let mut n = 2;
        while plan.sections.iter().any(|s| s.id == id) {
            id = format!("revise-{}-{n}", m.id);
            n += 1;
        }
        let titles: Vec<String> = m.after.iter().map(|t| track_title(&plan, t)).collect();
        Section {
            id,
            title: format!("Revision: {}", titles.join(" + ")),
            mode: majority_kind(&plan, &nodes),
            node_ids: nodes,
            style: "mixed".into(),
            plugins: Vec::new(),
            notes: "Written by the app before the merged track: the learner taught these nodes in separate branch sessions (the [branches] message says what each covered and where they struggled). Ask one fresh check per node, in a new context, before anything new; re-teach briefly only what misses.".into(),
            exit: "Each node listed passes one fresh check.".into(),
            desktop_only: false,
            needs_install: Vec::new(),
            target_level: Some(Level::Explain),
            assumed_known: false,
            known_note: String::new(),
            learner_note: String::new(),
            hunt: None,
            revision: true,
        }
    });
    if let Some(dst) = plan.tracks.iter_mut().find(|t| t.id == m.id) {
        let mut ids: Vec<String> = revision.iter().map(|s| s.id.clone()).collect();
        ids.extend(leftovers.iter().cloned());
        ids.extend(dst.section_ids.iter().cloned());
        dst.section_ids = ids;
    }
    if let Some(r) = &revision {
        plan.sections.push(r.clone());
    }
    // Plan order follows the tracks, so the card reads lane by lane.
    let mut ordered: Vec<Section> = Vec::new();
    for t in &plan.tracks {
        for sid in &t.section_ids {
            if let Some(s) = plan.sections.iter().find(|s| &s.id == sid) {
                ordered.push(s.clone());
            }
        }
    }
    for s in &plan.sections {
        if !ordered.iter().any(|o| o.id == s.id) {
            ordered.push(s.clone());
        }
    }
    plan.sections = ordered;
    Some(Merged {
        plan,
        completed,
        revision,
        leftovers,
    })
}

/// Per input, up to [`REVISION_PER_INPUT`] nodes of the sections it
/// completed: the ones with a missed check first, then the latest section's.
fn key_nodes(inputs: &[(&str, &SessionState)]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (track, st) in inputs {
        let Some(p) = &st.plan else { continue };
        let upstream = p.upstream(track);
        let done: Vec<&Section> = st
            .completed_sections
            .iter()
            .filter_map(|id| p.sections.iter().find(|s| &s.id == id))
            .filter(|s| !s.revision && p.track_of(&s.id).is_some_and(|t| upstream.contains(&t.id)))
            .collect();
        let missed = |n: &String| {
            st.checks
                .get(n)
                .is_some_and(|v| v.iter().any(|c| c.verdict != "correct"))
        };
        let mut picked: Vec<String> = Vec::new();
        let candidates = done
            .iter()
            .flat_map(|s| s.node_ids.iter())
            .filter(|&n| missed(n))
            .chain(done.iter().rev().flat_map(|s| s.node_ids.iter()));
        for n in candidates {
            if picked.len() == REVISION_PER_INPUT {
                break;
            }
            if !picked.contains(n) && !out.contains(n) {
                picked.push(n.clone());
            }
        }
        out.extend(picked);
    }
    out.truncate(REVISION_MAX);
    out
}

fn majority_kind(plan: &Plan, nodes: &[String]) -> Kind {
    let mut counts: Vec<(Kind, usize)> = Vec::new();
    for n in nodes {
        let Some(kind) = plan.nodes.iter().find(|x| &x.id == n).map(|x| x.kind) else {
            continue;
        };
        match counts.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, c)) => *c += 1,
            None => counts.push((kind, 1)),
        }
    }
    counts
        .into_iter()
        .max_by_key(|(_, c)| *c)
        .map_or(Kind::Mechanism, |(k, _)| k)
}

/// What one input branch did, for the merge's `[branches]` message.
fn input_summary(track: &str, session_title: &str, f: &Facts, leftovers: &[String]) -> String {
    let st = &f.state;
    let Some(p) = &st.plan else {
        return String::new();
    };
    let upstream = p.upstream(track);
    let in_scope = |id: &str| p.track_of(id).is_some_and(|t| upstream.contains(&t.id));
    let done: Vec<String> = st
        .completed_sections
        .iter()
        .filter(|id| in_scope(id))
        .map(|id| section_title(p, id))
        .collect();
    let unfinished: Vec<String> = leftovers
        .iter()
        .filter(|id| in_scope(id))
        .map(|id| section_title(p, id))
        .collect();
    // Accepted `section_done` calls: evidence per section, the latest carry-over.
    let mut accepted: Vec<&str> = Vec::new();
    for o in &f.ops {
        if let Op::Tool(t) = o {
            if t.name == "section_done" && t.result.as_ref().is_some_and(|r| !r.is_error) {
                accepted.push(t.call_id.as_str());
            }
        }
    }
    let mut evidence = Vec::new();
    let mut carry = String::new();
    for o in &f.ops {
        if let Op::Tool(t) = o {
            if t.name == "section_done"
                && t.result.is_none()
                && accepted.contains(&t.call_id.as_str())
            {
                let sid = t
                    .args
                    .get("sectionId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if !in_scope(sid) {
                    continue;
                }
                if let Some(e) = t.args.get("evidence").and_then(|v| v.as_str()) {
                    evidence.push(format!(
                        "{}: {}",
                        section_title(p, sid),
                        crate::agent::clip(e, 240)
                    ));
                }
                if let Some(c) = t.args.get("carryOver").and_then(|v| v.as_str()) {
                    c.clone_into(&mut carry);
                }
            }
        }
    }
    let nodes: Vec<&String> = p
        .sections
        .iter()
        .filter(|s| in_scope(&s.id))
        .flat_map(|s| s.node_ids.iter())
        .collect();
    let (mut passed, mut missed) = (0, 0);
    let mut struggled: Vec<String> = Vec::new();
    for n in &nodes {
        for c in st.checks.get(*n).into_iter().flatten() {
            if c.verdict == "correct" {
                passed += 1;
            } else {
                missed += 1;
                let title = node_title(p, n);
                if !struggled.contains(&title) {
                    struggled.push(title);
                }
            }
        }
    }
    let mut out = format!(
        "**{}** (session \"{session_title}\"{})\n- Done: {}\n",
        track_title(p, track),
        if st.ended { "" } else { ", not ended" },
        joined(&done),
    );
    if !unfinished.is_empty() {
        out.push_str(&format!(
            "- Left unfinished, taught here after the revision: {}\n",
            joined(&unfinished)
        ));
    }
    out.push_str(&format!("- Evidence: {}\n", joined(&evidence)));
    if !carry.trim().is_empty() {
        out.push_str(&format!(
            "- Carry-over: {}\n",
            crate::agent::clip(carry.trim(), 300)
        ));
    }
    out.push_str(&format!("- Checks: {passed} passed, {missed} missed"));
    if !struggled.is_empty() {
        out.push_str(&format!("; struggled with: {}", struggled.join(", ")));
    }
    out.push('\n');
    out
}

// ---------------------------------------------------------------------------
// The harness side
// ---------------------------------------------------------------------------

impl Harness {
    /// A family session read in full, from a cache keyed by file size and
    /// mtime. `None` when it isn't on this device (or has no header).
    pub(crate) fn branch_facts(&self, id: &str) -> Option<Arc<Facts>> {
        let stamp = self.store.file_stamp(id).ok()?;
        let key = self.repo.join(id);
        if let Some((s, f)) = lock(&FACTS).get(&key) {
            if *s == stamp {
                return Some(Arc::clone(f));
            }
        }
        let facts = Arc::new(facts_of(self.store.read_uncached(id).ok()?)?);
        lock(&FACTS).insert(key, (stamp, Arc::clone(&facts)));
        Some(facts)
    }

    /// Writes a new fork or merge session, grants its plan plugins (as an
    /// approval does) and tells the UI. `None` when it already existed.
    fn create_branch_session(
        &self,
        ops: &[Op],
        removed: &[RemovedPlugin],
    ) -> Result<Option<SessionSummary>, Error> {
        let Some(summary) = self.store.create_from(ops)? else {
            return Ok(None);
        };
        let state = replay_state(&self.store.ops(&summary.id)?);
        if let Some(plan) = state.plan.clone() {
            self.grant_plan_plugins(&summary.id, &state, &plan, removed)?;
        }
        Ok(Some(summary))
    }

    /// Fork every root track but the first into its own session (§4.26).
    /// `approve` calls this before writing its own ops, with the origin's
    /// ops as they stand: they become each fork's copied start. Returns the
    /// origin's `branch` op and the notice for its transcript.
    pub(crate) fn fork_tracks(
        &self,
        origin: &str,
        ops: &[Op],
        state: &SessionState,
        removed: &[RemovedPlugin],
    ) -> Result<(BranchOp, String), Error> {
        let plan = state
            .plan
            .as_ref()
            .ok_or_else(|| Error::Invalid("there is no plan to branch".into()))?;
        let roots = plan.root_tracks();
        let own = roots
            .first()
            .ok_or_else(|| Error::Invalid("the plan's tracks have no root track".into()))?;
        let header = ops
            .iter()
            .find_map(|o| match o {
                Op::Session(h) => Some(h.clone()),
                _ => None,
            })
            .ok_or_else(|| Error::Corrupt(format!("session {origin} has no header")))?;
        let last = ops
            .last()
            .ok_or_else(|| Error::Corrupt(format!("session {origin} is empty")))?;
        let (cut_at, cut_id) = (last.at().to_owned(), last.op_id().to_owned());
        let family = state.title.clone().unwrap_or_else(|| header.title.clone());
        let mut children = Vec::new();
        for track in roots.iter().skip(1) {
            let id = fork_id(origin, &track.id);
            let title = format!("{family} · {}", track.title);
            let mut stamp = Stamp::new(ms_of(&cut_at) + 1, &id);
            let mut out = vec![Op::Session(Header {
                op_id: header.op_id.clone(),
                at: header.at.clone(),
                id: id.clone(),
                mode: Mode::Learn,
                title: title.clone(),
                fork: Some(ForkLink {
                    parent: origin.to_owned(),
                    up_to_at: cut_at.clone(),
                    up_to_op_id: cut_id.clone(),
                    track_id: track.id.clone(),
                }),
                merge: None,
            })];
            out.extend(copied_prefix(ops, &cut_at, &cut_id));
            out.push(stamp.title(&title));
            out.push(stamp.message(NOTICE_TAG, fork_notice(plan, track, &family)));
            out.push(Op::Approve(ApproveOp {
                op_id: stamp.op_id(),
                at: stamp.at.clone(),
                plan_version: state.plan_version,
                removed_plugins: removed.to_vec(),
            }));
            stamp.close_copied_jobs(state, &mut out);
            stamp.cover_copied(&id, (&cut_at, &cut_id), &mut out);
            let first = track
                .section_ids
                .iter()
                .find(|s| !state.completed_sections.contains(s))
                .cloned();
            let first_title = first
                .as_deref()
                .map_or_else(|| "(none)".to_owned(), |s| section_title(plan, s));
            out.push(stamp.phase(first));
            out.push(stamp.message(
                "plan-notice",
                format!(
                    "[plan update] Plan approved (version {}). Teaching starts with: {first_title}.",
                    state.plan_version
                ),
            ));
            if let Some(summary) = self.create_branch_session(&out, removed)? {
                self.announce(&summary.id);
            }
            children.push(BranchChild {
                session_id: id,
                track: track.id.clone(),
            });
        }
        let names: Vec<String> = roots
            .iter()
            .skip(1)
            .map(|t| format!("\"{family} · {}\"", t.title))
            .collect();
        let notice = format!(
            "{NOTICE_PREFIX} This plan splits into {} tracks that run as separate sessions and merge later. This session keeps branch 1 of {}: **{}** ({}). The other branches are new sessions on Home, ready to start when you open them: {}.{}",
            plan.tracks.len(),
            plan.tracks.len(),
            own.title,
            own.section_ids
                .iter()
                .map(|s| section_title(plan, s))
                .collect::<Vec<_>>()
                .join(", "),
            names.join(", "),
            feeds_line(plan, &own.id),
        );
        Ok((
            BranchOp {
                op_id: sessions::op_id(),
                at: now_iso(),
                track: own.id.clone(),
                plan_version: state.plan_version,
                up_to_at: cut_at,
                up_to_op_id: cut_id,
                children,
            },
            notice,
        ))
    }

    /// Creates the merge session of every merge track whose input sessions
    /// have all ended (§4.26). `ending` is a session whose `session_end` is
    /// running right now: its result op isn't written yet, so it counts as
    /// ended here. Idempotent: an existing merge (made here or synced in) is
    /// left alone. Returns the sessions it created.
    pub(crate) fn create_ready_merges(
        &self,
        origin: &str,
        plan: &Plan,
        ending: Option<&str>,
    ) -> Result<Vec<SessionSummary>, Error> {
        static ONE: Mutex<()> = Mutex::new(());
        let _one = lock(&ONE);
        let mut created = Vec::new();
        for m in plan.tracks.iter().filter(|t| !t.after.is_empty()) {
            let id = merge_id(origin, &m.id);
            if self.store.exists(&id) {
                continue;
            }
            let mut inputs: Vec<(String, String, Arc<Facts>)> = Vec::new();
            for t in &m.after {
                let sid = track_session(origin, plan, t);
                let Some(f) = self.branch_facts(&sid) else {
                    break;
                };
                if !(f.state.ended || ending == Some(sid.as_str())) {
                    break;
                }
                inputs.push((t.clone(), sid, f));
            }
            if inputs.len() != m.after.len() {
                continue;
            }
            if let Some(summary) = self.create_merge(origin, m, &id, &inputs)? {
                created.push(summary);
            }
        }
        Ok(created)
    }

    fn create_merge(
        &self,
        origin: &str,
        m: &Track,
        id: &str,
        inputs: &[(String, String, Arc<Facts>)],
    ) -> Result<Option<SessionSummary>, Error> {
        // Every session of the family starts with the same copied ops: the
        // origin's when this device has it, else any input's copy.
        let origin_facts = self.branch_facts(origin);
        let source = origin_facts
            .clone()
            .filter(|f| f.cut.is_some())
            .or_else(|| {
                inputs
                    .iter()
                    .map(|i| Arc::clone(&i.2))
                    .find(|f| f.cut.is_some())
            })
            .ok_or_else(|| {
                Error::Invalid("the branch sessions don't say where their shared start ends".into())
            })?;
        let Some((cut_at, cut_id)) = source.cut.clone() else {
            return Ok(None);
        };
        let prefix = copied_prefix(&source.ops, &cut_at, &cut_id);
        let copied = replay_state(&prefix);
        let family = origin_facts
            .as_ref()
            .map(|f| {
                f.state
                    .title
                    .clone()
                    .unwrap_or_else(|| f.header.title.clone())
            })
            .or_else(|| copied.title.clone())
            .unwrap_or_else(|| "Branching plan".to_owned());
        let states: Vec<(&str, &SessionState)> = inputs
            .iter()
            .map(|(t, _, f)| (t.as_str(), &f.state))
            .collect();
        let Some(merged) = merge_plan(m, &states) else {
            return Ok(None);
        };
        let removed: Vec<RemovedPlugin> = {
            let mut all: Vec<RemovedPlugin> = Vec::new();
            for (_, _, f) in inputs {
                if let Some(Op::Approve(a)) =
                    f.ops.iter().rev().find(|o| matches!(o, Op::Approve(_)))
                {
                    for r in &a.removed_plugins {
                        if !all.contains(r) {
                            all.push(r.clone());
                        }
                    }
                }
            }
            all
        };
        let end_ms = inputs
            .iter()
            .filter_map(|(_, _, f)| f.end_at.as_deref().map(ms_of))
            .max()
            .unwrap_or(0)
            .max(ms_of(&cut_at))
            + 1;
        let mut stamp = Stamp::new(end_ms, id);
        let title = format!("{family} · {}", m.title);
        let mut out = vec![Op::Session(Header {
            op_id: source.header.op_id.clone(),
            at: source.header.at.clone(),
            id: id.to_owned(),
            mode: Mode::Learn,
            title: title.clone(),
            fork: None,
            merge: Some(MergeLink {
                origin: origin.to_owned(),
                parents: inputs.iter().map(|(_, sid, _)| sid.clone()).collect(),
                up_to_at: cut_at.clone(),
                up_to_op_id: cut_id.clone(),
                track_id: m.id.clone(),
                completed: merged.completed.clone(),
            }),
        })];
        out.extend(prefix);
        out.push(stamp.title(&title));
        // The first thing the merge says: what each branch did, and the
        // revision that comes first.
        let own: Vec<String> = merged
            .plan
            .track(&m.id)
            .map(|t| {
                t.section_ids
                    .iter()
                    .filter(|s| merged.revision.as_ref().is_none_or(|r| &r.id != *s))
                    .map(|s| section_title(&merged.plan, s))
                    .collect()
            })
            .unwrap_or_default();
        let mut text = format!(
            "{NOTICE_PREFIX} Every branch feeding **{}** has ended. This session merges them and continues the plan.\n\n",
            m.title
        );
        match &merged.revision {
            Some(r) => text.push_str(&format!(
                "It starts with a short revision ({}): one fresh check on each of {}; anything that misses is re-taught briefly. Then: {}.\n\n",
                r.title,
                r.node_ids
                    .iter()
                    .map(|n| node_title(&merged.plan, n))
                    .collect::<Vec<_>>()
                    .join(", "),
                own.join(", "),
            )),
            None => text.push_str(&format!("Next: {}.\n\n", own.join(", "))),
        }
        for (track, sid, f) in inputs {
            let session_title = self
                .store
                .summary(sid)
                .map_or_else(|_| f.header.title.clone(), |s| s.title);
            text.push_str(&input_summary(track, &session_title, f, &merged.leftovers));
            text.push('\n');
        }
        let notice = stamp.message(NOTICE_TAG, text.trim_end().to_owned());
        let through = (notice_at(&notice), notice_id(&notice));
        out.push(notice);
        out.push(Op::Plan(sessions::RecordOp {
            op_id: stamp.op_id(),
            at: stamp.at.clone(),
            payload: serde_json::to_value(&merged.plan)?,
        }));
        out.push(Op::Approve(ApproveOp {
            op_id: stamp.op_id(),
            at: stamp.at.clone(),
            plan_version: copied.plan_version + 1,
            removed_plugins: removed.clone(),
        }));
        stamp.close_copied_jobs(&copied, &mut out);
        stamp.cover_copied(id, (&through.0, &through.1), &mut out);
        let first = merged.plan.track(&m.id).and_then(|t| {
            t.section_ids
                .iter()
                .find(|s| !merged.completed.contains(s))
                .cloned()
        });
        out.push(stamp.phase(first));
        self.create_branch_session(&out, &removed)
    }

    /// `session_end` in a branched session (§4.26): create the merges that
    /// became ready and say what happens next, for the tool result. A
    /// failure is logged, never the turn's. The UI hears about the changed
    /// sessions once this turn is over, when this session's `session_end`
    /// result is in the log and it lists as ended.
    pub(crate) fn end_branch(self: &Arc<Self>, session_id: &str, state: &SessionState) -> String {
        let (Some(branch), Some(plan)) = (&state.branch, &state.plan) else {
            return String::new();
        };
        let origin = if branch.kind == "origin" || branch.origin.is_empty() {
            session_id.to_owned()
        } else {
            branch.origin.clone()
        };
        let mut changed: Vec<String> = plan
            .tracks
            .iter()
            .map(|t| track_session(&origin, plan, &t.id))
            .filter(|id| self.store.exists(id))
            .collect();
        match self.create_ready_merges(&origin, plan, Some(session_id)) {
            Ok(created) => changed.extend(created.into_iter().map(|s| s.id)),
            Err(e) => tracing::warn!("merge sessions of {origin}: {e}"),
        }
        let me = Arc::clone(self);
        let sid = session_id.to_owned();
        self.spawn_tracked(
            session_id,
            Box::pin(async move {
                let guard = me.turn_lock(&sid);
                let _turn = guard.lock().await;
                for id in changed {
                    me.announce(&id);
                }
            }),
        );
        let Some(feeds) = plan.feeds(&branch.track) else {
            return String::new();
        };
        let merge = merge_id(&origin, &feeds.id);
        if self.store.exists(&merge) {
            return format!(
                " This branch was the last one feeding \"{}\": its merge session is ready on Home and starts with a short revision.",
                feeds.title
            );
        }
        let waiting: Vec<String> = feeds
            .after
            .iter()
            .filter(|t| **t != branch.track)
            .filter(|t| {
                let sid = track_session(&origin, plan, t);
                !self.branch_facts(&sid).is_some_and(|f| f.state.ended)
            })
            .map(|t| track_title(plan, t))
            .collect();
        format!(
            " The merge \"{}\" starts in its own session once these branches end too: {}.",
            feeds.title,
            waiting.join(", ")
        )
    }

    /// Tells the UI a session's summary (with its lineage) changed.
    pub(crate) fn announce(&self, id: &str) {
        let Ok(summary) = self.store.summary(id) else {
            return;
        };
        let mut list = vec![summary];
        self.apply_lineage(&mut list);
        if let Some(session) = list.into_iter().find(|s| s.id == id) {
            self.emit(
                id,
                AgentEvent::Session {
                    session: Box::new(session),
                },
            );
        }
    }

    /// Completes the lineage of the listed sessions of branched plans and
    /// creates merges that became ready elsewhere (an input ended on another
    /// device and synced in). Only sessions of branched plans are read in
    /// full, through a cache keyed by file size and mtime.
    pub(crate) fn apply_lineage(&self, list: &mut Vec<SessionSummary>) {
        let mut origins: Vec<String> = Vec::new();
        for s in list.iter() {
            let origin = match &s.lineage {
                Some(l) => l.origin.clone(),
                // A session listed alone (an announcement) may be an origin.
                None if list.len() == 1 => match self.branch_facts(&s.id) {
                    Some(f) if f.state.branch.as_ref().is_some_and(|b| b.kind == "origin") => {
                        s.id.clone()
                    }
                    _ => continue,
                },
                None => continue,
            };
            if !origins.contains(&origin) {
                origins.push(origin);
            }
        }
        for origin in origins {
            let Some(plan) = self.family_plan(&origin, &list[..]) else {
                continue;
            };
            if list.len() > 1 {
                match self.create_ready_merges(&origin, &plan, None) {
                    Ok(created) => {
                        for s in created {
                            if !list.iter().any(|x| x.id == s.id) {
                                list.insert(0, s);
                            }
                        }
                    }
                    Err(e) => tracing::warn!("merge sessions of {origin}: {e}"),
                }
            }
            for s in list.iter_mut() {
                let member =
                    s.id == origin || s.lineage.as_ref().is_some_and(|l| l.origin == origin);
                if member {
                    if let Some(l) = self.lineage_of(&origin, &plan, &s.id) {
                        s.lineage = Some(l);
                    }
                }
            }
        }
    }

    /// The branched plan of a family: the origin's, else a listed member's
    /// (every member has the same tracks; only sections change per branch).
    fn family_plan(&self, origin: &str, list: &[SessionSummary]) -> Option<Plan> {
        let from = |id: &str| {
            self.branch_facts(id)
                .and_then(|f| f.state.plan.clone())
                .filter(|p| !p.tracks.is_empty())
        };
        from(origin).or_else(|| {
            list.iter()
                .filter(|s| s.lineage.as_ref().is_some_and(|l| l.origin == origin))
                .find_map(|s| from(&s.id))
        })
    }

    fn lineage_of(&self, origin: &str, plan: &Plan, id: &str) -> Option<Lineage> {
        let facts = self.branch_facts(id)?;
        let branch = facts.state.branch.as_ref()?;
        let track = plan.track(&branch.track)?;
        let ended = |sid: &str| self.branch_facts(sid).is_some_and(|f| f.state.ended);
        let present = |sid: &str| self.store.exists(sid);
        let inputs = track
            .after
            .iter()
            .map(|a| {
                let sid = track_session(origin, plan, a);
                let here = present(&sid);
                LineageInput {
                    ended: here && ended(&sid),
                    session_id: here.then_some(sid),
                    track: a.clone(),
                    track_title: track_title(plan, a),
                }
            })
            .collect();
        let feeds = plan.feeds(&track.id).map(|f| {
            let sid = merge_id(origin, &f.id);
            LineageFeed {
                track: f.id.clone(),
                track_title: f.title.clone(),
                session_id: present(&sid).then_some(sid),
            }
        });
        let waiting_on = match (&feeds, plan.feeds(&track.id)) {
            (Some(feed), Some(f)) if feed.session_id.is_none() => f
                .after
                .iter()
                .filter(|a| **a != track.id)
                .filter(|a| {
                    let sid = track_session(origin, plan, a);
                    !(present(&sid) && ended(&sid))
                })
                .map(|a| track_title(plan, a))
                .collect(),
            _ => Vec::new(),
        };
        let origin_title = self
            .store
            .summary(origin)
            .map(|s| s.title)
            .ok()
            .or_else(|| facts.state.title.clone())
            .unwrap_or_default();
        Some(Lineage {
            kind: branch.kind.clone(),
            origin: origin.to_owned(),
            origin_title,
            track: track.id.clone(),
            track_title: track.title.clone(),
            index: plan.tracks.iter().position(|t| t.id == track.id)? + 1,
            count: plan.tracks.len(),
            inputs,
            feeds,
            ended: facts.state.ended,
            started: facts.started,
            waiting_on,
        })
    }
}

fn notice_at(op: &Op) -> String {
    op.at().to_owned()
}

fn notice_id(op: &Op) -> String {
    op.op_id().to_owned()
}

/// ` When this track is done, …` for a track that feeds a merge.
fn feeds_line(plan: &Plan, track: &str) -> String {
    match plan.feeds(track) {
        Some(f) => {
            let others: Vec<String> = f
                .after
                .iter()
                .filter(|a| a.as_str() != track)
                .map(|a| track_title(plan, a))
                .collect();
            format!(
                " When this track is done the session ends with a hand-off, and **{}** continues in a new session once {} {} done too.",
                f.title,
                others.join(" and "),
                if others.len() == 1 { "is" } else { "are" },
            )
        }
        None => String::new(),
    }
}

fn fork_notice(plan: &Plan, track: &Track, family: &str) -> String {
    let index = plan
        .tracks
        .iter()
        .position(|t| t.id == track.id)
        .map_or(0, |i| i + 1);
    format!(
        "{NOTICE_PREFIX} This session is branch {index} of {} of \"{family}\": it teaches **{}** ({}). The conversation above is copied from the session the plan was made in; the plan's other tracks run in their own sessions.{}",
        plan.tracks.len(),
        track.title,
        track
            .section_ids
            .iter()
            .map(|s| section_title(plan, s))
            .collect::<Vec<_>>()
            .join(", "),
        feeds_line(plan, &track.id),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{CheckVerdict, PlanNode};

    fn section(id: &str, nodes: &[&str]) -> Section {
        serde_json::from_value(json!({
            "id": id, "title": format!("Section {id}"), "nodeIds": nodes, "mode": "mechanism",
            "style": "mixed", "notes": "", "exit": "e"
        }))
        .unwrap()
    }

    fn track(id: &str, sections: &[&str], after: &[&str]) -> Track {
        Track {
            id: id.into(),
            title: format!("Track {id}"),
            section_ids: sections.iter().map(|s| (*s).to_owned()).collect(),
            after: after.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn plan() -> Plan {
        let node = |id: &str| PlanNode {
            id: id.into(),
            title: format!("Node {id}"),
            kind: Kind::Mechanism,
            requires: vec![],
            graph_node: None,
            weight: None,
            practice_only: false,
        };
        Plan {
            version: 1,
            goal: "g".into(),
            approach: "a".into(),
            depth: String::new(),
            explore: String::new(),
            minutes_per_sitting: None,
            style: String::new(),
            nodes: ["n1", "n2", "n3", "n4", "n5"]
                .iter()
                .map(|n| node(n))
                .collect(),
            sections: vec![
                section("a1", &["n1"]),
                section("a2", &["n2"]),
                section("b1", &["n3", "n4"]),
                section("m1", &["n5"]),
            ],
            tracks: vec![
                track("a", &["a1", "a2"], &[]),
                track("b", &["b1"], &[]),
                track("m", &["m1"], &["a", "b"]),
            ],
        }
    }

    fn input(plan: Plan, completed: &[&str], missed: &[&str]) -> SessionState {
        let mut st = SessionState {
            plan: Some(plan),
            completed_sections: completed.iter().map(|s| (*s).to_owned()).collect(),
            ..SessionState::default()
        };
        for n in missed {
            st.checks
                .entry((*n).to_owned())
                .or_default()
                .push(CheckVerdict {
                    node_id: (*n).to_owned(),
                    question_id: "q".into(),
                    verdict: "wrong".into(),
                    level: None,
                });
        }
        st
    }

    #[test]
    fn ids_are_derived_alike_on_every_device_and_differ_per_track() {
        let origin = Ulid::new().to_string();
        assert_eq!(merge_id(&origin, "m"), merge_id(&origin, "m"));
        assert_ne!(merge_id(&origin, "m"), merge_id(&origin, "n"));
        assert_ne!(merge_id(&origin, "a"), fork_id(&origin, "a"));
        // Same file year as the origin: the id carries the origin's time.
        let parsed = Ulid::from_string(&fork_id(&origin, "b")).unwrap();
        assert_eq!(parsed.timestamp_ms(), origin_ms(&origin));
        let p = plan();
        assert_eq!(track_session(&origin, &p, "a"), origin);
        assert_eq!(track_session(&origin, &p, "b"), fork_id(&origin, "b"));
        assert_eq!(track_session(&origin, &p, "m"), merge_id(&origin, "m"));
        // Ops built in one millisecond keep their build order.
        let mut s = Stamp::new(1_000, "x");
        let (first, second) = (s.op_id(), s.op_id());
        assert!(first < second);
    }

    #[test]
    fn a_merge_takes_each_branchs_track_and_revises_before_continuing() {
        // Branch a revised its own track (a2 got a new exit) and finished;
        // branch b finished b1 with a missed check on n4.
        let mut a_plan = plan();
        a_plan.sections[1].exit = "revised in branch a".into();
        let a = input(a_plan, &["a1", "a2"], &[]);
        let b = input(plan(), &["b1"], &["n4"]);
        let m = plan().track("m").unwrap().clone();
        let merged = merge_plan(&m, &[("a", &a), ("b", &b)]).unwrap();

        assert_eq!(merged.completed, ["a1", "a2", "b1"]);
        let a2 = merged.plan.sections.iter().find(|s| s.id == "a2").unwrap();
        assert_eq!(
            a2.exit, "revised in branch a",
            "a branch's own revision survives the merge"
        );
        let revision = merged.revision.as_ref().unwrap();
        assert!(revision.revision);
        // The missed node first, then the latest section's; two per input.
        assert_eq!(revision.node_ids, ["n2", "n1", "n4", "n3"]);
        let track = merged.plan.track("m").unwrap();
        assert_eq!(track.section_ids, [revision.id.as_str(), "m1"]);
        assert!(merged.leftovers.is_empty());
    }

    #[test]
    fn unfinished_branch_sections_move_to_the_merge_after_the_revision() {
        let a = input(plan(), &["a1"], &[]);
        let b = input(plan(), &[], &[]);
        let m = plan().track("m").unwrap().clone();
        let merged = merge_plan(&m, &[("a", &a), ("b", &b)]).unwrap();
        assert_eq!(merged.leftovers, ["a2", "b1"]);
        let track = merged.plan.track("m").unwrap();
        assert_eq!(track.section_ids, ["revise-m", "a2", "b1", "m1"]);
        assert_eq!(merged.plan.track("a").unwrap().section_ids, ["a1"]);
        assert!(merged.plan.track("b").unwrap().section_ids.is_empty());
        // Plan order follows the tracks.
        let order: Vec<&str> = merged.plan.sections.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(order, ["a1", "revise-m", "a2", "b1", "m1"]);
    }
}
