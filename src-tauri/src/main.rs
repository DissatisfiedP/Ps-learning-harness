//! Tauri shell: window + IPC commands over [`harness::Harness`].
//!
//! Every command that touches disk or the network is `async` (Tauri runs
//! non-async commands on the main thread) and does blocking file IO inside
//! `spawn_blocking`. Learn-session events (turns, ops, jobs) arrive on one
//! channel the UI registers with `subscribe_events`; text deltas are
//! coalesced (~16 ms / 2 KB) before they cross the IPC boundary.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use harness::providers::workbuddy::LoginStart;
use harness::providers::{ModelInfo, ProviderId};
use harness::{
    AgentEvent, ChatEvent, Coalescer, Envelope, Harness, Mode, ModelChoice, ModelRole,
    ProviderStatus, RemovedPlugin, RoleInfo, RoleTestResult, RoleUsageEntry, Session, SessionState,
    SessionSummary, Settings, StoredMessage,
};
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{Manager, State};
use tauri_plugin_opener::OpenerExt;

type Result<T, E = String> = std::result::Result<T, E>;
type H<'a> = State<'a, Arc<Harness>>;

/// The UI's event channel for Learn sessions (replaced on each subscribe).
#[derive(Default)]
struct Events(Arc<Mutex<Option<Channel>>>);

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn forward(channel: &Channel) -> impl FnMut(ChatEvent<'_>) + Send + '_ {
    move |event| {
        if let Ok(json) = serde_json::to_string(&event) {
            // A closed channel means the window went away; the turn still
            // completes and is saved.
            let _ = channel.send(InvokeResponseBody::Json(json));
        }
    }
}

/// Run blocking IO off the async workers.
async fn blocking<T: Send + 'static>(
    h: &Arc<Harness>,
    f: impl FnOnce(&Harness) -> Result<T, harness::Error> + Send + 'static,
) -> Result<T> {
    let h = Arc::clone(h);
    tauri::async_runtime::spawn_blocking(move || f(&h))
        .await
        .map_err(err)?
        .map_err(err)
}

// ---- sessions ---------------------------------------------------------------

#[tauri::command]
async fn list_sessions(h: H<'_>) -> Result<Vec<SessionSummary>> {
    blocking(&h, |h| h.list_sessions()).await
}

#[tauri::command]
async fn load_session(h: H<'_>, id: String) -> Result<Session> {
    let harness = Arc::clone(&h);
    let session = blocking(&h, {
        let id = id.clone();
        move |h| h.load_session(&id)
    })
    .await?;
    // Jobs a previous run left unfinished are restarted once.
    if session.summary.mode == Mode::Learn && !session.summary.read_only {
        harness.recover_jobs(&id).map_err(err)?;
    }
    Ok(session)
}

#[tauri::command]
async fn delete_session(h: H<'_>, id: String) -> Result<()> {
    blocking(&h, move |h| h.delete_session(&id)).await
}

#[tauri::command]
async fn session_state(h: H<'_>, session_id: String) -> Result<SessionState> {
    blocking(&h, move |h| h.session_state(&session_id)).await
}

#[tauri::command]
fn subscribe_events(events: State<'_, Events>, on_event: Channel) {
    if let Ok(mut slot) = events.0.lock() {
        *slot = Some(on_event);
    }
}

// ---- Learn (agent runtime) ----------------------------------------------------

#[tauri::command]
async fn start_teaching(h: H<'_>, text: String) -> Result<SessionSummary> {
    h.start_teaching(&text).await.map_err(err)
}

#[tauri::command]
async fn send_learner_message(h: H<'_>, session_id: String, text: String) -> Result<()> {
    h.send_learner_message(&session_id, &text)
        .await
        .map_err(err)
}

#[tauri::command]
async fn answer(
    h: H<'_>,
    session_id: String,
    call_id: String,
    payload: serde_json::Value,
) -> Result<()> {
    h.answer(&session_id, &call_id, payload).await.map_err(err)
}

#[tauri::command]
async fn approve(h: H<'_>, session_id: String, removed_plugins: Vec<RemovedPlugin>) -> Result<()> {
    h.approve(&session_id, removed_plugins).await.map_err(err)
}

#[tauri::command]
async fn request_plan_change(h: H<'_>, session_id: String, text: String) -> Result<()> {
    h.request_plan_change(&session_id, &text).await.map_err(err)
}

#[tauri::command]
async fn resume_turn(h: H<'_>, session_id: String) -> Result<()> {
    h.resume_turn(&session_id).await.map_err(err)
}

#[tauri::command]
fn cancel_job(h: H<'_>, session_id: String, job_id: String) -> Result<()> {
    h.cancel_job(&session_id, &job_id).map_err(err)
}

// ---- Practice (single prompt, unchanged until the coach) -----------------------

#[tauri::command]
async fn send_message(
    h: H<'_>,
    session_id: Option<String>,
    mode: Mode,
    text: String,
    on_event: Channel,
) -> Result<StoredMessage> {
    h.send(session_id.as_deref(), mode, &text, &mut forward(&on_event))
        .await
        .map_err(err)
}

#[tauri::command]
async fn retry_reply(h: H<'_>, session_id: String, on_event: Channel) -> Result<StoredMessage> {
    h.retry(&session_id, &mut forward(&on_event))
        .await
        .map_err(err)
}

#[tauri::command]
fn cancel_reply(h: H<'_>, session_id: String) {
    h.cancel(&session_id);
}

// ---- settings ----------------------------------------------------------------

#[tauri::command]
fn get_settings(h: H<'_>) -> Settings {
    h.settings()
}

#[tauri::command]
fn list_providers(h: H<'_>) -> Vec<ProviderStatus> {
    h.providers()
}

#[tauri::command]
async fn list_models(h: H<'_>, provider: ProviderId) -> Result<Vec<ModelInfo>> {
    h.models(provider).await.map_err(err)
}

#[tauri::command]
async fn save_api_key(h: H<'_>, provider: ProviderId, key: String) -> Result<()> {
    blocking(&h, move |h| h.save_api_key(provider, &key)).await
}

#[tauri::command]
async fn remove_credential(h: H<'_>, provider: ProviderId) -> Result<()> {
    blocking(&h, move |h| h.remove_credential(provider)).await
}

#[tauri::command]
async fn set_base_url(h: H<'_>, provider: ProviderId, url: Option<String>) -> Result<()> {
    blocking(&h, move |h| h.set_base_url(provider, url.as_deref())).await
}

#[tauri::command]
async fn set_role_model(h: H<'_>, role: ModelRole, choice: Option<ModelChoice>) -> Result<()> {
    h.set_role_model(role, choice).await.map_err(err)
}

#[tauri::command]
async fn test_role(h: H<'_>, role: ModelRole) -> Result<RoleTestResult> {
    h.test_role(role).await.map_err(err)
}

#[tauri::command]
fn list_roles(h: H<'_>) -> Vec<RoleInfo> {
    h.roles()
}

#[tauri::command]
fn role_usage(h: H<'_>) -> Vec<RoleUsageEntry> {
    h.role_usage()
}

/// Starts WorkBuddy browser sign-in and opens the approval page.
#[tauri::command]
async fn workbuddy_login_start(app: tauri::AppHandle, h: H<'_>) -> Result<LoginStart> {
    let start = h.workbuddy_login_start().await.map_err(err)?;
    // If no browser can be launched the UI still shows the link.
    let _ = app.opener().open_url(&start.auth_url, None::<&str>);
    Ok(start)
}

#[tauri::command]
async fn workbuddy_login_wait(h: H<'_>, state: String) -> Result<Option<String>> {
    h.workbuddy_login_wait(&state).await.map_err(err)
}

#[tauri::command]
fn workbuddy_login_cancel(h: H<'_>) {
    h.workbuddy_login_cancel();
}

/// Opens provider key pages and similar links in the system browser.
#[tauri::command]
fn open_external(app: tauri::AppHandle, url: String) -> Result<()> {
    if !url.starts_with("https://") {
        return Err("only https links can be opened".into());
    }
    app.opener().open_url(url, None::<&str>).map_err(err)
}

fn data_dirs(
    app: &tauri::App,
) -> std::result::Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
    let paths = app.path();
    let repo = match std::env::var_os("LEARNING_HARNESS_REPO") {
        Some(dir) => PathBuf::from(dir),
        None => paths.app_data_dir()?.join("repo"),
    };
    Ok((repo, paths.app_config_dir()?))
}

/// Route runtime events through a coalescer to the subscribed channel.
fn wire_events(harness: &Harness, events: &Events) {
    let slot = Arc::clone(&events.0);
    let coalescer = Coalescer::new(Arc::new(move |session_id: &str, event: AgentEvent| {
        let envelope = Envelope {
            session_id,
            event: &event,
        };
        let Ok(json) = serde_json::to_string(&envelope) else {
            return;
        };
        if let Ok(slot) = slot.lock() {
            if let Some(channel) = slot.as_ref() {
                let _ = channel.send(InvokeResponseBody::Json(json));
            }
        }
    }));
    harness.set_event_sink(Some(coalescer.sink()));
    // Flush a stalled tail of text even when no further delta arrives.
    tauri::async_runtime::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(16));
        loop {
            tick.tick().await;
            coalescer.flush_due();
        }
    });
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let (repo, config) = data_dirs(app)?;
            let harness = Arc::new(Harness::open(&repo, &config)?);
            let events = Events::default();
            wire_events(&harness, &events);
            app.manage(harness);
            app.manage(events);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_sessions,
            load_session,
            delete_session,
            session_state,
            subscribe_events,
            start_teaching,
            send_learner_message,
            answer,
            approve,
            request_plan_change,
            resume_turn,
            cancel_job,
            send_message,
            retry_reply,
            cancel_reply,
            get_settings,
            list_providers,
            list_models,
            save_api_key,
            remove_credential,
            set_base_url,
            set_role_model,
            test_role,
            list_roles,
            role_usage,
            workbuddy_login_start,
            workbuddy_login_wait,
            workbuddy_login_cancel,
            open_external,
        ])
        .run(tauri::generate_context!())
        .expect("failed to start Learning Harness");
}
