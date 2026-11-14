//! Web and image search for the researcher agent and the `live-search`
//! plugin.
//!
//! Web chain, first non-empty result wins: Brave Search API (when
//! `BRAVE_API_KEY` is set) → Wikipedia search API (keyless, ignores
//! `recency`). Image chain: Brave image search (same key) → Wikimedia
//! Commons file search (keyless; captions from the file's own description).
//! [`Endpoints`] names the base URLs so tests can point at a local server.

use std::time::Duration;

use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};

const TIMEOUT: Duration = Duration::from_secs(30);
/// Wikimedia asks API clients to identify themselves.
const USER_AGENT: &str = "LearningHarness/0.1 (research-search)";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default)]
    pub recency: Option<Recency>,
    /// 1–20, default 10.
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Recency {
    Day,
    Week,
    Month,
    Year,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub provider: &'static str,
    pub sources: Vec<SearchSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchSource {
    pub title: String,
    pub url: String,
    pub snippet: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("{provider}: {message}")]
    Provider {
        provider: &'static str,
        message: String,
    },
    #[error("no results ({0})")]
    Exhausted(String),
}

impl SearchRequest {
    fn limit(&self) -> usize {
        self.limit.unwrap_or(10).clamp(1, 20)
    }
}

/// Base URLs of the search APIs. `Default` is the real services.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    /// Brave web search (`…/res/v1/web/search`).
    pub brave_web: String,
    /// Brave image search (`…/res/v1/images/search`).
    pub brave_images: String,
    /// Wikipedia's `api.php`.
    pub wikipedia: String,
    /// Wikimedia Commons' `api.php`.
    pub commons: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            brave_web: "https://api.search.brave.com/res/v1/web/search".into(),
            brave_images: "https://api.search.brave.com/res/v1/images/search".into(),
            wikipedia: "https://en.wikipedia.org/w/api.php".into(),
            commons: "https://commons.wikimedia.org/w/api.php".into(),
        }
    }
}

/// One image hit. `caption` comes from the source's own title or
/// description, never from looking at the pixels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageSource {
    /// A display-sized image URL (a thumbnail where the service offers one).
    pub image_url: String,
    /// The page the image comes from.
    pub page_url: String,
    pub caption: String,
    /// Licence and author line when the service reports one.
    pub credit: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageResponse {
    pub provider: &'static str,
    pub images: Vec<ImageSource>,
}

fn brave_key() -> Option<String> {
    std::env::var("BRAVE_API_KEY")
        .ok()
        .map(|k| k.trim().to_owned())
        .filter(|k| !k.is_empty())
}

pub async fn search(client: &Client, req: &SearchRequest) -> Result<SearchResponse, SearchError> {
    search_at(client, req, &Endpoints::default()).await
}

/// [`search`] against explicit endpoints.
pub async fn search_at(
    client: &Client,
    req: &SearchRequest,
    at: &Endpoints,
) -> Result<SearchResponse, SearchError> {
    let mut failures = Vec::new();
    if let Some(key) = brave_key() {
        match brave(client, req, &key, &at.brave_web).await {
            Ok(r) if !r.sources.is_empty() => return Ok(r),
            Ok(_) => failures.push("brave: no results".to_owned()),
            Err(e) => failures.push(e.to_string()),
        }
    }
    match wikipedia(client, req, &at.wikipedia).await {
        Ok(r) if !r.sources.is_empty() => return Ok(r),
        Ok(_) => failures.push("wikipedia: no results".to_owned()),
        Err(e) => failures.push(e.to_string()),
    }
    Err(SearchError::Exhausted(failures.join("; ")))
}

/// Image search: Brave images (with a key), then Wikimedia Commons.
/// `limit` is clamped to 1–20.
pub async fn search_images(
    client: &Client,
    query: &str,
    limit: usize,
    at: &Endpoints,
) -> Result<ImageResponse, SearchError> {
    let limit = limit.clamp(1, 20);
    let mut failures = Vec::new();
    if let Some(key) = brave_key() {
        match brave_images(client, query, limit, &key, &at.brave_images).await {
            Ok(r) if !r.images.is_empty() => return Ok(r),
            Ok(_) => failures.push("brave images: no results".to_owned()),
            Err(e) => failures.push(e.to_string()),
        }
    }
    match commons(client, query, limit, &at.commons).await {
        Ok(r) if !r.images.is_empty() => return Ok(r),
        Ok(_) => failures.push("commons: no results".to_owned()),
        Err(e) => failures.push(e.to_string()),
    }
    Err(SearchError::Exhausted(failures.join("; ")))
}

/// Numbered source list for a model's context.
pub fn format_for_llm(resp: &SearchResponse) -> String {
    let mut out = String::new();
    for (i, s) in resp.sources.iter().enumerate() {
        let title = if s.title.trim().is_empty() {
            &s.url
        } else {
            s.title.trim()
        };
        out.push_str(&format!("[{}] {title}\n    {}\n", i + 1, s.url));
        if let Some(snippet) = s
            .snippet
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            out.push_str(&format!("    {}\n", truncate(snippet, 240)));
        }
    }
    out.truncate(out.trim_end().len());
    out
}

async fn brave(
    client: &Client,
    req: &SearchRequest,
    key: &str,
    endpoint: &str,
) -> Result<SearchResponse, SearchError> {
    let fail = |message: String| SearchError::Provider {
        provider: "brave",
        message,
    };
    let mut query: Vec<(&str, String)> =
        vec![("q", req.query.clone()), ("count", req.limit().to_string())];
    if let Some(r) = req.recency {
        let f = match r {
            Recency::Day => "pd",
            Recency::Week => "pw",
            Recency::Month => "pm",
            Recency::Year => "py",
        };
        query.push(("freshness", f.to_owned()));
    }
    let resp = client
        .get(endpoint)
        .query(&query)
        .header("Accept", "application/json")
        .header("X-Subscription-Token", key)
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|e| fail(e.without_url().to_string()))?;
    if !resp.status().is_success() {
        return Err(fail(format!("HTTP {}", resp.status())));
    }
    let v: serde_json::Value = resp.json().await.map_err(|e| fail(e.to_string()))?;
    let sources = v
        .pointer("/web/results")
        .and_then(|r| r.as_array())
        .into_iter()
        .flatten()
        .filter_map(|r| {
            let url = r.get("url")?.as_str()?;
            Some(SearchSource {
                title: decode_entities(&strip_tags(
                    r.get("title").and_then(|t| t.as_str()).unwrap_or_default(),
                )),
                url: url.to_owned(),
                snippet: r
                    .get("description")
                    .and_then(|t| t.as_str())
                    .map(|s| decode_entities(&strip_tags(s))),
            })
        })
        .take(req.limit())
        .collect();
    Ok(SearchResponse {
        provider: "brave",
        sources,
    })
}

async fn wikipedia(
    client: &Client,
    req: &SearchRequest,
    endpoint: &str,
) -> Result<SearchResponse, SearchError> {
    let fail = |message: String| SearchError::Provider {
        provider: "wikipedia",
        message,
    };
    let limit = req.limit().to_string();
    let v: serde_json::Value = client
        .get(endpoint)
        .query(&[
            ("action", "query"),
            ("list", "search"),
            ("format", "json"),
            ("utf8", "1"),
            ("srsearch", req.query.as_str()),
            ("srlimit", limit.as_str()),
        ])
        .header("User-Agent", USER_AGENT)
        .timeout(TIMEOUT)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| fail(e.without_url().to_string()))?
        .json()
        .await
        .map_err(|e| fail(e.to_string()))?;
    Ok(SearchResponse {
        provider: "wikipedia",
        sources: parse_wikipedia(&v),
    })
}

/// `query.search[]` entries: `title` plus an HTML `snippet` with match spans.
pub fn parse_wikipedia(v: &serde_json::Value) -> Vec<SearchSource> {
    v.pointer("/query/search")
        .and_then(|s| s.as_array())
        .into_iter()
        .flatten()
        .filter_map(|hit| {
            let title = hit.get("title")?.as_str()?;
            let mut url = Url::parse("https://en.wikipedia.org/wiki/").ok()?;
            url.path_segments_mut()
                .ok()?
                .pop_if_empty()
                .push(&title.replace(' ', "_"));
            Some(SearchSource {
                title: title.to_owned(),
                url: url.into(),
                snippet: hit
                    .get("snippet")
                    .and_then(|s| s.as_str())
                    .map(|s| decode_entities(&strip_tags(s)))
                    .filter(|s| !s.is_empty()),
            })
        })
        .collect()
}

async fn brave_images(
    client: &Client,
    query: &str,
    limit: usize,
    key: &str,
    endpoint: &str,
) -> Result<ImageResponse, SearchError> {
    let fail = |message: String| SearchError::Provider {
        provider: "brave images",
        message,
    };
    let resp = client
        .get(endpoint)
        .query(&[("q", query), ("count", &limit.to_string())])
        .header("Accept", "application/json")
        .header("X-Subscription-Token", key)
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|e| fail(e.without_url().to_string()))?;
    if !resp.status().is_success() {
        return Err(fail(format!("HTTP {}", resp.status())));
    }
    let v: serde_json::Value = resp.json().await.map_err(|e| fail(e.to_string()))?;
    Ok(ImageResponse {
        provider: "brave",
        images: parse_brave_images(&v).into_iter().take(limit).collect(),
    })
}

/// Brave `results[]`: `title`, `url` (the page), `thumbnail.src`, `source`.
pub fn parse_brave_images(v: &serde_json::Value) -> Vec<ImageSource> {
    v.pointer("/results")
        .and_then(|r| r.as_array())
        .into_iter()
        .flatten()
        .filter_map(|r| {
            let image_url = r
                .pointer("/thumbnail/src")
                .or_else(|| r.pointer("/properties/url"))?
                .as_str()?;
            let page_url = r.get("url")?.as_str()?;
            Some(ImageSource {
                image_url: image_url.to_owned(),
                page_url: page_url.to_owned(),
                caption: decode_entities(&strip_tags(
                    r.get("title").and_then(|t| t.as_str()).unwrap_or_default(),
                )),
                credit: r
                    .get("source")
                    .and_then(|s| s.as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
            })
        })
        .collect()
}

async fn commons(
    client: &Client,
    query: &str,
    limit: usize,
    endpoint: &str,
) -> Result<ImageResponse, SearchError> {
    let fail = |message: String| SearchError::Provider {
        provider: "commons",
        message,
    };
    let limit = limit.to_string();
    let v: serde_json::Value = client
        .get(endpoint)
        .query(&[
            ("action", "query"),
            ("format", "json"),
            ("formatversion", "2"),
            ("generator", "search"),
            ("gsrnamespace", "6"),
            ("gsrsearch", query),
            ("gsrlimit", limit.as_str()),
            ("prop", "imageinfo"),
            ("iiprop", "url|mime|extmetadata"),
            ("iiurlwidth", "480"),
        ])
        .header("User-Agent", USER_AGENT)
        .timeout(TIMEOUT)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| fail(e.without_url().to_string()))?
        .json()
        .await
        .map_err(|e| fail(e.to_string()))?;
    Ok(ImageResponse {
        provider: "commons",
        images: parse_commons(&v),
    })
}

/// Commons `query.pages[]` (formatversion 2), in search order (`index`).
/// Raster files only; the caption is the file's description, else its name.
pub fn parse_commons(v: &serde_json::Value) -> Vec<ImageSource> {
    let mut pages: Vec<&serde_json::Value> = v
        .pointer("/query/pages")
        .and_then(|p| p.as_array())
        .into_iter()
        .flatten()
        .collect();
    pages.sort_by_key(|p| p.get("index").and_then(|i| i.as_i64()).unwrap_or(i64::MAX));
    pages
        .into_iter()
        .filter_map(|p| {
            let info = p.pointer("/imageinfo/0")?;
            let mime = info.get("mime").and_then(|m| m.as_str()).unwrap_or("");
            if !matches!(
                mime,
                "image/png" | "image/jpeg" | "image/gif" | "image/webp"
            ) {
                return None;
            }
            let image_url = info.get("thumburl").or_else(|| info.get("url"))?.as_str()?;
            let page_url = info.get("descriptionurl")?.as_str()?;
            let meta = |k: &str| {
                info.pointer(&format!("/extmetadata/{k}/value"))
                    .and_then(|v| v.as_str())
                    .map(|s| decode_entities(&strip_tags(s)))
                    .filter(|s| !s.is_empty())
            };
            let title = p.get("title").and_then(|t| t.as_str()).unwrap_or_default();
            let name = title.strip_prefix("File:").unwrap_or(title);
            let name = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
            let caption = meta("ImageDescription").unwrap_or_else(|| name.to_owned());
            let credit = match (meta("Artist"), meta("LicenseShortName")) {
                (Some(a), Some(l)) => Some(format!("{a}, {l}")),
                (a, l) => a.or(l),
            };
            Some(ImageSource {
                image_url: image_url.to_owned(),
                page_url: page_url.to_owned(),
                caption: truncate(&caption, 300),
                credit,
            })
        })
        .collect()
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let decoded = rest.find(';').filter(|&end| end <= 10).and_then(|end| {
            let c = match &rest[1..end] {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                e => e
                    .strip_prefix("#x")
                    .or_else(|| e.strip_prefix("#X"))
                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                    .or_else(|| e.strip_prefix('#').and_then(|d| d.parse().ok()))
                    .and_then(char::from_u32),
            }?;
            Some((c, end))
        });
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &rest[end + 1..];
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

fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wikipedia_hits_become_encoded_urls_with_plain_snippets() {
        let v = serde_json::json!({ "query": { "search": [
            { "title": "Ownership (Rust)", "snippet": "the <span class=\"searchmatch\">borrow</span> checker &amp; lifetimes" },
            { "title": "Bayes' theorem", "snippet": "" },
        ]}});
        let r = parse_wikipedia(&v);
        assert_eq!(r[0].url, "https://en.wikipedia.org/wiki/Ownership_(Rust)");
        assert_eq!(
            r[0].snippet.as_deref(),
            Some("the borrow checker & lifetimes")
        );
        assert_eq!(r[1].url, "https://en.wikipedia.org/wiki/Bayes'_theorem");
        assert_eq!(r[1].snippet, None);
        assert!(parse_wikipedia(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn entity_decoding_handles_numeric_and_leaves_stray_ampersands() {
        assert_eq!(
            decode_entities("a &#39;b&#x27; &lt;c&gt; & d &bogus;"),
            "a 'b' <c> & d &bogus;"
        );
    }

    #[test]
    fn llm_format_numbers_sources_and_truncates_snippets() {
        let resp = SearchResponse {
            provider: "wikipedia",
            sources: vec![
                SearchSource {
                    title: " ".into(),
                    url: "https://a.example".into(),
                    snippet: Some("x".repeat(300)),
                },
                SearchSource {
                    title: "B".into(),
                    url: "https://b.example".into(),
                    snippet: None,
                },
            ],
        };
        let text = format_for_llm(&resp);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "[1] https://a.example");
        assert_eq!(lines[2].trim().chars().count(), 241);
        assert_eq!(lines[3], "[2] B");
        assert!(!text.ends_with('\n'));
    }

    #[test]
    fn commons_hits_keep_search_order_rasters_and_source_captions() {
        let page = |index: i64, title: &str, mime: &str, desc: Option<&str>| {
            let mut meta = serde_json::json!({ "Artist": { "value": "<a href=\"u\">Jo</a>" } });
            if let Some(d) = desc {
                meta["ImageDescription"] = serde_json::json!({ "value": d });
            }
            serde_json::json!({ "index": index, "title": title, "imageinfo": [{
                "mime": mime, "url": "https://up/orig", "thumburl": format!("https://up/{index}"),
                "descriptionurl": format!("https://commons/{index}"), "extmetadata": meta,
            }]})
        };
        let v = serde_json::json!({ "query": { "pages": [
            page(2, "File:Rook in snow.jpg", "image/jpeg", None),
            page(3, "File:Diagram.svg", "image/svg+xml", Some("a diagram")),
            page(1, "File:Crow.png", "image/png", Some("<i>Corvus</i> &amp; friends")),
        ]}});
        let r = parse_commons(&v);
        assert_eq!(r.len(), 2, "the SVG is dropped");
        assert_eq!(r[0].caption, "Corvus & friends");
        assert_eq!(r[0].image_url, "https://up/1");
        assert_eq!(r[0].page_url, "https://commons/1");
        assert_eq!(r[0].credit.as_deref(), Some("Jo"));
        assert_eq!(
            r[1].caption, "Rook in snow",
            "the file name stands in for a missing description"
        );
        assert!(parse_commons(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn brave_image_hits_need_a_page_and_an_image_url() {
        let v = serde_json::json!({ "results": [
            { "title": "Gothic &amp; <b>nave</b>", "url": "https://p/1", "thumbnail": { "src": "https://t/1" }, "source": "p.example" },
            { "title": "no page", "thumbnail": { "src": "https://t/2" } },
            { "title": "original only", "url": "https://p/3", "properties": { "url": "https://o/3" }, "source": "" },
        ]});
        let r = parse_brave_images(&v);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].caption, "Gothic & nave");
        assert_eq!(r[0].credit.as_deref(), Some("p.example"));
        assert_eq!(r[1].image_url, "https://o/3");
        assert_eq!(r[1].credit, None);
    }
}
