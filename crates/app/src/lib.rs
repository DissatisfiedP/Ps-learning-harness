//! Tauri shell: window + IPC commands over [`harness::Harness`].
//!
//! Every command that touches disk or the network is `async` (Tauri runs
//! non-async commands on the main thread) and does blocking file IO inside
//! `spawn_blocking`. Learn-session events (turns, ops, jobs) arrive on one
//! channel the UI registers with `subscribe_events`; text deltas are
//! coalesced (~16 ms / 2 KB) before they cross the IPC boundary.

mod focus;
#[cfg(target_os = "linux")]
mod pen;
#[cfg(target_os = "linux")]
mod pinch;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use harness::providers::workbuddy::LoginStart;
use harness::providers::{ModelInfo, ProviderId};
use harness::{
    AgentEvent, Coalescer, Envelope, Harness, KnowledgeMap, Mode, ModelChoice, ModelRole,
    ProviderStatus, RemovedPlugin, RoleInfo, RoleTestResult, RoleUsageEntry, Session, SessionState,
    SessionSummary, Settings,
};
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{Manager, RunEvent, State, WindowEvent};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

type Result<T, E = String> = std::result::Result<T, E>;
type H<'a> = State<'a, Arc<Harness>>;

/// The UI's event channel for Learn sessions (replaced on each subscribe).
#[derive(Default)]
struct Events(Arc<Mutex<Option<Channel>>>);

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// CSP for a plugin view document. The view is a separate, sandboxed origin:
/// it may run its own inline script and styles, and fetch nothing. The
/// `sandbox` directive keeps the document opaque-origin even if it is ever
/// loaded outside the `<iframe sandbox>` the UI creates.
const VIEW_CSP: &str = "sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; font-src data:; connect-src 'none'; base-uri 'none'; form-action 'none'";

/// The view document's URL. A `srcdoc` frame would inherit the app CSP
/// (`script-src 'self'`), which blocks the view's inline script; a custom
/// scheme gives the document its own CSP.
#[cfg(any(target_os = "windows", target_os = "android"))]
fn view_url(id: &str) -> String {
    format!("http://pluginview.localhost/{id}")
}

#[cfg(not(any(target_os = "windows", target_os = "android")))]
fn view_url(id: &str) -> String {
    format!("pluginview://localhost/{id}")
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

/// "Continue here anyway" on a session locked on mobile for a desktop-only
/// plugin: records the choice and returns the session as `load_session` does.
#[tauri::command]
async fn mobile_unlock(h: H<'_>, session_id: String) -> Result<Session> {
    let harness = Arc::clone(&h);
    let session = blocking(&h, {
        let id = session_id.clone();
        move |h| h.mobile_unlock(&id)
    })
    .await?;
    // Now writable: restart what a previous run left unfinished, as on open.
    if !session.summary.read_only {
        harness.recover_jobs(&session_id).map_err(err)?;
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

/// The Map page: the standalone knowledge graph (topics, claims, edges).
#[tauri::command]
async fn knowledge_map(h: H<'_>) -> Result<KnowledgeMap> {
    blocking(&h, |h| h.knowledge_map()).await
}

/// The learner model per graph node, for the Map's node card (arch §4.8).
#[tauri::command]
async fn learner_nodes(
    h: H<'_>,
) -> Result<std::collections::BTreeMap<String, harness::learner::NodeView>> {
    blocking(&h, |h| h.learner_nodes()).await
}

/// Nodes due for review, most depended-on first (arch §4.13).
#[tauri::command]
async fn review_queue(h: H<'_>) -> Result<Vec<harness::learner::ReviewItem>> {
    blocking(&h, |h| h.review_queue()).await
}

/// The Map page's review list for `extract_edges` output (ui-design §7.6).
#[tauri::command]
async fn edge_proposals(h: H<'_>) -> Result<harness::proposals::Review> {
    blocking(&h, |h| h.edge_proposals()).await
}

/// Accepts/dismisses proposals by key; the core validates before touching
/// `graph/edges.jsonl`.
#[tauri::command]
async fn decide_edge_proposals(
    h: H<'_>,
    accept: Vec<String>,
    dismiss: Vec<String>,
) -> Result<harness::proposals::Decision> {
    blocking(&h, move |h| h.decide_edge_proposals(&accept, &dismiss)).await
}

/// Extract by hand: build the knowledge graph from the session's teaching
/// not yet in it (the `graph` job). Returns the job id; it never writes into
/// the conversation.
#[tauri::command]
async fn graph_build(h: H<'_>, session_id: String) -> Result<String> {
    let harness = Arc::clone(&h);
    tauri::async_runtime::spawn_blocking(move || harness.graph_build(&session_id))
        .await
        .map_err(err)?
        .map_err(err)
}

/// What the Home list's Extract item needs: uncovered tutor tokens, the
/// manual minimum and whether a build is running.
#[tauri::command]
async fn graph_status(h: H<'_>, session_id: String) -> Result<harness::ExtractionStatus> {
    blocking(&h, move |h| h.graph_status(&session_id)).await
}

/// Recompute the topic-split proposals for the Map page.
#[tauri::command]
async fn graph_splits(h: H<'_>) -> Result<Vec<harness::ProposedSplit>> {
    blocking(&h, |h| h.graph_splits()).await
}

/// Accept or dismiss proposed topic splits; accepting appends `split` ops,
/// with the learner's names for the groups (`titles`: topic → name per group).
#[tauri::command]
async fn decide_splits(
    h: H<'_>,
    accept: Vec<String>,
    dismiss: Vec<String>,
    titles: Option<std::collections::HashMap<String, Vec<String>>>,
) -> Result<harness::graph::SplitDecision> {
    let titles = titles.unwrap_or_default();
    blocking(&h, move |h| h.decide_splits(&accept, &dismiss, &titles)).await
}

/// The learner moved an idea to another topic on the Map (a learner `place` op).
#[tauri::command]
async fn place_node(h: H<'_>, node_id: String, home: String) -> Result<harness::graph::Placed> {
    blocking(&h, move |h| h.place_node(&node_id, &home)).await
}

/// Where an idea came from, for the Map's card.
#[tauri::command]
async fn node_provenance(
    h: H<'_>,
    node_id: String,
) -> Result<harness::graph::history::NodeHistory> {
    blocking(&h, move |h| h.node_provenance(&node_id)).await
}

#[tauri::command]
fn subscribe_events(events: State<'_, Events>, on_event: Channel) {
    if let Ok(mut slot) = events.0.lock() {
        *slot = Some(on_event);
    }
}

// ---- Learn (agent runtime) ----------------------------------------------------

#[tauri::command]
async fn start_teaching(
    h: H<'_>,
    text: String,
    attachments: Option<Vec<harness::Attachment>>,
) -> Result<SessionSummary> {
    h.start_teaching_with(&text, attachments.as_deref().unwrap_or_default())
        .await
        .map_err(err)
}

#[tauri::command]
async fn send_learner_message(
    h: H<'_>,
    session_id: String,
    text: String,
    attachments: Option<Vec<harness::Attachment>>,
) -> Result<()> {
    h.send_learner_message_with(
        &session_id,
        &text,
        attachments.as_deref().unwrap_or_default(),
    )
    .await
    .map_err(err)
}

/// The learner's comment from the desk: interrupts the tutor (stops a reply,
/// sets an open question aside) and is answered now.
#[tauri::command]
async fn comment(h: H<'_>, session_id: String, text: String) -> Result<()> {
    h.comment(&session_id, &text).await.map_err(err)
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

/// The learner's decision on an install offer (`sessionState.installOffers`):
/// the plugin is installed now, or they turned the offer down.
#[tauri::command]
async fn resolve_install_offer(
    h: H<'_>,
    session_id: String,
    plugin: String,
    installed: bool,
) -> Result<()> {
    h.resolve_install_offer(&session_id, &plugin, installed)
        .await
        .map_err(err)
}

#[tauri::command]
async fn request_plan_change(h: H<'_>, session_id: String, text: String) -> Result<()> {
    h.request_plan_change(&session_id, &text).await.map_err(err)
}

#[tauri::command]
async fn edit_plan(
    h: H<'_>,
    session_id: String,
    edits: serde_json::Value,
    comment: Option<String>,
) -> Result<()> {
    h.edit_plan(&session_id, edits, comment.as_deref())
        .await
        .map_err(err)
}

#[tauri::command]
async fn answer_plan_question(h: H<'_>, session_id: String, answer: String) -> Result<()> {
    h.answer_plan_question(&session_id, &answer)
        .await
        .map_err(err)
}

/// The learner came back from a hunt (docs/learning-system.md §5): notes,
/// links and files are all optional.
#[tauri::command]
async fn hunt_return(
    h: H<'_>,
    session_id: String,
    hunt_id: String,
    payload: serde_json::Value,
) -> Result<()> {
    h.hunt_return(&session_id, &hunt_id, payload)
        .await
        .map_err(err)
}

#[tauri::command]
async fn hunt_skip(h: H<'_>, session_id: String, hunt_id: String) -> Result<()> {
    h.hunt_skip(&session_id, &hunt_id).await.map_err(err)
}

/// The learner's hunt toggle on the plan card or sheet (§5). Only the learner
/// may enable a hunt; no turn is run.
#[tauri::command]
async fn set_hunt(h: H<'_>, session_id: String, section_id: String, enabled: bool) -> Result<()> {
    h.set_hunt(&session_id, &section_id, enabled).map_err(err)
}

/// The frontier list: topics parked by hunts and debriefs.
#[tauri::command]
async fn frontier(h: H<'_>) -> Result<Vec<harness::FrontierItem>> {
    blocking(&h, |h| h.frontier()).await
}

/// Start a new Learn session from a frontier item (§5).
#[tauri::command]
async fn start_from_frontier(h: H<'_>, item_id: String) -> Result<SessionSummary> {
    h.start_from_frontier(&item_id).await.map_err(err)
}

#[tauri::command]
async fn resume_turn(h: H<'_>, session_id: String) -> Result<()> {
    h.resume_turn(&session_id).await.map_err(err)
}

#[tauri::command]
fn cancel_job(h: H<'_>, session_id: String, job_id: String) -> Result<()> {
    h.cancel_job(&session_id, &job_id).map_err(err)
}

// ---- Practice (the coach, docs/ui-design.md §11b) ---------------------------------

/// New Practice session on the Map's picked ideas (none: the scheduler
/// picks). `format` is `stream` (default) or `set`.
#[tauri::command]
async fn start_practice(
    h: H<'_>,
    targets: Option<Vec<String>>,
    format: Option<String>,
    size: Option<u32>,
) -> Result<SessionSummary> {
    h.start_practice(
        targets.as_deref().unwrap_or_default(),
        format.as_deref().unwrap_or("stream"),
        size,
    )
    .await
    .map_err(err)
}

/// What the learner may see of a Practice session.
#[tauri::command]
async fn practice_state(h: H<'_>, session_id: String) -> Result<harness::PracticeView> {
    blocking(&h, move |h| h.practice_state(&session_id)).await
}

/// One submission: working, a question or a claimed answer.
#[tauri::command]
async fn practice_submit(
    h: H<'_>,
    session_id: String,
    text: String,
    confidence: Option<u8>,
    attachments: Option<Vec<harness::Attachment>>,
) -> Result<()> {
    h.practice_submit(
        &session_id,
        &text,
        confidence,
        attachments.as_deref().unwrap_or_default(),
    )
    .await
    .map_err(err)
}

#[tauri::command]
async fn practice_hint(h: H<'_>, session_id: String) -> Result<()> {
    h.practice_hint(&session_id).await.map_err(err)
}

#[tauri::command]
async fn practice_dismiss(h: H<'_>, session_id: String) -> Result<()> {
    h.practice_dismiss(&session_id).await.map_err(err)
}

#[tauri::command]
async fn practice_next(h: H<'_>, session_id: String) -> Result<()> {
    h.practice_next(&session_id).await.map_err(err)
}

#[tauri::command]
async fn practice_submit_set(
    h: H<'_>,
    session_id: String,
    answers: Vec<harness::practice::SetAnswer>,
) -> Result<()> {
    h.practice_submit_set(&session_id, answers)
        .await
        .map_err(err)
}

#[tauri::command]
async fn practice_end(h: H<'_>, session_id: String, skip_retests: Option<bool>) -> Result<()> {
    h.practice_end(&session_id, skip_retests.unwrap_or(false))
        .await
        .map_err(err)
}

#[tauri::command]
async fn practice_resume(h: H<'_>, session_id: String) -> Result<()> {
    h.practice_resume(&session_id).await.map_err(err)
}

// ---- Exam mode (docs/ui-design.md §11c) -------------------------------------------

/// The exam goal's ranked plan, the day's sitting, the mix and the score estimate.
#[tauri::command]
async fn exam_plan(h: H<'_>, session_id: String) -> Result<harness::exam::ExamPlan> {
    blocking(&h, move |h| h.exam_plan(&session_id)).await
}

/// New timed exam paper from the exam goal's session; returns when the whole
/// paper is posed (the clock starts then).
#[tauri::command]
async fn start_exam(
    h: H<'_>,
    goal_session_id: String,
    budget_min: u32,
    size: Option<u32>,
) -> Result<SessionSummary> {
    h.start_exam(&goal_session_id, budget_min, size)
        .await
        .map_err(err)
}

/// Save the answers typed so far; the deadline hands in the latest save.
#[tauri::command]
async fn practice_exam_save(
    h: H<'_>,
    session_id: String,
    answers: Vec<harness::practice::SetAnswer>,
) -> Result<()> {
    blocking(&h, move |h| h.practice_exam_save(&session_id, answers)).await
}

#[tauri::command]
fn cancel_reply(h: H<'_>, session_id: String) {
    h.cancel(&session_id);
}

// ---- composer, workspace, plugins ----------------------------------------------

/// Native file picker ("+" button). Returns paths only; the caller stages them.
#[tauri::command]
async fn pick_files(app: tauri::AppHandle) -> Result<Vec<String>> {
    tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .blocking_pick_files()
            .map(|files| {
                files
                    .into_iter()
                    .filter_map(|f| f.into_path().ok())
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect()
            })
            .ok_or_else(|| "no files chosen".to_owned())
    })
    .await
    .map_err(err)?
}

/// Native folder picker. tauri-plugin-dialog has folder picking on desktop
/// only; on mobile the command refuses, and the UI shows the message.
#[cfg(desktop)]
#[tauri::command]
async fn pick_folder(app: tauri::AppHandle) -> Result<String> {
    tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .blocking_pick_folder()
            .and_then(|f| f.into_path().ok())
            .map(|p| p.to_string_lossy().into_owned())
            .ok_or_else(|| "no folder chosen".to_owned())
    })
    .await
    .map_err(err)?
}

#[cfg(mobile)]
#[tauri::command]
async fn pick_folder() -> Result<String> {
    Err("Folders can't be picked on this device.".to_owned())
}

#[tauri::command]
async fn stage_image(h: H<'_>, data: String, name: Option<String>) -> Result<harness::Attachment> {
    blocking(&h, move |h| h.stage_image(&data, name.as_deref())).await
}

/// Drops a pending image the learner removed before sending.
#[tauri::command]
async fn unstage_image(h: H<'_>, path: String) -> Result<()> {
    blocking(&h, move |h| h.unstage_image(&path)).await
}

#[tauri::command]
async fn stage_file(
    h: H<'_>,
    session_id: Option<String>,
    path: String,
) -> Result<harness::Attachment> {
    blocking(&h, move |h| h.stage_file(session_id.as_deref(), &path)).await
}

/// A data URL for a stored image attachment, for message thumbnails.
#[tauri::command]
async fn attachment_data(h: H<'_>, path: String) -> Result<String> {
    blocking(&h, move |h| h.attachment_data(&path)).await
}

#[tauri::command]
async fn mentions(
    h: H<'_>,
    session_id: Option<String>,
    query: String,
) -> Result<Vec<harness::Mention>> {
    blocking(&h, move |h| h.mentions(session_id.as_deref(), &query)).await
}

#[tauri::command]
fn vision_capabilities(h: H<'_>) -> harness::VisionCapabilities {
    h.vision_capabilities()
}

#[tauri::command]
async fn attach_workspace(h: H<'_>, session_id: String, path: String) -> Result<String> {
    blocking(&h, move |h| h.attach_workspace(&session_id, &path)).await
}

#[tauri::command]
async fn detach_workspace(h: H<'_>, session_id: String) -> Result<()> {
    blocking(&h, move |h| h.detach_workspace(&session_id)).await
}

#[tauri::command]
fn workspace_path(h: H<'_>, session_id: String) -> Option<String> {
    h.workspace_path(&session_id)
        .map(|p| p.to_string_lossy().into_owned())
}

#[tauri::command]
async fn plugin_view(h: H<'_>, plugin_id: String) -> Result<harness::PluginView> {
    blocking(&h, move |h| h.plugin_view(&plugin_id)).await
}

/// Where to point the view iframe: the `pluginview` scheme below, which
/// serves the document with `VIEW_CSP`.
#[tauri::command]
async fn plugin_view_url(h: H<'_>, plugin_id: String) -> Result<String> {
    blocking(&h, move |h| {
        h.plugin_view(&plugin_id)?;
        Ok(view_url(&plugin_id))
    })
    .await
}

#[tauri::command]
async fn plugin_view_call(
    h: H<'_>,
    plugin_id: String,
    function: String,
    args: serde_json::Value,
) -> Result<serde_json::Value> {
    blocking(&h, move |h| h.plugin_view_call(&plugin_id, &function, args)).await
}

#[tauri::command]
async fn view_state(
    h: H<'_>,
    session_id: String,
    artifact_id: String,
    summary: String,
) -> Result<()> {
    blocking(&h, move |h| {
        h.view_state(&session_id, &artifact_id, &summary)
    })
    .await
}

/// Lists plugins; folder plugins are re-read from disk when they changed.
#[tauri::command]
async fn plugins(h: H<'_>) -> Result<Vec<harness::plugins::PluginInfo>> {
    blocking(&h, |h| Ok(h.plugins())).await
}

#[tauri::command]
async fn set_plugin_enabled(h: H<'_>, id: String, enabled: bool) -> Result<()> {
    blocking(&h, move |h| h.set_plugin_enabled(&id, enabled)).await
}

#[tauri::command]
async fn install_plugin(
    h: H<'_>,
    registry: String,
    id: String,
    version: String,
) -> Result<harness::plugins::PluginInfo> {
    h.install_plugin(&registry, &id, &version)
        .await
        .map_err(err)
}

#[tauri::command]
async fn add_folder_plugin(h: H<'_>, path: String) -> Result<harness::plugins::PluginInfo> {
    blocking(&h, move |h| h.add_folder_plugin(&path)).await
}

#[tauri::command]
async fn remove_plugin(h: H<'_>, id: String) -> Result<()> {
    blocking(&h, move |h| h.remove_plugin(&id)).await
}

#[tauri::command]
async fn missing_locked(h: H<'_>) -> Result<Vec<harness::plugins::install::LockEntry>> {
    blocking(&h, |h| h.missing_locked()).await
}

#[tauri::command]
fn registries(h: H<'_>) -> Vec<harness::plugins::install::Registry> {
    h.registries()
}

#[tauri::command]
async fn add_registry(h: H<'_>, name: String, url: String) -> Result<()> {
    blocking(&h, move |h| h.add_registry(&name, &url)).await
}

#[tauri::command]
async fn remove_registry(h: H<'_>, url: String) -> Result<()> {
    blocking(&h, move |h| h.remove_registry(&url)).await
}

#[tauri::command]
async fn refresh_registries(h: H<'_>) -> Result<Vec<harness::plugins::install::RegistryStatus>> {
    Ok(h.refresh_registries().await)
}

#[tauri::command]
async fn registry_catalog(h: H<'_>) -> Result<Vec<harness::plugins::load::RegistryCatalog>> {
    blocking(&h, |h| Ok(h.registry_catalog())).await
}

#[tauri::command]
async fn install_candidate(h: H<'_>, id: String) -> Result<Option<serde_json::Value>> {
    blocking(&h, move |h| {
        Ok(harness::plugins::load::install_candidate(h, &id)
            .and_then(|c| serde_json::to_value(c).ok()))
    })
    .await
}

// ---- reference libraries (architecture §4.27) ----------------------------------

/// Import progress, one JSON event per file, to the UI's channel.
fn progress_to(channel: Channel) -> impl FnMut(&harness::ImportProgress) {
    move |p| {
        if let Ok(json) = serde_json::to_string(p) {
            let _ = channel.send(InvokeResponseBody::Json(json));
        }
    }
}

#[tauri::command]
async fn libraries(h: H<'_>) -> Result<Vec<harness::LibrarySummary>> {
    blocking(&h, |h| h.libraries()).await
}

/// Import a folder (picked with `pick_folder`) as a library; a folder this
/// device imported before is refreshed instead. Desktop only.
#[tauri::command]
async fn library_import(
    h: H<'_>,
    path: String,
    options: Option<harness::ImportOptions>,
    on_progress: Channel,
) -> Result<harness::ImportReport> {
    blocking(&h, move |h| {
        let mut send = progress_to(on_progress);
        h.library_import(&path, options.unwrap_or_default(), &mut send)
    })
    .await
}

#[tauri::command]
async fn library_refresh(
    h: H<'_>,
    slug: String,
    max_file_mb: Option<u64>,
    on_progress: Channel,
) -> Result<harness::ImportReport> {
    blocking(&h, move |h| {
        let mut send = progress_to(on_progress);
        h.library_refresh(&slug, max_file_mb, &mut send)
    })
    .await
}

#[tauri::command]
async fn library_remove(h: H<'_>, slug: String) -> Result<()> {
    blocking(&h, move |h| h.library_remove(&slug)).await
}

#[tauri::command]
async fn library_update(
    h: H<'_>,
    slug: String,
    name: Option<String>,
    description: Option<String>,
    topic: Option<String>,
) -> Result<harness::LibrarySummary> {
    blocking(&h, move |h| {
        h.library_update(
            &slug,
            name.as_deref(),
            description.as_deref(),
            topic.as_deref(),
        )
    })
    .await
}

// ---- image sets (docs/plugins.md §12, `image-set`) -------------------------------

#[tauri::command]
async fn image_sets(h: H<'_>) -> Result<Vec<harness::plugins::native::image_set::SetInfo>> {
    blocking(&h, |h| Ok(h.image_sets())).await
}

/// Copy a labelled folder (picked with `pick_folder`; one subfolder of
/// images per label) into the data repo as an image set. Desktop only.
#[tauri::command]
async fn image_set_import(
    h: H<'_>,
    path: String,
    name: Option<String>,
) -> Result<harness::plugins::native::image_set::SetImportReport> {
    blocking(&h, move |h| h.image_set_import(&path, name.as_deref())).await
}

#[tauri::command]
async fn image_set_remove(h: H<'_>, id: String) -> Result<()> {
    blocking(&h, move |h| h.image_set_remove(&id)).await
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

/// Where keys and tokens are kept (OS secure store or the 0600 fallback),
/// with any notice Settings should show.
#[tauri::command]
fn secret_storage(h: H<'_>) -> harness::secrets::StorageStatus {
    h.secret_storage()
}

#[tauri::command]
async fn list_models(h: H<'_>, provider: ProviderId, refresh: bool) -> Result<Vec<ModelInfo>> {
    h.models(provider, refresh).await.map_err(err)
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

/// Remembers the learner's default starting point (measure | quick | new).
/// The UI sends `{ value }`, like the other settings commands.
#[tauri::command]
async fn set_starting_point(h: H<'_>, value: String) -> Result<()> {
    blocking(&h, move |h| h.set_starting_point(&value)).await
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

// ---- sync (docs/sync-build.md) ----------------------------------------------

/// Configured remote, branch, ahead/behind and the last failure. No network.
#[tauri::command]
async fn sync_status(h: H<'_>) -> Result<harness::SyncStatus> {
    blocking(&h, |h| h.sync_status()).await
}

/// Saves the device-local remote and token; an empty remote disconnects sync.
#[tauri::command]
async fn set_sync(
    h: H<'_>,
    remote: Option<String>,
    token: Option<String>,
    branch: Option<String>,
) -> Result<harness::SyncStatus> {
    blocking(&h, move |h| {
        h.set_sync(remote.as_deref(), token.as_deref(), branch.as_deref())
    })
    .await
}

/// Saves the device-local WebDAV folder URL, username and app password; a
/// blank URL disconnects WebDAV, an absent password keeps the saved one.
#[tauri::command]
async fn set_webdav(
    h: H<'_>,
    url: Option<String>,
    username: Option<String>,
    password: Option<String>,
) -> Result<harness::SyncStatus> {
    blocking(&h, move |h| {
        h.set_webdav(url.as_deref(), username.as_deref(), password.as_deref())
    })
    .await
}

/// Runs git, then WebDAV (whichever are configured) and reports what they did.
#[tauri::command]
async fn sync_now(h: H<'_>) -> Result<harness::SyncReport> {
    blocking(&h, |h| h.sync_now()).await
}

/// Clears the "conflicts need attention" banner (the copies stay in history).
#[tauri::command]
async fn acknowledge_conflicts(h: H<'_>) -> Result<harness::SyncStatus> {
    blocking(&h, |h| h.acknowledge_conflicts()).await
}

/// The app went to the background (the UI's `visibilitychange` → hidden; on
/// mobile there is no close): sync if a configured target has local
/// progress. Best effort; returns whether a run started.
#[tauri::command]
async fn sync_if_changed(h: H<'_>) -> Result<bool> {
    blocking(&h, |h| Ok(h.sync_if_changed())).await
}

/// The app came back to the foreground (or the UI's catch-up timer fired):
/// a full background run, so another device's work arrives without a
/// restart. Best effort; returns whether a run started.
#[tauri::command]
async fn sync_in_background(h: H<'_>) -> Result<bool> {
    blocking(&h, |h| Ok(h.sync_in_background())).await
}

/// Set once the exit has been held for a last sync, so the exit that follows
/// passes straight through.
#[cfg(desktop)]
static EXIT_HELD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The learner closed the app: hold the exit, hide the windows so the app
/// looks closed, give a last sync at most `EXIT_DEADLINE` (it returns at once
/// when nothing is configured or changed), then exit for real.
#[cfg(desktop)]
fn sync_then_exit(app: &tauri::AppHandle, api: &tauri::ExitRequestApi) {
    let Some(h) = app.try_state::<Arc<Harness>>() else {
        return;
    };
    if EXIT_HELD.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let h = Arc::clone(&h);
    api.prevent_exit();
    for window in app.webview_windows().values() {
        let _ = window.hide();
    }
    let handle = app.clone();
    let spawned = std::thread::Builder::new()
        .name("lh-exit-sync".into())
        .spawn(move || {
            h.sync_before_exit(harness::sync::EXIT_DEADLINE);
            handle.exit(0);
        });
    if spawned.is_err() {
        app.exit(0);
    }
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

/// Starts Claude subscription sign-in: keeps the PKCE verifier and opens the
/// approval page in the system browser.
#[tauri::command]
fn claude_login_start(
    app: tauri::AppHandle,
    h: H<'_>,
) -> Result<harness::providers::claude_oauth::LoginStart> {
    let start = h.claude_login_start();
    let _ = app.opener().open_url(&start.auth_url, None::<&str>);
    Ok(start)
}

/// Exchanges the code the browser showed for a subscription grant.
#[tauri::command]
async fn claude_login_submit(h: H<'_>, code: String) -> Result<Option<String>> {
    h.claude_login_submit(&code).await.map_err(err)
}

#[tauri::command]
fn claude_login_cancel(h: H<'_>) {
    h.claude_login_cancel();
}

/// Starts ChatGPT sign-in: binds localhost:1455 for the redirect and opens
/// the approval page in the system browser.
#[tauri::command]
fn codex_login_start(
    app: tauri::AppHandle,
    h: H<'_>,
) -> Result<harness::providers::codex_oauth::LoginStart> {
    let start = h.codex_login_start().map_err(err)?;
    let _ = app.opener().open_url(&start.auth_url, None::<&str>);
    Ok(start)
}

/// Waits (up to 5 minutes) for the browser redirect and saves the grant.
#[tauri::command]
async fn codex_login_wait(h: H<'_>) -> Result<Option<String>> {
    h.codex_login_wait().await.map_err(err)
}

#[tauri::command]
fn codex_login_cancel(h: H<'_>) {
    h.codex_login_cancel();
}

/// Opens provider key pages and similar links in the system browser.
#[tauri::command]
fn open_external(app: tauri::AppHandle, url: String) -> Result<()> {
    if !url.starts_with("https://") {
        return Err("only https links can be opened".into());
    }
    app.opener().open_url(url, None::<&str>).map_err(err)
}

// ---- focus mode (ui-design §7.8) ----------------------------------------------

/// The notification backend the app can pause (e.g. "mako"); `None` hides
/// the Settings option.
#[tauri::command]
async fn focus_dnd_support(focus: State<'_, Arc<focus::Focus>>) -> Result<Option<&'static str>> {
    let focus = Arc::clone(&focus);
    tauri::async_runtime::spawn_blocking(move || focus.support())
        .await
        .map_err(err)
}

/// Silence notifications (only if the learner had not) or undo that.
#[tauri::command]
async fn focus_dnd(focus: State<'_, Arc<focus::Focus>>, on: bool) -> Result<()> {
    let focus = Arc::clone(&focus);
    tauri::async_runtime::spawn_blocking(move || if on { focus.enable() } else { focus.restore() })
        .await
        .map_err(err)?
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

/// Builds and runs the app: called by the desktop binary (`main.rs`) and, via
/// `mobile_entry_point`, by the Android/iOS host.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    harness::logging::init(harness::logging::Level::INFO);
    tauri::Builder::default()
        // Plugin views get their own document and CSP; see VIEW_CSP. Building
        // the document reads the package from disk, so it runs off the main
        // thread like the IO commands.
        .register_asynchronous_uri_scheme_protocol("pluginview", |ctx, request, responder| {
            let h = Arc::clone(&ctx.app_handle().state::<Arc<Harness>>());
            let uri = request.uri();
            let path = uri.path().trim_matches('/').to_owned();
            let id = if path.is_empty() {
                uri.host().unwrap_or_default().to_owned()
            } else {
                path
            };
            tauri::async_runtime::spawn_blocking(move || {
                let response = match h.view_document(&id) {
                    Ok(body) => tauri::http::Response::builder()
                        .header("Content-Type", "text/html; charset=utf-8")
                        .header("Content-Security-Policy", VIEW_CSP)
                        .header("Cache-Control", "no-store")
                        .body(body)
                        .expect("valid view response"),
                    Err(e) => tauri::http::Response::builder()
                        .status(404)
                        .header("Content-Type", "text/plain; charset=utf-8")
                        .header("Content-Security-Policy", "default-src 'none'")
                        .body(e.to_string().into_bytes())
                        .expect("valid error response"),
                };
                responder.respond(response);
            });
        })
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let (repo, config) = data_dirs(app)?;
            let harness = Arc::new(Harness::open(&repo, &config)?);
            let events = Events::default();
            wire_events(&harness, &events);
            // Undo do-not-disturb left behind by a crash.
            let focus = Arc::new(focus::Focus::new(&config));
            let _ = focus.restore();
            // Indexes the plugin browser and `plugin_search` read from cache:
            // fetch the default registry (and any other) at most once a day.
            let registries = Arc::clone(&harness);
            tauri::async_runtime::spawn(async move {
                registries
                    .refresh_stale_registries(Duration::from_secs(24 * 60 * 60))
                    .await;
            });
            // Jobs a previous run left open restart now, not when their
            // session is next opened (desktop and mobile alike).
            let recovery = Arc::clone(&harness);
            tauri::async_runtime::spawn_blocking(move || recovery.recover_interrupted_jobs());
            app.manage(harness);
            app.manage(events);
            app.manage(focus);
            // WebKitGTK would magnify the whole app on a touchpad pinch; the
            // page (the Map) gets the pinch instead.
            #[cfg(target_os = "linux")]
            pinch::route_to_page(app);
            // WebKitGTK gives the page a stylus as a mouse; the page learns
            // which events are the pen, its eraser end and its pressure.
            #[cfg(target_os = "linux")]
            pen::route_to_page(app);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_sessions,
            load_session,
            mobile_unlock,
            delete_session,
            session_state,
            knowledge_map,
            learner_nodes,
            review_queue,
            edge_proposals,
            decide_edge_proposals,
            graph_build,
            graph_status,
            graph_splits,
            decide_splits,
            place_node,
            node_provenance,
            subscribe_events,
            start_teaching,
            send_learner_message,
            comment,
            answer,
            approve,
            resolve_install_offer,
            request_plan_change,
            edit_plan,
            answer_plan_question,
            hunt_return,
            hunt_skip,
            set_hunt,
            frontier,
            start_from_frontier,
            resume_turn,
            cancel_job,
            start_practice,
            practice_state,
            practice_submit,
            practice_hint,
            practice_dismiss,
            practice_next,
            practice_submit_set,
            practice_end,
            practice_resume,
            exam_plan,
            start_exam,
            practice_exam_save,
            cancel_reply,
            get_settings,
            list_providers,
            secret_storage,
            list_models,
            save_api_key,
            remove_credential,
            set_base_url,
            set_role_model,
            set_starting_point,
            test_role,
            list_roles,
            role_usage,
            sync_status,
            set_sync,
            set_webdav,
            sync_now,
            acknowledge_conflicts,
            sync_if_changed,
            sync_in_background,
            workbuddy_login_start,
            workbuddy_login_wait,
            workbuddy_login_cancel,
            claude_login_start,
            claude_login_submit,
            claude_login_cancel,
            codex_login_start,
            codex_login_wait,
            codex_login_cancel,
            open_external,
            pick_files,
            pick_folder,
            stage_image,
            unstage_image,
            stage_file,
            attachment_data,
            mentions,
            vision_capabilities,
            attach_workspace,
            detach_workspace,
            workspace_path,
            plugin_view,
            plugin_view_url,
            plugin_view_call,
            view_state,
            plugins,
            set_plugin_enabled,
            install_plugin,
            add_folder_plugin,
            remove_plugin,
            missing_locked,
            registries,
            add_registry,
            remove_registry,
            refresh_registries,
            registry_catalog,
            install_candidate,
            libraries,
            library_import,
            library_refresh,
            library_remove,
            library_update,
            image_sets,
            image_set_import,
            image_set_remove,
            focus_dnd_support,
            focus_dnd,
        ])
        .build(tauri::generate_context!())
        .expect("failed to start P's Learning Harness")
        .run(|app, event| {
            // A learner-initiated exit (`code: None`) waits for a last sync;
            // the programmatic exit that follows carries a code.
            #[cfg(desktop)]
            if let RunEvent::ExitRequested {
                code: None, api, ..
            } = &event
            {
                sync_then_exit(app, api);
            }
            let closing = matches!(
                event,
                RunEvent::Exit
                    | RunEvent::WindowEvent {
                        event: WindowEvent::Destroyed,
                        ..
                    }
            );
            if closing {
                if let Some(focus) = app.try_state::<Arc<focus::Focus>>() {
                    let _ = focus.restore();
                }
            }
        });
}
