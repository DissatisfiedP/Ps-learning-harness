//! Sessions as append-only op logs: `sessions/<yyyy>/<ulid>.jsonl` in the data
//! repo (architecture §4.17, §5). Each line is one op; readers order by `at`
//! (ties by `opId`, which is monotonic within a process, see [`op_id`]).
//!
//! The op set is the target set from §5 `SessionOp`. Every op carries `opId`
//! (ULID) and `at`. Derived [`crate::state::SessionState`] is rebuilt by
//! replaying these ops and is never stored.
//!
//! Crash safety: each op is one `write_all` followed by `sync_data`, so a
//! crash can only tear the final line. Readers repair that tail, skip (and
//! count) any line that still does not parse instead of failing the whole
//! session, and a Learn session is read-only unless its second op is the
//! initial `phase` op (`read_file`, `scan_head`).
//!
//! Performance (build prompt M7): `list` reads only each file's first two
//! lines plus its mtime, and titles generated later come from a device-local
//! index; the parsed ops of recently used sessions stay in a small LRU so a
//! turn never re-reads its session file.

use std::collections::{HashMap, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use chrono::{DateTime, Datelike, Utc};
use providers::{ContentBlock, Role};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ulid::Ulid;

use crate::now_iso;
use crate::state::Phase;
use crate::Error;

/// Open sessions whose parsed ops stay in memory.
const LRU_SESSIONS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Learn,
    Practice,
}

/// Why an assistant message ended, when it was not a normal finish.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Ending {
    MaxTokens,
    Refusal,
    /// The learner pressed Stop.
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    /// image | file | folder | library
    pub kind: String,
    /// Image: repo-relative path under `attachments/`. File/folder: the
    /// absolute path the learner chose (device-specific), or a path relative
    /// to the session workspace for files inside it. Library: the library id
    /// under `<repo>/library/` (a session link, not a copy).
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredMessage {
    pub op_id: String,
    pub at: String,
    pub role: Role,
    pub content: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub thinking: String,
    /// `provider/model` that wrote an assistant message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ending: Option<Ending>,
    /// Marks a message the app wrote into the conversation: `research-brief`
    /// (a delivered job result) or `plan-notice` (a planner answer/approval).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// The job whose result this message delivers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    /// Provider-native blocks (signed thinking, encrypted reasoning) that must
    /// be replayed to the same wire in tool loops, even after a restart.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub native: Vec<ContentBlock>,
    /// Learner attachments (images copied into the repo; files and folders
    /// referenced by path). Empty for tutor messages.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Header {
    pub op_id: String,
    pub at: String,
    pub id: String,
    pub mode: Mode,
    pub title: String,
    /// Set on a session forked for another root track of a branching plan
    /// (architecture §4.26).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork: Option<ForkLink>,
    /// Set on the session created when every input of a merge track ended
    /// (architecture §4.26).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge: Option<MergeLink>,
}

/// Where a forked session came from: its ops through `upTo` are a copy of
/// the parent's (the pre-plan transcript, goal, probe and plan), and it
/// teaches `trackId`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkLink {
    pub parent: String,
    pub up_to_at: String,
    pub up_to_op_id: String,
    pub track_id: String,
}

/// What a merge session merges: the sessions of the merge track's inputs
/// (in the track's `after` order), the copied pre-plan cut of the origin, the
/// track it teaches, and the sections its inputs completed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeLink {
    pub origin: String,
    pub parents: Vec<String>,
    pub up_to_at: String,
    pub up_to_op_id: String,
    pub track_id: String,
    #[serde(default)]
    pub completed: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhaseOp {
    pub op_id: String,
    pub at: String,
    pub phase: Phase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section_id: Option<String>,
}

/// A record op (`goal`, `brief`, `probe`, `plan`): the phase tool's arguments,
/// verbatim (architecture §5).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordOp {
    pub op_id: String,
    pub at: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanRequestOp {
    pub op_id: String,
    pub at: String,
    /// probe | tutor | learner
    pub from: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub learner_words: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanKeepOp {
    pub op_id: String,
    pub at: String,
    pub reason: String,
}

/// The learner's batched plan-card edits (docs/learning-system.md §4): one
/// structured diff plus an optional comment. Never applied by replay; the
/// planner job that follows it reads this op to decide the new plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanEditOp {
    pub op_id: String,
    pub at: String,
    /// Plan-level fields (`depth`, `explore`, `minutesPerSitting`, `style`)
    /// and a `sections` array of per-section edits.
    #[serde(default)]
    pub edits: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

/// The planner's one question on the plan card (§4). `call_id` is the
/// `plan_ask` tool call; it ends the planner run and the learner's
/// [`PlanAnswerOp`] starts a fresh one.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanAskOp {
    pub op_id: String,
    pub at: String,
    pub call_id: String,
    pub question: String,
    #[serde(default)]
    pub options: Vec<String>,
}

/// The learner's answer to a [`PlanAskOp`], matched by `call_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanAnswerOp {
    pub op_id: String,
    pub at: String,
    pub call_id: String,
    pub answer: String,
}

/// An assumed-known section's check was missed (§4): the section loses
/// `assumedKnown` and the normal per-node gate applies. Written by code in
/// `verdict_record`; no planner call.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnownRefutedOp {
    pub op_id: String,
    pub at: String,
    pub section_id: String,
    pub question_id: String,
}

/// The learner switched a section's hunt on or off (docs/learning-system.md
/// §5). Only the learner may enable a hunt, so this is the only op that does;
/// replay applies the latest toggle per section onto the plan's `Hunt`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HuntToggleOp {
    pub op_id: String,
    pub at: String,
    pub section_id: String,
    pub enabled: bool,
}

/// The tutor's `hunt_send` (§5): the learner goes to find things out before
/// the section is taught. `hunt_id` is this tool call's id — one
/// conversation-visible id, never a second one.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HuntSendOp {
    pub op_id: String,
    pub at: String,
    pub hunt_id: String,
    pub section_id: String,
    pub question: String,
    #[serde(default)]
    pub look_at: Value,
    #[serde(default)]
    pub bring_back: Vec<String>,
}

/// The learner came back from a hunt (§5). Notes, links and files are all
/// optional: a tip remembered without its source is still what they learned.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HuntReturnOp {
    pub op_id: String,
    pub at: String,
    pub hunt_id: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub links: Vec<String>,
    #[serde(default)]
    pub files: Vec<String>,
}

/// The learner skipped the hunt (§5): allowed before or during, at no cost.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HuntSkipOp {
    pub op_id: String,
    pub at: String,
    pub hunt_id: String,
}

/// The tutor's `debrief_record` (§5): each claim it judged, with the sort it
/// gave it. Never evidence for the section's exit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebriefOp {
    pub op_id: String,
    pub at: String,
    pub hunt_id: String,
    #[serde(default)]
    pub claims: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApproveOp {
    pub op_id: String,
    pub at: String,
    pub plan_version: u32,
    pub removed_plugins: Vec<RemovedPlugin>,
}

/// The origin of a branching plan forked its other root tracks at approval
/// (architecture §4.26): `track` is the one it keeps, `children` the new
/// sessions, and `upTo` the last op they copied (the same cut a merge copies
/// later). Written once, right after the `approve` op.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchOp {
    pub op_id: String,
    pub at: String,
    pub track: String,
    pub plan_version: u32,
    pub up_to_at: String,
    pub up_to_op_id: String,
    pub children: Vec<BranchChild>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchChild {
    pub session_id: String,
    pub track: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemovedPlugin {
    pub section_id: String,
    pub plugin: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginGrantOp {
    pub op_id: String,
    pub at: String,
    pub section_id: String,
    pub plugin: String,
    /// plan | request | learner
    pub by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleOp {
    pub op_id: String,
    pub at: String,
    pub title: String,
}

/// A read root granted by a learner attachment (W3). Device-local paths in
/// the synced log are unavoidable here: the grant is meaningless without the
/// path, and another device simply finds it missing and asks again.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileGrantOp {
    pub op_id: String,
    pub at: String,
    pub path: String,
    /// file | folder
    pub kind: String,
}

/// A session attached to (or detached from) a learner folder. The absolute
/// path is device-local; only the folder name is synced so another device can
/// ask the learner to point at it (build prompt W4).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceOp {
    pub op_id: String,
    pub at: String,
    pub attached: bool,
    pub name: String,
}

/// One `workspace_write` (build prompt W4). The content is not stored, only
/// the hash the agent last saw or wrote, so learner edits are never clobbered.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceWriteOp {
    pub op_id: String,
    pub at: String,
    pub path: String,
    pub sha256: String,
    pub size: u64,
}

/// A short state summary a view posted (W7 learning hook). The core keeps
/// only the latest per artifact and writes it just before the learner's next
/// message; replay renders it on that message in the provider request. Other
/// view interaction is not logged.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewStateOp {
    pub op_id: String,
    pub at: String,
    pub artifact_id: String,
    pub summary: String,
}

/// A figure produced by a plugin tool (build prompt W6). Replay never re-runs
/// plugins: everything needed to display the artifact is here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactOp {
    pub op_id: String,
    pub at: String,
    pub artifact_id: String,
    pub plugin_id: String,
    pub plugin_version: String,
    pub package_hash: String,
    pub tool: String,
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub data: Value,
    pub fallback_svg: String,
    /// The plugin's `platforms` when the figure was made, so a device that
    /// doesn't have the plugin knows where it runs (docs/sync-build.md,
    /// "Sessions on mobile"). Absent on older ops.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platforms: Option<Vec<String>>,
}

/// Notes from a compaction turn that stand in for every conversation op with
/// `at <= cut_at` (architecture §4.15).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionOp {
    pub op_id: String,
    pub at: String,
    pub summary: String,
    /// The `at` of the newest op covered by this summary.
    pub cut_at: String,
    /// The `opId` of that op (ties on `at` are ordered by `opId`).
    pub cut_op_id: String,
}

/// One tool call is two ops, matched by `callId`: the call (persisted before
/// acting, `result` absent) and later its result (`args` omitted). An append-
/// only log can't fill in a result, and a pending call must survive restarts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolOp {
    pub op_id: String,
    pub at: String,
    pub call_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub args: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ToolResultValue>,
    /// `planner` for the planner's own loop (not part of the conversation);
    /// absent for the tutor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The plugin a tutor call went to (call ops only; absent for core tools
    /// and on older ops).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
    /// That plugin's `platforms` when the call was written (see
    /// [`ArtifactOp::platforms`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platforms: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultValue {
    pub content: String,
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobOp {
    pub op_id: String,
    pub at: String,
    pub job_id: String,
    /// `research` (a researcher job) or `plan` (a planner run).
    pub kind: String,
    /// queued | running | done | failed | aborted
    pub state: String,
    /// What the job was asked; persisted on `queued` so a job interrupted by
    /// an app restart can be started again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
    /// One-line description for the job chip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    /// Blocking jobs (the planner's `research`) return to their caller and
    /// are never delivered into the conversation.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub blocking: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The learner chose "Continue here anyway" on a session locked on mobile
/// because it used plugins that don't run there (docs/sync-build.md,
/// "Sessions on mobile"). Synced, so every phone and tablet follows the one
/// decision; desktop ignores it. `plugins` are the ids it unlocked: a
/// desktop-only plugin first used after it locks the session again.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileUnlockOp {
    pub op_id: String,
    pub at: String,
    #[serde(default)]
    pub plugins: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum Op {
    Session(Header),
    Message(StoredMessage),
    Phase(PhaseOp),
    Goal(RecordOp),
    Brief(RecordOp),
    Probe(RecordOp),
    Plan(RecordOp),
    PlanRequest(PlanRequestOp),
    PlanKeep(PlanKeepOp),
    PlanEdit(PlanEditOp),
    PlanAsk(PlanAskOp),
    PlanAnswer(PlanAnswerOp),
    KnownRefuted(KnownRefutedOp),
    HuntToggle(HuntToggleOp),
    HuntSend(HuntSendOp),
    HuntReturn(HuntReturnOp),
    HuntSkip(HuntSkipOp),
    Debrief(DebriefOp),
    Approve(ApproveOp),
    PluginGrant(PluginGrantOp),
    Tool(ToolOp),
    Job(JobOp),
    Compaction(CompactionOp),
    Title(TitleOp),
    Workspace(WorkspaceOp),
    WorkspaceWrite(WorkspaceWriteOp),
    Artifact(ArtifactOp),
    FileGrant(FileGrantOp),
    ViewState(ViewStateOp),
    MobileUnlock(MobileUnlockOp),
    Branch(BranchOp),
}

impl Op {
    pub fn at(&self) -> &str {
        match self {
            Op::Session(h) => &h.at,
            Op::Message(m) => &m.at,
            Op::Phase(o) => &o.at,
            Op::Goal(o) | Op::Brief(o) | Op::Probe(o) | Op::Plan(o) => &o.at,
            Op::PlanRequest(o) => &o.at,
            Op::PlanKeep(o) => &o.at,
            Op::PlanEdit(o) => &o.at,
            Op::PlanAsk(o) => &o.at,
            Op::PlanAnswer(o) => &o.at,
            Op::KnownRefuted(o) => &o.at,
            Op::HuntToggle(o) => &o.at,
            Op::HuntSend(o) => &o.at,
            Op::HuntReturn(o) => &o.at,
            Op::HuntSkip(o) => &o.at,
            Op::Debrief(o) => &o.at,
            Op::Approve(o) => &o.at,
            Op::PluginGrant(o) => &o.at,
            Op::Tool(o) => &o.at,
            Op::Job(o) => &o.at,
            Op::Compaction(o) => &o.at,
            Op::Title(o) => &o.at,
            Op::Workspace(o) => &o.at,
            Op::WorkspaceWrite(o) => &o.at,
            Op::Artifact(o) => &o.at,
            Op::FileGrant(o) => &o.at,
            Op::ViewState(o) => &o.at,
            Op::MobileUnlock(o) => &o.at,
            Op::Branch(o) => &o.at,
        }
    }

    pub fn op_id(&self) -> &str {
        match self {
            Op::Session(h) => &h.op_id,
            Op::Message(m) => &m.op_id,
            Op::Phase(o) => &o.op_id,
            Op::Goal(o) | Op::Brief(o) | Op::Probe(o) | Op::Plan(o) => &o.op_id,
            Op::PlanRequest(o) => &o.op_id,
            Op::PlanKeep(o) => &o.op_id,
            Op::PlanEdit(o) => &o.op_id,
            Op::PlanAsk(o) => &o.op_id,
            Op::PlanAnswer(o) => &o.op_id,
            Op::KnownRefuted(o) => &o.op_id,
            Op::HuntToggle(o) => &o.op_id,
            Op::HuntSend(o) => &o.op_id,
            Op::HuntReturn(o) => &o.op_id,
            Op::HuntSkip(o) => &o.op_id,
            Op::Debrief(o) => &o.op_id,
            Op::Approve(o) => &o.op_id,
            Op::PluginGrant(o) => &o.op_id,
            Op::Tool(o) => &o.op_id,
            Op::Job(o) => &o.op_id,
            Op::Compaction(o) => &o.op_id,
            Op::Title(o) => &o.op_id,
            Op::Workspace(o) => &o.op_id,
            Op::WorkspaceWrite(o) => &o.op_id,
            Op::Artifact(o) => &o.op_id,
            Op::FileGrant(o) => &o.op_id,
            Op::ViewState(o) => &o.op_id,
            Op::MobileUnlock(o) => &o.op_id,
            Op::Branch(o) => &o.op_id,
        }
    }

    /// Sort key: `at`, then `opId`.
    fn key(&self) -> (&str, &str) {
        (self.at(), self.op_id())
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub id: String,
    pub mode: Mode,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    /// Learn sessions from before the agent runtime (no `phase` op) open
    /// read-only, and on mobile so does one that used a desktop-only plugin.
    pub read_only: bool,
    /// Why `read_only` is set: [`READ_ONLY_LEGACY`] or `"desktop-plugin"`
    /// (the mobile lock, `mobile.rs`); `None` when writable.
    pub read_only_reason: Option<String>,
    /// For `"desktop-plugin"`: the display names of the plugins that caused
    /// the lock (title if known, else id). Empty otherwise.
    pub desktop_only_plugins: Vec<String>,
    /// Where this session sits in a branching plan (architecture §4.26):
    /// `None` for every session of a plan without tracks. The store fills
    /// what the header says; `Harness::list_sessions` completes it.
    pub lineage: Option<crate::branches::Lineage>,
}

/// [`SessionSummary::read_only_reason`] of a session from before the agent
/// runtime (or whose `phase` op is not second).
pub const READ_ONLY_LEGACY: &str = "legacy";

/// [`SessionSummary::read_only_reason`] for the store's own rule.
fn legacy_reason(read_only: bool) -> Option<String> {
    read_only.then(|| READ_ONLY_LEGACY.to_owned())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    #[serde(flatten)]
    pub summary: SessionSummary,
    pub messages: Vec<StoredMessage>,
    /// All ops, ordered by `at`, for state replay and the UI.
    pub ops: Vec<Op>,
    /// Lines this file had that did not parse and were skipped (a corrupt
    /// line, or an op kind written by a newer build). Zero for a healthy
    /// file; the UI can warn on it instead of the session failing to open.
    #[serde(default)]
    pub skipped_ops: usize,
}

pub struct Store {
    root: PathBuf,
    /// Device-local title index (`<config>/session-index.json`).
    index_path: PathBuf,
    index: Mutex<Option<HashMap<String, String>>>,
    /// Parsed ops (plus what the parse had to skip) of recently used
    /// sessions, most recent last, each with the file's length and mtime
    /// when it was read: a file that sync rewrote is read again.
    cache: Mutex<VecDeque<(String, FileStamp, ParsedOps)>>,
}

/// What one read of a session file produced.
#[derive(Debug, Clone)]
pub(crate) struct ParsedOps {
    /// Ops in replay order (`at`, then `opId`).
    pub(crate) ops: Vec<Op>,
    /// Lines that did not parse and were skipped; blank lines don't count.
    pub(crate) skipped: usize,
    /// The read-only rule for this file, see [`second_line_is_phase`].
    pub(crate) runtime: bool,
}

impl Store {
    /// `repo` is the synced data repo; `local_dir` is device-local (the title
    /// index is derived data and never synced).
    pub fn new(repo: &Path, local_dir: &Path) -> Self {
        Self {
            root: repo.join("sessions"),
            index_path: local_dir.join("session-index.json"),
            index: Mutex::new(None),
            cache: Mutex::new(VecDeque::new()),
        }
    }

    pub fn create(&self, mode: Mode, first_message: &str) -> Result<SessionSummary, Error> {
        let id = Ulid::new();
        let at = now_iso();
        let header = Header {
            op_id: op_id(),
            at: at.clone(),
            id: id.to_string(),
            mode,
            title: title_from(first_message),
            fork: None,
            merge: None,
        };
        let path = self.path(&id);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        append(&path, &Op::Session(header.clone()))?;
        // Learn sessions start in Measure (the phase machine's initial state);
        // the phase op on line 2 is also what marks them non-legacy.
        if mode == Mode::Learn {
            append(
                &path,
                &Op::Phase(PhaseOp {
                    op_id: op_id(),
                    at: now_iso(),
                    phase: Phase::Measure,
                    section_id: None,
                }),
            )?;
        }
        Ok(SessionSummary {
            id: header.id,
            mode,
            title: header.title,
            created_at: at.clone(),
            updated_at: at,
            read_only: false,
            read_only_reason: None,
            desktop_only_plugins: Vec::new(),
            lineage: None,
        })
    }

    /// Writes a whole new session file at once (a forked or merge session,
    /// architecture §4.26). `ops[0]` must be its header. The file appears
    /// complete or not at all (written aside, then renamed in), so a crash
    /// can't leave a half-copied session that blocks the real one. `Ok(None)`
    /// when a file with this id already exists: created earlier here, or by
    /// another device and synced.
    pub(crate) fn create_from(&self, ops: &[Op]) -> Result<Option<SessionSummary>, Error> {
        static CREATE: Mutex<()> = Mutex::new(());
        let Some(Op::Session(header)) = ops.first() else {
            return Err(Error::Invalid(
                "a new session starts with its header".into(),
            ));
        };
        let ulid = Ulid::from_string(&header.id)
            .map_err(|_| Error::Invalid(format!("`{}` is not a session id", header.id)))?;
        let path = self.path(&ulid);
        let Some(dir) = path.parent() else {
            return Err(Error::Invalid("no sessions folder".into()));
        };
        fs::create_dir_all(dir)?;
        let mut bytes = Vec::new();
        for op in ops {
            bytes.extend(serde_json::to_vec(op)?);
            bytes.push(b'\n');
        }
        let _one = lock(&CREATE);
        if path.exists() {
            return Ok(None);
        }
        let part = dir.join(format!(".{}.jsonl.part", header.id));
        {
            let mut file = fs::File::create(&part)?;
            file.write_all(&bytes)?;
            file.sync_data()?;
        }
        fs::rename(&part, &path)?;
        if let Some(title) = ops.iter().rev().find_map(|o| match o {
            Op::Title(t) => Some(t.title.as_str()),
            _ => None,
        }) {
            self.set_indexed_title(&header.id, title);
        }
        self.summary(&header.id).map(Some)
    }

    /// Whether a session file with this id exists on this device.
    pub(crate) fn exists(&self, id: &str) -> bool {
        self.existing_path(id).is_ok()
    }

    pub fn append(&self, id: &str, message: &StoredMessage) -> Result<(), Error> {
        self.append_op(id, &Op::Message(message.clone()))
    }

    /// Append any op (one write per op keeps lines whole under O_APPEND) and
    /// keep the cached op list in step.
    pub fn append_op(&self, id: &str, op: &Op) -> Result<(), Error> {
        let path = self.existing_path(id)?;
        let before = stamp(&path);
        append(&path, op)?;
        let after = stamp(&path);
        let mut cache = lock(&self.cache);
        if let Some(pos) = cache.iter().position(|(k, _, _)| k == id) {
            // Only an entry that matched the file before this write can take
            // the op incrementally; anything else (sync wrote the file) is
            // dropped and read again on next use.
            match (&mut cache[pos], before, after) {
                ((_, cached, parsed), Some(before), Some(after)) if *cached == before => {
                    let at = parsed.ops.partition_point(|o| o.key() <= op.key());
                    parsed.ops.insert(at, op.clone());
                    *cached = after;
                }
                _ => {
                    cache.remove(pos);
                }
            }
        }
        drop(cache);
        if let Op::Title(t) = op {
            self.set_indexed_title(id, &t.title);
        }
        Ok(())
    }

    pub fn load(&self, id: &str) -> Result<Session, Error> {
        let path = self.existing_path(id)?;
        let mut session = session_from_ops(&path, self.parsed_ops(id)?)?;
        if let Some(title) = self.indexed_title(id) {
            session.summary.title = title;
        }
        Ok(session)
    }

    /// All ops for a session, sorted by `at` (ties by `opId`). Served from the
    /// LRU after the first read.
    pub fn ops(&self, id: &str) -> Result<Vec<Op>, Error> {
        Ok(self.parsed_ops(id)?.ops)
    }

    /// As [`Store::ops`], with the parse report (skipped lines and the
    /// read-only rule). Cached together with the ops.
    pub(crate) fn parsed_ops(&self, id: &str) -> Result<ParsedOps, Error> {
        let path = self.existing_path(id)?;
        let current = stamp(&path);
        {
            let mut cache = lock(&self.cache);
            if let Some(pos) = cache.iter().position(|(k, _, _)| k == id) {
                if let Some(entry) = cache.remove(pos) {
                    if current.is_some_and(|now| now == entry.1) {
                        let parsed = entry.2.clone();
                        cache.push_back(entry);
                        return Ok(parsed);
                    }
                }
            }
        }
        let parsed = read_file(&path)?;
        let Some(read_at) = current else {
            return Ok(parsed);
        };
        let mut cache = lock(&self.cache);
        if !cache.iter().any(|(k, _, _)| k == id) {
            cache.push_back((id.to_owned(), read_at, parsed.clone()));
            while cache.len() > LRU_SESSIONS {
                cache.pop_front();
            }
        }
        Ok(parsed)
    }

    pub fn summary(&self, id: &str) -> Result<SessionSummary, Error> {
        let path = self.existing_path(id)?;
        let mut s = read_header(&path)?
            .ok_or_else(|| Error::Corrupt(format!("{} has no session header", path.display())))?;
        if let Some(title) = self.indexed_title(id) {
            s.title = title;
        }
        Ok(s)
    }

    pub fn delete(&self, id: &str) -> Result<(), Error> {
        fs::remove_file(self.existing_path(id)?)?;
        lock(&self.cache).retain(|(k, _, _)| k != id);
        Ok(())
    }

    /// Length and mtime (ns since the epoch) of a session file: the key of a
    /// device-local cache derived from it (`mobile.rs`).
    pub(crate) fn file_stamp(&self, id: &str) -> Result<(u64, u64), Error> {
        let path = self.existing_path(id)?;
        stamp(&path).ok_or_else(|| Error::Corrupt(format!("{} can't be read", path.display())))
    }
}

/// A session file's length and mtime (ns since the epoch).
type FileStamp = (u64, u64);

fn stamp(path: &Path) -> Option<FileStamp> {
    let meta = fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
    Some((meta.len(), mtime))
}

impl Store {
    /// A session's ops read from its file, leaving the LRU alone: a scan
    /// over every session must not evict the ones in use.
    pub(crate) fn read_uncached(&self, id: &str) -> Result<Vec<Op>, Error> {
        Ok(read_file(&self.existing_path(id)?)?.ops)
    }

    /// Newest activity first. Reads only the first two lines + mtime (M7).
    pub fn list(&self) -> Result<Vec<SessionSummary>, Error> {
        let mut out = Vec::new();
        for path in self.files()? {
            if let Some(summary) = read_header(&path)? {
                out.push(summary);
            }
        }
        self.ensure_index(&out);
        if let Some(index) = lock(&self.index).as_ref() {
            for s in &mut out {
                if let Some(t) = index.get(&s.id) {
                    s.title.clone_from(t);
                }
            }
        }
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(out)
    }

    fn files(&self) -> Result<Vec<PathBuf>, Error> {
        let mut out = Vec::new();
        let years = match fs::read_dir(&self.root) {
            Ok(dir) => dir,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        for year in years {
            let year = year?.path();
            if !year.is_dir() {
                continue;
            }
            for entry in fs::read_dir(year)? {
                let path = entry?.path();
                if path.extension().is_some_and(|e| e == "jsonl") {
                    out.push(path);
                }
            }
        }
        Ok(out)
    }

    // ---- title index ----------------------------------------------------

    fn indexed_title(&self, id: &str) -> Option<String> {
        self.load_index();
        lock(&self.index).as_ref()?.get(id).cloned()
    }

    fn set_indexed_title(&self, id: &str, title: &str) {
        self.load_index();
        let mut guard = lock(&self.index);
        let index = guard.get_or_insert_with(HashMap::new);
        index.insert(id.to_owned(), title.to_owned());
        // The index is derived; failing to write it only costs a rebuild.
        if let Err(e) = write_index(&self.index_path, index) {
            tracing::debug!("session index not written: {e}");
        }
    }

    /// Load the index file if present (not rebuilding it).
    fn load_index(&self) {
        let mut guard = lock(&self.index);
        if guard.is_some() {
            return;
        }
        if let Ok(text) = fs::read_to_string(&self.index_path) {
            if let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&text) {
                *guard = Some(map);
            }
        }
    }

    /// Rebuild the index when it is missing: one full scan for `title` ops.
    fn ensure_index(&self, sessions: &[SessionSummary]) {
        self.load_index();
        let mut guard = lock(&self.index);
        if guard.is_some() {
            return;
        }
        let mut map = HashMap::new();
        for s in sessions {
            let Ok(path) = self.existing_path(&s.id) else {
                continue;
            };
            if let Ok(parsed) = read_file(&path) {
                if let Some(t) = parsed.ops.iter().rev().find_map(|o| match o {
                    Op::Title(t) => Some(t.title.clone()),
                    _ => None,
                }) {
                    map.insert(s.id.clone(), t);
                }
            }
        }
        if let Err(e) = write_index(&self.index_path, &map) {
            tracing::debug!("session index not written: {e}");
        }
        *guard = Some(map);
    }

    fn path(&self, id: &Ulid) -> PathBuf {
        let year = DateTime::<Utc>::from(id.datetime()).year();
        self.root.join(year.to_string()).join(format!("{id}.jsonl"))
    }

    /// Parsing as a ULID also rules out path traversal through the id.
    fn existing_path(&self, id: &str) -> Result<PathBuf, Error> {
        let ulid = Ulid::from_string(id).map_err(|_| Error::NotFound(format!("session {id}")))?;
        let path = self.path(&ulid);
        if path.exists() {
            Ok(path)
        } else {
            Err(Error::NotFound(format!("session {id}")))
        }
    }
}

/// A fresh op id. Monotonic within the process, so two ops written in the same
/// millisecond still replay in the order they were written.
pub(crate) fn op_id() -> String {
    static GENERATOR: Mutex<ulid::Generator> = Mutex::new(ulid::Generator::new());
    let mut generator = lock(&GENERATOR);
    generator
        .generate()
        .unwrap_or_else(|_| Ulid::new())
        .to_string()
}

pub(crate) fn new_message(role: Role, content: String) -> StoredMessage {
    StoredMessage {
        op_id: op_id(),
        at: now_iso(),
        role,
        content,
        thinking: String::new(),
        model: None,
        ending: None,
        tag: None,
        job_id: None,
        native: Vec::new(),
        attachments: Vec::new(),
    }
}

/// Builds a record op with a fresh opId + timestamp.
pub(crate) fn record(payload: Value) -> RecordOp {
    RecordOp {
        op_id: op_id(),
        at: now_iso(),
        payload,
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn title_from(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("Untitled");
    match line.char_indices().nth(80) {
        Some((i, _)) => format!("{}…", line[..i].trim_end()),
        None => line.to_owned(),
    }
}

/// Appends one op as one line. `sync_data` afterwards means a crash loses at
/// most the op in flight, not the whole tail of the file. The cost is one
/// fsync per op, and ops arrive tens per turn (never per streaming delta), so
/// this is not on any hot path.
fn append(path: &Path, op: &Op) -> Result<(), Error> {
    let mut line = serde_json::to_vec(op)?;
    line.push(b'\n');
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(&line)?;
    file.sync_data()?;
    Ok(())
}

/// Trims ASCII whitespace (spaces, `\r`, ...) off a raw line.
fn trim_ws(line: &[u8]) -> &[u8] {
    let blank = u8::is_ascii_whitespace;
    let start = line.iter().position(|b| !blank(b)).unwrap_or(line.len());
    let end = line
        .iter()
        .rposition(|b| !blank(b))
        .map_or(start, |i| i + 1);
    &line[start..end]
}

/// First 80 characters of a line, for a warning that stays one line long.
fn preview(line: &[u8]) -> String {
    let head = String::from_utf8_lossy(&line[..line.len().min(160)]);
    head.chars().take(80).collect()
}

/// The one read-only rule, deliberately narrow.
///
/// A session is a *runtime* session - one the tutor may append to - iff its
/// **second op is a `Phase` op**. [`Store::create`] writes exactly that shape
/// (`session`, then `phase`).
///
/// Everything else that is a Learn session is read-only:
///
/// * legacy files, from before the agent runtime, have no `phase` op at all;
/// * a merged or hand-edited file whose `phase` op sits anywhere but second is
///   read-only too: replay would otherwise enter the phase machine at an
///   arbitrary point, so we refuse rather than guess.
///
/// This deliberately tests the second *line*, not the second op of the sorted
/// replay order: `list` reads two lines while `load` sorts every op, and both
/// go through this function so the UI and the write path cannot disagree.
///
/// Takes the lines of a session file from its start. Returns `None` when the
/// first line is not a header op (nothing can open such a file), else the
/// header and whether the session is a runtime session.
fn scan_head<I, S>(lines: I) -> Option<(Header, bool)>
where
    I: Iterator<Item = S>,
    S: AsRef<[u8]>,
{
    let mut non_blank = 0usize;
    let mut header: Option<Header> = None;
    let mut runtime = false;
    for line in lines {
        let raw = trim_ws(line.as_ref());
        if raw.is_empty() {
            continue;
        }
        non_blank += 1;
        if non_blank == 1 {
            match serde_json::from_slice::<Op>(raw) {
                Ok(Op::Session(h)) => header = Some(h),
                _ => return None,
            }
        } else {
            runtime = matches!(serde_json::from_slice::<Op>(raw), Ok(Op::Phase(_)));
            break;
        }
    }
    header.map(|h| (h, runtime))
}

/// Heals a file whose last line a crash cut short.
///
/// One `write_all` per op means at most the tail can be torn: the complete
/// prefix is always whole lines. A tail that does not parse is dropped at the
/// last newline (and only that tail); a tail that *does* parse only gets the
/// newline it lost, so the next append cannot glue two ops together. No other
/// byte of the file is ever rewritten.
fn repair_torn_tail(path: &Path, bytes: &mut Vec<u8>) -> Result<(), Error> {
    if bytes.is_empty() || bytes.last() == Some(&b'\n') {
        return Ok(());
    }
    let last_nl = bytes.iter().rposition(|&b| b == b'\n');
    let tail_start = last_nl.map_or(0, |i| i + 1);
    let tail = trim_ws(&bytes[tail_start..]);
    if serde_json::from_slice::<Op>(tail).is_ok() {
        // A complete op that only lost its newline: add it, so the next append
        // cannot glue two ops into one line. Never truncate a complete op.
        let repaired = OpenOptions::new()
            .append(true)
            .open(path)
            .and_then(|mut f| {
                f.write_all(b"\n")?;
                f.sync_data()
            });
        match repaired {
            Ok(()) => {
                bytes.push(b'\n');
                tracing::warn!(
                    path = %path.display(),
                    "session file's last op had no newline; added it"
                );
            }
            // Read-only mount, say: the op is still readable, so carry on.
            Err(e) => tracing::warn!(
                path = %path.display(),
                "session file's last op has no newline and could not be fixed: {e}"
            ),
        }
        return Ok(());
    }
    let Some(last_nl) = last_nl else {
        // One incomplete line and nothing before it: truncating would leave an
        // empty file behind, so let it be reported as a skipped line instead.
        return Ok(());
    };
    let dropped = bytes.len() - last_nl - 1;
    match OpenOptions::new().append(true).open(path).and_then(|f| {
        f.set_len(u64::try_from(last_nl + 1).unwrap_or(u64::MAX))?;
        f.sync_data()
    }) {
        Ok(()) => {
            bytes.truncate(last_nl + 1);
            tracing::warn!(
                path = %path.display(),
                dropped,
                "session file ends in a torn op; truncated to the last complete line"
            );
        }
        // Not fatal: the torn line is skipped below either way.
        Err(e) => tracing::warn!(
            path = %path.display(),
            dropped,
            "session file ends in a torn op that could not be truncated: {e}"
        ),
    }
    Ok(())
}

/// Reads a session file, skipping and reporting lines that do not parse.
///
/// One bad line (a corrupt write, a hand edit, or an op kind from a newer
/// build) must never cost the learner the whole session, so it is skipped and
/// counted instead of failing the read. Blank lines are not corruption and
/// stay silent.
fn read_file(path: &Path) -> Result<ParsedOps, Error> {
    let mut bytes = fs::read(path)?;
    repair_torn_tail(path, &mut bytes)?;
    let runtime = scan_head(bytes.split(|&b| b == b'\n')).is_some_and(|(_, runtime)| runtime);
    let mut ops: Vec<Op> = Vec::new();
    let mut skipped = 0usize;
    let mut first_bad = String::new();
    for line in bytes.split(|&b| b == b'\n') {
        let line = trim_ws(line);
        if line.is_empty() {
            continue;
        }
        match serde_json::from_slice::<Op>(line) {
            Ok(op) => ops.push(op),
            Err(_) => {
                skipped += 1;
                if first_bad.is_empty() {
                    first_bad = preview(line);
                }
            }
        }
    }
    if skipped > 0 {
        tracing::warn!(
            path = %path.display(),
            skipped,
            first = %first_bad,
            "session ops that do not parse were skipped"
        );
    }
    // Lines from merged devices may interleave; `at` is the order of record.
    ops.sort_by(|a, b| a.key().cmp(&b.key()));
    Ok(ParsedOps {
        ops,
        skipped,
        runtime,
    })
}

fn session_from_ops(path: &Path, parsed: ParsedOps) -> Result<Session, Error> {
    let ParsedOps {
        ops,
        skipped,
        runtime,
    } = parsed;
    let header = ops
        .iter()
        .find_map(|op| match op {
            Op::Session(h) => Some(h.clone()),
            _ => None,
        })
        .ok_or_else(|| Error::Corrupt(format!("{} has no session header", path.display())))?;
    let messages: Vec<StoredMessage> = ops
        .iter()
        .filter_map(|op| match op {
            Op::Message(m) => Some(m.clone()),
            _ => None,
        })
        .collect();
    let updated_at = ops
        .last()
        .map_or_else(|| header.at.clone(), |o| o.at().to_owned());
    let title = ops
        .iter()
        .rev()
        .find_map(|o| match o {
            Op::Title(t) => Some(t.title.clone()),
            _ => None,
        })
        .unwrap_or_else(|| header.title.clone());
    let read_only = header.mode == Mode::Learn && !runtime;
    let lineage = crate::branches::Lineage::from_header(&header);
    Ok(Session {
        summary: SessionSummary {
            id: header.id,
            mode: header.mode,
            title,
            created_at: header.at,
            updated_at,
            read_only,
            read_only_reason: legacy_reason(read_only),
            desktop_only_plugins: Vec::new(),
            lineage,
        },
        messages,
        ops,
        skipped_ops: skipped,
    })
}

/// Reads only the first two non-blank lines (header, then the initial `phase`
/// op of a runtime Learn session) for `list`, via the same [`scan_head`] rule
/// `load` uses; `updated_at` comes from the file mtime.
fn read_header(path: &Path) -> Result<Option<SessionSummary>, Error> {
    let lines = first_lines(path, 2)?;
    let Some((h, runtime)) = scan_head(lines.iter().map(Vec::as_slice)) else {
        return Ok(None);
    };
    let updated = fs::metadata(path)?
        .modified()
        .ok()
        .map(DateTime::<Utc>::from)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_else(|| h.at.clone());
    let read_only = h.mode == Mode::Learn && !runtime;
    let lineage = crate::branches::Lineage::from_header(&h);
    Ok(Some(SessionSummary {
        id: h.id,
        mode: h.mode,
        title: h.title,
        created_at: h.at,
        updated_at: updated,
        read_only,
        read_only_reason: legacy_reason(read_only),
        desktop_only_plugins: Vec::new(),
        lineage,
    }))
}

/// The first `want` non-blank lines of a file, as raw bytes: `list` never
/// reads a session body, and invalid UTF-8 in a torn line must not matter.
fn first_lines(path: &Path, want: usize) -> Result<Vec<Vec<u8>>, Error> {
    let mut reader = std::io::BufReader::new(fs::File::open(path)?);
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut buf = Vec::new();
    while out.len() < want {
        buf.clear();
        if reader.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        if !trim_ws(&buf).is_empty() {
            out.push(buf.clone());
        }
    }
    Ok(out)
}

fn write_index(path: &Path, index: &HashMap<String, String>) -> Result<(), Error> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec(index)?)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lh-test-{name}-{}", Ulid::new()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn store(repo: &Path) -> Store {
        Store::new(repo, &repo.join("local"))
    }

    #[test]
    fn messages_survive_reload_in_time_order_and_list_is_newest_first() {
        let repo = temp_repo("store");
        let store = store(&repo);
        let older = store.create(Mode::Learn, "first").unwrap();
        let newer = store.create(Mode::Practice, "second").unwrap();

        let mut late = new_message(Role::Assistant, "late".into());
        late.at = "2999-01-01T00:00:00.000Z".into();
        let early = new_message(Role::User, "early".into());
        store.append(&older.id, &late).unwrap();
        store.append(&older.id, &early).unwrap();

        // Both the cached and the on-disk order put `early` first.
        for s in [store.load(&older.id).unwrap(), {
            let fresh = Store::new(&repo, &repo.join("local"));
            fresh.load(&older.id).unwrap()
        }] {
            let contents: Vec<&str> = s.messages.iter().map(|m| m.content.as_str()).collect();
            assert_eq!(contents, ["early", "late"]);
            assert!(!s.summary.read_only);
        }
        assert!(!store.load(&newer.id).unwrap().summary.read_only);

        let ids: Vec<String> = store.list().unwrap().into_iter().map(|s| s.id).collect();
        assert_eq!(ids.len(), 2);
        store.delete(&older.id).unwrap();
        assert!(matches!(store.load(&older.id), Err(Error::NotFound(_))));
        fs::remove_dir_all(repo).unwrap();
    }

    #[test]
    fn a_file_rewritten_behind_the_cache_is_read_again() {
        let repo = temp_repo("stale");
        let store = store(&repo);
        let s = store.create(Mode::Learn, "first").unwrap();
        store
            .append(&s.id, &new_message(Role::User, "mine".into()))
            .unwrap();
        assert_eq!(store.load(&s.id).unwrap().messages.len(), 1, "now cached");

        // Sync writes the file (another device's op), not through this store.
        let sync = Store::new(&repo, &repo.join("other"));
        sync.append(
            &s.id,
            &new_message(Role::Assistant, "from the phone".into()),
        )
        .unwrap();
        let seen = |store: &Store| -> Vec<String> {
            store
                .load(&s.id)
                .unwrap()
                .messages
                .into_iter()
                .map(|m| m.content)
                .collect()
        };
        assert_eq!(seen(&store), ["mine", "from the phone"]);

        // A later local append keeps both, cached and on disk.
        store
            .append(&s.id, &new_message(Role::User, "again".into()))
            .unwrap();
        assert_eq!(seen(&store), ["mine", "from the phone", "again"]);
        assert_eq!(seen(&Store::new(&repo, &repo.join("local"))), seen(&store));
        fs::remove_dir_all(repo).unwrap();
    }

    #[test]
    fn ops_written_in_one_millisecond_replay_in_write_order() {
        let repo = temp_repo("order");
        let store = store(&repo);
        let s = store.create(Mode::Learn, "x").unwrap();
        let at = "2030-01-01T00:00:00.000Z".to_owned();
        for i in 0..50 {
            let mut m = new_message(Role::User, i.to_string());
            m.at.clone_from(&at);
            store.append(&s.id, &m).unwrap();
        }
        let fresh = Store::new(&repo, &repo.join("local"));
        let got: Vec<String> = fresh
            .load(&s.id)
            .unwrap()
            .messages
            .into_iter()
            .map(|m| m.content)
            .collect();
        let want: Vec<String> = (0..50).map(|i| i.to_string()).collect();
        assert_eq!(got, want);
        fs::remove_dir_all(repo).unwrap();
    }

    #[test]
    fn titles_from_title_ops_show_in_list_and_survive_a_lost_index() {
        let repo = temp_repo("titles");
        let store = store(&repo);
        let s = store.create(Mode::Learn, "first message").unwrap();
        store
            .append_op(
                &s.id,
                &Op::Title(TitleOp {
                    op_id: op_id(),
                    at: now_iso(),
                    title: "Voltage dividers".into(),
                }),
            )
            .unwrap();
        assert_eq!(store.list().unwrap()[0].title, "Voltage dividers");
        fs::remove_file(repo.join("local/session-index.json")).unwrap();
        let fresh = Store::new(&repo, &repo.join("local"));
        assert_eq!(fresh.list().unwrap()[0].title, "Voltage dividers");
        fs::remove_dir_all(repo).unwrap();
    }

    #[test]
    fn legacy_learn_session_without_phase_is_read_only() {
        let repo = temp_repo("legacy");
        let dir = repo.join("sessions/2026");
        fs::create_dir_all(&dir).unwrap();
        let id = Ulid::new().to_string();
        let line = format!(
            "{{\"op\":\"session\",\"opId\":\"{}\",\"at\":\"2026-01-01T00:00:00.000Z\",\"id\":\"{}\",\"mode\":\"learn\",\"title\":\"old\"}}\n",
            Ulid::new(),
            id
        );
        // Ids are ULIDs, whose timestamp picks the year directory.
        let year = DateTime::<Utc>::from(Ulid::from_string(&id).unwrap().datetime()).year();
        let dir = dir.parent().unwrap().join(year.to_string());
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{id}.jsonl")), line).unwrap();
        let store = store(&repo);
        let s = store.load(&id).unwrap();
        assert!(s.summary.read_only);
        assert_eq!(
            s.summary.read_only_reason.as_deref(),
            Some(READ_ONLY_LEGACY)
        );
        let listed = &store.list().unwrap()[0];
        assert!(listed.read_only);
        assert_eq!(listed.read_only_reason.as_deref(), Some(READ_ONLY_LEGACY));
        fs::remove_dir_all(repo).unwrap();
    }

    #[test]
    fn ids_that_are_not_ulids_never_touch_the_filesystem() {
        let store = Store::new(Path::new("/nonexistent"), Path::new("/nonexistent"));
        assert!(matches!(
            store.load("../../etc/passwd"),
            Err(Error::NotFound(_))
        ));
    }

    // ---- crash safety and forward compatibility ------------------------

    /// Writes raw bytes as a session file under the year dir its id picks.
    fn raw_file(repo: &Path, id: &str, body: &[u8]) -> PathBuf {
        let year = DateTime::<Utc>::from(Ulid::from_string(id).unwrap().datetime()).year();
        let dir = repo.join("sessions").join(year.to_string());
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{id}.jsonl"));
        fs::write(&path, body).unwrap();
        path
    }

    fn header_line(id: &str) -> String {
        let op = Op::Session(Header {
            op_id: op_id(),
            at: "2026-01-01T00:00:00.000Z".into(),
            id: id.into(),
            mode: Mode::Learn,
            title: "t".into(),
            fork: None,
            merge: None,
        });
        format!("{}\n", serde_json::to_string(&op).unwrap())
    }

    fn phase_line(at: &str) -> String {
        let op = Op::Phase(PhaseOp {
            op_id: op_id(),
            at: at.into(),
            phase: Phase::Measure,
            section_id: None,
        });
        format!("{}\n", serde_json::to_string(&op).unwrap())
    }

    fn message_line(content: &str, at: &str) -> String {
        let mut m = new_message(Role::User, content.into());
        m.at = at.into();
        format!("{}\n", serde_json::to_string(&Op::Message(m)).unwrap())
    }

    #[test]
    fn a_torn_final_line_is_repaired_and_the_complete_ops_open() {
        let repo = temp_repo("torn");
        let id = Ulid::new().to_string();
        let complete = format!(
            "{}{}{}",
            header_line(&id),
            phase_line("2026-01-01T00:00:01.000Z"),
            message_line("whole", "2026-01-01T00:00:02.000Z"),
        );
        // A crash in the middle of `write_all`: the tail is half an op and
        // never got its newline.
        let torn = message_line("half", "2026-01-01T00:00:03.000Z");
        let mut body = complete.clone().into_bytes();
        body.extend_from_slice(&torn.as_bytes()[..torn.len() / 2]);
        let path = raw_file(&repo, &id, &body);

        // Before the fix this was `Err`: one unparseable line sank the session.
        let store = store(&repo);
        let s = store.load(&id).unwrap();
        let contents: Vec<&str> = s.messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, ["whole"]);
        assert_eq!(s.skipped_ops, 0, "a torn tail is repaired, not skipped");
        assert!(!s.summary.read_only);
        // The tail was dropped on disk; not one other byte changed.
        assert_eq!(fs::read(&path).unwrap(), complete.as_bytes());
        fs::remove_dir_all(repo).unwrap();
    }

    #[test]
    fn a_tail_that_parses_but_lost_its_newline_keeps_its_op() {
        let repo = temp_repo("nonewline");
        let id = Ulid::new().to_string();
        let last = message_line("last", "2026-01-01T00:00:02.000Z");
        let complete = format!(
            "{}{}{}",
            header_line(&id),
            phase_line("2026-01-01T00:00:01.000Z"),
            last,
        );
        let path = raw_file(&repo, &id, complete.trim_end_matches('\n').as_bytes());

        let store = store(&repo);
        let s = store.load(&id).unwrap();
        let contents: Vec<&str> = s.messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, ["last"], "a complete op is never truncated");
        assert_eq!(s.skipped_ops, 0);
        // Only the missing newline was added, so the next append stays whole.
        assert_eq!(fs::read(&path).unwrap(), complete.as_bytes());
        store
            .append(&id, &new_message(Role::User, "next".into()))
            .unwrap();
        let fresh = Store::new(&repo, &repo.join("local"));
        let after = fresh.parsed_ops(&id).unwrap();
        assert_eq!(after.skipped, 0);
        assert_eq!(after.ops.len(), 4);
        fs::remove_dir_all(repo).unwrap();
    }

    #[test]
    fn a_corrupt_middle_line_is_skipped_counted_and_the_rest_replays() {
        let repo = temp_repo("corrupt");
        let id = Ulid::new().to_string();
        let body = format!(
            "{}{}{}{}{}",
            header_line(&id),
            phase_line("2026-01-01T00:00:01.000Z"),
            message_line("one", "2026-01-01T00:00:02.000Z"),
            "{\"op\":\"message\",\"opId\":\"torn-mid-write\n",
            message_line("two", "2026-01-01T00:00:04.000Z"),
        );
        let path = raw_file(&repo, &id, body.as_bytes());

        let store = store(&repo);
        let s = store.load(&id).unwrap();
        let contents: Vec<&str> = s.messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, ["one", "two"], "the rest of the log replays");
        assert_eq!(s.skipped_ops, 1);
        assert!(
            !s.summary.read_only,
            "a damaged line is not damage to the header"
        );
        // A skipped line is never rewritten.
        assert_eq!(fs::read(&path).unwrap(), body.as_bytes());
        // A fresh store (no LRU) reports the same count.
        let fresh = Store::new(&repo, &repo.join("local"));
        assert_eq!(fresh.parsed_ops(&id).unwrap().skipped, 1);
        fs::remove_dir_all(repo).unwrap();
    }

    #[test]
    fn an_op_kind_from_a_newer_build_is_skipped_not_fatal() {
        let repo = temp_repo("future");
        let id = Ulid::new().to_string();
        // Forward compatibility: a newer build may write an op kind this one
        // has never heard of. It must cost that one op, nothing more.
        let body = format!(
            "{}{}{}{}",
            header_line(&id),
            phase_line("2026-01-01T00:00:01.000Z"),
            "{\"op\":\"futureThing\",\"opId\":\"01J00000000000000000000002\",\"at\":\"2026-01-01T00:00:02.000Z\",\"payload\":1}\n",
            message_line("still here", "2026-01-01T00:00:03.000Z"),
        );
        raw_file(&repo, &id, body.as_bytes());

        let s = store(&repo).load(&id).unwrap();
        assert_eq!(s.skipped_ops, 1);
        assert_eq!(s.messages.len(), 1);
        assert_eq!(s.messages[0].content, "still here");
        // The session stays writable: the unknown op was line 3, not line 2.
        assert!(!s.summary.read_only);
        fs::remove_dir_all(repo).unwrap();
    }

    #[test]
    fn a_phase_op_anywhere_but_second_is_read_only_for_list_and_load() {
        let repo = temp_repo("merged");
        let id = Ulid::new().to_string();
        // Merged/hand-edited: `phase` is on line 3. Its `at` would even sort it
        // second - the rule is the second *line*, deliberately, so both readers
        // agree instead of one trusting sort order and the other line order.
        let body = format!(
            "{}{}{}",
            header_line(&id),
            message_line("hi", "2026-01-01T00:00:01.000Z"),
            phase_line("2026-01-01T00:00:00.500Z"),
        );
        raw_file(&repo, &id, body.as_bytes());

        let store = store(&repo);
        let s = store.load(&id).unwrap();
        assert!(s.summary.read_only);
        assert!(
            !store.parsed_ops(&id).unwrap().runtime,
            "not a runtime session"
        );
        assert!(store.list().unwrap()[0].read_only);
        fs::remove_dir_all(repo).unwrap();
    }

    #[test]
    fn a_normal_append_reads_back_byte_identical_ops() {
        let repo = temp_repo("roundtrip");
        let store = store(&repo);
        let s = store.create(Mode::Learn, "voltage").unwrap();
        store
            .append(&s.id, &new_message(Role::User, "hi".into()))
            .unwrap();
        let path = store.existing_path(&s.id).unwrap();
        let bytes = fs::read(&path).unwrap();
        assert_eq!(bytes.last(), Some(&b'\n'));
        let want: Vec<u8> = store
            .ops(&s.id)
            .unwrap()
            .iter()
            .flat_map(|op| {
                let mut line = serde_json::to_vec(op).unwrap();
                line.push(b'\n');
                line
            })
            .collect();
        assert_eq!(bytes, want, "appends round-trip byte for byte");
        // Reading is side-effect free for a healthy file.
        let parsed = store.parsed_ops(&s.id).unwrap();
        assert_eq!(parsed.skipped, 0);
        assert!(parsed.runtime);
        assert_eq!(parsed.ops.len(), 3);
        assert_eq!(fs::read(&path).unwrap(), bytes);
        fs::remove_dir_all(repo).unwrap();
    }

    #[test]
    fn title_is_first_nonblank_line_capped() {
        assert_eq!(title_from("\n  Derivatives  \nmore"), "Derivatives");
        let long = "x".repeat(100);
        assert_eq!(title_from(&long).chars().count(), 81);
    }
}
