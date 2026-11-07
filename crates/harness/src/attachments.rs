//! Learner attachments (build prompt W3, D11).
//!
//! Images (pasted, picked or dropped) wait in a device-local staging folder
//! (`<config>/staged-images/`) while they are chips in the composer, and move
//! into the data repo (`attachments/<yyyy>/<ulid>.<ext>`) only when the
//! message is sent, so a removed chip never reaches the synced repo. Files
//! and folders are referenced by path and read with the file tools. The
//! attachment descriptor type lives in `sessions.rs` next to the message op
//! that stores it.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use base64::Engine;
use chrono::{Datelike, Utc};
use ulid::Ulid;

use crate::files;
use crate::sessions::{Attachment, Op};
use crate::Error;

/// Largest image accepted from paste, the picker or a drop, and the largest
/// the provider request builder will inline.
pub const MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;

/// Device-local folder holding images that are chips but not yet sent.
pub fn staging_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("staged-images")
}

/// Drop every staged image (startup: chips don't survive a restart).
pub fn clear_staging(staging: &Path) {
    let _ = fs::remove_dir_all(staging);
}

fn extension(media_type: &str) -> &'static str {
    match media_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "bin",
    }
}

/// The media type the bytes prove, if they are a PNG, JPEG, GIF or WebP.
fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// `attachments/<yyyy>/<file>` → the file name, when `rel` has exactly that
/// shape (no `..`, no extra segments, a plain file name).
fn image_file_name(rel: &str) -> Option<&str> {
    let mut parts = rel.trim().split('/');
    let (Some("attachments"), Some(year), Some(file), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    let plain = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '.');
    (year.chars().all(|c| c.is_ascii_digit()) && plain(file) && !file.starts_with('.'))
        .then_some(file)
}

/// Read an image file (at most [`MAX_IMAGE_BYTES`]); the bytes decide the
/// media type.
fn read_image(path: &Path) -> Result<(&'static str, Vec<u8>), Error> {
    let meta = fs::metadata(path)?;
    if meta.len() > MAX_IMAGE_BYTES {
        return Err(Error::Invalid("that image is too large".into()));
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    fs::File::open(path)?.read_to_end(&mut bytes)?;
    let media = sniff(&bytes).ok_or_else(|| Error::Invalid("that file is not an image".into()))?;
    Ok((media, bytes))
}

/// The committed repo file for an image attachment, refusing anything
/// outside this repo's `attachments/` (symlinks included).
fn repo_image(repo: &Path, rel: &str) -> Result<PathBuf, Error> {
    image_file_name(rel).ok_or_else(|| Error::Invalid("not an image attachment".into()))?;
    let root = repo.join("attachments").canonicalize()?;
    let path = repo.join(rel.trim()).canonicalize()?;
    if !path.starts_with(&root) {
        return Err(Error::Invalid("not an image attachment".into()));
    }
    Ok(path)
}

/// Stage image bytes: checked, written to the staging folder, described by
/// the repo path they will have once sent.
fn stage_bytes(staging: &Path, bytes: &[u8], name: Option<&str>) -> Result<Attachment, Error> {
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(Error::Invalid(format!(
            "images must be at most {} MiB",
            MAX_IMAGE_BYTES / (1024 * 1024)
        )));
    }
    // The bytes decide: a file renamed to .png is refused rather than
    // inlined as an "image".
    let media = sniff(bytes)
        .ok_or_else(|| Error::Invalid("that file is not a PNG, JPEG, GIF or WebP image".into()))?;
    let file = format!("{}.{}", Ulid::new(), extension(media));
    fs::create_dir_all(staging)?;
    fs::write(staging.join(&file), bytes)?;
    Ok(Attachment {
        kind: "image".into(),
        path: format!("attachments/{}/{file}", Utc::now().year()),
        media_type: Some(media.to_owned()),
        bytes: Some(bytes.len() as u64),
        name: name.map(str::to_owned),
    })
}

/// Stage a pasted image (base64, optionally a whole data URL).
pub fn store_image(
    staging: &Path,
    data_base64: &str,
    name: Option<&str>,
) -> Result<Attachment, Error> {
    let clean = data_base64
        .rsplit_once(',')
        .filter(|(head, _)| head.contains("base64"))
        .map_or(data_base64, |(_, tail)| tail);
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(clean.trim())
        .map_err(|_| Error::Invalid("the pasted image is not valid base64".into()))?;
    stage_bytes(staging, &bytes, name)
}

/// Descriptor for a picked, dropped or mentioned path. A PNG/JPEG/GIF/WebP
/// file of at most [`MAX_IMAGE_BYTES`] is staged as an image; anything else
/// is a file or folder reference and nothing is copied. Mention paths arrive
/// in display form (`~/notes.md`, or relative to the session `workspace`).
/// Paths inside the workspace are recorded workspace-relative, so the op log
/// never holds the workspace's absolute path.
pub fn reference(
    path: &str,
    workspace: Option<&Path>,
    staging: &Path,
) -> Result<Attachment, Error> {
    let expanded = files::expand_home(path.trim());
    let full = if expanded.is_absolute() {
        expanded
    } else {
        workspace
            .map(|ws| ws.join(&expanded))
            .ok_or_else(|| Error::Invalid(format!("`{path}` is not an absolute path")))?
    };
    let meta = fs::metadata(&full)?;
    let name = full.file_name().map(|n| n.to_string_lossy().into_owned());
    if meta.is_file() && meta.len() <= MAX_IMAGE_BYTES {
        let mut head = [0u8; 12];
        let n = fs::File::open(&full)?.read(&mut head)?;
        if sniff(&head[..n]).is_some() {
            let (_, bytes) = read_image(&full)?;
            return stage_bytes(staging, &bytes, name.as_deref());
        }
    }
    let in_workspace = workspace
        .and_then(|ws| ws.canonicalize().ok())
        .zip(full.canonicalize().ok())
        .and_then(|(ws, f)| {
            f.strip_prefix(&ws)
                .ok()
                .filter(|r| !r.as_os_str().is_empty())
                .map(|r| r.to_string_lossy().replace('\\', "/"))
        });
    Ok(Attachment {
        kind: if meta.is_dir() { "folder" } else { "file" }.into(),
        path: in_workspace.unwrap_or_else(|| full.to_string_lossy().into_owned()),
        media_type: None,
        bytes: meta.is_file().then_some(meta.len()),
        name,
    })
}

/// Delete a staged image (its chip was removed or dropped before Send).
/// Never touches the repo: an image already sent stays.
pub fn unstage(staging: &Path, rel: &str) -> Result<(), Error> {
    let file =
        image_file_name(rel).ok_or_else(|| Error::Invalid("not an image attachment".into()))?;
    match fs::remove_file(staging.join(file)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Move each staged image of a message being sent into the repo. Images
/// already in the repo (a resent or restored chip) are left as they are.
pub fn commit(repo: &Path, staging: &Path, attachments: &[Attachment]) -> Result<(), Error> {
    for a in attachments.iter().filter(|a| a.kind == "image") {
        let file = image_file_name(&a.path)
            .ok_or_else(|| Error::Invalid("not an image attachment".into()))?;
        let dest = repo.join(a.path.trim());
        if dest.is_file() {
            continue;
        }
        let src = staging.join(file);
        if !src.is_file() {
            return Err(Error::Invalid(format!(
                "the image {} is no longer available; add it again",
                a.name.as_deref().unwrap_or(file)
            )));
        }
        if let Some(dir) = dest.parent() {
            fs::create_dir_all(dir)?;
        }
        // The staging folder may be on another filesystem than the repo.
        if fs::rename(&src, &dest).is_err() {
            fs::copy(&src, &dest)?;
            fs::remove_file(&src)?;
        }
    }
    Ok(())
}

/// A data URL for an image chip (staged) or a reopened learner message
/// (in the repo). Only this repo's `attachments/` and the staging folder are
/// readable.
pub fn data_url(repo: &Path, staging: &Path, rel: &str) -> Result<String, Error> {
    let file =
        image_file_name(rel).ok_or_else(|| Error::Invalid("not an image attachment".into()))?;
    let path = match repo_image(repo, rel) {
        Ok(path) => path,
        Err(_) => staging.join(file),
    };
    let (media, bytes) = read_image(&path)?;
    Ok(format!(
        "data:{media};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

/// Read a sent image attachment as `(media type, base64)` for a provider
/// request. The descriptor's path must stay inside the repo's `attachments/`
/// and the bytes, not the descriptor, decide the media type.
pub fn image_block(repo: &Path, a: &Attachment) -> Option<(String, String)> {
    if a.kind != "image" {
        return None;
    }
    let (media, bytes) = read_image(&repo_image(repo, &a.path).ok()?).ok()?;
    Some((
        media.to_owned(),
        base64::engine::general_purpose::STANDARD.encode(bytes),
    ))
}

/// One line for the model per referenced file/folder (W3: path, type, size).
/// The "read it" hint is added only when the agent has the file tools.
pub fn reference_line(a: &Attachment, file_tools: bool) -> String {
    let size = a.bytes.map_or_else(String::new, |b| {
        if a.kind == "folder" {
            String::new()
        } else if b >= 1024 {
            format!(", {} KiB", b / 1024)
        } else {
            format!(", {b} B")
        }
    });
    let hint = if file_tools {
        " — read it with file_read or file_find"
    } else {
        ""
    };
    format!("[attached {}] {}{size}{hint}", a.kind, a.path)
}

/// Every absolute path that must become a session read root because a
/// message references it (outside the home root; workspace files are
/// recorded relative and need no grant).
pub fn grants_for(attachments: &[Attachment]) -> Vec<String> {
    let home = files::home_dir();
    attachments
        .iter()
        .filter(|a| a.kind == "file" || a.kind == "folder")
        .filter(|a| {
            let path = Path::new(&a.path);
            path.is_absolute()
                && home
                    .as_deref()
                    .and_then(|h| path.strip_prefix(h).ok())
                    .is_none()
        })
        .map(|a| a.path.clone())
        .collect()
}

/// Append a read-root grant op for each path not already granted.
pub fn grant_roots(
    harness: &crate::Harness,
    session_id: &str,
    paths: &[String],
) -> Result<(), Error> {
    let existing: Vec<String> = harness
        .store
        .ops(session_id)?
        .iter()
        .filter_map(|o| match o {
            Op::FileGrant(g) => Some(g.path.clone()),
            _ => None,
        })
        .collect();
    for path in paths {
        if existing.contains(path) {
            continue;
        }
        let kind = if Path::new(path).is_dir() {
            "folder"
        } else {
            "file"
        };
        harness.append(
            session_id,
            Op::FileGrant(crate::sessions::FileGrantOp {
                op_id: crate::sessions::op_id(),
                at: crate::now_iso(),
                path: path.clone(),
                kind: kind.into(),
            }),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as B64;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest-of-the-file";

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lh-attach-{name}-{}", Ulid::new()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_pasted_png_waits_in_staging_and_reaches_the_repo_only_on_send() {
        let (repo, staging) = (temp("repo"), temp("staging"));
        let a = store_image(&staging, &B64.encode(PNG), Some("shot.png")).unwrap();
        assert_eq!(a.kind, "image");
        assert_eq!(a.media_type.as_deref(), Some("image/png"));
        assert_eq!(a.bytes, Some(PNG.len() as u64));
        let year = Utc::now().year();
        assert!(a.path.starts_with(&format!("attachments/{year}/")) && a.path.ends_with(".png"));
        // Before Send: nothing in the repo, the chip thumbnail still loads.
        assert!(!repo.join("attachments").exists());
        assert!(data_url(&repo, &staging, &a.path)
            .unwrap()
            .starts_with("data:image/png;base64,"));
        assert!(image_block(&repo, &a).is_none());

        commit(&repo, &staging, std::slice::from_ref(&a)).unwrap();
        assert_eq!(fs::read(repo.join(&a.path)).unwrap(), PNG);
        assert_eq!(fs::read_dir(&staging).unwrap().count(), 0);
        let (media, data) = image_block(&repo, &a).unwrap();
        assert_eq!(media, "image/png");
        assert_eq!(B64.decode(data).unwrap(), PNG);
        // Committing again (a restored chip) keeps the repo copy.
        commit(&repo, &staging, std::slice::from_ref(&a)).unwrap();
        let _ = fs::remove_dir_all(repo);
        let _ = fs::remove_dir_all(staging);
    }

    #[test]
    fn an_unstaged_image_is_deleted_and_cannot_be_sent() {
        let (repo, staging) = (temp("repo"), temp("staging"));
        let a = store_image(&staging, &B64.encode(PNG), None).unwrap();
        unstage(&staging, &a.path).unwrap();
        assert_eq!(fs::read_dir(&staging).unwrap().count(), 0);
        let err = commit(&repo, &staging, std::slice::from_ref(&a))
            .unwrap_err()
            .to_string();
        assert!(err.contains("add it again"), "{err}");
        assert!(!repo.join(&a.path).exists());
        // Unstaging a sent image never deletes the repo copy.
        let sent = store_image(&staging, &B64.encode(PNG), None).unwrap();
        commit(&repo, &staging, std::slice::from_ref(&sent)).unwrap();
        unstage(&staging, &sent.path).unwrap();
        assert!(repo.join(&sent.path).is_file());
        assert!(unstage(&staging, "attachments/2026/../../config.json").is_err());
        let _ = fs::remove_dir_all(repo);
        let _ = fs::remove_dir_all(staging);
    }

    #[test]
    fn a_data_url_prefix_is_stripped_and_non_images_are_refused() {
        let staging = temp("refuse");
        let url = format!("data:image/png;base64,{}", B64.encode(PNG));
        assert!(store_image(&staging, &url, None).is_ok());
        assert!(store_image(&staging, &B64.encode(b"<html>"), None).is_err());
        assert!(store_image(&staging, "not base64 at all!", None).is_err());
        // A WAV file is RIFF too, but not WebP.
        assert!(store_image(&staging, &B64.encode(b"RIFF\0\0\0\0WAVEfmt "), None).is_err());
        let _ = fs::remove_dir_all(staging);
    }

    #[test]
    fn an_oversized_image_is_refused() {
        let staging = temp("big");
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.resize(MAX_IMAGE_BYTES as usize + 1, 0);
        let err = store_image(&staging, &B64.encode(&bytes), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("MiB"), "{err}");
        let _ = fs::remove_dir_all(staging);
    }

    #[test]
    fn a_picked_image_file_is_staged_and_other_files_are_referenced() {
        let (dir, staging) = (temp("ref"), temp("staging"));
        let file = dir.join("notes.md");
        fs::write(&file, "hello").unwrap();
        let a = reference(file.to_str().unwrap(), None, &staging).unwrap();
        assert_eq!((a.kind.as_str(), a.bytes), ("file", Some(5)));
        assert_eq!(a.name.as_deref(), Some("notes.md"));
        assert_eq!(
            reference(dir.to_str().unwrap(), None, &staging)
                .unwrap()
                .kind,
            "folder"
        );
        assert!(reference_line(&a, true).contains("5 B — read it with file_read"));
        assert!(!reference_line(&a, false).contains("file_read"));

        // An image named .txt is still an image; the bytes decide.
        fs::write(dir.join("shot.txt"), PNG).unwrap();
        let img = reference(dir.join("shot.txt").to_str().unwrap(), None, &staging).unwrap();
        assert_eq!(
            (img.kind.as_str(), img.name.as_deref()),
            ("image", Some("shot.txt"))
        );
        assert!(img.path.starts_with("attachments/"));
        assert_eq!(fs::read_dir(&staging).unwrap().count(), 1);
        // An image over the cap stays a file reference.
        let mut big = PNG.to_vec();
        big.resize(MAX_IMAGE_BYTES as usize + 1, 0);
        fs::write(dir.join("big.png"), big).unwrap();
        assert_eq!(
            reference(dir.join("big.png").to_str().unwrap(), None, &staging)
                .unwrap()
                .kind,
            "file"
        );
        let _ = fs::remove_dir_all(dir);
        let _ = fs::remove_dir_all(staging);
    }

    #[test]
    fn workspace_files_are_recorded_relative_to_the_workspace() {
        let (ws, staging) = (temp("ws"), temp("staging"));
        fs::create_dir_all(ws.join("notes")).unwrap();
        fs::write(ws.join("notes/a.md"), "a").unwrap();
        let picked =
            reference(ws.join("notes/a.md").to_str().unwrap(), Some(&ws), &staging).unwrap();
        assert_eq!(picked.path, "notes/a.md");
        let mentioned = reference("notes/a.md", Some(&ws), &staging).unwrap();
        assert_eq!(mentioned.path, "notes/a.md");
        assert!(grants_for(&[picked]).is_empty());
        assert!(reference("notes/a.md", None, &staging).is_err());
        let _ = fs::remove_dir_all(ws);
        let _ = fs::remove_dir_all(staging);
    }

    #[test]
    fn image_blocks_stay_inside_the_repo_attachments() {
        let repo = temp("escape");
        fs::write(repo.join("secret.png"), PNG).unwrap();
        let forged = Attachment {
            kind: "image".into(),
            path: "attachments/2026/../../secret.png".into(),
            media_type: Some("image/png".into()),
            bytes: None,
            name: None,
        };
        assert!(image_block(&repo, &forged).is_none());
        // The bytes decide the media type, not the descriptor.
        fs::create_dir_all(repo.join("attachments/2026")).unwrap();
        fs::write(
            repo.join("attachments/2026/x.png"),
            b"<svg onload=alert(1)>",
        )
        .unwrap();
        let lying = Attachment {
            path: "attachments/2026/x.png".into(),
            ..forged
        };
        assert!(image_block(&repo, &lying).is_none());
        let _ = fs::remove_dir_all(repo);
    }
}
