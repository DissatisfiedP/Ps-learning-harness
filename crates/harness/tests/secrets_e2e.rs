//! Saved sign-ins live in the secure store (`secrets.rs`), and a refresh
//! writes the rotated grant back through it. A fake WorkBuddy on 127.0.0.1
//! rotates single-use refresh tokens the way the real one does: reusing a
//! spent one fails, so two requests refreshing at once would sign the
//! learner out. The store is in memory; it is shared across reopened
//! harnesses as the OS store is across launches.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use harness::providers::{workbuddy, Credential, ProviderId};
use harness::secrets::{credential_item, SecretStore};
use harness::Harness;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// The fake account: which refresh token is live and how often it rotated.
struct Account {
    live_refresh: Mutex<String>,
    live_access: Mutex<String>,
    refreshes: AtomicUsize,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

async fn serve(account: Arc<Account>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let account = Arc::clone(&account);
            tokio::spawn(async move {
                let mut req = Vec::new();
                let mut buf = [0u8; 4096];
                let head = loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    req.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&req).into_owned();
                    if let Some(split) = text.find("\r\n\r\n") {
                        let len = header(&text[..split], "content-length")
                            .and_then(|v| v.parse::<usize>().ok())
                            .unwrap_or(0);
                        if req.len() >= split + 4 + len {
                            break text[..split].to_owned();
                        }
                    }
                };
                let path = head.split_whitespace().nth(1).unwrap_or_default();
                let body = if path.starts_with("/v2/plugin/auth/token/refresh") {
                    // Slow enough that a second request arrives meanwhile.
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    let offered = header(&head, "x-refresh-token").unwrap_or_default();
                    let mut live = account.live_refresh.lock().unwrap();
                    if offered == *live {
                        let n = account.refreshes.fetch_add(1, Ordering::SeqCst) + 2;
                        *live = format!("r{n}");
                        *account.live_access.lock().unwrap() = format!("a{n}");
                        json!({ "code": 0, "msg": "", "data": {
                            "accessToken": format!("a{n}"), "refreshToken": format!("r{n}"),
                            "expiresIn": 30 * 24 * 3600, "refreshExpiresIn": 90 * 24 * 3600,
                            "domain": "example" } })
                    } else {
                        json!({ "code": 11001, "msg": "refresh token already used" })
                    }
                } else if path.starts_with("/v3/config") {
                    let live = format!("Bearer {}", account.live_access.lock().unwrap());
                    if header(&head, "authorization") == Some(live.as_str()) {
                        json!({ "code": 0, "msg": "", "data": { "models": [{ "id": "wb-model" }] } })
                    } else {
                        json!({ "code": 401, "msg": "token expired" })
                    }
                } else {
                    json!({ "code": 404, "msg": path })
                };
                let body = body.to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    base
}

fn saved(secrets: &SecretStore) -> workbuddy::Session {
    let json = secrets
        .get(&credential_item(ProviderId::Workbuddy))
        .unwrap()
        .expect("a saved WorkBuddy sign-in");
    match serde_json::from_str::<Credential>(&json).unwrap() {
        Credential::Workbuddy(session) => session,
        other => panic!("not a WorkBuddy session: {other:?}"),
    }
}

fn model_ids(listed: &[harness::providers::ModelInfo]) -> Vec<&str> {
    listed.iter().map(|m| m.id.as_str()).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refresh_rotates_once_and_the_new_grant_survives_a_restart() {
    let root = std::env::temp_dir().join(format!("lh-e2e-secrets-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (repo, config) = (root.join("repo"), root.join("config"));
    let account = Arc::new(Account {
        live_refresh: Mutex::new("r1".into()),
        live_access: Mutex::new("a1".into()),
        refreshes: AtomicUsize::new(0),
    });
    let base = serve(Arc::clone(&account)).await;

    // A sign-in saved by an earlier launch, an hour from expiry: inside
    // WorkBuddy's one-day refresh margin.
    let secrets = Arc::new(SecretStore::memory());
    let expiring = Credential::Workbuddy(workbuddy::Session {
        access_token: "a1".into(),
        refresh_token: "r1".into(),
        expires_at: now() + 3600,
        refresh_expires_at: now() + 30 * 24 * 3600,
        domain: "example".into(),
        account: Some("learner".into()),
    });
    secrets
        .set(
            &credential_item(ProviderId::Workbuddy),
            &serde_json::to_string(&expiring).unwrap(),
        )
        .unwrap();
    let h = Arc::new(Harness::open_with_secrets(&repo, &config, Arc::clone(&secrets)).unwrap());
    h.set_base_url(ProviderId::Workbuddy, Some(&base)).unwrap();

    // Two requests need the refresh at the same moment: one rotates, the
    // other waits and uses its result instead of spending r1 again.
    let (first, second) = tokio::join!(
        h.models(ProviderId::Workbuddy, true),
        h.models(ProviderId::Workbuddy, true)
    );
    assert_eq!(model_ids(&first.unwrap()), ["wb-model"]);
    assert_eq!(model_ids(&second.unwrap()), ["wb-model"]);
    assert_eq!(account.refreshes.load(Ordering::SeqCst), 1);

    // The rotated grant went through the store, account label kept.
    let stored = saved(&secrets);
    assert_eq!(
        (stored.access_token.as_str(), stored.refresh_token.as_str()),
        ("a2", "r2")
    );
    assert_eq!(stored.account.as_deref(), Some("learner"));
    assert!(stored.expires_at > now() + 29 * 24 * 3600);

    // The next launch reads it back and has nothing to refresh.
    drop(h);
    let reopened = Harness::open_with_secrets(&repo, &config, Arc::clone(&secrets)).unwrap();
    assert_eq!(
        reopened.saved_credential(ProviderId::Workbuddy),
        Some(Credential::Workbuddy(stored))
    );
    let listed = reopened.models(ProviderId::Workbuddy, true).await.unwrap();
    assert_eq!(model_ids(&listed), ["wb-model"]);
    assert_eq!(account.refreshes.load(Ordering::SeqCst), 1);
    assert!(
        !config.join("credentials.json").exists(),
        "no plaintext copy is written"
    );
    let _ = std::fs::remove_dir_all(&root);
}
