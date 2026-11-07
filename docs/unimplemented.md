# What is not implemented

Status: 2026-11-07. This is a one-page inventory; `docs/agent-system-plan.md` is the detailed record
(what is built, why, and the numbered known gaps this file condenses). `docs/architecture.md` §8 is the
build order these items follow.

## 1. Verification debt (the largest risk)

- **No real-model run is on record.** Every agent path is proven against the scripted provider
  (`crates/harness/tests/{runtime,learning,graph,plugins,files}_e2e.rs`). Untested against a real model:
  level tagging, hunt eligibility, the "parts I already know" split, plan edits, debrief sorting, artifact
  placement, the graph builder, and researcher brief quality.
- **The UI is verified in headless Chromium only**, against a mocked Tauri bridge — never in WebKitGTK and
  never on a device. Plugin-view isolation likewise: Chromium, not WebKitGTK.
- **Sync is proven against local bare repositories only.** No real host, no token flow, no two devices, no
  phone. libgit2 cross-compiling to Android is unproven.
- **The CI bundle jobs (AppImage, APK) have never run.** They were written from the Tauri CLI's help text and
  embedded mobile template.

## 2. Designed, not built

- **Learner model** (arch §4.8): `outcomes.jsonl`, FSRS memory state, demonstrated layers, misconception
  notes, and everything that reads them (probe skipping, planning around held nodes, review scheduling).
- **Practice** (arch §4.11–4.13): the coach, the problem checker job, problem generation with committed
  answers, stream/set formats, and **exam mode** (papers, coverage scheduling, simulation). Practice today is
  one prompt with no tools, and "Practise n ideas" on the Map persists nothing.
- **Mobile runtime** (arch §8.7): CI now produces an APK, but the app has never run on Android; iOS has no
  project at all (needs a Mac and a developer account).
- **Native plugins** (`sandbox`, `code-runner`, `repl`, `cas`, `live-search`, `image-set`) and the seven
  catalogue manifests: none is implemented or loadable. The Luau runtime, registry, install/lock, views and
  the reference `circuit` package are real; the catalogue is not. `argument-map` (the one Luau candidate) is
  not written either.
- **Public plugin registry**: registries are supported, but the owner has published none, so a fresh install
  has zero plugins.
- **Secure OS storage for credentials**: still a `0600` file; no keychain/keystore.
- **Tree-sitter plugin**, **Typst artifact renderer**, **plugin-builder agent**: designed/rejected/optional,
  all absent.

## 3. Half-built seams

- **Graph**: the op log, builder job, aliases, chains, splitter and read model exist; the Map has **no**
  surface for split proposals, learner drag-to-move of a node, or a node's provenance. The builder has never
  run against a real model.
- **Sync has no UI**: `sync_status`/`set_sync`/`sync_now`/`acknowledge_conflicts` are commands only; Settings
  has no remote/token/status/needs-attention surface (deliberately deferred by the owner).
- **Plan sheet during teaching**: status + free-text change, not the plan card's structured controls.
- **Debrief**: claims are recorded and sorted by the model, but the UI has no sorted-claim list.
- **`targetLevel`** is schema-optional, so a plan section can arrive without one.
- **`page_fetch`** has no URL allowlist — a fetched page can steer the researcher at localhost/LAN/metadata
  addresses.
- **`session_state`** truncates the pinned goal and brief to 600 characters, so long constraints and venues
  are cut from the model's summary (the full tool call is still in the conversation).
- **Smaller**: `Store::ops` clones the whole log per call inside the tutor loop; `redact` skips secrets under
  12 characters; `answer_plan_question` lacks the open-job guard `edit_plan` has; a repeated
  `verdict_record` can leave a stale `correct` in `checks`; interrupted jobs restart when their session is
  opened, not at app start; persisted native reasoning blocks have no size cap; a turn already running is
  invisible after a restart (the UI guesses).

## 4. Deliberately not built

- **Code-execution plugins** — `sandbox`, `code-runner`, `repl`, `cas`, tree-sitter — excluded by the owner.
- **Sync UI** — excluded by the owner for now.
- **iOS** — blocked on hardware/account, not design.
