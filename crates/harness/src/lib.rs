//! App core: sessions, provider connections, and the agent runtime (tutor,
//! planner, researcher). The Tauri shell is a thin command layer over
//! [`Harness`].

mod agent;
mod artifacts;
mod attachments;
mod config;
pub mod events;
mod extraction;
mod files;
mod frontier;
pub mod graph;
mod jobs;
pub mod knowledge;
mod mobile;
mod models;
mod pdf;
pub mod plugins;
mod prompts;
pub mod proposals;
pub mod replay;
mod sessions;
pub mod state;
pub mod sync;
pub mod tools;
mod web;
mod workspace;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use providers::claude_oauth::{self, PendingLogin};
use providers::codex_oauth;
use providers::workbuddy::{self, LoginStart};
use providers::{
    stream_chat, ChatRequest, Client, ContentBlock, Credential, CredentialKind, Message,
    ProviderError, ProviderId, Role, StopReason, StreamEvent, ToolSpec, Usage,
};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{oneshot, Semaphore};

pub use config::{ModelChoice, ModelRole, Roles, Settings};
pub use events::{AgentEvent, AgentName, Coalescer, Envelope, EventSink};
pub use extraction::ExtractionStatus;
pub use files::Mention;
pub use frontier::FrontierItem;
pub use knowledge::{
    Checks, KnowledgeChain, KnowledgeEdge, KnowledgeMap, KnowledgeNode, KnowledgeTopic,
    ProposedSplit, SplitGroup,
};
pub use plugins::{PluginManifest, PluginSource};
pub use prompts::{Composer, TemplateValues};
pub use providers;
pub use sessions::{
    ApproveOp, Attachment, CompactionOp, Ending, Header, Mode, Op, PhaseOp, RecordOp,
    RemovedPlugin, Session, SessionSummary, StoredMessage, ToolOp, ToolResultValue,
};
pub use state::SessionState;
pub use sync::{
    ConflictPath, SyncConfig, SyncReport, SyncStatus, WebdavConfig, WebdavReport, WebdavStatus,
};

const MAX_TOKENS: u32 = 8192;
const LOGIN_POLL: Duration = Duration::from_millis(1500);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error("storage: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid data: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Corrupt(String),
    #[error("{0} not found")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    #[error("prompt: {0}")]
    Prompt(String),
    #[error("{0}")]
    Sync(String),
}

/// Streamed to the UI during a Practice turn (`send`/`retry`). Learn sessions
/// use [`AgentEvent`]s through the event sink instead.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ChatEvent<'a> {
    /// First event when a new session was created.
    Session {
        session: &'a SessionSummary,
    },
    Text {
        delta: &'a str,
    },
    Thinking {
        delta: &'a str,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStatus {
    pub id: ProviderId,
    pub name: &'static str,
    pub credential: CredentialKind,
    pub key_url: Option<&'static str>,
    pub env_var: &'static str,
    /// `saved`, `env`, or absent when not connected.
    pub source: Option<&'static str>,
    pub account: Option<String>,
    pub base_url: Option<String>,
    pub default_base_url: &'static str,
}
/// In-memory token usage per role (architecture §4.22). Accumulated as model
/// calls complete; never persisted (derived, device-local).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// One role's usage for the Settings UI.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleUsageEntry {
    pub role: ModelRole,
    pub usage: RoleUsage,
}

/// Result of the per-role tool-call test button.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleTestResult {
    pub called: bool,
    pub model: String,
}

/// Static role metadata for the Settings UI (architecture §4.22).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleInfo {
    pub role: ModelRole,
    pub label: &'static str,
    pub weight: &'static str,
    pub phrase: &'static str,
    pub guidance: &'static str,
    pub min_context: Option<u32>,
    pub vision_relevant: bool,
}

pub struct Harness {
    pub(crate) repo: PathBuf,
    pub(crate) client: Client,
    pub(crate) store: sessions::Store,
    pub(crate) config: Mutex<config::Config>,
    /// Stop handles of running tutor/practice turns, by session.
    pub(crate) running: Mutex<HashMap<String, oneshot::Sender<()>>>,
    pub(crate) login_cancelled: AtomicBool,
    /// PKCE verifier/state between the two Claude subscription login steps.
    pub(crate) pending_claude_login: Mutex<Option<PendingLogin>>,
    /// PKCE verifier/state and the bound loopback socket between ChatGPT
    /// sign-in start and wait. Dropping it frees port 1455.
    pub(crate) pending_codex_login: Mutex<Option<codex_oauth::PendingLogin>>,
    pub(crate) codex_login_cancelled: AtomicBool,
    /// Input/output tokens per role, accumulated in memory.
    pub(crate) role_usage: Mutex<HashMap<ModelRole, RoleUsage>>,
    /// Per-session turn lock: tutor turns, answers and job deliveries for one
    /// session are serialized; different sessions never wait on each other.
    pub(crate) turn_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Registered plugins: installed packages, folder plugins and natives.
    pub(crate) plugins: Mutex<plugins::PluginRegistry>,
    /// Device-local copy of the data repo path (plugin installs, registries).
    pub(crate) config_dir: PathBuf,
    /// Last model list per provider plus the models.dev metadata
    /// (`models-cache.json`). Read on every turn's context lookup. Shared
    /// with the background metadata refresh.
    pub(crate) model_cache: Arc<Mutex<config::ModelCache>>,
    /// Held while the models.dev metadata is being fetched: one fetch at a time.
    pub(crate) catalog_fetch: Arc<tokio::sync::Mutex<()>>,
    /// session → workspace folder mapping (device-local).
    pub(crate) workspaces: Mutex<workspace::Workspaces>,
    /// (session, workspace-relative path) → sha256 the agent last saw.
    pub(crate) workspace_hashes: Mutex<HashMap<(String, String), String>>,
    /// Running jobs by job id.
    pub(crate) jobs: Mutex<HashMap<String, jobs::JobHandle>>,
    /// Background tasks by session (jobs, wakes, titles).
    pub(crate) background: Mutex<HashMap<String, Vec<tokio::task::JoinHandle<()>>>>,
    /// Sessions whose interrupted jobs were already restarted.
    pub(crate) recovered: Mutex<HashSet<String>>,
    /// Concurrency per role: Reader 3 (researcher, titles), Planner 1, and
    /// one graph builder at a time.
    pub(crate) reader_slots: Arc<Semaphore>,
    pub(crate) planner_slots: Arc<Semaphore>,
    pub(crate) librarian_slots: Arc<Semaphore>,
    /// Held while deciding and queueing a graph build, so a manual Extract
    /// and an end-of-turn build can't both queue one for a session.
    pub(crate) graph_spawn: Mutex<()>,
    /// Where runtime events go (the UI); `None` drops them.
    pub(crate) sink: Mutex<Option<EventSink>>,
    /// session → artifact → latest view-state summary since the learner's
    /// last message; flushed as `viewState` ops when they next send (W7).
    pub(crate) view_states: Mutex<HashMap<String, BTreeMap<String, String>>>,
    /// Mobile only: each Learn session file's mobile-lock facts, keyed by its
    /// length and mtime (`<config>/session-locks.json`, loaded on first use).
    pub(crate) session_locks: Mutex<Option<mobile::LockCache>>,
    /// Git sync of the data repo (device-local settings + one run at a time).
    pub(crate) sync: Arc<sync::Sync>,
}

impl Harness {
    /// `repo` is the synced data repo; `config_dir` is device-local. Cheap:
    /// no session is read and no prompt composed until first use.
    pub fn open(repo: &Path, config_dir: &Path) -> Result<Self, Error> {
        std::fs::create_dir_all(repo)?;
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .build()
            .map_err(|e| Error::Invalid(format!("http client: {e}")))?;
        Ok(Self {
            repo: repo.to_owned(),
            client,
            store: sessions::Store::new(repo, config_dir),
            config: Mutex::new(config::Config::load(config_dir)?),
            running: Mutex::new(HashMap::new()),
            login_cancelled: AtomicBool::new(false),
            pending_claude_login: Mutex::new(None),
            pending_codex_login: Mutex::new(None),
            codex_login_cancelled: AtomicBool::new(false),
            role_usage: Mutex::new(HashMap::new()),
            turn_locks: Mutex::new(HashMap::new()),
            plugins: Mutex::new(plugins::PluginRegistry::default()),
            config_dir: config_dir.to_owned(),
            model_cache: Arc::new(Mutex::new(config::ModelCache::load(config_dir)?)),
            catalog_fetch: Arc::new(tokio::sync::Mutex::new(())),
            workspaces: Mutex::new(workspace::Workspaces::load(config_dir)?),
            workspace_hashes: Mutex::new(HashMap::new()),
            jobs: Mutex::new(HashMap::new()),
            background: Mutex::new(HashMap::new()),
            recovered: Mutex::new(HashSet::new()),
            reader_slots: Arc::new(Semaphore::new(3)),
            planner_slots: Arc::new(Semaphore::new(1)),
            librarian_slots: Arc::new(Semaphore::new(1)),
            graph_spawn: Mutex::new(()),
            sink: Mutex::new(None),
            view_states: Mutex::new(HashMap::new()),
            session_locks: Mutex::new(None),
            sync: Arc::new(sync::Sync::new(repo, config_dir)),
        })
        .inspect(|harness: &Self| {
            // Chips don't survive a restart, so neither do their staged images.
            crate::attachments::clear_staging(&harness.staging_dir());
            if let Err(e) = harness.reload_plugins() {
                tracing::warn!(error = %e, "could not load plugins");
            }
            // Best effort: a configured sync catches up in the background, and
            // an offline launch only records `last_error` (the UI banner).
            harness.sync.trigger();
            // Every 20 minutes, a run when there is local progress. The timer
            // holds a weak reference and ends with the Harness.
            harness.sync.start_periodic(sync::PERIODIC_INTERVAL);
        })
    }

    // ---- sync (docs/sync-build.md) --------------------------------------

    /// Configuration plus local ahead/behind counts; no network.
    pub fn sync_status(&self) -> Result<SyncStatus, Error> {
        self.sync.status()
    }

    /// Save the device-local remote and token. An empty remote disconnects
    /// sync and forgets the token.
    pub fn set_sync(
        &self,
        remote: Option<&str>,
        token: Option<&str>,
        branch: Option<&str>,
    ) -> Result<SyncStatus, Error> {
        self.sync.set(remote, token, branch)?;
        self.sync.status()
    }

    /// Save the device-local WebDAV folder URL, username and app password.
    /// A blank URL disconnects WebDAV; a `None` password keeps the saved one
    /// and `""` clears it.
    pub fn set_webdav(
        &self,
        url: Option<&str>,
        username: Option<&str>,
        password: Option<&str>,
    ) -> Result<SyncStatus, Error> {
        self.sync.set_webdav(url, username, password)?;
        self.sync.status()
    }

    /// One full sync run: git (commit, fetch, merge, resolve, push), then a
    /// WebDAV pass, each when configured.
    pub fn sync_now(&self) -> Result<SyncReport, Error> {
        self.sync.sync()
    }

    /// The learner saw the conflict copies: clear the banner state (the
    /// `.conflicts/` files stay in the repo and its history).
    pub fn acknowledge_conflicts(&self) -> Result<SyncStatus, Error> {
        self.sync.acknowledge()?;
        self.sync.status()
    }

    /// Best-effort background sync; never blocks the caller or a turn.
    pub fn sync_in_background(&self) {
        self.sync.trigger();
    }

    /// Best-effort background run only when a configured target has local
    /// progress (the app going to the background on mobile). No network for
    /// the check; never blocks on the run.
    pub fn sync_if_changed(&self) -> bool {
        self.sync.trigger_if_changed()
    }

    /// Closing the app: whether a last sync is worth holding the exit for (a
    /// run in flight, or local progress on a configured target).
    pub fn sync_pending_before_exit(&self) -> bool {
        self.sync.pending_before_exit()
    }

    /// One last sync before the app closes, blocking at most `deadline`
    /// ([`sync::EXIT_DEADLINE`] in the app). Returns whether it finished.
    pub fn sync_before_exit(&self, deadline: Duration) -> bool {
        self.sync.sync_before_exit(deadline)
    }

    /// Where runtime events (turns, ops, jobs) go.
    pub fn set_event_sink(&self, sink: Option<EventSink>) {
        *lock(&self.sink) = sink;
    }

    // ---- sessions -------------------------------------------------------

    /// Every session, from each file's first two lines (M7). On mobile, a
    /// Learn session that used a desktop-only plugin is listed read-only,
    /// from a device-local cache that re-reads a file only when it changed.
    pub fn list_sessions(&self) -> Result<Vec<SessionSummary>, Error> {
        let mut list = self.store.list()?;
        self.apply_mobile_locks(&mut list);
        Ok(list)
    }

    /// A session with its ops. On mobile, a Learn session that used a
    /// desktop-only plugin comes back read-only (docs/sync-build.md,
    /// "Sessions on mobile").
    pub fn load_session(&self, id: &str) -> Result<Session, Error> {
        let mut session = self.store.load(id)?;
        self.apply_mobile_lock(&mut session);
        Ok(session)
    }

    pub fn delete_session(&self, id: &str) -> Result<(), Error> {
        lock(&self.view_states).remove(id);
        self.cancel(id);
        let jobs: Vec<String> = lock(&self.jobs)
            .iter()
            .filter(|(_, h)| h.session_id == id)
            .map(|(k, _)| k.clone())
            .collect();
        for job in jobs {
            let _ = self.cancel_job(id, &job);
        }
        self.store.delete(id)?;
        self.forget_mobile_lock(id);
        Ok(())
    }

    /// The Map page's data: the standalone knowledge graph (topics, claims,
    /// prerequisite edges that may cross topics, and per-topic check totals)
    /// read from `<repo>/graph/` (ui-design §7.6, architecture §4.2/§4.5).
    pub fn knowledge_map(&self) -> Result<KnowledgeMap, Error> {
        knowledge::load(&self.repo)
    }

    /// The Map page's review list: proposed prerequisite edges the extractor
    /// wrote, with the titles the UI shows instead of ids (ui-design §7.6).
    pub fn edge_proposals(&self) -> Result<proposals::Review, Error> {
        proposals::review(&self.repo)
    }

    /// Accepts and dismisses proposals by key (`from\0to`). Validation happens
    /// here, not in the UI: what survives is appended to `graph/edges.jsonl`,
    /// what the graph refuses is reported, and both leave the review file.
    pub fn decide_edge_proposals(
        &self,
        accept: &[String],
        dismiss: &[String],
    ) -> Result<proposals::Decision, Error> {
        proposals::decide(&self.repo, accept, dismiss)
    }

    /// Extract by hand (the Home list's Extract): start the graph builder
    /// over this session's teaching not yet in the graph (architecture §4.7).
    /// Runs in the background on the Librarian role, appends ops, and never
    /// delivers anything into the conversation. Refuses with a readable
    /// message while a build is open or with less than a quarter of the
    /// automatic threshold to read.
    pub fn graph_build(self: &Arc<Self>, session_id: &str) -> Result<String, Error> {
        self.spawn_graph_job(session_id, extraction::Trigger::Manual)?
            .ok_or_else(|| Error::Invalid("nothing new to extract".into()))
    }

    /// How much of this session's teaching is not in the graph yet, and
    /// whether a build is running: what the Home list's Extract shows.
    pub fn graph_status(&self, session_id: &str) -> Result<ExtractionStatus, Error> {
        Ok(extraction::status(&self.store.ops(session_id)?))
    }

    /// Recompute the topic-split proposals (writing
    /// `graph/splits.proposed.json`) and return them.
    pub fn graph_splits(&self) -> Result<Vec<ProposedSplit>, Error> {
        graph::splits(&self.repo)
    }

    /// Accept and/or dismiss proposed topic splits. Accepting appends a
    /// `split` op per topic; either way the proposal file is rewritten.
    pub fn decide_splits(
        &self,
        accept: &[String],
        dismiss: &[String],
    ) -> Result<graph::SplitDecision, Error> {
        graph::decide_splits(&self.repo, accept, dismiss)
    }

    /// Practice mode: appends the learner's message (creating the session
    /// when `session_id` is `None`) and streams the reply from
    /// `prompts/practice.md`. Learn sessions run through the agent runtime
    /// ([`Harness::start_teaching`] and friends).
    pub async fn send(
        &self,
        session_id: Option<&str>,
        mode: Mode,
        text: &str,
        on_event: &mut (dyn FnMut(ChatEvent<'_>) + Send),
    ) -> Result<StoredMessage, Error> {
        self.send_with(session_id, mode, text, &[], on_event).await
    }

    /// As [`send`], with attachments (W3).
    pub async fn send_with(
        &self,
        session_id: Option<&str>,
        mode: Mode,
        text: &str,
        attachments: &[Attachment],
        on_event: &mut (dyn FnMut(ChatEvent<'_>) + Send),
    ) -> Result<StoredMessage, Error> {
        if mode != Mode::Practice {
            return Err(Error::Invalid(
                "Learn sessions run through the tutor; use start_teaching".into(),
            ));
        }
        let text = text.trim();
        if text.is_empty() {
            return Err(Error::Invalid("message is empty".into()));
        }
        // Practice has no file tools: it takes images only (ui-design §7.3).
        if attachments.iter().any(|a| a.kind != "image") {
            return Err(Error::Invalid(
                "Practice can't read files or folders; attach images only".into(),
            ));
        }
        if !attachments.is_empty() && self.role_vision(ModelRole::Tutor) == Some(false) {
            return Err(Error::Invalid(
                "this model can't see images; send without them or choose another Tutor model"
                    .into(),
            ));
        }
        // Fail before writing anything if no model is usable.
        self.model_choice()?;
        let id = match session_id {
            Some(id) => {
                if self.store.load(id)?.summary.mode != Mode::Practice {
                    return Err(Error::Invalid("this is not a Practice session".into()));
                }
                id.to_owned()
            }
            None => {
                let summary = self.store.create(mode, text)?;
                on_event(ChatEvent::Session { session: &summary });
                summary.id
            }
        };
        self.commit_attachments(attachments)?;
        let mut message = sessions::new_message(Role::User, text.to_owned());
        message.attachments = attachments.to_vec();
        self.store.append(&id, &message)?;
        self.reply(&id, on_event).await
    }

    /// Practice mode: re-runs the reply after a failed turn (last message
    /// must be the learner's).
    pub async fn retry(
        &self,
        session_id: &str,
        on_event: &mut (dyn FnMut(ChatEvent<'_>) + Send),
    ) -> Result<StoredMessage, Error> {
        let session = self.store.load(session_id)?;
        if session.summary.mode != Mode::Practice {
            return Err(Error::Invalid(
                "Learn sessions resume with resume_turn".into(),
            ));
        }
        if session.messages.last().map(|m| m.role) != Some(Role::User) {
            return Err(Error::Invalid("nothing to retry".into()));
        }
        self.reply(session_id, on_event).await
    }

    /// Stops a running turn; the partial reply is kept as interrupted.
    pub fn cancel(&self, session_id: &str) {
        if let Some(stop) = lock(&self.running).remove(session_id) {
            let _ = stop.send(());
        }
    }

    async fn reply(
        &self,
        session_id: &str,
        on_event: &mut (dyn FnMut(ChatEvent<'_>) + Send),
    ) -> Result<StoredMessage, Error> {
        let session = self.store.load(session_id)?;
        let choice = self.model_choice()?;
        let system = practice_prompt(&self.repo)?;
        let history: Vec<Message> = replay::conversation_with(
            &session.ops,
            Some(&self.repo),
            self.role_vision(ModelRole::Tutor) != Some(false),
            false,
        );
        let req = ChatRequest {
            model: &choice.model,
            system: &system,
            messages: &history,
            max_tokens: MAX_TOKENS,
            effort: choice.effort.as_deref(),
            tools: &[],
            cache_key: Some(session_id),
        };

        let (stop_tx, mut stop_rx) = oneshot::channel();
        if lock(&self.running)
            .insert(session_id.to_owned(), stop_tx)
            .is_some()
        {
            return Err(Error::Invalid(
                "a reply is already running in this session".into(),
            ));
        }
        let mut text = String::new();
        let mut thinking = String::new();
        let mut on_delta = |ev: StreamEvent<'_>| match ev {
            StreamEvent::Text(d) => {
                text.push_str(d);
                on_event(ChatEvent::Text { delta: d });
            }
            StreamEvent::Thinking(d) => {
                thinking.push_str(d);
                on_event(ChatEvent::Thinking { delta: d });
            }
        };
        let outcome = tokio::select! {
            r = self.stream_with_refresh(choice.provider, session_id, &req, &mut on_delta) => Some(r),
            _ = &mut stop_rx => None,
        };
        lock(&self.running).remove(session_id);

        let ending = match outcome {
            Some(Ok(done)) => {
                self.add_usage(ModelRole::Tutor, &done.usage);
                match done.stop {
                    StopReason::End | StopReason::ToolUse => None,
                    StopReason::MaxTokens => Some(Ending::MaxTokens),
                    StopReason::Refusal => Some(Ending::Refusal),
                }
            }
            Some(Err(e)) => return Err(e),
            None => Some(Ending::Interrupted),
        };
        let mut message = sessions::new_message(Role::Assistant, text);
        message.thinking = thinking;
        message.model = Some(format!("{}/{}", choice.provider.spec().name, choice.model));
        message.ending = ending;
        self.store.append(session_id, &message)?;
        Ok(message)
    }

    /// WorkBuddy sessions are refreshed ahead of expiry and once more if the
    /// gateway rejects the token anyway.
    async fn stream_with_refresh(
        &self,
        provider: ProviderId,
        conversation_id: &str,
        req: &ChatRequest<'_>,
        on_delta: &mut (dyn FnMut(StreamEvent<'_>) + Send),
    ) -> Result<providers::Completion, Error> {
        let credential = self.current_credential(provider).await?;
        let base = self.base_url(provider);
        let endpoint = provider.endpoint(&credential, base.as_deref(), conversation_id)?;
        let first = stream_chat(&self.client, &endpoint, req, on_delta).await;
        match (first, &credential) {
            (Err(ProviderError::Unauthorized(_)), Credential::Workbuddy(s))
                if s.can_refresh(unix_now()) =>
            {
                let fresh = self.refresh_workbuddy(s).await?;
                let endpoint = provider.endpoint(&fresh, base.as_deref(), conversation_id)?;
                Ok(stream_chat(&self.client, &endpoint, req, on_delta).await?)
            }
            (Err(ProviderError::Unauthorized(_)), Credential::ClaudeCode(s)) if s.can_refresh() => {
                let fresh = self.refresh_claude(s).await?;
                let endpoint = provider.endpoint(&fresh, base.as_deref(), conversation_id)?;
                Ok(stream_chat(&self.client, &endpoint, req, on_delta).await?)
            }
            (Err(ProviderError::Unauthorized(_)), Credential::Codex(s)) if s.can_refresh() => {
                let fresh = self.refresh_codex(s).await?;
                let endpoint = provider.endpoint(&fresh, base.as_deref(), conversation_id)?;
                Ok(stream_chat(&self.client, &endpoint, req, on_delta).await?)
            }
            (result, _) => Ok(result?),
        }
    }

    // ---- providers ------------------------------------------------------

    pub fn settings(&self) -> Settings {
        lock(&self.config).settings.clone()
    }

    /// The learner's saved default starting point (§3), normalized on load to
    /// `measure` when the settings file holds anything else.
    pub fn saved_starting_point(&self) -> String {
        lock(&self.config).settings.starting_point.clone()
    }

    /// Static metadata for the five role cards in Settings.
    pub fn roles(&self) -> Vec<RoleInfo> {
        ModelRole::ALL
            .iter()
            .map(|&role| RoleInfo {
                role,
                label: role.label(),
                weight: role.weight(),
                phrase: role.role_phrase(),
                guidance: role.guidance(),
                min_context: role.min_context(),
                vision_relevant: role.vision_relevant(),
            })
            .collect()
    }

    pub fn providers(&self) -> Vec<ProviderStatus> {
        let config = lock(&self.config);
        ProviderId::ALL
            .iter()
            .map(|&id| {
                let spec = id.spec();
                let saved = config.credentials.get(&id);
                let (source, account) = match saved {
                    Some(Credential::Workbuddy(s)) => (Some("saved"), s.account.clone()),
                    Some(Credential::ClaudeCode(s)) => (Some("saved"), s.account.clone()),
                    Some(Credential::Codex(s)) => (Some("saved"), s.account.clone()),
                    Some(Credential::ApiKey { .. }) => (Some("saved"), None),
                    None if id.credential_from_env().is_some() => (Some("env"), None),
                    None => (None, None),
                };
                ProviderStatus {
                    id,
                    name: spec.name,
                    credential: spec.credential,
                    key_url: spec.key_url,
                    env_var: spec.env_var,
                    source,
                    account,
                    base_url: config.settings.base_urls.get(&id).cloned(),
                    default_base_url: spec.default_base_url,
                }
            })
            .collect()
    }

    pub fn save_api_key(&self, provider: ProviderId, key: &str) -> Result<(), Error> {
        let key = key.trim();
        if key.is_empty() {
            return Err(Error::Invalid("key is empty".into()));
        }
        let mut config = lock(&self.config);
        config.credentials.insert(
            provider,
            Credential::ApiKey {
                key: key.to_owned(),
            },
        );
        config.save_credentials()
    }

    pub fn remove_credential(&self, provider: ProviderId) -> Result<(), Error> {
        let mut config = lock(&self.config);
        if config.credentials.remove(&provider).is_some() {
            config.save_credentials()?;
        }
        Ok(())
    }

    pub fn set_base_url(&self, provider: ProviderId, url: Option<&str>) -> Result<(), Error> {
        let mut config = lock(&self.config);
        match url.map(str::trim).filter(|u| !u.is_empty()) {
            Some(u) if !(u.starts_with("http://") || u.starts_with("https://")) => {
                return Err(Error::Invalid(
                    "base URL must start with http:// or https://".into(),
                ))
            }
            Some(u) => config.settings.base_urls.insert(provider, u.to_owned()),
            None => config.settings.base_urls.remove(&provider),
        };
        config.save_settings()
    }

    /// Remember the learner's default starting point for new sessions (§3).
    pub fn set_starting_point(&self, point: &str) -> Result<(), Error> {
        let point = point.trim();
        if !matches!(point, "measure" | "quick" | "new") {
            return Err(Error::Invalid(
                "starting point must be measure, quick or new".into(),
            ));
        }
        let mut config = lock(&self.config);
        config.settings.starting_point = point.to_owned();
        config.save_settings()
    }

    /// Set (or clear, with `None`) the model for one role. WorkBuddy efforts
    /// are validated against the live catalog so unsupported efforts are
    /// never sent.
    pub async fn set_role_model(
        &self,
        role: ModelRole,
        choice: Option<ModelChoice>,
    ) -> Result<(), Error> {
        let choice = match choice {
            None => None,
            Some(mut c) => {
                let model = c.model.trim().to_owned();
                if model.is_empty() {
                    return Err(Error::Invalid("model id is empty".into()));
                }
                c.model = model.clone();
                c.effort = match (c.provider, c.effort) {
                    (ProviderId::Workbuddy, Some(effort)) => self
                        .models(ProviderId::Workbuddy, false)
                        .await?
                        .iter()
                        .find(|m| m.id == model)
                        .filter(|m| m.efforts.contains(&effort))
                        .map(|_| effort),
                    (_, effort) => effort,
                };
                Some(c)
            }
        };
        let mut config = lock(&self.config);
        config.settings.roles.set(role, choice);
        config.save_settings()
    }

    /// One tiny request that must call the `ping` tool. Passes only if the
    /// model actually makes a tool call through its provider (architecture
    /// §4.22 "test button per role").
    pub async fn test_role(&self, role: ModelRole) -> Result<RoleTestResult, Error> {
        let choice = lock(&self.config).resolve(role)?;
        self.credential(choice.provider)?;
        let tools = vec![ToolSpec {
            name: "ping".into(),
            description: "Confirm tool calling works.".into(),
            input_schema: serde_json::json!({ "type": "object", "properties": {} }),
        }];
        let messages = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::text("call ping")],
        }];
        let req = ChatRequest {
            model: &choice.model,
            system: "You must call the ping tool. Call it now.",
            messages: &messages,
            max_tokens: 64,
            effort: choice.effort.as_deref(),
            tools: &tools,
            cache_key: None,
        };
        let completion = self
            .stream_with_refresh(choice.provider, "role-test", &req, &mut |_| {})
            .await?;
        self.add_usage(role, &completion.usage);
        let called = completion.tool_calls.iter().any(|c| c.name == "ping");
        Ok(RoleTestResult {
            called,
            model: choice.model,
        })
    }

    /// Accumulate usage for a role (used by the single-prompt path today and
    /// by each agent turn later).
    fn add_usage(&self, role: ModelRole, usage: &Usage) {
        let mut map = lock(&self.role_usage);
        let entry = map.entry(role).or_default();
        entry.input_tokens += u64::from(usage.input_tokens);
        entry.output_tokens += u64::from(usage.output_tokens);
    }

    /// Snapshot of per-role usage for the Settings UI.
    pub fn role_usage(&self) -> Vec<RoleUsageEntry> {
        let map = lock(&self.role_usage);
        ModelRole::ALL
            .iter()
            .map(|&role| RoleUsageEntry {
                role,
                usage: map.get(&role).cloned().unwrap_or_default(),
            })
            .collect()
    }

    /// Browser sign-in, step 1: returns the URL the learner must open.
    pub async fn workbuddy_login_start(&self) -> Result<LoginStart, Error> {
        self.login_cancelled.store(false, Ordering::SeqCst);
        Ok(workbuddy::begin_login(&self.client, &self.workbuddy_base()).await?)
    }

    /// Step 2: waits (up to 5 minutes) for the learner to approve, then saves
    /// the session.
    pub async fn workbuddy_login_wait(&self, state: &str) -> Result<Option<String>, Error> {
        let base = self.workbuddy_base();
        let started = std::time::Instant::now();
        while started.elapsed() < LOGIN_TIMEOUT {
            tokio::time::sleep(LOGIN_POLL).await;
            if self.login_cancelled.load(Ordering::SeqCst) {
                return Err(Error::Invalid("sign-in cancelled".into()));
            }
            if let Some(session) =
                workbuddy::poll_login(&self.client, &base, state, unix_now()).await?
            {
                let account = session.account.clone();
                let mut config = lock(&self.config);
                config
                    .credentials
                    .insert(ProviderId::Workbuddy, Credential::Workbuddy(session));
                config.save_credentials()?;
                return Ok(account);
            }
        }
        Err(Error::Invalid("sign-in timed out; try again".into()))
    }

    pub fn workbuddy_login_cancel(&self) {
        self.login_cancelled.store(true, Ordering::SeqCst);
    }

    /// Step 1 of Claude subscription sign-in: keep the PKCE verifier and
    /// return the URL for the learner to open in a browser.
    pub fn claude_login_start(&self) -> claude_oauth::LoginStart {
        let pending = PendingLogin::new();
        let start = pending.start();
        *lock(&self.pending_claude_login) = Some(pending);
        start
    }

    /// Step 2: exchange the `code[#state]` the browser showed, then save the
    /// subscription grant. Returns the account label, if the token response
    /// carried one.
    pub async fn claude_login_submit(&self, pasted: &str) -> Result<Option<String>, Error> {
        let pending = lock(&self.pending_claude_login)
            .take()
            .ok_or_else(|| Error::Invalid("start sign-in again".into()))?;
        let session = pending.exchange(&self.client, pasted).await?;
        let account = session.account.clone();
        let mut config = lock(&self.config);
        config
            .credentials
            .insert(ProviderId::ClaudeCode, Credential::ClaudeCode(session));
        config.save_credentials()?;
        Ok(account)
    }

    pub fn claude_login_cancel(&self) {
        *lock(&self.pending_claude_login) = None;
    }

    /// Step 1 of ChatGPT sign-in: bind localhost:1455 (fails clearly if the
    /// port is busy) and return the URL for the learner to open.
    pub fn codex_login_start(&self) -> Result<codex_oauth::LoginStart, Error> {
        // Drop any earlier attempt first so its socket frees the port.
        *lock(&self.pending_codex_login) = None;
        self.codex_login_cancelled.store(false, Ordering::SeqCst);
        let pending = codex_oauth::PendingLogin::new()?;
        let start = pending.start();
        *lock(&self.pending_codex_login) = Some(pending);
        Ok(start)
    }

    /// Step 2: wait (up to 5 minutes) for the browser redirect, exchange the
    /// code and save the grant. Returns the account email, if any.
    pub async fn codex_login_wait(&self) -> Result<Option<String>, Error> {
        let pending = lock(&self.pending_codex_login)
            .take()
            .ok_or_else(|| Error::Invalid("start sign-in again".into()))?;
        let session = pending
            .finish(&self.client, &self.codex_login_cancelled)
            .await?;
        let account = session.account.clone();
        let mut config = lock(&self.config);
        config
            .credentials
            .insert(ProviderId::OpenaiCodex, Credential::Codex(session));
        config.save_credentials()?;
        Ok(account)
    }

    pub fn codex_login_cancel(&self) {
        self.codex_login_cancelled.store(true, Ordering::SeqCst);
        *lock(&self.pending_codex_login) = None;
    }

    // ---- learner commands: attachments, mentions, views -------------------

    /// Device-local folder for images that are chips but not yet sent.
    pub(crate) fn staging_dir(&self) -> PathBuf {
        crate::attachments::staging_dir(&self.config_dir)
    }

    /// Stage a pasted image (device-local until Send) and describe it (W3).
    pub fn stage_image(&self, data_base64: &str, name: Option<&str>) -> Result<Attachment, Error> {
        crate::attachments::store_image(&self.staging_dir(), data_base64, name)
    }

    /// Describe a picked, dropped or mentioned path: an image file is staged
    /// like a pasted one; a file or folder is referenced, never copied.
    /// Paths inside the session workspace are kept workspace-relative.
    pub fn stage_file(&self, session_id: Option<&str>, path: &str) -> Result<Attachment, Error> {
        let workspace = session_id.and_then(|id| lock(&self.workspaces).get(id));
        crate::attachments::reference(path, workspace.as_deref(), &self.staging_dir())
    }

    /// Delete a staged image whose chip was removed before Send.
    pub fn unstage_image(&self, path: &str) -> Result<(), Error> {
        crate::attachments::unstage(&self.staging_dir(), path)
    }

    /// Move a message's staged images into the repo (`attachments/<yyyy>/`)
    /// as it is sent.
    pub(crate) fn commit_attachments(&self, attachments: &[Attachment]) -> Result<(), Error> {
        crate::attachments::commit(&self.repo, &self.staging_dir(), attachments)
    }

    /// A data URL for an image chip or a sent image (message thumbnails).
    pub fn attachment_data(&self, path: &str) -> Result<String, Error> {
        crate::attachments::data_url(&self.repo, &self.staging_dir(), path)
    }

    /// `@`-mention candidates under home, the session workspace and the
    /// session's granted roots.
    pub fn mentions(
        &self,
        session_id: Option<&str>,
        query: &str,
    ) -> Result<Vec<files::Mention>, Error> {
        let mut roots: Vec<PathBuf> = files::home_dir().into_iter().collect();
        let mut workspace = None;
        if let Some(id) = session_id {
            workspace = lock(&self.workspaces).get(id);
            if let Ok(state) = self.session_state(id) {
                roots.extend(state.file_grants.iter().map(PathBuf::from));
            }
        }
        Ok(files::mentions(&roots, workspace.as_deref(), query, 50))
    }

    /// Vision capability for the two agents that can receive images.
    pub fn vision_capabilities(&self) -> VisionCapabilities {
        VisionCapabilities {
            tutor: self.role_vision(ModelRole::Tutor),
            practice: self.role_vision(ModelRole::Tutor),
        }
    }

    /// Metadata for a plugin's interactive view: what the UI needs to build
    /// the bridge. The document itself is served separately by [`Self::view_document`].
    pub fn plugin_view(&self, plugin_id: &str) -> Result<PluginView, Error> {
        let plugin = self.enabled_plugin(plugin_id)?;
        let view = plugin
            .manifest()
            .view
            .first()
            .cloned()
            .ok_or_else(|| Error::Invalid("that plugin has no view".into()))?;
        Ok(PluginView {
            name: view.name,
            calls: view.calls,
            answer: view.answer,
        })
    }

    /// The inlined view document for the `pluginview` URI scheme. Kept out of
    /// `plugin_view` so the iframe's HTML is never sent over the UI command
    /// channel — only the sandboxed scheme response carries it.
    pub fn view_document(&self, plugin_id: &str) -> Result<Vec<u8>, Error> {
        let plugin = self.enabled_plugin(plugin_id)?;
        if plugin.manifest().view.is_empty() {
            return Err(Error::Invalid("that plugin has no view".into()));
        }
        Ok(plugin.view_html().map_err(Error::Invalid)?.into_bytes())
    }

    /// A bridge call from a view (allow-listed by the manifest; the plugin
    /// must be enabled).
    pub fn plugin_view_call(
        &self,
        plugin_id: &str,
        function: &str,
        args: Value,
    ) -> Result<Value, Error> {
        let plugin = self.enabled_plugin(plugin_id)?;
        plugin.view_call(function, &args).map_err(Error::Invalid)
    }

    /// Hold a view's short state summary for the learner's next message.
    /// Only the latest per artifact is kept; it is stored as a `viewState`
    /// op when the learner sends, so ordinary interaction is not logged.
    pub fn view_state(
        &self,
        session_id: &str,
        artifact_id: &str,
        summary: &str,
    ) -> Result<(), Error> {
        let ops = self.learn_ops(session_id)?;
        if !ops
            .iter()
            .any(|o| matches!(o, Op::Artifact(a) if a.artifact_id == artifact_id))
        {
            return Err(Error::Invalid(format!("unknown artifact {artifact_id}")));
        }
        lock(&self.view_states)
            .entry(session_id.to_owned())
            .or_default()
            .insert(artifact_id.to_owned(), sanitize_view_state(summary));
        Ok(())
    }

    // ---- internals ------------------------------------------------------

    fn model_choice(&self) -> Result<ModelChoice, Error> {
        let choice = lock(&self.config).resolve(ModelRole::Tutor)?;
        self.credential(choice.provider)?;
        Ok(choice)
    }

    fn credential(&self, provider: ProviderId) -> Result<Credential, Error> {
        lock(&self.config)
            .credentials
            .get(&provider)
            .cloned()
            .or_else(|| provider.credential_from_env())
            .ok_or(Error::Provider(ProviderError::NotConnected(
                provider.spec().name,
            )))
    }

    fn base_url(&self, provider: ProviderId) -> Option<String> {
        lock(&self.config)
            .settings
            .base_urls
            .get(&provider)
            .cloned()
    }

    /// The saved or environment credential; expiring sessions are refreshed
    /// before the request (WorkBuddy: 1 day; Claude and ChatGPT: 5 minutes).
    async fn current_credential(&self, provider: ProviderId) -> Result<Credential, Error> {
        let credential = self.credential(provider)?;
        match &credential {
            Credential::Workbuddy(s)
                if s.needs_refresh(unix_now()) && s.can_refresh(unix_now()) =>
            {
                self.refresh_workbuddy(s).await
            }
            Credential::ClaudeCode(s) if s.needs_refresh(unix_now()) && s.can_refresh() => {
                self.refresh_claude(s).await
            }
            Credential::Codex(s) if s.needs_refresh(unix_now()) && s.can_refresh() => {
                self.refresh_codex(s).await
            }
            _ => Ok(credential),
        }
    }

    /// WorkBuddy host, or the learner's base URL override for every call
    /// (sign-in, refresh, catalog, chat).
    fn workbuddy_base(&self) -> String {
        self.base_url(ProviderId::Workbuddy)
            .map(|b| b.trim().trim_end_matches('/').to_owned())
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| workbuddy::HOST.to_owned())
    }

    async fn refresh_workbuddy(&self, session: &workbuddy::Session) -> Result<Credential, Error> {
        let base = self.workbuddy_base();
        let mut fresh = workbuddy::refresh(&self.client, &base, session, unix_now()).await?;
        if fresh.account.is_none() {
            fresh.account.clone_from(&session.account);
        }
        let credential = Credential::Workbuddy(fresh);
        let mut config = lock(&self.config);
        config
            .credentials
            .insert(ProviderId::Workbuddy, credential.clone());
        config.save_credentials()?;
        Ok(credential)
    }

    /// Rotate a Claude subscription grant and persist it. A failed refresh
    /// surfaces `Unauthorized` so the UI can ask the learner to sign in again.
    async fn refresh_claude(&self, session: &claude_oauth::Session) -> Result<Credential, Error> {
        let fresh = claude_oauth::refresh(&self.client, session, unix_now()).await?;
        let credential = Credential::ClaudeCode(fresh);
        let mut config = lock(&self.config);
        config
            .credentials
            .insert(ProviderId::ClaudeCode, credential.clone());
        config.save_credentials()?;
        Ok(credential)
    }

    /// Rotate a ChatGPT grant and persist it (including a rotated refresh
    /// token). A failed refresh surfaces `Unauthorized` ("sign in again").
    async fn refresh_codex(&self, session: &codex_oauth::Session) -> Result<Credential, Error> {
        let fresh = codex_oauth::refresh(&self.client, session, unix_now()).await?;
        let credential = Credential::Codex(fresh);
        let mut config = lock(&self.config);
        config
            .credentials
            .insert(ProviderId::OpenaiCodex, credential.clone());
        config.save_credentials()?;
        Ok(credential)
    }
}

/// Vision capability for the composer (W2/W3). `None` = unverified.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VisionCapabilities {
    pub tutor: Option<bool>,
    pub practice: Option<bool>,
}

/// Metadata for a plugin's interactive view (the document is served by the
/// `pluginview` scheme, not sent here).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginView {
    pub name: String,
    pub calls: Vec<String>,
    pub answer: bool,
}

/// A view-state summary as one line: control characters and line breaks
/// become spaces, capped at 500 characters (W7).
pub(crate) fn sanitize_view_state(summary: &str) -> String {
    summary
        .chars()
        .map(|c| match c {
            c if c.is_control() => ' ',
            '\u{2028}' | '\u{2029}' => ' ',
            c => c,
        })
        .take(500)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// The Practice prompt, rendered like every other prompt (unchanged until the
/// coach is built). A learner override in `<repo>/prompts/practice.md` wins
/// over the bundled text.
fn practice_prompt(repo: &Path) -> Result<String, Error> {
    Composer::new(repo).compose_practice()
}

/// A poisoned lock only means another command panicked mid-update of plain
/// data; keep serving.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn unix_now() -> i64 {
    chrono::Utc::now().timestamp()
}
