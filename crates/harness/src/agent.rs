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
use std::path::Path;
use std::sync::Arc;

use providers::{
    ChatRequest, Completion, Message, ProviderId, Role, StopReason, StreamEvent, ToolCall, ToolSpec,
};
use serde_json::{json, Value};
use tokio::sync::oneshot;

use crate::events::{AgentEvent, AgentName};
use crate::files;
use crate::plugins::PluginContext;
use crate::prompts::{render, Composer, TemplateValues};
use crate::replay::{self, replay_state};
use crate::sessions::{
    self, op_id, ApproveOp, DebriefOp, HuntReturnOp, HuntSendOp, HuntSkipOp, HuntToggleOp,
    KnownRefutedOp, Op, PhaseOp, PlanAnswerOp, PlanEditOp, PlanRequestOp, PluginGrantOp,
    RemovedPlugin, ToolOp, ToolResultValue,
};
use crate::state::{Level, Phase, PlanQuestion, Section, SessionState};
use crate::tools::{self, opt_str, req_str, MAX_PLUGINS_PER_SECTION};
use crate::workspace;
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
        let parsed = self.store.parsed_ops(session_id)?;
        let mode = parsed.ops.iter().find_map(|o| match o {
            Op::Session(h) => Some(h.mode),
            _ => None,
        });
        if mode != Some(Mode::Learn) {
            return Err(Error::Invalid("this is not a Learn session".into()));
        }
        // The same rule `list` and the UI use: only a session whose second op
        // is the initial `phase` op may be written to (sessions.rs).
        if !parsed.runtime {
            return Err(Error::Invalid(READ_ONLY.into()));
        }
        Ok(parsed.ops)
    }

    /// Derived state of a session (never stored).
    pub fn session_state(&self, session_id: &str) -> Result<SessionState, Error> {
        Ok(replay_state(&self.store.ops(session_id)?))
    }

    /// Register a native in-tree plugin (the trait stays for system-level
    /// tools; none ship in this build).
    pub fn register_native_plugin(
        &self,
        plugin: Arc<dyn crate::plugins::Plugin>,
    ) -> Result<(), Error> {
        let id = plugin.manifest().id.clone();
        let hash = format!("native-{id}");
        lock(&self.plugins).register(
            plugin,
            crate::plugins::PluginSource::Folder {
                name: "native".into(),
            },
            hash,
        );
        Ok(())
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
                            Some(providers::Credential::ClaudeCode(s)) => Some(s.access_token),
                            Some(providers::Credential::Codex(s)) => Some(s.access_token),
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

    /// Context window of the model behind `role`, from the cached catalog.
    pub(crate) fn context_window(&self, role: ModelRole) -> usize {
        let Ok(choice) = lock(&self.config).resolve(role) else {
            return DEFAULT_CONTEXT;
        };
        self.model_info(choice.provider, &choice.model)
            .and_then(|m| m.context_window)
            .map_or(DEFAULT_CONTEXT, |c| c as usize)
    }

    // ---- composition -------------------------------------------------------

    /// The tutor's system prompt and tool list for `phase` (normally the
    /// session's phase).
    pub(crate) fn compose_tutor(
        &self,
        session_id: &str,
        ops: &[Op],
        state: &SessionState,
        phase: Phase,
    ) -> Result<(String, Vec<ToolSpec>), Error> {
        let composer = Composer::new(&self.repo);
        let mut flags = HashSet::new();
        flags.insert("mermaid".to_owned());
        // Learning-system flags (docs/learning-system.md, "Feature flags"): each is on
        // when the data its blocks need can exist.
        flags.insert("levels".to_owned());
        flags.insert("starting_point".to_owned());
        flags.insert("plan_controls".to_owned());
        flags.insert("hunts".to_owned());
        // A hunt is live only when the planner marked it available *and* the
        // learner switched it on; replay turns a skip into off (§5).
        let hunt_active = state.hunt_active();
        if hunt_active {
            flags.insert("hunt_active".to_owned());
        }
        // A known section is greyed until a miss refutes it (§4), and only the
        // current section can be taught.
        if state
            .current_section()
            .is_some_and(|s| s.assumed_known && !state.known_refuted.contains(&s.id))
        {
            flags.insert("known_section".to_owned());
        }
        if !Composer::brief_venues(state.brief.as_ref()).is_empty() {
            flags.insert("field_guide".to_owned());
        }
        if state.calibration.is_some() {
            flags.insert("calibration".to_owned());
        }
        let workspace = lock(&self.workspaces).get(session_id).is_some();
        if workspace {
            flags.insert("workspace".to_owned());
        }
        let mut base = TemplateValues::new();
        base.set(
            "session_state",
            Composer::session_state_with(state, Some(&self.saved_starting_point())),
        );
        base.set(
            "workspace_name",
            state
                .workspace
                .clone()
                .unwrap_or_else(|| "workspace".into()),
        );
        let (fragments, plugin_tools) = {
            let registry = self.plugin_registry();
            (
                registry.fragments(&state.active_plugins, "tutor"),
                registry.tool_specs(&state.active_plugins, "tutor"),
            )
        };
        let mut values = TemplateValues::new();
        values.set("active_plugins", "");
        // The hunt fragment (§5): `teach.md` renders it inside
        // `{{#hunt_active}}`, so its own placeholder is resolved here and the
        // value is empty whenever the flag drops the block.
        values.set("hunt.state", state.hunt_state());
        values.set(
            "hunt",
            if hunt_active {
                let mut hunt_values = TemplateValues::new();
                hunt_values.set("hunt.state", state.hunt_state());
                render(
                    &composer.load("tutor/hunt.md")?,
                    &hunt_values,
                    &HashSet::new(),
                )?
            } else {
                String::new()
            },
        );
        set_section_level_values(&mut values, None);
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
                set_section_level_values(&mut values, section);
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
        let mut tools = tools::tutor_tools(phase, workspace);
        if phase == Phase::Teach {
            tools.extend(plugin_tools);
        }
        Ok((system, tools))
    }

    pub(crate) fn compose_planner(&self, state: &SessionState) -> Result<String, Error> {
        let composer = Composer::new(&self.repo);
        let mut flags = HashSet::new();
        flags.insert("levels".to_owned());
        flags.insert("starting_point".to_owned());
        flags.insert("plan_controls".to_owned());
        flags.insert("hunts".to_owned());
        // The planner is told a hunt can be offered; it is never told one is
        // live for teaching, but the keys exist so no prompt fails to render.
        let hunt_active = state.hunt_active();
        if hunt_active {
            flags.insert("hunt_active".to_owned());
        }
        if !Composer::brief_venues(state.brief.as_ref()).is_empty() {
            flags.insert("field_guide".to_owned());
        }
        if state.calibration.is_some() {
            flags.insert("calibration".to_owned());
        }
        let mut values = TemplateValues::new();
        values.set(
            "session_state",
            Composer::session_state_with(state, Some(&self.saved_starting_point())),
        );
        values.set("hunt.state", state.hunt_state());
        values.set(
            "hunt",
            if hunt_active {
                let mut hunt_values = TemplateValues::new();
                hunt_values.set("hunt.state", state.hunt_state());
                render(
                    &composer.load("tutor/hunt.md")?,
                    &hunt_values,
                    &HashSet::new(),
                )?
            } else {
                String::new()
            },
        );
        // The planner has no current section; the keys exist so a prompt may
        // mention them without failing to render.
        set_section_level_values(&mut values, None);
        let catalog = self.plugin_registry().available();
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
        composer.compose_planner(&values, exam, &flags)
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
            let (system, tools) = self.compose_tutor(session_id, &ops, &state, prompt_phase)?;
            let mut messages = replay::conversation_with(
                &ops,
                Some(&self.repo),
                self.role_vision(ModelRole::Tutor) != Some(false),
                true,
            );
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
            "question_ask" => match self.question_view_ok(&ops, &state, args) {
                Ok(()) => Ok(gate_question(&ops, &state, args)),
                Err(e) => Ok(refuse("question_ask", e)),
            },
            "choice_ask" => Ok(gate_choice(args)),
            "goal_set" => self.goal_set(session_id, &state, args),
            "verdict_record" => self.verdict_record(session_id, &ops, args),
            "probe_done" => self.probe_done(session_id, &ops, &state, args),
            "research_request" => self.research_request(session_id, args),
            "plan_change_request" => self.plan_change_request(session_id, args),
            "plugin_request" => self.plugin_request(session_id, &state, args),
            "section_done" => self.section_done(session_id, &state, args),
            "hunt_send" => self.hunt_send(session_id, &state, call, args),
            "debrief_record" => self.debrief_record(session_id, &ops, args),
            "frontier_park" => self.frontier_park(session_id, &state, args),
            "session_end" => {
                if state.phase != Phase::Close {
                    self.append(session_id, phase_op(Phase::Close, None))?;
                }
                // Once per session: the graph is built from the finished
                // transcript. It never delivers anything into the
                // conversation and never wakes the tutor.
                self.spawn_graph_on_close(session_id);
                // The transcript is complete: push it in the background (the
                // graph job's own ops go out with the next trigger).
                self.sync_in_background();
                Ok(done("Session ended."))
            }
            "file_find" | "file_read" => Ok(self.file_tool(session_id, &state, name, args).await),
            "workspace_write" => self.workspace_write(session_id, &state, args),
            _ => Ok(self.plugin_call(session_id, &state, name, args).await),
        }
    }

    fn goal_set(
        self: &Arc<Self>,
        session_id: &str,
        state: &SessionState,
        args: &Value,
    ) -> Result<Exec, Error> {
        let name = "goal_set";
        let goal = match req_str(args, "goal") {
            Ok(g) => g.to_owned(),
            Err(e) => return Ok(refuse(name, e)),
        };
        let purpose = match opt_str(args, "purpose") {
            Some(p @ ("curiosity" | "project" | "work" | "exam" | "course")) => p,
            _ => {
                return Ok(refuse(
                    name,
                    "`purpose` must be curiosity, project, work, exam or course",
                ))
            }
        };
        if let Some(depth) = opt_str(args, "depth") {
            if !matches!(depth, "pass" | "solid" | "deep") {
                return Ok(refuse(name, "`depth` must be pass, solid or deep"));
            }
        }
        if let Some(explore) = opt_str(args, "explore") {
            if !matches!(explore, "off" | "suggest" | "on") {
                return Ok(refuse(name, "`explore` must be off, suggest or on"));
            }
        }
        if let Some(point) = opt_str(args, "startingPoint") {
            if !matches!(point, "measure" | "quick" | "new") {
                return Ok(refuse(
                    name,
                    "`startingPoint` must be measure, quick or new",
                ));
            }
        }

        // Normalise the record so replay and the gates read one shape: the
        // depth default, explore off (always for course and exam goals), and
        // the learner's saved starting point when the model didn't name one.
        let settled = matches!(purpose, "course" | "exam");
        let explore = if settled {
            "off"
        } else {
            opt_str(args, "explore").unwrap_or("off")
        };
        let explore_forced = settled && opt_str(args, "explore").is_some_and(|e| e != "off");
        let depth = opt_str(args, "depth").unwrap_or("solid");
        let saved = lock(&self.config).settings.starting_point.clone();
        let point = opt_str(args, "startingPoint").unwrap_or(match saved.as_str() {
            p @ ("measure" | "quick" | "new") => p,
            _ => "measure",
        });
        let mut record = args.clone();
        if let Some(obj) = record.as_object_mut() {
            obj.insert("depth".into(), json!(depth));
            obj.insert("explore".into(), json!(explore));
            obj.insert("startingPoint".into(), json!(point));
        }
        let note = if explore_forced {
            "\nNote: explore stays off for course and exam goals."
        } else {
            ""
        };

        let first = state.goal.is_none();
        self.append(session_id, Op::Goal(sessions::record(record.clone())))?;
        if first {
            let job_args = json!({
                "kind": "scope",
                "question": goal,
                "context": record.to_string(),
            });
            self.spawn_job(session_id, "research", job_args, &format!("scope: {goal}"))?;
            if state.title.is_none() {
                self.spawn_title(session_id, record);
            }
            Ok(done(format!(
                "Goal recorded. The scope research is running in the background; its brief will arrive as a message. Start probing now.{note}"
            )))
        } else {
            Ok(done(format!("Goal updated.{note}")))
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
        // A known section's check was missed: code records it here (§4). The
        // section un-greys and falls back to the normal per-node gate; the
        // tutor teaches it on the spot, so there is no planner job and no
        // phase change.
        self.refute_known_section(session_id, ops, qid, &question, args)?;
        Ok(done("Verdict recorded."))
    }

    /// Append `knownRefuted` when a missed check lands on the current,
    /// assumed-known, not-yet-refuted section (§4). Only a `check` question
    /// counts: a prediction is not the section's exit evidence.
    fn refute_known_section(
        &self,
        session_id: &str,
        ops: &[Op],
        question_id: &str,
        question: &Value,
        args: &Value,
    ) -> Result<(), Error> {
        if opt_str(args, "verdict") == Some("correct") {
            return Ok(());
        }
        if question.get("role").and_then(Value::as_str) != Some("check") {
            return Ok(());
        }
        let state = replay_state(ops);
        let Some(section) = state.current_section() else {
            return Ok(());
        };
        if !section.assumed_known || state.known_refuted.contains(&section.id) {
            return Ok(());
        }
        let node = question.get("nodeId").and_then(Value::as_str).unwrap_or("");
        if !section.node_ids.iter().any(|n| n == node) {
            return Ok(());
        }
        self.append(
            session_id,
            Op::KnownRefuted(KnownRefutedOp {
                op_id: op_id(),
                at: now_iso(),
                section_id: section.id.clone(),
                question_id: question_id.to_owned(),
            }),
        )
    }

    fn probe_done(
        self: &Arc<Self>,
        session_id: &str,
        ops: &[Op],
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
        let Some(strands) = args.get("strands").and_then(Value::as_array) else {
            return Ok(refuse(name, "`strands` must be an array"));
        };
        // Starting point decides how many questions the probe could have
        // asked (§3); the strand records must match it.
        let starting_point = state.starting_point();
        let asked = probe_questions(ops);
        if starting_point == "quick" && asked > QUICK_PROBE_QUESTIONS {
            return Ok(refuse(name, "quick check: at most five probe questions"));
        }
        if starting_point == "new" && asked > 0 {
            return Ok(refuse(
                name,
                "the learner starts new: no probe questions were asked, so there is nothing to bracket",
            ));
        }
        for (i, strand) in strands.iter().enumerate() {
            let label = strand
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("strand {}", i + 1));
            let confidence = strand.get("confidence").and_then(Value::as_str);
            let reason = strand.get("reason").and_then(Value::as_str);
            if let Some(reason) = reason {
                if !matches!(reason, "new" | "skipped") {
                    return Ok(refuse(
                        name,
                        format!("strand `{label}`: `reason` must be new or skipped"),
                    ));
                }
                if confidence != Some("unprobed") {
                    return Ok(refuse(
                        name,
                        format!(
                            "strand `{label}`: `reason` only goes with `confidence: \"unprobed\"`"
                        ),
                    ));
                }
            }
            if starting_point == "new" && (confidence != Some("unprobed") || reason != Some("new"))
            {
                return Ok(refuse(
                    name,
                    format!(
                        "the learner starts new: every strand needs `confidence: \"unprobed\"` and `reason: \"new\"`; strand `{label}` doesn't"
                    ),
                ));
            }
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
        if !self.plugin_registry().grantable(plugin) {
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
        // A known section closes on one passing check at its target level,
        // until a miss refutes it (§4) — then every node needs one again.
        if let Err(why) = close_problems(state, &section) {
            return Ok(refuse(name, why));
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

    // ---- hunts (§5) ---------------------------------------------------------

    /// The tutor's `hunt_send` (§5): the learner goes and finds things out
    /// before this section is taught. Refused unless the planner marked the
    /// section's hunt available, the learner switched it on (replay carries
    /// their toggle), it wasn't skipped, and nothing has been sent yet.
    fn hunt_send(
        &self,
        session_id: &str,
        state: &SessionState,
        call: &ToolCall,
        args: &Value,
    ) -> Result<Exec, Error> {
        let name = "hunt_send";
        let Some(section) = state.current_section() else {
            return Ok(refuse(name, "no section is being taught"));
        };
        if opt_str(args, "sectionId") != Some(section.id.as_str()) {
            return Ok(refuse(
                name,
                format!("the current section is `{}`", section.id),
            ));
        }
        let Some(hunt) = &section.hunt else {
            return Ok(refuse(
                name,
                format!(
                    "section `{}` has no hunt: the planner didn't mark one available",
                    section.id
                ),
            ));
        };
        if !hunt.available {
            return Ok(refuse(
                name,
                format!("section `{}` has no hunt available", section.id),
            ));
        }
        if state
            .hunt
            .as_ref()
            .is_some_and(|h| h.state == "skipped" && h.section_id == section.id)
        {
            return Ok(refuse(
                name,
                "the learner skipped this section's hunt: teach the section normally",
            ));
        }
        if !hunt.enabled {
            return Ok(refuse(
                name,
                "only the learner can enable a hunt: this section's hunt isn't switched on",
            ));
        }
        if state
            .hunt
            .as_ref()
            .is_some_and(|h| h.section_id == section.id)
        {
            let hunt_id = state
                .hunt
                .as_ref()
                .map(|h| h.hunt_id.as_str())
                .unwrap_or("");
            return Ok(refuse(
                name,
                format!(
                    "this section's hunt is already sent (`{hunt_id}`); the debrief is the next step, not a second hunt"
                ),
            ));
        }
        let question = match req_str(args, "question") {
            Ok(q) => q.trim().to_owned(),
            Err(e) => return Ok(refuse(name, e)),
        };
        let Some(look_at) = args.get("lookAt").and_then(Value::as_array) else {
            return Ok(refuse(name, "`lookAt` must list where to look"));
        };
        if look_at.is_empty() {
            return Ok(refuse(name, "`lookAt` must list at least one place"));
        }
        for (i, entry) in look_at.iter().enumerate() {
            let kind = entry.get("kind").and_then(Value::as_str).unwrap_or("");
            if !HUNT_KINDS.contains(&kind) {
                return Ok(refuse(
                    name,
                    format!(
                        "lookAt[{i}].kind `{kind}` must be one of {}",
                        HUNT_KINDS.join(", ")
                    ),
                ));
            }
            if opt_str(entry, "why").is_none() {
                return Ok(refuse(
                    name,
                    format!("lookAt[{i}] needs a `why`: one line on what to look for there"),
                ));
            }
        }
        let bring_back: Vec<String> = args
            .get("bringBack")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        if bring_back.is_empty() {
            return Ok(refuse(name, "`bringBack` must say what to come back with"));
        }
        self.append(
            session_id,
            Op::HuntSend(HuntSendOp {
                op_id: op_id(),
                at: now_iso(),
                hunt_id: call.id.clone(),
                section_id: section.id.clone(),
                question,
                look_at: Value::Array(look_at.clone()),
                bring_back,
            }),
        )?;
        Ok(done(
            "Hunt sent: the learner has the card. End your turn; they come back whenever they like, and you debrief what they found before teaching.",
        ))
    }

    /// The debrief (§5): every claim sorted. Refused until the learner has
    /// returned from that hunt, and refused a second time for the same hunt.
    fn debrief_record(&self, session_id: &str, ops: &[Op], args: &Value) -> Result<Exec, Error> {
        let name = "debrief_record";
        let hunt_id = match req_str(args, "huntId") {
            Ok(h) => h.to_owned(),
            Err(e) => return Ok(refuse(name, e)),
        };
        let sent = ops
            .iter()
            .any(|o| matches!(o, Op::HuntSend(h) if h.hunt_id == hunt_id));
        if !sent {
            return Ok(refuse(name, format!("no hunt with huntId `{hunt_id}`")));
        }
        let returned = ops
            .iter()
            .any(|o| matches!(o, Op::HuntReturn(r) if r.hunt_id == hunt_id));
        if !returned {
            let skipped = ops
                .iter()
                .any(|o| matches!(o, Op::HuntSkip(k) if k.hunt_id == hunt_id));
            if skipped {
                return Ok(refuse(
                    name,
                    "the learner skipped this hunt; there is nothing to debrief, teach the section normally",
                ));
            }
            return Ok(refuse(
                name,
                format!(
                    "hunt `{hunt_id}` hasn't come back yet: debrief what the learner brought back, once they return"
                ),
            ));
        }
        if ops
            .iter()
            .any(|o| matches!(o, Op::Debrief(d) if d.hunt_id == hunt_id))
        {
            return Ok(refuse(name, "that hunt is already debriefed"));
        }
        let Some(claims) = args.get("claims").and_then(Value::as_array) else {
            return Ok(refuse(
                name,
                "`claims` must list what the learner brought back",
            ));
        };
        if claims.is_empty() {
            return Ok(refuse(name, "`claims` must list at least one claim"));
        }
        for (i, claim) in claims.iter().enumerate() {
            if opt_str(claim, "claim").is_none() {
                return Ok(refuse(name, format!("claims[{i}] needs `claim`")));
            }
            let verdict = claim.get("verdict").and_then(Value::as_str).unwrap_or("");
            if !DEBRIEF_VERDICTS.contains(&verdict) {
                return Ok(refuse(
                    name,
                    format!(
                        "claims[{i}].verdict `{verdict}` must be one of {}",
                        DEBRIEF_VERDICTS.join(", ")
                    ),
                ));
            }
        }
        let count = claims.len();
        self.append(
            session_id,
            Op::Debrief(DebriefOp {
                op_id: op_id(),
                at: now_iso(),
                hunt_id,
                claims: Value::Array(claims.clone()),
            }),
        )?;
        Ok(done(format!(
            "Debrief recorded ({count} claims). Teaching this section can start; the hunt's findings are reference points, never evidence."
        )))
    }

    /// `frontier_park` (§5): an off-route topic goes to `frontier.jsonl`, not
    /// to the session's plan, so the learner can start a session from it.
    fn frontier_park(
        &self,
        session_id: &str,
        state: &SessionState,
        args: &Value,
    ) -> Result<Exec, Error> {
        let name = "frontier_park";
        let topic = match req_str(args, "topic") {
            Ok(t) => t.trim().to_owned(),
            Err(e) => return Ok(refuse(name, e)),
        };
        let why = opt_str(args, "why").unwrap_or("").trim().to_owned();
        let hunt_id = state
            .hunt
            .as_ref()
            .map(|h| h.hunt_id.as_str())
            .filter(|id| !id.is_empty());
        match crate::frontier::append(&self.repo, session_id, hunt_id, &topic, &why) {
            Ok(_) => Ok(done(format!(
                "Parked on the frontier: {topic}. The learner can start a session from it later."
            ))),
            Err(e) => Ok(refuse(
                name,
                format!("could not save the frontier item: {e}"),
            )),
        }
    }

    /// Answer-mode gate (W7): the referenced artifact must exist, belong to an
    /// installed, enabled plugin that is active in the current section and
    /// whose view declares `answer = true`, and carry a `target` that stays
    /// in the call op.
    fn question_view_ok(
        &self,
        ops: &[Op],
        state: &SessionState,
        args: &Value,
    ) -> Result<(), String> {
        let Some(view) = args.get("view") else {
            return Ok(());
        };
        let Some(view) = view.as_object() else {
            return Err("`view` must be an object".into());
        };
        let Some(artifact_id) = view.get("artifactId").and_then(Value::as_str) else {
            return Err("view.artifactId is required".into());
        };
        if view
            .get("task")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            return Err("view.task is required".into());
        }
        if !view.get("target").is_some_and(Value::is_object) {
            return Err("view.target must be an object".into());
        }
        let artifact = ops.iter().find_map(|o| match o {
            Op::Artifact(a) if a.artifact_id == artifact_id => Some(a),
            _ => None,
        });
        let Some(artifact) = artifact else {
            return Err(format!("no artifact `{artifact_id}` in this session"));
        };
        let registry = self.plugin_registry();
        let Some(plugin) = registry.by_id(&artifact.plugin_id) else {
            return Err(format!(
                "the plugin that made `{artifact_id}` isn't installed"
            ));
        };
        if !registry.is_enabled(&artifact.plugin_id) {
            return Err(format!(
                "the plugin that made `{artifact_id}` is disabled; ask without the view"
            ));
        }
        if !state.active_plugins.contains(&artifact.plugin_id) {
            return Err(format!(
                "the plugin that made `{artifact_id}` isn't active in this section; ask without the view"
            ));
        }
        if !plugin.manifest().view.first().is_some_and(|v| v.answer) {
            return Err("that artifact's view can't be an answer surface".into());
        }
        Ok(())
    }

    /// Grade a view submission through the plugin's `grade(answer, target)`.
    /// The plugin never records a verdict; the tutor still calls
    /// `verdict_record` with the `questionId`.
    fn grade_view_answer(
        &self,
        session_id: &str,
        question_id: &str,
        view: &Value,
        payload: &Value,
    ) -> Result<Value, Error> {
        let artifact_id = view
            .get("artifactId")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Invalid("the question has no artifact".into()))?;
        let ops = self.store.ops(session_id)?;
        let artifact = ops
            .iter()
            .find_map(|o| match o {
                Op::Artifact(a) if a.artifact_id == artifact_id => Some(a.clone()),
                _ => None,
            })
            .ok_or_else(|| Error::Invalid("that artifact no longer exists".into()))?;
        let plugin = match self.enabled_plugin(&artifact.plugin_id) {
            Ok(plugin) => plugin,
            Err(Error::NotFound(_)) => {
                return Err(Error::Invalid(
                    "the plugin that made this view isn't installed".into(),
                ))
            }
            Err(_) => {
                return Err(Error::Invalid(
                    "the plugin that made this view is disabled; enable it in Settings → Plugins to submit this answer".into(),
                ))
            }
        };
        let answer = payload.get("answer").cloned().unwrap_or(Value::Null);
        let target = view.get("target").cloned().unwrap_or(Value::Null);
        let grade = plugin
            .grade(&answer, &target)
            .map_err(|e| Error::Invalid(format!("the plugin couldn't grade the answer: {e}")))?;
        Ok(json!({
            "questionId": question_id,
            "artifactId": artifact_id,
            "answer": answer,
            "view": grade,
        }))
    }

    /// Read roots for this session: home, the attached workspace, and every
    /// path a learner attachment granted (W3/W4).
    fn file_roots(&self, session_id: &str, state: &SessionState) -> Vec<std::path::PathBuf> {
        let mut roots = Vec::new();
        if let Some(home) = files::home_dir() {
            roots.push(home);
        }
        if let Some(ws) = lock(&self.workspaces).get(session_id) {
            roots.push(ws);
        }
        for path in &state.file_grants {
            roots.push(std::path::PathBuf::from(path));
        }
        roots
    }

    async fn file_tool(
        &self,
        session_id: &str,
        state: &SessionState,
        name: &str,
        args: &Value,
    ) -> Exec {
        let roots = self.file_roots(session_id, state);
        if roots.is_empty() {
            return refuse(name, "no home directory on this device");
        }
        let args = args.clone();
        let tool = name.to_owned();
        let workspace = lock(&self.workspaces).get(session_id);
        let result = tokio::task::spawn_blocking(move || match tool.as_str() {
            "file_find" => match req_str(&args, "pattern") {
                Ok(p) => {
                    files::file_find_roots(&roots, workspace.as_deref(), p).map(|text| (text, None))
                }
                Err(e) => Err(e),
            },
            _ => match req_str(&args, "path") {
                Ok(p) => {
                    let lines = opt_str(&args, "lines");
                    // A relative path names a workspace file first (the
                    // paths workspace_write takes), then one under home.
                    let relative = !p.trim().starts_with('~') && !Path::new(p.trim()).is_absolute();
                    let in_workspace = workspace.as_deref().filter(|ws| {
                        relative && workspace::resolve(ws, p).is_ok_and(|f| f.is_file())
                    });
                    let (out, full) = match in_workspace {
                        Some(ws) => (files::file_read(ws, p, lines)?, files::resolve(ws, p)),
                        None => (
                            files::file_read_roots(&roots, p, lines)?,
                            files::resolve_roots(&roots, p),
                        ),
                    };
                    // Remember the hash of the workspace file the agent
                    // actually saw, keyed by its workspace-relative path, so
                    // workspace_write can tell learner edits from its own.
                    let seen = workspace.as_deref().zip(full.ok()).and_then(|(ws, full)| {
                        Some((
                            workspace::relative(ws, &full)?,
                            workspace::file_hash(&full)?,
                        ))
                    });
                    Ok((out, seen))
                }
                Err(e) => Err(e),
            },
        })
        .await;
        match result {
            Ok(Ok((text, seen))) => {
                if let Some((path, hash)) = seen {
                    lock(&self.workspace_hashes).insert((session_id.to_owned(), path), hash);
                }
                done(text)
            }
            Ok(Err(e)) => refuse(name, e),
            Err(e) => refuse(name, format!("internal error ({e})")),
        }
    }

    fn workspace_write(
        &self,
        session_id: &str,
        state: &SessionState,
        args: &Value,
    ) -> Result<Exec, Error> {
        let name = "workspace_write";
        let Some(root) = lock(&self.workspaces).get(session_id) else {
            return Ok(refuse(name, "no workspace is attached to this session"));
        };
        let path = match req_str(args, "path") {
            Ok(p) => p,
            Err(e) => return Ok(refuse(name, e)),
        };
        let content = match req_str(args, "content") {
            Ok(c) => c,
            Err(e) => return Ok(refuse(name, e)),
        };
        // Same key as file_read records: the workspace-relative path.
        let rel = match workspace::resolve(&root, path) {
            Ok(full) => workspace::relative(&root, &full).unwrap_or_else(|| path.to_owned()),
            Err(e) => return Ok(refuse(name, e)),
        };
        let key = (session_id.to_owned(), rel.clone());
        let expected = lock(&self.workspace_hashes)
            .get(&key)
            .cloned()
            .or_else(|| state.workspace_writes.get(&rel).cloned());
        let result = workspace::write(&root, path, content, expected.as_deref());
        match result {
            Err(e) => Ok(refuse(name, e)),
            Ok((_, hash, size)) => {
                lock(&self.workspace_hashes).insert(key, hash.clone());
                self.append(
                    session_id,
                    Op::WorkspaceWrite(sessions::WorkspaceWriteOp {
                        op_id: op_id(),
                        at: now_iso(),
                        path: rel,
                        sha256: hash,
                        size,
                    }),
                )?;
                Ok(done(format!(
                    "Wrote {path} ({size} bytes) in the workspace. The learner may edit it in their own editor; re-read before relying on it."
                )))
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
        let (owner, manifest, package_hash) = {
            let registry = self.plugin_registry();
            let Some(plugin) = registry.owner(&state.active_plugins, name) else {
                return refuse(name, "unknown tool");
            };
            let manifest = plugin.manifest().clone();
            let hash = registry.package_hash(&manifest.id).unwrap_or_default();
            (plugin, manifest, hash)
        };
        let fut = owner.call(
            name,
            args,
            PluginContext {
                session_id: session_id.to_owned(),
                platform: state.platform.clone(),
            },
        );
        let out = fut.await;
        if out.is_error {
            return Exec::Done {
                content: out.content,
                is_error: true,
            };
        }
        let Some(artifact) = out.artifact else {
            return Exec::Done {
                content: out.content,
                is_error: false,
            };
        };
        let id = ulid::Ulid::new().to_string();
        let svg = match crate::artifacts::sanitize_svg(&artifact.svg) {
            Ok(svg) => svg,
            Err(e) => return refuse(name, e),
        };
        let op = sessions::ArtifactOp {
            op_id: op_id(),
            at: now_iso(),
            artifact_id: id.clone(),
            plugin_id: manifest.id,
            plugin_version: manifest.version,
            package_hash,
            tool: name.to_owned(),
            input: args.clone(),
            data: artifact.data,
            fallback_svg: svg,
        };
        if let Err(e) = self.append(session_id, Op::Artifact(op)) {
            return refuse(name, format!("couldn't store the artifact ({e})"));
        }
        let mut content = out.content;
        if !content.is_empty() {
            content.push_str("\n\n");
        }
        content.push_str(&crate::artifacts::placement_line(&id));
        Exec::Done {
            content,
            is_error: false,
        }
    }

    // ---- learner commands --------------------------------------------------

    /// Create a Learn session from the learner's opening message and run the
    /// first tutor turn.
    pub async fn start_teaching(self: &Arc<Self>, text: &str) -> Result<SessionSummary, Error> {
        self.start_teaching_with(text, &[]).await
    }

    /// As [`start_teaching`], with the attachments chosen on Home (W3).
    pub async fn start_teaching_with(
        self: &Arc<Self>,
        text: &str,
        attachments: &[crate::sessions::Attachment],
    ) -> Result<SessionSummary, Error> {
        let text = text.trim();
        if text.is_empty() {
            return Err(Error::Invalid("message is empty".into()));
        }
        self.check_vision(attachments)?;
        lock(&self.config).resolve(ModelRole::Tutor)?;
        let summary = self.store.create(Mode::Learn, text)?;
        self.emit(
            &summary.id,
            AgentEvent::Session {
                session: summary.clone(),
            },
        );
        self.send_learner_message_with(&summary.id, text, attachments)
            .await?;
        self.store.summary(&summary.id)
    }

    /// The learner wrote a message (not an answer to a card).
    pub async fn send_learner_message(
        self: &Arc<Self>,
        session_id: &str,
        text: &str,
    ) -> Result<(), Error> {
        self.send_learner_message_with(session_id, text, &[]).await
    }

    pub async fn send_learner_message_with(
        self: &Arc<Self>,
        session_id: &str,
        text: &str,
        attachments: &[crate::sessions::Attachment],
    ) -> Result<(), Error> {
        let text = text.trim();
        if text.is_empty() {
            return Err(Error::Invalid("message is empty".into()));
        }
        self.check_vision(attachments)?;
        let guard = self.turn_lock(session_id);
        let _turn = guard.lock().await;
        let ops = self.learn_ops(session_id)?;
        let state = replay_state(&ops);
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
        self.commit_attachments(attachments)?;
        let held = lock(&self.view_states).remove(session_id);
        for (artifact_id, summary) in held.into_iter().flatten() {
            self.append(
                session_id,
                Op::ViewState(sessions::ViewStateOp {
                    op_id: sessions::op_id(),
                    at: crate::now_iso(),
                    artifact_id,
                    summary,
                }),
            )?;
        }
        let mut message = sessions::new_message(Role::User, text.to_owned());
        message.attachments = attachments.to_vec();
        self.append(session_id, Op::Message(message))?;
        if !attachments.is_empty() {
            let grants = crate::attachments::grants_for(attachments);
            crate::attachments::grant_roots(self, session_id, &grants)?;
        }
        self.tutor_turn(session_id).await
    }

    /// Image attachments are refused up front when the Tutor model is known
    /// to lack vision; the composer asks first (W2/W3).
    fn check_vision(&self, attachments: &[crate::sessions::Attachment]) -> Result<(), Error> {
        if attachments.iter().any(|a| a.kind == "image")
            && self.role_vision(ModelRole::Tutor) == Some(false)
        {
            return Err(Error::Invalid(
                "this model can't see images; send without them or choose another Tutor model"
                    .into(),
            ));
        }
        Ok(())
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
        let content = match pending.args.get("view") {
            Some(view) => self.grade_view_answer(session_id, call_id, view, &payload)?,
            None => answer_result(&pending.name, call_id, &pending.args, &payload)?,
        };
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
        let mut dropped: Vec<String> = Vec::new();
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
                if !self.plugin_registry().grantable(p) {
                    dropped.push(format!("{} ({})", s.title, p));
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
        if !dropped.is_empty() {
            let mut m = sessions::new_message(
                Role::User,
                format!(
                    "[plan update] Left out until installed: {}. The section starts without it; the plan card offers Install.",
                    dropped.join(", ")
                ),
            );
            m.tag = Some("plan-notice".into());
            self.append(session_id, Op::Message(m))?;
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

    /// The learner sent the plan card's batched edits (§4): one `planEdit` op
    /// and one planner job whose request spells out every edited field, the
    /// learner's comment and the rules for applying them. No op is rewritten;
    /// the planner revises the plan through the ordinary revision path.
    pub async fn edit_plan(
        self: &Arc<Self>,
        session_id: &str,
        edits: Value,
        comment: Option<&str>,
    ) -> Result<(), Error> {
        let Some(fields) = edits.as_object() else {
            return Err(Error::Invalid(
                "the plan edits must be one JSON object".into(),
            ));
        };
        if fields.is_empty() {
            return Err(Error::Invalid("the plan edits are empty".into()));
        }
        let comment = comment.map(str::trim).filter(|c| !c.is_empty());
        let state = replay_state(&self.learn_ops(session_id)?);
        if state.plan.is_none() {
            return Err(Error::Invalid("there is no plan to edit yet".into()));
        }
        if state.open_jobs.iter().any(|j| j.kind == "plan") {
            return Err(Error::Invalid(
                "the planner is already working; wait for its answer".into(),
            ));
        }
        self.append(
            session_id,
            Op::PlanEdit(PlanEditOp {
                op_id: op_id(),
                at: now_iso(),
                edits: edits.clone(),
                comment: comment.map(str::to_owned),
            }),
        )?;
        let request = plan_edit_request(&edits, comment);
        self.spawn_job(
            session_id,
            "plan",
            json!({ "request": request }),
            "Plan changes requested",
        )?;
        Ok(())
    }

    /// The learner answered the planner's `plan_ask` (§4): one `planAnswer`
    /// op, then a fresh planner run carrying the question, its options, the
    /// answer and the edit request it was asked about. The planner always
    /// reads the whole session, so no resume state is stored.
    pub async fn answer_plan_question(
        self: &Arc<Self>,
        session_id: &str,
        answer: &str,
    ) -> Result<(), Error> {
        let answer = answer.trim();
        if answer.is_empty() {
            return Err(Error::Invalid("write an answer".into()));
        }
        let ops = self.learn_ops(session_id)?;
        let state = replay_state(&ops);
        let Some(question) = state.pending_plan_question.clone() else {
            return Err(Error::Invalid(
                "the planner isn't waiting for an answer".into(),
            ));
        };
        self.append(
            session_id,
            Op::PlanAnswer(PlanAnswerOp {
                op_id: op_id(),
                at: now_iso(),
                call_id: question.call_id.clone(),
                answer: answer.to_owned(),
            }),
        )?;
        let request = plan_answer_request(&question, answer, latest_plan_edit(&ops).as_ref());
        self.spawn_job(
            session_id,
            "plan",
            json!({ "request": request }),
            "Plan question answered",
        )?;
        Ok(())
    }

    /// The learner came back from a hunt (§5): one `huntReturn` op carrying
    /// whatever they brought (notes, links and files, none required), the
    /// return itself as a learner-side message, then a tutor turn so the
    /// debrief happens now. The turn lock is the same one job delivery takes,
    /// so a return never lands inside a running turn; a pending question
    /// defers the wake, and the message is read by the next turn.
    pub async fn hunt_return(
        self: &Arc<Self>,
        session_id: &str,
        hunt_id: &str,
        payload: Value,
    ) -> Result<(), Error> {
        let guard = self.turn_lock(session_id);
        let _turn = guard.lock().await;
        let ops = self.learn_ops(session_id)?;
        let Some(send) = ops.iter().find_map(|o| match o {
            Op::HuntSend(h) if h.hunt_id == hunt_id => Some(h.clone()),
            _ => None,
        }) else {
            return Err(Error::NotFound(format!("hunt `{hunt_id}`")));
        };
        if ops.iter().any(|o| match o {
            Op::HuntReturn(r) => r.hunt_id == hunt_id,
            Op::HuntSkip(k) => k.hunt_id == hunt_id,
            _ => false,
        }) {
            return Err(Error::Invalid(format!(
                "hunt `{hunt_id}` is already finished"
            )));
        }
        let notes = payload
            .get("notes")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_owned();
        let links = string_list(payload.get("links"));
        let files = string_list(payload.get("files"));
        self.append(
            session_id,
            Op::HuntReturn(HuntReturnOp {
                op_id: op_id(),
                at: now_iso(),
                hunt_id: hunt_id.to_owned(),
                notes: notes.clone(),
                links: links.clone(),
                files: files.clone(),
            }),
        )?;
        let mut m =
            sessions::new_message(Role::User, hunt_return_text(&send, &notes, &links, &files));
        m.tag = Some("hunt-return".into());
        self.append(session_id, Op::Message(m))?;
        let state = replay_state(&self.store.ops(session_id)?);
        if state.phase == Phase::Teach && state.pending_call.is_none() && !state.ended {
            self.tutor_turn(session_id).await?;
        }
        Ok(())
    }

    /// The learner skipped a hunt (§5): one `huntSkip` op, then the same wake
    /// as a return, so teaching starts normally right away. A skip also
    /// switches the section's hunt off in replay, whatever the toggle said.
    pub async fn hunt_skip(self: &Arc<Self>, session_id: &str, hunt_id: &str) -> Result<(), Error> {
        let guard = self.turn_lock(session_id);
        let _turn = guard.lock().await;
        let ops = self.learn_ops(session_id)?;
        let Some(send) = ops.iter().find_map(|o| match o {
            Op::HuntSend(h) if h.hunt_id == hunt_id => Some(h.clone()),
            _ => None,
        }) else {
            return Err(Error::NotFound(format!("hunt `{hunt_id}`")));
        };
        if ops.iter().any(|o| match o {
            Op::HuntSkip(k) => k.hunt_id == hunt_id,
            Op::HuntReturn(r) => r.hunt_id == hunt_id,
            _ => false,
        }) {
            return Err(Error::Invalid(format!(
                "hunt `{hunt_id}` is already finished"
            )));
        }
        self.append(
            session_id,
            Op::HuntSkip(HuntSkipOp {
                op_id: op_id(),
                at: now_iso(),
                hunt_id: hunt_id.to_owned(),
            }),
        )?;
        let mut m = sessions::new_message(
            Role::User,
            format!(
                "[hunt skip] The learner skipped the hunt `{hunt_id}` (section `{}`), question: {}. Teach the section normally; there is nothing to debrief.",
                send.section_id, send.question
            ),
        );
        m.tag = Some("hunt-skip".into());
        self.append(session_id, Op::Message(m))?;
        let state = replay_state(&self.store.ops(session_id)?);
        if state.phase == Phase::Teach && state.pending_call.is_none() && !state.ended {
            self.tutor_turn(session_id).await?;
        }
        Ok(())
    }

    /// The learner switched a section's hunt on or off (§5): one `huntToggle`
    /// op and no turn. Replay applies it onto the plan's section, so gates and
    /// prompts read one value from the next compose on.
    pub fn set_hunt(&self, session_id: &str, section_id: &str, enabled: bool) -> Result<(), Error> {
        let state = replay_state(&self.learn_ops(session_id)?);
        let Some(plan) = &state.plan else {
            return Err(Error::Invalid("there is no plan yet".into()));
        };
        let Some(section) = plan.sections.iter().find(|s| s.id == section_id) else {
            return Err(Error::NotFound(format!("section `{section_id}`")));
        };
        if !section.hunt.as_ref().is_some_and(|h| h.available) {
            return Err(Error::Invalid(format!(
                "section `{section_id}` has no hunt the planner marked available"
            )));
        }
        // A skip is the learner's last word on that hunt (§5): the toggle
        // can't bring a skipped hunt back.
        if enabled
            && state
                .hunt
                .as_ref()
                .is_some_and(|h| h.section_id == section_id && h.state == "skipped")
        {
            return Err(Error::Invalid(
                "that hunt was skipped; it can't be switched back on".into(),
            ));
        }
        self.append(
            session_id,
            Op::HuntToggle(HuntToggleOp {
                op_id: op_id(),
                at: now_iso(),
                section_id: section_id.to_owned(),
                enabled,
            }),
        )
    }

    /// The frontier list (§5): every topic parked by a hunt or debrief, in
    /// the order it was parked. A corrupt line is skipped, never an error.
    pub fn frontier(&self) -> Result<Vec<crate::FrontierItem>, Error> {
        Ok(crate::frontier::items(&self.repo))
    }

    /// Start a Learn session from a frontier item (§5): the item's topic and
    /// why are the learner's opening words, and teaching starts as usual.
    pub async fn start_from_frontier(
        self: &Arc<Self>,
        item_id: &str,
    ) -> Result<SessionSummary, Error> {
        let item = crate::frontier::items(&self.repo)
            .into_iter()
            .find(|i| i.id == item_id)
            .ok_or_else(|| Error::NotFound(format!("frontier item `{item_id}`")))?;
        let mut text = item.topic.trim().to_owned();
        if !item.why.trim().is_empty() {
            text.push_str(&format!("\n\nWhy it's on my list: {}", item.why.trim()));
        }
        self.start_teaching(&text).await
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

/// What the planner must do with a plan-card diff (§4, "How the planner
/// handles edits"). The same rules are in `planner/base.md` with the
/// `plan_controls` flag; the request repeats them so the job is
/// self-contained.
const PLAN_EDIT_RULES: &str = "\
How to apply these edits (docs/learning-system.md §4):
- \"Don't need this\" (`keep: false`): remove the section. If a later section depends on it, either fold the minimum it needs into that section's notes, or keep it and explain in `approach`, in one sentence, why it is needed. If the learner sends the same removal again, remove it and fold what you must.
- \"Parts I already know…\" (`knownNote`): split the section in two — a known section (`assumedKnown: true`, `knownNote` = the learner's words) holding the nodes they say they know, plus the rest taught normally. If the comment covers the whole section, mark the whole section known. Don't argue with the claim: the tutor verifies it with one check.
- How far (`targetLevel`): set the section's `targetLevel` and rewrite its `exit` at that level. If it conflicts with the goal, warn in one sentence in `approach` but apply their choice.
- Depth, style and sitting length: re-derive the defaults only for sections the learner has not set explicitly; never override an explicit section setting.
- Explore: apply it; if it is now `off`, remove every hunt.
- Unclear or conflicting: ask one question on the card with plan_ask, then end your turn instead of calling plan_write.
- Re-approval rules are unchanged: removing a section or changing the goal needs the learner's approval; other changes apply with a notice.";

/// The planner request for a plan-card diff: every edited field by name, the
/// learner's comment, the raw JSON (so nothing is lost) and the rules.
fn plan_edit_request(edits: &Value, comment: Option<&str>) -> String {
    format!(
        "Plan edits from the learner (plan card). One structured diff; apply it with plan_write, or ask one question with plan_ask and end your turn.\n\n{}\n{}Edits as JSON:\n{}\n\n{}",
        describe_plan_edits(edits),
        comment_line(comment),
        serde_json::to_string(edits).unwrap_or_default(),
        PLAN_EDIT_RULES,
    )
}

/// The planner request for an answer to `plan_ask`: the question, its
/// options, the answer, and the edit request it was asked about (the planner
/// reads the whole session too, but the edit is an op, not a message).
fn plan_answer_request(
    question: &PlanQuestion,
    answer: &str,
    edit: Option<&(Value, Option<String>)>,
) -> String {
    let options = if question.options.is_empty() {
        "(none)".to_owned()
    } else {
        question.options.join(" / ")
    };
    let mut out = format!(
        "The learner answered your plan question.\n\nQuestion: {}\nOptions: {options}\nLearner's answer: {answer}\n\n",
        question.question
    );
    match edit {
        Some((edits, comment)) => {
            out.push_str("This answer settles the edit request below; apply it with plan_write now, or ask one more question with plan_ask if it left something genuinely ambiguous.\n\n");
            out.push_str(&plan_edit_request(edits, comment.as_deref()));
        }
        None => out.push_str(
            "There are no plan edits on record; revise the plan from the learner's answer and the whole session, and call plan_write.\n\n",
        ),
    }
    out
}

/// The newest `planEdit` op: the edits the pending question was asked about.
fn latest_plan_edit(ops: &[Op]) -> Option<(Value, Option<String>)> {
    ops.iter().rev().find_map(|o| match o {
        Op::PlanEdit(e) => Some((e.edits.clone(), e.comment.clone())),
        _ => None,
    })
}

/// The learner's edits as planner-readable lines: plan-level fields first,
/// then each edited section. Unknown keys are still spelled out, so a control
/// added later reaches the planner instead of being dropped.
fn describe_plan_edits(edits: &Value) -> String {
    let empty = serde_json::Map::new();
    let fields = edits.as_object().unwrap_or(&empty);
    let mut out = String::from("Plan-level:\n");
    let mut any_plan = false;
    for (key, value) in fields {
        if key == "sections" || value.is_null() {
            continue;
        }
        out.push_str(&format!("- {key}: {}\n", plan_value(key, value)));
        any_plan = true;
    }
    if !any_plan {
        out.push_str("- (unchanged)\n");
    }
    let Some(sections) = fields.get("sections").and_then(Value::as_array) else {
        return out;
    };
    out.push_str("\nSections:\n");
    if sections.is_empty() {
        out.push_str("- (none)\n");
        return out;
    }
    for section in sections {
        match section
            .get("id")
            .or_else(|| section.get("sectionId"))
            .and_then(Value::as_str)
        {
            Some(id) => out.push_str(&format!("- `{id}`:\n")),
            None => out.push_str("- (a section without an id; match it by title):\n"),
        }
        let mut any = false;
        if let Some(fields) = section.as_object() {
            for (key, value) in fields {
                if key == "id" || key == "sectionId" || value.is_null() {
                    continue;
                }
                out.push_str(&format!("  - {key}: {}\n", plan_value(key, value)));
                any = true;
            }
        }
        if !any {
            out.push_str("  - (unchanged)\n");
        }
    }
    out
}

/// One edit value as text. The two control-shaped fields get the learner's
/// meaning spelled out; everything else is its JSON value.
fn plan_value(key: &str, value: &Value) -> String {
    match value {
        Value::Bool(keep) if key == "keep" => if *keep {
            "true (keep this section)"
        } else {
            "false (remove this section)"
        }
        .to_owned(),
        Value::String(s) => format!("\"{}\"", s.trim()),
        Value::Array(items) if items.is_empty() => "(none)".to_owned(),
        Value::Array(items) => items
            .iter()
            .map(|i| plan_value("", i))
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    }
}

fn comment_line(comment: Option<&str>) -> String {
    match comment {
        Some(c) => format!("Learner's comment: \"{c}\"\n"),
        None => "Learner's comment: (none)\n".to_owned(),
    }
}

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

/// Most probe questions a `quick` starting point allows (§3).
const QUICK_PROBE_QUESTIONS: usize = 5;

/// The places a hunt can send the learner (§5).
const HUNT_KINDS: [&str; 7] = [
    "forum",
    "datasheet",
    "docs",
    "lecture",
    "video",
    "paper",
    "person",
];

/// How the debrief sorts a claim (§5): a wrong claim is still recorded, and a
/// practitioner tip is a tip, not a law.
const DEBRIEF_VERDICTS: [&str; 5] = ["correct", "partial", "wrong", "tip", "offroute"];

/// Distinct probe `question_ask` calls the log accepted (the current call is
/// already appended when a gate runs). A refused call spends no budget.
fn probe_questions(ops: &[Op]) -> usize {
    let refused: HashSet<&str> = ops
        .iter()
        .filter_map(|op| match op {
            Op::Tool(t) if t.result.as_ref().is_some_and(|r| r.is_error) => {
                Some(t.call_id.as_str())
            }
            _ => None,
        })
        .collect();
    let mut accepted: HashSet<&str> = HashSet::new();
    for op in ops {
        if let Op::Tool(t) = op {
            if t.name == "question_ask"
                && t.args.get("role").and_then(Value::as_str) == Some("probe")
                && !refused.contains(t.call_id.as_str())
            {
                accepted.insert(t.call_id.as_str());
            }
        }
    }
    accepted.len()
}

fn gate_question(ops: &[Op], state: &SessionState, args: &Value) -> Exec {
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
    // A level is what the question demands, not how hard it is (§2). Every
    // probe and check carries one.
    if matches!(role, "probe" | "check") {
        let level = args
            .get("level")
            .and_then(Value::as_str)
            .and_then(Level::parse);
        if level.is_none() {
            return refuse(
                name,
                format!("a {role} question needs `level`: explain, use, analyze or design"),
            );
        }
    }
    if role == "probe" {
        match state.starting_point() {
            "new" => {
                return refuse(
                    name,
                    "the learner starts new: no probe questions; their first checks are the measurement",
                )
            }
            "quick" if probe_questions(ops) > QUICK_PROBE_QUESTIONS => {
                return refuse(name, "quick check: at most five probe questions")
            }
            _ => {}
        }
    }
    // A hunted section is taught only after the debrief (§5): exploring first
    // is the point, and the debrief is what corrects what the learner found.
    if state.phase == Phase::Teach && state.hunt_blocks_teaching() {
        return refuse(
            name,
            "this section's hunt isn't debriefed yet: call `hunt_send` first if you haven't, and `debrief_record` once the learner is back; teaching questions wait for the debrief",
        );
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

/// A JSON array of strings, trimmed and without empties (a hunt return's
/// `links` and `files`; the UI sends only non-empty ones anyway).
fn string_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The learner-side message that brings a hunt's return into the conversation
/// (§5). Everything is optional: a tip remembered without its source is still
/// what they learned, so an empty return is a return too.
fn hunt_return_text(send: &HuntSendOp, notes: &str, links: &[String], files: &[String]) -> String {
    let mut text = format!(
        "[hunt return] The learner is back from the hunt `{}` (section `{}`).\nHunt question: {}",
        send.hunt_id, send.section_id, send.question
    );
    if !notes.is_empty() {
        text.push_str(&format!("\nNotes: {notes}"));
    }
    if !links.is_empty() {
        text.push_str(&format!("\nLinks: {}", links.join(", ")));
    }
    if !files.is_empty() {
        text.push_str(&format!(
            "\nFiles (names only; unreadable ones reach you by name): {}",
            files.join(", ")
        ));
    }
    if notes.is_empty() && links.is_empty() && files.is_empty() {
        text.push_str(
            "\nThey brought nothing written back: debrief what they found in their own words.",
        );
    }
    text.push_str("\nDebrief it with `debrief_record` before teaching this section.");
    text
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

/// Why a section may not close yet, or `Ok` when its gates are met: a known
/// section needs one passing check at its target level until a miss refutes
/// it (§4); every other section needs every node passed *and* one passing
/// check at the target level (§2). Older sections have no level, so the
/// level rule can't apply to them.
fn close_problems(state: &SessionState, section: &Section) -> Result<(), String> {
    let known = section.assumed_known && !state.known_refuted.contains(&section.id);
    if !known {
        let missing: Vec<&str> = section
            .node_ids
            .iter()
            .filter(|n| !state.node_passed(n))
            .map(String::as_str)
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "these nodes have no passing check question yet: {}",
                missing.join(", ")
            ));
        }
    }
    match section.target_level {
        Some(target) => {
            if !state.section_passed_at(section, target) {
                return Err(format!(
                    "no passing check in this section is at `{}` or above, its target level",
                    target.as_str()
                ));
            }
        }
        None if known && !section.node_ids.iter().any(|n| state.node_passed(n)) => {
            // A known section still needs one passing check, even when the
            // plan predates levels.
            return Err(
                "a known section needs one passing check at its target level before it closes"
                    .to_owned(),
            );
        }
        None => {}
    }
    Ok(())
}

/// The section keys the `levels`, `plan_controls` and `known_section` blocks
/// read. Always set: a flag that is off drops its block, so nothing is
/// evaluated, and an absent section gets empty defaults.
fn set_section_level_values(values: &mut TemplateValues, section: Option<&Section>) {
    values.set(
        "section.targetLevel",
        section
            .and_then(|s| s.target_level)
            .map_or("(none)", Level::as_str),
    );
    values.set(
        "section.knownNote",
        section.map(|s| s.known_note.clone()).unwrap_or_default(),
    );
    values.set(
        "section.learnerNote",
        section.map(|s| s.learner_note.clone()).unwrap_or_default(),
    );
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
        let ops: Vec<Op> = Vec::new();
        let probe = json!({"role":"probe","level":"use","format":"free","prompt":"p","answer":"a"});
        assert!(matches!(gate_question(&ops, &measure, &probe), Exec::Pause));
        let check = json!({"role":"check","level":"use","nodeId":"n","format":"free","prompt":"p","answer":"a"});
        assert!(matches!(
            gate_question(&ops, &measure, &check),
            Exec::Done { is_error: true, .. }
        ));
        let bad_choice = json!({"role":"probe","level":"use","format":"choice","options":["a","b"],"prompt":"p","answer":"c"});
        assert!(matches!(
            gate_question(&ops, &measure, &bad_choice),
            Exec::Done { is_error: true, .. }
        ));
        // A probe without a level is refused, and the error names the levels.
        let unlevelled = json!({"role":"probe","format":"free","prompt":"p","answer":"a"});
        match gate_question(&ops, &measure, &unlevelled) {
            Exec::Done { content, is_error } => {
                assert!(is_error);
                assert!(content.contains("`level`"), "{content}");
                assert!(content.contains("analyze"), "{content}");
            }
            Exec::Pause => panic!("a probe without a level must be refused"),
        }
        // A predict question may go without one.
        let teach = teach_state();
        let predict =
            json!({"role":"predict","nodeId":"n1","format":"free","prompt":"p","answer":"a"});
        assert!(matches!(gate_question(&ops, &teach, &predict), Exec::Pause));
        // A check question may not.
        let check = json!({"role":"check","nodeId":"n1","format":"free","prompt":"p","answer":"a"});
        match gate_question(&ops, &teach, &check) {
            Exec::Done { content, is_error } => {
                assert!(is_error);
                assert!(content.contains("`level`"), "{content}");
            }
            Exec::Pause => panic!("a check without a level must be refused"),
        }
    }

    fn test_section(target_level: Option<Level>, assumed_known: bool) -> Section {
        Section {
            id: "s1".into(),
            title: "S".into(),
            node_ids: vec!["n1".into(), "n2".into()],
            mode: crate::state::Kind::Mechanism,
            style: "socratic".into(),
            plugins: vec![],
            notes: String::new(),
            exit: String::new(),
            desktop_only: false,
            needs_install: vec![],
            target_level,
            assumed_known,
            known_note: String::new(),
            learner_note: String::new(),
            hunt: None,
        }
    }

    /// A teach-phase state on one section with two nodes.
    fn teach_state() -> SessionState {
        let section = test_section(Some(Level::Use), false);
        SessionState {
            phase: Phase::Teach,
            plan: Some(crate::state::Plan {
                version: 1,
                goal: "g".into(),
                approach: "a".into(),
                depth: "solid".into(),
                explore: "off".into(),
                minutes_per_sitting: None,
                style: String::new(),
                nodes: vec![],
                sections: vec![section],
            }),
            current_section: Some("s1".into()),
            ..Default::default()
        }
    }

    /// A passing check at `level` on `node`.
    fn passed(state: &mut SessionState, node: &str, level: Option<Level>) {
        state
            .checks
            .entry(node.to_owned())
            .or_default()
            .push(crate::state::CheckVerdict {
                node_id: node.to_owned(),
                question_id: format!("q-{node}-{}", level.map_or("none", Level::as_str)),
                verdict: "correct".into(),
                level,
            });
    }

    #[test]
    fn section_done_needs_a_passing_check_at_the_target_level() {
        let mut state = teach_state();
        let section = state.current_section().unwrap().clone();
        // Every node passed, but only at `explain`: the target level is `use`.
        passed(&mut state, "n1", Some(Level::Explain));
        passed(&mut state, "n2", Some(Level::Explain));
        let why = close_problems(&state, &section).unwrap_err();
        assert!(why.contains("`use`"), "the refusal names the level: {why}");
        // One check at the target level is enough.
        passed(&mut state, "n1", Some(Level::Use));
        assert!(close_problems(&state, &section).is_ok());

        // A wrong answer above the level doesn't count.
        let mut wrong = teach_state();
        wrong.checks.insert(
            "n1".into(),
            vec![crate::state::CheckVerdict {
                node_id: "n1".into(),
                question_id: "q1".into(),
                verdict: "wrong".into(),
                level: Some(Level::Design),
            }],
        );
        assert!(
            close_problems(&wrong, &wrong.current_section().unwrap().clone())
                .unwrap_err()
                .contains("no passing check")
        );
    }

    #[test]
    fn known_sections_close_on_one_check_until_refuted() {
        let mut state = teach_state();
        state.plan.as_mut().unwrap().sections[0].assumed_known = true;
        let section = state.current_section().unwrap().clone();
        // Nothing passed: a known section still refuses without one check.
        assert!(close_problems(&state, &section).is_err());
        passed(&mut state, "n2", Some(Level::Use));
        // One node passed at the target level closes it, though n1 has none.
        assert!(close_problems(&state, &section).is_ok());
        // After a miss, the normal per-node rule is back.
        state.known_refuted = vec!["s1".into()];
        let why = close_problems(&state, &section).unwrap_err();
        assert!(why.contains("no passing check"), "{why}");

        // Older sections have no level: the level rule doesn't apply, and an
        // older plan can't have a known section, so both nodes are needed.
        let mut old = teach_state();
        old.plan.as_mut().unwrap().sections[0].target_level = None;
        passed(&mut old, "n1", None);
        passed(&mut old, "n2", None);
        let section = old.current_section().unwrap().clone();
        assert!(close_problems(&old, &section).is_ok());
    }

    /// The ops a probe question leaves in the log: the call with its args.
    fn probe_calls(n: usize) -> Vec<Op> {
        (0..n)
            .map(|i| {
                Op::Tool(ToolOp {
                    op_id: op_id(),
                    at: now_iso(),
                    call_id: format!("p{i}"),
                    name: "question_ask".into(),
                    args: json!({"role":"probe","level":"use","format":"free","prompt":"p","answer":"a"}),
                    result: None,
                    agent: None,
                })
            })
            .collect()
    }

    #[test]
    fn starting_point_new_refuses_probe_questions_and_quick_limits_them() {
        let probe = json!({"role":"probe","level":"use","format":"free","prompt":"p","answer":"a"});

        let new = SessionState {
            goal: Some(json!({ "purpose": "curiosity", "startingPoint": "new" })),
            ..Default::default()
        };
        match gate_question(&[], &new, &probe) {
            Exec::Done { content, is_error } => {
                assert!(is_error);
                assert!(content.contains("starts new"), "{content}");
            }
            Exec::Pause => panic!("new must refuse probe questions"),
        }

        let quick = SessionState {
            goal: Some(json!({ "purpose": "curiosity", "startingPoint": "quick" })),
            ..Default::default()
        };
        // Five probe calls in the log, this one being the fifth.
        assert!(matches!(
            gate_question(&probe_calls(5), &quick, &probe),
            Exec::Pause
        ));
        // The sixth is refused.
        match gate_question(&probe_calls(6), &quick, &probe) {
            Exec::Done { content, is_error } => {
                assert!(is_error);
                assert!(content.contains("at most five"), "{content}");
            }
            Exec::Pause => panic!("the sixth quick probe must be refused"),
        }

        // The default starting point is unchanged: no limit.
        assert!(matches!(
            gate_question(&probe_calls(9), &SessionState::default(), &probe),
            Exec::Pause
        ));

        // A refused probe (an error result) spends no budget.
        let mut six = probe_calls(6);
        six.push(Op::Tool(ToolOp {
            op_id: op_id(),
            at: now_iso(),
            call_id: "p0".into(),
            name: "question_ask".into(),
            args: Value::Null,
            result: Some(ToolResultValue {
                content: "refused".into(),
                is_error: true,
            }),
            agent: None,
        }));
        assert!(matches!(gate_question(&six, &quick, &probe), Exec::Pause));
    }
}
