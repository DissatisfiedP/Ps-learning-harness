//! Tauri shell: window + IPC commands over [`harness::Harness`].

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use harness::providers::workbuddy::LoginStart;
use harness::providers::{ModelInfo, ProviderId};
use harness::{
    ChatEvent, Harness, Mode, ModelChoice, ModelRole, ProviderStatus, RoleInfo, RoleTestResult,
    RoleUsageEntry, Session, SessionSummary, Settings, StoredMessage,
};
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{Manager, State};
use tauri_plugin_opener::OpenerExt;

type Result<T, E = String> = std::result::Result<T, E>;

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

#[tauri::command]
fn list_sessions(h: State<'_, Harness>) -> Result<Vec<SessionSummary>> {
    h.list_sessions().map_err(err)
}

#[tauri::command]
fn load_session(h: State<'_, Harness>, id: String) -> Result<Session> {
    h.load_session(&id).map_err(err)
}

#[tauri::command]
fn delete_session(h: State<'_, Harness>, id: String) -> Result<()> {
    h.delete_session(&id).map_err(err)
}

#[tauri::command]
async fn send_message(
    h: State<'_, Harness>,
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
async fn retry_reply(
    h: State<'_, Harness>,
    session_id: String,
    on_event: Channel,
) -> Result<StoredMessage> {
    h.retry(&session_id, &mut forward(&on_event))
        .await
        .map_err(err)
}

#[tauri::command]
fn cancel_reply(h: State<'_, Harness>, session_id: String) {
    h.cancel(&session_id);
}

#[tauri::command]
fn get_settings(h: State<'_, Harness>) -> Settings {
    h.settings()
}

#[tauri::command]
fn list_providers(h: State<'_, Harness>) -> Vec<ProviderStatus> {
    h.providers()
}

#[tauri::command]
async fn list_models(h: State<'_, Harness>, provider: ProviderId) -> Result<Vec<ModelInfo>> {
    h.models(provider).await.map_err(err)
}

#[tauri::command]
fn save_api_key(h: State<'_, Harness>, provider: ProviderId, key: String) -> Result<()> {
    h.save_api_key(provider, &key).map_err(err)
}

#[tauri::command]
fn remove_credential(h: State<'_, Harness>, provider: ProviderId) -> Result<()> {
    h.remove_credential(provider).map_err(err)
}

#[tauri::command]
fn set_base_url(h: State<'_, Harness>, provider: ProviderId, url: Option<String>) -> Result<()> {
    h.set_base_url(provider, url.as_deref()).map_err(err)
}

#[tauri::command]
async fn set_role_model(
    h: State<'_, Harness>,
    role: ModelRole,
    choice: Option<ModelChoice>,
) -> Result<()> {
    h.set_role_model(role, choice).await.map_err(err)
}

#[tauri::command]
async fn test_role(h: State<'_, Harness>, role: ModelRole) -> Result<RoleTestResult> {
    h.test_role(role).await.map_err(err)
}

#[tauri::command]
fn list_roles(h: State<'_, Harness>) -> Vec<RoleInfo> {
    h.roles()
}

#[tauri::command]
fn role_usage(h: State<'_, Harness>) -> Vec<RoleUsageEntry> {
    h.role_usage()
}

/// Starts WorkBuddy browser sign-in and opens the approval page.
#[tauri::command]
async fn workbuddy_login_start(app: tauri::AppHandle, h: State<'_, Harness>) -> Result<LoginStart> {
    let start = h.workbuddy_login_start().await.map_err(err)?;
    // If no browser can be launched the UI still shows the link.
    let _ = app.opener().open_url(&start.auth_url, None::<&str>);
    Ok(start)
}

#[tauri::command]
async fn workbuddy_login_wait(h: State<'_, Harness>, state: String) -> Result<Option<String>> {
    h.workbuddy_login_wait(&state).await.map_err(err)
}

#[tauri::command]
fn workbuddy_login_cancel(h: State<'_, Harness>) {
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

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let (repo, config) = data_dirs(app)?;
            app.manage(Harness::open(&repo, &config)?);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_sessions,
            load_session,
            delete_session,
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
