//! `live-search`: web search, page fetch and captioned image search inside
//! the lesson (docs/plugins.md §12.1, architecture §4.20 "Is web search
//! standard or a plugin?").
//!
//! - `live_search_web` runs the researcher's search chain
//!   (`research_search::search_at`: Brave with `BRAVE_API_KEY`, else
//!   Wikipedia) and returns the numbered source list.
//! - `live_search_fetch` is the researcher's `web::page_fetch`, behind the
//!   same URL guard (no loopback, private or metadata addresses, on every
//!   redirect hop).
//! - `live_search_images` searches images (Brave images, else Wikimedia
//!   Commons), downloads each through the guard, and returns one `svg`
//!   artifact whose `<image>`s are `data:` URIs: the only image source the
//!   artifact sanitizer and the view CSP (`img-src data:`) allow, so nothing
//!   is loaded from the network when the figure is shown or replayed. The
//!   model gets the captions, source pages and credits as text; captions come
//!   from the source, never from the pixels.

use std::fmt::Write as _;
use std::sync::Arc;

use providers::ToolSpec;
use research_search::{Endpoints, ImageSource, Recency, SearchRequest};
use serde_json::{json, Value};

use super::image_set::{encode_fit, image_mime, EMBED_BUDGET};
use super::{done, fail, NativeEnv, NativePackage};
use crate::plugins::{ArtifactPayload, Plugin, PluginContext, PluginManifest, ToolFuture};
use crate::web::GuardedClient;

/// Web results per search.
const WEB_RESULTS: usize = 8;
/// Images per figure: default and most.
const DEFAULT_IMAGES: usize = 4;
const MAX_IMAGES: usize = 6;
/// Largest image download.
const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;
const CAPTION_CHARS: usize = 70;

/// The network side of the plugin, separate from its manifest so tests can
/// point it at local servers.
#[derive(Clone)]
pub struct Search {
    /// For the search APIs (fixed hosts, not model-chosen URLs).
    pub client: providers::Client,
    /// For every model- or result-chosen URL.
    pub web: GuardedClient,
    pub endpoints: Endpoints,
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("`{key}` is required"))
}

/// One fetched image, ready for the figure.
#[derive(Debug, Clone)]
pub struct Shown {
    pub source: ImageSource,
    pub data_uri: String,
}

impl Search {
    pub async fn web(&self, args: &Value) -> Result<String, String> {
        let query = str_arg(args, "query")?;
        let recency =
            match args.get("recency").and_then(Value::as_str) {
                None => None,
                Some(r) => Some(serde_json::from_value::<Recency>(json!(r)).map_err(|_| {
                    format!("`recency` must be day, week, month or year, not `{r}`")
                })?),
            };
        let req = SearchRequest {
            query: query.to_owned(),
            recency,
            limit: Some(WEB_RESULTS),
        };
        let resp = research_search::search_at(&self.client, &req, &self.endpoints)
            .await
            .map_err(|e| format!("search failed: {e}"))?;
        Ok(research_search::format_for_llm(&resp))
    }

    pub async fn fetch(&self, args: &Value) -> Result<String, String> {
        crate::web::page_fetch(&self.web, str_arg(args, "url")?).await
    }

    /// Download one image through the guard (PNG, JPEG, GIF or WebP by
    /// content) and encode it as a `data:` URI of at most `budget` bytes.
    async fn image(&self, url: &str, budget: usize) -> Result<String, String> {
        let (resp, _) = self.web.get(url).await?;
        let (bytes, capped) = crate::web::read_body(resp, MAX_IMAGE_BYTES).await?;
        if capped {
            return Err("image over the 4 MiB download cap".into());
        }
        let mime = image_mime(&bytes).ok_or("not a PNG, JPEG, GIF or WebP image")?;
        tokio::task::spawn_blocking(move || encode_fit(&bytes, mime, budget))
            .await
            .map_err(|e| format!("image encoding failed ({e})"))?
    }

    /// Search and download up to `count` images, in search order. Every hit
    /// downloads at once; a hit whose download fails is skipped (reported in
    /// the text), so a dead thumbnail doesn't sink the call.
    pub async fn images(&self, args: &Value) -> Result<(Vec<Shown>, Vec<String>), String> {
        let query = str_arg(args, "query")?;
        let count = args
            .get("count")
            .and_then(Value::as_u64)
            .map_or(DEFAULT_IMAGES, |c| c as usize)
            .clamp(1, MAX_IMAGES);
        let resp = research_search::search_images(&self.client, query, count * 2, &self.endpoints)
            .await
            .map_err(|e| format!("image search failed: {e}"))?;
        let budget = EMBED_BUDGET / count;
        let mut downloads = tokio::task::JoinSet::new();
        for (i, source) in resp.images.iter().enumerate() {
            let this = self.clone();
            let url = source.image_url.clone();
            downloads.spawn(async move { (i, this.image(&url, budget).await) });
        }
        let mut results: Vec<Option<Result<String, String>>> = vec![None; resp.images.len()];
        while let Some(done) = downloads.join_next().await {
            if let Ok((i, r)) = done {
                results[i] = Some(r);
            }
        }
        let mut shown = Vec::new();
        let mut failed = Vec::new();
        for (source, result) in resp.images.into_iter().zip(results) {
            if shown.len() == count {
                break;
            }
            match result.unwrap_or_else(|| Err("download task failed".into())) {
                Ok(data_uri) => shown.push(Shown { source, data_uri }),
                Err(e) => failed.push(format!("{} ({e})", source.page_url)),
            }
        }
        if shown.is_empty() {
            return Err(format!(
                "no image could be downloaded: {}",
                failed.join("; ")
            ));
        }
        Ok((shown, failed))
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn clip(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

/// The figure: images two per row in fixed boxes, each with its number and
/// a shortened caption underneath.
pub fn figure(shown: &[Shown]) -> String {
    const BOX_W: usize = 300;
    const BOX_H: usize = 220;
    const CELL_H: usize = BOX_H + 34;
    let cols = shown.len().min(2);
    let rows = shown.len().div_ceil(2);
    let width = 10 + cols * (BOX_W + 10);
    let height = 10 + rows * CELL_H;
    let mut svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}" width="{width}" height="{height}" font-family="sans-serif" font-size="13">"#
    );
    for (i, s) in shown.iter().enumerate() {
        let x = 10 + (i % 2) * (BOX_W + 10);
        let y = 10 + (i / 2) * CELL_H;
        let _ = write!(
            svg,
            r#"<image x="{x}" y="{y}" width="{BOX_W}" height="{BOX_H}" preserveAspectRatio="xMidYMid meet" href="{}"/>"#,
            esc(&s.data_uri)
        );
        let _ = write!(
            svg,
            r#"<text x="{x}" y="{}" fill="currentColor">{}. {}</text>"#,
            y + BOX_H + 20,
            i + 1,
            esc(&clip(&s.source.caption, CAPTION_CHARS))
        );
    }
    svg.push_str("</svg>");
    svg
}

/// What the model reads: one numbered entry per image.
pub fn listing(query: &str, shown: &[Shown], failed: &[String]) -> String {
    let mut out = format!(
        "{} image(s) for \"{query}\", numbered as in the figure:\n",
        shown.len()
    );
    for (i, s) in shown.iter().enumerate() {
        let _ = write!(
            out,
            "[{}] {}\n    source: {}\n",
            i + 1,
            s.source.caption,
            s.source.page_url
        );
        if let Some(credit) = &s.source.credit {
            let _ = writeln!(out, "    credit: {credit}");
        }
    }
    if !failed.is_empty() {
        let _ = writeln!(out, "Skipped (download failed): {}", failed.join("; "));
    }
    out.truncate(out.trim_end().len());
    out
}

/// Stored artifact data: the sources, not the pixels (they are in the
/// fallback figure already).
pub fn artifact_data(query: &str, shown: &[Shown]) -> Value {
    json!({
        "query": query,
        "images": shown.iter().enumerate().map(|(i, s)| json!({
            "n": i + 1,
            "caption": s.source.caption,
            "pageUrl": s.source.page_url,
            "imageUrl": s.source.image_url,
            "credit": s.source.credit,
        })).collect::<Vec<_>>(),
    })
}

/// The `live-search` built-in.
pub struct LiveSearch {
    pkg: NativePackage,
    search: Search,
}

impl LiveSearch {
    pub fn new(env: &NativeEnv) -> Self {
        Self::with(Search {
            client: env.client.clone(),
            // The same construction already succeeded for `Harness::web`.
            web: GuardedClient::new().expect("the guarded HTTP client builds"),
            endpoints: Endpoints::default(),
        })
    }

    pub fn with(search: Search) -> Self {
        let pkg = NativePackage::parse(
            include_str!("live-search/plugin.toml"),
            &[
                (
                    "schemas/live_search_web.json",
                    include_str!("live-search/schemas/live_search_web.json"),
                ),
                (
                    "schemas/live_search_fetch.json",
                    include_str!("live-search/schemas/live_search_fetch.json"),
                ),
                (
                    "schemas/live_search_images.json",
                    include_str!("live-search/schemas/live_search_images.json"),
                ),
                (
                    "prompts/tutor.md",
                    include_str!("live-search/prompts/tutor.md"),
                ),
            ],
        )
        .expect("the live-search package is valid");
        Self { pkg, search }
    }
}

impl Plugin for LiveSearch {
    fn manifest(&self) -> &PluginManifest {
        &self.pkg.manifest
    }

    fn tools(&self) -> Vec<ToolSpec> {
        self.pkg.tools.clone()
    }

    fn fragment(&self, agent: &str) -> String {
        self.pkg.fragment(agent)
    }

    fn call(self: Arc<Self>, name: &str, args: &Value, _ctx: PluginContext) -> ToolFuture {
        let entry = self.pkg.entry(name).map(str::to_owned);
        let name = name.to_owned();
        let args = args.clone();
        Box::pin(async move {
            let text = match entry.as_deref() {
                Some("web") => self.search.web(&args).await,
                Some("fetch") => self.search.fetch(&args).await,
                Some("images") => {
                    return match self.search.images(&args).await {
                        Ok((shown, failed)) => {
                            let query = str_arg(&args, "query").unwrap_or_default();
                            let mut out = done(listing(query, &shown, &failed));
                            out.artifact = Some(ArtifactPayload {
                                kind: "svg".into(),
                                svg: figure(&shown),
                                data: artifact_data(query, &shown),
                            });
                            out
                        }
                        Err(e) => fail(e),
                    }
                }
                _ => Err(format!("live-search has no tool `{name}`")),
            };
            match text {
                Ok(text) => done(text),
                Err(e) => fail(e),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use super::*;

    /// A loopback HTTP server: `path` (query string ignored) → (status,
    /// content type, body). `{base}` in a body becomes the server's base URL.
    fn serve(routes: Vec<(&'static str, u16, &'static str, Vec<u8>)>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let routes: Vec<_> = routes
            .into_iter()
            .map(|(p, s, t, b)| {
                let b = match String::from_utf8(b.clone()) {
                    Ok(text) => text.replace("{base}", &base).into_bytes(),
                    Err(_) => b,
                };
                (p, s, t, b)
            })
            .collect();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut req = Vec::new();
                let mut buf = [0u8; 4096];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => req.extend_from_slice(&buf[..n]),
                    }
                }
                let line = String::from_utf8_lossy(&req)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                let path = line
                    .split(' ')
                    .nth(1)
                    .unwrap_or("/")
                    .split('?')
                    .next()
                    .unwrap_or("/")
                    .to_owned();
                let (status, ctype, body) = routes
                    .iter()
                    .find(|(p, ..)| *p == path)
                    .map(|(_, s, t, b)| (*s, *t, b.clone()))
                    .unwrap_or((404, "text/plain", b"not found".to_vec()));
                let head = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: {ctype}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        base
    }

    fn png(w: u32, h: u32, noisy: bool) -> Vec<u8> {
        let mut s: u32 = 0x9E37_79B9;
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            if !noisy {
                return image::Rgb([x as u8, y as u8, 90]);
            }
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

    fn search_at(base: &str) -> Search {
        Search {
            client: providers::Client::new(),
            web: GuardedClient::exempting("127.0.0.1".parse().unwrap()).unwrap(),
            endpoints: Endpoints {
                brave_web: format!("{base}/brave/web"),
                brave_images: format!("{base}/brave/images"),
                wikipedia: format!("{base}/wiki/api.php"),
                commons: format!("{base}/commons/api.php"),
            },
        }
    }

    fn commons_page(index: u32, title: &str, mime: &str, path: &str, desc: &str) -> Value {
        json!({
            "index": index,
            "title": format!("File:{title}"),
            "imageinfo": [{
                "mime": mime,
                "url": format!("{{base}}/orig{path}"),
                "thumburl": format!("{{base}}{path}"),
                "descriptionurl": format!("https://commons.example/wiki/File:{title}"),
                "extmetadata": {
                    "ImageDescription": { "value": desc },
                    "Artist": { "value": "<a href=\"x\">A. Painter</a>" },
                    "LicenseShortName": { "value": "CC BY-SA 4.0" },
                },
            }],
        })
    }

    fn routes() -> Vec<(&'static str, u16, &'static str, Vec<u8>)> {
        let wiki = json!({ "query": { "search": [
            { "title": "Gothic architecture", "snippet": "pointed <span>arches</span> &amp; ribbed vaults" },
        ]}});
        let commons = json!({ "query": { "pages": [
            commons_page(3, "Big.png", "image/png", "/img/big.png", "A large <b>rose</b> window"),
            commons_page(1, "Chartres.jpg", "image/jpeg", "/img/small.png", "Chartres west front"),
            commons_page(2, "Broken.png", "image/png", "/img/broken.png", "Not really an image"),
            commons_page(4, "Plan.svg", "image/svg+xml", "/img/plan.svg", "A floor plan"),
        ]}});
        vec![
            ("/wiki/api.php", 200, "application/json", wiki.to_string().into_bytes()),
            ("/commons/api.php", 200, "application/json", commons.to_string().into_bytes()),
            (
                "/page",
                200,
                "text/html",
                b"<html><head><title>Rib vaults</title></head><body><h2>Structure</h2><p>Ribs carry the load.</p></body></html>".to_vec(),
            ),
            ("/img/small.png", 200, "image/png", png(40, 30, false)),
            ("/img/big.png", 200, "image/png", png(900, 700, true)),
            ("/img/broken.png", 200, "image/png", b"<html>moved</html>".to_vec()),
        ]
    }

    #[tokio::test]
    async fn web_search_lists_sources_and_fetch_reads_pages_through_the_guard() {
        let base = serve(routes());
        let s = search_at(&base);
        let text = s
            .web(&json!({ "query": "gothic vaults", "recency": "year" }))
            .await
            .unwrap();
        assert!(
            text.starts_with(
                "[1] Gothic architecture\n    https://en.wikipedia.org/wiki/Gothic_architecture"
            ),
            "{text}"
        );
        assert!(text.contains("pointed arches & ribbed vaults"), "{text}");
        let err = s
            .web(&json!({ "query": "x", "recency": "decade" }))
            .await
            .unwrap_err();
        assert!(err.contains("`recency` must be"), "{err}");
        assert!(s.web(&json!({ "query": "  " })).await.is_err());

        let page = s
            .fetch(&json!({ "url": format!("{base}/page") }))
            .await
            .unwrap();
        assert!(page.starts_with("# Rib vaults"), "{page}");
        assert!(
            page.contains("## Structure") && page.contains("Ribs carry the load."),
            "{page}"
        );
        let err = s
            .fetch(&json!({ "url": format!("{base}/missing") }))
            .await
            .unwrap_err();
        assert!(err.contains("404"), "{err}");
        for blocked in [
            "http://10.0.0.1/admin",
            "http://169.254.169.254/latest/meta-data",
            "file:///etc/passwd",
        ] {
            let err = s.fetch(&json!({ "url": blocked })).await.unwrap_err();
            assert!(
                err.contains("refused") || err.contains("http"),
                "{blocked}: {err}"
            );
        }
        // The production guard refuses the loopback server itself.
        let guarded = Search {
            web: GuardedClient::new().unwrap(),
            ..s
        };
        let err = guarded
            .fetch(&json!({ "url": format!("{base}/page") }))
            .await
            .unwrap_err();
        assert!(err.contains("refused"), "{err}");
    }

    #[tokio::test]
    async fn image_search_embeds_downloaded_images_as_a_sanitizable_figure() {
        let base = serve(routes());
        let plugin = Arc::new(LiveSearch::with(search_at(&base)));
        let ctx = PluginContext {
            session_id: "s".into(),
            platform: "desktop".into(),
        };
        let out = Arc::clone(&plugin)
            .call(
                "live_search_images",
                &json!({ "query": "gothic", "count": 2 }),
                ctx.clone(),
            )
            .await;
        assert!(!out.is_error, "{}", out.content);
        // Search order (Commons `index`), the broken hit skipped and named,
        // the SVG file never offered.
        let text = &out.content;
        assert!(text.starts_with("2 image(s) for \"gothic\""), "{text}");
        assert!(text.contains("[1] Chartres west front\n    source: https://commons.example/wiki/File:Chartres.jpg"), "{text}");
        assert!(text.contains("credit: A. Painter, CC BY-SA 4.0"), "{text}");
        assert!(text.contains("[2] A large rose window"), "{text}");
        assert!(text.contains("Skipped (download failed): https://commons.example/wiki/File:Broken.png (not a PNG"), "{text}");
        assert!(!text.contains("floor plan"), "{text}");

        let artifact = out.artifact.expect("a figure");
        let clean = crate::artifacts::sanitize_svg(&artifact.svg).unwrap();
        assert_eq!(
            clean.matches("href=\"data:image/").count(),
            2,
            "{}",
            &clean[..200.min(clean.len())]
        );
        assert!(!clean.contains(&base), "no network URL reaches the figure");
        assert!(artifact.svg.len() <= crate::artifacts::MAX_SVG_BYTES);
        assert_eq!(
            artifact.data["images"][1]["pageUrl"],
            json!("https://commons.example/wiki/File:Big.png")
        );

        let none = Arc::clone(&plugin)
            .call("live_search_images", &json!({ "query": "x" }), ctx.clone())
            .await;
        assert!(
            !none.is_error,
            "count defaults to 4 and takes what downloads"
        );
        let bad = plugin.call("live_search_nope", &json!({}), ctx).await;
        assert!(bad.is_error);
    }
}
