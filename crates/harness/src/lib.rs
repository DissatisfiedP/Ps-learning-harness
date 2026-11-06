//! App core: sessions, provider connections, and the agent runtime (tutor,
//! planner, researcher). The Tauri shell is a thin command layer over
//! [`Harness`].

mod agent;
mod config;
pub mod events;
mod files;
mod jobs;
mod map;
pub mod plugins;
mod prompts;
pub mod replay;
mod sessions;
pub mod state;
pub mod tools;
mod web;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use providers::workbuddy::{self, LoginStart};
use providers::{
    stream_chat, ChatRequest, Client, ContentBlock, Credential, CredentialKind, Message, ModelInfo,
    ProviderError, ProviderId, Role, StopReason, StreamEvent, ToolSpec, Usage,
};
use serde::Serialize;
use tokio::sync::{oneshot, Semaphore};

pub use config::{ModelChoice, ModelRole, Roles, Settings};
pub use events::{AgentEvent, AgentName, Coalescer, Envelope, EventSink};
pub use map::{MapNode, MapPlan};
pub use prompts::{Composer, PluginManifest, TemplateValues};
pub use providers;
pub use sessions::{Ending, Mode, Op, RemovedPlugin, Session, SessionSummary, StoredMessage};
pub use state::SessionState;

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
    pub models: Vec<ModelInfo>,
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
    /// Input/output tokens per role, accumulated in memory.
    pub(crate) role_usage: Mutex<HashMap<ModelRole, RoleUsage>>,
    /// Per-session turn lock: tutor turns, answers and job deliveries for one
    /// session are serialized; different sessions never wait on each other.
    pub(crate) turn_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Registered plugins (scaffolding, M5).
    pub(crate) plugins: Mutex<plugins::PluginRegistry>,
    /// Running jobs by job id.
    pub(crate) jobs: Mutex<HashMap<String, jobs::JobHandle>>,
    /// Background tasks by session (jobs, wakes, titles).
    pub(crate) background: Mutex<HashMap<String, Vec<tokio::task::JoinHandle<()>>>>,
    /// Sessions whose interrupted jobs were already restarted.
    pub(crate) recovered: Mutex<HashSet<String>>,
    /// Concurrency per role: Reader 3 (researcher, titles), Planner 1.
    pub(crate) reader_slots: Arc<Semaphore>,
    pub(crate) planner_slots: Arc<Semaphore>,
    /// Where runtime events go (the UI); `None` drops them.
    pub(crate) sink: Mutex<Option<EventSink>>,
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
            role_usage: Mutex::new(HashMap::new()),
            turn_locks: Mutex::new(HashMap::new()),
            plugins: Mutex::new(plugins::PluginRegistry::default()),
            jobs: Mutex::new(HashMap::new()),
            background: Mutex::new(HashMap::new()),
            recovered: Mutex::new(HashSet::new()),
            reader_slots: Arc::new(Semaphore::new(3)),
            planner_slots: Arc::new(Semaphore::new(1)),
            sink: Mutex::new(None),
        })
    }

    /// Where runtime events (turns, ops, jobs) go.
    pub fn set_event_sink(&self, sink: Option<EventSink>) {
        *lock(&self.sink) = sink;
    }

    // ---- sessions -------------------------------------------------------

    pub fn list_sessions(&self) -> Result<Vec<SessionSummary>, Error> {
        self.store.list()
    }

    pub fn load_session(&self, id: &str) -> Result<Session, Error> {
        self.store.load(id)
    }

    pub fn delete_session(&self, id: &str) -> Result<(), Error> {
        self.cancel(id);
        let jobs: Vec<String> = lock(&self.jobs)
            .iter()
            .filter(|(_, h)| h.session_id == id)
            .map(|(k, _)| k.clone())
            .collect();
        for job in jobs {
            let _ = self.cancel_job(id, &job);
        }
        self.store.delete(id)
    }

    /// The Map page's data: every non-legacy Learn session with an approved
    /// plan, newest first, with check verdicts per idea (ui-design §7.6).
    pub fn learning_map(&self) -> Result<Vec<MapPlan>, Error> {
        let mut out = Vec::new();
        for summary in self.store.list()? {
            if summary.mode != Mode::Learn || summary.read_only {
                continue;
            }
            let state = self.store.with_ops(&summary.id, replay::replay_state)?;
            out.extend(map::plan_of(summary, state));
        }
        Ok(out)
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
        if mode != Mode::Practice {
            return Err(Error::Invalid(
                "Learn sessions run through the tutor; use start_teaching".into(),
            ));
        }
        let text = text.trim();
        if text.is_empty() {
            return Err(Error::Invalid("message is empty".into()));
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
        self.store
            .append(&id, &sessions::new_message(Role::User, text.to_owned()))?;
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
        let history: Vec<Message> = session
            .messages
            .iter()
            .filter(|m| !m.content.trim().is_empty())
            .map(|m| Message::text(m.role, m.content.clone()))
            .collect();
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
            (result, _) => Ok(result?),
        }
    }

    // ---- providers ------------------------------------------------------

    pub fn settings(&self) -> Settings {
        lock(&self.config).settings.clone()
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
                    models: id.suggested_models(),
                }
            })
            .collect()
    }

    /// Model list for the picker. WorkBuddy's comes from the account's live
    /// catalog; the others are suggestions (any id can be typed).
    pub async fn models(&self, provider: ProviderId) -> Result<Vec<ModelInfo>, Error> {
        if provider != ProviderId::Workbuddy {
            return Ok(provider.suggested_models());
        }
        let credential = self.current_credential(provider).await?;
        let base = self.workbuddy_base();
        match (
            workbuddy::list_models(&self.client, &base, &credential).await,
            &credential,
        ) {
            (Err(ProviderError::Unauthorized(_)), Credential::Workbuddy(s))
                if s.can_refresh(unix_now()) =>
            {
                let fresh = self.refresh_workbuddy(s).await?;
                Ok(workbuddy::list_models(&self.client, &base, &fresh).await?)
            }
            (result, _) => Ok(result?),
        }
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
                        .models(ProviderId::Workbuddy)
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

    /// The saved or environment credential; a WorkBuddy session is refreshed
    /// first when less than a day of it remains.
    async fn current_credential(&self, provider: ProviderId) -> Result<Credential, Error> {
        let credential = self.credential(provider)?;
        match &credential {
            Credential::Workbuddy(s)
                if s.needs_refresh(unix_now()) && s.can_refresh(unix_now()) =>
            {
                self.refresh_workbuddy(s).await
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
}

/// The Practice prompt (unchanged until the coach is built). A learner
/// override in `<repo>/prompts/practice.md` wins over the bundled text.
fn practice_prompt(repo: &Path) -> Result<String, Error> {
    Composer::new(repo).load("practice.md")
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
