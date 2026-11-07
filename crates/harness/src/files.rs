//! Read-only access to the learner's files (`file_find`, `file_read`, and the
//! researcher's `source_*` tools).
//!
//! Every path is resolved inside one root (the home directory): relative paths
//! are joined to it, `~/` is expanded, and the canonical path (symlinks
//! resolved) must still lie inside the canonical root, so a symlink can't
//! escape it. Reads are capped at 256 KiB. A PDF is read by its pages
//! (`crate::pdf`): text pages come back tagged `[p.3] …`, and a scanned page
//! says it has no text layer instead of pretending to be empty.

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

pub const MAX_READ_BYTES: u64 = 256 * 1024;
const MAX_FIND_RESULTS: usize = 50;
const MAX_FIND_VISITS: usize = 20_000;
const MAX_FIND_DEPTH: usize = 8;
const MAX_SEARCH_HITS: usize = 50;
const DEFAULT_READ_LINES: usize = 200;
/// Pages a PDF `source_read` gives when the model names no page range.
const DEFAULT_PDF_PAGES: u32 = 5;
/// First-line length kept per page in a PDF outline.
const MAX_OUTLINE_LINE_CHARS: usize = 200;
/// Pages listed in a PDF outline.
const MAX_OUTLINE_PAGES: usize = 200;

/// The learner's home directory, the root for every file tool.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// Expand a leading `~` / `~/` using the home directory; other paths are
/// returned unchanged. Mention paths use this display form (`~/notes.md`).
pub fn expand_home(path: &str) -> PathBuf {
    if path == "~" {
        return home_dir().unwrap_or_else(|| PathBuf::from(path));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

/// Resolve `path` inside `root`, refusing anything that escapes it.
pub fn resolve(root: &Path, path: &str) -> Result<PathBuf, String> {
    resolve_roots(std::slice::from_ref(&root.to_path_buf()), path)
}

/// Resolve `path` inside any of `roots` (W3/W4 read roots). Relative paths are
/// joined to the first root (the home directory); absolute paths must lie in
/// one of the roots after canonicalisation (symlinks included).
pub fn resolve_roots(roots: &[PathBuf], path: &str) -> Result<PathBuf, String> {
    let path = path.trim();
    if path.is_empty() {
        return Err("path is empty".into());
    }
    let canonical_roots: Vec<PathBuf> =
        roots.iter().filter_map(|r| r.canonicalize().ok()).collect();
    let root = canonical_roots
        .first()
        .ok_or_else(|| "home directory unavailable".to_owned())?;
    let candidate = if let Some(rest) = path.strip_prefix("~/") {
        root.join(rest)
    } else if path == "~" {
        root.clone()
    } else {
        let p = Path::new(path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            root.join(p)
        }
    };
    let canonical = candidate
        .canonicalize()
        .map_err(|_| format!("`{path}` doesn't exist"))?;
    if canonical_roots.iter().any(|r| canonical.starts_with(r)) {
        Ok(canonical)
    } else {
        Err(format!("`{path}` is outside the directories you granted"))
    }
}

fn is_pdf(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

/// Parse `"20-80"` / `"20"` into a 1-based inclusive range.
fn parse_range(spec: &str) -> Result<(usize, usize), String> {
    let spec = spec.trim();
    let (a, b) = match spec.split_once('-') {
        Some((a, b)) => (a.trim(), b.trim()),
        None => (spec, spec),
    };
    let start: usize = a
        .parse()
        .map_err(|_| format!("bad line range `{spec}` (use e.g. `20-80`)"))?;
    let end: usize = if b.is_empty() {
        usize::MAX
    } else {
        b.parse()
            .map_err(|_| format!("bad line range `{spec}` (use e.g. `20-80`)"))?
    };
    if start == 0 || end < start {
        return Err(format!("bad line range `{spec}` (lines start at 1)"));
    }
    Ok((start, end))
}

/// What a resolved file is, as far as reading goes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Pdf,
}

/// Resolve `path` inside `roots` and decide how it reads: a PDF by its
/// `%PDF` header (or a `.pdf` name), anything else as text.
fn resolve_file_roots(roots: &[PathBuf], path: &str) -> Result<(PathBuf, Kind), String> {
    let full = resolve_roots(roots, path)?;
    if full.is_dir() {
        return Err(format!("`{path}` is a directory; use file_find"));
    }
    let mut file = fs::File::open(&full).map_err(|e| format!("can't open `{path}` ({e})"))?;
    let mut head = [0u8; 1024];
    let n = file
        .read(&mut head)
        .map_err(|e| format!("can't read `{path}` ({e})"))?;
    let head = &head[..n];
    let kind = if crate::pdf::is_pdf(head) || is_pdf(&full) {
        Kind::Pdf
    } else {
        Kind::Text
    };
    if kind == Kind::Text && head.contains(&0) {
        return Err(format!("`{path}` is not a text file"));
    }
    Ok((full, kind))
}

fn resolve_file(root: &Path, path: &str) -> Result<(PathBuf, Kind), String> {
    resolve_file_roots(std::slice::from_ref(&root.to_path_buf()), path)
}

fn open_text(root: &Path, path: &str) -> Result<(PathBuf, fs::File), String> {
    let (full, kind) = resolve_file(root, path)?;
    if kind == Kind::Pdf {
        return Err(format!("`{path}` is a PDF; read it by its pages"));
    }
    let file = fs::File::open(&full).map_err(|e| format!("can't open `{path}` ({e})"))?;
    Ok((full, file))
}

/// A PDF's bytes, within the parser's input cap.
fn read_pdf(path: &str, full: &Path) -> Result<Vec<u8>, String> {
    let size = fs::metadata(full).map(|m| m.len()).unwrap_or(0);
    if size > crate::pdf::MAX_PDF_BYTES as u64 {
        return Err(format!(
            "`{path}` is {} MiB, over the {} MiB PDF cap",
            size / (1024 * 1024),
            crate::pdf::MAX_PDF_BYTES / (1024 * 1024)
        ));
    }
    fs::read(full).map_err(|e| format!("can't read `{path}` ({e})"))
}

/// The bytes of a PDF the file tools may read (inside `root`, within the
/// input cap). Used by the researcher's `source_page_image`.
pub fn read_pdf_file(root: &Path, path: &str) -> Result<Vec<u8>, String> {
    let (full, kind) = resolve_file(root, path)?;
    if kind != Kind::Pdf {
        return Err(format!("`{path}` isn't a PDF"));
    }
    read_pdf(path, &full)
}

/// `[p.N] …` text of a PDF the tutor reads whole: the page count and the
/// document's total text are capped by [`crate::pdf`], and the result itself
/// by the read cap.
fn pdf_text(path: &str, full: &Path) -> Result<String, String> {
    let bytes = read_pdf(path, full)?;
    let pages = crate::pdf::pages(&bytes).map_err(|e| format!("`{path}`: {e}"))?;
    Ok(crate::pdf::tagged(&pages, MAX_READ_BYTES as usize))
}

/// Parse `"3"`, `"3-5"` or `"3-"` into a 1-based inclusive page range.
fn parse_pages(spec: &str) -> Result<(u32, u32), String> {
    let spec = spec.trim();
    let (a, b) = match spec.split_once('-') {
        Some((a, b)) => (a.trim(), b.trim()),
        None => (spec, spec),
    };
    let start: u32 = a
        .parse()
        .map_err(|_| format!("bad page range `{spec}` (use e.g. `3-5`)"))?;
    let end: u32 = if b.is_empty() {
        u32::MAX
    } else {
        b.parse()
            .map_err(|_| format!("bad page range `{spec}` (use e.g. `3-5`)"))?
    };
    if start == 0 || end < start {
        return Err(format!("bad page range `{spec}` (pages start at 1)"));
    }
    Ok((start, end))
}

/// Read lines `[start, end]` (1-based), stopping at the byte cap.
fn read_lines(file: fs::File, start: usize, end: usize) -> (String, usize, bool) {
    let mut out = String::new();
    let mut last = 0;
    let mut capped = false;
    for (i, line) in BufReader::new(file).lines().enumerate() {
        let n = i + 1;
        if n < start {
            continue;
        }
        if n > end {
            break;
        }
        let Ok(line) = line else { break };
        if out.len() + line.len() + 1 > MAX_READ_BYTES as usize {
            capped = true;
            break;
        }
        out.push_str(&line);
        out.push('\n');
        last = n;
    }
    (out, last, capped)
}

/// `file_read { path, lines? }`.
pub fn file_read(root: &Path, path: &str, lines: Option<&str>) -> Result<String, String> {
    file_read_roots(std::slice::from_ref(&root.to_path_buf()), path, lines)
}

/// `file_read` over the home root plus session-granted roots (W3).
pub fn file_read_roots(
    roots: &[PathBuf],
    path: &str,
    lines: Option<&str>,
) -> Result<String, String> {
    let (full, kind) = resolve_file_roots(roots, path)?;
    if kind == Kind::Pdf {
        if lines.is_some() {
            return Err(format!(
                "`lines` applies to text files; `{path}` is a PDF and comes back as its pages (`[p.N] …`)"
            ));
        }
        return pdf_text(path, &full);
    }
    let file = fs::File::open(&full).map_err(|e| format!("can't open `{path}` ({e})"))?;
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    let (start, end) = match lines {
        Some(spec) => parse_range(spec)?,
        None if size > MAX_READ_BYTES => {
            return Err(format!(
                "`{path}` is {} KiB, over the 256 KiB cap; read a line range (`lines: \"1-200\"`) or use research_request with kind source",
                size / 1024
            ))
        }
        None => (1, usize::MAX),
    };
    let (text, last, capped) = read_lines(file, start, end);
    let mut out = text;
    if capped {
        out.push_str(&format!(
            "\n[stopped at line {last}: 256 KiB cap; read a later range to continue]\n"
        ));
    }
    if out.is_empty() {
        out = format!("(`{path}` has no lines in that range)");
    }
    Ok(out)
}

/// `file_find { pattern }`: paths under the root (relative, `~/`-prefixed)
/// whose relative path matches the glob, or whose name contains the pattern
/// when it has no wildcard. Hidden directories and symlinks are skipped.
pub fn file_find(root: &Path, pattern: &str) -> Result<String, String> {
    file_find_roots(std::slice::from_ref(&root.to_path_buf()), None, pattern)
}

/// How a searched root prints its hits.
#[derive(Clone, Copy)]
enum Shown {
    /// `~/rel`
    Home,
    /// `rel`, as `file_read` and `workspace_write` take workspace paths.
    Workspace,
    /// The absolute path (granted roots outside home).
    Absolute,
}

/// The canonical roots a search walks, each once: the session workspace
/// first, then home (`roots[0]`), then granted roots. A root inside one
/// already listed is covered by it; a workspace inside home is walked once,
/// as the workspace, so no hit appears twice and the op log never holds the
/// workspace's absolute path.
fn search_roots(roots: &[PathBuf], workspace: Option<&Path>) -> Vec<(PathBuf, Shown)> {
    let mut out: Vec<(PathBuf, Shown)> = Vec::new();
    let workspace = workspace.and_then(|w| w.canonicalize().ok());
    if let Some(ws) = &workspace {
        out.push((ws.clone(), Shown::Workspace));
    }
    for (i, root) in roots.iter().enumerate() {
        let Ok(root) = root.canonicalize() else {
            continue;
        };
        if i == 0 && workspace.as_ref() != Some(&root) {
            out.push((root, Shown::Home));
        } else if !out.iter().any(|(r, _)| root.starts_with(r)) {
            out.push((root, Shown::Absolute));
        }
    }
    out
}

/// One entry met by [`walk`].
struct Found<'a> {
    name: &'a str,
    /// Path relative to its root, `/`-separated.
    rel: &'a str,
    /// The path as printed to the model or the learner.
    shown: String,
    dir: bool,
}

/// Walk the [`search_roots`] depth-first (symlinks and hidden directories
/// skipped, depth and visit capped), calling `visit` until it returns false.
/// Returns false when no root exists.
fn walk(
    roots: &[PathBuf],
    workspace: Option<&Path>,
    mut visit: impl FnMut(Found<'_>) -> bool,
) -> bool {
    let searched = search_roots(roots, workspace);
    let mut visits = 0usize;
    for (root, shown) in &searched {
        let mut stack: Vec<(PathBuf, usize)> = vec![(root.clone(), 0)];
        while let Some((dir, depth)) = stack.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                visits += 1;
                if visits > MAX_FIND_VISITS {
                    return true;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_symlink() || (kind.is_dir() && name.starts_with('.')) {
                    continue;
                }
                let path = entry.path();
                // Another searched root (a workspace inside home) is walked
                // on its own.
                if kind.is_dir() && searched.iter().any(|(r, _)| r == &path) {
                    continue;
                }
                let rel = path
                    .strip_prefix(root)
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_else(|_| name.clone());
                let printed = match shown {
                    Shown::Home => format!("~/{rel}"),
                    Shown::Workspace => rel.clone(),
                    Shown::Absolute => path.to_string_lossy().into_owned(),
                };
                let more = visit(Found {
                    name: &name,
                    rel: &rel,
                    shown: printed,
                    dir: kind.is_dir(),
                });
                if !more {
                    return true;
                }
                if kind.is_dir() && depth < MAX_FIND_DEPTH {
                    stack.push((path, depth + 1));
                }
            }
        }
    }
    !searched.is_empty()
}

/// `file_find` over the home root (`roots[0]`), the session `workspace` and
/// granted roots (W3/W4). Home hits are shown `~/`-relative, workspace hits
/// relative to the workspace, granted-root hits absolute.
pub fn file_find_roots(
    roots: &[PathBuf],
    workspace: Option<&Path>,
    pattern: &str,
) -> Result<String, String> {
    let pattern = pattern.trim().trim_start_matches("~/");
    if pattern.is_empty() {
        return Err("pattern is empty".into());
    }
    if pattern.starts_with('/') || pattern.split('/').any(|c| c == "..") {
        return Err("the pattern must be relative".into());
    }
    let wildcard = pattern.contains(['*', '?']);
    let needle = pattern.to_lowercase();
    let mut hits = Vec::new();
    let any_root = walk(roots, workspace, |f| {
        if f.dir {
            return true;
        }
        let matched = if wildcard {
            glob_match(pattern, if pattern.contains('/') { f.rel } else { f.name })
        } else {
            f.rel.to_lowercase().contains(&needle)
        };
        if matched {
            hits.push(f.shown);
        }
        hits.len() < MAX_FIND_RESULTS
    });
    if !any_root {
        return Err("home directory unavailable".into());
    }
    hits.sort();
    if hits.is_empty() {
        Ok(format!("no files match `{pattern}`"))
    } else {
        Ok(hits.join("\n"))
    }
}

/// One `@`-mention candidate.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Mention {
    pub path: String,
    pub name: String,
    /// file | folder
    pub kind: String,
}

/// Files and folders for the composer's `@` list over the same roots and
/// path forms as [`file_find_roots`], hidden directories skipped, capped
/// (W3).
pub fn mentions(
    roots: &[PathBuf],
    workspace: Option<&Path>,
    query: &str,
    limit: usize,
) -> Vec<Mention> {
    let needle = query.trim().trim_start_matches('@').to_lowercase();
    let mut out: Vec<Mention> = Vec::new();
    walk(roots, workspace, |f| {
        if needle.is_empty()
            || f.rel.to_lowercase().contains(&needle)
            || f.name.to_lowercase().contains(&needle)
        {
            out.push(Mention {
                path: f.shown,
                name: f.name.to_owned(),
                kind: if f.dir { "folder" } else { "file" }.into(),
            });
        }
        out.len() < limit
    });
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Glob with `*` (within a path segment), `**` (any number of segments) and
/// `?`. Case-insensitive.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    fn segs(s: &str) -> Vec<String> {
        s.split('/').map(str::to_lowercase).collect()
    }
    fn seg_match(p: &[char], t: &[char]) -> bool {
        match (p.first(), t.first()) {
            (None, None) => true,
            (Some('*'), _) => seg_match(&p[1..], t) || (!t.is_empty() && seg_match(p, &t[1..])),
            (Some('?'), Some(_)) => seg_match(&p[1..], &t[1..]),
            (Some(a), Some(b)) if a == b => seg_match(&p[1..], &t[1..]),
            _ => false,
        }
    }
    fn walk(p: &[String], t: &[String]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some(s) if s == "**" => walk(&p[1..], t) || (!t.is_empty() && walk(p, &t[1..])),
            Some(s) => {
                !t.is_empty() && {
                    let pc: Vec<char> = s.chars().collect();
                    let tc: Vec<char> = t[0].chars().collect();
                    seg_match(&pc, &tc) && walk(&p[1..], &t[1..])
                }
            }
        }
    }
    walk(&segs(pattern), &segs(text))
}

/// `source_outline { path }`: Markdown headings with line numbers, or a
/// PDF's pages with the first line of each.
pub fn source_outline(root: &Path, path: &str) -> Result<String, String> {
    let (full, kind) = resolve_file(root, path)?;
    if kind == Kind::Pdf {
        let bytes = read_pdf(path, &full)?;
        let pages = crate::pdf::extract(&bytes).map_err(|e| format!("`{path}`: {e}"))?;
        let mut out = format!("{} pages\n", pages.len());
        for p in pages.iter().take(MAX_OUTLINE_PAGES) {
            let first = p.text.lines().map(str::trim).find(|line| !line.is_empty());
            let line = match first {
                Some(line) => line.chars().take(MAX_OUTLINE_LINE_CHARS).collect(),
                None => crate::pdf::scan_page_line(p.number),
            };
            let tag = format!("p.{}", p.number);
            out.push_str(&format!("{:>6}: {line}\n", tag));
        }
        if pages.len() > MAX_OUTLINE_PAGES {
            out.push_str(&format!(
                "(more than {MAX_OUTLINE_PAGES} pages; the rest are not listed)\n"
            ));
        }
        return Ok(out);
    }
    let (_, file) = open_text(root, path)?;
    let mut out = String::new();
    let mut total = 0;
    for (i, line) in BufReader::new(file).lines().enumerate() {
        let Ok(line) = line else { break };
        total = i + 1;
        let t = line.trim_start();
        if t.starts_with('#') && out.len() < 16_000 {
            out.push_str(&format!("{:>6}: {}\n", i + 1, t));
        }
    }
    if out.is_empty() {
        Ok(format!(
            "no headings; {total} lines. Use source_search, then source_read with a line range."
        ))
    } else {
        Ok(format!("{total} lines\n{out}"))
    }
}

/// `source_search { path, query }`: case-insensitive line matches, or a
/// PDF's per-page hits with the page number and a little context.
pub fn source_search(root: &Path, path: &str, query: &str) -> Result<String, String> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Err("query is empty".into());
    }
    let (full, kind) = resolve_file(root, path)?;
    if kind == Kind::Pdf {
        let bytes = read_pdf(path, &full)?;
        let pages = crate::pdf::extract(&bytes).map_err(|e| format!("`{path}`: {e}"))?;
        let mut hits = Vec::new();
        for p in &pages {
            if p.scanned {
                continue;
            }
            for line in p.text.lines() {
                if line.to_lowercase().contains(&needle) {
                    hits.push(format!("p.{}: {}", p.number, around(line, &needle)));
                    if hits.len() >= MAX_SEARCH_HITS {
                        hits.push("(more matches not shown)".into());
                        break;
                    }
                }
            }
            if hits.len() > MAX_SEARCH_HITS {
                break;
            }
        }
        if hits.is_empty() {
            return Ok(format!(
                "no page matches `{query}` in {} pages{}",
                pages.len(),
                if pages.iter().any(|p| p.scanned) {
                    " (pages with no text layer are scans: `source_page_image` shows one as an image)"
                } else {
                    ""
                }
            ));
        }
        return Ok(hits.join("\n"));
    }
    let (_, file) = open_text(root, path)?;
    let mut hits = Vec::new();
    for (i, line) in BufReader::new(file).lines().enumerate() {
        let Ok(line) = line else { break };
        if line.to_lowercase().contains(&needle) {
            let shown: String = line.chars().take(300).collect();
            hits.push(format!("{:>6}: {}", i + 1, shown.trim()));
            if hits.len() >= MAX_SEARCH_HITS {
                hits.push("(more matches not shown)".into());
                break;
            }
        }
    }
    if hits.is_empty() {
        Ok(format!("no lines match `{query}`"))
    } else {
        Ok(hits.join("\n"))
    }
}

/// A line bounded to the neighbourhood of the match, so one huge line can't
/// flood the result.
fn around(line: &str, needle: &str) -> String {
    const BEFORE: usize = 60;
    const AFTER: usize = 140;
    let line = line.trim();
    let chars: Vec<char> = line.chars().collect();
    let lower = line.to_lowercase();
    let Some(byte) = lower.find(needle) else {
        return line.chars().take(BEFORE + AFTER).collect();
    };
    let at = lower[..byte]
        .chars()
        .count()
        .min(chars.len().saturating_sub(1));
    let start = at.saturating_sub(BEFORE);
    let end = (at + needle.chars().count() + AFTER).min(chars.len());
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

/// `source_read { path, pages | lines }`: a PDF by page range (`"3"`,
/// `"3-5"`, `"3-"`), a text file by line range.
pub fn source_read(
    root: &Path,
    path: &str,
    pages: Option<&str>,
    lines: Option<&str>,
) -> Result<String, String> {
    let (full, kind) = resolve_file(root, path)?;
    if kind == Kind::Pdf {
        if lines.is_some() {
            return Err(format!(
                "`lines` applies to text files; `{path}` is a PDF, so read it by `pages` (e.g. `\"3-5\"`)"
            ));
        }
        return pdf_pages(path, &full, pages);
    }
    if pages.is_some() {
        return Err(format!(
            "`pages` applies to PDFs; `{path}` is a text file, so read it by `lines`"
        ));
    }
    let spec = lines.map_or_else(|| format!("1-{DEFAULT_READ_LINES}"), str::to_owned);
    let (start, end) = parse_range(&spec)?;
    let file = fs::File::open(&full).map_err(|e| format!("can't open `{path}` ({e})"))?;
    let (text, last, capped) = read_lines(file, start, end);
    let mut out = String::new();
    for (i, line) in text.lines().enumerate() {
        out.push_str(&format!("{:>6}: {line}\n", start + i));
    }
    if capped {
        out.push_str(&format!("[stopped at line {last}: 256 KiB cap]\n"));
    }
    if out.is_empty() {
        out = "(no lines in that range)".into();
    }
    Ok(out)
}

/// The requested pages of a PDF, tagged `[p.N]`, with the page range and the
/// document's page count as a header so a citation can name its page.
fn pdf_pages(path: &str, full: &Path, spec: Option<&str>) -> Result<String, String> {
    let bytes = read_pdf(path, full)?;
    let (start, end) = match spec {
        Some(spec) => parse_pages(spec)?,
        None => (1, DEFAULT_PDF_PAGES),
    };
    let (pages, count) =
        crate::pdf::pages_in(&bytes, start, end).map_err(|e| format!("`{path}`: {e}"))?;
    let last = pages.last().map_or(start, |p| p.number);
    let header = if last == start {
        format!("page {start} of {count}\n")
    } else {
        format!("pages {start}-{last} of {count}\n")
    };
    Ok(format!(
        "{header}{}",
        crate::pdf::tagged(&pages, MAX_READ_BYTES as usize)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lh-files-{}", ulid::Ulid::new()));
        fs::create_dir_all(dir.join("notes/week1")).unwrap();
        fs::write(
            dir.join("notes/week1/ohm.md"),
            "# Ohm\nV = IR\n## Dividers\nratio\n",
        )
        .unwrap();
        let pdf = crate::pdf::fixtures::text_pdf(&[
            "Chapter one of the paper, with plenty of words on it",
            "Chapter two of the paper, and the ratio appears here too",
            "",
        ]);
        fs::write(dir.join("paper.pdf"), &pdf).unwrap();
        // The same bytes under a name that says nothing: the header decides.
        fs::write(dir.join("notes/article.dat"), &pdf).unwrap();
        // A `.pdf` whose bytes aren't one: the parser says so.
        fs::write(dir.join("broken.pdf"), "%PDF-1.4 not really a pdf").unwrap();
        dir
    }

    #[test]
    fn reads_inside_the_root_and_refuses_escapes() {
        let root = temp_root();
        assert!(file_read(&root, "notes/week1/ohm.md", Some("2-2"))
            .unwrap()
            .starts_with("V = IR"));
        assert!(file_read(&root, "~/notes/week1/ohm.md", None)
            .unwrap()
            .contains("ratio"));
        assert!(file_read(&root, "../../etc/passwd", None).is_err());
        assert!(file_read(&root, "/etc/hostname", None).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc", root.join("escape")).unwrap();
            let err = file_read(&root, "escape/hostname", None).unwrap_err();
            assert!(
                err.contains("outside") || err.contains("doesn't exist"),
                "{err}"
            );
        }
        // A .pdf that isn't one is refused by the parser, not by a blanket ban.
        let err = file_read(&root, "broken.pdf", None).unwrap_err();
        assert!(err.contains("can't be read"), "{err}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_pdf_reads_by_page_through_every_file_tool() {
        let root = temp_root();
        let text = file_read(&root, "paper.pdf", None).unwrap();
        assert!(text.contains("[p.1]"), "{text}");
        assert!(text.contains("[p.3]"), "{text}");
        assert!(text.contains("Chapter two"), "{text}");
        // A page with no text layer says so instead of looking empty.
        assert!(text.contains("no text layer"), "{text}");
        assert!(text.contains("source_page_image"), "{text}");
        // The header decides, not the extension.
        assert!(file_read(&root, "notes/article.dat", None)
            .unwrap()
            .contains("[p.2]"));
        // `lines` is for text files.
        assert!(file_read(&root, "paper.pdf", Some("1-2"))
            .unwrap_err()
            .contains("`lines` applies to text files"));

        let outline = source_outline(&root, "paper.pdf").unwrap();
        assert!(outline.starts_with("3 pages"), "{outline}");
        assert!(outline.contains("p.1: Chapter one"), "{outline}");
        assert!(outline.contains("p.3: (no text layer"), "{outline}");

        let hits = source_search(&root, "paper.pdf", "ratio").unwrap();
        assert!(hits.contains("p.2:"), "{hits}");
        assert!(hits.contains("ratio"), "{hits}");

        let page = source_read(&root, "paper.pdf", Some("2"), None).unwrap();
        assert!(page.starts_with("page 2 of 3"), "{page}");
        assert!(page.contains("[p.2]"), "{page}");
        assert!(!page.contains("[p.1]"), "{page}");
        let range = source_read(&root, "paper.pdf", Some("2-"), None).unwrap();
        assert!(range.starts_with("pages 2-3 of 3"), "{range}");
        assert!(range.contains("no text layer"), "{range}");
        let pair = source_read(&root, "paper.pdf", Some("1-2"), None).unwrap();
        assert!(pair.starts_with("pages 1-2 of 3"), "{pair}");
        assert!(!pair.contains("[p.3]"), "{pair}");
        assert!(source_read(&root, "paper.pdf", Some("2-x"), None)
            .unwrap_err()
            .contains("bad page range"));
        // Both arguments are refused on the wrong kind of file.
        assert!(source_read(&root, "paper.pdf", None, Some("1-2"))
            .unwrap_err()
            .contains("by `pages`"));
        assert!(source_read(&root, "notes/week1/ohm.md", Some("1"), None)
            .unwrap_err()
            .contains("`pages` applies to PDFs"));
        assert!(source_read(&root, "paper.pdf", Some("9"), None)
            .unwrap_err()
            .contains("has 3 pages"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn finds_by_glob_or_name_and_outlines() {
        let root = temp_root();
        assert!(file_find(&root, "notes/**/*.md")
            .unwrap()
            .contains("~/notes/week1/ohm.md"));
        assert!(file_find(&root, "ohm").unwrap().contains("ohm.md"));
        assert!(file_find(&root, "../x").is_err());
        let outline = source_outline(&root, "notes/week1/ohm.md").unwrap();
        assert!(outline.contains("3: ## Dividers"));
        assert!(source_search(&root, "notes/week1/ohm.md", "ratio")
            .unwrap()
            .contains("4: ratio"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn glob_rules() {
        assert!(glob_match("*.md", "a.md"));
        assert!(!glob_match("*.md", "a/b.md"));
        assert!(glob_match("**/*.md", "a/b/c.md"));
        assert!(glob_match("uni/**/w?.txt", "uni/x/y/w1.txt"));
    }

    #[test]
    fn workspace_hits_are_relative_to_the_workspace_and_listed_once() {
        // Workspace inside home: each hit once, workspace-relative.
        let home = temp_root();
        let ws = home.join("notes");
        let found = file_find_roots(&[home.clone(), ws.clone()], Some(&ws), "ohm").unwrap();
        assert_eq!(found, "week1/ohm.md");
        let listed = mentions(&[home.clone(), ws.clone()], Some(&ws), "ohm", 50);
        let paths: Vec<&str> = listed.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, ["week1/ohm.md"]);
        // Home hits keep `~/`; a grant inside home isn't walked twice.
        let pdf = file_find_roots(
            &[home.clone(), ws.clone(), home.join("notes/week1")],
            Some(&ws),
            "paper",
        )
        .unwrap();
        assert_eq!(pdf, "~/paper.pdf");

        // Workspace outside home: no absolute workspace path in the output.
        let other = temp_root();
        let outside = other.join("notes");
        let found =
            file_find_roots(&[home.clone(), outside.clone()], Some(&outside), "*.md").unwrap();
        assert!(found.lines().any(|l| l == "week1/ohm.md"), "{found}");
        let canonical = outside.canonicalize().unwrap();
        assert!(!found.contains(&*canonical.to_string_lossy()), "{found}");
        let listed = mentions(
            &[home.clone(), outside.clone()],
            Some(&outside),
            "week1",
            50,
        );
        assert!(
            listed.iter().all(|m| !Path::new(&m.path).is_absolute()),
            "{listed:?}"
        );
        fs::remove_dir_all(home).unwrap();
        fs::remove_dir_all(other).unwrap();
    }
}
