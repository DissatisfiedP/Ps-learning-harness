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
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Mechanism => "mechanism",
            Kind::Fact => "fact",
            Kind::Procedure => "procedure",
            Kind::Perceptual => "perceptual",
            Kind::Argument => "argument",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "mechanism" => Kind::Mechanism,
            "fact" => Kind::Fact,
            "procedure" => Kind::Procedure,
            "perceptual" => Kind::Perceptual,
            "argument" => Kind::Argument,
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    #[serde(default)]
    pub version: u32,
    pub goal: String,
    pub approach: String,
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

/// Derived session state, rebuilt by replaying ops. Never stored.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionState {
    pub phase: Phase,
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
    pub plan_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_section: Option<String>,
    /// nodeId -> passing checks seen so far.
    pub checks: BTreeMap<String, Vec<CheckVerdict>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_call: Option<PendingCall>,
    /// Job results delivered but not yet consumed into a turn.
    pub queued_jobs: Vec<serde_json::Value>,
    pub granted_plugins: Vec<String>,
    /// desktop | mobile
    pub platform: String,
    /// Learner-model summary, or "no graph yet" (graph is out of scope).
    pub learner_state: String,
    /// Session title, generated once after goal_set.
    pub title: Option<String>,
}

impl SessionState {
    /// Section currently being taught, if any.
    pub fn current_section(&self) -> Option<&Section> {
        let id = self.current_section.as_ref()?;
        self.plan.as_ref()?.sections.iter().find(|s| &s.id == id)
    }
}
