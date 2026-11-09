# What is not implemented

Status: 2026-11-09. This is a one-page inventory; `docs/agent-system-plan.md` is the detailed record
(what is built, why, and the numbered known gaps this file condenses). `docs/architecture.md` §8 is the
build order these items follow.

## 1. Verification debt (the largest risk)

- **No real-model run is on record.** Every agent path is proven against the scripted provider
  (`crates/harness/tests/{runtime,learning,graph,plugins,files}_e2e.rs`). Untested against a real model:
  level tagging, hunt eligibility, the "parts I already know" split, plan edits, debrief sorting, artifact
  placement, the graph builder, and researcher brief quality.
- **The UI is verified in headless Chromium only**, against a mocked Tauri bridge — never in WebKitGTK and
  never on a device. Plugin-view isolation likewise: Chromium, not WebKitGTK.
- **Sync is proven against local servers only.** Git against a temp bare repository, WebDAV against an
  in-process test server: no real host (GitHub, Koofr, Nextcloud), no token flow, no TLS server, no phone.
  The mobile session lock ran only with an injected platform. On Android, libgit2 and OpenSSL are built from
  source (`openssl-sys/vendored`), but nothing has synced from a phone.
- **No green Android CI run is on record yet.** The bundle jobs now run on every push, and `release.yml`
  publishes what a push run built. The Android job has been fixed run by run: the `android-37.0` platform
  package, a library target for `crates/app`, the r29 NDK, OpenSSL (vendored, then the NDK's `llvm-ranlib`),
  a desktop-only `pick_folder`, and Luau integers that are `i32` on the 32-bit ABIs. CI now builds one
  release APK per ABI (`--split-per-abi`), signed with a fixed key from the repository secrets; neither the
  release build nor the signing step has run yet. No APK has been installed on a device.
- **The macOS and iOS bundles are unproven on hardware.** CI builds an ad-hoc-signed Apple Silicon DMG and an
  unsigned device IPA (`--no-sign`, for sideloading with a personal Apple ID). On Apple targets OpenSSL is
  built from source so the binary links no Homebrew library; libgit2 itself uses Secure Transport there. No
  IPA has been installed on a device, and WKWebView rendering has not been checked.

## 2. Designed, not built

- **Learner model** (arch §4.8): `outcomes.jsonl`, FSRS memory state, demonstrated layers, misconception
  notes, and everything that reads them (probe skipping, planning around held nodes, review scheduling).
- **Practice** (arch §4.11–4.13): the coach, the problem checker job, problem generation with committed
  answers, stream/set formats, and **exam mode** (papers, coverage scheduling, simulation). Practice today is
  one prompt with no tools, and "Practise n ideas" on the Map persists nothing.
- **Mobile runtime** (arch §8.7): the app has never run on Android or iOS. The iOS project is generated in CI
  (`cargo tauri ios init`) and not checked in; the IPA is unsigned, so installs depend on re-signing by a
  sideloading tool. On mobile `pick_folder` refuses (tauri-plugin-dialog picks folders on desktop
  only), so workspaces and folder plugins can't be attached from a phone.
- **Native plugins** (`sandbox`, `code-runner`, `repl`, `cas`, `live-search`, `image-set`) and the seven
  catalogue manifests: none is implemented or loadable. The Luau runtime (host API as `@harness/*` modules),
  registry, install/lock, views and the reference `circuit` package are real; the catalogue is not.
  `argument-map` (the one Luau candidate) is not written either.
- **Secure OS storage for credentials**: still a `0600` file; no keychain/keystore. The ChatGPT OAuth
  tokens (access, refresh, expiry) are stored there too.
- **Tree-sitter plugin**, **Typst artifact renderer**, **plugin-builder agent**: designed/rejected/optional,
  all absent.

## 3. Half-built seams

- **Graph**: the op log, builder job, aliases, chains, splitter and read model exist; the Map has **no**
  surface for split proposals, learner drag-to-move of a node, or a node's provenance. The builder has never
  run against a real model.
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
- **Signed iOS/macOS distribution** (App Store, TestFlight, Developer ID notarization) — needs a paid Apple
  developer account.
