//! Model catalogs: what each provider currently serves.
//!
//! Every provider publishes its own list, so the picker asks the provider
//! instead of carrying a hand-written table. The response shapes differ
//! (Anthropic reports capabilities and limits, OpenAI reports ids, Codex
//! reports a preset record), so each family has a parser. A field the provider
//! does not report stays `None`, which the UI words as "unverified".
//!
//! Listings are only part of the story: OpenAI and Z.AI return ids without
//! metadata, so [`fetch_catalog`] also reads the public models.dev catalog for
//! context windows, vision and effort levels. A live value always wins.

use std::collections::BTreeMap;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    claude_oauth, codex_oauth, workbuddy, Capabilities, Client, Credential, ModelInfo,
    ProviderError, ProviderId,
};

/// A hung catalog call must not block Settings.
const TIMEOUT: Duration = Duration::from_secs(30);
/// Anthropic pages at up to 1000 models per request; ten pages is far more
/// than any account has.
const MAX_PAGES: usize = 10;
const APP_USER_AGENT: &str = concat!("ps-learning-harness/", env!("CARGO_PKG_VERSION"));

/// Effort levels in the order the providers report them.
const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// Ids no chat model contains. OpenAI and Z.AI list everything the key can
/// reach — embeddings, speech, image generation — and return nothing but ids,
/// so the name is the only signal available.
const NON_CHAT: [&str; 11] = [
    "embedding",
    "tts",
    "whisper",
    "dall-e",
    "moderation",
    "sora",
    "realtime",
    "image",
    "audio",
    "babbage",
    "davinci",
];

/// The public metadata catalog (models.dev), trimmed to the providers this app
/// speaks to: provider key → model id → metadata.
pub const METADATA_URL: &str = "https://models.dev/api.json";
pub type Catalog = BTreeMap<String, BTreeMap<String, CatalogModel>>;

/// Metadata a listing endpoint may not report. Every field is optional: an
/// absent field means "not stated", never "unsupported".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogModel {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub efforts: Vec<String>,
}

impl CatalogModel {
    fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// The models `provider` currently serves, from the provider's own listing
/// endpoint. Base URL overrides (proxies, local servers) are honoured.
pub async fn list(
    client: &Client,
    provider: ProviderId,
    credential: &Credential,
    base_url: Option<&str>,
) -> Result<Vec<ModelInfo>, ProviderError> {
    let base = provider.base_url(base_url);
    if provider == ProviderId::Workbuddy {
        return workbuddy::list_models(client, &base, credential).await;
    }
    let mut headers = if provider == ProviderId::ClaudeCode {
        // The subscription endpoint needs the OAuth beta and identity headers.
        let session = uuid::Uuid::new_v4().simple().to_string();
        claude_oauth::headers(credential, &session)?
    } else {
        provider.auth_headers(credential)?
    };
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

    match provider {
        ProviderId::Anthropic | ProviderId::ClaudeCode => {
            let mut out = Vec::new();
            let mut after: Option<String> = None;
            for _ in 0..MAX_PAGES {
                let url = match &after {
                    Some(after) => format!("{base}/models?limit=1000&after_id={after}"),
                    None => format!("{base}/models?limit=1000"),
                };
                let (page, next) = anthropic_page(&get(client, &url, &headers).await?)?;
                out.extend(page);
                match next {
                    Some(next) => after = Some(next),
                    None => break,
                }
            }
            Ok(out)
        }
        ProviderId::OpenaiCodex => {
            let url = format!(
                "{base}/models?client_version={}",
                codex_oauth::CLIENT_VERSION
            );
            let body = get(client, &url, &headers).await?;
            Ok(models_array(&body)?
                .iter()
                .filter_map(codex_model)
                .collect())
        }
        ProviderId::Openai | ProviderId::Zai => {
            let body = get(client, &format!("{base}/models"), &headers).await?;
            Ok(models_array(&body)?
                .iter()
                .filter_map(|m| openai_model(m, true))
                .collect())
        }
        ProviderId::Openrouter | ProviderId::Deepseek => {
            let body = get(client, &format!("{base}/models"), &headers).await?;
            Ok(models_array(&body)?
                .iter()
                .filter_map(|m| openai_model(m, false))
                .collect())
        }
        ProviderId::Workbuddy => unreachable!("handled above"),
    }
}

/// Anthropic's `/v1/models` page: the models plus the cursor for the next page.
fn anthropic_page(body: &Value) -> Result<(Vec<ModelInfo>, Option<String>), ProviderError> {
    let data = body
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(no_list)?;
    let models = data
        .iter()
        .filter(|m| str_at(m, "/lifecycle").as_deref() != Some("retired"))
        .filter_map(anthropic_model)
        .collect();
    let next = match bool_at(body, "/has_more") {
        Some(true) => str_at(body, "/last_id"),
        _ => None,
    };
    Ok((models, next))
}

fn anthropic_model(m: &Value) -> Option<ModelInfo> {
    let id = str_at(m, "/id")?;
    let caps = m.get("capabilities");
    let effort = caps.and_then(|c| c.get("effort"));
    let efforts = effort
        .filter(|e| bool_at(e, "/supported") != Some(false))
        .map(|e| {
            EFFORTS
                .iter()
                .filter(|level| bool_at(e, &format!("/{level}/supported")) == Some(true))
                .map(|level| (*level).to_owned())
                .collect()
        })
        .unwrap_or_default();
    Some(ModelInfo {
        name: str_at(m, "/display_name").unwrap_or_else(|| id.clone()),
        description: str_at(m, "/description"),
        context_window: num_at(m, "/max_input_tokens"),
        max_output_tokens: num_at(m, "/max_tokens"),
        efforts,
        default_effort: None,
        cost_label: None,
        capabilities: Capabilities {
            // Anthropic's model list states no tool-calling capability.
            tool_calls: None,
            vision: caps.and_then(|c| bool_at(c, "/image_input/supported")),
        },
        id,
    })
}

/// OpenAI-compatible `/models` entry (OpenAI, Z.AI, DeepSeek, OpenRouter).
/// `chat_only` drops the non-chat ids those endpoints also list.
fn openai_model(m: &Value, chat_only: bool) -> Option<ModelInfo> {
    let id = str_at(m, "/id")?;
    if chat_only && NON_CHAT.iter().any(|p| id.to_ascii_lowercase().contains(p)) {
        return None;
    }
    // OpenRouter states output modalities: an image-only model is not a chat
    // model even though it is listed next to them.
    let outputs = strs_at(m, "/architecture/output_modalities");
    if !outputs.is_empty() && !outputs.iter().any(|o| o == "text") {
        return None;
    }
    let tools = m
        .get("supported_parameters")
        .and_then(Value::as_array)
        .map(|params| params.iter().any(|p| p.as_str() == Some("tools")));
    Some(ModelInfo {
        name: str_at(m, "/name").unwrap_or_else(|| id.clone()),
        description: str_at(m, "/description"),
        context_window: num_at(m, "/context_length")
            .or_else(|| num_at(m, "/context_window"))
            .or_else(|| num_at(m, "/top_provider/context_length")),
        max_output_tokens: num_at(m, "/max_output_tokens")
            .or_else(|| num_at(m, "/top_provider/max_completion_tokens")),
        efforts: strs_at(m, "/effort/supported_levels"),
        default_effort: str_at(m, "/effort/default_level"),
        cost_label: None,
        capabilities: Capabilities {
            tool_calls: tools,
            vision: input_vision(m),
        },
        id,
    })
}

/// Codex `/models` preset (`{ "models": [ { "slug": … } ] }`). Sign-in is
/// always ChatGPT, where the Codex CLI keeps presets with
/// `supported_in_api: false` (that flag gates API-key use only).
fn codex_model(m: &Value) -> Option<ModelInfo> {
    let id = str_at(m, "/slug")?;
    if matches!(str_at(m, "/visibility").as_deref(), Some("hide" | "hidden")) {
        return None;
    }
    let efforts = m
        .get("supported_reasoning_levels")
        .and_then(Value::as_array)
        .map(|levels| levels.iter().filter_map(|l| str_at(l, "/effort")).collect())
        .unwrap_or_default();
    Some(ModelInfo {
        name: str_at(m, "/display_name").unwrap_or_else(|| id.clone()),
        description: str_at(m, "/description"),
        context_window: num_at(m, "/context_window"),
        max_output_tokens: None,
        efforts,
        default_effort: str_at(m, "/default_reasoning_level"),
        cost_label: None,
        capabilities: Capabilities {
            // Every preset the Codex backend lists is callable with tools.
            tool_calls: None,
            vision: input_vision(m),
        },
        id,
    })
}

/// Vision from whichever modality list a provider uses, if it has one.
fn input_vision(m: &Value) -> Option<bool> {
    for path in [
        "/input_modalities",
        "/architecture/input_modalities",
        "/modalities/input",
    ] {
        let inputs = strs_at(m, path);
        if !inputs.is_empty() {
            return Some(inputs.iter().any(|i| i == "image"));
        }
    }
    None
}

async fn get(client: &Client, url: &str, headers: &HeaderMap) -> Result<Value, ProviderError> {
    let resp = client
        .get(url)
        .headers(headers.clone())
        .header(USER_AGENT, APP_USER_AGENT)
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(ProviderError::transport)?;
    let status = resp.status().as_u16();
    let body = resp.text().await.map_err(ProviderError::transport)?;
    if !(200..300).contains(&status) {
        return Err(crate::error_from_body(status, &body));
    }
    serde_json::from_str(&body).map_err(|e| ProviderError::Protocol(format!("model list: {e}")))
}

fn models_array(body: &Value) -> Result<&Vec<Value>, ProviderError> {
    body.get("data")
        .or_else(|| body.get("models"))
        .and_then(Value::as_array)
        .ok_or_else(no_list)
}

fn no_list() -> ProviderError {
    ProviderError::Protocol("the provider returned no model list".into())
}

/// Fetches the models.dev catalog, trimmed to the providers this app speaks
/// to. `Ok(None)` means the server confirmed the cached copy is current.
pub async fn fetch_catalog(
    client: &Client,
    etag: Option<&str>,
) -> Result<Option<(Catalog, Option<String>)>, ProviderError> {
    let mut req = client
        .get(METADATA_URL)
        .header(ACCEPT, HeaderValue::from_static("application/json"))
        .header(USER_AGENT, APP_USER_AGENT)
        .timeout(TIMEOUT);
    if let Some(etag) = etag {
        req = req.header("If-None-Match", etag);
    }
    let resp = req.send().await.map_err(ProviderError::transport)?;
    let status = resp.status().as_u16();
    if status == 304 {
        return Ok(None);
    }
    let next_etag = resp
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let body = resp.text().await.map_err(ProviderError::transport)?;
    if !(200..300).contains(&status) {
        return Err(crate::error_from_body(status, &body));
    }
    let body: Value = serde_json::from_str(&body)
        .map_err(|e| ProviderError::Protocol(format!("models.dev: {e}")))?;
    Ok(Some((catalog_from(&body), next_etag)))
}

/// Trims the models.dev payload to the keys [`ProviderId::catalog_key`]
/// returns, keeping only what a picker shows.
fn catalog_from(body: &Value) -> Catalog {
    let mut out = Catalog::new();
    for key in ["anthropic", "openai", "openrouter", "deepseek", "zai"] {
        let Some(models) = body
            .get(key)
            .and_then(|p| p.get("models"))
            .and_then(Value::as_object)
        else {
            continue;
        };
        let mut entries = BTreeMap::new();
        for (id, m) in models {
            let inputs = strs_at(m, "/modalities/input");
            let vision = if inputs.is_empty() {
                bool_at(m, "/attachment")
            } else {
                Some(inputs.iter().any(|i| i == "image"))
            };
            let efforts: Vec<String> = m
                .get("reasoning_options")
                .and_then(Value::as_array)
                .map(|options| {
                    options
                        .iter()
                        .filter(|o| str_at(o, "/type").as_deref() == Some("effort"))
                        .flat_map(|o| strs_at(o, "/values"))
                        .collect()
                })
                .unwrap_or_default();
            let entry = CatalogModel {
                name: str_at(m, "/name"),
                context_window: num_at(m, "/limit/context"),
                max_output_tokens: num_at(m, "/limit/output"),
                vision,
                tool_calls: bool_at(m, "/tool_call"),
                efforts,
            };
            if !entry.is_empty() {
                entries.insert(id.clone(), entry);
            }
        }
        out.insert(key.to_owned(), entries);
    }
    out
}

/// Fills the fields a provider's listing left unset from the public catalog.
/// Live values are never replaced.
pub fn enrich(models: &mut [ModelInfo], provider: ProviderId, catalog: &Catalog) {
    let Some(entries) = provider.catalog_key().and_then(|key| catalog.get(key)) else {
        return;
    };
    for model in models {
        let Some(meta) = entries.get(&model.id) else {
            continue;
        };
        model.context_window = model.context_window.or(meta.context_window);
        model.max_output_tokens = model.max_output_tokens.or(meta.max_output_tokens);
        model.capabilities.vision = model.capabilities.vision.or(meta.vision);
        model.capabilities.tool_calls = model.capabilities.tool_calls.or(meta.tool_calls);
        if model.efforts.is_empty() {
            model.efforts.clone_from(&meta.efforts);
        }
    }
}

fn str_at(v: &Value, path: &str) -> Option<String> {
    v.pointer(path)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn strs_at(v: &Value, path: &str) -> Vec<String> {
    v.pointer(path)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn num_at(v: &Value, path: &str) -> Option<u32> {
    v.pointer(path)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0)
}

fn bool_at(v: &Value, path: &str) -> Option<bool> {
    v.pointer(path).and_then(Value::as_bool)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn anthropic_page_reads_capabilities_limits_and_the_cursor() {
        let body = json!({
            "data": [
                {
                    "type": "model",
                    "id": "claude-sonnet-4-5",
                    "display_name": "Claude Sonnet 4.5",
                    "lifecycle": "active",
                    "max_input_tokens": 200000,
                    "max_tokens": 64000,
                    "capabilities": {
                        "image_input": { "supported": true },
                        "effort": {
                            "supported": true,
                            "low": { "supported": true },
                            "medium": { "supported": true },
                            "high": { "supported": true },
                            "xhigh": { "supported": false }
                        }
                    }
                },
                { "id": "claude-2.0", "display_name": "Claude 2", "lifecycle": "retired" }
            ],
            "has_more": true,
            "last_id": "claude-sonnet-4-5"
        });
        let (models, next) = anthropic_page(&body).unwrap();
        assert_eq!(next.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(models.len(), 1, "retired models are dropped");
        let m = &models[0];
        assert_eq!(m.id, "claude-sonnet-4-5");
        assert_eq!(m.name, "Claude Sonnet 4.5");
        assert_eq!(m.context_window, Some(200_000));
        assert_eq!(m.max_output_tokens, Some(64_000));
        assert_eq!(m.capabilities.vision, Some(true));
        assert_eq!(m.capabilities.tool_calls, None, "not stated, never guessed");
        assert_eq!(m.efforts, ["low", "medium", "high"]);
    }

    #[test]
    fn anthropic_without_has_more_has_no_cursor() {
        let (_, next) = anthropic_page(&json!({ "data": [] })).unwrap();
        assert!(next.is_none());
        assert!(anthropic_page(&json!({})).is_err());
    }

    #[test]
    fn openai_list_drops_non_chat_ids_and_keeps_ids() {
        let body = json!({ "object": "list", "data": [
            { "id": "gpt-5.5", "object": "model", "owned_by": "openai" },
            { "id": "text-embedding-3-small", "object": "model" },
            { "id": "tts-1", "object": "model" },
            { "id": "dall-e-3", "object": "model" },
            { "id": "gpt-4o-realtime-preview", "object": "model" }
        ]});
        let models: Vec<ModelInfo> = models_array(&body)
            .unwrap()
            .iter()
            .filter_map(|m| openai_model(m, true))
            .collect();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gpt-5.5");
        assert_eq!(models[0].name, "gpt-5.5");
        assert_eq!(models[0].context_window, None);
    }

    #[test]
    fn openrouter_entry_reads_context_modalities_and_tools() {
        let model = json!({
            "id": "anthropic/claude-sonnet-4.5",
            "name": "Anthropic: Claude Sonnet 4.5",
            "description": "Balanced model",
            "context_length": 1000000,
            "architecture": {
                "input_modalities": ["text", "image", "pdf"],
                "output_modalities": ["text"]
            },
            "top_provider": { "max_completion_tokens": 64000 },
            "supported_parameters": ["tools", "tool_choice", "temperature"]
        });
        let m = openai_model(&model, false).unwrap();
        assert_eq!(m.name, "Anthropic: Claude Sonnet 4.5");
        assert_eq!(m.context_window, Some(1_000_000));
        assert_eq!(m.max_output_tokens, Some(64_000));
        assert_eq!(m.capabilities.vision, Some(true));
        assert_eq!(m.capabilities.tool_calls, Some(true));
    }

    #[test]
    fn openrouter_image_model_is_not_a_chat_model() {
        let model = json!({
            "id": "google/imagen-4",
            "name": "Imagen 4",
            "architecture": { "output_modalities": ["image"] }
        });
        assert!(openai_model(&model, false).is_none());
    }

    #[test]
    fn deepseek_entry_reads_limits_modalities_and_efforts() {
        let model = json!({
            "id": "deepseek-v4-pro",
            "object": "model",
            "owned_by": "deepseek",
            "name": "DeepSeek V4 Pro",
            "context_window": 1048576,
            "max_output_tokens": 393216,
            "input_modalities": ["text", "image"],
            "effort": { "supported_levels": ["low", "high", "max"], "default_level": "high" }
        });
        let m = openai_model(&model, false).unwrap();
        assert_eq!(m.context_window, Some(1_048_576));
        assert_eq!(m.max_output_tokens, Some(393_216));
        assert_eq!(m.capabilities.vision, Some(true));
        assert_eq!(m.efforts, ["low", "high", "max"]);
        assert_eq!(m.default_effort.as_deref(), Some("high"));
    }

    #[test]
    fn codex_presets_read_slug_levels_and_visibility() {
        let body = json!({ "models": [
            {
                "slug": "gpt-6-astra",
                "display_name": "GPT-6-Astra",
                "description": "Frontier",
                "context_window": 272000,
                "input_modalities": ["text", "image"],
                "default_reasoning_level": "low",
                "supported_reasoning_levels": [
                    { "effort": "low" }, { "effort": "high" }
                ],
                "visibility": "list",
                "supported_in_api": true
            },
            { "slug": "hidden-one", "visibility": "hide", "supported_in_api": true },
            { "slug": "chatgpt-only", "visibility": "list", "supported_in_api": false }
        ]});
        let models: Vec<ModelInfo> = models_array(&body)
            .unwrap()
            .iter()
            .filter_map(codex_model)
            .collect();
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["gpt-6-astra", "chatgpt-only"]);
        assert_eq!(models[0].context_window, Some(272_000));
        assert_eq!(models[0].capabilities.vision, Some(true));
        assert_eq!(models[0].efforts, ["low", "high"]);
        assert_eq!(models[0].default_effort.as_deref(), Some("low"));
    }

    #[test]
    fn catalog_trims_to_known_providers_and_reads_metadata() {
        let body = json!({
            "anthropic": { "models": {
                "claude-sonnet-5-5": {
                    "id": "claude-sonnet-5-5",
                    "name": "Claude Sonnet 5.5",
                    "attachment": true,
                    "tool_call": true,
                    "reasoning_options": [
                        { "type": "toggle" },
                        { "type": "effort", "values": ["low", "medium", "high"] }
                    ],
                    "modalities": { "input": ["text", "image"], "output": ["text"] },
                    "limit": { "context": 1000000, "output": 128000 }
                }
            }},
            "openai": { "models": {
                "gpt-5.5": { "name": "GPT-5.5", "tool_call": true, "limit": { "context": 200000 } }
            }},
            "someone-else": { "models": { "x": { "name": "X" } } }
        });
        let catalog = catalog_from(&body);
        assert_eq!(catalog.len(), 2, "only the providers we speak to");
        let claude = &catalog["anthropic"]["claude-sonnet-5-5"];
        assert_eq!(claude.context_window, Some(1_000_000));
        assert_eq!(claude.max_output_tokens, Some(128_000));
        assert_eq!(claude.vision, Some(true));
        assert_eq!(claude.tool_calls, Some(true));
        assert_eq!(claude.efforts, ["low", "medium", "high"]);
    }

    #[test]
    fn enrich_fills_gaps_and_never_overwrites_live_values() {
        let mut models = vec![
            ModelInfo::named("gpt-5.5"),
            ModelInfo::named("gpt-unlisted"),
            ModelInfo {
                context_window: Some(123),
                capabilities: Capabilities {
                    vision: Some(false),
                    tool_calls: None,
                },
                ..ModelInfo::named("gpt-5.5-dated")
            },
        ];
        let body = json!({ "openai": { "models": {
            "gpt-5.5": {
                "name": "GPT-5.5",
                "tool_call": true,
                "limit": { "context": 200000, "output": 128000 },
                "modalities": { "input": ["text", "image"] }
            },
            "gpt-5.5-dated": {
                "tool_call": true,
                "limit": { "context": 999999 },
                "modalities": { "input": ["text", "image"] }
            }
        }}});
        enrich(&mut models, ProviderId::Openai, &catalog_from(&body));
        assert_eq!(models[0].context_window, Some(200_000));
        assert_eq!(models[0].max_output_tokens, Some(128_000));
        assert_eq!(models[0].capabilities.vision, Some(true));
        assert_eq!(models[0].capabilities.tool_calls, Some(true));
        assert_eq!(models[1].context_window, None, "not in the catalog");
        assert_eq!(models[2].context_window, Some(123), "live value wins");
        assert_eq!(
            models[2].capabilities.vision,
            Some(false),
            "live value wins"
        );
        assert_eq!(models[2].capabilities.tool_calls, Some(true), "gap filled");
    }

    #[test]
    fn catalog_fetch_is_skipped_for_providers_without_a_key() {
        assert_eq!(ProviderId::Workbuddy.catalog_key(), None);
        assert_eq!(ProviderId::ClaudeCode.catalog_key(), Some("anthropic"));
        assert_eq!(ProviderId::OpenaiCodex.catalog_key(), Some("openai"));
    }
}
