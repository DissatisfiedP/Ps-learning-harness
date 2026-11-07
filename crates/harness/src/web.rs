//! `page_fetch` for the researcher: GET with a 20 s timeout and a 2 MB cap
//! (32 MiB for a PDF), HTML converted to text in-tree (the `html2text` crate
//! isn't available offline; a small converter is enough for reading
//! articles), truncated to about 40k characters. A PDF comes back as
//! page-tagged text (`[p.3] …`), so a paper can be read without leaving the
//! tool; a scanned page says it has no text layer.

use std::time::Duration;

use providers::Client;

const TIMEOUT: Duration = Duration::from_secs(20);
const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_CHARS: usize = 40_000;
const USER_AGENT: &str = "LearningHarness/0.1 (researcher)";

/// True when the URL's path ends in `.pdf` (query string and fragment
/// ignored), so a PDF gets the larger fetch cap before its headers arrive.
fn looks_like_pdf_url(url: &str) -> bool {
    url.trim()
        .to_ascii_lowercase()
        .split(['?', '#'])
        .next()
        .is_some_and(|p| p.ends_with(".pdf"))
}

/// GET `url`: http(s) only, a success status only. Returns the response and
/// its lowercased content type.
async fn get(client: &Client, url: &str) -> Result<(reqwest::Response, String), String> {
    let url = url.trim();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("only http(s) URLs can be fetched".into());
    }
    let resp = client
        .get(url)
        .header("user-agent", USER_AGENT)
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("fetch failed: {}", e.without_url()))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("the server answered {status}"));
    }
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    Ok((resp, content_type))
}

/// Read the body up to `limit` bytes; `true` means the rest was dropped.
async fn read_body(mut resp: reqwest::Response, limit: usize) -> Result<(Vec<u8>, bool), String> {
    let mut body: Vec<u8> = Vec::new();
    let mut capped = false;
    let read = async {
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| format!("read failed: {}", e.without_url()))?
        {
            let room = limit.saturating_sub(body.len());
            if chunk.len() >= room {
                body.extend_from_slice(&chunk[..room]);
                capped = true;
                break;
            }
            body.extend_from_slice(&chunk);
        }
        Ok::<(), String>(())
    };
    tokio::time::timeout(TIMEOUT, read)
        .await
        .map_err(|_| "timed out after 20 s".to_owned())??;
    Ok((body, capped))
}

/// Fetch a page as text. Errors are written for the model.
pub async fn page_fetch(client: &Client, url: &str) -> Result<String, String> {
    let pdf_url = looks_like_pdf_url(url);
    let (resp, content_type) = get(client, url).await?;
    let pdf = pdf_url || content_type.contains("pdf");
    let limit = if pdf {
        crate::pdf::MAX_PDF_BYTES
    } else {
        MAX_BYTES
    };
    let (body, capped) = read_body(resp, limit).await?;
    if pdf || crate::pdf::is_pdf(&body) {
        if capped {
            return Err(format!(
                "the PDF is over the {} MiB fetch cap; fetch a smaller document",
                crate::pdf::MAX_PDF_BYTES / (1024 * 1024)
            ));
        }
        return fetch_pdf_text(&body);
    }
    let textual = content_type.is_empty()
        || content_type.starts_with("text/")
        || content_type.contains("html")
        || content_type.contains("xml")
        || content_type.contains("json");
    if !textual {
        return Err(format!("unsupported content type `{content_type}`"));
    }
    let raw = String::from_utf8_lossy(&body);
    let looks_html = content_type.contains("html")
        || raw.trim_start().get(..15).is_some_and(|h| {
            let h = h.to_ascii_lowercase();
            h.starts_with("<!doctype html") || h.starts_with("<html")
        });
    let mut text = if looks_html {
        html_to_text(&raw)
    } else {
        raw.into_owned()
    };
    let mut truncated = capped;
    if let Some((i, _)) = text.char_indices().nth(MAX_CHARS) {
        text.truncate(i);
        truncated = true;
    }
    if truncated {
        text.push_str("\n\n[truncated]");
    }
    Ok(text)
}

/// A fetched PDF as page-tagged text, capped at [`MAX_CHARS`] bytes.
fn fetch_pdf_text(body: &[u8]) -> Result<String, String> {
    let pages = crate::pdf::pages(body)?;
    Ok(crate::pdf::tagged(&pages, MAX_CHARS))
}

/// Fetch a PDF's bytes over HTTP (at most the PDF input cap), for the
/// researcher's `source_page_image`.
pub async fn fetch_pdf(client: &Client, url: &str) -> Result<Vec<u8>, String> {
    let (resp, content_type) = get(client, url).await?;
    let (body, capped) = read_body(resp, crate::pdf::MAX_PDF_BYTES).await?;
    if capped {
        return Err(format!(
            "the PDF is over the {} MiB fetch cap",
            crate::pdf::MAX_PDF_BYTES / (1024 * 1024)
        ));
    }
    if !crate::pdf::is_pdf(&body) {
        return Err(format!(
            "`{}` isn't a PDF (content type `{content_type}`)",
            url.trim()
        ));
    }
    Ok(body)
}

/// Convert HTML to readable text: drop scripts/styles/head, turn block
/// elements into line breaks, list items into `- `, headings into `#`, decode
/// entities and collapse whitespace.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 3);
    let mut title = String::new();
    let mut rest = html;
    let mut skip_until: Option<&'static str> = None;
    let mut in_title = false;
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            push_text(&mut out, rest, skip_until.is_some(), in_title, &mut title);
            break;
        };
        push_text(
            &mut out,
            &rest[..lt],
            skip_until.is_some(),
            in_title,
            &mut title,
        );
        rest = &rest[lt..];
        if rest.starts_with("<!--") {
            rest = rest.find("-->").map_or("", |e| &rest[e + 3..]);
            continue;
        }
        let Some(gt) = rest.find('>') else { break };
        let tag = &rest[1..gt];
        rest = &rest[gt + 1..];
        let closing = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        if let Some(end) = skip_until {
            if closing && name == end {
                skip_until = None;
            }
            continue;
        }
        match name.as_str() {
            "script" | "style" | "noscript" | "svg" | "template" | "iframe" if !closing => {
                skip_until = Some(match name.as_str() {
                    "script" => "script",
                    "style" => "style",
                    "noscript" => "noscript",
                    "svg" => "svg",
                    "template" => "template",
                    _ => "iframe",
                });
            }
            "title" => in_title = !closing,
            "br" => out.push('\n'),
            "li" if !closing => out.push_str("\n- "),
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" if !closing => {
                let level = name[1..].parse::<usize>().unwrap_or(1);
                out.push_str("\n\n");
                out.push_str(&"#".repeat(level));
                out.push(' ');
            }
            "td" | "th" if !closing => out.push_str(" | "),
            "p" | "div" | "section" | "article" | "header" | "footer" | "ul" | "ol" | "table"
            | "tr" | "blockquote" | "pre" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "dt"
            | "dd" | "figure" | "figcaption" | "main" | "nav" | "aside" => out.push_str("\n\n"),
            _ => {}
        }
    }
    let body = collapse(&out);
    let title = collapse(&title);
    if title.is_empty() {
        body
    } else {
        format!("# {title}\n\n{body}")
    }
}

fn push_text(out: &mut String, text: &str, skipping: bool, in_title: bool, title: &mut String) {
    if skipping || text.is_empty() {
        return;
    }
    // Line breaks in HTML source are just whitespace; structure comes from tags.
    let decoded = decode_entities(text).replace(['\n', '\r', '\t'], " ");
    if in_title {
        title.push_str(&decoded);
    } else {
        out.push_str(&decoded);
    }
}

/// Collapse runs of spaces within lines and runs of blank lines.
fn collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blank = 0;
    for line in s.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        if words.is_empty() {
            blank += 1;
            continue;
        }
        if !out.is_empty() {
            out.push_str(if blank > 0 { "\n\n" } else { "\n" });
        }
        blank = 0;
        out.push_str(&words.join(" "));
    }
    out
}

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let end = rest.find(';').filter(|&e| e <= 10);
        let decoded = end.and_then(|e| {
            let name = &rest[1..e];
            let ch = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" | "#39" => Some('\''),
                "nbsp" => Some(' '),
                "ndash" => Some('–'),
                "mdash" => Some('—'),
                "hellip" => Some('…'),
                _ => name
                    .strip_prefix("#x")
                    .or_else(|| name.strip_prefix("#X"))
                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                    .or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok()))
                    .and_then(char::from_u32),
            };
            ch.map(|c| (c, e))
        });
        match decoded {
            Some((c, e)) => {
                out.push(c);
                rest = &rest[e + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_becomes_readable_text() {
        let html = r#"<!doctype html><html><head><title>Ohm&#39;s law</title>
            <style>body{color:red}</style><script>var x = "<p>no</p>";</script></head>
            <body><h2>Statement</h2><p>V&nbsp;=&nbsp;I&middot;R holds for   ohmic
            materials.</p><ul><li>one</li><li>two &amp; three</li></ul><!-- hidden --></body></html>"#;
        let text = html_to_text(html);
        assert!(text.starts_with("# Ohm's law"), "{text}");
        assert!(text.contains("## Statement"));
        assert!(text.contains("holds for ohmic materials."));
        assert!(text.contains("- two & three"));
        assert!(!text.contains("color:red") && !text.contains("var x") && !text.contains("hidden"));
    }

    /// The PDF half of `page_fetch`, tested without a server: a PDF body
    /// becomes page-tagged text; a PDF named by its URL or content type takes
    /// the same path.
    #[test]
    fn a_fetched_pdf_comes_back_as_page_tagged_text() {
        assert!(looks_like_pdf_url("https://example.org/paper.pdf"));
        assert!(looks_like_pdf_url(
            "https://example.org/paper.PDF?dl=1#page=3"
        ));
        assert!(!looks_like_pdf_url("https://example.org/paper.html"));
        let pdf = crate::pdf::fixtures::text_pdf(&[
            "A first page with plenty of words to count as a text layer",
            "",
        ]);
        assert!(crate::pdf::is_pdf(&pdf));
        let text = fetch_pdf_text(&pdf).unwrap();
        assert!(text.contains("[p.1]"), "{text}");
        assert!(text.contains("[p.2] (no text layer"), "{text}");
        let err = fetch_pdf_text(b"<html>not a pdf</html>").unwrap_err();
        assert!(err.contains("isn't a PDF"), "{err}");
    }
}
