# Sync build: interfaces

Status: **built 2026-11-07.** What shipped, the deviations and what is unproven are recorded in
`docs/agent-system-plan.md` ("Decisions in the graph, sync, plan-card and CI build", and gaps 13 and 16).
Deviations from the text below: `git2` 0.21 with `vendored-libgit2` and the system OpenSSL, a `conflicts` ledger kept
in `sync.json` for the needs-attention list, and no UI yet (the commands exist; Settings does not).

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
