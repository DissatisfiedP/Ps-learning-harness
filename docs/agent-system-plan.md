# Agent system plan

`learning-harness-architecture.md` (the architecture) is the source of truth. This file records where the code departs from it and what is built so far.

## Decisions that depart from the architecture

| Decision | Chosen | Why |
|---|---|---|
| Core language | Rust for the agent loop, providers, storage and tools. The UI is web tech in the Tauri webview. | Owner override of arch §6.2. Provider calls and streaming live next to the Tauri shell with no second IPC layer. Prompts stay as Markdown, editable without a rebuild. |
| Frontend build | Plain ES modules with vendored `marked`, `DOMPurify` and `KaTeX`. No bundler. | R1 (lightweight). Nothing in the UI needs a compile step yet. |
| Dev server | None. The old `crates/container` HTTP server was removed. | R1 forbids a separate server process. Its logic duplicated the Tauri commands. |
| Keyless web search | Wikipedia search API. | DuckDuckGo's HTML endpoint serves its bot page to Rust HTTP clients. Mojeek serves a captcha. |

## Layout

```
crates/
  providers/        wire formats (OpenAI chat, OpenAI Responses, Anthropic Messages),
                    SSE decoding, provider catalog, WorkBuddy sign-in + catalog
  harness/          sessions (op logs), device config + credentials, prompts, tutor turn
    prompts/        mode prompts. learn.md and practice.md are live (<repo>/prompts/<name>.md overrides them).
                    tutor/, coach/, jobs/, plugins/ and shared/ are the target set: see prompts/README.md
  research-search/  web search for the future researcher agent (Brave → Wikipedia)
src-tauri/          Tauri v2 shell: one command per Harness method
src/                UI: app.js, settings.js, markdown.js, api.js, styles.css, vendor/
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
- **Sessions:** `sessions/<yyyy>/<ulid>.jsonl` in the data repo (arch §4.17). The first line is a `session` op, then one `message` op per turn. Readers order by `at`.
- **Device-local state** lives in the OS config dir and never in the repo (arch §4.18):
  - `settings.json`: model choice and base URLs;
  - `credentials.json`: keys and tokens, written with mode 0600.
- **Tutor turn:**
  - mode prompt (Learn or Practice) + full transcript;
  - streamed to the UI over a Tauri channel;
  - the learner can stop it; the partial reply is kept and marked `interrupted`;
  - a failed turn can be retried.
- **UI:**
  - sessions sidebar grouped by date;
  - Learn/Practice home screen;
  - streaming Markdown with KaTeX math and a collapsible reasoning section;
  - settings for the model and provider connections;
  - responsive down to phone width.

Data locations: the data repo is `$LEARNING_HARNESS_REPO`, or `<app data dir>/repo` by default. Config is `<app config dir>`. On Linux these are `~/.local/share/com.learning-harness.app/repo` and `~/.config/com.learning-harness.app`.

## Not built yet

These follow arch §8. Items 1–3, the tutor + planner and the researcher job are specified as an executable work order in `docs/build-prompt.md`.

1. **Tool calling** in the providers crate, across all three wire formats. Every target prompt depends on it.
2. **Phase state machine and prompt composer.**
   - Phases: Measure (goal → scope research → probe) → Plan → Approve → Teach (per section) → Close, with per-phase tool sets enforced in code (arch §4.10).
   - The composer assembles shared → tutor base → phase → section + mode + plugins.
   - Today the single `learn.md` prompt only *asks* the model to probe first.
3. **Plugin loader:** reads `prompts/plugins/*.md` frontmatter, filters by platform, enforces three plugins per section, handles `plugin_request` (arch §4.20).
4. **Graph storage:** nodes, topics, the op log, alias resolution, chain re-validation (arch §4.2–4.6), and the graph builder job (arch §4.7).
5. **Outcomes log, FSRS and the practice coach,** with the problem checker job (arch §4.8, §4.11–4.13).
6. **Git sync** with per-file conflict resolution via `git2` (arch §4.18, §6.4).
7. **Secure OS storage for credentials** (arch §6.5, still open). Today: a 0600 file outside the repo.
8. **The researcher job,** using `research-search` plus document tools.

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
