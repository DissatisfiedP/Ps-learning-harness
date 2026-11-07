//! Artifacts and their placement (build prompt W6, D7/D8).
//!
//! A plugin tool returns `{ text, artifact: { kind: "svg", svg, data } }`.
//! The core sanitizes the SVG (no scripts, event handlers or external
//! references), stores an `artifact` op with the fallback drawing and the
//! data a view needs, and tells the model how to place it.

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use crate::Error;

/// Largest fallback SVG accepted from a plugin.
pub const MAX_SVG_BYTES: usize = 256 * 1024;

const BANNED: &[&str] = &[
    "script",
    "foreignobject",
    "iframe",
    "object",
    "embed",
    "audio",
    "video",
    // Animation can rewrite attributes (e.g. `href`) after sanitising.
    "animate",
    "animatetransform",
    "animatemotion",
    "set",
    "discard",
    // CSS can carry `@import url(...)`; plugins draw with presentation
    // attributes, so nothing legitimate is lost.
    "style",
];

/// Strip `<script>`, event attributes and external references from SVG.
pub fn sanitize_svg(svg: &str) -> Result<String, Error> {
    if svg.len() > MAX_SVG_BYTES {
        return Err(Error::Invalid(format!(
            "the fallback SVG is {} KiB (limit {} KiB); ask for a simpler figure",
            svg.len() / 1024,
            MAX_SVG_BYTES / 1024
        )));
    }
    let mut reader = Reader::from_str(svg);
    reader.config_mut().trim_text(false);
    let mut out: Vec<u8> = Vec::with_capacity(svg.len());
    let mut skip_depth = 0usize;
    loop {
        let event = reader
            .read_event()
            .map_err(|e| Error::Invalid(format!("the SVG is not well-formed XML ({e})")))?;
        match event {
            Event::Start(start) => {
                let name = local_name(&start);
                if BANNED.contains(&name.as_str()) {
                    skip_depth += 1;
                    continue;
                }
                if skip_depth > 0 {
                    skip_depth += 1;
                    continue;
                }
                write_start(&mut out, &start, false)?;
            }
            Event::Empty(start) => {
                let name = local_name(&start);
                if !BANNED.contains(&name.as_str()) && skip_depth == 0 {
                    write_start(&mut out, &start, true)?;
                }
            }
            Event::End(end) => {
                if skip_depth > 0 {
                    skip_depth -= 1;
                    continue;
                }
                let name = end.name().as_ref().to_owned();
                out.extend_from_slice(format!("</{name}>").as_bytes());
            }
            // Text is re-emitted exactly as parsed. Entities arrive as their
            // own events, so this branch never carries raw markup.
            Event::Text(text) => {
                if skip_depth == 0 {
                    out.extend_from_slice(text.into_inner().as_bytes());
                }
            }
            // Re-encode entities rather than passing them through: an entity
            // the SVG has no definition for (e.g. `&nbsp;`) would otherwise
            // break the document, and dropping the event would silently
            // delete every `&lt;`/`&amp;` in a label.
            Event::GeneralRef(reference) => {
                if skip_depth == 0 {
                    if let Some(ch) = decode_entity(reference.into_inner().as_ref()) {
                        out.extend_from_slice(escape_text(&ch).as_bytes());
                    }
                }
            }
            // CDATA is only meaningful inside <style>/<script>, both refused.
            Event::CData(_) => {}
            Event::Comment(_) => {}
            Event::Decl(_) | Event::PI(_) | Event::DocType(_) => {}
            Event::Eof => break,
        }
    }
    let text = String::from_utf8(out).map_err(|_| Error::Invalid("the SVG is not UTF-8".into()))?;
    if !text.to_ascii_lowercase().contains("<svg") {
        return Err(Error::Invalid("the fallback is not an SVG document".into()));
    }
    Ok(text)
}

fn local_name(start: &BytesStart) -> String {
    let raw = start.name().as_ref().to_owned();
    let local = raw.rsplit(':').next().unwrap_or(&raw);
    local.to_ascii_lowercase()
}

fn write_start(out: &mut Vec<u8>, start: &BytesStart, empty: bool) -> Result<(), Error> {
    let name = start.name().as_ref().to_owned();
    out.push(b'<');
    out.extend_from_slice(name.as_bytes());
    for attr in start.attributes().flatten() {
        let key = attr.key.as_ref().to_owned();
        let lower = key.to_ascii_lowercase();
        // Event handlers and inline CSS: plugins draw with presentation
        // attributes, so `style` carries nothing legitimate.
        if lower.starts_with("on") || lower == "style" {
            continue;
        }
        // Values are unescaped here; they must be escaped again on the way
        // out, or `&quot;` in the source could close the attribute early.
        let value = attr
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .unwrap_or_default()
            .into_owned();
        let low = value.to_ascii_lowercase();
        if low.contains("javascript:") || !safe_css_refs(&low) {
            continue;
        }
        if (lower == "href" || lower == "xlink:href") && !safe_href(&low) {
            continue;
        }
        out.push(b' ');
        out.extend_from_slice(key.as_bytes());
        out.extend_from_slice(b"=\"");
        out.extend_from_slice(escape_attr(&value).as_bytes());
        out.push(b'"');
    }
    if empty {
        out.extend_from_slice(b"/>");
    } else {
        out.push(b'>');
    }
    Ok(())
}

/// CSS functions that fetch; `url(...)` may only point inside the document
/// (`url(#id)`). A backslash could hide any of these behind a CSS escape.
fn safe_css_refs(low: &str) -> bool {
    const FETCHING: [&str; 5] = ["image-set(", "image(", "src(", "cross-fade(", "element("];
    if low.contains('\\') || FETCHING.iter().any(|f| low.contains(f)) {
        return false;
    }
    let mut rest = low;
    while let Some(i) = rest.find("url(") {
        let after = rest[i + 4..].trim_start();
        if !after.starts_with('#') {
            return false;
        }
        rest = after;
    }
    true
}

/// `href` may point inside the document or embed a raster image; an embedded
/// SVG could carry its own external references.
fn safe_href(low: &str) -> bool {
    low.starts_with('#')
        || [
            "data:image/png;base64,",
            "data:image/jpeg;base64,",
            "data:image/gif;base64,",
            "data:image/webp;base64,",
        ]
        .iter()
        .any(|p| low.starts_with(p))
}

/// The five XML entities plus numeric character references; anything else is
/// dropped (an undefined entity would break the document).
fn decode_entity(name: &str) -> Option<String> {
    match name {
        "lt" => return Some("<".into()),
        "gt" => return Some(">".into()),
        "amp" => return Some("&".into()),
        "quot" => return Some("\"".into()),
        "apos" => return Some("'".into()),
        _ => {}
    }
    let digits = name.strip_prefix('#')?;
    let code = match digits.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => digits.parse::<u32>().ok()?,
    };
    char::from_u32(code).map(String::from)
}

fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// The handle line appended to a plugin tool's text result.
pub fn placement_line(id: &str) -> String {
    format!("artifact: {id} — place it once with ![a short description](artifact:{id}) next to the sentence that refers to it")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sanitize(svg: &str) -> String {
        sanitize_svg(svg).expect("sanitizes")
    }

    #[test]
    fn scripts_event_handlers_and_document_wrappers_are_removed() {
        let out = sanitize(
            r#"<?xml version="1.0"?><!DOCTYPE svg><svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script><rect width="10" height="10" onload="alert(2)"/><style>@import url(http://x/y.css);</style><g><circle r="3"/></g></svg>"#,
        );
        assert!(!out.to_ascii_lowercase().contains("<script"));
        assert!(!out.to_ascii_lowercase().contains("onload"));
        assert!(!out.contains("@import"));
        assert!(out.contains("<circle"));
        assert!(out.starts_with("<svg"));
    }

    #[test]
    fn an_escaped_tag_in_text_stays_text() {
        let out = sanitize(r#"<svg><text>&lt;script&gt;alert(1)&lt;/script&gt;</text></svg>"#);
        assert!(out.contains("&lt;script&gt;"), "out = {out:?}");
        assert!(!out.contains("<script"));
    }

    #[test]
    fn an_escaped_quote_cannot_close_an_attribute() {
        let out = sanitize(r#"<svg><rect x="&quot; onload=&quot;alert(1)"/></svg>"#);
        assert_eq!(out, "<svg><rect x=\"&quot; onload=&quot;alert(1)\"/></svg>");
    }

    #[test]
    fn external_references_are_dropped_and_internal_ones_kept() {
        for (href, kept) in [
            ("#grad", true),
            ("data:image/png;base64,AAAA", true),
            ("data:image/svg+xml;base64,AAAA", false),
            ("https://example.com/x.svg", false),
            ("javascript:alert(1)", false),
        ] {
            let out = sanitize(&format!(r#"<svg><use href="{href}"/></svg>"#));
            assert_eq!(out.contains("href"), kept, "{href} → {out}");
        }
        // A CSS reference may only point inside the document.
        assert!(sanitize(r#"<svg><rect fill="url(#grad)"/></svg>"#).contains("url(#grad)"));
        assert!(sanitize(r#"<svg><rect fill="url( #grad)"/></svg>"#).contains("fill="));
    }

    #[test]
    fn fetching_css_and_animation_are_stripped() {
        for attr in [
            r#"fill="\75 rl(https://x)""#,
            r#"fill="&#92;75 rl(https://x)""#,
            r#"fill="URL(https://x)""#,
            r#"fill="image-set(&quot;https://x&quot; 1x)""#,
            r#"fill="-webkit-image-set(https://x 1x)""#,
            r#"fill="image(https://x)""#,
            r#"fill="cross-fade(url(#a), url(#b))""#,
            r#"fill="element(#a)""#,
            r#"fill="src(https://x)""#,
            r#"style="background:url(https://x)""#,
            r#"style="fill:red""#,
        ] {
            let out = sanitize(&format!(r#"<svg><rect {attr}/></svg>"#));
            assert_eq!(out, "<svg><rect/></svg>", "{attr}");
        }
        for el in [
            "animate",
            "set",
            "animateTransform",
            "animateMotion",
            "discard",
        ] {
            let out = sanitize(&format!(
                r##"<svg><a href="#x"><{el} attributeName="href" to="https://x"/></a><{el}></{el}></svg>"##
            ));
            assert_eq!(out, r##"<svg><a href="#x"></a></svg>"##, "{el}");
        }
    }

    #[test]
    fn an_oversized_or_non_svg_fallback_is_refused() {
        let big = "<svg>".to_owned() + &"x".repeat(MAX_SVG_BYTES) + "</svg>";
        assert!(sanitize_svg(&big).is_err());
        assert!(sanitize_svg("<div>no</div>").is_err());
    }
}
