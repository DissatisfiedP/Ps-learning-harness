//! Claude Pro/Max (Claude Code) subscription sign-in, via the same OAuth
//! authorization-code + PKCE flow the Claude Code CLI uses. No API key: the
//! learner approves in the browser and pastes the one-time code back.
//!
//! Constants are copied from the Claude Code client (`9d1c250a-…`, the
//! `claude.com` authorize URL, `platform.claude.com` token endpoint, the
//! `oauth-2025-04-20` beta and the "You are Claude Code" identity prefix) and
//! checked against omp's provider documentation. Requests use `Authorization:
//! Bearer` (never `x-api-key`) against the normal `/v1/messages` endpoint.

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{ProviderError, Wire};

/// Public client id for the Claude Code CLI. Not a secret.
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// `code=true` shows the subscription upsell; `loginWithClaudeAi` selects the
/// Claude.ai (Pro/Max) path rather than the Console API-key path.
pub const AUTHORIZE_URL: &str = "https://claude.com/cai/oauth/authorize";
/// Hosted callback: after approving, the browser shows a code to paste.
pub const MANUAL_REDIRECT_URL: &str = "https://platform.claude.com/oauth/code/callback";
pub const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
pub const SCOPES: &str =
    "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
/// Inference beta. `oauth-2025-04-20` is required for Bearer (subscription)
/// auth; `claude-code-20250219` identifies the client.
pub const OAUTH_BETA: &str = "claude-code-20250219,oauth-2025-04-20";
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Subscription requests must prepend this to `system[0]`.
pub const CLAUDE_CODE_PREFIX: &str = "You are Claude Code, Anthropic's official CLI for Claude.";
pub const BASE_URL: &str = "https://api.anthropic.com/v1";
const USER_AGENT_VALUE: &str = "claude-cli/2.1.280 (external, cli)";

/// A stored subscription grant. `expires_at` is Unix seconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    pub account: Option<String>,
}

impl Session {
    /// Refresh a little before expiry (5 min), as the CLI does.
    pub fn needs_refresh(&self, now: i64) -> bool {
        self.expires_at - now < 300
    }
    pub fn can_refresh(&self) -> bool {
        !self.refresh_token.is_empty()
    }
}

/// What `login_start` hands the UI: a URL to open and the PKCE verifier kept
/// server-side until `login_submit` exchanges the pasted code.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginStart {
    pub auth_url: String,
    pub state: String,
}

/// Server-side state between the two login steps. Never serialized to disk.
pub struct PendingLogin {
    pub verifier: String,
    pub state: String,
}

impl Default for PendingLogin {
    fn default() -> Self {
        Self::new()
    }
}

impl PendingLogin {
    /// Build the authorize URL with PKCE S256 and a random `state`.
    pub fn new() -> Self {
        use rand::distributions::Distribution;
        let mut rng = rand::thread_rng();
        let verifier: String = rand::distributions::Alphanumeric
            .sample_iter(&mut rng)
            .take(64)
            .map(char::from)
            .collect();
        let state: String = rand::distributions::Alphanumeric
            .sample_iter(&mut rng)
            .take(32)
            .map(char::from)
            .collect();
        Self { verifier, state }
    }

    pub fn start(&self) -> LoginStart {
        let challenge = {
            let digest = Sha256::digest(self.verifier.as_bytes());
            base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, digest)
        };
        let mut url = reqwest::Url::parse(AUTHORIZE_URL).expect("static authorize URL");
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("code", "true");
            q.append_pair("client_id", CLIENT_ID);
            q.append_pair("response_type", "code");
            q.append_pair("redirect_uri", MANUAL_REDIRECT_URL);
            q.append_pair("scope", SCOPES);
            q.append_pair("code_challenge", &challenge);
            q.append_pair("code_challenge_method", "S256");
            q.append_pair("state", &self.state);
        }
        LoginStart {
            auth_url: url.to_string(),
            state: self.state.clone(),
        }
    }

    /// Exchange the pasted `code[#state]` for a session. The browser shows the
    /// code as `<code>#<state>`; accept either that or a bare code.
    pub async fn exchange(
        self,
        client: &reqwest::Client,
        pasted: &str,
    ) -> Result<Session, ProviderError> {
        let (code, returned_state) = match pasted.trim().split_once('#') {
            Some((c, s)) => (c.trim(), Some(s.trim())),
            None => (pasted.trim(), None),
        };
        if code.is_empty() {
            return Err(ProviderError::Config(
                "no authorization code was pasted".into(),
            ));
        }
        if let Some(s) = returned_state {
            if s != self.state {
                return Err(ProviderError::Config(
                    "OAuth state mismatch; start sign-in again".into(),
                ));
            }
        }
        let body = serde_json::json!({
            "grant_type": "authorization_code",
            "code": code,
            "redirect_uri": MANUAL_REDIRECT_URL,
            "client_id": CLIENT_ID,
            "code_verifier": self.verifier,
            "state": self.state,
        });
        token_request(client, &body, &self.state).await
    }
}

/// Rotate the access token. A failed refresh means the grant is gone and the
/// learner must sign in again (the caller surfaces `Unauthorized`).
pub async fn refresh(
    client: &reqwest::Client,
    session: &Session,
    now: i64,
) -> Result<Session, ProviderError> {
    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": session.refresh_token,
        "client_id": CLIENT_ID,
        "scope": SCOPES,
    });
    let mut next = token_request(client, &body, "")
        .await
        .map_err(|e| match e {
            ProviderError::Api { .. } | ProviderError::Unauthorized(_) => {
                ProviderError::Unauthorized("subscription sign-in expired; sign in again".into())
            }
            other => other,
        })?;
    // A refresh response may omit the rotating refresh token; keep the old one.
    if next.refresh_token.is_empty() {
        next.refresh_token.clone_from(&session.refresh_token);
    }
    if next.account.is_none() {
        next.account.clone_from(&session.account);
    }
    // Sanity: never store a token that is already stale.
    if next.expires_at <= now {
        next.expires_at = now + 3600;
    }
    Ok(next)
}

async fn token_request(
    client: &reqwest::Client,
    body: &serde_json::Value,
    _state: &str,
) -> Result<Session, ProviderError> {
    let resp = client
        .post(TOKEN_URL)
        .header(CONTENT_TYPE, "application/json")
        .json(body)
        .send()
        .await
        .map_err(ProviderError::transport)?;
    let status = resp.status();
    let text = resp.text().await.map_err(ProviderError::transport)?;
    if !status.is_success() {
        return Err(crate::error_from_body(status.as_u16(), &text));
    }
    let data: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| ProviderError::Protocol(format!("token response: {e}")))?;
    let access_token = data["access_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ProviderError::Protocol("token response has no access_token".into()))?
        .to_owned();
    let refresh_token = data["refresh_token"].as_str().unwrap_or("").to_owned();
    let expires_in = data["expires_in"].as_i64().unwrap_or(3600);
    let account = data["account"]["email_address"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| data["organization"]["name"].as_str().map(str::to_owned));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    Ok(Session {
        access_token,
        refresh_token,
        expires_at: now + expires_in,
        account,
    })
}

/// Build inference headers for a subscription request. `x-api-key` is never
/// used here; the endpoint path is the same `/messages` as an API key.
pub(crate) fn endpoint(
    credential: &crate::Credential,
    base: &str,
    conversation_id: &str,
) -> Result<crate::Endpoint, ProviderError> {
    let mut headers = headers(credential, conversation_id)?;
    headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    Ok(crate::Endpoint {
        wire: Wire::AnthropicMessages,
        url: format!("{}/messages", base.trim_end_matches('/')),
        headers,
        claude_code: true,
    })
}

/// The subscription identity headers every call carries (inference and the
/// model listing alike). No `Accept`: each caller sets its own.
pub(crate) fn headers(
    credential: &crate::Credential,
    conversation_id: &str,
) -> Result<HeaderMap, ProviderError> {
    let crate::Credential::ClaudeCode(session) = credential else {
        return Err(ProviderError::Config(
            "Claude Code needs a subscription sign-in".into(),
        ));
    };
    let mut headers = HeaderMap::new();
    crate::catalog::insert(
        &mut headers,
        AUTHORIZATION.as_str(),
        &format!("Bearer {}", session.access_token),
    )?;
    crate::catalog::insert(&mut headers, "anthropic-version", ANTHROPIC_VERSION)?;
    crate::catalog::insert(&mut headers, "anthropic-beta", OAUTH_BETA)?;
    crate::catalog::insert(&mut headers, "x-app", "cli")?;
    let ua = HeaderValue::from_static(USER_AGENT_VALUE);
    headers.insert(USER_AGENT, ua);
    crate::catalog::insert(&mut headers, "X-Claude-Code-Session-Id", conversation_id)?;
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Credential;

    fn session() -> Session {
        Session {
            access_token: "sk-ant-oat-test".into(),
            refresh_token: "refresh".into(),
            expires_at: 4_000_000_000,
            account: Some("learner@example.com".into()),
        }
    }

    #[test]
    fn authorize_url_carries_pkce_and_subscription_params() {
        let pending = PendingLogin::new();
        let start = pending.start();
        let url = reqwest::Url::parse(&start.auth_url).unwrap();
        assert_eq!(url.origin().ascii_serialization(), "https://claude.com");
        let q: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(q["code"], "true");
        assert_eq!(q["client_id"], CLIENT_ID);
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["redirect_uri"], MANUAL_REDIRECT_URL);
        assert_eq!(q["scope"], SCOPES);
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["state"], start.state);
        // S256 challenge = base64url(sha256(verifier)), no padding.
        let digest = Sha256::digest(pending.verifier.as_bytes());
        let expected =
            base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, digest);
        assert_eq!(q["code_challenge"], expected.as_str());
        assert!(!q["code_challenge"].contains('='));
    }

    #[test]
    fn endpoint_uses_bearer_not_x_api_key_and_marks_claude_code() {
        let ep = endpoint(&Credential::ClaudeCode(session()), BASE_URL, "conv-1").unwrap();
        assert_eq!(ep.url, "https://api.anthropic.com/v1/messages");
        assert_eq!(ep.headers["authorization"], "Bearer sk-ant-oat-test");
        assert!(ep.headers.get("x-api-key").is_none());
        assert_eq!(ep.headers["anthropic-version"], ANTHROPIC_VERSION);
        assert_eq!(ep.headers["anthropic-beta"], OAUTH_BETA);
        assert_eq!(ep.headers["x-app"], "cli");
        assert_eq!(ep.headers["x-claude-code-session-id"], "conv-1");
        assert_eq!(ep.headers["user-agent"], USER_AGENT_VALUE);
        assert!(ep.claude_code);
    }

    #[test]
    fn endpoint_refuses_other_credentials() {
        assert!(endpoint(&Credential::ApiKey { key: "k".into() }, BASE_URL, "c").is_err());
    }

    #[test]
    fn needs_refresh_only_near_expiry() {
        let s = session();
        assert!(!s.needs_refresh(s.expires_at - 301));
        assert!(s.needs_refresh(s.expires_at - 299));
        assert!(s.can_refresh());
    }
}
