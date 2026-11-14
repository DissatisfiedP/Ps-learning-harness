//! Text out of the zipped-XML and RTF formats a reference library imports
//! (architecture §4.27): Word, PowerPoint and Excel (`.docx`, `.pptx`,
//! `.xlsx`), OpenDocument (`.odt`, `.ods`, `.odp`), EPUB and RTF.
//!
//! The zipped formats are read with `zip` (stored and deflate entries, which
//! is what every Office, LibreOffice and EPUB writer emits) and `quick-xml`;
//! RTF is a small reader of its own whose `\'hh` bytes are decoded with
//! `encoding_rs` in the document's code page. Every part is capped
//! ([`MAX_PART_BYTES`], and [`MAX_UNPACKED_BYTES`] for one file), so a zip
//! bomb is refused rather than unpacked; text stops being collected a little
//! past [`super::MAX_TEXT_BYTES`], so the caller's cap still cuts it and
//! marks the file `truncated`.
//!
//! The text: one line per paragraph, `#` headings where the document marks
//! them, a table row as its cells joined by ` | `, a spreadsheet's sheets
//! under `## <sheet>`, and slides tagged `[p.N]` like PDF pages.

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{Cursor, Read};

use quick_xml::events::{BytesRef, BytesStart, Event};
use quick_xml::Reader;
use zip::result::ZipError;
use zip::ZipArchive;

use super::Format;

/// Most bytes one part of an archive may unpack to.
const MAX_PART_BYTES: u64 = 64 * 1024 * 1024;
/// Most bytes one file's parts may unpack to together.
const MAX_UNPACKED_BYTES: u64 = 256 * 1024 * 1024;
/// Most entries an archive may list.
const MAX_ENTRIES: usize = 20_000;
/// Text collected before extraction stops: past the repo cap, so the cap
/// cuts it (at a line break, with its note) as it does every other format.
const TEXT_BUDGET: usize = super::MAX_TEXT_BYTES + 64 * 1024;
/// Most cells written for one spreadsheet or table row.
const MAX_ROW_CELLS: usize = 256;

/// One document's text.
#[derive(Debug)]
pub(super) struct Doc {
    pub text: String,
    /// The title the document's metadata declares, if any.
    pub title: Option<String>,
    /// Slides, for a presentation.
    pub pages: Option<u32>,
}

/// Extract `bytes` as `format`. `Err` is a sentence saying why the file
/// can't be read (it becomes the index note of a `failed` file).
pub(super) fn extract(format: Format, bytes: &[u8]) -> Result<Doc, String> {
    match format {
        Format::Docx => docx(bytes),
        Format::Pptx => pptx(bytes),
        Format::Xlsx => xlsx(bytes),
        Format::Odt | Format::Ods | Format::Odp => odf(format, bytes),
        Format::Epub => epub(bytes),
        Format::Rtf => rtf(bytes),
        other => Err(format!("`{}` is not an office format", other.as_str())),
    }
}

// ---------------------------------------------------------------------------
// Zip archives
// ---------------------------------------------------------------------------

struct Zip<'a> {
    archive: ZipArchive<Cursor<&'a [u8]>>,
    unpacked: u64,
}

impl<'a> Zip<'a> {
    fn open(bytes: &'a [u8]) -> Result<Self, String> {
        let archive = ZipArchive::new(Cursor::new(bytes)).map_err(|e| {
            format!("this file isn't a readable zip archive, as its format needs ({e})")
        })?;
        if archive.len() > MAX_ENTRIES {
            return Err(format!(
                "this archive lists {} entries, over the {MAX_ENTRIES}-entry cap",
                archive.len()
            ));
        }
        Ok(Zip {
            archive,
            unpacked: 0,
        })
    }

    fn names(&self) -> Vec<String> {
        self.archive.file_names().map(str::to_owned).collect()
    }

    /// A part's text (UTF-8, a BOM dropped); `None` when there is no such
    /// part.
    fn part(&mut self, name: &str) -> Result<Option<String>, String> {
        let limit = MAX_PART_BYTES.min(MAX_UNPACKED_BYTES.saturating_sub(self.unpacked));
        let too_big = || {
            format!(
                "`{name}` unpacks past the {} MiB cap for one file",
                limit / (1024 * 1024)
            )
        };
        let buf = {
            let mut entry = match self.archive.by_name(name) {
                Ok(e) => e,
                Err(ZipError::FileNotFound) => return Ok(None),
                Err(e) => return Err(format!("`{name}` can't be unpacked ({e})")),
            };
            if entry.size() > limit {
                return Err(too_big());
            }
            let mut buf = Vec::with_capacity(entry.size() as usize);
            entry
                .by_ref()
                .take(limit + 1)
                .read_to_end(&mut buf)
                .map_err(|e| format!("`{name}` can't be unpacked ({e})"))?;
            buf
        };
        if buf.len() as u64 > limit {
            return Err(too_big());
        }
        self.unpacked += buf.len() as u64;
        let bytes = buf.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&buf);
        Ok(Some(String::from_utf8_lossy(bytes).into_owned()))
    }
}

/// `a/b/c.xml` → `a/b`.
fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// `%XX` escapes decoded (EPUB hrefs and some relationship targets carry
/// them); a malformed escape is kept as it is.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A part name for `target` as seen from the folder `base`: a leading `/`
/// starts at the archive root, `..` climbs, a `#fragment` is dropped.
fn join(base: &str, target: &str) -> String {
    let target = target.split('#').next().unwrap_or("");
    let target = percent_decode(target);
    let (mut parts, rest): (Vec<&str>, &str) = match target.strip_prefix('/') {
        Some(abs) => (Vec::new(), abs),
        None => (base.split('/').filter(|s| !s.is_empty()).collect(), &target),
    };
    for seg in rest.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// `ppt/presentation.xml` → `ppt/_rels/presentation.xml.rels`.
fn rels_of(part: &str) -> String {
    match part.rsplit_once('/') {
        Some((dir, file)) => format!("{dir}/_rels/{file}.rels"),
        None => format!("_rels/{part}.rels"),
    }
}

struct Rel {
    id: String,
    kind: String,
    target: String,
}

/// The internal relationships of `part`, targets resolved to part names.
fn rels(zip: &mut Zip, part: &str) -> Result<Vec<Rel>, String> {
    let Some(xml) = zip.part(&rels_of(part))? else {
        return Ok(Vec::new());
    };
    let base = dir_of(part);
    let mut out = Vec::new();
    // A broken relationships part leaves the document's defaults.
    let _ = walk(&xml, |n| {
        if let Node::Open(e) = n {
            if local(e) == "Relationship" && attr(e, "TargetMode").as_deref() != Some("External") {
                out.push(Rel {
                    id: attr(e, "Id").unwrap_or_default(),
                    kind: attr(e, "Type").unwrap_or_default(),
                    target: join(base, &attr(e, "Target").unwrap_or_default()),
                });
            }
        }
        true
    });
    Ok(out)
}

/// The package's main part (`officeDocument` relationship), else `default`.
fn main_part(zip: &mut Zip, default: &str) -> Result<String, String> {
    Ok(rels(zip, "")?
        .into_iter()
        .find(|r| r.kind.ends_with("/officeDocument"))
        .map_or_else(|| default.to_owned(), |r| r.target))
}

// ---------------------------------------------------------------------------
// XML
// ---------------------------------------------------------------------------

enum Node<'a, 'b> {
    Open(&'a BytesStart<'b>),
    Close(&'a str),
    Text(&'a str),
}

fn local<'a>(e: &'a BytesStart) -> &'a str {
    e.local_name().into_inner()
}

/// An attribute by local name, whatever its prefix.
fn attr(e: &BytesStart, name: &str) -> Option<String> {
    find_attr(e, name, false)
}

/// An attribute by local name that has a namespace prefix (`r:id`, as
/// opposed to a plain `id` on the same element).
fn attr_prefixed(e: &BytesStart, name: &str) -> Option<String> {
    find_attr(e, name, true)
}

fn find_attr(e: &BytesStart, name: &str, prefixed: bool) -> Option<String> {
    e.attributes()
        .with_checks(false)
        .flatten()
        .find(|a| {
            a.key.local_name().into_inner() == name && (!prefixed || a.key.prefix().is_some())
        })
        .and_then(|a| {
            a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .ok()
                .map(Cow::into_owned)
        })
}

/// The five XML entities and character references; anything else is
/// dropped.
fn entity(r: &BytesRef) -> Option<String> {
    if let Ok(Some(c)) = r.resolve_char_ref() {
        return Some(c.to_string());
    }
    let c = match &*r.borrow().into_inner() {
        "lt" => '<',
        "gt" => '>',
        "amp" => '&',
        "quot" => '"',
        "apos" => '\'',
        _ => return None,
    };
    Some(c.to_string())
}

/// Walk `xml`, calling `visit` per element open (an empty element is an
/// open and a close), close and text run, until it returns false. A
/// malformed document stops the walk with an `Err`; what was visited
/// before stays visited.
fn walk(xml: &str, mut visit: impl FnMut(Node<'_, '_>) -> bool) -> Result<(), String> {
    let mut reader = Reader::from_str(xml);
    let config = reader.config_mut();
    config.expand_empty_elements = true;
    config.check_end_names = false;
    loop {
        let go = match reader.read_event() {
            Ok(Event::Start(e)) => visit(Node::Open(&e)),
            Ok(Event::End(e)) => visit(Node::Close(e.local_name().into_inner())),
            Ok(Event::Text(t)) => visit(Node::Text(&t.xml10_content())),
            Ok(Event::CData(t)) => visit(Node::Text(&t.into_inner())),
            Ok(Event::GeneralRef(r)) => match entity(&r) {
                Some(s) => visit(Node::Text(&s)),
                None => true,
            },
            Ok(Event::Eof) => return Ok(()),
            Ok(_) => true,
            Err(e) => return Err(format!("its XML is malformed ({e})")),
        };
        if !go {
            return Ok(());
        }
    }
}

/// The text of the first `<…:title>` element (`dc:title` in Office core
/// properties, OpenDocument meta and an EPUB package).
fn first_title(xml: &str) -> Option<String> {
    let mut depth = 0usize;
    let mut title = String::new();
    let mut done = false;
    let _ = walk(xml, |n| {
        match n {
            Node::Open(e) if local(e) == "title" => depth += 1,
            Node::Close("title") if depth > 0 => {
                depth -= 1;
                done = depth == 0;
            }
            Node::Text(s) if depth > 0 => title.push_str(s),
            _ => {}
        }
        !done
    });
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    (!title.is_empty()).then_some(title)
}

fn part_title(zip: &mut Zip, part: &str) -> Option<String> {
    zip.part(part).ok().flatten().and_then(|x| first_title(&x))
}

/// Keep what a malformed part yielded before it broke; refuse it when it
/// yielded nothing.
fn tolerate(walked: Result<(), String>, b: &Builder, what: &str) -> Result<(), String> {
    match walked {
        Err(e) if b.is_empty() => Err(format!("{what}: {e}")),
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Building the text: paragraphs, table rows and cells, slides
// ---------------------------------------------------------------------------

enum Frame {
    /// A paragraph's text and heading level (0: body).
    Para(String, usize),
    Row(Vec<String>),
    Cell(String),
}

#[derive(Default)]
struct Builder {
    out: String,
    frames: Vec<Frame>,
    /// `[p.N] ` waiting for its slide's first line.
    page: Option<String>,
}

impl Builder {
    fn full(&self) -> bool {
        self.out.len() > TEXT_BUDGET
    }

    fn is_empty(&self) -> bool {
        self.out.trim().is_empty()
    }

    fn text(&mut self, s: &str) {
        if let Some(Frame::Para(t, _) | Frame::Cell(t)) = self.frames.last_mut() {
            t.push_str(s);
        }
    }

    fn open_para(&mut self, level: usize) {
        self.frames.push(Frame::Para(String::new(), level));
    }

    fn set_level(&mut self, level: usize) {
        if let Some(Frame::Para(_, l)) = self.frames.last_mut() {
            *l = level;
        }
    }

    fn close_para(&mut self) {
        if let Some(Frame::Para(..)) = self.frames.last() {
            if let Some(Frame::Para(text, level)) = self.frames.pop() {
                self.deliver(&text, level);
            }
        }
    }

    fn open_row(&mut self) {
        self.frames.push(Frame::Row(Vec::new()));
    }

    fn close_row(&mut self) {
        if let Some(Frame::Row(_)) = self.frames.last() {
            if let Some(Frame::Row(cells)) = self.frames.pop() {
                let line = row_line(cells);
                self.deliver(&line, 0);
            }
        }
    }

    fn open_cell(&mut self) {
        self.frames.push(Frame::Cell(String::new()));
    }

    /// Close a cell that stands for `repeat` equal cells (OpenDocument
    /// spreadsheets repeat runs of cells).
    fn close_cell(&mut self, repeat: usize) {
        if let Some(Frame::Cell(_)) = self.frames.last() {
            if let Some(Frame::Cell(text)) = self.frames.pop() {
                let text = one_line(&text);
                match self.frames.last_mut() {
                    Some(Frame::Row(cells)) => {
                        let room = MAX_ROW_CELLS.saturating_sub(cells.len());
                        let n = if text.is_empty() {
                            repeat.min(room)
                        } else {
                            repeat.clamp(1, 16).min(room)
                        };
                        cells.extend(std::iter::repeat_n(text, n));
                    }
                    _ => self.deliver(&text, 0),
                }
            }
        }
    }

    /// A finished paragraph or row goes into the frame around it (as one
    /// line), or onto the page.
    fn deliver(&mut self, text: &str, level: usize) {
        if text.trim().is_empty() {
            return;
        }
        match self.frames.last_mut() {
            Some(Frame::Para(t, _) | Frame::Cell(t)) => {
                if !t.is_empty() {
                    t.push(' ');
                }
                t.push_str(&one_line(text));
            }
            Some(Frame::Row(cells)) => {
                if cells.len() < MAX_ROW_CELLS {
                    cells.push(one_line(text));
                }
            }
            None => self.line(text, level),
        }
    }

    /// Lines onto the page: a heading's first line gets its `#`s, a slide's
    /// first line its `[p.N]` tag.
    fn line(&mut self, text: &str, level: usize) {
        let lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
        for (i, l) in lines.enumerate() {
            if let Some(tag) = self.page.take() {
                self.out.push_str(&tag);
            }
            if i == 0 && level > 0 {
                self.out.push_str(&"#".repeat(level.min(6)));
                self.out.push(' ');
            }
            self.out.push_str(l);
            self.out.push('\n');
        }
    }

    /// Slide `n` starts: a slide before it that had no text says so.
    fn page(&mut self, n: usize) {
        self.flush_page();
        self.page = Some(format!("[p.{n}] "));
    }

    fn flush_page(&mut self) {
        if let Some(tag) = self.page.take() {
            self.out.push_str(tag.trim_end());
            self.out.push_str(" (no text on this slide)\n");
        }
    }

    fn finish(mut self) -> String {
        self.flush_page();
        self.out
    }
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A row's cells joined by ` | `, trailing empty cells dropped; empty when
/// every cell is.
fn row_line(mut cells: Vec<String>) -> String {
    while cells.last().is_some_and(|c| c.trim().is_empty()) {
        cells.pop();
    }
    cells.join(" | ")
}

// ---------------------------------------------------------------------------
// Office Open XML: Word, PowerPoint, Excel
// ---------------------------------------------------------------------------

/// `Heading2` → 2, `Title` → 1 (style ids as Word writes them).
fn heading_level(style: &str) -> Option<usize> {
    let s = style.to_ascii_lowercase();
    if s == "title" {
        return Some(1);
    }
    let n: usize = s.strip_prefix("heading")?.trim().parse().ok()?;
    (1..=9).contains(&n).then_some(n)
}

/// The text of a WordprocessingML or DrawingML part: paragraphs (`p`),
/// text runs (`t`; deleted text and field codes are other elements and are
/// left out), tabs, breaks and table rows and cells.
fn ooxml_text(xml: &str, b: &mut Builder) -> Result<(), String> {
    let mut in_t = 0usize;
    // Inside tab-stop lists, `tab` is a stop, not a tab.
    let mut in_tabs = 0usize;
    walk(xml, |n| {
        match n {
            Node::Open(e) => match local(e) {
                "p" => b.open_para(0),
                "t" => in_t += 1,
                "tabs" | "tabLst" => in_tabs += 1,
                "tab" if in_tabs == 0 => b.text("\t"),
                "br" | "cr" => b.text("\n"),
                "tr" => b.open_row(),
                "tc" => b.open_cell(),
                "pStyle" => {
                    if let Some(level) = attr(e, "val").as_deref().and_then(heading_level) {
                        b.set_level(level);
                    }
                }
                "outlineLvl" => {
                    if let Some(n) = attr(e, "val").and_then(|v| v.parse::<usize>().ok()) {
                        if n < 9 {
                            b.set_level(n + 1);
                        }
                    }
                }
                _ => {}
            },
            Node::Close(name) => match name {
                "p" => b.close_para(),
                "t" => in_t = in_t.saturating_sub(1),
                "tabs" | "tabLst" => in_tabs = in_tabs.saturating_sub(1),
                "tr" => b.close_row(),
                "tc" => b.close_cell(1),
                _ => {}
            },
            Node::Text(s) => {
                if in_t > 0 {
                    b.text(s);
                }
            }
        }
        !b.full()
    })
}

fn docx(bytes: &[u8]) -> Result<Doc, String> {
    let mut zip = Zip::open(bytes)?;
    let main = main_part(&mut zip, "word/document.xml")?;
    let xml = zip
        .part(&main)?
        .ok_or("this isn't a Word document: it has no `word/document.xml`")?;
    let mut b = Builder::default();
    tolerate(ooxml_text(&xml, &mut b), &b, "the document can't be read")?;
    Ok(Doc {
        title: part_title(&mut zip, "docProps/core.xml"),
        text: b.finish(),
        pages: None,
    })
}

/// A presentation's slide parts in show order: the `sldId` list and its
/// relationships, else `slideN.xml` by number.
fn slide_parts(zip: &mut Zip, main: &str) -> Result<Vec<String>, String> {
    let mut ids = Vec::new();
    if let Some(xml) = zip.part(main)? {
        let _ = walk(&xml, |n| {
            if let Node::Open(e) = n {
                if local(e) == "sldId" {
                    ids.extend(attr_prefixed(e, "id"));
                }
            }
            true
        });
    }
    let rels = rels(zip, main)?;
    let mut parts: Vec<String> = ids
        .iter()
        .filter_map(|id| rels.iter().find(|r| &r.id == id))
        .map(|r| r.target.clone())
        .collect();
    if parts.is_empty() {
        let mut numbered: Vec<(u32, String)> = zip
            .names()
            .into_iter()
            .filter_map(|n| {
                let num = n
                    .strip_prefix("ppt/slides/slide")?
                    .strip_suffix(".xml")?
                    .parse()
                    .ok()?;
                Some((num, n))
            })
            .collect();
        numbered.sort();
        parts = numbered.into_iter().map(|(_, n)| n).collect();
    }
    Ok(parts)
}

fn pptx(bytes: &[u8]) -> Result<Doc, String> {
    let mut zip = Zip::open(bytes)?;
    let main = main_part(&mut zip, "ppt/presentation.xml")?;
    let slides = slide_parts(&mut zip, &main)?;
    if slides.is_empty() {
        return Err("this isn't a PowerPoint presentation: it has no slides".into());
    }
    let mut b = Builder::default();
    let mut broken = 0;
    for (i, part) in slides.iter().enumerate() {
        if b.full() {
            break;
        }
        b.page(i + 1);
        match zip.part(part)? {
            Some(xml) => {
                if ooxml_text(&xml, &mut b).is_err() {
                    broken += 1;
                }
            }
            None => broken += 1,
        }
    }
    if broken == slides.len() {
        return Err("none of this presentation's slides can be read".into());
    }
    Ok(Doc {
        title: part_title(&mut zip, "docProps/core.xml"),
        text: b.finish(),
        pages: Some(slides.len() as u32),
    })
}

/// The shared strings of a workbook (`si` items; phonetic runs left out).
fn shared_strings(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    let (mut in_t, mut in_rph) = (0usize, 0usize);
    let _ = walk(xml, |n| {
        match n {
            Node::Open(e) => match local(e) {
                "si" => cur = Some(String::new()),
                "t" => in_t += 1,
                "rPh" => in_rph += 1,
                _ => {}
            },
            Node::Close(name) => match name {
                "si" => out.extend(cur.take()),
                "t" => in_t = in_t.saturating_sub(1),
                "rPh" => in_rph = in_rph.saturating_sub(1),
                _ => {}
            },
            Node::Text(s) => {
                if in_t > 0 && in_rph == 0 {
                    if let Some(c) = cur.as_mut() {
                        c.push_str(s);
                    }
                }
            }
        }
        true
    });
    out
}

/// `C12` → 2 (0-based column).
fn column_of(cell_ref: &str) -> Option<usize> {
    let letters: Vec<u8> = cell_ref
        .bytes()
        .take_while(u8::is_ascii_alphabetic)
        .map(|b| b.to_ascii_uppercase())
        .collect();
    if letters.is_empty() || letters.len() > 3 {
        return None;
    }
    let n = letters
        .iter()
        .fold(0usize, |n, b| n * 26 + (b - b'A' + 1) as usize);
    Some(n - 1)
}

struct Cell {
    kind: String,
    column: Option<usize>,
    value: String,
}

/// One worksheet's rows: values (`v`) and inline strings (`is`), shared
/// strings looked up, booleans spelled out, gaps between cells kept.
fn sheet_rows(xml: &str, shared: &[String], b: &mut Builder) -> Result<(), String> {
    let mut row: Option<Vec<String>> = None;
    let mut cell: Option<Cell> = None;
    let (mut in_v, mut in_is, mut in_rph) = (0usize, 0usize, 0usize);
    walk(xml, |n| {
        match n {
            Node::Open(e) => match local(e) {
                "row" => row = Some(Vec::new()),
                "c" => {
                    cell = Some(Cell {
                        kind: attr(e, "t").unwrap_or_default(),
                        column: attr(e, "r").as_deref().and_then(column_of),
                        value: String::new(),
                    })
                }
                "v" => in_v += 1,
                "is" => in_is += 1,
                "rPh" => in_rph += 1,
                _ => {}
            },
            Node::Close(name) => match name {
                "v" => in_v = in_v.saturating_sub(1),
                "is" => in_is = in_is.saturating_sub(1),
                "rPh" => in_rph = in_rph.saturating_sub(1),
                "c" => {
                    if let (Some(c), Some(r)) = (cell.take(), row.as_mut()) {
                        let raw = c.value.trim();
                        let value = match c.kind.as_str() {
                            "s" => raw
                                .parse::<usize>()
                                .ok()
                                .and_then(|i| shared.get(i))
                                .cloned()
                                .unwrap_or_default(),
                            "b" => if raw == "1" { "TRUE" } else { "FALSE" }.to_owned(),
                            _ => raw.to_owned(),
                        };
                        if let Some(col) = c.column {
                            while r.len() < col.min(MAX_ROW_CELLS) {
                                r.push(String::new());
                            }
                        }
                        if r.len() < MAX_ROW_CELLS {
                            r.push(one_line(&value));
                        }
                    }
                }
                "row" => {
                    if let Some(r) = row.take() {
                        let line = row_line(r);
                        b.line(&line, 0);
                    }
                }
                _ => {}
            },
            Node::Text(s) => {
                if let Some(c) = cell.as_mut() {
                    if in_v > 0 || (in_is > 0 && in_rph == 0) {
                        c.value.push_str(s);
                    }
                }
            }
        }
        !b.full()
    })
}

fn xlsx(bytes: &[u8]) -> Result<Doc, String> {
    let mut zip = Zip::open(bytes)?;
    let main = main_part(&mut zip, "xl/workbook.xml")?;
    let workbook = zip
        .part(&main)?
        .ok_or("this isn't an Excel workbook: it has no `xl/workbook.xml`")?;
    let mut sheets: Vec<(String, String)> = Vec::new();
    walk(&workbook, |n| {
        if let Node::Open(e) = n {
            if local(e) == "sheet" {
                sheets.push((
                    attr(e, "name").unwrap_or_default(),
                    attr_prefixed(e, "id").unwrap_or_default(),
                ));
            }
        }
        true
    })
    .map_err(|e| format!("the workbook can't be read: {e}"))?;
    let rels = rels(&mut zip, &main)?;
    let strings_part = rels
        .iter()
        .find(|r| r.kind.ends_with("/sharedStrings"))
        .map_or_else(|| "xl/sharedStrings.xml".to_owned(), |r| r.target.clone());
    let shared = zip
        .part(&strings_part)?
        .map(|x| shared_strings(&x))
        .unwrap_or_default();
    let mut b = Builder::default();
    for (name, id) in &sheets {
        if b.full() {
            break;
        }
        let Some(rel) = rels.iter().find(|r| &r.id == id) else {
            continue;
        };
        let Some(xml) = zip.part(&rel.target)? else {
            continue;
        };
        b.line(if name.is_empty() { "Sheet" } else { name }, 2);
        // A sheet that breaks keeps the rows read before it broke.
        let _ = sheet_rows(&xml, &shared, &mut b);
    }
    Ok(Doc {
        title: part_title(&mut zip, "docProps/core.xml"),
        text: b.finish(),
        pages: None,
    })
}

// ---------------------------------------------------------------------------
// OpenDocument: text, spreadsheet, presentation
// ---------------------------------------------------------------------------

/// Runs of white space as one space: OpenDocument collapses them (extra
/// spaces are `text:s` elements).
fn collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !space {
                out.push(' ');
            }
            space = true;
        } else {
            out.push(c);
            space = false;
        }
    }
    out
}

fn odf(format: Format, bytes: &[u8]) -> Result<Doc, String> {
    let mut zip = Zip::open(bytes)?;
    let xml = zip
        .part("content.xml")?
        .ok_or("this isn't an OpenDocument file: it has no `content.xml`")?;
    let mut b = Builder::default();
    let mut slides = 0usize;
    // Comments, tracked deletions and speaker notes are left out.
    let mut skip = 0usize;
    let mut repeats: Vec<usize> = Vec::new();
    let walked = walk(&xml, |n| {
        if skip > 0 {
            match n {
                Node::Open(_) => skip += 1,
                Node::Close(_) => skip -= 1,
                Node::Text(_) => {}
            }
            return true;
        }
        match n {
            Node::Open(e) => match local(e) {
                "annotation" | "tracked-changes" | "notes" => skip = 1,
                "p" => b.open_para(0),
                "h" => b.open_para(
                    attr(e, "outline-level")
                        .and_then(|l| l.parse::<usize>().ok())
                        .unwrap_or(1)
                        .clamp(1, 6),
                ),
                "s" => {
                    let n = attr(e, "c")
                        .and_then(|c| c.parse::<usize>().ok())
                        .unwrap_or(1);
                    b.text(&" ".repeat(n.clamp(1, 64)));
                }
                "tab" => b.text("\t"),
                "line-break" => b.text("\n"),
                "table" if format == Format::Ods => {
                    if let Some(name) = attr(e, "name") {
                        b.line(&name, 2);
                    }
                }
                "table-row" => b.open_row(),
                "table-cell" | "covered-table-cell" => {
                    repeats.push(
                        attr(e, "number-columns-repeated")
                            .and_then(|r| r.parse().ok())
                            .unwrap_or(1),
                    );
                    b.open_cell();
                }
                "page" if format == Format::Odp => {
                    slides += 1;
                    b.page(slides);
                }
                _ => {}
            },
            Node::Close(name) => match name {
                "p" | "h" => b.close_para(),
                "table-row" => b.close_row(),
                "table-cell" | "covered-table-cell" => b.close_cell(repeats.pop().unwrap_or(1)),
                _ => {}
            },
            Node::Text(s) => b.text(&collapse(s)),
        }
        !b.full()
    });
    tolerate(walked, &b, "the document can't be read")?;
    Ok(Doc {
        title: part_title(&mut zip, "meta.xml"),
        text: b.finish(),
        pages: (format == Format::Odp).then_some(slides as u32),
    })
}

// ---------------------------------------------------------------------------
// EPUB
// ---------------------------------------------------------------------------

fn is_html_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".xhtml", ".html", ".htm"]
        .iter()
        .any(|e| lower.ends_with(e))
}

fn epub(bytes: &[u8]) -> Result<Doc, String> {
    let mut zip = Zip::open(bytes)?;
    let mut package = None;
    if let Some(container) = zip.part("META-INF/container.xml")? {
        let _ = walk(&container, |n| {
            if let Node::Open(e) = n {
                if local(e) == "rootfile" {
                    package = attr(e, "full-path");
                }
            }
            package.is_none()
        });
    }
    let mut chapters: Vec<String> = Vec::new();
    let mut title = None;
    if let Some(opf_path) = package {
        if let Some(opf) = zip.part(&opf_path)? {
            title = first_title(&opf);
            let base = dir_of(&opf_path);
            let mut manifest: HashMap<String, (String, String)> = HashMap::new();
            let mut spine: Vec<String> = Vec::new();
            let _ = walk(&opf, |n| {
                if let Node::Open(e) = n {
                    match local(e) {
                        "item" => {
                            if let (Some(id), Some(href)) = (attr(e, "id"), attr(e, "href")) {
                                manifest
                                    .insert(id, (href, attr(e, "media-type").unwrap_or_default()));
                            }
                        }
                        "itemref" => spine.extend(attr(e, "idref")),
                        _ => {}
                    }
                }
                true
            });
            for id in &spine {
                if let Some((href, media)) = manifest.get(id) {
                    let part = join(base, href);
                    if (media.contains("html") || is_html_name(href)) && !chapters.contains(&part) {
                        chapters.push(part);
                    }
                }
            }
        }
    }
    if chapters.is_empty() {
        chapters = zip
            .names()
            .into_iter()
            .filter(|n| is_html_name(n))
            .collect();
        chapters.sort();
    }
    if chapters.is_empty() {
        return Err("this isn't an EPUB book: it has no chapters".into());
    }
    let mut text = String::new();
    for part in &chapters {
        if text.len() > TEXT_BUDGET {
            break;
        }
        if let Some(html) = zip.part(part)? {
            let chapter = crate::web::html_to_text(&html);
            let chapter = chapter.trim();
            if !chapter.is_empty() {
                text.push_str(chapter);
                text.push_str("\n\n");
            }
        }
    }
    Ok(Doc {
        text,
        title,
        pages: None,
    })
}

// ---------------------------------------------------------------------------
// RTF
// ---------------------------------------------------------------------------

/// Destinations whose text is not the document's: tables of fonts, colours
/// and styles, metadata, pictures, embedded objects, field instructions,
/// headers and footers.
const RTF_SKIP: &[&str] = &[
    "fonttbl",
    "colortbl",
    "stylesheet",
    "info",
    "pict",
    "object",
    "objdata",
    "datastore",
    "themedata",
    "colorschememapping",
    "latentstyles",
    "listtable",
    "listoverridetable",
    "revtbl",
    "rsidtbl",
    "generator",
    "xmlnstbl",
    "fldinst",
    "filetbl",
    "header",
    "headerl",
    "headerr",
    "headerf",
    "footer",
    "footerl",
    "footerr",
    "footerf",
    "bkmkstart",
    "bkmkend",
    "mmathPr",
    "pgdsctbl",
];

/// The encoding of `\ansicpgN`.
fn code_page(n: i32) -> &'static encoding_rs::Encoding {
    use encoding_rs::*;
    match n {
        65001 => UTF_8,
        936 => GBK,
        54936 => GB18030,
        950 => BIG5,
        932 => SHIFT_JIS,
        949 => EUC_KR,
        874 => WINDOWS_874,
        866 => IBM866,
        10000 => MACINTOSH,
        1250..=1258 => {
            Encoding::for_label(format!("windows-{n}").as_bytes()).unwrap_or(WINDOWS_1252)
        }
        _ => WINDOWS_1252,
    }
}

#[derive(Clone, Copy)]
struct RtfGroup {
    skip: bool,
    /// Fallback characters after `\uN` (`\ucN`).
    uc: usize,
}

struct Rtf {
    out: String,
    /// Code-page bytes waiting to be decoded together (a multi-byte
    /// character arrives as several `\'hh`).
    bytes: Vec<u8>,
    encoding: &'static encoding_rs::Encoding,
    group: RtfGroup,
    stack: Vec<RtfGroup>,
    /// Fallback characters still to drop after a `\uN`.
    fallback: usize,
    /// The high half of a UTF-16 surrogate pair from `\uN`.
    high: Option<u32>,
}

impl Rtf {
    fn flush(&mut self) {
        if !self.bytes.is_empty() {
            let (s, _) = self.encoding.decode_without_bom_handling(&self.bytes);
            self.out.push_str(&s);
            self.bytes.clear();
        }
    }

    fn push(&mut self, s: &str) {
        if !self.group.skip {
            self.flush();
            self.out.push_str(s);
        }
    }

    fn byte(&mut self, b: u8) {
        if self.fallback > 0 {
            self.fallback -= 1;
        } else if !self.group.skip {
            if b < 0x80 {
                self.flush();
                self.out.push(b as char);
            } else {
                self.bytes.push(b);
            }
        }
    }

    fn unicode(&mut self, n: i32) {
        let code = if n < 0 { n + 65536 } else { n } as u32;
        self.fallback = self.group.uc;
        if (0xD800..0xDC00).contains(&code) {
            self.high = Some(code);
            return;
        }
        let code = match self.high.take() {
            Some(high) if (0xDC00..0xE000).contains(&code) => {
                0x10000 + ((high - 0xD800) << 10) + (code - 0xDC00)
            }
            _ => code,
        };
        if let Some(c) = char::from_u32(code) {
            self.push(&c.to_string());
        }
    }

    fn word(&mut self, word: &str, param: Option<i32>) {
        if self.fallback > 0 {
            self.fallback -= 1;
            return;
        }
        match word {
            w if RTF_SKIP.contains(&w) => self.group.skip = true,
            "par" | "line" | "sect" | "page" | "row" => self.push("\n"),
            "cell" | "nestcell" => self.push(" | "),
            "tab" => self.push("\t"),
            "emdash" => self.push("—"),
            "endash" => self.push("–"),
            "bullet" => self.push("•"),
            "lquote" => self.push("‘"),
            "rquote" => self.push("’"),
            "ldblquote" => self.push("“"),
            "rdblquote" => self.push("”"),
            "emspace" | "enspace" | "qmspace" => self.push(" "),
            "u" => {
                if let Some(n) = param {
                    self.unicode(n);
                }
            }
            "uc" => self.group.uc = param.unwrap_or(1).clamp(0, 8) as usize,
            "ansicpg" => {
                self.flush();
                self.encoding = code_page(param.unwrap_or(1252));
            }
            _ => {}
        }
    }

    fn run(&mut self, src: &[u8]) {
        let mut i = 0;
        while i < src.len() && self.out.len() <= TEXT_BUDGET {
            match src[i] {
                b'{' => {
                    self.stack.push(self.group);
                    i += 1;
                }
                b'}' => {
                    self.flush();
                    self.fallback = 0;
                    if let Some(g) = self.stack.pop() {
                        self.group = g;
                    }
                    i += 1;
                }
                b'\r' | b'\n' => i += 1,
                b'\\' => {
                    let Some(&next) = src.get(i + 1) else { break };
                    if next.is_ascii_alphabetic() {
                        let start = i + 1;
                        let mut j = start;
                        while j < src.len() && src[j].is_ascii_alphabetic() && j - start < 32 {
                            j += 1;
                        }
                        let word = String::from_utf8_lossy(&src[start..j]).into_owned();
                        let num_start = j;
                        if j < src.len() && src[j] == b'-' {
                            j += 1;
                        }
                        while j < src.len() && src[j].is_ascii_digit() && j - num_start < 11 {
                            j += 1;
                        }
                        let param = std::str::from_utf8(&src[num_start..j])
                            .ok()
                            .and_then(|s| s.parse::<i32>().ok());
                        if j < src.len() && src[j] == b' ' {
                            j += 1;
                        }
                        i = j;
                        if word == "bin" {
                            // Raw binary data follows; never text.
                            i = i.saturating_add(param.unwrap_or(0).max(0) as usize);
                            continue;
                        }
                        self.word(&word, param);
                    } else {
                        i += 2;
                        match next {
                            b'\'' => {
                                let hex = src.get(i..i + 2).and_then(|h| {
                                    std::str::from_utf8(h)
                                        .ok()
                                        .and_then(|h| u8::from_str_radix(h, 16).ok())
                                });
                                if let Some(b) = hex {
                                    i += 2;
                                    self.byte(b);
                                }
                            }
                            b'*' => self.group.skip = true,
                            b'{' | b'}' | b'\\' => self.byte(next),
                            b'~' => self.push(" "),
                            b'_' => self.push("-"),
                            b'\r' | b'\n' => self.push("\n"),
                            _ => {}
                        }
                    }
                }
                b => {
                    self.byte(b);
                    i += 1;
                }
            }
        }
        self.flush();
    }
}

fn rtf(bytes: &[u8]) -> Result<Doc, String> {
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let src = &bytes[start..];
    if !src.starts_with(b"{\\rtf") {
        return Err("this file isn't RTF (no `{\\rtf` header)".into());
    }
    let mut r = Rtf {
        out: String::new(),
        bytes: Vec::new(),
        encoding: encoding_rs::WINDOWS_1252,
        group: RtfGroup { skip: false, uc: 1 },
        stack: Vec::new(),
        fallback: 0,
        high: None,
    };
    r.run(src);
    // One line per paragraph, no runs of blank lines.
    let mut text = String::new();
    let mut blank = 0;
    for line in r.out.lines().map(str::trim_end) {
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        text.push_str(line);
        text.push('\n');
    }
    Ok(Doc {
        text: text.trim_start_matches('\n').to_owned(),
        title: None,
        pages: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use zip::write::SimpleFileOptions;
    use zip::CompressionMethod;

    /// A zip of `parts`, alternating deflated and stored entries (both
    /// methods occur in real files).
    fn zip_of(parts: &[(&str, &str)]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (i, (name, body)) in parts.iter().enumerate() {
            let method = if i % 2 == 0 {
                CompressionMethod::Deflated
            } else {
                CompressionMethod::Stored
            };
            w.start_file(
                *name,
                SimpleFileOptions::default().compression_method(method),
            )
            .unwrap();
            w.write_all(body.as_bytes()).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    const ROOT_RELS: &str = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="MAIN"/></Relationships>"#;
    const CORE: &str = r#"<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Cloud storage notes</dc:title></cp:coreProperties>"#;

    fn root_rels(main: &str) -> String {
        ROOT_RELS.replace("MAIN", main)
    }

    #[test]
    fn docx_keeps_headings_paragraphs_tables_and_tabs() {
        let doc = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Storage</w:t></w:r></w:p>
<w:p><w:pPr><w:tabs><w:tab w:val="left" w:pos="720"/></w:tabs></w:pPr><w:r><w:t xml:space="preserve">Objects live in </w:t></w:r><w:r><w:t>buckets &amp; versions</w:t></w:r><w:r><w:delText>deleted words</w:delText></w:r></w:p>
<w:tbl><w:tr><w:tc><w:p><w:r><w:t>OBS</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>objects</w:t></w:r></w:p></w:tc><w:tc><w:p/></w:tc></w:tr></w:tbl>
<w:p><w:r><w:t>A</w:t></w:r><w:r><w:tab/><w:t>B</w:t></w:r><w:r><w:br/><w:t>C&#233;</w:t></w:r></w:p>
<w:p><w:pPr><w:outlineLvl w:val="1"/></w:pPr><w:r><w:t>Outlined</w:t></w:r></w:p>
</w:body></w:document>"#;
        let bytes = zip_of(&[
            ("_rels/.rels", &root_rels("word/document.xml")),
            ("word/document.xml", doc),
            ("docProps/core.xml", CORE),
        ]);
        let out = extract(Format::Docx, &bytes).unwrap();
        assert_eq!(
            out.text,
            "# Storage\nObjects live in buckets & versions\nOBS | objects\nA\tB\nCé\n## Outlined\n"
        );
        assert_eq!(out.title.as_deref(), Some("Cloud storage notes"));
        assert_eq!(out.pages, None);
    }

    #[test]
    fn pptx_tags_slides_in_show_order() {
        let slide = |body: &str| {
            format!(
                r#"<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><p:cSld><p:spTree><p:sp><p:txBody>{body}</p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#
            )
        };
        let shown_first = slide("<a:p><a:r><a:t>Intro to routing</a:t></a:r></a:p><a:p><a:r><a:t>OSPF</a:t></a:r><a:br/><a:r><a:t>BGP</a:t></a:r></a:p>");
        let empty = slide("<a:p></a:p>");
        let table = slide("<a:tbl><a:tr><a:tc><a:txBody><a:p><a:r><a:t>Port</a:t></a:r></a:p></a:txBody></a:tc><a:tc><a:txBody><a:p><a:r><a:t>22</a:t></a:r></a:p></a:txBody></a:tc></a:tr></a:tbl>");
        let presentation = r#"<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><p:sldIdLst><p:sldId id="256" r:id="rId9"/><p:sldId id="257" r:id="rId2"/><p:sldId id="258" r:id="rId3"/></p:sldIdLst></p:presentation>"#;
        let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="/ppt/slides/slide3.xml"/><Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide2.xml"/></Relationships>"#;
        let bytes = zip_of(&[
            ("_rels/.rels", &root_rels("ppt/presentation.xml")),
            ("ppt/presentation.xml", presentation),
            ("ppt/_rels/presentation.xml.rels", rels),
            ("ppt/slides/slide1.xml", &empty),
            ("ppt/slides/slide2.xml", &shown_first),
            ("ppt/slides/slide3.xml", &table),
        ]);
        let out = extract(Format::Pptx, &bytes).unwrap();
        assert_eq!(
            out.text,
            "[p.1] Intro to routing\nOSPF\nBGP\n[p.2] (no text on this slide)\n[p.3] Port | 22\n"
        );
        assert_eq!(out.pages, Some(3));
        assert_eq!(out.title, None);
    }

    #[test]
    fn xlsx_writes_each_sheet_with_shared_inline_and_boolean_cells() {
        let workbook = r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Ports" sheetId="1" r:id="rId1"/><sheet name="Empty" sheetId="2" r:id="rId4"/></sheets></workbook>"#;
        let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/><Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/></Relationships>"#;
        let strings = r#"<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><si><t>SSH</t></si><si><r><t>Tel</t></r><r><t>net</t></r><rPh><t>reading</t></rPh></si></sst>"#;
        let sheet = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="C1"><f>SUM(1,21)</f><v>22</v></c></row><row r="2"><c r="A2" t="s"><v>1</v></c><c r="B2" t="inlineStr"><is><t>plain text</t></is></c><c r="C2" t="b"><v>1</v></c></row><row r="3"><c r="A3"/></row></sheetData></worksheet>"#;
        let empty = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData/></worksheet>"#;
        let bytes = zip_of(&[
            ("_rels/.rels", &root_rels("xl/workbook.xml")),
            ("xl/workbook.xml", workbook),
            ("xl/_rels/workbook.xml.rels", rels),
            ("xl/sharedStrings.xml", strings),
            ("xl/worksheets/sheet1.xml", sheet),
            ("xl/worksheets/sheet2.xml", empty),
        ]);
        let out = extract(Format::Xlsx, &bytes).unwrap();
        assert_eq!(
            out.text,
            "## Ports\nSSH |  | 22\nTelnet | plain text | TRUE\n## Empty\n"
        );
    }

    const ODF_NS: &str = r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/""#;

    fn odf_of(body: &str, title: Option<&str>) -> Vec<u8> {
        let content = format!(
            "<office:document-content {ODF_NS}><office:body>{body}</office:body></office:document-content>"
        );
        let meta = format!(
            "<office:document-meta {ODF_NS}><office:meta><dc:title>{}</dc:title></office:meta></office:document-meta>",
            title.unwrap_or("")
        );
        zip_of(&[
            ("mimetype", "application/vnd.oasis.opendocument.text"),
            ("content.xml", &content),
            ("meta.xml", &meta),
        ])
    }

    #[test]
    fn odt_collapses_spaces_and_leaves_out_comments() {
        let body = r#"<office:text>
  <text:h text:outline-level="2">Routing</text:h>
  <text:p>OSPF   uses<text:s text:c="2"/>areas<text:tab/>fast</text:p>
  <text:p>Visible<office:annotation><text:p>a reviewer's comment</text:p></office:annotation> text</text:p>
  <table:table table:name="T1"><table:table-row><table:table-cell><text:p>a</text:p></table:table-cell><table:table-cell table:number-columns-repeated="2"/><table:table-cell><text:p>d</text:p></table:table-cell></table:table-row></table:table>
</office:text>"#;
        let out = extract(Format::Odt, &odf_of(body, Some("Network notes"))).unwrap();
        assert_eq!(
            out.text,
            "## Routing\nOSPF uses  areas\tfast\nVisible text\na |  |  | d\n"
        );
        assert_eq!(out.title.as_deref(), Some("Network notes"));
    }

    #[test]
    fn ods_names_each_sheet_and_skips_repeated_blank_rows() {
        let body = r#"<office:spreadsheet><table:table table:name="Ports">
<table:table-row><table:table-cell><text:p>SSH</text:p></table:table-cell><table:table-cell><text:p>22</text:p></table:table-cell><table:table-cell table:number-columns-repeated="1020"/></table:table-row>
<table:table-row table:number-rows-repeated="1048570"><table:table-cell table:number-columns-repeated="1024"/></table:table-row>
</table:table></office:spreadsheet>"#;
        let out = extract(Format::Ods, &odf_of(body, None)).unwrap();
        assert_eq!(out.text, "## Ports\nSSH | 22\n");
        assert_eq!(out.title, None);
    }

    #[test]
    fn odp_tags_pages_and_leaves_out_speaker_notes() {
        let body = r#"<office:presentation>
<draw:page draw:name="one"><draw:frame><draw:text-box><text:p>Welcome</text:p></draw:text-box></draw:frame><presentation:notes><draw:frame><draw:text-box><text:p>speaker only</text:p></draw:text-box></draw:frame></presentation:notes></draw:page>
<draw:page draw:name="two"/>
<draw:page draw:name="three"><draw:frame><draw:text-box><text:p>Agenda</text:p></draw:text-box></draw:frame></draw:page>
</office:presentation>"#;
        let out = extract(Format::Odp, &odf_of(body, None)).unwrap();
        assert_eq!(
            out.text,
            "[p.1] Welcome\n[p.2] (no text on this slide)\n[p.3] Agenda\n"
        );
        assert_eq!(out.pages, Some(3));
    }

    #[test]
    fn epub_reads_chapters_in_spine_order() {
        let container = r#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#;
        let opf = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/"><metadata><dc:title>Network Book</dc:title></metadata>
<manifest><item id="c1" href="text/b.xhtml" media-type="application/xhtml+xml"/><item id="c2" href="text/a%20b.xhtml" media-type="application/xhtml+xml"/><item id="img" href="cover.png" media-type="image/png"/></manifest>
<spine><itemref idref="c1"/><itemref idref="img"/><itemref idref="c2"/></spine></package>"#;
        let chapter = |h: &str, p: &str| {
            format!("<?xml version=\"1.0\"?><html xmlns=\"http://www.w3.org/1999/xhtml\"><head><title>x</title></head><body><h1>{h}</h1><p>{p}</p></body></html>")
        };
        let bytes = zip_of(&[
            ("mimetype", "application/epub+zip"),
            ("META-INF/container.xml", container),
            ("OEBPS/content.opf", opf),
            (
                "OEBPS/text/a b.xhtml",
                &chapter("Second chapter", "Beta words"),
            ),
            (
                "OEBPS/text/b.xhtml",
                &chapter("First chapter", "Alpha words"),
            ),
        ]);
        let out = extract(Format::Epub, &bytes).unwrap();
        let first = out.text.find("Alpha words").expect(&out.text);
        let second = out.text.find("Beta words").expect(&out.text);
        assert!(
            first < second,
            "spine order, not file-name order: {}",
            out.text
        );
        assert!(out.text.contains("First chapter"), "{}", out.text);
        assert_eq!(out.title.as_deref(), Some("Network Book"));
    }

    #[test]
    fn rtf_decodes_code_pages_and_unicode_and_skips_tables() {
        let src = br"{\rtf1\ansi\ansicpg1252\deff0{\fonttbl{\f0 Times New Roman;}}{\colortbl;\red0\green0\blue0;}{\*\generator Riched20;}{\info{\title Hidden}}
\pard\f0 Caf\'e9 na\u239?ve \{braces\}\par
Line two\tab end\emdash ok\par
{\pict\pngblip 89504e47}{\uc2 two\u8364??}\uc1  smile\u-10179?\u-8704?\par}";
        let out = extract(Format::Rtf, src).unwrap();
        assert_eq!(
            out.text,
            "Café naïve {braces}\nLine two\tend—ok\ntwo€ smile😀\n"
        );
        // A Chinese document in its code page.
        let gbk = br"{\rtf1\ansi\ansicpg936 \'c4\'e3\'ba\'c3\par}";
        assert_eq!(extract(Format::Rtf, gbk).unwrap().text, "你好\n");
        assert!(extract(Format::Rtf, b"plain words")
            .unwrap_err()
            .contains("isn't RTF"));
    }

    #[test]
    fn broken_or_foreign_archives_fail_with_a_reason() {
        let junk = extract(Format::Docx, b"PK\x03\x04 not really a zip").unwrap_err();
        assert!(junk.contains("zip archive"), "{junk}");
        // A zip that isn't the format it is named as.
        let other = zip_of(&[("readme.txt", "hello")]);
        let refused = |f: Format| extract(f, &other).err().unwrap_or_default();
        assert!(refused(Format::Docx).contains("Word document"));
        assert!(refused(Format::Xlsx).contains("Excel workbook"));
        assert!(refused(Format::Pptx).contains("no slides"));
        assert!(refused(Format::Odt).contains("OpenDocument"));
        assert!(refused(Format::Epub).contains("EPUB"));
    }

    #[test]
    fn text_stops_past_the_repo_cap() {
        let para = format!("<w:p><w:r><w:t>{}</w:t></w:r></w:p>", "word ".repeat(200));
        let doc = format!(
            "<w:document xmlns:w=\"w\"><w:body>{}</w:body></w:document>",
            para.repeat(super::super::MAX_TEXT_BYTES / 1000 + 400)
        );
        let bytes = zip_of(&[("word/document.xml", &doc)]);
        let out = extract(Format::Docx, &bytes).unwrap();
        assert!(
            out.text.len() > super::super::MAX_TEXT_BYTES,
            "the repo cap cuts it, not the extractor"
        );
        assert!(out.text.len() <= TEXT_BUDGET + 1100, "{}", out.text.len());
    }
}
