//! Loading installed/folder plugins at startup and the install, registry and
//! search operations behind the Settings and plan-chip commands.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, MutexGuard};

use serde_json::Value;

use super::install::{
    self, FolderPlugin, IndexPlugin, IndexVersion, LockEntry, LockSource, Registry, RegistryStatus,
};
use super::{LuauPlugin, Plugin, PluginInfo, PluginRegistry, PluginSource};
use crate::config::ModelRole;
use crate::replay::replay_state;
use crate::{lock, Error, Harness};

impl Harness {
    /// This device's platform (`desktop` | `mobile`), which plugins are
    /// offered and whether the mobile session rules apply. The build target
    /// decides it; tests set `mobile` to run the phone rules on a desktop
    /// build (docs/sync-build.md, "Sessions on mobile").
    pub fn set_platform(&self, platform: &str) -> Result<(), Error> {
        if !matches!(platform, "desktop" | "mobile") {
            return Err(Error::Invalid(format!(
                "unknown platform `{platform}`; use desktop or mobile"
            )));
        }
        lock(&self.plugins).set_platform(platform);
        Ok(())
    }

    /// Whether this device is a phone or tablet.
    pub(crate) fn is_mobile(&self) -> bool {
        lock(&self.plugins).platform() == "mobile"
    }

    /// Rebuild the registry from installed packages, folder plugins and the
    /// learner's enable/disable choices. A broken package is skipped with a
    /// warning, never crashing the app.
    pub fn reload_plugins(&self) -> Result<(), Error> {
        let (folders, disabled) = {
            let config = lock(&self.config);
            (
                config.settings.plugin_folders.clone(),
                config.settings.disabled_plugins.clone(),
            )
        };
        // Keep the platform the registry already has (the build target's,
        // or the one a test set).
        let platform = lock(&self.plugins).platform().to_owned();
        let mut registry = PluginRegistry::new(&platform);
        let lock_entries = install::read_lock(&self.repo).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "plugins.lock unreadable; loading without it");
            Vec::new()
        });
        let source_for = |id: &str| -> Option<PluginSource> {
            lock_entries
                .iter()
                .find(|e| e.id == id)
                .map(|e| match &e.source {
                    LockSource::Registry { registry } => PluginSource::Registry {
                        url: registry.clone(),
                    },
                    LockSource::Folder { folder } => PluginSource::Folder {
                        name: folder.clone(),
                    },
                })
        };

        let root = self.config_dir.join("plugins");
        if let Ok(ids) = std::fs::read_dir(&root) {
            for id_dir in ids.flatten().filter(|e| e.path().is_dir()) {
                // Several installed versions may sit side by side; load the
                // one in the lock (else the last copy by name).
                let id = id_dir.file_name().to_string_lossy().into_owned();
                let locked = lock_entries
                    .iter()
                    .find(|e| e.id == id)
                    .map(|e| id_dir.path().join(e.sha256.to_ascii_lowercase()))
                    .filter(|d| d.is_dir());
                let mut versions: Vec<PathBuf> = std::fs::read_dir(id_dir.path())
                    .map(|d| {
                        d.flatten()
                            .map(|e| e.path())
                            .filter(|p| p.is_dir())
                            .collect()
                    })
                    .unwrap_or_default();
                versions.sort();
                let Some(dir) = locked.or_else(|| versions.pop()) else {
                    continue;
                };
                match LuauPlugin::open(&dir) {
                    Ok(p) => {
                        let hash = super::package_hash(&dir);
                        let id = p.manifest().id.clone();
                        let source = source_for(&id)
                            .unwrap_or(PluginSource::Registry { url: String::new() });
                        registry.register(Arc::new(p), source, hash);
                    }
                    Err(e) => tracing::warn!(path = %dir.display(), error = %e, "skipping plugin"),
                }
            }
        }
        // Folder plugins win over an installed copy with the same id.
        for folder in folders {
            let dir = PathBuf::from(&folder.path);
            match LuauPlugin::open(&dir) {
                Ok(p) => registry.register_folder(p, folder.id.clone(), dir),
                Err(e) => tracing::warn!(path = %folder.path, error = %e, "skipping folder plugin"),
            }
        }
        for id in disabled {
            registry.disable(&id);
        }
        *lock(&self.plugins) = registry;
        Ok(())
    }

    /// The plugin registry, with folder plugins reloaded first if their files
    /// changed. Every path that hands a plugin out (catalog, tools, views,
    /// calls, grading) goes through here.
    pub(crate) fn plugin_registry(&self) -> MutexGuard<'_, PluginRegistry> {
        let mut registry = lock(&self.plugins);
        registry.refresh_folders();
        registry
    }

    /// An installed plugin that is also enabled (views, bridge calls).
    pub(crate) fn enabled_plugin(&self, id: &str) -> Result<Arc<dyn Plugin>, Error> {
        let registry = self.plugin_registry();
        let plugin = registry
            .by_id(id)
            .ok_or_else(|| Error::NotFound(format!("plugin {id}")))?;
        if !registry.is_enabled(id) {
            return Err(Error::Invalid(format!(
                "plugin `{id}` is disabled; enable it in Settings → Plugins"
            )));
        }
        Ok(plugin)
    }

    /// An unfinished session whose current section has plugin `id` active
    /// (its title, for the refusal).
    fn session_using(&self, id: &str) -> Option<String> {
        let sessions = self.store.list().ok()?;
        sessions.into_iter().filter(|s| !s.read_only).find_map(|s| {
            let state = replay_state(&self.store.ops(&s.id).ok()?);
            (!state.ended
                && state.phase == crate::state::Phase::Teach
                && state.active_plugins.iter().any(|p| p == id))
            .then_some(s.title)
        })
    }

    /// Updates never apply mid-section to a running session (W8): a session
    /// keeps the version it was granted until its next section starts.
    fn check_not_in_use(&self, id: &str) -> Result<(), Error> {
        if !lock(&self.plugins).is_installed(id) {
            return Ok(());
        }
        if let Some(title) = self.session_using(id) {
            return Err(Error::Invalid(format!(
                "`{id}` is in use in the current section of the session “{title}”; update it after that section ends"
            )));
        }
        Ok(())
    }

    pub fn plugins(&self) -> Vec<PluginInfo> {
        self.plugin_registry().info()
    }

    pub fn set_plugin_enabled(&self, id: &str, enabled: bool) -> Result<(), Error> {
        {
            let mut config = lock(&self.config);
            config.settings.disabled_plugins.retain(|p| p != id);
            if !enabled {
                config.settings.disabled_plugins.push(id.to_owned());
            }
            config.save_settings()?;
        }
        let mut registry = lock(&self.plugins);
        if enabled {
            registry.enable(id);
        } else {
            registry.disable(id);
        }
        Ok(())
    }

    /// Install a package into `plugins/<id>/<sha256>/`, validate it and add
    /// the lock entry. Consent is the caller's job (the UI shows it first).
    /// Refused while a running tutor turn has the plugin active.
    pub async fn install_plugin(
        &self,
        registry_url: &str,
        id: &str,
        version: &str,
    ) -> Result<PluginInfo, Error> {
        install::check_id(id)?;
        self.check_not_in_use(id)?;
        let lists = |index: &install::RegistryIndex| {
            index
                .plugins
                .iter()
                .any(|p| p.id == id && p.versions.iter().any(|v| v.version == version))
        };
        // A cached index may predate the version asked for: fetch once more.
        let mut index = self.load_index(registry_url).await?;
        if !lists(&index) {
            index = self.fetch_index_cached(registry_url).await?;
        }
        let plugin = index
            .plugins
            .iter()
            .find(|p| p.id == id)
            .ok_or_else(|| Error::Invalid(format!("`{id}` is not in that registry")))?;
        let ver = plugin
            .versions
            .iter()
            .find(|v| v.version == version)
            .ok_or_else(|| Error::Invalid(format!("`{id}` has no version {version}")))?;
        let bytes = install::download(&self.client, &ver.url, &ver.sha256).await?;
        let dir = install::install_dir(&self.config_dir, id, &ver.sha256.to_ascii_lowercase())?;
        // Unpack, open and hash off the async runtime (disk and CPU work).
        let (package, hash) = {
            let dir = dir.clone();
            let (id, version) = (id.to_owned(), version.to_owned());
            tokio::task::spawn_blocking(move || -> Result<(LuauPlugin, String), Error> {
                if let Some(parent) = dir.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                // A package that fails to unpack or validate leaves nothing
                // behind (a stray directory would be loaded on restart).
                let opened = install::unpack(&bytes, &dir).and_then(|()| {
                    let package = LuauPlugin::open(&dir).map_err(Error::Invalid)?;
                    let manifest = package.manifest();
                    if manifest.id != id || manifest.version != version {
                        return Err(Error::Invalid(format!(
                            "the archive contains {} {}, not {id} {version}",
                            manifest.id, manifest.version
                        )));
                    }
                    Ok(package)
                });
                match opened {
                    Ok(package) => {
                        let hash = super::package_hash(&dir);
                        Ok((package, hash))
                    }
                    Err(e) => {
                        let _ = std::fs::remove_dir_all(&dir);
                        if let Some(parent) = dir.parent() {
                            // Only succeeds when no other version is installed.
                            let _ = std::fs::remove_dir(parent);
                        }
                        Err(e)
                    }
                }
            })
            .await
            .map_err(|e| Error::Invalid(format!("install failed ({e})")))??
        };
        // The download may have taken a while; check again before swapping.
        if let Err(e) = self.check_not_in_use(id) {
            let locked = install::read_lock(&self.repo)?
                .iter()
                .any(|l| l.id == id && l.sha256.eq_ignore_ascii_case(&ver.sha256));
            if !locked {
                let _ = std::fs::remove_dir_all(&dir);
            }
            return Err(e);
        }
        let mut registry = lock(&self.plugins);
        registry.register(
            Arc::new(package),
            PluginSource::Registry {
                url: registry_url.to_owned(),
            },
            hash,
        );
        let mut entries = install::read_lock(&self.repo)?;
        entries.retain(|e| e.id != id);
        entries.push(LockEntry {
            id: id.to_owned(),
            version: version.to_owned(),
            sha256: ver.sha256.clone(),
            source: LockSource::Registry {
                registry: registry_url.to_owned(),
            },
        });
        install::write_lock(&self.repo, &entries)?;
        drop(registry);
        Ok(lock(&self.plugins)
            .info()
            .into_iter()
            .find(|i| i.id == id)
            .expect("just registered"))
    }

    /// Add a development plugin by folder and remember it in settings.
    pub fn add_folder_plugin(&self, path: &str) -> Result<PluginInfo, Error> {
        let dir = PathBuf::from(path)
            .canonicalize()
            .map_err(|e| Error::Invalid(format!("can't open that folder ({e})")))?;
        let package = LuauPlugin::open(&dir).map_err(Error::Invalid)?;
        let manifest = package.manifest().clone();
        let hash = super::package_hash(&dir);
        let name = manifest.id.clone();
        {
            let mut config = lock(&self.config);
            config
                .settings
                .plugin_folders
                .retain(|f| f.id != manifest.id);
            config.settings.plugin_folders.push(FolderPlugin {
                id: manifest.id.clone(),
                path: dir.to_string_lossy().into_owned(),
            });
            config.save_settings()?;
        }
        lock(&self.plugins).register_folder(package, name.clone(), dir.clone());
        let mut entries = install::read_lock(&self.repo)?;
        entries.retain(|e| e.id != manifest.id);
        entries.push(LockEntry {
            id: manifest.id.clone(),
            version: manifest.version.clone(),
            sha256: hash,
            source: LockSource::Folder { folder: name },
        });
        install::write_lock(&self.repo, &entries)?;
        Ok(lock(&self.plugins)
            .info()
            .into_iter()
            .find(|i| i.id == manifest.id)
            .expect("just registered"))
    }

    pub fn remove_plugin(&self, id: &str) -> Result<(), Error> {
        install::check_id(id)?;
        {
            let mut config = lock(&self.config);
            config.settings.plugin_folders.retain(|f| f.id != id);
            config.settings.disabled_plugins.retain(|p| p != id);
            config.save_settings()?;
        }
        install::remove_installed(&self.config_dir, id)?;
        lock(&self.plugins).remove(id);
        let mut entries = install::read_lock(&self.repo)?;
        entries.retain(|e| e.id != id);
        install::write_lock(&self.repo, &entries)?;
        Ok(())
    }

    /// The data repo's `plugins.lock` entries.
    pub fn plugin_lock(&self) -> Result<Vec<LockEntry>, Error> {
        install::read_lock(&self.repo)
    }

    /// Locked plugins whose locked copy is missing on this device (the UI
    /// offers the consent screen again). A registry entry counts as missing
    /// when its `<id>/<sha256>` package is gone, even if another version of
    /// the id loaded instead.
    pub fn missing_locked(&self) -> Result<Vec<LockEntry>, Error> {
        let registry = lock(&self.plugins);
        Ok(install::read_lock(&self.repo)?
            .into_iter()
            .filter(|e| match &e.source {
                LockSource::Registry { .. } => {
                    !install::install_dir(&self.config_dir, &e.id, &e.sha256.to_ascii_lowercase())
                        .is_ok_and(|d| d.is_dir())
                }
                LockSource::Folder { .. } => !registry.is_installed(&e.id),
            })
            .collect())
    }

    pub fn add_registry(&self, name: &str, url: &str) -> Result<(), Error> {
        let url = url.trim();
        if url.is_empty()
            || !(url.starts_with("https://") || url.starts_with("file://") || url.starts_with('/'))
        {
            return Err(Error::Invalid(
                "a registry URL must be https://, file:// or an absolute path".into(),
            ));
        }
        let mut config = lock(&self.config);
        if url == install::DEFAULT_REGISTRY_URL {
            // The built-in default comes back; it is never stored as a row.
            config.settings.hide_default_registry = false;
            config.settings.registries.retain(|r| r.url != url);
            return config.save_settings();
        }
        config.settings.registries.retain(|r| r.url != url);
        config.settings.registries.push(Registry {
            name: name.trim().to_owned(),
            url: url.to_owned(),
            builtin: false,
        });
        config.save_settings()
    }

    pub fn remove_registry(&self, url: &str) -> Result<(), Error> {
        let mut config = lock(&self.config);
        if url == install::DEFAULT_REGISTRY_URL {
            config.settings.hide_default_registry = true;
        }
        config.settings.registries.retain(|r| r.url != url);
        config.save_settings()
    }

    /// The built-in default (first, unless the learner removed it), then the
    /// learner's own registries.
    pub fn registries(&self) -> Vec<Registry> {
        let config = lock(&self.config);
        let settings = &config.settings;
        let default = (!settings.hide_default_registry).then(Registry::default_registry);
        default
            .into_iter()
            .chain(settings.registries.iter().cloned())
            .collect()
    }

    /// The cached index for `url`, fetched (and cached) when there is none.
    async fn load_index(&self, url: &str) -> Result<install::RegistryIndex, Error> {
        let cache = install::cache_path(&self.config_dir, url);
        if let Ok(text) = std::fs::read_to_string(&cache) {
            if let Ok(index) = install::parse_index(&text) {
                return Ok(index);
            }
        }
        self.fetch_index_cached(url).await
    }

    /// Fetch the index for `url` now and replace its cached copy.
    async fn fetch_index_cached(&self, url: &str) -> Result<install::RegistryIndex, Error> {
        let cache = install::cache_path(&self.config_dir, url);
        let text = install::fetch_index(&self.client, url).await?;
        let index = install::parse_index(&text)?;
        if let Some(dir) = cache.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let _ = std::fs::write(&cache, &text);
        Ok(index)
    }

    /// Fetch every configured registry and cache its index (W8 Settings).
    pub async fn refresh_registries(&self) -> Vec<RegistryStatus> {
        let mut out = Vec::new();
        for registry in self.registries() {
            out.push(self.refresh_registry(registry).await);
        }
        out
    }

    /// At startup: fetch the registries whose cached index is missing or
    /// older than `max_age`, so the Settings browser and `plugin_search`
    /// (which read only the cache) see the default registry without the
    /// learner pressing Refresh. Failures are logged; the cache stays as it was.
    pub async fn refresh_stale_registries(&self, max_age: std::time::Duration) {
        for registry in self.registries() {
            let cache = install::cache_path(&self.config_dir, &registry.url);
            let fresh = std::fs::metadata(&cache)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age < max_age);
            if fresh {
                continue;
            }
            let status = self.refresh_registry(registry).await;
            if let Some(error) = status.error {
                tracing::warn!(registry = %status.url, %error, "registry refresh failed");
            }
        }
    }

    async fn refresh_registry(&self, registry: Registry) -> RegistryStatus {
        let fetched = match install::fetch_index(&self.client, &registry.url).await {
            Ok(text) => install::parse_index(&text).map(|index| (text, index)),
            Err(e) => Err(e),
        };
        let (ok, error, plugins) = match fetched {
            Ok((text, index)) => {
                let cache = install::cache_path(&self.config_dir, &registry.url);
                if let Some(dir) = cache.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(&cache, &text);
                (true, None, index.plugins.len())
            }
            Err(e) => (false, Some(e.to_string()), 0),
        };
        RegistryStatus {
            name: registry.name,
            url: registry.url,
            ok,
            error,
            fetched_at: Some(crate::now_iso()),
            plugins,
        }
    }

    /// Cached index contents, for the Settings browser and the consent screen.
    pub fn registry_catalog(&self) -> Vec<RegistryCatalog> {
        self.registries()
            .into_iter()
            .map(|r| {
                let cache = install::cache_path(&self.config_dir, &r.url);
                let index = std::fs::read_to_string(&cache)
                    .ok()
                    .and_then(|t| install::parse_index(&t).ok());
                RegistryCatalog {
                    name: r.name,
                    url: r.url,
                    fetched: index.is_some(),
                    plugins: index.map(|i| i.plugins).unwrap_or_default(),
                }
            })
            .collect()
    }

    /// `plugin_search` for the planner (W9): up to five uninstalled matches
    /// from cached indexes, with permissions and platforms. Every whitespace-
    /// separated term must appear somewhere in the searchable text, and only
    /// entries that list this device's platform are offered.
    pub fn plugin_search(&self, query: &str) -> String {
        let terms: Vec<String> = query.split_whitespace().map(|t| t.to_lowercase()).collect();
        if terms.is_empty() {
            return "plugin_search needs a query".into();
        }
        let installed: HashSet<String> = lock(&self.plugins)
            .info()
            .into_iter()
            .map(|p| p.id)
            .collect();
        let platform = self.plugin_registry().platform().to_owned();
        let mut seen = HashSet::new();
        let mut hits: Vec<Value> = Vec::new();
        for catalog in self.registry_catalog() {
            for p in catalog.plugins {
                if installed.contains(&p.id) || !seen.insert(p.id.clone()) {
                    continue;
                }
                let haystack = format!("{} {} {} {}", p.id, p.title, p.description, p.use_when);
                if !query_matches(&terms, &haystack, &p.platforms, &platform) {
                    continue;
                }
                let version = p.versions.iter().max_by(|a, b| {
                    semver::Version::parse(&a.version)
                        .ok()
                        .cmp(&semver::Version::parse(&b.version).ok())
                });
                hits.push(serde_json::json!({
                    "id": p.id,
                    "title": p.title,
                    "description": p.description,
                    "useWhen": p.use_when,
                    "avoidWhen": p.avoid_when,
                    "platforms": p.platforms,
                    "permissions": p.permissions,
                    "version": version.map(|v| v.version.clone()),
                    "registry": catalog.url,
                }));
                if hits.len() >= 5 {
                    break;
                }
            }
            if hits.len() >= 5 {
                break;
            }
        }
        if hits.is_empty() {
            return format!("no plugin matches `{query}` in the configured registries");
        }
        serde_json::to_string_pretty(&Value::Array(hits)).unwrap_or_default()
    }

    /// Role vision capability: `None` means the catalog doesn't say.
    pub fn role_vision(&self, role: ModelRole) -> Option<bool> {
        let choice = lock(&self.config).resolve(role).ok()?;
        self.model_info(choice.provider, &choice.model)
            .and_then(|m| m.capabilities.vision)
    }
}

/// True when every search term appears in `haystack` (case-insensitive) and
/// `platforms` lists `platform`. Pure, so the matching rule is unit-testable.
fn query_matches(terms: &[String], haystack: &str, platforms: &[String], platform: &str) -> bool {
    if !platforms.iter().any(|p| p == platform) {
        return false;
    }
    let haystack = haystack.to_lowercase();
    terms.iter().all(|t| haystack.contains(t))
}

#[cfg(test)]
mod search_tests {
    use super::query_matches;

    fn terms(s: &str) -> Vec<String> {
        s.split_whitespace().map(|t| t.to_lowercase()).collect()
    }
    fn platforms(p: &[&str]) -> Vec<String> {
        p.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn every_term_must_appear_in_the_text() {
        let hay = "circuit Draw and solve DC and AC circuits use_when need a schematic";
        assert!(query_matches(
            &terms("draw circuits"),
            hay,
            &platforms(&["desktop"]),
            "desktop"
        ));
        assert!(!query_matches(
            &terms("draw diagram"),
            hay,
            &platforms(&["desktop"]),
            "desktop"
        ));
        // Case-insensitive, and a single term still works.
        assert!(query_matches(
            &terms("CIRCUIT"),
            hay,
            &platforms(&["desktop"]),
            "desktop"
        ));
    }

    #[test]
    fn wrong_platform_is_never_offered() {
        let hay = "circuit draw circuits";
        assert!(!query_matches(
            &terms("circuit"),
            hay,
            &platforms(&["mobile"]),
            "desktop"
        ));
        assert!(query_matches(
            &terms("circuit"),
            hay,
            &platforms(&["desktop", "mobile"]),
            "desktop"
        ));
        // An empty platforms list is not installable anywhere.
        assert!(!query_matches(&terms("circuit"), hay, &[], "desktop"));
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryCatalog {
    pub name: String,
    pub url: String,
    pub fetched: bool,
    pub plugins: Vec<IndexPlugin>,
}

/// One registry's details for a consent screen.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallCandidate {
    pub registry: String,
    pub plugin: IndexPlugin,
    pub versions: Vec<IndexVersion>,
}

/// Look up the consent-screen details for a plugin in cached indexes.
pub fn install_candidate(harness: &Harness, id: &str) -> Option<InstallCandidate> {
    for catalog in harness.registry_catalog() {
        if let Some(plugin) = catalog.plugins.into_iter().find(|p| p.id == id) {
            return Some(InstallCandidate {
                registry: catalog.url,
                versions: plugin.versions.clone(),
                plugin,
            });
        }
    }
    None
}

#[cfg(test)]
mod registry_tests {
    use crate::plugins::install::{Registry, DEFAULT_REGISTRY_URL};
    use crate::Harness;

    #[test]
    fn the_default_registry_is_listed_until_removed_and_returns_when_re_added() {
        let dir = std::env::temp_dir().join(format!("lh-registries-{}", ulid::Ulid::new()));
        let open = || Harness::open(&dir.join("repo"), &dir.join("config")).unwrap();
        let urls = |h: &Harness| {
            h.registries()
                .into_iter()
                .map(|r| r.url)
                .collect::<Vec<_>>()
        };
        let settings = || std::fs::read_to_string(dir.join("config/settings.json")).unwrap();

        let h = open();
        assert_eq!(h.registries(), vec![Registry::default_registry()]);
        h.add_registry("Mine", "https://example.org/index.json")
            .unwrap();
        assert_eq!(
            urls(&h),
            [DEFAULT_REGISTRY_URL, "https://example.org/index.json"]
        );
        // Only the learner's own registry is stored.
        assert!(!settings().contains(DEFAULT_REGISTRY_URL), "{}", settings());

        h.remove_registry(DEFAULT_REGISTRY_URL).unwrap();
        drop(h);
        let h = open();
        assert_eq!(
            urls(&h),
            ["https://example.org/index.json"],
            "removal survives a restart"
        );

        h.add_registry("Again", DEFAULT_REGISTRY_URL).unwrap();
        let listed = h.registries();
        assert_eq!(listed[0], Registry::default_registry());
        assert_eq!(
            listed.len(),
            2,
            "re-adding restores the default, not a second row"
        );
        assert!(!settings().contains(DEFAULT_REGISTRY_URL), "{}", settings());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
