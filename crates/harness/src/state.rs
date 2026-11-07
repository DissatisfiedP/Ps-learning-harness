//! Session flow data types (architecture §4.10, §5).
//!
//! These are the target op-log payload shapes. [`SessionState`] is *derived*:
//! it is rebuilt by replaying ops (M4), never stored. The prompt composer (M3)
//! renders a compact view of it as `session_state`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Teaching phase. The phase machine (M4/M5) owns transitions; this is the
/// value persisted in `phase` ops and read by the composer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    #[default]
    Measure,
    Plan,
    AwaitApproval,
    Teach,
    Close,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::Measure => "measure",
            Phase::Plan => "plan",
            Phase::AwaitApproval => "awaitApproval",
            Phase::Teach => "teach",
            Phase::Close => "close",
        }
    }
}

/// A plan node's knowledge kind (decides mode + grading, architecture §4.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Mechanism,
    Fact,
    Procedure,
    Perceptual,
    Argument,
    /// A practitioner's rule of thumb: useful and true enough, not a law. It
    /// carries `attribution` and `scope` on the graph node that holds it
    /// (`docs/graph-build.md`). A plan node never uses it.
    Heuristic,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Mechanism => "mechanism",
            Kind::Fact => "fact",
            Kind::Procedure => "procedure",
            Kind::Perceptual => "perceptual",
            Kind::Argument => "argument",
            Kind::Heuristic => "heuristic",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "mechanism" => Kind::Mechanism,
            "fact" => Kind::Fact,
            "procedure" => Kind::Procedure,
            "perceptual" => Kind::Perceptual,
            "argument" => Kind::Argument,
            "heuristic" => Kind::Heuristic,
            _ => return None,
        })
    }
}

/// How far a section goes (docs/learning-system.md §2), in the four plain
/// levels of the revised Bloom taxonomy. Ordered: Explain < Use < Analyze <
/// Design.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Explain,
    Use,
    Analyze,
    Design,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Explain => "explain",
            Level::Use => "use",
            Level::Analyze => "analyze",
            Level::Design => "design",
        }
    }

    pub fn parse(s: &str) -> Option<Level> {
        Some(match s.trim() {
            "explain" => Level::Explain,
            "use" => Level::Use,
            "analyze" => Level::Analyze,
            "design" => Level::Design,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanNode {
    pub id: String,
    pub title: String,
    pub kind: Kind,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_node: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<f64>,
    #[serde(default)]
    pub practice_only: bool,
}

/// A hunt the planner marked available in a section (docs/learning-system.md
/// §5). The planner may only make one *available*; `enabled` is never set by
/// `plan_write` and is derived from the learner's `huntToggle` ops at replay.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hunt {
    pub available: bool,
    #[serde(default)]
    pub reason: String,
    /// Never set by `plan_write`; derived from the learner's `huntToggle` ops.
    /// Defaulted so a planner writing `{ available, reason }` — what the
    /// prompt tells it to write — isn't refused over a field it must not set.
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Section {
    pub id: String,
    pub title: String,
    pub node_ids: Vec<String>,
    pub mode: Kind,
    /// socratic | expository | mixed
    pub style: String,
    #[serde(default)]
    pub plugins: Vec<String>,
    #[serde(default)]
    pub notes: String,
    pub exit: String,
    #[serde(default)]
    pub desktop_only: bool,
    /// Plugins in `plugins` that weren't installed and enabled on this device
    /// when the plan was written (W9). Set by code in `plan_write`, never by
    /// the model; the plan card shows an install chip for each.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs_install: Vec<String>,
    /// How far this section must go (§2). `None` only in older sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_level: Option<Level>,
    /// The learner said they already know this part; one passing check at
    /// `target_level` closes it (§4), until a miss refutes it.
    #[serde(default)]
    pub assumed_known: bool,
    /// The learner's "parts I already know" comment (§4).
    #[serde(default)]
    pub known_note: String,
    /// The learner's free-text note for the planner (§4).
    #[serde(default)]
    pub learner_note: String,
    /// The hunt the planner marked available for this section (§5), or `None`
    /// in older sessions and in sections with no hunt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hunt: Option<Hunt>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    #[serde(default)]
    pub version: u32,
    pub goal: String,
    pub approach: String,
    /// pass | solid | deep (§2): the default the learner chose, mirrored from
    /// the goal.
    #[serde(default)]
    pub depth: String,
    /// off | suggest | on (§5).
    #[serde(default)]
    pub explore: String,
    /// Preferred sitting length in minutes, when the learner chose one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minutes_per_sitting: Option<u32>,
    /// The default section style: "Let me reason it out" (socratic) or
    /// "Explain it to me" (expository).
    #[serde(default)]
    pub style: String,
    pub nodes: Vec<PlanNode>,
    pub sections: Vec<Section>,
}

/// A recorded check verdict for section_done gating.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckVerdict {
    pub node_id: String,
    pub question_id: String,
    pub verdict: String, // correct | partial | wrong | dontknow
    /// The level the graded question was written to demand, copied from its
    /// `question_ask` args at replay. `None` when the question had no level
    /// (older sessions).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<Level>,
}

/// How well the learner's confidence matched their results (§7). Derived from
/// the ops, shown only once the thresholds are met.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Calibration {
    /// Graded answers that also carried a confidence.
    pub graded: u32,
    /// The buckets with at least five answers, in ascending confidence.
    pub buckets: Vec<CalibrationBucket>,
    /// One observational sentence per bucket whose accuracy is at least 15
    /// points off, otherwise the single "Confidence matches results." line.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationBucket {
    /// The bucket's confidence value: 10, 30, 50, 70 or 90.
    pub confidence: u32,
    /// Guessing | Unsure | Even odds | Fairly sure | Certain (the buttons'
    /// wording, ui-design §7.2.4).
    pub label: String,
    pub correct: u32,
    pub total: u32,
}

impl CalibrationBucket {
    /// The share of correct answers in this bucket, as a rounded percentage.
    pub fn percent(&self) -> u32 {
        percent(self.correct, self.total)
    }
}

/// `correct / total` as a rounded percentage (0 when there is nothing to
/// divide).
pub fn percent(correct: u32, total: u32) -> u32 {
    if total == 0 {
        return 0;
    }
    (correct * 100 + total / 2) / total
}

/// A pending interactive tool call (question_ask / choice_ask). Derivable
/// from the op log, so it survives an app restart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingCall {
    pub call_id: String,
    pub name: String,
    pub args: serde_json::Value,
}

/// The planner's open question on the plan card (docs/learning-system.md §4):
/// the latest `planAsk` op with no `planAnswer` for its call id. Derivable,
/// so it survives an app restart; the learner's answer starts a new planner
/// run, so no resume state is stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanQuestion {
    /// The `plan_ask` tool call id the answer is matched by.
    pub call_id: String,
    pub question: String,
    #[serde(default)]
    pub options: Vec<String>,
}

/// One hunt, derived from the ops (docs/learning-system.md §5). `hunt_id` is
/// the `hunt_send` tool call's id, the one id the learner, the UI and the
/// tutor all use. Every hunt in the log has one, in `SessionState.hunts`; the
/// current section's is also `SessionState.hunt`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HuntState {
    pub hunt_id: String,
    pub section_id: String,
    pub question: String,
    #[serde(default)]
    pub look_at: serde_json::Value,
    #[serde(default)]
    pub bring_back: Vec<String>,
    /// ready | out | returned | skipped
    pub state: String,
    /// What the learner brought back (`notes`, `links`, `files`), once returned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returned: Option<serde_json::Value>,
    #[serde(default)]
    pub debriefed: bool,
}

/// A job result that is done but not yet delivered into the conversation
/// (it waits while a learner answer is pending, §4.16 delivery rule).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedResult {
    pub job_id: String,
    pub kind: String,
    pub question: String,
    pub result: serde_json::Value,
}

/// A job that is queued or running according to the op log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenJob {
    pub job_id: String,
    pub kind: String,
    pub state: String,
    pub question: String,
    pub blocking: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
}

/// A plugin granted for one section (by the plan or by `plugin_request`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Grant {
    pub section_id: String,
    pub plugin: String,
    /// plan | request | learner
    pub by: String,
}

/// Derived session state, rebuilt by replaying ops. Never stored.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionState {
    pub phase: Phase,
    /// `session_end` succeeded.
    pub ended: bool,
    /// The goal record, verbatim JSON from `goal_set`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<serde_json::Value>,
    /// The scope field brief, verbatim JSON from `brief_submit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<serde_json::Value>,
    /// Probe map (strands with floor/ceiling), from `probe_done`.
    pub probe: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<Plan>,
    /// Number of `plan` ops so far (1 = the first plan).
    pub plan_version: u32,
    /// The plan version the learner last approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_section: Option<String>,
    /// Sections closed by an accepted `section_done`, in order.
    pub completed_sections: Vec<String>,
    /// nodeId -> passing checks seen so far.
    pub checks: BTreeMap<String, Vec<CheckVerdict>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_call: Option<PendingCall>,
    /// Done job results not yet delivered into the conversation.
    pub queued_jobs: Vec<QueuedResult>,
    /// Jobs still queued or running according to the log.
    pub open_jobs: Vec<OpenJob>,
    /// Plugins granted per section (plan sections after learner removals,
    /// plus mid-section `plugin_request` grants).
    pub grants: Vec<Grant>,
    /// Plugins active in the current section.
    pub active_plugins: Vec<String>,
    /// `nodeId` of the latest teach-phase `question_ask` (map highlight).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_node_id: Option<String>,
    /// desktop | mobile
    pub platform: String,
    /// Learner-model summary, or "no graph yet" (graph is out of scope).
    pub learner_state: String,
    /// Session title, generated once after goal_set.
    pub title: Option<String>,
    /// The attached workspace folder's name (the absolute path is
    /// device-local), when one is attached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// path -> sha256 of the last agent write in the workspace.
    #[serde(default)]
    pub workspace_writes: BTreeMap<String, String>,
    /// Absolute paths the learner granted as read roots (W3).
    #[serde(default)]
    pub file_grants: Vec<String>,
    /// nodeId -> highest level with a passing check (derived, never stored;
    /// §2). Empty for sessions whose checks carried no level.
    #[serde(default)]
    pub node_levels: BTreeMap<String, Level>,
    /// Sections whose assumed-known check was missed (§4). Code appends them
    /// when the `knownRefuted` op is replayed; such a section un-greys and
    /// falls back to the normal per-node gate.
    #[serde(default)]
    pub known_refuted: Vec<String>,
    /// The calibration block (§7), present only when the thresholds are met.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calibration: Option<Calibration>,
    /// The planner's open question on the plan card (§4): a `planAsk` with no
    /// `planAnswer` yet. `answer_plan_question` refuses when it is `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_plan_question: Option<PlanQuestion>,
    /// The current section's hunt lifecycle (§5), from `huntSend`,
    /// `huntReturn`, `huntSkip` and `debrief`. `None` until the tutor sends
    /// one for the current section.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hunt: Option<HuntState>,
    /// Every hunt the tutor sent (§5), in op order, each with the lifecycle
    /// its own ops imply. The flow resolves a hunt card by `huntId` here, so a
    /// card from an earlier section keeps its own finished state instead of
    /// passing itself off as live; `hunt` is the same record for the current
    /// section, which the gates and the prompt read.
    #[serde(default)]
    pub hunts: Vec<HuntState>,
}

impl SessionState {
    /// Section currently being taught, if any.
    pub fn current_section(&self) -> Option<&Section> {
        let id = self.current_section.as_ref()?;
        self.plan.as_ref()?.sections.iter().find(|s| &s.id == id)
    }

    /// The first plan section not yet completed: where teaching resumes after
    /// an approval or a revision.
    pub fn next_open_section(&self) -> Option<String> {
        self.plan
            .as_ref()?
            .sections
            .iter()
            .find(|s| !self.completed_sections.contains(&s.id))
            .map(|s| s.id.clone())
    }

    /// Whether a node has at least one `check` question graded `correct`.
    pub fn node_passed(&self, node_id: &str) -> bool {
        self.checks
            .get(node_id)
            .is_some_and(|v| v.iter().any(|c| c.verdict == "correct"))
    }

    /// The highest level with a passing check on `node_id` (§2), derived.
    pub fn node_level(&self, node_id: &str) -> Option<Level> {
        self.node_levels.get(node_id).copied()
    }

    /// Whether some passing check in `section` was written to demand `level`
    /// or above. A check with no level (an older session) never counts.
    pub fn section_passed_at(&self, section: &Section, level: Level) -> bool {
        section.node_ids.iter().any(|id| {
            self.checks.get(id).is_some_and(|verdicts| {
                verdicts
                    .iter()
                    .any(|c| c.verdict == "correct" && c.level.is_some_and(|l| l >= level))
            })
        })
    }

    /// The learner's chosen starting point (§3): `measure` | `quick` | `new`,
    /// from the goal record. `measure` when the goal doesn't say.
    pub fn starting_point(&self) -> &str {
        self.goal
            .as_ref()
            .and_then(|g| g.get("startingPoint"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("measure")
    }

    /// Whether the current section's hunt is live (§5): the planner marked it
    /// available and the learner switched it on. A skip switches it off in
    /// replay, so a skipped hunt is never active.
    pub fn hunt_active(&self) -> bool {
        self.current_section()
            .and_then(|s| s.hunt.as_ref())
            .is_some_and(|h| h.available && h.enabled)
    }

    /// The hunt lifecycle for the prompt and the session state line (§5):
    /// `ready` (not sent yet), `out` (the learner has it), `returned`,
    /// `skipped`. Empty when the current section has no live hunt and no hunt
    /// record, so an older session renders exactly as before.
    pub fn hunt_state(&self) -> &'static str {
        if let Some(hunt) = &self.hunt {
            return match hunt.state.as_str() {
                "out" => "out",
                "returned" => "returned",
                "skipped" => "skipped",
                _ => "ready",
            };
        }
        if self.hunt_active() {
            "ready"
        } else {
            ""
        }
    }

    /// Whether a hunt has to be debriefed before teaching may continue (§5):
    /// the hunt is live (or was sent) and not skipped, and no `debrief` exists.
    pub fn hunt_blocks_teaching(&self) -> bool {
        let Some(hunt) = &self.hunt else {
            return self.hunt_active();
        };
        self.hunt_active() && hunt.state != "skipped" && !hunt.debriefed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_are_ordered_parsed_and_serialized_lowercase() {
        assert!(Level::Explain < Level::Use);
        assert!(Level::Use < Level::Analyze);
        assert!(Level::Analyze < Level::Design);
        for level in [Level::Explain, Level::Use, Level::Analyze, Level::Design] {
            assert_eq!(Level::parse(level.as_str()), Some(level));
            assert_eq!(
                serde_json::to_value(level).unwrap(),
                serde_json::json!(level.as_str())
            );
        }
        assert_eq!(Level::parse(" Bloom "), None);
        assert_eq!(Level::parse("Analyze"), None, "levels are lowercase slugs");
        // A section without a level is an older section, not an error.
        let section: Section = serde_json::from_value(serde_json::json!({
            "id": "s1", "title": "S", "nodeIds": ["n1"], "mode": "mechanism",
            "style": "socratic", "notes": "", "exit": "e"
        }))
        .unwrap();
        assert_eq!(section.target_level, None);
        assert!(!section.assumed_known);
        assert!(section.known_note.is_empty() && section.learner_note.is_empty());
        // An unknown level is refused by serde, which check_plan reports.
        assert!(serde_json::from_value::<Section>(serde_json::json!({
            "id": "s1", "title": "S", "nodeIds": ["n1"], "mode": "mechanism",
            "style": "socratic", "notes": "", "exit": "e", "targetLevel": "bloom"
        }))
        .is_err());
    }

    #[test]
    fn starting_point_defaults_to_measure() {
        let mut state = SessionState::default();
        assert_eq!(state.starting_point(), "measure");
        state.goal = Some(serde_json::json!({ "startingPoint": "new" }));
        assert_eq!(state.starting_point(), "new");
    }

    #[test]
    fn a_sections_hunt_parses_the_shape_the_planner_is_told_to_write() {
        // `planner/base.md` tells the planner to write `{ available, reason }`
        // and never `enabled`; the prompt and the record must agree.
        let section: Section = serde_json::from_value(serde_json::json!({
            "id": "s1", "title": "S", "nodeIds": ["n1"], "mode": "mechanism",
            "style": "socratic", "notes": "", "exit": "e",
            "hunt": { "available": true, "reason": "practitioner lore" }
        }))
        .unwrap();
        let hunt = section.hunt.unwrap();
        assert!(hunt.available);
        assert_eq!(hunt.reason, "practitioner lore");
        assert!(!hunt.enabled, "the planner can never enable a hunt");

        // A section from before hunts has none, and serializing one without a
        // hunt leaves the key out entirely.
        let old: Section = serde_json::from_value(serde_json::json!({
            "id": "s1", "title": "S", "nodeIds": ["n1"], "mode": "mechanism",
            "style": "socratic", "notes": "", "exit": "e"
        }))
        .unwrap();
        assert_eq!(old.hunt, None);
        assert!(serde_json::to_value(&old).unwrap().get("hunt").is_none());
    }

    #[test]
    fn the_hunt_state_line_follows_the_current_sections_hunt() {
        let section = |enabled: bool| Section {
            hunt: Some(Hunt {
                available: true,
                reason: String::new(),
                enabled,
            }),
            ..serde_json::from_value(serde_json::json!({
                "id": "s1", "title": "S", "nodeIds": ["n1"], "mode": "mechanism",
                "style": "socratic", "notes": "", "exit": "e"
            }))
            .unwrap()
        };
        let state = |section: Section, hunt: Option<HuntState>| SessionState {
            plan: Some(Plan {
                version: 1,
                goal: "g".into(),
                approach: "a".into(),
                depth: String::new(),
                explore: "suggest".into(),
                minutes_per_sitting: None,
                style: String::new(),
                nodes: vec![],
                sections: vec![section],
            }),
            current_section: Some("s1".into()),
            hunt,
            ..Default::default()
        };

        // Off: nothing to say and nothing to gate.
        let off = state(section(false), None);
        assert_eq!(off.hunt_state(), "");
        assert!(!off.hunt_blocks_teaching());

        // On but unsent: `ready`, and teaching waits for the hunt.
        let on = state(section(true), None);
        assert_eq!(on.hunt_state(), "ready");
        assert!(on.hunt_blocks_teaching());

        let sent = |state: &str, debriefed: bool| HuntState {
            hunt_id: "h1".into(),
            section_id: "s1".into(),
            question: "q".into(),
            look_at: serde_json::json!([]),
            bring_back: vec![],
            state: state.into(),
            returned: None,
            debriefed,
        };
        let out = state(section(true), Some(sent("out", false)));
        assert_eq!(out.hunt_state(), "out");
        assert!(out.hunt_blocks_teaching());
        let debriefed = state(section(true), Some(sent("returned", true)));
        assert_eq!(debriefed.hunt_state(), "returned");
        assert!(!debriefed.hunt_blocks_teaching());
        // A skip switches the hunt off in replay, so the lifecycle name is the
        // only thing left to read.
        let skipped = state(section(false), Some(sent("skipped", false)));
        assert_eq!(skipped.hunt_state(), "skipped");
        assert!(!skipped.hunt_blocks_teaching());
    }
}
