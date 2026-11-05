//! Plugin scaffolding (architecture §4.20). No real plugins are built: only
//! the trait, the registry, platform filtering and the loader limits. Tests
//! register a test-only `echo` plugin to prove the planner → tutor → call path.
//!
//! A plugin's manifest comes from `prompts/plugins/<id>.md` frontmatter when
//! that file exists (registration matches by `id`), else from the
//! implementation itself; the fragment the tutor gets is that file's body.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use providers::ToolSpec;

use crate::prompts::PluginManifest;

/// Most tools one plugin may add (prompts/README.md "Plugins").
pub const MAX_TOOLS_PER_PLUGIN: usize = 3;
/// Fragments are "about 200 words at most"; anything past this is refused.
pub const MAX_FRAGMENT_WORDS: usize = 250;

/// What a plugin tool returns to the agent loop.
#[derive(Debug, Clone)]
pub struct ToolOutcome {
    pub content: String,
    pub is_error: bool,
}

/// Context passed to a plugin tool call. Owned, so the returned future is
/// `'static` and no lock is held while it runs.
#[derive(Debug, Clone)]
pub struct PluginContext {
    pub session_id: String,
    pub platform: String,
}

pub type ToolFuture = Pin<Box<dyn Future<Output = ToolOutcome> + Send>>;

/// A plugin implementation. `call` returns a boxed future so the trait stays
/// object-safe without `async_trait`.
pub trait Plugin: Send + Sync {
    fn manifest(&self) -> &PluginManifest;
    fn tools(&self) -> Vec<ToolSpec>;
    /// Fragment used when no `prompts/plugins/<id>.md` exists.
    fn fragment(&self) -> String {
        String::new()
    }
    fn call(&self, name: &str, args: &serde_json::Value, ctx: PluginContext) -> ToolFuture;
}

struct Entry {
    plugin: Arc<dyn Plugin>,
    manifest: PluginManifest,
    fragment: String,
}

pub struct PluginRegistry {
    entries: Vec<Entry>,
    platform: String,
}

impl PluginRegistry {
    pub fn new(platform: &str) -> Self {
        Self {
            entries: Vec::new(),
            platform: platform.to_owned(),
        }
    }

    /// Register an implementation. `prompt` is the parsed
    /// `prompts/plugins/<id>.md` (manifest, body) when that file exists.
    pub fn register(
        &mut self,
        plugin: Arc<dyn Plugin>,
        prompt: Option<(PluginManifest, String)>,
    ) -> Result<(), String> {
        let (manifest, fragment) = match prompt {
            Some((m, body)) => (m, body),
            None => (plugin.manifest().clone(), plugin.fragment()),
        };
        let tools = plugin.tools();
        if tools.len() > MAX_TOOLS_PER_PLUGIN {
            return Err(format!(
                "plugin `{}` has {} tools (at most {MAX_TOOLS_PER_PLUGIN})",
                manifest.id,
                tools.len()
            ));
        }
        let words = fragment.split_whitespace().count();
        if words > MAX_FRAGMENT_WORDS {
            return Err(format!(
                "plugin `{}` fragment has {words} words (about 200 at most)",
                manifest.id
            ));
        }
        self.entries.retain(|e| e.manifest.id != manifest.id);
        self.entries.push(Entry {
            plugin,
            manifest,
            fragment,
        });
        Ok(())
    }

    /// The catalog: registered plugins available on this platform.
    pub fn available(&self) -> Vec<PluginManifest> {
        self.entries
            .iter()
            .filter(|e| self.on_platform(&e.manifest))
            .map(|e| e.manifest.clone())
            .collect()
    }

    /// Whether `id` is registered and available on this platform.
    pub fn grantable(&self, id: &str) -> bool {
        self.entries
            .iter()
            .any(|e| e.manifest.id == id && self.on_platform(&e.manifest))
    }

    /// Tool specs of the given active plugins.
    pub fn tool_specs(&self, ids: &[String]) -> Vec<ToolSpec> {
        self.active(ids).flat_map(|e| e.plugin.tools()).collect()
    }

    /// Fragments of the given active plugins, joined.
    pub fn fragments(&self, ids: &[String]) -> String {
        self.active(ids)
            .map(|e| e.fragment.trim())
            .filter(|f| !f.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// The active plugin that owns tool `name`, if any.
    pub fn owner(&self, ids: &[String], name: &str) -> Option<Arc<dyn Plugin>> {
        self.active(ids)
            .find(|e| e.plugin.tools().iter().any(|t| t.name == name))
            .map(|e| Arc::clone(&e.plugin))
    }

    pub fn platform(&self) -> &str {
        &self.platform
    }

    fn active<'a>(&'a self, ids: &'a [String]) -> impl Iterator<Item = &'a Entry> + 'a {
        self.entries
            .iter()
            .filter(move |e| ids.contains(&e.manifest.id) && self.on_platform(&e.manifest))
    }

    fn on_platform(&self, m: &PluginManifest) -> bool {
        m.platforms.iter().any(|p| p == &self.platform)
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new(if cfg!(any(target_os = "android", target_os = "ios")) {
            "mobile"
        } else {
            "desktop"
        })
    }
}
