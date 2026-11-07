//! The providers the app can talk to and how each one is addressed.

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};

use crate::{claude_oauth, jwt_claims, workbuddy, Endpoint, ProviderError, Wire};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderId {
    Anthropic,
    ClaudeCode,
    Openai,
    OpenaiCodex,
    Openrouter,
    Deepseek,
    Zai,
    Workbuddy,
}

/// How the learner connects a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CredentialKind {
    ApiKey,
    /// A ChatGPT access token (the one the Codex CLI stores).
    ChatgptToken,
    /// Browser sign-in, or an API key from the account's key page.
    WorkbuddyAccount,
    /// Claude Pro/Max subscription sign-in (Claude Code OAuth).
    ClaudeSubscription,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Credential {
    ApiKey { key: String },
    Workbuddy(workbuddy::Session),
    ClaudeCode(claude_oauth::Session),
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSpec {
    pub id: ProviderId,
    pub name: &'static str,
    #[serde(skip)]
    pub wire: Wire,
    pub default_base_url: &'static str,
    pub env_var: &'static str,
    pub credential: CredentialKind,
    pub key_url: Option<&'static str>,
}

/// Every provider serves its models from a listing endpoint; nothing is
/// hardcoded here. `crate::models` fetches the list (see `docs/models.md`).
const SPECS: [ProviderSpec; 8] = [
    ProviderSpec {
        id: ProviderId::Anthropic,
        name: "Anthropic",
        wire: Wire::AnthropicMessages,
        default_base_url: "https://api.anthropic.com/v1",
        env_var: "ANTHROPIC_API_KEY",
        credential: CredentialKind::ApiKey,
        key_url: Some("https://console.anthropic.com/settings/keys"),
    },
    ProviderSpec {
        id: ProviderId::ClaudeCode,
        name: "Claude (subscription)",
        wire: Wire::AnthropicMessages,
        default_base_url: claude_oauth::BASE_URL,
        env_var: "ANTHROPIC_OAUTH_TOKEN",
        credential: CredentialKind::ClaudeSubscription,
        key_url: None,
    },
    ProviderSpec {
        id: ProviderId::Openai,
        name: "OpenAI",
        wire: Wire::OpenaiResponses,
        default_base_url: "https://api.openai.com/v1",
        env_var: "OPENAI_API_KEY",
        credential: CredentialKind::ApiKey,
        key_url: Some("https://platform.openai.com/api-keys"),
    },
    ProviderSpec {
        id: ProviderId::OpenaiCodex,
        name: "ChatGPT (Codex)",
        wire: Wire::OpenaiResponses,
        default_base_url: "https://chatgpt.com/backend-api/codex",
        env_var: "OPENAI_CODEX_OAUTH_TOKEN",
        credential: CredentialKind::ChatgptToken,
        key_url: None,
    },
    ProviderSpec {
        id: ProviderId::Openrouter,
        name: "OpenRouter",
        wire: Wire::OpenaiChat,
        default_base_url: "https://openrouter.ai/api/v1",
        env_var: "OPENROUTER_API_KEY",
        credential: CredentialKind::ApiKey,
        key_url: Some("https://openrouter.ai/settings/keys"),
    },
    ProviderSpec {
        id: ProviderId::Deepseek,
        name: "DeepSeek",
        wire: Wire::OpenaiChat,
        default_base_url: "https://api.deepseek.com",
        env_var: "DEEPSEEK_API_KEY",
        credential: CredentialKind::ApiKey,
        key_url: Some("https://platform.deepseek.com/api_keys"),
    },
    ProviderSpec {
        id: ProviderId::Zai,
        name: "Z.AI",
        wire: Wire::OpenaiChat,
        default_base_url: "https://api.z.ai/api/paas/v4",
        env_var: "ZAI_API_KEY",
        credential: CredentialKind::ApiKey,
        key_url: Some("https://z.ai/manage-apikey/apikey-list"),
    },
    ProviderSpec {
        id: ProviderId::Workbuddy,
        name: "WorkBuddy",
        wire: Wire::OpenaiChat,
        default_base_url: workbuddy::HOST,
        env_var: "WORKBUDDY_API_KEY",
        credential: CredentialKind::WorkbuddyAccount,
        key_url: Some(workbuddy::KEYS_URL),
    },
];

impl ProviderId {
    pub const ALL: [ProviderId; 8] = [
        ProviderId::Anthropic,
        ProviderId::ClaudeCode,
        ProviderId::Openai,
        ProviderId::OpenaiCodex,
        ProviderId::Openrouter,
        ProviderId::Deepseek,
        ProviderId::Zai,
        ProviderId::Workbuddy,
    ];

    pub fn spec(self) -> &'static ProviderSpec {
        &SPECS[self as usize]
    }

    pub fn credential_from_env(self) -> Option<Credential> {
        let key = std::env::var(self.spec().env_var).ok()?;
        let key = key.trim().to_owned();
        if key.is_empty() {
            return None;
        }
        // ANTHROPIC_OAUTH_TOKEN is a subscription bearer, not an API key.
        // It has no refresh token, so give it a long, non-refreshing expiry.
        if self == ProviderId::ClaudeCode {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            return Some(Credential::ClaudeCode(claude_oauth::Session {
                access_token: key,
                refresh_token: String::new(),
                expires_at: now + 30 * 24 * 3600,
                account: None,
            }));
        }
        Some(Credential::ApiKey { key })
    }

    /// The base URL in use: the learner's override, else the provider's.
    pub fn base_url(self, override_url: Option<&str>) -> String {
        override_url
            .map(str::trim)
            .filter(|b| !b.is_empty())
            .unwrap_or(self.spec().default_base_url)
            .trim_end_matches('/')
            .to_owned()
    }

    /// The models.dev provider key that describes this provider's models
    /// (public metadata for context window, vision, efforts). `None` when no
    /// public catalog covers it.
    pub fn catalog_key(self) -> Option<&'static str> {
        match self {
            ProviderId::Anthropic | ProviderId::ClaudeCode => Some("anthropic"),
            ProviderId::Openai | ProviderId::OpenaiCodex => Some("openai"),
            ProviderId::Openrouter => Some("openrouter"),
            ProviderId::Deepseek => Some("deepseek"),
            ProviderId::Zai => Some("zai"),
            ProviderId::Workbuddy => None,
        }
    }

    /// Auth headers for a keyed provider, without wire specifics (no
    /// `Accept`, no path). WorkBuddy and the Claude subscription have their
    /// own header sets in their modules.
    pub fn auth_headers(self, credential: &Credential) -> Result<HeaderMap, ProviderError> {
        let spec = self.spec();
        let Credential::ApiKey { key } = credential else {
            return Err(ProviderError::Config(format!(
                "{} needs an API key",
                spec.name
            )));
        };
        let mut headers = HeaderMap::new();
        match self {
            ProviderId::Anthropic => {
                insert(&mut headers, "x-api-key", key)?;
                insert(&mut headers, "anthropic-version", "2023-06-01")?;
            }
            ProviderId::OpenaiCodex => {
                insert(
                    &mut headers,
                    AUTHORIZATION.as_str(),
                    &format!("Bearer {key}"),
                )?;
                let account = jwt_claims(key)
                    .and_then(|c| {
                        c.pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
                            .and_then(|v| v.as_str())
                            .map(str::to_owned)
                    })
                    .ok_or_else(|| {
                        ProviderError::Config(
                            "this is not a ChatGPT access token (no account id claim)".into(),
                        )
                    })?;
                insert(&mut headers, "chatgpt-account-id", &account)?;
                insert(&mut headers, "OpenAI-Beta", "responses=experimental")?;
            }
            ProviderId::Openrouter => {
                insert(
                    &mut headers,
                    AUTHORIZATION.as_str(),
                    &format!("Bearer {key}"),
                )?;
                insert(&mut headers, "X-Title", "P's Learning Harness")?;
            }
            _ => insert(
                &mut headers,
                AUTHORIZATION.as_str(),
                &format!("Bearer {key}"),
            )?,
        }
        Ok(headers)
    }

    pub fn endpoint(
        self,
        credential: &Credential,
        base_url: Option<&str>,
        conversation_id: &str,
    ) -> Result<Endpoint, ProviderError> {
        let spec = self.spec();
        let base = self.base_url(base_url);
        if self == ProviderId::Workbuddy {
            return workbuddy::endpoint(credential, &base, conversation_id);
        }
        if self == ProviderId::ClaudeCode {
            return claude_oauth::endpoint(credential, &base, conversation_id);
        }
        let mut headers = self.auth_headers(credential)?;
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let path = match spec.wire {
            Wire::OpenaiChat => "/chat/completions",
            Wire::OpenaiResponses => "/responses",
            Wire::AnthropicMessages => "/messages",
        };
        Ok(Endpoint {
            wire: spec.wire,
            url: format!("{base}{path}"),
            headers,
            claude_code: false,
        })
    }
}

pub(crate) fn insert(
    headers: &mut HeaderMap,
    name: &str,
    value: &str,
) -> Result<(), ProviderError> {
    let name = HeaderName::from_bytes(name.as_bytes())
        .map_err(|_| ProviderError::Config(format!("invalid header name {name}")))?;
    let value = HeaderValue::from_str(value)
        .map_err(|_| ProviderError::Config("credential contains invalid characters".into()))?;
    headers.insert(name, value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_table_is_indexed_by_id() {
        for id in ProviderId::ALL {
            assert_eq!(id.spec().id, id);
        }
    }

    #[test]
    fn base_url_override_replaces_host_and_keeps_wire_path() {
        let cred = Credential::ApiKey { key: "k".into() };
        let ep = ProviderId::Openai
            .endpoint(&cred, Some("http://127.0.0.1:8080/v1/"), "c")
            .unwrap();
        assert_eq!(ep.url, "http://127.0.0.1:8080/v1/responses");
        let ep = ProviderId::Deepseek.endpoint(&cred, None, "c").unwrap();
        assert_eq!(ep.url, "https://api.deepseek.com/chat/completions");
        assert_eq!(ep.headers["authorization"], "Bearer k");
    }

    #[test]
    fn codex_requires_account_claim() {
        use base64::Engine;
        let enc = |v: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v);
        let token = format!(
            "{}.{}.sig",
            enc("{}"),
            enc(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acct_1"}}"#)
        );
        let ep = ProviderId::OpenaiCodex
            .endpoint(&Credential::ApiKey { key: token }, None, "c")
            .unwrap();
        assert_eq!(ep.headers["chatgpt-account-id"], "acct_1");
        assert!(ProviderId::OpenaiCodex
            .endpoint(
                &Credential::ApiKey {
                    key: "sk-plain".into()
                },
                None,
                "c"
            )
            .is_err());
    }
}
