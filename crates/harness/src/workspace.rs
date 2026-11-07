//! Session workspace folders (build prompt W4, D12).
//!
//! The mapping session → absolute folder path is device-local
//! (`<config>/workspaces.json`); the session op log records only the folder's
//! name. The agent may read inside the folder and write text files through
//! `workspace_write`, which refuses to clobber a file whose bytes changed
//! since the agent last saw them.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Error;

/// Largest single `workspace_write` payload.
pub const MAX_WRITE_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Map {
    #[serde(default)]
    workspaces: HashMap<String, String>,
}

/// Device-local workspace mapping.
pub struct Workspaces {
    path: PathBuf,
    map: Map,
}

impl Workspaces {
    pub fn load(config_dir: &Path) -> Result<Self, Error> {
        let path = config_dir.join("workspaces.json");
        let map = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Map::default(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self { path, map })
    }

    fn save(&self) -> Result<(), Error> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(&self.map)?)?;
        fs::rename(tmp, &self.path)?;
        Ok(())
    }

    pub fn get(&self, session_id: &str) -> Option<PathBuf> {
        self.map.workspaces.get(session_id).map(PathBuf::from)
    }

    pub fn attach(&mut self, session_id: &str, dir: &Path) -> Result<String, Error> {
        let canonical = dir
            .canonicalize()
            .map_err(|e| Error::Invalid(format!("can't open that folder ({e})")))?;
        if !canonical.is_dir() {
            return Err(Error::Invalid("that path is not a folder".into()));
        }
        let name = canonical
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| canonical.to_string_lossy().into_owned());
        self.map.workspaces.insert(
            session_id.to_owned(),
            canonical.to_string_lossy().into_owned(),
        );
        self.save()?;
        Ok(name)
    }

    pub fn detach(&mut self, session_id: &str) -> Result<(), Error> {
        if self.map.workspaces.remove(session_id).is_some() {
            self.save()?;
        }
        Ok(())
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn file_hash(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    Some(sha256_hex(&bytes))
}

/// Resolve `path` inside the workspace, refusing escapes through `..` or
/// symlinks (the same rule as the file tools). The path is checked lexically
/// first (relative, no `..`), so a not-yet-existing folder can't be used to
/// climb out; then the nearest existing entry (a dangling symlink counts) is
/// canonicalised and must stay inside the canonical root.
pub fn resolve(root: &Path, path: &str) -> Result<PathBuf, String> {
    use std::path::Component;
    let root = root
        .canonicalize()
        .map_err(|_| "the workspace folder is unavailable".to_owned())?;
    let mut rel = PathBuf::new();
    for part in Path::new(path.trim()).components() {
        match part {
            Component::Normal(p) => rel.push(p),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(format!(
                    "`{path}` is outside the workspace (`..` is not allowed)"
                ))
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                "`{path}` is outside the workspace (use a path relative to the workspace folder)"
            ))
            }
        }
    }
    if rel.as_os_str().is_empty() {
        return Err("path is empty".into());
    }
    let candidate = root.join(&rel);
    // The target may not exist yet; canonicalise the nearest existing entry.
    let mut probe = candidate.clone();
    while probe.symlink_metadata().is_err() {
        match probe.parent() {
            Some(p) => probe = p.to_path_buf(),
            None => return Err(format!("`{path}` is outside the workspace")),
        }
    }
    let canonical = probe
        .canonicalize()
        .map_err(|_| format!("`{path}` is outside the workspace"))?;
    if !canonical.starts_with(&root) {
        return Err(format!("`{path}` is outside the workspace"));
    }
    Ok(candidate)
}

/// The workspace-relative form (`/`-separated) of `full`, a path inside the
/// workspace, or `None` when it lies outside. Hashes the agent has seen are
/// keyed by this, so a read and a write of the same file always agree
/// however the agent spelled the path.
pub fn relative(root: &Path, full: &Path) -> Option<String> {
    let root = root.canonicalize().ok()?;
    let rel = full.strip_prefix(&root).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// `workspace_write { path, content }` body (the tool wrapper lives in
/// `agent.rs` so it can record the op).
pub fn write(
    root: &Path,
    path: &str,
    content: &str,
    expected_hash: Option<&str>,
) -> Result<(PathBuf, String, u64), String> {
    if content.len() > MAX_WRITE_BYTES {
        return Err(format!(
            "the file is over the {} KiB limit; write a smaller file",
            MAX_WRITE_BYTES / 1024
        ));
    }
    let full = resolve(root, path)?;
    if full.is_dir() {
        return Err(format!("`{path}` is a folder"));
    }
    if let Some(ext) = full.extension() {
        let ext = ext.to_string_lossy().to_ascii_lowercase();
        if matches!(
            ext.as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "pdf" | "zip" | "gz" | "wasm" | "exe" | "so"
        ) {
            return Err(format!("`{path}` is not a text file"));
        }
    }
    if full.exists() {
        let mut old = Vec::new();
        fs::File::open(&full)
            .and_then(|mut f| f.read_to_end(&mut old))
            .map_err(|e| format!("can't read `{path}` ({e})"))?;
        let current = sha256_hex(&old);
        match expected_hash {
            Some(want) if want == current => {}
            _ => {
                return Err(format!(
                    "`{path}` changed since you last read or wrote it (or you haven't read it); call file_read first, then write again"
                ))
            }
        }
    }
    if let Some(dir) = full.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("can't create the folder ({e})"))?;
    }
    fs::write(&full, content).map_err(|e| format!("can't write `{path}` ({e})"))?;
    let hash = sha256_hex(content.as_bytes());
    Ok((full, hash, content.len() as u64))
}

impl crate::Harness {
    /// Attach a folder to a session (W4). Records only the folder's name in
    /// the op log; the absolute path stays in `<config>/workspaces.json`.
    pub fn attach_workspace(&self, session_id: &str, path: &str) -> Result<String, crate::Error> {
        let expanded = crate::files::expand_home(path);
        let name = crate::lock(&self.workspaces).attach(session_id, &expanded)?;
        self.append(
            session_id,
            crate::Op::Workspace(crate::sessions::WorkspaceOp {
                op_id: crate::sessions::op_id(),
                at: crate::now_iso(),
                attached: true,
                name: name.clone(),
            }),
        )?;
        Ok(name)
    }

    pub fn detach_workspace(&self, session_id: &str) -> Result<(), crate::Error> {
        crate::lock(&self.workspaces).detach(session_id)?;
        let name = crate::replay::replay_state(&self.store.ops(session_id)?)
            .workspace
            .unwrap_or_default();
        self.append(
            session_id,
            crate::Op::Workspace(crate::sessions::WorkspaceOp {
                op_id: crate::sessions::op_id(),
                at: crate::now_iso(),
                attached: false,
                name,
            }),
        )?;
        // Drop remembered hashes: write access is gone until re-attach.
        crate::lock(&self.workspace_hashes).retain(|(s, _), _| s != session_id);
        Ok(())
    }

    pub fn workspace_path(&self, session_id: &str) -> Option<PathBuf> {
        crate::lock(&self.workspaces).get(session_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lh-ws-{name}-{}", ulid::Ulid::new()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn paths_cannot_escape_the_workspace() {
        let dir = root("escape");
        assert!(resolve(&dir, "../outside.txt").is_err());
        assert!(resolve(&dir, "/etc/passwd").is_err());
        assert!(resolve(&dir, "").is_err());
        // A symlink inside the folder that points out is refused.
        let outside = root("outside");
        std::os::unix::fs::symlink(&outside, dir.join("link")).unwrap();
        assert!(resolve(&dir, "link/x.txt").is_err());
        assert!(resolve(&dir, "sub/dir/new.txt").is_ok());
        assert_eq!(
            resolve(&dir, "./sub/./a.md").unwrap(),
            dir.canonicalize().unwrap().join("sub/a.md")
        );
        // `..` through folders that don't exist yet, or that do, never
        // resolves; the write creates nothing on the way.
        for escape in ["nope/../../x.txt", "a/../../x", "sub/../x.txt"] {
            let err = resolve(&dir, escape).unwrap_err();
            assert!(err.contains("outside the workspace"), "{escape}: {err}");
            assert!(write(&dir, escape, "x", None).is_err());
        }
        let absolute = dir.join("inside.txt");
        assert!(
            resolve(&dir, absolute.to_str().unwrap()).is_err(),
            "absolute paths too"
        );
        assert!(!dir.join("nope").exists() && !dir.parent().unwrap().join("x.txt").exists());
        // A dangling symlink pointing out is refused rather than written through.
        std::os::unix::fs::symlink(outside.join("new.txt"), dir.join("dangling")).unwrap();
        assert!(write(&dir, "dangling", "x", None).is_err());
        assert!(!outside.join("new.txt").exists());
        let _ = fs::remove_dir_all(dir);
        let _ = fs::remove_dir_all(outside);
    }

    #[test]
    fn a_learner_edit_is_never_clobbered() {
        let dir = root("guard");
        let (path, hash, size) = write(&dir, "task.md", "first", None).unwrap();
        assert_eq!(size, 5);
        assert_eq!(file_hash(&path).as_deref(), Some(hash.as_str()));
        // Rewriting with the hash the agent last saw is allowed.
        write(&dir, "task.md", "second", Some(&hash)).unwrap();
        // An external edit invalidates it.
        fs::write(dir.join("task.md"), "learner edit").unwrap();
        let err = write(&dir, "task.md", "third", Some(&hash)).unwrap_err();
        assert!(err.contains("file_read"), "{err}");
        assert_eq!(
            fs::read_to_string(dir.join("task.md")).unwrap(),
            "learner edit"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn writes_stay_textual_and_bounded() {
        let dir = root("limits");
        assert!(write(&dir, "pic.png", "x", None).is_err());
        let big = "x".repeat(MAX_WRITE_BYTES + 1);
        let err = write(&dir, "big.txt", &big, None).unwrap_err();
        assert!(err.contains("KiB"), "{err}");
        // Directories are created as needed.
        assert!(write(&dir, "a/b/c.md", "x", None).is_ok());
        let _ = fs::remove_dir_all(dir);
    }
}
