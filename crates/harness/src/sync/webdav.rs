//! WebDAV sync target (docs/sync-build.md, "WebDAV target").
//!
//! WebDAV has no history, so one pass is a three-way comparison per path: the
//! local file's content hash and the remote ETag, each against what the
//! device-local manifest (`<config>/webdav-state.json`) recorded at the last
//! successful sync of that path. Writes are conditional (`If-Match` with the
//! ETag last seen, `If-None-Match: *` for a new file), so another device
//! writing in between surfaces as a `412` and is merged in rather than
//! overwritten. Merges follow the git rules (step 5): `*.jsonl` op logs are a
//! union by `opId`, anything else keeps the local copy and parks the remote one
//! in `.conflicts/`.
//!
//! The manifest is written only for paths that succeeded, so an interrupted
//! pass resumes instead of redoing everything or losing a change.
//!
//! The password is an app password kept in `sync.json` next to the git token.
//! It only ever leaves the device in the Basic auth header; `Debug` output
//! hides it and every message that could reach a log or the UI is scrubbed of
//! it.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use quick_xml::events::Event;
use quick_xml::Reader;
use reqwest::header::{CONTENT_TYPE, ETAG, IF_MATCH, IF_NONE_MATCH};
use reqwest::{Client, Method, RequestBuilder, Response, StatusCode, Url};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{conflict_stamp, union_jsonl, ConflictPath};
use crate::Error;

/// The device-local manifest, `<config>/webdav-state.json`.
pub(super) const MANIFEST_FILE: &str = "webdav-state.json";
/// A `412` is retried this many times before the path is left for the next
/// run.
const MAX_RETRIES: usize = 3;
/// The manifest is flushed after this many changed entries, so a crash mid
/// pass only redoes the last few paths.
const SAVE_EVERY: usize = 25;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// A file whose mtime is this close to the moment it was recorded could be
/// written again within the same filesystem tick, so its stat is not trusted
/// as proof that it is unchanged (git's "racy clean" rule).
const RACY_WINDOW: Duration = Duration::from_secs(2);
/// Prefix of a version made from the modification time and size, for servers
/// that report no ETag. It detects changes but can't be sent in `If-Match`.
const SYNTHETIC: &str = "lm:";
const AUTH_REJECTED: &str = "the WebDAV username or app password was rejected";
const UNREACHABLE: &str = "the WebDAV server is unreachable; check the network and the URL";
const PROPFIND_BODY: &str = concat!(
    r#"<?xml version="1.0" encoding="utf-8"?>"#,
    r#"<d:propfind xmlns:d="DAV:"><d:prop>"#,
    r#"<d:resourcetype/><d:getetag/><d:getlastmodified/><d:getcontentlength/>"#,
    r#"</d:prop></d:propfind>"#
);

/// The `webdav` object in `sync.json`.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebdavConfig {
    /// The folder URL, normalised to end with `/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// App password; never logged, never in an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// ISO time of the last pass that synced every path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync_at: Option<String>,
    /// The last failure, kept until a pass succeeds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl fmt::Debug for WebdavConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebdavConfig")
            .field("url", &self.url)
            .field("username", &self.username)
            .field(
                "password",
                &self.password.as_ref().map(|_| "[password removed]"),
            )
            .field("last_sync_at", &self.last_sync_at)
            .field("last_error", &self.last_error)
            .finish()
    }
}

impl WebdavConfig {
    pub fn configured(&self) -> bool {
        self.url
            .as_deref()
            .is_some_and(|url| !url.trim().is_empty())
    }

    /// Nothing saved at all: `sync.json` leaves the object out.
    pub(super) fn is_unset(&self) -> bool {
        *self == Self::default()
    }

    /// Blanks the password out of any text that could reach a log or the UI.
    pub(super) fn scrub(&self, message: &str) -> String {
        match self.password.as_deref().filter(|p| !p.is_empty()) {
            Some(password) => message.replace(password, "[password removed]"),
            None => message.to_owned(),
        }
    }
}

/// The WebDAV half of [`super::SyncStatus`]. No network.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebdavStatus {
    pub configured: bool,
    pub url: Option<String>,
    pub username: Option<String>,
    /// An app password is saved (never the password itself).
    pub has_password: bool,
    pub last_sync_at: Option<String>,
    pub last_error: Option<String>,
    /// Configured and never synced, or a local file differs from (or is
    /// missing in) the manifest.
    pub unsynced: bool,
}

/// What one WebDAV pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebdavReport {
    pub uploaded: u32,
    pub downloaded: u32,
    /// Paths changed on both sides and merged.
    pub merged: u32,
    pub deleted_remote: u32,
    pub deleted_local: u32,
    /// Paths left for the next run (kept changing, or the server refused one).
    pub skipped: u32,
}

/// A pass that reached the end; per-path failures are in `failures`.
#[derive(Debug, Default)]
pub(super) struct Outcome {
    pub report: WebdavReport,
    pub conflicts: Vec<ConflictPath>,
    pub needs_attention: Vec<String>,
    pub failures: Vec<String>,
}

impl Outcome {
    /// One sentence for `lastError` when some paths were left behind.
    pub(super) fn failure_message(&self) -> Option<String> {
        let first = self.failures.first()?;
        Some(match self.failures.len() {
            1 => format!("1 file was left for the next run: {first}"),
            n => format!("{n} files were left for the next run; the first: {first}"),
        })
    }
}

/// Accept `https://` anywhere and `http://` only on the loopback host (a local
/// test server). The result has no query or fragment and ends with `/`.
pub(super) fn normalise_url(raw: &str) -> Result<Url, Error> {
    let invalid = |message: &str| Error::Invalid(message.to_owned());
    let mut url = Url::parse(raw.trim()).map_err(|_| {
        invalid("the WebDAV URL is not a valid address; it looks like https://example.com/dav/learning/")
    })?;
    let secure = url.scheme() == "https" && url.host_str().is_some_and(|host| !host.is_empty());
    let loopback = url.scheme() == "http"
        && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if !secure && !loopback {
        return Err(invalid(
            "the WebDAV URL must start with https:// (http:// only works for localhost)",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid(
            "put the WebDAV username and app password in their own fields, not in the URL",
        ));
    }
    url.set_query(None);
    url.set_fragment(None);
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

/// Forget every path's sync state (WebDAV disconnected, or a new URL).
pub(super) fn delete_manifest(config_dir: &Path) {
    if let Err(e) = fs::remove_file(config_dir.join(MANIFEST_FILE)) {
        if e.kind() != io::ErrorKind::NotFound {
            tracing::warn!("could not delete the WebDAV manifest: {e}");
        }
    }
}

/// Whether a local file differs from what the last pass recorded. No network,
/// and cheap enough for the UI to poll: a file whose size and mtime match the
/// manifest counts as clean (racy entries included, since this is advisory),
/// and only a changed stat is hashed. An unreadable repo or manifest counts
/// as unsynced.
pub(super) fn local_changes(repo: &Path, config_dir: &Path, url: &str) -> bool {
    let Ok(base) = normalise_url(url) else {
        return true;
    };
    let manifest = Manifest::load(&config_dir.join(MANIFEST_FILE), base.as_str());
    let Ok(local) = scan_local(repo) else {
        return true;
    };
    if local.len() != manifest.files.len() {
        return true;
    }
    local
        .iter()
        .any(|(rel, stat)| match manifest.files.get(rel) {
            Some(entry) if entry.stat_matches(*stat) => false,
            Some(entry) => {
                fs::read(repo.join(rel)).map_or(true, |bytes| hash_bytes(&bytes) != entry.hash)
            }
            None => true,
        })
}

/// One WebDAV pass over the data repo. `Err` is a pass-wide failure (auth,
/// network, an unusable URL); the manifest still keeps every path that
/// succeeded before it.
pub(super) fn run(repo: &Path, config_dir: &Path, config: &WebdavConfig) -> Result<Outcome, Error> {
    let manifest_path = config_dir.join(MANIFEST_FILE);
    // A dedicated thread with its own runtime: the caller may be a blocking
    // task inside the app's runtime, where nesting `block_on` is not allowed.
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| Error::Sync(format!("could not start the WebDAV client: {e}")))?;
                runtime.block_on(async {
                    let dav = Dav::new(config)?;
                    pass(repo, &manifest_path, &dav).await
                })
            })
            .join()
            .unwrap_or_else(|_| Err(Error::Sync("the WebDAV sync stopped unexpectedly".into())))
    })
}

// ---- manifest ---------------------------------------------------------------

/// `webdav-state.json`: what each path looked like at its last successful
/// sync, for the URL it was synced with.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    #[serde(default)]
    url: String,
    #[serde(default)]
    files: BTreeMap<String, Entry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    /// The remote ETag (unquoted).
    etag: String,
    /// sha256 of the local bytes, hex.
    hash: String,
    /// Size and mtime (ns) of the bytes the hash describes; `mtime: 0` means
    /// unknown (the file changed while sync wrote it), so it is hashed again.
    #[serde(default)]
    size: u64,
    #[serde(default)]
    mtime: u64,
    /// The mtime was within [`RACY_WINDOW`] of the moment the manifest
    /// vouched for it: a same-tick write could hide behind it, so the pass
    /// hashes the file again (`status()` doesn't).
    #[serde(default)]
    racy: bool,
}

impl Entry {
    fn stat_matches(&self, stat: Stat) -> bool {
        self.mtime != 0 && self.mtime == stat.mtime && self.size == stat.size
    }
}

impl Manifest {
    /// A missing, corrupt or other-URL manifest starts over: every path then
    /// compares bytes once, which loses nothing.
    fn load(path: &Path, url: &str) -> Self {
        let manifest: Manifest = match crate::config::read_json(path) {
            Ok(manifest) => manifest,
            Err(e) => {
                tracing::warn!("WebDAV manifest unreadable, starting over: {e}");
                Manifest::default()
            }
        };
        if manifest.url == url {
            manifest
        } else {
            Manifest {
                url: url.to_owned(),
                files: BTreeMap::new(),
            }
        }
    }

    /// Write the manifest. An entry stops being racy once it is written at
    /// least [`RACY_WINDOW`] after its mtime: any later write lands in a later
    /// filesystem tick and moves the mtime (git's racy-clean rule, measured
    /// against the index write).
    fn save(&mut self, path: &Path) -> Result<(), Error> {
        for entry in self.files.values_mut() {
            if entry.racy && entry.mtime != 0 && !is_racy(entry.mtime) {
                entry.racy = false;
            }
        }
        crate::config::write_json(path, self, true)
    }
}

// ---- local files ------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct Stat {
    size: u64,
    mtime: u64,
}

fn stat_of(metadata: &fs::Metadata) -> Stat {
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
    Stat {
        size: metadata.len(),
        mtime,
    }
}

fn now_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

/// Too recent for a matching stat to prove the content unchanged.
fn is_racy(mtime: u64) -> bool {
    now_nanos().saturating_sub(mtime) < RACY_WINDOW.as_nanos() as u64
}

/// Every regular file in the data repo, keyed by its `/`-separated relative
/// path. `.git/` and symlinks are skipped.
fn scan_local(repo: &Path) -> Result<BTreeMap<String, Stat>, Error> {
    let mut out = BTreeMap::new();
    let mut stack = vec![(repo.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        for entry in entries {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue; // not UTF-8: no portable remote name
            };
            let rel = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            if !safe_rel(&rel) {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_dir() {
                stack.push((entry.path(), rel));
            } else if kind.is_file() {
                out.insert(rel, stat_of(&entry.metadata()?));
            }
        }
    }
    Ok(out)
}

/// A relative path sync may read or write: no `..`, no empty segment, nothing
/// inside `.git/`, nothing a filesystem would read as a separator.
fn safe_rel(rel: &str) -> bool {
    !rel.is_empty()
        && rel.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment != ".git"
                && !segment.contains(['\\', '\0'])
        })
}

fn hash_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// The file's hash, from the manifest when its stat matches and isn't racy.
fn local_hash(repo: &Path, rel: &str, stat: Stat, entry: Option<&Entry>) -> io::Result<String> {
    if let Some(entry) = entry {
        if !entry.racy && entry.stat_matches(stat) {
            return Ok(entry.hash.clone());
        }
    }
    Ok(hash_bytes(&fs::read(repo.join(rel))?))
}

fn is_op_log(rel: &str) -> bool {
    rel.ends_with(".jsonl")
}

// ---- the pass ---------------------------------------------------------------

/// How a step failed: the whole pass, or just this path.
enum Failure {
    Fatal(Error),
    Path(String),
}

impl Failure {
    fn into_error(self) -> Error {
        match self {
            Failure::Fatal(error) => error,
            Failure::Path(message) => Error::Sync(message),
        }
    }
}

type Step<T> = Result<T, Failure>;

fn local_failure(rel: &str, error: &io::Error) -> Failure {
    Failure::Path(format!("{rel} could not be read or written here: {error}"))
}

async fn pass(repo: &Path, manifest_path: &Path, dav: &Dav) -> Result<Outcome, Error> {
    let mut pass = Pass {
        repo,
        dav,
        manifest: Manifest::load(manifest_path, dav.base.as_str()),
        manifest_path,
        unsaved: 0,
        dirs: HashSet::new(),
        stamp: conflict_stamp(),
        outcome: Outcome::default(),
    };
    let result = pass.apply().await;
    let saved = pass.manifest.save(manifest_path);
    result?;
    saved?;
    Ok(pass.outcome)
}

struct Pass<'a> {
    repo: &'a Path,
    dav: &'a Dav,
    manifest: Manifest,
    manifest_path: &'a Path,
    unsaved: usize,
    /// Remote folders known to exist (relative, `""` is the sync folder).
    dirs: HashSet<String>,
    /// One conflict stamp per pass, so a retried merge overwrites its copy.
    stamp: String,
    outcome: Outcome,
}

impl Pass<'_> {
    async fn apply(&mut self) -> Result<(), Error> {
        let remote = self.dav.list_tree().await.map_err(Failure::into_error)?;
        self.dirs = remote.dirs;
        let local = scan_local(self.repo)?;
        let paths: BTreeSet<String> = local
            .keys()
            .chain(remote.files.keys())
            .chain(self.manifest.files.keys())
            .cloned()
            .collect();
        for rel in paths {
            let step = self
                .sync_path(
                    &rel,
                    local.get(&rel).copied(),
                    remote.files.get(&rel).map(String::as_str),
                )
                .await;
            match step {
                Ok(()) => {}
                Err(Failure::Fatal(error)) => return Err(error),
                Err(Failure::Path(message)) => {
                    tracing::debug!("WebDAV left {rel} for the next run: {message}");
                    self.outcome.report.skipped += 1;
                    self.outcome.failures.push(message);
                }
            }
        }
        Ok(())
    }

    /// The three-way decision for one path (docs/sync-build.md, step 2).
    async fn sync_path(
        &mut self,
        rel: &str,
        local: Option<Stat>,
        remote: Option<&str>,
    ) -> Step<()> {
        let entry = self.manifest.files.get(rel).cloned();
        let hash = match local {
            Some(stat) => Some(
                local_hash(self.repo, rel, stat, entry.as_ref())
                    .map_err(|e| local_failure(rel, &e))?,
            ),
            None => None,
        };
        match (entry, hash, remote) {
            (_, None, None) => {
                self.forget(rel);
                Ok(())
            }
            (None, Some(_), None) => self.upload(rel, None).await,
            (None, None, Some(_)) => self.download(rel, None).await,
            // New on both sides: identical bytes are just recorded.
            (None, Some(_), Some(_)) => self.merge(rel).await,
            (Some(entry), Some(hash), Some(etag)) => {
                match (hash != entry.hash, etag != entry.etag) {
                    (false, false) => {
                        if let Some(stat) = local {
                            self.refresh_stat(rel, stat);
                        }
                        Ok(())
                    }
                    (true, false) => self.upload(rel, Some(&entry.etag)).await,
                    (false, true) => self.download(rel, Some(&hash)).await,
                    (true, true) => self.merge(rel).await,
                }
            }
            // Gone remotely: delete here unless it changed here (a change
            // beats a delete).
            (Some(entry), Some(hash), None) => {
                if hash == entry.hash {
                    self.delete_local(rel, &entry.hash).await
                } else {
                    self.upload(rel, None).await
                }
            }
            // Gone here: delete remotely unless it changed there.
            (Some(entry), None, Some(etag)) => {
                if etag == entry.etag {
                    self.delete_remote(rel, &entry.etag).await
                } else {
                    self.download(rel, None).await
                }
            }
        }
    }

    /// `PUT` the local file: `If-Match` the manifest's ETag, or
    /// `If-None-Match: *` for a new one. A `412` turns into a merge.
    async fn upload(&mut self, rel: &str, expected: Option<&str>) -> Step<()> {
        let Some((bytes, stat)) = read_local(self.repo, rel)? else {
            return Ok(()); // deleted since the scan: the next run sees it
        };
        self.ensure_parents(rel).await?;
        match self.dav.put(rel, &bytes, expected).await? {
            Put::Stored(etag) => {
                self.record(rel, etag, &bytes, Some(stat));
                self.outcome.report.uploaded += 1;
                Ok(())
            }
            Put::Changed => self.merge(rel).await,
        }
    }

    /// `GET` and write locally. `expected` is the local hash the decision
    /// was made on (`None`: no local file); a learner write since then turns
    /// this into a merge.
    async fn download(&mut self, rel: &str, expected: Option<&str>) -> Step<()> {
        let Some((bytes, etag)) = self.dav.get(rel).await? else {
            return Ok(()); // deleted remotely since the listing
        };
        let current = read_local(self.repo, rel)?;
        let current_hash = current.as_ref().map(|(bytes, _)| hash_bytes(bytes));
        if current_hash.as_deref() != expected {
            return match current {
                Some(_) => self.merge(rel).await,
                None => Ok(()), // deleted here meanwhile: the next run decides
            };
        }
        write_local(self.repo, rel, &bytes)?;
        self.record(rel, etag, &bytes, None);
        self.outcome.report.downloaded += 1;
        Ok(())
    }

    /// Changed on both sides (or new on both): `GET`, merge by file type,
    /// write locally, `PUT` with `If-Match` the ETag just read. A `412` starts
    /// over, at most [`MAX_RETRIES`] more times.
    async fn merge(&mut self, rel: &str) -> Step<()> {
        for _ in 0..=MAX_RETRIES {
            let local = read_local(self.repo, rel)?;
            let remote = self.dav.get(rel).await?;
            let Some((local_bytes, stat)) = local else {
                // Gone here: the remote copy is the change, and it wins.
                if let Some((bytes, etag)) = remote {
                    write_local(self.repo, rel, &bytes)?;
                    self.record(rel, etag, &bytes, None);
                    self.outcome.report.downloaded += 1;
                } else {
                    self.forget(rel);
                }
                return Ok(());
            };
            let Some((remote_bytes, etag)) = remote else {
                // Gone remotely: the local change wins over the delete.
                self.ensure_parents(rel).await?;
                match self.dav.put(rel, &local_bytes, None).await? {
                    Put::Stored(etag) => {
                        self.record(rel, etag, &local_bytes, Some(stat));
                        self.outcome.report.uploaded += 1;
                        return Ok(());
                    }
                    Put::Changed => continue,
                }
            };
            if remote_bytes == local_bytes {
                self.record(rel, etag, &local_bytes, Some(stat));
                return Ok(());
            }

            let merged = if is_op_log(rel) {
                union_jsonl(&local_bytes, &remote_bytes)
            } else {
                self.park(rel, &remote_bytes)?;
                local_bytes.clone()
            };
            let local_stat = if merged == local_bytes {
                Some(stat)
            } else {
                if !replace_local(self.repo, rel, &local_bytes, &merged)? {
                    continue; // the learner wrote meanwhile: merge again
                }
                None
            };
            let etag = if merged == remote_bytes {
                etag
            } else {
                match self.dav.put(rel, &merged, Some(&etag)).await? {
                    Put::Stored(etag) => etag,
                    Put::Changed => continue,
                }
            };
            self.record(rel, etag, &merged, local_stat);
            self.outcome.report.merged += 1;
            if is_op_log(rel) {
                if merged != local_bytes && merged != remote_bytes {
                    self.outcome.conflicts.push(ConflictPath {
                        path: rel.to_owned(),
                        resolution: "union".into(),
                    });
                }
            } else {
                self.outcome.conflicts.push(ConflictPath {
                    path: rel.to_owned(),
                    resolution: "local-wins".into(),
                });
                if !self.outcome.needs_attention.iter().any(|p| p == rel) {
                    self.outcome.needs_attention.push(rel.to_owned());
                }
            }
            return Ok(());
        }
        Err(Failure::Path(format!(
            "{rel} kept changing on the WebDAV server while syncing"
        )))
    }

    /// `DELETE` with `If-Match`; a `412` means it changed remotely, and the
    /// change wins: it comes back here.
    async fn delete_remote(&mut self, rel: &str, etag: &str) -> Step<()> {
        match self.dav.delete(rel, etag).await? {
            Delete::Done => {
                self.forget(rel);
                self.outcome.report.deleted_remote += 1;
                Ok(())
            }
            Delete::Changed => self.download(rel, None).await,
        }
    }

    /// Delete the local copy, unless the learner changed it since the scan.
    async fn delete_local(&mut self, rel: &str, hash: &str) -> Step<()> {
        let Some((bytes, _)) = read_local(self.repo, rel)? else {
            self.forget(rel);
            return Ok(());
        };
        if hash_bytes(&bytes) != hash {
            return self.upload(rel, None).await;
        }
        match fs::remove_file(self.repo.join(rel)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(local_failure(rel, &e)),
        }
        self.forget(rel);
        self.outcome.report.deleted_local += 1;
        Ok(())
    }

    /// The remote version of a local-wins conflict goes to
    /// `.conflicts/<path>.<stamp>`; it uploads on the next pass like any file.
    fn park(&self, rel: &str, bytes: &[u8]) -> Step<()> {
        write_local(
            self.repo,
            &format!(".conflicts/{rel}.{}", self.stamp),
            bytes,
        )
    }

    /// `MKCOL` every missing parent folder of `rel`, top-down.
    async fn ensure_parents(&mut self, rel: &str) -> Step<()> {
        let parts: Vec<&str> = rel.split('/').collect();
        for depth in 1..parts.len() {
            let dir = parts[..depth].join("/");
            if self.dirs.contains(&dir) {
                continue;
            }
            let url = self.dav.url(&dir, true)?;
            match self.dav.mkcol(url).await? {
                Mkcol::Created => {}
                Mkcol::MissingParent => {
                    return Err(Failure::Path(format!(
                        "the folder {dir} could not be created on the WebDAV server"
                    )))
                }
                Mkcol::Refused(status) => {
                    return Err(unexpected(status, "creating the folder", &dir))
                }
            }
            self.dirs.insert(dir);
        }
        Ok(())
    }

    /// Remember what `rel` looks like now. `stat` is the one taken before the
    /// bytes were read; `None` means sync just wrote `bytes`, so the file is
    /// stat'ed after the write, and a size that doesn't match (a turn appended
    /// in between) leaves the mtime unknown so the next pass hashes it.
    fn record(&mut self, rel: &str, etag: String, bytes: &[u8], stat: Option<Stat>) {
        let stat = stat.or_else(|| {
            fs::symlink_metadata(self.repo.join(rel))
                .ok()
                .map(|metadata| stat_of(&metadata))
                .filter(|stat| stat.size == bytes.len() as u64)
        });
        let (size, mtime) = stat.map_or((0, 0), |s| (s.size, s.mtime));
        self.manifest.files.insert(
            rel.to_owned(),
            Entry {
                etag,
                hash: hash_bytes(bytes),
                size,
                mtime,
                racy: mtime != 0 && is_racy(mtime),
            },
        );
        self.touched();
    }

    /// An unchanged file (just hashed) whose stat moved or was racy: remember
    /// the current stat so later passes and `status()` skip hashing it.
    fn refresh_stat(&mut self, rel: &str, stat: Stat) {
        if let Some(entry) = self.manifest.files.get_mut(rel) {
            if entry.racy || !entry.stat_matches(stat) {
                entry.size = stat.size;
                entry.mtime = stat.mtime;
                entry.racy = is_racy(stat.mtime);
                self.touched();
            }
        }
    }

    fn forget(&mut self, rel: &str) {
        if self.manifest.files.remove(rel).is_some() {
            self.touched();
        }
    }

    fn touched(&mut self) {
        self.unsaved += 1;
        if self.unsaved >= SAVE_EVERY {
            self.unsaved = 0;
            if let Err(e) = self.manifest.save(self.manifest_path) {
                tracing::warn!("WebDAV manifest not saved: {e}");
            }
        }
    }
}

/// Stat, then read: a write after the stat moves the mtime, so a stale stat
/// never vouches for newer bytes.
fn read_local(repo: &Path, rel: &str) -> Step<Option<(Vec<u8>, Stat)>> {
    let path = repo.join(rel);
    let stat = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() => stat_of(&metadata),
        Ok(_) => return Ok(None),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(local_failure(rel, &e)),
    };
    match fs::read(&path) {
        Ok(bytes) => Ok(Some((bytes, stat))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(local_failure(rel, &e)),
    }
}

fn write_local(repo: &Path, rel: &str, bytes: &[u8]) -> Step<()> {
    let path = repo.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| local_failure(rel, &e))?;
    }
    // In place, like the git merge: a session log open for appending keeps
    // pointing at the same file.
    fs::write(&path, bytes).map_err(|e| local_failure(rel, &e))
}

/// Write `bytes` only if the file still holds `expected`; `false` when the
/// learner changed it in between.
fn replace_local(repo: &Path, rel: &str, expected: &[u8], bytes: &[u8]) -> Step<bool> {
    match read_local(repo, rel)? {
        Some((current, _)) if current == expected => {
            write_local(repo, rel, bytes)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

// ---- the client -------------------------------------------------------------

#[derive(Default)]
struct Remote {
    /// Relative path → normalised version (ETag, or a synthetic one).
    files: BTreeMap<String, String>,
    dirs: HashSet<String>,
}

enum Put {
    Stored(String),
    /// `412`: the precondition failed, another device wrote first.
    Changed,
}

enum Delete {
    Done,
    Changed,
}

enum Mkcol {
    /// Created, or already there (`405`).
    Created,
    /// `409`: a parent folder is missing.
    MissingParent,
    Refused(StatusCode),
}

struct Dav {
    client: Client,
    /// The sync folder, ending with `/`.
    base: Url,
    /// `base`'s path, percent-decoded, ending with `/`.
    base_path: String,
    username: String,
    password: Option<String>,
}

impl Dav {
    fn new(config: &WebdavConfig) -> Result<Self, Error> {
        let raw = config
            .url
            .as_deref()
            .filter(|url| !url.trim().is_empty())
            .ok_or_else(|| Error::Sync("WebDAV is not configured".into()))?;
        let base = normalise_url(raw)?;
        let base_path = percent_decode(base.path())
            .ok_or_else(|| Error::Sync("the WebDAV URL's path is not valid UTF-8".into()))?;
        let client = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!("learning-harness/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| Error::Sync("could not start the WebDAV client".into()))?;
        Ok(Self {
            client,
            base,
            base_path,
            username: config.username.clone().unwrap_or_default(),
            password: config.password.clone().filter(|p| !p.is_empty()),
        })
    }

    fn url(&self, rel: &str, dir: bool) -> Step<Url> {
        if rel.is_empty() {
            return Ok(self.base.clone());
        }
        let mut encoded = rel
            .split('/')
            .map(encode_segment)
            .collect::<Vec<_>>()
            .join("/");
        if dir {
            encoded.push('/');
        }
        self.base
            .join(&encoded)
            .map_err(|_| Failure::Path(format!("{rel} is not a valid path on the WebDAV server")))
    }

    async fn send(
        &self,
        method: Method,
        url: Url,
        build: impl FnOnce(RequestBuilder) -> RequestBuilder,
    ) -> Step<Response> {
        let mut request = self.client.request(method, url);
        if !self.username.is_empty() || self.password.is_some() {
            request = request.basic_auth(&self.username, self.password.as_deref());
        }
        let response = build(request)
            .send()
            .await
            .map_err(|e| Failure::Fatal(transport(&e)))?;
        match response.status() {
            StatusCode::UNAUTHORIZED => Err(Failure::Fatal(Error::Sync(AUTH_REJECTED.into()))),
            StatusCode::INSUFFICIENT_STORAGE => Err(Failure::Fatal(Error::Sync(
                "the WebDAV server is out of space".into(),
            ))),
            _ => Ok(response),
        }
    }

    /// `PROPFIND` a folder (`Depth: 1`) or a file (`Depth: 0`); `None` on 404.
    async fn propfind(&self, rel: &str, dir: bool) -> Step<Option<Vec<DavEntry>>> {
        let url = self.url(rel, dir)?;
        let method = Method::from_bytes(b"PROPFIND")
            .map_err(|_| Failure::Fatal(Error::Sync("could not build a WebDAV request".into())))?;
        let depth = if dir { "1" } else { "0" };
        let response = self
            .send(method, url.clone(), |request| {
                request
                    .header("Depth", depth)
                    .header(CONTENT_TYPE, "application/xml; charset=utf-8")
                    .body(PROPFIND_BODY)
            })
            .await?;
        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(unexpected(status, "listing", rel));
        }
        let text = response
            .text()
            .await
            .map_err(|e| Failure::Fatal(transport(&e)))?;
        let entries = parse_multistatus(&text).ok_or_else(|| {
            Failure::Fatal(Error::Sync(
                "the WebDAV server sent a folder listing that could not be read".into(),
            ))
        })?;
        Ok(Some(
            entries
                .into_iter()
                .filter_map(|mut entry| {
                    entry.rel = href_to_rel(&url, &entry.href, &self.base_path)?;
                    Some(entry)
                })
                .collect(),
        ))
    }

    /// Every file under the sync folder, walking `Depth: 1` listings (many
    /// servers refuse `Depth: infinity`). A missing sync folder is created.
    async fn list_tree(&self) -> Step<Remote> {
        let mut remote = Remote::default();
        remote.dirs.insert(String::new());
        let mut queue = vec![String::new()];
        while let Some(dir) = queue.pop() {
            let listing = self.propfind(&dir, true).await;
            let listing = if dir.is_empty() {
                // The sync folder itself: anything but success stops the pass.
                match listing {
                    Err(Failure::Path(message)) => {
                        return Err(Failure::Fatal(Error::Sync(message)))
                    }
                    other => other?,
                }
            } else {
                listing?
            };
            let Some(entries) = listing else {
                if dir.is_empty() {
                    self.create_base().await?;
                }
                continue; // a folder deleted mid-walk has no files
            };
            for entry in entries {
                let inside = dir.is_empty()
                    || entry
                        .rel
                        .strip_prefix(&dir)
                        .is_some_and(|rest| rest.starts_with('/'));
                if entry.rel == dir || !inside || !safe_rel(&entry.rel) {
                    continue;
                }
                if entry.dir {
                    if remote.dirs.insert(entry.rel.clone()) {
                        queue.push(entry.rel);
                    }
                } else {
                    let version = entry.version();
                    remote.files.insert(entry.rel, version);
                }
            }
        }
        Ok(remote)
    }

    /// `MKCOL` the sync folder, and its parents when the server says one is
    /// missing.
    async fn create_base(&self) -> Step<()> {
        let fatal = |status: StatusCode| {
            Failure::Fatal(Error::Sync(format!(
                "the sync folder could not be created on the WebDAV server (it answered {status})"
            )))
        };
        match self.mkcol(self.base.clone()).await? {
            Mkcol::Created => return Ok(()),
            Mkcol::Refused(status) => return Err(fatal(status)),
            Mkcol::MissingParent => {}
        }
        let segments: Vec<&str> = self
            .base
            .path()
            .split('/')
            .filter(|segment| !segment.is_empty())
            .collect();
        for depth in 1..=segments.len() {
            let mut url = self.base.clone();
            url.set_path(&format!("/{}/", segments[..depth].join("/")));
            let result = self.mkcol(url).await?;
            // A parent may be refused (a server root nobody may write to)
            // and still exist; only the sync folder itself must succeed.
            if depth == segments.len() {
                match result {
                    Mkcol::Created => {}
                    Mkcol::MissingParent => return Err(fatal(StatusCode::CONFLICT)),
                    Mkcol::Refused(status) => return Err(fatal(status)),
                }
            }
        }
        Ok(())
    }

    async fn mkcol(&self, url: Url) -> Step<Mkcol> {
        let method = Method::from_bytes(b"MKCOL")
            .map_err(|_| Failure::Fatal(Error::Sync("could not build a WebDAV request".into())))?;
        let status = self.send(method, url, |request| request).await?.status();
        Ok(
            if status.is_success() || status == StatusCode::METHOD_NOT_ALLOWED {
                Mkcol::Created
            } else if status == StatusCode::CONFLICT {
                Mkcol::MissingParent
            } else {
                Mkcol::Refused(status)
            },
        )
    }

    /// The bytes and version of one file; `None` on 404.
    async fn get(&self, rel: &str) -> Step<Option<(Vec<u8>, String)>> {
        let url = self.url(rel, false)?;
        let response = self.send(Method::GET, url, |request| request).await?;
        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(unexpected(status, "downloading", rel));
        }
        let etag = header_etag(&response);
        let bytes = response
            .bytes()
            .await
            .map_err(|e| Failure::Fatal(transport(&e)))?
            .to_vec();
        let etag = match etag {
            Some(etag) => etag,
            None => self.version(rel).await?,
        };
        Ok(Some((bytes, etag)))
    }

    /// `PUT` with `If-Match: <etag>`, or `If-None-Match: *` when `expected`
    /// is `None`.
    async fn put(&self, rel: &str, bytes: &[u8], expected: Option<&str>) -> Step<Put> {
        let url = self.url(rel, false)?;
        let body = bytes.to_vec();
        let response = self
            .send(Method::PUT, url, |request| {
                let request = match expected {
                    None => request.header(IF_NONE_MATCH, "*"),
                    Some(etag) if etag.starts_with(SYNTHETIC) => request,
                    Some(etag) => request.header(IF_MATCH, format!("\"{etag}\"")),
                };
                request.body(body)
            })
            .await?;
        let status = response.status();
        if status == StatusCode::PRECONDITION_FAILED {
            return Ok(Put::Changed);
        }
        if !status.is_success() {
            return Err(unexpected(status, "uploading", rel));
        }
        let etag = match header_etag(&response) {
            Some(etag) => etag,
            None => self.version(rel).await?,
        };
        Ok(Put::Stored(etag))
    }

    async fn delete(&self, rel: &str, etag: &str) -> Step<Delete> {
        let url = self.url(rel, false)?;
        let response = self
            .send(Method::DELETE, url, |request| {
                if etag.starts_with(SYNTHETIC) {
                    request
                } else {
                    request.header(IF_MATCH, format!("\"{etag}\""))
                }
            })
            .await?;
        let status = response.status();
        if status == StatusCode::PRECONDITION_FAILED {
            return Ok(Delete::Changed);
        }
        if status.is_success() || status == StatusCode::NOT_FOUND {
            return Ok(Delete::Done);
        }
        Err(unexpected(status, "deleting", rel))
    }

    /// A file's version from a `Depth: 0` listing, for servers that leave
    /// the `ETag` header off a response.
    async fn version(&self, rel: &str) -> Step<String> {
        self.propfind(rel, false)
            .await?
            .and_then(|entries| entries.into_iter().find(|entry| !entry.dir))
            .map(|entry| entry.version())
            .ok_or_else(|| {
                Failure::Path(format!("the WebDAV server reported no version for {rel}"))
            })
    }
}

/// A status nobody planned for, as a sentence. A refused sync folder is the
/// credentials' fault; a refused single file only skips that file.
fn unexpected(status: StatusCode, what: &str, rel: &str) -> Failure {
    if rel.is_empty() {
        if status == StatusCode::FORBIDDEN {
            return Failure::Fatal(Error::Sync(AUTH_REJECTED.into()));
        }
        return Failure::Fatal(Error::Sync(format!(
            "the WebDAV server answered {status} when {what} the sync folder; check that the URL points at a WebDAV folder"
        )));
    }
    Failure::Path(format!(
        "the WebDAV server answered {status} when {what} {rel}"
    ))
}

/// A transport error as a sentence, plus the innermost cause the OS or the TLS
/// layer gave ("os error 101", a DNS failure, "connection refused"): without
/// it every network fault reads the same and can't be told apart. The URL is
/// left out (it is in Settings); the password never reaches a URL or a cause,
/// it travels in the Authorization header.
fn transport(error: &reqwest::Error) -> Error {
    let mut chain = String::new();
    let mut innermost = error.to_string();
    let mut source: Option<&dyn std::error::Error> = Some(error);
    while let Some(current) = source {
        let text = current.to_string();
        chain.push_str(&text.to_lowercase());
        chain.push(' ');
        innermost = text;
        source = current.source();
    }
    let detail = format!(" ({})", innermost.trim());
    if chain.contains("certificate") {
        Error::Sync(format!(
            "the WebDAV server's certificate could not be verified{detail}"
        ))
    } else if error.is_timeout() {
        Error::Sync("the WebDAV server did not answer in time; run sync again".into())
    } else {
        Error::Sync(format!("{UNREACHABLE}{detail}"))
    }
}

fn header_etag(response: &Response) -> Option<String> {
    response
        .headers()
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .map(normalise_etag)
        .filter(|etag| !etag.is_empty())
}

/// `W/"abc"` and `"abc"` both become `abc`.
fn normalise_etag(raw: &str) -> String {
    let raw = raw.trim();
    let raw = raw.strip_prefix("W/").unwrap_or(raw);
    raw.trim_matches('"').to_owned()
}

// ---- multistatus ------------------------------------------------------------

#[derive(Debug, Default)]
struct DavEntry {
    href: String,
    /// Relative to the sync folder, filled in after parsing.
    rel: String,
    dir: bool,
    etag: String,
    modified: String,
    length: String,
}

impl DavEntry {
    fn version(&self) -> String {
        let etag = normalise_etag(&self.etag);
        if etag.is_empty() {
            format!("{SYNTHETIC}{}:{}", self.modified.trim(), self.length.trim())
        } else {
            etag
        }
    }
}

/// The `response` elements of a `207 Multi-Status` body. Namespace prefixes
/// are ignored (`D:`, `d:`, none), and entities in text are resolved.
fn parse_multistatus(xml: &str) -> Option<Vec<DavEntry>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut stack: Vec<String> = Vec::new();
    let mut entries = Vec::new();
    let mut current: Option<DavEntry> = None;
    loop {
        match reader.read_event().ok()? {
            Event::Start(start) => {
                let name = local_name(start.local_name().as_ref());
                if name == "response" {
                    current = Some(DavEntry::default());
                }
                mark_collection(&name, &stack, current.as_mut());
                stack.push(name);
            }
            Event::Empty(start) => {
                let name = local_name(start.local_name().as_ref());
                mark_collection(&name, &stack, current.as_mut());
            }
            Event::End(_) => {
                if stack.pop().as_deref() == Some("response") {
                    entries.extend(current.take());
                }
            }
            Event::Text(text) => append_text(&stack, current.as_mut(), &text.into_inner()),
            Event::CData(data) => {
                append_text(&stack, current.as_mut(), &data.into_inner());
            }
            // An entity nobody defined is dropped rather than failing the
            // whole listing.
            Event::GeneralRef(reference) => {
                if let Some(resolved) = resolve_entity(&reference.into_inner()) {
                    append_text(&stack, current.as_mut(), &resolved);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Some(entries)
}

fn local_name(raw: &str) -> String {
    raw.to_ascii_lowercase()
}

fn mark_collection(name: &str, stack: &[String], entry: Option<&mut DavEntry>) {
    if name == "collection" && stack.last().map(String::as_str) == Some("resourcetype") {
        if let Some(entry) = entry {
            entry.dir = true;
        }
    }
}

fn append_text(stack: &[String], entry: Option<&mut DavEntry>, text: &str) {
    let Some(entry) = entry else {
        return;
    };
    let parent = stack.len().checked_sub(2).map(|i| stack[i].as_str());
    match stack.last().map(String::as_str) {
        Some("href") if parent == Some("response") => entry.href.push_str(text),
        Some("getetag") => entry.etag.push_str(text),
        Some("getlastmodified") => entry.modified.push_str(text),
        Some("getcontentlength") => entry.length.push_str(text),
        _ => {}
    }
}

/// The five XML entities and numeric character references.
fn resolve_entity(name: &str) -> Option<String> {
    let named = match name {
        "lt" => Some('<'),
        "gt" => Some('>'),
        "amp" => Some('&'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => None,
    };
    if let Some(ch) = named {
        return Some(ch.to_string());
    }
    let digits = name.strip_prefix('#')?;
    let code = match digits.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => digits.parse::<u32>().ok()?,
    };
    char::from_u32(code).map(String::from)
}

// ---- hrefs ------------------------------------------------------------------

/// The path of an `href` relative to the sync folder, or `None` when it lies
/// outside it. Hrefs may be full URLs, absolute paths or relative to the
/// listed folder, and are compared percent-decoded.
fn href_to_rel(listed: &Url, href: &str, base_path: &str) -> Option<String> {
    let resolved = listed.join(href.trim()).ok()?;
    let path = percent_decode(resolved.path())?;
    let rest = match path.strip_prefix(base_path) {
        Some(rest) => rest,
        None if path == base_path.trim_end_matches('/') => "",
        None => return None,
    };
    Some(rest.trim_end_matches('/').to_owned())
}

/// Everything but RFC 3986 unreserved characters is escaped, so a segment
/// can't be read as a scheme, a query or a separator.
fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `%XX` escapes decoded; `None` when the result is not UTF-8.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escape = bytes
            .get(i + 1..i + 3)
            .filter(|hex| bytes[i] == b'%' && hex.iter().all(u8::is_ascii_hexdigit));
        if let Some(hex) = escape {
            let hex = std::str::from_utf8(hex).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hrefs_resolve_in_every_form_a_server_sends() {
        let listed = Url::parse("https://dav.example.com/dav/my%20data/sessions/").unwrap();
        let base = "/dav/my data/";
        // Absolute path, full URL, a different host's URL, and relative.
        assert_eq!(
            href_to_rel(&listed, "/dav/my%20data/sessions/a%20b.jsonl", base).as_deref(),
            Some("sessions/a b.jsonl")
        );
        assert_eq!(
            href_to_rel(
                &listed,
                "https://other.example/dav/my%20data/sessions/",
                base
            )
            .as_deref(),
            Some("sessions")
        );
        assert_eq!(
            href_to_rel(&listed, "x.jsonl", base).as_deref(),
            Some("sessions/x.jsonl")
        );
        // The folder itself, with and without the slash; outside it.
        assert_eq!(
            href_to_rel(&listed, "/dav/my%20data/", base).as_deref(),
            Some("")
        );
        assert_eq!(
            href_to_rel(&listed, "/dav/my%20data", base).as_deref(),
            Some("")
        );
        assert_eq!(href_to_rel(&listed, "/dav/other/a.jsonl", base), None);
        assert_eq!(href_to_rel(&listed, "/dav/my%20data/../escape", base), None);
    }

    #[test]
    fn multistatus_tolerates_prefixes_and_entities() {
        let xml = r#"<?xml version="1.0"?>
<multistatus xmlns="DAV:"><response><href>/d/</href><propstat><prop>
<resourcetype><collection/></resourcetype></prop></propstat></response>
<x:response xmlns:x="DAV:"><x:href>/d/a&amp;b.md</x:href><x:propstat><x:prop>
<x:resourcetype/><x:getetag>&quot;e1&quot;</x:getetag></x:prop></x:propstat>
<x:propstat><x:prop><x:getlastmodified/></x:prop></x:propstat></x:response>
<D:response xmlns:D="DAV:"><D:href>/d/n.json</D:href><D:propstat><D:prop>
<D:getlastmodified>Mon, 09 Nov 2026 10:00:00 GMT</D:getlastmodified>
<D:getcontentlength>12</D:getcontentlength></D:prop></D:propstat></D:response>
</multistatus>"#;
        let entries = parse_multistatus(xml).unwrap();
        assert_eq!(entries.len(), 3);
        assert!(entries[0].dir);
        assert_eq!(entries[1].href, "/d/a&b.md");
        assert!(!entries[1].dir);
        assert_eq!(entries[1].version(), "e1");
        assert_eq!(
            entries[2].version(),
            "lm:Mon, 09 Nov 2026 10:00:00 GMT:12",
            "no ETag: a version from the date and size"
        );
    }

    #[test]
    fn unsafe_paths_are_never_synced() {
        for bad in [
            "",
            "../x",
            "a//b",
            "a/./b",
            ".git/config",
            "a/.git/x",
            "a\\b",
        ] {
            assert!(!safe_rel(bad), "{bad}");
        }
        assert!(safe_rel(".conflicts/graph/topics.json.20260101T000000Z"));
        assert!(safe_rel("prompts/my notes.md"));
    }
}
