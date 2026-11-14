//! `problem_commit` validation: every refusal the coach can fix, all at once.
//!
//! The arguments are untrusted model output. A commit must match what the
//! app asked for (`next:` — target, layer, level, distractor flag, no harder
//! than the cue distance planned), name real graph nodes, carry a worked
//! solution with its decision points, and move only through ideas the
//! learner holds (or the statement gives).

use std::collections::HashSet;

use serde_json::Value;

use crate::knowledge::KnowledgeMap;
use crate::learner::Layer;
use crate::plugins::native::cas;
use crate::state::Level;

use super::{CheckScript, Criterion, Move, Problem, ProblemPlan, ProblemType, MAX_CUE};

/// Everything validation compares a commit with.
pub struct Expect<'a> {
    pub plan: &'a ProblemPlan,
    pub map: &'a KnowledgeMap,
    /// The nodes the learner holds.
    pub held: &'a HashSet<String>,
    /// For a retest: the surface of the problem it retests.
    pub original_surface: Option<&'a str>,
}

fn live(map: &KnowledgeMap, id: &str) -> Option<String> {
    let id = map.canonical(id);
    (map.node(&id).is_some() && !map.is_tombstoned(&id)).then_some(id)
}

fn text(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Node ids from an array argument, folded to their canonical node. A name
/// that is not a live graph node is a problem, listed once.
fn node_list(
    args: &Value,
    key: &str,
    map: &KnowledgeMap,
    problems: &mut Vec<String>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let Some(items) = args.get(key) else {
        return out;
    };
    let Some(items) = items.as_array() else {
        problems.push(format!("`{key}` must be an array of graph node ids"));
        return out;
    };
    for item in items {
        match item.as_str().map(str::trim) {
            Some(id) => match live(map, id) {
                Some(id) => {
                    if !out.contains(&id) {
                        out.push(id);
                    }
                }
                None => problems.push(format!(
                    "`{key}`: `{id}` is not a node of the knowledge graph (use graph_search for ids)"
                )),
            },
            None => problems.push(format!("`{key}` must hold strings")),
        }
    }
    out
}

/// Validate a `problem_commit` call. `Ok` is the problem as the app keeps it;
/// `Err` lists every problem found.
pub fn validate(args: &Value, expect: &Expect) -> Result<Problem, Vec<String>> {
    let plan = expect.plan;
    let map = expect.map;
    let mut problems: Vec<String> = Vec::new();

    let statement = text(args, "statement");
    if statement.is_none() {
        problems.push("`statement` is required".into());
    }

    let kind = match args.get("type") {
        None | Some(Value::Null) => ProblemType::Standard,
        Some(v) => match v.as_str() {
            Some("standard") => ProblemType::Standard,
            Some("critique") => ProblemType::Critique,
            Some("design") => ProblemType::Design,
            _ => {
                problems.push("`type` is standard, critique or design".into());
                ProblemType::Standard
            }
        },
    };

    let plan_target = map.canonical(&plan.target);
    let targets = node_list(args, "targets", map, &mut problems);
    if targets.is_empty() {
        problems.push("`targets` needs at least one graph node id".into());
    } else if !targets.contains(&plan_target) {
        problems.push(format!(
            "`targets` must include `{plan_target}`, the node the app asked for"
        ));
    }
    let hosts = node_list(args, "hosts", map, &mut problems);

    let layer = match args.get("layer").and_then(Value::as_str) {
        Some("recall") => Some(Layer::Recall),
        Some("discrimination") => Some(Layer::Discrimination),
        Some("composition") => Some(Layer::Composition),
        _ => None,
    };
    match layer {
        None => problems.push("`layer` is recall, discrimination or composition".into()),
        Some(l) if l != plan.layer => problems.push(format!(
            "`layer` must be `{}`, as the app asked",
            layer_name(plan.layer)
        )),
        Some(_) => {}
    }
    match args
        .get("level")
        .and_then(Value::as_str)
        .and_then(Level::parse)
    {
        None => problems.push("`level` is explain, use, analyze or design".into()),
        Some(l) if l != plan.level => problems.push(format!(
            "`level` must be `{}`: write a problem that demands exactly that",
            plan.level.as_str()
        )),
        Some(_) => {}
    }
    let cue = match args.get("cueDistance").and_then(Value::as_u64) {
        Some(c) if c <= u64::from(MAX_CUE) => {
            let c = c as u8;
            if c > plan.cue_distance {
                problems.push(format!(
                    "`cueDistance` {c} is further from the lesson than the {} the app asked for; write it at {} or closer, or commit the honest (lower) distance",
                    plan.cue_distance, plan.cue_distance
                ));
            }
            c
        }
        _ => {
            problems.push("`cueDistance` is an integer from 0 to 3".into());
            plan.cue_distance
        }
    };
    match args.get("isDistractor").and_then(Value::as_bool) {
        None => problems.push("`isDistractor` (true or false) is required".into()),
        Some(d) if d != plan.is_distractor => problems.push(if plan.is_distractor {
            "this one is a distractor: it must not need the target, only look as if it might (`isDistractor: true`)".into()
        } else {
            "`isDistractor` must be false: the problem has to need the target".into()
        }),
        Some(_) => {}
    }

    let surface = text(args, "surface");
    match (&surface, expect.original_surface) {
        (None, _) => {
            problems.push("`surface` (the world the problem is set in) is required".into())
        }
        (Some(s), Some(orig)) if s.eq_ignore_ascii_case(orig.trim()) => problems.push(format!(
            "this is a retest: `surface` must differ from `{orig}`, the original's"
        )),
        _ => {}
    }

    let allowed = allowed_nodes(expect, &targets, &hosts);
    let solution = solution(args, map, &allowed, &mut problems);

    let final_answer = text(args, "finalAnswer").unwrap_or_default();
    let variants: Vec<String> = match args.get("acceptableVariants") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
        Some(_) => {
            problems.push("`acceptableVariants` must be an array of strings".into());
            Vec::new()
        }
    };
    let rubric = rubric(args, &mut problems);
    if kind == ProblemType::Design && !rubric.iter().any(|c| c.essential) {
        problems.push(
            "a design problem needs a `rubric` with at least one essential criterion (the constraints)"
                .into(),
        );
    }
    if final_answer.is_empty() && rubric.is_empty() {
        problems.push(
            "`finalAnswer` is required; an open answer with no single value needs a `rubric` instead"
                .into(),
        );
    }
    let check = check_script(args, &mut problems);
    // A paper's problems carry marks and a mark scheme (the rubric).
    let marks = if plan.why == super::WHY_EXAM {
        if rubric.is_empty() {
            problems.push(
                "an exam problem needs its mark scheme as the `rubric`: one criterion per mark-bearing step (the library's mark scheme when it has one for this kind of question)"
                    .into(),
            );
        }
        let marks = args
            .get("marks")
            .and_then(Value::as_u64)
            .and_then(|m| u32::try_from(m).ok())
            .filter(|m| super::MARKS.contains(m));
        if marks.is_none() {
            problems.push(format!(
                "`marks` (what the problem is worth) is an integer from {} to {}",
                super::MARKS.start(),
                super::MARKS.end()
            ));
        }
        marks
    } else {
        None
    };

    if !problems.is_empty() {
        return Err(problems);
    }
    Ok(Problem {
        statement: statement.unwrap_or_default(),
        targets,
        hosts,
        layer: plan.layer,
        level: plan.level,
        cue_distance: cue,
        is_distractor: plan.is_distractor,
        kind,
        solution,
        final_answer,
        acceptable_variants: variants,
        rubric,
        check,
        surface: surface.unwrap_or_default(),
        marks,
    })
}

fn layer_name(l: Layer) -> &'static str {
    match l {
        Layer::Recall => "recall",
        Layer::Discrimination => "discrimination",
        Layer::Composition => "composition",
    }
}

/// The ideas a solution move may use: the learner's held nodes, the targets
/// and hosts, and the prerequisites of the targets (the learner needed them
/// to learn the target at all).
fn allowed_nodes(expect: &Expect, targets: &[String], hosts: &[String]) -> HashSet<String> {
    let mut allowed: HashSet<String> = expect.held.clone();
    allowed.extend(targets.iter().cloned());
    allowed.extend(hosts.iter().cloned());
    for e in expect.map.prerequisites() {
        if targets.contains(&expect.map.canonical(&e.to)) {
            allowed.insert(expect.map.canonical(&e.from));
        }
    }
    allowed
}

fn solution(
    args: &Value,
    map: &KnowledgeMap,
    allowed: &HashSet<String>,
    problems: &mut Vec<String>,
) -> Vec<Move> {
    let Some(items) = args.get("solution").and_then(Value::as_array) else {
        problems.push(
            "`solution` is required: an array of moves `{ move, usesNode?, decisionPoint }`".into(),
        );
        return Vec::new();
    };
    if items.is_empty() {
        problems.push("`solution` needs at least one move".into());
        return Vec::new();
    }
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let n = i + 1;
        let Some(text) = text(item, "move") else {
            problems.push(format!("solution move {n} needs a `move` text"));
            continue;
        };
        let Some(decision_point) = item.get("decisionPoint").and_then(Value::as_bool) else {
            problems.push(format!(
                "solution move {n} needs `decisionPoint` (true where a new idea has to come to mind)"
            ));
            continue;
        };
        let uses_node = match text_of(item, "usesNode") {
            None => None,
            Some(id) => match live(map, &id) {
                None => {
                    problems.push(format!(
                        "solution move {n}: `usesNode` `{id}` is not a node of the knowledge graph"
                    ));
                    None
                }
                Some(id) if !allowed.contains(&id) => {
                    problems.push(format!(
                        "solution move {n} uses `{id}`, which the learner doesn't hold (it is not among the held nodes, the targets, the hosts or the targets' prerequisites): give the fact in the statement and drop `usesNode`, or change the move"
                    ));
                    None
                }
                Some(id) => Some(id),
            },
        };
        out.push(Move {
            text,
            uses_node,
            decision_point,
        });
    }
    if !out.is_empty() && !out.iter().any(|m| m.decision_point) {
        problems.push("mark at least one solution move as a `decisionPoint`".into());
    }
    out
}

fn text_of(v: &Value, key: &str) -> Option<String> {
    text(v, key)
}

fn rubric(args: &Value, problems: &mut Vec<String>) -> Vec<Criterion> {
    let Some(items) = args.get("rubric") else {
        return Vec::new();
    };
    if items.is_null() {
        return Vec::new();
    }
    let Some(items) = items.as_array() else {
        problems.push("`rubric` must be an array of `{ id, criterion, essential }`".into());
        return Vec::new();
    };
    let mut out: Vec<Criterion> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let n = i + 1;
        let (Some(id), Some(criterion), Some(essential)) = (
            text(item, "id"),
            text(item, "criterion"),
            item.get("essential").and_then(Value::as_bool),
        ) else {
            problems.push(format!(
                "rubric criterion {n} needs `id`, `criterion` and `essential` (true or false)"
            ));
            continue;
        };
        if out.iter().any(|c| c.id == id) {
            problems.push(format!("rubric id `{id}` is used twice"));
            continue;
        }
        out.push(Criterion {
            id,
            criterion,
            essential,
        });
    }
    out
}

fn check_script(args: &Value, problems: &mut Vec<String>) -> Option<CheckScript> {
    let check = args.get("check")?;
    if check.is_null() {
        return None;
    }
    let language = text(check, "language").unwrap_or_default();
    let code = check.get("code").and_then(Value::as_str).unwrap_or("");
    if language != "python" {
        problems.push("`check.language` must be `python`".into());
        return None;
    }
    if code.trim().is_empty() {
        problems.push("`check.code` is empty: write the script or leave `check` out".into());
        return None;
    }
    if code.len() > cas::MAX_CODE {
        problems.push(format!(
            "`check.code` is {} bytes (at most {})",
            code.len(),
            cas::MAX_CODE
        ));
        return None;
    }
    Some(CheckScript {
        language,
        code: code.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{fold, GraphOp};
    use crate::state::Kind;
    use serde_json::json;

    fn map() -> KnowledgeMap {
        let node = |id: &str, home: &str| GraphOp::node(id, Some(id), "s", Kind::Fact, home, None);
        fold(&[
            GraphOp::topic("calculus", "Calculus", None),
            GraphOp::topic("kin", "Kinematics", None),
            node("deriv", "calculus"),
            node("velocity", "kin"),
            node("limit", "calculus"),
            node("stranger", "kin"),
            GraphOp::edge("limit", "deriv", "requires", None, None, None),
        ])
    }

    fn plan() -> ProblemPlan {
        ProblemPlan {
            target: "deriv".into(),
            layer: Layer::Recall,
            level: Level::Use,
            cue_distance: 2,
            hosts: vec!["velocity".into()],
            is_distractor: false,
            why: "due".into(),
            retest_of: None,
            reached: None,
            aim: None,
            plugins: vec![],
        }
    }

    fn good() -> Value {
        json!({
            "statement": "A ball's height is h(t)=20t-4.9t^2 m. When is it highest?",
            "targets": ["deriv"], "hosts": ["velocity"],
            "layer": "recall", "level": "use", "cueDistance": 2, "isDistractor": false,
            "solution": [
                { "move": "Write the height as a function of t", "usesNode": "velocity", "decisionPoint": false },
                { "move": "The maximum is where the slope is zero", "usesNode": "deriv", "decisionPoint": true },
                { "move": "h'(t)=20-9.8t=0, t≈2.04 s", "usesNode": "limit", "decisionPoint": false }
            ],
            "finalAnswer": "t ≈ 2.04 s", "acceptableVariants": ["2.0 s"],
            "surface": "projectile"
        })
    }

    fn run(args: &Value, held: &[&str]) -> Result<Problem, Vec<String>> {
        let held: HashSet<String> = held.iter().map(|s| (*s).to_owned()).collect();
        let map = map();
        let plan = plan();
        validate(
            args,
            &Expect {
                plan: &plan,
                map: &map,
                held: &held,
                original_surface: None,
            },
        )
    }

    fn with(mut v: Value, key: &str, value: Value) -> Value {
        v[key] = value;
        v
    }

    #[test]
    fn a_complete_commit_is_accepted_as_the_problem_the_app_keeps() {
        let p = run(&good(), &["velocity"]).unwrap();
        assert_eq!(p.targets, ["deriv"]);
        assert_eq!(p.solution.len(), 3);
        assert_eq!(p.cue_distance, 2);
        assert_eq!(p.kind, ProblemType::Standard);
    }

    #[test]
    fn an_exam_problem_needs_marks_and_its_mark_scheme() {
        let held: HashSet<String> = ["velocity".to_owned()].into();
        let map = map();
        let mut plan = plan();
        plan.why = crate::practice::WHY_EXAM.into();
        let run = |args: &Value| {
            validate(
                args,
                &Expect {
                    plan: &plan,
                    map: &map,
                    held: &held,
                    original_surface: None,
                },
            )
        };
        let rubric =
            json!([{ "id": "m1", "criterion": "Sets the slope to zero", "essential": true }]);
        let all = run(&good()).unwrap_err().join("\n");
        assert!(all.contains("mark scheme as the `rubric`"), "{all}");
        assert!(
            all.contains("`marks` (what the problem is worth) is an integer from 1 to 40"),
            "{all}"
        );
        let no_marks = run(&with(good(), "rubric", rubric.clone()))
            .unwrap_err()
            .join("\n");
        assert!(!no_marks.contains("rubric"), "{no_marks}");
        for bad in [json!(0), json!(41), json!("5"), json!(2.5)] {
            let args = with(with(good(), "rubric", rubric.clone()), "marks", bad.clone());
            assert!(run(&args).is_err(), "{bad} accepted");
        }
        let ok = run(&with(with(good(), "rubric", rubric), "marks", json!(6))).unwrap();
        assert_eq!(ok.marks, Some(6));
        // Outside a paper, marks are not part of the problem.
        assert_eq!(
            self::run(&with(good(), "marks", json!(6)), &["velocity"])
                .unwrap()
                .marks,
            None
        );
    }

    #[test]
    fn every_problem_with_a_commit_is_listed_together() {
        let args = json!({
            "statement": " ", "targets": ["nope"], "layer": "discrimination", "level": "design",
            "cueDistance": 3, "isDistractor": true, "solution": [], "surface": ""
        });
        let problems = run(&args, &[]).unwrap_err();
        let all = problems.join("\n");
        for needle in [
            "`statement` is required",
            "`nope` is not a node of the knowledge graph",
            "`layer` must be `recall`",
            "`level` must be `use`",
            "cueDistance` 3 is further from the lesson than the 2",
            "`isDistractor` must be false",
        ] {
            assert!(all.contains(needle), "missing {needle:?} in\n{all}");
        }
        assert!(all.contains("`solution` needs at least one move"));
        assert!(all.contains("`surface`"));
    }

    #[test]
    fn the_target_the_app_asked_for_must_be_among_the_targets() {
        let p = run(&with(good(), "targets", json!(["velocity"])), &["velocity"]).unwrap_err();
        assert!(
            p.iter().any(|m| m.contains("must include `deriv`")),
            "{p:?}"
        );
    }

    #[test]
    fn moves_may_only_use_ideas_the_learner_holds() {
        // `stranger` is neither held, a target, a host nor a prerequisite.
        let mut args = good();
        args["solution"][0]["usesNode"] = json!("stranger");
        let p = run(&args, &["velocity"]).unwrap_err();
        assert!(
            p.iter()
                .any(|m| m.contains("solution move 1 uses `stranger`")),
            "{p:?}"
        );
        // Held, it is fine; the target's prerequisite (`limit`) always is.
        assert!(run(&args, &["velocity", "stranger"]).is_ok());
    }

    #[test]
    fn a_solution_needs_a_decision_point_and_each_move_says_whether_it_is_one() {
        let mut args = good();
        for m in args["solution"].as_array_mut().unwrap() {
            m["decisionPoint"] = json!(false);
        }
        let p = run(&args, &["velocity"]).unwrap_err();
        assert!(
            p.iter().any(|m| m.contains("at least one solution move")),
            "{p:?}"
        );
        let mut args = good();
        args["solution"][1]
            .as_object_mut()
            .unwrap()
            .remove("decisionPoint");
        let p = run(&args, &["velocity"]).unwrap_err();
        assert!(
            p.iter().any(|m| m.contains("move 2 needs `decisionPoint`")),
            "{p:?}"
        );
    }

    #[test]
    fn design_problems_and_open_answers_need_a_rubric() {
        let design = with(good(), "type", json!("design"));
        let p = run(&design, &["velocity"]).unwrap_err();
        assert!(p.iter().any(|m| m.contains("essential criterion")), "{p:?}");
        let rubric = json!([
            { "id": "c1", "criterion": "stays within 5%", "essential": true },
            { "id": "c2", "criterion": "uses E12 values", "essential": false }
        ]);
        let ok = run(&with(design, "rubric", rubric.clone()), &["velocity"]).unwrap();
        assert_eq!(ok.rubric.len(), 2);

        // No single final value: the rubric stands in for `finalAnswer`.
        let open = with(with(good(), "finalAnswer", json!("")), "rubric", rubric);
        assert!(run(&open, &["velocity"]).is_ok());
        let none = with(good(), "finalAnswer", json!(""));
        assert!(run(&none, &["velocity"])
            .unwrap_err()
            .iter()
            .any(|m| m.contains("`finalAnswer` is required")));
        let dup = with(
            good(),
            "rubric",
            json!([{ "id": "a", "criterion": "x", "essential": true },
                   { "id": "a", "criterion": "y", "essential": false }]),
        );
        assert!(run(&dup, &["velocity"])
            .unwrap_err()
            .iter()
            .any(|m| m.contains("used twice")));
    }

    #[test]
    fn a_check_script_must_be_python_with_code() {
        let ok = with(
            good(),
            "check",
            json!({ "language": "python", "code": "print(2.04)" }),
        );
        assert!(run(&ok, &["velocity"]).unwrap().check.is_some());
        let bad = with(good(), "check", json!({ "language": "ruby", "code": "x" }));
        assert!(run(&bad, &["velocity"])
            .unwrap_err()
            .iter()
            .any(|m| m.contains("`python`")));
        let empty = with(
            good(),
            "check",
            json!({ "language": "python", "code": " " }),
        );
        assert!(run(&empty, &["velocity"])
            .unwrap_err()
            .iter()
            .any(|m| m.contains("`check.code` is empty")));
    }

    #[test]
    fn a_retest_must_use_a_new_surface_and_a_lower_cue_distance_is_fine() {
        let map = map();
        let plan = plan();
        let held: HashSet<String> = ["velocity".to_owned()].into();
        let expect = Expect {
            plan: &plan,
            map: &map,
            held: &held,
            original_surface: Some("Projectile"),
        };
        let p = validate(&good(), &expect).unwrap_err();
        assert!(p.iter().any(|m| m.contains("retest")), "{p:?}");
        let lower = with(
            with(good(), "surface", json!("a flywheel")),
            "cueDistance",
            json!(1),
        );
        assert_eq!(validate(&lower, &expect).unwrap().cue_distance, 1);
    }
}
