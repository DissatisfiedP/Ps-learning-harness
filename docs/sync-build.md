# Sync build: interfaces

Status: **built 2026-11-07.** What shipped, the deviations and what is unproven are recorded in
`docs/agent-system-plan.md` ("Decisions in the graph, sync, plan-card and CI build", and gaps 13 and 16).
Deviations from the text below: `git2` 0.21 with `vendored-libgit2` and the system OpenSSL, a `conflicts` ledger kept
in `sync.json` for the needs-attention list, and no UI yet (the commands exist; Settings does not).

**Planned, not built (decided 2026-11-08):** WebDAV as a second sync target next to git; see
[WebDAV target](#webdav-target-planned-not-built) at the end.

Source of truth for behaviour: `docs/architecture.md` §4.18 (algorithm, layout, what is not synced) and §6.4 (why `git2`).
This file fixes the interfaces.

## What sync is

The data repo (`$LEARNING_HARNESS_REPO`, default `<app data dir>/repo`) is already a plain-file store of append-only
op logs. Sync makes it a git repository with app-controlled merges, so one learner's devices stay consistent through
any host. **Device-local state never syncs**: `settings.json`, `credentials.json`, `models-cache.json`,
`workspace*.json`, `session-index.json`, `plugins/<id>/<sha256>/`, `page-images/`, staged images.

## Crate and auth

- `git2` (libgit2) with `vendored-libgit2`; HTTPS remotes use the system OpenSSL (`libssl`). Add it to
  `crates/harness/Cargo.toml`; the workspace already builds offline against the local registry cache, and CI installs
  `libssl-dev`.
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
    pub needs_attention: Vec<String>,   // paths parked in .conflicts/ and not yet acknowledged
}

pub struct SyncReport {
    pub committed: bool,
    pub pulled: u32,      // commits merged
    pub pushed: u32,      // commits pushed
    pub conflicts: Vec<ConflictPath>,  // { path, resolution: "union" | "local-wins" | "inquired" }
    pub needs_attention: Vec<String>,
}
```

## Commands (Harness methods + Tauri)

| method | args | effect |
|---|---|---|
| `sync_status(&self)` | — | reads `sync.json` + the repo (ahead/behind by counting commits; no network) |
| `set_sync(&self, remote: Option<&str>, token: Option<&str>, branch: Option<&str>)` | `{ remote, token, branch }` | saves device-local config; an empty remote disconnects sync |
| `sync_now(&self)` | — | runs the algorithm above, returns `SyncReport` |
| `acknowledge_conflicts(&self)` | — | clears `needsAttention` (the `.conflicts/` files stay in history) |

Triggers, all best effort and never blocking a turn: on `Harness::open` when configured, after `session_end` (after the
graph job is spawned), and on the command. Offline or misconfigured → a recorded `last_error` and `unsynced: true`;
the session keeps working read-write (the "not synced" state is what the UI banner reads).

## Verification

- Unit tests against a **local bare repository** created in a temp dir (no network): first sync commits and pushes;
  a second clone's commit is pulled; two devices both appending to `sessions/<yyyy>/<id>.jsonl` merge by union with no
  duplicate `opId` and no lost op; a conflicting `graph/topics.json` produces local-wins plus a `.conflicts/` file plus
  `needsAttention`; a rejected token and an unreachable remote give readable errors and leave the tree unchanged.
- `cargo test --workspace --offline`, `cargo clippy --workspace --all-targets --offline`, `scripts/ui-check/run.sh ipc placement`.

## WebDAV target (planned, not built)

Decided 2026-11-08; nothing below exists in code yet.

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

### Settings

`sync.json` (mode 0600, never synced) gains a `webdav` object next to the git fields:
`{ url, username, password, lastSyncAt, lastError }`. The password is an app password, stored like the git token and
never logged. Only `https://` URLs are accepted (plus `http://localhost` for a local test server). The data lives under
one folder at that URL, with the same relative paths as the data repo and no `.git/`. The device-local files listed in
"What sync is" are excluded exactly as for git.

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

### Client

Use `reqwest` (already a dependency, `rustls`, so no OpenSSL on any platform). The client issues `PROPFIND`, `GET`,
`PUT`, `DELETE` and `MKCOL` directly. Apache OpenDAL's `services-webdav` is the alternative if its writes support
`If-Match`; check that before choosing it, because conditional writes are what make step 3 safe.

### Sessions that used desktop-only plugins

Sync carries every session to every device. A session whose plugins don't all list `mobile` in `platforms` opens
**read-only on mobile**, with a note naming the plugin and saying to continue on desktop. It stays fully writable on
desktop. The existing `read_only` path is reused, with its own reason so the note differs from the legacy one.

The `pluginGrant` and `artifact` ops record only the plugin id, version and hash, and a desktop-only plugin is never
installed on a phone. So the plugin's `platforms` must be stored in those ops when they are written (a new field,
defaulting to absent). For older ops without it, mobile falls back to the cached registry index. **Open:** whether a
plugin found in neither place locks the session (safe) or leaves it writable.

### Verification (when built)

- Tests against a local WebDAV server in a temp dir, e.g. `rclone serve webdav`, with no network: first upload;
  pulling another device's write; both sides appending to one session merge by union; a concurrent write produces a
  412, then a re-merge and success; a deleted session stays deleted on the other device; a wrong password gives a
  readable error and leaves local files untouched.
- Git and WebDAV both on: a change made on device A with git only and device B with WebDAV only reaches both through
  a third device that has both targets, with no duplicate `opId`.
