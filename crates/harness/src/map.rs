//! The learning map: each Learn session's approved plan with the check
//! verdicts recorded against its ideas, for the UI's Map page
//! (docs/ui-design.md §7.6). It stands in for the knowledge graph until graph
//! storage exists (docs/agent-system-plan.md, "Not built yet").

use serde::Serialize;

use crate::sessions::SessionSummary;
use crate::state::{Kind, Plan, SessionState};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MapPlan {
    pub session_id: String,
    pub title: String,
    pub updated_at: String,
    pub goal: String,
    pub nodes: Vec<MapNode>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MapNode {
    pub id: String,
    pub title: String,
    pub kind: Kind,
    /// Ids of in-plan prerequisites.
    pub requires: Vec<String>,
    /// Title of the plan section that teaches this idea.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    /// `correct` check verdicts.
    pub correct: u32,
    /// Check verdicts other than `correct` (partial, wrong, don't know).
    pub missed: u32,
}

/// The map entry for one session; `None` until its plan was approved.
pub(crate) fn plan_of(summary: SessionSummary, mut state: SessionState) -> Option<MapPlan> {
    state.approved_version?;
    let Plan {
        goal,
        nodes,
        sections,
        ..
    } = state.plan?;
    let nodes = nodes
        .into_iter()
        .map(|n| {
            let verdicts = state.checks.remove(&n.id).unwrap_or_default();
            let correct = verdicts.iter().filter(|v| v.verdict == "correct").count();
            let section = sections
                .iter()
                .find(|s| s.node_ids.contains(&n.id))
                .map(|s| s.title.clone());
            MapNode {
                id: n.id,
                title: n.title,
                kind: n.kind,
                requires: n.requires,
                section,
                correct: correct as u32,
                missed: (verdicts.len() - correct) as u32,
            }
        })
        .collect();
    Some(MapPlan {
        session_id: summary.id,
        title: summary.title,
        updated_at: summary.updated_at,
        goal,
        nodes,
    })
}
