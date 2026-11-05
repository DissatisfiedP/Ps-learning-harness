//! Read-only access to the learner's files (`file_find`, `file_read`, and the
//! researcher's `source_*` tools).
//!
//! Every path is resolved inside one root (the home directory): relative paths
//! are joined to it, `~/` is expanded, and the canonical path (symlinks
//! resolved) must still lie inside the canonical root, so a symlink can't
//! escape it. Reads are capped at 256 KiB. PDFs are refused with a clear
//! message because PDF reading isn't built yet.

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

pub const MAX_READ_BYTES: u64 = 256 * 1024;
const MAX_FIND_RESULTS: usize = 50;
const MAX_FIND_VISITS: usize = 20_000;
const MAX_FIND_DEPTH: usize = 8;
const MAX_SEARCH_HITS: usize = 50;
const DEFAULT_READ_LINES: usize = 200;

pub const PDF_NOT_BUILT: &str = "PDF reading isn't built yet; ask the learner for a text or Markdown copy, or work from other sources";

/// The learner's home directory, the root for every file tool.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// Resolve `path` inside `root`, refusing anything that escapes it.
pub fn resolve(root: &Path, path: &str) -> Result<PathBuf, String> {
    let path = path.trim();
    if path.is_empty() {
        return Err("path is empty".into());
    }
    let root = root
        .canonicalize()
        .map_err(|e| format!("home directory unavailable ({e})"))?;
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
    if !canonical.starts_with(&root) {
        return Err(format!("`{path}` is outside the home directory"));
    }
    Ok(canonical)
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

fn open_text(root: &Path, path: &str) -> Result<(PathBuf, fs::File), String> {
    let full = resolve(root, path)?;
    if full.is_dir() {
        return Err(format!("`{path}` is a directory; use file_find"));
    }
    if is_pdf(&full) {
        return Err(PDF_NOT_BUILT.into());
    }
    let mut file = fs::File::open(&full).map_err(|e| format!("can't open `{path}` ({e})"))?;
    let mut head = [0u8; 4096];
    let n = file
        .read(&mut head)
        .map_err(|e| format!("can't read `{path}` ({e})"))?;
    if head[..n].contains(&0) {
        return Err(format!("`{path}` is not a text file"));
    }
    let file = fs::File::open(&full).map_err(|e| format!("can't open `{path}` ({e})"))?;
    Ok((full, file))
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
    let (full, file) = open_text(root, path)?;
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
        out = format!("({} has no lines in that range)", full.display());
    }
    Ok(out)
}

/// `file_find { pattern }`: paths under the root (relative, `~/`-prefixed)
/// whose relative path matches the glob, or whose name contains the pattern
/// when it has no wildcard. Hidden directories and symlinks are skipped.
pub fn file_find(root: &Path, pattern: &str) -> Result<String, String> {
    let pattern = pattern.trim().trim_start_matches("~/");
    if pattern.is_empty() {
        return Err("pattern is empty".into());
    }
    if pattern.starts_with('/') || pattern.split('/').any(|c| c == "..") {
        return Err("the pattern must be relative to the home directory".into());
    }
    let root = root
        .canonicalize()
        .map_err(|e| format!("home directory unavailable ({e})"))?;
    let wildcard = pattern.contains(['*', '?']);
    let needle = pattern.to_lowercase();
    let mut hits = Vec::new();
    let mut visits = 0usize;
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.clone(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            visits += 1;
            if visits > MAX_FIND_VISITS || hits.len() >= MAX_FIND_RESULTS {
                break;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            let path = entry.path();
            let rel = path
                .strip_prefix(&root)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| name.clone());
            if kind.is_dir() {
                if !name.starts_with('.') && depth < MAX_FIND_DEPTH {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            let matched = if wildcard {
                if pattern.contains('/') {
                    glob_match(pattern, &rel)
                } else {
                    glob_match(pattern, &name)
                }
            } else {
                rel.to_lowercase().contains(&needle)
            };
            if matched {
                hits.push(format!("~/{rel}"));
            }
        }
    }
    hits.sort();
    if hits.is_empty() {
        Ok(format!("no files match `{pattern}`"))
    } else {
        Ok(hits.join("\n"))
    }
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

/// `source_outline { path }`: Markdown headings with line numbers.
pub fn source_outline(root: &Path, path: &str) -> Result<String, String> {
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

/// `source_search { path, query }`: case-insensitive line matches.
pub fn source_search(root: &Path, path: &str, query: &str) -> Result<String, String> {
    let (_, file) = open_text(root, path)?;
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Err("query is empty".into());
    }
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

/// `source_read { path, pages | lines }`.
pub fn source_read(
    root: &Path,
    path: &str,
    pages: Option<&str>,
    lines: Option<&str>,
) -> Result<String, String> {
    let full = resolve(root, path)?;
    if is_pdf(&full) {
        return Err(PDF_NOT_BUILT.into());
    }
    if pages.is_some() && lines.is_none() {
        return Err("`pages` applies to PDFs, which aren't readable yet; use `lines`".into());
    }
    let spec = lines.map_or_else(|| format!("1-{DEFAULT_READ_LINES}"), str::to_owned);
    let (start, end) = parse_range(&spec)?;
    let (_, file) = open_text(root, path)?;
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
        fs::write(dir.join("paper.pdf"), "%PDF-1.4").unwrap();
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
        assert_eq!(
            file_read(&root, "paper.pdf", None).unwrap_err(),
            PDF_NOT_BUILT
        );
        assert_eq!(
            source_read(&root, "paper.pdf", None, None).unwrap_err(),
            PDF_NOT_BUILT
        );
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
}
