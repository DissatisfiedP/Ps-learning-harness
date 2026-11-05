//! The agent runtime (architecture §4.10, §4.15; build prompt M4/M5).
//!
//! One tutor turn: compose the system prompt for the current phase, build the
//! messages from the ops, stream one model call with the phase's tools,
//! persist every tool call before executing it, execute it, persist the
//! result, and loop (at most 12 model calls per turn). Interactive tools
//! (`question_ask`, `choice_ask`) end the turn with the call pending; the
//! learner's [`Harness::answer`] persists the result and resumes.
//!
//! Every gate is here, in code: a refused call is answered with an `is_error`
//! tool result that says why, so the model can recover. Turn state is always
//! re-derived from the op log, so a turn can resume after a restart.
//!
//! Concurrency: one `tokio::sync::Mutex` per session serializes tutor turns,
//! learner answers and job deliveries, so a job result is never interleaved
//! into a running turn. No std lock is held across an `.await`.

use std::collections::HashSet;
use std::sync::Arc;

use providers::{
    ChatRequest, Completion, Message, ProviderId, Role, StopReason, StreamEvent, ToolCall, ToolSpec,
};
use serde_json::{json, Value};
use tokio::sync::oneshot;

use crate::events::{AgentEvent, AgentName};
use crate::files;
use crate::plugins::{Plugin, PluginContext};
use crate::prompts::{Composer, TemplateValues};
use crate::replay::{self, replay_state};
use crate::sessions::{
    self, op_id, ApproveOp, Op, PhaseOp, PlanRequestOp, PluginGrantOp, RemovedPlugin, ToolOp,
    ToolResultValue,
};
use crate::state::{Phase, SessionState};
use crate::tools::{self, opt_str, req_str, MAX_PLUGINS_PER_SECTION};
use crate::{lock, now_iso, Ending, Error, Harness, Mode, ModelRole, SessionSummary};

/// Most model calls in one agent turn.
pub(crate) const MAX_LOOP: usize = 12;
/// Context window assumed when the catalog doesn't say.
const DEFAULT_CONTEXT: usize = 100_000;

const READ_ONLY: &str =
    "this session was made before the agent runtime and is read-only; start a new session";

/// Result of executing one tool call.
pub(crate) enum Exec {
    Done {
        content: String,
        is_error: bool,
    },
    /// Interactive: the turn ends with the call pending.
    Pause,
}

pub(crate) fn done(content: impl Into<String>) -> Exec {
    Exec::Done {
        content: content.into(),
        is_error: false,
    }
}

pub(crate) fn refuse(name: &str, why: impl std::fmt::Display) -> Exec {
    Exec::Done {
        content: format!("{name} refused: {why}"),
        is_error: true,
    }
}

/// What one streamed model call produced.
pub(crate) struct Reply {
    pub completion: Completion,
    pub text: String,
    pub thinking: String,
    pub model: String,
    pub interrupted: bool,
}

pub(crate) fn tool_op(call: &ToolCall, agent: Option<&str>) -> Op {
    Op::Tool(ToolOp {
        op_id: op_id(),
        at: now_iso(),
        call_id: call.id.clone(),
        name: call.name.clone(),
        args: call.arguments.clone(),
        result: None,
        agent: agent.map(str::to_owned),
    })
}

pub(crate) fn result_op(call: &ToolCall, content: &str, is_error: bool, agent: Option<&str>) -> Op {
    Op::Tool(ToolOp {
        op_id: op_id(),
        at: now_iso(),
        call_id: call.id.clone(),
        name: call.name.clone(),
        args: Value::Null,
        result: Some(ToolResultValue {
            content: content.to_owned(),
            is_error,
        }),
        agent: agent.map(str::to_owned),
    })
}

pub(crate) fn phase_op(phase: Phase, section_id: Option<String>) -> Op {
    Op::Phase(PhaseOp {
        op_id: op_id(),
        at: now_iso(),
        phase,
        section_id,
    })
}

impl Harness {
    // ---- plumbing --------------------------------------------------------

    pub(crate) fn emit(&self, session_id: &str, event: AgentEvent) {
        let sink = lock(&self.sink).clone();
        if let Some(sink) = sink {
            sink(session_id, event);
        }
    }

    /// Append an op and tell the UI.
    pub(crate) fn append(&self, session_id: &str, op: Op) -> Result<(), Error> {
        self.store.append_op(session_id, &op)?;
        if let Ok(v) = serde_json::to_value(&op) {
            self.emit(session_id, AgentEvent::Op { op: v });
        }
        Ok(())
    }

    pub(crate) fn turn_lock(&self, session_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(
            lock(&self.turn_locks)
                .entry(session_id.to_owned())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
    }

    /// Ops of a writable Learn session.
    pub(crate) fn learn_ops(&self, session_id: &str) -> Result<Vec<Op>, Error> {
        let ops = self.store.ops(session_id)?;
        let mode = ops.iter().find_map(|o| match o {
            Op::Session(h) => Some(h.mode),
            _ => None,
        });
        if mode != Some(Mode::Learn) {
            return Err(Error::Invalid("this is not a Learn session".into()));
        }
        if !ops.iter().any(|o| matches!(o, Op::Phase(_))) {
            return Err(Error::Invalid(READ_ONLY.into()));
        }
        Ok(ops)
    }

    /// Derived state of a session (never stored).
    pub fn session_state(&self, session_id: &str) -> Result<SessionState, Error> {
        Ok(replay_state(&self.store.ops(session_id)?))
    }

    /// Register a plugin implementation (M5 scaffolding). Its manifest comes
    /// from `prompts/plugins/<id>.md` when that file exists.
    pub fn register_plugin(&self, plugin: Arc<dyn Plugin>) -> Result<(), Error> {
        let id = plugin.manifest().id.clone();
        let composer = Composer::new(&self.repo);
        let path = format!("plugins/{id}.md");
        let prompt = if Composer::bundled(&path).is_some()
            || self.repo.join("prompts").join(&path).exists()
        {
            Some(composer.plugin(&path)?)
        } else {
            None
        };
        lock(&self.plugins)
            .register(plugin, prompt)
            .map_err(Error::Invalid)
    }

    /// Remove every saved credential string from text that will be persisted
    /// or shown (error messages can echo request details).
    pub(crate) fn redact(&self, text: &str) -> String {
        let secrets: Vec<String> = {
            let config = lock(&self.config);
            config
                .credentials
                .values()
                .filter_map(|c| serde_json::to_value(c).ok())
                .flat_map(|v| strings_in(&v))
                .chain(
                    ProviderId::ALL
                        .iter()
                        .filter_map(|p| match p.credential_from_env() {
                            Some(providers::Credential::ApiKey { key }) => Some(key),
                            _ => None,
                        }),
                )
                .filter(|s| s.len() >= 12)
                .collect()
        };
        let mut out = text.to_owned();
        for s in secrets {
            out = out.replace(&s, "[redacted]");
        }
        out
    }

    // ---- model calls -------------------------------------------------------

    /// Stream one model call for `role`. `stream_to` forwards text/thinking to
    /// the UI under that agent; `cancel` stops the call (partial text kept).
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn call_role(
        &self,
        role: ModelRole,
        session_id: &str,
        cache_key: &str,
        system: &str,
        messages: &[Message],
        tools: &[ToolSpec],
        stream_to: Option<AgentName>,
        cancel: Option<oneshot::Receiver<()>>,
    ) -> Result<Reply, Error> {
        let choice = lock(&self.config).resolve(role)?;
        let req = ChatRequest {
            model: &choice.model,
            system,
            messages,
            max_tokens: crate::MAX_TOKENS,
            effort: choice.effort.as_deref(),
            tools,
            cache_key: Some(cache_key),
        };
        let mut text = String::new();
        let mut thinking = String::new();
        let started = std::time::Instant::now();
        let mut first_token: Option<std::time::Duration> = None;
        let mut on_delta = |ev: StreamEvent<'_>| {
            if first_token.is_none() {
                first_token = Some(started.elapsed());
            }
            match ev {
                StreamEvent::Text(d) => {
                    text.push_str(d);
                    if let Some(agent) = stream_to {
                        self.emit(
                            session_id,
                            AgentEvent::Text {
                                agent,
                                delta: d.to_owned(),
                            },
                        );
                    }
                }
                StreamEvent::Thinking(d) => {
                    thinking.push_str(d);
                    if let Some(agent) = stream_to {
                        self.emit(
                            session_id,
                            AgentEvent::Thinking {
                                agent,
                                delta: d.to_owned(),
                            },
                        );
                    }
                }
            }
        };
        let call = self.stream_with_refresh(choice.provider, cache_key, &req, &mut on_delta);
        let outcome = match cancel {
            Some(mut stop) => tokio::select! {
                r = call => Some(r),
                _ = &mut stop => None,
            },
            None => Some(call.await),
        };
        tracing::debug!(
            role = role.label(),
            first_token_ms = first_token.map(|d| d.as_millis() as u64),
            total_ms = started.elapsed().as_millis() as u64,
            "model call"
        );
        let model = format!("{}/{}", choice.provider.spec().name, choice.model);
        match outcome {
            Some(Ok(completion)) => {
                self.add_usage(role, &completion.usage);
                Ok(Reply {
                    completion,
                    text,
                    thinking,
                    model,
                    interrupted: false,
                })
            }
            Some(Err(e)) => Err(Error::Invalid(self.redact(&e.to_string()))),
            None => Ok(Reply {
                completion: Completion {
                    stop: StopReason::End,
                    usage: providers::Usage::default(),
                    tool_calls: Vec::new(),
                    native: Vec::new(),
                },
                text,
                thinking,
                model,
                interrupted: true,
            }),
        }
    }

    /// Context window of the model behind `role`, from the catalog.
    pub(crate) fn context_window(&self, role: ModelRole) -> usize {
        let Ok(choice) = lock(&self.config).resolve(role) else {
            return DEFAULT_CONTEXT;
        };
        choice
            .provider
            .suggested_models()
            .into_iter()
            .find(|m| m.id == choice.model)
            .and_then(|m| m.context_window)
            .map_or(DEFAULT_CONTEXT, |c| c as usize)
    }

    // ---- composition -------------------------------------------------------

    /// The tutor's system prompt and tool list for `phase` (normally the
    /// session's phase).
    pub(crate) fn compose_tutor(
        &self,
        ops: &[Op],
        state: &SessionState,
        phase: Phase,
    ) -> Result<(String, Vec<ToolSpec>), Error> {
        let composer = Composer::new(&self.repo);
        let flags = HashSet::new(); // mermaid stays off until the UI renders it
        let mut base = TemplateValues::new();
        base.set("session_state", Composer::session_state(state));
        let (fragments, plugin_tools) = {
            let registry = lock(&self.plugins);
            (
                registry.fragments(&state.active_plugins),
                registry.tool_specs(&state.active_plugins),
            )
        };
        let mut values = TemplateValues::new();
        values.set("active_plugins", "");
        let (file, label) = match phase {
            Phase::Measure => ("tutor/phases/measure.md", "measure"),
            Phase::Teach | Phase::Close => {
                let section = state.current_section().or_else(|| {
                    // In Close, the last section frames the prompt.
                    let plan = state.plan.as_ref()?;
                    let last = state.completed_sections.last()?;
                    plan.sections.iter().find(|s| &s.id == last)
                });
                let carry = carry_over(ops);
                match section {
                    Some(s) => {
                        values.set("section.title", s.title.clone());
                        values.set("section.id", s.id.clone());
                        values.set("section.nodes", section_nodes(state, &s.node_ids));
                        values.set("section.style", s.style.clone());
                        values.set("section.exit", s.exit.clone());
                        values.set("section.notes", non_empty(&s.notes, "(none)"));
                        values.set(
                            "mode",
                            composer.load(&format!("tutor/modes/{}.md", s.mode.as_str()))?,
                        );
                    }
                    None => {
                        for k in [
                            "section.title",
                            "section.id",
                            "section.nodes",
                            "section.style",
                            "section.exit",
                            "section.notes",
                        ] {
                            values.set(k, "(none)");
                        }
                        values.set("mode", "");
                    }
                }
                values.set("section.carryOver", non_empty(&carry, "(none)"));
                values.set("active_plugins", fragments);
                (
                    "tutor/phases/teach.md",
                    if phase == Phase::Close {
                        "close"
                    } else {
                        "teach"
                    },
                )
            }
            Phase::Plan | Phase::AwaitApproval => {
                return Err(Error::Invalid(
                    "the tutor has no turn while the plan is being made".into(),
                ))
            }
        };
        let system = composer.compose_tutor(file, label, &base, &values, &flags)?;
        let mut tools = tools::tutor_tools(phase);
        if phase == Phase::Teach {
            tools.extend(plugin_tools);
        }
        Ok((system, tools))
    }

    pub(crate) fn compose_planner(&self, state: &SessionState) -> Result<String, Error> {
        let composer = Composer::new(&self.repo);
        let mut values = TemplateValues::new();
        values.set("session_state", Composer::session_state(state));
        let catalog = lock(&self.plugins).available();
        values.set(
            "plugin_catalog",
            composer.plugin_catalog(&state.platform, &catalog),
        );
        let goal = state.goal.as_ref();
        let exam = goal
            .and_then(|g| g.get("purpose"))
            .and_then(Value::as_str)
            .filter(|p| *p == "exam");
        if exam.is_some() {
            let exam_rec = goal.and_then(|g| g.get("exam"));
            values.set(
                "exam",
                exam_rec
                    .and_then(|e| e.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("the learner's exam"),
            );
            values.set(
                "days_left",
                exam_rec
                    .and_then(|e| e.get("date"))
                    .and_then(Value::as_str)
                    .and_then(days_until)
                    .map_or_else(|| "unknown".to_owned(), |d| d.to_string()),
            );
            values.set("sittings_left", "unknown");
        }
        composer.compose_planner(&values, exam, &HashSet::new())
    }

    // ---- the tutor turn ----------------------------------------------------

    /// One tutor turn, with events. The caller holds the session's turn lock.
    pub(crate) async fn tutor_turn(self: &Arc<Self>, session_id: &str) -> Result<(), Error> {
        self.emit(
            session_id,
            AgentEvent::TurnStart {
                agent: AgentName::Tutor,
            },
        );
        let started = std::time::Instant::now();
        let result = self.tutor_loop(session_id).await;
        tracing::debug!(
            session = session_id,
            ms = started.elapsed().as_millis() as u64,
            ok = result.is_ok(),
            "tutor turn"
        );
        self.emit(
            session_id,
            AgentEvent::TurnEnd {
                agent: AgentName::Tutor,
                error: result.as_ref().err().map(|e| self.redact(&e.to_string())),
            },
        );
        result
    }

    async fn tutor_loop(self: &Arc<Self>, session_id: &str) -> Result<(), Error> {
        self.deliver_queued(session_id)?;
        let start_phase = replay_state(&self.store.ops(session_id)?).phase;
        if !is_tutor_phase(start_phase) {
            return Ok(());
        }
        let mut handed_off = false;
        let mut compacted = false;
        for _ in 0..MAX_LOOP {
            let ops = self.store.ops(session_id)?;
            let state = replay_state(&ops);
            if state.pending_call.is_some() || state.ended {
                break;
            }
            // After probe_done the phase is Plan; the tutor gets one last call
            // (with its previous phase's prompt) to tell the learner.
            let handoff = !is_tutor_phase(state.phase);
            if handoff && handed_off {
                break;
            }
            let prompt_phase = if handoff { start_phase } else { state.phase };
            let (system, tools) = self.compose_tutor(&ops, &state, prompt_phase)?;
            let mut messages = replay::conversation(&ops);
            ensure_user_last(&mut messages);

            let window = self.context_window(ModelRole::Tutor);
            if !compacted && replay::estimate_tokens(&system, &messages) > window * 7 / 10 {
                compacted = true;
                if self.compact(session_id, &ops, window).await? {
                    continue;
                }
            }

            let (stop_tx, stop_rx) = oneshot::channel();
            lock(&self.running).insert(session_id.to_owned(), stop_tx);
            let reply = self
                .call_role(
                    ModelRole::Tutor,
                    session_id,
                    session_id,
                    &system,
                    &messages,
                    &tools,
                    Some(AgentName::Tutor),
                    Some(stop_rx),
                )
                .await;
            lock(&self.running).remove(session_id);
            let reply = reply?;
            self.persist_reply(session_id, &reply)?;
            if reply.interrupted {
                break;
            }
            let calls = reply.completion.tool_calls.clone();
            if calls.is_empty() || reply.completion.stop == StopReason::Refusal {
                break;
            }
            if handoff {
                handed_off = true;
                for c in &calls {
                    self.append(session_id, tool_op(c, None))?;
                    let msg = "not run: the plan phase has started; tell the learner in one line and end your turn";
                    self.append(session_id, result_op(c, msg, true, None))?;
                }
                continue;
            }
            let mut paused = false;
            for call in &calls {
                self.append(session_id, tool_op(call, None))?;
                if paused {
                    let msg = "not run: wait for the learner's answer to your question first (one question per turn); call it again then if you still need it";
                    self.append(session_id, result_op(call, msg, true, None))?;
                    continue;
                }
                let started = std::time::Instant::now();
                let exec = self.execute_tutor_tool(session_id, call, &tools).await?;
                tracing::debug!(tool = %call.name, ms = started.elapsed().as_millis() as u64, "tool");
                match exec {
                    Exec::Pause => paused = true,
                    Exec::Done { content, is_error } => {
                        self.append(session_id, result_op(call, &content, is_error, None))?;
                    }
                }
            }
            if paused {
                break;
            }
            if !is_tutor_phase(replay_state(&self.store.ops(session_id)?).phase) && handed_off {
                break;
            }
        }
        Ok(())
    }

    /// Persist a model reply as an assistant message (text, thinking, native
    /// blocks). Skipped when the reply is completely empty.
    fn persist_reply(&self, session_id: &str, reply: &Reply) -> Result<(), Error> {
        let has_calls = !reply.completion.tool_calls.is_empty();
        if reply.text.is_empty()
            && reply.thinking.is_empty()
            && reply.completion.native.is_empty()
            && !has_calls
            && !reply.interrupted
        {
            return Ok(());
        }
        let mut m = sessions::new_message(Role::Assistant, reply.text.clone());
        m.thinking.clone_from(&reply.thinking);
        m.native.clone_from(&reply.completion.native);
        m.model = Some(reply.model.clone());
        m.ending = if reply.interrupted {
            Some(Ending::Interrupted)
        } else {
            match reply.completion.stop {
                StopReason::MaxTokens => Some(Ending::MaxTokens),
                StopReason::Refusal => Some(Ending::Refusal),
                StopReason::End | StopReason::ToolUse => None,
            }
        };
        self.append(session_id, Op::Message(m))
    }

    /// Compaction turn (architecture §4.15): the tutor's role writes notes
    /// that replace the older turns. Returns whether a compaction was written.
    async fn compact(
        self: &Arc<Self>,
        session_id: &str,
        ops: &[Op],
        window: usize,
    ) -> Result<bool, Error> {
        let Some(cut) = replay::compaction_cut(ops, window / 5) else {
            return Ok(false);
        };
        let (previous, tail) = replay::after_cut(ops);
        let first = ops.len() - tail.len();
        if cut < first {
            return Ok(false);
        }
        let mut values = TemplateValues::new();
        values.set("turns_to_summarize", replay::transcript(&ops[first..=cut]));
        values.set("previous_summary", previous.unwrap_or("(none)"));
        let system = Composer::new(&self.repo).compose_compaction(&values, &HashSet::new())?;
        let messages = [Message::text(Role::User, "Write the notes now.")];
        let reply = self
            .call_role(
                ModelRole::Tutor,
                session_id,
                session_id,
                &system,
                &messages,
                &[],
                None,
                None,
            )
            .await?;
        if reply.text.trim().is_empty() {
            return Ok(false);
        }
        let covered = &ops[cut];
        self.append(
            session_id,
            Op::Compaction(sessions::CompactionOp {
                op_id: op_id(),
                at: now_iso(),
                summary: reply.text,
                cut_at: covered.at().to_owned(),
                cut_op_id: covered.op_id().to_owned(),
            }),
        )?;
        Ok(true)
    }

    // ---- tool execution (tutor) -------------------------------------------

    async fn execute_tutor_tool(
        self: &Arc<Self>,
        session_id: &str,
        call: &ToolCall,
        offered: &[ToolSpec],
    ) -> Result<Exec, Error> {
        let name = call.name.as_str();
        let args = &call.arguments;
        if !offered.iter().any(|t| t.name == name) {
            let state = self.session_state(session_id)?;
            return Ok(refuse(
                name,
                format!(
                    "`{name}` isn't available in the {} phase",
                    state.phase.label()
                ),
            ));
        }
        if let Value::String(raw) = args {
            return Ok(refuse(
                name,
                format!(
                    "the arguments were not valid JSON ({}); send one JSON object",
                    clip(raw, 200)
                ),
            ));
        }
        if !args.is_object() {
            return Ok(refuse(name, "the arguments must be a JSON object"));
        }
        let ops = self.store.ops(session_id)?;
        let state = replay_state(&ops);
        match name {
            "question_ask" => Ok(gate_question(&state, args)),
            "choice_ask" => Ok(gate_choice(args)),
            "goal_set" => self.goal_set(session_id, &state, args),
            "verdict_record" => self.verdict_record(session_id, &ops, args),
            "probe_done" => self.probe_done(session_id, &state, args),
            "research_request" => self.research_request(session_id, args),
            "plan_change_request" => self.plan_change_request(session_id, args),
            "plugin_request" => self.plugin_request(session_id, &state, args),
            "section_done" => self.section_done(session_id, &state, args),
            "session_end" => {
                if state.phase != Phase::Close {
                    self.append(session_id, phase_op(Phase::Close, None))?;
                }
                Ok(done("Session ended."))
            }
            "file_find" | "file_read" => Ok(file_tool(name, args).await),
            _ => Ok(self.plugin_call(session_id, &state, name, args).await),
        }
    }

    fn goal_set(
        self: &Arc<Self>,
        session_id: &str,
        state: &SessionState,
        args: &Value,
    ) -> Result<Exec, Error> {
        let goal = match req_str(args, "goal") {
            Ok(g) => g.to_owned(),
            Err(e) => return Ok(refuse("goal_set", e)),
        };
        match opt_str(args, "purpose") {
            Some("curiosity" | "project" | "work" | "exam") => {}
            _ => {
                return Ok(refuse(
                    "goal_set",
                    "`purpose` must be curiosity, project, work or exam",
                ))
            }
        }
        let first = state.goal.is_none();
        self.append(session_id, Op::Goal(sessions::record(args.clone())))?;
        if first {
            let job_args = json!({
                "kind": "scope",
                "question": goal,
                "context": args.to_string(),
            });
            self.spawn_job(session_id, "research", job_args, &format!("scope: {goal}"))?;
            if state.title.is_none() {
                self.spawn_title(session_id, args.clone());
            }
            Ok(done("Goal recorded. The scope research is running in the background; its brief will arrive as a message. Start probing now."))
        } else {
            Ok(done("Goal updated."))
        }
    }

    fn verdict_record(&self, session_id: &str, ops: &[Op], args: &Value) -> Result<Exec, Error> {
        let name = "verdict_record";
        let qid = match req_str(args, "questionId") {
            Ok(q) => q,
            Err(e) => return Ok(refuse(name, e)),
        };
        if !matches!(
            opt_str(args, "verdict"),
            Some("correct" | "partial" | "wrong")
        ) {
            return Ok(refuse(name, "`verdict` must be correct, partial or wrong"));
        }
        let question = ops.iter().find_map(|o| match o {
            Op::Tool(t) if t.call_id == qid && t.result.is_none() && t.name == "question_ask" => {
                Some(t.args.clone())
            }
            _ => None,
        });
        let Some(question) = question else {
            return Ok(refuse(name, format!("no question with questionId `{qid}`")));
        };
        let answered = ops.iter().any(|o| {
            matches!(o, Op::Tool(t) if t.call_id == qid && t.result.as_ref().is_some_and(|r| !r.is_error))
        });
        if !answered {
            return Ok(refuse(name, "that question hasn't been answered yet"));
        }
        if question.get("format").and_then(Value::as_str) == Some("choice") {
            return Ok(refuse(name, "choice questions are graded by the app"));
        }
        let _ = session_id;
        Ok(done("Verdict recorded."))
    }

    fn probe_done(
        self: &Arc<Self>,
        session_id: &str,
        state: &SessionState,
        args: &Value,
    ) -> Result<Exec, Error> {
        let name = "probe_done";
        if state.pending_call.is_some() {
            return Ok(refuse(name, "a question is still open"));
        }
        if state.brief.is_none() {
            return Ok(refuse(
                name,
                "the scope brief hasn't arrived. Keep probing other strands, or say in one line that you're waiting and end your turn; the brief's arrival will wake you",
            ));
        }
        if !args.get("strands").is_some_and(Value::is_array) {
            return Ok(refuse(name, "`strands` must be an array"));
        }
        self.append(session_id, Op::Probe(sessions::record(args.clone())))?;
        self.append(session_id, phase_op(Phase::Plan, None))?;
        self.append(
            session_id,
            Op::PlanRequest(PlanRequestOp {
                op_id: op_id(),
                at: now_iso(),
                from: "probe".into(),
                reason: Some("initial".into()),
                evidence: None,
                learner_words: None,
            }),
        )?;
        self.spawn_job(
            session_id,
            "plan",
            json!({ "request": INITIAL_PLAN_REQUEST }),
            "Building the plan",
        )?;
        Ok(done("Accepted. The planner is building the plan from this conversation. Tell the learner in one line, then end your turn."))
    }

    fn research_request(self: &Arc<Self>, session_id: &str, args: &Value) -> Result<Exec, Error> {
        let name = "research_request";
        let kind = match opt_str(args, "kind") {
            Some(k @ ("verify" | "scope" | "source")) => k,
            _ => return Ok(refuse(name, "`kind` must be verify, scope or source")),
        };
        let question = match req_str(args, "question") {
            Ok(q) => q,
            Err(e) => return Ok(refuse(name, e)),
        };
        if kind == "source" && opt_str(args, "path").is_none() {
            return Ok(refuse(name, "kind source needs `path`"));
        }
        let job = self.spawn_job(
            session_id,
            "research",
            args.clone(),
            &format!("{kind}: {question}"),
        )?;
        Ok(done(format!(
            "Research started (job {job}). The brief will arrive as a message; keep going meanwhile."
        )))
    }

    fn plan_change_request(
        self: &Arc<Self>,
        session_id: &str,
        args: &Value,
    ) -> Result<Exec, Error> {
        let name = "plan_change_request";
        let reason = match opt_str(args, "reason") {
            Some(r @ ("learner" | "missing-foundation" | "ahead" | "detour" | "probe-wrong")) => r,
            _ => {
                return Ok(refuse(
                    name,
                    "`reason` must be learner, missing-foundation, ahead, detour or probe-wrong",
                ))
            }
        };
        let evidence = match req_str(args, "evidence") {
            Ok(e) => e,
            Err(e) => return Ok(refuse(name, e)),
        };
        let words = opt_str(args, "learnerWords");
        self.append(
            session_id,
            Op::PlanRequest(PlanRequestOp {
                op_id: op_id(),
                at: now_iso(),
                from: "tutor".into(),
                reason: Some(reason.to_owned()),
                evidence: Some(evidence.to_owned()),
                learner_words: words.map(str::to_owned),
            }),
        )?;
        let mut request = format!(
            "Change request from the tutor (plan_change_request).\nreason: {reason}\nevidence: {evidence}\n"
        );
        if let Some(w) = words {
            request.push_str(&format!("learner's words: \"{w}\"\n"));
        }
        request.push_str("Check the evidence against the conversation, then answer with plan_keep or a revised plan_write.");
        self.spawn_job(
            session_id,
            "plan",
            json!({ "request": request }),
            &format!("Plan change: {reason}"),
        )?;
        Ok(done("Sent to the planner. Its answer will arrive as a message. Tell the learner in one line that the plan is being checked, then end your turn."))
    }

    fn plugin_request(
        &self,
        session_id: &str,
        state: &SessionState,
        args: &Value,
    ) -> Result<Exec, Error> {
        let name = "plugin_request";
        let plugin = match req_str(args, "plugin") {
            Ok(p) => p,
            Err(e) => return Ok(refuse(name, e)),
        };
        let Some(section) = state.current_section.clone() else {
            return Ok(refuse(name, "no section is being taught"));
        };
        if !lock(&self.plugins).grantable(plugin) {
            return Ok(refuse(
                name,
                format!(
                    "`{plugin}` is unknown or not available on {}",
                    state.platform
                ),
            ));
        }
        if state.active_plugins.iter().any(|p| p == plugin) {
            return Ok(done(format!(
                "`{plugin}` is already active in this section."
            )));
        }
        if state.active_plugins.len() >= MAX_PLUGINS_PER_SECTION {
            return Ok(refuse(
                name,
                format!("this section already has {MAX_PLUGINS_PER_SECTION} plugins (the limit)"),
            ));
        }
        self.append(
            session_id,
            Op::PluginGrant(PluginGrantOp {
                op_id: op_id(),
                at: now_iso(),
                section_id: section,
                plugin: plugin.to_owned(),
                by: "request".into(),
            }),
        )?;
        Ok(done(format!(
            "Granted: `{plugin}` is active from your next step."
        )))
    }

    fn section_done(
        &self,
        session_id: &str,
        state: &SessionState,
        args: &Value,
    ) -> Result<Exec, Error> {
        let name = "section_done";
        let Some(section) = state.current_section().cloned() else {
            return Ok(refuse(name, "no section is being taught"));
        };
        if opt_str(args, "sectionId") != Some(section.id.as_str()) {
            return Ok(refuse(
                name,
                format!("the current section is `{}`", section.id),
            ));
        }
        if req_str(args, "evidence").is_err() {
            return Ok(refuse(name, "missing `evidence`"));
        }
        let missing: Vec<&str> = section
            .node_ids
            .iter()
            .filter(|n| !state.node_passed(n))
            .map(String::as_str)
            .collect();
        if !missing.is_empty() {
            return Ok(refuse(
                name,
                format!(
                    "these nodes have no passing check question yet: {}",
                    missing.join(", ")
                ),
            ));
        }
        let next = state.plan.as_ref().and_then(|p| {
            p.sections
                .iter()
                .find(|s| s.id != section.id && !state.completed_sections.contains(&s.id))
        });
        match next {
            Some(n) => {
                self.append(session_id, phase_op(Phase::Teach, Some(n.id.clone())))?;
                Ok(done(format!(
                    "Section closed. Next section: {} (`{}`); its mode, notes and plugins are now in your instructions.",
                    n.title, n.id
                )))
            }
            None => {
                self.append(session_id, phase_op(Phase::Close, None))?;
                Ok(done(
                    "That was the last section. Write the close, then call session_end.",
                ))
            }
        }
    }

    async fn plugin_call(
        &self,
        session_id: &str,
        state: &SessionState,
        name: &str,
        args: &Value,
    ) -> Exec {
        let owner = lock(&self.plugins).owner(&state.active_plugins, name);
        let Some(plugin) = owner else {
            return refuse(name, "unknown tool");
        };
        // The future is 'static: no lock is held while it runs.
        let fut = plugin.call(
            name,
            args,
            PluginContext {
                session_id: session_id.to_owned(),
                platform: state.platform.clone(),
            },
        );
        let out = fut.await;
        Exec::Done {
            content: out.content,
            is_error: out.is_error,
        }
    }

    // ---- learner commands --------------------------------------------------

    /// Create a Learn session from the learner's opening message and run the
    /// first tutor turn.
    pub async fn start_teaching(self: &Arc<Self>, text: &str) -> Result<SessionSummary, Error> {
        let text = text.trim();
        if text.is_empty() {
            return Err(Error::Invalid("message is empty".into()));
        }
        lock(&self.config).resolve(ModelRole::Tutor)?;
        let summary = self.store.create(Mode::Learn, text)?;
        self.emit(
            &summary.id,
            AgentEvent::Session {
                session: summary.clone(),
            },
        );
        self.send_learner_message(&summary.id, text).await?;
        self.store.summary(&summary.id)
    }

    /// The learner wrote a message (not an answer to a card).
    pub async fn send_learner_message(
        self: &Arc<Self>,
        session_id: &str,
        text: &str,
    ) -> Result<(), Error> {
        let text = text.trim();
        if text.is_empty() {
            return Err(Error::Invalid("message is empty".into()));
        }
        let guard = self.turn_lock(session_id);
        let _turn = guard.lock().await;
        let state = replay_state(&self.learn_ops(session_id)?);
        if state.pending_call.is_some() {
            return Err(Error::Invalid("answer the open question first".into()));
        }
        match state.phase {
            Phase::Plan => {
                return Err(Error::Invalid(
                    "the planner is building the plan; wait for the plan card".into(),
                ))
            }
            Phase::AwaitApproval => {
                return Err(Error::Invalid(
                    "approve the plan or request a change on the plan card".into(),
                ))
            }
            _ => {}
        }
        self.append(
            session_id,
            Op::Message(sessions::new_message(Role::User, text.to_owned())),
        )?;
        self.tutor_turn(session_id).await
    }

    /// The learner answered the pending `question_ask` / `choice_ask`.
    /// Persists the result (grading choice questions by code) and resumes.
    pub async fn answer(
        self: &Arc<Self>,
        session_id: &str,
        call_id: &str,
        payload: Value,
    ) -> Result<(), Error> {
        let guard = self.turn_lock(session_id);
        let _turn = guard.lock().await;
        let state = replay_state(&self.learn_ops(session_id)?);
        let pending = state
            .pending_call
            .ok_or_else(|| Error::Invalid("no question is waiting for an answer".into()))?;
        if pending.call_id != call_id {
            return Err(Error::Invalid("that question is no longer open".into()));
        }
        let content = answer_result(&pending.name, call_id, &pending.args, &payload)?;
        let call = ToolCall {
            id: pending.call_id.clone(),
            name: pending.name.clone(),
            arguments: Value::Null,
        };
        self.append(
            session_id,
            result_op(&call, &content.to_string(), false, None),
        )?;
        self.tutor_turn(session_id).await
    }

    /// Approve the plan (minus the plugins the learner removed), move to
    /// Teach at the first open section and start the tutor.
    pub async fn approve(
        self: &Arc<Self>,
        session_id: &str,
        removed: Vec<RemovedPlugin>,
    ) -> Result<(), Error> {
        let guard = self.turn_lock(session_id);
        let _turn = guard.lock().await;
        let state = replay_state(&self.learn_ops(session_id)?);
        if state.phase != Phase::AwaitApproval {
            return Err(Error::Invalid(
                "there is no plan waiting for approval".into(),
            ));
        }
        let Some(plan) = state.plan.clone() else {
            return Err(Error::Invalid(
                "there is no plan waiting for approval".into(),
            ));
        };
        for r in &removed {
            let known = plan
                .sections
                .iter()
                .any(|s| s.id == r.section_id && s.plugins.contains(&r.plugin));
            if !known {
                return Err(Error::Invalid(format!(
                    "section `{}` has no plugin `{}`",
                    r.section_id, r.plugin
                )));
            }
        }
        let first_approval = state.approved_version.is_none();
        self.append(
            session_id,
            Op::Approve(ApproveOp {
                op_id: op_id(),
                at: now_iso(),
                plan_version: state.plan_version,
                removed_plugins: removed.clone(),
            }),
        )?;
        self.grant_plan_plugins(session_id, &state, &plan, &removed)?;
        let next = state.next_open_section();
        let title = next
            .as_ref()
            .and_then(|id| plan.sections.iter().find(|s| &s.id == id))
            .map_or_else(|| "(none)".to_owned(), |s| s.title.clone());
        match next {
            Some(id) => self.append(session_id, phase_op(Phase::Teach, Some(id)))?,
            None => self.append(session_id, phase_op(Phase::Close, None))?,
        }
        let mut m = sessions::new_message(
            Role::User,
            format!(
                "[plan update] Plan approved (version {}). Teaching {} with: {title}.",
                state.plan_version,
                if first_approval {
                    "starts"
                } else {
                    "continues"
                }
            ),
        );
        m.tag = Some("plan-notice".into());
        self.append(session_id, Op::Message(m))?;
        self.tutor_turn(session_id).await
    }

    /// Persist `pluginGrant` (by plan) for each plan plugin of an open section
    /// that isn't granted yet and wasn't removed by the learner.
    pub(crate) fn grant_plan_plugins(
        &self,
        session_id: &str,
        state: &SessionState,
        plan: &crate::state::Plan,
        removed: &[RemovedPlugin],
    ) -> Result<(), Error> {
        for s in &plan.sections {
            if state.completed_sections.contains(&s.id) {
                continue;
            }
            for p in &s.plugins {
                let is_removed = removed
                    .iter()
                    .any(|r| r.section_id == s.id && &r.plugin == p);
                let granted = state
                    .grants
                    .iter()
                    .any(|g| g.section_id == s.id && &g.plugin == p);
                if is_removed || granted {
                    continue;
                }
                self.append(
                    session_id,
                    Op::PluginGrant(PluginGrantOp {
                        op_id: op_id(),
                        at: now_iso(),
                        section_id: s.id.clone(),
                        plugin: p.clone(),
                        by: "plan".into(),
                    }),
                )?;
            }
        }
        Ok(())
    }

    /// The learner asks for a plan change (plan card, or the map during
    /// teach). Runs the planner in the background with their words.
    pub async fn request_plan_change(
        self: &Arc<Self>,
        session_id: &str,
        text: &str,
    ) -> Result<(), Error> {
        let text = text.trim();
        if text.is_empty() {
            return Err(Error::Invalid("describe the change you want".into()));
        }
        let state = replay_state(&self.learn_ops(session_id)?);
        if state.phase == Phase::Measure {
            return Err(Error::Invalid("there is no plan yet".into()));
        }
        if state.open_jobs.iter().any(|j| j.kind == "plan") {
            return Err(Error::Invalid(
                "the planner is already working; wait for its answer".into(),
            ));
        }
        self.append(
            session_id,
            Op::PlanRequest(PlanRequestOp {
                op_id: op_id(),
                at: now_iso(),
                from: "learner".into(),
                reason: Some("learner".into()),
                evidence: None,
                learner_words: Some(text.to_owned()),
            }),
        )?;
        let request = if state.plan.is_none() {
            format!("{INITIAL_PLAN_REQUEST}\nThe learner adds: \"{text}\"")
        } else {
            format!(
                "Change request from the learner: \"{text}\"\nRevise the plan with plan_write, or answer with plan_keep if the change isn't warranted."
            )
        };
        self.spawn_job(
            session_id,
            "plan",
            json!({ "request": request }),
            "Plan change requested",
        )?;
        Ok(())
    }

    /// Run the tutor again after a failed turn (nothing pending).
    pub async fn resume_turn(self: &Arc<Self>, session_id: &str) -> Result<(), Error> {
        let guard = self.turn_lock(session_id);
        let _turn = guard.lock().await;
        let state = replay_state(&self.learn_ops(session_id)?);
        if state.pending_call.is_some() {
            return Err(Error::Invalid("answer the open question first".into()));
        }
        self.tutor_turn(session_id).await
    }
}

pub(crate) const INITIAL_PLAN_REQUEST: &str =
    "Initial plan: measuring is done (probe_done). Build the plan for this learner from the whole conversation and call plan_write.";

fn is_tutor_phase(p: Phase) -> bool {
    matches!(p, Phase::Measure | Phase::Teach | Phase::Close)
}

/// Some wires treat a trailing assistant message as a prefill; a turn always
/// answers a user-side message.
pub(crate) fn ensure_user_last(messages: &mut Vec<Message>) {
    if messages.last().is_none_or(|m| m.role == Role::Assistant) {
        messages.push(Message::text(Role::User, "(continue)"));
    }
}

fn gate_question(state: &SessionState, args: &Value) -> Exec {
    let name = "question_ask";
    let role = opt_str(args, "role").unwrap_or("");
    let allowed: &[&str] = match state.phase {
        Phase::Measure => &["probe"],
        Phase::Teach => &["predict", "check"],
        _ => &[],
    };
    if !allowed.contains(&role) {
        return refuse(
            name,
            format!(
                "role `{role}` isn't available in the {} phase (allowed: {})",
                state.phase.label(),
                allowed.join(", ")
            ),
        );
    }
    if req_str(args, "prompt").is_err() || req_str(args, "answer").is_err() {
        return refuse(name, "`prompt` and a committed `answer` are required");
    }
    match opt_str(args, "format") {
        Some("free") => {}
        Some("choice") => {
            let options: Vec<&str> = args
                .get("options")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            if options.len() < 2 {
                return refuse(name, "a choice question needs at least two `options`");
            }
            let answer = opt_str(args, "answer").unwrap_or("");
            if !options.iter().any(|o| same_answer(o, answer)) {
                return refuse(name, "`answer` must be exactly one of `options`");
            }
        }
        _ => return refuse(name, "`format` must be free or choice"),
    }
    if state.phase == Phase::Teach {
        let node = opt_str(args, "nodeId").unwrap_or("");
        let in_section = state
            .current_section()
            .is_some_and(|s| s.node_ids.iter().any(|n| n == node));
        if !in_section {
            let nodes = state
                .current_section()
                .map(|s| s.node_ids.join(", "))
                .unwrap_or_default();
            return refuse(
                name,
                format!("nodeId `{node}` is not in the current section (its nodes: {nodes})"),
            );
        }
    }
    Exec::Pause
}

fn gate_choice(args: &Value) -> Exec {
    if req_str(args, "question").is_err() {
        return refuse("choice_ask", "missing `question`");
    }
    let n = args
        .get("options")
        .and_then(Value::as_array)
        .map_or(0, |a| a.iter().filter(|v| v.is_string()).count());
    if n == 0 {
        return refuse("choice_ask", "`options` must list at least one option");
    }
    Exec::Pause
}

fn same_answer(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    norm(a) == norm(b)
}

/// Build the tool result for a learner's answer. Choice questions are graded
/// here, by code; "I don't know" carries no confidence and is never wrong.
pub(crate) fn answer_result(
    name: &str,
    call_id: &str,
    args: &Value,
    payload: &Value,
) -> Result<Value, Error> {
    let answer = payload
        .get("answer")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if name == "choice_ask" {
        if answer.is_empty() {
            return Err(Error::Invalid("choose an option or write your own".into()));
        }
        return Ok(json!({ "answer": answer }));
    }
    let dont_know = payload
        .get("dontKnow")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !dont_know && answer.is_empty() {
        return Err(Error::Invalid(
            "write an answer, or choose \"I don't know\"".into(),
        ));
    }
    let thinking = payload
        .get("thinking")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let mut out = json!({
        "questionId": call_id,
        "answer": if dont_know { "" } else { answer },
        "thinking": thinking,
        "dontKnow": dont_know,
    });
    if let Some(t) = payload
        .get("thinkingWhileReading")
        .and_then(Value::as_str)
        .filter(|t| !t.trim().is_empty())
    {
        out["thinkingWhileReading"] = json!(t.trim());
    }
    if !dont_know {
        if let Some(c) = payload.get("confidence").and_then(Value::as_f64) {
            out["confidence"] = json!(c.clamp(0.0, 100.0).round() as u8);
        }
    }
    if args.get("format").and_then(Value::as_str) == Some("choice") && !dont_know {
        let committed = args.get("answer").and_then(Value::as_str).unwrap_or("");
        let verdict = if same_answer(answer, committed) {
            "correct"
        } else {
            "wrong"
        };
        out["verdict"] = json!(verdict);
    }
    Ok(out)
}

async fn file_tool(name: &str, args: &Value) -> Exec {
    let Some(root) = files::home_dir() else {
        return refuse(name, "no home directory on this device");
    };
    let args = args.clone();
    let tool = name.to_owned();
    let result = tokio::task::spawn_blocking(move || match tool.as_str() {
        "file_find" => match req_str(&args, "pattern") {
            Ok(p) => files::file_find(&root, p),
            Err(e) => Err(e),
        },
        _ => match req_str(&args, "path") {
            Ok(p) => files::file_read(&root, p, opt_str(&args, "lines")),
            Err(e) => Err(e),
        },
    })
    .await;
    match result {
        Ok(Ok(text)) => done(text),
        Ok(Err(e)) => refuse(name, e),
        Err(e) => refuse(name, format!("internal error ({e})")),
    }
}

/// `carryOver` of the latest accepted `section_done`.
fn carry_over(ops: &[Op]) -> String {
    let mut accepted: HashSet<&str> = HashSet::new();
    for op in ops.iter().rev() {
        if let Op::Tool(t) = op {
            if t.name != "section_done" {
                continue;
            }
            match &t.result {
                Some(r) if !r.is_error => {
                    accepted.insert(t.call_id.as_str());
                }
                None if accepted.contains(t.call_id.as_str()) => {
                    return opt_str(&t.args, "carryOver").unwrap_or("").to_owned();
                }
                _ => {}
            }
        }
    }
    String::new()
}

fn section_nodes(state: &SessionState, ids: &[String]) -> String {
    let nodes = state.plan.as_ref().map(|p| &p.nodes);
    ids.iter()
        .map(|id| {
            let node = nodes.and_then(|ns| ns.iter().find(|n| &n.id == id));
            let mark = if state.node_passed(id) { " ✓" } else { "" };
            match node {
                Some(n) => format!("`{}` {} ({}){mark}", n.id, n.title, n.kind.as_str()),
                None => format!("`{id}`{mark}"),
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn non_empty(s: &str, fallback: &str) -> String {
    if s.trim().is_empty() {
        fallback.to_owned()
    } else {
        s.to_owned()
    }
}

fn days_until(date: &str) -> Option<i64> {
    let d = chrono::NaiveDate::parse_from_str(date.get(..10)?, "%Y-%m-%d").ok()?;
    Some((d - chrono::Utc::now().date_naive()).num_days())
}

fn strings_in(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a.iter().flat_map(strings_in).collect(),
        Value::Object(o) => o.values().flat_map(strings_in).collect(),
        _ => Vec::new(),
    }
}

pub(crate) fn clip(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choice_answers_are_graded_by_code_and_dont_know_is_never_wrong() {
        let args = json!({"format":"choice","options":["3 V","6 V"],"answer":"6 V"});
        let r = answer_result(
            "question_ask",
            "q1",
            &args,
            &json!({"answer":" 6  v","thinking":"ratio","confidence":80}),
        )
        .unwrap();
        assert_eq!(r["verdict"], "correct");
        assert_eq!(r["questionId"], "q1");
        let r = answer_result(
            "question_ask",
            "q1",
            &args,
            &json!({"answer":"3 V","thinking":""}),
        )
        .unwrap();
        assert_eq!(r["verdict"], "wrong");
        let r = answer_result(
            "question_ask",
            "q1",
            &args,
            &json!({"dontKnow":true,"confidence":90}),
        )
        .unwrap();
        assert!(r.get("verdict").is_none() && r.get("confidence").is_none());
        assert!(answer_result("question_ask", "q1", &args, &json!({"answer":""})).is_err());
        let free = json!({"format":"free","answer":"x"});
        assert!(
            answer_result("question_ask", "q", &free, &json!({"answer":"y"}))
                .unwrap()
                .get("verdict")
                .is_none()
        );
    }

    #[test]
    fn question_gates_follow_the_phase() {
        let measure = SessionState::default();
        let probe = json!({"role":"probe","format":"free","prompt":"p","answer":"a"});
        assert!(matches!(gate_question(&measure, &probe), Exec::Pause));
        let check = json!({"role":"check","nodeId":"n","format":"free","prompt":"p","answer":"a"});
        assert!(matches!(
            gate_question(&measure, &check),
            Exec::Done { is_error: true, .. }
        ));
        let bad_choice =
            json!({"role":"probe","format":"choice","options":["a","b"],"prompt":"p","answer":"c"});
        assert!(matches!(
            gate_question(&measure, &bad_choice),
            Exec::Done { is_error: true, .. }
        ));
    }
}
