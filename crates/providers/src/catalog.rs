//! The providers the app can talk to and how each one is addressed.

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};

use crate::{jwt_claims, workbuddy, Endpoint, ModelInfo, ProviderError, Wire};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderId {
    Anthropic,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Credential {
    ApiKey { key: String },
    Workbuddy(workbuddy::Session),
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
    /// Suggested model ids; any id the provider serves can be typed in.
    pub models: &'static [&'static str],
}

const SPECS: [ProviderSpec; 7] = [
    ProviderSpec {
        id: ProviderId::Anthropic,
        name: "Anthropic",
        wire: Wire::AnthropicMessages,
        default_base_url: "https://api.anthropic.com/v1",
        env_var: "ANTHROPIC_API_KEY",
        credential: CredentialKind::ApiKey,
        key_url: Some("https://console.anthropic.com/settings/keys"),
        models: &["claude-sonnet-4-5", "claude-opus-4-5"],
    },
    ProviderSpec {
        id: ProviderId::Openai,
        name: "OpenAI",
        wire: Wire::OpenaiResponses,
        default_base_url: "https://api.openai.com/v1",
        env_var: "OPENAI_API_KEY",
        credential: CredentialKind::ApiKey,
        key_url: Some("https://platform.openai.com/api-keys"),
        models: &["gpt-5.5", "gpt-5"],
    },
    ProviderSpec {
        id: ProviderId::OpenaiCodex,
        name: "ChatGPT (Codex)",
        wire: Wire::OpenaiResponses,
        default_base_url: "https://chatgpt.com/backend-api/codex",
        env_var: "OPENAI_CODEX_OAUTH_TOKEN",
        credential: CredentialKind::ChatgptToken,
        key_url: None,
        models: &["gpt-5.5"],
    },
    ProviderSpec {
        id: ProviderId::Openrouter,
        name: "OpenRouter",
        wire: Wire::OpenaiChat,
        default_base_url: "https://openrouter.ai/api/v1",
        env_var: "OPENROUTER_API_KEY",
        credential: CredentialKind::ApiKey,
        key_url: Some("https://openrouter.ai/settings/keys"),
        models: &["deepseek/deepseek-chat", "anthropic/claude-sonnet-4.5"],
    },
    ProviderSpec {
        id: ProviderId::Deepseek,
        name: "DeepSeek",
        wire: Wire::OpenaiChat,
        default_base_url: "https://api.deepseek.com",
        env_var: "DEEPSEEK_API_KEY",
        credential: CredentialKind::ApiKey,
        key_url: Some("https://platform.deepseek.com/api_keys"),
        models: &["deepseek-chat", "deepseek-reasoner"],
    },
    ProviderSpec {
        id: ProviderId::Zai,
        name: "Z.AI",
        wire: Wire::OpenaiChat,
        default_base_url: "https://api.z.ai/api/paas/v4",
        env_var: "ZAI_API_KEY",
        credential: CredentialKind::ApiKey,
        key_url: Some("https://z.ai/manage-apikey/apikey-list"),
        models: &["glm-4.6"],
    },
    ProviderSpec {
        id: ProviderId::Workbuddy,
        name: "WorkBuddy",
        wire: Wire::OpenaiChat,
        default_base_url: workbuddy::HOST,
        env_var: "WORKBUDDY_API_KEY",
        credential: CredentialKind::WorkbuddyAccount,
        key_url: Some(workbuddy::KEYS_URL),
        // Catalog is per-account; fetched live by `workbuddy::list_models`.
        models: &[],
    },
];

impl ProviderId {
    pub const ALL: [ProviderId; 7] = [
        ProviderId::Anthropic,
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
        let key = key.trim();
        (!key.is_empty()).then(|| Credential::ApiKey {
            key: key.to_owned(),
        })
    }

    /// Static model suggestions (WorkBuddy's come from its live catalog).
    pub fn suggested_models(self) -> Vec<ModelInfo> {
        self.spec()
            .models
            .iter()
            .map(|id| ModelInfo::named(id))
            .collect()
    }

    pub fn endpoint(
        self,
        credential: &Credential,
        base_url: Option<&str>,
        conversation_id: &str,
    ) -> Result<Endpoint, ProviderError> {
        let spec = self.spec();
        let base = base_url
            .map(str::trim)
            .filter(|b| !b.is_empty())
            .unwrap_or(spec.default_base_url)
            .trim_end_matches('/');
        if self == ProviderId::Workbuddy {
            return workbuddy::endpoint(credential, base, conversation_id);
        }
        let Credential::ApiKey { key } = credential else {
            return Err(ProviderError::Config(format!(
                "{} needs an API key",
                spec.name
            )));
        };

        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let path = match spec.wire {
            Wire::OpenaiChat => "/chat/completions",
            Wire::OpenaiResponses => "/responses",
            Wire::AnthropicMessages => "/messages",
        };
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
                insert(&mut headers, "X-Title", "Learning Harness")?;
            }
            _ => insert(
                &mut headers,
                AUTHORIZATION.as_str(),
                &format!("Bearer {key}"),
            )?,
        }
        Ok(Endpoint {
            wire: spec.wire,
            url: format!("{base}{path}"),
            headers,
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
