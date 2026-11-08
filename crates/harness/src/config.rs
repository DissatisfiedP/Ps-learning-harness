//! Device-local state, kept outside the synced data repo (architecture §4.18:
//! keys and tokens never enter the repo).
//!
//! - `settings.json`: per-role model choices, per-provider base URL overrides.
//! - `credentials.json`: API keys and WorkBuddy sessions, mode 0600.
//! - `models-cache.json`: the model list each provider last returned, plus the
//!   public metadata that fills the fields a listing leaves out.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use providers::{Credential, ModelInfo, ProviderId};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::Error;

/// The five model roles from architecture §4.22. The harness maps agents to
/// roles; only Tutor is required, every other unset role falls back to Tutor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelRole {
    Tutor,
    Planner,
    Reader,
    Librarian,
    Builder,
}

impl ModelRole {
    pub const ALL: [ModelRole; 5] = [
        ModelRole::Tutor,
        ModelRole::Planner,
        ModelRole::Reader,
        ModelRole::Librarian,
        ModelRole::Builder,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ModelRole::Tutor => "tutor",
            ModelRole::Planner => "planner",
            ModelRole::Reader => "reader",
            ModelRole::Librarian => "librarian",
            ModelRole::Builder => "builder",
        }
    }

    /// Settings label, exactly as architecture §4.22.
    pub fn label(self) -> &'static str {
        match self {
            ModelRole::Tutor => "Tutor",
            ModelRole::Planner => "Planner",
            ModelRole::Reader => "Reader",
            ModelRole::Librarian => "Librarian",
            ModelRole::Builder => "Builder",
        }
    }

    /// The weight tag: heavy / medium / light.
    pub fn weight(self) -> &'static str {
        match self {
            ModelRole::Tutor | ModelRole::Planner => "heavy",
            ModelRole::Reader => "light",
            ModelRole::Librarian | ModelRole::Builder => "medium",
        }
    }

    /// The short "what it does" tag shown after the weight.
    pub fn role_phrase(self) -> &'static str {
        match self {
            ModelRole::Tutor => "talks with you",
            ModelRole::Planner => "thinks rarely, deeply",
            ModelRole::Reader => "searches and reads",
            ModelRole::Librarian => "keeps your knowledge graph",
            ModelRole::Builder => "code and visuals",
        }
    }

    /// Guidance shown to the learner, from architecture §4.22.
    pub fn guidance(self) -> &'static str {
        match self {
            ModelRole::Tutor => "Your best model. Learning quality depends on it most.",
            ModelRole::Planner => "Best reasoning model, high effort. Called rarely.",
            ModelRole::Reader => "Fast, cheap, long context. Vision reads scans.",
            ModelRole::Librarian => "Not the cheapest: its mistakes stay in your graph.",
            ModelRole::Builder => "Good coding model. Figures need vision.",
        }
    }

    /// Minimum recommended context window, in tokens (None = no strict min).
    pub fn min_context(self) -> Option<u32> {
        match self {
            ModelRole::Tutor => Some(128_000),
            ModelRole::Reader => Some(200_000),
            _ => None,
        }
    }

    /// Roles where vision is useful (warning only when explicitly missing).
    pub fn vision_relevant(self) -> bool {
        matches!(self, ModelRole::Reader | ModelRole::Builder)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelChoice {
    pub provider: ProviderId,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

/// One optional model choice per role (architecture §4.22).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Roles {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tutor: Option<ModelChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planner: Option<ModelChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reader: Option<ModelChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub librarian: Option<ModelChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builder: Option<ModelChoice>,
}

impl Roles {
    pub fn get(&self, role: ModelRole) -> Option<&ModelChoice> {
        match role {
            ModelRole::Tutor => self.tutor.as_ref(),
            ModelRole::Planner => self.planner.as_ref(),
            ModelRole::Reader => self.reader.as_ref(),
            ModelRole::Librarian => self.librarian.as_ref(),
            ModelRole::Builder => self.builder.as_ref(),
        }
    }

    pub fn set(&mut self, role: ModelRole, choice: Option<ModelChoice>) {
        match role {
            ModelRole::Tutor => self.tutor = choice,
            ModelRole::Planner => self.planner = choice,
            ModelRole::Reader => self.reader = choice,
            ModelRole::Librarian => self.librarian = choice,
            ModelRole::Builder => self.builder = choice,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub roles: Roles,
    #[serde(default)]
    pub base_urls: BTreeMap<ProviderId, String>,
    /// Legacy single model. Read once for migration, then dropped on save.
    /// Not a compatibility shim: it is removed the first time settings are
    /// written after load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelChoice>,
    /// Plugin registries the learner added (W8). The built-in default
    /// (`install::DEFAULT_REGISTRY_URL`) is not stored here; see
    /// `hide_default_registry`.
    #[serde(default)]
    pub registries: Vec<crate::plugins::install::Registry>,
    /// The learner removed the built-in default registry. Adding its URL
    /// again clears this.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hide_default_registry: bool,
    /// Development plugins added by folder.
    #[serde(default)]
    pub plugin_folders: Vec<crate::plugins::install::FolderPlugin>,
    /// Installed plugins the learner turned off.
    #[serde(default)]
    pub disabled_plugins: Vec<String>,
    /// How a new session measures the learner (docs/learning-system.md §3):
    /// `measure` (the full probe), `quick` (at most five questions) or `new`
    /// (no probe questions). Remembered as the learner's default; the tutor
    /// may still set a different one per goal.
    #[serde(default = "default_starting_point")]
    pub starting_point: String,
}

/// The default starting point: the full probe, which is the most useful one
/// for planning (§3).
pub fn default_starting_point() -> String {
    "measure".to_owned()
}

impl Settings {
    /// Migrate the old single `model` field into `roles.tutor`, then clear it
    /// so the next save drops it from the file entirely.
    fn migrate(&mut self) {
        if self.roles.tutor.is_none() {
            if let Some(old) = self.model.take() {
                self.roles.tutor = Some(old);
            }
        }
        // Always clear: even if tutor was already set, never keep the legacy
        // field around.
        self.model = None;
        // A missing or garbled starting point falls back to the default
        // (a fresh settings file is `Default`, which is empty).
        if !matches!(self.starting_point.as_str(), "measure" | "quick" | "new") {
            self.starting_point = default_starting_point();
        }
    }
}

pub struct Config {
    dir: PathBuf,
    pub settings: Settings,
    pub credentials: BTreeMap<ProviderId, Credential>,
}

impl Config {
    pub fn load(dir: &Path) -> Result<Self, Error> {
        fs::create_dir_all(dir)?;
        let mut settings: Settings = read_json(&dir.join("settings.json"))?;
        settings.migrate();
        Ok(Self {
            dir: dir.to_owned(),
            settings,
            credentials: read_json(&dir.join("credentials.json"))?,
        })
    }

    pub fn save_settings(&self) -> Result<(), Error> {
        write_json(&self.dir.join("settings.json"), &self.settings, false)
    }

    pub fn save_credentials(&self) -> Result<(), Error> {
        write_json(&self.dir.join("credentials.json"), &self.credentials, true)
    }

    /// Resolve a role to a usable model choice. An unset role falls back to
    /// the Tutor model; if Tutor itself is unset, this is the single required
    /// setting that blocks all agents.
    pub fn resolve(&self, role: ModelRole) -> Result<ModelChoice, Error> {
        if let Some(choice) = self.settings.roles.get(role) {
            return Ok(choice.clone());
        }
        self.settings
            .roles
            .tutor
            .clone()
            .ok_or_else(|| Error::Invalid("choose a Tutor model in Settings".into()))
    }
}

/// The model list one provider last returned, and the base URL it came from.
/// A proxy's list is not the provider's own, so the base URL is part of the
/// entry: switching overrides must not show the other server's models.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedModels {
    pub base_url: String,
    /// Unix seconds when the fetch completed.
    pub fetched_at: u64,
    pub models: Vec<ModelInfo>,
}

/// The trimmed models.dev metadata, with the validator needed to revalidate
/// without downloading the full catalog again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedCatalog {
    pub fetched_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    pub providers: providers::Catalog,
}

/// `models-cache.json`. Device-local and rebuildable: a model list belongs to
/// an account and a base URL, so it is never synced.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCache {
    #[serde(default)]
    pub providers: BTreeMap<ProviderId, CachedModels>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<CachedCatalog>,
}

impl ModelCache {
    pub fn load(dir: &Path) -> Result<Self, Error> {
        read_json(&dir.join("models-cache.json"))
    }

    pub fn save(&self, dir: &Path) -> Result<(), Error> {
        write_json(&dir.join("models-cache.json"), self, false)
    }
}

pub(crate) fn read_json<T: DeserializeOwned + Default>(path: &Path) -> Result<T, Error> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| Error::Corrupt(format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e.into()),
    }
}

/// Write-then-rename so a crash never leaves a half-written file.
pub(crate) fn write_json<T: Serialize>(path: &Path, value: &T, private: bool) -> Result<(), Error> {
    let tmp = path.with_extension("json.tmp");
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = private;
    let mut file = opts.open(&tmp)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_moves_legacy_model_into_tutor_and_drops_it() {
        let mut s = Settings {
            model: Some(ModelChoice {
                provider: ProviderId::Anthropic,
                model: "claude".into(),
                effort: None,
            }),
            ..Default::default()
        };
        s.migrate();
        assert_eq!(s.roles.tutor.as_ref().unwrap().model, "claude");
        assert!(s.model.is_none());
        let json: serde_json::Value = serde_json::to_value(&s).unwrap();
        assert!(
            json.get("model").is_none(),
            "legacy top-level model field must not serialize: {json}"
        );
    }

    #[test]
    fn migrate_keeps_existing_tutor_and_clears_legacy() {
        let mut s = Settings {
            roles: Roles {
                tutor: Some(ModelChoice {
                    provider: ProviderId::Openai,
                    model: "gpt".into(),
                    effort: None,
                }),
                ..Default::default()
            },
            model: Some(ModelChoice {
                provider: ProviderId::Zai,
                model: "old".into(),
                effort: None,
            }),
            ..Default::default()
        };
        s.migrate();
        assert_eq!(s.roles.tutor.as_ref().unwrap().model, "gpt");
        assert!(s.model.is_none());
    }

    #[test]
    fn the_starting_point_defaults_to_measure_and_serializes_camel_case() {
        let mut fresh = Settings::default();
        fresh.migrate();
        assert_eq!(fresh.starting_point, "measure");
        let json = serde_json::to_value(&fresh).unwrap();
        assert_eq!(json["startingPoint"], "measure", "{json}");

        // A stored value survives a load; a garbled one is repaired.
        let mut stored: Settings =
            serde_json::from_value(serde_json::json!({ "startingPoint": "quick" })).unwrap();
        stored.migrate();
        assert_eq!(stored.starting_point, "quick");
        let mut garbled: Settings =
            serde_json::from_value(serde_json::json!({ "startingPoint": "sometimes" })).unwrap();
        garbled.migrate();
        assert_eq!(garbled.starting_point, "measure");
        let mut missing: Settings = serde_json::from_value(serde_json::json!({})).unwrap();
        missing.migrate();
        assert_eq!(missing.starting_point, "measure");
    }

    #[test]
    fn resolve_falls_back_to_tutor_and_errors_without_it() {
        let dir = std::env::temp_dir().join(format!("lh-cfg-{}", ulid::Ulid::new()));
        let mut cfg = Config::load(&dir).unwrap();
        // No roles: error names the Tutor setting.
        assert!(cfg
            .resolve(ModelRole::Reader)
            .unwrap_err()
            .to_string()
            .contains("choose a Tutor model"));
        cfg.settings.roles.tutor = Some(ModelChoice {
            provider: ProviderId::Anthropic,
            model: "t".into(),
            effort: None,
        });
        assert_eq!(cfg.resolve(ModelRole::Librarian).unwrap().model, "t");
        cfg.settings.roles.planner = Some(ModelChoice {
            provider: ProviderId::Openai,
            model: "p".into(),
            effort: None,
        });
        assert_eq!(cfg.resolve(ModelRole::Planner).unwrap().model, "p");
        let _ = std::fs::remove_dir_all(dir);
    }
}
