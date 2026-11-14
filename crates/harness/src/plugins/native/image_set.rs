//! `image-set`: labelled image drills (docs/plugins.md §12.2).
//!
//! **Where sets live.** `<repo>/image-sets/<setId>/<label>/<image>`: the
//! folder name is the label. Sets sit in the synced data repo, like message
//! images (architecture §4.25), because a drill must work on every device;
//! a learner's own labelled folder is copied in once with [`import`]
//! (desktop, the source is never touched). An optional
//! `<setId>/set.json` `{ "title": "…" }` names the set. Only PNG, JPEG, GIF
//! and WebP files up to [`MAX_IMAGE_BYTES`] count; hidden entries and
//! symlinks are skipped.
//!
//! **Drill.** `image_set_drill` picks interleaved items and embeds each image
//! as a `data:` URI (the only image source the artifact sanitizer and the
//! view CSP allow) in an answer-mode figure. Images are shrunk to fit
//! [`EMBED_BUDGET`] split across the drill's images, so the fallback SVG and
//! the artifact `data` each stay under the 256 KiB artifact caps. The answer
//! key goes back as `target` text for the tutor only (with a checksum, so a
//! miscopied target is refused); the `data` the view gets has no labels.
//! `grade` scores the view's `{ answers }` against the target and returns
//! per-item results plus the confusion pairs.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use serde::Serialize;
use serde_json::{json, Value};

use super::{blocking, done, fail, ready, NativeEnv, NativePackage};
use crate::plugins::{ArtifactPayload, Plugin, PluginContext, PluginManifest, ToolFuture};
use providers::ToolSpec;

/// Folder under the data repo holding the sets.
pub const ROOT: &str = "image-sets";
/// Largest image file a set may use (the attachment cap, §4.25).
pub const MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;
/// Longest side of an image as embedded in a figure.
pub const DISPLAY_PX: u32 = 360;
/// Total bytes of `data:` URIs one figure may embed: under the 256 KiB caps
/// on the fallback SVG and on artifact data, with room for the markup.
pub const EMBED_BUDGET: usize = 232 * 1024;
/// Longest sides tried, in order, when an image must shrink to its budget.
const SIDES: [u32; 7] = [DISPLAY_PX, 280, 220, 170, 130, 100, 80];
/// Most images one drill embeds.
const MAX_DRILL_IMAGES: usize = 36;
/// Most images [`import`] copies.
const MAX_IMPORT_IMAGES: usize = 2000;

/// One set as `image_set_list` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetInfo {
    pub id: String,
    pub title: String,
    pub labels: Vec<LabelInfo>,
    pub size: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LabelInfo {
    pub label: String,
    pub count: usize,
}

/// A set's images by label, labels sorted, files sorted within a label.
type Set = BTreeMap<String, Vec<PathBuf>>;

fn hidden(name: &str) -> bool {
    name.starts_with('.')
}

pub(crate) fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

fn has_image_ext(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "webp"
        )
    })
}

/// Regular, visible, non-symlink entries of `dir`, sorted by name.
fn entries(dir: &Path) -> Vec<(String, PathBuf, std::fs::Metadata)> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<_> = read
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let meta = std::fs::symlink_metadata(e.path()).ok()?;
            (!hidden(&name) && !meta.file_type().is_symlink()).then(|| (name, e.path(), meta))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Usable images of one label folder.
fn label_images(dir: &Path) -> Vec<PathBuf> {
    entries(dir)
        .into_iter()
        .filter(|(_, p, m)| {
            m.is_file() && m.len() > 0 && m.len() <= MAX_IMAGE_BYTES && has_image_ext(p)
        })
        .map(|(_, p, _)| p)
        .collect()
}

fn read_set(dir: &Path) -> Set {
    entries(dir)
        .into_iter()
        .filter(|(_, _, m)| m.is_dir())
        .filter_map(|(label, p, _)| {
            let images = label_images(&p);
            (!images.is_empty()).then_some((label, images))
        })
        .collect()
}

fn set_title(dir: &Path, id: &str) -> String {
    std::fs::read_to_string(dir.join("set.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("title")?.as_str().map(str::trim).map(str::to_owned))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| id.to_owned())
}

/// Every set with at least one label, sorted by id.
pub fn list_sets(repo: &Path) -> Vec<SetInfo> {
    entries(&repo.join(ROOT))
        .into_iter()
        .filter(|(_, _, m)| m.is_dir())
        .filter_map(|(id, dir, _)| {
            let set = read_set(&dir);
            if set.is_empty() {
                return None;
            }
            let labels: Vec<LabelInfo> = set
                .iter()
                .map(|(label, images)| LabelInfo {
                    label: label.clone(),
                    count: images.len(),
                })
                .collect();
            Some(SetInfo {
                title: set_title(&dir, &id),
                size: labels.iter().map(|l| l.count).sum(),
                labels,
                id,
            })
        })
        .collect()
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && !hidden(id) && !id.contains(['/', '\\']) && id != ".."
}

fn load_set(repo: &Path, id: &str) -> Result<Set, String> {
    let known = || {
        let ids: Vec<String> = list_sets(repo).into_iter().map(|s| s.id).collect();
        if ids.is_empty() {
            format!("no image sets exist yet (the learner adds labelled folders under `{ROOT}/`)")
        } else {
            format!("available sets: {}", ids.join(", "))
        }
    };
    if !valid_id(id) {
        return Err(format!("no image set `{id}`; {}", known()));
    }
    let set = read_set(&repo.join(ROOT).join(id));
    if set.is_empty() {
        return Err(format!("no image set `{id}`; {}", known()));
    }
    Ok(set)
}

/// The drill formats of `image_set_drill`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Classify,
    SameOrDifferent,
    OddOneOut,
}

impl Format {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "classify" => Some(Self::Classify),
            "same-or-different" => Some(Self::SameOrDifferent),
            "odd-one-out" => Some(Self::OddOneOut),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Classify => "classify",
            Self::SameOrDifferent => "same-or-different",
            Self::OddOneOut => "odd-one-out",
        }
    }

    fn per_item(self) -> usize {
        match self {
            Self::Classify => 1,
            Self::SameOrDifferent => 2,
            Self::OddOneOut => 3,
        }
    }
}

/// Small xorshift PRNG: drills need variety, not cryptographic randomness.
pub struct Rng(u64);

impl Rng {
    pub fn seeded(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn fresh() -> Self {
        Self::seeded(ulid::Ulid::new().random() as u64)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            v.swap(i, self.below(i + 1));
        }
    }
}

/// Draws images of each label without repeats until a label runs out, then
/// starts that label's shuffled deck again.
struct Decks<'a> {
    set: &'a Set,
    decks: HashMap<&'a str, Vec<&'a PathBuf>>,
}

impl<'a> Decks<'a> {
    fn new(set: &'a Set) -> Self {
        Self {
            set,
            decks: HashMap::new(),
        }
    }

    fn draw(&mut self, label: &'a str, rng: &mut Rng) -> &'a PathBuf {
        let deck = self.decks.entry(label).or_default();
        if deck.is_empty() {
            deck.extend(self.set[label].iter());
            rng.shuffle(deck);
        }
        deck.pop().expect("labels in a set have images")
    }

    /// Two different images of `label` (the label has at least two).
    fn draw_two(&mut self, label: &'a str, rng: &mut Rng) -> [&'a PathBuf; 2] {
        let a = self.draw(label, rng);
        let mut b = self.draw(label, rng);
        while b == a {
            b = self.draw(label, rng);
        }
        [a, b]
    }
}

/// One drill item: its images with their labels, and the key.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub images: Vec<(String, PathBuf)>,
    pub key: Value,
}

/// Interleaved label order: a random label each time, never the previous
/// one when another is available.
fn pick_other(labels: &[&str], prev: Option<&str>, rng: &mut Rng) -> usize {
    let candidates: Vec<usize> = (0..labels.len())
        .filter(|&i| Some(labels[i]) != prev)
        .collect();
    if candidates.is_empty() {
        rng.below(labels.len())
    } else {
        candidates[rng.below(candidates.len())]
    }
}

/// The labels a drill uses: `labels` (deduplicated, each must exist) or
/// every label of the set. At least two must remain.
pub fn chosen_labels<'a>(set: &'a Set, labels: Option<&[String]>) -> Result<Vec<&'a str>, String> {
    let all: Vec<&str> = set.keys().map(String::as_str).collect();
    let chosen: Vec<&str> = match labels {
        Some(wanted) => {
            let mut out = Vec::new();
            for w in wanted {
                let Some(l) = all.iter().find(|l| **l == w.as_str()) else {
                    return Err(format!(
                        "the set has no label `{w}`; its labels: {}",
                        all.join(", ")
                    ));
                };
                if !out.contains(l) {
                    out.push(*l);
                }
            }
            out
        }
        None => all.clone(),
    };
    if chosen.len() < 2 {
        return Err(format!(
            "a drill needs at least two labels; this set has: {}",
            all.join(", ")
        ));
    }
    Ok(chosen)
}

/// Choose the drill items over [`chosen_labels`].
pub fn plan_drill(
    set: &Set,
    labels: Option<&[String]>,
    n: usize,
    format: Format,
    rng: &mut Rng,
) -> Result<Vec<Item>, String> {
    let chosen = chosen_labels(set, labels)?;
    if n == 0 {
        return Err("`n` must be at least 1".into());
    }
    if n * format.per_item() > MAX_DRILL_IMAGES {
        return Err(format!(
            "at most {} {} items per drill",
            MAX_DRILL_IMAGES / format.per_item(),
            format.as_str()
        ));
    }
    let mut decks = Decks::new(set);
    let entry = |label: &str, path: &PathBuf| (label.to_owned(), path.clone());
    let mut items = Vec::with_capacity(n);
    let mut prev: Option<&str> = None;
    match format {
        Format::Classify => {
            for _ in 0..n {
                let label = chosen[pick_other(&chosen, prev, rng)];
                let img = decks.draw(label, rng);
                items.push(Item {
                    images: vec![entry(label, img)],
                    key: json!(label),
                });
                prev = Some(label);
            }
        }
        Format::SameOrDifferent => {
            let pairable: Vec<&str> = chosen
                .iter()
                .copied()
                .filter(|l| set[*l].len() >= 2)
                .collect();
            // Half the items are "same" pairs when any label has two images.
            let same_count = if pairable.is_empty() {
                0
            } else {
                n / 2 + (n % 2) * rng.below(2)
            };
            let mut kinds: Vec<bool> = (0..n).map(|i| i < same_count).collect();
            rng.shuffle(&mut kinds);
            for same in kinds {
                if same {
                    let label = pairable[pick_other(&pairable, prev, rng)];
                    let [a, b] = decks.draw_two(label, rng);
                    items.push(Item {
                        images: vec![entry(label, a), entry(label, b)],
                        key: json!("same"),
                    });
                    prev = Some(label);
                } else {
                    let first = pick_other(&chosen, prev, rng);
                    let mut second = rng.below(chosen.len() - 1);
                    if second >= first {
                        second += 1;
                    }
                    let (la, lb) = (chosen[first], chosen[second]);
                    let a = decks.draw(la, rng);
                    let b = decks.draw(lb, rng);
                    items.push(Item {
                        images: vec![entry(la, a), entry(lb, b)],
                        key: json!("different"),
                    });
                    prev = Some(lb);
                }
            }
        }
        Format::OddOneOut => {
            let majority: Vec<&str> = chosen
                .iter()
                .copied()
                .filter(|l| set[*l].len() >= 2)
                .collect();
            if majority.is_empty() {
                return Err("odd-one-out needs a label with at least two images".into());
            }
            for _ in 0..n {
                let main = majority[pick_other(&majority, prev, rng)];
                let others: Vec<&str> = chosen.iter().copied().filter(|l| *l != main).collect();
                let odd = others[rng.below(others.len())];
                let [a, b] = decks.draw_two(main, rng);
                let o = decks.draw(odd, rng);
                let pos = rng.below(3);
                let mut images = vec![entry(main, a), entry(main, b)];
                images.insert(pos, entry(odd, o));
                items.push(Item {
                    images,
                    key: json!(pos),
                });
                prev = Some(main);
            }
        }
    }
    Ok(items)
}

/// An image file as a `data:` URI of at most `budget` bytes, for the figure
/// and the view (see [`encode_fit`]).
pub fn data_uri(path: &Path, budget: usize) -> Result<String, String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let bytes = std::fs::read(path).map_err(|e| format!("can't read `{name}` ({e})"))?;
    let mime = image_mime(&bytes)
        .ok_or_else(|| format!("`{name}` is not a PNG, JPEG, GIF or WebP image"))?;
    encode_fit(&bytes, mime, budget).map_err(|e| format!("`{name}`: {e}"))
}

/// Image bytes as a `data:` URI of at most `budget` bytes. A PNG or JPEG
/// that is small enough in both pixels ([`DISPLAY_PX`]) and bytes is kept
/// as it is; otherwise it becomes a JPEG at the largest of [`SIDES`] that
/// fits. GIF and WebP (not decodable here) are kept only when they fit.
pub(crate) fn encode_fit(bytes: &[u8], mime: &str, budget: usize) -> Result<String, String> {
    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    let uri_len = |n: usize, mime: &str| "data:;base64,".len() + mime.len() + n.div_ceil(3) * 4;
    if !matches!(mime, "image/png" | "image/jpeg") {
        if uri_len(bytes.len(), mime) <= budget {
            return Ok(format!("data:{mime};base64,{}", b64(bytes)));
        }
        return Err("a GIF or WebP image too large to show here; use a PNG or JPEG".into());
    }
    let img =
        image::load_from_memory(bytes).map_err(|e| format!("the image can't be decoded ({e})"))?;
    let longest = img.width().max(img.height());
    if longest <= DISPLAY_PX && uri_len(bytes.len(), mime) <= budget {
        return Ok(format!("data:{mime};base64,{}", b64(bytes)));
    }
    for side in SIDES {
        let side = side.min(longest);
        let thumb = img.thumbnail(side, side).to_rgb8();
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80)
            .encode_image(&thumb)
            .map_err(|e| format!("the image can't be re-encoded ({e})"))?;
        if uri_len(out.len(), "image/jpeg") <= budget {
            return Ok(format!("data:image/jpeg;base64,{}", b64(&out)));
        }
    }
    Err("the image can't be shrunk enough to show here".into())
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The static figure: one row per item, numbered, images in fixed boxes
/// (odd-one-out images lettered A–C), drawn from the artifact `data`.
pub fn render(data: &Value) -> Result<String, String> {
    const BOX_W: usize = 160;
    const BOX_H: usize = 120;
    const ROW: usize = BOX_H + 30;
    let items = data
        .get("items")
        .and_then(Value::as_array)
        .ok_or("drill data has no items")?;
    let odd = data.get("format").and_then(Value::as_str) == Some("odd-one-out");
    let widest = items
        .iter()
        .map(|i| {
            i.get("images")
                .and_then(Value::as_array)
                .map_or(0, Vec::len)
        })
        .max()
        .unwrap_or(1)
        .max(1);
    let width = 40 + widest * (BOX_W + 10);
    let height = 10 + items.len().max(1) * ROW;
    let mut svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}" width="{width}" height="{height}" font-family="sans-serif" font-size="14">"#
    );
    for (i, item) in items.iter().enumerate() {
        let y = 10 + i * ROW;
        let _ = write!(
            svg,
            r#"<text x="4" y="{}" fill="currentColor">{}.</text>"#,
            y + 18,
            i + 1
        );
        let images = item
            .get("images")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for (j, src) in images.iter().enumerate() {
            let Some(src) = src.as_str().filter(|s| s.starts_with("data:image/")) else {
                continue;
            };
            let x = 40 + j * (BOX_W + 10);
            let _ = write!(
                svg,
                r#"<image x="{x}" y="{y}" width="{BOX_W}" height="{BOX_H}" preserveAspectRatio="xMidYMid meet" href="{}"/>"#,
                esc(src)
            );
            if odd {
                let letter = char::from(b'A' + j as u8);
                let _ = write!(
                    svg,
                    r#"<text x="{}" y="{}" text-anchor="middle" fill="currentColor">{letter}</text>"#,
                    x + BOX_W / 2,
                    y + BOX_H + 18
                );
            }
        }
    }
    svg.push_str("</svg>");
    Ok(svg)
}

/// The built drill: the view's `data` (no labels), the tutor's `target`.
pub struct Drill {
    pub data: Value,
    pub target: Value,
}

/// The integrity check stored in a target: a miscopied `key` or `labels`
/// would grade the learner against the wrong answers.
fn target_check(format: &str, key: &Value, labels: &Value) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("{format}\n{key}\n{labels}").as_bytes());
    digest[..6].iter().map(|b| format!("{b:02x}")).collect()
}

pub fn build_drill(
    set_id: &str,
    items: &[Item],
    format: Format,
    choices: &[String],
) -> Result<Drill, String> {
    let slots = items.iter().map(|i| i.images.len()).sum::<usize>().max(1);
    let budget = EMBED_BUDGET / slots;
    let mut cache: HashMap<&PathBuf, String> = HashMap::new();
    let mut view_items = Vec::with_capacity(items.len());
    for item in items {
        let mut images = Vec::with_capacity(item.images.len());
        for (_, path) in &item.images {
            let uri = match cache.get(path) {
                Some(u) => u.clone(),
                None => {
                    let u = data_uri(path, budget)?;
                    cache.insert(path, u.clone());
                    u
                }
            };
            images.push(uri);
        }
        view_items.push(json!({ "images": images }));
    }
    let mut data = json!({ "setId": set_id, "format": format.as_str(), "items": view_items });
    if format == Format::Classify {
        data["choices"] = json!(choices);
    }
    let key = json!(items.iter().map(|i| i.key.clone()).collect::<Vec<_>>());
    let labels = json!(items
        .iter()
        .map(|i| i.images.iter().map(|(l, _)| l.clone()).collect::<Vec<_>>())
        .collect::<Vec<_>>());
    let check = target_check(format.as_str(), &key, &labels);
    let target = json!({ "format": format.as_str(), "key": key, "labels": labels, "check": check });
    Ok(Drill { data, target })
}

/// Score `{ answers: [...] }` from the view against a drill `target`.
///
/// Returns `{ correct, score, total, detail, items: [{ item, expected,
/// given, correct }], confusions: [{ expected, given, count }] }`. A
/// confusion is a label taken for another: in `classify` the given label for
/// the true one; in `same-or-different` a "same" on a different pair (both
/// labels); in `odd-one-out` the label of the image picked for the true odd
/// one's label. A target whose `check` doesn't match its `format`, `key`
/// and `labels` (the tutor miscopied it) is refused rather than graded.
pub fn grade(answer: &Value, target: &Value) -> Result<Value, String> {
    let format = target
        .get("format")
        .and_then(Value::as_str)
        .and_then(Format::parse)
        .ok_or("the target is not an image-set drill target (no valid `format`)")?;
    let key_value = target.get("key").cloned().unwrap_or(Value::Null);
    let labels_value = target.get("labels").cloned().unwrap_or(Value::Null);
    if target.get("check").and_then(Value::as_str)
        != Some(target_check(format.as_str(), &key_value, &labels_value).as_str())
    {
        return Err("the drill's answer key was not copied exactly from image_set_drill, so it can't be trusted; ask the drill again".into());
    }
    let key = key_value.as_array().ok_or("the target has no `key`")?;
    let labels = labels_value
        .as_array()
        .filter(|l| l.len() == key.len())
        .ok_or("the target's `labels` don't match its `key`")?;
    let given = answer
        .get("answers")
        .and_then(Value::as_array)
        .ok_or("the answer has no `answers` list")?;
    if given.len() != key.len() {
        return Err(format!(
            "expected {} answers, got {}",
            key.len(),
            given.len()
        ));
    }
    let label_at = |i: usize, j: usize| {
        labels[i]
            .get(j)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let mut confusions: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut rows = Vec::with_capacity(key.len());
    let mut score = 0;
    for (i, (k, g)) in key.iter().zip(given).enumerate() {
        let correct = k == g || (k.as_u64().is_some() && k.as_u64() == g.as_u64());
        let (expected, shown) = match format {
            Format::Classify => (k.clone(), g.clone()),
            Format::SameOrDifferent => (k.clone(), g.clone()),
            Format::OddOneOut => {
                let letter = |v: &Value| {
                    v.as_u64()
                        .filter(|&p| p < 3)
                        .map(|p| {
                            json!(format!(
                                "{} ({})",
                                char::from(b'A' + p as u8),
                                label_at(i, p as usize)
                            ))
                        })
                        .unwrap_or_else(|| v.clone())
                };
                (letter(k), letter(g))
            }
        };
        if correct {
            score += 1;
        } else {
            let pair = match format {
                Format::Classify => g.as_str().map(|g| (label_at(i, 0), g.to_owned())),
                Format::SameOrDifferent => {
                    (g.as_str() == Some("same")).then(|| (label_at(i, 0), label_at(i, 1)))
                }
                Format::OddOneOut => k
                    .as_u64()
                    .zip(g.as_u64().filter(|&p| p < 3))
                    .map(|(k, g)| (label_at(i, k as usize), label_at(i, g as usize))),
            };
            if let Some(pair) = pair {
                *confusions.entry(pair).or_default() += 1;
            }
        }
        rows.push(
            json!({ "item": i + 1, "expected": expected, "given": shown, "correct": correct }),
        );
    }
    let mut pairs: Vec<((String, String), usize)> = confusions.into_iter().collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let total = key.len();
    let mut detail = format!("{score}/{total} correct");
    if !pairs.is_empty() {
        let list: Vec<String> = pairs
            .iter()
            .map(|((e, g), c)| format!("{e} → {g} ×{c}"))
            .collect();
        let _ = write!(detail, "; confused: {}", list.join(", "));
    }
    Ok(json!({
        "correct": score == total,
        "score": score,
        "total": total,
        "detail": detail,
        "items": rows,
        "confusions": pairs
            .iter()
            .map(|((e, g), c)| json!({ "expected": e, "given": g, "count": c }))
            .collect::<Vec<_>>(),
    }))
}

/// What [`import`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetImportReport {
    pub set: SetInfo,
    /// Files skipped: not an image by content, over the size cap, or past
    /// the image limit.
    pub skipped: usize,
}

/// A set id from a learner-chosen name: lowercase ASCII letters, digits and
/// `-`.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_owned()
}

/// Copy a learner's labelled folder (one subfolder per label) into
/// `<repo>/image-sets/<slug(name)>/`, replacing a set with that id. Only
/// files whose bytes are PNG, JPEG, GIF or WebP and at most
/// [`MAX_IMAGE_BYTES`] are copied; the source is never modified.
pub fn import(repo: &Path, src: &Path, name: &str) -> Result<SetImportReport, String> {
    let id = slug(name);
    if id.is_empty() {
        return Err("give the set a name with letters or digits".into());
    }
    if !src.is_dir() {
        return Err(format!("`{}` is not a folder", src.display()));
    }
    let mut plan: Vec<(String, String, Vec<u8>)> = Vec::new();
    let mut skipped = 0;
    for (label, dir, meta) in entries(src) {
        if !meta.is_dir() {
            continue;
        }
        for (file, path, meta) in entries(&dir) {
            if !meta.is_file() {
                continue;
            }
            if meta.len() == 0 || meta.len() > MAX_IMAGE_BYTES || plan.len() >= MAX_IMPORT_IMAGES {
                skipped += 1;
                continue;
            }
            match std::fs::read(&path) {
                Ok(bytes) if image_mime(&bytes).is_some() => {
                    plan.push((label.clone(), file, bytes))
                }
                _ => skipped += 1,
            }
        }
    }
    let labels: std::collections::BTreeSet<&str> =
        plan.iter().map(|(l, _, _)| l.as_str()).collect();
    if labels.len() < 2 {
        return Err(format!(
            "`{}` needs at least two subfolders of images (folder name = label); found {}",
            src.display(),
            labels.len()
        ));
    }
    let dest = repo.join(ROOT).join(&id);
    let staging = repo.join(ROOT).join(format!(".{id}.importing"));
    let io = |e: std::io::Error| format!("can't write the set ({e})");
    let _ = std::fs::remove_dir_all(&staging);
    for (label, file, bytes) in &plan {
        let dir = staging.join(label);
        std::fs::create_dir_all(&dir).map_err(io)?;
        let mut path = dir.join(file);
        if !has_image_ext(&path) {
            let ext = match image_mime(bytes) {
                Some("image/png") => "png",
                Some("image/gif") => "gif",
                Some("image/webp") => "webp",
                _ => "jpg",
            };
            path.set_extension(ext);
        }
        std::fs::write(&path, bytes).map_err(io)?;
    }
    std::fs::write(
        staging.join("set.json"),
        json!({ "title": name.trim() }).to_string(),
    )
    .map_err(io)?;
    if dest.exists() {
        std::fs::remove_dir_all(&dest).map_err(io)?;
    }
    std::fs::rename(&staging, &dest).map_err(io)?;
    let set = list_sets(repo)
        .into_iter()
        .find(|s| s.id == id)
        .ok_or("the imported set has no usable images")?;
    Ok(SetImportReport { set, skipped })
}

/// The `image-set` built-in.
pub struct ImageSetPlugin {
    pkg: NativePackage,
    repo: PathBuf,
}

impl ImageSetPlugin {
    pub fn new(env: &NativeEnv) -> Self {
        Self::at(env.repo.clone())
    }

    pub fn at(repo: PathBuf) -> Self {
        let pkg = NativePackage::parse(
            include_str!("image-set/plugin.toml"),
            &[
                (
                    "schemas/image_set_list.json",
                    include_str!("image-set/schemas/image_set_list.json"),
                ),
                (
                    "schemas/image_set_drill.json",
                    include_str!("image-set/schemas/image_set_drill.json"),
                ),
                (
                    "prompts/tutor.md",
                    include_str!("image-set/prompts/tutor.md"),
                ),
                (
                    "prompts/practice.md",
                    include_str!("image-set/prompts/practice.md"),
                ),
                (
                    "views/drill.html",
                    include_str!("image-set/views/drill.html"),
                ),
            ],
        )
        .expect("the image-set package is valid");
        Self { pkg, repo }
    }
}

/// `image_set_list`: the sets as JSON, or how the learner adds one.
pub fn list_tool(repo: &Path) -> String {
    let sets = list_sets(repo);
    if sets.is_empty() {
        return format!(
            "No image sets yet. The learner adds one on a desktop with Settings → Libraries → Import image set… (a folder with one subfolder of images per label); it is copied into `{ROOT}/` in their data and syncs."
        );
    }
    serde_json::to_string_pretty(&sets).unwrap_or_default()
}

/// `image_set_drill`: the tool text (with the target) and the figure.
pub fn drill_tool(
    repo: &Path,
    args: &Value,
    rng: &mut Rng,
) -> Result<(String, ArtifactPayload), String> {
    let set_id = args
        .get("setId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("`setId` is required")?;
    let n = args
        .get("n")
        .and_then(Value::as_u64)
        .ok_or("`n` must be a whole number")? as usize;
    let format = args
        .get("format")
        .and_then(Value::as_str)
        .and_then(Format::parse)
        .ok_or("`format` must be classify, same-or-different or odd-one-out")?;
    let labels: Option<Vec<String>> = match args.get("labels") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            serde_json::from_value(v.clone())
                .map_err(|_| "`labels` must be a list of label names")?,
        ),
    };
    let set = load_set(repo, set_id)?;
    let choices: Vec<String> = chosen_labels(&set, labels.as_deref())?
        .into_iter()
        .map(str::to_owned)
        .collect();
    let items = plan_drill(&set, labels.as_deref(), n, format, rng)?;
    let drill = build_drill(set_id, &items, format, &choices)?;
    let svg = render(&drill.data)?;
    let text = format!(
        "Drill ready: {} {} item(s) from `{set_id}` over {}. Ask it with question_ask, view: {{ artifactId: <the id below>, task, target }}, with this target copied exactly (it is the answer key; never show or hint it):\ntarget: {}",
        items.len(),
        format.as_str(),
        choices.join(", "),
        drill.target
    );
    Ok((
        text,
        ArtifactPayload {
            kind: "svg".into(),
            svg,
            data: drill.data,
        },
    ))
}

impl Plugin for ImageSetPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.pkg.manifest
    }

    fn tools(&self) -> Vec<ToolSpec> {
        self.pkg.tools.clone()
    }

    fn fragment(&self, agent: &str) -> String {
        self.pkg.fragment(agent)
    }

    fn view_html(&self) -> Result<String, String> {
        self.pkg.view_html()
    }

    fn grade(&self, answer: &Value, target: &Value) -> Result<Value, String> {
        grade(answer, target)
    }

    fn call(self: Arc<Self>, name: &str, args: &Value, _ctx: PluginContext) -> ToolFuture {
        let repo = self.repo.clone();
        match self.pkg.entry(name) {
            Some("list") => blocking(move || done(list_tool(&repo))),
            Some("drill") => {
                let args = args.clone();
                blocking(move || match drill_tool(&repo, &args, &mut Rng::fresh()) {
                    Ok((text, artifact)) => {
                        let mut out = done(text);
                        out.artifact = Some(artifact);
                        out
                    }
                    Err(e) => fail(e),
                })
            }
            _ => ready(fail(format!("image-set has no tool `{name}`"))),
        }
    }
}

impl crate::Harness {
    /// The labelled image sets in the data repo (`image_set_list`'s view).
    pub fn image_sets(&self) -> Vec<SetInfo> {
        list_sets(&self.repo)
    }

    /// Copy a learner's labelled folder (subfolder name = label) into the
    /// data repo as an image set named `name` (default: the folder name).
    /// Desktop only, like library import; blocking.
    pub fn image_set_import(
        &self,
        path: &str,
        name: Option<&str>,
    ) -> Result<SetImportReport, crate::Error> {
        use crate::Error;
        if self.is_mobile() {
            return Err(Error::Invalid(
                "image sets are imported on a desktop; this device uses the synced copies".into(),
            ));
        }
        let raw = crate::files::expand_home(path.trim());
        if !raw.is_absolute() {
            return Err(Error::Invalid(format!("`{path}` is not an absolute path")));
        }
        let source = raw
            .canonicalize()
            .map_err(|e| Error::Invalid(format!("can't open `{path}` ({e})")))?;
        let repo = self
            .repo
            .canonicalize()
            .unwrap_or_else(|_| self.repo.clone());
        if repo.starts_with(&source) || source.starts_with(&repo) {
            return Err(Error::Invalid(format!(
                "`{path}` contains or is inside the app's data folder; pick the labelled folder itself"
            )));
        }
        let folder = source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = name
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or(&folder);
        import(&self.repo, &source, name).map_err(Error::Invalid)
    }

    /// Delete image set `id` from the data repo (every device, at the next
    /// sync). The folder it was imported from is not touched.
    pub fn image_set_remove(&self, id: &str) -> Result<(), crate::Error> {
        let dir = self.repo.join(ROOT).join(id);
        if !valid_id(id) || !dir.is_dir() {
            return Err(crate::Error::NotFound(format!("image set `{id}`")));
        }
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lh-imageset-{name}-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn png(w: u32, h: u32, shade: u8) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(w, h, image::Rgb([shade, 0, 0]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    /// `<repo>/image-sets/<id>/<label>/<label>-<i>.png`, `per` images each.
    fn make_set(repo: &Path, id: &str, labels: &[&str], per: usize) {
        for (li, label) in labels.iter().enumerate() {
            let dir = repo.join(ROOT).join(id).join(label);
            std::fs::create_dir_all(&dir).unwrap();
            for i in 0..per {
                std::fs::write(
                    dir.join(format!("{label}-{i}.png")),
                    png(4, 4, (li * 40 + i) as u8),
                )
                .unwrap();
            }
        }
    }

    #[test]
    fn listing_counts_usable_images_per_label_and_skips_the_rest() {
        let repo = scratch("list");
        make_set(&repo, "birds", &["crow", "raven"], 3);
        let set = repo.join(ROOT).join("birds");
        std::fs::write(set.join("crow/notes.txt"), "x").unwrap();
        std::fs::write(set.join("crow/.hidden.png"), png(2, 2, 1)).unwrap();
        std::fs::write(set.join("crow/empty.png"), b"").unwrap();
        std::fs::create_dir_all(set.join("empty-label")).unwrap();
        std::fs::create_dir_all(set.join(".git/objects")).unwrap();
        std::fs::write(set.join("set.json"), r#"{"title":"Corvids"}"#).unwrap();
        std::fs::create_dir_all(repo.join(ROOT).join("nothing/also-nothing")).unwrap();
        make_set(&repo, "art", &["baroque"], 1);

        let sets = list_sets(&repo);
        assert_eq!(sets.len(), 2, "{sets:?}");
        assert_eq!(sets[0].id, "art");
        assert_eq!(sets[0].title, "art");
        assert_eq!(sets[1].title, "Corvids");
        assert_eq!(sets[1].size, 6);
        assert_eq!(
            sets[1].labels,
            vec![
                LabelInfo {
                    label: "crow".into(),
                    count: 3
                },
                LabelInfo {
                    label: "raven".into(),
                    count: 3
                }
            ]
        );
        assert!(list_sets(&scratch("none")).is_empty());
        let err = load_set(&repo, "../birds").unwrap_err();
        assert!(err.contains("available sets: art, birds"), "{err}");
    }

    #[test]
    fn classify_interleaves_labels_and_keys_each_image_with_its_folder() {
        let repo = scratch("classify");
        make_set(&repo, "s", &["a", "b", "c"], 2);
        let set = load_set(&repo, "s").unwrap();
        for seed in 1..20 {
            let items =
                plan_drill(&set, None, 10, Format::Classify, &mut Rng::seeded(seed)).unwrap();
            assert_eq!(items.len(), 10);
            for w in items.windows(2) {
                assert_ne!(w[0].key, w[1].key, "consecutive items share a label");
            }
            for it in &items {
                let (label, path) = &it.images[0];
                assert_eq!(it.key, json!(label));
                assert_eq!(
                    path.parent()
                        .unwrap()
                        .file_name()
                        .unwrap()
                        .to_str()
                        .unwrap(),
                    label
                );
            }
        }
        // A label filter keeps only those labels.
        let only: Vec<String> = vec!["a".into(), "c".into()];
        let items =
            plan_drill(&set, Some(&only), 6, Format::Classify, &mut Rng::seeded(3)).unwrap();
        assert!(items.iter().all(|i| i.key != json!("b")));
        let err = plan_drill(
            &set,
            Some(&["a".into(), "z".into()]),
            2,
            Format::Classify,
            &mut Rng::seeded(1),
        )
        .unwrap_err();
        assert!(err.contains("no label `z`"), "{err}");
        let err = plan_drill(
            &set,
            Some(&["a".into()]),
            2,
            Format::Classify,
            &mut Rng::seeded(1),
        )
        .unwrap_err();
        assert!(err.contains("at least two labels"), "{err}");
        let err = plan_drill(&set, None, 13, Format::OddOneOut, &mut Rng::seeded(1)).unwrap_err();
        assert!(err.contains("at most 12 odd-one-out items"), "{err}");
    }

    #[test]
    fn pair_and_triple_keys_match_the_labels_shown() {
        let repo = scratch("pairs");
        make_set(&repo, "s", &["a", "b"], 3);
        let set = load_set(&repo, "s").unwrap();
        let items =
            plan_drill(&set, None, 8, Format::SameOrDifferent, &mut Rng::seeded(7)).unwrap();
        let same = items.iter().filter(|i| i.key == json!("same")).count();
        assert_eq!(same, 4, "half the pairs are same-label");
        for it in &items {
            let [(la, pa), (lb, pb)] = [&it.images[0], &it.images[1]];
            assert_eq!(it.key == json!("same"), la == lb);
            assert_ne!(pa, pb, "a pair never shows one image twice");
        }
        let items = plan_drill(&set, None, 6, Format::OddOneOut, &mut Rng::seeded(9)).unwrap();
        for it in &items {
            let odd = it.key.as_u64().unwrap() as usize;
            let labels: Vec<&String> = it.images.iter().map(|(l, _)| l).collect();
            let others: Vec<&&String> = labels
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != odd)
                .map(|(_, l)| l)
                .collect();
            assert_eq!(others[0], others[1]);
            assert_ne!(labels[odd], *others[0]);
        }
    }

    #[test]
    fn drill_data_hides_labels_and_large_images_are_downscaled() {
        let repo = scratch("data");
        make_set(&repo, "s", &["secretalpha", "secretbeta"], 2);
        std::fs::write(
            repo.join(ROOT).join("s/secretalpha/big.png"),
            png(1200, 800, 9),
        )
        .unwrap();
        let set = load_set(&repo, "s").unwrap();
        let items = plan_drill(&set, None, 6, Format::Classify, &mut Rng::seeded(5)).unwrap();
        let choices: Vec<String> = set.keys().cloned().collect();
        let drill = build_drill("s", &items, Format::Classify, &choices).unwrap();
        // The view sees the choices but not which image is which.
        let mut data = drill.data.clone();
        data["choices"] = json!([]);
        assert!(
            !data.to_string().contains("secret"),
            "labels leaked into the view data"
        );
        assert_eq!(drill.target["key"].as_array().unwrap().len(), 6);

        let uri = data_uri(&repo.join(ROOT).join("s/secretalpha/big.png"), EMBED_BUDGET).unwrap();
        let b64 = uri
            .strip_prefix("data:image/jpeg;base64,")
            .expect("downscaled to JPEG");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .unwrap();
        let img = image::load_from_memory(&bytes).unwrap();
        assert_eq!((img.width(), img.height()), (DISPLAY_PX, 240));
        let small = data_uri(
            &repo.join(ROOT).join("s/secretbeta/secretbeta-0.png"),
            EMBED_BUDGET,
        )
        .unwrap();
        assert!(small.starts_with("data:image/png;base64,"));
        std::fs::write(repo.join("fake.png"), b"not an image").unwrap();
        assert!(data_uri(&repo.join("fake.png"), EMBED_BUDGET).is_err());

        let svg = render(&drill.data).unwrap();
        assert_eq!(svg.matches("<image ").count(), 6);
        let clean = crate::artifacts::sanitize_svg(&svg).unwrap();
        assert_eq!(
            clean.matches("data:image/").count(),
            6,
            "the sanitizer keeps data: images"
        );
    }

    /// Pixel noise compresses badly, so it exercises the size budget.
    fn noise_png(w: u32, h: u32, seed: u32) -> Vec<u8> {
        let mut s = seed.wrapping_mul(2_654_435_761) | 1;
        let img = image::RgbImage::from_fn(w, h, |_, _| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            let b = s.to_le_bytes();
            image::Rgb([b[0], b[1], b[2]])
        });
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn a_full_drill_of_large_images_fits_the_artifact_caps() {
        let repo = scratch("budget");
        for (li, label) in ["a", "b", "c"].iter().enumerate() {
            let dir = repo.join(ROOT).join("s").join(label);
            std::fs::create_dir_all(&dir).unwrap();
            for i in 0..4 {
                std::fs::write(
                    dir.join(format!("{i}.png")),
                    noise_png(600, 400, (li * 10 + i + 1) as u32),
                )
                .unwrap();
            }
        }
        let set = load_set(&repo, "s").unwrap();
        let items = plan_drill(&set, None, 12, Format::OddOneOut, &mut Rng::seeded(2)).unwrap();
        let drill = build_drill("s", &items, Format::OddOneOut, &[]).unwrap();
        let svg = render(&drill.data).unwrap();
        assert!(
            svg.len() <= crate::artifacts::MAX_SVG_BYTES,
            "svg is {} bytes",
            svg.len()
        );
        assert!(drill.data.to_string().len() <= 256 * 1024);
        assert_eq!(
            crate::artifacts::sanitize_svg(&svg)
                .unwrap()
                .matches("<image")
                .count(),
            36
        );

        // One image under a tight budget shrinks below the display size.
        let uri = data_uri(&repo.join(ROOT).join("s/a/0.png"), 6000).unwrap();
        assert!(uri.len() <= 6000, "{}", uri.len());
        let b64 = uri.strip_prefix("data:image/jpeg;base64,").unwrap();
        let img = image::load_from_memory(
            &base64::engine::general_purpose::STANDARD
                .decode(b64)
                .unwrap(),
        )
        .unwrap();
        assert!(img.width() < DISPLAY_PX);
        let gif = b"GIF89a"
            .iter()
            .copied()
            .chain(std::iter::repeat_n(0, 4000))
            .collect::<Vec<u8>>();
        let err = encode_fit(&gif, "image/gif", 1000).unwrap_err();
        assert!(err.contains("GIF or WebP"), "{err}");
    }

    /// A target as `build_drill` would sign it.
    fn signed(format: &str, key: Value, labels: Value) -> Value {
        let check = target_check(format, &key, &labels);
        json!({ "format": format, "key": key, "labels": labels, "check": check })
    }

    #[test]
    fn grading_scores_items_and_counts_confusions() {
        let target = signed(
            "classify",
            json!(["crow", "raven", "crow", "rook"]),
            json!([["crow"], ["raven"], ["crow"], ["rook"]]),
        );
        let r = grade(
            &json!({ "answers": ["raven", "raven", "raven", "rook"] }),
            &target,
        )
        .unwrap();
        assert_eq!(r["correct"], json!(false));
        assert_eq!(r["score"], json!(2));
        assert_eq!(
            r["confusions"],
            json!([{ "expected": "crow", "given": "raven", "count": 2 }])
        );
        assert_eq!(r["detail"], json!("2/4 correct; confused: crow → raven ×2"));
        assert_eq!(
            r["items"][1],
            json!({ "item": 2, "expected": "raven", "given": "raven", "correct": true })
        );
        let all = grade(
            &json!({ "answers": ["crow", "raven", "crow", "rook"] }),
            &target,
        )
        .unwrap();
        assert_eq!(all["correct"], json!(true));
        assert_eq!(all["confusions"], json!([]));
        let err = grade(&json!({ "answers": ["crow"] }), &target).unwrap_err();
        assert!(err.contains("expected 4 answers, got 1"), "{err}");
        assert!(grade(&json!({ "answers": [] }), &json!({ "probe": "Vout" })).is_err());

        // A miscopied key is refused, not graded.
        let mut tampered = target.clone();
        tampered["key"][0] = json!("raven");
        let err = grade(
            &json!({ "answers": ["raven", "raven", "crow", "rook"] }),
            &tampered,
        )
        .unwrap_err();
        assert!(err.contains("not copied exactly"), "{err}");

        let pairs = signed(
            "same-or-different",
            json!(["different", "same"]),
            json!([["crow", "raven"], ["crow", "crow"]]),
        );
        let r = grade(&json!({ "answers": ["same", "different"] }), &pairs).unwrap();
        assert_eq!(r["score"], json!(0));
        assert_eq!(
            r["confusions"],
            json!([{ "expected": "crow", "given": "raven", "count": 1 }])
        );

        let odd = signed(
            "odd-one-out",
            json!([2]),
            json!([["crow", "crow", "raven"]]),
        );
        let r = grade(&json!({ "answers": [0] }), &odd).unwrap();
        assert_eq!(
            r["confusions"],
            json!([{ "expected": "raven", "given": "crow", "count": 1 }])
        );
        assert_eq!(r["items"][0]["expected"], json!("C (raven)"));
        assert_eq!(
            grade(&json!({ "answers": [2] }), &odd).unwrap()["correct"],
            json!(true)
        );
    }

    #[test]
    fn import_copies_real_images_by_label_and_replaces_the_set() {
        let repo = scratch("import-repo");
        let src = scratch("import-src");
        std::fs::create_dir_all(src.join("Gothic")).unwrap();
        std::fs::create_dir_all(src.join("Romanesque")).unwrap();
        std::fs::write(src.join("Gothic/a.png"), png(3, 3, 1)).unwrap();
        std::fs::write(src.join("Gothic/b"), png(3, 3, 2)).unwrap();
        std::fs::write(src.join("Gothic/fake.jpg"), b"text").unwrap();
        std::fs::write(src.join("Romanesque/c.png"), png(3, 3, 3)).unwrap();
        std::fs::write(src.join("loose.png"), png(3, 3, 4)).unwrap();

        let report = import(&repo, &src, "Church styles!").unwrap();
        assert_eq!(report.set.id, "church-styles");
        assert_eq!(report.set.title, "Church styles!");
        assert_eq!(report.skipped, 1);
        assert_eq!(
            report.set.labels,
            vec![
                LabelInfo {
                    label: "Gothic".into(),
                    count: 2
                },
                LabelInfo {
                    label: "Romanesque".into(),
                    count: 1
                }
            ]
        );
        assert!(
            repo.join(ROOT).join("church-styles/Gothic/b.png").is_file(),
            "extension added from content"
        );
        assert!(src.join("Gothic/b").is_file(), "the source is untouched");

        std::fs::remove_dir_all(src.join("Romanesque")).unwrap();
        let err = import(&repo, &src, "Church styles").unwrap_err();
        assert!(err.contains("at least two subfolders"), "{err}");
        assert_eq!(
            list_sets(&repo)[0].size,
            3,
            "a failed import leaves the old set"
        );
        std::fs::create_dir_all(src.join("Baroque")).unwrap();
        std::fs::write(src.join("Baroque/d.png"), png(3, 3, 5)).unwrap();
        let again = import(&repo, &src, "church styles").unwrap();
        assert_eq!(
            again
                .set
                .labels
                .iter()
                .map(|l| l.label.as_str())
                .collect::<Vec<_>>(),
            ["Baroque", "Gothic"]
        );
        assert!(import(&repo, &src, "  !!  ").is_err());
    }

    /// The tutor's path: list, drill, copy the target from the tool text
    /// into the question, the view submits answers, the plugin grades.
    #[tokio::test]
    async fn the_drill_tool_text_carries_a_target_the_plugin_grades() {
        let repo = scratch("tool");
        let plugin = Arc::new(ImageSetPlugin::at(repo.clone()));
        assert!(plugin.view_html().unwrap().contains("type: 'submit'"));
        assert!(plugin.fragment("practice").contains("image_set_drill"));
        let ctx = PluginContext {
            session_id: "s".into(),
            platform: "desktop".into(),
        };
        let empty = Arc::clone(&plugin)
            .call("image_set_list", &json!({}), ctx.clone())
            .await;
        assert!(
            empty.content.starts_with("No image sets yet"),
            "{}",
            empty.content
        );

        make_set(&repo, "birds", &["crow", "raven", "rook"], 3);
        let listed = Arc::clone(&plugin)
            .call("image_set_list", &json!({}), ctx.clone())
            .await;
        let sets: Value = serde_json::from_str(&listed.content).unwrap();
        assert_eq!(sets[0]["labels"][2], json!({ "label": "rook", "count": 3 }));

        let args = json!({ "setId": "birds", "labels": ["crow", "raven"], "n": 5, "format": "classify", "interleave": true });
        let out = Arc::clone(&plugin)
            .call("image_set_drill", &args, ctx.clone())
            .await;
        assert!(!out.is_error, "{}", out.content);
        let artifact = out.artifact.expect("a drill figure");
        assert_eq!(artifact.data["choices"], json!(["crow", "raven"]));
        assert_eq!(artifact.data["items"].as_array().unwrap().len(), 5);
        let line = out
            .content
            .lines()
            .find_map(|l| l.strip_prefix("target: "))
            .unwrap();
        let target: Value = serde_json::from_str(line).unwrap();
        let key = target["key"].clone();
        let r = plugin.grade(&json!({ "answers": key }), &target).unwrap();
        assert_eq!(r["correct"], json!(true));
        assert_eq!(r["score"], json!(5));

        let missing = Arc::clone(&plugin)
            .call(
                "image_set_drill",
                &json!({ "setId": "fish", "n": 2, "format": "classify", "interleave": true }),
                ctx,
            )
            .await;
        assert!(
            missing.is_error && missing.content.contains("available sets: birds"),
            "{}",
            missing.content
        );
    }
}
