//! Registries, install and the plugin lock (build prompt W8, D9).
//!
//! An index is one `index.json` served from an HTTPS URL or a local path.
//! Install verifies sha256, unpacks under `<config>/plugins/<id>/<sha256>/`,
//! validates through the W5 loader, then records the plugin in the data
//! repo's `plugins.lock`.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Error;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryIndex {
    pub registry: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub plugins: Vec<IndexPlugin>,
}

/// One plugin in a registry index. Keys follow `plugin.toml` (`use_when`,
/// `avoid_when`), as the W8 index format specifies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexPlugin {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub use_when: String,
    #[serde(default)]
    pub avoid_when: String,
    #[serde(default)]
    pub platforms: Vec<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub versions: Vec<IndexVersion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexVersion {
    pub version: String,
    pub api: u32,
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub published: Option<String>,
}

/// One line of `plugins.lock` in the data repo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LockEntry {
    pub id: String,
    pub version: String,
    pub sha256: String,
    pub source: LockSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LockSource {
    Registry { registry: String },
    Folder { folder: String },
}

/// The owner's registry (the `Ps-learning-harness-plugins` repository),
/// listed on every device unless the learner removes it.
pub const DEFAULT_REGISTRY_NAME: &str = "P's Learning Harness plugins";
pub const DEFAULT_REGISTRY_URL: &str =
    "https://dissatisfiedp.github.io/Ps-learning-harness-plugins/index.json";

/// A registry this device lists: the built-in default, or one the learner
/// added (`settings.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Registry {
    pub name: String,
    pub url: String,
    /// The built-in default. Never stored: settings hold only the learner's
    /// own registries.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub builtin: bool,
}

impl Registry {
    pub fn default_registry() -> Self {
        Self {
            name: DEFAULT_REGISTRY_NAME.to_owned(),
            url: DEFAULT_REGISTRY_URL.to_owned(),
            builtin: true,
        }
    }
}

/// A development plugin loaded from a folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderPlugin {
    pub id: String,
    pub path: String,
}

/// One configured registry's last fetch state, for Settings.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryStatus {
    pub name: String,
    pub url: String,
    pub ok: bool,
    pub error: Option<String>,
    pub fetched_at: Option<String>,
    pub plugins: usize,
}

/// Largest package archive accepted for download (compressed).
pub const MAX_ARCHIVE_BYTES: u64 = 16 * 1024 * 1024;
/// Largest unpacked size of one archive entry, and of the whole package.
pub const MAX_ENTRY_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_UNPACKED_BYTES: u64 = 64 * 1024 * 1024;

pub fn parse_index(text: &str) -> Result<RegistryIndex, Error> {
    let index: RegistryIndex =
        serde_json::from_str(text).map_err(|e| Error::Invalid(format!("registry index: {e}")))?;
    if index.registry != 1 {
        return Err(Error::Invalid(format!(
            "registry format {} is not supported (this build reads 1)",
            index.registry
        )));
    }
    // Ids and hashes become path segments under config/plugins/.
    for p in &index.plugins {
        if !super::manifest::valid_id(&p.id) {
            return Err(Error::Invalid(format!(
                "registry index: plugin id `{}` is not a valid plugin id",
                p.id
            )));
        }
        if let Some(v) = p.versions.iter().find(|v| !valid_sha(&v.sha256)) {
            return Err(Error::Invalid(format!(
                "registry index: `{}` {} has sha256 `{}`, not 64 hex digits",
                p.id, v.version, v.sha256
            )));
        }
    }
    Ok(index)
}

fn valid_sha(sha: &str) -> bool {
    sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Read an index from `file://`, a local path, or an HTTP(S) URL.
pub async fn fetch_index(client: &reqwest::Client, url: &str) -> Result<String, Error> {
    let local = url.strip_prefix("file://").map(PathBuf::from).or_else(|| {
        if url.starts_with("http://") || url.starts_with("https://") {
            None
        } else {
            Some(PathBuf::from(url))
        }
    });
    if let Some(path) = local {
        return std::fs::read_to_string(path).map_err(Error::Io);
    }
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| Error::Invalid(format!("can't fetch the registry ({e})")))?;
    if !response.status().is_success() {
        return Err(Error::Invalid(format!(
            "registry returned HTTP {}",
            response.status()
        )));
    }
    response
        .text()
        .await
        .map_err(|e| Error::Invalid(format!("can't read the registry ({e})")))
}

/// Download an archive from a URL (or a local path) and verify its hash.
pub async fn download(
    client: &reqwest::Client,
    url: &str,
    expected_sha: &str,
) -> Result<Vec<u8>, Error> {
    let bytes = if let Some(path) = url.strip_prefix("file://").map(PathBuf::from).or_else(|| {
        if url.starts_with("http://") || url.starts_with("https://") {
            None
        } else {
            Some(PathBuf::from(url))
        }
    }) {
        let meta = std::fs::metadata(&path)?;
        if meta.len() > MAX_ARCHIVE_BYTES {
            return Err(too_big_archive());
        }
        std::fs::read(path)?
    } else {
        let response = client
            .get(url)
            .send()
            .await
            .map_err(|e| Error::Invalid(format!("can't download the plugin ({e})")))?;
        if !response.status().is_success() {
            return Err(Error::Invalid(format!(
                "download returned HTTP {}",
                response.status()
            )));
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_ARCHIVE_BYTES)
        {
            return Err(too_big_archive());
        }
        let mut response = response;
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| Error::Invalid(format!("can't read the download ({e})")))?
        {
            bytes.extend_from_slice(&chunk);
            if bytes.len() as u64 > MAX_ARCHIVE_BYTES {
                return Err(too_big_archive());
            }
        }
        bytes
    };
    let actual = crate::workspace::sha256_hex(&bytes);
    if !actual.eq_ignore_ascii_case(expected_sha) {
        return Err(Error::Invalid(format!(
            "the archive's sha256 is {actual}, not the index's {expected_sha}; refusing to install"
        )));
    }
    Ok(bytes)
}

fn too_big_archive() -> Error {
    Error::Invalid(format!(
        "the package archive is over {} MiB; refusing to install",
        MAX_ARCHIVE_BYTES / (1024 * 1024)
    ))
}

/// Unpack a `.tar.gz` package into `dest`. The archive may put `plugin.toml`
/// at its root or inside one top-level directory. Refuses absolute paths and
/// `..` entries. The staging directory is removed whether or not it works.
pub fn unpack(bytes: &[u8], dest: &Path) -> Result<(), Error> {
    let temp = dest.with_extension("unpack");
    let _ = std::fs::remove_dir_all(&temp);
    let result = unpack_via(bytes, &temp, dest);
    let _ = std::fs::remove_dir_all(&temp);
    result
}

fn unpack_via(bytes: &[u8], temp: &Path, dest: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(temp)?;
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    let mut total: u64 = 0;
    for entry in archive
        .entries()
        .map_err(|e| Error::Invalid(format!("archive: {e}")))?
    {
        let mut entry = entry.map_err(|e| Error::Invalid(format!("archive: {e}")))?;
        let raw = entry
            .path()
            .map_err(|e| Error::Invalid(format!("archive path: {e}")))?
            .to_path_buf();
        let rel = entry_path(&raw)
            .ok_or_else(|| Error::Invalid("the archive contains an unsafe path".into()))?;
        let out = temp.join(&rel);
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir)?;
        }
        if entry.header().entry_type().is_dir() {
            std::fs::create_dir_all(&out)?;
        } else {
            let mut buf = Vec::new();
            (&mut entry)
                .take(MAX_ENTRY_BYTES + 1)
                .read_to_end(&mut buf)
                .map_err(|e| Error::Invalid(format!("archive entry: {e}")))?;
            if buf.len() as u64 > MAX_ENTRY_BYTES {
                return Err(Error::Invalid(format!(
                    "archive entry `{}` unpacks to over {} MiB; refusing to install",
                    rel.display(),
                    MAX_ENTRY_BYTES / (1024 * 1024)
                )));
            }
            total += buf.len() as u64;
            if total > MAX_UNPACKED_BYTES {
                return Err(Error::Invalid(format!(
                    "the package unpacks to over {} MiB; refusing to install",
                    MAX_UNPACKED_BYTES / (1024 * 1024)
                )));
            }
            std::fs::write(&out, buf)?;
        }
    }
    let root = if temp.join("plugin.toml").is_file() {
        temp.to_path_buf()
    } else {
        let mut found = None;
        for entry in std::fs::read_dir(temp)?.flatten() {
            if entry.path().join("plugin.toml").is_file() {
                found = Some(entry.path());
                break;
            }
        }
        found.ok_or_else(|| Error::Invalid("the archive has no plugin.toml".into()))?
    };
    if dest.exists() {
        std::fs::remove_dir_all(dest)?;
    }
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(&root)?.flatten() {
        let target = dest.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// The path an archive entry is written to, relative to the extraction root.
/// Absolute paths and `..` are refused; `.` components are dropped.
fn entry_path(raw: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in raw.components() {
        match component {
            std::path::Component::Normal(part) => out.push(part),
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)?.flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

pub fn lock_path(repo: &Path) -> PathBuf {
    repo.join("plugins.lock")
}

/// The lock, or an error when it exists but can't be parsed (a corrupt lock
/// is never silently treated as empty and overwritten).
pub fn read_lock(repo: &Path) -> Result<Vec<LockEntry>, Error> {
    match std::fs::read(lock_path(repo)) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
            Error::Invalid(format!(
                "plugins.lock in the data folder can't be read ({e}); fix or delete it"
            ))
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

pub fn write_lock(repo: &Path, entries: &[LockEntry]) -> Result<(), Error> {
    let mut sorted = entries.to_vec();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));
    std::fs::write(lock_path(repo), serde_json::to_vec_pretty(&sorted)?)?;
    Ok(())
}

/// Path where an installed package lives: `<config>/plugins/<id>/<sha256>`.
/// Refuses an id or hash that isn't a safe path segment.
pub fn install_dir(config_dir: &Path, id: &str, sha: &str) -> Result<PathBuf, Error> {
    check_id(id)?;
    if !valid_sha(sha) {
        return Err(Error::Invalid(format!("`{sha}` is not a sha256")));
    }
    Ok(config_dir.join("plugins").join(id).join(sha))
}

/// Remove every installed copy of `id` (all hashes).
pub fn remove_installed(config_dir: &Path, id: &str) -> Result<(), Error> {
    check_id(id)?;
    let dir = config_dir.join("plugins").join(id);
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    Ok(())
}

/// An id used as a path segment must be a valid plugin id.
pub fn check_id(id: &str) -> Result<(), Error> {
    if super::manifest::valid_id(id) {
        Ok(())
    } else {
        Err(Error::Invalid(format!("`{id}` is not a valid plugin id")))
    }
}

/// Cache path for one registry index.
pub fn cache_path(config_dir: &Path, url: &str) -> PathBuf {
    let key = crate::workspace::sha256_hex(url.as_bytes());
    config_dir
        .join("registry-cache")
        .join(format!("{key}.json"))
}
#[cfg(test)]
mod tests {
    use super::*;

    fn targz(files: &[(&str, &str)]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let enc = flate2::write::GzEncoder::new(&mut out, flate2::Compression::fast());
            let mut builder = tar::Builder::new(enc);
            for (path, body) in files {
                let mut header = tar::Header::new_gnu();
                header.set_size(body.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder
                    .append_data(&mut header, path, body.as_bytes())
                    .unwrap();
            }
            builder.finish().unwrap();
        }
        out
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lh-install-{name}-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn unpack_handles_a_single_top_level_directory_and_a_flat_archive() {
        let dir = scratch("unpack");
        for (name, prefix) in [("nested", "circuit-0.1.0/"), ("flat", "")] {
            let dest = dir.join(name);
            let bytes = targz(&[
                (&format!("{prefix}plugin.toml"), "id = \"circuit\"\n"),
                (&format!("{prefix}src/main.luau"), "return {}\n"),
            ]);
            unpack(&bytes, &dest).unwrap();
            assert!(dest.join("plugin.toml").is_file(), "{name}");
            assert!(dest.join("src/main.luau").is_file(), "{name}");
            assert!(!dest.join("circuit-0.1.0").exists(), "{name}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unpack_refuses_traversal_and_missing_manifests() {
        let dir = scratch("refuse");
        let dest = dir.join("dest");
        // The tar writer refuses to *create* `..` entries, so the guard is
        // tested on the path rule itself.
        assert_eq!(entry_path(Path::new("../evil.txt")), None);
        assert_eq!(entry_path(Path::new("/etc/passwd")), None);
        assert_eq!(entry_path(Path::new(".")), None);
        assert_eq!(
            entry_path(Path::new("./plugin.toml")),
            Some(PathBuf::from("plugin.toml"))
        );
        assert_eq!(
            entry_path(Path::new("src/./main.luau")),
            Some(PathBuf::from("src/main.luau"))
        );

        let no_manifest = targz(&[("src/main.luau", "return {}\n")]);
        let err = unpack(&no_manifest, &dest).unwrap_err().to_string();
        assert!(err.contains("no plugin.toml"), "{err}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_lock_file_round_trips_sorted_and_tolerates_absence() {
        let repo = scratch("lock");
        assert!(read_lock(&repo).unwrap().is_empty());
        let entries = vec![
            LockEntry {
                id: "zeta".into(),
                version: "1.0.0".into(),
                sha256: "b".into(),
                source: LockSource::Registry {
                    registry: "https://example.com/index.json".into(),
                },
            },
            LockEntry {
                id: "alpha".into(),
                version: "0.2.0".into(),
                sha256: "a".into(),
                source: LockSource::Folder {
                    folder: "dev".into(),
                },
            },
        ];
        write_lock(&repo, &entries).unwrap();
        let read = read_lock(&repo).unwrap();
        assert_eq!(
            read.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["alpha", "zeta"]
        );
        assert_eq!(read, {
            let mut sorted = entries.clone();
            sorted.sort_by(|a, b| a.id.cmp(&b.id));
            sorted
        });
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn a_corrupt_lock_is_an_error_not_an_empty_list() {
        let repo = scratch("corrupt");
        std::fs::write(lock_path(&repo), "[{ not json").unwrap();
        let err = read_lock(&repo).unwrap_err().to_string();
        assert!(
            err.contains("plugins.lock") && err.contains("can't be read"),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn ids_that_are_not_safe_path_segments_are_refused() {
        let config = scratch("ids");
        let victim = config.join("victim");
        std::fs::create_dir_all(&victim).unwrap();
        let sha = "a".repeat(64);
        for id in ["../x", "..", "a/b", "", "Up"] {
            assert!(
                install_dir(&config, id, &sha).is_err(),
                "install_dir {id:?}"
            );
            assert!(remove_installed(&config, id).is_err(), "remove {id:?}");
        }
        // `plugins/../victim` would have deleted this.
        assert!(remove_installed(&config, "../victim").is_err());
        assert!(victim.is_dir());
        assert!(install_dir(&config, "circuit", "../../etc").is_err());
        assert_eq!(
            install_dir(&config, "circuit", &sha).unwrap(),
            config.join("plugins/circuit").join(&sha)
        );
        let index = |id: &str, sha: &str| {
            serde_json::json!({ "registry": 1, "plugins": [{ "id": id, "title": "t",
                "versions": [{ "version": "0.1.0", "api": 1, "url": "u", "sha256": sha }] }] })
            .to_string()
        };
        let err = parse_index(&index("../x", &sha)).unwrap_err().to_string();
        assert!(
            err.contains("plugin id `../x` is not a valid plugin id"),
            "{err}"
        );
        let err = parse_index(&index("circuit", "../../x"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("not 64 hex digits"), "{err}");
        assert!(parse_index(&index("circuit", &sha)).is_ok());
        let _ = std::fs::remove_dir_all(config);
    }

    #[test]
    fn an_entry_over_the_size_cap_is_refused() {
        let dir = scratch("bomb");
        let big = "a".repeat(MAX_ENTRY_BYTES as usize + 1);
        let bytes = targz(&[
            ("plugin.toml", "id = \"x\"\n"),
            ("src/big.luau", big.as_str()),
        ]);
        let err = unpack(&bytes, &dir.join("dest")).unwrap_err().to_string();
        assert!(err.contains("unpacks to over 16 MiB"), "{err}");
        assert!(!dir.join("dest").exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
