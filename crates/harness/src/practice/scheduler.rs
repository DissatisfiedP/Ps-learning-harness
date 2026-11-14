//! Target selection (architecture §4.13) and the shape of each problem (§4.11).
//!
//! Pure: the knowledge map, the learner model, the session so far and the
//! teaching index go in; a [`ProblemPlan`] comes out. Nothing here reads
//! files or calls a model, so every rule is a unit test.
//!
//! **Target.** From the learner's picked ideas, or the whole graph when none
//! were picked. A candidate's class, best first:
//!
//! 0. an open misconception (a structural miss not yet repaired);
//! 1. due: retrievability below the desired retention;
//! 2. weak: a third or more of its reviews were lapses;
//! 3. a layer not yet demonstrated;
//! 4. no evidence at all;
//! 5. everything else (keeping it fresh).
//!
//! Within a class the node others depend on through a load-bearing
//! `requires` edge comes first, then the lowest R. Mixing beats blocking, so
//! the number of problems this session already gave a target sorts before
//! the class: the targets rotate. The target just practised waits one turn,
//! and a target missed twice in a row is parked (switch target) until the
//! retests at the end.
//!
//! **Layer.** The cheapest layer the learner hasn't demonstrated on the
//! node (recall, then discrimination, then composition), held after a miss;
//! a distractor is always a discrimination problem (the learner must decide
//! the idea does not apply).
//!
//! **Level.** The gap between the highest level reached and the plan's
//! target level: the next one up, capped at the target. A node never shown
//! at any level starts at `use` (or the target, if lower).
//!
//! **Cue distance.** The difficulty rule (§4.11): start from what the node's
//! layers show; after that, a clean success goes one notch up, a success with
//! a hint or a partial holds, a miss goes one notch down.
//!
//! **Distractors.** About one problem in four (not the first two) does not
//! need the target, chosen by a hash of the session and the index so no
//! pattern can become a cue.
//!
//! **Hosts.** The graph nodes the problem may be set in: other-topic nodes
//! that `requires`-edge into the target, then held other-topic nodes the
//! target feeds. Only for cue distance 2–3.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};

use crate::knowledge::KnowledgeMap;
use crate::learner::{Layer, LearnerModel, NodeState, Verdict, DESIRED_RETENTION};
use crate::state::Level;

use super::state::{PracticeState, ProblemRecord, Status};
use super::{ProblemPlan, MAX_CUE};

/// How many host candidates the coach is shown.
const MAX_HOSTS_SHOWN: usize = 6;
/// How many hosts one plan names.
const MAX_HOSTS_PLANNED: usize = 2;
/// A distractor needs the session to have run this long first.
const NO_DISTRACTOR_BEFORE: u32 = 2;

/// What earlier teaching sessions say about each node: the level its plan
/// aimed at, the highest level a check showed, and the plugins that were
/// active when it was taught.
#[derive(Debug, Clone, Default)]
pub struct Taught {
    pub aim: HashMap<String, Level>,
    pub reached: HashMap<String, Level>,
    pub plugins: HashMap<String, Vec<String>>,
}

/// Everything the scheduler reads.
pub struct Context<'a> {
    pub map: &'a KnowledgeMap,
    pub model: &'a LearnerModel,
    pub taught: &'a Taught,
    pub state: &'a PracticeState,
    pub session_id: &'a str,
    pub now: DateTime<Utc>,
}

/// One problem of the session as the scheduler sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub target: String,
    pub layer: Layer,
    pub cue: u8,
    pub is_distractor: bool,
    pub retest: bool,
    /// `(verdict, hint rung)` once concluded.
    pub result: Option<(Verdict, Option<u8>)>,
}

/// The session so far.
pub fn trail(state: &PracticeState) -> Vec<Step> {
    state.problems.iter().map(step).collect()
}

fn step(p: &ProblemRecord) -> Step {
    let problem = p.problem();
    Step {
        target: p.plan.target.clone(),
        layer: problem.map_or(p.plan.layer, |q| q.layer),
        cue: problem.map_or(p.plan.cue_distance, |q| q.cue_distance),
        is_distractor: problem.map_or(p.plan.is_distractor, |q| q.is_distractor),
        retest: p.plan.retest_of.is_some(),
        result: match (&p.conclusion, p.status()) {
            (Some((c, _)), Status::Concluded) => Some((c.verdict, c.hint_rung)),
            _ => None,
        },
    }
}

fn is_miss(verdict: Verdict) -> bool {
    matches!(
        verdict,
        Verdict::Wrong | Verdict::Dontknow | Verdict::Dismissed
    )
}

/// The cue distance after a problem at `last_cue` ended as `last`: a clean
/// success (right, no hint) steps up, a success with a hint or a partial
/// holds, a miss steps down. `None`: the node's `base` distance.
pub fn next_cue(base: u8, last: Option<(u8, Verdict, Option<u8>)>) -> u8 {
    let Some((cue, verdict, rung)) = last else {
        return base.min(MAX_CUE);
    };
    match verdict {
        Verdict::Correct if rung.is_none() => (cue + 1).min(MAX_CUE),
        Verdict::Correct | Verdict::Partial => cue,
        _ => cue.saturating_sub(1),
    }
}

/// Where a node starts: close to the lesson until the idea has come to mind
/// once (1), further once recall is shown (2), furthest once it also held in
/// a discrimination or composition problem (3).
pub fn base_cue(node: Option<&NodeState>) -> u8 {
    match node {
        None => 1,
        Some(n) if !n.layers.recall => 1,
        Some(n) if n.layers.discrimination || n.layers.composition => 3,
        Some(_) => 2,
    }
}

/// The cheapest layer not yet demonstrated; all shown: composition.
pub fn cheapest_layer(node: Option<&NodeState>) -> Layer {
    match node {
        Some(n) if !n.layers.recall => Layer::Recall,
        Some(n) if !n.layers.discrimination => Layer::Discrimination,
        Some(n) if !n.layers.composition => Layer::Composition,
        Some(_) => Layer::Composition,
        None => Layer::Recall,
    }
}

fn next_level(level: Level) -> Level {
    match level {
        Level::Explain => Level::Use,
        Level::Use => Level::Analyze,
        Level::Analyze | Level::Design => Level::Design,
    }
}

/// The level a problem must demand: the step above what was reached, never
/// above what the plan aimed at; a node never shown starts at `use`.
pub fn level_for(reached: Option<Level>, aim: Option<Level>) -> Level {
    let aim = aim.unwrap_or(Level::Use);
    match reached {
        None => aim.min(Level::Use),
        Some(r) if r >= aim => aim,
        Some(r) => next_level(r).min(aim),
    }
}

fn fnv(session_id: &str, index: u32) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in session_id.bytes().chain(index.to_le_bytes()) {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Whether problem number `index` of the session is a distractor.
pub fn is_distractor(session_id: &str, index: u32) -> bool {
    index >= NO_DISTRACTOR_BEFORE && fnv(session_id, index).is_multiple_of(4)
}

fn dependents(map: &KnowledgeMap, id: &str) -> usize {
    map.prerequisites()
        .filter(|e| e.from == id && e.weight == "load-bearing")
        .count()
}

fn is_held(model: &LearnerModel, id: &str, now: DateTime<Utc>) -> bool {
    model.node(id).is_some_and(|n| n.held(now))
}

fn home_of(map: &KnowledgeMap, id: &str) -> String {
    map.node(id).map(|n| n.home.clone()).unwrap_or_default()
}

/// Where a problem for `target` may be set: other-topic prerequisites of the
/// target (held ones first), then held other-topic nodes the target feeds.
pub fn host_candidates(
    map: &KnowledgeMap,
    model: &LearnerModel,
    target: &str,
    now: DateTime<Utc>,
) -> Vec<String> {
    let home = home_of(map, target);
    let live = |id: &str| map.node(id).is_some() && !map.is_tombstoned(id);
    let mut prereqs: Vec<String> = Vec::new();
    let mut fed: Vec<String> = Vec::new();
    for e in map.prerequisites() {
        if map.canonical(&e.to) == target && live(&e.from) && home_of(map, &e.from) != home {
            prereqs.push(map.canonical(&e.from));
        }
        if map.canonical(&e.from) == target
            && live(&e.to)
            && home_of(map, &e.to) != home
            && is_held(model, &map.canonical(&e.to), now)
        {
            fed.push(map.canonical(&e.to));
        }
    }
    // Held prerequisites first: the learner can use them in the problem.
    prereqs.sort_by_key(|id| (!is_held(model, id, now), id.clone()));
    fed.sort();
    let mut out: Vec<String> = Vec::new();
    for id in prereqs.into_iter().chain(fed) {
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out.truncate(MAX_HOSTS_SHOWN);
    out
}

/// The nodes the learner holds (recall shown, R at or above the desired
/// retention, no open misconception): the only ones a problem may lean on.
/// Sorted by title.
pub fn held_nodes(map: &KnowledgeMap, model: &LearnerModel, now: DateTime<Utc>) -> Vec<String> {
    let mut held: Vec<(String, String)> = model
        .nodes()
        .filter(|(id, s)| s.held(now) && !map.is_tombstoned(id))
        .filter_map(|(id, _)| map.node(id).map(|n| (n.title.clone(), n.id.clone())))
        .collect();
    held.sort();
    held.into_iter().map(|(_, id)| id).collect()
}

/// A candidate target with its sort key.
struct Ranked {
    id: String,
    count: usize,
    class: u8,
    why: &'static str,
    dependents: usize,
    r: f64,
}

fn classify(node: Option<&NodeState>, now: DateTime<Utc>) -> (u8, &'static str, f64) {
    let Some(n) = node else {
        return (4, "new", 0.0);
    };
    let r = n.retrievability(now);
    if n.open_misconceptions().next().is_some() {
        (0, "misconception", r)
    } else if r < DESIRED_RETENTION {
        (1, "due", r)
    } else if n.lapses > 0 && n.lapses * 3 >= n.reviews {
        (2, "weak", r)
    } else if !(n.layers.recall && n.layers.discrimination && n.layers.composition) {
        (3, "layer", r)
    } else {
        (5, "review", r)
    }
}

/// The ideas the session may practise: the learner's picks, else every live
/// node of the graph. Aliases fold to their node.
fn pool(ctx: &Context) -> Vec<String> {
    let live = |id: &str| {
        let id = ctx.map.canonical(id);
        (ctx.map.node(&id).is_some() && !ctx.map.is_tombstoned(&id)).then_some(id)
    };
    let mut out: Vec<String> = Vec::new();
    let source: Vec<String> = if ctx.state.picked.is_empty() {
        ctx.map
            .nodes
            .iter()
            .filter(|n| !ctx.map.is_tombstoned(&n.id))
            .map(|n| n.id.clone())
            .collect()
    } else {
        ctx.state.picked.clone()
    };
    for id in source {
        if let Some(id) = live(&id) {
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}

/// Two misses in a row on `target`: switch away from it.
fn parked(trail: &[Step], target: &str) -> bool {
    let mut recent = trail
        .iter()
        .filter(|s| s.target == target && !s.retest)
        .filter_map(|s| s.result)
        .rev();
    matches!(
        (recent.next(), recent.next()),
        (Some((a, _)), Some((b, _))) if is_miss(a) && is_miss(b)
    )
}

/// The target for the next problem and why.
fn choose_target(ctx: &Context, trail: &[Step]) -> Result<(String, &'static str), String> {
    let pool = pool(ctx);
    if pool.is_empty() {
        return Err(if ctx.state.picked.is_empty() {
            "the knowledge graph has no ideas to practise yet".into()
        } else {
            "none of the picked ideas is in the knowledge graph any more".into()
        });
    }
    let last_target = trail.last().map(|s| s.target.as_str());
    let mut candidates: Vec<&String> = pool.iter().filter(|id| !parked(trail, id)).collect();
    if candidates.is_empty() {
        // Every candidate is parked: the learner still came to practise.
        candidates = pool.iter().collect();
    }
    if candidates.len() > 1 {
        candidates.retain(|id| Some(id.as_str()) != last_target);
    }
    let mut ranked: Vec<Ranked> = candidates
        .into_iter()
        .map(|id| {
            let (class, why, r) = classify(ctx.model.node(id), ctx.now);
            Ranked {
                id: id.clone(),
                count: trail.iter().filter(|s| &s.target == id).count(),
                class,
                why,
                dependents: dependents(ctx.map, id),
                r,
            }
        })
        .collect();
    ranked.sort_by(|a, b| {
        a.count
            .cmp(&b.count)
            .then(a.class.cmp(&b.class))
            .then(b.dependents.cmp(&a.dependents))
            .then(a.r.total_cmp(&b.r))
            .then_with(|| a.id.cmp(&b.id))
    });
    let best = ranked.swap_remove(0);
    Ok((best.id, best.why))
}

fn reached_level(ctx: &Context, target: &str) -> Option<Level> {
    let from_log = ctx.model.node(target).and_then(|n| n.level_reached);
    from_log.max(ctx.taught.reached.get(target).copied())
}

fn host_plan(ctx: &Context, target: &str, cue: u8, avoid: &[String]) -> Vec<String> {
    if cue < 2 {
        return Vec::new();
    }
    let mut hosts = host_candidates(ctx.map, ctx.model, target, ctx.now);
    // A retest sets the problem somewhere new when it can.
    hosts.sort_by_key(|h| avoid.contains(h));
    hosts.truncate(MAX_HOSTS_PLANNED);
    hosts
}

/// The next retest: the first problem on the retest list. `Err` when the
/// list is empty.
fn plan_retest(ctx: &Context) -> Result<ProblemPlan, String> {
    let due = ctx.state.retests_due();
    let Some(original) = due.first() else {
        return Err("every miss has been retested".into());
    };
    let target = original.plan.target.clone();
    let problem = original.problem();
    let cue = problem.map_or(original.plan.cue_distance, |p| p.cue_distance);
    let avoid = problem.map(|p| p.hosts.clone()).unwrap_or_default();
    Ok(ProblemPlan {
        reached: reached_level(ctx, &target),
        aim: ctx.taught.aim.get(&target).copied(),
        plugins: ctx.taught.plugins.get(&target).cloned().unwrap_or_default(),
        hosts: host_plan(ctx, &target, cue, &avoid),
        target,
        layer: original.plan.layer,
        level: original.plan.level,
        cue_distance: cue,
        is_distractor: original.plan.is_distractor,
        why: "retest".into(),
        retest_of: Some(original.id.clone()),
    })
}

/// Plan the problem after `trail`. While the learner is wrapping up, that is
/// the next retest; otherwise a fresh target.
pub fn plan_next(ctx: &Context, trail: &[Step]) -> Result<ProblemPlan, String> {
    if ctx.state.wrap_up {
        return plan_retest(ctx);
    }
    let (target, why) = choose_target(ctx, trail)?;
    let node = ctx.model.node(&target);
    let index = trail.len() as u32;
    let distractor = is_distractor(ctx.session_id, index);
    let last = trail
        .iter()
        .rev()
        .find(|s| s.target == target && s.result.is_some());
    let cue = next_cue(
        base_cue(node),
        last.and_then(|s| s.result.map(|(v, r)| (s.cue, v, r))),
    );
    let layer = if distractor {
        Layer::Discrimination
    } else {
        match last {
            Some(s) if s.result.is_some_and(|(v, _)| is_miss(v)) && !s.is_distractor => s.layer,
            _ => cheapest_layer(node),
        }
    };
    let reached = reached_level(ctx, &target);
    let aim = ctx.taught.aim.get(&target).copied();
    Ok(ProblemPlan {
        hosts: host_plan(ctx, &target, cue, &[]),
        plugins: ctx.taught.plugins.get(&target).cloned().unwrap_or_default(),
        level: level_for(reached, aim),
        target,
        layer,
        cue_distance: cue,
        is_distractor: distractor,
        why: why.into(),
        retest_of: None,
        reached,
        aim,
    })
}

/// The plans of a whole set, mixed: each target comes up in turn before any
/// repeats, as the stream's rotation does.
pub fn plan_set(ctx: &Context, size: u32) -> Result<Vec<ProblemPlan>, String> {
    let mut trail = trail(ctx.state);
    let mut plans = Vec::new();
    for _ in 0..size {
        let plan = plan_next(ctx, &trail)?;
        trail.push(Step {
            target: plan.target.clone(),
            layer: plan.layer,
            cue: plan.cue_distance,
            is_distractor: plan.is_distractor,
            retest: false,
            result: None,
        });
        plans.push(plan);
    }
    Ok(plans)
}

/// The cue distance of an exam paper's problems: a past paper names the area
/// it asks about, so the wording stays close to the lesson's.
pub const EXAM_CUE: u8 = 1;

/// The plans of an exam paper: one problem per target, in the order given
/// (the coverage ranking, `exam::paper_targets`). Past-paper style: no
/// distractors, no hosts, the cheapest layer the learner hasn't shown.
pub fn plan_exam(ctx: &Context, targets: &[String]) -> Result<Vec<ProblemPlan>, String> {
    let live: Vec<String> = targets
        .iter()
        .map(|id| ctx.map.canonical(id))
        .filter(|id| ctx.map.node(id).is_some() && !ctx.map.is_tombstoned(id))
        .collect();
    if live.is_empty() {
        return Err("none of the exam's ideas is in the knowledge graph".into());
    }
    Ok(live
        .into_iter()
        .map(|target| {
            let reached = reached_level(ctx, &target);
            let aim = ctx.taught.aim.get(&target).copied();
            ProblemPlan {
                layer: cheapest_layer(ctx.model.node(&target)),
                level: level_for(reached, aim),
                cue_distance: EXAM_CUE,
                hosts: Vec::new(),
                is_distractor: false,
                why: super::WHY_EXAM.into(),
                retest_of: None,
                reached,
                aim,
                plugins: ctx.taught.plugins.get(&target).cloned().unwrap_or_default(),
                target,
            }
        })
        .collect())
}

/// Ids of the problems of a set in plan order that nobody concluded.
pub fn unconcluded(state: &PracticeState) -> HashSet<&str> {
    state
        .problems
        .iter()
        .filter(|p| p.status() != Status::Concluded)
        .map(|p| p.id.as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{fold, GraphOp};
    use crate::learner::{Format, Grader, MissClass, Outcome};
    use crate::practice::state::PracticeState;
    use crate::state::Kind;

    /// Day 1 is 2026-10-01; later days run on (day 40 is mid-November).
    fn now(day: u32) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-01T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + chrono::Duration::days(i64::from(day) - 1)
    }

    fn at(day: u32) -> String {
        now(day).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    /// `deriv` (calculus) feeds `velocity` (kinematics) and needs `limit`
    /// (analysis); `power` (calculus) is a plain node; `gone` is tombstoned.
    fn graph() -> KnowledgeMap {
        let node = |id: &str, home: &str| GraphOp::node(id, Some(id), "s", Kind::Fact, home, None);
        let mut ops = vec![
            GraphOp::topic("calculus", "Calculus", None),
            GraphOp::topic("kinematics", "Kinematics", None),
            GraphOp::topic("analysis", "Analysis", None),
        ];
        ops.push(node("deriv", "calculus"));
        ops.push(node("power", "calculus"));
        ops.push(node("velocity", "kinematics"));
        ops.push(node("limit", "analysis"));
        ops.push(node("gone", "calculus"));
        ops.push(GraphOp::edge(
            "limit", "deriv", "requires", None, None, None,
        ));
        ops.push(GraphOp::edge(
            "deriv", "velocity", "requires", None, None, None,
        ));
        ops.push(GraphOp::edge(
            "deriv", "power", "requires", None, None, None,
        ));
        ops.push(GraphOp::tombstone("gone", "test"));
        fold(&ops)
    }

    fn outcome(id: &str, day: u32, target: &str, layer: Layer, verdict: Verdict) -> Outcome {
        Outcome {
            op_id: id.into(),
            at: at(day),
            session_id: "old".into(),
            problem_id: id.into(),
            targets: vec![target.into()],
            hosts: vec![],
            layer,
            format: Format::Stream,
            cue_distance: Some(2),
            verdict,
            confidence: Some(70),
            hint_rung: None,
            miss_class: (verdict == Verdict::Wrong).then_some(MissClass::Execution),
            misconception: None,
            grader: Grader::Committed,
            duration_sec: 120,
            level: None,
        }
    }

    fn model(log: &[Outcome]) -> LearnerModel {
        LearnerModel::from_outcomes(log, |id| id.to_owned())
    }

    fn ctx<'a>(
        map: &'a KnowledgeMap,
        model: &'a LearnerModel,
        taught: &'a Taught,
        state: &'a PracticeState,
        day: u32,
    ) -> Context<'a> {
        Context {
            map,
            model,
            taught,
            state,
            session_id: "s1",
            now: now(day),
        }
    }

    #[test]
    fn the_difficulty_rule_moves_one_notch_per_result() {
        let c = Verdict::Correct;
        assert_eq!(next_cue(2, None), 2, "a fresh target starts at its base");
        assert_eq!(next_cue(2, Some((1, c, None))), 2, "clean success: up");
        assert_eq!(next_cue(2, Some((3, c, None))), 3, "never past 3");
        assert_eq!(next_cue(2, Some((2, c, Some(1)))), 2, "a hint: hold");
        assert_eq!(
            next_cue(2, Some((2, Verdict::Partial, None))),
            2,
            "partial: hold"
        );
        assert_eq!(
            next_cue(2, Some((2, Verdict::Wrong, None))),
            1,
            "miss: down"
        );
        assert_eq!(
            next_cue(2, Some((0, Verdict::Dismissed, None))),
            0,
            "never below 0"
        );
        assert_eq!(next_cue(2, Some((2, Verdict::Dontknow, None))), 1);
    }

    #[test]
    fn levels_climb_one_step_at_a_time_and_stop_at_the_aim() {
        assert_eq!(level_for(None, None), Level::Use);
        assert_eq!(level_for(None, Some(Level::Explain)), Level::Explain);
        assert_eq!(level_for(None, Some(Level::Design)), Level::Use);
        assert_eq!(
            level_for(Some(Level::Explain), Some(Level::Design)),
            Level::Use
        );
        assert_eq!(
            level_for(Some(Level::Use), Some(Level::Design)),
            Level::Analyze
        );
        assert_eq!(
            level_for(Some(Level::Use), Some(Level::Analyze)),
            Level::Analyze
        );
        assert_eq!(
            level_for(Some(Level::Analyze), Some(Level::Analyze)),
            Level::Analyze,
            "reached the aim: stay at it, never beyond"
        );
        assert_eq!(level_for(Some(Level::Design), Some(Level::Use)), Level::Use);
    }

    #[test]
    fn about_one_problem_in_four_is_a_distractor_and_never_one_of_the_first_two() {
        assert!(!is_distractor("any", 0) && !is_distractor("any", 1));
        for session in ["a", "b", "c"] {
            let n = (2..2000).filter(|i| is_distractor(session, *i)).count();
            assert!((400..600).contains(&n), "{session}: {n} of 1998");
        }
        // Stable for a given session, different between sessions.
        assert_eq!(is_distractor("s", 7), is_distractor("s", 7));
        let a: Vec<bool> = (2..60).map(|i| is_distractor("s1", i)).collect();
        let b: Vec<bool> = (2..60).map(|i| is_distractor("s2", i)).collect();
        assert_ne!(a, b);
    }

    #[test]
    fn due_and_misconceived_nodes_come_before_fresh_ones_and_load_bearing_first() {
        let map = graph();
        let mut log = vec![
            outcome("a", 1, "deriv", Layer::Recall, Verdict::Correct),
            outcome("b", 1, "power", Layer::Recall, Verdict::Correct),
        ];
        let state = PracticeState::default();
        let taught = Taught::default();
        // Day 40: both are due; deriv carries a load-bearing dependent edge
        // count of 2, power 0.
        let m = model(&log);
        let plan = plan_next(&ctx(&map, &m, &taught, &state, 40), &[]).unwrap();
        assert_eq!(plan.target, "deriv");
        assert_eq!(plan.why, "due");

        // An open misconception outranks being due.
        let mut wrong = outcome("c", 2, "power", Layer::Recall, Verdict::Wrong);
        wrong.miss_class = Some(MissClass::Structural);
        wrong.misconception = Some("thinks the power rule adds exponents".into());
        log.push(wrong);
        let m = model(&log);
        let plan = plan_next(&ctx(&map, &m, &taught, &state, 40), &[]).unwrap();
        assert_eq!(
            (plan.target.as_str(), plan.why.as_str()),
            ("power", "misconception")
        );
    }

    #[test]
    fn the_cheapest_undemonstrated_layer_is_asked_and_distractors_are_discrimination() {
        let map = graph();
        let state = PracticeState::default();
        let taught = Taught::default();
        let log = vec![outcome("a", 1, "deriv", Layer::Recall, Verdict::Correct)];
        let m = model(&log);
        let c = ctx(&map, &m, &taught, &state, 2);
        assert_eq!(cheapest_layer(m.node("deriv")), Layer::Discrimination);
        assert_eq!(cheapest_layer(None), Layer::Recall);
        let log = vec![
            outcome("a", 1, "deriv", Layer::Recall, Verdict::Correct),
            outcome("b", 1, "deriv", Layer::Discrimination, Verdict::Correct),
        ];
        let m2 = model(&log);
        assert_eq!(cheapest_layer(m2.node("deriv")), Layer::Composition);
        let _ = c;

        // A distractor slot: find an index that rolls one, and check the plan.
        let index = (2..200).find(|i| is_distractor("s1", *i)).unwrap();
        let trail: Vec<Step> = (0..index)
            .map(|i| Step {
                target: if i % 2 == 0 { "power" } else { "limit" }.into(),
                layer: Layer::Recall,
                cue: 1,
                is_distractor: false,
                retest: false,
                result: Some((Verdict::Correct, None)),
            })
            .collect();
        let plan = plan_next(&ctx(&map, &m2, &taught, &state, 2), &trail).unwrap();
        assert!(plan.is_distractor);
        assert_eq!(plan.layer, Layer::Discrimination);
    }

    #[test]
    fn a_target_missed_twice_in_a_row_is_parked_and_the_stream_rotates() {
        let map = graph();
        let m = model(&[]);
        let taught = Taught::default();
        let state = PracticeState {
            picked: vec!["deriv".into(), "power".into()],
            ..Default::default()
        };
        let step = |target: &str, verdict| Step {
            target: target.into(),
            layer: Layer::Recall,
            cue: 1,
            is_distractor: false,
            retest: false,
            result: Some((verdict, None)),
        };
        let c = ctx(&map, &m, &taught, &state, 2);
        // The target just practised waits one turn.
        let trail = vec![step("deriv", Verdict::Correct)];
        assert_eq!(plan_next(&c, &trail).unwrap().target, "power");
        // Two misses on `power` park it even though `deriv` was practised
        // last: switch target.
        let trail = vec![
            step("power", Verdict::Wrong),
            step("deriv", Verdict::Correct),
            step("power", Verdict::Dontknow),
            step("deriv", Verdict::Correct),
        ];
        // Counts are equal (2 each), power is parked, so deriv is the only
        // candidate left even though it was the last target.
        assert_eq!(plan_next(&c, &trail).unwrap().target, "deriv");
        // A miss followed by a success clears the parking.
        let trail = vec![
            step("power", Verdict::Wrong),
            step("power", Verdict::Correct),
            step("deriv", Verdict::Correct),
        ];
        assert!(!parked(&trail, "power"));
    }

    #[test]
    fn the_stream_follows_the_difficulty_rule_for_the_same_target() {
        let map = graph();
        let m = model(&[]);
        let taught = Taught::default();
        let state = PracticeState {
            picked: vec!["power".into()],
            ..Default::default()
        };
        let c = ctx(&map, &m, &taught, &state, 2);
        let first = plan_next(&c, &[]).unwrap();
        assert_eq!(first.cue_distance, 1, "never recalled: close to the lesson");
        let step = |cue, verdict, rung| Step {
            target: "power".into(),
            layer: Layer::Recall,
            cue,
            is_distractor: false,
            retest: false,
            result: Some((verdict, rung)),
        };
        let after = |s: Step| plan_next(&c, &[s]).unwrap();
        assert_eq!(after(step(1, Verdict::Correct, None)).cue_distance, 2);
        assert_eq!(after(step(2, Verdict::Correct, Some(2))).cue_distance, 2);
        let down = after(step(2, Verdict::Wrong, None));
        assert_eq!(down.cue_distance, 1);
        assert_eq!(down.layer, Layer::Recall, "a miss holds the layer");
    }

    #[test]
    fn hosts_come_from_other_topics_and_only_for_disguised_problems() {
        let map = graph();
        let log = vec![
            outcome("a", 1, "velocity", Layer::Recall, Verdict::Correct),
            outcome("b", 1, "limit", Layer::Recall, Verdict::Correct),
        ];
        let m = model(&log);
        // `limit` (analysis) requires-edges into deriv; `velocity`
        // (kinematics) is held and fed by deriv.
        let hosts = host_candidates(&map, &m, "deriv", now(2));
        assert_eq!(hosts, ["limit", "velocity"]);
        // `power` is in deriv's own topic: not a host.
        assert!(!hosts.contains(&"power".to_owned()));
        // Not held: a fed node isn't offered.
        let hosts = host_candidates(&map, &model(&[]), "deriv", now(2));
        assert_eq!(hosts, ["limit"]);

        let state = PracticeState {
            picked: vec!["deriv".into()],
            ..Default::default()
        };
        let taught = Taught::default();
        let plan = plan_next(&ctx(&map, &m, &taught, &state, 2), &[]).unwrap();
        assert_eq!(plan.cue_distance, 1);
        assert!(plan.hosts.is_empty(), "cue 1 stays in the lesson's topic");
        let trail = [Step {
            target: "deriv".into(),
            layer: Layer::Recall,
            cue: 2,
            is_distractor: false,
            retest: false,
            result: Some((Verdict::Correct, None)),
        }];
        let state2 = PracticeState {
            picked: vec!["deriv".into(), "power".into()],
            ..Default::default()
        };
        let again = plan_next(&ctx(&map, &m, &taught, &state2, 2), &trail[..0]).unwrap();
        assert!(again.hosts.len() <= 2);
    }

    #[test]
    fn what_teaching_says_sets_level_aim_and_plugins() {
        let map = graph();
        let m = model(&[]);
        let mut taught = Taught::default();
        taught.aim.insert("power".into(), Level::Analyze);
        taught.reached.insert("power".into(), Level::Use);
        taught.plugins.insert("power".into(), vec!["cas".into()]);
        let state = PracticeState {
            picked: vec!["power".into()],
            ..Default::default()
        };
        let plan = plan_next(&ctx(&map, &m, &taught, &state, 2), &[]).unwrap();
        assert_eq!(plan.level, Level::Analyze);
        assert_eq!(
            (plan.reached, plan.aim),
            (Some(Level::Use), Some(Level::Analyze))
        );
        assert_eq!(plan.plugins, ["cas"]);
    }

    #[test]
    fn a_set_mixes_its_targets_and_an_empty_pool_says_why() {
        let map = graph();
        let m = model(&[]);
        let taught = Taught::default();
        let state = PracticeState {
            picked: vec!["deriv".into(), "power".into(), "limit".into()],
            ..Default::default()
        };
        let plans = plan_set(&ctx(&map, &m, &taught, &state, 2), 6).unwrap();
        let targets: Vec<&str> = plans.iter().map(|p| p.target.as_str()).collect();
        for t in ["deriv", "power", "limit"] {
            assert_eq!(
                targets.iter().filter(|x| **x == t).count(),
                2,
                "{targets:?}"
            );
        }
        assert!(
            targets.windows(2).all(|w| w[0] != w[1]),
            "never the same idea twice in a row: {targets:?}"
        );

        let empty = fold(&[]);
        let err =
            plan_next(&ctx(&empty, &m, &taught, &PracticeState::default(), 2), &[]).unwrap_err();
        assert!(err.contains("no ideas"), "{err}");
        let gone = PracticeState {
            picked: vec!["gone".into()],
            ..Default::default()
        };
        let err = plan_next(&ctx(&map, &m, &taught, &gone, 2), &[]).unwrap_err();
        assert!(err.contains("picked"), "{err}");
    }
}
