//! Device-local state, kept outside the synced data repo (architecture §4.18:
//! keys and tokens never enter the repo).
//!
//! - `settings.json`: chosen model and per-provider base URL overrides.
//! - `credentials.json`: API keys and WorkBuddy sessions, mode 0600.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use providers::{Credential, ProviderId};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelChoice {
    pub provider: ProviderId,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub model: Option<ModelChoice>,
    #[serde(default)]
    pub base_urls: BTreeMap<ProviderId, String>,
}

pub struct Config {
    dir: PathBuf,
    pub settings: Settings,
    pub credentials: BTreeMap<ProviderId, Credential>,
}

impl Config {
    pub fn load(dir: &Path) -> Result<Self, Error> {
        fs::create_dir_all(dir)?;
        Ok(Self {
            dir: dir.to_owned(),
            settings: read_json(&dir.join("settings.json"))?,
            credentials: read_json(&dir.join("credentials.json"))?,
        })
    }

    pub fn save_settings(&self) -> Result<(), Error> {
        write_json(&self.dir.join("settings.json"), &self.settings, false)
    }

    pub fn save_credentials(&self) -> Result<(), Error> {
        write_json(&self.dir.join("credentials.json"), &self.credentials, true)
    }
}

fn read_json<T: DeserializeOwned + Default>(path: &Path) -> Result<T, Error> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| Error::Corrupt(format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e.into()),
    }
}

/// Write-then-rename so a crash never leaves a half-written file.
fn write_json<T: Serialize>(path: &Path, value: &T, private: bool) -> Result<(), Error> {
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
