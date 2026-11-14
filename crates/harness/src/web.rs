//! `page_fetch` for the researcher: GET with a 20 s timeout and a 2 MB cap
//! (32 MiB for a PDF), HTML converted to text in-tree (the `html2text` crate
//! isn't available offline; a small converter is enough for reading
//! articles), truncated to about 40k characters. A PDF comes back as
//! page-tagged text (`[p.3] …`), so a paper can be read without leaving the
//! tool; a scanned page says it has no text layer.
//!
//! Every fetch goes through a [`GuardedClient`]: a URL the model was steered
//! to must not reach this device, its LAN or a cloud metadata service. Only
//! `http`/`https` URLs are fetched, and loopback, private (RFC 1918, ULA
//! `fc00::/7`, which holds the EC2 metadata `fd00:ec2::254`), link-local
//! (`169.254.0.0/16`, which holds `169.254.169.254`; `fe80::/10`), shared
//! (`100.64.0.0/10`), unspecified, multicast, broadcast and reserved
//! addresses are refused — as IP literals in the URL, as DNS answers (the
//! client's resolver drops them, so a name can't rebind between a check and
//! the connect), and on every redirect hop. IPv4 embedded in IPv6 (mapped,
//! NAT64 `64:ff9b::/96`, 6to4 `2002::/16`) is judged as the IPv4 address. A
//! system proxy, when one is configured, resolves the name itself; the
//! literal and redirect checks still apply then.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use providers::Client;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::Url;

const TIMEOUT: Duration = Duration::from_secs(20);
const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_CHARS: usize = 40_000;
const MAX_REDIRECTS: usize = 10;
const USER_AGENT: &str = "LearningHarness/0.1 (researcher)";

/// Why `ip` may not be fetched (a phrase for the model's error), or `None`
/// for a public address.
fn refused_kind(ip: IpAddr) -> Option<&'static str> {
    match ip {
        IpAddr::V4(v4) => refused_v4(v4),
        IpAddr::V6(v6) => refused_v6(v6),
    }
}

fn refused_v4(ip: Ipv4Addr) -> Option<&'static str> {
    let [a, b, c, _] = ip.octets();
    Some(if ip.is_unspecified() || a == 0 {
        "unspecified"
    } else if ip.is_loopback() {
        "loopback (this device)"
    } else if ip == Ipv4Addr::new(169, 254, 169, 254) {
        "cloud metadata"
    } else if ip.is_link_local() {
        "link-local"
    } else if ip.is_private() {
        "private network (LAN)"
    } else if a == 100 && (64..128).contains(&b) {
        "shared (carrier-grade NAT)"
    } else if ip.is_broadcast() {
        "broadcast"
    } else if ip.is_multicast() {
        "multicast"
    } else if a >= 240 || (a == 192 && b == 0 && c == 0) || (a == 198 && (b & 0xfe) == 18) {
        "reserved"
    } else {
        return None;
    })
}

fn refused_v6(ip: Ipv6Addr) -> Option<&'static str> {
    let seg = ip.segments();
    if let Some(v4) = ip.to_ipv4_mapped() {
        return refused_v4(v4);
    }
    // NAT64 (64:ff9b::/96) and 6to4 (2002::/16) carry an IPv4 address the
    // gateway connects to.
    if seg[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        let [.., hi, lo] = seg;
        return refused_v4(Ipv4Addr::from((u32::from(hi) << 16) | u32::from(lo)));
    }
    if seg[0] == 0x2002 {
        return refused_v4(Ipv4Addr::from(
            (u32::from(seg[1]) << 16) | u32::from(seg[2]),
        ));
    }
    Some(if ip.is_unspecified() {
        "unspecified"
    } else if ip.is_loopback() {
        "loopback (this device)"
    } else if ip == Ipv6Addr::new(0xfd00, 0xec2, 0, 0, 0, 0, 0, 0x254) {
        "cloud metadata"
    } else if (seg[0] & 0xfe00) == 0xfc00 {
        "private network (unique local)"
    } else if (seg[0] & 0xffc0) == 0xfe80 || (seg[0] & 0xffc0) == 0xfec0 {
        "link-local"
    } else if ip.is_multicast() {
        "multicast"
    } else if seg[..6] == [0, 0, 0, 0, 0, 0] {
        // Deprecated IPv4-compatible `::a.b.c.d`.
        "reserved"
    } else {
        return None;
    })
}

/// The refusal for one address, written for the model.
fn refusal(host: &str, ip: IpAddr, kind: &str) -> String {
    let at = if host == ip.to_string() {
        String::new()
    } else {
        format!(" (it resolves to {ip})")
    };
    format!(
        "refused: `{host}` is a {kind} address{at}; only public web pages can be fetched — find a public source instead"
    )
}

/// The scheme and IP-literal half of the guard (names are judged by the
/// client's resolver). `exempt` lets one IP literal through (tests).
fn check(url: &Url, exempt: Option<IpAddr>) -> Result<(), String> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!(
            "refused: only http(s) URLs can be fetched, not `{}:`",
            url.scheme()
        ));
    }
    // The URL parser has already normalised IPv4 spellings (`0x7f.1`,
    // `2130706433`) to dotted form; IPv6 hosts come bracketed.
    let Some(host) = url.host_str() else {
        return Err("refused: the URL has no host".into());
    };
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let Ok(ip) = bare.parse::<IpAddr>() else {
        return Ok(());
    };
    match refused_kind(ip) {
        Some(kind) if exempt != Some(ip) => Err(refusal(&ip.to_string(), ip, kind)),
        _ => Ok(()),
    }
}

/// A refusal carried through reqwest's error chain (from the redirect policy
/// or the resolver), so the model reads the reason rather than "error
/// following redirect".
#[derive(Debug)]
struct Refused(String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

/// System DNS (`getaddrinfo`, via tokio) with refused addresses dropped; a
/// name left with no public address fails with the reason.
struct PublicOnly;

impl Resolve for PublicOnly {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let host = name.as_str().to_owned();
            let all: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let public: Vec<SocketAddr> = all
                .iter()
                .copied()
                .filter(|a| refused_kind(a.ip()).is_none())
                .collect();
            if public.is_empty() {
                let why = all.first().map_or_else(
                    || format!("refused: `{host}` has no address"),
                    |a| refusal(&host, a.ip(), refused_kind(a.ip()).unwrap_or("refused")),
                );
                return Err(Box::new(Refused(why)) as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new(public.into_iter()) as Addrs)
        })
    }
}

/// The HTTP client every model-steered fetch uses (see the module doc).
#[derive(Clone)]
pub struct GuardedClient {
    client: Client,
    exempt: Option<IpAddr>,
}

impl GuardedClient {
    pub fn new() -> Result<Self, String> {
        Self::build(None)
    }

    /// As [`GuardedClient::new`], letting the one IP literal `ip` through
    /// (in URLs and redirects; DNS answers are never exempt). For tests
    /// against a local server.
    #[allow(dead_code)]
    pub(crate) fn exempting(ip: IpAddr) -> Result<Self, String> {
        Self::build(Some(ip))
    }

    fn build(exempt: Option<IpAddr>) -> Result<Self, String> {
        let policy = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                return attempt.error(Refused(format!(
                    "refused: more than {MAX_REDIRECTS} redirects"
                )));
            }
            match check(attempt.url(), exempt) {
                Ok(()) => attempt.follow(),
                Err(why) => {
                    let why = why.strip_prefix("refused: ").unwrap_or(&why);
                    let why = format!("refused: the page redirected to {}: {why}", attempt.url());
                    attempt.error(Refused(why))
                }
            }
        });
        let client = Client::builder()
            .connect_timeout(TIMEOUT)
            .redirect(policy)
            .dns_resolver(Arc::new(PublicOnly))
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        Ok(Self { client, exempt })
    }

    /// Parse `url` and apply the scheme and IP-literal checks. Errors are
    /// written for the model.
    pub fn check_url(&self, url: &str) -> Result<Url, String> {
        let url = url.trim();
        let parsed = Url::parse(url).map_err(|e| format!("`{url}` is not a valid URL ({e})"))?;
        check(&parsed, self.exempt)?;
        Ok(parsed)
    }

    /// GET `url` through the guard; a success status only. Returns the
    /// response and its lowercased content type.
    pub async fn get(&self, url: &str) -> Result<(reqwest::Response, String), String> {
        let url = self.check_url(url)?;
        let resp = self
            .client
            .get(url)
            .header("user-agent", USER_AGENT)
            .timeout(TIMEOUT)
            .send()
            .await
            .map_err(fetch_error)?;
        let status = resp.status();
        if !status.is_success() {
            // reqwest hands back a redirect it won't follow (another scheme)
            // without asking the policy: say where it pointed and why.
            let target = (resp.headers().get("location"))
                .and_then(|v| v.to_str().ok())
                .and_then(|l| resp.url().join(l).ok());
            if let (true, Some(target)) = (status.is_redirection(), target) {
                let why = check(&target, self.exempt)
                    .err()
                    .unwrap_or_else(|| "refused: it could not be followed".into());
                let why = why.strip_prefix("refused: ").unwrap_or(&why);
                return Err(format!("refused: the page redirected to {target}: {why}"));
            }
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
}

/// A failed send as text for the model: a guard refusal anywhere in the
/// chain is the whole message.
fn fetch_error(e: reqwest::Error) -> String {
    let refused = {
        let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&e);
        let mut found = None;
        while let Some(err) = source {
            if let Some(r) = err.downcast_ref::<Refused>() {
                found = Some(r.0.clone());
                break;
            }
            source = err.source();
        }
        found
    };
    refused.unwrap_or_else(|| format!("fetch failed: {}", e.without_url()))
}

/// True when the URL's path ends in `.pdf` (query string and fragment
/// ignored), so a PDF gets the larger fetch cap before its headers arrive.
fn looks_like_pdf_url(url: &str) -> bool {
    url.trim()
        .to_ascii_lowercase()
        .split(['?', '#'])
        .next()
        .is_some_and(|p| p.ends_with(".pdf"))
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
pub async fn page_fetch(client: &GuardedClient, url: &str) -> Result<String, String> {
    let pdf_url = looks_like_pdf_url(url);
    let (resp, content_type) = client.get(url).await?;
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
pub async fn fetch_pdf(client: &GuardedClient, url: &str) -> Result<Vec<u8>, String> {
    let (resp, content_type) = client.get(url).await?;
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
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn local_private_and_metadata_addresses_are_refused() {
        let refused = [
            "127.0.0.1",
            "127.9.9.9",
            "0.0.0.0",
            "0.1.2.3",
            "10.0.0.5",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "169.254.10.10",
            "100.64.0.1",
            "100.100.100.200",
            "255.255.255.255",
            "224.0.0.1",
            "240.0.0.1",
            "::1",
            "::",
            "fd00:ec2::254",
            "fc00::1",
            "fe80::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "64:ff9b::a00:1",
            "2002:c0a8:101::1",
            "::127.0.0.1",
        ];
        for ip in refused {
            let ip: IpAddr = ip.parse().unwrap();
            assert!(refused_kind(ip).is_some(), "{ip} must be refused");
        }
        assert_eq!(
            refused_kind("169.254.169.254".parse().unwrap()),
            Some("cloud metadata")
        );
        assert_eq!(
            refused_kind("fd00:ec2::254".parse().unwrap()),
            Some("cloud metadata")
        );
        for ip in [
            "93.184.216.34",
            "8.8.8.8",
            "172.32.0.1",
            "100.128.0.1",
            "2606:4700::1111",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
        ] {
            let ip: IpAddr = ip.parse().unwrap();
            assert_eq!(refused_kind(ip), None, "{ip} is public");
        }
    }

    #[test]
    fn only_public_http_urls_pass_the_literal_check() {
        let web = GuardedClient::new().unwrap();
        for url in [
            "file:///etc/passwd",
            "ftp://example.org/x",
            "gopher://example.org/",
            "http://127.0.0.1:8080/admin",
            // Other spellings of loopback, normalised by the URL parser.
            "http://0x7f.1/",
            "http://2130706433/",
            "http://[::1]/",
            "http://[::ffff:7f00:1]/",
            "http://169.254.169.254/latest/meta-data/",
            "http://[fd00:ec2::254]/",
            "https://192.168.0.10/",
            "http://0.0.0.0:3000/",
        ] {
            let err = web.check_url(url).unwrap_err();
            assert!(err.starts_with("refused:"), "{url}: {err}");
        }
        let err = web.check_url("http://169.254.169.254/").unwrap_err();
        assert!(err.contains("cloud metadata"), "{err}");
        assert!(web.check_url("https://example.org/a?b=1").is_ok());
        assert!(web.check_url("  http://93.184.216.34/  ").is_ok());
        assert!(web
            .check_url("not a url")
            .unwrap_err()
            .contains("not a valid URL"));
        // The test exemption lets exactly one literal through.
        let exempt = GuardedClient::exempting("127.0.0.1".parse().unwrap()).unwrap();
        assert!(exempt.check_url("http://127.0.0.1:9/").is_ok());
        assert!(exempt.check_url("http://127.0.0.2:9/").is_err());
    }

    /// A one-route-per-path HTTP server on 127.0.0.1: `/page` answers HTML,
    /// `/to?<url>` redirects to `<url>` (`PORT` replaced by its own port).
    async fn serve() -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let path = req.split_whitespace().nth(1).unwrap_or("/").to_owned();
                    let resp = match path.strip_prefix("/to?") {
                        Some(to) => format!(
                            "HTTP/1.1 302 Found\r\nLocation: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            to.replace("PORT", &port.to_string())
                        ),
                        None => {
                            let body = "<html><body><p>local page</p></body></html>";
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            )
                        }
                    };
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn fetches_refuse_local_targets_by_literal_by_dns_and_on_redirect() {
        let port = serve().await;
        let web = GuardedClient::new().unwrap();
        let err = page_fetch(&web, &format!("http://127.0.0.1:{port}/page"))
            .await
            .unwrap_err();
        assert!(
            err.starts_with("refused:") && err.contains("loopback"),
            "{err}"
        );
        // A name is judged by what it resolves to.
        let err = page_fetch(&web, &format!("http://localhost:{port}/page"))
            .await
            .unwrap_err();
        assert!(
            err.starts_with("refused: `localhost`") && err.contains("resolves to"),
            "{err}"
        );

        // With the server's own address let through, the page is read...
        let exempt = GuardedClient::exempting("127.0.0.1".parse().unwrap()).unwrap();
        let text = page_fetch(&exempt, &format!("http://127.0.0.1:{port}/page"))
            .await
            .unwrap();
        assert!(text.contains("local page"), "{text}");
        // ...an exempt redirect is followed...
        let ok = format!("http://127.0.0.1:{port}/to?http://127.0.0.1:PORT/page");
        assert!(page_fetch(&exempt, &ok)
            .await
            .unwrap()
            .contains("local page"));
        // ...but every hop is checked: another loopback literal is refused,
        let err = page_fetch(
            &exempt,
            &format!("http://127.0.0.1:{port}/to?http://127.0.0.2:PORT/page"),
        )
        .await
        .unwrap_err();
        assert!(
            err.starts_with("refused: the page redirected to http://127.0.0.2:")
                && err.contains("loopback"),
            "{err}"
        );
        // a name resolving to loopback is refused by the resolver (never exempt),
        let err = page_fetch(
            &exempt,
            &format!("http://127.0.0.1:{port}/to?http://localhost:PORT/page"),
        )
        .await
        .unwrap_err();
        assert!(err.starts_with("refused: `localhost`"), "{err}");
        // and so is a non-http scheme.
        let err = page_fetch(
            &exempt,
            &format!("http://127.0.0.1:{port}/to?file:///etc/passwd"),
        )
        .await
        .unwrap_err();
        assert!(err.contains("only http(s)"), "{err}");
    }

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
