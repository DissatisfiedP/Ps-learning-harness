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
use crate::sessions::{self, op_id, JobOp, Op, PlanKeepOp, TitleOp};
use crate::state::{Phase, QueuedResult, SessionState};
use crate::tools::{self, opt_str, req_str};
use crate::{lock, now_iso, Error, Harness, ModelRole};

type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Most model calls in one researcher run.
const MAX_RESEARCH_STEPS: usize = 16;

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

    /// Persist a queued job and start it. `kind` is `research` or `plan`.
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
        let semaphore = if kind == "plan" {
            Arc::clone(&self.planner_slots)
        } else {
            Arc::clone(&self.reader_slots)
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
        let mut task = format!(
            "Task `{kind}`.\n\nQuestion: {}\n",
            opt_str(args, "question").unwrap_or("")
        );
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
        let catalog = lock(&self.plugins).available();
        values.set(
            "plugin_catalog",
            composer.plugin_catalog(&state.platform, &catalog),
        );
        let system = composer.compose_job("jobs/researcher.md", &values, &Default::default())?;
        let tools = tools::researcher_tools();
        let cache_key = op_id();
        let mut messages = vec![Message::text(Role::User, "Begin the task.")];
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
            messages.push(assistant_message(
                &reply.text,
                &reply.completion.native,
                &calls,
            ));
            if calls.is_empty() {
                messages.push(Message::text(
                    Role::User,
                    "Finish with exactly one brief_submit.",
                ));
                continue;
            }
            let mut results = Vec::new();
            let mut brief = None;
            for c in &calls {
                let (content, is_error) = match self.research_tool(&kind, c).await {
                    ResearchStep::Brief(b) => {
                        brief = Some(b);
                        ("Brief submitted.".to_owned(), false)
                    }
                    ResearchStep::Result(text, err) => (text, err),
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
        }
        Err(Error::Invalid(
            "the researcher stopped without brief_submit".into(),
        ))
    }

    async fn research_tool(&self, task_kind: &str, call: &ToolCall) -> ResearchStep {
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
            "brief_submit" => match check_brief(task_kind, args) {
                Ok(()) => ResearchStep::Brief(args.clone()),
                Err(e) => ResearchStep::Result(format!("brief_submit refused: {e}"), true),
            },
            other => fail(format!("unknown tool `{other}`")),
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
        let mut messages = replay::conversation(&ops);
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
            "research" => {
                let exec = match self.research_blocking(sid, args).await {
                    Ok(brief) => done(serde_json::to_string_pretty(&brief).unwrap_or_default()),
                    Err(e) => refuse(name, self.redact(&e.to_string())),
                };
                Ok((exec, None))
            }
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
        let manifests = lock(&self.plugins).available();
        let plan = match tools::check_plan(args, &state, &manifests) {
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
        self.append(sid, Op::Plan(sessions::record(args.clone())))?;
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

enum ResearchStep {
    Brief(Value),
    Result(String, bool),
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
    fn titles_are_cleaned() {
        assert_eq!(
            clean_title("\n\"Voltage dividers.\"\nmore"),
            "Voltage dividers"
        );
    }
}
