# Agent system plan

`learning-harness-architecture.md` (the architecture) is the source of truth. This file records where the code departs from it and what is built so far.

## Decisions that depart from the architecture

| Decision | Chosen | Why |
|---|---|---|
| Core language | Rust for the agent loop, providers, storage and tools. The UI is web tech in the Tauri webview. | Owner override of arch §6.2. Provider calls and streaming live next to the Tauri shell with no second IPC layer. Prompts stay as Markdown, editable without a rebuild. |
| Frontend build | Plain ES modules with vendored `marked`, `DOMPurify` and `KaTeX`. No bundler. | R1 (lightweight). Nothing in the UI needs a compile step yet. |
| Dev server | None. The old `crates/container` HTTP server was removed. | R1 forbids a separate server process. Its logic duplicated the Tauri commands. |
| Keyless web search | Wikipedia search API. | DuckDuckGo's HTML endpoint serves its bot page to Rust HTTP clients. Mojeek serves a captcha. |

## Decisions for the agent-runtime build (docs/build-prompt.md)

These settle what the work order left open. Recorded before code so the choices are auditable.

| Decision | Chosen | Why |
|---|---|---|
| Prompt bundling | `build.rs` walks `prompts/**` and generates a `path -> &'static str` map (via `include_str!`). | `include_dir` is not in the offline crate cache; a build script is dependency-free and gives a compile-time map. |
| Plugin/job frontmatter parse | Tiny hand-rolled YAML frontmatter parser for the flat `key: value` and `key: [a, b]` shapes used in `plugins/*.md`. | The manifests are flat; keeps the composer dependency-light. |
| HTML→text for `page_fetch` | Small in-tree stripper (drop script/style, tags → text, collapse whitespace). | `html2text` is not in the offline crate cache. |
| Streaming text display | `Message` content is `Vec<ContentBlock>`; the harness builds replay blocks, and the UI streams only `Text` deltas. | Content blocks are required by M1; the runtime owns the single copy of text. |
| Cross-wire `Native` blocks | Dropped in `body()` when `block.wire != endpoint.wire`. | Spec: replay unchanged only to the same wire. |
| `session_state` `learner_state` | Renders `no graph yet` (no graph built, out of scope). | Out-of-scope seam, §4. |
| Per-session locking | One `tokio::sync::Mutex` per session (map behind a `std::sync::Mutex` that is only held to look it up). A tutor turn, a learner `answer`/`approve`, and a job delivery each hold it; planner and researcher jobs never do (they only append ops). No std lock is held across an `.await`. | M7.8; also the mechanism of the delivery rule (a result can't interleave into a running turn). |
| Job delivery | A job's result is persisted on its `job` op (`done`/`failed`). Delivery takes the session lock, skips if an answer is pending (the next tutor turn delivers first), else appends a tagged user message (`research-brief` / `plan-notice`, with `jobId`) and wakes the tutor in Measure/Teach/Close. A message's `jobId` marks the result delivered, so pending deliveries are derivable after a restart. | M4 delivery rule; resumable. |
| Scope `brief` op | Written when the scope brief is *delivered*, not when the researcher submits it. | The `probe_done` gate then means "the tutor has the brief in its conversation", never just "it exists in the log". |
| Tool op shape | Two ops per call, matched by `callId`: the call (before acting, no `result`) and the result (`args` omitted). Planner tool ops carry `agent: "planner"` and are not part of the tutor's messages. Researcher tool loops stay in memory. | An append-only log can't fill in a result; the planner's loop is not conversation; jobs don't touch the conversation (prompts/README). |
| `questionId` | The `question_ask` call id. The learner's answer result includes it as `questionId`, so `verdict_record` and `probe_done.evidence` can cite it. | The contract had no id field; see README fix in `docs/build-report.md`. |
| Op ordering | Op ids come from a process-wide monotonic ULID generator, so two ops written in one millisecond replay in write order (`at`, then `opId`). | A tool call and its result are often written in the same ms. |
| Plan phase | `Plan` is used for the initial planning only. Revisions (learner or `plan_change_request`) run the planner as a background job while the session stays in Teach; only a revision that needs approval moves to `awaitApproval`. | The tutor and learner keep talking while the planner thinks; matches "Plan → Teach: plan_keep, or a minor change". |
| Teaching position | The current section is always "the first plan section not closed by an accepted `section_done`", recomputed on approve and on applied revisions. | A section added before the current one (missing foundation) is taught next. |
| Plan plugins | On approval (and on applied revisions) each open section's plugins that the learner didn't remove become `pluginGrant` ops with `by: "plan"`. Active plugins = grants for the current section still listed by the plan, plus `by: "request"` grants. | One source of truth for active plugins. |
| Hand-off line | After `probe_done` the tutor gets one more model call with its Measure prompt and tools (any call is answered "not run") so it can tell the learner the plan is being built. | `measure.md` asks for that line; Anthropic rejects tool history without tools. |
| Compaction | Tutor only; the planner reads the same compacted conversation. The compaction turn uses `compaction.md` as the system prompt with no tools; the cut is always just before a learner-side message with no call awaiting its result, keeping ≥ 20% of the window verbatim. Context window from the provider catalog, else 100k. | Simplest form of arch §4.15 that never splits a call from its result. |
| Events | One process-wide event sink (`set_event_sink`); Tauri forwards it through a coalescer (16 ms / 2 KB) to a channel the UI registers with `subscribe_events`. Practice keeps the per-call channel. | Jobs and wakes emit after the command that caused them returned. |
| Title index | Titles from `title` ops are cached in `<config>/session-index.json` (device-local, write-then-rename, rebuilt by one full scan when missing). | `list` reads only two lines per file. |
| Job recovery | Opening a Learn session marks jobs left `queued`/`running` by a previous run `aborted` and starts them again from their persisted `args`; undelivered results are delivered. | Otherwise a lost scope job blocks `probe_done` forever. |
| Redaction | Error text persisted in `job` ops or shown via `turnEnd` has every saved/env credential string replaced with `[redacted]`. | No secrets in op logs. |
| `tracing` | Added as a dependency (cached); `debug` events for model calls (first token, total), tool time, turn time and job duration. No subscriber is installed by the library. | M7.9. |
| Capability values | `capabilities` fields are `Option<bool>`; catalogs leave `None` = unverified. | M1/M2 warnings show "unverified". |
| Legacy sessions | Learn sessions without a `phase` op are read-only; Learn sessions are created with a `phase` op on line 2, Practice sessions without one (Practice is never read-only). `learn.md` and its loader branch are deleted. | M4 clean cutover. |
| Learning map before the graph | A read-only `learning_map` command returns every approved Learn plan with per-node check counts; the UI's Map page (ui-design §7.6) draws it and starts Practice on picked ideas. | The knowledge graph (Not built yet 1) is the real source; plans plus check verdicts are the learner data that exists today. The command reads op logs without touching the open-session LRU. |

## Layout

```
crates/
  providers/        wire formats (OpenAI chat, OpenAI Responses, Anthropic Messages),
                    SSE decoding, provider catalog, WorkBuddy sign-in + catalog
  harness/          sessions (op logs), config + credentials, prompt composer, agent runtime:
                    agent.rs (tutor loop, gates, commands), jobs.rs (researcher, planner,
                    delivery, titles), replay.rs (derived state, messages, compaction),
                    tools.rs (schemas, plan checks), files.rs, web.rs, plugins.rs, events.rs
    prompts/        bundled by build.rs; <repo>/prompts/<path> overrides. See prompts/README.md
  research-search/  web search for the researcher (Brave → Wikipedia)
src-tauri/          Tauri v2 shell: one command per Harness method, async + spawn_blocking for IO
src/                UI: app.js, cards.js, timeline.js, reply.js, stream.js, markdown.js,
                    map.js + map-layout.js (Map page), settings.js, api.js, styles.css, vendor/
```

## What is built

- **Providers** (`crates/providers`):
  - Anthropic, OpenAI and OpenRouter, DeepSeek and Z.AI use an API key.
  - ChatGPT/Codex uses an access token.
  - WorkBuddy uses browser sign-in or an API key; details in `docs/workbuddy-provider.md`.
  - All of them stream.
  - A stream that stops early is an error, not a silent stop.
  - Gateway error envelopes become readable messages.
  - Each provider's base URL can be overridden, for proxies or local OpenAI-compatible servers.
- **Sessions:** `sessions/<yyyy>/<ulid>.jsonl` in the data repo (arch §4.17), with every target op of arch §5 (`phase`, `goal`, `brief`, `probe`, `plan`, `planRequest`, `planKeep`, `approve`, `pluginGrant`, `tool`, `job`, `compaction`, `title`). Readers order by `at`. `SessionState` is derived by replay, never stored. `list` reads two lines per file; open sessions' ops stay in an LRU of 8.
- **Device-local state** lives in the OS config dir and never in the repo (arch §4.18):
  - `settings.json`: model roles and base URLs;
  - `credentials.json`: keys and tokens, written with mode 0600;
  - `session-index.json`: generated titles (derived).
- **Tool calling** in all three wires, with native-block replay and prompt caching.
- **Model roles** (arch §4.22): Tutor required, others fall back to it; Settings role cards with test button and usage.
- **Prompt composer:** shared → agent base → phase → section + mode + plugins; every placeholder required.
- **Agent runtime** (Learn mode): tutor (Measure/Teach/Close) and planner agents, the phase machine and all gates of arch §4.10 in code, interactive questions that survive restarts, researcher jobs (`research-search`, `page_fetch`, text/Markdown source tools, `brief_submit`), the blocking planner `research`, job delivery and wake, compaction turns, titles by the Reader role, cancellation of turns and jobs.
- **Plugin scaffolding:** `Plugin` trait, registry filtered by platform, manifests from `prompts/plugins/*.md` (matched by id), three per section, three tools per plugin, `plugin_request`, `pluginGrant` ops. No real plugins are registered, so the planner's catalog says "none available".
- **Practice mode:** unchanged single prompt (`practice.md`) until the coach is built.
- **Legacy Learn sessions** (no `phase` op) open read-only.
- **UI ("Carrel", `docs/ui-design.md`):** Home (start + session list); one running head and desk per session; question, choice and plan blocks; plan sheet; job lines; plan notices; block-cached streaming Markdown + KaTeX; lazy timeline; settings role fieldsets; the Map page (each approved plan as a layered map of ideas with check state; pick ideas → Practice).

Data locations: the data repo is `$LEARNING_HARNESS_REPO`, or `<app data dir>/repo` by default. Config is `<app config dir>`. On Linux these are `~/.local/share/com.learning-harness.app/repo` and `~/.config/com.learning-harness.app`.

## Not built yet

These follow arch §8.

1. **Graph storage:** nodes, topics, the op log, alias resolution, chain re-validation (arch §4.2–4.6), the graph builder job (arch §4.7) and `graph_search`. `learner_state` renders "no graph yet". Until then the Map page draws plan maps (`learning_map`).
2. **Outcomes log, FSRS and the practice coach,** with the problem checker job (arch §4.8, §4.11–4.13).
3. **Real plugins** (sandbox, code runner, REPL, CAS, live search, image sets, demos, figures, argument map) and measure-phase read-only plugins suggested by the brief.
4. **PDF and page-image reading** (`source_page_image`, PDFs in `source_*`/`page_fetch`): refused with "PDF reading isn't built yet".
5. **Git sync** with per-file conflict resolution via `git2` (arch §4.18, §6.4).
6. **Secure OS storage for credentials** (arch §6.5, still open). Today: a 0600 file outside the repo.

## Agent roster (target)

Principles:
- **One live agent per conversation with the learner.**
- **The plan belongs to a separate planner**, so the tutor can't drift from it.
- **Any other separate agent** exists only when it must not know something, would flood the conversation, or doesn't work on the conversation.

Prompts and tool contracts are in `crates/harness/prompts/README.md`; the reasoning is in arch §4.10.

| Agent | Kind | Prompt | Tools (beyond shared core) | Model role (arch §4.22) |
|---|---|---|---|---|
| Tutor | live: measure + teach | `tutor/base.md` + `tutor/phases/{measure,teach}.md` + `tutor/modes/*` + `plugins/*` | measure: `goal_set`, `question_ask`, `verdict_record`, `probe_done`; teach: `question_ask`, `verdict_record`, `plan_change_request`, `plugin_request`, `section_done`, `session_end`; always: `choice_ask`, `research_request`, `file_find`, `file_read` | Tutor |
| Planner | reads the whole session, speaks via the plan card | `planner/base.md` (+ `planner/exam.md`) | `plan_write`, `plan_keep`, `research` (blocking) | Planner |
| Coach | live, practice | `coach/base.md` + `plugins/*` | `problem_commit`, `problem_conclude`, `research_request`, `file_find`, `file_read` | Tutor |
| Researcher | job | `jobs/researcher.md` | `web_search`, `page_fetch` (`research-search`), `source_outline`, `source_search`, `source_read`, `source_page_image`, `brief_submit` | Reader |
| Problem checker | job, two turns (blind, then told the target) | `jobs/problem-checker.md` | `audit_submit`, `necessity_submit` | Planner |
| Graph builder | job | `jobs/graph-builder.md` | `graph_search`, `graph_submit` | Librarian |
| Topic splitter | job | `jobs/topic-splitter.md` | `split_suggest` | Librarian |
| Demo builder | job | `jobs/demo-builder.md` | `demo_test`, `demo_submit` | Builder |
| Figure maker | job | `jobs/figure-maker.md` | `figure_render`, `figure_submit` | Builder |

Compaction is a turn of the live agent driven by `prompts/compaction.md`, not a separate agent.

Model roles (arch §4.22): **Tutor** (heavy, talks with you), **Planner** (heavy, thinks rarely and deeply), **Reader** (light, searches and reads), **Librarian** (medium, keeps the graph), **Builder** (medium, code and visuals). Only Tutor is required; unset roles fall back to the Tutor model. They replace today's single model choice.
