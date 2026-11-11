//! Reference libraries (architecture §4.27): a folder of the learner's
//! references for a topic — exam outlines and guides, question banks, guides
//! to writing questions, notes — imported once on a desktop and readable by
//! every agent on every device through `library_list`, `library_search` and
//! `library_read`.
//!
//! **Only text syncs.** Import copies the folder in (the source is never
//! touched), extracts each file's text into the data repo, and keeps the
//! copied originals device-local:
//!
//! - `<repo>/library/<slug>/library.json`: the index ([`Library`]): name,
//!   description, topic, and one entry per file with its kind, size, hash,
//!   page count and status. Paths are relative to the imported folder; no
//!   absolute path reaches the repo.
//! - `<repo>/library/<slug>/text/<relative path>.md`: the extracted text,
//!   capped at [`MAX_TEXT_BYTES`] per file (the index says when it was cut).
//!   PDF pages are tagged `[p.N]`; a page with no text layer says so.
//! - `<config>/library/<slug>/local.json`: device-local; the source folder
//!   and each file's size and mtime, so a refresh re-reads only what changed.
//! - `<config>/library/<slug>/originals/<relative path>`: device-local
//!   copies of the originals. Never synced.
//!
//! Import skips files over the size cap (default [`DEFAULT_MAX_FILE_MB`] MB),
//! build/cache/VCS folders and hidden entries, and file types that carry no
//! text (media, archives, binaries). Office formats are counted as
//! unsupported (no extractor). A scanned PDF with no text layer is indexed
//! as `no-text`: there is no OCR anywhere in the harness.
//!
//! Kinds are cheap heuristics over the path, the first heading and (for
//! question banks) the shape of the text: `question-guide`,
//! `question-bank`, `guide`, `notes`, `reference`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent::{done, refuse, Exec};
use crate::prompts::{render, Composer, TemplateValues};
use crate::sessions::{Attachment, Op};
use crate::workspace::sha256_hex;
use crate::{now_iso, Error, Harness};

/// Default per-file size cap at import, in MB (the learner may change it).
pub const DEFAULT_MAX_FILE_MB: u64 = 50;
/// Most extracted text kept per file in the repo; the rest is cut and the
/// index marks the file `truncated`.
pub const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024;
/// Most text one `library_read` returns.
pub const MAX_READ_BYTES: usize = 256 * 1024;
/// Most files one library indexes.
const MAX_FILES: usize = 20_000;
/// Deepest folder level walked.
const MAX_DEPTH: usize = 32;
/// Most pages read from one PDF.
const MAX_PDF_PAGES: usize = 3_000;
/// Hits one search returns, and at most this many per file.
const MAX_HITS: usize = 20;
const MAX_HITS_PER_FILE: usize = 3;
/// Search terms used from one query.
const MAX_TERMS: usize = 12;
/// `library_list` output cap.
const MAX_LIST_BYTES: usize = 64 * 1024;
/// Libraries named in a prompt block; the rest are counted.
const MAX_PROMPT_LIBRARIES: usize = 12;
/// Example paths kept per skip reason.
const MAX_SKIP_SAMPLES: usize = 5;

const DIR: &str = "library";
const INDEX: &str = "library.json";
const LOCAL: &str = "local.json";

/// Folders never walked: version control, dependencies, build output and
/// caches. Every hidden folder (`.name`) is skipped as well.
const IGNORED_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "__pycache__",
    "venv",
    "site-packages",
    "bower_components",
    "build",
    "dist",
    "_build",
    "CVS",
];

/// Extensions with no text to extract.
const NO_TEXT_EXT: &[&str] = &[
    // images
    "png",
    "jpg",
    "jpeg",
    "gif",
    "webp",
    "bmp",
    "tif",
    "tiff",
    "ico",
    "heic",
    "heif",
    "svg",
    "psd",
    "ai",
    "eps",
    "raw",
    "cr2",
    "nef", // video
    "mp4",
    "mkv",
    "mov",
    "avi",
    "webm",
    "m4v",
    "flv",
    "wmv",
    "mpg",
    "mpeg",
    "3gp", // audio
    "mp3",
    "wav",
    "flac",
    "ogg",
    "oga",
    "m4a",
    "aac",
    "opus",
    "wma",
    "aiff",
    "mid",
    "midi",
    // archives and disk images
    "zip",
    "tar",
    "gz",
    "tgz",
    "bz2",
    "xz",
    "7z",
    "rar",
    "zst",
    "lz",
    "lzma",
    "cab",
    "jar",
    "war",
    "apk",
    "ipa",
    "deb",
    "rpm",
    "dmg",
    "iso",
    "img",
    "vmdk",
    "qcow2",
    "ova", // binaries
    "exe",
    "dll",
    "so",
    "dylib",
    "o",
    "a",
    "lib",
    "bin",
    "class",
    "pyc",
    "pyo",
    "wasm",
    "msi",
    "dat", // fonts
    "ttf",
    "otf",
    "woff",
    "woff2",
    "eot", // data blobs and models
    "sqlite",
    "sqlite3",
    "db",
    "mdb",
    "pt",
    "pth",
    "onnx",
    "h5",
    "hdf5",
    "ckpt",
    "safetensors",
    "npy",
    "npz",
    "pkl",
    "pickle",
    "parquet",
    "feather",
    "arrow",
    "tfrecord",
];

/// Formats that hold text this module can't extract (no zip reader).
const UNSUPPORTED_EXT: &[&str] = &[
    "docx", "xlsx", "pptx", "odt", "ods", "odp", "epub", "doc", "xls", "ppt", "rtf", "pages",
    "key", "numbers",
];

/// Extensions read as plain text without sniffing.
const TEXT_EXT: &[&str] = &[
    "txt",
    "text",
    "rst",
    "adoc",
    "asciidoc",
    "org",
    "tex",
    "bib",
    "csv",
    "tsv",
    "json",
    "jsonl",
    "ndjson",
    "yaml",
    "yml",
    "toml",
    "xml",
    "ini",
    "cfg",
    "conf",
    "log",
    "srt",
    "vtt",
    "sub",
    "py",
    "js",
    "mjs",
    "cjs",
    "ts",
    "tsx",
    "jsx",
    "rs",
    "go",
    "java",
    "kt",
    "kts",
    "scala",
    "c",
    "h",
    "cc",
    "cpp",
    "hpp",
    "cxx",
    "cs",
    "rb",
    "php",
    "sh",
    "bash",
    "zsh",
    "fish",
    "ps1",
    "bat",
    "sql",
    "r",
    "m",
    "swift",
    "lua",
    "luau",
    "pl",
    "hs",
    "ml",
    "ex",
    "exs",
    "clj",
    "dart",
    "vue",
    "css",
    "scss",
    "less",
    "nix",
    "dockerfile",
    "makefile",
    "cmake",
    "gradle",
    "properties",
    "env",
];

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// What a file is for, decided by cheap heuristics at import.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// How to write questions or labs (an authoring or item-writing guide).
    QuestionGuide,
    /// Questions: past papers, question banks, quizzes, mock exams.
    QuestionBank,
    /// Exam outlines, syllabi, manuals, courseware, tutorials.
    Guide,
    /// The learner's or someone's notes, digests and summaries.
    Notes,
    /// Everything else.
    Reference,
}

impl Kind {
    pub const ALL: [Kind; 5] = [
        Kind::QuestionGuide,
        Kind::QuestionBank,
        Kind::Guide,
        Kind::Notes,
        Kind::Reference,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::QuestionGuide => "question-guide",
            Kind::QuestionBank => "question-bank",
            Kind::Guide => "guide",
            Kind::Notes => "notes",
            Kind::Reference => "reference",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.as_str() == s.trim())
    }
}

/// How far a file's text could be extracted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Ok,
    /// Text kept up to [`MAX_TEXT_BYTES`].
    Truncated,
    /// No text: a scanned PDF (no OCR) or an empty document.
    NoText,
    /// The extractor refused the file (malformed, encrypted, too many pages).
    Failed,
}

/// One indexed file of a library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryFile {
    /// Relative to the imported folder, `/`-separated.
    pub path: String,
    pub kind: Kind,
    /// pdf | markdown | html | notebook | text
    pub format: String,
    /// The first heading or line, else the file name.
    pub title: String,
    /// Size of the original.
    pub bytes: u64,
    /// sha256 of the original.
    pub sha256: String,
    pub status: Status,
    /// Size of the extracted text in the repo (0 when there is none).
    #[serde(default)]
    pub text_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<u32>,
    /// PDF pages with no text layer.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub scanned_pages: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

fn default_max_file_bytes() -> u64 {
    DEFAULT_MAX_FILE_MB * 1024 * 1024
}

/// `<repo>/library/<slug>/library.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Library {
    /// The folder name under `<repo>/library/` (the folder wins on load).
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub topic: String,
    #[serde(default)]
    pub created: String,
    #[serde(default)]
    pub updated: String,
    #[serde(default = "default_max_file_bytes")]
    pub max_file_bytes: u64,
    #[serde(default)]
    pub files: Vec<LibraryFile>,
    /// Skip reason → count, from the last import.
    #[serde(default)]
    pub skipped: BTreeMap<String, u64>,
    /// Skip reason → a few relative paths, so the learner sees what was left out.
    #[serde(default)]
    pub skipped_samples: BTreeMap<String, Vec<String>>,
}

/// `<config>/library/<slug>/local.json`: this device's link to the source.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Local {
    source: PathBuf,
    #[serde(default)]
    files: BTreeMap<String, Stamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Stamp {
    bytes: u64,
    /// Nanoseconds since the epoch.
    mtime: u64,
}

/// One library for the UI.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibrarySummary {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub topic: String,
    pub updated: String,
    pub files: usize,
    /// Kind → file count (kinds with no files are left out).
    pub kinds: BTreeMap<String, usize>,
    pub no_text: usize,
    pub failed: usize,
    pub truncated: usize,
    pub text_bytes: u64,
    pub skipped: BTreeMap<String, u64>,
    pub skipped_samples: BTreeMap<String, Vec<String>>,
    pub max_file_bytes: u64,
    /// The imported folder, when this device imported it (device-local).
    pub source: Option<String>,
    /// Whether this device holds the copied originals (and can refresh).
    pub originals: bool,
}

/// What an import or refresh did.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub library: LibrarySummary,
    pub added: usize,
    pub changed: usize,
    pub unchanged: usize,
    pub removed: usize,
}

/// Streamed while an import runs: `scan`, then `extract` once per file,
/// then `done`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportProgress {
    pub slug: String,
    pub stage: &'static str,
    pub done: usize,
    pub total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// The learner's choices at import. Unset fields keep what an existing
/// library has (or the defaults for a new one).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOptions {
    pub name: Option<String>,
    pub description: Option<String>,
    pub topic: Option<String>,
    pub max_file_mb: Option<u64>,
}

// ---------------------------------------------------------------------------
// Paths and storage
// ---------------------------------------------------------------------------

fn library_dir(repo: &Path, slug: &str) -> PathBuf {
    repo.join(DIR).join(slug)
}

fn index_path(repo: &Path, slug: &str) -> PathBuf {
    library_dir(repo, slug).join(INDEX)
}

fn text_path(repo: &Path, slug: &str, rel: &str) -> PathBuf {
    library_dir(repo, slug)
        .join("text")
        .join(format!("{rel}.md"))
}

fn local_dir(config: &Path, slug: &str) -> PathBuf {
    config.join(DIR).join(slug)
}

fn original_path(config: &Path, slug: &str, rel: &str) -> PathBuf {
    local_dir(config, slug).join("originals").join(rel)
}

/// A slug is lowercase ASCII letters, digits and dashes: safe as a folder
/// name on every platform and in every sync target.
fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 64
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !slug.starts_with('-')
}

fn check_slug(slug: &str) -> Result<(), Error> {
    if valid_slug(slug) {
        Ok(())
    } else {
        Err(Error::Invalid(format!("`{slug}` is not a library id")))
    }
}

pub(crate) fn slugify(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.len() >= 48 {
            break;
        }
    }
    let out = out.trim_matches('-').to_owned();
    if out.is_empty() {
        "library".to_owned()
    } else {
        out
    }
}

/// A relative path that stays inside its root: no absolute path, no `..`.
fn safe_rel(rel: &str) -> bool {
    !rel.is_empty()
        && !rel.starts_with('/')
        && !rel.contains('\\')
        && !rel.contains(':')
        && rel
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// One library's index; `None` when there is none.
pub(crate) fn load(repo: &Path, slug: &str) -> Result<Option<Library>, Error> {
    if !valid_slug(slug) {
        return Ok(None);
    }
    match fs::read(index_path(repo, slug)) {
        Ok(bytes) => {
            let mut lib: Library = serde_json::from_slice(&bytes)?;
            lib.slug = slug.to_owned();
            Ok(Some(lib))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Every readable library, by slug. A malformed index is skipped with a
/// warning: one bad file must not hide the others.
pub(crate) fn load_all(repo: &Path) -> Vec<Library> {
    let Ok(entries) = fs::read_dir(repo.join(DIR)) else {
        return Vec::new();
    };
    let mut slugs: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|s| valid_slug(s))
        .collect();
    slugs.sort();
    slugs
        .into_iter()
        .filter_map(|slug| match load(repo, &slug) {
            Ok(lib) => lib,
            Err(e) => {
                tracing::warn!(library = %slug, error = %e, "unreadable library index");
                None
            }
        })
        .collect()
}

fn save(repo: &Path, lib: &Library) -> Result<(), Error> {
    write_atomic(
        &index_path(repo, &lib.slug),
        &serde_json::to_vec_pretty(lib)?,
    )
}

fn load_local(config: &Path, slug: &str) -> Option<Local> {
    let bytes = fs::read(local_dir(config, slug).join(LOCAL)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn save_local(config: &Path, slug: &str, local: &Local) -> Result<(), Error> {
    write_atomic(
        &local_dir(config, slug).join(LOCAL),
        &serde_json::to_vec_pretty(local)?,
    )
}

/// Remove a file, then each parent folder it leaves empty, up to `stop`.
fn remove_and_prune(path: &Path, stop: &Path) {
    if fs::remove_file(path).is_err() {
        return;
    }
    let mut dir = path.parent();
    while let Some(d) = dir {
        if d == stop || !d.starts_with(stop) || fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}

fn summary(lib: &Library, config: &Path) -> LibrarySummary {
    let mut kinds = BTreeMap::new();
    for f in &lib.files {
        *kinds.entry(f.kind.as_str().to_owned()).or_insert(0) += 1;
    }
    let count = |s: Status| lib.files.iter().filter(|f| f.status == s).count();
    let local = load_local(config, &lib.slug);
    LibrarySummary {
        slug: lib.slug.clone(),
        name: lib.name.clone(),
        description: lib.description.clone(),
        topic: lib.topic.clone(),
        updated: lib.updated.clone(),
        files: lib.files.len(),
        kinds,
        no_text: count(Status::NoText),
        failed: count(Status::Failed),
        truncated: count(Status::Truncated),
        text_bytes: lib.files.iter().map(|f| f.text_bytes).sum(),
        skipped: lib.skipped.clone(),
        skipped_samples: lib.skipped_samples.clone(),
        max_file_bytes: lib.max_file_bytes,
        source: local
            .as_ref()
            .map(|l| l.source.to_string_lossy().into_owned()),
        originals: local.is_some() && local_dir(config, &lib.slug).join("originals").is_dir(),
    }
}

/// One import or removal per library at a time, keyed by the library's
/// folder in its repo (two harnesses, or tests, may share a slug).
static BUSY: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

struct Busy(PathBuf);

impl Busy {
    fn claim(repo: &Path, slug: &str) -> Result<Busy, Error> {
        let key = library_dir(repo, slug);
        let mut busy = crate::lock(&BUSY);
        let set = busy.get_or_insert_with(HashSet::new);
        if !set.insert(key.clone()) {
            return Err(Error::Invalid(format!(
                "the library `{slug}` is already being imported; wait for it to finish"
            )));
        }
        Ok(Busy(key))
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        if let Some(set) = crate::lock(&BUSY).as_mut() {
            set.remove(&self.0);
        }
    }
}

// ---------------------------------------------------------------------------
// Scanning the source folder
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Pdf,
    Markdown,
    Html,
    Notebook,
    Text,
}

impl Format {
    fn as_str(self) -> &'static str {
        match self {
            Format::Pdf => "pdf",
            Format::Markdown => "markdown",
            Format::Html => "html",
            Format::Notebook => "notebook",
            Format::Text => "text",
        }
    }
}

/// How a file name reads: a known format, a skip reason, or `None` when
/// only its bytes can tell (sniffed as text or binary).
fn format_of(name: &str) -> Result<Option<Format>, &'static str> {
    let lower = name.to_ascii_lowercase();
    let ext = match lower.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => ext,
        _ => return Ok(None),
    };
    Ok(Some(match ext {
        "pdf" => Format::Pdf,
        "md" | "markdown" | "mdx" => Format::Markdown,
        "html" | "htm" | "xhtml" => Format::Html,
        "ipynb" => Format::Notebook,
        e if TEXT_EXT.contains(&e) => Format::Text,
        e if NO_TEXT_EXT.contains(&e) => return Err("no-text-type"),
        e if UNSUPPORTED_EXT.contains(&e) => return Err("unsupported"),
        _ => return Ok(None),
    }))
}

/// Text when the head holds no NUL and is UTF-8 (a character cut at the end
/// of the head is fine).
fn looks_text(head: &[u8]) -> bool {
    if head.contains(&0) {
        return false;
    }
    match std::str::from_utf8(head) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none(),
    }
}

#[derive(Debug)]
struct Candidate {
    rel: String,
    abs: PathBuf,
    stamp: Stamp,
    format: Format,
}

#[derive(Debug, Default)]
struct Scan {
    files: Vec<Candidate>,
    skipped: BTreeMap<String, u64>,
    samples: BTreeMap<String, Vec<String>>,
}

impl Scan {
    fn skip(&mut self, reason: &str, rel: String) {
        *self.skipped.entry(reason.to_owned()).or_insert(0) += 1;
        let samples = self.samples.entry(reason.to_owned()).or_default();
        if samples.len() < MAX_SKIP_SAMPLES {
            samples.push(rel);
        }
    }
}

fn rel_of(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn mtime_ns(meta: &fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as u64)
}

/// Walk `source` (symlinks are never followed) and sort what's found into
/// files to import and skips by reason.
fn scan(source: &Path, max_file_bytes: u64) -> Scan {
    let mut out = Scan::default();
    let mut stack = vec![(source.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            out.skip("unreadable", rel_of(source, &dir));
            continue;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            let rel = rel_of(source, &path);
            let Ok(kind) = entry.file_type() else {
                out.skip("unreadable", rel);
                continue;
            };
            if kind.is_symlink() {
                out.skip("symlink", rel);
                continue;
            }
            if kind.is_dir() {
                if name.starts_with('.') || IGNORED_DIRS.contains(&name.as_str()) {
                    out.skip("ignored-folder", format!("{rel}/"));
                } else if depth + 1 >= MAX_DEPTH {
                    out.skip("too-deep", format!("{rel}/"));
                } else {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            if name.starts_with('.') || name.starts_with("~$") {
                out.skip("hidden", rel);
                continue;
            }
            let format = match format_of(&name) {
                Ok(f) => f,
                Err(reason) => {
                    out.skip(reason, rel);
                    continue;
                }
            };
            let Ok(meta) = entry.metadata() else {
                out.skip("unreadable", rel);
                continue;
            };
            if meta.len() == 0 {
                out.skip("empty", rel);
                continue;
            }
            if meta.len() > max_file_bytes {
                out.skip("too-large", rel);
                continue;
            }
            let format = match format {
                Some(f) => f,
                None => {
                    let mut head = [0u8; 8192];
                    let n = fs::File::open(&path)
                        .and_then(|mut f| f.read(&mut head))
                        .unwrap_or(0);
                    if n == 0 {
                        out.skip("unreadable", rel);
                        continue;
                    }
                    if !looks_text(&head[..n]) {
                        out.skip("no-text-type", rel);
                        continue;
                    }
                    Format::Text
                }
            };
            if out.files.len() >= MAX_FILES {
                out.skip("over-file-limit", rel);
                continue;
            }
            out.files.push(Candidate {
                rel,
                abs: path,
                stamp: Stamp {
                    bytes: meta.len(),
                    mtime: mtime_ns(&meta),
                },
                format,
            });
        }
    }
    out.files.sort_by(|a, b| a.rel.cmp(&b.rel));
    out
}

// ---------------------------------------------------------------------------
// Extraction and classification
// ---------------------------------------------------------------------------

struct Extracted {
    text: Option<String>,
    title: String,
    pages: Option<u32>,
    scanned_pages: u32,
    status: Status,
    note: Option<String>,
}

fn lossy_text(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    String::from_utf8_lossy(bytes).replace("\r\n", "\n")
}

/// A notebook's cells as Markdown: text cells as they are, code cells
/// fenced. Not a notebook after all: the raw text.
fn notebook_text(raw: &str) -> String {
    let Ok(nb) = serde_json::from_str::<Value>(raw) else {
        return raw.to_owned();
    };
    let lang = nb
        .pointer("/metadata/kernelspec/language")
        .and_then(Value::as_str)
        .unwrap_or("");
    let source = |cell: &Value| match cell.get("source") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(lines)) => lines.iter().filter_map(Value::as_str).collect(),
        _ => String::new(),
    };
    let mut out = String::new();
    for cell in nb
        .get("cells")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let body = source(cell);
        if body.trim().is_empty() {
            continue;
        }
        if cell.get("cell_type").and_then(Value::as_str) == Some("code") {
            out.push_str(&format!("```{lang}\n{}\n```\n\n", body.trim_end()));
        } else {
            out.push_str(body.trim_end());
            out.push_str("\n\n");
        }
    }
    out
}

/// The first Markdown heading (or `Title:` line) in the first lines, else
/// the first short non-empty line, else the file name.
fn title_of(text: &str, rel: &str) -> String {
    let lines = || {
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .take(40)
    };
    let strip_tag = |l: &str| -> String {
        match page_tag(l) {
            Some(_) => l.split_once(']').map_or(l, |(_, r)| r).trim().to_owned(),
            None => l.to_owned(),
        }
    };
    let heading = lines().find_map(|l| {
        l.strip_prefix("# ")
            .or_else(|| l.strip_prefix("Title:"))
            .map(|t| t.trim().to_owned())
    });
    let title = heading
        .or_else(|| {
            lines()
                .map(strip_tag)
                .find(|l| !l.is_empty() && l.chars().count() <= 160)
        })
        .unwrap_or_else(|| rel.rsplit('/').next().unwrap_or(rel).to_owned());
    clip_chars(&title, 120)
}

fn clip_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

/// Lowercase words of a path or title: runs of letters and digits.
fn tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Whether the text looks like a question bank: many option lines with a
/// capital letter (`A.`, `(B)`, `C)`, `D、`) and answer lines that start
/// with an answer marker (`Answer: B`, `答案：D`), in its first 400 KiB.
fn looks_like_questions(text: &str) -> bool {
    let mut end = text.len().min(400 * 1024);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let head = &text[..end];
    let mut options = 0;
    let mut answers = 0;
    for line in head.lines() {
        let l = line.trim_start();
        let l = page_tag(l).map_or(l, |_| l.split_once(']').map_or(l, |(_, r)| r.trim_start()));
        let opt = l
            .strip_prefix('(')
            .or_else(|| l.strip_prefix('（'))
            .unwrap_or(l);
        let mut chars = opt.chars();
        if let (Some(letter), Some(sep)) = (chars.next(), chars.next()) {
            if ('A'..='H').contains(&letter)
                && matches!(sep, '.' | ')' | '）' | '．' | '、' | ':' | '：')
            {
                options += 1;
            }
        }
        let lower = l.to_lowercase();
        let marker = [
            "answer:",
            "answer：",
            "answers:",
            "correct answer",
            "ans:",
            "ans.",
            "答案",
            "正确答案",
            "参考答案",
        ];
        if marker.iter().any(|m| lower.starts_with(m)) {
            answers += 1;
        }
    }
    options >= 12 && answers >= 4 && answers * 8 >= options
}

/// Sort a file into a [`Kind`] from its path, title and text. Order
/// matters: a question-writing guide names questions, a bank's folder
/// outranks its file name, and "exam guide" is a guide, not an exam.
fn classify(rel: &str, title: &str, text: &str) -> Kind {
    let lower_rel = rel.to_lowercase();
    let file = lower_rel
        .rsplit('/')
        .next()
        .unwrap_or(&lower_rel)
        .to_owned();
    let name_and_title = format!("{file} {}", title.to_lowercase());
    let path_tokens = tokens(&lower_rel);
    let file_tokens = tokens(&name_and_title);
    let has = |toks: &[String], words: &[&str]| toks.iter().any(|t| words.contains(&t.as_str()));
    let phrase = |s: &str, words: &[&str]| words.iter().any(|w| s.contains(w));

    // 1. How to write questions.
    let writing = has(&file_tokens, &["authoring"])
        || phrase(
            &name_and_title,
            &[
                "item writing",
                "item-writing",
                "question writing",
                "question-writing",
                "writing questions",
                "writing-questions",
                "write questions",
                "writing good questions",
                "question design",
                "question-design",
                "how to write",
                "命题",
                "出题",
            ],
        );
    if writing {
        return Kind::QuestionGuide;
    }

    // 2. Question banks named as such (the file or a folder).
    let bank_words = [
        "bank",
        "banks",
        "qbank",
        "quiz",
        "quizzes",
        "mock",
        "mocks",
        "questions",
        "pastpaper",
        "pastpapers",
        "dumps",
        "flashcards",
        "exercises",
        "worksheet",
        "worksheets",
    ];
    let numbered_q = path_tokens.iter().any(|t| {
        t.len() > 1 && t.ends_with('q') && t[..t.len() - 1].bytes().all(|b| b.is_ascii_digit())
    });
    if has(&path_tokens, &bank_words[..])
        || numbered_q
        || phrase(
            &lower_rel,
            &[
                "past-paper",
                "past paper",
                "past_paper",
                "practice-test",
                "practice test",
                "practice-exam",
                "practice exam",
                "sample-questions",
                "sample questions",
                "题库",
                "真题",
                "试题",
                "习题",
                "考题",
            ],
        )
    {
        return Kind::QuestionBank;
    }

    // A digest or summary folder holds someone's notes, whatever they cover.
    let folders = &path_tokens[..path_tokens.len().saturating_sub(tokens(&file).len())];
    if has(folders, &["digest", "digests", "summaries", "cheatsheets"]) {
        return Kind::Notes;
    }

    // 3. Guides, by the file's own name or title.
    let guide_words = [
        "guide",
        "guides",
        "guidebook",
        "outline",
        "outlines",
        "syllabus",
        "syllabi",
        "curriculum",
        "manual",
        "handbook",
        "courseware",
        "tutorial",
        "tutorials",
        "overview",
        "introduction",
        "intro",
        "primer",
        "blueprint",
        "objectives",
        "quickstart",
        "readme",
        "lab",
        "labs",
        "course",
        "chapter",
        "lecture",
        "lesson",
        "textbook",
        "大纲",
        "指南",
        "手册",
        "教程",
    ];
    if has(&file_tokens, &guide_words[..])
        || phrase(
            &name_and_title,
            &[
                "getting started",
                "getting-started",
                "quick start",
                "quick-start",
                "大纲",
                "指南",
            ],
        )
    {
        return Kind::Guide;
    }

    // 4. A question bank by the shape of its text.
    if looks_like_questions(text) {
        return Kind::QuestionBank;
    }

    // 5. Notes, by folder or name.
    if has(
        &path_tokens,
        &[
            "notes",
            "note",
            "digest",
            "digests",
            "summary",
            "summaries",
            "cheatsheet",
            "cheatsheets",
            "flashcard",
            "journal",
            "笔记",
        ],
    ) || phrase(&lower_rel, &["cheat-sheet", "cheat sheet", "笔记"])
    {
        return Kind::Notes;
    }

    // 6. An exam or test paper by its file name.
    if has(
        &file_tokens,
        &[
            "exam", "exams", "test", "tests", "paper", "papers", "prelim", "final", "finals",
            "midterm",
        ],
    ) {
        return Kind::QuestionBank;
    }
    Kind::Reference
}

/// Cut text at the last line break before [`MAX_TEXT_BYTES`], with a note.
fn cap_text(mut text: String) -> (String, bool) {
    if text.len() <= MAX_TEXT_BYTES {
        return (text, false);
    }
    let mut cut = MAX_TEXT_BYTES - 128;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let cut = text[..cut].rfind('\n').map_or(cut, |i| i + 1);
    text.truncate(cut);
    text.push_str("\n[truncated: the rest of this file passed the 2 MiB text cap]\n");
    (text, true)
}

fn extract(format: Format, bytes: &[u8], rel: &str, max_file_bytes: u64) -> Extracted {
    let plain = |text: String| {
        let title = title_of(&text, rel);
        Extracted {
            status: if text.trim().is_empty() {
                Status::NoText
            } else {
                Status::Ok
            },
            note: text
                .trim()
                .is_empty()
                .then(|| "no text in this file".to_owned()),
            text: (!text.trim().is_empty()).then_some(text),
            title,
            pages: None,
            scanned_pages: 0,
        }
    };
    match format {
        Format::Markdown | Format::Text => plain(lossy_text(bytes)),
        Format::Html => plain(crate::web::html_to_text(&lossy_text(bytes))),
        Format::Notebook => plain(notebook_text(&lossy_text(bytes))),
        Format::Pdf => {
            let cap = (max_file_bytes as usize).max(bytes.len());
            match crate::pdf::extract_lenient(bytes, cap, MAX_PDF_PAGES) {
                Err(e) => Extracted {
                    text: None,
                    title: title_of("", rel),
                    pages: None,
                    scanned_pages: 0,
                    status: Status::Failed,
                    note: Some(e),
                },
                Ok(pages) => {
                    let scanned = pages.iter().filter(|p| p.scanned).count() as u32;
                    let count = pages.len() as u32;
                    if scanned == count {
                        return Extracted {
                            text: None,
                            title: title_of("", rel),
                            pages: Some(count),
                            scanned_pages: scanned,
                            status: Status::NoText,
                            note: Some(
                                "a scan: no page has a text layer, and there is no OCR".into(),
                            ),
                        };
                    }
                    let mut text = String::new();
                    for p in &pages {
                        if p.scanned {
                            text.push_str(&format!(
                                "[p.{}] (no text layer on this page: a scan)\n",
                                p.number
                            ));
                        } else {
                            text.push_str(&format!("[p.{}] {}\n", p.number, p.text.trim_end()));
                        }
                    }
                    let title = pages
                        .iter()
                        .find(|p| !p.scanned)
                        .map_or_else(|| title_of("", rel), |p| title_of(&p.text, rel));
                    Extracted {
                        text: Some(text),
                        title,
                        pages: Some(count),
                        scanned_pages: scanned,
                        status: Status::Ok,
                        note: (scanned > 0)
                            .then(|| format!("{scanned} of {count} pages are scans with no text")),
                    }
                }
            }
        }
    }
}

/// Extract one file into the repo and describe it for the index.
fn import_file(
    repo: &Path,
    slug: &str,
    c: &Candidate,
    bytes: &[u8],
    sha256: String,
    max_file_bytes: u64,
) -> Result<LibraryFile, Error> {
    let ex = extract(c.format, bytes, &c.rel, max_file_bytes);
    let target = text_path(repo, slug, &c.rel);
    let mut status = ex.status;
    let mut note = ex.note;
    let mut text_bytes = 0;
    let classify_text = ex.text.as_deref().unwrap_or("");
    let kind = classify(&c.rel, &ex.title, classify_text);
    match ex.text {
        Some(text) => {
            let (text, cut) = cap_text(text);
            if cut {
                status = Status::Truncated;
                note = Some(match note {
                    Some(n) => format!("{n}; text cut at the 2 MiB cap"),
                    None => "text cut at the 2 MiB cap".into(),
                });
            }
            if let Some(dir) = target.parent() {
                fs::create_dir_all(dir)?;
            }
            fs::write(&target, text.as_bytes())?;
            text_bytes = text.len() as u64;
        }
        None => remove_and_prune(&target, &library_dir(repo, slug)),
    }
    Ok(LibraryFile {
        path: c.rel.clone(),
        kind,
        format: c.format.as_str().to_owned(),
        title: ex.title,
        bytes: c.stamp.bytes,
        sha256,
        status,
        text_bytes,
        pages: ex.pages,
        scanned_pages: ex.scanned_pages,
        note,
    })
}

/// What [`import_into`] may change on the library record.
#[derive(Debug, Default)]
struct Meta {
    name: Option<String>,
    description: Option<String>,
    topic: Option<String>,
    max_file_bytes: Option<u64>,
}

/// Import `source` into the library `slug` (new or existing): incremental,
/// so an unchanged file (same size and mtime as this device last saw, or
/// the same hash as the index holds) is neither copied nor extracted again.
fn import_into(
    repo: &Path,
    config: &Path,
    slug: &str,
    source: &Path,
    meta: Meta,
    progress: &mut dyn FnMut(&ImportProgress),
) -> Result<ImportReport, Error> {
    check_slug(slug)?;
    let _busy = Busy::claim(repo, slug)?;
    let now = now_iso();
    let mut lib = load(repo, slug)?.unwrap_or_else(|| Library {
        slug: slug.to_owned(),
        name: slug.to_owned(),
        description: String::new(),
        topic: String::new(),
        created: now.clone(),
        updated: now.clone(),
        max_file_bytes: default_max_file_bytes(),
        files: Vec::new(),
        skipped: BTreeMap::new(),
        skipped_samples: BTreeMap::new(),
    });
    if let Some(name) = meta.name.filter(|n| !n.trim().is_empty()) {
        lib.name = clip_chars(name.trim(), 120);
    }
    if let Some(d) = meta.description {
        lib.description = clip_chars(d.trim(), 2000);
    }
    if let Some(t) = meta.topic {
        lib.topic = clip_chars(t.trim(), 120);
    }
    if let Some(max) = meta.max_file_bytes.filter(|m| *m > 0) {
        lib.max_file_bytes = max;
    }
    let old_local = load_local(config, slug).filter(|l| l.source == source);

    let tell = |progress: &mut dyn FnMut(&ImportProgress), stage, done, total, path| {
        progress(&ImportProgress {
            slug: slug.to_owned(),
            stage,
            done,
            total,
            path,
        });
    };
    tell(progress, "scan", 0, 0, None);
    let mut found = scan(source, lib.max_file_bytes);
    let total = found.files.len();

    let old: HashMap<String, LibraryFile> =
        lib.files.drain(..).map(|f| (f.path.clone(), f)).collect();
    let has_text = |f: &LibraryFile| match f.status {
        Status::Ok | Status::Truncated => text_path(repo, slug, &f.path).is_file(),
        Status::NoText | Status::Failed => true,
    };
    let mut files = Vec::with_capacity(total);
    let mut stamps = BTreeMap::new();
    let (mut added, mut changed, mut unchanged) = (0, 0, 0);
    let candidates = std::mem::take(&mut found.files);
    for (i, c) in candidates.iter().enumerate() {
        tell(progress, "extract", i, total, Some(c.rel.clone()));
        let prev = old.get(&c.rel);
        let original = original_path(config, slug, &c.rel);
        let same_stamp = old_local
            .as_ref()
            .and_then(|l| l.files.get(&c.rel))
            .is_some_and(|s| *s == c.stamp);
        if let Some(p) = prev {
            if same_stamp && has_text(p) && original.is_file() {
                files.push(p.clone());
                stamps.insert(c.rel.clone(), c.stamp);
                unchanged += 1;
                continue;
            }
        }
        let bytes = match fs::read(&c.abs) {
            Ok(b) => b,
            Err(_) => {
                found.skip("unreadable", c.rel.clone());
                continue;
            }
        };
        let hash = sha256_hex(&bytes);
        // Copy in: the original is kept on this device only.
        if let Some(dir) = original.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&original, &bytes)?;
        match prev.filter(|p| p.sha256 == hash && has_text(p)) {
            Some(p) => {
                let mut f = p.clone();
                f.bytes = c.stamp.bytes;
                files.push(f);
                unchanged += 1;
            }
            None => {
                let f = import_file(repo, slug, c, &bytes, hash, lib.max_file_bytes)?;
                if prev.is_some() {
                    changed += 1;
                } else {
                    added += 1;
                }
                files.push(f);
            }
        }
        stamps.insert(c.rel.clone(), c.stamp);
    }

    // Files gone from the folder (or now skipped) leave the library.
    let kept: HashSet<&str> = files.iter().map(|f| f.path.as_str()).collect();
    let mut removed = 0;
    let lib_root = library_dir(repo, slug);
    let local_root = local_dir(config, slug);
    for rel in old.keys().filter(|r| !kept.contains(r.as_str())) {
        if safe_rel(rel) {
            remove_and_prune(&text_path(repo, slug, rel), &lib_root);
            remove_and_prune(&original_path(config, slug, rel), &local_root);
        }
        removed += 1;
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));
    lib.files = files;
    lib.skipped = found.skipped;
    lib.skipped_samples = found.samples;
    lib.updated = now_iso();
    save(repo, &lib)?;
    save_local(
        config,
        slug,
        &Local {
            source: source.to_owned(),
            files: stamps,
        },
    )?;
    tell(progress, "done", total, total, None);
    Ok(ImportReport {
        library: summary(&lib, config),
        added,
        changed,
        unchanged,
        removed,
    })
}

/// The library a folder imports into: the one this device already imported
/// from it; else the name's slug when it is free, or when it holds a
/// library imported on another device whose files mostly match this folder
/// (the same folder on a second computer); else the slug with a number.
fn choose_slug(repo: &Path, config: &Path, source: &Path, name: &str) -> String {
    if let Ok(entries) = fs::read_dir(config.join(DIR)) {
        let mut slugs: Vec<String> = entries
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|s| valid_slug(s))
            .collect();
        slugs.sort();
        for slug in slugs {
            if load_local(config, &slug).is_some_and(|l| l.source == source)
                && index_path(repo, &slug).is_file()
            {
                return slug;
            }
        }
    }
    let base = slugify(name);
    let mut found: Option<HashSet<String>> = None;
    for n in 1.. {
        let slug = if n == 1 {
            base.clone()
        } else {
            format!("{base}-{n}")
        };
        let in_repo = load(repo, &slug).ok().flatten();
        let local = load_local(config, &slug);
        match (in_repo, local) {
            (None, _) => return slug,
            (Some(lib), None) => {
                let rels = found.get_or_insert_with(|| {
                    scan(source, lib.max_file_bytes)
                        .files
                        .into_iter()
                        .map(|c| c.rel)
                        .collect()
                });
                let shared = lib.files.iter().filter(|f| rels.contains(&f.path)).count();
                if !lib.files.is_empty() && shared * 2 >= lib.files.len() {
                    return slug;
                }
            }
            (Some(_), Some(_)) => {}
        }
    }
    unreachable!("an unused slug always exists")
}

// ---------------------------------------------------------------------------
// Agent tools: list, search, read
// ---------------------------------------------------------------------------

fn size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{bytes} B")
    }
}

fn kind_counts(lib: &Library) -> String {
    Kind::ALL
        .into_iter()
        .filter_map(|k| {
            let n = lib.files.iter().filter(|f| f.kind == k).count();
            (n > 0).then(|| format!("{n} {}", k.as_str()))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn kind_arg(kind: Option<&str>) -> Result<Option<Kind>, String> {
    match kind.map(str::trim).filter(|k| !k.is_empty()) {
        None => Ok(None),
        Some(k) => Kind::parse(k).map(Some).ok_or_else(|| {
            format!(
                "unknown kind `{k}`; use one of {}",
                Kind::ALL.map(Kind::as_str).join(", ")
            )
        }),
    }
}

/// The libraries `library` names (all of them when `None`).
fn scope(repo: &Path, library: Option<&str>) -> Result<Vec<Library>, String> {
    let all = load_all(repo);
    if all.is_empty() {
        return Err("there are no reference libraries yet; the learner adds one from the + button on Home or in Settings → Libraries".into());
    }
    match library.map(str::trim).filter(|l| !l.is_empty()) {
        None => Ok(all),
        Some(slug) => {
            let slugs: Vec<String> = all.iter().map(|l| l.slug.clone()).collect();
            let one: Vec<Library> = all.into_iter().filter(|l| l.slug == slug).collect();
            if one.is_empty() {
                Err(format!(
                    "no library `{slug}`; the libraries are: {}",
                    slugs.join(", ")
                ))
            } else {
                Ok(one)
            }
        }
    }
}

fn file_line(lib: &Library, f: &LibraryFile) -> String {
    let mut parts = Vec::new();
    if let Some(p) = f.pages {
        parts.push(format!("{p} pages"));
    }
    parts.push(if f.text_bytes > 0 {
        format!("{} of text", size(f.text_bytes))
    } else {
        size(f.bytes)
    });
    match f.status {
        Status::Ok if f.scanned_pages > 0 => {
            parts.push(format!("{} scanned pages", f.scanned_pages))
        }
        Status::Ok => {}
        Status::Truncated => parts.push("text cut at 2 MiB".into()),
        Status::NoText => parts.push("no text (scan, no OCR)".into()),
        Status::Failed => parts.push("couldn't be read".into()),
    }
    format!(
        "- `{}/{}` — {} · {}\n",
        lib.slug,
        f.path,
        f.title,
        parts.join(" · ")
    )
}

/// `library_list { library?, kind? }`.
pub(crate) fn list(
    repo: &Path,
    library: Option<&str>,
    kind: Option<&str>,
) -> Result<String, String> {
    let kind = kind_arg(kind)?;
    let libs = scope(repo, library)?;
    let mut out = String::new();
    let mut cut = false;
    for lib in &libs {
        out.push_str(&format!("## {} (`{}`)\n", lib.name, lib.slug));
        if !lib.topic.is_empty() {
            out.push_str(&format!("Topic: {}\n", lib.topic));
        }
        if !lib.description.is_empty() {
            out.push_str(&lib.description);
            out.push('\n');
        }
        let no_text = lib
            .files
            .iter()
            .filter(|f| f.status == Status::NoText)
            .count();
        out.push_str(&format!("{} files: {}", lib.files.len(), kind_counts(lib)));
        if no_text > 0 {
            out.push_str(&format!("; {no_text} without text"));
        }
        out.push_str(".\n");
        for k in Kind::ALL
            .into_iter()
            .filter(|k| kind.is_none_or(|want| want == *k))
        {
            let mut files = lib.files.iter().filter(|f| f.kind == k).peekable();
            if files.peek().is_none() {
                continue;
            }
            out.push_str(&format!("### {}\n", k.as_str()));
            for f in files {
                let line = file_line(lib, f);
                if out.len() + line.len() > MAX_LIST_BYTES {
                    cut = true;
                    break;
                }
                out.push_str(&line);
            }
            if cut {
                break;
            }
        }
        if cut {
            break;
        }
        out.push('\n');
    }
    if cut {
        out.push_str("\n[list cut at 64 KiB: pass `library` and `kind` to see the rest]\n");
    }
    out.push_str("Paths are `<library>/<file>`: pass them to library_read; search inside with library_search.\n");
    Ok(out)
}

/// `[p.12] …` → 12.
fn page_tag(line: &str) -> Option<u32> {
    let rest = line.strip_prefix("[p.")?;
    let end = rest.find(']')?;
    rest[..end].parse().ok()
}

fn search_terms(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for raw in query.split_whitespace() {
        let t = raw
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase();
        let long_enough = t.chars().count() >= 2 || !t.is_ascii();
        if !t.is_empty() && long_enough && !terms.contains(&t) {
            terms.push(t);
        }
        if terms.len() == MAX_TERMS {
            break;
        }
    }
    terms
}

/// A window of the line around the first matched term.
fn snippet(line: &str, lower: &str, term: &str) -> String {
    const BEFORE: usize = 60;
    const AFTER: usize = 160;
    let line = line.trim();
    let chars: Vec<char> = line.chars().collect();
    let at = lower
        .trim()
        .find(term)
        .map_or(0, |b| lower.trim()[..b].chars().count())
        .min(chars.len().saturating_sub(1));
    let start = at.saturating_sub(BEFORE);
    let end = (at + term.chars().count() + AFTER).min(chars.len());
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(&chars[start..end]);
    if end < chars.len() {
        out.push('…');
    }
    out
}

#[derive(Debug)]
struct Hit {
    score: u32,
    slug: String,
    path: String,
    kind: Kind,
    line: usize,
    page: Option<u32>,
    snippet: String,
}

/// `library_search { query, library?, kind? }`: every query word is a
/// case-insensitive term; a line scores by how many distinct terms it holds
/// (squared), how often, whether it holds the whole query as a phrase, and
/// whether the file's path or title names the terms. Best lines first, at
/// most three per file, each with its line number and PDF page.
pub(crate) fn search(
    repo: &Path,
    query: &str,
    library: Option<&str>,
    kind: Option<&str>,
) -> Result<String, String> {
    let kind = kind_arg(kind)?;
    let terms = search_terms(query);
    if terms.is_empty() {
        return Err("`query` needs at least one word".into());
    }
    let phrase = query.trim().to_lowercase();
    let libs = scope(repo, library)?;
    let mut hits: Vec<Hit> = Vec::new();
    for lib in &libs {
        for f in &lib.files {
            if kind.is_some_and(|k| k != f.kind)
                || !matches!(f.status, Status::Ok | Status::Truncated)
            {
                continue;
            }
            let Ok(text) = fs::read_to_string(text_path(repo, &lib.slug, &f.path)) else {
                continue;
            };
            let lower_all = text.to_lowercase();
            if !terms.iter().any(|t| lower_all.contains(t.as_str())) {
                continue;
            }
            let named = format!("{} {}", f.path, f.title).to_lowercase();
            let name_bonus = terms.iter().filter(|t| named.contains(t.as_str())).count() as u32 * 3;
            let mut page = None;
            let mut file_hits: Vec<Hit> = Vec::new();
            // Lowercasing never adds or removes a line break, so the lines
            // of the two strings pair up.
            for (i, (line, lower)) in text.lines().zip(lower_all.lines()).enumerate() {
                if let Some(p) = page_tag(line) {
                    page = Some(p);
                }
                let mut distinct = 0u32;
                let mut total = 0u32;
                let mut first: Option<&str> = None;
                for t in &terms {
                    let n = lower.matches(t.as_str()).count() as u32;
                    if n > 0 {
                        distinct += 1;
                        total += n;
                        first.get_or_insert(t.as_str());
                    }
                }
                let Some(first) = first else { continue };
                let mut score = distinct * distinct * 10 + total.min(5) + name_bonus;
                if terms.len() > 1 && lower.contains(&phrase) {
                    score += 25;
                }
                file_hits.push(Hit {
                    score,
                    slug: lib.slug.clone(),
                    path: f.path.clone(),
                    kind: f.kind,
                    line: i + 1,
                    page,
                    snippet: snippet(line, lower, first),
                });
            }
            file_hits.sort_by(|a, b| b.score.cmp(&a.score).then(a.line.cmp(&b.line)));
            file_hits.truncate(MAX_HITS_PER_FILE);
            hits.extend(file_hits);
        }
    }
    let where_ = match library.map(str::trim).filter(|l| !l.is_empty()) {
        Some(l) => format!("library `{l}`"),
        None => "the libraries".to_owned(),
    };
    if hits.is_empty() {
        return Ok(format!(
            "No matches for \"{}\" in {where_}{}. Try fewer or other words (the text's own language and terms); library_list shows what each library holds.",
            query.trim(),
            kind.map_or(String::new(), |k| format!(" ({})", k.as_str()))
        ));
    }
    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.slug.cmp(&b.slug))
            .then_with(|| a.path.cmp(&b.path))
            .then(a.line.cmp(&b.line))
    });
    let more = hits.len() > MAX_HITS;
    hits.truncate(MAX_HITS);
    let mut out = format!(
        "Best matches for \"{}\" in {where_}, best first (read around one with library_read {{ path, from, to }}):\n",
        query.trim()
    );
    for (n, h) in hits.iter().enumerate() {
        let page = h.page.map_or(String::new(), |p| format!(" [p.{p}]"));
        out.push_str(&format!(
            "{}. {}/{}:{}{page} ({}) — {}\n",
            n + 1,
            h.slug,
            h.path,
            h.line,
            h.kind.as_str(),
            h.snippet
        ));
    }
    if more {
        out.push_str("[more matches: narrow with `library`, `kind` or more words]\n");
    }
    Ok(out)
}

/// `<slug>/<relative path>`, with an optional `library/` in front.
fn split_path(path: &str) -> Result<(&str, &str), String> {
    let p = path.trim().trim_start_matches('/');
    let p = p.strip_prefix("library/").unwrap_or(p);
    let (slug, rel) = p
        .split_once('/')
        .ok_or_else(|| format!("`{path}` isn't a library path; use `<library>/<file>` as library_list and library_search print it"))?;
    if !valid_slug(slug) || !safe_rel(rel) {
        return Err(format!("`{path}` isn't a library path; use `<library>/<file>` as library_list and library_search print it"));
    }
    Ok((slug, rel))
}

/// `library_read { path, from?, to? }`: lines `from..=to` (1-based) of a
/// file's extracted text, at most [`MAX_READ_BYTES`].
pub(crate) fn read(
    repo: &Path,
    path: &str,
    from: Option<usize>,
    to: Option<usize>,
) -> Result<String, String> {
    let (slug, rel) = split_path(path)?;
    let lib = load(repo, slug)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no library `{slug}`; library_list shows the libraries"))?;
    let f = lib.files.iter().find(|f| f.path == rel).ok_or_else(|| {
        format!("`{rel}` is not in library `{slug}`; library_list shows its files")
    })?;
    match f.status {
        Status::NoText => {
            return Err(format!(
                "`{slug}/{rel}` has no text: {}. Its original stays on the device that imported the library",
                f.note.as_deref().unwrap_or("a scan with no text layer, and there is no OCR")
            ))
        }
        Status::Failed => {
            return Err(format!(
                "`{slug}/{rel}` couldn't be extracted at import: {}",
                f.note.as_deref().unwrap_or("unknown error")
            ))
        }
        Status::Ok | Status::Truncated => {}
    }
    let text = fs::read_to_string(text_path(repo, slug, rel)).map_err(|_| {
        format!(
            "the text of `{slug}/{rel}` isn't on this device yet; it arrives with the next sync"
        )
    })?;
    let total = text.lines().count();
    let start = from.unwrap_or(1).max(1);
    if start > total {
        return Err(format!(
            "line {start} is past the end: `{slug}/{rel}` has {total} lines"
        ));
    }
    let end = to.unwrap_or(usize::MAX).min(total);
    if end < start {
        return Err(format!("`to` ({end}) is before `from` ({start})"));
    }
    let mut body = String::new();
    let mut last = start - 1;
    for line in text.lines().skip(start - 1).take(end + 1 - start) {
        if body.len() + line.len() + 1 > MAX_READ_BYTES && last >= start {
            break;
        }
        body.push_str(line);
        body.push('\n');
        last += 1;
    }
    let mut head = format!("{slug}/{rel} — {}", f.kind.as_str());
    if let Some(p) = f.pages {
        head.push_str(&format!(", {p} pages"));
    }
    head.push_str(&format!(", lines {start}-{last} of {total}"));
    if f.status == Status::Truncated {
        head.push_str(" (the extracted text was cut at 2 MiB)");
    }
    let mut out = format!("{head}\n\n{body}");
    if last < total {
        out.push_str(&format!(
            "\n[more: continue with from={}{}]\n",
            last + 1,
            if last < end {
                " (256 KiB per read)"
            } else {
                ""
            }
        ));
    }
    Ok(out)
}

fn line_arg(args: &Value, key: &str) -> Result<Option<usize>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n
            .as_u64()
            .filter(|n| *n >= 1)
            .map(|n| Some(n as usize))
            .ok_or_else(|| format!("`{key}` must be a line number from 1")),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) => s
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|n| *n >= 1)
            .map(Some)
            .ok_or_else(|| format!("`{key}` must be a line number from 1")),
        Some(_) => Err(format!("`{key}` must be a line number from 1")),
    }
}

/// Run one library tool (blocking file IO).
pub(crate) fn run_tool(repo: &Path, name: &str, args: &Value) -> Result<String, String> {
    use crate::tools::{opt_str, req_str};
    match name {
        "library_list" => list(repo, opt_str(args, "library"), opt_str(args, "kind")),
        "library_search" => search(
            repo,
            req_str(args, "query")?,
            opt_str(args, "library"),
            opt_str(args, "kind"),
        ),
        "library_read" => read(
            repo,
            req_str(args, "path")?,
            line_arg(args, "from")?,
            line_arg(args, "to")?,
        ),
        other => Err(format!("unknown tool `{other}`")),
    }
}

// ---------------------------------------------------------------------------
// Prompt block and session links
// ---------------------------------------------------------------------------

/// The libraries the learner linked to a session (a `library` attachment
/// on one of their messages), first link first.
pub(crate) fn linked(ops: &[Op]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for op in ops {
        if let Op::Message(m) = op {
            for a in m.attachments.iter().filter(|a| a.kind == "library") {
                if !out.contains(&a.path) {
                    out.push(a.path.clone());
                }
            }
        }
    }
    out
}

/// One line per library for the prompt block, linked libraries first.
fn prompt_lines(libs: &[Library], linked: &[String]) -> String {
    let mut ordered: Vec<&Library> = linked
        .iter()
        .filter_map(|s| libs.iter().find(|l| &l.slug == s))
        .collect();
    ordered.extend(libs.iter().filter(|l| !linked.contains(&l.slug)));
    // A learner-written field must not open a template placeholder.
    let clean = |s: &str| s.replace("{{", "{ {").replace('\n', " ");
    let mut out = String::new();
    for lib in ordered.iter().take(MAX_PROMPT_LIBRARIES) {
        let mut line = format!("- **{}** (`{}`", clean(&lib.name), lib.slug);
        if linked.contains(&lib.slug) {
            line.push_str(", linked to this session");
        }
        line.push(')');
        if !lib.topic.is_empty() {
            line.push_str(&format!(" — topic: {}", clean(&lib.topic)));
        }
        if !lib.description.is_empty() {
            line.push_str(&format!(" — {}", clip_chars(&clean(&lib.description), 300)));
        }
        line.push_str(&format!(
            ". {} files: {}",
            lib.files.len(),
            kind_counts(lib)
        ));
        let no_text = lib
            .files
            .iter()
            .filter(|f| f.status == Status::NoText)
            .count();
        if no_text > 0 {
            line.push_str(&format!("; {no_text} scans without text"));
        }
        line.push_str(".\n");
        out.push_str(&line);
    }
    if ordered.len() > MAX_PROMPT_LIBRARIES {
        out.push_str(&format!(
            "- … and {} more (library_list shows them all).\n",
            ordered.len() - MAX_PROMPT_LIBRARIES
        ));
    }
    out.trim_end().to_owned()
}

impl Harness {
    /// The `shared/library.md` block for `agent` (its flag: `tutor`,
    /// `planner`, `researcher`, `coach`), listing every library with the
    /// session's linked ones first; empty when there are none, so a learner
    /// without libraries pays no tokens. Starts with a blank line, ready to
    /// append to a composed system prompt.
    pub(crate) fn library_prompt(&self, ops: &[Op], agent: &str) -> Result<String, Error> {
        let libs = load_all(&self.repo);
        if libs.is_empty() {
            return Ok(String::new());
        }
        let mut values = TemplateValues::new();
        values.set("libraries", prompt_lines(&libs, &linked(ops)));
        let flags = HashSet::from([agent.to_owned()]);
        let text = render(
            &Composer::new(&self.repo).load("shared/library.md")?,
            &values,
            &flags,
        )?;
        Ok(format!("\n\n{}", text.trim_end()))
    }

    /// Run `library_list`, `library_search` or `library_read` off the async
    /// workers.
    pub(crate) async fn library_tool(&self, name: &str, args: &Value) -> Exec {
        let repo = self.repo.clone();
        let tool = name.to_owned();
        let args = args.clone();
        match tokio::task::spawn_blocking(move || run_tool(&repo, &tool, &args)).await {
            Ok(Ok(text)) => done(text),
            Ok(Err(e)) => refuse(name, e),
            Err(e) => refuse(name, format!("internal error ({e})")),
        }
    }

    // ---- the learner's commands (Home '+', Settings → Libraries) --------

    /// Every library, by id, with this device's link to its source.
    pub fn libraries(&self) -> Result<Vec<LibrarySummary>, Error> {
        Ok(load_all(&self.repo)
            .iter()
            .map(|l| summary(l, &self.config_dir))
            .collect())
    }

    fn library_desktop_only(&self) -> Result<(), Error> {
        if self.is_mobile() {
            return Err(Error::Invalid(
                "libraries are imported and refreshed on a desktop; this device reads their synced text".into(),
            ));
        }
        Ok(())
    }

    /// Import a folder as a reference library (copy in, skip junk, extract
    /// text). A folder this device already imported is refreshed instead,
    /// so picking it again never duplicates it. Blocking: run it off the UI
    /// thread; `progress` gets one event per file.
    pub fn library_import(
        &self,
        path: &str,
        options: ImportOptions,
        progress: &mut dyn FnMut(&ImportProgress),
    ) -> Result<ImportReport, Error> {
        self.library_desktop_only()?;
        let raw = crate::files::expand_home(path.trim());
        if !raw.is_absolute() {
            return Err(Error::Invalid(format!("`{path}` is not an absolute path")));
        }
        let source = raw
            .canonicalize()
            .map_err(|e| Error::Invalid(format!("can't open `{path}` ({e})")))?;
        if !source.is_dir() {
            return Err(Error::Invalid(format!("`{path}` is not a folder")));
        }
        // The data repo and this device's settings hold the library itself:
        // importing a folder around or inside them would copy it into itself.
        for (own, what) in [
            (&self.repo, "the app's data folder"),
            (&self.config_dir, "the app's settings folder"),
        ] {
            let own = own.canonicalize().unwrap_or_else(|_| own.clone());
            if own.starts_with(&source) || source.starts_with(&own) {
                return Err(Error::Invalid(format!(
                    "`{path}` contains or is inside {what}; pick the folder of references itself"
                )));
            }
        }
        let folder_name = source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "library".into());
        let name = options
            .name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| folder_name.clone());
        let slug = choose_slug(&self.repo, &self.config_dir, &source, &name);
        let fresh = load(&self.repo, &slug)?.is_none();
        let meta = Meta {
            name: if fresh { Some(name) } else { options.name },
            description: options.description,
            topic: options.topic,
            max_file_bytes: options.max_file_mb.map(|mb| mb.saturating_mul(1024 * 1024)),
        };
        let report = import_into(&self.repo, &self.config_dir, &slug, &source, meta, progress)?;
        self.sync_in_background();
        Ok(report)
    }

    /// Re-read a library's source folder on the device that imported it:
    /// only new and changed files are extracted, and files gone from the
    /// folder leave the library.
    pub fn library_refresh(
        &self,
        slug: &str,
        max_file_mb: Option<u64>,
        progress: &mut dyn FnMut(&ImportProgress),
    ) -> Result<ImportReport, Error> {
        self.library_desktop_only()?;
        check_slug(slug)?;
        if load(&self.repo, slug)?.is_none() {
            return Err(Error::NotFound(format!("library `{slug}`")));
        }
        let local = load_local(&self.config_dir, slug).ok_or_else(|| {
            Error::Invalid(
                "this library was imported on another device; import its folder here (+ on Home or Settings → Libraries) to refresh it from this one".into(),
            )
        })?;
        if !local.source.is_dir() {
            return Err(Error::Invalid(format!(
                "the folder `{}` is gone; import it again from where it is now",
                local.source.display()
            )));
        }
        let meta = Meta {
            max_file_bytes: max_file_mb.map(|mb| mb.saturating_mul(1024 * 1024)),
            ..Meta::default()
        };
        let report = import_into(
            &self.repo,
            &self.config_dir,
            slug,
            &local.source,
            meta,
            progress,
        )?;
        self.sync_in_background();
        Ok(report)
    }

    /// Remove a library: its text and index from the repo (every device, at
    /// the next sync) and this device's copied originals. The source folder
    /// is never touched.
    pub fn library_remove(&self, slug: &str) -> Result<(), Error> {
        check_slug(slug)?;
        let _busy = Busy::claim(&self.repo, slug)?;
        let repo_dir = library_dir(&self.repo, slug);
        let local = local_dir(&self.config_dir, slug);
        if !repo_dir.exists() && !local.exists() {
            return Err(Error::NotFound(format!("library `{slug}`")));
        }
        if repo_dir.exists() {
            fs::remove_dir_all(&repo_dir)?;
        }
        if local.exists() {
            fs::remove_dir_all(&local)?;
        }
        self.sync_in_background();
        Ok(())
    }

    /// Rename a library or set its description or topic (synced). `None`
    /// keeps a field; an empty name is refused.
    pub fn library_update(
        &self,
        slug: &str,
        name: Option<&str>,
        description: Option<&str>,
        topic: Option<&str>,
    ) -> Result<LibrarySummary, Error> {
        check_slug(slug)?;
        let _busy = Busy::claim(&self.repo, slug)?;
        let mut lib =
            load(&self.repo, slug)?.ok_or_else(|| Error::NotFound(format!("library `{slug}`")))?;
        if let Some(n) = name {
            if n.trim().is_empty() {
                return Err(Error::Invalid("a library needs a name".into()));
            }
            lib.name = clip_chars(n.trim(), 120);
        }
        if let Some(d) = description {
            lib.description = clip_chars(d.trim(), 2000);
        }
        if let Some(t) = topic {
            lib.topic = clip_chars(t.trim(), 120);
        }
        lib.updated = now_iso();
        save(&self.repo, &lib)?;
        self.sync_in_background();
        Ok(summary(&lib, &self.config_dir))
    }
}

/// Refuse a `library` attachment naming no library, before anything is
/// written (the composer's Home '+' links libraries this way).
pub(crate) fn check_attachments(repo: &Path, attachments: &[Attachment]) -> Result<(), Error> {
    for a in attachments.iter().filter(|a| a.kind == "library") {
        if load(repo, a.path.trim())?.is_none() {
            return Err(Error::Invalid(format!(
                "the library `{}` doesn't exist (removed?); take its chip off and try again",
                a.name.as_deref().unwrap_or(&a.path)
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::fixtures::text_pdf;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lh-library-{name}-{}", ulid::Ulid::new()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn put(root: &Path, rel: &str, bytes: &[u8]) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn bank_text() -> String {
        let mut s = String::from("# Practice set\n\n");
        for i in 1..=6 {
            s.push_str(&format!(
                "{i}. Which service stores objects?\n(A) ECS\n(B) OBS\n(C) EVS\n(D) VPC\nAnswer: B\n\n"
            ));
        }
        s
    }

    /// A small references folder: guides, a bank, notes, a scan, and junk.
    fn fixture() -> PathBuf {
        let src = temp("src");
        put(
            &src,
            "2026-exam-guide.pdf",
            &text_pdf(&[
                "Exam guide overview for the cloud track, stages one to three",
                "Stage two covers storage and networking across the cloud track",
            ]),
        );
        put(&src, "scanned-outline.pdf", &text_pdf(&["", ""]));
        put(
            &src,
            "docs/lab-authoring-guide.md",
            b"# Lab authoring guide\n\nHow to write a lab task.\n",
        );
        put(&src, "drills/set1.md", bank_text().as_bytes());
        put(
            &src,
            "notes/digest.md",
            b"# Digest\n\nObject storage keeps buckets.\n",
        );
        put(
            &src,
            "reference/glossary.txt",
            b"ECS: elastic cloud server\nOBS: object storage service\n",
        );
        put(&src, "page.html", b"<html><head><title>T</title><script>x()</script></head><body><h1>Cloud page</h1><p>Hello &amp; welcome</p></body></html>");
        // Junk that must never be imported.
        put(&src, ".git/config", b"[core]");
        put(&src, "node_modules/pkg/index.js", b"module.exports = 1");
        put(&src, "work/__pycache__/x.pyc", b"\0\0");
        put(&src, "video.mp4", b"\0\0\0 ftyp");
        put(&src, "bundle.zip", b"PK\x03\x04");
        put(&src, "slides.docx", b"PK\x03\x04");
        put(&src, "blob.unknownext", b"\0\x01\x02binary");
        put(&src, "big.txt", &vec![b'x'; 200 * 1024]);
        src
    }

    /// Per-file cap in these tests: above every fixture but `big.txt`.
    const CAP: u64 = 100 * 1024;

    fn import(repo: &Path, config: &Path, src: &Path, max: u64) -> ImportReport {
        let meta = Meta {
            name: Some("Cloud refs".into()),
            max_file_bytes: Some(max),
            ..Meta::default()
        };
        import_into(repo, config, "cloud-refs", src, meta, &mut |_| {}).unwrap()
    }

    #[test]
    fn import_skips_junk_and_oversized_files() {
        let (src, repo, config) = (fixture(), temp("repo"), temp("config"));
        let report = import(&repo, &config, &src, CAP);
        let lib = load(&repo, "cloud-refs").unwrap().unwrap();
        let paths: Vec<&str> = lib.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "2026-exam-guide.pdf",
                "docs/lab-authoring-guide.md",
                "drills/set1.md",
                "notes/digest.md",
                "page.html",
                "reference/glossary.txt",
                "scanned-outline.pdf",
            ]
        );
        assert_eq!(report.added, 7);
        let skipped = |r: &str| lib.skipped.get(r).copied().unwrap_or(0);
        assert_eq!(skipped("ignored-folder"), 3, "{:?}", lib.skipped); // .git, node_modules, __pycache__
        assert_eq!(skipped("no-text-type"), 3, "{:?}", lib.skipped); // mp4, zip, sniffed blob
        assert_eq!(skipped("unsupported"), 1);
        assert_eq!(skipped("too-large"), 1);
        assert_eq!(lib.skipped_samples["too-large"], ["big.txt"]);
        // Nothing of the junk reached the repo or the device copy.
        assert!(!repo.join("library/cloud-refs/text/video.mp4.md").exists());
        assert!(!config.join("library/cloud-refs/originals/.git").exists());
        assert!(!config
            .join("library/cloud-refs/originals/node_modules")
            .exists());
    }

    #[test]
    fn import_writes_text_to_the_repo_and_originals_to_the_device() {
        let (src, repo, config) = (fixture(), temp("repo"), temp("config"));
        import(&repo, &config, &src, CAP);
        let lib = load(&repo, "cloud-refs").unwrap().unwrap();
        let file = |p: &str| lib.files.iter().find(|f| f.path == p).unwrap().clone();

        let guide = file("2026-exam-guide.pdf");
        assert_eq!(
            (guide.kind, guide.status, guide.pages),
            (Kind::Guide, Status::Ok, Some(2))
        );
        let text = fs::read_to_string(repo.join("library/cloud-refs/text/2026-exam-guide.pdf.md"))
            .unwrap();
        assert!(text.starts_with("[p.1] Exam guide overview"), "{text}");
        assert!(text.contains("\n[p.2] Stage two covers storage"), "{text}");
        assert_eq!(guide.text_bytes, text.len() as u64);

        let scan = file("scanned-outline.pdf");
        assert_eq!((scan.status, scan.scanned_pages), (Status::NoText, 2));
        assert!(!repo
            .join("library/cloud-refs/text/scanned-outline.pdf.md")
            .exists());

        assert_eq!(
            file("docs/lab-authoring-guide.md").kind,
            Kind::QuestionGuide
        );
        assert_eq!(file("drills/set1.md").kind, Kind::QuestionBank);
        assert_eq!(file("notes/digest.md").kind, Kind::Notes);
        assert_eq!(file("reference/glossary.txt").kind, Kind::Reference);
        let html = fs::read_to_string(repo.join("library/cloud-refs/text/page.html.md")).unwrap();
        assert!(
            html.contains("Hello & welcome") && !html.contains("x()"),
            "{html}"
        );

        // Originals are copied to the device, byte for byte, and the source
        // folder's absolute path never reaches the synced index.
        assert_eq!(
            fs::read(config.join("library/cloud-refs/originals/notes/digest.md")).unwrap(),
            fs::read(src.join("notes/digest.md")).unwrap()
        );
        assert!(!config.join("library/cloud-refs/originals/big.txt").exists());
        let index = fs::read_to_string(repo.join("library/cloud-refs/library.json")).unwrap();
        assert!(!index.contains(&*src.to_string_lossy()), "{index}");
        let summary = summary(&lib, &config);
        assert_eq!(summary.source.as_deref(), Some(&*src.to_string_lossy()));
        assert!(summary.originals);
        assert_eq!(summary.kinds["question-bank"], 1);
    }

    #[test]
    fn refresh_re_extracts_only_what_changed() {
        let (src, repo, config) = (fixture(), temp("repo"), temp("config"));
        import(&repo, &config, &src, CAP);
        let glossary = repo.join("library/cloud-refs/text/reference/glossary.txt.md");
        // Mark an untouched file's text: a re-extraction would overwrite it.
        fs::write(&glossary, "MARK\n").unwrap();

        put(
            &src,
            "notes/digest.md",
            b"# Digest\n\nObject storage keeps buckets and versions.\n",
        );
        fs::remove_file(src.join("page.html")).unwrap();
        put(&src, "notes/new.md", b"# New\n\nfresh\n");
        let report = import(&repo, &config, &src, CAP);
        assert_eq!(
            (
                report.added,
                report.changed,
                report.removed,
                report.unchanged
            ),
            (1, 1, 1, 5)
        );
        assert_eq!(fs::read_to_string(&glossary).unwrap(), "MARK\n");
        assert!(
            fs::read_to_string(repo.join("library/cloud-refs/text/notes/digest.md.md"))
                .unwrap()
                .contains("versions")
        );
        assert!(!repo.join("library/cloud-refs/text/page.html.md").exists());
        assert!(!config
            .join("library/cloud-refs/originals/page.html")
            .exists());

        // A device without the size/mtime stamps (another computer with the
        // same folder) still reuses the synced text by hash.
        let other = temp("config2");
        let report = import(&repo, &other, &src, 1024);
        assert_eq!((report.added, report.changed, report.unchanged), (0, 0, 7));
        assert_eq!(fs::read_to_string(&glossary).unwrap(), "MARK\n");
        assert!(other
            .join("library/cloud-refs/originals/notes/new.md")
            .is_file());
    }

    #[test]
    fn long_text_is_cut_at_the_cap_and_marked() {
        let (src, repo, config) = (temp("src"), temp("repo"), temp("config"));
        let line = "storage line with words\n";
        put(
            &src,
            "huge.txt",
            line.repeat(MAX_TEXT_BYTES / line.len() + 100).as_bytes(),
        );
        import(&repo, &config, &src, 64 * 1024 * 1024);
        let lib = load(&repo, "cloud-refs").unwrap().unwrap();
        assert_eq!(lib.files[0].status, Status::Truncated);
        let text = fs::read_to_string(repo.join("library/cloud-refs/text/huge.txt.md")).unwrap();
        assert!(text.len() <= MAX_TEXT_BYTES);
        assert!(text.ends_with("passed the 2 MiB text cap]\n"));
        assert!(read(&repo, "cloud-refs/huge.txt", Some(1), Some(2))
            .unwrap()
            .contains("cut at 2 MiB"));
    }

    #[test]
    fn search_ranks_lines_with_more_terms_first_and_cites_pages() {
        let (src, repo, config) = (fixture(), temp("repo"), temp("config"));
        import(&repo, &config, &src, CAP);
        let out = search(&repo, "object storage", None, None).unwrap();
        let first = out.lines().nth(1).unwrap();
        // Both terms as a phrase beat a line with one of them.
        assert!(
            first.starts_with("1. cloud-refs/notes/digest.md:3 (notes)"),
            "{out}"
        );
        assert!(
            out.contains("cloud-refs/reference/glossary.txt:2 (reference)"),
            "{out}"
        );

        let pdf = search(&repo, "networking", None, None).unwrap();
        assert!(
            pdf.contains("cloud-refs/2026-exam-guide.pdf:") && pdf.contains("[p.2] (guide)"),
            "{pdf}"
        );

        let banks = search(&repo, "service", None, Some("question-bank")).unwrap();
        assert!(
            banks.contains("cloud-refs/drills/set1.md:3 (question-bank)"),
            "{banks}"
        );
        assert!(!banks.contains("glossary"), "{banks}");
        assert!(search(&repo, "nothing-like-this", None, None)
            .unwrap()
            .starts_with("No matches"));
        assert!(search(&repo, "storage", Some("nope"), None)
            .unwrap_err()
            .contains("cloud-refs"));
        assert!(search(&repo, "storage", None, Some("bogus")).is_err());
    }

    #[test]
    fn read_slices_lines_and_refuses_paths_outside_the_index() {
        let (src, repo, config) = (fixture(), temp("repo"), temp("config"));
        import(&repo, &config, &src, CAP);
        let out = read(&repo, "cloud-refs/drills/set1.md", Some(3), Some(4)).unwrap();
        assert!(
            out.starts_with("cloud-refs/drills/set1.md — question-bank, lines 3-4 of"),
            "{out}"
        );
        assert!(
            out.contains("1. Which service stores objects?\n(A) ECS\n"),
            "{out}"
        );
        assert!(out.contains("[more: continue with from=5]"), "{out}");
        assert!(read(&repo, "library/cloud-refs/notes/digest.md", None, None).is_ok());
        assert!(read(&repo, "cloud-refs/drills/set1.md", Some(999), None)
            .unwrap_err()
            .contains("past the end"));
        assert!(read(&repo, "cloud-refs/../../etc/passwd", None, None).is_err());
        assert!(read(&repo, "cloud-refs/library.json", None, None)
            .unwrap_err()
            .contains("not in library"));
        assert!(read(&repo, "cloud-refs/scanned-outline.pdf", None, None)
            .unwrap_err()
            .contains("no OCR"));
    }

    #[test]
    fn tools_dispatch_by_name_and_validate_arguments() {
        let (src, repo, config) = (fixture(), temp("repo"), temp("config"));
        let empty = temp("empty");
        assert!(run_tool(&empty, "library_list", &serde_json::json!({}))
            .unwrap_err()
            .contains("no reference libraries"));
        import(&repo, &config, &src, CAP);
        let list = run_tool(
            &repo,
            "library_list",
            &serde_json::json!({ "kind": "question-guide" }),
        )
        .unwrap();
        assert!(
            list.contains("## Cloud refs (`cloud-refs`)")
                && list.contains("`cloud-refs/docs/lab-authoring-guide.md`"),
            "{list}"
        );
        assert!(!list.contains("glossary"), "{list}");
        let read = run_tool(
            &repo,
            "library_read",
            &serde_json::json!({ "path": "cloud-refs/notes/digest.md", "from": "3", "to": 3 }),
        )
        .unwrap();
        assert!(read.contains("Object storage keeps buckets."), "{read}");
        assert!(run_tool(
            &repo,
            "library_read",
            &serde_json::json!({ "path": "cloud-refs/notes/digest.md", "from": 0 })
        )
        .is_err());
        assert!(run_tool(&repo, "library_search", &serde_json::json!({}))
            .unwrap_err()
            .contains("query"));
    }

    #[test]
    fn the_prompt_lists_linked_libraries_first() {
        let lib = |slug: &str, name: &str| Library {
            slug: slug.into(),
            name: name.into(),
            description: "with {{braces}}".into(),
            topic: String::new(),
            created: String::new(),
            updated: String::new(),
            max_file_bytes: 0,
            files: Vec::new(),
            skipped: BTreeMap::new(),
            skipped_samples: BTreeMap::new(),
        };
        let libs = [lib("alpha", "Alpha"), lib("beta", "Beta")];
        let text = prompt_lines(&libs, &["beta".into()]);
        let lines: Vec<&str> = text.lines().collect();
        assert!(
            lines[0].starts_with("- **Beta** (`beta`, linked to this session)"),
            "{text}"
        );
        assert!(lines[1].starts_with("- **Alpha** (`alpha`)"), "{text}");
        assert!(!text.contains("{{"));
    }

    #[test]
    fn a_second_folder_with_the_same_name_gets_its_own_library() {
        let (repo, config) = (temp("repo"), temp("config"));
        let a = temp("a").join("refs");
        let b = temp("b").join("refs");
        put(&a, "one.md", b"# One\n");
        put(&b, "two.md", b"# Two\n");
        let first = choose_slug(&repo, &config, &a, "refs");
        import_into(&repo, &config, &first, &a, Meta::default(), &mut |_| {}).unwrap();
        assert_eq!(first, "refs");
        // The same folder again is the same library.
        assert_eq!(choose_slug(&repo, &config, &a, "refs"), "refs");
        // Another folder named alike is a new one.
        assert_eq!(choose_slug(&repo, &config, &b, "refs"), "refs-2");
        // On another device, the same folder adopts the synced library.
        assert_eq!(choose_slug(&repo, &temp("config2"), &a, "refs"), "refs");
        assert_eq!(choose_slug(&repo, &temp("config3"), &b, "refs"), "refs-2");
    }

    #[test]
    fn classification_reads_names_before_text() {
        assert_eq!(classify("2026-global-exam.pdf", "", ""), Kind::QuestionBank);
        assert_eq!(
            classify("2026-global-exam-test-guide.pdf", "", ""),
            Kind::Guide
        );
        assert_eq!(
            classify(
                "downloads/cloud-certs/HCIA-Exam-Outline-H13-511.pdf",
                "",
                ""
            ),
            Kind::Guide
        );
        assert_eq!(classify("past-papers/INDEX.md", "", ""), Kind::QuestionBank);
        assert_eq!(
            classify("hcia-ai-v4.0-900q-master.pdf", "", ""),
            Kind::QuestionBank
        );
        assert_eq!(
            classify("notes/digest/global-exam-2026.md", "Global exam digest", ""),
            Kind::Notes
        );
        assert_eq!(
            classify("misc/a.md", "How to write questions", ""),
            Kind::QuestionGuide
        );
        assert_eq!(classify("misc/a.md", "", &bank_text()), Kind::QuestionBank);
        assert_eq!(classify("misc/a.md", "", "plain words"), Kind::Reference);
        assert_eq!(
            classify(
                "notes/digest/ai-fundamentals-part1.md",
                "AI course chapter digest",
                ""
            ),
            Kind::Notes
        );
        // Prose that mentions answering and lists e.g./i.e. lines is no bank.
        let prose = "I can answer questions.\ne. g. lines\n".repeat(20);
        assert_eq!(
            classify("docs/model-calling.md", "", &prose),
            Kind::Reference
        );
    }
}
