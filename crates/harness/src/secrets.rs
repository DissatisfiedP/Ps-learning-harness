//! Secrets: API keys, sign-in sessions and the sync token/password, kept in
//! the platform's secure store (architecture §6.5), never in a plain file
//! when that store works.
//!
//! | Platform | Store |
//! |---|---|
//! | Linux, BSD | the Secret Service over D-Bus (GNOME Keyring, KWallet, KeePassXC) |
//! | macOS | the login keychain |
//! | iOS | the data-protection keychain |
//! | Windows | Credential Manager |
//! | Android | SharedPreferences encrypted with a key held in the Android Keystore |
//!
//! Items are named `<namespace>/<name>` under the service
//! `com.pslearningharness.app`; the namespace is a ULID kept in
//! `<config>/secret-store.json` (with the chosen backend), so two config
//! directories on one account never share items. Names in use:
//! `credential/<provider-id>` (the JSON of a `providers::Credential`),
//! `sync/git-token` and `sync/webdav-password`.
//!
//! Choosing the backend, once per launch ([`SecretStore::open`]):
//! - the marker says `os`: the OS store is used without a probe. If it cannot
//!   be opened the store is **unavailable**: reads find nothing and writes
//!   fail with a message naming the store. Nothing is ever written to a
//!   plain file again once the OS store has held the secrets.
//! - no marker, or the marker says `file`: the OS store is probed (write,
//!   read back, delete). If it works, every item of `<config>/secrets.json`
//!   is copied in, the marker becomes `os`, and the file is deleted. If it
//!   does not (a Linux desktop without a Secret Service), the secrets stay in
//!   `secrets.json` (mode 0600) and [`StorageStatus::notice`] says so, which
//!   Settings shows; the next launch tries the OS store again.
//!
//! A value longer than the backend allows (Windows: 2560 bytes) is split:
//! the item itself holds a head `\0chunks:<n>:<generation>` and the parts are
//! `<name>#<generation>.<i>`. A new value writes its parts, then the head,
//! then deletes the old parts, so a reader never sees a mix.
//!
//! Every operation runs under one lock, so concurrent writers (a refresh and
//! a sign-in, a sync run and Settings) are serialised; callers that must not
//! refresh twice (rotating refresh tokens) hold their own lock around their
//! read-refresh-write (`Harness::refresh_*`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::{lock, Error};

/// The service every item is filed under.
const SERVICE: &str = "com.pslearningharness.app";
/// Which backend holds the secrets and the namespace of their items.
const MARKER_FILE: &str = "secret-store.json";
/// The 0600 fallback when the OS store does not work.
const FALLBACK_FILE: &str = "secrets.json";
/// The item the probe writes and deletes.
const PROBE: &str = "probe";
const CHUNK_HEAD: &[u8] = b"\0chunks:";

/// The item holding the saved credential of a provider.
pub fn credential_item(provider: providers::ProviderId) -> String {
    let id = serde_json::to_value(provider)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    format!("credential/{id}")
}

pub const GIT_TOKEN: &str = "sync/git-token";
pub const WEBDAV_PASSWORD: &str = "sync/webdav-password";

/// Where the secrets are, for Settings (`secret_storage`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageStatus {
    /// `os`, `file`, `unavailable` or `memory` (tests).
    pub backend: &'static str,
    /// The store, as a phrase: "the macOS Keychain", or the file's path.
    pub label: String,
    /// The secrets are in the OS store.
    pub secure: bool,
    /// Something the learner should know: the file fallback, an unreachable
    /// store, or the last failed read.
    pub notice: Option<String>,
}

/// One place to keep named byte strings.
trait Backend: Send + Sync {
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>, String>;
    fn set(&self, name: &str, value: &[u8]) -> Result<(), String>;
    /// A missing item is not an error.
    fn delete(&self, name: &str) -> Result<(), String>;
    /// The longest value one item holds.
    fn max_len(&self) -> usize {
        usize::MAX
    }
}

// ---- the OS store ------------------------------------------------------------

struct Keyring {
    store: Arc<keyring_core::CredentialStore>,
    namespace: String,
}

impl Keyring {
    fn entry(&self, name: &str) -> Result<keyring_core::Entry, String> {
        let user = format!("{}/{name}", self.namespace);
        let label = format!("P's Learning Harness: {name}");
        let modifiers = platform::LABELLED
            .then(|| std::collections::HashMap::from([("label", label.as_str())]));
        self.store
            .build(SERVICE, &user, modifiers.as_ref())
            .map_err(|e| e.to_string())
    }
}

impl Backend for Keyring {
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>, String> {
        match self.entry(name)?.get_secret() {
            Ok(value) => Ok(Some(value)),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn set(&self, name: &str, value: &[u8]) -> Result<(), String> {
        self.entry(name)?
            .set_secret(value)
            .map_err(|e| e.to_string())
    }

    fn delete(&self, name: &str) -> Result<(), String> {
        match self.entry(name)?.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn max_len(&self) -> usize {
        platform::MAX_LEN
    }
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
))]
mod platform {
    pub const LABEL: &str = "the Secret Service (desktop keyring)";
    /// Items carry a label the keyring's UI (Seahorse, KWallet) shows.
    pub const LABELLED: bool = true;
    pub const MAX_LEN: usize = usize::MAX;
    pub fn store() -> keyring_core::Result<std::sync::Arc<keyring_core::CredentialStore>> {
        Ok(zbus_secret_service_keyring_store::Store::new()?)
    }
}

#[cfg(target_os = "macos")]
mod platform {
    pub const LABEL: &str = "the macOS Keychain";
    pub const LABELLED: bool = false;
    pub const MAX_LEN: usize = usize::MAX;
    pub fn store() -> keyring_core::Result<std::sync::Arc<keyring_core::CredentialStore>> {
        Ok(apple_native_keyring_store::keychain::Store::new()?)
    }
}

#[cfg(target_os = "ios")]
mod platform {
    pub const LABEL: &str = "the iOS Keychain";
    pub const LABELLED: bool = false;
    pub const MAX_LEN: usize = usize::MAX;
    pub fn store() -> keyring_core::Result<std::sync::Arc<keyring_core::CredentialStore>> {
        Ok(apple_native_keyring_store::protected::Store::new()?)
    }
}

#[cfg(target_os = "windows")]
mod platform {
    pub const LABEL: &str = "Windows Credential Manager";
    pub const LABELLED: bool = false;
    /// `CRED_MAX_CREDENTIAL_BLOB_SIZE`.
    pub const MAX_LEN: usize = 2560;
    pub fn store() -> keyring_core::Result<std::sync::Arc<keyring_core::CredentialStore>> {
        Ok(windows_native_keyring_store::Store::new()?)
    }
}

#[cfg(target_os = "android")]
mod platform {
    pub const LABEL: &str = "the Android Keystore";
    pub const LABELLED: bool = false;
    pub const MAX_LEN: usize = usize::MAX;
    pub fn store() -> keyring_core::Result<std::sync::Arc<keyring_core::CredentialStore>> {
        Ok(android_native_keyring_store::Store::new()?)
    }
}

#[cfg(not(any(unix, target_os = "windows")))]
mod platform {
    pub const LABEL: &str = "a secure store";
    pub const LABELLED: bool = false;
    pub const MAX_LEN: usize = usize::MAX;
    pub fn store() -> keyring_core::Result<std::sync::Arc<keyring_core::CredentialStore>> {
        Err(keyring_core::Error::NotSupportedByStore(
            "this platform has no secure store".into(),
        ))
    }
}

fn open_platform(namespace: &str) -> Result<Arc<dyn Backend>, String> {
    let store = platform::store().map_err(|e| e.to_string())?;
    Ok(Arc::new(Keyring {
        store,
        namespace: namespace.to_owned(),
    }))
}

// ---- the 0600 file -----------------------------------------------------------

struct File {
    path: PathBuf,
}

impl File {
    fn read(&self) -> Result<BTreeMap<String, String>, String> {
        crate::config::read_json(&self.path).map_err(|e| e.to_string())
    }

    fn write(&self, items: &BTreeMap<String, String>) -> Result<(), String> {
        crate::config::write_json(&self.path, items, true).map_err(|e| e.to_string())
    }
}

impl Backend for File {
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>, String> {
        Ok(self.read()?.remove(name).map(String::into_bytes))
    }

    fn set(&self, name: &str, value: &[u8]) -> Result<(), String> {
        let value = String::from_utf8(value.to_vec()).map_err(|e| e.to_string())?;
        let mut items = self.read()?;
        items.insert(name.to_owned(), value);
        self.write(&items)
    }

    fn delete(&self, name: &str) -> Result<(), String> {
        let mut items = self.read()?;
        if items.remove(name).is_some() {
            self.write(&items)?;
        }
        Ok(())
    }
}

// ---- unreachable, and in memory ------------------------------------------------

struct Unavailable {
    reason: String,
}

impl Backend for Unavailable {
    fn get(&self, _: &str) -> Result<Option<Vec<u8>>, String> {
        Err(self.reason.clone())
    }

    fn set(&self, _: &str, _: &[u8]) -> Result<(), String> {
        Err(self.reason.clone())
    }

    fn delete(&self, _: &str) -> Result<(), String> {
        Err(self.reason.clone())
    }
}

#[derive(Default)]
struct Memory {
    items: Mutex<BTreeMap<String, Vec<u8>>>,
    max_len: Option<usize>,
    /// Tests: every call fails with this.
    failing: Mutex<Option<String>>,
}

impl Memory {
    fn check(&self) -> Result<(), String> {
        match lock(&self.failing).clone() {
            Some(reason) => Err(reason),
            None => Ok(()),
        }
    }
}

impl Backend for Memory {
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>, String> {
        self.check()?;
        Ok(lock(&self.items).get(name).cloned())
    }

    fn set(&self, name: &str, value: &[u8]) -> Result<(), String> {
        self.check()?;
        if self.max_len.is_some_and(|max| value.len() > max) {
            return Err(format!("{name}: value too long"));
        }
        lock(&self.items).insert(name.to_owned(), value.to_vec());
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<(), String> {
        self.check()?;
        lock(&self.items).remove(name);
        Ok(())
    }

    fn max_len(&self) -> usize {
        self.max_len.unwrap_or(usize::MAX)
    }
}

// ---- the store ---------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Os,
    File,
    Unavailable,
    Memory,
}

/// `secret-store.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Marker {
    /// `os` or `file`.
    backend: String,
    namespace: String,
}

/// The device's secrets. Cheap to share (`Arc`); every call is serialised.
pub struct SecretStore {
    backend: Arc<dyn Backend>,
    kind: Kind,
    label: String,
    /// Why the store is not the OS one, fixed at open.
    notice: Option<String>,
    /// The last failed read or write, until a later one succeeds.
    last_error: Mutex<Option<String>>,
    ops: Mutex<()>,
}

impl std::fmt::Debug for SecretStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretStore")
            .field("kind", &self.kind)
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

impl SecretStore {
    /// The platform's store for `config_dir` (see the module docs).
    pub fn open(config_dir: &Path) -> Self {
        // The marker and the fallback file live there; a missing directory
        // shows up as the file backend's own error otherwise.
        if let Err(e) = std::fs::create_dir_all(config_dir) {
            tracing::warn!("could not create {}: {e}", config_dir.display());
        }
        select(config_dir, platform::LABEL, open_platform)
    }

    /// Secrets in memory only, for tests: nothing touches the OS store.
    pub fn memory() -> Self {
        Self::with(
            Arc::new(Memory::default()),
            Kind::Memory,
            "memory".into(),
            None,
        )
    }

    /// A store that refuses every call, as a locked keyring does.
    #[cfg(test)]
    pub(crate) fn refusing(reason: &str) -> Self {
        Self::with(
            Arc::new(Unavailable {
                reason: reason.to_owned(),
            }),
            Kind::Unavailable,
            "the keyring".into(),
            None,
        )
    }

    fn with(backend: Arc<dyn Backend>, kind: Kind, label: String, notice: Option<String>) -> Self {
        Self {
            backend,
            kind,
            label,
            notice,
            last_error: Mutex::new(None),
            ops: Mutex::new(()),
        }
    }

    pub fn status(&self) -> StorageStatus {
        StorageStatus {
            backend: match self.kind {
                Kind::Os => "os",
                Kind::File => "file",
                Kind::Unavailable => "unavailable",
                Kind::Memory => "memory",
            },
            label: self.label.clone(),
            secure: self.kind == Kind::Os,
            notice: lock(&self.last_error)
                .clone()
                .or_else(|| self.notice.clone()),
        }
    }

    pub fn get(&self, name: &str) -> Result<Option<String>, Error> {
        let _op = lock(&self.ops);
        let value = self.outcome(read(&*self.backend, name))?;
        value
            .map(|bytes| {
                String::from_utf8(bytes)
                    .map_err(|_| Error::Corrupt(format!("the saved {name} is not text")))
            })
            .transpose()
    }

    pub fn set(&self, name: &str, value: &str) -> Result<(), Error> {
        let _op = lock(&self.ops);
        self.outcome(write(&*self.backend, name, value.as_bytes()))
    }

    pub fn delete(&self, name: &str) -> Result<(), Error> {
        let _op = lock(&self.ops);
        self.outcome(remove(&*self.backend, name))
    }

    /// Records a failure for Settings (and clears it on success), as a
    /// sentence naming the store. A secret value never reaches a message:
    /// the platform errors carry item names at most.
    fn outcome<T>(&self, result: Result<T, String>) -> Result<T, Error> {
        let mut last = lock(&self.last_error);
        match result {
            Ok(value) => {
                *last = None;
                Ok(value)
            }
            Err(reason) => {
                let message = format!("{} could not be used: {reason}", self.label);
                *last = Some(message.clone());
                Err(Error::Secrets(message))
            }
        }
    }
}

/// Reads one value, joining its parts when it was split.
fn read(backend: &dyn Backend, name: &str) -> Result<Option<Vec<u8>>, String> {
    let Some(value) = backend.get(name)? else {
        return Ok(None);
    };
    let Some((count, generation)) = chunk_head(&value) else {
        return Ok(Some(value));
    };
    let mut joined = Vec::new();
    for i in 0..count {
        let part = backend
            .get(&part_name(name, &generation, i))?
            .ok_or_else(|| format!("{name}: part {i} of {count} is missing"))?;
        joined.extend_from_slice(&part);
    }
    Ok(Some(joined))
}

fn write(backend: &dyn Backend, name: &str, value: &[u8]) -> Result<(), String> {
    let old = backend.get(name)?.as_deref().and_then(chunk_head);
    let max = backend.max_len();
    if value.len() <= max {
        backend.set(name, value)?;
    } else {
        // A head shorter than the limit is always possible: the limit is
        // thousands of bytes, the head tens.
        let generation = ulid::Ulid::new().to_string();
        let parts: Vec<&[u8]> = value.chunks(max).collect();
        for (i, part) in parts.iter().enumerate() {
            backend.set(&part_name(name, &generation, i), part)?;
        }
        let head = format!(
            "{}{}:{generation}",
            String::from_utf8_lossy(CHUNK_HEAD),
            parts.len()
        );
        backend.set(name, head.as_bytes())?;
    }
    if let Some((count, generation)) = old {
        delete_parts(backend, name, count, &generation);
    }
    Ok(())
}

fn remove(backend: &dyn Backend, name: &str) -> Result<(), String> {
    let old = backend.get(name)?.as_deref().and_then(chunk_head);
    backend.delete(name)?;
    if let Some((count, generation)) = old {
        delete_parts(backend, name, count, &generation);
    }
    Ok(())
}

/// Old parts are garbage once the head moved on; a failure only leaves an
/// unreferenced item behind.
fn delete_parts(backend: &dyn Backend, name: &str, count: usize, generation: &str) {
    for i in 0..count {
        if let Err(e) = backend.delete(&part_name(name, generation, i)) {
            tracing::warn!("could not delete an old part of {name}: {e}");
        }
    }
}

fn chunk_head(value: &[u8]) -> Option<(usize, String)> {
    let rest = std::str::from_utf8(value.strip_prefix(CHUNK_HEAD)?).ok()?;
    let (count, generation) = rest.split_once(':')?;
    Some((count.parse().ok()?, generation.to_owned()))
}

fn part_name(name: &str, generation: &str, i: usize) -> String {
    format!("{name}#{generation}.{i}")
}

/// Picks the backend for `config_dir`; `open_os` opens the OS store for a
/// namespace. Never fails: a broken store becomes [`Kind::Unavailable`] or
/// the file, with a notice.
fn select<F>(config_dir: &Path, label: &str, open_os: F) -> SecretStore
where
    F: FnOnce(&str) -> Result<Arc<dyn Backend>, String>,
{
    let marker_path = config_dir.join(MARKER_FILE);
    let file_path = config_dir.join(FALLBACK_FILE);
    let marker: Option<Marker> = match crate::config::read_json::<Option<Marker>>(&marker_path) {
        Ok(marker) => marker,
        Err(e) => {
            tracing::warn!("{MARKER_FILE} unreadable, choosing the store again: {e}");
            None
        }
    };
    let file = File {
        path: file_path.clone(),
    };

    if let Some(marker) = marker.as_ref().filter(|m| m.backend == "os") {
        return match open_os(&marker.namespace) {
            Ok(os) => {
                // A crash between writing the marker and deleting the file
                // leaves it behind: finish the move.
                let notice = if file_path.exists() {
                    migrate(&file, &*os, &file_path).err().map(|reason| {
                        format!(
                            "some keys are still in {} because {label} refused them ({reason}); they move there at the next launch",
                            file_path.display()
                        )
                    })
                } else {
                    None
                };
                SecretStore::with(os, Kind::Os, label.to_owned(), notice)
            }
            Err(reason) => {
                tracing::warn!("{label} unreachable: {reason}");
                let notice = format!(
                    "Keys are saved in {label}, which could not be reached ({reason}). Saved keys can't be read and new ones can't be saved until it is available again."
                );
                SecretStore::with(
                    Arc::new(Unavailable {
                        reason: format!("it could not be reached ({reason})"),
                    }),
                    Kind::Unavailable,
                    label.to_owned(),
                    Some(notice),
                )
            }
        };
    }

    let namespace = marker
        .map(|m| m.namespace)
        .unwrap_or_else(|| ulid::Ulid::new().to_string());
    let os = open_os(&namespace).and_then(|os| probe(&*os).map(|()| os));
    let reason = match os {
        Ok(os) => match migrate(&file, &*os, &file_path).and_then(|()| {
            write_marker(&marker_path, "os", &namespace)?;
            match std::fs::remove_file(&file_path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                // The marker already says `os`: the next launch finishes.
                Err(e) => {
                    tracing::warn!("could not delete {}: {e}", file_path.display());
                    Ok(())
                }
            }
        }) {
            Ok(()) => return SecretStore::with(os, Kind::Os, label.to_owned(), None),
            Err(reason) => reason,
        },
        Err(reason) => reason,
    };
    tracing::warn!("{label} not used, keeping secrets in {FALLBACK_FILE}: {reason}");
    if let Err(e) = write_marker(&marker_path, "file", &namespace) {
        tracing::warn!("could not write {MARKER_FILE}: {e}");
    }
    let notice = format!(
        "{} could not be used ({reason}), so keys are kept in {}, readable only by your account. They move there automatically once it works.",
        capitalise(label),
        file_path.display()
    );
    SecretStore::with(
        Arc::new(file),
        Kind::File,
        file_path.display().to_string(),
        Some(notice),
    )
}

/// Write, read back and delete one item: opening a store proves little (the
/// Secret Service crate opens without a daemon behind the bus name).
fn probe(os: &dyn Backend) -> Result<(), String> {
    let value = ulid::Ulid::new().to_string();
    os.set(PROBE, value.as_bytes())?;
    let read = os.get(PROBE)?;
    os.delete(PROBE)?;
    if read.as_deref() == Some(value.as_bytes()) {
        Ok(())
    } else {
        Err("a test item did not read back".into())
    }
}

/// Copies every item of the fallback file into `os`, joined values included.
fn migrate(file: &File, os: &dyn Backend, path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let items = file.read()?;
    for (name, value) in &items {
        if name.contains('#') {
            // A part of a split value; `read` joins it under its head.
            continue;
        }
        if let Some(value) = read(file, name)? {
            write(os, name, &value)?;
        } else {
            write(os, name, value.as_bytes())?;
        }
    }
    Ok(())
}

fn write_marker(path: &Path, backend: &str, namespace: &str) -> Result<(), String> {
    crate::config::write_json(
        path,
        &Marker {
            backend: backend.to_owned(),
            namespace: namespace.to_owned(),
        },
        false,
    )
    .map_err(|e| e.to_string())
}

fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lh-secrets-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn memory(max_len: Option<usize>) -> Arc<Memory> {
        Arc::new(Memory {
            max_len,
            ..Memory::default()
        })
    }

    fn opener(os: &Arc<Memory>) -> impl FnOnce(&str) -> Result<Arc<dyn Backend>, String> {
        let os = Arc::clone(os);
        move |_| Ok(os as Arc<dyn Backend>)
    }

    fn failing(_: &str) -> Result<Arc<dyn Backend>, String> {
        Err("no Secret Service on the session bus".into())
    }

    fn marker(dir: &Path) -> Marker {
        serde_json::from_slice(&std::fs::read(dir.join(MARKER_FILE)).unwrap()).unwrap()
    }

    #[test]
    fn long_values_split_and_join_and_leave_no_stale_parts() {
        // The head (`\0chunks:<n>:<ulid>`, 36 bytes) must fit one item.
        let os = memory(Some(64));
        let store = SecretStore::with(os.clone(), Kind::Os, "test".into(), None);
        // `é` straddles the first part boundary: parts are bytes, not chars.
        let long = "x".repeat(63) + "é" + &"z".repeat(86);
        store.set("credential/openai-codex", &long).unwrap();
        assert_eq!(store.get("credential/openai-codex").unwrap().unwrap(), long);
        // 151 bytes over 64 → head + 3 parts.
        assert_eq!(lock(&os.items).len(), 4);

        // A longer value replaces every part; a short one drops them all.
        let longer = "y".repeat(300);
        store.set("credential/openai-codex", &longer).unwrap();
        assert_eq!(
            store.get("credential/openai-codex").unwrap().unwrap(),
            longer
        );
        assert_eq!(lock(&os.items).len(), 1 + 5);
        store.set("credential/openai-codex", "short").unwrap();
        assert_eq!(lock(&os.items).len(), 1);
        assert_eq!(
            store.get("credential/openai-codex").unwrap().unwrap(),
            "short"
        );

        store.set("credential/openai-codex", &long).unwrap();
        store.delete("credential/openai-codex").unwrap();
        assert!(lock(&os.items).is_empty(), "delete removes the parts too");
        assert_eq!(store.get("credential/openai-codex").unwrap(), None);
    }

    #[test]
    fn a_working_os_store_takes_over_the_file_and_the_file_is_deleted() {
        let dir = temp();
        // A previous launch without a Secret Service.
        let first = select(&dir, "the keyring", failing);
        assert_eq!(first.status().backend, "file");
        first.set(GIT_TOKEN, "ghp_secret").unwrap();
        let namespace = marker(&dir).namespace;
        assert!(dir.join(FALLBACK_FILE).exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(FALLBACK_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        // Now it works: the token moves, the file goes, the namespace stays.
        let os = memory(None);
        let store = select(&dir, "the keyring", opener(&os));
        let status = store.status();
        assert_eq!(
            (status.backend, status.secure, status.notice),
            ("os", true, None)
        );
        assert_eq!(store.get(GIT_TOKEN).unwrap().as_deref(), Some("ghp_secret"));
        assert!(!dir.join(FALLBACK_FILE).exists());
        assert_eq!(
            marker(&dir),
            Marker {
                backend: "os".into(),
                namespace
            }
        );
        assert!(!lock(&os.items).contains_key(PROBE), "the probe cleans up");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn without_an_os_store_secrets_stay_in_the_private_file_with_a_notice() {
        let dir = temp();
        let store = select(&dir, "the Secret Service (desktop keyring)", failing);
        let status = store.status();
        assert_eq!(status.backend, "file");
        assert!(!status.secure);
        let notice = status.notice.unwrap();
        assert!(
            notice.starts_with(
                "The Secret Service (desktop keyring) could not be used (no Secret Service"
            ),
            "{notice}"
        );
        assert!(notice.contains(FALLBACK_FILE), "{notice}");
        store.set(WEBDAV_PASSWORD, "app-pass").unwrap();
        // A second launch reads it back.
        let again = select(&dir, "x", failing);
        assert_eq!(
            again.get(WEBDAV_PASSWORD).unwrap().as_deref(),
            Some("app-pass")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn once_the_os_store_held_the_secrets_a_failure_never_falls_back_to_a_file() {
        let dir = temp();
        let os = memory(None);
        let first = select(&dir, "the keyring", opener(&os));
        first.set(GIT_TOKEN, "ghp_secret").unwrap();

        let store = select(&dir, "the keyring", failing);
        let status = store.status();
        assert_eq!(status.backend, "unavailable");
        assert!(status.notice.unwrap().contains("could not be reached"));
        assert!(store.get(GIT_TOKEN).is_err());
        let error = store.set(GIT_TOKEN, "ghp_new").unwrap_err().to_string();
        assert!(
            error.starts_with("the keyring could not be used"),
            "{error}"
        );
        assert!(
            !dir.join(FALLBACK_FILE).exists(),
            "no plaintext copy appears"
        );
        assert_eq!(marker(&dir).backend, "os");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_refused_migration_keeps_the_file_and_finishes_later() {
        let dir = temp();
        select(&dir, "k", failing)
            .set(GIT_TOKEN, "ghp_secret")
            .unwrap();
        // The probe passes, then the store refuses every other item (a
        // collection locked between the two).
        let os = memory(None);
        let refusing = {
            let os = Arc::clone(&os);
            move |_: &str| -> Result<Arc<dyn Backend>, String> {
                struct AfterProbe(Arc<Memory>);
                impl Backend for AfterProbe {
                    fn get(&self, n: &str) -> Result<Option<Vec<u8>>, String> {
                        self.0.get(n)
                    }
                    fn set(&self, n: &str, v: &[u8]) -> Result<(), String> {
                        if n == PROBE {
                            self.0.set(n, v)
                        } else {
                            Err("the collection is locked".into())
                        }
                    }
                    fn delete(&self, n: &str) -> Result<(), String> {
                        self.0.delete(n)
                    }
                }
                Ok(Arc::new(AfterProbe(os)))
            }
        };
        let store = select(&dir, "k", refusing);
        assert_eq!(store.status().backend, "file");
        assert!(store
            .status()
            .notice
            .unwrap()
            .contains("the collection is locked"));
        assert_eq!(store.get(GIT_TOKEN).unwrap().as_deref(), Some("ghp_secret"));
        assert_eq!(marker(&dir).backend, "file");

        let store = select(&dir, "k", opener(&os));
        assert_eq!(store.status().backend, "os");
        assert_eq!(store.get(GIT_TOKEN).unwrap().as_deref(), Some("ghp_secret"));
        assert!(!dir.join(FALLBACK_FILE).exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn failures_show_in_the_status_until_an_operation_succeeds() {
        let os = memory(None);
        let store = SecretStore::with(os.clone(), Kind::Os, "the keyring".into(), None);
        *lock(&os.failing) = Some("prompt dismissed".into());
        assert!(store.get(GIT_TOKEN).is_err());
        assert_eq!(
            store.status().notice.as_deref(),
            Some("the keyring could not be used: prompt dismissed")
        );
        *lock(&os.failing) = None;
        store.get(GIT_TOKEN).unwrap();
        assert_eq!(store.status().notice, None);
    }
}
