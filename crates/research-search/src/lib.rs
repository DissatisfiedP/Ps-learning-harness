//! Web search for the researcher agent.
//!
//! Chain, first non-empty result wins: Brave Search API (when
//! `BRAVE_API_KEY` is set) → Wikipedia search API (keyless, ignores
//! `recency`).

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

pub async fn search(client: &Client, req: &SearchRequest) -> Result<SearchResponse, SearchError> {
    let mut failures = Vec::new();
    let brave_key = std::env::var("BRAVE_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty());
    if let Some(key) = brave_key {
        match brave(client, req, key.trim()).await {
            Ok(r) if !r.sources.is_empty() => return Ok(r),
            Ok(_) => failures.push("brave: no results".to_owned()),
            Err(e) => failures.push(e.to_string()),
        }
    }
    match wikipedia(client, req).await {
        Ok(r) if !r.sources.is_empty() => return Ok(r),
        Ok(_) => failures.push("wikipedia: no results".to_owned()),
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
        .get("https://api.search.brave.com/res/v1/web/search")
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

async fn wikipedia(client: &Client, req: &SearchRequest) -> Result<SearchResponse, SearchError> {
    let fail = |message: String| SearchError::Provider {
        provider: "wikipedia",
        message,
    };
    let limit = req.limit().to_string();
    let v: serde_json::Value = client
        .get("https://en.wikipedia.org/w/api.php")
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
}
