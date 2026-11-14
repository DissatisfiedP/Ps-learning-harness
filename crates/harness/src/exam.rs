//! Exam mode (architecture §4.14): coverage, priority, the daily plan and the
//! score simulation.
//!
//! An exam goal's session holds the planner's plan. Each plan node maps to a
//! graph node (`graphNode`, else its own id) and carries the exam's weight
//! for it (`PlanNode.weight`, 0–1: the planner reads the syllabus weights or
//! the topic frequency in the past papers and writes it with `plan_write`).
//! A node with no weight takes the weight from the goal's `exam.weights` text
//! when a line of it names the node ([`parse_weights`]), else
//! [`DEFAULT_WEIGHT`]. Every function below is pure: the plan's topics, the
//! learner model and a clock go in, a ranked, explained plan comes out. It is
//! recomputed on every call, so the priorities move daily with R.
//!
//! **Priority** (§4.14): `topicWeight × (1 − R) × (layerGap + 1)`, where R is
//! the node's retrievability now (0 without evidence) and `layerGap` the
//! number of practice layers (recall, discrimination, composition) the
//! learner has not shown on it.
//!
//! **Mode.** Teaching sessions are reserved for nodes that keep producing
//! structural misses (an open misconception, or [`TEACH_AFTER_STRUCTURAL`]
//! recorded ones); everything else goes through practice.
//!
//! **Simulation.** The expected score is the weight-averaged probability of
//! recall, now and on the exam day (the forgetting curve carried to that
//! day), and the nodes whose one successful review today would raise it most.
//! It is an estimate: FSRS's parameters were fit on flashcards, and an exam
//! answer needs more than recall **[inference]** (architecture §4.13).

use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::knowledge::KnowledgeMap;
use crate::learner::{fsrs, LearnerModel, NodeState};
use crate::sessions::{Mode, Op};
use crate::state::Plan;
use crate::{Error, Harness};

/// The weight of a node nobody weighted.
pub const DEFAULT_WEIGHT: f64 = 0.5;
/// Recorded structural misses after which a node is taught, not practised.
pub const TEACH_AFTER_STRUCTURAL: usize = 2;
/// The horizon of the gains when the exam has no date, in days.
pub const HORIZON_DAYS: f64 = 14.0;
/// The shortest and longest time budget of a paper, in minutes.
pub const BUDGET_MIN: std::ops::RangeInclusive<u32> = 5..=360;
/// The budget when the plan states no sitting length, in minutes.
pub const DEFAULT_BUDGET_MIN: u32 = 60;
/// Minutes of paper per problem, which sets how many a budget holds.
pub const MINUTES_PER_PROBLEM: u32 = 15;
/// Entries "what would raise it most" lists.
const RAISE_SHOWN: usize = 5;
/// Days before the exam in which a timed paper is part of the day.
const PAPER_WINDOW_DAYS: i64 = 7;
/// Days a paper keeps a new one off the day's plan.
const PAPER_EVERY_DAYS: i64 = 3;
/// Nodes in a day of practice.
const PRACTISE_NODES: usize = 3;
/// Share of the practice sittings that are timed papers (one in three).
const PAPER_SITTING_RATIO: i64 = 3;

// ---------------------------------------------------------------------------
// Coverage: the plan's nodes with their exam weights
// ---------------------------------------------------------------------------

/// One examinable idea: a graph node with the exam's weight for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Topic {
    /// The graph node.
    pub id: String,
    pub title: String,
    /// The graph topic it lives in, and that topic's title.
    pub home: String,
    pub home_title: String,
    /// The plan node it came from.
    pub plan_node: String,
    pub weight: f64,
    /// Where the weight came from: `plan`, `goal` (the goal's weights text) or
    /// `default`.
    pub weight_source: &'static str,
    /// The planner judged that practice alone handles it.
    pub practice_only: bool,
}

/// A plan node that is not (yet) a live idea of the graph, so it cannot be
/// ranked or practised.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Unmapped {
    pub plan_node: String,
    pub title: String,
}

fn live(map: &KnowledgeMap, id: &str) -> Option<String> {
    let id = map.canonical(id);
    (map.node(&id).is_some() && !map.is_tombstoned(&id)).then_some(id)
}

/// The graph node a plan node stands for: its `graphNode`, else the graph
/// node with its id, else the live node whose title (or alias) is the plan
/// node's title, ignoring case. The planner reuses graph ids when the graph
/// has the idea, and a node taught since carries the planner's title.
fn resolve(map: &KnowledgeMap, node: &crate::state::PlanNode) -> Option<String> {
    if let Some(id) = live(map, node.graph_node.as_deref().unwrap_or(&node.id)) {
        return Some(id);
    }
    let title = node.title.trim().to_lowercase();
    if title.is_empty() {
        return None;
    }
    map.nodes
        .iter()
        .filter(|n| !map.is_tombstoned(&n.id))
        .find(|n| {
            n.title.trim().to_lowercase() == title
                || n.aliases.iter().any(|a| a.trim().to_lowercase() == title)
        })
        .map(|n| n.id.clone())
}

fn clamp_weight(w: f64) -> f64 {
    if w.is_finite() {
        w.clamp(0.0, 1.0)
    } else {
        DEFAULT_WEIGHT
    }
}

/// The weights a goal's free text states, one per line: `name: 30%`,
/// `name - 0.3`, `name (12)`. `%` divides by 100; a plain number up to 1 is
/// taken as it is; larger plain numbers are points, scaled so the largest is
/// 1. Lines without a trailing number are skipped.
pub fn parse_weights(text: &str) -> Vec<(String, f64)> {
    let mut rows: Vec<(String, f64, bool)> = Vec::new();
    for line in text.lines() {
        let line = line.trim().trim_start_matches(['-', '*', '•', '+']).trim();
        let line = line.trim_end_matches([')', '.', ' ']);
        let percent = line.ends_with('%');
        let body = line.trim_end_matches('%');
        let start = body
            .rfind(|c: char| !(c.is_ascii_digit() || c == '.'))
            .map_or(0, |i| {
                i + body[i..].chars().next().map_or(1, char::len_utf8)
            });
        let Ok(value) = body[start..].parse::<f64>() else {
            continue;
        };
        let name = body[..start]
            .trim_end_matches(|c: char| {
                c.is_whitespace() || matches!(c, ':' | '-' | '–' | '=' | '(' | ',' | '~')
            })
            .trim();
        if name.chars().count() < 3 || !value.is_finite() || value < 0.0 {
            continue;
        }
        rows.push((name.to_owned(), value, percent));
    }
    let max_points = rows
        .iter()
        .filter(|(_, v, p)| !p && *v > 1.0)
        .map(|(_, v, _)| *v)
        .fold(0.0, f64::max);
    rows.into_iter()
        .map(|(name, v, percent)| {
            let w = if percent {
                v / 100.0
            } else if v > 1.0 {
                v / max_points
            } else {
                v
            };
            (name, clamp_weight(w))
        })
        .collect()
}

/// The weight a goal's text gives a node, by the node naming it (either
/// string holding the other, case-insensitively, at least 3 characters).
fn text_weight(rows: &[(String, f64)], title: &str) -> Option<f64> {
    let title = title.to_lowercase();
    rows.iter()
        .find(|(name, _)| {
            let name = name.to_lowercase();
            title.contains(&name) || name.contains(&title)
        })
        .map(|(_, w)| *w)
}

/// The exam's topics from the plan: each plan node on a live graph node, with
/// its weight. Two plan nodes on one graph node keep the heavier. `weights`
/// is the goal's `exam.weights` text.
pub fn topics_of(plan: &Plan, map: &KnowledgeMap, weights: &str) -> (Vec<Topic>, Vec<Unmapped>) {
    let rows = parse_weights(weights);
    let mut topics: Vec<Topic> = Vec::new();
    let mut unmapped: Vec<Unmapped> = Vec::new();
    for node in &plan.nodes {
        let Some(id) = resolve(map, node) else {
            unmapped.push(Unmapped {
                plan_node: node.id.clone(),
                title: node.title.clone(),
            });
            continue;
        };
        let Some(g) = map.node(&id) else { continue };
        let (weight, source) = match (node.weight, text_weight(&rows, &g.title)) {
            (Some(w), _) => (clamp_weight(w), "plan"),
            (None, Some(w)) => (w, "goal"),
            (None, None) => (DEFAULT_WEIGHT, "default"),
        };
        let candidate = Topic {
            id: id.clone(),
            title: g.title.clone(),
            home: g.home.clone(),
            home_title: map
                .topics
                .iter()
                .find(|t| t.id == g.home)
                .map_or_else(|| g.home.clone(), |t| t.title.clone()),
            plan_node: node.id.clone(),
            weight,
            weight_source: source,
            practice_only: node.practice_only,
        };
        match topics.iter_mut().find(|t| t.id == id) {
            Some(t) if candidate.weight > t.weight => *t = candidate,
            Some(_) => {}
            None => topics.push(candidate),
        }
    }
    (topics, unmapped)
}

// ---------------------------------------------------------------------------
// Priority and the ranked nodes
// ---------------------------------------------------------------------------

/// Practice layers the learner has not shown on the node (0–3).
pub fn layer_gap(node: Option<&NodeState>) -> u32 {
    match node {
        None => 3,
        Some(n) => {
            let shown = [
                n.layers.recall,
                n.layers.discrimination,
                n.layers.composition,
            ];
            3 - shown.iter().filter(|s| **s).count() as u32
        }
    }
}

/// `topicWeight × (1 − R) × (layerGap + 1)`.
pub fn priority(weight: f64, retrievability: f64, layer_gap: u32) -> f64 {
    weight * (1.0 - retrievability.clamp(0.0, 1.0)) * (f64::from(layer_gap) + 1.0)
}

/// How a node is worked on today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkMode {
    Practice,
    Teach,
}

/// One ranked node with the terms of its priority.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub id: String,
    pub title: String,
    pub home: String,
    pub home_title: String,
    pub plan_node: String,
    pub weight: f64,
    pub weight_source: &'static str,
    /// Retrievability now; 0 without evidence.
    pub retrievability: f64,
    pub layer_gap: u32,
    pub priority: f64,
    pub mode: WorkMode,
    /// Why this mode, in words.
    pub reason: String,
    /// The node has at least one recorded answer.
    pub evidence: bool,
    pub practice_only: bool,
    /// Structural misses recorded on the node.
    pub structural_misses: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub misconception: Option<String>,
}

fn mode_of(
    node: Option<&NodeState>,
    r: f64,
    gap: u32,
) -> (WorkMode, String, usize, Option<String>) {
    let Some(n) = node else {
        return (
            WorkMode::Practice,
            "no answers yet: the first problems show what is there".into(),
            0,
            None,
        );
    };
    let structural = n.misconceptions.len();
    let open = n.open_misconceptions().last().map(|m| m.text.clone());
    if let Some(text) = &open {
        return (
            WorkMode::Teach,
            format!("an open misconception: {text}"),
            structural,
            open,
        );
    }
    if structural >= TEACH_AFTER_STRUCTURAL {
        return (
            WorkMode::Teach,
            format!("{structural} structural misses recorded: practice isn't repairing it"),
            structural,
            None,
        );
    }
    let reason = if r < crate::learner::DESIRED_RETENTION {
        format!("recall has faded (R {r:.2}): a fresh problem brings it back")
    } else if gap > 0 {
        format!("{gap} practice layer(s) not yet shown")
    } else {
        "held: keep it fresh".to_owned()
    };
    (WorkMode::Practice, reason, structural, None)
}

/// The topics ranked by priority, highest first (ties: heavier, then id).
pub fn rank(topics: &[Topic], model: &LearnerModel, now: DateTime<Utc>) -> Vec<Entry> {
    let mut out: Vec<Entry> = topics
        .iter()
        .map(|t| {
            let node = model.node(&t.id);
            let r = node.map_or(0.0, |n| n.retrievability(now));
            let gap = layer_gap(node);
            let (mode, reason, structural, misconception) = mode_of(node, r, gap);
            Entry {
                id: t.id.clone(),
                title: t.title.clone(),
                home: t.home.clone(),
                home_title: t.home_title.clone(),
                plan_node: t.plan_node.clone(),
                weight: t.weight,
                weight_source: t.weight_source,
                retrievability: r,
                layer_gap: gap,
                priority: priority(t.weight, r, gap),
                mode,
                reason,
                evidence: node.is_some(),
                practice_only: t.practice_only,
                structural_misses: structural,
                misconception,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.priority
            .total_cmp(&a.priority)
            .then(b.weight.total_cmp(&a.weight))
            .then_with(|| a.id.cmp(&b.id))
    });
    out
}

fn paper_pool(entries: &[Entry]) -> Vec<&Entry> {
    entries
        .iter()
        .filter(|e| e.mode == WorkMode::Practice && e.weight > 0.0)
        .collect()
}

/// What a paper's problems are written on: the best-ranked practice-mode
/// nodes with some weight, cycling through them when the paper has more
/// problems than there are nodes. A node needing teaching is not on a paper.
pub fn paper_targets(entries: &[Entry], size: usize) -> Vec<String> {
    let pool = paper_pool(entries);
    if pool.is_empty() {
        return Vec::new();
    }
    (0..size).map(|i| pool[i % pool.len()].id.clone()).collect()
}

/// How many problems a budget holds.
pub fn paper_size(budget_min: u32) -> u32 {
    budget_min.div_ceil(MINUTES_PER_PROBLEM).clamp(
        *crate::practice::SET_SIZE.start(),
        *crate::practice::SET_SIZE.end(),
    )
}

// ---------------------------------------------------------------------------
// The day's plan and the mix
// ---------------------------------------------------------------------------

/// What today's sitting should be.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Today {
    /// `teach`, `paper`, `practice`, `over` (the exam date has passed) or
    /// `none` (nothing ranked yet).
    pub kind: &'static str,
    pub node_ids: Vec<String>,
    pub reason: String,
}

/// The days before the exam split into sittings.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Mix {
    /// Share of the total priority held by nodes that need teaching.
    pub teach_share: f64,
    pub teach_nodes: usize,
    pub practice_nodes: usize,
    /// One sitting a day until the exam; `None` without a date.
    pub sittings: Option<i64>,
    pub teach_sittings: Option<i64>,
    pub practice_sittings: Option<i64>,
    /// Of the practice sittings, the timed papers.
    pub papers: Option<i64>,
}

fn days_since(now: DateTime<Utc>, at: DateTime<Utc>) -> i64 {
    (now - at).num_days()
}

/// Today's sitting: the top-ranked node's mode decides between teaching and
/// practice, and in the exam's last week a timed paper replaces practice when
/// none was sat in the last [`PAPER_EVERY_DAYS`] days.
pub fn today(
    entries: &[Entry],
    days_left: Option<i64>,
    last_paper: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Today {
    if days_left.is_some_and(|d| d < 0) {
        return Today {
            kind: "over",
            node_ids: Vec::new(),
            reason: "the exam date has passed".into(),
        };
    }
    let ranked: Vec<&Entry> = entries.iter().filter(|e| e.priority > 0.0).collect();
    if ranked.is_empty() {
        return Today {
            kind: "none",
            node_ids: Vec::new(),
            reason: if entries.is_empty() {
                "the plan has no ideas in the knowledge graph yet".into()
            } else {
                "nothing is left to gain: every weighted idea is held".into()
            },
        };
    }
    // A node that needs teaching and ranks near the top is the day's work:
    // practice isn't repairing it.
    if let Some(t) = ranked
        .iter()
        .take(PRACTISE_NODES)
        .find(|e| e.mode == WorkMode::Teach)
    {
        return Today {
            kind: "teach",
            node_ids: vec![t.id.clone()],
            reason: format!(
                "{} ranks near the top and needs teaching: {}",
                t.title, t.reason
            ),
        };
    }
    let practice: Vec<&Entry> = entries
        .iter()
        .filter(|e| e.mode == WorkMode::Practice && e.priority > 0.0)
        .collect();
    let paper_due = days_left.is_some_and(|d| d <= PAPER_WINDOW_DAYS)
        && last_paper.is_none_or(|p| days_since(now, p) >= PAPER_EVERY_DAYS);
    if paper_due {
        return Today {
            kind: "paper",
            node_ids: practice
                .iter()
                .take(PRACTISE_NODES * 2)
                .map(|e| e.id.clone())
                .collect(),
            reason: format!(
                "the exam is {} away and no paper was sat in the last {PAPER_EVERY_DAYS} days",
                match days_left {
                    Some(0) => "today".to_owned(),
                    Some(1) => "1 day".to_owned(),
                    Some(d) => format!("{d} days"),
                    None => "close".to_owned(),
                }
            ),
        };
    }
    Today {
        kind: "practice",
        node_ids: practice
            .iter()
            .take(PRACTISE_NODES)
            .map(|e| e.id.clone())
            .collect(),
        reason: "the highest-priority ideas by weight, weakness and layers not yet shown".into(),
    }
}

/// The recommended mix of teaching and practice for the days left.
pub fn mix(entries: &[Entry], days_left: Option<i64>) -> Mix {
    let total: f64 = entries.iter().map(|e| e.priority).sum();
    let teach: Vec<&Entry> = entries
        .iter()
        .filter(|e| e.mode == WorkMode::Teach)
        .collect();
    let teach_priority: f64 = teach.iter().map(|e| e.priority).sum();
    let share = if total > 0.0 {
        teach_priority / total
    } else {
        0.0
    };
    let sittings = days_left.map(|d| d.max(0));
    let teach_sittings = sittings.map(|s| {
        if teach.is_empty() {
            0
        } else {
            let wanted = ((s as f64) * share).round() as i64;
            wanted.max(1).min(teach.len() as i64).min(s)
        }
    });
    let practice_sittings = sittings.zip(teach_sittings).map(|(s, t)| s - t);
    Mix {
        teach_share: share,
        teach_nodes: teach.len(),
        practice_nodes: entries.len() - teach.len(),
        sittings,
        teach_sittings,
        practice_sittings,
        papers: practice_sittings.map(|p| {
            if p == 0 {
                0
            } else {
                (p / PAPER_SITTING_RATIO).max(1)
            }
        }),
    }
}

// ---------------------------------------------------------------------------
// The score simulation
// ---------------------------------------------------------------------------

/// One topic of the graph's coverage and its expected score.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopicScore {
    pub home: String,
    pub title: String,
    pub nodes: usize,
    pub weight: f64,
    pub expected_now: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_exam: Option<f64>,
}

/// A node whose one successful review today lifts the score.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Raise {
    pub id: String,
    pub title: String,
    pub mode: WorkMode,
    /// Points of the whole score (0–1) gained on the exam day.
    pub gain: f64,
}

/// The expected score and what would raise it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Simulation {
    /// Always `estimate`: the UI labels it so.
    pub label: &'static str,
    pub note: &'static str,
    /// Days the gains are measured at (the days left, else [`HORIZON_DAYS`]).
    pub horizon_days: f64,
    /// Weight-averaged recall probability now, 0–1.
    pub expected_now: f64,
    /// The same on the exam day if nothing more is practised; `None` without a date.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_exam: Option<f64>,
    /// Weighted nodes with no answer recorded (counted as 0).
    pub unseen: usize,
    pub topics: Vec<TopicScore>,
    pub raise: Vec<Raise>,
}

const SIMULATION_NOTE: &str = "An estimate, not a prediction. It reads each idea's recall probability from FSRS, whose parameters were fit on flashcards, not exam answers [inference]; it counts an idea with no answers as 0 and an exam answer needs more than recall.";

fn memory(n: &NodeState) -> fsrs::Memory {
    fsrs::Memory {
        stability: n.fsrs.stability,
        difficulty: n.fsrs.difficulty,
    }
}

fn days_between(from: DateTime<Utc>, to: DateTime<Utc>) -> f64 {
    (to - from).num_seconds().max(0) as f64 / 86_400.0
}

/// Recall probability `ahead` days from now (0 without evidence).
fn recall_in(node: Option<&NodeState>, now: DateTime<Utc>, ahead: f64) -> f64 {
    node.map_or(0.0, |n| {
        fsrs::retrievability(memory(n), days_between(n.fsrs.last_review, now) + ahead)
    })
}

/// Recall probability `ahead` days from now after one clean review today.
fn recall_after_review(node: Option<&NodeState>, now: DateTime<Utc>, ahead: f64) -> f64 {
    let reviewed = match node {
        Some(n) => fsrs::step(
            memory(n),
            days_between(n.fsrs.last_review, now),
            fsrs::Rating::Good,
        ),
        None => fsrs::init(fsrs::Rating::Good),
    };
    fsrs::retrievability(reviewed, ahead)
}

/// Expected score now and on the exam day, per topic and overall, and the
/// nodes that would raise it most.
pub fn simulate(
    entries: &[Entry],
    model: &LearnerModel,
    now: DateTime<Utc>,
    days_left: Option<i64>,
) -> Simulation {
    let ahead = days_left.map(|d| d.max(0) as f64);
    let horizon = ahead.unwrap_or(HORIZON_DAYS);
    let total: f64 = entries.iter().map(|e| e.weight).sum();
    let share = |w: f64| if total > 0.0 { w / total } else { 0.0 };

    let mut topics: Vec<TopicScore> = Vec::new();
    let mut expected_now = 0.0;
    let mut expected_exam = 0.0;
    let mut raise: Vec<Raise> = Vec::new();
    for e in entries {
        let node = model.node(&e.id);
        let r_now = e.retrievability;
        let r_exam = ahead.map(|a| recall_in(node, now, a));
        expected_now += share(e.weight) * r_now;
        expected_exam += share(e.weight) * r_exam.unwrap_or(0.0);
        let at_horizon = recall_in(node, now, horizon);
        let gain = share(e.weight) * (recall_after_review(node, now, horizon) - at_horizon);
        if gain > 0.0 {
            raise.push(Raise {
                id: e.id.clone(),
                title: e.title.clone(),
                mode: e.mode,
                gain,
            });
        }
        let topic = match topics.iter().position(|t| t.home == e.home) {
            Some(i) => &mut topics[i],
            None => {
                topics.push(TopicScore {
                    home: e.home.clone(),
                    title: e.home_title.clone(),
                    nodes: 0,
                    weight: 0.0,
                    expected_now: 0.0,
                    expected_exam: ahead.map(|_| 0.0),
                });
                topics.last_mut().expect("just pushed")
            }
        };
        topic.nodes += 1;
        topic.weight += e.weight;
        // Held as weighted sums, divided below.
        topic.expected_now += e.weight * r_now;
        if let (Some(sum), Some(r)) = (topic.expected_exam.as_mut(), r_exam) {
            *sum += e.weight * r;
        }
    }
    for t in &mut topics {
        if t.weight > 0.0 {
            t.expected_now /= t.weight;
            if let Some(x) = t.expected_exam.as_mut() {
                *x /= t.weight;
            }
        }
    }
    topics.sort_by(|a, b| {
        b.weight
            .total_cmp(&a.weight)
            .then_with(|| a.home.cmp(&b.home))
    });
    raise.sort_by(|a, b| b.gain.total_cmp(&a.gain).then_with(|| a.id.cmp(&b.id)));
    raise.truncate(RAISE_SHOWN);
    Simulation {
        label: "estimate",
        note: SIMULATION_NOTE,
        horizon_days: horizon,
        expected_now,
        expected_exam: ahead.map(|_| expected_exam),
        unseen: entries
            .iter()
            .filter(|e| !e.evidence && e.weight > 0.0)
            .count(),
        topics,
        raise,
    }
}

// ---------------------------------------------------------------------------
// Past papers and their mark schemes, from the reference libraries
// ---------------------------------------------------------------------------

/// A past paper (a `question-bank` file of a reference library) and the mark
/// scheme filed beside it, when the library has one.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Paper {
    pub library: String,
    /// `<library>/<file>`, as `library_read` takes it.
    pub path: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pages: Option<u32>,
    /// `<library>/<file>` of the mark scheme.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mark_scheme: Option<String>,
}

/// Words that make a file a mark scheme (or answer key) rather than a paper.
const SCHEME_WORDS: [&str; 10] = [
    "scheme",
    "schemes",
    "markscheme",
    "memo",
    "memorandum",
    "solutions",
    "solution",
    "marking",
    "answers",
    "answer",
];

/// A file name's words, lowercase, without the extension.
fn tokens(path: &str) -> Vec<String> {
    let file = path.rsplit('/').next().unwrap_or(path);
    let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
    stem.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn is_scheme(path: &str) -> bool {
    tokens(path)
        .iter()
        .any(|t| SCHEME_WORDS.contains(&t.as_str()))
}

/// A file's words with the mark-scheme words taken out: a paper and its
/// scheme share this key (`2023-paper1.pdf`, `2023-paper1-mark-scheme.pdf`).
fn stem_key(path: &str) -> Vec<String> {
    tokens(path)
        .into_iter()
        .filter(|t| {
            !SCHEME_WORDS.contains(&t.as_str()) && !matches!(t.as_str(), "mark" | "marks" | "key")
        })
        .collect()
}

/// The past papers of the libraries, each with its mark scheme when a file of
/// the same library has the paper's name and a mark-scheme word. Only files
/// whose text was extracted count.
pub fn papers_of(libraries: &[crate::library::Library]) -> Vec<Paper> {
    use crate::library::{Kind, Status};
    let mut out: Vec<Paper> = Vec::new();
    for lib in libraries {
        let readable =
            |f: &&crate::library::LibraryFile| matches!(f.status, Status::Ok | Status::Truncated);
        let schemes: Vec<&crate::library::LibraryFile> = lib
            .files
            .iter()
            .filter(readable)
            .filter(|f| is_scheme(&f.path))
            .collect();
        for f in lib.files.iter().filter(readable) {
            if f.kind != Kind::QuestionBank || is_scheme(&f.path) {
                continue;
            }
            let key = stem_key(&f.path);
            let scheme = schemes
                .iter()
                .find(|s| !key.is_empty() && stem_key(&s.path) == key)
                .map(|s| format!("{}/{}", lib.slug, s.path));
            out.push(Paper {
                library: lib.slug.clone(),
                path: format!("{}/{}", lib.slug, f.path),
                title: f.title.clone(),
                pages: f.pages,
                mark_scheme: scheme,
            });
        }
    }
    out
}

/// The papers in this device's libraries.
pub fn papers(repo: &std::path::Path) -> Vec<Paper> {
    papers_of(&crate::library::load_all(repo))
}

/// What the coach is told about the papers: where the past papers are (for
/// the style) and which have a mark scheme to grade by.
pub fn paper_lines(papers: &[Paper]) -> String {
    if papers.is_empty() {
        return "No past papers are in the learner's libraries: write the problems in the style the exam's name and syllabus suggest, and mark them with a scheme of your own.\n".into();
    }
    let mut out = String::new();
    for p in papers.iter().take(12) {
        out.push_str(&format!(
            "- `{}` — {}{}\n",
            p.path,
            p.title,
            p.mark_scheme
                .as_ref()
                .map_or(" (no mark scheme in the library)".into(), |m| format!(
                    " (mark scheme: `{m}`)"
                ))
        ));
    }
    if papers.len() > 12 {
        out.push_str(&format!(
            "… and {} more (library_list shows them all).\n",
            papers.len() - 12
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// The plan for the UI
// ---------------------------------------------------------------------------

/// The exam record of the goal (`goal_set.exam`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExamInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub syllabus: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub past_papers: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weights: Option<String>,
}

/// What a paper may be set to.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaperLimits {
    pub default_budget_min: u32,
    pub min_budget_min: u32,
    pub max_budget_min: u32,
    pub minutes_per_problem: u32,
    /// How many problems the default budget holds.
    pub default_problems: u32,
    /// Practice-mode ideas a paper could be written on.
    pub targets: usize,
}

/// `exam_plan`: the ranked plan, the day's sitting, the mix and the estimate.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExamPlan {
    pub session_id: String,
    pub exam: ExamInfo,
    /// RFC 3339; the clock the plan was computed on.
    pub now: String,
    /// Whole days to the exam date; negative once it has passed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub days_left: Option<i64>,
    pub nodes: Vec<Entry>,
    pub unmapped: Vec<Unmapped>,
    pub today: Today,
    pub mix: Mix,
    pub simulation: Simulation,
    /// Past papers and mark schemes in the learner's reference libraries.
    pub papers: Vec<Paper>,
    /// When the last exam paper's outcomes were written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_paper_at: Option<String>,
    pub paper: PaperLimits,
}

/// Whole days from `now`'s date to `date` (`YYYY-MM-DD…`).
pub fn days_left(date: &str, now: DateTime<Utc>) -> Option<i64> {
    let d = NaiveDate::parse_from_str(date.trim().get(..10)?, "%Y-%m-%d").ok()?;
    Some((d - now.date_naive()).num_days())
}

fn text_field(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// The goal's exam record, or why the goal isn't an exam.
pub fn exam_info(goal: &Value) -> Result<ExamInfo, String> {
    if goal.get("purpose").and_then(Value::as_str) != Some("exam") {
        return Err("this session's goal is not an exam".into());
    }
    let exam = goal.get("exam").cloned().unwrap_or(Value::Null);
    Ok(ExamInfo {
        name: text_field(&exam, "name")
            .or_else(|| text_field(goal, "goal"))
            .unwrap_or_else(|| "the exam".into()),
        // The goal's deadline stands in for a missing exam date.
        date: text_field(&exam, "date").or_else(|| {
            goal.get("constraints")
                .and_then(|c| text_field(c, "deadline"))
        }),
        syllabus: text_field(&exam, "syllabus"),
        past_papers: text_field(&exam, "pastPapers"),
        weights: text_field(&exam, "weights"),
    })
}

/// Everything above together; pure.
pub struct Inputs<'a> {
    pub session_id: &'a str,
    pub info: ExamInfo,
    pub plan: &'a Plan,
    pub map: &'a KnowledgeMap,
    pub model: &'a LearnerModel,
    pub papers: Vec<Paper>,
    pub last_paper_at: Option<DateTime<Utc>>,
    pub now: DateTime<Utc>,
}

pub fn build(i: Inputs) -> ExamPlan {
    let left = i.info.date.as_deref().and_then(|d| days_left(d, i.now));
    let (topics, unmapped) = topics_of(i.plan, i.map, i.info.weights.as_deref().unwrap_or(""));
    let nodes = rank(&topics, i.model, i.now);
    let default_budget = i
        .plan
        .minutes_per_sitting
        .unwrap_or(DEFAULT_BUDGET_MIN)
        .clamp(*BUDGET_MIN.start(), *BUDGET_MIN.end());
    ExamPlan {
        session_id: i.session_id.to_owned(),
        today: today(&nodes, left, i.last_paper_at, i.now),
        mix: mix(&nodes, left),
        simulation: simulate(&nodes, i.model, i.now, left),
        paper: PaperLimits {
            default_budget_min: default_budget,
            min_budget_min: *BUDGET_MIN.start(),
            max_budget_min: *BUDGET_MIN.end(),
            minutes_per_problem: MINUTES_PER_PROBLEM,
            default_problems: paper_size(default_budget),
            targets: paper_pool(&nodes).len(),
        },
        nodes,
        unmapped,
        papers: i.papers,
        last_paper_at: i
            .last_paper_at
            .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        exam: i.info,
        now: i.now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        days_left: left,
    }
}

impl Harness {
    /// The exam plan of an exam goal's Learn session, on the harness clock.
    pub fn exam_plan(&self, session_id: &str) -> Result<ExamPlan, Error> {
        self.exam_plan_at(session_id, self.clock())
    }

    pub(crate) fn exam_plan_at(
        &self,
        session_id: &str,
        now: DateTime<Utc>,
    ) -> Result<ExamPlan, Error> {
        let ops = self.store.ops(session_id)?;
        let mode = ops.iter().find_map(|o| match o {
            Op::Session(h) => Some(h.mode),
            _ => None,
        });
        if mode != Some(Mode::Learn) {
            return Err(Error::Invalid(
                "the exam plan belongs to the Learn session of an exam goal".into(),
            ));
        }
        let state = crate::replay::replay_state(&ops);
        let goal = state
            .goal
            .as_ref()
            .ok_or_else(|| Error::Invalid("this session has no goal yet".into()))?;
        let info = exam_info(goal).map_err(Error::Invalid)?;
        let plan = state.plan.as_ref().ok_or_else(|| {
            Error::Invalid(
                "the exam has no plan yet: the plan's weighted ideas are what the exam plan ranks"
                    .into(),
            )
        })?;
        let map = crate::knowledge::load(&self.repo)?;
        let (outcomes, _) = crate::learner::read_outcomes(&self.repo)?;
        let model = LearnerModel::from_outcomes(&outcomes, |id| map.canonical(id));
        let last_paper_at = outcomes
            .iter()
            .filter(|o| o.format == crate::learner::Format::Exam)
            .filter_map(|o| DateTime::parse_from_rfc3339(&o.at).ok())
            .map(|t| t.with_timezone(&Utc))
            .max();
        Ok(build(Inputs {
            session_id,
            info,
            plan,
            map: &map,
            model: &model,
            papers: papers(&self.repo),
            last_paper_at,
            now,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learner::{Fsrs, Layers, Misconception, NodeState};
    use crate::state::{Kind, PlanNode};
    use chrono::Duration;

    fn days_ago(now: DateTime<Utc>, d: i64) -> DateTime<Utc> {
        now - Duration::days(d)
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-14T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn topic(id: &str, home: &str, weight: f64) -> Topic {
        Topic {
            id: id.into(),
            title: id.into(),
            home: home.into(),
            home_title: home.to_uppercase(),
            plan_node: id.into(),
            weight,
            weight_source: "plan",
            practice_only: false,
        }
    }

    /// `misconceptions` recorded, the first `resolved` of them resolved.
    fn state(
        stability: f64,
        last_days: i64,
        layers: [bool; 3],
        misconceptions: usize,
        resolved: usize,
    ) -> NodeState {
        NodeState {
            fsrs: Fsrs {
                difficulty: 5.0,
                stability,
                last_review: days_ago(now(), last_days),
            },
            layers: Layers {
                recall: layers[0],
                discrimination: layers[1],
                composition: layers[2],
            },
            misconceptions: (0..misconceptions)
                .map(|i| Misconception {
                    text: format!("belief {i}"),
                    at: "2026-11-01T00:00:00Z".into(),
                    session_id: "s".into(),
                    resolved_at: (i < resolved).then(|| "2026-11-02T00:00:00Z".into()),
                })
                .collect(),
            reviews: 3,
            lapses: 0,
            level_reached: None,
        }
    }

    fn model(nodes: Vec<(&str, NodeState)>) -> LearnerModel {
        LearnerModel::for_tests(nodes.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
    }

    #[test]
    fn priority_multiplies_weight_forgetting_and_unshown_layers() {
        // Full weight, nothing recalled, no layer shown: 1 × 1 × 4.
        assert!((priority(1.0, 0.0, 3) - 4.0).abs() < 1e-12);
        // Half weight, R 0.8, one layer missing: 0.5 × 0.2 × 2.
        assert!((priority(0.5, 0.8, 1) - 0.2).abs() < 1e-12);
        // A held, fully shown idea has nothing left to gain.
        assert!(priority(1.0, 1.0, 0) < 1e-12);
        assert_eq!(priority(0.0, 0.0, 3), 0.0, "weightless: off the syllabus");
        assert_eq!(layer_gap(None), 3);
        assert_eq!(
            layer_gap(Some(&state(10.0, 1, [true, false, true], 0, 0))),
            1
        );
    }

    #[test]
    fn ranking_orders_by_priority_and_explains_the_terms() {
        let m = model(vec![
            // Fresh and fully shown: low priority however heavy.
            ("held", state(60.0, 1, [true, true, true], 0, 0)),
            // Old review: R has fallen; two layers missing.
            ("faded", state(5.0, 30, [true, false, false], 0, 0)),
        ]);
        let topics = [
            topic("held", "a", 1.0),
            topic("faded", "a", 0.6),
            topic("unseen", "b", 0.1),
        ];
        let ranked = rank(&topics, &m, now());
        let order: Vec<&str> = ranked.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(order, ["faded", "unseen", "held"], "{ranked:#?}");
        let faded = &ranked[0];
        assert_eq!(faded.layer_gap, 2);
        assert!(faded.retrievability < 0.8, "{}", faded.retrievability);
        let want = priority(0.6, faded.retrievability, 2);
        assert!((faded.priority - want).abs() < 1e-12);
        let unseen = &ranked[1];
        assert!(!unseen.evidence);
        assert_eq!((unseen.retrievability, unseen.layer_gap), (0.0, 3));
        assert!((unseen.priority - 0.1 * 4.0).abs() < 1e-12);
    }

    #[test]
    fn a_node_is_taught_only_for_open_or_repeated_structural_misses() {
        let m = model(vec![
            ("open", state(5.0, 3, [true, false, false], 1, 0)),
            // Two misses recorded and both repaired since: still taught.
            ("repeat", state(5.0, 3, [true, true, false], 2, 2)),
            ("faded", state(2.0, 40, [true, false, false], 0, 0)),
        ]);
        let topics = [
            topic("open", "a", 0.5),
            topic("repeat", "a", 0.5),
            topic("faded", "a", 0.5),
            topic("unseen", "a", 0.5),
        ];
        let by: std::collections::BTreeMap<String, Entry> = rank(&topics, &m, now())
            .into_iter()
            .map(|e| (e.id.clone(), e))
            .collect();
        assert_eq!(by["open"].mode, WorkMode::Teach);
        assert!(
            by["open"].reason.contains("belief 0"),
            "{}",
            by["open"].reason
        );
        assert_eq!(by["repeat"].mode, WorkMode::Teach);
        assert_eq!(by["repeat"].structural_misses, 2);
        assert_eq!(
            by["faded"].mode,
            WorkMode::Practice,
            "forgetting is practised"
        );
        assert!(
            by["faded"].reason.contains("faded"),
            "{}",
            by["faded"].reason
        );
        assert_eq!(by["unseen"].mode, WorkMode::Practice);
    }

    #[test]
    fn a_paper_is_written_on_practice_nodes_in_priority_order_and_cycles() {
        let m = model(vec![("teachme", state(5.0, 3, [true, false, false], 1, 0))]);
        let topics = [
            topic("a", "x", 0.9),
            topic("b", "x", 0.5),
            topic("off", "x", 0.0),
            topic("teachme", "x", 1.0),
        ];
        let ranked = rank(&topics, &m, now());
        assert_eq!(paper_targets(&ranked, 5), ["a", "b", "a", "b", "a"]);
        assert_eq!(paper_targets(&ranked, 1), ["a"]);
        assert!(paper_targets(&[], 3).is_empty());
        assert_eq!(paper_size(5), 2, "at least two problems");
        assert_eq!(paper_size(60), 4);
        assert_eq!(paper_size(90), 6);
        assert_eq!(paper_size(360), 10, "at most ten");
    }

    #[test]
    fn weights_text_parses_percentages_fractions_and_points() {
        let rows =
            parse_weights("Derivatives: 30%\n- Integrals - 0.2\n* Series (12)\nTotal\nLimits 6");
        assert_eq!(rows.len(), 4, "{rows:?}");
        assert_eq!(rows[0], ("Derivatives".to_owned(), 0.3));
        assert_eq!(rows[1], ("Integrals".to_owned(), 0.2));
        // Points scale so the largest is 1.
        assert_eq!(rows[2], ("Series".to_owned(), 1.0));
        assert_eq!(rows[3], ("Limits".to_owned(), 0.5));
        assert!(parse_weights("").is_empty());
        assert!(parse_weights("ab: 50%").is_empty(), "too short a name");
    }

    fn plan_with(nodes: Vec<PlanNode>) -> Plan {
        Plan {
            version: 1,
            goal: "pass".into(),
            approach: String::new(),
            depth: "pass".into(),
            explore: "off".into(),
            minutes_per_sitting: Some(45),
            style: String::new(),
            nodes,
            sections: vec![],
            tracks: vec![],
        }
    }

    fn plan_node(id: &str, title: &str, weight: Option<f64>) -> PlanNode {
        PlanNode {
            id: id.into(),
            title: title.into(),
            kind: Kind::Mechanism,
            requires: vec![],
            graph_node: None,
            weight,
            practice_only: false,
        }
    }

    fn map_of(ids: &[(&str, &str)]) -> KnowledgeMap {
        use crate::graph::{append, provenance, GraphOp};
        let dir = std::env::temp_dir().join(format!(
            "lh-exam-map-{}-{}",
            std::process::id(),
            ulid::Ulid::new()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut ops = vec![GraphOp::topic("calc", "Calculus", None)];
        for (id, title) in ids {
            ops.push(GraphOp::node(
                id,
                Some(title),
                "A statement.",
                Kind::Mechanism,
                "calc",
                Some(provenance("import", None, "seed")),
            ));
        }
        append(&dir, &ops).unwrap();
        let map = crate::knowledge::load(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        map
    }

    #[test]
    fn topics_take_the_plans_weight_then_the_goals_text_then_the_default() {
        let map = map_of(&[
            ("deriv", "Derivatives"),
            ("integ", "Integrals"),
            ("limit", "Limits"),
        ]);
        let plan = plan_with(vec![
            plan_node("deriv", "Derivatives", Some(0.8)),
            plan_node("integ", "Integrals", None),
            plan_node("limit", "Limits", None),
            plan_node("ghost", "Not in the graph", Some(0.9)),
        ]);
        let (topics, unmapped) = topics_of(&plan, &map, "Integrals: 40%");
        let get = |id: &str| topics.iter().find(|t| t.id == id).unwrap();
        assert_eq!(
            (get("deriv").weight, get("deriv").weight_source),
            (0.8, "plan")
        );
        assert_eq!(
            (get("integ").weight, get("integ").weight_source),
            (0.4, "goal")
        );
        assert_eq!(
            (get("limit").weight, get("limit").weight_source),
            (DEFAULT_WEIGHT, "default")
        );
        assert_eq!(get("deriv").home_title, "Calculus");
        assert_eq!(unmapped.len(), 1);
        assert_eq!(unmapped[0].plan_node, "ghost");

        // Two plan nodes on one graph node keep the heavier weight.
        let mut dup = plan_with(vec![
            plan_node("a", "Derivatives", Some(0.2)),
            plan_node("b", "Derivatives again", Some(0.7)),
        ]);
        dup.nodes[0].graph_node = Some("deriv".into());
        dup.nodes[1].graph_node = Some("deriv".into());
        let (topics, _) = topics_of(&dup, &map, "");
        assert_eq!(topics.len(), 1);
        assert_eq!(topics[0].weight, 0.7);

        // A plan node whose own id is not in the graph is found by its title.
        let by_title = plan_with(vec![plan_node("integrals-1", "  integrals ", Some(0.5))]);
        let (topics, unmapped) = topics_of(&by_title, &map, "");
        assert!(unmapped.is_empty(), "{unmapped:?}");
        assert_eq!(topics[0].id, "integ");
    }

    #[test]
    fn days_left_counts_calendar_days_and_goes_negative_after_the_date() {
        assert_eq!(days_left("2026-11-24", now()), Some(10));
        assert_eq!(days_left("2026-11-14T08:00:00Z", now()), Some(0));
        assert_eq!(days_left("2026-11-10", now()), Some(-4));
        assert_eq!(days_left("next spring", now()), None);
    }

    #[test]
    fn the_estimate_falls_toward_the_exam_and_a_review_today_raises_it() {
        let m = model(vec![
            ("a", state(4.0, 2, [true, true, true], 0, 0)),
            ("b", state(4.0, 2, [true, true, true], 0, 0)),
        ]);
        let ranked = rank(
            &[
                topic("a", "x", 0.75),
                topic("b", "y", 0.25),
                topic("c", "y", 0.0),
            ],
            &m,
            now(),
        );
        let sim = simulate(&ranked, &m, now(), Some(20));
        assert_eq!(sim.label, "estimate");
        assert!(sim.note.contains("[inference]"));
        let exam = sim.expected_exam.unwrap();
        assert!(
            exam < sim.expected_now && sim.expected_now > 0.0 && exam >= 0.0,
            "{sim:#?}"
        );
        // Two topics; x holds three quarters of the weight.
        assert_eq!(sim.topics.len(), 2);
        assert_eq!(sim.topics[0].home, "x");
        assert_eq!(sim.unseen, 0, "c has no answers but no weight: not counted");
        let first = &sim.raise[0];
        assert_eq!(
            first.id, "a",
            "the heavier idea gains more: {:#?}",
            sim.raise
        );
        assert!(first.gain > 0.0 && sim.raise.windows(2).all(|w| w[0].gain >= w[1].gain));

        // No date: no exam-day figure, gains at the fixed horizon.
        let undated = simulate(&ranked, &m, now(), None);
        assert!(undated.expected_exam.is_none());
        assert_eq!(undated.horizon_days, HORIZON_DAYS);
        // The estimate of an empty plan is zero, never NaN.
        let empty = simulate(&[], &m, now(), Some(5));
        assert_eq!((empty.expected_now, empty.expected_exam), (0.0, Some(0.0)));
    }

    #[test]
    fn the_day_is_teaching_a_structural_node_else_a_paper_in_the_last_week_else_practice() {
        let teach = model(vec![("t", state(5.0, 3, [true, false, false], 1, 0))]);
        let ranked = rank(&[topic("t", "x", 1.0), topic("p", "x", 0.4)], &teach, now());
        let day = today(&ranked, Some(30), None, now());
        assert_eq!(
            (day.kind, day.node_ids.as_slice()),
            ("teach", &["t".to_owned()][..])
        );

        let calm = model(vec![]);
        let ranked = rank(&[topic("p", "x", 0.4), topic("q", "x", 0.9)], &calm, now());
        let far = today(&ranked, Some(30), None, now());
        assert_eq!(far.kind, "practice");
        assert_eq!(far.node_ids, ["q", "p"]);
        let near = today(&ranked, Some(5), None, now());
        assert_eq!(near.kind, "paper");
        let recent = today(&ranked, Some(5), Some(days_ago(now(), 1)), now());
        assert_eq!(recent.kind, "practice", "a paper was sat yesterday");
        let stale = today(&ranked, Some(5), Some(days_ago(now(), 4)), now());
        assert_eq!(stale.kind, "paper");
        assert_eq!(today(&ranked, Some(-1), None, now()).kind, "over");
        assert_eq!(today(&[], Some(5), None, now()).kind, "none");
    }

    #[test]
    fn the_mix_follows_the_priority_held_by_nodes_that_need_teaching() {
        let teach = model(vec![("t", state(5.0, 3, [true, false, false], 1, 0))]);
        let ranked = rank(&[topic("t", "x", 1.0), topic("p", "x", 1.0)], &teach, now());
        let m = mix(&ranked, Some(12));
        assert_eq!((m.teach_nodes, m.practice_nodes), (1, 1));
        assert!(m.teach_share > 0.0 && m.teach_share < 1.0);
        let (t, p) = (m.teach_sittings.unwrap(), m.practice_sittings.unwrap());
        assert_eq!(t + p, 12);
        assert_eq!(t, 1, "one teach node: one sitting");
        assert_eq!(m.papers, Some(p / 3));

        let none = mix(
            &rank(&[topic("p", "x", 1.0)], &model(vec![]), now()),
            Some(9),
        );
        assert_eq!(
            (none.teach_sittings, none.practice_sittings),
            (Some(0), Some(9))
        );
        assert_eq!(none.papers, Some(3));
        let undated = mix(&ranked, None);
        assert!(undated.sittings.is_none() && undated.papers.is_none());
        assert_eq!(mix(&ranked, Some(0)).papers, Some(0));
    }

    fn file(
        path: &str,
        kind: crate::library::Kind,
        status: crate::library::Status,
    ) -> crate::library::LibraryFile {
        crate::library::LibraryFile {
            path: path.into(),
            kind,
            format: "pdf".into(),
            title: path.into(),
            bytes: 1,
            sha256: String::new(),
            status,
            text_bytes: 1,
            pages: Some(4),
            scanned_pages: 0,
            note: None,
        }
    }

    #[test]
    fn past_papers_pair_with_the_mark_scheme_filed_beside_them() {
        use crate::library::{Kind, Library, Status};
        let lib = Library {
            slug: "maths".into(),
            name: "Maths".into(),
            description: String::new(),
            topic: String::new(),
            created: String::new(),
            updated: String::new(),
            max_file_bytes: 1,
            files: vec![
                file("papers/2023-paper1.pdf", Kind::QuestionBank, Status::Ok),
                file(
                    "papers/2023-paper1-mark-scheme.pdf",
                    Kind::QuestionBank,
                    Status::Ok,
                ),
                file("papers/2022-paper1.pdf", Kind::QuestionBank, Status::Ok),
                // A scheme whose text could not be read is no scheme.
                file(
                    "papers/2022-paper1-answers.pdf",
                    Kind::QuestionBank,
                    Status::NoText,
                ),
                file("papers/2021-paper1.pdf", Kind::QuestionBank, Status::NoText),
                file("syllabus.pdf", Kind::Guide, Status::Ok),
            ],
            skipped: Default::default(),
            skipped_samples: Default::default(),
            classifier: 0,
        };
        let found = papers_of(&[lib]);
        let names: Vec<&str> = found.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(
            names,
            [
                "maths/papers/2023-paper1.pdf",
                "maths/papers/2022-paper1.pdf"
            ]
        );
        assert_eq!(
            found[0].mark_scheme.as_deref(),
            Some("maths/papers/2023-paper1-mark-scheme.pdf")
        );
        assert!(found[1].mark_scheme.is_none());
        let told = paper_lines(&found);
        assert!(
            told.contains("mark scheme: `maths/papers/2023-paper1-mark-scheme.pdf`"),
            "{told}"
        );
        assert!(told.contains("no mark scheme in the library"), "{told}");
        assert!(paper_lines(&[]).contains("No past papers"));
    }

    #[test]
    fn the_plan_reads_the_goal_and_keeps_the_estimate_labelled() {
        let goal = serde_json::json!({
            "goal": "Pass calculus", "purpose": "exam",
            "exam": { "name": "MATH101 final", "date": "2026-11-24", "weights": "Integrals: 40%" }
        });
        let info = exam_info(&goal).unwrap();
        assert_eq!(info.name, "MATH101 final");
        assert!(exam_info(&serde_json::json!({ "purpose": "curiosity" })).is_err());
        // The goal's deadline stands in for a missing date.
        let by_deadline = exam_info(&serde_json::json!({
            "goal": "g", "purpose": "exam", "constraints": { "deadline": "2026-12-01" }
        }))
        .unwrap();
        assert_eq!(by_deadline.date.as_deref(), Some("2026-12-01"));
        assert_eq!(by_deadline.name, "g");

        let map = map_of(&[("deriv", "Derivatives"), ("integ", "Integrals")]);
        let plan = plan_with(vec![
            plan_node("deriv", "Derivatives", Some(0.8)),
            plan_node("integ", "Integrals", None),
        ]);
        let m = model(vec![]);
        let built = build(Inputs {
            session_id: "s1",
            info,
            plan: &plan,
            map: &map,
            model: &m,
            papers: vec![],
            last_paper_at: None,
            now: now(),
        });
        assert_eq!(built.days_left, Some(10));
        assert_eq!(built.nodes.len(), 2);
        assert_eq!(built.nodes[0].id, "deriv", "0.8 × 4 beats 0.4 × 4");
        assert_eq!(built.simulation.label, "estimate");
        assert_eq!(built.paper.default_budget_min, 45);
        assert_eq!(built.paper.default_problems, 3);
        assert_eq!(built.paper.targets, 2);
        // A weight written in the goal text moved the second node's priority.
        assert!((built.nodes[1].weight - 0.4).abs() < 1e-12);
        assert_eq!(built.nodes[1].weight_source, "goal");
    }
}
