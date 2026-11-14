# What is not implemented

Status: 2026-11-14. This is a one-page inventory; `docs/agent-system-plan.md` is the detailed record
(what is built, why, and the numbered known gaps this file condenses). `docs/architecture.md` §8 is the
build order these items follow.

Everything the 2026-11-09 version of this file listed as "designed, not built" or "half-built" is now
built, except the items below. What remains is mostly **proof**, not code.

## 1. Verification debt (the largest risk)

- **Almost no real-model run is on record.** Every agent path is proven against the scripted provider
  (`crates/harness/tests/*_e2e.rs`). The one real-model exercise was replaying tutor turns on Claude Sonnet 5.5
  with the stored subscription credential (2026-11-13); it found that model returns text written before a tool call
  as an empty or summarised `thinking` block, so an explanation sent in the same step as a question card never
  reaches the learner (since 2026-11-15 the tutor delivers such text with `message_send` and a question whose
  explanation stayed hidden is refused; proven on one real turn, see `docs/claude-subscription.md`). Still untested
  against a real model: level tagging, hunt eligibility, the "parts I already know" split, plan edits, debrief
  sorting, artifact placement, the graph builder, the researcher, **the practice coach, the problem checker, exam
  papers and marking**, and whether the measurer and planner actually skip what the learner model shows as held.
- **The UI is verified in headless Chromium only**, against a mocked Tauri bridge (thirteen `scripts/ui-check`
  cases) — never in WebKitGTK and never on a device. Plugin-view isolation likewise: Chromium, not WebKitGTK.
- **Sync is proven against local servers only.** Git against a temp bare repository, WebDAV against an
  in-process test server: no real host (GitHub, Koofr, Nextcloud), no TLS server, no phone. The mobile session
  lock ran only with an injected platform. libgit2 and OpenSSL are built from source on Android, but nothing has
  synced from a phone.
- **Secure storage is proven on Linux only** (GNOME keyring, live). The macOS/iOS keychain, Windows Credential
  Manager and Android Keystore paths compile but have never run on those systems.
- **Native plugins that run programs were tested against stand-ins.** `sandbox` never ran against a real
  Podman/Docker (none installed here; a fake runtime script stood in), and the practice answer scripts never ran
  against a real `python3` + SymPy (a stand-in `python3` did). `cas`, `repl` and `code-runner` ran for real where
  the toolchain was on `PATH`.
- **No green Android CI run is on record yet.** The bundle jobs run on every push and `release.yml` publishes what
  a push run built, but neither the release APK build nor the signing step has run. No APK has been installed on a
  device. `cargo check -p harness --target aarch64-linux-android` passes (all natives included).
- **The macOS and iOS bundles are unproven on hardware.** CI builds an ad-hoc-signed Apple Silicon DMG and an
  unsigned device IPA; no IPA has been installed and WKWebView rendering has not been checked.

## 2. Designed, not built

- **Practice, remaining pieces** (arch §4.11–4.14): problems are never imported from a past paper (the coach reads
  papers for style and marking and writes its own); no code picks question-bank problems "imported first" (only the
  coach's `library_*` tools exist); an exam timer doesn't survive an app restart until the next command or
  `practice_resume`; FSRS is not refit on the learner's own log.
- **Mobile runtime** (arch §8.7): the app has never run on Android or iOS. The iOS project is generated in CI and
  not checked in; the IPA is unsigned, so installs depend on re-signing. On mobile `pick_folder` refuses
  (tauri-plugin-dialog picks folders on desktop only), so workspaces, folder plugins, reference libraries and image
  sets can't be imported from a phone.
- **Typst artifact renderer** and a **plugin-builder agent**: rejected/optional, absent.

## 3. Half-built seams

- **Graph**: the builder and splitter have never run against a real model (so the Map's split proposals, node
  provenance quotes and the `heuristic` node kind with `attribution`/`scope` are unproven against real output).
- **Reference libraries** (arch §4.27): legacy binary Office files (`.doc`, `.xls`, `.ppt`) and Apple iWork
  packages are skipped as `unsupported`; scans have no text (no OCR — `library_page_image` shows a page to a
  vision-capable researcher, and only on the device that imported the library); search is an index over text, proven
  to equal a full scan and timed at 80 MB, untested at several hundred MB.
- **Plan-time plugin consent**: a built-in plugin that declares a permission stays off until the learner enables it
  in Settings; the planner can only name it after that.
- **Git sync on Android is fixed on desktop only.** The phone's OpenSSL can read no certificates (`no-stdio`), so
  git's HTTPS trust is decided by rustls in libgit2's certificate callback (`docs/sync-build.md`); proven on
  desktop with OpenSSL stripped of its roots, not yet on a phone. Certificates the learner installed on the phone are
  not trusted by git. WebDAV and the model clients use the same rustls trust.
- **A session file that was double-encoded** (the 2026-11-10 session `01M723XJ0N…` held `â€¦` for `…` in 73
  lines, first seen in sync commit `eaf6fef`). Traced 2026-11-14: that commit was made on the desktop and changed
  exactly the lines holding non-ASCII text, byte for byte (UTF-8 read as Windows-1252 and written back; key order
  and every ASCII line untouched), in the same commit that replaced 32 graph ops with the field-named graph being
  developed that hour. No harness path transcodes text (session and graph logs are written as bytes or from parsed
  values; the union merge keeps lines verbatim), so this was a hand-run tool on the data repo during development,
  not sync **[inference]**. The session has since been deleted; nothing was repaired.

## 4. Deliberately not built

- **Signed iOS/macOS distribution** (App Store, TestFlight, Developer ID notarization) — needs a paid Apple
  developer account.
