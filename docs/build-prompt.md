# Build prompt: agent runtime, tutor + planner, model roles

You are working unattended on **Learning Harness**, a Tauri v2 desktop app (Android later) that teaches through AI agents. The repo is `~/Projects/Ps_learn_harness`. You have about two hours, and no one will answer questions. Make the conservative choice, write it down in `docs/agent-system-plan.md`, and continue.

Your job: turn today's single-prompt chat into the **agent runtime** the design describes. That means:
- tool calling;
- model roles;
- the prompt composer;
- the phase machine;
- the tutor and planner agents and the researcher job;
- plugin *scaffolding*;
- the UI for all of it;

and the app must stay fast. Explicitly out of scope: sync, the knowledge graph, practice, real plugins (see "Out of scope").

---

## 0. Before you touch code

1. **Enter the dev shell:** `nix develop` (or `direnv allow`). If the directory is not a git repo, run `git init && git add -A && git commit -m "baseline"`, so every change you make can be reviewed and reverted. Commit at the end of every milestone below, with a message naming the milestone.
2. **Read these, in this order.** They are the spec. Don't skim the first three.
   1. `learning-harness-architecture.md`. Mainly:
      - §3 Vocabulary;
      - §4.10 Session flow and agent topology;
      - §4.15 Context management;
      - §4.16 Background jobs;
      - §4.20 Sections and plugins;
      - §4.21 Research;
      - §4.22 Model roles;
      - §5 Schemas;
      - §6.2–6.5 Stack.
   2. `crates/harness/prompts/README.md`: every agent, the composition order, the placeholders, and **the tool contracts. Tool names and fields in code must match these exactly.**
   3. `docs/agent-system-plan.md`: what is built and what isn't.
   4. The prompt files you will wire up:
      - `tutor/base.md`, `tutor/phases/measure.md`, `tutor/phases/teach.md`, `tutor/modes/*.md`;
      - `planner/base.md`, `planner/exam.md`;
      - `jobs/researcher.md`;
      - `compaction.md`;
      - `shared/*.md`;
      - `plugins/*.md` (only their frontmatter matters for now).
   5. The code:
      - `crates/providers/src/{lib.rs,sse.rs,openai_chat.rs,openai_responses.rs,anthropic.rs,catalog.rs}` and `crates/providers/tests/stream.rs`;
      - `crates/harness/src/{lib.rs,sessions.rs,config.rs}`;
      - `crates/research-search/src/lib.rs`;
      - `src-tauri/src/main.rs`;
      - `src/{app.js,settings.js,markdown.js,api.js,styles.css}`.
3. **Licensing.** LearnHarness (AGPL-3.0) inspired this design. Do not copy code or text from it.

## 1. What exists (so you extend it rather than rebuild it)

**Workspace:** `crates/providers`, `crates/harness`, `crates/research-search`, `src-tauri`, plus the plain ES-module UI in `src/` with vendored `marked`, `DOMPurify` and `KaTeX`. There is no bundler and no Node build. Lints: `unsafe_code = "forbid"`, `clippy::all = warn`.

**providers:**
- `stream_chat(client, endpoint, &ChatRequest, on_event) -> Completion`.
- `ChatRequest { model, system, messages: &[Message { role: User|Assistant, content: String }], max_tokens, effort }`.
- `StreamEvent::{Text, Thinking}`.
- Three wires:
  - `OpenaiChat`: OpenAI, OpenRouter, DeepSeek, Z.AI, WorkBuddy;
  - `OpenaiResponses`: ChatGPT/Codex;
  - `AnthropicMessages`.
- A stream that is cut off is an error.
- `ModelInfo` comes from provider catalogs.
- **There is no tool calling.**

**harness:**
- `Harness::send(session_id, mode, text, on_event)` runs one turn using `prompts/learn.md` or `prompts/practice.md` plus the full transcript.
- Sessions are append-only JSONL op logs at `<repo>/sessions/<yyyy>/<ulid>.jsonl`, with ops `session` and `message`. Readers sort by `at`.
- `config.rs` keeps `settings.json` (a single `model: Option<ModelChoice>` plus `base_urls`) and `credentials.json` (mode 0600, write-then-rename) in the OS config dir.

**src-tauri:**
- One command per `Harness` method.
- Streaming goes to the UI over a `tauri::ipc::Channel`.

**UI:**
- A sessions sidebar, the Learn/Practice home, streaming Markdown with KaTeX, and settings for providers and the single model.

**Tests:**
- Wire tests serve canned SSE from a one-shot local `TcpListener` (`crates/providers/tests/stream.rs`). Reuse that pattern for everything below.

## 2. Target behaviour, in one paragraph

A teaching session runs like this:

1. **Measure (tutor).** The **tutor** pins down the goal (`goal_set`), which starts a background **researcher** `scope` job. The tutor starts probing immediately and re-aims when the brief arrives as a message. It can call the researcher again at any time.
2. **Plan (planner).** `probe_done` is refused until the brief has arrived. When it succeeds, the **planner** runs. The planner is a separate agent whose messages are the whole session and whose system prompt is its own. It calls `plan_write`, and the UI shows a **plan card** with the map, the approach text, each section's mode, and plugin chips. The learner clicks Approve or requests a change, which goes back to the planner.
3. **Teach (tutor).** After Approve, the tutor teaches section by section. Its prompt carries the section header, the planner's notes, the section's mode fragment and the active plugins' fragments. It **cannot** edit the plan. It can call `plan_change_request` with evidence; the planner then replies with `plan_keep` or a new `plan_write`. `section_done` is refused until every node in the section has a passing check.
4. **Close (tutor).** The tutor calls `session_end`.

All gates are in code.

---

## 3. Milestones (in order; stop only at a milestone boundary with everything green)

### M1 · Tool calling in `providers`

Extend the provider types without breaking the existing API's callers. Update every caller in the same change.

**Messages become content blocks:**
- `Text(String)`;
- `ToolCall { id, name, arguments: serde_json::Value }` (assistant messages);
- `ToolResult { call_id, content: String, is_error: bool }`;
- `Native { wire: Wire, raw: serde_json::Value }`: opaque provider blocks that must be replayed unchanged to the same wire. Examples: Anthropic `thinking` blocks with signatures, and Responses `reasoning` items with encrypted content. Both providers require these to be sent back in tool loops. Drop them when the next request goes to a different wire.

**Requests and results:**
- `ChatRequest` gains `tools: &[ToolSpec { name, description, input_schema: Value }]`.
- `Completion` gains `tool_calls: Vec<ToolCall>`.
- `StopReason` gains `ToolUse`.

**Wire mapping:**
- **OpenAI chat:**
  - request: `tools: [{type:"function", function:{name, description, parameters}}]`;
  - streamed `choices[].delta.tool_calls[i]` fragments are accumulated by `index` (id and name arrive once; `function.arguments` arrives in pieces);
  - replay as an assistant message with `tool_calls`, followed by `{role:"tool", tool_call_id, content}`;
  - `finish_reason: "tool_calls"` → `ToolUse`;
  - WorkBuddy uses this wire, so it is covered automatically.
- **OpenAI Responses:**
  - request: `tools: [{type:"function", name, description, parameters}]`;
  - streamed: `response.output_item.added` (type `function_call`, with `call_id` and `name`) followed by `response.function_call_arguments.delta` / `.done`;
  - replay as `function_call` and `function_call_output` input items keyed by `call_id`;
  - keep `reasoning` items as `Native`.
- **Anthropic:**
  - request: `tools: [{name, description, input_schema}]`;
  - streamed: `content_block_start` of type `tool_use` followed by `input_json_delta.partial_json` fragments;
  - replay as `tool_use` blocks in the assistant message and `tool_result` blocks in the next user message;
  - `stop_reason: "tool_use"` → `ToolUse`;
  - keep `thinking` / `redacted_thinking` blocks (with signatures) as `Native`.

**Malformed arguments.** If the arguments JSON doesn't parse at the end of a stream, return the call with `arguments: Value::String(raw)`. The runtime then answers it with an `is_error` tool result, so the model can correct itself. Never panic.

**Prompt caching:**
- Anthropic: put `cache_control: {type:"ephemeral"}` on the last system block and on the last message.
- OpenAI wires: send `prompt_cache_key` set to the session id where the wire accepts it.

**Model capabilities.** `ModelInfo` gains `capabilities: { tool_calls: Option<bool>, vision: Option<bool> }`. Fill it only where a catalog says so; otherwise leave it `None` ("unverified").

**Tests.** For each wire, add an SSE fixture test with one text delta plus two tool calls whose argument JSON is split across chunk boundaries, and a test with a malformed argument. Also add a request-body test that asserts the replay shape for tool results and for `Native` blocks.

### M2 · Model roles

Arch §4.22 is the spec.

- `Settings` gains `roles: { tutor, planner, reader, librarian, builder }`, each an `Option<ModelChoice>`.
  - On load, migrate the old `model` field into `roles.tutor`, then drop `model` on the next save.
  - `ModelChoice.effort` already exists. Planner defaults to `high` when the model supports effort.
- `enum ModelRole { Tutor, Planner, Reader, Librarian, Builder }` resolves to a `ModelChoice`. An unset role falls back to `tutor`. If `tutor` is unset, the error is "choose a Tutor model in Settings".
- Commands:
  - `set_role_model(role, choice | null)`;
  - `test_role(role)`: one tiny request with a single trivial tool, `ping {}`, plus the instruction "call ping". It passes only if the model actually calls the tool. Report the result.
  - Remove `set_model`.
- Settings UI: one card per role, showing:
  - the label exactly as in arch §4.22 (e.g. **Tutor** · heavy · *talks with you*);
  - the guidance sentence;
  - the provider/model picker and an effort picker;
  - a "using Tutor model" chip when the role is unset;
  - capability warnings (tool calls missing, vision missing for Reader/Builder, context window below the role's minimum: Tutor 128k, Reader 200k recommended), with "unverified" for unknown values;
  - a Test button.
- Tutor is first and required; the others are collapsed under "Other roles (optional)".
- Per-role token usage (input and output) is accumulated in memory and shown on the card.

### M3 · Prompt composer

- **Bundling.** Bundle `crates/harness/prompts/**` into the binary. Use the `include_dir` crate, or a `build.rs` that generates a path→`&'static str` map (pick one).
- **Overrides.** A learner override at `<repo>/prompts/<same relative path>` wins when it exists and is non-empty. This generalizes today's `prompt()`.
- **Template syntax:** `{{name}}`, `{{a.b}}`, and `{{#flag}}…{{/flag}}`. No other syntax.
  - **Every placeholder must be supplied.** A missing value is an error, not an empty string, so prompt/code drift fails loudly.
  - Plugin files have YAML frontmatter. Parse the manifest from it, and use only the body as the fragment.
- **Composition**, exactly as `prompts/README.md` → Composition:
  - tutor = the shared fragments in the listed order + `tutor/base.md`, with `phase_instructions` = the phase file;
  - in teach, the phase file gets `mode` and `active_plugins`;
  - planner = its shared fragments + `planner/base.md` + `exam_mode`.
- **`session_state`** is rendered as compact Markdown (goal record, brief summary, probe map, plan with the current section marked, platform). Keep it under ~2k tokens; for full detail, the agent has the transcript.
- **Test.** Compose every agent × phase × mode with representative state, and assert there are no leftover `{{` and no errors. This test is what keeps the prompts and the code in agreement.

### M4 · Agent runtime and session ops

New module(s) in `crates/harness`. Suggested names: `agent.rs`, `tools.rs`, `phase.rs`, `jobs.rs`.

**Op log.** Extend the session op log with the target ops from arch §5 `SessionOp`:
- `phase`, `goal`, `brief`, `probe`, `plan`, `planRequest`, `planKeep`, `approve`, `pluginGrant`;
- `tool` (call and result);
- `job`.

Every op has `opId` (ULID) and `at`. Readers keep sorting by `at`.

**Session state.** `SessionState` is derived by replaying ops: phase, goal, brief, probe map, plan (and version), current section, the passing checks per node, the pending interactive call, queued job results, and granted plugins. Never store derived state.

**The agent loop**, for one agent turn:
1. Compose the system prompt.
2. Build the messages from the ops.
3. Call `stream_chat` with that agent's **phase-scoped tool list**. A tool that isn't available is never sent.
4. Stream the text to the UI.
5. On `ToolUse`, persist each call as a `tool` op **before** executing it, execute it, persist the result, and loop.
6. Cap the loop at 12 iterations per turn.

**Tool kinds:**
- **Immediate tools** run inline: `goal_set`, `probe_done`, `section_done`, `verdict_record`, `file_find`, `file_read`, `plugin_request`, `plan_write`, `plan_keep`.
  - A gate refusal is returned as an `is_error` tool result explaining why (for example "probe_done refused: the scope brief hasn't arrived"), so the model can recover.
  - `file_read` is home-anchored, read-only and size-capped (256 KiB).
- **Interactive tools** pause the loop: `question_ask`, `choice_ask`. The turn ends with the call pending, and the UI renders the card.
  - A new command, `answer(session_id, call_id, payload)`, persists the result and resumes the loop.
  - The pending state survives an app restart because it is derivable from the ops.
- **Job tools** spawn a background job and return an acknowledgement immediately: `research_request`, and `plan_change_request` (which runs the planner).

**Jobs:**
- Each job is a `tokio` task with a persisted `job` op (queued → running → done/failed).
- Concurrency is bounded per role by a semaphore (3 for Reader, 1 for Planner).
- Job results are appended to the session as a user-role message, marked `[research brief]` / `[plan update]`.
- **Delivery rule:**
  - if a learner answer is pending, the result waits and is included at the start of the next tutor turn;
  - if nothing is pending and the tutor is idle, a tutor turn is started automatically ("wake");
  - never interleave a result into a running turn.

**Planner runs:**
- The model role is Planner.
- The messages are the session transcript plus a final user message stating the request (`initial` / learner change text / `plan_change_request` with reason and evidence).
- `research` is **blocking**: it awaits the researcher job and returns its brief as the tool result.
- The planner must end with `plan_write` or `plan_keep`.
- `plan_write` validates the plan:
  - unique slugs;
  - `requires` refers only to existing nodes and is acyclic;
  - every node belongs to exactly one section;
  - modes are valid;
  - plugins exist and are available on the platform;
  - at most 3 plugins per section;
  - on revision, completed sections are unchanged.
  Invalid plans are returned as `is_error` with the specific problems.

**Researcher job:**
- Role: Reader. Prompt: `jobs/researcher.md`.
- Tools:
  - `web_search` → `research-search`;
  - `page_fetch` → GET with a 20 s timeout and a 2 MB cap, HTML converted to text with a small crate such as `html2text`, truncated to ~40k chars;
  - `source_outline` / `source_search` / `source_read` for text and Markdown files;
  - `brief_submit`.
- For PDFs, return an `is_error` saying PDF reading isn't built yet.
- On `brief_submit`, persist a `brief` op (for `scope`) and deliver it.

**Compaction:**
- When the estimated prompt size (characters/4) of the next request exceeds 70% of the role model's context window (or 100k when unknown), run a compaction turn for that agent using `compaction.md`.
- Persist the summary as a `compaction` op that covers ops up to a cut point.
- Message building then uses that summary plus the ops after the cut.
- The pinned state lives in the system prompt and is never compacted.

**Cancellation.** The existing cancel stops the current agent turn and keeps partial text marked `interrupted`, as today. Jobs have their own cancel.

**Titles.** The session title is generated once, after `goal_set`, by the Reader role from the goal record (one short call, no tools).

**Legacy sessions.** Sessions without any `phase` op open read-only, with a "Start a new session" button. Do not keep the old single-prompt Learn path alive. Delete `prompts/learn.md` and its loader branch. `practice.md` and the Practice mode stay exactly as they are until the coach is built.

### M5 · Phase machine, gates, plugin scaffolding

**Phase gates** (arch §4.10, "Gates are in code"):
- The tutor never gets `plan_write`.
- `question_ask` with role `predict` or `check`, and `section_done`, exist only in teach.
- In teach, `question_ask.nodeId` must be in the current section.
- `probe_done` is refused while a question is pending or before a `brief` op exists.
- `section_done` is refused unless every node in the section has at least one `check` question with verdict `correct` (choice graded by code; free recall via `verdict_record`).
- A plan revision that changes the goal or removes any section → AwaitApproval. Otherwise apply it, append a visible notice, and wake the tutor.

**Approval:**
- `approve(session_id, removed_plugins)` persists `approve` and moves to teach, section 1, then starts the tutor turn.
- `request_plan_change(session_id, text)` runs the planner with the learner's words. It works from the plan card and from the map at any time during teach.

**Plugin scaffolding (no real plugins):**
- `trait Plugin { fn manifest(&self) -> &PluginManifest; fn tools(&self) -> Vec<ToolSpec>; async fn call(&self, name, args, ctx) -> ToolOutcome }` (or the equivalent with boxed futures).
- A `PluginRegistry` with `register`, plus a catalog of *registered* plugins filtered by platform. With nothing registered, the catalog is empty and the planner's `plugin_catalog` says "none available".
- Manifests are parsed from `prompts/plugins/*.md` frontmatter. Registration matches by `id`.
- `plugin_request` is granted only for registered, platform-available plugins, and only within the three-per-section limit. Grants are persisted as `pluginGrant` ops.
- Active plugins' tools and fragments are added to the tutor's prompt and tool list.
- Prove the path with one **test-only** plugin (an `echo` tool) registered in tests: the planner selects it, the tutor gets its tool, a call round-trips, and the limits are enforced.

### M6 · UI

Keep the plain ES-module, no-bundler architecture. New views:
- **Phase strip:** Measure · Plan · Teach (section n/m) · Close.
- **Question card** for `question_ask`:
  - `probe` and `predict` show open;
  - `check` shows collapsed behind "Check your understanding";
  - `free` gets a textarea, `choice` gets options;
  - every card has a thinking field, a confidence slider (0–100; hidden for "I don't know"), an "I don't know" button and Submit;
  - after grading, the card shows the verdict and the explanation.
- **Choice card** for `choice_ask`: the options, with "(recommended)" marked, plus an "Other" text field.
- **Plan card:**
  - the approach prose;
  - sections in order, each with title, mode badge, style, exit, removable plugin chips, and its nodes (title + kind) indented beneath;
  - Approve, and "Request a change" (a textarea).
  - After approval, the plan stays in a side panel (collapsible; a drawer on narrow widths) with the current section and node highlighted from the latest `question_ask.nodeId`, plus a "Change plan" action.
- **Job chips** in the conversation: "Researching: <question>" → done, with the brief collapsible.
- **Plan notices:** "Plan revised: <one-line summary>" and "Plan kept: <reason>".
- **Settings:** the model roles cards (M2).

Accessibility: every control is a real labeled element, and everything works from the keyboard.

### M7 · Performance (do not skip; these are measured problems in today's code)

1. **Streaming render is O(n²).** `src/app.js` `paint()` calls `view.update`, which re-parses the *entire* reply's Markdown and re-runs KaTeX on every animation frame.
   - Split the streaming text into blocks. A block is complete once a blank line follows it, outside a code fence or `$$`.
   - Render each completed block once and cache its HTML keyed by its text. Re-render only the open tail block per frame.
   - Cache `katex.renderToString` results by `(tex, display)`.
   - The same applies to the thinking panel.
   - Acceptance: a 20k-character reply streams with no frame over 16 ms after the first second (check in WebKit's dev tools or with `performance.now()` logging around paint).
2. **Session list reads every byte of history.** `Store::list` fully parses every session file, and the UI calls `list_sessions` after every turn.
   - Read only the header line and take `updated_at` from file mtime, or keep a device-local derived index (`<config>/session-index.json`, rebuilt when missing or stale).
   - Update the single changed entry instead of re-listing after a turn.
   - Acceptance: 1,000 sessions list in < 50 ms.
3. **Sync commands block the UI thread.** Tauri runs non-`async` commands on the main thread, and `list_sessions` / `load_session` / `delete_session` do file IO there. Make every command that touches disk or network `async`, and run blocking IO inside `tokio::task::spawn_blocking`.
4. **IPC chatter.** Coalesce stream deltas in Rust: flush to the `Channel` every ~16 ms or every 2 KB, whichever comes first, instead of once per provider chunk.
5. **Opening long sessions** rebuilds every message's DOM.
   - Render the last ~40 messages and lazily prepend older ones when the user scrolls up.
   - Add `content-visibility: auto` with `contain-intrinsic-size` to message containers.
   - Acceptance: a 500-message session opens in < 300 ms.
6. **Don't re-read the session file every turn.** Keep the parsed op list of each open session in memory (an LRU of ~8) and append to it as ops are written.
7. **Startup** must not scan sessions or compose prompts eagerly; do it lazily.
8. **Jobs never block a live turn.** Agent turns and jobs share the tokio runtime, but neither may hold a lock across an `.await`. Use per-session `tokio::sync::Mutex` (or an actor task per session), never one global lock.
9. **Instrument it.** Add `tracing` spans for:
   - turn latency (request → first token → done);
   - tool execution time;
   - job durations.

   Log them at `debug`.

### M8 · End-to-end proof

**Scripted test.** Write a `tests/` integration test in `crates/harness` with a **scripted fake provider**: a local server like the one in `providers/tests/stream.rs`, but replaying a queue of SSE responses chosen by which agent is calling (inspect the system prompt's first line). It must drive one full session:
1. goal → `goal_set`;
2. the scope job (`brief_submit`);
3. two probe questions answered via `answer`;
4. `probe_done` refused before the brief, then accepted;
5. the planner `plan_write` (with one invalid attempt that is rejected first);
6. approve;
7. teach: a predict question, then a check, then `verdict_record`;
8. `section_done` refused, then accepted;
9. a `plan_change_request` answered with `plan_keep`;
10. `session_end`.

Assert on the op log and on `SessionState` at each step. Also test:
- restart in the middle, with a question pending, and resume;
- role fallback (only Tutor set);
- legacy session read-only behavior.

**Manual run.** Run `cargo run -p harness-ui` and walk through a real session with whatever provider key is configured in the environment. If none is configured, say so in the report and rely on the scripted test.

---

## 4. Out of scope (do not build; leave clean seams)

- Git sync, and anything in arch §4.18.
- Knowledge graph storage, graph builder, topic splitter, `graph_search`. Leave the tool absent; `learner_state` renders "no graph yet".
- Practice coach, problem checker, outcomes, FSRS. The existing Practice mode stays untouched.
- Real plugins: sandbox, code runner, REPL, CAS, live search, image sets, demos, figures, argument map. Only the scaffolding and the test-only plugin.
- PDF and vision page reading, dictation, Android/iOS builds. Keep the core crates free of desktop-only APIs so Android stays possible.
- The secure keychain (credentials stay in the 0600 file).

## 5. Engineering rules

- **Style.** Match the existing code: `thiserror` errors, serde `camelCase`, write-then-rename for whole-file writes, append-only op logs, small modules with doc comments explaining *why*, no `unsafe`, and no `unwrap()` on anything that comes from the network, disk or a model.
- **Model output is untrusted input.** Validate tool arguments against the expected shape and answer bad input with `is_error` results, never panics. File tools must refuse paths outside the home directory, and must refuse symlinks that escape it.
- **No secrets in logs or op logs.** Strip credentials from any error text you persist.
- **Clean cutover.** When something replaces an old path (`set_model`, `learn.md`, single-model settings), update every caller and delete the old path in the same milestone. Migrate stored settings once; no compatibility shims.
- **Prompts.** Don't rewrite prompt content. If you find a mismatch between a prompt and a tool contract, fix the code to match `prompts/README.md`. If the README itself is wrong, fix the README and the prompt together and list the change in your report.
- **Tests.** They must catch real regressions: gate refusals, replay shapes, resumability, validation errors. Don't add tests that assert on prompt wording or mock echoes.
- **Gates after every milestone** (inside `nix develop`):
  - `cargo fmt --all`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace`
  - `cargo build -p harness-ui`

  All must pass before you commit.

## 6. When you stop

Update `docs/agent-system-plan.md`: move what you built from "Not built yet" into "What is built", and record any decision you made that the docs didn't settle.

Then write `docs/build-report.md`:
- milestones completed, and anything partial;
- how to run the end-to-end test;
- performance numbers you measured, before and after;
- prompt and README fixes you made;
- open problems, ordered by how much they block the next step.

If time runs short, finish and commit the current milestone with all gates green rather than leaving several half-done. M1 → M4 → M5 are the critical path; M2 and M7 come right after; M6 can be thinner (cards and the plan card first).
