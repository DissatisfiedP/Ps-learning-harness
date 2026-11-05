# Build report: agent runtime, tutor + planner, model roles

Work order: `docs/build-prompt.md`. Decisions the docs left open are recorded in `docs/agent-system-plan.md` ("Decisions for the agent-runtime build").

## Milestones

| Milestone | State | Notes |
|---|---|---|
| M1 Tool calling | done | Content blocks, three wires, split arguments, malformed → `Value::String`, `Native` replay, caching, capabilities. Later fix: the OpenAI chat wire now puts tool results before user text in the same turn and joins several text blocks; Anthropic merges consecutive same-role messages. |
| M2 Model roles | done | Settings migration, fallback to Tutor, role cards, test button, per-role usage. |
| M3 Prompt composer | done | `build.rs` bundle, overrides, strict placeholders, frontmatter, composition test. |
| M4 Agent runtime + ops | done | All §5 ops; replayed `SessionState`; tutor loop (persist call → execute → persist result, cap 12); interactive tools that survive restarts; tokio jobs with `job` ops, Reader 3 / Planner 1 semaphores, the delivery rule and wake; planner runs with blocking `research`; researcher with `research-search`, `page_fetch` (20 s, 2 MB, in-tree HTML→text, 40k chars, PDF refused) and `source_*`; compaction turns; Reader-role titles; turn and job cancellation; recovery of jobs interrupted by a restart; legacy sessions read-only; `learn.md` deleted. |
| M5 Phase machine, gates, plugins | done | Every §4.10 gate in code (see below); `approve`, `request_plan_change`; `Plugin` trait + registry + limits + `plugin_request` + `pluginGrant`. Proven with test-only echo plugins in the E2E test. |
| M6 UI | done, not run inside Tauri | Phase strip, question/choice/plan cards, plan side panel/drawer, job chips, plan notices, read-only sessions. Exercised in headless Chromium against a mocked Tauri backend only (see "Open problems"). |
| M7 Performance | done | See numbers below. |
| M8 End-to-end proof | done (scripted) | `crates/harness/tests/runtime_e2e.rs`. No provider key is configured in this environment, so the manual `cargo run -p harness-ui` walk-through with a real model was not possible. |

Gates (all green at the final commit): `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo build -p harness-ui`.

### Gates in code (M5)

| Gate | Where |
|---|---|
| Tutor never gets `plan_write`/`plan_keep` | `tools::tutor_tools` never lists them; `agent::execute_tutor_tool` refuses any tool not offered this call. Unit test + E2E request check. |
| `predict`/`check` and `section_done` only in teach | phase-scoped schemas (`role` enum) + `gate_question`. |
| Teach `question_ask.nodeId` in the current section | `gate_question` (E2E: out-of-section question refused). |
| `probe_done` refused while a question is open or before the brief | `Harness::probe_done`; the `brief` op is written when the brief is delivered (E2E: refused, then accepted). |
| `section_done` needs a passing check per node | `SessionState::node_passed`: only `check` questions graded `correct` (choice by code in `answer`, free recall by an accepted `verdict_record`). E2E: refused, then accepted. |
| Revision that changes the goal or removes a section → AwaitApproval; else applied with a notice and a wake | `tools::needs_approval`, `jobs::plan_write`. |
| `plan_write` validation | `tools::validate_plan`: slugs unique, `requires` known and acyclic, each node in exactly one section, valid modes/styles, plugins in the platform catalog, ≤ 3 per section, completed sections unchanged. |
| `plugin_request` | registered + platform-available, ≤ 3 active in the section, persisted as `pluginGrant` (E2E: limit and unknown id refused). |

## Running the end-to-end test

```
cargo test -p harness --test runtime_e2e
```

A local HTTP server speaks the OpenAI chat SSE wire and replays queued responses chosen by the first line of each request's system prompt (tutor `## Voice`, planner `## Accuracy`, researcher `# Researcher`, title, compaction, practice). Only the Tutor role is configured, so every agent exercises the role fallback. The researcher's reply is held until the test releases it, which makes "probe_done before the brief" deterministic. Tests:

- `scripted_session_runs_end_to_end`: the 10 steps of the work order (goal_set; scope job + `brief_submit`; two probe answers, one graded by code; `probe_done` refused then accepted after the brief wakes the tutor; an invalid `plan_write` (cycle) rejected, then a valid one; approve; predict, `section_done` refused, check, `verdict_record`; the echo plugin round trip and plugin limits; `section_done` accepted; `plan_change_request` answered by `plan_keep` whose notice wakes the tutor; `session_end`). It asserts on the op log and `SessionState` after each step, plus the tool lists sent per phase, the planner's catalog, role fallback, that no credential reaches the op log, and the events the UI receives.
- `a_pending_question_survives_restart_and_resumes`: a new `Harness` on the same directories sees the pending `choice_ask`, refuses free text, accepts the answer and replays call + result in wire order.
- `legacy_learn_sessions_are_read_only_and_practice_still_works`.
- `a_long_conversation_is_compacted_before_the_next_turn`.

## Performance (M7)

| Item | Before | After |
|---|---|---|
| 1. Streaming a 20k-char Markdown + KaTeX reply (288 formulas, 12 code blocks, 740 deltas), headless Chromium 155 | full re-render per frame: max frame after 1 s **131.7 ms**, p95 110.5 ms, 644 frames > 16 ms, 41.9 s of render work | block cache + KaTeX cache: max frame after 1 s **7.9–10.2 ms**, p95 5.6–5.8 ms, 0 frames > 16 ms, 2.8 s of work; identical output |
| 2. `list_sessions`, 1,000 sessions × 60 messages | parsed every file: ≈ 57 ms release / 353 ms debug (measured as a full parse of all files), and called after every turn | header + mtime: **3.4 ms** release / 9.8 ms debug; first list after the title index is lost rebuilds it once (41 ms release). The UI updates one sidebar entry from events instead of re-listing. |
| 3. Blocking commands | sync commands on the main thread | every disk/network command is `async`; file IO runs in `spawn_blocking` |
| 4. IPC chatter | one channel message per provider chunk | coalesced in Rust: flush every 16 ms or 2 KB, and before any other event (unit-tested) |
| 5. Opening a 500-message session | 1,687 ms (eager DOM) | 66–100 ms (last 40 items, older ones prepended on scroll; `content-visibility: auto`) |
| 6. Session file re-read per turn | every turn re-read and re-parsed | LRU of 8 parsed op lists, appended in place |
| 7. Startup | KaTeX script loaded eagerly | no session scan or prompt composition at `Harness::open`; KaTeX loads lazily |
| 8. Jobs vs turns | n/a | per-session `tokio::sync::Mutex`; no std lock across `.await` |
| 9. Instrumentation | none | `tracing` debug events: model call (first token, total), tool time, turn time, job duration |

UI numbers come from a throwaway bench page (not committed) in headless Chromium, not from WebKitGTK inside the app. List numbers come from a throwaway test on this machine.

## Prompt and README fixes

No prompt text was rewritten. `crates/harness/prompts/README.md`:
- **Status** paragraph: describes the live composer instead of `learn.md` (deleted).
- **`question_ask` result** gains `questionId`. The contract had no id, but `verdict_record { questionId }` and `probe_done.evidence: [questionId]` need one. It is the call id, returned in the answer result.
- **`file_find` / `file_read`** fields written down (`{ pattern }`, `{ path, lines? }`); the README named the tools but not their arguments.
- **`session_end`** is available in close as well as teach: after the last `section_done` the phase is Close, and `teach.md` tells the tutor to write the close and then call `session_end`.

## Open problems, most blocking first

1. **No real-model run.** The runtime is proven only against the scripted provider; no key was available. Prompt/tool fit with real models (e.g. how reliably they quote `questionId`, or call `probe_done` with the right shape) is untested.
2. **UI not run inside Tauri/WebKitGTK.** It was exercised in Chromium against a mocked backend only. Lazy KaTeX loading needs CSP `script-src 'self'`, which `tauri.conf.json` has, but this wasn't confirmed in the app.
3. **UI can't see a turn already running when a session is opened**, and after a restart it guesses that a trailing learner message means a failed turn (it offers "Get a reply" → `resume_turn`). Exposing "turn running" and "last turn failed" in `session_state` would remove the guesswork.
4. **Native reasoning blocks are persisted** (`message.native`) so tool loops replay after restarts. They may be large; a size cap isn't enforced.
5. **Compaction** is implemented for the tutor only and is triggered on the estimated size (characters / 4) against the catalog context window (100k when unknown); the WorkBuddy live catalog isn't consulted for context sizes.
6. **Job recovery** restarts interrupted jobs when a session is opened, not at app start; a session nobody opens keeps its stale `running` job op.
7. **Measure-phase plugins** suggested by the brief aren't activated (no real plugins exist); `active_plugins` is empty in measure.
8. **`source_page_image` and PDFs** are refused ("PDF reading isn't built yet"), as the work order allows.
