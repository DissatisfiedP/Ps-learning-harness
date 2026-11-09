# Sync build: interfaces

Status: **git built 2026-11-07; WebDAV, the mobile lock and the sync UI built 2026-11-09.** What shipped, the
deviations and what is unproven are recorded in `docs/agent-system-plan.md` ("Decisions in the graph, sync, plan-card
and CI build", and gaps 13 and 16) and in the [WebDAV target](#webdav-target) section below.
Deviations from the git text below: `git2` 0.21 with `vendored-libgit2` and the system OpenSSL (built from source on
Android), and a `conflicts` ledger kept in `sync.json` for the needs-attention list.

UI (owner decision 2026-11-09): a **Sync** section in Settings after Providers (Git and WebDAV groups, each with its own
status and Disconnect, one "Sync now", the needs-attention list with "Acknowledge"), a status button plus "Sync now" in
the Home header, and a "not synced" / "needs a look" banner on Home only. Spec: `docs/ui-design.md` §7.1 and §7.5.2.

Source of truth for behaviour: `docs/architecture.md` §4.18 (algorithm, layout, what is not synced) and §6.4 (why `git2`).
This file fixes the interfaces.

## What sync is

The data repo (`$LEARNING_HARNESS_REPO`, default `<app data dir>/repo`) is already a plain-file store of append-only
op logs. Sync makes it a git repository with app-controlled merges, so one learner's devices stay consistent through
any host. **Device-local state never syncs**: `settings.json`, `credentials.json`, `models-cache.json`,
`workspace*.json`, `session-index.json`, `plugins/<id>/<sha256>/`, `page-images/`, staged images.

## Crate and auth

- `git2` (libgit2) with `vendored-libgit2`; HTTPS remotes use the system OpenSSL (`libssl`) on desktop and OpenSSL
  built from source (`openssl-sys/vendored`) on Android. Add it to `crates/harness/Cargo.toml`; the workspace already
  builds offline against the local registry cache, and CI installs `libssl-dev`.
- **Android trust store.** OpenSSL built from source has no CA roots on a phone: its built-in folder (`OPENSSLDIR`,
  `/usr/local/ssl` by default) doesn't exist there, so every HTTPS remote failed with "the SSL certificate is
  invalid" (fixed 2026-11-09). `.cargo/config.toml` sets `OPENSSL_CONFIG_DIR` to the app's private
  `/data/data/com.pslearningharness.app/files/ssl`, which the vendored build makes `OPENSSLDIR`. At startup, before
  anything touches libgit2, `Harness::open` concatenates Android's roots (the Conscrypt APEX,
  `/apex/com.android.conscrypt/cacerts`, then `/system/etc/security/cacerts`) into `cert.pem` there
  (`sync::install_android_trust_store`), and OpenSSL loads it when libgit2 initialises it. `git2::opts::set_ssl_cert_file`
  would be simpler but is `unsafe`, which the workspace forbids. Limits: the path is for the primary Android user
  (a work profile has its own data folder), and user-installed CAs aren't included. WebDAV doesn't use this: it
  runs on rustls with its own bundled roots, like the model clients.
- Remote: an HTTPS URL plus a personal access token, stored **device-locally** in `<config>/sync.json` (mode 0600,
  never in the repo, never synced): `{ remote, token, branch, lastSyncAt, lastError }`. SSH keys are out of scope.
- The repo's `.gitattributes` is written on first use when missing: `*.jsonl merge=union` (sessions, graph ops,
  frontier) so a learner running `git` by hand still gets sane merges.

## Algorithm (one `sync()` run, single-flight)

1. Ensure the data repo is a git repository (init on first use, branch from settings, default `main`), set the remote,
   and write `.gitattributes` if absent.
2. `git add -A` + commit local changes when anything is dirty (`sync: <ISO timestamp>`), authored as the learner's
   configured name or `learning-harness` / `learning-harness@localhost`.
3. `fetch` the remote branch. Nothing new → push if ahead, done.
4. `merge` the fetched commit into the local branch.
5. For **every conflicted path**, resolve by type:
   - **`*.jsonl`**: union merge in code — read both sides' lines, drop unparseable lines, deduplicate by `opId` (an op
     with no `opId`, e.g. a hand-written line, is keyed by its whole line), sort by `(at, opId)` when both exist, write
     the result, and stage it. Readers already sort, so line order is cosmetic.
   - **anything under `graph/`** that is JSON (e.g. `topics.json`): last-writer-wins is not safe here — write the local
     version in place and the remote version to `.conflicts/<path>.<timestamp>`, record the path in `needsAttention`.
   - **`.md` files** (learner prompt overrides): `updatedAt` is not present, so: local wins, remote to `.conflicts/`,
     record `needsAttention`.
   - **anything else**: local wins, remote to `.conflicts/`, record `needsAttention`.
6. Commit the merge, then `push`. Leave the merge commit's message in the repo (`sync: merge <short sha>`).
7. Return and persist a `SyncReport`.

Merge failures that are not content conflicts (auth, network, diverged histories, a dirty index) return a readable
error and leave the repo untouched; the local working tree must never be left mid-merge (abort on error).

## Python-free, panic-free

Every failure path returns `Error`/`SyncError` with a sentence a learner can act on ("the token was rejected",
"the network is unreachable", "the remote has unrelated history"). No unwraps on git2 results that can fail for
learner reasons.

## Types (serialized to the UI)

```rust
pub struct SyncStatus {
    pub configured: bool,
    pub remote: Option<String>,
    pub branch: String,
    pub last_sync_at: Option<String>,   // ISO
    pub last_error: Option<String>,     // last failure, kept until a success
    pub unsynced: bool,                 // dirty tree or ahead of the remote
    pub ahead: u32,
    pub behind: u32,
    pub needs_attention: Vec<String>,   // paths parked in .conflicts/ and not yet acknowledged (both targets)
    pub has_token: bool,                // a git token is saved (never the token itself)
    pub webdav: WebdavStatus,           // { configured, url, username, hasPassword, lastSyncAt, lastError, unsynced }
}

pub struct SyncReport {
    pub committed: bool,
    pub pulled: u32,      // commits merged
    pub pushed: u32,      // commits pushed
    pub conflicts: Vec<ConflictPath>,  // { path, resolution: "union" | "local-wins" | "inquired" }
    pub needs_attention: Vec<String>,
    pub webdav: Option<WebdavReport>,  // { uploaded, downloaded, merged, deletedRemote, deletedLocal, skipped }
}
```

## Commands (Harness methods + Tauri)

| method | args | effect |
|---|---|---|
| `sync_status(&self)` | — | reads `sync.json` + the repo (ahead/behind by counting commits; no network) |
| `set_sync(&self, remote: Option<&str>, token: Option<&str>, branch: Option<&str>)` | `{ remote, token, branch }` | saves device-local config; an empty remote disconnects sync |
| `sync_now(&self)` | — | runs git (if configured), then WebDAV (if configured), single-flight; returns `SyncReport`, or an error naming every failed target |
| `set_webdav(&self, url: Option<&str>, username: Option<&str>, password: Option<&str>)` | `{ url, username, password }` | saves the WebDAV settings; an empty url disconnects WebDAV and deletes the manifest; `None` keeps a saved value, `""` clears it |
| `acknowledge_conflicts(&self)` | — | clears `needsAttention` (the `.conflicts/` files stay in history) |
| `sync_if_changed(&self) -> bool` | — | starts a background run only when a configured target has local changes (stat/status checks, no network); returns whether one started. The UI calls it when the app goes to the background |
| `sync_before_exit(&self, deadline)` | — (app only) | on desktop close: waits for a run in flight or starts one when there are local changes, blocking at most `deadline` (15 s); returns whether it finished |
| `mobile_unlock(&self, session_id)` | `{ sessionId }` | appends `mobileUnlock` to a session locked on this phone for a desktop-only plugin; returns the reloaded session |

Triggers, all best effort and never blocking a turn (each runs git, then WebDAV, under the one single-flight flag):
- on `Harness::open` when a target is configured;
- after `session_end` (after the graph job is spawned);
- every 20 minutes (`sync::PERIODIC_INTERVAL`) **only when there is local progress**: a configured git target with a
  dirty tree or unpushed commits, or a configured WebDAV target whose stat-only check finds a file the manifest doesn't
  match (`Sync::trigger_if_changed`, no network; remote-only changes don't count, launch and the button cover pulls).
  The timer thread holds a `Weak` reference and ends when the Harness is dropped;
- before the app closes on desktop: a learner-initiated `RunEvent::ExitRequested` (`code: None`) is prevented, the
  windows are hidden, `Sync::sync_before_exit` waits at most 15 s (`sync::EXIT_DEADLINE`) for a run in flight or
  starts one when there is local progress (returning at once when there's nothing to sync), then the app exits with
  `AppHandle::exit(0)`. That second request carries a code and passes through, and `RunEvent::Exit` still restores
  focus mode. A run cut off by the deadline records "the sync before closing did not finish in time; it runs again at
  the next launch" on the targets it hadn't finished;
- when the app goes to the background (mobile has no close): the UI calls `sync_if_changed` on
  `document.visibilitychange` → hidden. Tauri 2.12 has no suspend/background `RunEvent` on Android, so this is the
  webview's event; it is non-blocking and does nothing without local progress;
- the Sync now button (`sync_now`).

Offline or misconfigured → a recorded `last_error` on that target and `unsynced: true`; the session keeps working
read-write (the "not synced" state is what the Home banner reads).

## Verification

- Unit tests against a **local bare repository** created in a temp dir (no network): first sync commits and pushes;
  a second clone's commit is pulled; two devices both appending to `sessions/<yyyy>/<id>.jsonl` merge by union with no
  duplicate `opId` and no lost op; a conflicting `graph/topics.json` produces local-wins plus a `.conflicts/` file plus
  `needsAttention`; a rejected token and an unreachable remote give readable errors and leave the tree unchanged.
- `cargo test --workspace --offline`, `cargo clippy --workspace --all-targets --offline`, `scripts/ui-check/run.sh ipc placement sync`.

## WebDAV target

Decided 2026-11-08, built 2026-11-09: the engine is `crates/harness/src/sync/webdav.rs`, the orchestration lives in
`sync.rs` (`Sync::sync`/`trigger`/`status`/`set_webdav`), and the Tauri command is `set_webdav`. Deviations from the
plan below: the tests run against an in-process WebDAV server instead of `rclone serve webdav`; the manifest also keeps
each file's size, mtime and a "racy" flag so unchanged files aren't re-hashed (details under "As built"); a server that
reports no ETag gets a version made from `getlastmodified` + `getcontentlength` (changes are still detected, but writes
to it can't be conditional); and a pass that had to leave a path behind counts as a failed WebDAV run.

### Why

Git needs a git host and a personal access token, and libgit2's HTTPS transport needs OpenSSL, which Android doesn't
ship. WebDAV is a plain file protocol that free storage already speaks: [Koofr](https://koofr.eu/) gives 10 GB free with
per-app passwords, and a self-hosted Nextcloud works the same way. Joplin offers the same family of targets
(WebDAV, Nextcloud, S3, Dropbox, OneDrive). The data is small: one real session is 264 KB, so even 250 MB is roughly a
thousand sessions of that size.

### Git and WebDAV are independent

Each device chooses **none, git, WebDAV, or both**. Each target has its own device-local settings, status and last
error, and either can be turned off without touching the other.

Running both is safe because every merge here is a union: `*.jsonl` lines are deduplicated by `opId`, and the other
file types follow the step-5 rules. Data that arrives through one target and leaves through the other converges to the
same files, whatever the order. One `sync()` run stays single-flight and runs the configured targets in a fixed order
(git, then WebDAV) so the status reads predictably. A failure in one target is recorded on that target and doesn't stop
the other. Conflict copies from either target go to the same `.conflicts/` folder and `needsAttention` list.

As built: `sync_now` fails with "sync is not configured" when neither target is; otherwise it returns `Ok` only when
every configured target succeeded, and an `Err` joins the messages (`"<git message>; WebDAV: <message>"`; a lone
WebDAV failure is just its message). The background triggers (launch, after `session_end`) fire when either target is
configured. A WebDAV pass writes files into the working tree; git commits them on its next run, which is safe because
the merges are unions. Disconnecting git no longer clears the shared needs-attention list while WebDAV is still
configured, and the other way round.

### Settings

`sync.json` (mode 0600, never synced) gains a `webdav` object next to the git fields:
`{ url, username, password, lastSyncAt, lastError }` (absent in older files, which still load). The password is an
app password, stored like the git token and never logged; `WebdavConfig`'s `Debug` hides it and every stored or
returned message is scrubbed of it. Only `https://` URLs are accepted, plus `http://` on `localhost`, `127.0.0.1`
and `[::1]` (any port) for a local test server; a URL with a username or password in it is refused with a sentence.
The URL is saved normalised (no query or fragment, ending with `/`). The data lives under one folder at that URL,
with the same relative paths as the data repo and no `.git/`; `.conflicts/` syncs like any folder. The device-local
files listed in "What sync is" are excluded exactly as for git (they live in `<config>`, not the repo).

`set_webdav { url, username, password }` returns the `SyncStatus`. A blank URL disconnects WebDAV: the URL, username,
password and error are cleared and `<config>/webdav-state.json` is deleted. An absent username or password keeps the
saved one and `""` clears it. A new URL resets the manifest and `lastSyncAt`. `SyncStatus` gains `hasToken` (git) and
`webdav: { configured, url, username, hasPassword, lastSyncAt, lastError, unsynced }`, where `unsynced` means
configured and either never synced or a local file differs from (or is missing in) the manifest; no network. It is
stat-only so the UI can poll it: a matching size and mtime counts as clean, and only a file whose stat moved is hashed.
`SyncReport.webdav` carries `{ uploaded, downloaded, merged, deletedRemote, deletedLocal, skipped }`.

### Algorithm (one WebDAV pass)

WebDAV has no history, so the pass is a three-way comparison against a device-local manifest,
`<config>/webdav-state.json`. For every path it holds the remote ETag and the local content hash recorded at the last
successful sync.

1. List the remote folder with `PROPFIND` (`Depth: 1`, walked recursively; many servers refuse `Depth: infinity`),
   keeping each file's ETag.
2. Per path, compare local (hash vs manifest) with remote (ETag vs manifest):
   - **only local changed or new:** `PUT` with `If-Match: <manifest ETag>`, or `If-None-Match: *` for a new file;
   - **only remote changed or new:** `GET` and write locally;
   - **both changed:** `GET`, merge by file type (same rules as git step 5), write locally, then `PUT` with
     `If-Match: <the ETag just read>`;
   - **gone locally, still in the manifest, remote unchanged:** `DELETE` remote with `If-Match` (deleted sessions
     must not come back);
   - **gone remotely, still in the manifest, local unchanged:** delete locally;
   - **gone on one side but changed on the other:** keep the changed copy (a change beats a delete).
3. A `412 Precondition Failed` means another device wrote in between: re-read, re-merge and retry (bounded, e.g.
   three times), then record an error and leave the file for the next run.
4. Create missing folders with `MKCOL`. Write the manifest only for paths that succeeded, so an interrupted pass
   resumes instead of redoing everything or losing a change.

As built, beyond the steps above:

- The manifest records the folder URL it was written for, so a manifest from another URL is ignored. It is
  flushed every 25 changed entries and at the end of the pass, including a pass that stopped on an error. A missing
  or unreadable manifest starts over: every path that exists on both sides compares bytes once.
- Each entry keeps the size and mtime of the bytes its hash describes (stat'ed after a download is written; left
  unknown if a turn appended in between). The pass trusts a matching stat only once it is no longer "racy": an entry
  is racy until the manifest is written at least 2 s after its mtime (git's racy-clean rule), so a same-tick write
  can't hide behind it. Racy entries are hashed again by the pass; `status()` treats them as clean.
- New on both sides with identical bytes: just recorded, no conflict. Different bytes: the both-changed merge.
- Both changed, `*.jsonl`: `union_jsonl` (the git step 5 code). Anything else: local wins, the remote bytes go to
  `.conflicts/<path>.<stamp>` (uploaded on the next pass) and the path joins `needsAttention` through the same list
  the git run uses.
- A learner write between the decision and the local write (a turn appending while sync runs) turns a download into a
  merge, and restarts a merge, so no appended op is lost. Local files are written in place, like the git merge.
- `412` on `PUT` or `DELETE`: re-read, re-merge, retry, at most three retries. Then the path is skipped (counted in
  `skipped`), the other paths continue, and `lastError` reads "1 file was left for the next run: …".
- `DELETE` answered with `412` means the file changed remotely: it comes back locally (a change beats a delete).
- A missing sync folder is created with `MKCOL`, its parents first when the server answers `409`; `405` ("already
  exists") counts as success. Paths with `..`, empty segments, backslashes or a `.git` segment are never read or
  written, and nothing outside the data repo is touched.
- `PROPFIND` hrefs may be absolute paths, full URLs (any host) or relative, with any namespace prefix and
  percent-encoding; they are compared percent-decoded against the folder's path.

### Client

`reqwest` 0.12 (already a dependency, `rustls`, so no OpenSSL on any platform), async without the `blocking` feature:
each pass builds a current-thread tokio runtime on its own thread, so it is safe from a Tauri blocking task. The client
issues `PROPFIND`, `GET`, `PUT`, `DELETE` and `MKCOL` directly with Basic auth, a 15 s connect timeout and a 120 s
request timeout. OpenDAL was not used. Errors are sentences: `401` (and `403` on the sync folder) → "the WebDAV
username or app password was rejected", connect/DNS failures → "the WebDAV server is unreachable; check the network
and the URL", a certificate failure, a timeout and `507` have their own sentences, and any other status names the
status and the path.

### Sessions on mobile: desktop-only plugins and attached folders

Built 2026-11-09: the rule lives in `crates/harness/src/mobile.rs`; the turn-time refusal and the tutor's device lines
in `agent.rs`; the Tauri command is `mobile_unlock`. Deviations are listed at the end of this subsection.

Sync carries every session to every device. On mobile a session **stays writable** unless a plugin that doesn't run
on phones has actually been used. Planning a section with such a plugin, granting it, or asking for it
(`plugin_request`) is not use.

**When it locks.** On mobile, `load_session` returns a Learn session read-only, and every write path (`learn_ops`:
messages, answers, approvals, hunts, plan edits, view states) refuses it, when either:
- the log shows the tutor already used a plugin whose `platforms` lacks `mobile`: a tutor tool call (planner and
  librarian calls never count) or an `artifact` op of it; or
- on the phone, the tutor calls such a plugin's tool. The call is recorded, answered with the tool error
  "`<name>` refused: `<plugin id>` only runs on desktop; teach without it", and the turn goes on, so the tutor writes
  its reply. The recorded call locks the session once the turn ends.

A job that finishes into a locked session (a research job the phone started in the turn that locked it) records its
end op as usual but is not delivered and wakes no tutor turn (`flush_deliveries` in `jobs.rs`); its result stays
queued, and the next turn delivers it, on desktop or after "Continue here anyway".

`SessionSummary` gains `readOnlyReason` (`"legacy"` for the existing rule, `"desktop-plugin"` for this one, `null` when
writable) and `desktopOnlyPlugins` (display names: the registry index title, else the installed title, else the id).
`readOnly` stays the one boolean every caller reads. Desktop is never affected.

**The session list.** On mobile, `list_sessions` applies the same rule, so a locked session is listed locked (meta
"Locked", dimmed row; `docs/ui-design.md` §7.1) before it is opened. `list` itself still reads only each file's first
two lines (M7); the lock comes from a device-local cache, `<config>/session-locks.json` (never in the repo), keyed by
session id. Each entry holds the file's length and mtime and the registry-independent facts: the plugin uses (the
recorded id and `platforms`, or, for an older call op without `plugin`, its tool name) and the ids released by
`mobileUnlock` ops. An entry is rebuilt (one uncached read of that file, through the same parser) only when the length
or mtime changed, and only for Learn sessions that aren't legacy read-only; any append, `mobileUnlock` included,
therefore refreshes it. The installed plugins and cached indexes are applied at list time, so a registry refresh
changes the result (and the names) without a rebuild. Entries of deleted sessions are dropped, on `delete_session`
and on the next list. A failed cache write only costs a rebuild, as with the title index. Desktop reads and writes no
cache.

**How the phone knows.** Plugin tool calls record `plugin` (the id) and `platforms`, and `artifact` ops record
`platforms`, when they are written (optional fields, absent on older ops). Each op is judged by its own recorded
`platforms` first, then by the cached registry indexes (`<config>/registry-cache/`, read only, never fetched). An older
tool op without `plugin` is matched to a plugin by an installed plugin declaring the tool, else by the index entry
whose `<id>_` prefix (dashes as underscores) the tool name starts with; the tutor's own tool names never match. A
plugin found in neither place, or listed with no platforms, **does not lock**, and calling it on the phone still fails
as before ("isn't available in the teach phase"). At write time the stamp comes from the installed manifest, else an
earlier op of the same session that recorded it, else the index.

**Override.** The lock note's "Continue here anyway" calls `mobile_unlock { sessionId }`, which appends a
`mobileUnlock` op naming the plugins that locked the session (`{ plugins: [id] }`) and returns the reloaded session,
the same shape as `load_session` (the Tauri command then restarts unfinished jobs, as opening does). It is refused
unless the session is currently locked for a desktop-only plugin on this device. After it, on mobile:
- the session is writable;
- the desktop-only plugins this session used are left out of the tutor's tool list and plugin fragments, and the
  pinned session state gets a `## This device` section with one line naming them (title and id) and telling the tutor
  to teach without them;
- their figures show the static SVG fallback, and an answer-mode view from one of them stays unanswerable: a plugin
  that isn't installed on the phone already showed only the fallback with "Plugin missing or off"; one that is
  installed but doesn't list `mobile` now does the same (`PluginInfo.onPlatform`, false there), with "This figure's
  plugin doesn't run on this device", and the tutor's `question_ask` refuses a view from it;
- a later attempt to call one of them is refused the same way but doesn't lock again.

The op is synced, so the learner decides once for every phone and tablet; replay and desktop ignore it. A
desktop-only plugin first used *after* the unlock is not covered by it and locks the session again.

**Attached folders never lock.** The folder's absolute path is device-local (`<config>/workspaces.json`); the log
records only its name. On a device without the mapping the session has no folder: the `workspace` prompt flag is off,
and `workspace_write` (not offered then) refuses with "no workspace is attached to this session" rather than "isn't
available in this phase". When the log names a folder this device has no mapping for, on any platform, the `## This
device` section carries "The folder `<name>` is attached on another device; its files aren't available here."

**Deviations.**
- Tool call ops record the plugin id as well as `platforms` (the doc named only `platforms`): an older op's tool name
  can't be mapped back to an id with dashes, and the lock note needs the id.
- `mobileUnlock` names the plugins it releases, so a desktop-only plugin first used after it locks again (the doc
  didn't say).
- The platform is the build target's (`android`/`ios` → `mobile`) and can be set with `Harness::set_platform`, which
  the tests use; the replayed `SessionState.platform` is still always `desktop`, as before this change.
- The UI re-checks the lock after a tutor turn in which a tool or artifact op listed platforms without `mobile`
  (one extra `load_session`, also on desktop, where it never locks) and re-opens the session if it is now read-only.

### Verification (when built)

- `crates/harness/src/sync/webdav_tests.rs`, against an in-process WebDAV server on `127.0.0.1:0` over a temp dir
  (PROPFIND Depth 0/1 with `infinity` refused, GET, PUT/DELETE honouring `If-Match`/`If-None-Match`, MKCOL, Basic auth,
  an ETag that changes on every write, hrefs as both absolute paths and full URLs, a hook that writes between a GET and
  the next PUT); no network, no external binary. Covered: first upload creates the missing (percent-encoded) sync
  folder and a second device pulls byte-identical files, a third holding identical bytes records them without a
  conflict, and a later write on one device reaches the other; both sides appending to one session merge by union
  (torn line dropped, no duplicate `opId`, the union uploaded and pulled back); a concurrent write produces exactly one
  412, then a re-merge and success with every device's op kept; a path that keeps changing is skipped after three
  retries while the other paths sync, with a readable `lastError`, and recovers on the next run; a deleted session
  stays deleted on the other device and across later runs, and a change beats a delete; a wrong password gives "the
  WebDAV username or app password was rejected", leaves local files byte-identical and never appears in the error,
  the status JSON or `Debug`; a `graph/topics.json` conflict keeps local, parks the remote copy in `.conflicts/`
  (uploaded on the next pass) and lists the path in `needsAttention`; `set_webdav` refuses non-https, non-loopback
  http, malformed and credential-carrying URLs, keeps the password when none is given, clears it on `""`, resets the
  manifest on a new URL, and forgets everything on disconnect. Unit tests cover href resolution, multistatus parsing
  with prefixes and entities, and the path safety check.
- Git and WebDAV both on (`git_and_webdav_carry_changes_between_devices_through_a_third`): device A with git only
  (a local bare repository), device B with WebDAV only, device C with both; a change made on A and one made on B reach
  every device, all three end with identical files and no duplicate `opId`.
- Not proven: a real server (Koofr, Nextcloud) — only the in-process server has been exercised.
- Mobile lock (`crates/harness/tests/mobile_e2e.rs`, scripted provider; the "phone" is a second `Harness` on the same
  data repo with its own config dir and `set_platform("mobile")`, opened fresh after each desktop write): a session
  that only planned and granted a desktop-only plugin stays writable on a phone whose index knows the plugin; after a
  desktop call the op carries `plugin`/`platforms`, desktop stays writable, a bare phone opens it read-only
  (`desktop-plugin`, named by id), refuses `send_learner_message` and `answer` without appending, and a phone with the
  index names it by title; `mobile_unlock` makes it writable and is refused a second time; the next phone turn has no
  `bench_run` tool, the plugin's title in the prompt, refuses the call with "only runs on desktop" and stays writable;
  a fresh desktop parses the `mobileUnlock` op (no skipped lines) and still offers the tool. A call made on the phone
  is refused, the tutor's reply follows, and the session is locked after the turn. A research job started in that
  turn finishes into the locked session: its `done` op is recorded, no brief is delivered and no tutor request is
  made; after `mobile_unlock` the next turn delivers it (the test fails without the `flush_deliveries` guard). A
  plugin with unknown platforms (not installed, no index, no earlier op) fails as before and doesn't lock. An attached
  folder never locks: the phone turn's prompt names `thesis-notes` and `workspace_write` refuses with "no workspace is
  attached to this session". The session list
  (`the_phone_lists_a_locked_session_as_locked_from_a_device_local_cache`): desktop lists the used-plugin session
  writable and writes no `session-locks.json`; the phone lists it locked (`desktop-plugin`, named by id), caches it,
  and names it by title after a registry refresh without the file changing; after `mobile_unlock` it lists writable;
  after `delete_session` the cache has no entry.
  Unit tests in `mobile.rs` cover grants and `plugin_request` not locking, artifacts, recorded platforms winning over
  the index, the index fallback by tool-name prefix, empty platform lists, planner calls, and an unlock releasing only
  the plugins it names. The desk note and "Continue here anyway" (unlock → writable, a turn with a desktop-only op →
  re-opened locked, the legacy note unchanged) were checked in headless Chromium against a mocked backend, and the
  Home list at 390 px against the ui-check fake bridge: the locked row reads "Locked · ⟨time⟩" with a greyed title,
  stays enabled, and opens to the lock note; the legacy row still reads "Read-only".
- Not proven on mobile: no Android/iOS build ran these paths; the mobile rules ran only through `set_platform`.
