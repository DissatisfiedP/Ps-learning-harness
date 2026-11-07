//! PDF reading: the page text of a text-layer PDF, and the page image of a
//! scan.
//!
//! There is no OCR here and none anywhere else: a PDF page is read either as
//! its text layer (when the page has one) or as an image the vision model
//! looks at. A page carries text when it was born digital; a scan carries
//! only pixels, so [`PageText::scanned`] marks pages whose extracted text is
//! too short to be a real text layer, and the file tools say so plainly
//! instead of pretending the page is empty.
//!
//! Everything here takes bytes and returns strings or image bytes: the model
//! never sees a PDF. Errors are written for the model (what failed, which cap
//! was hit, what to do instead), and a malformed file yields an error, never
//! a panic. The caps are the constants below: 32 MiB of input, 400 pages,
//! 64 KiB of text per page, 512 KiB of text per whole-document read, and
//! 8 MiB per page image. Encrypted files are refused; there is no password
//! support.

use std::collections::BTreeMap;
use std::io::{Cursor, Read};

use lopdf::{Dictionary, Document, Object, ObjectId, Stream};

/// Largest PDF accepted, in bytes.
pub const MAX_PDF_BYTES: usize = 32 * 1024 * 1024;
/// Most pages read from one document.
pub const MAX_PAGES: usize = 400;
/// Most text kept from one page.
pub const MAX_PAGE_TEXT: usize = 64 * 1024;
/// Most text read from one document at a time (a page range may be read
/// instead of the whole file).
pub const MAX_TOTAL_TEXT: usize = 512 * 1024;
/// Largest page image kept, in bytes.
pub const MAX_PAGE_IMAGE: usize = 8 * 1024 * 1024;
/// Most raw image bytes decoded for one page (a Flate bomb would otherwise
/// ask for gigabytes).
const MAX_DECODED_IMAGE: usize = 64 * 1024 * 1024;
/// A page whose trimmed text is shorter than this is probably a scan.
const SCAN_TEXT_CHARS: usize = 40;

/// What a page says in the text layer: the text, and whether the page is
/// probably a scan (almost no text, so the words live in the pixels).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageText {
    /// 1-based, as a reader sees it.
    pub number: u32,
    pub text: String,
    pub scanned: bool,
}

/// One page as an image the model can look at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageImage {
    pub bytes: Vec<u8>,
    pub media_type: &'static str,
}

/// True when the bytes carry a PDF header: `%PDF-` and a version digit, at
/// the start or after a few bytes of junk some writers prepend. The version
/// digit keeps a note that merely mentions `%PDF-` from being taken for one.
pub fn is_pdf(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(1024)];
    let Some(at) = head.windows(5).position(|w| w == b"%PDF-") else {
        return false;
    };
    at <= 64 && head.get(at + 5).is_some_and(|b| b.is_ascii_digit())
}

fn mib(bytes: usize) -> String {
    let kib = (bytes as f64) / 1024.0;
    if kib < 1024.0 {
        format!("{} KiB", kib.ceil() as u64)
    } else {
        format!("{:.1} MiB", kib / 1024.0)
    }
}

/// Load a document, refusing what this module can't read. Every error is a
/// sentence for the model.
fn document(bytes: &[u8]) -> Result<Document, String> {
    if bytes.is_empty() {
        return Err("the PDF is empty".into());
    }
    if bytes.len() > MAX_PDF_BYTES {
        return Err(format!(
            "this PDF is {}, over the {} cap; ask the learner for the pages you need",
            mib(bytes.len()),
            mib(MAX_PDF_BYTES)
        ));
    }
    if !is_pdf(bytes) {
        return Err("this file isn't a PDF (no `%PDF` header)".into());
    }
    let doc = Document::load_mem(bytes).map_err(|e| format!("this PDF can't be read: {e}"))?;
    if doc.is_encrypted() {
        return Err(
            "this PDF is encrypted and passwords aren't supported; ask the learner for an unlocked copy"
                .into(),
        );
    }
    Ok(doc)
}

/// Every page of the document, with the input, page-count and per-page caps
/// applied. The whole-text cap is [`pages`]' business: callers that keep only
/// matches or first lines use this and emit little.
pub(crate) fn extract(bytes: &[u8]) -> Result<Vec<PageText>, String> {
    let doc = document(bytes)?;
    let count = doc.get_pages().len();
    if count == 0 {
        return Err("this PDF has no pages".into());
    }
    if count > MAX_PAGES {
        return Err(format!(
            "this PDF has {count} pages, over the {MAX_PAGES}-page cap; ask the learner for the pages you need"
        ));
    }
    drop(doc);
    // A malformed file can make the extractor panic; a panic is not an answer
    // the model can act on, so it becomes one.
    let texts = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem_by_pages(bytes))
        .map_err(|_| "this PDF is malformed: its text couldn't be extracted".to_owned())?
        .map_err(|e| format!("this PDF's text couldn't be read: {e}"))?;
    let mut out = Vec::with_capacity(texts.len());
    for (i, raw) in texts.into_iter().enumerate() {
        let number = i as u32 + 1;
        // Extractors pad pages with blank lines; a page's text is its words.
        let text = raw.trim().to_owned();
        if text.len() > MAX_PAGE_TEXT {
            return Err(format!(
                "page {number} has {} of text, over the {} per-page cap; this PDF's text layer is unusable",
                mib(text.len()),
                mib(MAX_PAGE_TEXT)
            ));
        }
        let scanned = text.chars().count() < SCAN_TEXT_CHARS;
        out.push(PageText {
            number,
            text,
            scanned,
        });
    }
    Ok(out)
}

/// Every page's text; refused when the document's text passes the
/// [`MAX_TOTAL_TEXT`] cap (read a page range with [`pages_in`] instead).
pub fn pages(bytes: &[u8]) -> Result<Vec<PageText>, String> {
    let pages = extract(bytes)?;
    cap_total(&pages)?;
    Ok(pages)
}

/// Pages `start..=end` (1-based, `end` past the last page is fine) plus the
/// document's page count, so a caller can say "pages 3-5 of 42". Refused
/// when those pages' text passes the [`MAX_TOTAL_TEXT`] cap, so a page range
/// works on a document too long to read whole.
pub fn pages_in(bytes: &[u8], start: u32, end: u32) -> Result<(Vec<PageText>, u32), String> {
    if start == 0 {
        return Err("page numbers start at 1".into());
    }
    let end = end.max(start);
    let mut pages = extract(bytes)?;
    let count = pages.len() as u32;
    if start > count {
        return Err(format!(
            "page {start} doesn't exist: this PDF has {count} pages"
        ));
    }
    pages.retain(|p| p.number >= start && p.number <= end);
    cap_total(&pages)?;
    Ok((pages, count))
}

fn cap_total(pages: &[PageText]) -> Result<(), String> {
    let total: usize = pages.iter().map(|p| p.text.len()).sum();
    if total > MAX_TOTAL_TEXT {
        return Err(format!(
            "this PDF's text is {}, over the {} cap; read a page range instead (`pages: \"3-5\"`)",
            mib(total),
            mib(MAX_TOTAL_TEXT)
        ));
    }
    Ok(())
}

/// The line that stands in for a page with no text layer: it says what the
/// page is and which tool turns it into something readable.
pub fn scan_page_line(page: u32) -> String {
    format!(
        "(no text layer on this page: it is a scan, so its words are pixels — `source_page_image` fetches page {page} as an image)"
    )
}

/// Page-tagged text as a reader sees it: `[p.3] …` for every page, stopping
/// at `budget` bytes with a note that names the cap.
pub fn tagged(pages: &[PageText], budget: usize) -> String {
    let mut out = String::new();
    let mut cut_at = None;
    for p in pages {
        let body = if p.scanned {
            scan_page_line(p.number)
        } else {
            p.text.trim_end().to_owned()
        };
        let block = format!("[p.{}] {body}\n", p.number);
        if !out.is_empty() && out.len() + block.len() > budget {
            cut_at = Some(p.number);
            break;
        }
        out.push_str(&block);
    }
    if let Some(page) = cut_at {
        out.push_str(&format!(
            "\n[stopped before page {page}: {} cap]\n",
            mib(budget)
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Page images
// ---------------------------------------------------------------------------

fn num(dict: &Dictionary, key: &[u8]) -> Option<f64> {
    match dict.get(key).ok()? {
        Object::Integer(i) => Some(*i as f64),
        Object::Real(r) => Some(*r as f64),
        _ => None,
    }
}

fn name(dict: &Dictionary, key: &[u8]) -> Option<String> {
    dict.get(key)
        .and_then(Object::as_name)
        .ok()
        .map(|n| String::from_utf8_lossy(n).into_owned())
}

/// An embedded image of one page.
///
/// The page's `/XObject` images are walked with `lopdf`; the largest by
/// width × height wins (a page usually carries one picture, but a scan may be
/// split into strips). `DCTDecode` streams are JPEG already and pass through;
/// 8-bit `FlateDecode` DeviceRGB/DeviceGray samples are re-encoded as PNG.
/// Every other encoding (JPX, CCITT, JBIG2, indexed or CMYK samples) is
/// refused by name: showing it wrong would be worse than saying so.
pub fn page_image(bytes: &[u8], page: u32) -> Result<PageImage, String> {
    let doc = document(bytes)?;
    let by_number: BTreeMap<u32, ObjectId> = doc.get_pages();
    let count = by_number.len() as u32;
    if page == 0 {
        return Err("page numbers start at 1".into());
    }
    let Some(page_id) = by_number.get(&page).copied() else {
        return Err(format!(
            "page {page} doesn't exist: this PDF has {count} pages"
        ));
    };
    let streams = page_images(&doc, page_id, page)?;
    let Some(stream) = streams.into_iter().max_by_key(|(w, h, _)| w * h) else {
        return Err(format!(
            "page {page} has no embedded image, so there is nothing to show"
        ));
    };
    let (_, _, stream) = stream;
    let filters = stream
        .filters()
        .map(|fs| {
            fs.iter()
                .map(|f| String::from_utf8_lossy(f).into_owned())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if filters.len() == 1 && filters[0] == "DCTDecode" {
        if stream.content.len() > MAX_PAGE_IMAGE {
            return Err(format!(
                "page {page}'s image is {}, over the {} image cap",
                mib(stream.content.len()),
                mib(MAX_PAGE_IMAGE)
            ));
        }
        return Ok(PageImage {
            bytes: stream.content.clone(),
            media_type: "image/jpeg",
        });
    }
    let raw = filters.is_empty() || filters.iter().all(|f| f == "FlateDecode");
    if !raw {
        return Err(format!(
            "page {page}'s image uses an encoding this reader can't show ({}); only JPEG and Flate-compressed RGB or gray pages can be shown as images",
            filters.join(", ")
        ));
    }
    if let Ok(params) = stream.dict.get(b"DecodeParms").and_then(Object::as_dict) {
        if num(params, b"Predictor").is_some_and(|p| p > 1.0) {
            return Err(format!(
                "page {page}'s image uses a PNG predictor, which this reader can't show"
            ));
        }
    }
    let width = num(&stream.dict, b"Width").unwrap_or(0.0);
    let height = num(&stream.dict, b"Height").unwrap_or(0.0);
    if width <= 0.0 || height <= 0.0 || width > f64::from(u32::MAX) || height > f64::from(u32::MAX)
    {
        return Err(format!("page {page}'s image has no usable size"));
    }
    let (width, height) = (width as u32, height as u32);
    let channels = match name(&stream.dict, b"ColorSpace").as_deref() {
        Some("DeviceRGB") => 3usize,
        Some("DeviceGray") => 1,
        Some(other) => {
            return Err(format!(
                "page {page}'s image uses colour space `{other}`; only DeviceRGB and DeviceGray can be shown"
            ))
        }
        None => {
            return Err(format!(
                "page {page}'s image doesn't say which colour space it uses"
            ))
        }
    };
    let bpc = num(&stream.dict, b"BitsPerComponent").unwrap_or(0.0);
    if bpc != 8.0 {
        return Err(format!(
            "page {page}'s image has {} bits per channel; only 8-bit images can be shown",
            if bpc > 0.0 {
                bpc.to_string()
            } else {
                "no".into()
            }
        ));
    }
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|px| px.checked_mul(channels))
        .ok_or_else(|| format!("page {page}'s image size overflows"))?;
    if expected > MAX_DECODED_IMAGE {
        return Err(format!(
            "page {page}'s image is {width}×{height}, over the {} image cap",
            mib(MAX_DECODED_IMAGE)
        ));
    }
    let data = if filters.is_empty() {
        stream.content.clone()
    } else {
        let mut out = Vec::with_capacity(expected);
        flate2::read::ZlibDecoder::new(stream.content.as_slice())
            .take(expected as u64 + 1)
            .read_to_end(&mut out)
            .map_err(|e| format!("page {page}'s image couldn't be decompressed: {e}"))?;
        out
    };
    if data.len() != expected {
        return Err(format!(
            "page {page}'s image data doesn't match its {width}×{height} size"
        ));
    }
    let mut png = Cursor::new(Vec::new());
    if channels == 3 {
        let img = image::RgbImage::from_raw(width, height, data)
            .ok_or_else(|| format!("page {page}'s image data doesn't match its size"))?;
        img.write_to(&mut png, image::ImageFormat::Png)
            .map_err(|e| format!("page {page}'s image couldn't be converted: {e}"))?;
    } else {
        let img = image::GrayImage::from_raw(width, height, data)
            .ok_or_else(|| format!("page {page}'s image data doesn't match its size"))?;
        img.write_to(&mut png, image::ImageFormat::Png)
            .map_err(|e| format!("page {page}'s image couldn't be converted: {e}"))?;
    }
    let bytes = png.into_inner();
    if bytes.len() > MAX_PAGE_IMAGE {
        return Err(format!(
            "page {page}'s image is {}, over the {} image cap",
            mib(bytes.len()),
            mib(MAX_PAGE_IMAGE)
        ));
    }
    Ok(PageImage {
        bytes,
        media_type: "image/png",
    })
}

/// The image XObjects a page draws, with their declared size, largest first
/// is the caller's choice.
fn page_images(
    doc: &Document,
    page_id: ObjectId,
    page: u32,
) -> Result<Vec<(u64, u64, &Stream)>, String> {
    let (inline, resource_ids) = doc
        .get_page_resources(page_id)
        .map_err(|e| format!("page {page}'s resources can't be read: {e}"))?;
    let mut resources: Vec<&Dictionary> = Vec::new();
    if let Some(dict) = inline {
        resources.push(dict);
    }
    for id in &resource_ids {
        if let Ok(dict) = doc.get_dictionary(*id) {
            resources.push(dict);
        }
    }
    let mut out = Vec::new();
    for dict in resources {
        let Ok(xobjects) = dict.get(b"XObject") else {
            continue;
        };
        let xobjects = match xobjects {
            Object::Reference(id) => doc.get_dictionary(*id).ok(),
            Object::Dictionary(d) => Some(d),
            _ => None,
        };
        let Some(xobjects) = xobjects else { continue };
        for (_, value) in xobjects.iter() {
            let stream = match value {
                Object::Reference(id) => doc.get_object(*id).and_then(Object::as_stream).ok(),
                Object::Stream(s) => Some(s),
                _ => None,
            };
            let Some(stream) = stream else { continue };
            if name(&stream.dict, b"Subtype").as_deref() != Some("Image") {
                continue;
            }
            let w = num(&stream.dict, b"Width").unwrap_or(0.0).max(0.0) as u64;
            let h = num(&stream.dict, b"Height").unwrap_or(0.0).max(0.0) as u64;
            out.push((w, h, stream));
        }
    }
    Ok(out)
}

/// Test-only PDFs, built with `lopdf` so no binary fixture lives in the repo.
#[cfg(test)]
pub(crate) mod fixtures {
    use lopdf::{dictionary, Document, Object, Stream};

    /// A PDF with one text line per given page; an empty string is a blank
    /// page, which is what a scan looks like in the text layer.
    pub(crate) fn text_pdf(lines: &[&str]) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
        });
        let resources = dictionary! { "Font" => dictionary! { "F1" => font_id } };
        let mut kids = Vec::new();
        for line in lines {
            let content = Stream::new(
                dictionary! {},
                format!("BT /F1 18 Tf 72 720 Td ({line}) Tj ET").into_bytes(),
            );
            let content_id = doc.add_object(content);
            let page = doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
                "Contents" => content_id,
                "Resources" => resources.clone(),
            });
            kids.push(Object::Reference(page));
        }
        let count = kids.len() as i64;
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => kids, "Count" => count,
            }),
        );
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);
        let mut out = Vec::new();
        doc.save_to(&mut out).unwrap();
        out
    }

    /// A one-page PDF whose one image is `stream`, with the given resource
    /// name and size.
    pub(crate) fn image_pdf(stream: Stream) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let image_id = doc.add_object(stream);
        let content = Stream::new(
            dictionary! {},
            b"q 200 0 0 200 50 100 cm /Im1 Do Q".to_vec(),
        );
        let content_id = doc.add_object(content);
        let page = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            "Contents" => content_id,
            "Resources" => dictionary! { "XObject" => dictionary! { "Im1" => image_id } },
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page)],
                "Count" => 1,
            }),
        );
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);

        let mut out = Vec::new();
        doc.save_to(&mut out).unwrap();
        out
    }

    /// A tiny baseline JPEG, encoded in-test so the fixture needs no binary.
    pub(crate) fn jpeg(w: u32, h: u32) -> Vec<u8> {
        let img =
            image::RgbImage::from_fn(w, h, |x, y| image::Rgb([x as u8 * 20, y as u8 * 20, 90]));
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut out)
            .encode_image(&img)
            .unwrap();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{image_pdf, jpeg, text_pdf};
    use super::*;
    use lopdf::dictionary;
    use std::io::Write;

    #[test]
    fn reads_page_text_with_page_numbers() {
        let pdf = text_pdf(&[
            "Alpha page one carries plenty of words to be a text layer",
            "Beta page two also carries plenty of words for a text layer",
        ]);
        let pages = pages(&pdf).unwrap();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].number, 1);
        assert_eq!(pages[1].number, 2);
        assert!(pages[0].text.contains("Alpha"), "{:?}", pages[0].text);
        assert!(pages[1].text.contains("Beta"), "{:?}", pages[1].text);
        assert!(!pages[0].scanned);
        let tagged = tagged(&pages, MAX_TOTAL_TEXT);
        assert!(tagged.contains("[p.1]"), "{tagged}");
        assert!(tagged.contains("[p.2]"), "{tagged}");
    }

    #[test]
    fn a_page_with_no_text_layer_is_marked_scanned() {
        let pdf = text_pdf(&[
            "",
            "Some real text on the second page that is long enough to count",
        ]);
        let pages = pages(&pdf).unwrap();
        assert!(pages[0].scanned);
        assert!(!pages[1].scanned);
        let tagged = tagged(&pages, MAX_TOTAL_TEXT);
        assert!(tagged.contains("no text layer"), "{tagged}");
        assert!(tagged.contains("source_page_image"), "{tagged}");
    }

    #[test]
    fn a_flate_rgb_image_is_re_encoded_as_png() {
        let (w, h) = (3u32, 2u32);
        let raw: Vec<u8> = (0..(w * h * 3) as u8).map(|i| i * 3).collect();
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&raw).unwrap();
        let data = enc.finish().unwrap();
        let pdf = image_pdf(Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Image",
                "Width" => w as i64, "Height" => h as i64,
                "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8,
                "Filter" => "FlateDecode",
            },
            data,
        ));
        let image = page_image(&pdf, 1).unwrap();
        assert_eq!(image.media_type, "image/png");
        assert_eq!(&image.bytes[..8], b"\x89PNG\r\n\x1a\n");
        let decoded = image::load_from_memory(&image.bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (w, h));
        assert_eq!(decoded.to_rgb8().into_raw(), raw);
    }

    #[test]
    fn a_jpeg_image_passes_through_untouched() {
        let jpeg = jpeg(8, 6);
        let pdf = image_pdf(Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Image",
                "Width" => 8, "Height" => 6,
                "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8,
                "Filter" => "DCTDecode",
            },
            jpeg.clone(),
        ));
        let image = page_image(&pdf, 1).unwrap();
        assert_eq!(image.media_type, "image/jpeg");
        assert_eq!(image.bytes, jpeg);
        let decoded = image::load_from_memory(&image.bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (8, 6));
    }

    #[test]
    fn an_unsupported_filter_is_refused_by_name() {
        let pdf = image_pdf(Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Image",
                "Width" => 4, "Height" => 4,
                "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8,
                "Filter" => "JPXDecode",
            },
            vec![0u8; 16],
        ));
        let err = page_image(&pdf, 1).unwrap_err();
        assert!(err.contains("JPXDecode"), "{err}");
        assert!(err.contains("page 1"), "{err}");
    }

    #[test]
    fn an_encrypted_pdf_says_so() {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let page = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => vec![Object::Reference(page)], "Count" => 1,
            }),
        );
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);
        let crypt = doc.add_object(dictionary! { "Filter" => "Standard" });
        doc.trailer.set("Encrypt", Object::Reference(crypt));
        let mut out = Vec::new();
        doc.save_to(&mut out).unwrap();
        let err = pages(&out).unwrap_err();
        assert!(err.contains("encrypted"), "{err}");
        assert!(page_image(&out, 1).unwrap_err().contains("encrypted"));
    }

    #[test]
    fn a_401_page_document_is_over_the_page_cap() {
        let lines: Vec<&str> = (0..401).map(|_| "x").collect();
        let pdf = text_pdf(&lines);
        let err = pages(&pdf).unwrap_err();
        assert!(err.contains("401 pages"), "{err}");
        assert!(err.contains("400"), "{err}");
    }

    #[test]
    fn a_page_can_be_read_by_its_number() {
        let pdf = text_pdf(&["one", "two", "three"]);
        let (three, count) = pages_in(&pdf, 3, 3).unwrap();
        assert_eq!(count, 3);
        assert_eq!(three.len(), 1);
        assert_eq!(three[0].number, 3);
        assert!(three[0].text.contains("three"));
        let (range, _) = pages_in(&pdf, 1, 2).unwrap();
        assert_eq!(
            range.iter().map(|p| p.number).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(pages_in(&pdf, 9, 9).unwrap_err().contains("has 3 pages"));
        assert!(pages_in(&pdf, 0, 2).unwrap_err().contains("start at 1"));
    }

    #[test]
    fn a_malformed_file_is_an_error_not_a_panic() {
        assert!(pages(b"%PDF-1.4 trailing nonsense").is_err());
        assert!(pages(b"not a pdf").unwrap_err().contains("isn't a PDF"));
        assert!(!is_pdf(b"just text"));
        assert!(is_pdf(b"%PDF-1.7\n"));
        // A note that mentions the header isn't a PDF.
        assert!(!is_pdf(b"# Notes\nthe `%PDF-` header starts every PDF\n"));
    }

    #[test]
    fn the_budget_stops_the_text_and_names_the_cap() {
        let pdf = text_pdf(&["one", "two", "three"]);
        let all = pages(&pdf).unwrap();
        let text = tagged(&all, 12);
        assert!(text.starts_with("[p.1]"), "{text}");
        assert!(text.contains("cap"), "{text}");
    }
}
