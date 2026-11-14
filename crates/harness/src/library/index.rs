//! The search index of a reference library (architecture §4.27), so
//! `library_search` reads only the parts of the text that can match instead
//! of scanning every file.
//!
//! **What it holds.** Each indexed file (status `ok` or `truncated`) is cut
//! at line breaks into at most [`MAX_BLOCKS`] blocks; for every token of the
//! lowercased text the index keeps the blocks it occurs in, as one bit mask
//! per (token, file). A token is a run of letters and digits; a CJK, kana,
//! Hangul or Thai character is a token of its own (those scripts have no
//! spaces between words). Each block remembers its first line and the
//! `[p.N]` page in effect where it starts.
//!
//! **Why the results don't change.** A query term is a substring, not a
//! word: `ospf` matches `ospfv3`. A line holding a term holds each of the
//! term's own tokens ("pieces") inside one of its tokens, so a block that
//! can hold the term is one where every piece is inside some token of the
//! block. The index answers that over its vocabulary (one substring scan of
//! the distinct tokens per piece) and returns a superset of the blocks that
//! match; the search then scores the lines of those blocks exactly as a
//! full scan would.
//!
//! **Where it lives.** Device-local, in
//! `<config>/library/<slug>/search-index.bin` (never synced: any device
//! derives it from the synced text), and in memory for the process. A
//! search stats each text file and re-indexes the ones whose size or mtime
//! changed (sync delivered new text, or a refresh re-extracted it); an
//! import refreshes it as its last step. A missing, corrupt or older-format
//! file is rebuilt.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::{local_dir, mtime_ns, page_tag, text_path, Library, Status};

/// The index file in a library's device-local folder.
pub(super) const INDEX_FILE: &str = "search-index.bin";
const MAGIC: &[u8; 8] = b"LHLIBIX\x01";
/// Most blocks per file: one bit each in a `u32`.
const MAX_BLOCKS: usize = 32;
/// Smallest block: a small file is one block.
const MIN_BLOCK_BYTES: usize = 16 * 1024;

/// A line-aligned slice of a file's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Block {
    /// Byte range in the text file.
    pub start: u32,
    pub end: u32,
    /// 0-based index of the block's first line.
    pub line: u32,
    /// The `[p.N]` page in effect before the first line.
    pub page: Option<u32>,
}

struct Segment {
    /// The text file's size and mtime when it was indexed.
    len: u64,
    mtime: u64,
    blocks: Vec<Block>,
    /// (token id, mask of the blocks holding it), sorted by token id.
    postings: Vec<(u32, u32)>,
}

#[derive(Default)]
pub(super) struct Index {
    vocab: Vec<String>,
    ids: HashMap<String, u32>,
    files: HashMap<String, Segment>,
    /// Changed since it was last written.
    dirty: bool,
}

/// A query against one index: per term, per piece, the vocabulary tokens
/// that contain the piece (a bit per token id).
pub(super) struct Query {
    terms: Vec<Vec<Vec<u64>>>,
}

fn is_unspaced(c: char) -> bool {
    matches!(c as u32,
        0x0E00..=0x0E7F          // Thai
        | 0x3040..=0x30FF        // Hiragana, Katakana
        | 0x31F0..=0x31FF
        | 0x3400..=0x4DBF        // CJK Extension A
        | 0x4E00..=0x9FFF        // CJK Unified Ideographs
        | 0xAC00..=0xD7AF        // Hangul syllables
        | 0xF900..=0xFAFF        // CJK compatibility
        | 0x20000..=0x3FFFF) // CJK Extensions B–H
}

/// The tokens of lowercased text, in order (repeats included).
pub(super) fn for_each_token(lower: &str, mut f: impl FnMut(&str)) {
    let mut start: Option<usize> = None;
    for (i, c) in lower.char_indices() {
        if c.is_alphanumeric() {
            if is_unspaced(c) {
                if let Some(s) = start.take() {
                    f(&lower[s..i]);
                }
                f(&lower[i..i + c.len_utf8()]);
            } else if start.is_none() {
                start = Some(i);
            }
        } else if let Some(s) = start.take() {
            f(&lower[s..i]);
        }
    }
    if let Some(s) = start {
        f(&lower[s..]);
    }
}

fn set(bits: &mut [u64], id: u32) {
    bits[id as usize / 64] |= 1 << (id % 64);
}

fn has(bits: &[u64], id: u32) -> bool {
    bits.get(id as usize / 64)
        .is_some_and(|w| w & (1 << (id % 64)) != 0)
}

impl Index {
    fn intern(vocab: &mut Vec<String>, ids: &mut HashMap<String, u32>, token: &str) -> u32 {
        if let Some(&id) = ids.get(token) {
            return id;
        }
        let id = vocab.len() as u32;
        vocab.push(token.to_owned());
        ids.insert(token.to_owned(), id);
        id
    }

    /// Index one file's text.
    fn segment(&mut self, text: &str, len: u64, mtime: u64) -> Segment {
        let target = (text.len() / MAX_BLOCKS + 1).max(MIN_BLOCK_BYTES);
        let mut blocks: Vec<Block> = Vec::new();
        let mut postings: HashMap<u32, u32> = HashMap::new();
        let (mut page, mut offset) = (None, 0usize);
        let (mut block_start, mut block_line, mut block_page) = (0usize, 0usize, None);
        let (vocab, ids) = (&mut self.vocab, &mut self.ids);
        // `split_inclusive` yields exactly the lines `str::lines` does, with
        // their line breaks, so offsets and line numbers agree with it.
        for (i, line) in text.split_inclusive('\n').enumerate() {
            if offset - block_start >= target && blocks.len() + 1 < MAX_BLOCKS {
                blocks.push(Block {
                    start: block_start as u32,
                    end: offset as u32,
                    line: block_line as u32,
                    page: block_page,
                });
                (block_start, block_line, block_page) = (offset, i, page);
            }
            if let Some(p) = page_tag(line) {
                page = Some(p);
            }
            let bit = 1u32 << blocks.len();
            for_each_token(&line.to_lowercase(), |t| {
                *postings.entry(Self::intern(vocab, ids, t)).or_insert(0) |= bit;
            });
            offset += line.len();
        }
        blocks.push(Block {
            start: block_start as u32,
            end: offset as u32,
            line: block_line as u32,
            page: block_page,
        });
        let mut postings: Vec<(u32, u32)> = postings.into_iter().collect();
        postings.sort_unstable();
        Segment {
            len,
            mtime,
            blocks,
            postings,
        }
    }

    /// Bring the index in line with `lib`'s text on disk: drop files that
    /// left the library (or lost their text), index new and changed ones.
    fn refresh(&mut self, repo: &Path, lib: &Library) {
        let wanted: HashSet<&str> = lib
            .files
            .iter()
            .filter(|f| matches!(f.status, Status::Ok | Status::Truncated))
            .map(|f| f.path.as_str())
            .collect();
        let before = self.files.len();
        self.files.retain(|rel, _| wanted.contains(rel.as_str()));
        self.dirty |= self.files.len() != before;
        for rel in wanted {
            let path = text_path(repo, &lib.slug, rel);
            let Ok(meta) = fs::metadata(&path) else {
                self.dirty |= self.files.remove(rel).is_some();
                continue;
            };
            let (len, mtime) = (meta.len(), mtime_ns(&meta));
            if self
                .files
                .get(rel)
                .is_some_and(|s| s.len == len && s.mtime == mtime)
            {
                continue;
            }
            self.dirty = true;
            // Text that isn't UTF-8 is unreadable to the search as well.
            match fs::read_to_string(&path) {
                Ok(text) if text.len() <= u32::MAX as usize => {
                    let seg = self.segment(&text, len, mtime);
                    self.files.insert(rel.to_owned(), seg);
                }
                _ => {
                    self.files.remove(rel);
                }
            }
        }
    }

    /// The vocabulary tokens each piece of each term can match.
    pub(super) fn query(&self, terms: &[String]) -> Query {
        let words = self.vocab.len().div_ceil(64);
        let mut cache: HashMap<String, Vec<u64>> = HashMap::new();
        let terms = terms
            .iter()
            .map(|term| {
                let mut pieces: Vec<String> = Vec::new();
                for_each_token(term, |p| pieces.push(p.to_owned()));
                pieces
                    .into_iter()
                    .map(|piece| {
                        cache
                            .entry(piece.clone())
                            .or_insert_with(|| {
                                let mut bits = vec![0u64; words];
                                let single = piece.chars().next().is_some_and(is_unspaced);
                                if single {
                                    if let Some(&id) = self.ids.get(&piece) {
                                        set(&mut bits, id);
                                    }
                                } else {
                                    for (id, token) in self.vocab.iter().enumerate() {
                                        if token.contains(piece.as_str()) {
                                            set(&mut bits, id as u32);
                                        }
                                    }
                                }
                                bits
                            })
                            .clone()
                    })
                    .collect()
            })
            .collect();
        Query { terms }
    }

    /// The blocks of `rel` that may hold a term, each with a bit per term
    /// it may hold (bit `i`: `terms[i]`). Empty when the file isn't indexed.
    pub(super) fn candidates(&self, rel: &str, query: &Query) -> Vec<(Block, u32)> {
        let Some(seg) = self.files.get(rel) else {
            return Vec::new();
        };
        let mut piece_masks: Vec<Vec<u32>> =
            query.terms.iter().map(|p| vec![0u32; p.len()]).collect();
        for &(token, mask) in &seg.postings {
            for (t, pieces) in query.terms.iter().enumerate() {
                for (p, bits) in pieces.iter().enumerate() {
                    if has(bits, token) {
                        piece_masks[t][p] |= mask;
                    }
                }
            }
        }
        let term_masks: Vec<u32> = piece_masks
            .iter()
            .map(|pm| {
                if pm.is_empty() {
                    0
                } else {
                    pm.iter().fold(u32::MAX, |a, m| a & m)
                }
            })
            .collect();
        seg.blocks
            .iter()
            .enumerate()
            .filter_map(|(b, block)| {
                let terms = term_masks
                    .iter()
                    .enumerate()
                    .filter(|(_, m)| *m & (1 << b) != 0)
                    .fold(0u32, |acc, (t, _)| acc | (1 << t));
                (terms != 0).then_some((*block, terms))
            })
            .collect()
    }

    // ---- the file ---------------------------------------------------------

    /// Drop vocabulary no file uses any more, keeping ids in order so every
    /// posting list stays sorted.
    fn compact(&mut self) {
        let mut used = vec![false; self.vocab.len()];
        for seg in self.files.values() {
            for &(t, _) in &seg.postings {
                used[t as usize] = true;
            }
        }
        if used.iter().all(|u| *u) {
            return;
        }
        let mut remap = vec![u32::MAX; self.vocab.len()];
        let mut vocab = Vec::new();
        for (old, token) in std::mem::take(&mut self.vocab).into_iter().enumerate() {
            if used[old] {
                remap[old] = vocab.len() as u32;
                vocab.push(token);
            }
        }
        for seg in self.files.values_mut() {
            for p in &mut seg.postings {
                p.0 = remap[p.0 as usize];
            }
        }
        self.ids = vocab
            .iter()
            .enumerate()
            .map(|(i, t)| (t.clone(), i as u32))
            .collect();
        self.vocab = vocab;
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        let put = |out: &mut Vec<u8>, n: u32| out.extend_from_slice(&n.to_le_bytes());
        let put_str = |out: &mut Vec<u8>, s: &str| {
            out.extend_from_slice(&(s.len() as u32).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        };
        put(&mut out, self.vocab.len() as u32);
        for t in &self.vocab {
            put_str(&mut out, t);
        }
        let mut rels: Vec<&String> = self.files.keys().collect();
        rels.sort();
        put(&mut out, rels.len() as u32);
        for rel in rels {
            let seg = &self.files[rel];
            put_str(&mut out, rel);
            out.extend_from_slice(&seg.len.to_le_bytes());
            out.extend_from_slice(&seg.mtime.to_le_bytes());
            put(&mut out, seg.blocks.len() as u32);
            for b in &seg.blocks {
                put(&mut out, b.start);
                put(&mut out, b.end);
                put(&mut out, b.line);
                put(&mut out, b.page.unwrap_or(0));
            }
            put(&mut out, seg.postings.len() as u32);
            for &(t, m) in &seg.postings {
                put(&mut out, t);
                put(&mut out, m);
            }
        }
        out
    }

    /// The index in `bytes`; `None` when it isn't one this version wrote
    /// whole (it is rebuilt then).
    fn decode(bytes: &[u8]) -> Option<Index> {
        struct Cur<'a>(&'a [u8]);
        impl<'a> Cur<'a> {
            fn take(&mut self, n: usize) -> Option<&'a [u8]> {
                if self.0.len() < n {
                    return None;
                }
                let (head, rest) = self.0.split_at(n);
                self.0 = rest;
                Some(head)
            }
            fn u32(&mut self) -> Option<u32> {
                Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
            }
            fn u64(&mut self) -> Option<u64> {
                Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
            }
            /// A count of items at least `min` bytes each: never more than
            /// the bytes left could hold.
            fn count(&mut self, min: usize) -> Option<usize> {
                let n = self.u32()? as usize;
                (n.saturating_mul(min) <= self.0.len()).then_some(n)
            }
            fn string(&mut self) -> Option<String> {
                let n = self.u32()? as usize;
                String::from_utf8(self.take(n)?.to_vec()).ok()
            }
        }
        let mut c = Cur(bytes);
        if c.take(MAGIC.len())? != MAGIC {
            return None;
        }
        let n = c.count(4)?;
        let mut vocab = Vec::with_capacity(n);
        for _ in 0..n {
            vocab.push(c.string()?);
        }
        let mut files = HashMap::new();
        for _ in 0..c.count(4)? {
            let rel = c.string()?;
            let (len, mtime) = (c.u64()?, c.u64()?);
            let nb = c.count(16)?;
            if nb == 0 || nb > MAX_BLOCKS {
                return None;
            }
            let mut blocks = Vec::with_capacity(nb);
            for _ in 0..nb {
                let (start, end, line, page) = (c.u32()?, c.u32()?, c.u32()?, c.u32()?);
                blocks.push(Block {
                    start,
                    end,
                    line,
                    page: (page != 0).then_some(page),
                });
            }
            let np = c.count(8)?;
            let mut postings = Vec::with_capacity(np);
            for _ in 0..np {
                let (t, m) = (c.u32()?, c.u32()?);
                if t as usize >= vocab.len() {
                    return None;
                }
                postings.push((t, m));
            }
            files.insert(
                rel,
                Segment {
                    len,
                    mtime,
                    blocks,
                    postings,
                },
            );
        }
        if !c.0.is_empty() {
            return None;
        }
        let ids = vocab
            .iter()
            .enumerate()
            .map(|(i, t)| (t.clone(), i as u32))
            .collect();
        Some(Index {
            vocab,
            ids,
            files,
            dirty: false,
        })
    }

    fn load(path: &Path) -> Index {
        fs::read(path)
            .ok()
            .and_then(|b| Index::decode(&b))
            .unwrap_or_default()
    }

    fn save(&mut self, path: &Path) {
        self.compact();
        let write = || -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension("bin.tmp");
            fs::write(&tmp, self.encode())?;
            fs::rename(&tmp, path)
        };
        match write() {
            Ok(()) => self.dirty = false,
            // The index is a cache: a device that can't write it searches
            // from memory and tries again next time.
            Err(e) => tracing::warn!(path = %path.display(), error = %e, "library index not saved"),
        }
    }
}

/// Every library's index this process has opened, by index file.
static OPEN: Mutex<Option<HashMap<PathBuf, Arc<Mutex<Index>>>>> = Mutex::new(None);

fn index_file(config: &Path, slug: &str) -> PathBuf {
    local_dir(config, slug).join(INDEX_FILE)
}

fn open(path: &Path) -> Arc<Mutex<Index>> {
    let mut all = crate::lock(&OPEN);
    let all = all.get_or_insert_with(HashMap::new);
    Arc::clone(
        all.entry(path.to_owned())
            .or_insert_with(|| Arc::new(Mutex::new(Index::load(path)))),
    )
}

/// Run `f` on `lib`'s index, brought up to date with its text first (and
/// written back when that changed it).
pub(super) fn with_index<R>(
    config: &Path,
    repo: &Path,
    lib: &Library,
    f: impl FnOnce(&Index) -> R,
) -> R {
    let path = index_file(config, &lib.slug);
    let index = open(&path);
    let mut index = crate::lock(&index);
    index.refresh(repo, lib);
    if index.dirty {
        index.save(&path);
    }
    f(&index)
}

/// After an import: re-index the files it extracted (`changed`, whose text
/// may have been rewritten within one mtime tick) and the rest as needed.
pub(super) fn update(config: &Path, repo: &Path, lib: &Library, changed: &[String]) {
    let path = index_file(config, &lib.slug);
    let index = open(&path);
    let mut index = crate::lock(&index);
    for rel in changed {
        index.dirty |= index.files.remove(rel).is_some();
    }
    index.refresh(repo, lib);
    if index.dirty {
        index.save(&path);
    }
}

/// Forget a removed library's index (its file goes with the folder).
pub(super) fn forget(config: &Path, slug: &str) {
    if let Some(all) = crate::lock(&OPEN).as_mut() {
        all.remove(&index_file(config, slug));
    }
}

/// Bytes `block` of a text file, when they are still there and UTF-8.
pub(super) fn read_block(file: &mut fs::File, block: &Block) -> Option<String> {
    let len = (block.end - block.start) as usize;
    let mut buf = vec![0u8; len];
    file.seek(SeekFrom::Start(u64::from(block.start))).ok()?;
    file.read_exact(&mut buf).ok()?;
    String::from_utf8(buf).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_split_words_and_unspaced_scripts() {
        let mut t = Vec::new();
        for_each_token("ospfv3 area-0, 题库ok 1.2 ข้อสอบ", |s| {
            t.push(s.to_owned())
        });
        // Thai combining vowels are marks, not alphanumeric: they split.
        assert_eq!(&t[..7], ["ospfv3", "area", "0", "题", "库", "ok", "1"]);
        assert_eq!(t[7], "2");
    }

    #[test]
    fn blocks_cover_the_text_on_line_breaks_and_carry_pages() {
        let mut idx = Index::default();
        let line = "[p.1] alpha words here\n".to_string()
            + &"filler line of text\n".repeat(4000)
            + "[p.2] beta\n"
            + &"more filler text\n".repeat(4000);
        let seg = idx.segment(&line, line.len() as u64, 0);
        assert!(seg.blocks.len() > 1 && seg.blocks.len() <= MAX_BLOCKS);
        assert_eq!(seg.blocks[0].start, 0);
        assert_eq!(seg.blocks.last().unwrap().end as usize, line.len());
        for w in seg.blocks.windows(2) {
            assert_eq!(w[0].end, w[1].start);
            assert_eq!(&line[w[1].start as usize - 1..w[1].start as usize], "\n");
            let lines_before = line[..w[1].start as usize].lines().count();
            assert_eq!(w[1].line as usize, lines_before);
        }
        assert_eq!(seg.blocks[0].page, None);
        assert_eq!(seg.blocks.last().unwrap().page, Some(2));
    }

    #[test]
    fn the_file_round_trips_and_garbage_is_refused() {
        let mut idx = Index::default();
        let seg = idx.segment("[p.3] Hello world\nsecond 题\n", 27, 42);
        idx.files.insert("a.md".into(), seg);
        let bytes = idx.encode();
        let back = Index::decode(&bytes).unwrap();
        assert_eq!(back.vocab, idx.vocab);
        let (a, b) = (&idx.files["a.md"], &back.files["a.md"]);
        assert_eq!(
            (a.len, a.mtime, &a.blocks, &a.postings),
            (b.len, b.mtime, &b.blocks, &b.postings)
        );
        assert!(Index::decode(&bytes[..bytes.len() - 1]).is_none());
        assert!(Index::decode(b"not an index").is_none());
        let mut huge = bytes.clone();
        huge[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(
            Index::decode(&huge).is_none(),
            "a count past the bytes left"
        );
    }

    #[test]
    fn compaction_keeps_postings_sorted_and_drops_unused_tokens() {
        let mut idx = Index::default();
        let gone = idx.segment("zeta only here\n", 1, 1);
        idx.files.insert("gone.md".into(), gone);
        let kept = idx.segment("alpha zeta beta\n", 1, 1);
        idx.files.insert("kept.md".into(), kept);
        idx.files.remove("gone.md");
        idx.compact();
        assert_eq!(idx.vocab, ["zeta", "alpha", "beta"]);
        let seg = &idx.files["kept.md"];
        assert!(seg.postings.windows(2).all(|w| w[0].0 < w[1].0));
        let q = idx.query(&["lph".into()]);
        assert_eq!(idx.candidates("kept.md", &q).len(), 1);
    }
}
