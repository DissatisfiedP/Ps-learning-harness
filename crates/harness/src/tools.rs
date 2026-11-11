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

use crate::plugins::PluginManifest;
use crate::state::{Level, Phase, Plan, Section, SessionState, Track};

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
        "prompt": { "type": "string", "description": "The question alone: the task and the data it needs. Feedback on the last answer and any teaching go in your message text before this call, never here." },
        "options": strings(),
        "answer": { "type": "string", "description": "Your committed answer; for choice format, the exact text of the correct option." },
        "explanation": { "type": "string" },
        "level": { "type": "string", "enum": ["explain", "use", "analyze", "design"],
            "description": "How far the question demands the learner go: explain (state it and why), use (apply it to a new case), analyze (take it apart, diagnose, judge), design (build something new to constraints). Required: the app refuses a probe or check question without one. Write the question to this level." },
        "view": { "type": "object", "description": "Answer in an interactive view (W7). Only with a view whose plugin has answer = true.", "properties": {
            "artifactId": { "type": "string" },
            "task": { "type": "string", "description": "What the learner should do in the view." },
            "target": { "type": "object", "description": "Grading target, committed here and never shown to the learner." }
        }, "required": ["artifactId", "task", "target"] }
    });
    if teach {
        props["nodeId"] =
            json!({ "type": "string", "description": "A node in the current section." });
    } else {
        props["strand"] = json!({ "type": "string" });
    }
    let required: &[&str] = if teach {
        &["role", "nodeId", "level", "format", "prompt", "answer"]
    } else {
        &["role", "level", "format", "prompt", "answer"]
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
        "Record your verdict on a free-recall answer, judged strictly against your committed answer. The learner sees only the verdict; say what they need in your message text.",
        json!({
            "questionId": { "type": "string" },
            "verdict": { "type": "string", "enum": ["correct", "partial", "wrong"] },
            "note": { "type": "string", "description": "Your diagnosis for the record, never shown to the learner: what was present, what was missing, which wrong model shows. Read by the planner and your later turns." }
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
            "Find the learner's files by name or glob (read-only, inside the home directory and folders they attached).",
            json!({ "pattern": { "type": "string", "description": "A file name fragment or glob such as `uni/**/*.md`." } }),
            &["pattern"],
        ),
        tool(
            "file_read",
            "Read a text file, or a line range such as `20-80`, or a PDF (its pages come back tagged `[p.N]`). Read-only, inside the home directory and folders the learner attached, at most 256 KiB.",
            json!({ "path": { "type": "string" }, "lines": { "type": "string" } }),
            &["path"],
        ),
    ]
}

/// The reference-library tools (`library.rs`): read-only, every phase and
/// every agent that reads material, on every device (only text syncs).
fn library_tools() -> [ToolSpec; 3] {
    let kind = json!({ "type": "string", "enum": ["question-guide", "question-bank", "guide", "notes", "reference"] });
    [
        tool(
            "library_list",
            "List the learner's reference libraries and their files by kind (question-guide, question-bank, guide, notes, reference), with page counts and which files have no text. Read-only.",
            json!({
                "library": { "type": "string", "description": "A library id, e.g. `huawei-ict`; all libraries when left out." },
                "kind": kind.clone()
            }),
            &[],
        ),
        tool(
            "library_search",
            "Search the text of the learner's reference libraries. Every word is a case-insensitive term; returns up to 20 best lines as `<library>/<file>:<line> [p.N] (kind) — snippet`. Read-only.",
            json!({
                "query": { "type": "string", "description": "A few distinctive words in the text's own language." },
                "library": { "type": "string", "description": "Limit to one library id." },
                "kind": kind
            }),
            &["query"],
        ),
        tool(
            "library_read",
            "Read a library file's extracted text by line range (PDF pages are tagged `[p.N]`), at most 256 KiB per read. Read-only.",
            json!({
                "path": { "type": "string", "description": "`<library>/<file>` as library_list or library_search print it." },
                "from": { "type": "integer", "minimum": 1, "description": "First line; default 1." },
                "to": { "type": "integer", "minimum": 1, "description": "Last line; default as far as 256 KiB goes." }
            }),
            &["path"],
        ),
    ]
}

/// Read-only access to the learner's knowledge graph for the tutor (measure,
/// teach) and the planner (`graph::read`). Writing stays the graph builder's.
fn graph_read_tools() -> [ToolSpec; 2] {
    [
        tool(
            "graph_search",
            "Search the learner's knowledge graph (what earlier sessions recorded) for nodes and topics. Returns up to 20 matches with ids, homes, titles, statements and the learner's record (checks, which session recorded it and when). Read-only; says so when the graph is empty.",
            json!({ "query": { "type": "string", "description": "A word or phrase from a title, id or statement, e.g. `divider`." } }),
            &["query"],
        ),
        tool(
            "graph_read",
            "Read one node or topic of the learner's knowledge graph by id. A node: its statement, home, prerequisites (`requires`), what needs it, and the learner's record. A topic: its path, subtopics and nodes. Read-only.",
            json!({ "id": { "type": "string", "description": "A node or topic id from graph_search or the learner state." } }),
            &["id"],
        ),
    ]
}

fn workspace_write() -> ToolSpec {
    tool(
        "workspace_write",
        "Write a text file inside the session's attached workspace folder (creates folders; never deletes; refuses to overwrite a file that changed since you last read or wrote it). Only available when a workspace is attached.",
        json!({
            "path": { "type": "string", "description": "Path relative to the workspace folder." },
            "content": { "type": "string", "description": "The whole file, at most 256 KiB." }
        }),
        &["path", "content"],
    )
}

fn plugin_search() -> ToolSpec {
    tool(
        "plugin_search",
        "Search the learner's configured plugin registries for an uninstalled plugin. Read-only; installs nothing. Use only when the section's exit can't be met, or is clearly worse, without a tool.",
        json!({ "query": { "type": "string", "description": "What the section needs, e.g. `circuit` or `draw circuits`." } }),
        &["query"],
    )
}

fn session_end() -> ToolSpec {
    tool("session_end", "Close the session.", json!({}), &[])
}

/// The planner's only direct line to the learner (§4): one question on the
/// plan card. Calling it ends the planner run; the answer starts a new one.
fn plan_ask() -> ToolSpec {
    tool(
        "plan_ask",
        "Ask the learner one question on the plan card, when their edits are ambiguous or conflict and you can't resolve them sensibly. Give at least two options; the learner may also write their own answer. This ends your run: do not also call plan_write. The learner's answer starts a new planning run with the whole session again.",
        json!({
            "question": { "type": "string", "description": "One question, answerable in a line." },
            "options": { "type": "array", "minItems": 2, "items": { "type": "string" }, "description": "At least two options to choose from." }
        }),
        &["question", "options"],
    )
}

/// The tutor's hunt card (docs/learning-system.md §5): a question the learner
/// answers themselves, where to look, and what to bring back. Only the learner
/// switches a hunt on, so nothing here can enable one.
fn hunt_send() -> ToolSpec {
    tool(
        "hunt_send",
        "Send the learner on a hunt before you teach this section: one question to answer themselves, where to look, and what to bring back. Only offered when the learner switched this section's hunt on. Write two or three sentences about what the hunt is for, then end your turn; they come back whenever they like, and you debrief what they found before teaching.",
        json!({
            "sectionId": { "type": "string", "description": "The current section." },
            "question": { "type": "string", "description": "A question to answer, never a topic: it should make them compare, explain or find where people disagree. Not \"learn about op-amps\" but \"find out why op-amp circuits almost always use negative feedback, and where people disagree about it\"." },
            "lookAt": { "type": "array", "minItems": 1, "items": { "type": "object", "properties": {
                "kind": { "type": "string", "enum": ["forum", "datasheet", "docs", "lecture", "video", "paper", "person"], "description": "forum, datasheet, docs, lecture, video, paper, or a person to ask." },
                "venue": { "type": "string", "description": "The place by name, from the field guide when one is listed." },
                "why": { "type": "string", "description": "One line on what to look for there." }
            }, "required": ["kind", "why"] } },
            "bringBack": { "type": "array", "minItems": 1, "items": { "type": "string" }, "description": "What to come back with: claims (sources if they have them, not required), open questions, anything surprising, practical tips." }
        }),
        &["sectionId", "question", "lookAt", "bringBack"],
    )
}

/// The debrief (§5): every claim the learner brought back, sorted. The gate
/// refuses teaching questions until it exists, or the hunt was skipped.
fn debrief_record() -> ToolSpec {
    tool(
        "debrief_record",
        "Record how you sorted each claim the learner brought back from their hunt, before you teach. Nothing found on a hunt counts as evidence; the debrief is what corrects it. Judge a claim without a source on its content; if a claim and its source disagree, ask rather than marking it wrong.",
        json!({
            "huntId": { "type": "string", "description": "The hunt's id, from the hunt card." },
            "claims": { "type": "array", "minItems": 1, "items": { "type": "object", "properties": {
                "claim": { "type": "string", "description": "What the learner brought back, in their words." },
                "source": { "type": "string", "description": "Where they got it, when they said." },
                "verdict": { "type": "string", "enum": ["correct", "partial", "wrong", "tip", "offroute"], "description": "correct, partial, wrong, tip (a practitioner's rule of thumb, labelled as one) or offroute (interesting but not this section: park it with frontier_park)." },
                "note": { "type": "string", "description": "What you tell the learner about it." },
                "attribution": { "type": "string", "description": "For a tip: who says it." },
                "scope": { "type": "string", "description": "For a tip: when it applies, and when it doesn't." }
            }, "required": ["claim", "verdict", "note"] } }
        }),
        &["huntId", "claims"],
    )
}

/// Off-route topics from hunts and debriefs (§5). Appends to the frontier
/// list, which lives outside the session.
fn frontier_park() -> ToolSpec {
    tool(
        "frontier_park",
        "Park an interesting but off-route topic for later: the learner can start a new session from it whenever they like.",
        json!({
            "topic": { "type": "string", "description": "The subject, in a few words." },
            "why": { "type": "string", "description": "One line on why it is worth a session." }
        }),
        &["topic", "why"],
    )
}

/// A standing instruction from the learner about how to be taught, kept in
/// the goal's `preferences` so it reaches every later turn and the planner.
fn preference_add() -> ToolSpec {
    tool(
        "preference_add",
        "Record a standing instruction the learner gave about how they want to be taught (\"write your own questions instead of copying them from the PDF\", \"less theory first\"). It joins the goal's preferences, which you see every turn, and holds for the rest of the session until they change it. Record it the moment they say it, then follow it.",
        json!({
            "instruction": { "type": "string", "description": "The instruction as a short imperative, in the learner's sense." },
            "replaces": { "type": "string", "description": "The exact text of an earlier preference this one replaces, if any." }
        }),
        &["instruction"],
    )
}

/// The tutor's tools for a phase. Plugin tools are added by the caller.
pub fn tutor_tools(phase: Phase, workspace: bool) -> Vec<ToolSpec> {
    let mut t = vec![choice_ask(), research_request(), frontier_park()];
    t.extend(file_tools());
    t.extend(library_tools());
    if matches!(phase, Phase::Measure | Phase::Teach) {
        t.extend(graph_read_tools());
    }
    if matches!(phase, Phase::Measure | Phase::Teach | Phase::Close) {
        t.push(preference_add());
    }
    match phase {
        Phase::Measure => {
            t.push(tool(
                "goal_set",
                "Record the concrete goal. Starts the background scope research.",
                json!({
                    "goal": { "type": "string" },
                    "purpose": { "type": "string", "enum": ["curiosity", "project", "work", "exam", "course"] },
                    "depth": { "type": "string", "enum": ["pass", "solid", "deep"], "description": "How far to go: pass (get through it), solid, deep. Default solid; course and exam goals usually want pass." },
                    "explore": { "type": "string", "enum": ["off", "suggest", "on"], "description": "Whether hunts (going to find things out before teaching) may be offered. Default off, and always off for course and exam goals." },
                    "startingPoint": { "type": "string", "enum": ["measure", "quick", "new"], "description": "How the learner is measured: measure (the full probe), quick (at most five questions), new (no probe questions). Falls back to the learner's saved default." },
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
                        "levelReached": { "type": "string", "enum": ["none", "explain", "use", "analyze", "design"], "description": "The highest level the learner actually showed on this strand." },
                        "confidence": { "type": "string", "enum": ["solid", "rough", "unprobed"] },
                        "reason": { "type": "string", "enum": ["new", "skipped"], "description": "Only for an unprobed strand: new (the learner starts from scratch) or skipped (not reached)." },
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
            t.push(hunt_send());
            t.push(debrief_record());
            t.push(tool(
                "section_done",
                "Close the current section. Refused while any node in it lacks a passing check, and until one passing check is at the section's target level.",
                json!({
                    "sectionId": { "type": "string" },
                    "evidence": { "type": "string" },
                    "carryOver": { "type": "string" }
                }),
                &["sectionId", "evidence", "carryOver"],
            ));
            t.push(session_end());
            if workspace {
                t.push(workspace_write());
            }
        }
        Phase::Close => t.push(session_end()),
        Phase::Plan | Phase::AwaitApproval => {}
    }
    t
}

/// The tutor's own tool names in every phase (anything else is a plugin's).
static CORE_TUTOR_TOOLS: std::sync::LazyLock<HashSet<String>> = std::sync::LazyLock::new(|| {
    [Phase::Measure, Phase::Teach, Phase::Close]
        .into_iter()
        .flat_map(|phase| tutor_tools(phase, true))
        .map(|t| t.name)
        .collect()
});

/// Whether `name` is one of the tutor's own tools in any phase, i.e. not a
/// plugin's. Plugin tool names start with `<id>_`, so a cached registry index
/// can only be asked about a name that isn't one of these.
pub(crate) fn is_core_tutor_tool(name: &str) -> bool {
    CORE_TUTOR_TOOLS.contains(name)
}

pub fn planner_tools() -> Vec<ToolSpec> {
    let mut t = vec![
        plugin_search(),
        tool(
            "plan_write",
            "Write or revise the plan. Checked by the app; problems come back as an error to fix.",
            json!({
                "goal": { "type": "string" },
                "approach": { "type": "string" },
                "depth": { "type": "string", "enum": ["pass", "solid", "deep"], "description": "The learner's depth choice; derive each section's targetLevel from it." },
                "explore": { "type": "string", "enum": ["off", "suggest", "on"], "description": "The learner's explore choice; hunts may only be marked available when it is suggest or on." },
                "minutesPerSitting": { "type": "number", "description": "Preferred sitting length in minutes." },
                "style": { "type": "string", "enum": ["socratic", "expository", "mixed"], "description": "The learner's default section style: let me reason it out (socratic), explain it to me (expository)." },
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
                    "desktopOnly": { "type": "boolean" },
                    "targetLevel": { "type": "string", "enum": ["explain", "use", "analyze", "design"], "description": "How far this section must go; write `exit` at exactly this level." },
                    "assumedKnown": { "type": "boolean", "description": "A known section: the learner says they already hold it. One passing check at the target level closes it." },
                    "knownNote": { "type": "string", "description": "The learner's words about what they already know." },
                    "learnerNote": { "type": "string", "description": "The learner's note for the planner about this section." },
                    "hunt": { "type": "object", "description": "A hunt the learner may choose to take before this section is taught (docs/learning-system.md §5). Leave it out when the four conditions don't hold. Only the learner switches it on: the app refuses `enabled: true`, and `available` is refused while explore is off.", "properties": {
                        "available": { "type": "boolean", "description": "True when this section is worth a hunt: explore is suggest or on, the material isn't fully settled, the brief lists a venue the learner can read now, and the deadline allows it." },
                        "reason": { "type": "string", "description": "One line the learner reads on the plan card." },
                        "enabled": { "type": "boolean", "description": "Never yours to set. Leave it out; the app refuses true and derives it from the learner's toggle." }
                    }, "required": ["available", "reason"] },
                    "revision": { "type": "boolean", "description": "Only on a revision section the app wrote at the start of a merge track; keep such a section unchanged (or remove it). Never set it yourself." }
                }, "required": ["id", "title", "nodeIds", "mode", "style", "plugins", "notes", "exit"] } },
                "tracks": { "type": "array", "description": "Only to split the plan into strands taught in parallel sessions that merge later (architecture §4.26); leave it out otherwise. Every section in exactly one track. Root tracks have no `after`; a merge track lists the two or more tracks it joins; exactly one final track.", "items": { "type": "object", "properties": {
                    "id": { "type": "string", "description": "Lowercase slug." },
                    "title": { "type": "string", "description": "Short name the learner sees on the session list." },
                    "sectionIds": { "type": "array", "items": { "type": "string" }, "description": "This track's sections, in teaching order." },
                    "after": { "type": "array", "items": { "type": "string" }, "description": "Empty for a root track; for a merge track, the two or more tracks it joins." }
                }, "required": ["id", "title", "sectionIds", "after"] } }
            }),
            &["goal", "approach", "nodes", "sections"],
        ),
        tool(
            "plan_keep",
            "Answer a change request without changing the plan.",
            json!({ "reason": { "type": "string" } }),
            &["reason"],
        ),
        plan_ask(),
        research(),
    ];
    t.extend(file_tools());
    t.extend(graph_read_tools());
    t.extend(library_tools());
    t
}

pub fn researcher_tools() -> Vec<ToolSpec> {
    let mut t = vec![
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
            "Fetch a web page as text (HTML converted, truncated to about 40k characters); a PDF comes back as its pages tagged `[p.N]`.",
            json!({ "url": { "type": "string" } }),
            &["url"],
        ),
        tool(
            "source_outline",
            "Outline a learner document: Markdown headings with line numbers, or a PDF's page count and each page's first line.",
            json!({ "path": { "type": "string" } }),
            &["path"],
        ),
        tool(
            "source_search",
            "Search a learner document; returns matching lines with line numbers, or a PDF's matching pages with page numbers.",
            json!({ "path": { "type": "string" }, "query": { "type": "string" } }),
            &["path", "query"],
        ),
        tool(
            "source_read",
            "Read part of a learner document: a PDF by page range (`pages`, e.g. `3-5`), a text file by line range (`lines`, e.g. `120-180`). PDF pages come back tagged `[p.N]`.",
            json!({
                "path": { "type": "string" },
                "pages": { "type": "string", "description": "PDF only: `3`, `3-5` or `3-`." },
                "lines": { "type": "string", "description": "Text files only: `120-180`." }
            }),
            &["path"],
        ),
        tool(
            "source_page_image",
            "Look at one page of a PDF as an image: the way to read a scan, a figure or a formula the text layer gets wrong. A page that has a text layer is refused here — read it with source_read.",
            json!({
                "path": { "type": "string", "description": "A learner file path, as source_read takes it; or use `url`." },
                "url": { "type": "string", "description": "A PDF over http(s), instead of `path`." },
                "page": { "type": "integer", "description": "1-based page number." }
            }),
            &["page"],
        ),
        tool(
            "brief_submit",
            "Submit your brief. This is the job's single result and ends the job. Which fields are required depends on `kind`, and JSON Schema cannot say \"required for this enum value\": only `kind` is required below, and these are required by kind — `scope`: `summary` and `strands`; `verify`: `question` and `answer`; `source`: `question` and `answer`. A brief missing them is refused and you get the objection back, so fill the fields for your task's kind in the one call.",
            json!({
                "kind": {
                    "type": "string",
                    "enum": ["scope", "verify", "source"],
                    "description": "The kind of task you were given; it must match it (a `scope` task takes a `scope` brief)."
                },
                "summary": {
                    "type": "string",
                    "description": "scope, required: the field map in 5–10 sentences."
                },
                "strands": {
                    "type": "array",
                    "description": "scope, required: what reaching the goal depends on, checked against at least two independent sources.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string" },
                            "why": { "type": "string", "description": "Why the goal needs this strand." },
                            "kind": {
                                "type": "string",
                                "enum": ["mechanism", "fact", "procedure", "perceptual", "argument"]
                            },
                            "prerequisites": strings(),
                            "discourse": {
                                "type": "string",
                                "enum": ["settled", "practitioner-lore", "active-debate"]
                            }
                        },
                        "required": ["name", "why", "kind"]
                    }
                },
                "foundations": strings(),
                "standardOrder": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "scope: how reputable courses sequence the material."
                },
                "orderDisagreements": {
                    "type": "string",
                    "description": "scope: where those courses disagree about the order."
                },
                "misconceptions": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "strand": { "type": "string" },
                            "misconception": { "type": "string" },
                            "source": { "type": "string" }
                        },
                        "required": ["strand", "misconception"]
                    }
                },
                "conventions": strings(),
                "suggestedPlugins": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "plugin": { "type": "string" },
                            "strand": { "type": "string" },
                            "phase": { "type": "string", "enum": ["measure", "teach"] },
                            "reason": { "type": "string" }
                        },
                        "required": ["plugin", "strand", "phase", "reason"]
                    }
                },
                "venues": {
                    "type": "array",
                    "description": "scope: where people in this field ask questions, discuss, or look things up.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string" },
                            "url": { "type": "string" },
                            "kind": {
                                "type": "string",
                                "enum": ["ask", "discussion", "reference"]
                            },
                            "entryLevel": {
                                "type": "string",
                                "description": "What the learner must already hold for the venue to be useful, e.g. \"anyone\" or \"op-amp basics\"."
                            },
                            "note": { "type": "string", "description": "One line on what the venue is good for." },
                            "checked": {
                                "type": "string",
                                "description": "The date you confirmed the venue is active."
                            }
                        },
                        "required": ["name", "kind", "entryLevel", "note", "checked"]
                    }
                },
                "question": {
                    "type": "string",
                    "description": "verify, source, required: the exact question you answered."
                },
                "answer": {
                    "type": "string",
                    "description": "verify, source, required: the answer, 1–6 sentences."
                },
                "confidence": { "type": "string", "enum": ["high", "medium", "low"] },
                "quotes": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "text": { "type": "string" },
                            "url": { "type": "string" }
                        },
                        "required": ["text", "url"]
                    }
                },
                "caveats": strings(),
                "citations": {
                    "type": "array",
                    "description": "source: the pages of the document you answered from, quoted verbatim.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "page": { "type": "string" },
                            "quote": { "type": "string" }
                        },
                        "required": ["page", "quote"]
                    }
                },
                "notFound": {
                    "type": "boolean",
                    "description": "source: true when the document doesn't answer the question."
                },
                "sources": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "title": { "type": "string" },
                            "url": { "type": "string" },
                            "path": { "type": "string" },
                            "note": { "type": "string" }
                        },
                        "required": ["title"]
                    }
                },
                "uncertain": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "What you could not establish. An admitted gap is better than a guess."
                }
            }),
            &["kind"],
        ),
    ];
    t.extend(library_tools());
    t
}

/// The graph builder's tools (`jobs/graph-builder.md`): read the session's
/// messages, search the graph, then submit what the session established.
/// `graph_submit` is validated by code (`graph::check`) and the run continues
/// on a refusal.
pub fn graph_tools() -> Vec<ToolSpec> {
    vec![
        tool(
            "message_read",
            "Read messages of the list in full, in order: `from` and `to` for a range (read the whole list at once with from 1), or `index` for one.",
            json!({
                "from": { "type": "integer", "minimum": 1 },
                "to": { "type": "integer", "minimum": 1, "description": "Defaults to the last message." },
                "index": { "type": "integer", "minimum": 1 }
            }),
            &[],
        ),
        tool(
            "graph_search",
            "Search the knowledge graph for candidate nodes and topics. Returns up to 20 matches with their ids, titles and home topics. Search for each proposition before deciding identity, and for the ideas it depends on.",
            json!({ "query": { "type": "string" } }),
            &["query"],
        ),
        tool(
            "graph_submit",
            "Submit topics, nodes, edges, merges, chains and splits. Checked by the app; every problem comes back in one refusal to fix. A run ends when this is accepted; an empty submission is accepted and adds nothing.",
            json!({
                "topics": { "type": "array", "items": { "type": "object", "properties": {
                    "topicId": { "type": "string", "description": "A lowercase slug." },
                    "title": { "type": "string" },
                    "parent": { "type": "string", "description": "An existing or submitted topic id; omit for a root." }
                }, "required": ["topicId", "title"] } },
                "nodes": { "type": "array", "items": { "type": "object", "properties": {
                    "nodeId": { "type": "string", "description": "A lowercase slug; reuse an existing id to update it." },
                    "title": { "type": "string" },
                    "statement": { "type": "string", "description": "The claim, self-contained, math in LaTeX." },
                    "kind": { "type": "string", "enum": ["mechanism", "fact", "procedure", "perceptual", "argument", "heuristic"] },
                    "home": { "type": "string", "description": "Exactly one topic id." },
                    "attribution": { "type": "string", "description": "Required for a heuristic: whose rule of thumb it is." },
                    "scope": { "type": "string", "description": "Required for a heuristic: when it applies, and when it doesn't." },
                    "provenance": { "type": "object", "properties": {
                        "type": { "type": "string", "enum": ["session", "import", "hunt", "learner", "builder"] },
                        "sessionId": { "type": "string" },
                        "huntId": { "type": "string" },
                        "at": { "type": "string" }
                    }, "required": ["type"] }
                }, "required": ["nodeId", "statement", "kind", "home"] } },
                "edges": { "type": "array", "items": { "type": "object", "properties": {
                    "from": { "type": "string" },
                    "to": { "type": "string" },
                    "relation": { "type": "string", "enum": ["requires", "contrasts", "supports", "attacks"] },
                    "weight": { "type": "string", "enum": ["load-bearing", "trivial"] },
                    "why": { "type": "string" },
                    "provenance": { "type": "object" }
                }, "required": ["from", "to"] } },
                "sameAs": { "type": "array", "description": "Two ids that are the same proposition; both must exist.", "items": { "type": "object", "properties": {
                    "a": { "type": "string" },
                    "b": { "type": "string" }
                }, "required": ["a", "b"] } },
                "chains": { "type": "array", "items": { "type": "object", "properties": {
                    "chainId": { "type": "string" },
                    "premises": strings(),
                    "conclusion": { "type": "string" },
                    "statement": { "type": "string" }
                }, "required": ["premises", "conclusion"] } },
                "splits": { "type": "array", "description": "A better-named split of a topic over 30 nodes; it becomes a proposal, not structure, until the learner accepts it.", "items": { "type": "object", "properties": {
                    "topic": { "type": "string" },
                    "groups": { "type": "array", "items": { "type": "object", "properties": {
                        "title": { "type": "string" },
                        "nodeIds": strings(),
                        "why": { "type": "string" }
                    }, "required": ["title", "nodeIds"] } }
                }, "required": ["topic", "groups"] } },
                "venues": { "type": "array", "description": "Where the field lives for a topic: a place to ask, somewhere the field discusses things, or a reference to read. Attach each venue to the topic whose material it serves; a venue that serves two topics goes on the nearer ancestor both share.", "items": { "type": "object", "properties": {
                    "topicId": { "type": "string", "description": "An existing or submitted topic id." },
                    "kind": { "type": "string", "enum": ["ask", "discussion", "reference"] },
                    "name": { "type": "string" },
                    "url": { "type": "string" },
                    "entryLevel": { "type": "string", "description": "What the learner must already hold for the venue to be useful, e.g. \"anyone\" or \"op-amp basics\"." },
                    "note": { "type": "string", "description": "One line on what the venue is good for." },
                    "checked": { "type": "string", "description": "The date the venue was confirmed active, when one is known." },
                    "provenance": { "type": "object" }
                }, "required": ["topicId", "kind", "name", "entryLevel"] } }
            }),
            &[],
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
/// is listed so the planner can fix them all at once. On success each
/// section's `needs_install` lists its plugins missing from `manifests` (the
/// installed, enabled catalog); any value the model sent is replaced.
pub fn check_plan(
    args: &Value,
    state: &SessionState,
    manifests: &[PluginManifest],
    installable: &HashSet<String>,
) -> Result<Plan, Vec<String>> {
    // A level the model invented is refused by name, before serde turns it
    // into a generic schema error (the typed `Level` accepts only the four).
    let unknown_levels = unknown_target_levels(args);
    if !unknown_levels.is_empty() {
        return Err(unknown_levels);
    }
    let mut plan: Plan = serde_json::from_value(args.clone())
        .map_err(|e| vec![format!("plan_write arguments don't match the schema: {e}")])?;
    for s in &mut plan.sections {
        s.needs_install = s
            .plugins
            .iter()
            .filter(|p| !manifests.iter().any(|m| &m.id == *p))
            .cloned()
            .collect();
    }
    let problems = validate_plan(&plan, state, manifests, installable);
    if problems.is_empty() {
        Ok(plan)
    } else {
        Err(problems)
    }
}

/// Sections whose `targetLevel` isn't one of the four levels. `Level`'s
/// serde is strict, so this only ever fires on model output; it exists to
/// name the section instead of returning a deserialization error.
fn unknown_target_levels(args: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    let Some(sections) = args.get("sections").and_then(Value::as_array) else {
        return problems;
    };
    for section in sections {
        let Some(raw) = section.get("targetLevel").and_then(Value::as_str) else {
            continue;
        };
        if Level::parse(raw).is_none() {
            let id = section
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("(no id)");
            problems.push(format!(
                "section `{id}` has targetLevel `{raw}` (must be explain, use, analyze or design)"
            ));
        }
    }
    problems
}

/// The `plan_write` arguments as stored in the `plan` op: the model's object
/// with each section's `needsInstall` replaced by the code-computed value
/// from [`check_plan`] (omitted when empty).
pub fn plan_record(args: &Value, plan: &Plan) -> Value {
    let mut record = args.clone();
    if let Some(sections) = record.get_mut("sections").and_then(Value::as_array_mut) {
        for (raw, checked) in sections.iter_mut().zip(&plan.sections) {
            let Some(raw) = raw.as_object_mut() else {
                continue;
            };
            raw.remove("needsInstall");
            if !checked.needs_install.is_empty() {
                raw.insert("needsInstall".into(), json!(checked.needs_install));
            }
        }
    }
    record
}

/// Every rule from the build prompt: unique slugs; `requires` names existing
/// nodes and is acyclic; each node in exactly one section; valid modes and
/// styles; plugins registered and available on the platform; at most three
/// per section; completed sections unchanged on revision.
pub fn validate_plan(
    plan: &Plan,
    state: &SessionState,
    manifests: &[PluginManifest],
    installable: &HashSet<String>,
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
            // A revision section re-checks nodes other sections own.
            if !s.revision {
                *owner.entry(id.as_str()).or_insert(0) += 1;
            }
        }
        // Revision sections are written by the app when branches merge
        // (architecture §4.26); the planner may keep or remove one, never
        // make or change one.
        if s.revision {
            let kept = state.plan.as_ref().is_some_and(|p| {
                p.sections
                    .iter()
                    .any(|b| b.id == s.id && b.revision && same_section(s, b))
            });
            if !kept {
                problems.push(format!(
                    "section `{}` sets `revision`: revision sections are written by the app; leave `revision` out of your sections, and keep an existing revision section unchanged",
                    s.id
                ));
            }
        }
        if s.plugins.len() > MAX_PLUGINS_PER_SECTION {
            problems.push(format!(
                "section `{}` has {} plugins (at most {MAX_PLUGINS_PER_SECTION})",
                s.id,
                s.plugins.len()
            ));
        }
        // Hunts (§5): the planner may only mark one available, and only while
        // explore is on. Enabling it is the learner's decision, never the
        // planner's, so an `enabled` of true is always refused. A course or
        // exam goal never gets hunts at all: `goal_set` forces its explore
        // off, and the plan's own `explore` can't bring them back.
        if let Some(hunt) = &s.hunt {
            if hunt.enabled {
                problems.push(format!(
                    "section `{}` sets `hunt.enabled`: only the learner can enable a hunt",
                    s.id
                ));
            }
            let purpose = goal_purpose(state);
            if hunt.available && matches!(purpose, "course" | "exam") {
                problems.push(format!(
                    "section `{}` marks a hunt available, but this is a `{purpose}` goal: course and exam goals never get hunts",
                    s.id
                ));
            } else if hunt.available && !matches!(plan.explore.as_str(), "suggest" | "on") {
                let explore = if plan.explore.is_empty() {
                    "off"
                } else {
                    plan.explore.as_str()
                };
                problems.push(format!(
                    "section `{}` marks a hunt available while `explore` is `{explore}`: hunts need explore `suggest` or `on`",
                    s.id
                ));
            }
        }
        for p in &s.plugins {
            match manifests.iter().find(|m| &m.id == p) {
                // An uninstalled plugin from a configured registry is allowed;
                // the plan card shows install, and teaching drops it until the
                // learner installs it (W9).
                None if installable.contains(p) => {}
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

    // On revision, sections already completed must be unchanged (install
    // state is the device's, not the plan's, so `needs_install` is ignored).
    if let Some(previous) = &state.plan {
        for done in &state.completed_sections {
            let before = previous.sections.iter().find(|s| &s.id == done);
            let after = plan.sections.iter().find(|s| &s.id == done);
            match (before, after) {
                (Some(b), Some(a)) if !same_section(a, b) => {
                    problems.push(format!("completed section `{done}` was changed"));
                }
                (Some(_), None) => problems.push(format!("completed section `{done}` was removed")),
                _ => {}
            }
        }
    }
    problems.extend(track_problems(plan, state));
    problems.sort();
    problems.dedup();
    problems
}

/// The `tracks` rules (architecture §4.26). Tracks form an in-tree: two or
/// more root tracks; a merge track joins two or more tracks; every track but
/// one final track feeds exactly one merge; every section is in exactly one
/// track; a node requires only nodes on its own track or on tracks that
/// merge into it. A plan splits only before its first approval. Once it has
/// forked, the tracks (ids, titles, `after`) and the other tracks' sections
/// are fixed: each session changes only its own track.
fn track_problems(plan: &Plan, state: &SessionState) -> Vec<String> {
    let mut problems = Vec::new();
    let previous = state.plan.as_ref();
    if let Some(branch) = &state.branch {
        let before: &[Track] = previous.map(|p| p.tracks.as_slice()).unwrap_or_default();
        let shape = |ts: &[Track]| -> Vec<(String, String, Vec<String>)> {
            ts.iter()
                .map(|t| (t.id.clone(), t.title.clone(), t.after.clone()))
                .collect()
        };
        if shape(&plan.tracks) != shape(before) {
            problems.push(format!(
                "this session teaches track `{}` of a plan that already split into sessions: keep `tracks` (ids, titles, after) exactly as they are",
                branch.track
            ));
        } else {
            for t in before.iter().filter(|t| t.id != branch.track) {
                if plan
                    .track(&t.id)
                    .is_some_and(|n| n.section_ids != t.section_ids)
                {
                    problems.push(format!(
                        "track `{}` runs in another session: keep its sectionIds as they are and change only this session's track `{}`",
                        t.id, branch.track
                    ));
                }
            }
        }
    } else if !plan.tracks.is_empty()
        && state.approved_version.is_some()
        && previous.is_some_and(|p| p.tracks.is_empty())
    {
        problems.push(
            "a plan can only split into tracks before it is first approved; leave `tracks` out"
                .to_owned(),
        );
    }
    if plan.tracks.is_empty() {
        return problems;
    }
    // A merge input may have handed all its sections on to the merge.
    let forked = state.branch.is_some();
    if plan.tracks.len() < 2 {
        problems.push(
            "`tracks` needs at least two tracks; leave it out for a plan that doesn't split"
                .to_owned(),
        );
    }
    let mut ids: HashSet<&str> = HashSet::new();
    for t in &plan.tracks {
        if !is_slug(&t.id) {
            problems.push(format!("track id `{}` is not a lowercase slug", t.id));
        }
        if !ids.insert(t.id.as_str()) {
            problems.push(format!("duplicate track id `{}`", t.id));
        }
        if t.title.trim().is_empty() {
            problems.push(format!("track `{}` has no title", t.id));
        }
        if t.section_ids.is_empty() && !forked {
            problems.push(format!("track `{}` has no sections", t.id));
        }
        let mut seen: HashSet<&str> = HashSet::new();
        for a in &t.after {
            if !seen.insert(a.as_str()) {
                problems.push(format!("track `{}` lists `{a}` twice in `after`", t.id));
            } else if a == &t.id {
                problems.push(format!("track `{}` lists itself in `after`", t.id));
            } else if plan.track(a).is_none() {
                problems.push(format!("track `{}` is after unknown track `{a}`", t.id));
            }
        }
        if let [only] = t.after.as_slice() {
            problems.push(format!(
                "track `{}` comes after `{only}` alone: that is a continuation, so append its sections to `{only}` instead (a merge track joins two or more tracks)",
                t.id
            ));
        }
    }
    if plan.root_tracks().len() < 2 {
        problems.push(
            "a plan with tracks needs at least two root tracks (no `after`) that run in parallel"
                .to_owned(),
        );
    }
    let finals: Vec<&str> = plan
        .tracks
        .iter()
        .filter(|t| !plan.tracks.iter().any(|o| o.after.contains(&t.id)))
        .map(|t| t.id.as_str())
        .collect();
    if finals.len() != 1 {
        problems.push(format!(
            "exactly one track must be final (in no other track's `after`) so the branches merge back into one; now: {}",
            if finals.is_empty() {
                "none".to_owned()
            } else {
                finals.join(", ")
            }
        ));
    }
    for t in &plan.tracks {
        let feeds = plan
            .tracks
            .iter()
            .filter(|o| o.after.contains(&t.id))
            .count();
        if feeds > 1 {
            problems.push(format!(
                "track `{}` feeds {feeds} merges; each track feeds at most one",
                t.id
            ));
        }
    }
    let cyclic: Vec<&str> = plan
        .tracks
        .iter()
        .filter(|t| track_reaches(plan, &t.id, &t.id))
        .map(|t| t.id.as_str())
        .collect();
    if !cyclic.is_empty() {
        problems.push(format!(
            "`after` has a cycle through: {}",
            cyclic.join(", ")
        ));
    }
    let mut holder: HashMap<&str, usize> = HashMap::new();
    for t in &plan.tracks {
        for s in &t.section_ids {
            if !plan.sections.iter().any(|x| &x.id == s) {
                problems.push(format!("track `{}` lists unknown section `{s}`", t.id));
            }
            *holder.entry(s.as_str()).or_insert(0) += 1;
        }
    }
    for s in &plan.sections {
        match holder.get(s.id.as_str()) {
            None => problems.push(format!("section `{}` is in no track", s.id)),
            Some(n) if *n > 1 => problems.push(format!(
                "section `{}` is in {n} tracks (must be exactly one)",
                s.id
            )),
            Some(_) => {}
        }
    }
    // A node may build on its own track and on the tracks that merge into
    // it, never on a strand running in parallel.
    if cyclic.is_empty() {
        let home = |node: &str| {
            plan.sections
                .iter()
                .find(|s| !s.revision && s.node_ids.iter().any(|n| n == node))
                .and_then(|s| plan.track_of(&s.id))
        };
        for n in &plan.nodes {
            let Some(t) = home(&n.id) else { continue };
            let upstream = plan.upstream(&t.id);
            for r in &n.requires {
                let Some(u) = home(r) else { continue };
                if !upstream.contains(&u.id) {
                    problems.push(format!(
                        "node `{}` (track `{}`) requires `{r}` on track `{}`, which doesn't merge into `{}`: move one of them, or make `{}` come after `{}`",
                        n.id, t.id, u.id, t.id, t.id, u.id
                    ));
                }
            }
        }
    }
    problems
}

/// Whether `target` is reachable from `from` along `after` edges (one step
/// or more).
fn track_reaches(plan: &Plan, from: &str, target: &str) -> bool {
    let mut stack: Vec<&str> = plan
        .track(from)
        .map(|t| t.after.iter().map(String::as_str).collect())
        .unwrap_or_default();
    let mut seen: HashSet<&str> = HashSet::new();
    while let Some(id) = stack.pop() {
        if id == target {
            return true;
        }
        if seen.insert(id) {
            if let Some(t) = plan.track(id) {
                stack.extend(t.after.iter().map(String::as_str));
            }
        }
    }
    false
}

/// The goal's `purpose` as `goal_set` recorded it (§5), or `""` before the
/// plan's goal exists. `course` and `exam` goals force explore off and never
/// get hunts.
fn goal_purpose(state: &SessionState) -> &str {
    state
        .goal
        .as_ref()
        .and_then(|g| g.get("purpose"))
        .and_then(Value::as_str)
        .unwrap_or("")
}

fn same_section(a: &Section, b: &Section) -> bool {
    Section {
        needs_install: Vec::new(),
        ..a.clone()
    } == Section {
        needs_install: Vec::new(),
        ..b.clone()
    }
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
            version: "0.1.0".into(),
            api: 1,
            title: id.into(),
            description: "test".into(),
            license: "MIT".into(),
            tools: vec![],
            platforms: platforms.iter().map(|p| (*p).to_owned()).collect(),
            permissions: vec![],
            use_when: String::new(),
            avoid_when: String::new(),
            cost: String::new(),
            renderer: vec![],
            view: vec![],
            prompts: Default::default(),
        }
    }

    fn installable() -> HashSet<String> {
        HashSet::new()
    }

    fn plan(v: Value) -> Plan {
        serde_json::from_value(v).unwrap()
    }

    fn base() -> Value {
        json!({
            "goal": "g", "approach": "a",
            "depth": "solid", "explore": "off", "minutesPerSitting": 30, "style": "mixed",
            "nodes": [
                {"id":"a","title":"A","kind":"mechanism","requires":[]},
                {"id":"b","title":"B","kind":"fact","requires":["a"]}
            ],
            "sections": [
                {"id":"s1","title":"S1","nodeIds":["a"],"mode":"mechanism","style":"socratic","plugins":[],"notes":"","exit":"x",
                 "targetLevel":"use","assumedKnown":false,"knownNote":"","learnerNote":"keep it short"},
                {"id":"s2","title":"S2","nodeIds":["b"],"mode":"fact","style":"mixed","plugins":[],"notes":"","exit":"y",
                 "targetLevel":"analyze","assumedKnown":true,"knownNote":"I know Ohm's law","learnerNote":""}
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
        let checked = check_plan(&base(), &state(), &[], &installable()).unwrap();
        assert!(validate_plan(&checked, &state(), &[], &installable()).is_empty());
        assert_eq!(checked.depth, "solid");
        assert_eq!(checked.explore, "off");
        assert_eq!(checked.minutes_per_sitting, Some(30));
        assert_eq!(checked.style, "mixed");
        assert_eq!(checked.sections[0].target_level, Some(Level::Use));
        assert!(checked.sections[1].assumed_known);
        assert_eq!(checked.sections[1].known_note, "I know Ohm's law");
        assert_eq!(checked.sections[0].learner_note, "keep it short");
    }

    #[test]
    fn an_unknown_target_level_is_refused_by_name() {
        let mut v = base();
        v["sections"][1]["targetLevel"] = json!("mastery");
        let problems = check_plan(&v, &state(), &[], &installable()).unwrap_err();
        assert!(
            problems
                .join("\n")
                .contains("section `s2` has targetLevel `mastery`"),
            "{problems:?}"
        );
        // A level the model left out is fine: older plans have none.
        let mut v = base();
        v["sections"][1]
            .as_object_mut()
            .unwrap()
            .remove("targetLevel");
        let checked = check_plan(&v, &state(), &[], &installable()).unwrap();
        assert_eq!(checked.sections[1].target_level, None);
    }

    #[test]
    fn goal_and_question_schemas_carry_the_learning_system_fields() {
        let measure = tutor_tools(Phase::Measure, false);
        let goal = measure.iter().find(|t| t.name == "goal_set").unwrap();
        assert_eq!(
            goal.input_schema["properties"]["purpose"]["enum"],
            json!(["curiosity", "project", "work", "exam", "course"])
        );
        assert_eq!(
            goal.input_schema["properties"]["depth"]["enum"],
            json!(["pass", "solid", "deep"])
        );
        assert_eq!(
            goal.input_schema["properties"]["explore"]["enum"],
            json!(["off", "suggest", "on"])
        );
        assert_eq!(
            goal.input_schema["properties"]["startingPoint"]["enum"],
            json!(["measure", "quick", "new"])
        );
        let probe = measure.iter().find(|t| t.name == "question_ask").unwrap();
        assert!(
            probe.input_schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!("level")),
            "a probe question must carry a level"
        );
        let probe_done = measure.iter().find(|t| t.name == "probe_done").unwrap();
        let strand = &probe_done.input_schema["properties"]["strands"]["items"]["properties"];
        assert_eq!(
            strand["levelReached"]["enum"],
            json!(["none", "explain", "use", "analyze", "design"])
        );
        assert_eq!(strand["reason"]["enum"], json!(["new", "skipped"]));

        // Teach: the schema asks for a level on every question, so the model
        // sees the requirement the gate enforces.
        let teach = tutor_tools(Phase::Teach, false);
        let q = teach.iter().find(|t| t.name == "question_ask").unwrap();
        assert_eq!(
            q.input_schema["properties"]["level"]["enum"],
            json!(["explain", "use", "analyze", "design"])
        );
        assert!(
            q.input_schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!("level")),
            "a teach-phase question must carry a level"
        );
        let described = q.input_schema["properties"]["level"]["description"]
            .as_str()
            .unwrap();
        for level in ["explain", "use", "analyze", "design"] {
            assert!(described.contains(level), "`{level}` is not described");
        }
        let plan_write = planner_tools()
            .into_iter()
            .find(|t| t.name == "plan_write")
            .unwrap();
        let sections = &plan_write.input_schema["properties"]["sections"]["items"]["properties"];
        assert_eq!(
            sections["targetLevel"]["enum"],
            json!(["explain", "use", "analyze", "design"])
        );
        for key in ["assumedKnown", "knownNote", "learnerNote"] {
            assert!(sections.get(key).is_some(), "plan_write lacks {key}");
        }
    }

    #[test]
    fn the_brief_schema_declares_every_field_the_researcher_prompt_uses() {
        let tools = researcher_tools();
        let brief = tools.iter().find(|t| t.name == "brief_submit").unwrap();
        let props = &brief.input_schema["properties"];
        // Every field the fenced templates in jobs/researcher.md use: a model
        // that follows the schema must never be refused for a field the
        // schema never mentioned.
        for key in [
            "kind",
            "summary",
            "strands",
            "foundations",
            "standardOrder",
            "orderDisagreements",
            "misconceptions",
            "conventions",
            "suggestedPlugins",
            "venues",
            "question",
            "answer",
            "confidence",
            "quotes",
            "caveats",
            "citations",
            "notFound",
            "sources",
            "uncertain",
        ] {
            assert!(props.get(key).is_some(), "brief_submit lacks `{key}`");
        }
        let strand = &props["strands"]["items"]["properties"];
        for key in ["name", "why", "kind", "prerequisites", "discourse"] {
            assert!(strand.get(key).is_some(), "strands lack `{key}`");
        }
        assert_eq!(
            strand["kind"]["enum"],
            json!(["mechanism", "fact", "procedure", "perceptual", "argument"])
        );
        assert_eq!(
            strand["discourse"]["enum"],
            json!(["settled", "practitioner-lore", "active-debate"])
        );
        let plugin = &props["suggestedPlugins"]["items"]["properties"];
        for key in ["plugin", "strand", "phase", "reason"] {
            assert!(plugin.get(key).is_some(), "suggestedPlugins lack `{key}`");
        }
        assert_eq!(plugin["phase"]["enum"], json!(["measure", "teach"]));
        let venue = &props["venues"]["items"]["properties"];
        for key in ["name", "url", "kind", "entryLevel", "note", "checked"] {
            assert!(venue.get(key).is_some(), "venues lack `{key}`");
        }
        assert_eq!(
            venue["kind"]["enum"],
            json!(["ask", "discussion", "reference"])
        );
        assert_eq!(
            props["confidence"]["enum"],
            json!(["high", "medium", "low"])
        );

        // Which fields are required depends on `kind`, which one JSON Schema
        // `required` list cannot say: the list names only `kind`, and the
        // description carries the per-kind requirement the code enforces.
        assert_eq!(brief.input_schema["required"], json!(["kind"]));
        let described = brief.description.to_lowercase();
        assert!(
            described.contains("`scope`: `summary` and `strands`"),
            "the description doesn't say what a scope brief needs: {}",
            brief.description
        );
        assert!(
            described.contains("`verify`: `question` and `answer`"),
            "the description doesn't say what a verify brief needs: {}",
            brief.description
        );
        assert!(
            described.contains("`source`: `question` and `answer`"),
            "the description doesn't say what a source brief needs: {}",
            brief.description
        );
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
        let problems = validate_plan(&plan(v), &state(), &manifests, &installable()).join("\n");
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
    fn a_course_or_exam_goal_never_gets_a_hunt() {
        let mut v = base();
        v["explore"] = json!("on");
        v["sections"][0]["hunt"] = json!({ "available": true, "reason": "practitioner lore" });

        // With no goal record, the plan's own explore decides, and it is on.
        let checked = check_plan(&v, &state(), &[], &installable()).unwrap();
        assert!(validate_plan(&checked, &state(), &[], &installable()).is_empty());

        // `goal_set` forces explore off for a course or exam goal, and the
        // plan can't bring hunts back by declaring its own explore on.
        for purpose in ["course", "exam"] {
            let mut s = state();
            s.goal = Some(json!({ "purpose": purpose, "explore": "off" }));
            let problems = check_plan(&v, &s, &[], &installable()).expect_err("a hunt is refused");
            assert_eq!(
                problems,
                [format!(
                    "section `s1` marks a hunt available, but this is a `{purpose}` goal: course and exam goals never get hunts"
                )],
                "{problems:?}"
            );
            // The same plan without the hunt is fine.
            let mut plain = base();
            plain["explore"] = json!("on");
            assert!(check_plan(&plain, &s, &[], &installable()).is_ok());
        }

        // A curiosity goal keeps hunts, on the plan's own explore.
        let mut curious = state();
        curious.goal = Some(json!({ "purpose": "curiosity", "explore": "suggest" }));
        assert!(check_plan(&v, &curious, &[], &installable()).is_ok());
    }

    #[test]
    fn an_invalid_mode_is_a_schema_error() {
        let mut v = base();
        v["sections"][0]["mode"] = json!("vibes");
        assert!(check_plan(&v, &state(), &[], &installable()).is_err());
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
        assert!(validate_plan(&added, &s, &[], &installable()).is_empty());
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
        assert!(validate_plan(&plan(changed), &s, &[], &installable())
            .join(" ")
            .contains("completed section `s1` was changed"));
    }

    #[test]
    fn needs_install_is_computed_by_code_and_ignored_for_completed_sections() {
        let mut v = base();
        v["sections"][0]["plugins"] = json!(["circuit", "p1"]);
        // A model-supplied value is replaced, never trusted.
        v["sections"][0]["needsInstall"] = json!(["p1"]);
        v["sections"][1]["needsInstall"] = json!(["bogus"]);
        let installable: HashSet<String> = ["circuit".to_owned()].into();
        let manifests = [manifest("p1", &["desktop"])];
        let checked = check_plan(&v, &state(), &manifests, &installable).unwrap();
        assert_eq!(checked.sections[0].needs_install, ["circuit"]);
        assert!(checked.sections[1].needs_install.is_empty());
        let record = plan_record(&v, &checked);
        assert_eq!(record["sections"][0]["needsInstall"], json!(["circuit"]));
        assert!(record["sections"][1].get("needsInstall").is_none());

        // Once circuit is installed, a revision keeping the completed
        // section verbatim is not a change to it.
        let mut s = state();
        s.plan = Some(checked);
        s.approved_version = Some(1);
        s.completed_sections = vec!["s1".into()];
        let manifests = [
            manifest("p1", &["desktop"]),
            manifest("circuit", &["desktop"]),
        ];
        let mut revised = base();
        revised["sections"][0]["plugins"] = json!(["circuit", "p1"]);
        let revised = check_plan(&revised, &s, &manifests, &HashSet::new()).unwrap();
        assert!(revised.sections[0].needs_install.is_empty());
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
            let names: Vec<String> = tutor_tools(phase, false)
                .into_iter()
                .map(|t| t.name)
                .collect();
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
        let measure = tutor_tools(Phase::Measure, false);
        let q = measure.iter().find(|t| t.name == "question_ask").unwrap();
        assert_eq!(
            q.input_schema["properties"]["role"]["enum"],
            json!(["probe"])
        );
    }

    #[test]
    fn graph_reads_reach_the_measuring_and_teaching_tutor_and_the_planner_but_never_graph_submit() {
        let names =
            |tools: Vec<ToolSpec>| -> Vec<String> { tools.into_iter().map(|t| t.name).collect() };
        for phase in [
            Phase::Measure,
            Phase::Plan,
            Phase::AwaitApproval,
            Phase::Teach,
            Phase::Close,
        ] {
            let tutor = names(tutor_tools(phase, true));
            let reads = matches!(phase, Phase::Measure | Phase::Teach);
            for t in ["graph_search", "graph_read"] {
                assert_eq!(tutor.iter().any(|n| n == t), reads, "{t} in {phase:?}");
            }
            assert!(!tutor.iter().any(|n| n == "graph_submit"), "{phase:?}");
        }
        let planner = names(planner_tools());
        assert!(planner.iter().any(|n| n == "graph_search"));
        assert!(planner.iter().any(|n| n == "graph_read"));
        assert!(!planner.iter().any(|n| n == "graph_submit"));
        // The builder keeps the only write.
        assert!(names(graph_tools()).iter().any(|n| n == "graph_submit"));
    }
}
