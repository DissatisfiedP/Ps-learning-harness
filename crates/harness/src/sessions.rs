//! Sessions as append-only op logs: `sessions/<yyyy>/<ulid>.jsonl` in the data
//! repo (architecture §4.17, §5). Each line is one op; readers order by `at`
//! (ties by `opId`, which is monotonic within a process, see [`op_id`]).
//!
//! The op set is the target set from §5 `SessionOp`. Every op carries `opId`
//! (ULID) and `at`. Derived [`crate::state::SessionState`] is rebuilt by
//! replaying these ops and is never stored.
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Header {
    pub op_id: String,
    pub at: String,
    pub id: String,
    pub mode: Mode,
    pub title: String,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApproveOp {
    pub op_id: String,
    pub at: String,
    pub plan_version: u32,
    pub removed_plugins: Vec<RemovedPlugin>,
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
    Approve(ApproveOp),
    PluginGrant(PluginGrantOp),
    Tool(ToolOp),
    Job(JobOp),
    Compaction(CompactionOp),
    Title(TitleOp),
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
            Op::Approve(o) => &o.at,
            Op::PluginGrant(o) => &o.at,
            Op::Tool(o) => &o.at,
            Op::Job(o) => &o.at,
            Op::Compaction(o) => &o.at,
            Op::Title(o) => &o.at,
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
            Op::Approve(o) => &o.op_id,
            Op::PluginGrant(o) => &o.op_id,
            Op::Tool(o) => &o.op_id,
            Op::Job(o) => &o.op_id,
            Op::Compaction(o) => &o.op_id,
            Op::Title(o) => &o.op_id,
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
    /// read-only.
    pub read_only: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    #[serde(flatten)]
    pub summary: SessionSummary,
    pub messages: Vec<StoredMessage>,
    /// All ops, ordered by `at`, for state replay and the UI.
    pub ops: Vec<Op>,
}

pub struct Store {
    root: PathBuf,
    /// Device-local title index (`<config>/session-index.json`).
    index_path: PathBuf,
    index: Mutex<Option<HashMap<String, String>>>,
    /// Parsed, sorted ops of recently used sessions, most recent last.
    cache: Mutex<VecDeque<(String, Vec<Op>)>>,
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
        })
    }

    pub fn append(&self, id: &str, message: &StoredMessage) -> Result<(), Error> {
        self.append_op(id, &Op::Message(message.clone()))
    }

    /// Append any op (one write per op keeps lines whole under O_APPEND) and
    /// keep the cached op list in step.
    pub fn append_op(&self, id: &str, op: &Op) -> Result<(), Error> {
        let path = self.existing_path(id)?;
        append(&path, op)?;
        let mut cache = lock(&self.cache);
        if let Some((_, ops)) = cache.iter_mut().find(|(k, _)| k == id) {
            let pos = ops.partition_point(|o| o.key() <= op.key());
            ops.insert(pos, op.clone());
        }
        drop(cache);
        if let Op::Title(t) = op {
            self.set_indexed_title(id, &t.title);
        }
        Ok(())
    }

    pub fn load(&self, id: &str) -> Result<Session, Error> {
        let path = self.existing_path(id)?;
        let ops = self.ops(id)?;
        let mut session = session_from_ops(&path, ops)?;
        if let Some(title) = self.indexed_title(id) {
            session.summary.title = title;
        }
        Ok(session)
    }

    /// All ops for a session, sorted by `at` (ties by `opId`). Served from the
    /// LRU after the first read.
    pub fn ops(&self, id: &str) -> Result<Vec<Op>, Error> {
        {
            let mut cache = lock(&self.cache);
            if let Some(pos) = cache.iter().position(|(k, _)| k == id) {
                if let Some(entry) = cache.remove(pos) {
                    let ops = entry.1.clone();
                    cache.push_back(entry);
                    return Ok(ops);
                }
            }
        }
        let ops = read_ops(&self.existing_path(id)?)?;
        let mut cache = lock(&self.cache);
        if !cache.iter().any(|(k, _)| k == id) {
            cache.push_back((id.to_owned(), ops.clone()));
            while cache.len() > LRU_SESSIONS {
                cache.pop_front();
            }
        }
        Ok(ops)
    }

    /// Runs `f` over a session's ops without touching the LRU order or
    /// filling it (scans over every session would otherwise evict the open
    /// ones). A cached session is borrowed, not cloned.
    pub fn with_ops<R>(&self, id: &str, f: impl FnOnce(&[Op]) -> R) -> Result<R, Error> {
        {
            let cache = lock(&self.cache);
            if let Some((_, ops)) = cache.iter().find(|(k, _)| k == id) {
                return Ok(f(ops));
            }
        }
        Ok(f(&read_ops(&self.existing_path(id)?)?))
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
        lock(&self.cache).retain(|(k, _)| k != id);
        Ok(())
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
            if let Ok(ops) = read_ops(&path) {
                if let Some(t) = ops.iter().rev().find_map(|o| match o {
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

fn append(path: &Path, op: &Op) -> Result<(), Error> {
    let mut line = serde_json::to_vec(op)?;
    line.push(b'\n');
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(&line)?;
    Ok(())
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

fn read_ops(path: &Path) -> Result<Vec<Op>, Error> {
    let text = fs::read_to_string(path)?;
    let mut ops: Vec<Op> = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        ops.push(serde_json::from_str(line)?);
    }
    // Lines from merged devices may interleave; `at` is the order of record.
    ops.sort_by(|a, b| a.key().cmp(&b.key()));
    Ok(ops)
}

fn session_from_ops(path: &Path, ops: Vec<Op>) -> Result<Session, Error> {
    let header = ops
        .iter()
        .find_map(|op| match op {
            Op::Session(h) => Some(h.clone()),
            _ => None,
        })
        .ok_or_else(|| Error::Corrupt(format!("{} has no session header", path.display())))?;
    let has_phase = ops.iter().any(|op| matches!(op, Op::Phase(_)));
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
        .unwrap_or(header.title);
    Ok(Session {
        summary: SessionSummary {
            id: header.id,
            mode: header.mode,
            title,
            created_at: header.at,
            updated_at,
            read_only: header.mode == Mode::Learn && !has_phase,
        },
        messages,
        ops,
    })
}

/// Reads only the first two lines (header, then the initial `phase` op of a
/// Learn session) for `list`; `updated_at` comes from the file mtime.
fn read_header(path: &Path) -> Result<Option<SessionSummary>, Error> {
    let file = fs::File::open(path)?;
    let mut lines = std::io::BufReader::new(file).lines();
    let line = match lines.next() {
        Some(Ok(l)) => l,
        _ => return Ok(None),
    };
    let Ok(Op::Session(h)) = serde_json::from_str::<Op>(&line) else {
        return Ok(None);
    };
    let has_phase = match lines.next() {
        Some(Ok(l)) => matches!(serde_json::from_str::<Op>(&l), Ok(Op::Phase(_))),
        _ => false,
    };
    let updated = fs::metadata(path)?
        .modified()
        .ok()
        .map(DateTime::<Utc>::from)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_else(|| h.at.clone());
    Ok(Some(SessionSummary {
        id: h.id,
        mode: h.mode,
        title: h.title,
        created_at: h.at,
        updated_at: updated,
        read_only: h.mode == Mode::Learn && !has_phase,
    }))
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
        assert!(store.list().unwrap()[0].read_only);
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

    #[test]
    fn title_is_first_nonblank_line_capped() {
        assert_eq!(title_from("\n  Derivatives  \nmore"), "Derivatives");
        let long = "x".repeat(100);
        assert_eq!(title_from(&long).chars().count(), 81);
    }
}
