//! Tool schemas, scoped by agent and phase (prompts/README.md "Tool
//! contracts"; names and fields match it exactly), and the code-side plan
//! checks (build prompt M4 `plan_write`, M5 gates).
//!
//! A tool that isn't available in a phase is never described to the model;
//! the executor re-checks every gate anyway, because model output is
//! untrusted input.

use std::collections::{HashMap, HashSet};

use providers::ToolSpec;
use serde_json::{json, Value};

use crate::prompts::PluginManifest;
use crate::state::{Phase, Plan, SessionState};

/// Most plugins one section may have (architecture §4.20).
pub const MAX_PLUGINS_PER_SECTION: usize = 3;

fn tool(name: &str, description: &str, props: Value, required: &[&str]) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.into(),
        input_schema: json!({ "type": "object", "properties": props, "required": required }),
    }
}

fn strings() -> Value {
    json!({ "type": "array", "items": { "type": "string" } })
}

fn choice_ask() -> ToolSpec {
    tool(
        "choice_ask",
        "Ask the learner to choose at a genuine fork with no correct answer. The learner may answer in their own words. Ends your turn.",
        json!({
            "question": { "type": "string" },
            "options": strings(),
            "recommended": { "type": "string", "description": "The option you recommend (must be one of options)." }
        }),
        &["question", "options"],
    )
}

fn question_ask(roles: &[&str], teach: bool) -> ToolSpec {
    let mut props = json!({
        "role": { "type": "string", "enum": roles },
        "format": { "type": "string", "enum": ["free", "choice"] },
        "prompt": { "type": "string" },
        "options": strings(),
        "answer": { "type": "string", "description": "Your committed answer; for choice format, the exact text of the correct option." },
        "explanation": { "type": "string" }
    });
    if teach {
        props["nodeId"] =
            json!({ "type": "string", "description": "A node in the current section." });
    } else {
        props["strand"] = json!({ "type": "string" });
    }
    let required: &[&str] = if teach {
        &["role", "nodeId", "format", "prompt", "answer"]
    } else {
        &["role", "format", "prompt", "answer"]
    };
    tool(
        "question_ask",
        "Ask one graded question. Ends your turn; the learner's answer, thinking and confidence come back as the result, with `questionId` for verdict_record.",
        props,
        required,
    )
}

fn verdict_record() -> ToolSpec {
    tool(
        "verdict_record",
        "Record your verdict on a free-recall answer, judged strictly against your committed answer.",
        json!({
            "questionId": { "type": "string" },
            "verdict": { "type": "string", "enum": ["correct", "partial", "wrong"] },
            "note": { "type": "string" }
        }),
        &["questionId", "verdict", "note"],
    )
}

fn research_props() -> Value {
    json!({
        "kind": { "type": "string", "enum": ["verify", "scope", "source"] },
        "question": { "type": "string" },
        "context": { "type": "string" },
        "path": { "type": "string", "description": "For kind source: the learner's document." }
    })
}

fn research_request() -> ToolSpec {
    tool(
        "research_request",
        "Start a background research job. Returns an acknowledgement; the brief arrives later as a message. Several can run at once.",
        research_props(),
        &["kind", "question"],
    )
}

fn research() -> ToolSpec {
    tool(
        "research",
        "Run research and wait for it: returns the researcher's brief as the result.",
        research_props(),
        &["kind", "question"],
    )
}

fn file_tools() -> [ToolSpec; 2] {
    [
        tool(
            "file_find",
            "Find the learner's files by name or glob (read-only, inside the home directory).",
            json!({ "pattern": { "type": "string", "description": "A file name fragment or glob such as `uni/**/*.md`." } }),
            &["pattern"],
        ),
        tool(
            "file_read",
            "Read a text file, or a line range such as `20-80` (read-only, inside the home directory, at most 256 KiB).",
            json!({ "path": { "type": "string" }, "lines": { "type": "string" } }),
            &["path"],
        ),
    ]
}

fn session_end() -> ToolSpec {
    tool("session_end", "Close the session.", json!({}), &[])
}

/// The tutor's tools for a phase. Plugin tools are added by the caller.
pub fn tutor_tools(phase: Phase) -> Vec<ToolSpec> {
    let mut t = vec![choice_ask(), research_request()];
    t.extend(file_tools());
    match phase {
        Phase::Measure => {
            t.push(tool(
                "goal_set",
                "Record the concrete goal. Starts the background scope research.",
                json!({
                    "goal": { "type": "string" },
                    "purpose": { "type": "string", "enum": ["curiosity", "project", "work", "exam"] },
                    "successLooksLike": strings(),
                    "constraints": { "type": "object", "properties": {
                        "deadline": { "type": "string" },
                        "minutesPerSitting": { "type": "number" },
                        "device": { "type": "string" }
                    } },
                    "exam": { "type": "object", "properties": {
                        "name": { "type": "string" },
                        "date": { "type": "string" },
                        "syllabus": { "type": "string" },
                        "pastPapers": { "type": "string" },
                        "weights": { "type": "string" }
                    } },
                    "materials": strings(),
                    "outOfScope": strings(),
                    "preferences": strings(),
                    "orientationFirst": { "type": "boolean" }
                }),
                &["goal", "purpose", "successLooksLike"],
            ));
            t.push(question_ask(&["probe"], false));
            t.push(verdict_record());
            t.push(tool(
                "probe_done",
                "Finish measuring. Refused while a question is open or before the scope brief has arrived. On success the planner builds the plan.",
                json!({
                    "strands": { "type": "array", "items": { "type": "object", "properties": {
                        "name": { "type": "string" },
                        "kind": { "type": "string" },
                        "floor": { "type": "string" },
                        "ceiling": { "type": "string" },
                        "misconceptions": strings(),
                        "buildFrom": { "type": "string" },
                        "confidence": { "type": "string", "enum": ["solid", "rough", "unprobed"] },
                        "evidence": strings()
                    } } },
                    "notes": { "type": "string" }
                }),
                &["strands", "notes"],
            ));
        }
        Phase::Teach => {
            t.push(question_ask(&["predict", "check"], true));
            t.push(verdict_record());
            t.push(tool(
                "plan_change_request",
                "Ask the planner to change the route, with evidence. Returns an acknowledgement; the planner's answer arrives as a message.",
                json!({
                    "reason": { "type": "string", "enum": ["learner", "missing-foundation", "ahead", "detour", "probe-wrong"] },
                    "evidence": { "type": "string" },
                    "learnerWords": { "type": "string" }
                }),
                &["reason", "evidence"],
            ));
            t.push(tool(
                "plugin_request",
                "Ask for a plugin in this section. Granted or refused by the app (unknown id, platform, three-per-section limit).",
                json!({ "plugin": { "type": "string" }, "reason": { "type": "string" } }),
                &["plugin", "reason"],
            ));
            t.push(tool(
                "section_done",
                "Close the current section. Refused while any node in it lacks a passing check.",
                json!({
                    "sectionId": { "type": "string" },
                    "evidence": { "type": "string" },
                    "carryOver": { "type": "string" }
                }),
                &["sectionId", "evidence", "carryOver"],
            ));
            t.push(session_end());
        }
        Phase::Close => t.push(session_end()),
        Phase::Plan | Phase::AwaitApproval => {}
    }
    t
}

pub fn planner_tools() -> Vec<ToolSpec> {
    let mut t = vec![
        tool(
            "plan_write",
            "Write or revise the plan. Checked by the app; problems come back as an error to fix.",
            json!({
                "goal": { "type": "string" },
                "approach": { "type": "string" },
                "nodes": { "type": "array", "items": { "type": "object", "properties": {
                    "id": { "type": "string" },
                    "title": { "type": "string" },
                    "kind": { "type": "string", "enum": ["mechanism", "fact", "procedure", "perceptual", "argument"] },
                    "requires": strings(),
                    "weight": { "type": "number", "description": "Exam mode: 0–1." },
                    "practiceOnly": { "type": "boolean", "description": "Exam mode." }
                }, "required": ["id", "title", "kind", "requires"] } },
                "sections": { "type": "array", "items": { "type": "object", "properties": {
                    "id": { "type": "string" },
                    "title": { "type": "string" },
                    "nodeIds": strings(),
                    "mode": { "type": "string", "enum": ["mechanism", "fact", "procedure", "perceptual", "argument"] },
                    "style": { "type": "string", "enum": ["socratic", "expository", "mixed"] },
                    "plugins": strings(),
                    "notes": { "type": "string" },
                    "exit": { "type": "string" },
                    "desktopOnly": { "type": "boolean" }
                }, "required": ["id", "title", "nodeIds", "mode", "style", "plugins", "notes", "exit"] } }
            }),
            &["goal", "approach", "nodes", "sections"],
        ),
        tool(
            "plan_keep",
            "Answer a change request without changing the plan.",
            json!({ "reason": { "type": "string" } }),
            &["reason"],
        ),
        research(),
    ];
    t.extend(file_tools());
    t
}

pub fn researcher_tools() -> Vec<ToolSpec> {
    vec![
        tool(
            "web_search",
            "Search the web. Returns numbered results with URLs and snippets.",
            json!({
                "query": { "type": "string" },
                "recency": { "type": "string", "enum": ["day", "week", "month", "year"] }
            }),
            &["query"],
        ),
        tool(
            "page_fetch",
            "Fetch a web page as text (HTML converted, truncated to about 40k characters).",
            json!({ "url": { "type": "string" } }),
            &["url"],
        ),
        tool(
            "source_outline",
            "Outline a learner document: its headings with line numbers.",
            json!({ "path": { "type": "string" } }),
            &["path"],
        ),
        tool(
            "source_search",
            "Search a learner document; returns matching lines with line numbers.",
            json!({ "path": { "type": "string" }, "query": { "type": "string" } }),
            &["path", "query"],
        ),
        tool(
            "source_read",
            "Read part of a learner document by line range, e.g. `120-180`.",
            json!({
                "path": { "type": "string" },
                "pages": { "type": "string" },
                "lines": { "type": "string" }
            }),
            &["path"],
        ),
        tool(
            "brief_submit",
            "Submit your brief. This is the job's single result and ends the job.",
            json!({
                "kind": { "type": "string", "enum": ["scope", "verify", "source"] },
                "summary": { "type": "string" },
                "question": { "type": "string" },
                "answer": { "type": "string" }
            }),
            &["kind"],
        ),
    ]
}

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

/// A required string argument, or the error text for the model.
pub fn req_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("missing or empty `{key}` (string)"))
}

pub fn opt_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

// ---------------------------------------------------------------------------
// Plan checks
// ---------------------------------------------------------------------------

/// Parse and check a `plan_write` payload. On failure, every specific problem
/// is listed so the planner can fix them all at once.
pub fn check_plan(
    args: &Value,
    state: &SessionState,
    manifests: &[PluginManifest],
) -> Result<Plan, Vec<String>> {
    let plan: Plan = serde_json::from_value(args.clone())
        .map_err(|e| vec![format!("plan_write arguments don't match the schema: {e}")])?;
    let problems = validate_plan(&plan, state, manifests);
    if problems.is_empty() {
        Ok(plan)
    } else {
        Err(problems)
    }
}

/// Every rule from the build prompt: unique slugs; `requires` names existing
/// nodes and is acyclic; each node in exactly one section; valid modes and
/// styles; plugins registered and available on the platform; at most three
/// per section; completed sections unchanged on revision.
pub fn validate_plan(
    plan: &Plan,
    state: &SessionState,
    manifests: &[PluginManifest],
) -> Vec<String> {
    let mut problems = Vec::new();
    if plan.goal.trim().is_empty() {
        problems.push("`goal` is empty".to_owned());
    }
    if plan.nodes.is_empty() {
        problems.push("the plan has no nodes".to_owned());
    }
    if plan.sections.is_empty() {
        problems.push("the plan has no sections".to_owned());
    }

    let mut ids: HashSet<&str> = HashSet::new();
    for n in &plan.nodes {
        if !is_slug(&n.id) {
            problems.push(format!("node id `{}` is not a lowercase slug", n.id));
        }
        if !ids.insert(n.id.as_str()) {
            problems.push(format!("duplicate node id `{}`", n.id));
        }
    }
    for n in &plan.nodes {
        for r in &n.requires {
            if !ids.contains(r.as_str()) {
                problems.push(format!("node `{}` requires unknown node `{r}`", n.id));
            }
        }
    }
    if let Some(cycle) = find_cycle(plan) {
        problems.push(format!("`requires` has a cycle: {}", cycle.join(" → ")));
    }

    let mut section_ids: HashSet<&str> = HashSet::new();
    let mut owner: HashMap<&str, usize> = HashMap::new();
    for s in &plan.sections {
        if !section_ids.insert(s.id.as_str()) {
            problems.push(format!("duplicate section id `{}`", s.id));
        }
        if s.node_ids.is_empty() {
            problems.push(format!("section `{}` has no nodes", s.id));
        }
        if !matches!(s.style.as_str(), "socratic" | "expository" | "mixed") {
            problems.push(format!(
                "section `{}` has style `{}` (must be socratic, expository or mixed)",
                s.id, s.style
            ));
        }
        for id in &s.node_ids {
            if !ids.contains(id.as_str()) {
                problems.push(format!("section `{}` lists unknown node `{id}`", s.id));
            }
            *owner.entry(id.as_str()).or_insert(0) += 1;
        }
        if s.plugins.len() > MAX_PLUGINS_PER_SECTION {
            problems.push(format!(
                "section `{}` has {} plugins (at most {MAX_PLUGINS_PER_SECTION})",
                s.id,
                s.plugins.len()
            ));
        }
        for p in &s.plugins {
            match manifests.iter().find(|m| &m.id == p) {
                None => problems.push(format!(
                    "section `{}` uses plugin `{p}`, which isn't in the catalog",
                    s.id
                )),
                Some(m) if !m.platforms.iter().any(|x| x == &state.platform) => problems.push(
                    format!("plugin `{p}` is not available on {}", state.platform),
                ),
                Some(_) => {}
            }
        }
    }
    for (id, count) in &owner {
        if *count > 1 {
            problems.push(format!(
                "node `{id}` is in {count} sections (must be exactly one)"
            ));
        }
    }
    for n in &plan.nodes {
        if !owner.contains_key(n.id.as_str()) {
            problems.push(format!("node `{}` is in no section", n.id));
        }
    }

    // On revision, sections already completed must be unchanged.
    if let Some(previous) = &state.plan {
        for done in &state.completed_sections {
            let before = previous.sections.iter().find(|s| &s.id == done);
            let after = plan.sections.iter().find(|s| &s.id == done);
            match (before, after) {
                (Some(b), Some(a)) if a != b => {
                    problems.push(format!("completed section `{done}` was changed"));
                }
                (Some(_), None) => problems.push(format!("completed section `{done}` was removed")),
                _ => {}
            }
        }
    }
    problems.sort();
    problems.dedup();
    problems
}

fn is_slug(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// A cycle in node `requires` edges, if any (DFS with colors).
fn find_cycle(plan: &Plan) -> Option<Vec<String>> {
    fn visit<'a>(
        id: &'a str,
        edges: &HashMap<&'a str, &'a [String]>,
        color: &mut HashMap<&'a str, u8>,
        stack: &mut Vec<&'a str>,
    ) -> Option<Vec<String>> {
        match color.get(id) {
            Some(1) => {
                let start = stack.iter().position(|s| *s == id).unwrap_or(0);
                let mut cycle: Vec<String> =
                    stack[start..].iter().map(|s| (*s).to_owned()).collect();
                cycle.push(id.to_owned());
                return Some(cycle);
            }
            Some(2) => return None,
            _ => {}
        }
        color.insert(id, 1);
        stack.push(id);
        for d in edges.get(id).copied().unwrap_or_default() {
            if let Some(c) = visit(d.as_str(), edges, color, stack) {
                return Some(c);
            }
        }
        stack.pop();
        color.insert(id, 2);
        None
    }
    let edges: HashMap<&str, &[String]> = plan
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n.requires.as_slice()))
        .collect();
    let mut color = HashMap::new();
    let mut stack = Vec::new();
    plan.nodes
        .iter()
        .find_map(|n| visit(n.id.as_str(), &edges, &mut color, &mut stack))
}

/// Whether the learner must approve this plan: always before the first
/// approval; afterwards when the goal changed or a section was removed.
pub fn needs_approval(new: &Plan, state: &SessionState) -> bool {
    let Some(previous) = &state.plan else {
        return true;
    };
    if state.approved_version.is_none() {
        return true;
    }
    new.goal.trim() != previous.goal.trim()
        || previous
            .sections
            .iter()
            .any(|s| !new.sections.iter().any(|x| x.id == s.id))
}

/// One line describing a revision, for the "Plan revised: …" notice.
pub fn revision_summary(new: &Plan, previous: Option<&Plan>) -> String {
    let Some(previous) = previous else {
        return format!("{} sections", new.sections.len());
    };
    let mut parts = Vec::new();
    let added: Vec<&str> = new
        .sections
        .iter()
        .filter(|s| !previous.sections.iter().any(|p| p.id == s.id))
        .map(|s| s.title.as_str())
        .collect();
    let removed: Vec<&str> = previous
        .sections
        .iter()
        .filter(|s| !new.sections.iter().any(|p| p.id == s.id))
        .map(|s| s.title.as_str())
        .collect();
    let changed: Vec<&str> = new
        .sections
        .iter()
        .filter(|s| previous.sections.iter().any(|p| p.id == s.id && p != *s))
        .map(|s| s.title.as_str())
        .collect();
    if !added.is_empty() {
        parts.push(format!("added {}", added.join(", ")));
    }
    if !removed.is_empty() {
        parts.push(format!("removed {}", removed.join(", ")));
    }
    if !changed.is_empty() {
        parts.push(format!("changed {}", changed.join(", ")));
    }
    if new.goal.trim() != previous.goal.trim() {
        parts.push("new goal".to_owned());
    }
    if parts.is_empty() {
        "notes and order adjusted".to_owned()
    } else {
        parts.join("; ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(id: &str, platforms: &[&str]) -> PluginManifest {
        PluginManifest {
            id: id.into(),
            title: id.into(),
            tools: vec![],
            platforms: platforms.iter().map(|p| (*p).to_owned()).collect(),
            phases: vec!["teach".into()],
            use_when: String::new(),
            avoid_when: String::new(),
            cost: String::new(),
        }
    }

    fn plan(v: Value) -> Plan {
        serde_json::from_value(v).unwrap()
    }

    fn base() -> Value {
        json!({
            "goal": "g", "approach": "a",
            "nodes": [
                {"id":"a","title":"A","kind":"mechanism","requires":[]},
                {"id":"b","title":"B","kind":"fact","requires":["a"]}
            ],
            "sections": [
                {"id":"s1","title":"S1","nodeIds":["a"],"mode":"mechanism","style":"socratic","plugins":[],"notes":"","exit":"x"},
                {"id":"s2","title":"S2","nodeIds":["b"],"mode":"fact","style":"mixed","plugins":[],"notes":"","exit":"y"}
            ]
        })
    }

    fn state() -> SessionState {
        SessionState {
            platform: "desktop".into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_valid_plan_passes() {
        assert!(validate_plan(&plan(base()), &state(), &[]).is_empty());
    }

    #[test]
    fn structural_problems_are_each_reported() {
        let mut v = base();
        v["nodes"][0]["requires"] = json!(["b"]); // cycle a <-> b
        v["nodes"][1]["requires"] = json!(["a", "ghost"]);
        v["sections"][1]["nodeIds"] = json!(["a", "b"]); // a in two sections
        v["sections"][0]["plugins"] = json!(["p1", "p2", "p3", "nope"]);
        v["sections"][0]["style"] = json!("lecture");
        let manifests = [
            manifest("p1", &["desktop"]),
            manifest("p2", &["desktop"]),
            manifest("p3", &["mobile"]),
        ];
        let problems = validate_plan(&plan(v), &state(), &manifests).join("\n");
        for needle in [
            "cycle",
            "unknown node `ghost`",
            "node `a` is in 2 sections",
            "4 plugins",
            "`nope`, which isn't in the catalog",
            "`p3` is not available on desktop",
            "style `lecture`",
        ] {
            assert!(
                problems.contains(needle),
                "missing {needle:?} in:\n{problems}"
            );
        }
    }

    #[test]
    fn an_invalid_mode_is_a_schema_error() {
        let mut v = base();
        v["sections"][0]["mode"] = json!("vibes");
        assert!(check_plan(&v, &state(), &[]).is_err());
    }

    #[test]
    fn revisions_keep_completed_sections_and_approval_rules_hold() {
        let first = plan(base());
        let mut s = state();
        assert!(
            needs_approval(&first, &s),
            "first plan always needs approval"
        );
        s.plan = Some(first.clone());
        s.approved_version = Some(1);
        s.completed_sections = vec!["s1".into()];

        // Adding a section is a minor change.
        let mut added = base();
        added["nodes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"c","title":"C","kind":"fact","requires":[]}));
        added["sections"].as_array_mut().unwrap().insert(1, json!({"id":"s0","title":"Fix","nodeIds":["c"],"mode":"fact","style":"mixed","plugins":[],"notes":"","exit":"z"}));
        let added = plan(added);
        assert!(validate_plan(&added, &s, &[]).is_empty());
        assert!(!needs_approval(&added, &s));
        assert!(revision_summary(&added, Some(&first)).contains("added Fix"));

        // Removing a section or changing the goal needs approval.
        let mut removed = base();
        removed["sections"] = json!([{"id":"s1","title":"S1","nodeIds":["a","b"],"mode":"mechanism","style":"socratic","plugins":[],"notes":"","exit":"x"}]);
        assert!(needs_approval(&plan(removed), &s));
        let mut goal = base();
        goal["goal"] = json!("other");
        assert!(needs_approval(&plan(goal), &s));

        // Changing a completed section is invalid.
        let mut changed = base();
        changed["sections"][0]["exit"] = json!("different");
        assert!(validate_plan(&plan(changed), &s, &[])
            .join(" ")
            .contains("completed section `s1` was changed"));
    }

    #[test]
    fn tutor_never_gets_plan_write_and_teach_tools_only_in_teach() {
        for phase in [
            Phase::Measure,
            Phase::Plan,
            Phase::AwaitApproval,
            Phase::Teach,
            Phase::Close,
        ] {
            let names: Vec<String> = tutor_tools(phase).into_iter().map(|t| t.name).collect();
            assert!(!names.iter().any(|n| n == "plan_write" || n == "plan_keep"));
            let teach_only = ["section_done", "plan_change_request", "plugin_request"];
            for t in teach_only {
                assert_eq!(
                    names.iter().any(|n| n == t),
                    phase == Phase::Teach,
                    "{t} in {phase:?}"
                );
            }
        }
        let measure = tutor_tools(Phase::Measure);
        let q = measure.iter().find(|t| t.name == "question_ask").unwrap();
        assert_eq!(
            q.input_schema["properties"]["role"]["enum"],
            json!(["probe"])
        );
    }
}
