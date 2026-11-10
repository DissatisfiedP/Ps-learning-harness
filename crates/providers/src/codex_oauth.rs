//! ChatGPT (Codex) subscription sign-in, via the same OAuth
//! authorization-code + PKCE flow the Codex CLI uses. The browser redirects
//! to a loopback listener on `localhost:1455/auth/callback`, so the learner
//! never copies a code or token.
//!
//! Constants are the Codex CLI's (`app_EMoamEEZ73f0CkXaXp7hrann`, the
//! `auth.openai.com` endpoints, the fixed 1455 redirect, which the client
//! registration requires). Requests use `Authorization: Bearer` plus the
//! `chatgpt-account-id` from the id_token.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use reqwest::header::{HeaderMap, AUTHORIZATION};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::ProviderError;

/// Public client id for the Codex CLI. Not a secret.
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
pub const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
pub const SCOPES: &str = "openid profile email offline_access";
pub const CALLBACK_PORT: u16 = 1455;
pub const CALLBACK_PATH: &str = "/auth/callback";
pub const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
pub const BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
pub const ORIGINATOR: &str = "codex_cli_rs";
/// Codex CLI release whose model catalog we request. The backend's `/models`
/// drops every preset whose `minimal_client_version` is above this, so our own
/// crate version (0.1.0) gets an empty list. Bump with new Codex releases.
pub const CLIENT_VERSION: &str = "0.161.0";
/// How long the loopback listener waits for the browser.
pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Claim namespace OpenAI puts the ChatGPT account id under.
const AUTH_CLAIM: &str = "/https:~1~1api.openai.com~1auth/chatgpt_account_id";

/// A stored ChatGPT grant. `expires_at` is Unix seconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    /// `chatgpt-account-id`, from the id_token. `None` for an env token,
    /// whose own claims carry it.
    pub account_id: Option<String>,
    /// Email from the id_token, shown as "Signed in as …".
    pub account: Option<String>,
}

impl Session {
    /// Refresh 5 minutes before expiry, as the CLI does.
    pub fn needs_refresh(&self, now: i64) -> bool {
        self.expires_at - now < 300
    }
    pub fn can_refresh(&self) -> bool {
        !self.refresh_token.is_empty()
    }
}

/// What `codex_login_start` hands the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginStart {
    pub auth_url: String,
    pub state: String,
}

/// PKCE verifier/state plus the bound loopback socket, held between start
/// and wait. Dropping it frees port 1455. Never serialized.
pub struct PendingLogin {
    pub verifier: String,
    pub state: String,
    listener: Option<std::net::TcpListener>,
}

impl PendingLogin {
    /// Bind the loopback listener first, so a busy port fails before the
    /// browser opens.
    pub fn new() -> Result<Self, ProviderError> {
        let listener = bind(CALLBACK_PORT)?;
        Ok(Self::with_listener(listener))
    }

    fn with_listener(listener: std::net::TcpListener) -> Self {
        use rand::distributions::Distribution;
        let mut rng = rand::thread_rng();
        let mut random = |n| -> String {
            rand::distributions::Alphanumeric
                .sample_iter(&mut rng)
                .take(n)
                .map(char::from)
                .collect()
        };
        Self {
            verifier: random(64),
            state: random(32),
            listener: Some(listener),
        }
    }

    pub fn start(&self) -> LoginStart {
        let challenge = {
            let digest = Sha256::digest(self.verifier.as_bytes());
            base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, digest)
        };
        let mut url = reqwest::Url::parse(AUTHORIZE_URL).expect("static authorize URL");
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", CLIENT_ID)
            .append_pair("redirect_uri", REDIRECT_URI)
            .append_pair("scope", SCOPES)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &self.state)
            .append_pair("id_token_add_organizations", "true")
            .append_pair("codex_cli_simplified_flow", "true");
        LoginStart {
            auth_url: url.to_string(),
            state: self.state.clone(),
        }
    }

    /// Wait for the browser redirect (up to `LOGIN_TIMEOUT`, or until
    /// `cancelled`), then exchange the code. The port is released when this
    /// returns, whatever the outcome.
    pub async fn finish(
        mut self,
        client: &reqwest::Client,
        cancelled: &AtomicBool,
    ) -> Result<Session, ProviderError> {
        let listener = self
            .listener
            .take()
            .ok_or_else(|| ProviderError::Config("sign-in already finished".into()))?;
        let code = wait_for_code(listener, &self.state, LOGIN_TIMEOUT, cancelled).await?;
        let form = [
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", REDIRECT_URI),
            ("client_id", CLIENT_ID),
            ("code_verifier", self.verifier.as_str()),
        ];
        let data = token_request(client, &form).await?;
        session_from(&data, unix_now())
    }
}

fn bind(port: u16) -> Result<std::net::TcpListener, ProviderError> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            ProviderError::Config(format!(
                "port {port} is in use (is a Codex CLI sign-in open?); close it and try again"
            ))
        } else {
            ProviderError::Config(format!("cannot listen on localhost:{port}: {e}"))
        }
    })?;
    listener
        .set_nonblocking(true)
        .map_err(|e| ProviderError::Config(format!("cannot listen on localhost:{port}: {e}")))?;
    Ok(listener)
}

const DONE_PAGE: &str = "<html><body><p>Login complete. Return to the app.</p></body></html>";

/// Serve `CALLBACK_PATH` until a request carries `code` (with the expected
/// `state`) or `error`. Other paths get 404 and the wait continues.
async fn wait_for_code(
    listener: std::net::TcpListener,
    state: &str,
    timeout: Duration,
    cancelled: &AtomicBool,
) -> Result<String, ProviderError> {
    let listener = tokio::net::TcpListener::from_std(listener)
        .map_err(|e| ProviderError::Config(format!("cannot listen for sign-in: {e}")))?;
    let deadline = Instant::now() + timeout;
    loop {
        if cancelled.load(Ordering::SeqCst) {
            return Err(ProviderError::Config("sign-in cancelled".into()));
        }
        if Instant::now() >= deadline {
            return Err(ProviderError::Config("sign-in timed out; try again".into()));
        }
        let accepted = tokio::select! {
            a = listener.accept() => a,
            () = tokio::time::sleep(Duration::from_millis(250)) => continue,
        };
        let Ok((mut stream, _)) = accepted else {
            continue;
        };
        let target = match read_target(&mut stream).await {
            Some(t) => t,
            None => continue,
        };
        let outcome = callback_outcome(&target, state);
        let (status, body) = match &outcome {
            Outcome::NotFound => ("404 Not Found", "Not found"),
            Outcome::Code(_) => ("200 OK", DONE_PAGE),
            Outcome::Failed(_) => ("400 Bad Request", "Sign-in failed. Return to the app."),
        };
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;
        match outcome {
            Outcome::NotFound => {}
            Outcome::Code(code) => return Ok(code),
            Outcome::Failed(e) => return Err(e),
        }
    }
}

/// The request target (`/path?query`) of a GET, read from the head only.
async fn read_target(stream: &mut tokio::net::TcpStream) -> Option<String> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 16 * 1024 {
        let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk))
            .await
            .ok()?
            .ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8_lossy(&buf);
    let mut parts = head.lines().next()?.split_whitespace();
    let _method = parts.next()?;
    parts.next().map(str::to_owned)
}

#[derive(Debug)]
enum Outcome {
    NotFound,
    Code(String),
    Failed(ProviderError),
}

fn callback_outcome(target: &str, state: &str) -> Outcome {
    let Ok(url) = reqwest::Url::parse(&format!("http://localhost{target}")) else {
        return Outcome::NotFound;
    };
    if url.path() != CALLBACK_PATH {
        return Outcome::NotFound;
    }
    let param = |k: &str| {
        url.query_pairs()
            .find(|(name, _)| name == k)
            .map(|(_, v)| v.into_owned())
    };
    if param("state").as_deref() != Some(state) {
        return Outcome::Failed(ProviderError::Config(
            "OAuth state mismatch; start sign-in again".into(),
        ));
    }
    if let Some(error) = param("error") {
        let detail = param("error_description").unwrap_or(error);
        return Outcome::Failed(ProviderError::Config(format!("sign-in failed: {detail}")));
    }
    match param("code").filter(|c| !c.is_empty()) {
        Some(code) => Outcome::Code(code),
        None => Outcome::Failed(ProviderError::Config(
            "the browser returned no authorization code".into(),
        )),
    }
}

/// Rotate the access token. Any rejection means the grant is gone and the
/// learner must sign in again (`Unauthorized`).
pub async fn refresh(
    client: &reqwest::Client,
    session: &Session,
    now: i64,
) -> Result<Session, ProviderError> {
    let form = [
        ("grant_type", "refresh_token"),
        ("refresh_token", session.refresh_token.as_str()),
        ("client_id", CLIENT_ID),
    ];
    let data = token_request(client, &form).await.map_err(|e| match e {
        ProviderError::Api { .. } | ProviderError::Unauthorized(_) => {
            ProviderError::Unauthorized("ChatGPT sign-in expired; sign in again".into())
        }
        other => other,
    })?;
    Ok(merge_refresh(session, session_from(&data, now)?))
}

/// A refresh response may omit the rotating refresh token or the id_token;
/// keep what the old session had.
fn merge_refresh(old: &Session, mut next: Session) -> Session {
    if next.refresh_token.is_empty() {
        next.refresh_token.clone_from(&old.refresh_token);
    }
    if next.account_id.is_none() {
        next.account_id.clone_from(&old.account_id);
    }
    if next.account.is_none() {
        next.account.clone_from(&old.account);
    }
    next
}

async fn token_request(
    client: &reqwest::Client,
    form: &[(&str, &str)],
) -> Result<serde_json::Value, ProviderError> {
    let resp = client
        .post(TOKEN_URL)
        .form(form)
        .send()
        .await
        .map_err(ProviderError::transport)?;
    let status = resp.status();
    let text = resp.text().await.map_err(ProviderError::transport)?;
    if !status.is_success() {
        return Err(crate::error_from_body(status.as_u16(), &text));
    }
    serde_json::from_str(&text).map_err(|e| ProviderError::Protocol(format!("token response: {e}")))
}

fn session_from(data: &serde_json::Value, now: i64) -> Result<Session, ProviderError> {
    let access_token = data["access_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ProviderError::Protocol("token response has no access_token".into()))?
        .to_owned();
    let claims = data["id_token"].as_str().and_then(crate::jwt_claims);
    let expires_in = data["expires_in"]
        .as_i64()
        .filter(|&s| s > 0)
        .unwrap_or(3600);
    Ok(Session {
        account_id: claims.as_ref().and_then(account_id),
        account: claims
            .as_ref()
            .and_then(|c| c["email"].as_str())
            .map(str::to_owned),
        access_token,
        refresh_token: data["refresh_token"].as_str().unwrap_or("").to_owned(),
        expires_at: now + expires_in,
    })
}

fn account_id(claims: &serde_json::Value) -> Option<String> {
    claims
        .pointer(AUTH_CLAIM)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Identity headers for every ChatGPT call (inference and model listing).
/// The account id comes from the stored session, else (env token) from the
/// access token's own claims.
pub(crate) fn headers(credential: &crate::Credential) -> Result<HeaderMap, ProviderError> {
    let crate::Credential::Codex(session) = credential else {
        // A token pasted under the old flow lands here: say how to move on.
        return Err(ProviderError::Config(
            "ChatGPT now needs a ChatGPT sign-in: in Settings, disconnect ChatGPT (Codex), then choose Sign in with ChatGPT".into(),
        ));
    };
    let account = session
        .account_id
        .clone()
        .or_else(|| {
            crate::jwt_claims(&session.access_token)
                .as_ref()
                .and_then(account_id)
        })
        .ok_or_else(|| {
            ProviderError::Config("this is not a ChatGPT access token (no account id)".into())
        })?;
    let mut headers = HeaderMap::new();
    let insert = crate::catalog::insert;
    insert(
        &mut headers,
        AUTHORIZATION.as_str(),
        &format!("Bearer {}", session.access_token),
    )?;
    insert(&mut headers, "chatgpt-account-id", &account)?;
    insert(&mut headers, "OpenAI-Beta", "responses=experimental")?;
    insert(&mut headers, "originator", ORIGINATOR)?;
    Ok(headers)
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Credential;
    use base64::Engine;

    fn jwt(claims: &str) -> String {
        let enc = |v: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v);
        format!("{}.{}.sig", enc("{}"), enc(claims))
    }

    fn session() -> Session {
        Session {
            access_token: "access-1".into(),
            refresh_token: "refresh-1".into(),
            expires_at: 4_000_000_000,
            account_id: Some("acct_1".into()),
            account: Some("learner@example.com".into()),
        }
    }

    fn pending() -> PendingLogin {
        PendingLogin::with_listener(bind(0).unwrap())
    }

    #[test]
    fn authorize_url_carries_pkce_state_and_codex_params() {
        let pending = pending();
        let start = pending.start();
        let url = reqwest::Url::parse(&start.auth_url).unwrap();
        assert_eq!(
            format!("{}{}", url.origin().ascii_serialization(), url.path()),
            AUTHORIZE_URL
        );
        let q: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["client_id"], CLIENT_ID);
        assert_eq!(q["redirect_uri"], "http://localhost:1455/auth/callback");
        assert_eq!(q["scope"], "openid profile email offline_access");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["state"], start.state);
        assert_eq!(start.state.len(), 32);
        assert_eq!(q["codex_cli_simplified_flow"], "true");
        assert_eq!(q["id_token_add_organizations"], "true");
        let digest = Sha256::digest(pending.verifier.as_bytes());
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
        assert_eq!(q["code_challenge"], expected.as_str());
    }

    #[test]
    fn token_response_yields_account_id_and_email_from_id_token() {
        let data = serde_json::json!({
            "access_token": "a",
            "refresh_token": "r",
            "expires_in": 600,
            "id_token": jwt(r#"{"email":"me@x.io","https://api.openai.com/auth":{"chatgpt_account_id":"acct_9"}}"#),
        });
        let s = session_from(&data, 1000).unwrap();
        assert_eq!(s.account_id.as_deref(), Some("acct_9"));
        assert_eq!(s.account.as_deref(), Some("me@x.io"));
        assert_eq!(s.expires_at, 1600);
        assert_eq!(s.refresh_token, "r");
    }

    #[test]
    fn refresh_keeps_old_refresh_token_and_account_when_omitted() {
        let data = serde_json::json!({ "access_token": "access-2", "expires_in": 3600 });
        let merged = merge_refresh(&session(), session_from(&data, 0).unwrap());
        assert_eq!(merged.access_token, "access-2");
        assert_eq!(merged.refresh_token, "refresh-1");
        assert_eq!(merged.account_id.as_deref(), Some("acct_1"));
        let rotated = serde_json::json!({ "access_token": "a3", "refresh_token": "refresh-2" });
        let merged = merge_refresh(&session(), session_from(&rotated, 0).unwrap());
        assert_eq!(merged.refresh_token, "refresh-2");
    }

    #[test]
    fn needs_refresh_inside_five_minutes() {
        let s = session();
        assert!(!s.needs_refresh(s.expires_at - 301));
        assert!(s.needs_refresh(s.expires_at - 299));
        assert!(s.needs_refresh(s.expires_at + 10));
    }

    #[test]
    fn headers_use_stored_account_else_token_claim() {
        let h = headers(&Credential::Codex(session())).unwrap();
        assert_eq!(h["authorization"], "Bearer access-1");
        assert_eq!(h["chatgpt-account-id"], "acct_1");
        assert_eq!(h["originator"], ORIGINATOR);
        assert_eq!(h["openai-beta"], "responses=experimental");
        let env = Session {
            access_token: jwt(
                r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acct_env"}}"#,
            ),
            refresh_token: String::new(),
            expires_at: 0,
            account_id: None,
            account: None,
        };
        let h = headers(&Credential::Codex(env.clone())).unwrap();
        assert_eq!(h["chatgpt-account-id"], "acct_env");
        let plain = Session {
            access_token: "sk-plain".into(),
            ..env
        };
        assert!(headers(&Credential::Codex(plain)).is_err());
        assert!(headers(&Credential::ApiKey { key: "k".into() }).is_err());
    }

    #[test]
    fn callback_checks_path_state_and_code() {
        assert!(matches!(callback_outcome("/", "s"), Outcome::NotFound));
        assert!(matches!(
            callback_outcome("/auth/callback?code=c1&state=s", "s"),
            Outcome::Code(c) if c == "c1"
        ));
        assert!(matches!(
            callback_outcome("/auth/callback?code=c1&state=bad", "s"),
            Outcome::Failed(_)
        ));
        assert!(matches!(
            callback_outcome("/auth/callback?error=access_denied&state=s", "s"),
            Outcome::Failed(_)
        ));
    }

    #[test]
    fn busy_port_is_a_clear_error() {
        let held = bind(0).unwrap();
        let port = held.local_addr().unwrap().port();
        let err = bind(port).unwrap_err().to_string();
        assert!(err.contains(&format!("port {port} is in use")), "{err}");
    }

    #[tokio::test]
    async fn listener_serves_404_then_captures_code_and_frees_port() {
        let listener = bind(0).unwrap();
        let port = listener.local_addr().unwrap().port();
        let cancelled = AtomicBool::new(false);
        let client = reqwest::Client::new();
        let base = format!("http://127.0.0.1:{port}");
        let hits = async {
            let r = client
                .get(format!("{base}/favicon.ico"))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 404);
            let r = client
                .get(format!("{base}/auth/callback?code=abc&state=st"))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 200);
            assert!(r.text().await.unwrap().contains("Login complete"));
        };
        let (code, ()) = tokio::join!(
            wait_for_code(listener, "st", Duration::from_secs(10), &cancelled),
            hits
        );
        assert_eq!(code.unwrap(), "abc");
        bind(port).expect("port released after the callback");
    }

    #[tokio::test]
    async fn listener_rejects_wrong_state_and_honours_cancel() {
        let listener = bind(0).unwrap();
        let port = listener.local_addr().unwrap().port();
        let cancelled = AtomicBool::new(false);
        let hit = async {
            let url = format!("http://127.0.0.1:{port}/auth/callback?code=abc&state=evil");
            reqwest::get(url).await.unwrap().status()
        };
        let (result, status) = tokio::join!(
            wait_for_code(listener, "st", Duration::from_secs(10), &cancelled),
            hit
        );
        assert_eq!(status, 400);
        assert!(result.unwrap_err().to_string().contains("state mismatch"));

        let cancelled = AtomicBool::new(true);
        let err = wait_for_code(bind(0).unwrap(), "st", Duration::from_secs(10), &cancelled)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("cancelled"));
    }
}
