//! Model catalogs: what each provider serves right now.
//!
//! The list always comes from the provider's own listing endpoint
//! (`providers::models`), never from a table in this repo. Lists are cached
//! device-locally for [`LIST_TTL`] so opening Settings does not fire one
//! request per provider, and the public models.dev metadata
//! ([`CATALOG_TTL`]) fills the fields a listing leaves out — context window,
//! vision, effort levels — so role warnings and compaction see real numbers.
//! The metadata is a multi-megabyte download, so only the very first listing
//! waits for it; a stale copy is used at once and refreshed in the background.
//!
//! A cached list is scoped to the base URL it came from: a proxy's list is not
//! the real provider's, and switching overrides must not show the wrong models.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use providers::{models, Catalog, Client, ModelInfo, ProviderError, ProviderId};

use crate::config::{CachedCatalog, CachedModels, ModelCache};
use crate::{lock, Error, Harness};

/// How long a provider's list is reused before the picker refetches.
const LIST_TTL: u64 = 6 * 60 * 60;
/// How long the models.dev metadata is reused. It changes when models ship,
/// which is not hourly.
const CATALOG_TTL: u64 = 7 * 24 * 60 * 60;

impl Harness {
    /// The models `provider` serves, from its listing endpoint.
    ///
    /// `refresh` bypasses the cache. A failed fetch falls back to the cached
    /// list for the same base URL — a stale picker beats no picker — and only
    /// errors when nothing is cached.
    pub async fn models(
        &self,
        provider: ProviderId,
        refresh: bool,
    ) -> Result<Vec<ModelInfo>, Error> {
        let credential = self.current_credential(provider).await?;
        let base = provider.base_url(self.base_url(provider).as_deref());
        let now = unix_now();
        if !refresh {
            if let Some(list) = self.cached_list(provider, &base, now) {
                return Ok(list);
            }
        }
        let mut result = models::list(&self.client, provider, &credential, Some(&base)).await;
        // A subscription session that expired since sign-in is refreshed
        // once before the failure is reported.
        if matches!(result, Err(ProviderError::Unauthorized(_))) {
            if let Some(fresh) = self.refresh_subscription(&credential).await? {
                result = models::list(&self.client, provider, &fresh, Some(&base)).await;
            }
        }
        let mut listed = match result {
            Ok(listed) => listed,
            Err(e) => match self.stored_list(provider, &base) {
                Some(cached) => {
                    tracing::warn!(
                        provider = provider.spec().name,
                        error = %e,
                        "model listing failed; using the cached list"
                    );
                    return Ok(cached);
                }
                None => return Err(e.into()),
            },
        };
        self.enrich(&mut listed, provider, now).await;
        self.store_list(provider, &base, &listed, now)?;
        Ok(listed)
    }

    /// A refreshed credential for an expired subscription session, when one
    /// can be had without asking the learner to sign in again.
    async fn refresh_subscription(
        &self,
        credential: &providers::Credential,
    ) -> Result<Option<providers::Credential>, Error> {
        use providers::Credential;
        match credential {
            Credential::Workbuddy(s) if s.can_refresh(unix_now() as i64) => {
                Ok(Some(self.refresh_workbuddy(s).await?))
            }
            Credential::ClaudeCode(s) if s.can_refresh() => Ok(Some(self.refresh_claude(s).await?)),
            Credential::Codex(s) if s.can_refresh() => Ok(Some(self.refresh_codex(s).await?)),
            _ => Ok(None),
        }
    }

    /// Metadata for one model of one provider, from the cache only (no
    /// network: this runs inside turns). `None` means the cache does not know
    /// the model, which the UI shows as "unverified".
    pub fn model_info(&self, provider: ProviderId, id: &str) -> Option<ModelInfo> {
        let base = provider.base_url(self.base_url(provider).as_deref());
        lock(&self.model_cache)
            .providers
            .get(&provider)
            .filter(|cached| cached.base_url == base)
            .and_then(|cached| cached.models.iter().find(|m| m.id == id).cloned())
    }

    /// The model's own output limit, sent as the request's `max_tokens`. The
    /// models are never capped below it: a cap costs nothing when unused
    /// (billing and rate limits count generated tokens) and a small one cuts
    /// thinking-heavy replies off mid tool call. `None` (not in the cache)
    /// leaves the choice to the wire (`providers::ChatRequest::max_tokens`).
    pub fn max_tokens(&self, provider: ProviderId, id: &str) -> Option<u32> {
        self.model_info(provider, id)
            .and_then(|m| m.max_output_tokens)
    }

    /// A cached list younger than [`LIST_TTL`], for the current base URL.
    fn cached_list(&self, provider: ProviderId, base: &str, now: u64) -> Option<Vec<ModelInfo>> {
        lock(&self.model_cache)
            .providers
            .get(&provider)
            .filter(|cached| {
                cached.base_url == base && now.saturating_sub(cached.fetched_at) < LIST_TTL
            })
            .map(|cached| cached.models.clone())
    }

    /// A cached list of any age, for the current base URL.
    fn stored_list(&self, provider: ProviderId, base: &str) -> Option<Vec<ModelInfo>> {
        lock(&self.model_cache)
            .providers
            .get(&provider)
            .filter(|cached| cached.base_url == base)
            .map(|cached| cached.models.clone())
    }

    fn store_list(
        &self,
        provider: ProviderId,
        base: &str,
        models: &[ModelInfo],
        now: u64,
    ) -> Result<(), Error> {
        let dir = self.config_dir.clone();
        let mut cache = lock(&self.model_cache);
        cache.providers.insert(
            provider,
            CachedModels {
                base_url: base.to_owned(),
                fetched_at: now,
                models: models.to_vec(),
            },
        );
        cache.save(&dir)
    }

    /// Fills the fields the listing left unset from models.dev. Best effort:
    /// without the catalog the list is still complete, only less annotated.
    async fn enrich(&self, listed: &mut [ModelInfo], provider: ProviderId, now: u64) {
        let Some(catalog) = self.catalog(provider, now).await else {
            return;
        };
        models::enrich(listed, provider, &catalog);
    }

    /// The trimmed models.dev catalog. A copy of any age is used at once; a
    /// stale one starts a background refresh. Only when there is no copy at
    /// all does the caller wait, and concurrent callers share that one
    /// download. `None` when the provider has no public catalog or no copy
    /// could be had.
    async fn catalog(&self, provider: ProviderId, now: u64) -> Option<Catalog> {
        provider.catalog_key()?;
        if let Some((providers, fetched_at)) = self.cached_catalog() {
            if now.saturating_sub(fetched_at) >= CATALOG_TTL {
                self.refresh_catalog_in_background();
            }
            return Some(providers);
        }
        let _fetching = self.catalog_fetch.lock().await;
        // Another caller may have finished the download while this one waited.
        if let Some((providers, _)) = self.cached_catalog() {
            return Some(providers);
        }
        fetch_catalog_into(&self.client, &self.model_cache, &self.config_dir, now).await
    }

    fn cached_catalog(&self) -> Option<(Catalog, u64)> {
        lock(&self.model_cache)
            .catalog
            .as_ref()
            .map(|c| (c.providers.clone(), c.fetched_at))
    }

    /// Refreshes stale metadata off the request path. Skipped while a fetch
    /// is already running, and outside a Tokio runtime.
    fn refresh_catalog_in_background(&self) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let Ok(fetching) = Arc::clone(&self.catalog_fetch).try_lock_owned() else {
            return;
        };
        let client = self.client.clone();
        let cache = Arc::clone(&self.model_cache);
        let dir = self.config_dir.clone();
        runtime.spawn(async move {
            let _fetching = fetching;
            fetch_catalog_into(&client, &cache, &dir, unix_now()).await;
        });
    }
}

/// Fetches models.dev into `cache` (conditionally, with the cached ETag) and
/// fills the stored lists' unset fields from a new copy, so models listed
/// under an older copy gain its metadata too. Returns the catalog in effect.
async fn fetch_catalog_into(
    client: &Client,
    cache: &Mutex<ModelCache>,
    dir: &Path,
    now: u64,
) -> Option<Catalog> {
    let etag = lock(cache).catalog.as_ref().and_then(|c| c.etag.clone());
    let fetched = models::fetch_catalog(client, etag.as_deref()).await;
    let mut cache = lock(cache);
    match fetched {
        Ok(Some((catalog, etag))) => {
            for (provider, cached) in cache.providers.iter_mut() {
                models::enrich(&mut cached.models, *provider, &catalog);
            }
            cache.catalog = Some(CachedCatalog {
                fetched_at: now,
                etag,
                providers: catalog,
            });
        }
        // 304: the cached copy is current; restart its clock.
        Ok(None) => {
            if let Some(cached) = cache.catalog.as_mut() {
                cached.fetched_at = now;
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "model metadata unavailable");
            return cache.catalog.as_ref().map(|c| c.providers.clone());
        }
    }
    if let Err(e) = cache.save(dir) {
        tracing::warn!(error = %e, "could not save the model metadata cache");
    }
    cache.catalog.as_ref().map(|c| c.providers.clone())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_lists_are_reused_and_stale_ones_are_only_a_fallback() {
        let dir = std::env::temp_dir().join(format!("lh-models-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&dir).unwrap();
        let base = ProviderId::Openai.base_url(None);
        let proxy = "http://127.0.0.1:8080/v1".to_owned();
        // A provider we never fetched from has nothing cached.
        let harness = Harness::open(&dir.join("repo"), &dir).unwrap();
        assert!(harness
            .cached_list(ProviderId::Openai, &base, 1_000)
            .is_none());
        assert!(harness.model_info(ProviderId::Openai, "gpt-5").is_none());

        harness
            .store_list(
                ProviderId::Openai,
                &base,
                &[ModelInfo::named("gpt-5")],
                1_000,
            )
            .unwrap();
        assert!(harness
            .cached_list(ProviderId::Openai, &base, 1_000 + LIST_TTL - 1)
            .is_some());
        assert!(harness
            .cached_list(ProviderId::Openai, &base, 1_000 + LIST_TTL)
            .is_none());
        // Stale, but the base URL still matches: good enough for a fallback.
        assert!(harness.stored_list(ProviderId::Openai, &base).is_some());
        // Another base URL is another provider's list.
        assert!(harness.stored_list(ProviderId::Openai, &proxy).is_none());
        assert_eq!(
            harness
                .model_info(ProviderId::Openai, "gpt-5")
                .map(|m| m.id),
            Some("gpt-5".to_owned())
        );
        // A second harness reads the file back.
        drop(harness);
        let reopened = Harness::open(&dir.join("repo"), &dir).unwrap();
        assert!(reopened
            .stored_list(ProviderId::Openai, &base)
            .is_some_and(|models| models.len() == 1));
        let _ = std::fs::remove_dir_all(dir);
    }
}
