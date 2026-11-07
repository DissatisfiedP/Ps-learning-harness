//! WorkBuddy (www.workbuddy.ai), Tencent's desktop agent, served by the
//! CodeBuddy gateway. There are no public API docs; everything here was read
//! from Tencent's own client (`@tencent-ai/codebuddy-code`, whose
//! `codebuddy-lite-wb` build is the CLI the WorkBuddy desktop app runs) and
//! checked against the live host. See `docs/workbuddy-provider.md`.
//!
//! Two ways to connect:
//! - browser sign-in: the IDE-plugin login (`/v2/plugin/auth/*`), giving a
//!   ~27-day access token plus a refresh token;
//! - API key from <https://www.workbuddy.ai/profile/keys>, sent the way the
//!   official CLI sends `CODEBUDDY_API_KEY` (`Authorization` + `X-API-Key`).
//!
//! Chat is OpenAI chat-completions at `/v2/chat/completions`, streaming only,
//! and the gateway requires the client headers its own app sends.

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::catalog::insert;
use crate::{
    error_from_body, jwt_claims, Capabilities, Client, Credential, Endpoint, ModelInfo,
    ProviderError, Wire,
};

pub const HOST: &str = "https://www.workbuddy.ai";
pub const KEYS_URL: &str = "https://www.workbuddy.ai/profile/keys";
const DOMAIN: &str = "www.workbuddy.ai";
const PLATFORM: &str = "workbuddy-ai";
/// Desktop build whose client headers the gateway expects (5.9.3, CLI 2.165.0).
const APP_VERSION: &str = "5.9.3";
const CLI_VERSION: &str = "2.165.0";
/// Gateway code: the learner has not finished signing in yet.
const LOGIN_PENDING: i64 = 11217;
/// Refresh this long before the access token expires.
const REFRESH_MARGIN_SECS: i64 = 24 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds.
    pub expires_at: i64,
    pub refresh_expires_at: i64,
    pub domain: String,
    pub account: Option<String>,
}

impl Session {
    pub fn needs_refresh(&self, now: i64) -> bool {
        self.expires_at - now < REFRESH_MARGIN_SECS
    }
    pub fn can_refresh(&self, now: i64) -> bool {
        !self.refresh_token.is_empty() && self.refresh_expires_at > now
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginStart {
    pub state: String,
    pub auth_url: String,
}

/// Step 1 of browser sign-in: get a login state and the URL to open.
pub async fn begin_login(client: &Client, base: &str) -> Result<LoginStart, ProviderError> {
    let req = client
        .post(format!("{base}/v2/plugin/auth/state?platform={PLATFORM}"))
        .headers(anonymous_headers())
        .json(&serde_json::json!({}));
    let (code, msg, data) = envelope(req).await?;
    if code != 0 {
        return Err(ProviderError::Api {
            status: 200,
            message: format!("{msg} ({code})"),
        });
    }
    serde_json::from_value(data).map_err(|e| ProviderError::Protocol(format!("login state: {e}")))
}

/// Step 2: poll until the learner approves in the browser. `Ok(None)` means
/// still pending; the official client polls every 1.5 s for up to 5 minutes.
pub async fn poll_login(
    client: &Client,
    base: &str,
    state: &str,
    now: i64,
) -> Result<Option<Session>, ProviderError> {
    let req = client
        .get(format!("{base}/v2/plugin/auth/token"))
        .query(&[("state", state)])
        .headers(anonymous_headers());
    match envelope(req).await? {
        (LOGIN_PENDING, ..) => Ok(None),
        (0, _, data) => session_from_token(&data, now).map(Some),
        (code, msg, _) => Err(ProviderError::Api {
            status: 200,
            message: format!("{msg} ({code})"),
        }),
    }
}

/// Rotates the token pair. Fails with `Unauthorized` when the refresh token
/// is no longer accepted, which means signing in again.
pub async fn refresh(
    client: &Client,
    base: &str,
    session: &Session,
    now: i64,
) -> Result<Session, ProviderError> {
    let mut headers = HeaderMap::new();
    insert(
        &mut headers,
        "Authorization",
        &format!("Bearer {}", session.access_token),
    )?;
    insert(&mut headers, "X-Refresh-Token", &session.refresh_token)?;
    insert(&mut headers, "X-Auth-Refresh-Source", "plugin")?;
    insert(&mut headers, "X-Domain", &session.domain)?;
    let req = client
        .post(format!("{base}/v2/plugin/auth/token/refresh"))
        .headers(headers)
        .json(&serde_json::json!({}));
    match envelope(req).await? {
        (0, _, data) => {
            let mut next = session_from_token(&data, now)?;
            if next.refresh_token.is_empty() {
                next.refresh_token.clone_from(&session.refresh_token);
                next.refresh_expires_at = session.refresh_expires_at;
            }
            Ok(next)
        }
        (code, msg, _) => Err(ProviderError::Unauthorized(format!("{msg} ({code})"))),
    }
}

/// The account's chat models from the product config (`/v3/config`).
pub async fn list_models(
    client: &Client,
    base: &str,
    credential: &Credential,
) -> Result<Vec<ModelInfo>, ProviderError> {
    let req = client
        .get(format!("{base}/v3/config"))
        .headers(client_headers(credential)?);
    match envelope(req).await? {
        (0, _, data) => Ok(parse_models(&data)),
        (code, msg, _) => Err(ProviderError::Api {
            status: 200,
            message: format!("{msg} ({code})"),
        }),
    }
}

pub(crate) fn endpoint(
    credential: &Credential,
    base: &str,
    conversation_id: &str,
) -> Result<Endpoint, ProviderError> {
    let mut headers = client_headers(credential)?;
    headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    // One id per request, as the official client mints for each model call.
    let message_id = uuid::Uuid::new_v4().simple().to_string();
    for (name, value) in [
        ("X-Conversation-ID", conversation_id),
        ("X-Conversation-Request-ID", &message_id),
        ("X-Conversation-Message-ID", &message_id),
        ("X-Request-ID", &message_id),
        ("X-Agent-Intent", "craft"),
        ("X-Agent-Purpose", "conversation"),
        ("X-Agent-Runtime", "local-agent"),
    ] {
        insert(&mut headers, name, value)?;
    }
    Ok(Endpoint {
        wire: Wire::OpenaiChat,
        url: format!("{base}/v2/chat/completions"),
        headers,
        claude_code: false,
    })
}

/// Headers every authenticated call carries: the credential plus the client
/// identity the gateway checks (requests without it are rejected as 11128).
fn client_headers(credential: &Credential) -> Result<HeaderMap, ProviderError> {
    let mut headers = HeaderMap::new();
    match credential {
        Credential::Workbuddy(session) => {
            insert(
                &mut headers,
                "Authorization",
                &format!("Bearer {}", session.access_token),
            )?;
            insert(&mut headers, "X-Domain", &session.domain)?;
        }
        Credential::ApiKey { key } => {
            insert(&mut headers, "Authorization", &format!("Bearer {key}"))?;
            insert(&mut headers, "X-API-Key", key)?;
        }
        Credential::ClaudeCode(_) => {
            return Err(ProviderError::Config(
                "WorkBuddy needs an API key or a WorkBuddy sign-in".into(),
            ));
        }
    }
    for (name, value) in [
        ("X-Product", "SaaS"),
        ("X-IDE-Type", "WorkBuddy"),
        ("X-IDE-Name", "WorkBuddy"),
        ("X-IDE-Version", APP_VERSION),
        ("X-Requested-With", "XMLHttpRequest"),
    ] {
        insert(&mut headers, name, value)?;
    }
    let ua = format!("WorkBuddy/{APP_VERSION} {PLATFORM}/{APP_VERSION} CLI/{CLI_VERSION}");
    headers.insert(
        USER_AGENT,
        HeaderValue::from_str(&ua).map_err(|_| ProviderError::Config("user agent".into()))?,
    );
    Ok(headers)
}

fn anonymous_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in [
        ("X-Domain", DOMAIN),
        ("X-No-Authorization", "true"),
        ("X-No-User-Id", "true"),
        ("X-No-Enterprise-Id", "true"),
        ("X-No-Department-Info", "true"),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    headers
}

/// Sends a request and splits the `{code, msg, data}` envelope.
async fn envelope(req: reqwest::RequestBuilder) -> Result<(i64, String, Value), ProviderError> {
    let resp = req
        .send()
        .await
        .map_err(|e| ProviderError::Transport(e.without_url().to_string()))?;
    let status = resp.status().as_u16();
    let text = resp
        .text()
        .await
        .map_err(|e| ProviderError::Transport(e.without_url().to_string()))?;
    let parsed: Option<Value> = serde_json::from_str(&text).ok();
    let code = parsed
        .as_ref()
        .and_then(|v| v.get("code"))
        .and_then(Value::as_i64);
    match (status, parsed, code) {
        (200..=299, Some(mut v), Some(code)) => {
            let msg = v
                .get("msg")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            Ok((
                code,
                msg,
                v.get_mut("data").map(Value::take).unwrap_or(Value::Null),
            ))
        }
        (400..=499, Some(v), Some(LOGIN_PENDING)) => {
            // Some deployments send the pending code with HTTP 400.
            let msg = v
                .get("msg")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            Ok((LOGIN_PENDING, msg, Value::Null))
        }
        _ => Err(error_from_body(status, &text)),
    }
}

fn session_from_token(data: &Value, now: i64) -> Result<Session, ProviderError> {
    let str_field = |k: &str| {
        data.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let secs = |k: &str| data.get(k).and_then(Value::as_i64).unwrap_or(0);
    let access_token = str_field("accessToken");
    if access_token.is_empty() {
        return Err(ProviderError::Protocol(
            "sign-in returned no access token".into(),
        ));
    }
    let domain = Some(str_field("domain"))
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| DOMAIN.to_owned());
    let account = jwt_claims(&access_token).and_then(|c| {
        ["nickname", "preferred_username", "email"]
            .iter()
            .find_map(|k| {
                c.get(*k)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
            })
    });
    Ok(Session {
        expires_at: now + secs("expiresIn"),
        refresh_expires_at: now + secs("refreshExpiresIn"),
        refresh_token: str_field("refreshToken"),
        access_token,
        domain,
        account,
    })
}

/// Chat models only: drops completion/classifier/image entries the config
/// also lists, puts the account default first.
fn parse_models(data: &Value) -> Vec<ModelInfo> {
    let Some(models) = data.get("models").and_then(Value::as_array) else {
        return Vec::new();
    };
    let as_u32 = |v: &Value, k: &str| {
        v.get(k)
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
    };
    let mut out: Vec<(bool, ModelInfo)> = models
        .iter()
        .filter_map(|m| {
            let id = m.get("id").and_then(Value::as_str)?;
            let tags: Vec<&str> = m
                .get("tags")
                .and_then(Value::as_array)
                .map(|t| t.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let skip = ["nes-", "completion-", "codewise-"]
                .iter()
                .any(|p| id.starts_with(p))
                || m.get("supportsExtra").and_then(Value::as_bool) == Some(true)
                || as_u32(m, "maxOutputTokens").is_some_and(|n| n <= 256)
                || tags.contains(&"text-to-image");
            if skip {
                return None;
            }
            let reasoning = m.get("reasoning");
            let efforts: Vec<String> = reasoning
                .and_then(|r| r.get("supportedEfforts"))
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            let default_effort = reasoning
                .and_then(|r| r.get("defaultEffort").or_else(|| r.get("effort")))
                .and_then(Value::as_str)
                .filter(|e| efforts.iter().any(|x| x == e))
                .or_else(|| efforts.first().map(String::as_str))
                .map(str::to_owned);
            let name = m
                .get("name")
                .and_then(Value::as_str)
                .filter(|n| !n.is_empty())
                .unwrap_or(id);
            let info = ModelInfo {
                id: id.to_owned(),
                name: name.to_owned(),
                description: m
                    .get("descriptionEn")
                    .and_then(Value::as_str)
                    .filter(|d| !d.is_empty())
                    .map(str::to_owned),
                context_window: as_u32(m, "maxInputTokens"),
                max_output_tokens: as_u32(m, "maxOutputTokens"),
                efforts,
                default_effort,
                cost_label: m.get("credits").and_then(Value::as_str).map(str::to_owned),
                capabilities: Capabilities::default(),
            };
            Some((
                m.get("isDefault").and_then(Value::as_bool) == Some(true),
                info,
            ))
        })
        .collect();
    out.sort_by_key(|(is_default, _)| !is_default);
    out.into_iter().map(|(_, m)| m).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn catalog_keeps_chat_models_and_effort_invariant() {
        let data = json!({ "models": [
            { "id": "hy3", "name": "Hy3", "maxOutputTokens": 32000, "maxInputTokens": 192000,
              "reasoning": { "defaultEffort": "max", "supportedEfforts": ["low", "high"] }, "credits": "x0.00 credits" },
            { "id": "completion-fast", "maxOutputTokens": 4000 },
            { "id": "classifier", "maxOutputTokens": 64 },
            { "id": "painter", "maxOutputTokens": 4000, "tags": ["text-to-image"] },
            { "id": "auto", "name": "Auto", "isDefault": true },
        ]});
        let models = parse_models(&data);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["auto", "hy3"]);
        // A default effort outside the supported set must not be sent.
        assert_eq!(models[1].default_effort.as_deref(), Some("low"));
        assert_eq!(models[1].context_window, Some(192_000));
        assert_eq!(parse_models(&json!({ "models": null })), Vec::new());
    }

    #[test]
    fn token_response_becomes_session_with_absolute_expiry() {
        let data = json!({ "accessToken": "a.b.c", "refreshToken": "r", "expiresIn": 100,
                           "refreshExpiresIn": 200, "domain": "" });
        let s = session_from_token(&data, 1_000).unwrap();
        assert_eq!((s.expires_at, s.refresh_expires_at), (1_100, 1_200));
        assert_eq!(s.domain, DOMAIN);
        assert!(s.needs_refresh(1_000));
        assert!(s.can_refresh(1_199) && !s.can_refresh(1_200));
        assert!(session_from_token(&json!({}), 0).is_err());
    }

    #[test]
    fn api_key_and_session_authenticate_differently() {
        let key = client_headers(&Credential::ApiKey {
            key: "wb-key".into(),
        })
        .unwrap();
        assert_eq!(key["x-api-key"], "wb-key");
        assert!(!key.contains_key("x-domain"));
        let session = Session {
            access_token: "tok".into(),
            refresh_token: String::new(),
            expires_at: 0,
            refresh_expires_at: 0,
            domain: DOMAIN.into(),
            account: None,
        };
        let ep = endpoint(&Credential::Workbuddy(session), HOST, "conv-1").unwrap();
        assert_eq!(ep.url, "https://www.workbuddy.ai/v2/chat/completions");
        assert_eq!(ep.headers["authorization"], "Bearer tok");
        assert_eq!(ep.headers["x-domain"], DOMAIN);
        assert_eq!(ep.headers["x-conversation-id"], "conv-1");
        assert!(!ep.headers.contains_key("x-api-key"));
    }
}
