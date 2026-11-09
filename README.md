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
- **Built:** sync to a git remote and/or a WebDAV folder (Settings → Sync, a status button and banner on Home; union
  merges over `*.jsonl`, `.conflicts/` for everything else; sessions that used a desktop-only plugin open read-only on
  phones until "Continue here anyway" — `docs/sync-build.md`), the knowledge graph's op log, builder job,
  alias/chain handling and topic splitter, PDF reading, the learning-system features, the Luau plugin runtime
  (host API as `@harness/*` modules) with the owner's plugin registry built in as the default, and ChatGPT
  sign-in through the Codex CLI OAuth flow.
- **Not built:** the mobile runtime (CI builds signed release Android APKs and an unsigned iOS IPA, but the app has never run on a device), the
  practice coach and its outcomes log, native catalogue plugins, and OS-keychain credential storage (today: a `0600`
  file). Plugin-view isolation is verified only in Chromium, not in WebKitGTK or on mobile.
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
request, and adds bundles that the commands above do not build:

| Artifact | Built with | Notes |
|---|---|---|
| `appimage` — Linux `*.AppImage` | `cargo tauri build --bundles appimage` on ubuntu-22.04 | built against glibc 2.35 so it runs on older distributions than the runner |
| `android-apk-arm64-v8a`, `android-apk-armeabi-v7a`, `android-apk-x86_64` — one `ps-learning-harness-<abi>.apk` each | `cargo tauri android init` + `cargo tauri android build --apk --split-per-abi --target aarch64 armv7 x86_64` (release) with NDK r29, then `zipalign -P 16` + `apksigner` with the repository's release key | one APK per ABI instead of a universal APK carrying all three native libraries, each named after the ABI its `lib/` holds; a **release** build (the debug Rust library is ~460 MB, nearly all debug info), signed with one fixed key from the repository secrets so every build installs over the last; no 32-bit x86 (`i686`) build, which is emulator-only in practice; the generated project lands in the git-ignored `crates/app/gen/android/`. OpenSSL (for git sync's HTTPS) is built from source for Android, with the NDK's `llvm-ranlib` as `TARGET_RANLIB` |
| `macos-dmg` — macOS `*.dmg` (Apple Silicon) | `cargo tauri build --bundles app,dmg` on macos-latest with `APPLE_SIGNING_IDENTITY=-` | arm64 only; signed **ad hoc**, not with a Developer ID, and not notarized, so Gatekeeper blocks the downloaded app until its quarantine flag is cleared (below); the job fails if the binary links any library outside the system (OpenSSL is built from source on Apple targets for this reason) |
| `ios-ipa-unsigned` — `ps-learning-harness-unsigned.ipa` | `cargo tauri ios init` + `scripts/ios-link-libs.sh` + `cargo tauri ios build --no-sign --target aarch64` on macos-latest | device (arm64) only; the script adds the zlib and iconv that libgit2 links against to the generated Xcode project, which leaves them out (run it after every local `ios init` too); **unsigned**, because the repository has no Apple developer account: it installs only after a sideloading tool re-signs it with your own Apple ID (below); the generated project lands in the git-ignored `crates/app/gen/apple/` |

Each is uploaded as a workflow artifact, with `if-no-files-found: error` so an empty bundle fails
the run. None is built locally by anything above.

`.github/workflows/release.yml` (run by hand from Actions → Release) tags a commit and publishes
the AppImage, APKs, DMG and IPA that commit's CI push run already built, without rebuilding them.

### Installing the macOS and iOS builds

- **macOS:** open the DMG and drag the app to Applications. Because the app is not notarized, macOS
  reports it as damaged or from an unidentified developer; clear the download quarantine once with
  `xattr -dr com.apple.quarantine "/Applications/P's Learning Harness.app"`, then open it normally.
- **iPhone/iPad:** install the IPA with a sideloading tool such as Sideloadly or AltStore, signed in with
  a free Apple ID. Turn on Developer Mode (Settings → Privacy & Security) when iOS asks. A free Apple ID's
  signature expires after 7 days, after which the app must be reinstalled the same way; app data is kept
  when the bundle identifier stays the same.
- Both devices share sessions through Settings → Sync (git remote or WebDAV), as on the other platforms.

### Android signing key

Android only installs an update signed with the same key as the installed app; a new key means
uninstalling first, which deletes the app's local data. CI therefore signs every APK with one key
kept in two repository secrets, and the `android` job fails without them. Create them once:

```sh
keytool -genkeypair -keystore ps-learning-harness-release.p12 -storetype PKCS12 \
  -alias release -keyalg RSA -keysize 4096 -validity 10000 -dname "CN=P's Learning Harness"
base64 -w0 ps-learning-harness-release.p12 | gh secret set ANDROID_KEYSTORE_BASE64
gh secret set ANDROID_KEYSTORE_PASSWORD   # the password keytool asked for
```

The alias must be `release`. Keep the `.p12` and its password backed up outside the repository:
losing either means no future APK can update an installed one.

## Data and config

Nothing is written into this checkout. Two locations, both outside it:

| What | Where | Override |
|---|---|---|
| **Data repo** — sessions (`sessions/<yyyy>/<ulid>.jsonl`), graph files, `frontier.jsonl`, attachments | `<app data dir>/repo`; on Linux `~/.local/share/com.pslearningharness.app/repo` | `$LEARNING_HARNESS_REPO` |
| **Device config** — `settings.json`, `credentials.json` (mode `0600`), `models-cache.json`, `session-index.json`, `workspaces.json`, installed `plugins/<id>/<sha256>/`, `registry-cache/` | `<app config dir>`; on Linux `~/.config/com.pslearningharness.app` | — (OS convention) |

The data repo is a plain directory of text files. In-app sync (Settings → Sync; `sync_now`, and best effort on launch
and after a session ends) carries it to a git remote, a WebDAV folder, or both. Git commits, merges with app-controlled
conflict rules and pushes to the HTTPS remote you configure; WebDAV compares each file against a device-local manifest
and uploads, downloads or merges it with the same rules. Pointing `$LEARNING_HARNESS_REPO` at your own repository also
works today, without the app's merge rules.

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
