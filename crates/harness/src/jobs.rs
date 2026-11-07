//! Background jobs (architecture §4.16; build prompt M4 "Jobs").
//!
//! Each job is a tokio task with a persisted `job` op (queued → running →
//! done/failed/aborted). Concurrency is bounded per model role (Reader 3,
//! Planner 1). A finished job's result is delivered into the conversation as
//! a tagged user message under the session's turn lock, so it never lands in
//! the middle of a running turn:
//! - a learner answer pending → the result waits; the next tutor turn
//!   delivers it first;
//! - otherwise it is appended and, in a tutor phase, the idle tutor is woken.
//!
//! Researcher runs never touch the conversation: their tool loop stays in
//! memory and only the brief comes back. The planner's tool calls are
//! persisted (tagged `agent: "planner"`) for the record and the UI, but are
//! not part of the tutor's messages.
//!
//! The graph builder (`docs/graph-build.md`) is stranger still: it reads a
//! session's transcript and appends ops to `<repo>/graph/ops.jsonl`. Its result
//! is never delivered and never wakes the tutor — the graph is not
//! conversation material — so the job op is the only trace, and its tool calls
//! are tagged `agent: "librarian"` to stay out of the replay.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use providers::{ContentBlock, Message, Role, ToolCall};
use serde_json::{json, Value};
use tokio::sync::oneshot;

use crate::agent::{done, ensure_user_last, phase_op, refuse, result_op, tool_op, Exec, MAX_LOOP};
use crate::events::{AgentEvent, AgentName};
use crate::files;
use crate::prompts::{Composer, TemplateValues};
use crate::replay::{self, replay_state};
use crate::sessions::{self, op_id, JobOp, Op, PlanAskOp, PlanKeepOp, TitleOp};
use crate::state::{Phase, QueuedResult, SessionState};
use crate::tools::{self, opt_str, req_str};
use crate::{lock, now_iso, Error, Harness, ModelRole};

type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Most model calls in one researcher run.
const MAX_RESEARCH_STEPS: usize = 16;

/// How many times a researcher run that answers in prose instead of calling
/// `brief_submit` is nudged back to the tool. Two, not more: the first nudge
/// is the plain instruction, the second repeats the exact call shape, and a
/// model that ignores both will not be talked into it — every extra nudge
/// only spends a step that the salvage below does not need. A run that keeps
/// writing prose is ended from its last answer instead of looping to
/// [`MAX_RESEARCH_STEPS`] and failing with nothing.
const MAX_PROSE_NUDGES: usize = 2;

/// Shortest assistant text a run's prose is salvaged into a brief from. Below
/// this the reply is a preamble, an apology or nothing: there is no answer to
/// keep, so the run fails and says so.
const MIN_SALVAGE_CHARS: usize = 200;

/// What a salvaged brief admits about itself, in `uncertain`.
const SALVAGE_NOTE: &str =
    "answered in prose without the structured brief; fields the tutor may need are missing";

/// Most characters of a session's transcript the graph builder reads
/// (`docs/graph-build.md`): a long session is clipped at the end, because the
/// goal, the plan and the first teaching are what most of the extraction rests
/// on.
const MAX_GRAPH_TRANSCRIPT: usize = 24_000;

/// Cap on the session plan shown to the builder as a hint.
const MAX_GRAPH_PLAN: usize = 4_000;

const TITLE_SYSTEM: &str = "Session title\n\nWrite a title of at most six words for a learning session with the goal below. Reply with the title only: no quotes, no punctuation at the end.";

/// What a job body produced.
enum JobEnd {
    /// Done; `Some` is delivered into the conversation.
    Done(Option<Value>),
    Failed(String),
    Aborted,
}

/// A running job's handle.
pub(crate) struct JobHandle {
    pub session_id: String,
    pub cancel: oneshot::Sender<()>,
}

impl Harness {
    fn job_op(&self, job_id: &str, kind: &str, state: &str) -> JobOp {
        JobOp {
            op_id: op_id(),
            at: now_iso(),
            job_id: job_id.to_owned(),
            kind: kind.to_owned(),
            state: state.to_owned(),
            args: None,
            question: None,
            blocking: false,
            result: None,
            error: None,
        }
    }

    /// Run `fut` as a background task tracked per session (so tests and
    /// shutdown can wait for it).
    pub(crate) fn spawn_tracked(&self, session_id: &str, fut: BoxFuture) {
        let handle = tokio::spawn(fut);
        lock(&self.background)
            .entry(session_id.to_owned())
            .or_default()
            .push(handle);
    }

    /// Wait until no background task (job, wake, title) of this session is
    /// running.
    pub async fn wait_background(&self, session_id: &str) {
        loop {
            let handles = lock(&self.background)
                .remove(session_id)
                .unwrap_or_default();
            if handles.is_empty() {
                return;
            }
            for h in handles {
                let _ = h.await;
            }
        }
    }

    /// Persist a queued job and start it. `kind` is `research`, `plan` or
    /// `graph`.
    pub(crate) fn spawn_job(
        self: &Arc<Self>,
        session_id: &str,
        kind: &str,
        args: Value,
        question: &str,
    ) -> Result<String, Error> {
        let job_id = op_id();
        let mut op = self.job_op(&job_id, kind, "queued");
        op.args = Some(args.clone());
        op.question = Some(question.to_owned());
        self.append(session_id, Op::Job(op))?;
        let me = Arc::clone(self);
        let sid = session_id.to_owned();
        let kind = kind.to_owned();
        let question = question.to_owned();
        let id = job_id.clone();
        let fut: BoxFuture = Box::pin(async move {
            me.run_job(sid, id, kind, args, question).await;
        });
        self.spawn_tracked(session_id, fut);
        Ok(job_id)
    }

    async fn run_job(
        self: Arc<Self>,
        sid: String,
        job_id: String,
        kind: String,
        args: Value,
        question: String,
    ) {
        let (cancel_tx, mut cancel_rx) = oneshot::channel();
        lock(&self.jobs).insert(
            job_id.clone(),
            JobHandle {
                session_id: sid.clone(),
                cancel: cancel_tx,
            },
        );
        let started = std::time::Instant::now();
        let semaphore = match kind.as_str() {
            "plan" => Arc::clone(&self.planner_slots),
            "graph" => Arc::clone(&self.librarian_slots),
            _ => Arc::clone(&self.reader_slots),
        };
        let end = tokio::select! {
            permit = semaphore.acquire_owned() => match permit {
                Err(_) => JobEnd::Failed("job slots closed".into()),
                Ok(permit) => {
                    let running = self.job_op(&job_id, &kind, "running");
                    let _ = self.append(&sid, Op::Job(running));
                    let end = tokio::select! {
                        end = self.job_body(&sid, &kind, &args) => end,
                        _ = &mut cancel_rx => JobEnd::Aborted,
                    };
                    drop(permit);
                    end
                }
            },
            _ = &mut cancel_rx => JobEnd::Aborted,
        };
        lock(&self.jobs).remove(&job_id);
        let mut op = self.job_op(&job_id, &kind, "done");
        op.question = Some(question);
        match end {
            JobEnd::Done(result) => op.result = result,
            JobEnd::Failed(e) => {
                op.state = "failed".into();
                op.error = Some(self.redact(&e));
            }
            JobEnd::Aborted => op.state = "aborted".into(),
        }
        tracing::debug!(job = %job_id, kind = %kind, state = %op.state, ms = started.elapsed().as_millis() as u64, "job");
        if let Err(e) = self.append(&sid, Op::Job(op)) {
            tracing::warn!("job {job_id}: could not persist its end: {e}");
            return;
        }
        self.flush_deliveries(&sid).await;
    }

    async fn job_body(self: &Arc<Self>, sid: &str, kind: &str, args: &Value) -> JobEnd {
        match kind {
            "research" => match self.research_body(sid, args).await {
                Ok(brief) => JobEnd::Done(Some(brief)),
                Err(e) => JobEnd::Failed(e.to_string()),
            },
            "plan" => {
                let request =
                    opt_str(args, "request").unwrap_or(crate::agent::INITIAL_PLAN_REQUEST);
                self.emit(
                    sid,
                    AgentEvent::TurnStart {
                        agent: AgentName::Planner,
                    },
                );
                let r = self.planner_run(sid, request).await;
                self.emit(
                    sid,
                    AgentEvent::TurnEnd {
                        agent: AgentName::Planner,
                        error: r.as_ref().err().map(|e| self.redact(&e.to_string())),
                    },
                );
                match r {
                    Ok(notice) => JobEnd::Done(notice.map(|n| json!({ "notice": n }))),
                    Err(e) => JobEnd::Failed(e.to_string()),
                }
            }
            // The graph builder never touches the conversation (see
            // `spawn_graph_job`); its result is the counts it appended.
            "graph" => match self.graph_body(sid).await {
                Ok(summary) => JobEnd::Done(Some(summary)),
                Err(e) => JobEnd::Failed(e.to_string()),
            },
            other => JobEnd::Failed(format!("unknown job kind `{other}`")),
        }
    }

    /// Cancel a running job.
    pub fn cancel_job(&self, session_id: &str, job_id: &str) -> Result<(), Error> {
        let handle = {
            let mut jobs = lock(&self.jobs);
            match jobs.get(job_id) {
                Some(h) if h.session_id == session_id => jobs.remove(job_id),
                _ => None,
            }
        };
        match handle {
            Some(h) => {
                let _ = h.cancel.send(());
                Ok(())
            }
            None => Err(Error::NotFound(format!("running job {job_id}"))),
        }
    }

    // ---- delivery ----------------------------------------------------------

    /// Append undelivered job results as tagged user messages (and, for a
    /// scope brief, the `brief` op the `probe_done` gate checks). The caller
    /// holds the turn lock and has checked that no answer is pending.
    pub(crate) fn deliver_queued(&self, sid: &str) -> Result<(), Error> {
        let state = replay_state(&self.store.ops(sid)?);
        if state.pending_call.is_some() {
            return Ok(());
        }
        for q in &state.queued_jobs {
            self.deliver(sid, q)?;
        }
        Ok(())
    }

    fn deliver(&self, sid: &str, q: &QueuedResult) -> Result<(), Error> {
        let (tag, content) = match q.kind.as_str() {
            "plan" => (
                "plan-notice",
                match q.result.get("error").and_then(Value::as_str) {
                    Some(e) => format!("[plan update] The planner couldn't finish: {e}"),
                    None => format!(
                        "[plan update] {}",
                        q.result.get("notice").and_then(Value::as_str).unwrap_or("")
                    ),
                },
            ),
            _ => {
                if let Some(e) = q.result.get("error").and_then(Value::as_str) {
                    (
                        "research-brief",
                        format!("[research brief] {} — the research failed: {e}", q.question),
                    )
                } else {
                    if q.result.get("kind").and_then(Value::as_str) == Some("scope") {
                        self.append(sid, Op::Brief(sessions::record(q.result.clone())))?;
                    }
                    let body = serde_json::to_string_pretty(&q.result).unwrap_or_default();
                    (
                        "research-brief",
                        format!("[research brief] {}\n\n```json\n{body}\n```", q.question),
                    )
                }
            }
        };
        let mut m = sessions::new_message(Role::User, content);
        m.tag = Some(tag.into());
        m.job_id = Some(q.job_id.clone());
        self.append(sid, Op::Message(m))
    }

    /// The delivery rule, run when a job ends: wait for any running turn,
    /// then deliver and wake the tutor unless an answer is pending.
    async fn flush_deliveries(self: &Arc<Self>, sid: &str) {
        let guard = self.turn_lock(sid);
        let _turn = guard.lock().await;
        let Ok(ops) = self.store.ops(sid) else { return };
        let state = replay_state(&ops);
        if state.queued_jobs.is_empty() || state.pending_call.is_some() {
            return;
        }
        if let Err(e) = self.deliver_queued(sid) {
            tracing::warn!("delivery failed: {e}");
            return;
        }
        if matches!(state.phase, Phase::Measure | Phase::Teach | Phase::Close) && !state.ended {
            // Errors are reported to the UI by the turn's TurnEnd event.
            let _ = self.tutor_turn(sid).await;
        }
    }

    // ---- researcher ---------------------------------------------------------

    /// Run the researcher (role Reader, `jobs/researcher.md`) until it calls
    /// `brief_submit`; returns the brief.
    async fn research_body(self: &Arc<Self>, sid: &str, args: &Value) -> Result<Value, Error> {
        let state = replay_state(&self.store.ops(sid)?);
        let kind = opt_str(args, "kind").unwrap_or("verify").to_owned();
        let question = opt_str(args, "question").unwrap_or("").to_owned();
        let mut task = format!("Task `{kind}`.\n\nQuestion: {question}\n");
        if let Some(c) = opt_str(args, "context") {
            task.push_str(&format!("\nContext: {c}\n"));
        }
        if let Some(p) = opt_str(args, "path") {
            task.push_str(&format!("\nDocument: {p}\n"));
        }
        if let Some(goal) = &state.goal {
            task.push_str(&format!("\nGoal record: {goal}\n"));
        }
        let composer = Composer::new(&self.repo);
        let mut values = TemplateValues::new();
        values.set("task", task);
        let catalog = self.plugin_registry().available();
        values.set(
            "plugin_catalog",
            composer.plugin_catalog(&state.platform, &catalog),
        );
        // The researcher is the only writer of `brief.venues`, so it is
        // always told to record the field guide (§6).
        let mut flags = HashSet::new();
        flags.insert("field_guide".to_owned());
        let system = composer.compose_job("jobs/researcher.md", &values, &flags)?;
        let tools = tools::researcher_tools();
        let cache_key = op_id();
        // Page images the model has been shown this run (a scan it asked to
        // look at), capped so a long document can't flood the context.
        let mut page_images = 0usize;
        let mut messages = vec![Message::text(Role::User, "Begin the task.")];
        // The latest assistant text, which a run that never calls
        // `brief_submit` is salvaged from (see `salvage_brief`).
        let mut last_text = String::new();
        let mut nudges = 0usize;
        for _ in 0..MAX_RESEARCH_STEPS {
            let reply = self
                .call_role(
                    ModelRole::Reader,
                    sid,
                    &cache_key,
                    &system,
                    &messages,
                    &tools,
                    None,
                    None,
                )
                .await?;
            let calls = reply.completion.tool_calls.clone();
            if !reply.text.trim().is_empty() {
                last_text = reply.text.clone();
            }
            messages.push(assistant_message(
                &reply.text,
                &reply.completion.native,
                &calls,
            ));
            if calls.is_empty() {
                nudges += 1;
                if nudges > MAX_PROSE_NUDGES {
                    // Nudged twice, still prose: stop spending steps on a
                    // model that won't call the tool and keep its answer.
                    break;
                }
                messages.push(Message::text(Role::User, brief_nudge(&kind, nudges)));
                continue;
            }
            let mut results = Vec::new();
            let mut brief = None;
            let mut shown: Vec<(String, String, String)> = Vec::new();
            for c in &calls {
                let (content, is_error) = match self.research_tool(&kind, c, &mut page_images).await
                {
                    ResearchStep::Brief(b) => {
                        brief = Some(b);
                        ("Brief submitted.".to_owned(), false)
                    }
                    ResearchStep::Result(text, err) => (text, err),
                    ResearchStep::Image {
                        text,
                        caption,
                        media_type,
                        data,
                    } => {
                        shown.push((caption, media_type, data));
                        (text, false)
                    }
                };
                results.push(ContentBlock::ToolResult {
                    call_id: c.id.clone(),
                    content,
                    is_error,
                });
            }
            if let Some(b) = brief {
                return Ok(b);
            }
            messages.push(Message {
                role: Role::User,
                content: results,
            });
            if !shown.is_empty() {
                // One user message after the results: the captions as text,
                // then the images, so the next model call sees the page.
                let mut content = Vec::new();
                content.push(ContentBlock::text(
                    shown
                        .iter()
                        .map(|(caption, _, _)| caption.as_str())
                        .collect::<Vec<_>>()
                        .join("\n"),
                ));
                for (_, media_type, data) in shown {
                    content.push(ContentBlock::Image { media_type, data });
                }
                messages.push(Message {
                    role: Role::User,
                    content,
                });
            }
        }
        // No `brief_submit`: keep the model's answer rather than fail the job
        // on a formatting miss. [`salvage_brief`] says when even that is not
        // possible.
        salvage_brief(&kind, &question, &last_text).map_err(Error::Invalid)
    }

    async fn research_tool(
        &self,
        task_kind: &str,
        call: &ToolCall,
        page_images: &mut usize,
    ) -> ResearchStep {
        let args = &call.arguments;
        let fail = |e: String| ResearchStep::Result(format!("{} failed: {e}", call.name), true);
        if !args.is_object() {
            return fail("the arguments were not a valid JSON object".into());
        }
        match call.name.as_str() {
            "web_search" => {
                let Ok(query) = req_str(args, "query") else {
                    return fail("missing `query`".into());
                };
                let recency =
                    opt_str(args, "recency").and_then(|r| serde_json::from_value(json!(r)).ok());
                let req = research_search::SearchRequest {
                    query: query.to_owned(),
                    recency,
                    limit: Some(8),
                };
                match research_search::search(&self.client, &req).await {
                    Ok(r) => ResearchStep::Result(research_search::format_for_llm(&r), false),
                    Err(e) => fail(e.to_string()),
                }
            }
            "page_fetch" => match req_str(args, "url") {
                Ok(url) => match crate::web::page_fetch(&self.client, url).await {
                    Ok(text) => ResearchStep::Result(text, false),
                    Err(e) => fail(e),
                },
                Err(e) => fail(e),
            },
            "source_outline" | "source_search" | "source_read" => {
                let Some(root) = files::home_dir() else {
                    return fail("no home directory on this device".into());
                };
                let name = call.name.clone();
                let args = args.clone();
                let r = tokio::task::spawn_blocking(move || {
                    let path = req_str(&args, "path")?;
                    match name.as_str() {
                        "source_outline" => files::source_outline(&root, path),
                        "source_search" => {
                            files::source_search(&root, path, req_str(&args, "query")?)
                        }
                        _ => files::source_read(
                            &root,
                            path,
                            opt_str(&args, "pages"),
                            opt_str(&args, "lines"),
                        ),
                    }
                })
                .await;
                match r {
                    Ok(Ok(text)) => ResearchStep::Result(text, false),
                    Ok(Err(e)) => fail(e),
                    Err(e) => fail(e.to_string()),
                }
            }
            "source_page_image" => self.source_page_image(args, page_images).await,
            "brief_submit" => match check_brief(task_kind, args) {
                Ok(()) => ResearchStep::Brief(args.clone()),
                Err(e) => ResearchStep::Result(format!("brief_submit refused: {e}"), true),
            },
            other => fail(format!("unknown tool `{other}`")),
        }
    }

    /// `source_page_image { path | url, page }`: one page of a PDF as an
    /// image for the model to look at, which is the only way to read a scan
    /// (there is no OCR anywhere in the harness).
    ///
    /// A page that has a text layer is sent back as a `source_read` hint
    /// instead, a model that can't see images gets words saying so, and at
    /// most [`MAX_PAGE_IMAGES_PER_RUN`] images reach one run.
    async fn source_page_image(&self, args: &Value, page_images: &mut usize) -> ResearchStep {
        let fail = |e: String| ResearchStep::Result(format!("source_page_image failed: {e}"), true);
        let page = match args.get("page") {
            Some(Value::Number(n)) => n.as_u64().and_then(|p| u32::try_from(p).ok()),
            Some(Value::String(s)) => s.trim().parse().ok(),
            _ => None,
        };
        let Some(page) = page.filter(|p| *p > 0) else {
            return fail("`page` must be a page number of 1 or more".into());
        };
        let (path, url) = (opt_str(args, "path"), opt_str(args, "url"));
        let bytes = match (path, url) {
            (Some(path), None) => {
                let Some(root) = files::home_dir() else {
                    return fail("no home directory on this device".into());
                };
                let path = path.to_owned();
                match tokio::task::spawn_blocking(move || files::read_pdf_file(&root, &path)).await
                {
                    Ok(Ok(bytes)) => bytes,
                    Ok(Err(e)) => return fail(e),
                    Err(e) => return fail(e.to_string()),
                }
            }
            (None, Some(url)) => match crate::web::fetch_pdf(&self.client, url).await {
                Ok(bytes) => bytes,
                Err(e) => return fail(e),
            },
            _ => return fail("give exactly one of `path` and `url`".into()),
        };
        let bytes = std::sync::Arc::new(bytes);
        // The page's text layer decides what to do, and rendering the image is
        // CPU work, so both run off the async runtime.
        let scanned = {
            let bytes = std::sync::Arc::clone(&bytes);
            let read = tokio::task::spawn_blocking(move || {
                crate::pdf::pages_in(&bytes, page, page).map(|(pages, _)| pages[0].scanned)
            })
            .await;
            match read {
                Ok(Ok(scanned)) => scanned,
                Ok(Err(e)) => return fail(e),
                Err(e) => return fail(e.to_string()),
            }
        };
        match page_decision(scanned, self.role_vision(ModelRole::Reader)) {
            PageDecision::TextLayer => ResearchStep::Result(
                format!("page {page} has a text layer; read it with source_read pages:\"{page}\""),
                false,
            ),
            PageDecision::NoVision => ResearchStep::Result(
                format!(
                    "page {page} has no text layer and this Reader model can't see images, so the page can't be read here; say in the brief that page {page} needs a text copy"
                ),
                true,
            ),
            PageDecision::ShowImage => {
                if *page_images >= MAX_PAGE_IMAGES_PER_RUN {
                    return fail(format!(
                        "only {MAX_PAGE_IMAGES_PER_RUN} page images can be looked at in one research run and all of them are used; work from the text you have"
                    ));
                }
                let bytes = std::sync::Arc::clone(&bytes);
                let image = match tokio::task::spawn_blocking(move || {
                    crate::pdf::page_image(&bytes, page)
                })
                .await
                {
                    Ok(Ok(image)) => image,
                    Ok(Err(e)) => return fail(e),
                    Err(e) => return fail(e.to_string()),
                };
                if image.bytes.len() as u64 > crate::attachments::MAX_IMAGE_BYTES {
                    return fail(format!(
                        "page {page}'s image is {} KiB, over the {} KiB cap a model can be sent",
                        image.bytes.len() / 1024,
                        crate::attachments::MAX_IMAGE_BYTES / 1024
                    ));
                }
                use base64::Engine as _;
                let caption = match (path, url) {
                    (Some(path), _) => format!("page {page} of {path} (image attached)"),
                    (None, Some(url)) => format!("page {page} of {url} (image attached)"),
                    _ => format!("page {page} (image attached)"),
                };
                *page_images += 1;
                ResearchStep::Image {
                    text: format!("page {page} is a scan; its image is attached"),
                    caption,
                    media_type: image.media_type.to_owned(),
                    data: base64::engine::general_purpose::STANDARD.encode(&image.bytes),
                }
            }
        }
    }

    // ---- planner ------------------------------------------------------------

    /// One planner run (role Planner): the conversation plus `request`, until
    /// `plan_write` succeeds or `plan_keep`. Returns the notice to deliver.
    async fn planner_run(
        self: &Arc<Self>,
        sid: &str,
        request: &str,
    ) -> Result<Option<String>, Error> {
        let ops = self.store.ops(sid)?;
        let state = replay_state(&ops);
        let system = self.compose_planner(&state)?;
        let tools = tools::planner_tools();
        let mut messages = replay::conversation_with(
            &ops,
            Some(&self.repo),
            self.role_vision(ModelRole::Planner) != Some(false),
            true,
        );
        messages.push(Message::text(Role::User, request.to_owned()));
        let cache_key = format!("{sid}-planner");
        for _ in 0..MAX_LOOP {
            ensure_user_last(&mut messages);
            let reply = self
                .call_role(
                    ModelRole::Planner,
                    sid,
                    &cache_key,
                    &system,
                    &messages,
                    &tools,
                    None,
                    None,
                )
                .await?;
            let calls = reply.completion.tool_calls.clone();
            messages.push(assistant_message(
                &reply.text,
                &reply.completion.native,
                &calls,
            ));
            if calls.is_empty() {
                messages.push(Message::text(
                    Role::User,
                    "End with plan_write (a new or revised plan) or plan_keep.",
                ));
                continue;
            }
            let mut results = Vec::new();
            let mut finished: Option<Option<String>> = None;
            for c in &calls {
                self.append(sid, tool_op(c, Some("planner")))?;
                let (exec, end) = if finished.is_some() {
                    (
                        refuse(&c.name, "not run: the plan was already settled"),
                        None,
                    )
                } else {
                    self.planner_tool(sid, c).await?
                };
                let Exec::Done { content, is_error } = exec else {
                    continue;
                };
                self.append(sid, result_op(c, &content, is_error, Some("planner")))?;
                results.push(ContentBlock::ToolResult {
                    call_id: c.id.clone(),
                    content,
                    is_error,
                });
                if end.is_some() {
                    finished = end;
                }
            }
            if let Some(notice) = finished {
                return Ok(notice);
            }
            messages.push(Message {
                role: Role::User,
                content: results,
            });
        }
        Err(Error::Invalid(
            "the planner stopped without plan_write or plan_keep".into(),
        ))
    }

    /// Execute one planner tool. The second value is `Some(notice)` when the
    /// run is finished.
    async fn planner_tool(
        self: &Arc<Self>,
        sid: &str,
        call: &ToolCall,
    ) -> Result<(Exec, Option<Option<String>>), Error> {
        let args = &call.arguments;
        let name = call.name.as_str();
        if !args.is_object() {
            return Ok((
                refuse(name, "the arguments must be one valid JSON object"),
                None,
            ));
        }
        match name {
            "plan_write" => self.plan_write(sid, args),
            "plan_keep" => {
                let reason = match req_str(args, "reason") {
                    Ok(r) => r.to_owned(),
                    Err(e) => return Ok((refuse(name, e), None)),
                };
                self.append(
                    sid,
                    Op::PlanKeep(PlanKeepOp {
                        op_id: op_id(),
                        at: now_iso(),
                        reason: reason.clone(),
                    }),
                )?;
                Ok((done("Kept."), Some(Some(format!("Plan kept: {reason}")))))
            }
            "plan_ask" => {
                let question = match req_str(args, "question") {
                    Ok(q) if !q.trim().is_empty() => q.trim().to_owned(),
                    _ => return Ok((refuse(name, "`question` is required"), None)),
                };
                let options: Vec<String> = args
                    .get("options")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::trim)
                            .filter(|o| !o.is_empty())
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                if options.len() < 2 {
                    return Ok((refuse(name, "give at least two `options`"), None));
                }
                // The run ends here with no notice: the question is on the
                // plan card and the learner's answer starts a new run (§4).
                self.append(
                    sid,
                    Op::PlanAsk(PlanAskOp {
                        op_id: op_id(),
                        at: now_iso(),
                        call_id: call.id.clone(),
                        question,
                        options,
                    }),
                )?;
                Ok((
                    done("Question sent to the learner. The plan waits for their answer."),
                    Some(None),
                ))
            }
            "research" => {
                let exec = match self.research_blocking(sid, args).await {
                    Ok(brief) => done(serde_json::to_string_pretty(&brief).unwrap_or_default()),
                    Err(e) => refuse(name, self.redact(&e.to_string())),
                };
                Ok((exec, None))
            }
            "plugin_search" => match req_str(args, "query") {
                Ok(query) => Ok((done(self.plugin_search(query)), None)),
                Err(e) => Ok((refuse(name, e), None)),
            },
            "file_find" | "file_read" => {
                let state = replay_state(&self.store.ops(sid)?);
                let _ = state;
                Ok((planner_file_tool(name, args).await, None))
            }
            other => Ok((refuse(other, "unknown tool"), None)),
        }
    }

    fn plan_write(
        self: &Arc<Self>,
        sid: &str,
        args: &Value,
    ) -> Result<(Exec, Option<Option<String>>), Error> {
        let ops = self.store.ops(sid)?;
        let state = replay_state(&ops);
        let manifests = self.plugin_registry().available();
        let platform = self.plugin_registry().platform().to_owned();
        let installable: std::collections::HashSet<String> = self
            .registry_catalog()
            .into_iter()
            .flat_map(|c| c.plugins)
            .filter(|p| p.platforms.iter().any(|x| x == &platform))
            .map(|p| p.id)
            .collect();
        let plan = match tools::check_plan(args, &state, &manifests, &installable) {
            Ok(p) => p,
            Err(problems) => {
                let text = format!(
                    "the plan has problems; fix all of them and call plan_write again:\n- {}",
                    problems.join("\n- ")
                );
                return Ok((refuse("plan_write", text), None));
            }
        };
        let needs_approval = tools::needs_approval(&plan, &state);
        let summary = tools::revision_summary(&plan, state.plan.as_ref());
        let revision_of_approved = state.approved_version.is_some();
        self.append(
            sid,
            Op::Plan(sessions::record(tools::plan_record(args, &plan))),
        )?;
        if needs_approval {
            self.append(sid, phase_op(Phase::AwaitApproval, None))?;
            let notice = revision_of_approved
                .then(|| format!("Plan revised: {summary}. The learner must approve it before teaching continues."));
            return Ok((done("Plan written. The learner sees the plan card and will approve it or ask for changes."), Some(notice)));
        }
        // A minor revision applies at once, with a notice; teaching resumes at
        // the first section not yet done (a section added before the current
        // one comes first).
        let after = replay_state(&self.store.ops(sid)?);
        self.grant_plan_plugins(sid, &after, &plan, &[])?;
        let next = after.next_open_section();
        let title = next
            .as_ref()
            .and_then(|id| plan.sections.iter().find(|s| &s.id == id))
            .map_or_else(|| "(none)".to_owned(), |s| s.title.clone());
        if next != state.current_section || state.phase != Phase::Teach {
            match &next {
                Some(id) => self.append(sid, phase_op(Phase::Teach, Some(id.clone())))?,
                None => self.append(sid, phase_op(Phase::Close, None))?,
            }
        }
        Ok((
            done("Revision applied."),
            Some(Some(format!(
                "Plan revised: {summary}. Current section: {title}."
            ))),
        ))
    }

    /// The planner's `research`: run the researcher now and return its brief.
    async fn research_blocking(self: &Arc<Self>, sid: &str, args: &Value) -> Result<Value, Error> {
        if !matches!(opt_str(args, "kind"), Some("verify" | "scope" | "source")) {
            return Err(Error::Invalid(
                "`kind` must be verify, scope or source".into(),
            ));
        }
        let question = req_str(args, "question")
            .map_err(Error::Invalid)?
            .to_owned();
        let job_id = op_id();
        let mut op = self.job_op(&job_id, "research", "queued");
        op.args = Some(args.clone());
        op.question = Some(question.clone());
        op.blocking = true;
        self.append(sid, Op::Job(op))?;
        let permit = Arc::clone(&self.reader_slots).acquire_owned().await;
        let mut running = self.job_op(&job_id, "research", "running");
        running.blocking = true;
        self.append(sid, Op::Job(running))?;
        let result = self.research_body(sid, args).await;
        drop(permit);
        let mut end = self.job_op(&job_id, "research", "done");
        end.blocking = true;
        end.question = Some(question);
        match &result {
            Ok(brief) => end.result = Some(brief.clone()),
            Err(e) => {
                end.state = "failed".into();
                end.error = Some(self.redact(&e.to_string()));
            }
        }
        self.append(sid, Op::Job(end))?;
        result
    }

    // ---- graph builder -----------------------------------------------------

    /// Persist and start a graph build for a session. `reason` is `manual`
    /// (the `graph_build` command) or `close` (after `session_end`). Refuses
    /// with a readable message while one is already open for that session.
    pub(crate) fn spawn_graph_job(
        self: &Arc<Self>,
        session_id: &str,
        reason: &str,
    ) -> Result<String, Error> {
        let state = replay_state(&self.store.ops(session_id)?);
        if state.open_jobs.iter().any(|j| j.kind == "graph") {
            return Err(Error::Invalid(
                "the graph builder is already running for this session".into(),
            ));
        }
        let args = json!({ "sessionId": session_id, "reason": reason });
        self.spawn_job(session_id, "graph", args, "graph build")
    }

    /// After `session_end`: build the graph once per session. A failure here
    /// must never affect the close turn, so it is only logged.
    pub(crate) fn spawn_graph_on_close(self: &Arc<Self>, session_id: &str) {
        let Ok(ops) = self.store.ops(session_id) else {
            return;
        };
        if ops
            .iter()
            .any(|o| matches!(o, Op::Job(j) if j.kind == "graph"))
        {
            return; // once per session
        }
        if let Err(e) = self.spawn_graph_job(session_id, "close") {
            tracing::warn!("graph build on close: {e}");
        }
    }

    /// Run the graph builder (role Librarian, `jobs/graph-builder.md`) until
    /// `graph_submit` passes. The tool loop stays in memory; only the ops it
    /// appends are kept, so nothing reaches the conversation.
    async fn graph_body(self: &Arc<Self>, sid: &str) -> Result<Value, Error> {
        let ops = self.store.ops(sid)?;
        let state = replay_state(&ops);
        let map = crate::knowledge::load(&self.repo)?;
        let composer = Composer::new(&self.repo);
        let mut values = TemplateValues::new();
        values.set("graph_summary", crate::graph::summary(&map));
        values.set(
            "transcript",
            crate::agent::clip(&replay::transcript(&ops), MAX_GRAPH_TRANSCRIPT),
        );
        values.set("plan", plan_hint(&state));
        let system = composer.compose_job("jobs/graph-builder.md", &values, &HashSet::new())?;
        let tools = tools::graph_tools();
        let cache_key = format!("{sid}-graph");
        let mut messages = vec![Message::text(Role::User, "Begin the task.")];
        for _ in 0..MAX_LOOP {
            let reply = self
                .call_role(
                    ModelRole::Librarian,
                    sid,
                    &cache_key,
                    &system,
                    &messages,
                    &tools,
                    None,
                    None,
                )
                .await?;
            let calls = reply.completion.tool_calls.clone();
            messages.push(assistant_message(
                &reply.text,
                &reply.completion.native,
                &calls,
            ));
            if calls.is_empty() {
                messages.push(Message::text(
                    Role::User,
                    "End with exactly one graph_submit call; do not answer in prose.",
                ));
                continue;
            }
            let mut results = Vec::new();
            let mut submitted = None;
            for c in &calls {
                self.append(sid, tool_op(c, Some("librarian")))?;
                let (exec, end) = if submitted.is_some() {
                    (
                        refuse(&c.name, "not run: the graph was already submitted"),
                        None,
                    )
                } else {
                    self.graph_tool(sid, c)?
                };
                let Exec::Done { content, is_error } = exec else {
                    continue;
                };
                self.append(sid, result_op(c, &content, is_error, Some("librarian")))?;
                results.push(ContentBlock::ToolResult {
                    call_id: c.id.clone(),
                    content,
                    is_error,
                });
                if end.is_some() {
                    submitted = end;
                }
            }
            if let Some(summary) = submitted {
                return Ok(summary);
            }
            messages.push(Message {
                role: Role::User,
                content: results,
            });
        }
        Err(Error::Invalid(
            "the graph builder stopped without an accepted graph_submit".into(),
        ))
    }

    /// Execute one graph-builder tool. The second value is the job's result
    /// when `graph_submit` was accepted — the run ends there.
    fn graph_tool(&self, sid: &str, call: &ToolCall) -> Result<(Exec, Option<Value>), Error> {
        let name = call.name.as_str();
        let args = &call.arguments;
        if !args.is_object() {
            return Ok((
                refuse(name, "the arguments must be one valid JSON object"),
                None,
            ));
        }
        match name {
            "graph_search" => match req_str(args, "query") {
                Ok(query) => {
                    let map = crate::knowledge::load(&self.repo)?;
                    Ok((done(crate::graph::search(&map, query)), None))
                }
                Err(e) => Ok((refuse(name, e), None)),
            },
            "graph_submit" => {
                let map = crate::knowledge::load(&self.repo)?;
                let at = now_iso();
                let batch = match crate::graph::check(args, &map, Some(sid), &at) {
                    Ok(batch) => batch,
                    Err(problems) => {
                        let text = format!(
                            "the submission has problems; fix all of them and call graph_submit again:\n- {}",
                            problems.join("\n- ")
                        );
                        return Ok((refuse(name, text), None));
                    }
                };
                crate::graph::append(&self.repo, &batch.ops)?;
                crate::graph::record_splits(&self.repo, batch.splits.clone())?;
                let mut topics = 0usize;
                let mut nodes = 0usize;
                let mut edges = 0usize;
                let mut chains = 0usize;
                let mut merges = 0usize;
                let mut venues = 0usize;
                for op in &batch.ops {
                    match op {
                        crate::graph::GraphOp::Topic { .. } => topics += 1,
                        crate::graph::GraphOp::Node { .. } => nodes += 1,
                        crate::graph::GraphOp::Edge { .. } => edges += 1,
                        crate::graph::GraphOp::Chain { .. } => chains += 1,
                        crate::graph::GraphOp::SameAs { .. } => merges += 1,
                        crate::graph::GraphOp::Venue { .. } => venues += 1,
                        _ => {}
                    }
                }
                let splits = batch.splits.len();
                let line = format!(
                    "Submitted: {topics} topic(s), {nodes} node(s), {edges} edge(s), \
                     {chains} chain(s), {merges} merge(s), {venues} venue(s), {splits} split(s)."
                );
                let counts = json!({
                    "topics": topics, "nodes": nodes, "edges": edges,
                    "chains": chains, "sameAs": merges, "venues": venues, "splits": splits,
                });
                Ok((done(line), Some(counts)))
            }
            other => Ok((refuse(other, "unknown tool"), None)),
        }
    }

    // ---- title -----------------------------------------------------------------

    /// Generate the session title once, after `goal_set` (role Reader, one
    /// short call, no tools).
    pub(crate) fn spawn_title(self: &Arc<Self>, sid: &str, goal: Value) {
        let me = Arc::clone(self);
        let session = sid.to_owned();
        let fut: BoxFuture = Box::pin(async move {
            let permit = Arc::clone(&me.reader_slots).acquire_owned().await;
            let messages = [Message::text(Role::User, goal.to_string())];
            let reply = me
                .call_role(
                    ModelRole::Reader,
                    &session,
                    &op_id(),
                    TITLE_SYSTEM,
                    &messages,
                    &[],
                    None,
                    None,
                )
                .await;
            drop(permit);
            let title = match reply {
                Ok(r) => clean_title(&r.text),
                Err(e) => {
                    tracing::debug!("title: {e}");
                    return;
                }
            };
            if title.is_empty() {
                return;
            }
            let op = Op::Title(TitleOp {
                op_id: op_id(),
                at: now_iso(),
                title,
            });
            if me.append(&session, op).is_ok() {
                if let Ok(summary) = me.store.summary(&session) {
                    me.emit(&session, AgentEvent::Session { session: summary });
                }
            }
        });
        self.spawn_tracked(sid, fut);
    }

    // ---- recovery ----------------------------------------------------------------

    /// Jobs left `queued`/`running` by a previous run of the app have no task:
    /// mark them aborted and start them again. Runs once per session.
    pub fn recover_jobs(self: &Arc<Self>, sid: &str) -> Result<(), Error> {
        if !lock(&self.recovered).insert(sid.to_owned()) {
            return Ok(());
        }
        let state: SessionState = replay_state(&self.store.ops(sid)?);
        let live: Vec<String> = lock(&self.jobs).keys().cloned().collect();
        for job in state.open_jobs {
            if live.contains(&job.job_id) {
                continue;
            }
            let mut op = self.job_op(&job.job_id, &job.kind, "aborted");
            op.blocking = job.blocking;
            op.error = Some("interrupted when the app closed".into());
            self.append(sid, Op::Job(op))?;
            if job.blocking {
                continue; // its planner run is restarted instead
            }
            if let Some(args) = job.args {
                self.spawn_job(sid, &job.kind, args, &job.question)?;
            }
        }
        // Results that finished but weren't delivered (app closed first).
        if !state.queued_jobs.is_empty() {
            let me = Arc::clone(self);
            let session = sid.to_owned();
            let fut: BoxFuture = Box::pin(async move { me.flush_deliveries(&session).await });
            self.spawn_tracked(sid, fut);
        }
        Ok(())
    }
}

/// Most page images one researcher run may see.
const MAX_PAGE_IMAGES_PER_RUN: usize = 4;

/// One tool result of the researcher's loop.
enum ResearchStep {
    /// The brief: the run ends.
    Brief(Value),
    /// Text for the model, and whether it is an error.
    Result(String, bool),
    /// Text for the model's tool-result block, plus a page image that goes
    /// into the next user message as an image block.
    Image {
        text: String,
        caption: String,
        media_type: String,
        data: String,
    },
}

/// What `source_page_image` should do for one page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PageDecision {
    /// A text layer is there: the model reads it as text instead.
    TextLayer,
    /// A scan, and the Reader model may see images: attach it.
    ShowImage,
    /// A scan, and the Reader model may not see images: refuse, in words.
    NoVision,
}

/// The decision behind `source_page_image`, as a pure function so the three
/// cases are testable without a model or a network.
fn page_decision(scanned: bool, vision: Option<bool>) -> PageDecision {
    if !scanned {
        PageDecision::TextLayer
    } else if vision == Some(false) {
        PageDecision::NoVision
    } else {
        PageDecision::ShowImage
    }
}

/// The session's plan as the builder's `{{plan}}` hint: what was being
/// taught, not a constraint.
fn plan_hint(state: &SessionState) -> String {
    match &state.plan {
        Some(plan) => match serde_json::to_string_pretty(plan) {
            Ok(text) => crate::agent::clip(&text, MAX_GRAPH_PLAN),
            Err(_) => "(the plan could not be rendered)".to_owned(),
        },
        None => "(no plan yet)".to_owned(),
    }
}

fn assistant_message(text: &str, native: &[ContentBlock], calls: &[ToolCall]) -> Message {
    let mut content: Vec<ContentBlock> = native.to_vec();
    if !text.is_empty() {
        content.push(ContentBlock::Text {
            text: text.to_owned(),
        });
    }
    for c in calls {
        content.push(ContentBlock::ToolCall {
            id: c.id.clone(),
            name: c.name.clone(),
            arguments: c.arguments.clone(),
        });
    }
    if content.is_empty() {
        content.push(ContentBlock::Text {
            text: "(no output)".into(),
        });
    }
    Message {
        role: Role::Assistant,
        content,
    }
}

/// The fields a brief must carry for `kind`, as one phrase. The single place
/// the code names them, so the nudge, the hard error and the tool schema
/// (`tools::researcher_tools`) can never tell the model less than
/// [`check_brief`] requires.
fn brief_fields(kind: &str) -> &'static str {
    match kind {
        "scope" => "summary, strands",
        _ => "question, answer",
    }
}

/// The user message sent after a prose reply: the first time the plain
/// instruction, after that the exact shape, with nothing else to read.
fn brief_nudge(kind: &str, nudge: usize) -> String {
    match nudge {
        1 => format!(
            "Finish with exactly one brief_submit (kind \"{kind}\"); required: {}.",
            brief_fields(kind)
        ),
        _ => format!(
            "Reply with the brief_submit tool call and nothing else: brief_submit {{ kind: \"{kind}\", {} }}",
            brief_fields(kind)
        ),
    }
}

/// The brief a run ends with when it never called `brief_submit`.
///
/// A model that answered the question in prose, well but in the wrong shape,
/// must not fail the job: `probe_done` waits for a `scope` brief, so a failed
/// run blocks the session. Substantive text becomes a brief of the task's
/// kind, carrying the answer and an `uncertain` note that the structured
/// fields are missing. Shorter text (a preamble, an apology, nothing) is not
/// an answer, so it is an `Err` naming what was missing and what the last
/// reply looked like.
fn salvage_brief(task_kind: &str, question: &str, last_text: &str) -> Result<Value, String> {
    let text = last_text.trim();
    if text.chars().count() < MIN_SALVAGE_CHARS {
        let seen = if text.is_empty() {
            "empty".to_owned()
        } else {
            format!("\"{}\"", crate::agent::clip(text, 120))
        };
        return Err(format!(
            "the researcher stopped without brief_submit: a `{task_kind}` brief needs {}, and its last reply was {seen}",
            brief_fields(task_kind)
        ));
    }
    let mut brief = json!({
        "kind": task_kind,
        "uncertain": [SALVAGE_NOTE],
    });
    let object = brief.as_object_mut().expect("a fresh json object");
    match task_kind {
        "scope" => {
            object.insert("summary".into(), json!(text));
            // The map it never sent: an empty strand list says "none known"
            // rather than inventing one, and keeps the brief the shape
            // `check_brief` accepts.
            object.insert("strands".into(), json!([]));
        }
        _ => {
            if !question.trim().is_empty() {
                object.insert("question".into(), json!(question));
            }
            object.insert("answer".into(), json!(text));
        }
    }
    Ok(brief)
}

/// Minimal shape checks on a brief (jobs/researcher.md).
fn check_brief(task_kind: &str, args: &Value) -> Result<(), String> {
    let kind = opt_str(args, "kind").unwrap_or("");
    if kind != task_kind {
        return Err(format!(
            "this task is `{task_kind}`, so the brief's kind must be `{task_kind}`"
        ));
    }
    match kind {
        "scope" => {
            req_str(args, "summary")?;
            if !args.get("strands").is_some_and(Value::is_array) {
                return Err("a scope brief needs `strands` (array)".into());
            }
        }
        _ => {
            req_str(args, "answer")?;
        }
    }
    Ok(())
}

fn clean_title(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let line = line
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '*' || c == '#' || c.is_whitespace());
    let line = line.trim_end_matches('.');
    crate::agent::clip(line, 60)
}

async fn planner_file_tool(name: &str, args: &Value) -> Exec {
    let Some(root) = files::home_dir() else {
        return refuse(name, "no home directory on this device");
    };
    let args = args.clone();
    let tool = name.to_owned();
    let r = tokio::task::spawn_blocking(move || {
        if tool == "file_find" {
            files::file_find(&root, req_str(&args, "pattern")?)
        } else {
            files::file_read(&root, req_str(&args, "path")?, opt_str(&args, "lines"))
        }
    })
    .await;
    match r {
        Ok(Ok(t)) => done(t),
        Ok(Err(e)) => refuse(name, e),
        Err(e) => refuse(name, e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn briefs_must_match_the_task_kind_and_shape() {
        assert!(check_brief("scope", &json!({"kind":"scope","summary":"s","strands":[]})).is_ok());
        assert!(check_brief("scope", &json!({"kind":"verify","answer":"a"})).is_err());
        assert!(check_brief("scope", &json!({"kind":"scope","summary":"s"})).is_err());
        assert!(check_brief("verify", &json!({"kind":"verify","answer":"a"})).is_ok());
    }

    #[test]
    fn prose_that_answers_is_salvaged_into_a_brief_of_the_task_kind() {
        let prose = "Dividers rest on one current through series parts, so the shares follow the \
                     resistances. A learner who reads the formula as a weighted average usually \
                     gets the ratio backwards; probe it with two unequal resistors."
            .repeat(2);
        assert!(prose.chars().count() >= MIN_SALVAGE_CHARS);

        let scope = salvage_brief("scope", "map the field", &prose).unwrap();
        assert_eq!(scope["kind"], "scope");
        assert_eq!(scope["summary"], prose);
        assert_eq!(scope["uncertain"], json!([SALVAGE_NOTE]));
        assert!(
            check_brief("scope", &scope).is_ok(),
            "the salvaged brief is the shape the code accepts"
        );
        assert!(
            scope.get("answer").is_none(),
            "no answer field on a scope brief"
        );

        let verify = salvage_brief("verify", "does Vout track R2?", &prose).unwrap();
        assert_eq!(verify["kind"], "verify");
        assert_eq!(verify["answer"], prose);
        assert_eq!(verify["question"], "does Vout track R2?");
        assert_eq!(verify["uncertain"], json!([SALVAGE_NOTE]));
        assert!(check_brief("verify", &verify).is_ok());
        assert!(verify.get("summary").is_none());

        // A task with no question leaves the field out rather than storing "".
        let unanswered = salvage_brief("source", "  ", &prose).unwrap();
        assert!(unanswered.get("question").is_none());
        assert_eq!(unanswered["answer"], prose);
    }

    #[test]
    fn prose_too_short_to_be_an_answer_fails_with_a_readable_reason() {
        let empty = salvage_brief("scope", "q", "  \n ").unwrap_err();
        assert!(empty.contains("without brief_submit"), "{empty}");
        assert!(empty.contains("summary, strands"), "{empty}");
        assert!(empty.contains("last reply was empty"), "{empty}");

        let prelude = "Let me look into that.\nI will search.".repeat(2);
        assert!(prelude.chars().count() < MIN_SALVAGE_CHARS);
        let short = salvage_brief("verify", "q", &prelude).unwrap_err();
        assert!(short.contains("question, answer"), "{short}");
        assert!(
            short.contains("Let me look into that."),
            "the reason quotes the last reply: {short}"
        );
        // Exactly at the threshold is substantive.
        let at = "x".repeat(MIN_SALVAGE_CHARS);
        assert!(salvage_brief("verify", "q", &at).is_ok());
        let under = "x".repeat(MIN_SALVAGE_CHARS - 1);
        assert!(salvage_brief("verify", "q", &under).is_err());
    }

    #[test]
    fn the_nudge_escalates_and_names_what_each_kind_needs() {
        assert_eq!(
            brief_nudge("scope", 1),
            "Finish with exactly one brief_submit (kind \"scope\"); required: summary, strands."
        );
        assert_eq!(
            brief_nudge("scope", 2),
            "Reply with the brief_submit tool call and nothing else: \
             brief_submit { kind: \"scope\", summary, strands }"
        );
        assert_eq!(
            brief_nudge("verify", 1),
            "Finish with exactly one brief_submit (kind \"verify\"); required: question, answer."
        );
    }

    #[test]
    fn the_brief_schema_declares_what_each_kind_is_checked_for() {
        let tools = crate::tools::researcher_tools();
        let brief = tools.iter().find(|t| t.name == "brief_submit").unwrap();
        let props = &brief.input_schema["properties"];
        for kind in ["scope", "verify", "source"] {
            for field in brief_fields(kind).split(", ") {
                assert!(
                    props.get(field).is_some(),
                    "`check_brief` refuses a `{kind}` brief without `{field}`, which the schema never declares"
                );
            }
        }
    }

    #[test]
    fn titles_are_cleaned() {
        assert_eq!(
            clean_title("\n\"Voltage dividers.\"\nmore"),
            "Voltage dividers"
        );
    }

    #[test]
    fn a_text_page_is_read_as_text_and_a_scan_as_an_image() {
        // A page with a text layer is never worth an image, whatever the
        // model can see.
        assert_eq!(page_decision(false, Some(true)), PageDecision::TextLayer);
        assert_eq!(page_decision(false, Some(false)), PageDecision::TextLayer);
        assert_eq!(page_decision(false, None), PageDecision::TextLayer);
        // A scan is an image, unless the Reader model can't see images.
        assert_eq!(page_decision(true, Some(true)), PageDecision::ShowImage);
        assert_eq!(page_decision(true, None), PageDecision::ShowImage);
        assert_eq!(page_decision(true, Some(false)), PageDecision::NoVision);
    }
}
