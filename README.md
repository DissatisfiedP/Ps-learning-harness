# P's Learning Harness

A single-user learning harness: a tutor that probes what you already know, plans a route
through a topic, teaches it section by section and checks each step, with everything it learns
written to an append-only op log of plain JSONL files you own. It is a desktop app — a Tauri v2
shell around a Rust core and a plain ES-module webview UI, no bundler and no separate server —
and it brings no model of its own: you pick a provider and a model per role (tutor, planner,
reader, librarian, builder), by API key or by subscription sign-in. Optional plugin packages
(Luau, sandboxed) add tools, figures and interactive views to a lesson.

## Status

Early. What that means today:

- The desktop shell works end to end against scripted tests: the Rust workspace's end-to-end
  tests drive the tutor, planner, plugins, attachments and workspace paths through a scripted
  provider, and `scripts/ui-check/` drives the UI in headless Chromium against a mocked Tauri
  backend. **No run against a real model is on record** — model behaviour and output quality are
  unverified.
- **Built:** git sync (commands `sync_status`/`set_sync`/`sync_now`, no Settings surface yet; union merges over
  `*.jsonl`, `.conflicts/` for everything else — `docs/sync-build.md`), the knowledge graph's op log, builder job,
  alias/chain handling and topic splitter, PDF reading, the learning-system features, the Luau plugin runtime
  (host API as `@harness/*` modules) with the owner's plugin registry built in as the default, and ChatGPT
  sign-in through the Codex CLI OAuth flow.
- **Not built:** the mobile runtime (CI builds a debug Android APK, but the app has never run on a device), the
  practice coach and its outcomes log, native catalogue plugins, OS-keychain credential storage (today: a `0600`
  file), the WebDAV sync target and the mobile session lock (both designed in `docs/sync-build.md`). Plugin-view
  isolation is verified only in Chromium, not in WebKitGTK or on mobile.
- `docs/unimplemented.md` is the one-page list of what is missing or unverified; `docs/agent-system-plan.md` is
  the running record of what is built, what is not, and where the code departs from the design. Read them before
  trusting any feature list, including this one.

## Requirements

- **Nix with flakes** (recommended): `nix develop` provides Rust (fenix, pinned by `flake.lock`),
  the `cargo-tauri` CLI, and the WebKitGTK/GTK libraries Tauri v2 links on Linux. With direnv,
  `.envrc` (`use flake`) enters it automatically. `nix develop .#android` adds the Android SDK, an
  NDK and the Rust std for the four Android ABIs `cargo tauri android` builds.
- **Or a manual toolchain:** stable Rust plus Tauri v2's Linux build dependencies —
  `libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev librsvg2-dev
  patchelf libssl-dev` (Debian/Ubuntu names) — and the `cargo-tauri` CLI.
- **No Node, npm or bundler.** The UI is plain ES modules with vendored libraries. Bun and
  Chromium are needed only for the optional UI check; `scripts/ui-check/run.sh` fetches both with
  `nix shell`.

## Run

```sh
nix develop
cargo tauri dev            # dev build of the desktop app
```

Without the Tauri CLI, the same binary builds and runs directly (the app package and its binary
are both named `ps-learning-harness`):

```sh
cargo run -p ps-learning-harness
```

## Test

```sh
cargo test --workspace              # Rust: unit + end-to-end, scripted provider, no network needed
cargo clippy --workspace --all-targets -- -D warnings

scripts/ui-check/run.sh ipc placement   # headless Chromium, mocked Tauri backend (nix shell)
# or, with Bun and Chromium already on PATH:
CHROMIUM=/path/to/chromium bun scripts/ui-check/run.js ipc placement
```

`cargo test --offline` works once the dependencies are in the local cargo cache.

CI (`.github/workflows/ci.yml`) runs the same checks on every push to `master` and every pull
request, and adds two bundles that the commands above do not build:

| Artifact | Built with | Notes |
|---|---|---|
| `appimage` — Linux `*.AppImage` | `cargo tauri build --bundles appimage` on ubuntu-22.04 | built against glibc 2.35 so it runs on older distributions than the runner |
| `android-apk` — `*.apk` | `cargo tauri android init` + `cargo tauri android build --apk --debug --target aarch64 armv7 x86_64` with NDK r29 | **debug**-signed, because a release APK is unsigned without a keystore and an unsigned APK will not install; no 32-bit x86 (`i686`) build, which is emulator-only in practice; the generated project lands in the git-ignored `crates/app/gen/android/`. OpenSSL (for git sync's HTTPS) is built from source for Android, with the NDK's `llvm-ranlib` as `TARGET_RANLIB` |

Both are uploaded as workflow artifacts, with `if-no-files-found: error` so an empty bundle fails
the run. Neither is built locally by anything above.

`.github/workflows/release.yml` (run by hand from Actions → Release) tags a commit and publishes
the AppImage and APK that commit's CI push run already built, without rebuilding them.

## Data and config

Nothing is written into this checkout. Two locations, both outside it:

| What | Where | Override |
|---|---|---|
| **Data repo** — sessions (`sessions/<yyyy>/<ulid>.jsonl`), graph files, `frontier.jsonl`, attachments | `<app data dir>/repo`; on Linux `~/.local/share/com.pslearningharness.app/repo` | `$LEARNING_HARNESS_REPO` |
| **Device config** — `settings.json`, `credentials.json` (mode `0600`), `models-cache.json`, `session-index.json`, `workspaces.json`, installed `plugins/<id>/<sha256>/`, `registry-cache/` | `<app config dir>`; on Linux `~/.config/com.pslearningharness.app` | — (OS convention) |

The data repo is a plain directory of text files. In-app sync makes it a git repository on demand
(`sync_now`, and best effort on launch and after a session ends): it commits, merges with
app-controlled conflict rules and pushes to the HTTPS remote you configure. Pointing
`$LEARNING_HARNESS_REPO` at your own repository also works today, without the app's merge rules.

## Repository map

```
crates/app          Tauri v2 shell: one IPC command per Harness method; lib.rs holds `run()`
                    (the mobile entry point), main.rs the desktop binary ps-learning-harness
crates/harness      the core: session op logs and replay, prompt composer, agent runtime
                    (tutor, planner, jobs), tools and gates, plugins (Luau runtime + host API),
                    knowledge graph, PDF/attachment/workspace handling
crates/providers    model providers: Anthropic Messages, OpenAI chat and Responses wires, SSE
                    decoding, live model catalogs, subscription sign-in
crates/research-search  web search for the researcher job (Brave → Wikipedia)
crates/harness/prompts  bundled prompt sources; a file at <repo>/prompts/<same path> overrides one
ui/                 webview UI ("Carrel"): plain ES modules, vendored Markdown/KaTeX/Prism/Mermaid
plugins/            plugin packages: circuit/ (reference), catalog/ (unbuilt natives), fixtures/
scripts/ui-check/   headless Chromium checks against a mocked Tauri backend
docs/               design docs; see below
```

## Docs

`docs/architecture.md` is the design source of truth — requirements, vocabulary, every
system-design decision with its alternatives, the data schemas and the stack. The rest:

- `docs/agent-system-plan.md` — what is built, the decisions taken while building, and what is not;
- `docs/unimplemented.md` — the one-page inventory of what is still missing or unverified;
- `docs/learning-system.md` — levels, starting point, plan controls, hunts, field guide, calibration;
- `docs/ui-design.md` — the UI design spec and its verification checklist;
- `docs/plugins.md` — plugin authoring (manifest, `@harness/*` host modules, views, limits);
- `docs/sync-build.md` — sync interfaces as built (git), plus the planned WebDAV target and mobile
  session lock;
- `docs/claude-subscription.md`, `docs/workbuddy-provider.md` — how those two provider sign-ins work;
- `crates/harness/prompts/README.md` — index of every agent prompt, how each is composed, the tool
  contracts, and the prompt word budgets.
