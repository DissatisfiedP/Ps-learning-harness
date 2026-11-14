//! The learner model (architecture §4.8, §4.13): what the learner holds, per
//! graph node, derived from the evidence log and never stored.
//!
//! # The log
//!
//! `<repo>/practice/outcomes.jsonl` holds one [`Outcome`] per line, exactly the
//! architecture §5 `Outcome` shape (camelCase). It is append-only. Sync merges
//! it like every other op log (`*.jsonl`): the union of both devices' lines,
//! deduplicated by `opId` and ordered by `(at, opId)` (`sync::union_jsonl`). So
//! every line carries a unique `opId` and an ISO `at`. Reading skips (and
//! counts) a line that doesn't parse, drops a repeated `opId`, and sorts by
//! `(at, opId)`, so the order the lines arrived in never matters.
//!
//! # What is derived
//!
//! [`LearnerModel`] is rebuilt from the whole log on every read. Nothing derived
//! is written anywhere, so a change to the model or its parameters applies to
//! all past evidence (§4.8). Targets are folded through the graph's `sameAs`
//! aliases when the graph is at hand ([`load`]). Per node, a [`NodeState`] has:
//!
//! - **FSRS memory** ([`fsrs`], FSRS-6 with default parameters). Every target
//!   of an outcome is stepped by the outcome's [`rating`]; a problem that
//!   exercises several targets updates each of them (§4.13).
//! - **Demonstrated layers** (recall, discrimination, composition). An outcome
//!   shows its layer when its verdict is `correct` and its rating is not
//!   Again: a success after the R3+ hint that named the idea shows nothing. A
//!   composition success also shows recall for every target, because those
//!   targets are its components (§4.13). A layer once shown stays shown;
//!   forgetting is FSRS's job, not the layers'.
//! - **Misconception notes**: the `misconception` text of an outcome, with its
//!   date and session. A note stays open until a later `correct` outcome on
//!   the node rated Good or Easy, which resolves every note before it.
//!
//! # Verdict → rating (§4.13)
//!
//! | Verdict | Rating |
//! |---|---|
//! | wrong, dontknow, dismissed | Again |
//! | correct or partial after hint rung R3+ | Again |
//! | correct after R1–R2 | Hard |
//! | partial without R3+ | Hard (retrieved, but not all of it) |
//! | correct, no hint, or only R0 (a redirect of attention) | Good |
//! | correct, no hint at all, confidence ≥ 90, fast | Easy |
//!
//! **Fast** means `0 < durationSec ≤` [`fast_limit`] for the format: 45 s for
//! a teaching `check` or a `predict`, 90 s for a practice problem (`stream`,
//! `set`, `exam`). `durationSec` runs from the moment the question is shown to
//! the moment the answer is sent, reading included. The limits are a
//! heuristic **[inference]**: FSRS's "Easy" was fitted on flashcards.
//!
//! # Readers
//!
//! - the `## Learner state` block and `graph_search` / `graph_read`
//!   (`graph::read`) tag each node with [`LearnerModel::note`]: held, due, or
//!   an open misconception; the probe skips held strands and the planner
//!   treats held nodes as held and re-teaches nodes with a misconception;
//! - [`LearnerModel::review_queue`] (Harness `review_queue`, Tauri
//!   `review_queue`);
//! - [`LearnerModel::views`] (Harness `learner_nodes`), shown on the Map's
//!   node card.
//!
//! # Writers
//!
//! Teaching sessions write outcomes today ([`question_outcome`]): a graded
//! `check` question on a plan node that maps to a graph node, and a graded
//! `probe` question tagged with a graph node, both as format `check`. The
//! practice coach (a later wave) writes `stream`, `set` and `exam` outcomes
//! through [`append_outcome`].
//!
//! # Stable API (other slices build on it)
//!
//! - [`append_outcome`]`(repo, &Outcome)`, [`load`]`(repo)`, [`load_with`]`(repo, &map)`;
//! - [`LearnerModel::node`] → [`NodeState`] `{ fsrs: Fsrs { difficulty,
//!   stability, last_review }, layers: Layers { recall, discrimination,
//!   composition }, misconceptions: Vec<Misconception { text, at, session_id,
//!   resolved_at }>, reviews, lapses }` with `retrievability(now)` and
//!   `held(now)`;
//! - [`LearnerModel::review_queue`]`(map, now, desired_retention)`: node ids;
//! - [`rating`]`(&Outcome)` for the table above.

pub mod fsrs;

use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::knowledge::KnowledgeMap;
use crate::sessions::Op;
use crate::state::{Level, Plan};
use crate::Error;

pub use fsrs::Rating;

/// The log, relative to the data repo.
pub const OUTCOMES: &str = "practice/outcomes.jsonl";

/// The retention the scheduler aims at (§4.13): below it a node is due.
pub const DESIRED_RETENTION: f64 = 0.9;

/// Longest misconception note kept on one outcome.
pub const MAX_MISCONCEPTION_CHARS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layer {
    Recall,
    Discrimination,
    Composition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Stream,
    Set,
    Exam,
    Check,
    Predict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Correct,
    Partial,
    Wrong,
    Dontknow,
    Dismissed,
}

impl Verdict {
    pub fn parse(s: &str) -> Option<Self> {
        serde_json::from_value(Value::String(s.to_owned())).ok()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MissClass {
    Cue,
    Discrimination,
    Execution,
    Careless,
    Structural,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Grader {
    Environment,
    Labels,
    Committed,
    Rubric,
    Free,
}

/// One graded answer: a line of `practice/outcomes.jsonl` (architecture §5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub op_id: String,
    pub at: String,
    pub session_id: String,
    pub problem_id: String,
    /// Graph node ids the problem exercises.
    pub targets: Vec<String>,
    /// Graph node ids whose world the problem is set in.
    #[serde(default)]
    pub hosts: Vec<String>,
    pub layer: Layer,
    pub format: Format,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cue_distance: Option<u8>,
    pub verdict: Verdict,
    /// 0–100; absent for `dontknow`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<u8>,
    /// Highest hint rung used (0–5); `null` when no hint was given.
    #[serde(default)]
    pub hint_rung: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub miss_class: Option<MissClass>,
    /// Short note when the miss is structural.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub misconception: Option<String>,
    pub grader: Grader,
    pub duration_sec: u32,
    /// The level the question demanded (`explain` / `use` / `analyze` /
    /// `design`); absent on answers recorded before levels existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<Level>,
}

impl Outcome {
    /// Why this outcome can't go into the log, or `None`.
    fn problem(&self) -> Option<String> {
        if self.op_id.trim().is_empty() || self.session_id.trim().is_empty() {
            return Some("`opId` and `sessionId` are required".into());
        }
        if parse_at(&self.at).is_none() {
            return Some(format!("`at` must be an ISO 8601 time, not `{}`", self.at));
        }
        if self.targets.is_empty() || self.targets.iter().any(|t| t.trim().is_empty()) {
            return Some("an outcome needs at least one target node".into());
        }
        if self.cue_distance.is_some_and(|c| c > 3) {
            return Some("`cueDistance` is 0–3".into());
        }
        if self.hint_rung.is_some_and(|r| r > 5) {
            return Some("`hintRung` is 0–5".into());
        }
        match (self.verdict, self.confidence) {
            (Verdict::Dontknow, Some(_)) => {
                return Some("a `dontknow` outcome carries no confidence".into())
            }
            (_, Some(c)) if c > 100 => return Some("`confidence` is 0–100".into()),
            _ => {}
        }
        if let Some(m) = &self.misconception {
            if m.trim().is_empty() || m.chars().count() > MAX_MISCONCEPTION_CHARS {
                return Some(format!(
                    "`misconception` is a short note of 1–{MAX_MISCONCEPTION_CHARS} characters"
                ));
            }
            if self.miss_class != Some(MissClass::Structural) {
                return Some("`misconception` goes with `missClass: structural`".into());
            }
            if self.verdict == Verdict::Correct {
                return Some("a correct answer carries no misconception".into());
            }
        }
        None
    }
}

/// Where the log lives.
pub fn path(repo: &Path) -> PathBuf {
    repo.join(OUTCOMES)
}

/// Append one outcome (the file and `practice/` are created on first use).
/// Refuses an outcome that breaks the §5 shape; never rewrites a line.
pub fn append_outcome(repo: &Path, outcome: &Outcome) -> Result<(), Error> {
    if let Some(problem) = outcome.problem() {
        return Err(Error::Invalid(format!("outcome: {problem}")));
    }
    let mut line = serde_json::to_string(outcome)?;
    line.push('\n');
    let path = path(repo);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    file.write_all(line.as_bytes())?;
    file.sync_data()?;
    Ok(())
}

/// Every outcome in the log, deduplicated by `opId` and ordered by
/// `(at, opId)`, plus how many lines were skipped (unparseable, or breaking
/// the §5 shape). A missing file is an empty log.
pub fn read_outcomes(repo: &Path) -> Result<(Vec<Outcome>, usize), Error> {
    let path = path(repo);
    if !path.is_file() {
        return Ok((Vec::new(), 0));
    }
    let text = std::fs::read_to_string(&path)?;
    let mut skipped = 0;
    let mut seen = HashSet::new();
    let mut out: Vec<(DateTime<Utc>, Outcome)> = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let parsed = serde_json::from_str::<Outcome>(line)
            .ok()
            .filter(|o| o.problem().is_none());
        let Some(o) = parsed else {
            skipped += 1;
            continue;
        };
        if !seen.insert(o.op_id.clone()) {
            continue;
        }
        let at = parse_at(&o.at).unwrap_or_default();
        out.push((at, o));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.op_id.cmp(&b.1.op_id)));
    Ok((out.into_iter().map(|(_, o)| o).collect(), skipped))
}

fn parse_at(at: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(at)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// The longest answer time that still counts as fast (Easy), per format.
pub fn fast_limit(format: Format) -> u32 {
    match format {
        Format::Check | Format::Predict => 45,
        Format::Stream | Format::Set | Format::Exam => 90,
    }
}

/// The FSRS rating an outcome gives each of its targets (§4.13; table in the
/// module docs).
pub fn rating(o: &Outcome) -> Rating {
    let rung = o.hint_rung;
    let surrendered = rung.is_some_and(|r| r >= 3);
    match o.verdict {
        Verdict::Wrong | Verdict::Dontknow | Verdict::Dismissed => Rating::Again,
        _ if surrendered => Rating::Again,
        Verdict::Partial => Rating::Hard,
        Verdict::Correct => match rung {
            Some(1 | 2) => Rating::Hard,
            Some(_) => Rating::Good,
            None => {
                let fast = o.duration_sec > 0 && o.duration_sec <= fast_limit(o.format);
                if fast && o.confidence.is_some_and(|c| c >= 90) {
                    Rating::Easy
                } else {
                    Rating::Good
                }
            }
        },
    }
}

/// FSRS memory state of one node.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Fsrs {
    pub difficulty: f64,
    /// Days until R falls to 0.9.
    pub stability: f64,
    #[serde(serialize_with = "iso")]
    pub last_review: DateTime<Utc>,
}

fn iso<S: serde::Serializer>(t: &DateTime<Utc>, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// Which practice layers the learner has shown on a node.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Layers {
    pub recall: bool,
    pub discrimination: bool,
    pub composition: bool,
}

impl Layers {
    fn shown(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.recall {
            out.push("recall");
        }
        if self.discrimination {
            out.push("discrimination");
        }
        if self.composition {
            out.push("composition");
        }
        out
    }
}

/// A misconception note with its provenance.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Misconception {
    pub text: String,
    pub at: String,
    pub session_id: String,
    /// When a later clean success resolved it; `None` while open.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<String>,
}

/// What the log says about one node.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeState {
    pub fsrs: Fsrs,
    pub layers: Layers,
    pub misconceptions: Vec<Misconception>,
    /// Outcomes that stepped this node.
    pub reviews: u32,
    /// Of those, the ones rated Again.
    pub lapses: u32,
    /// The highest level of any answer that showed the node (a success the
    /// learner didn't surrender with an R3+ hint); `None` when no answer
    /// carried a level.
    pub level_reached: Option<Level>,
}

impl NodeState {
    /// Probability of recall at `now`.
    pub fn retrievability(&self, now: DateTime<Utc>) -> f64 {
        let days = (now - self.fsrs.last_review).num_seconds().max(0) as f64 / 86_400.0;
        fsrs::retrievability(
            fsrs::Memory {
                stability: self.fsrs.stability,
                difficulty: self.fsrs.difficulty,
            },
            days,
        )
    }

    /// When R falls to [`DESIRED_RETENTION`].
    pub fn due(&self) -> DateTime<Utc> {
        let days = fsrs::next_interval(self.fsrs.stability, DESIRED_RETENTION);
        self.fsrs.last_review + Duration::seconds((days * 86_400.0).round() as i64)
    }

    pub fn open_misconceptions(&self) -> impl Iterator<Item = &Misconception> {
        self.misconceptions
            .iter()
            .filter(|m| m.resolved_at.is_none())
    }

    /// Held: recall shown, R still at or above the desired retention, and no
    /// open misconception. The probe skips held nodes and the planner treats
    /// them as held (§4.8).
    pub fn held(&self, now: DateTime<Utc>) -> bool {
        self.layers.recall
            && self.open_misconceptions().next().is_none()
            && self.retrievability(now) >= DESIRED_RETENTION
    }
}

/// Per-node learner state, derived from the log.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LearnerModel {
    nodes: BTreeMap<String, NodeState>,
}

/// One node as the UI shows it (the Map's node card).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeView {
    pub retrievability: f64,
    pub stability: f64,
    pub difficulty: f64,
    pub last_review: String,
    pub due: String,
    pub held: bool,
    pub layers: Layers,
    /// Open notes only, newest first.
    pub misconceptions: Vec<Misconception>,
    pub reviews: u32,
    pub lapses: u32,
}

/// One entry of the review queue.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewItem {
    pub id: String,
    pub title: String,
    pub home: String,
    pub retrievability: f64,
    /// Nodes that need this one through a load-bearing `requires` edge.
    pub dependents: usize,
}

impl LearnerModel {
    /// Fold outcomes (already in `(at, opId)` order) into per-node state.
    /// `canonical` maps a target id to the node it folds into.
    pub fn from_outcomes(outcomes: &[Outcome], canonical: impl Fn(&str) -> String) -> Self {
        let mut nodes: BTreeMap<String, NodeState> = BTreeMap::new();
        for o in outcomes {
            let Some(at) = parse_at(&o.at) else { continue };
            let r = rating(o);
            let shows = o.verdict == Verdict::Correct && r != Rating::Again;
            let clean = o.verdict == Verdict::Correct && r >= Rating::Good;
            let mut targets: Vec<String> = o.targets.iter().map(|t| canonical(t)).collect();
            targets.sort();
            targets.dedup();
            for id in targets {
                let state = match nodes.get_mut(&id) {
                    Some(s) => {
                        let days = (at - s.fsrs.last_review).num_seconds().max(0) as f64 / 86_400.0;
                        let m = fsrs::step(
                            fsrs::Memory {
                                stability: s.fsrs.stability,
                                difficulty: s.fsrs.difficulty,
                            },
                            days,
                            r,
                        );
                        s.fsrs = Fsrs {
                            difficulty: m.difficulty,
                            stability: m.stability,
                            last_review: at.max(s.fsrs.last_review),
                        };
                        s
                    }
                    None => {
                        let m = fsrs::init(r);
                        nodes.entry(id).or_insert(NodeState {
                            fsrs: Fsrs {
                                difficulty: m.difficulty,
                                stability: m.stability,
                                last_review: at,
                            },
                            layers: Layers::default(),
                            misconceptions: Vec::new(),
                            reviews: 0,
                            lapses: 0,
                            level_reached: None,
                        })
                    }
                };
                state.reviews += 1;
                if r == Rating::Again {
                    state.lapses += 1;
                }
                if shows {
                    match o.layer {
                        Layer::Recall => state.layers.recall = true,
                        Layer::Discrimination => state.layers.discrimination = true,
                        Layer::Composition => {
                            state.layers.composition = true;
                            state.layers.recall = true;
                        }
                    }
                    if let Some(level) = o.level {
                        state.level_reached = state.level_reached.max(Some(level));
                    }
                }
                if clean {
                    for m in &mut state.misconceptions {
                        m.resolved_at.get_or_insert_with(|| o.at.clone());
                    }
                }
                if let Some(text) = &o.misconception {
                    state.misconceptions.push(Misconception {
                        text: text.clone(),
                        at: o.at.clone(),
                        session_id: o.session_id.clone(),
                        resolved_at: None,
                    });
                }
            }
        }
        LearnerModel { nodes }
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// A model of hand-built node states, for tests of what reads one.
    #[cfg(test)]
    pub(crate) fn for_tests(nodes: BTreeMap<String, NodeState>) -> Self {
        Self { nodes }
    }

    pub fn node(&self, id: &str) -> Option<&NodeState> {
        self.nodes.get(id)
    }

    pub fn nodes(&self) -> impl Iterator<Item = (&String, &NodeState)> {
        self.nodes.iter()
    }

    /// Nodes with R below `desired_retention` that the graph still holds,
    /// most depended-on first (load-bearing `requires` out-degree), then
    /// lowest R, then id (§4.13).
    pub fn review_queue(
        &self,
        map: &KnowledgeMap,
        now: DateTime<Utc>,
        desired_retention: f64,
    ) -> Vec<String> {
        self.review_items(map, now, desired_retention)
            .into_iter()
            .map(|i| i.id)
            .collect()
    }

    /// [`Self::review_queue`] with what the UI shows for each entry.
    pub fn review_items(
        &self,
        map: &KnowledgeMap,
        now: DateTime<Utc>,
        desired_retention: f64,
    ) -> Vec<ReviewItem> {
        let mut items: Vec<ReviewItem> = self
            .nodes
            .iter()
            .filter(|(id, _)| !map.is_tombstoned(id))
            .filter_map(|(id, s)| {
                let node = map.node(id)?;
                let r = s.retrievability(now);
                (r < desired_retention).then(|| ReviewItem {
                    id: node.id.clone(),
                    title: node.title.clone(),
                    home: node.home.clone(),
                    retrievability: r,
                    dependents: load_bearing_dependents(map, &node.id),
                })
            })
            .collect();
        items.sort_by(|a, b| {
            b.dependents
                .cmp(&a.dependents)
                .then(a.retrievability.total_cmp(&b.retrievability))
                .then_with(|| a.id.cmp(&b.id))
        });
        items
    }

    /// Every node with evidence, as the UI shows it.
    pub fn views(&self, now: DateTime<Utc>) -> BTreeMap<String, NodeView> {
        self.nodes
            .iter()
            .map(|(id, s)| {
                let mut open: Vec<Misconception> = s.open_misconceptions().cloned().collect();
                open.reverse();
                let view = NodeView {
                    retrievability: s.retrievability(now),
                    stability: s.fsrs.stability,
                    difficulty: s.fsrs.difficulty,
                    last_review: s
                        .fsrs
                        .last_review
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    due: s.due().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    held: s.held(now),
                    layers: s.layers,
                    misconceptions: open,
                    reviews: s.reviews,
                    lapses: s.lapses,
                };
                (id.clone(), view)
            })
            .collect()
    }

    /// The node's record as a suffix for agent-facing node lines:
    /// ` · held (R 0.97; shown recall)`, ` · due for review (R 0.62; shown
    /// recall)`, ` · not yet recalled (R 0.40)`, plus the newest open
    /// misconception. Empty without evidence.
    pub fn note(&self, id: &str, now: DateTime<Utc>) -> String {
        let Some(s) = self.nodes.get(id) else {
            return String::new();
        };
        let r = s.retrievability(now);
        let shown = s.layers.shown();
        let shown = if shown.is_empty() {
            String::new()
        } else {
            format!("; shown {}", shown.join(", "))
        };
        let mut out = if s.held(now) {
            format!(" · held (R {r:.2}{shown})")
        } else if !s.layers.recall {
            format!(" · not yet recalled (R {r:.2}{shown})")
        } else if r < DESIRED_RETENTION {
            format!(" · due for review (R {r:.2}{shown})")
        } else {
            format!(" · recalled (R {r:.2}{shown})")
        };
        if let Some(m) = s.open_misconceptions().last() {
            let text: String = m.text.chars().take(120).collect();
            out.push_str(&format!(
                " · misconception {}: {text}",
                m.at.get(..10).unwrap_or(&m.at)
            ));
        }
        out
    }
}

/// Requires edges out of `id` marked load-bearing: how many nodes need it.
fn load_bearing_dependents(map: &KnowledgeMap, id: &str) -> usize {
    map.prerequisites()
        .filter(|e| e.from == id && e.weight == "load-bearing")
        .count()
}

/// The model from the log, with targets folded through `map`'s aliases.
/// Skipped lines are logged, never fatal.
pub fn load_with(repo: &Path, map: &KnowledgeMap) -> Result<LearnerModel, Error> {
    let (outcomes, skipped) = read_outcomes(repo)?;
    if skipped > 0 {
        tracing::warn!("{OUTCOMES}: {skipped} malformed line(s) skipped");
    }
    Ok(LearnerModel::from_outcomes(&outcomes, |id| {
        map.canonical(id)
    }))
}

/// [`load_with`] after reading the graph.
pub fn load(repo: &Path) -> Result<LearnerModel, Error> {
    let map = crate::knowledge::load(repo)?;
    load_with(repo, &map)
}

// ---------------------------------------------------------------------------
// Teaching-session evidence
// ---------------------------------------------------------------------------

/// The graph node a teaching question measures, or `None` when the graph has
/// no node for it yet (a node first taught in this session reaches the graph
/// only when the builder runs; its later checks count from then on).
///
/// - `check` (`nodeId` is a plan node): the plan node's `graphNode`, else a
///   graph node with the plan node's id (the planner reuses graph ids), else
///   the one graph node whose title matches the plan node's, ignoring case.
/// - `probe`: its `nodeId` (a graph node id), else the one graph node whose
///   title matches the strand's name.
fn question_target(args: &Value, plan: Option<&Plan>, map: &KnowledgeMap) -> Option<String> {
    let live = |id: &str| {
        map.node(id)
            .filter(|n| !map.is_tombstoned(&n.id))
            .map(|n| n.id.clone())
    };
    let by_title = |title: &str| {
        let title = title.trim().to_lowercase();
        let mut hits = map
            .nodes
            .iter()
            .filter(|n| !map.is_tombstoned(&n.id) && n.title.trim().to_lowercase() == title);
        match (hits.next(), hits.next()) {
            (Some(n), None) if !title.is_empty() => Some(n.id.clone()),
            _ => None,
        }
    };
    let node_id = args.get("nodeId").and_then(Value::as_str);
    match args.get("role").and_then(Value::as_str)? {
        "check" => {
            let id = node_id?;
            let plan_node = plan.and_then(|p| p.nodes.iter().find(|n| n.id == id));
            plan_node
                .and_then(|n| n.graph_node.as_deref())
                .and_then(live)
                .or_else(|| live(id))
                .or_else(|| plan_node.and_then(|n| by_title(&n.title)))
        }
        "probe" => match node_id {
            Some(id) => live(id),
            None => by_title(args.get("strand").and_then(Value::as_str)?),
        },
        _ => None,
    }
}

/// The outcome a graded teaching question gives, or `None` when it measures
/// no graph node (a `predict`, or a node the graph doesn't hold yet).
///
/// `question_id` is the `question_ask` call; its call and result ops give the
/// question, the time shown, the time answered and the confidence.
/// Multiple-choice questions train `discrimination` (§4.12); free recall
/// trains `recall`. A misconception note makes the miss `structural`.
pub(crate) fn question_outcome(
    session_id: &str,
    ops: &[Op],
    question_id: &str,
    verdict: Verdict,
    misconception: Option<&str>,
    map: &KnowledgeMap,
) -> Option<Outcome> {
    let mut asked: Option<(&Value, &str)> = None;
    let mut answered: Option<(&str, &str)> = None;
    for op in ops {
        if let Op::Tool(t) = op {
            if t.call_id != question_id {
                continue;
            }
            match &t.result {
                None if t.name == "question_ask" => asked = Some((&t.args, &t.at)),
                Some(r) if !r.is_error => answered = Some((&r.content, &t.at)),
                _ => {}
            }
        }
    }
    let (args, asked_at) = asked?;
    let (content, answered_at) = answered?;
    let plan = crate::replay::replay_state(ops).plan;
    let target = question_target(args, plan.as_ref(), map)?;
    let result: Value = serde_json::from_str(content).unwrap_or(Value::Null);
    let confidence = (verdict != Verdict::Dontknow)
        .then(|| result.get("confidence").and_then(Value::as_u64))
        .flatten()
        .map(|c| c.min(100) as u8);
    let duration = match (parse_at(asked_at), parse_at(answered_at)) {
        (Some(a), Some(b)) => (b - a).num_seconds().clamp(0, i64::from(u32::MAX)) as u32,
        _ => 0,
    };
    let layer = if args.get("format").and_then(Value::as_str) == Some("choice") {
        Layer::Discrimination
    } else {
        Layer::Recall
    };
    let misconception = misconception
        .map(str::trim)
        .filter(|m| !m.is_empty() && verdict != Verdict::Correct)
        .map(|m| m.chars().take(MAX_MISCONCEPTION_CHARS).collect::<String>());
    Some(Outcome {
        op_id: crate::sessions::op_id(),
        at: crate::now_iso(),
        session_id: session_id.to_owned(),
        problem_id: question_id.to_owned(),
        targets: vec![target],
        hosts: Vec::new(),
        layer,
        format: Format::Check,
        cue_distance: None,
        verdict,
        confidence,
        hint_rung: None,
        miss_class: misconception.as_ref().map(|_| MissClass::Structural),
        misconception,
        grader: Grader::Committed,
        duration_sec: duration,
        level: args
            .get("level")
            .and_then(Value::as_str)
            .and_then(Level::parse),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{fold, GraphOp};
    use crate::state::Kind;

    fn at(day: u32) -> String {
        format!("2026-11-{day:02}T10:00:00Z")
    }

    fn now(day: u32) -> DateTime<Utc> {
        parse_at(&at(day)).unwrap()
    }

    fn outcome(id: &str, day: u32, targets: &[&str], verdict: Verdict) -> Outcome {
        Outcome {
            op_id: id.into(),
            at: at(day),
            session_id: "s1".into(),
            problem_id: format!("p-{id}"),
            targets: targets.iter().map(|t| (*t).to_owned()).collect(),
            hosts: Vec::new(),
            layer: Layer::Recall,
            format: Format::Check,
            cue_distance: None,
            verdict,
            confidence: Some(70),
            hint_rung: None,
            miss_class: None,
            misconception: None,
            grader: Grader::Committed,
            duration_sec: 120,
            level: None,
        }
    }

    fn model(outcomes: &[Outcome]) -> LearnerModel {
        LearnerModel::from_outcomes(outcomes, str::to_owned)
    }

    fn temp_repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lh-learner-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn verdicts_map_to_ratings_per_the_scheduling_table() {
        let mut o = outcome("a", 1, &["n"], Verdict::Correct);
        let rate = |o: &Outcome| rating(o);
        for v in [Verdict::Wrong, Verdict::Dontknow, Verdict::Dismissed] {
            o.verdict = v;
            assert_eq!(rate(&o), Rating::Again, "{v:?}");
        }
        o.verdict = Verdict::Correct;
        for (rung, want) in [
            (Some(5), Rating::Again),
            (Some(3), Rating::Again),
            (Some(2), Rating::Hard),
            (Some(1), Rating::Hard),
            (Some(0), Rating::Good),
            (None, Rating::Good),
        ] {
            o.hint_rung = rung;
            assert_eq!(rate(&o), want, "rung {rung:?}");
        }
        o.verdict = Verdict::Partial;
        o.hint_rung = Some(3);
        assert_eq!(rate(&o), Rating::Again);
        o.hint_rung = None;
        assert_eq!(rate(&o), Rating::Hard);

        // Easy: no hint, confidence ≥ 90, fast (≤ 45 s for a check).
        o.verdict = Verdict::Correct;
        o.confidence = Some(90);
        o.duration_sec = 45;
        assert_eq!(rate(&o), Rating::Easy);
        o.duration_sec = 46;
        assert_eq!(rate(&o), Rating::Good, "slow");
        o.duration_sec = 0;
        assert_eq!(rate(&o), Rating::Good, "no timing is never fast");
        o.duration_sec = 30;
        o.confidence = Some(89);
        assert_eq!(rate(&o), Rating::Good, "unsure");
        o.confidence = Some(95);
        o.hint_rung = Some(0);
        assert_eq!(rate(&o), Rating::Good, "any hint rules out Easy");
        o.hint_rung = None;
        o.format = Format::Stream;
        o.duration_sec = 90;
        assert_eq!(rate(&o), Rating::Easy, "a practice problem gets 90 s");
    }

    #[test]
    fn replaying_outcomes_matches_the_fsrs_reference() {
        // Again, then Goods at 0, 1, 3, 8, 21 days: fsrs-rs's own vector.
        let mut log = vec![outcome("o0", 1, &["n"], Verdict::Wrong)];
        let mut day = 1;
        for (i, gap) in [0, 1, 3, 8, 21].iter().enumerate() {
            day += gap;
            let mut o = outcome(&format!("o{}", i + 1), 1, &["n"], Verdict::Correct);
            o.at = (now(1) + Duration::days(i64::from(day - 1)))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            log.push(o);
        }
        let m = model(&log);
        let n = m.node("n").unwrap();
        assert!((n.fsrs.stability - 53.62691).abs() < 1e-3, "{n:?}");
        assert!((n.fsrs.difficulty - 6.3574867).abs() < 1e-4, "{n:?}");
        assert_eq!((n.reviews, n.lapses), (6, 1));
        // R falls with time and is 0.9 one stability after the last review.
        let last = n.fsrs.last_review;
        assert!((n.retrievability(last) - 1.0).abs() < 1e-12);
        let at_s = last + Duration::seconds((n.fsrs.stability * 86_400.0) as i64);
        assert!((n.retrievability(at_s) - 0.9).abs() < 1e-4);
        assert!((n.due() - at_s).num_seconds().abs() <= 1);
    }

    #[test]
    fn every_target_of_a_problem_is_updated_and_aliases_fold() {
        let o = outcome("o1", 1, &["a", "b", "old-b"], Verdict::Correct);
        let m = LearnerModel::from_outcomes(&[o], |id| {
            if id == "old-b" {
                "b".into()
            } else {
                id.to_owned()
            }
        });
        assert_eq!(m.node("a").unwrap().reviews, 1);
        assert_eq!(
            m.node("b").unwrap().reviews,
            1,
            "an alias is one node, stepped once"
        );
        assert!(m.node("old-b").is_none());
    }

    #[test]
    fn layers_are_shown_only_by_unsurrendered_successes() {
        let mut wrong = outcome("o1", 1, &["n"], Verdict::Wrong);
        wrong.layer = Layer::Discrimination;
        let mut named = outcome("o2", 2, &["n"], Verdict::Correct);
        named.hint_rung = Some(3);
        let mut partial = outcome("o3", 3, &["n"], Verdict::Partial);
        partial.layer = Layer::Discrimination;
        let m = model(&[wrong, named, partial]);
        assert_eq!(m.node("n").unwrap().layers, Layers::default());

        let mut helped = outcome("o4", 4, &["n"], Verdict::Correct);
        helped.hint_rung = Some(2);
        helped.layer = Layer::Discrimination;
        let m = model(&[helped]);
        let l = m.node("n").unwrap().layers;
        assert!(l.discrimination && !l.recall && !l.composition);

        // A composition success counts as recall for each component, and a
        // later miss doesn't un-show a layer.
        let mut composed = outcome("o5", 5, &["a", "b"], Verdict::Correct);
        composed.layer = Layer::Composition;
        let m = model(&[composed, outcome("o6", 6, &["a"], Verdict::Wrong)]);
        for id in ["a", "b"] {
            let l = m.node(id).unwrap().layers;
            assert!(l.composition && l.recall && !l.discrimination, "{id}");
        }
    }

    #[test]
    fn misconceptions_keep_provenance_until_a_clean_success() {
        let mut miss = outcome("o1", 3, &["n"], Verdict::Wrong);
        miss.session_id = "s-miss".into();
        miss.miss_class = Some(MissClass::Structural);
        miss.misconception = Some("treats d/dx e^{2x} as e^{2x}".into());
        let mut helped = outcome("o2", 4, &["n"], Verdict::Correct);
        helped.hint_rung = Some(1);
        let m = model(&[miss.clone(), helped.clone()]);
        let n = m.node("n").unwrap();
        assert_eq!(
            n.misconceptions,
            [Misconception {
                text: "treats d/dx e^{2x} as e^{2x}".into(),
                at: at(3),
                session_id: "s-miss".into(),
                resolved_at: None,
            }],
            "a hinted success doesn't resolve it"
        );
        assert!(!n.held(now(4)), "an open misconception is never held");
        assert!(m
            .note("n", now(4))
            .contains("misconception 2026-11-03: treats"));

        let clean = outcome("o3", 5, &["n"], Verdict::Correct);
        let m = model(&[miss, helped, clean]);
        let n = m.node("n").unwrap();
        assert_eq!(
            n.misconceptions[0].resolved_at.as_deref(),
            Some(at(5).as_str())
        );
        assert!(n.held(now(5)));
        assert!(m.views(now(5))["n"].misconceptions.is_empty());
    }

    #[test]
    fn held_needs_recall_and_fresh_memory() {
        let m = model(&[outcome("o1", 1, &["n"], Verdict::Correct)]);
        let n = m.node("n").unwrap();
        assert!(n.held(now(1)));
        assert!(m
            .note("n", now(1))
            .starts_with(" · held (R 1.00; shown recall)"));
        // One Good review: S = w2 ≈ 2.3 days, so after a week R < 0.9.
        assert!(!n.held(now(8)));
        assert!(m.note("n", now(8)).starts_with(" · due for review (R 0."));
        let m = model(&[outcome("o1", 1, &["n"], Verdict::Wrong)]);
        assert!(m.note("n", now(1)).starts_with(" · not yet recalled"));
        assert_eq!(m.note("other", now(1)), "");
    }

    fn graph() -> KnowledgeMap {
        let node = |id: &str| GraphOp::node(id, Some(id), "s", Kind::Fact, "t", None);
        let mut ops = vec![GraphOp::topic("t", "T", None)];
        for id in ["hub", "mid", "leaf", "x", "y", "z", "gone"] {
            ops.push(node(id));
        }
        // hub feeds three nodes, mid two (one of them trivially), leaf none.
        for to in ["x", "y", "z"] {
            ops.push(GraphOp::edge("hub", to, "requires", None, None, None));
        }
        ops.push(GraphOp::edge("mid", "x", "requires", None, None, None));
        ops.push(GraphOp::edge(
            "mid",
            "y",
            "requires",
            Some("trivial"),
            None,
            None,
        ));
        ops.push(GraphOp::edge("mid", "z", "contrasts", None, None, None));
        ops.push(GraphOp::tombstone("gone", "test"));
        fold(&ops)
    }

    #[test]
    fn the_review_queue_puts_load_bearing_nodes_first_then_lowest_r() {
        let map = graph();
        // Good on day 1, except hub (Good on day 10: fresher than the rest);
        // x lapses on day 2, so its stability is lower than leaf's.
        let mut log: Vec<Outcome> = ["mid", "leaf", "x", "gone"]
            .iter()
            .map(|id| outcome(&format!("g-{id}"), 1, &[id], Verdict::Correct))
            .collect();
        log.push(outcome("a-x", 2, &["x"], Verdict::Wrong));
        log.push(outcome("g-hub", 10, &["hub"], Verdict::Correct));
        let m = model(&log);
        // Fresh memory is not due.
        assert_eq!(
            m.review_queue(&map, now(2), DESIRED_RETENTION),
            [] as [&str; 0]
        );

        let items = m.review_items(&map, now(20), DESIRED_RETENTION);
        let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        // Out-degree first (hub 3, mid 1: trivial and contrast edges don't
        // count), then lowest R (x before leaf, against id order); the
        // tombstoned node never.
        assert_eq!(ids, ["hub", "mid", "x", "leaf"], "{items:?}");
        assert_eq!(items[0].dependents, 3);
        assert_eq!(items[1].dependents, 1);
        assert!(
            items[0].retrievability > items[3].retrievability,
            "out-degree beats R"
        );
        assert!(items[2].retrievability < items[3].retrievability);
        assert_eq!(m.review_queue(&map, now(20), DESIRED_RETENTION), ids);
        // A lower desired retention lets more wait.
        assert!(m.review_queue(&map, now(20), 0.01).is_empty());
    }

    #[test]
    fn the_log_round_trips_skips_garbage_and_dedupes_by_op_id() {
        let repo = temp_repo("log");
        let first = outcome("b", 2, &["n"], Verdict::Correct);
        let mut dontknow = outcome("a", 3, &["n"], Verdict::Dontknow);
        dontknow.confidence = None;
        append_outcome(&repo, &first).unwrap();
        append_outcome(&repo, &dontknow).unwrap();
        let text = std::fs::read_to_string(path(&repo)).unwrap();
        let line: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(line["hintRung"], Value::Null, "hintRung is always written");
        assert_eq!(line["durationSec"], 120);
        assert!(line.get("missClass").is_none());

        // A torn line, a duplicate (sync union) and an out-of-order line.
        let mut early = outcome("c", 1, &["m"], Verdict::Wrong);
        early.confidence = Some(10);
        let extra = format!(
            "{{\"opId\":\"torn\n{}\n{}\n",
            serde_json::to_string(&first).unwrap(),
            serde_json::to_string(&early).unwrap()
        );
        std::fs::OpenOptions::new()
            .append(true)
            .open(path(&repo))
            .unwrap()
            .write_all(extra.as_bytes())
            .unwrap();
        let (log, skipped) = read_outcomes(&repo).unwrap();
        assert_eq!(skipped, 1);
        let ids: Vec<&str> = log.iter().map(|o| o.op_id.as_str()).collect();
        assert_eq!(ids, ["c", "b", "a"]);
        assert_eq!(
            load_with(&repo, &KnowledgeMap::default())
                .unwrap()
                .node("n")
                .unwrap()
                .reviews,
            2
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn append_refuses_outcomes_that_break_the_schema() {
        let repo = temp_repo("refuse");
        let ok = outcome("a", 1, &["n"], Verdict::Wrong);
        let cases: Vec<(Outcome, &str)> = vec![
            (
                Outcome {
                    targets: vec![],
                    ..ok.clone()
                },
                "target",
            ),
            (
                Outcome {
                    at: "yesterday".into(),
                    ..ok.clone()
                },
                "`at`",
            ),
            (
                Outcome {
                    verdict: Verdict::Dontknow,
                    ..ok.clone()
                },
                "no confidence",
            ),
            (
                Outcome {
                    confidence: Some(101),
                    ..ok.clone()
                },
                "0–100",
            ),
            (
                Outcome {
                    hint_rung: Some(6),
                    ..ok.clone()
                },
                "0–5",
            ),
            (
                Outcome {
                    cue_distance: Some(4),
                    ..ok.clone()
                },
                "0–3",
            ),
            (
                Outcome {
                    misconception: Some("x".into()),
                    ..ok.clone()
                },
                "structural",
            ),
        ];
        for (bad, why) in cases {
            let err = append_outcome(&repo, &bad).unwrap_err().to_string();
            assert!(err.contains(why), "{err}");
        }
        assert!(!path(&repo).exists(), "nothing was written");
        let _ = std::fs::remove_dir_all(&repo);
    }
}
