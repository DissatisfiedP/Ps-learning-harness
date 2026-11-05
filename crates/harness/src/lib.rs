//! App core: sessions, provider connections, and running a tutor turn.
//! The Tauri shell is a thin command layer over [`Harness`].

mod config;
mod sessions;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use providers::workbuddy::{self, LoginStart};
use providers::{
    stream_chat, ChatRequest, Client, Credential, CredentialKind, Message, ModelInfo,
    ProviderError, ProviderId, Role, StopReason, StreamEvent,
};
use serde::Serialize;
use tokio::sync::oneshot;

pub use config::{ModelChoice, Settings};
pub use providers;
pub use sessions::{Ending, Mode, Session, SessionSummary, StoredMessage};

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
}

/// Streamed to the UI while a turn runs.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ChatEvent<'a> {
    /// First event of `send` when it created a new session.
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

pub struct Harness {
    repo: PathBuf,
    client: Client,
    store: sessions::Store,
    config: Mutex<config::Config>,
    running: Mutex<HashMap<String, oneshot::Sender<()>>>,
    login_cancelled: AtomicBool,
}

impl Harness {
    /// `repo` is the synced data repo; `config_dir` is device-local.
    pub fn open(repo: &Path, config_dir: &Path) -> Result<Self, Error> {
        std::fs::create_dir_all(repo)?;
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .build()
            .map_err(|e| Error::Invalid(format!("http client: {e}")))?;
        Ok(Self {
            repo: repo.to_owned(),
            client,
            store: sessions::Store::new(repo),
            config: Mutex::new(config::Config::load(config_dir)?),
            running: Mutex::new(HashMap::new()),
            login_cancelled: AtomicBool::new(false),
        })
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
        self.store.delete(id)
    }

    /// Appends the learner's message (creating the session when `session_id`
    /// is `None`) and streams the tutor's reply.
    pub async fn send(
        &self,
        session_id: Option<&str>,
        mode: Mode,
        text: &str,
        on_event: &mut (dyn FnMut(ChatEvent<'_>) + Send),
    ) -> Result<StoredMessage, Error> {
        let text = text.trim();
        if text.is_empty() {
            return Err(Error::Invalid("message is empty".into()));
        }
        // Fail before writing anything if no model is usable.
        self.model_choice()?;
        let id = match session_id {
            Some(id) => {
                self.store.load(id)?;
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

    /// Re-runs the tutor after a failed turn (last message must be the learner's).
    pub async fn retry(
        &self,
        session_id: &str,
        on_event: &mut (dyn FnMut(ChatEvent<'_>) + Send),
    ) -> Result<StoredMessage, Error> {
        let session = self.store.load(session_id)?;
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
        let system = prompt(&self.repo, session.summary.mode)?;
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
            Some(Ok(done)) => match done.stop {
                StopReason::End | StopReason::ToolUse => None,
                StopReason::MaxTokens => Some(Ending::MaxTokens),
                StopReason::Refusal => Some(Ending::Refusal),
            },
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

    /// WorkBuddy gets `reasoning_effort` only for efforts the model's live
    /// catalog entry lists in `reasoning.supportedEfforts`; any other effort
    /// is dropped here so it is never sent.
    pub async fn set_model(&self, choice: ModelChoice) -> Result<(), Error> {
        let model = choice.model.trim().to_owned();
        if model.is_empty() {
            return Err(Error::Invalid("model id is empty".into()));
        }
        let effort = match (choice.provider, choice.effort) {
            (ProviderId::Workbuddy, Some(effort)) => self
                .models(ProviderId::Workbuddy)
                .await?
                .iter()
                .find(|m| m.id == model)
                .filter(|m| m.efforts.contains(&effort))
                .map(|_| effort),
            (_, effort) => effort,
        };
        let mut config = lock(&self.config);
        config.settings.model = Some(ModelChoice {
            provider: choice.provider,
            model,
            effort,
        });
        config.save_settings()
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
        let choice = lock(&self.config)
            .settings
            .model
            .clone()
            .ok_or_else(|| Error::Invalid("choose a model in Settings first".into()))?;
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

/// Mode prompt: the learner's override in `<repo>/prompts/<mode>.md` wins over
/// the bundled text.
fn prompt(repo: &Path, mode: Mode) -> Result<String, Error> {
    let (name, bundled) = match mode {
        Mode::Learn => ("learn.md", include_str!("../prompts/learn.md")),
        Mode::Practice => ("practice.md", include_str!("../prompts/practice.md")),
    };
    match std::fs::read_to_string(repo.join("prompts").join(name)) {
        Ok(text) if !text.trim().is_empty() => Ok(text),
        Ok(_) => Ok(bundled.to_owned()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(bundled.to_owned()),
        Err(e) => Err(e.into()),
    }
}

/// A poisoned lock only means another command panicked mid-update of plain
/// data; keep serving.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn unix_now() -> i64 {
    chrono::Utc::now().timestamp()
}
