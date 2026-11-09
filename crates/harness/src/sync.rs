//! Git sync of the data repo (docs/sync-build.md, architecture §4.18/§6.4).
//!
//! The data repo is a plain directory of append-only op logs; sync turns it
//! into a git repository whose merges the app controls, so one learner's
//! devices stay consistent through any host. Every run is the same six steps:
//! commit local changes, fetch, merge, resolve conflicts by file type, commit
//! the merge, push.
//!
//! Device-local state never enters the repo: the remote URL and the personal
//! access token live in `<config>/sync.json` (mode 0600) and are handed to
//! libgit2 through its credentials callback, so neither a commit message nor
//! `.git/config` nor a log line ever carries the token.
//!
//! A run is single-flight. Triggers (launch, after a session ends, the
//! `sync_now` command) are best effort: a second trigger while one is running
//! returns immediately, and every failure is kept as [`SyncStatus::last_error`]
//! for the UI's "not synced" banner instead of interrupting a turn.
//!
//! Failure safety: merges are built in memory (tree-level `merge_commits`), so
//! no `MERGE_HEAD` or half-merged index is ever left behind. On any error the
//! index is reset to `HEAD` and the working tree is left byte-identical; the
//! learner keeps working and the next run commits what accumulated.
//!
//! A second, independent target is WebDAV ([`webdav`]): a plain file protocol
//! that free storage speaks. Each device configures none, git, WebDAV or both;
//! one run does git, then WebDAV, under the same single-flight flag, and a
//! failure in one is recorded on that target without stopping the other.
//! Every merge is a union or local-wins, so data crossing from one target to
//! the other converges whatever the order.

mod webdav;
#[cfg(test)]
mod webdav_tests;

pub use webdav::{WebdavConfig, WebdavReport, WebdavStatus};

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use git2::{
    AutotagOption, Cred, CredentialType, ErrorClass, ErrorCode, FetchOptions, Index,
    IndexAddOption, IndexConflict, IndexEntry, IndexTime, Oid, PushOptions, RemoteCallbacks,
    Repository, RepositoryInitOptions, Signature, StatusOptions, Tree,
};
use serde::{Deserialize, Serialize};

use crate::{lock, now_iso, Error};

/// Device-local sync settings, `<config>/sync.json` (mode 0600).
const CONFIG_FILE: &str = "sync.json";
/// The one remote every sync uses.
const REMOTE: &str = "origin";
/// Written on first use so a learner running `git` by hand still gets sane
/// merges (docs/sync-build.md).
const GITATTRIBUTES: &str = "*.jsonl merge=union\n";
const DEFAULT_BRANCH: &str = "main";
/// Commit identity when the repo has no `user.name`/`user.email` of its own.
const DEFAULT_NAME: &str = "learning-harness";
const DEFAULT_EMAIL: &str = "learning-harness@localhost";
/// `sync.json` keeps at most this many unacknowledged conflict paths.
const MAX_CONFLICTS: usize = 100;
/// How often the background timer looks for local progress to sync.
pub const PERIODIC_INTERVAL: Duration = Duration::from_secs(20 * 60);
/// How long closing the app waits for its last sync.
pub const EXIT_DEADLINE: Duration = Duration::from_secs(15);
/// Which part of a run is in flight, for a deadline that cuts it short.
/// Changed under the settings lock, together with the outcome it records.
const STAGE_IDLE: u8 = 0;
const STAGE_GIT: u8 = 1;
const STAGE_WEBDAV: u8 = 2;
const UNFINISHED: &str =
    "the sync before closing did not finish in time; it runs again at the next launch";

fn default_branch() -> String {
    DEFAULT_BRANCH.to_owned()
}

/// `sync.json`: everything sync needs that must never sync itself. `Debug`
/// hides the token.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncConfig {
    /// The HTTPS URL, without any credentials in it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    /// Personal access token; never written into the repo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default = "default_branch")]
    pub branch: String,
    /// ISO time of the last successful run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync_at: Option<String>,
    /// The last failure, kept until a run succeeds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Conflict copies written to `.conflicts/` that the learner has not
    /// acknowledged yet (`acknowledge_conflicts` clears them).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<String>,
    /// The WebDAV target's settings; absent from older files.
    #[serde(default, skip_serializing_if = "WebdavConfig::is_unset")]
    pub webdav: WebdavConfig,
}

impl std::fmt::Debug for SyncConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncConfig")
            .field("remote", &self.remote)
            .field("token", &self.token.as_ref().map(|_| "[token removed]"))
            .field("branch", &self.branch)
            .field("last_sync_at", &self.last_sync_at)
            .field("last_error", &self.last_error)
            .field("conflicts", &self.conflicts)
            .field("webdav", &self.webdav)
            .finish()
    }
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            remote: None,
            token: None,
            branch: default_branch(),
            last_sync_at: None,
            last_error: None,
            conflicts: Vec::new(),
            webdav: WebdavConfig::default(),
        }
    }
}

impl SyncConfig {
    pub fn configured(&self) -> bool {
        self.remote.as_deref().is_some_and(|r| !r.trim().is_empty())
    }

    /// The branch to sync, defaulting to `main` when unset or blank.
    pub fn branch(&self) -> &str {
        let branch = self.branch.trim();
        if branch.is_empty() {
            DEFAULT_BRANCH
        } else {
            branch
        }
    }
}

/// What the Settings sync section (and the unsynced banner) reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    /// The git target is configured.
    pub configured: bool,
    pub remote: Option<String>,
    pub branch: String,
    pub last_sync_at: Option<String>,
    pub last_error: Option<String>,
    /// A dirty tree or local commits the remote does not have.
    pub unsynced: bool,
    pub ahead: u32,
    pub behind: u32,
    /// Paths parked in `.conflicts/` and not yet acknowledged, from either
    /// target.
    pub needs_attention: Vec<String>,
    /// A git token is saved (never the token itself).
    pub has_token: bool,
    pub webdav: WebdavStatus,
}

/// One path the merge could not combine, and how it was resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictPath {
    pub path: String,
    /// `union` (`*.jsonl`) or `local-wins` (everything else).
    pub resolution: String,
}

/// What one `sync()` run did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncReport {
    pub committed: bool,
    /// Commits merged in from the remote.
    pub pulled: u32,
    /// Commits sent to the remote.
    pub pushed: u32,
    pub conflicts: Vec<ConflictPath>,
    /// Conflict paths from this run that need the learner's attention.
    pub needs_attention: Vec<String>,
    /// What the WebDAV pass did, when that target ran and succeeded.
    pub webdav: Option<WebdavReport>,
}

/// The sync engine: device-local settings plus the one data repo.
pub struct Sync {
    repo: PathBuf,
    config_dir: PathBuf,
    state: Mutex<SyncConfig>,
    running: AtomicBool,
    /// `STAGE_*`: what the run in flight is doing.
    stage: AtomicU8,
    /// The periodic timer's wake-up channel. Dropping the engine drops the
    /// sender, which ends the timer thread.
    ticker: OnceLock<mpsc::Sender<()>>,
}

impl Sync {
    /// Loads `sync.json`; cheap, and never touches the network. A corrupt or
    /// unreadable file leaves sync unconfigured with a readable `lastError`
    /// instead of blocking the app from opening.
    pub fn new(repo: &Path, config_dir: &Path) -> Self {
        let state: SyncConfig = match crate::config::read_json(&config_dir.join(CONFIG_FILE)) {
            Ok(state) => state,
            Err(e) => {
                tracing::warn!("sync settings unreadable: {e}");
                SyncConfig {
                    last_error: Some(
                        "the saved sync settings could not be read; set the remote again".into(),
                    ),
                    ..SyncConfig::default()
                }
            }
        };
        Self {
            repo: repo.to_owned(),
            config_dir: config_dir.to_owned(),
            state: Mutex::new(state),
            running: AtomicBool::new(false),
            stage: AtomicU8::new(STAGE_IDLE),
            ticker: OnceLock::new(),
        }
    }

    pub fn config(&self) -> SyncConfig {
        lock(&self.state).clone()
    }

    fn save(&self, config: &SyncConfig) -> Result<(), Error> {
        crate::config::write_json(&self.config_dir.join(CONFIG_FILE), config, true)
    }

    /// Change the settings under the lock, then write the result. Mutating in
    /// place (rather than replacing a stale clone) keeps a background run from
    /// dropping a token the learner just saved.
    fn update<F>(&self, change: F) -> Result<(), Error>
    where
        F: FnOnce(&mut SyncConfig),
    {
        let snapshot = {
            let mut config = lock(&self.state);
            change(&mut config);
            config.clone()
        };
        self.save(&snapshot)
    }

    /// Save the device-local settings. An empty remote disconnects sync.
    ///
    /// A remote URL that carries a token is stored without it: the token moves
    /// into `sync.json` and only ever reaches git through the credentials
    /// callback.
    pub fn set(
        &self,
        remote: Option<&str>,
        token: Option<&str>,
        branch: Option<&str>,
    ) -> Result<(), Error> {
        let explicit_token = token.map(str::trim).filter(|t| !t.is_empty());
        let keep_saved = token.is_none();
        let remote = remote.map(str::trim).filter(|r| !r.is_empty());
        let branch = branch.map(str::trim).filter(|b| !b.is_empty());
        self.update(|config| {
            match remote {
                Some(remote) => {
                    let (url, embedded) = split_credentials(remote);
                    config.remote = Some(url);
                    // Keeping the saved token lets the UI change the URL or
                    // the branch without asking for the token again.
                    config.token = embedded
                        .or_else(|| explicit_token.map(str::to_owned))
                        .or_else(|| keep_saved.then(|| config.token.clone()).flatten());
                    config.last_error = None;
                }
                None => {
                    config.remote = None;
                    config.token = None;
                    // The list is shared: WebDAV may still own entries.
                    if !config.webdav.configured() {
                        config.conflicts.clear();
                    }
                    config.last_error = None;
                }
            }
            if let Some(branch) = branch {
                config.branch = branch.to_owned();
            }
        })
    }

    /// Save the WebDAV settings (docs/sync-build.md, "Settings").
    ///
    /// A blank URL disconnects WebDAV: the URL, username, password, error and
    /// the manifest are forgotten. Otherwise the URL must be `https://`, or
    /// `http://` on the loopback host. A `None` username or password keeps
    /// the saved one and `""` clears it. A new URL starts the manifest over,
    /// since what it recorded belongs to the old folder.
    pub fn set_webdav(
        &self,
        url: Option<&str>,
        username: Option<&str>,
        password: Option<&str>,
    ) -> Result<(), Error> {
        let url = match url.map(str::trim).filter(|url| !url.is_empty()) {
            Some(raw) => Some(webdav::normalise_url(raw)?.to_string()),
            None => None,
        };
        let mut reset = false;
        self.update(|config| match url {
            Some(url) => {
                let dav = &mut config.webdav;
                if dav.url.as_deref() != Some(url.as_str()) {
                    reset = true;
                    dav.last_sync_at = None;
                }
                dav.url = Some(url);
                if let Some(username) = username {
                    dav.username = Some(username.trim().to_owned()).filter(|u| !u.is_empty());
                }
                if let Some(password) = password {
                    dav.password = Some(password.to_owned()).filter(|p| !p.is_empty());
                }
                dav.last_error = None;
            }
            None => {
                reset = true;
                config.webdav = WebdavConfig::default();
                if !config.configured() {
                    config.conflicts.clear();
                }
            }
        })?;
        if reset {
            webdav::delete_manifest(&self.config_dir);
        }
        Ok(())
    }

    /// Clears the "needs attention" list. The `.conflicts/` copies stay in the
    /// repo and its history.
    pub fn acknowledge(&self) -> Result<(), Error> {
        if self.config().conflicts.is_empty() {
            return Ok(());
        }
        self.update(|config| config.conflicts.clear())
    }

    /// Configuration plus the repo's state, with **no** network access: the
    /// ahead/behind counts come from the remote-tracking ref the last fetch or
    /// push left behind, and WebDAV's `unsynced` from its manifest.
    pub fn status(&self) -> Result<SyncStatus, Error> {
        let config = self.config();
        let branch = config.branch().to_owned();
        let (ahead, behind, unsynced) = match Repository::open(&self.repo) {
            Ok(repo) => counts(&repo, &branch)?,
            // Not a repository yet: configured but never synced.
            Err(_) => (0, 0, config.configured()),
        };
        let dav = &config.webdav;
        let webdav = WebdavStatus {
            configured: dav.configured(),
            url: dav.url.clone(),
            username: dav.username.clone(),
            has_password: dav.password.as_deref().is_some_and(|p| !p.is_empty()),
            last_sync_at: dav.last_sync_at.clone(),
            last_error: dav.last_error.clone(),
            unsynced: self.webdav_unsynced(dav),
        };
        Ok(SyncStatus {
            configured: config.configured(),
            has_token: config.token.as_deref().is_some_and(|t| !t.is_empty()),
            remote: config.remote,
            branch,
            last_sync_at: config.last_sync_at,
            last_error: config.last_error,
            unsynced,
            ahead,
            behind,
            needs_attention: config.conflicts,
            webdav,
        })
    }

    /// WebDAV is configured and a local file differs from its manifest (or it
    /// never synced). Stat-only, no network.
    fn webdav_unsynced(&self, dav: &WebdavConfig) -> bool {
        dav.configured()
            && (dav.last_sync_at.is_none()
                || webdav::local_changes(
                    &self.repo,
                    &self.config_dir,
                    dav.url.as_deref().unwrap_or_default(),
                ))
    }

    /// A configured target has local progress since its last successful
    /// sync: git has a dirty tree or unpushed commits, or WebDAV has a file
    /// its manifest doesn't match. Remote-only changes don't count. No
    /// network; an unreadable repository counts as changed, so the run
    /// records why.
    pub fn has_local_changes(&self) -> bool {
        let config = self.config();
        let git = config.configured()
            && match Repository::open(&self.repo) {
                Ok(repo) => counts(&repo, config.branch()).map_or(true, |(_, _, dirty)| dirty),
                Err(_) => true,
            };
        git || self.webdav_unsynced(&config.webdav)
    }

    /// The periodic trigger, and the mobile one when the app goes to the
    /// background: a background run only when [`Sync::has_local_changes`].
    /// Nothing configured, nothing changed, or a run in flight: nothing
    /// happens. Returns whether a run started.
    pub fn trigger_if_changed(self: &Arc<Self>) -> bool {
        if self.running.load(Ordering::SeqCst) || !self.has_local_changes() {
            return false;
        }
        self.trigger()
    }

    /// Start the timer that calls [`Sync::trigger_if_changed`] every
    /// `interval` ([`PERIODIC_INTERVAL`] in the app). It holds only a weak
    /// reference, and ends as soon as the engine is dropped. A second call
    /// does nothing. The handle is for tests; the app lets the thread run.
    pub fn start_periodic(
        self: &Arc<Self>,
        interval: Duration,
    ) -> Option<std::thread::JoinHandle<()>> {
        let (sender, wake) = mpsc::channel::<()>();
        if self.ticker.set(sender).is_err() {
            return None;
        }
        let engine = Arc::downgrade(self);
        let spawned = std::thread::Builder::new()
            .name("lh-sync-timer".into())
            .spawn(move || loop {
                // Nothing is ever sent: a timeout is a tick, a disconnect
                // means the engine is gone.
                if wake.recv_timeout(interval) != Err(mpsc::RecvTimeoutError::Timeout) {
                    return;
                }
                let Some(engine) = engine.upgrade() else {
                    return;
                };
                engine.trigger_if_changed();
            });
        match spawned {
            Ok(handle) => Some(handle),
            Err(e) => {
                tracing::warn!("could not start the periodic sync: {e}");
                None
            }
        }
    }

    /// Closing the app: a run is in flight, or a configured target has local
    /// progress worth one last sync.
    pub fn pending_before_exit(&self) -> bool {
        self.running.load(Ordering::SeqCst) || self.has_local_changes()
    }

    /// One last sync before the app closes, waiting at most `deadline`
    /// (about [`EXIT_DEADLINE`]): a run already in flight is waited for, and
    /// otherwise one starts when there are local changes. Returns whether
    /// the run finished in time; when it didn't, the targets it hadn't
    /// finished record that in `lastError`, and the run keeps going until the
    /// process exits. Never blocks longer than `deadline` plus the local
    /// change check.
    pub fn sync_before_exit(self: &Arc<Self>, deadline: Duration) -> bool {
        let started = Instant::now();
        if !self.running.load(Ordering::SeqCst) {
            if !self.has_local_changes() {
                return true;
            }
            self.trigger();
        }
        while self.running.load(Ordering::SeqCst) {
            let elapsed = started.elapsed();
            if elapsed >= deadline {
                self.record_unfinished();
                return false;
            }
            std::thread::sleep((deadline - elapsed).min(Duration::from_millis(50)));
        }
        true
    }

    /// The deadline cut a run short: say so on every target it hadn't
    /// finished. The stage is read under the settings lock, where the run
    /// also records its outcomes, so a target that just finished keeps its
    /// real result.
    fn record_unfinished(&self) {
        let saved = self.update(|config| {
            let stage = self.stage.load(Ordering::SeqCst);
            if stage == STAGE_GIT && config.configured() {
                config.last_error = Some(UNFINISHED.into());
            }
            if stage != STAGE_IDLE && config.webdav.configured() {
                config.webdav.last_error = Some(UNFINISHED.into());
            }
        });
        if let Err(e) = saved {
            tracing::warn!("sync settings not saved: {e}");
        }
    }

    /// One full run: git (commit, fetch, merge, resolve, commit the merge,
    /// push) when configured, then a WebDAV pass when configured.
    ///
    /// Each target's failure is kept on that target and doesn't stop the
    /// other; the run is `Ok` only when every configured target succeeded.
    /// A second call while a run is in flight returns a readable error; the
    /// background [`Sync::trigger`] just skips it.
    pub fn sync(&self) -> Result<SyncReport, Error> {
        let config = self.config();
        if !config.configured() && !config.webdav.configured() {
            return Err(Error::Sync("sync is not configured".into()));
        }
        let flight = Flight::acquire(&self.running)?;
        let result = self.run_targets();
        drop(flight);
        result
    }

    /// Best-effort background run: never blocks the caller, and a trigger while
    /// a run is in flight is dropped. Nothing happens while unconfigured.
    /// Returns whether a run started.
    pub fn trigger(self: &Arc<Self>) -> bool {
        let config = self.config();
        if !config.configured() && !config.webdav.configured() {
            return false;
        }
        if self
            .running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return false;
        }
        let me = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("lh-sync".into())
            .spawn(move || {
                let flight = Flight(&me.running);
                let result = me.run_targets();
                drop(flight);
                if let Err(e) = result {
                    tracing::debug!("background sync: {e}");
                }
            });
        if spawned.is_err() {
            self.running.store(false, Ordering::SeqCst);
            return false;
        }
        true
    }

    /// Git, then WebDAV, in that fixed order so the status reads predictably.
    /// The caller holds the single-flight flag.
    fn run_targets(&self) -> Result<SyncReport, Error> {
        let git = self.config().configured();
        let mut report = SyncReport::default();
        let mut errors: Vec<String> = Vec::new();
        self.stage
            .store(if git { STAGE_GIT } else { STAGE_WEBDAV }, Ordering::SeqCst);
        if git {
            match self.finish(self.once()) {
                Ok(done) => report = done,
                Err(e) => errors.push(e.to_string()),
            }
        }
        // Read again: the learner may have changed WebDAV while git ran.
        let dav = self.config().webdav;
        if dav.configured() {
            let result = webdav::run(&self.repo, &self.config_dir, &dav);
            match self.finish_webdav(&dav, result) {
                Ok(outcome) => {
                    for path in outcome.needs_attention {
                        if !report.needs_attention.contains(&path) {
                            report.needs_attention.push(path);
                        }
                    }
                    report.conflicts.extend(outcome.conflicts);
                    report.webdav = Some(outcome.report);
                }
                Err(e) if git => errors.push(format!("WebDAV: {e}")),
                Err(e) => errors.push(e.to_string()),
            }
        }
        self.stage.store(STAGE_IDLE, Ordering::SeqCst);
        if errors.is_empty() {
            Ok(report)
        } else {
            Err(Error::Sync(errors.join("; ")))
        }
    }

    /// Persist what the git run produced (success time or the error) and hand
    /// the result back unchanged.
    fn finish(&self, result: Result<SyncReport, Error>) -> Result<SyncReport, Error> {
        let outcome = self.update(|config| match &result {
            Ok(report) => {
                self.stage.store(STAGE_WEBDAV, Ordering::SeqCst);
                config.last_sync_at = Some(now_iso());
                config.last_error = None;
                note_attention(config, &report.needs_attention);
            }
            Err(e) => {
                self.stage.store(STAGE_WEBDAV, Ordering::SeqCst);
                let token = config.token.clone();
                config.last_error = Some(redact(&e.to_string(), token.as_deref()));
            }
        });
        if let Err(e) = outcome {
            // A full or read-only config dir must not hide the sync result.
            tracing::warn!("sync settings not saved: {e}");
        }
        result
    }

    /// Persist what the WebDAV pass produced on the WebDAV target. Conflict
    /// paths join the shared list even when some other path failed; a pass
    /// that left paths behind is an error, so the banner shows it.
    fn finish_webdav(
        &self,
        dav: &WebdavConfig,
        result: Result<webdav::Outcome, Error>,
    ) -> Result<webdav::Outcome, Error> {
        let failure = match &result {
            Ok(outcome) => outcome.failure_message(),
            Err(e) => Some(e.to_string()),
        }
        .map(|message| dav.scrub(&message));
        let saved = self.update(|config| {
            self.stage.store(STAGE_IDLE, Ordering::SeqCst);
            if let Ok(outcome) = &result {
                note_attention(config, &outcome.needs_attention);
            }
            if !config.webdav.configured() {
                return; // disconnected while the pass ran
            }
            match &failure {
                None => {
                    config.webdav.last_sync_at = Some(now_iso());
                    config.webdav.last_error = None;
                }
                Some(message) => config.webdav.last_error = Some(message.clone()),
            }
        });
        if let Err(e) = saved {
            tracing::warn!("sync settings not saved: {e}");
        }
        match failure {
            Some(message) => Err(Error::Sync(message)),
            None => result,
        }
    }

    /// The algorithm. Split out so every failure path funnels through the
    /// index reset in [`Sync::once`].
    fn once(&self) -> Result<SyncReport, Error> {
        let config = self.config();
        let url = config
            .remote
            .clone()
            .filter(|r| !r.trim().is_empty())
            .ok_or_else(|| Error::Sync("sync is not configured".into()))?;
        let branch = config.branch().to_owned();
        let token = config.token.clone();

        let result = self.merge_and_push(&url, &branch, token.as_deref());
        if result.is_err() {
            // Never leave the index dirty or a stale merge state behind: the
            // learner keeps working with a clean working tree.
            if let Ok(repo) = Repository::open(&self.repo) {
                if let Err(e) = reset_index(&repo) {
                    tracing::debug!("sync index reset: {e}");
                }
            }
        }
        result
    }

    fn merge_and_push(
        &self,
        url: &str,
        branch: &str,
        token: Option<&str>,
    ) -> Result<SyncReport, Error> {
        // 1. A git repository, a remote, and the union-merge attribute.
        let repo = open_or_init(&self.repo, branch)?;
        // A crash or a hand-run `git merge` must not poison this run: dropping
        // the state is safe because the merge below is built in memory.
        repo.cleanup_state()
            .map_err(|e| fail("could not clear an unfinished merge", &e, token))?;
        ensure_gitattributes(&self.repo)?;
        configure_remote(&repo, url)?;

        // 2. Commit local changes so the merge starts from a clean tree.
        let committed = commit_all(&repo)?;
        let mut report = SyncReport {
            committed,
            ..SyncReport::default()
        };

        // 3. Fetch. A remote branch that does not exist yet is not an error.
        let fetched = fetch(&repo, branch, token)?;
        let local = head_oid(&repo)?;
        if let Some(remote) = fetched {
            let (_, behind) = repo
                .graph_ahead_behind(local, remote)
                .map_err(|e| fail("could not compare with the remote", &e, token))?;

            // 4–5. Merge, resolving every conflicted path by file type.
            if behind > 0 {
                let merged = merge_commit(&repo, &self.repo, local, remote, branch, token)?;
                report.pulled = behind.min(u32::MAX as usize) as u32;
                report.conflicts = merged.conflicts;
                report.needs_attention = merged.needs_attention;
            }
        }

        // 6. Push whatever the remote does not have.
        let local = head_oid(&repo)?;
        let ahead = match remote_tracking_oid(&repo, branch) {
            Some(remote) => {
                let (ahead, _) = repo
                    .graph_ahead_behind(local, remote)
                    .map_err(|e| fail("could not compare with the remote", &e, token))?;
                ahead
            }
            None => count_commits(&repo, local)?,
        };
        if ahead > 0 {
            push(&repo, branch, local, token)?;
            report.pushed = ahead.min(u32::MAX as usize) as u32;
        }
        Ok(report)
    }
}

/// Add conflict paths to the shared needs-attention list (either target).
fn note_attention(config: &mut SyncConfig, paths: &[String]) {
    for path in paths {
        if !config.conflicts.contains(path) && config.conflicts.len() < MAX_CONFLICTS {
            config.conflicts.push(path.clone());
        }
    }
}

/// Holds the single-flight flag for as long as a run lasts.
struct Flight<'a>(&'a AtomicBool);

impl<'a> Flight<'a> {
    fn acquire(flag: &'a AtomicBool) -> Result<Self, Error> {
        if flag
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(Error::Sync("a sync is already running".into()));
        }
        Ok(Flight(flag))
    }
}

impl Drop for Flight<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

// ---- repository setup -------------------------------------------------------

/// Open the data repo as a git repository, or initialise it on first use.
fn open_or_init(path: &Path, branch: &str) -> Result<Repository, Error> {
    if let Ok(repo) = Repository::open(path) {
        // A repository without a commit keeps its symbolic HEAD; point it at
        // the configured branch (it may have been created by CLI git).
        if repo.head().is_err() {
            repo.set_head(&format!("refs/heads/{branch}"))
                .map_err(|e| fail("could not select the branch", &e, None))?;
        }
        return Ok(repo);
    }
    let mut options = RepositoryInitOptions::new();
    options.initial_head(branch);
    options.no_reinit(true);
    Repository::init_opts(path, &options)
        .map_err(|e| fail("could not make the data folder a git repository", &e, None))
}

/// `*.jsonl merge=union` for anyone using CLI git by hand.
fn ensure_gitattributes(repo_dir: &Path) -> Result<(), Error> {
    let path = repo_dir.join(".gitattributes");
    if !path.exists() {
        fs::write(path, GITATTRIBUTES)?;
    }
    Ok(())
}

fn configure_remote(repo: &Repository, url: &str) -> Result<(), Error> {
    match repo.find_remote(REMOTE) {
        Ok(remote) => {
            if remote.url().ok() != Some(url) {
                repo.remote_set_url(REMOTE, url)
                    .map_err(|e| fail("could not update the remote", &e, None))?;
            }
        }
        Err(_) => {
            repo.remote(REMOTE, url)
                .map_err(|e| fail("could not add the remote", &e, None))?;
        }
    }
    // Fetch every branch the remote has; the merge only uses ours. An empty
    // remote (no commits) then produces no error instead of a missing ref.
    let refspec = format!("+refs/heads/*:refs/remotes/{REMOTE}/*");
    if let Err(e) = repo.remote_add_fetch(REMOTE, &refspec) {
        if e.code() != ErrorCode::Exists {
            return Err(fail("could not set the fetch refspec", &e, None));
        }
    }
    Ok(())
}

// ---- commits ----------------------------------------------------------------

fn signature(repo: &Repository) -> Result<Signature<'static>, Error> {
    if let Ok(config) = repo.config() {
        if let (Ok(name), Ok(email)) = (
            config.get_string("user.name"),
            config.get_string("user.email"),
        ) {
            if let Ok(sig) = Signature::now(&name, &email) {
                return Ok(sig);
            }
        }
    }
    Signature::now(DEFAULT_NAME, DEFAULT_EMAIL)
        .map_err(|e| fail("could not build a commit signature", &e, None))
}

/// `git add -A` + commit when anything changed. Returns whether a commit was
/// written.
fn commit_all(repo: &Repository) -> Result<bool, Error> {
    let mut index = repo
        .index()
        .map_err(|e| fail("could not read the index", &e, None))?;
    index
        .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
        .map_err(|e| fail("could not stage local changes", &e, None))?;
    // add_all adds and updates; update_all also drops entries for deleted files.
    index
        .update_all(["*"].iter(), None)
        .map_err(|e| fail("could not stage local changes", &e, None))?;
    index
        .write()
        .map_err(|e| fail("could not write the index", &e, None))?;
    let tree_oid = index
        .write_tree()
        .map_err(|e| fail("could not build a tree", &e, None))?;

    let head = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
    if head.as_ref().is_some_and(|t| t.id() == tree_oid) {
        return Ok(false); // nothing to commit
    }
    let tree = repo
        .find_tree(tree_oid)
        .map_err(|e| fail("could not read the tree", &e, None))?;
    let parents: Vec<git2::Commit<'_>> = match repo.head() {
        Ok(head) => head
            .peel_to_commit()
            .map(|c| vec![c])
            .map_err(|e| fail("could not read the last commit", &e, None))?,
        Err(_) => Vec::new(),
    };
    let refs: Vec<&git2::Commit<'_>> = parents.iter().collect();
    let sig = signature(repo)?;
    repo.commit(
        Some("HEAD"),
        &sig,
        &sig,
        &format!("sync: {}", now_iso()),
        &tree,
        &refs,
    )
    .map_err(|e| fail("could not commit local changes", &e, None))?;
    Ok(true)
}

// ---- network ----------------------------------------------------------------

fn remote_callbacks(token: Option<&str>) -> RemoteCallbacks<'static> {
    let token = token.map(str::to_owned);
    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(move |_url, username, allowed| {
        if allowed.contains(CredentialType::SSH_KEY) {
            // SSH keys are out of scope, but an agent is free to answer.
            return Cred::ssh_key_from_agent(username.unwrap_or("git"));
        }
        if allowed.contains(CredentialType::USER_PASS_PLAINTEXT) {
            if let Some(token) = token.as_deref() {
                let user = username.unwrap_or("x-access-token");
                return Cred::userpass_plaintext(user, token);
            }
        }
        if allowed.contains(CredentialType::USERNAME) {
            return Cred::username(username.unwrap_or("x-access-token"));
        }
        Cred::default()
    });
    callbacks
}

/// Fetch; returns the commit the remote branch now points at, if any.
fn fetch(repo: &Repository, branch: &str, token: Option<&str>) -> Result<Option<Oid>, Error> {
    let mut remote = repo
        .find_remote(REMOTE)
        .map_err(|e| fail("could not open the remote", &e, token))?;
    let mut options = FetchOptions::new();
    options.remote_callbacks(remote_callbacks(token));
    options.download_tags(AutotagOption::None);
    // No explicit refspec: the configured wildcard is used, so a remote
    // without our branch yet fetches nothing instead of failing.
    let refspecs: &[&str] = &[];
    remote
        .fetch(refspecs, Some(&mut options), None)
        .map_err(|e| fail("fetching failed", &e, token))?;
    Ok(remote_tracking_oid(repo, branch))
}

fn push(repo: &Repository, branch: &str, local: Oid, token: Option<&str>) -> Result<(), Error> {
    let mut remote = repo
        .find_remote(REMOTE)
        .map_err(|e| fail("could not open the remote", &e, token))?;
    let mut options = PushOptions::new();
    options.remote_callbacks(remote_callbacks(token));
    let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
    remote
        .push(&[refspec.as_str()], Some(&mut options))
        .map_err(|e| fail("pushing failed", &e, token))?;
    // Keep the ahead/behind counts right without fetching again.
    let tracking = format!("refs/remotes/{REMOTE}/{branch}");
    repo.reference(&tracking, local, true, "sync: push")
        .map_err(|e| fail("could not record the pushed commit", &e, token))?;
    // So `git status` / `git pull` in the data repo work by hand too. Config
    // only; best effort, since it is cosmetic.
    if let Ok(mut branch_ref) = repo.find_branch(branch, git2::BranchType::Local) {
        let _ = branch_ref.set_upstream(Some(&format!("{REMOTE}/{branch}")));
    }
    Ok(())
}

// ---- merge ------------------------------------------------------------------

struct Merged {
    conflicts: Vec<ConflictPath>,
    needs_attention: Vec<String>,
}

/// Merge `remote` into `local`: merge in memory, resolve every conflicted path,
/// commit the merge, write the changed files, then move the branch.
fn merge_commit(
    repo: &Repository,
    workdir: &Path,
    local: Oid,
    remote: Oid,
    branch: &str,
    token: Option<&str>,
) -> Result<Merged, Error> {
    let local_commit = repo
        .find_commit(local)
        .map_err(|e| fail("could not read the local commit", &e, token))?;
    let remote_commit = repo
        .find_commit(remote)
        .map_err(|e| fail("could not read the fetched commit", &e, token))?;
    let old_tree = local_commit
        .tree()
        .map_err(|e| fail("could not read the local tree", &e, token))?;
    let remote_tree = remote_commit
        .tree()
        .map_err(|e| fail("could not read the remote tree", &e, token))?;
    let mut index = repo
        .merge_commits(&local_commit, &remote_commit, None)
        .map_err(|e| fail("merging failed", &e, token))?;

    let mut merged = Merged {
        conflicts: Vec::new(),
        needs_attention: Vec::new(),
    };
    if index.has_conflicts() {
        let conflicts: Vec<IndexConflict> = index
            .conflicts()
            .and_then(|c| c.collect::<Result<Vec<_>, _>>())
            .map_err(|e| fail("could not read the merge conflicts", &e, token))?;
        let stamp = conflict_stamp();
        for conflict in conflicts {
            resolve_conflict(repo, &mut index, &conflict, &stamp, &mut merged)?;
        }
    }
    // libgit2 already applies the `merge=union` attribute from .gitattributes,
    // whose line union keeps duplicates and writes an unstable order. Normalise
    // every op log the merge touched through the same dedupe-and-sort pass.
    normalise_op_logs(repo, &mut index, &old_tree, &remote_tree, &mut merged)?;

    let tree_oid = index
        .write_tree_to(repo)
        .map_err(|e| fail("could not build the merged tree", &e, token))?;
    let tree = repo
        .find_tree(tree_oid)
        .map_err(|e| fail("could not read the merged tree", &e, token))?;
    let sig = signature(repo)?;
    let commit = repo
        .commit(
            None,
            &sig,
            &sig,
            &format!("sync: merge {}", short(remote)),
            &tree,
            &[&local_commit, &remote_commit],
        )
        .map_err(|e| fail("could not commit the merge", &e, token))?;

    // Nothing is written to disk until the merge commit exists: a failure
    // before this point leaves the working tree as it was.
    write_merged_paths(repo, workdir, &old_tree, &tree)?;
    let mut disk = repo
        .index()
        .map_err(|e| fail("could not read the index", &e, token))?;
    disk.read_tree(&tree)
        .map_err(|e| fail("could not update the index", &e, token))?;
    disk.write()
        .map_err(|e| fail("could not write the index", &e, token))?;
    let refname = format!("refs/heads/{branch}");
    repo.reference(&refname, commit, true, "sync: merge")
        .map_err(|e| fail("could not move the branch", &e, token))?;
    repo.set_head(&refname)
        .map_err(|e| fail("could not select the branch", &e, token))?;
    Ok(merged)
}

/// One conflicted path, resolved by file type (docs/sync-build.md):
///
/// * `*.jsonl`: union of both sides' lines, deduplicated by `opId`, unparseable
///   lines dropped, ordered by `(at, opId)`. Readers sort anyway, so this is
///   the one merge that keeps every op from both devices.
/// * anything else (`graph/*.json`, `.md` overrides, binaries): local wins.
///   The remote version is parked at `.conflicts/<path>.<timestamp>` and the
///   path is recorded for the UI.
fn resolve_conflict(
    repo: &Repository,
    index: &mut Index,
    conflict: &IndexConflict,
    stamp: &str,
    merged: &mut Merged,
) -> Result<(), Error> {
    let path = conflict_path(conflict)
        .ok_or_else(|| Error::Sync("a merge conflict had no path".into()))?;
    let ours = entry_bytes(repo, conflict.our.as_ref())?;
    let theirs = entry_bytes(repo, conflict.their.as_ref())?;

    if path.ends_with(".jsonl") {
        let union = union_jsonl(
            ours.as_deref().unwrap_or_default(),
            theirs.as_deref().unwrap_or_default(),
        );
        stage(repo, index, &path, &union)?;
        merged.conflicts.push(ConflictPath {
            path,
            resolution: "union".into(),
        });
        return Ok(());
    }

    if let Some(theirs) = theirs.as_deref() {
        let copy = format!(".conflicts/{path}.{stamp}");
        stage(repo, index, &copy, theirs)?;
    }
    match ours.as_deref() {
        Some(bytes) => {
            stage(repo, index, &path, bytes)?;
        }
        // Local deletion wins; the remote bytes are still parked above.
        None => index
            .conflict_remove(Path::new(&path))
            .map_err(|e| Error::Sync(format!("merging {path}: {}", cause(&e, None))))?,
    }
    merged.needs_attention.push(path.clone());
    merged.conflicts.push(ConflictPath {
        path,
        resolution: "local-wins".into(),
    });
    Ok(())
}

fn conflict_path(conflict: &IndexConflict) -> Option<String> {
    let entry = conflict
        .our
        .as_ref()
        .or(conflict.their.as_ref())
        .or(conflict.ancestor.as_ref())?;
    Some(String::from_utf8_lossy(&entry.path).into_owned())
}

fn entry_bytes(repo: &Repository, entry: Option<&IndexEntry>) -> Result<Option<Vec<u8>>, Error> {
    let Some(entry) = entry else {
        return Ok(None);
    };
    let blob = repo
        .find_blob(entry.id)
        .map_err(|e| fail("could not read a conflicted file", &e, None))?;
    Ok(Some(blob.content().to_vec()))
}

/// Dedupe and order every op log the merge touched.
///
/// `.gitattributes` carries `merge=union`, and libgit2 applies it during
/// `merge_commits`: the content is a line union, but it keeps a line both sides
/// carry and leaves the order dependent on the two sides. Re-running the union
/// pass over the merged blob gives the same result on every device: one line
/// per `opId`, ordered by `(at, opId)`.
///
/// An op log that ends up different from **both** sides was combined in code,
/// which is what the report calls a `union` resolution (a learner or an older
/// build may have deleted the attribute, in which case the path was a real
/// merge conflict and is already listed).
fn normalise_op_logs(
    repo: &Repository,
    index: &mut Index,
    local: &Tree<'_>,
    remote: &Tree<'_>,
    merged: &mut Merged,
) -> Result<(), Error> {
    let entries: Vec<(String, Oid)> = index
        .iter()
        .filter(|entry| entry.path.ends_with(b".jsonl"))
        .map(|entry| (String::from_utf8_lossy(&entry.path).into_owned(), entry.id))
        .collect();
    for (path, id) in entries {
        let blob = repo
            .find_blob(id)
            .map_err(|e| fail("could not read an op log after the merge", &e, None))?;
        let content = blob.content();
        let mut resolved = id;
        if !content.is_empty() {
            let normalised = union_jsonl(content, b"");
            if normalised != content {
                resolved = stage(repo, index, &path, &normalised)?;
            }
        }
        let side = |tree: &Tree<'_>| tree.get_path(Path::new(&path)).ok().map(|entry| entry.id());
        let from_both = side(local) != Some(resolved) && side(remote) != Some(resolved);
        if from_both && !merged.conflicts.iter().any(|c| c.path == path) {
            merged.conflicts.push(ConflictPath {
                path,
                resolution: "union".into(),
            });
        }
    }
    Ok(())
}

/// Stage the merged bytes for `path` at stage 0, which replaces every stage of
/// that path and so resolves the conflict.
///
/// The index `merge_commits` returns is in-memory and owns no repository, so
/// `add_frombuffer` is unavailable: the blob is written first and the entry
/// added by id. An orphaned blob from an aborted run is harmless.
fn stage(repo: &Repository, index: &mut Index, path: &str, bytes: &[u8]) -> Result<Oid, Error> {
    // Drop every stage of the path first: `add` alone would leave the
    // ancestor/our/their entries behind and the index unmergeable.
    let _ = index.conflict_remove(Path::new(path));
    let blob = repo
        .blob(bytes)
        .map_err(|e| Error::Sync(format!("merging {path}: {}", cause(&e, None))))?;
    let entry = IndexEntry {
        ctime: IndexTime::new(0, 0),
        mtime: IndexTime::new(0, 0),
        dev: 0,
        ino: 0,
        mode: 0o100644,
        uid: 0,
        gid: 0,
        file_size: 0,
        id: blob,
        flags: 0,
        flags_extended: 0,
        path: path.as_bytes().to_vec(),
    };
    index
        .add(&entry)
        .map_err(|e| Error::Sync(format!("merging {path}: {}", cause(&e, None))))?;
    Ok(blob)
}

/// `YYYYMMDDTHHMMSSZ`: sortable and valid on every filesystem.
fn conflict_stamp() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

/// Union merge of two op logs.
///
/// Lines that are not JSON are dropped (one torn write must not make the file
/// unreadable), duplicates are dropped by `opId` (a hand-written line without
/// one is keyed by its whole line), and the result is ordered by `(at, opId)`
/// so a diff of two devices is stable. Local lines win a duplicate.
fn union_jsonl(ours: &[u8], theirs: &[u8]) -> Vec<u8> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut lines: Vec<(Option<String>, Option<String>, String)> = Vec::new();
    for side in [ours, theirs] {
        for raw in side.split(|byte| *byte == b'\n') {
            let text = String::from_utf8_lossy(raw);
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
                continue; // torn or garbage: drop it, never fail the merge
            };
            let op_id = value
                .get("opId")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            let at = value
                .get("at")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            let key = op_id.clone().unwrap_or_else(|| text.to_owned());
            if !seen.insert(key) {
                continue;
            }
            lines.push((at, op_id, text.to_owned()));
        }
    }
    // Lines without an `at` sort after every real op; the sort is stable, so
    // they keep the local-then-remote order.
    let last = "\u{10ffff}".to_owned();
    lines.sort_by(|a, b| {
        let key = |line: &(Option<String>, Option<String>, String)| {
            (
                line.0.clone().unwrap_or_else(|| last.clone()),
                line.1.clone().unwrap_or_default(),
            )
        };
        key(a).cmp(&key(b))
    });
    let mut out = String::new();
    for (_, _, line) in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out.into_bytes()
}

/// Write the working tree changes the merge made.
///
/// Only paths that differ between the old and the merged tree are touched. If
/// a file changed on disk after the local commit (a turn or a job appending
/// while sync runs), an op log keeps those lines too (another union) and any
/// other file is left for the next run: sync never drops learner work.
fn write_merged_paths(
    repo: &Repository,
    workdir: &Path,
    old: &Tree<'_>,
    new: &Tree<'_>,
) -> Result<(), Error> {
    let diff = repo
        .diff_tree_to_tree(Some(old), Some(new), None)
        .map_err(|e| fail("could not diff the merged tree", &e, None))?;
    let deltas: Vec<_> = diff.deltas().collect();
    for delta in deltas {
        let Some(path) = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .map(Path::to_path_buf)
        else {
            continue;
        };
        let target = workdir.join(&path);
        let committed = old
            .get_path(&path)
            .ok()
            .and_then(|entry| repo.find_blob(entry.id()).ok())
            .map(|blob| blob.content().to_vec());
        let disk = fs::read(&target).ok();
        let concurrent = disk != committed;

        if delta.status() == git2::Delta::Deleted {
            // A local deletion is the merge result; a file written since the
            // commit is not ours to remove.
            if concurrent {
                continue;
            }
            let _ = fs::remove_file(&target);
            continue;
        }
        let bytes = repo
            .find_blob(delta.new_file().id())
            .map_err(|e| fail("could not read a merged file", &e, None))?
            .content()
            .to_vec();
        if concurrent {
            // An op log keeps the lines a turn appended while sync ran; any
            // other file keeps the learner's newer bytes, and the next sync
            // commits them.
            let is_op_log = path
                .extension()
                .is_some_and(|extension| extension == "jsonl");
            if let (true, Some(disk)) = (is_op_log, disk) {
                let merged = union_jsonl(&bytes, &disk);
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&target, merged)?;
            }
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&target, bytes)?;
    }
    Ok(())
}

// ---- inspection -------------------------------------------------------------

fn head_oid(repo: &Repository) -> Result<Oid, Error> {
    let head = repo
        .head()
        .map_err(|e| fail("the data repository has no commit", &e, None))?;
    head.target()
        .or_else(|| head.peel_to_commit().ok().map(|c| c.id()))
        .ok_or_else(|| Error::Sync("the data repository has no commit".into()))
}

fn remote_tracking_oid(repo: &Repository, branch: &str) -> Option<Oid> {
    repo.find_reference(&format!("refs/remotes/{REMOTE}/{branch}"))
        .ok()
        .and_then(|reference| reference.resolve().ok())
        .and_then(|reference| reference.target())
}

fn count_commits(repo: &Repository, from: Oid) -> Result<usize, Error> {
    let mut walk = repo
        .revwalk()
        .map_err(|e| Error::Sync(format!("could not read the history: {}", cause(&e, None))))?;
    walk.push(from)
        .map_err(|e| Error::Sync(format!("could not read the history: {}", cause(&e, None))))?;
    Ok(walk.filter(|oid| oid.is_ok()).count())
}

fn counts(repo: &Repository, branch: &str) -> Result<(u32, u32, bool), Error> {
    let mut options = StatusOptions::new();
    options.include_untracked(true).recurse_untracked_dirs(true);
    let dirty = repo
        .statuses(Some(&mut options))
        .map(|statuses| !statuses.is_empty())
        .map_err(|e| {
            Error::Sync(format!(
                "could not read the repository state: {}",
                cause(&e, None)
            ))
        })?;
    let local = repo.head().ok().and_then(|head| {
        head.target()
            .or_else(|| head.peel_to_commit().ok().map(|c| c.id()))
    });
    let remote = remote_tracking_oid(repo, branch);
    let (ahead, behind) = match (local, remote) {
        (Some(local), Some(remote)) => repo
            .graph_ahead_behind(local, remote)
            .map_err(|e| Error::Sync(format!("could not compare commits: {}", cause(&e, None))))?,
        (Some(local), None) => (count_commits(repo, local)?, 0),
        (None, Some(remote)) => (0, count_commits(repo, remote)?),
        (None, None) => (0, 0),
    };
    let ahead = ahead.min(u32::MAX as usize) as u32;
    let behind = behind.min(u32::MAX as usize) as u32;
    Ok((ahead, behind, dirty || ahead > 0))
}

/// Put the index back on `HEAD` without touching the working tree, so a failed
/// attempt leaves nothing staged.
fn reset_index(repo: &Repository) -> Result<(), Error> {
    let head = repo
        .head()
        .ok()
        .and_then(|head| head.peel(git2::ObjectType::Commit).ok());
    match head {
        Some(object) => repo
            .reset_default(Some(&object), Vec::<&Path>::new())
            .map_err(|e| Error::Sync(format!("could not reset the index: {}", cause(&e, None)))),
        None => {
            let mut index = repo.index().map_err(|e| {
                Error::Sync(format!("could not read the index: {}", cause(&e, None)))
            })?;
            index
                .clear()
                .and_then(|()| index.write())
                .map_err(|e| Error::Sync(format!("could not clear the index: {}", cause(&e, None))))
        }
    }
}

// ---- trust store ------------------------------------------------------------

/// Where Android keeps its trusted CA certificates: the Conscrypt APEX, which
/// Android 14+ updates through Google Play, then the system image (older
/// releases; on newer ones a stale copy). One certificate per file, PEM, with
/// OpenSSL's text dump around it.
#[cfg(target_os = "android")]
const ANDROID_CA_DIRS: [&str; 2] = [
    "/apex/com.android.conscrypt/cacerts",
    "/system/etc/security/cacerts",
];

/// The bundle written for OpenSSL, `<config>/git-ca-bundle.pem`.
#[cfg(target_os = "android")]
const CA_BUNDLE_FILE: &str = "git-ca-bundle.pem";

/// Give the Android OpenSSL the phone's trusted certificates. Called once at
/// startup, before anything touches libgit2.
///
/// The OpenSSL built from source for Android (`openssl-sys/vendored`) has no
/// roots of its own: its built-in folder (`/usr/local/ssl`) does not exist on
/// a phone, so every HTTPS remote failed with "the SSL certificate is
/// invalid". Android keeps its roots one file per certificate, under names
/// OpenSSL's directory lookup does not use, so they are concatenated into
/// `<config>/git-ca-bundle.pem`, and `SSL_CERT_FILE` is pointed at it.
///
/// Why the environment variable: it is how git2 itself configures OpenSSL. On
/// its first use git2 runs `openssl_probe::init_ssl_cert_env_vars`, which sets
/// `SSL_CERT_FILE`/`SSL_CERT_DIR` from a list of standard paths and keeps a
/// value that is already set when its file exists; libgit2's OpenSSL setup
/// then loads `SSL_CERT_FILE`. Setting it here, from the same single thread at
/// the same point in startup that git2 would, adds nothing git2 doesn't do. The
/// alternatives were worse: `git2::opts::set_ssl_cert_file` is `unsafe` (the
/// workspace forbids it), and a folder baked in at build time is only valid for
/// the phone's primary user (a work profile or second user has its own data
/// path, so the earlier fix failed there).
///
/// The order matters: libgit2 reads the file once, the first time any git2
/// call initialises it, so this runs first in `Harness::open`. The bundle is
/// rebuilt at every launch, so a root Android adds or removes (through a
/// system or Play update) takes effect on the next start. What happened is
/// kept in [`TRUST_STORE`] and added to every certificate error (a phone has
/// no log the learner can read). WebDAV (rustls with its own roots) does not
/// depend on any of this.
#[cfg(target_os = "android")]
pub(crate) fn install_android_trust_store(config_dir: &Path) {
    let note = prepare_trust_store(config_dir);
    tracing::info!("git trust store: {note}");
    let _ = TRUST_STORE.set(note);
}

/// The outcome of [`install_android_trust_store`], for certificate errors.
#[cfg(target_os = "android")]
static TRUST_STORE: OnceLock<String> = OnceLock::new();

/// Build and write the bundle, point `SSL_CERT_FILE` at it; returns a
/// one-line account of what it did.
#[cfg(target_os = "android")]
fn prepare_trust_store(config_dir: &Path) -> String {
    let mut tried = Vec::new();
    let mut found = None;
    for ca in ANDROID_CA_DIRS {
        if let Err(e) = fs::read_dir(ca) {
            tried.push(format!("{ca}: {e}"));
            continue;
        }
        match pem_bundle(&[Path::new(ca)]) {
            Some(bundle) => {
                found = Some((ca, bundle));
                break;
            }
            None => tried.push(format!("{ca}: no certificates")),
        }
    }
    let Some((ca, bundle)) = found else {
        return format!("no roots could be read ({})", tried.join("; "));
    };
    let count = bundle.matches("-----BEGIN CERTIFICATE-----").count();
    let path = config_dir.join(CA_BUNDLE_FILE);
    if fs::read_to_string(&path).ok().as_deref() != Some(bundle.as_str()) {
        if let Err(e) = fs::create_dir_all(config_dir).and_then(|()| fs::write(&path, &bundle)) {
            return format!(
                "{count} roots from {ca}, but {} could not be written: {e}",
                path.display()
            );
        }
    }
    std::env::set_var("SSL_CERT_FILE", &path);
    format!("{count} roots from {ca} in {}", path.display())
}

/// Appended to a certificate error: on Android, what startup did to the
/// trust store and where OpenSSL was actually told to look (`SSL_CERT_FILE`/
/// `SSL_CERT_DIR` as they are now, after git2's own `openssl_probe` ran at its
/// first use and may have set them); elsewhere nothing (the system OpenSSL has
/// its own).
fn trust_store_detail() -> String {
    #[cfg(target_os = "android")]
    {
        let var = |name: &str| {
            std::env::var_os(name).map_or_else(
                || "unset".to_owned(),
                |v| v.to_string_lossy().into_owned(),
            )
        };
        format!(
            "; trust store: {}; SSL_CERT_FILE={}; SSL_CERT_DIR={}",
            TRUST_STORE.get().map_or("not set up", String::as_str),
            var("SSL_CERT_FILE"),
            var("SSL_CERT_DIR"),
        )
    }
    #[cfg(not(target_os = "android"))]
    String::new()
}

/// Every PEM certificate under the first of `dirs` that holds any, joined into
/// one bundle. Text around the PEM blocks (Android's files carry OpenSSL's
/// text dump) is dropped. `None` when no directory yields a certificate.
#[cfg(any(target_os = "android", test))]
fn pem_bundle(dirs: &[&Path]) -> Option<String> {
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";
    for dir in dirs {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.is_file())
            .collect();
        files.sort();
        let mut bundle = String::new();
        for file in files {
            let Ok(text) = fs::read_to_string(&file) else {
                continue;
            };
            let mut rest = text.as_str();
            while let Some(start) = rest.find(BEGIN) {
                let Some(len) = rest[start..].find(END) else {
                    break;
                };
                let end = start + len + END.len();
                bundle.push_str(&rest[start..end]);
                bundle.push('\n');
                rest = &rest[end..];
            }
        }
        if !bundle.is_empty() {
            return Some(bundle);
        }
    }
    None
}

// ---- credentials and messages ----------------------------------------------

/// Split a remote URL into the URL to store and the token to keep locally.
///
/// `https://user:token@host/repo` becomes `https://user@host/repo` plus the
/// token; `https://token@host/repo` becomes `https://host/repo` plus the token
/// (a lone user part is a token, as GitHub and Gitea print it). The token then
/// only ever reaches git through the credentials callback.
fn split_credentials(remote: &str) -> (String, Option<String>) {
    let Some(scheme) = remote.find("://") else {
        return (remote.to_owned(), None);
    };
    let rest = &remote[scheme + 3..];
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let Some(at) = authority.rfind('@') else {
        return (remote.to_owned(), None);
    };
    let userinfo = &authority[..at];
    if userinfo.is_empty() {
        return (remote.to_owned(), None);
    }
    let (user, token) = match userinfo.split_once(':') {
        Some((user, password)) => (Some(user), Some(password)),
        None => (None, Some(userinfo)),
    };
    let mut url = String::with_capacity(remote.len());
    url.push_str(&remote[..scheme + 3]);
    if let Some(user) = user.filter(|user| !user.is_empty()) {
        url.push_str(user);
        url.push('@');
    }
    url.push_str(&authority[at + 1..]);
    url.push_str(&rest[authority_end..]);
    (
        url,
        token.filter(|token| !token.is_empty()).map(str::to_owned),
    )
}

/// Blanks a token out of any text that could reach a log line or the UI.
fn redact(message: &str, token: Option<&str>) -> String {
    match token.map(str::trim).filter(|token| !token.is_empty()) {
        Some(token) => message.replace(token, "[token removed]"),
        None => message.to_owned(),
    }
}

/// A sentence a learner can act on, never a libgit2 string on its own.
fn cause(error: &git2::Error, token: Option<&str>) -> String {
    let raw = error.message().to_lowercase();
    let message: String =
        if error.code() == ErrorCode::Auth || raw.contains("authentication") || raw.contains("401")
        {
            "the token was rejected".to_owned()
        } else if error.code() == ErrorCode::Certificate || raw.contains("certificate") {
            // libgit2's own text says which check failed (no CA store, an
            // expired or unknown issuer); the URL it may contain carries no
            // token, which only ever travels through the credentials callback.
            format!(
                "the remote's certificate could not be verified ({}{})",
                error.message().trim(),
                trust_store_detail()
            )
        } else if error.code() == ErrorCode::NotFound {
            "the remote repository was not found; check the URL and that the token can read it"
                .to_owned()
        } else if matches!(error.code(), ErrorCode::Exists | ErrorCode::NotFastForward) {
            "the remote changed while syncing; run sync again".to_owned()
        } else if matches!(
            error.class(),
            ErrorClass::Net | ErrorClass::Http | ErrorClass::Ssl
        ) || raw.contains("failed to connect")
            || raw.contains("could not resolve")
            || raw.contains("network is unreachable")
            || raw.contains("timed out")
        {
            // The class alone folds DNS, refused connections, TLS and proxy
            // failures into one sentence; libgit2's text tells them apart.
            format!(
                "the remote is unreachable; check the network and the URL ({})",
                error.message().trim()
            )
        } else {
            error.message().to_owned()
        };
    redact(&message, token)
}

fn fail(context: &str, error: &git2::Error, token: Option<&str>) -> Error {
    Error::Sync(format!("{context}: {}", cause(error, token)))
}

fn short(oid: Oid) -> String {
    oid.to_string().chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write as _};
    use std::net::TcpListener;

    /// A temp directory removed when the test ends.
    pub(super) struct Tmp(PathBuf);

    impl Tmp {
        pub(super) fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("lh-sync-{tag}-{}", ulid::Ulid::new()));
            fs::create_dir_all(&dir).unwrap();
            Tmp(dir)
        }

        pub(super) fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// One device: its data repo, its device-local config dir, its engine.
    pub(super) struct Device {
        _root: Tmp,
        pub(super) repo: PathBuf,
        pub(super) config: PathBuf,
        pub(super) sync: Arc<Sync>,
    }

    impl Device {
        pub(super) fn new(tag: &str) -> Self {
            let root = Tmp::new(tag);
            let repo = root.path().join("repo");
            let config = root.path().join("config");
            fs::create_dir_all(&repo).unwrap();
            fs::create_dir_all(&config).unwrap();
            let sync = Arc::new(Sync::new(&repo, &config));
            Self {
                _root: root,
                repo,
                config,
                sync,
            }
        }

        pub(super) fn connected(tag: &str, remote: &Path) -> Self {
            let device = Self::new(tag);
            device
                .sync
                .set(Some(remote.to_str().unwrap()), None, None)
                .unwrap();
            device
        }

        pub(super) fn write(&self, rel: &str, body: &str) {
            let path = self.repo.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, body).unwrap();
        }

        pub(super) fn append(&self, rel: &str, line: &str) {
            let path = self.repo.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap();
            file.write_all(format!("{line}\n").as_bytes()).unwrap();
        }

        pub(super) fn read(&self, rel: &str) -> String {
            fs::read_to_string(self.repo.join(rel)).unwrap()
        }

        pub(super) fn run(&self) -> SyncReport {
            self.sync.sync().unwrap()
        }
    }

    /// A bare repository standing in for any host, on the local filesystem.
    pub(super) fn bare(tag: &str) -> (Tmp, PathBuf) {
        let root = Tmp::new(tag);
        let path = root.path().join("remote.git");
        Repository::init_bare(&path).unwrap();
        (root, path)
    }

    pub(super) fn header(op_id: &str, at: &str) -> String {
        format!(
            r#"{{"op":"session","opId":"{op_id}","at":"{at}","id":"01HZZZZZZZZZZZZZZZZZZZZZZZ","mode":"learn","title":"Limits"}}"#
        )
    }

    pub(super) fn op(op_id: &str, at: &str, content: &str) -> String {
        format!(
            r#"{{"op":"message","opId":"{op_id}","at":"{at}","role":"user","content":"{content}"}}"#
        )
    }

    pub(super) fn op_ids(text: &str) -> Vec<String> {
        text.lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter_map(|value| {
                value
                    .get("opId")
                    .and_then(|id| id.as_str())
                    .map(str::to_owned)
            })
            .collect()
    }

    /// Every file in the repo except `.git`, path → bytes.
    pub(super) fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
        fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
            let Ok(entries) = fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if entry.file_name() == ".git" {
                    continue;
                }
                if path.is_dir() {
                    walk(root, &path, out);
                } else {
                    let rel = path
                        .strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned();
                    out.push((rel, fs::read(&path).unwrap()));
                }
            }
        }
        let mut out = Vec::new();
        walk(dir, dir, &mut out);
        out.sort();
        out
    }

    #[test]
    fn first_sync_commits_and_pushes() {
        let (_remote_root, remote) = bare("first");
        let device = Device::connected("first-device", &remote);
        device.write(
            "sessions/2026/a.jsonl",
            &format!(
                "{}\n{}\n",
                header("01H0000000000000000000000A", "2026-01-01T00:00:00Z"),
                op("01H0000000000000000000001A", "2026-01-01T00:00:01Z", "hi")
            ),
        );

        let report = device.run();
        assert!(report.committed, "the first run commits the data");
        assert_eq!(report.pushed, 1);
        assert_eq!(report.pulled, 0);
        assert!(report.conflicts.is_empty());

        // The union-merge attribute is written for anyone using git by hand.
        assert_eq!(
            device.read(".gitattributes"),
            "*.jsonl merge=union\n",
            "the data repo carries the union-merge attribute"
        );

        // The bare remote has the commit and both files.
        let remote_repo = Repository::open(&remote).unwrap();
        let head = remote_repo
            .find_reference("refs/heads/main")
            .unwrap()
            .peel_to_commit()
            .unwrap();
        let tree = head.tree().unwrap();
        assert!(tree.get_path(Path::new("sessions/2026/a.jsonl")).is_ok());
        assert!(tree.get_path(Path::new(".gitattributes")).is_ok());
        assert_eq!(head.parent_count(), 0);

        // Device-local settings never enter the repo, and are private.
        let settings = device.config.join("sync.json");
        assert!(settings.exists(), "sync.json is written");
        assert!(!device.repo.join("sync.json").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&settings).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "sync.json is owner-only");
        }

        // A second run has nothing to do.
        let again = device.run();
        assert!(!again.committed);
        assert_eq!(again.pushed, 0);
    }

    #[test]
    fn a_second_devices_commit_is_pulled() {
        let (_remote_root, remote) = bare("pull");
        let a = Device::connected("pull-a", &remote);
        a.write(
            "sessions/2026/a.jsonl",
            &format!(
                "{}\n{}\n",
                header("01H0000000000000000000000A", "2026-01-01T00:00:00Z"),
                op(
                    "01H0000000000000000000001A",
                    "2026-01-01T00:00:01Z",
                    "from a"
                )
            ),
        );
        a.run();

        // A fresh device joins through the host: it adopts the history.
        let b = Device::connected("pull-b", &remote);
        let adopted = b.run();
        assert!(
            adopted.pulled >= 1,
            "the first sync pulls the remote's work"
        );
        assert!(b.repo.join("sessions/2026/a.jsonl").exists());

        b.write(
            "sessions/2026/b.jsonl",
            &format!(
                "{}\n{}\n",
                header("01H0000000000000000000000B", "2026-01-02T00:00:00Z"),
                op(
                    "01H0000000000000000000001B",
                    "2026-01-02T00:00:01Z",
                    "from b"
                )
            ),
        );
        b.run();

        let pulled = a.run();
        assert!(pulled.pulled >= 1, "a pulls b's commit");
        assert!(a.repo.join("sessions/2026/b.jsonl").exists());
        assert!(a.read("sessions/2026/b.jsonl").contains("from b"));
    }

    #[test]
    fn two_devices_appending_to_one_session_merge_by_union() {
        let (_remote_root, remote) = bare("union");
        let a = Device::connected("union-a", &remote);
        let path = "sessions/2026/s.jsonl";
        a.write(
            path,
            &format!(
                "{}\n{}\n{}\n",
                header("01H00000000000000000000000", "2026-01-01T00:00:00Z"),
                op("01H00000000000000000000001", "2026-01-01T00:00:01Z", "one"),
                op("01H00000000000000000000002", "2026-01-01T00:00:02Z", "two"),
            ),
        );
        a.run();

        let b = Device::connected("union-b", &remote);
        b.run(); // base = a's commit

        // Both append from the same base: a genuine conflict on one file.
        a.append(
            path,
            &op(
                "01H00000000000000000000003",
                "2026-01-01T00:00:03Z",
                "from a",
            ),
        );
        b.append(
            path,
            &op(
                "01H00000000000000000000004",
                "2026-01-01T00:00:04Z",
                "from b",
            ),
        );
        // A torn write from one side must be dropped, not fatal.
        b.append(
            path,
            "{\"op\":\"message\",\"opId\":\"01H0000000000000000000000",
        );

        a.run(); // pushes a's op
        let report = b.run();

        assert_eq!(report.pulled, 2, "a's op and the merge a made");
        assert_eq!(report.conflicts.len(), 1, "{:?}", report.conflicts);
        assert_eq!(report.conflicts[0].path, path);
        assert_eq!(report.conflicts[0].resolution, "union");
        assert!(
            report.needs_attention.is_empty(),
            "a union merge loses nothing"
        );

        let merged = b.read(path);
        let ids = op_ids(&merged);
        assert_eq!(
            ids,
            vec![
                "01H00000000000000000000000",
                "01H00000000000000000000001",
                "01H00000000000000000000002",
                "01H00000000000000000000003",
                "01H00000000000000000000004",
            ],
            "every op appears once, ordered by (at, opId): {merged}"
        );
        assert_eq!(merged.lines().count(), 5);
        assert!(
            merged
                .lines()
                .all(|line| serde_json::from_str::<serde_json::Value>(line).is_ok()),
            "the torn line is dropped, every line parses: {merged}"
        );

        // a pulls the union back and ends with the same five ops.
        let pulled = a.run();
        assert!(pulled.pulled >= 1);
        assert_eq!(op_ids(&a.read(path)).len(), 5);
        assert!(a.read(path).contains("from b"));
    }

    #[test]
    fn a_graph_json_conflict_keeps_local_and_parks_the_remote_copy() {
        let (_remote_root, remote) = bare("graph");
        let a = Device::connected("graph-a", &remote);
        a.write(
            "graph/topics.json",
            r#"{"topics":[{"id":"t","title":"one"}]}"#,
        );
        a.run();

        let b = Device::connected("graph-b", &remote);
        b.run();

        let local = r#"{"topics":[{"id":"t","title":"local"}]}"#;
        let other = r#"{"topics":[{"id":"t","title":"remote"}]}"#;
        a.write("graph/topics.json", local);
        a.run();
        b.write("graph/topics.json", other);

        let report = b.run();
        assert_eq!(report.pulled, 2, "a's change and the merge a made");
        assert_eq!(
            report.conflicts,
            vec![ConflictPath {
                path: "graph/topics.json".into(),
                resolution: "local-wins".into(),
            }]
        );
        assert_eq!(report.needs_attention, vec!["graph/topics.json".to_owned()]);
        assert_eq!(b.read("graph/topics.json"), other, "local wins");

        let parked_dir = b.repo.join(".conflicts/graph");
        let parked: Vec<PathBuf> = fs::read_dir(&parked_dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(parked.len(), 1, "one copy of the remote version");
        assert!(parked[0]
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("topics.json."));
        assert_eq!(fs::read_to_string(&parked[0]).unwrap(), local);

        // The banner state survives a restart and clears on acknowledgment.
        let status = b.sync.status().unwrap();
        assert_eq!(status.needs_attention, vec!["graph/topics.json".to_owned()]);
        assert!(!status.unsynced, "the merge left a clean tree");

        let reloaded = Sync::new(&b.repo, &b.config);
        assert_eq!(reloaded.status().unwrap().needs_attention.len(), 1);
        b.sync.acknowledge().unwrap();
        assert!(b.sync.status().unwrap().needs_attention.is_empty());
        assert!(parked[0].exists(), "the copy stays in the repo");
        assert!(!b.run().committed, "acknowledging writes no commit");
    }

    #[test]
    fn an_unreachable_remote_is_readable_and_leaves_the_repo_untouched() {
        let (_remote_root, remote) = bare("unreachable");
        let a = Device::connected("unreachable-a", &remote);
        a.write(
            "sessions/2026/a.jsonl",
            &format!(
                "{}\n",
                header("01H0000000000000000000000A", "2026-01-01T00:00:00Z")
            ),
        );
        a.run();

        a.sync
            .set(Some("https://127.0.0.1:1/nope.git"), None, None)
            .unwrap();
        let before = snapshot(&a.repo);
        let error = a.sync.sync().unwrap_err().to_string();
        assert!(
            error.contains("unreachable") || error.contains("not found"),
            "{error}"
        );
        assert_eq!(snapshot(&a.repo), before, "the tree is byte-identical");

        // No merge state, no dirty index from the attempt.
        let repo = Repository::open(&a.repo).unwrap();
        assert_eq!(repo.state(), git2::RepositoryState::Clean);
        let head_tree = repo.head().unwrap().peel_to_tree().unwrap().id();
        assert_eq!(repo.index().unwrap().write_tree().unwrap(), head_tree);

        // The failure is kept for the banner; the learner keeps working.
        let status = a.sync.status().unwrap();
        assert!(status.last_error.unwrap().contains("unreachable"));
        assert!(!status.unsynced);
        a.append(
            "sessions/2026/a.jsonl",
            &op(
                "01H0000000000000000000001A",
                "2026-01-01T00:00:05Z",
                "still working",
            ),
        );
        assert!(a.sync.status().unwrap().unsynced, "local work is kept");
    }

    #[test]
    fn a_rejected_token_is_readable_and_never_reaches_the_repo() {
        // A host that refuses every credential, standing in for a revoked
        // token. No network beyond the loopback socket.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut buffer = [0u8; 2048];
                let _ = stream.read(&mut buffer);
                let _ = stream.write_all(
                    b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"git\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
        });

        let device = Device::new("token");
        device.write(
            "sessions/2026/a.jsonl",
            &format!(
                "{}\n",
                header("01H0000000000000000000000A", "2026-01-01T00:00:00Z")
            ),
        );
        device
            .sync
            .set(
                Some(&format!(
                    "http://alice:sekret-token@127.0.0.1:{port}/repo.git"
                )),
                None,
                None,
            )
            .unwrap();

        // The token moved out of the URL and into sync.json.
        let config = device.sync.config();
        assert_eq!(
            config.remote.as_deref(),
            Some(format!("http://alice@127.0.0.1:{port}/repo.git").as_str())
        );
        assert_eq!(config.token.as_deref(), Some("sekret-token"));

        let error = device.sync.sync().unwrap_err().to_string();
        assert!(error.contains("token"), "{error}");
        assert!(
            !error.contains("sekret-token"),
            "the token is never in a message"
        );

        // Nowhere but the device-local file: not the repo config, not any file
        // in the repo, not the stored error.
        let settings = fs::read_to_string(device.config.join("sync.json")).unwrap();
        assert!(
            settings.contains("sekret-token"),
            "the token is kept locally"
        );
        assert!(
            !format!("{:?}", device.sync.config()).contains("sekret-token"),
            "Debug output hides the token"
        );
        let git_config = fs::read_to_string(device.repo.join(".git/config")).unwrap();
        assert!(!git_config.contains("sekret-token"), "{git_config}");
        for (path, bytes) in snapshot(&device.repo) {
            assert!(
                !String::from_utf8_lossy(&bytes).contains("sekret-token"),
                "{path} carries the token"
            );
        }
        assert!(!device
            .sync
            .status()
            .unwrap()
            .last_error
            .unwrap()
            .contains("sekret-token"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(device.config.join("sync.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn status_reports_ahead_behind_and_unsynced_without_a_network() {
        let (_remote_root, remote) = bare("status");
        let root = Tmp::new("status-device");
        let repo_dir = root.path().join("repo");
        let config_dir = root.path().join("config");
        fs::create_dir_all(&repo_dir).unwrap();
        fs::create_dir_all(&config_dir).unwrap();
        let harness = crate::Harness::open(&repo_dir, &config_dir).unwrap();

        let unconfigured = harness.sync_status().unwrap();
        assert!(!unconfigured.configured);
        assert!(!unconfigured.unsynced);
        assert_eq!(unconfigured.branch, "main");
        assert_eq!(unconfigured.needs_attention, Vec::<String>::new());

        let configured = harness
            .set_sync(Some(remote.to_str().unwrap()), None, None)
            .unwrap();
        assert!(configured.configured);
        assert!(configured.unsynced, "configured but never synced");

        harness.sync_now().unwrap();
        let clean = harness.sync_status().unwrap();
        assert_eq!((clean.ahead, clean.behind), (0, 0));
        assert!(!clean.unsynced);
        assert!(clean.last_sync_at.is_some());
        assert!(clean.last_error.is_none());

        // A new local change is unsynced, still without any network.
        let path = repo_dir.join("graph/ops.jsonl");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "{\"op\":\"addTopic\",\"opId\":\"01H0000000000000000000000T\",\"at\":\"2026-01-03T00:00:00Z\"}\n",
        )
        .unwrap();
        let dirty = harness.sync_status().unwrap();
        assert!(dirty.unsynced);
        assert_eq!((dirty.ahead, dirty.behind), (0, 0));

        // A local commit is ahead; a moved remote-tracking ref is behind.
        let repo = Repository::open(&repo_dir).unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
            .unwrap();
        index.update_all(["*"].iter(), None).unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = Signature::now("test", "test@localhost").unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "local", &tree, &[&head])
            .unwrap();
        let ahead = harness.sync_status().unwrap();
        assert_eq!((ahead.ahead, ahead.behind), (1, 0));
        assert!(ahead.unsynced);

        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let child_tree = head.tree().unwrap();
        let child = repo
            .commit(None, &sig, &sig, "remote", &child_tree, &[&head])
            .unwrap();
        repo.reference("refs/remotes/origin/main", child, true, "test")
            .unwrap();
        let behind = harness.sync_status().unwrap();
        assert_eq!((behind.ahead, behind.behind), (0, 1));
        assert!(!behind.unsynced, "clean tree, nothing ahead");

        // An empty remote disconnects sync and forgets the device-local token.
        harness
            .set_sync(Some("https://alice:secret@example.com/r.git"), None, None)
            .unwrap();
        let disconnected = harness.set_sync(None, None, None).unwrap();
        assert!(!disconnected.configured);
        assert_eq!(disconnected.remote, None);
        let saved = fs::read_to_string(config_dir.join("sync.json")).unwrap();
        assert!(!saved.contains("secret"), "{saved}");
        assert!(!saved.contains("example.com"), "{saved}");
    }

    #[test]
    fn union_jsonl_drops_garbage_and_dedupes_by_op_id() {
        let ours = format!(
            "{}\n{}\nnot json at all\n{}\n",
            op("01H00000000000000000000002", "2026-01-01T00:00:02Z", "late"),
            op(
                "01H00000000000000000000001",
                "2026-01-01T00:00:01Z",
                "early"
            ),
            op("01H00000000000000000000002", "2026-01-01T00:00:02Z", "dup"),
        );
        let theirs = format!(
            "{{\"op\":\"message\",\"opId\":\"01H0000000000000000000000\n{}\n",
            op(
                "01H00000000000000000000003",
                "2026-01-01T00:00:03Z",
                "theirs"
            ),
        );
        let merged = union_jsonl(ours.as_bytes(), theirs.as_bytes());
        let text = String::from_utf8(merged).unwrap();
        assert_eq!(
            op_ids(&text),
            vec![
                "01H00000000000000000000001",
                "01H00000000000000000000002",
                "01H00000000000000000000003",
            ],
            "{text}"
        );
        assert!(
            text.contains("\"late\""),
            "the local duplicate wins: {text}"
        );
        assert!(!text.contains("dup"));
        assert!(!text.contains("not json at all"));

        // A line without an opId is keyed by the whole line.
        let plain = "{\"hand\":\"written\"}\n";
        let both = union_jsonl(plain.as_bytes(), plain.as_bytes());
        assert_eq!(String::from_utf8(both).unwrap(), plain);
    }

    #[test]
    fn unreadable_settings_do_not_block_opening() {
        let root = Tmp::new("corrupt");
        let repo = root.path().join("repo");
        let config = root.path().join("config");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&config).unwrap();
        fs::write(config.join("sync.json"), "{ this is not json").unwrap();

        let engine = Sync::new(&repo, &config);
        let status = engine.status().unwrap();
        assert!(!status.configured);
        assert!(status.last_error.unwrap().contains("could not be read"));
    }

    #[test]
    fn the_android_trust_store_becomes_one_pem_bundle() {
        // Android's layout: one file per root, the PEM block wrapped in
        // OpenSSL's text dump. The first directory with certificates wins
        // (the updatable APEX over the system image); empty ones are skipped.
        let root = Tmp::new("cacerts");
        let empty = root.path().join("apex");
        let system = root.path().join("system");
        fs::create_dir_all(&empty).unwrap();
        fs::create_dir_all(&system).unwrap();
        let pem =
            |body: &str| format!("-----BEGIN CERTIFICATE-----\n{body}\n-----END CERTIFICATE-----");
        fs::write(
            system.join("a1b2c3d4.0"),
            format!(
                "{}\nCertificate:\n    Data:\n        Version: 3 (0x2)\n",
                pem("AAAA")
            ),
        )
        .unwrap();
        fs::write(
            system.join("e5f6a7b8.0"),
            format!(
                "Certificate:\n    Data: ...\n{}\n{}\n",
                pem("BBBB"),
                pem("CCCC")
            ),
        )
        .unwrap();
        fs::write(system.join("README"), "no certificate here").unwrap();
        fs::write(
            system.join("f0f0f0f0.0"),
            "-----BEGIN CERTIFICATE-----\ncut off",
        )
        .unwrap();

        let bundle = pem_bundle(&[&root.path().join("missing"), &empty, &system]).unwrap();
        assert_eq!(
            bundle,
            format!("{}\n{}\n{}\n", pem("AAAA"), pem("BBBB"), pem("CCCC")),
            "only whole PEM blocks, in file order, with no text dump"
        );
        assert!(pem_bundle(&[&empty]).is_none());
    }

    #[test]
    fn remote_urls_lose_their_credentials() {
        assert_eq!(
            split_credentials("https://alice:secret@github.com/a/b.git"),
            (
                "https://alice@github.com/a/b.git".to_owned(),
                Some("secret".to_owned())
            )
        );
        // A lone user part is the token most hosts print.
        assert_eq!(
            split_credentials("https://ghp_abc@github.com/a/b.git"),
            (
                "https://github.com/a/b.git".to_owned(),
                Some("ghp_abc".to_owned())
            )
        );
        assert_eq!(
            split_credentials("https://github.com/a/b.git"),
            ("https://github.com/a/b.git".to_owned(), None)
        );
        assert_eq!(
            split_credentials("/tmp/bare.git"),
            ("/tmp/bare.git".to_owned(), None)
        );
    }
}
