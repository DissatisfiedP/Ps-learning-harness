//! Plugins as packages (build prompt W5, D5/D6): tools, prompt fragments,
//! renderers and interactive views, written in sandboxed Luau. Native Rust
//! plugins keep the same manifest shape for future system-level tools.
//!
//! One catalog path for the planner, one dispatch path for the tutor and
//! practice agents. See `docs/plugins.md` for the authoring specification.

pub mod host;
pub mod install;
pub mod load;
mod luau;
pub mod manifest;

use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use providers::ToolSpec;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use manifest::{PluginManifest, MAX_FRAGMENT_WORDS, MAX_TOOLS_PER_PLUGIN};
pub use manifest::{ViewDecl, API_VERSION};

/// What a plugin tool returns to the agent loop.
#[derive(Debug, Clone)]
pub struct ToolOutcome {
    pub content: String,
    pub is_error: bool,
    pub artifact: Option<ArtifactPayload>,
}

/// The figure a tool returns; `agent.rs` stores it as an `artifact` op.
#[derive(Debug, Clone)]
pub struct ArtifactPayload {
    pub kind: String,
    pub svg: String,
    pub data: Value,
}

/// Context passed to a plugin tool call (owned, so the future is `'static`).
#[derive(Debug, Clone)]
pub struct PluginContext {
    pub session_id: String,
    pub platform: String,
}

pub type ToolFuture = Pin<Box<dyn Future<Output = ToolOutcome> + Send>>;

/// A plugin implementation. `call` takes `Arc<Self>` so a Luau call can run on
/// a blocking thread without holding the dispatch path.
pub trait Plugin: Send + Sync {
    fn manifest(&self) -> &PluginManifest;
    fn tools(&self) -> Vec<ToolSpec>;
    /// The fragment for one agent, empty when the package has none.
    fn fragment(&self, _agent: &str) -> String {
        String::new()
    }
    fn view_html(&self) -> Result<String, String> {
        Err("this plugin has no view".into())
    }
    fn view_call(&self, name: &str, _args: &Value) -> Result<Value, String> {
        Err(format!("`{name}` is not a callable view function"))
    }
    fn grade(&self, _answer: &Value, _target: &Value) -> Result<Value, String> {
        Err("this plugin cannot grade".into())
    }
    fn call(self: Arc<Self>, name: &str, args: &Value, ctx: PluginContext) -> ToolFuture;
}

/// Where an installed plugin came from (recorded in `plugins.lock`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PluginSource {
    Registry { url: String },
    Folder { name: String },
}

impl PluginSource {
    pub fn label(&self) -> String {
        match self {
            PluginSource::Registry { url } => url.clone(),
            PluginSource::Folder { name } => format!("folder {name}"),
        }
    }
}

/// One installed plugin as the UI sees it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInfo {
    pub id: String,
    pub title: String,
    pub description: String,
    pub version: String,
    pub api: u32,
    pub platforms: Vec<String>,
    pub permissions: Vec<String>,
    pub use_when: String,
    pub avoid_when: String,
    pub cost: String,
    pub enabled: bool,
    pub source: String,
    pub package_hash: String,
    pub has_view: bool,
    pub tools: Vec<String>,
    /// Its manifest lists this device's platform. When it doesn't, its tools
    /// aren't offered here and the UI shows its figures as the fallback.
    pub on_platform: bool,
    /// Folder plugins: why the latest edit on disk couldn't be loaded (the
    /// last good version stays in use). Absent when it loaded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load_error: Option<String>,
}

/// A Luau package on disk.
pub struct LuauPlugin {
    manifest: PluginManifest,
    dir: PathBuf,
    runtime: Arc<luau::LuauRuntime>,
    tool_specs: Vec<ToolSpec>,
}

impl LuauPlugin {
    pub fn open(dir: &Path) -> Result<Self, String> {
        let manifest = manifest::read(dir)?;
        let mut tool_specs = Vec::with_capacity(manifest.tools.len());
        for tool in &manifest.tools {
            let text = std::fs::read_to_string(dir.join(&tool.input))
                .map_err(|e| format!("tool `{}` schema ({e})", tool.name))?;
            let input_schema: Value = serde_json::from_str(&text)
                .map_err(|e| format!("tool `{}` schema is not JSON ({e})", tool.name))?;
            if !input_schema.is_object() {
                return Err(format!("tool `{}` schema must be an object", tool.name));
            }
            tool_specs.push(ToolSpec {
                name: tool.name.clone(),
                description: tool.description.clone(),
                input_schema,
            });
        }
        Ok(Self {
            manifest,
            dir: dir.to_owned(),
            runtime: Arc::new(luau::LuauRuntime::new(dir)),
            tool_specs,
        })
    }

    pub fn fragment(&self, agent: &str) -> String {
        self.manifest
            .prompts
            .get(agent)
            .and_then(|p| std::fs::read_to_string(self.dir.join(p)).ok())
            .unwrap_or_default()
    }

    pub fn render(&self, data: &Value) -> Result<String, String> {
        let Some(renderer) = self.manifest.renderer.first() else {
            return Err("this plugin has no renderer".into());
        };
        self.runtime.render(&renderer.entry, data)
    }

    pub fn view_html(&self) -> Result<String, String> {
        let Some(view) = self.manifest.view.first() else {
            return Err("this plugin has no view".into());
        };
        let html = std::fs::read_to_string(self.dir.join(&view.html))
            .map_err(|e| format!("can't read the view ({e})"))?;
        Ok(inline_assets(&html, &self.dir))
    }

    pub fn view_call(&self, name: &str, args: &Value) -> Result<Value, String> {
        let Some(view) = self.manifest.view.first() else {
            return Err("this plugin has no view".into());
        };
        if !view.calls.iter().any(|c| c == name) {
            return Err(format!(
                "`{name}` is not one of the view's callable functions"
            ));
        }
        self.runtime.view_call(name, args)
    }

    pub fn grade(&self, answer: &Value, target: &Value) -> Result<Value, String> {
        self.runtime.grade(answer, target)
    }
}

impl Plugin for LuauPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn tools(&self) -> Vec<ToolSpec> {
        self.tool_specs.clone()
    }

    fn fragment(&self, agent: &str) -> String {
        LuauPlugin::fragment(self, agent)
    }

    fn view_html(&self) -> Result<String, String> {
        LuauPlugin::view_html(self)
    }

    fn view_call(&self, name: &str, args: &Value) -> Result<Value, String> {
        LuauPlugin::view_call(self, name, args)
    }

    fn grade(&self, answer: &Value, target: &Value) -> Result<Value, String> {
        LuauPlugin::grade(self, answer, target)
    }

    fn call(self: Arc<Self>, name: &str, args: &Value, _ctx: PluginContext) -> ToolFuture {
        let Some(decl) = self.manifest.tools.iter().find(|t| t.name == name).cloned() else {
            let missing = name.to_owned();
            return Box::pin(async move {
                ToolOutcome {
                    content: format!("unknown plugin tool `{missing}`"),
                    is_error: true,
                    artifact: None,
                }
            });
        };
        let runtime = Arc::clone(&self.runtime);
        let args = args.clone();
        Box::pin(async move {
            let entry = decl.entry.clone();
            let artifact = decl.artifact;
            match tokio::task::spawn_blocking(move || runtime.call_tool(&entry, &args, artifact))
                .await
            {
                Ok(out) => out,
                Err(e) => ToolOutcome {
                    content: format!("plugin call failed ({e})"),
                    is_error: true,
                    artifact: None,
                },
            }
        })
    }
}

/// Inline `<link rel=stylesheet>` and `<script src>` assets from the package
/// so the view is one self-contained document served under a
/// `default-src 'none'` CSP. Only files inside the package are inlined; any
/// other reference is left as it is (the CSP blocks it).
fn inline_assets(html: &str, dir: &Path) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(pos) = rest.find(['<']) {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        if tail.starts_with("<link") {
            if let Some(end) = tail.find('>') {
                let tag = &tail[..=end];
                if let Some(href) = attr(tag, "href") {
                    if let Some(css) = package_text(dir, href) {
                        out.push_str("<style>");
                        out.push_str(&css);
                        out.push_str("</style>");
                        rest = &tail[end + 1..];
                        continue;
                    }
                }
            }
        } else if tail.starts_with("<script") && attr(tail, "src").is_some() {
            if let Some(end) = tail.find("</script>") {
                let tag = &tail[..end];
                if let Some(src) = attr(tag, "src") {
                    if let Some(js) = package_text(dir, src) {
                        out.push_str("<script>");
                        out.push_str(&js);
                        out.push_str("</script>");
                        let after = end + "</script>".len();
                        rest = &tail[after..];
                        continue;
                    }
                }
            }
        }
        out.push('<');
        rest = &tail[1..];
    }
    out.push_str(rest);
    out
}

/// The text of `rel` inside the package `dir`, or `None` when it is missing
/// or resolves (through `..`, an absolute path or a symlink) outside it.
fn package_text(dir: &Path, rel: &str) -> Option<String> {
    let root = dir.canonicalize().ok()?;
    let path = root.join(rel).canonicalize().ok()?;
    if !path.starts_with(&root) {
        tracing::warn!(asset = rel, "view asset outside the package; not inlined");
        return None;
    }
    std::fs::read_to_string(path).ok()
}

fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!("{name}=");
    let start = tag.find(&needle)? + needle.len();
    let rest = &tag[start..];
    let quote = rest.chars().next()?;
    let rest = if quote == '"' || quote == '\'' {
        &rest[1..]
    } else {
        rest
    };
    let end = rest.find(|c: char| c == quote || c.is_whitespace() || c == '>')?;
    Some(&rest[..end])
}

struct Entry {
    plugin: Arc<dyn Plugin>,
    manifest: PluginManifest,
    package_hash: String,
    source: PluginSource,
    /// Folder plugins: reloaded when their files change (W8).
    watch: Option<Watch>,
}

/// A folder plugin's directory, the fingerprint of its files when it was
/// last opened, and why the latest edit couldn't be loaded.
struct Watch {
    dir: PathBuf,
    fingerprint: Fingerprint,
    error: Option<String>,
}

/// Cheap change detector for a package: newest mtime, total size and file
/// count over every file outside `tests/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fingerprint {
    newest: u128,
    bytes: u64,
    files: usize,
}

fn fingerprint(dir: &Path) -> Fingerprint {
    let mut files = Vec::new();
    collect(dir, dir, &mut files);
    let mut out = Fingerprint {
        newest: 0,
        bytes: 0,
        files: 0,
    };
    for (rel, path) in files {
        if rel.starts_with("tests/") {
            continue;
        }
        let Ok(meta) = std::fs::metadata(path) else {
            continue;
        };
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos());
        out.newest = out.newest.max(modified);
        out.bytes += meta.len();
        out.files += 1;
    }
    out
}

/// Catalog and dispatch for every plugin known on this device.
pub struct PluginRegistry {
    entries: Vec<Entry>,
    /// The learner's disabled ids; kept across (re)registration so an update
    /// never re-enables a plugin the learner turned off.
    disabled: HashSet<String>,
    platform: String,
}

impl PluginRegistry {
    pub fn new(platform: &str) -> Self {
        Self {
            entries: Vec::new(),
            disabled: HashSet::new(),
            platform: platform.to_owned(),
        }
    }

    pub fn platform(&self) -> &str {
        &self.platform
    }

    /// This device's platform (`desktop` | `mobile`). Startup decides it from
    /// the build target; tests change it to run the mobile rules.
    pub fn set_platform(&mut self, platform: &str) {
        platform.clone_into(&mut self.platform);
    }

    pub fn register(
        &mut self,
        plugin: Arc<dyn Plugin>,
        source: PluginSource,
        package_hash: String,
    ) {
        let manifest = plugin.manifest().clone();
        let id = manifest.id.clone();
        self.entries.retain(|e| e.manifest.id != id);
        self.entries.push(Entry {
            plugin,
            manifest,
            package_hash,
            source,
            watch: None,
        });
    }

    /// Register a folder plugin opened from `dir`; it is reloaded from there
    /// whenever its files change ([`PluginRegistry::refresh_folders`]).
    pub fn register_folder(&mut self, plugin: LuauPlugin, name: String, dir: PathBuf) {
        let hash = package_hash(&dir);
        self.register(Arc::new(plugin), PluginSource::Folder { name }, hash);
        if let Some(entry) = self.entries.last_mut() {
            entry.watch = Some(Watch {
                fingerprint: fingerprint(&dir),
                dir,
                error: None,
            });
        }
    }

    /// Reload every folder plugin whose files changed since it was opened. An
    /// edit that doesn't load keeps the last good version and records the
    /// error (shown as `loadError` in [`PluginRegistry::info`]).
    pub fn refresh_folders(&mut self) {
        for e in &mut self.entries {
            let Some(watch) = e.watch.as_mut() else {
                continue;
            };
            let now = fingerprint(&watch.dir);
            if now == watch.fingerprint {
                continue;
            }
            watch.fingerprint = now;
            match LuauPlugin::open(&watch.dir) {
                Ok(p) if p.manifest().id == e.manifest.id => {
                    e.manifest = p.manifest().clone();
                    e.package_hash = package_hash(&watch.dir);
                    e.plugin = Arc::new(p);
                    watch.error = None;
                }
                Ok(p) => {
                    watch.error = Some(format!(
                        "the folder's plugin id changed from `{}` to `{}`; add the folder again",
                        e.manifest.id,
                        p.manifest().id
                    ));
                }
                Err(err) => {
                    tracing::warn!(path = %watch.dir.display(), error = %err, "folder plugin edit not loaded");
                    watch.error = Some(err);
                }
            }
        }
    }

    pub fn disable(&mut self, id: &str) {
        self.disabled.insert(id.to_owned());
    }

    pub fn enable(&mut self, id: &str) {
        self.disabled.remove(id);
    }

    pub fn remove(&mut self, id: &str) {
        self.entries.retain(|e| e.manifest.id != id);
        self.disabled.remove(id);
    }

    pub fn is_installed(&self, id: &str) -> bool {
        self.entries.iter().any(|e| e.manifest.id == id)
    }

    pub fn is_enabled(&self, id: &str) -> bool {
        self.is_installed(id) && !self.disabled.contains(id)
    }

    pub fn info(&self) -> Vec<PluginInfo> {
        self.entries
            .iter()
            .map(|e| {
                let m = &e.manifest;
                PluginInfo {
                    id: m.id.clone(),
                    title: m.title.clone(),
                    description: m.description.clone(),
                    version: m.version.clone(),
                    api: m.api,
                    platforms: m.platforms.clone(),
                    permissions: m.permissions.clone(),
                    use_when: m.use_when.clone(),
                    avoid_when: m.avoid_when.clone(),
                    cost: m.cost.clone(),
                    enabled: !self.disabled.contains(&m.id),
                    source: e.source.label(),
                    package_hash: e.package_hash.clone(),
                    has_view: !m.view.is_empty(),
                    tools: m.tool_names(),
                    on_platform: self.on_platform(m),
                    load_error: e.watch.as_ref().and_then(|w| w.error.clone()),
                }
            })
            .collect()
    }

    /// The planner's catalog: installed, enabled plugins available here.
    pub fn available(&self) -> Vec<PluginManifest> {
        self.entries
            .iter()
            .filter(|e| !self.disabled.contains(&e.manifest.id) && self.on_platform(&e.manifest))
            .map(|e| e.manifest.clone())
            .collect()
    }

    /// Whether `id` is installed, enabled and available on this platform.
    pub fn grantable(&self, id: &str) -> bool {
        self.entries.iter().any(|e| {
            e.manifest.id == id
                && !self.disabled.contains(&e.manifest.id)
                && self.on_platform(&e.manifest)
        })
    }

    /// Tool specs of the active plugins that this agent may receive.
    pub fn tool_specs(&self, ids: &[String], agent: &str) -> Vec<ToolSpec> {
        self.active(ids)
            .flat_map(|e| {
                let m = &e.manifest;
                e.plugin.tools().into_iter().filter(move |spec| {
                    m.tools
                        .iter()
                        .find(|t| t.name == spec.name)
                        .is_some_and(|t| t.agents.iter().any(|a| a == agent))
                })
            })
            .collect()
    }

    /// Prompt fragments for the active plugins, one per plugin, in plan order.
    pub fn fragments(&self, ids: &[String], agent: &str) -> String {
        self.active(ids)
            .map(|e| e.plugin.fragment(agent))
            .map(|f| f.trim().to_owned())
            .filter(|f| !f.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub fn owner(&self, ids: &[String], name: &str) -> Option<Arc<dyn Plugin>> {
        self.active(ids)
            .find(|e| e.plugin.tools().iter().any(|t| t.name == name))
            .map(|e| Arc::clone(&e.plugin))
    }

    /// The manifest of an installed plugin (enabled or not, any platform).
    pub fn manifest(&self, id: &str) -> Option<&PluginManifest> {
        self.entries
            .iter()
            .find(|e| e.manifest.id == id)
            .map(|e| &e.manifest)
    }

    /// The installed plugin (enabled or not, any platform) that declares
    /// tool `name`.
    pub fn tool_owner(&self, name: &str) -> Option<&PluginManifest> {
        self.entries
            .iter()
            .map(|e| &e.manifest)
            .find(|m| m.tools.iter().any(|t| t.name == name))
    }

    /// Whether installed plugin `id` lists this device's platform.
    pub fn is_on_platform(&self, id: &str) -> bool {
        self.manifest(id).is_some_and(|m| self.on_platform(m))
    }

    /// The plugin that produced an artifact, if it is still installed.
    pub fn by_id(&self, id: &str) -> Option<Arc<dyn Plugin>> {
        self.entries
            .iter()
            .find(|e| e.manifest.id == id)
            .map(|e| Arc::clone(&e.plugin))
    }

    pub fn package_hash(&self, id: &str) -> Option<String> {
        self.entries
            .iter()
            .find(|e| e.manifest.id == id)
            .map(|e| e.package_hash.clone())
    }

    fn active<'a>(&'a self, ids: &'a [String]) -> impl Iterator<Item = &'a Entry> + 'a {
        self.entries.iter().filter(move |e| {
            ids.contains(&e.manifest.id)
                && !self.disabled.contains(&e.manifest.id)
                && self.on_platform(&e.manifest)
        })
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

/// Content hash of a package directory (sorted relative paths + bytes). Used
/// for `artifact.packageHash` and folder/installed identity.
pub fn package_hash(dir: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut files = Vec::new();
    collect(dir, dir, &mut files);
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = Sha256::new();
    for (rel, path) in files {
        hasher.update(rel.as_bytes());
        hasher.update([0u8]);
        if let Ok(bytes) = std::fs::read(path) {
            hasher.update((bytes.len() as u64).to_le_bytes());
            hasher.update(bytes);
        }
    }
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn collect(base: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            collect(base, &path, out);
        } else {
            let rel = path
                .strip_prefix(base)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            out.push((rel, path));
        }
    }
}

pub use luau::{analyze_package, test_package};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_package_files_are_inlined_into_a_view() {
        let root = std::env::temp_dir().join(format!("lh-view-{}", ulid::Ulid::new()));
        let pkg = root.join("pkg");
        std::fs::create_dir_all(pkg.join("views")).unwrap();
        std::fs::write(pkg.join("views/app.js"), "INSIDE_JS").unwrap();
        std::fs::write(pkg.join("views/app.css"), "INSIDE_CSS").unwrap();
        std::fs::write(root.join("outside.js"), "HOST_SECRET").unwrap();
        std::os::unix::fs::symlink(root.join("outside.js"), pkg.join("views/link.js")).unwrap();
        let secret = root.join("outside.js").display().to_string();
        let html = format!(
            r#"<link rel="stylesheet" href="views/app.css"><script src="./views/app.js"></script><script src="../outside.js"></script><script src="{secret}"></script><link rel="stylesheet" href="views/link.js"><script src="views/link.js"></script>"#
        );
        let out = inline_assets(&html, &pkg);
        assert!(out.contains("<style>INSIDE_CSS</style>"), "{out}");
        assert!(out.contains("<script>INSIDE_JS</script>"), "{out}");
        assert!(!out.contains("HOST_SECRET"), "{out}");
        // Refused references stay as written; the view's CSP blocks them.
        assert!(
            out.contains(r#"<script src="../outside.js"></script>"#),
            "{out}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn reinstalling_a_disabled_plugin_keeps_it_disabled() {
        let pkg = std::env::temp_dir().join(format!("lh-reg-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(pkg.join("src")).unwrap();
        std::fs::create_dir_all(pkg.join("schemas")).unwrap();
        std::fs::write(pkg.join("schemas/in.json"), "{}").unwrap();
        std::fs::write(
            pkg.join("src/main.luau"),
            "return { e = function() return 'x' end }",
        )
        .unwrap();
        std::fs::write(
            pkg.join("plugin.toml"),
            "id = \"kit\"\nversion = \"0.1.0\"\napi = 1\ntitle = \"Kit\"\ndescription = \"d\"\nlicense = \"MIT\"\nplatforms = [\"desktop\"]\nuse_when = \"u\"\navoid_when = \"a\"\ncost = \"instant\"\n\n[[tool]]\nname = \"kit_x\"\ndescription = \"d\"\nagents = [\"tutor\"]\ninput = \"schemas/in.json\"\nentry = \"e\"\n",
        )
        .unwrap();
        let source = || PluginSource::Registry { url: String::new() };
        let mut reg = PluginRegistry::new("desktop");
        reg.register(
            Arc::new(LuauPlugin::open(&pkg).unwrap()),
            source(),
            "h1".into(),
        );
        assert!(reg.is_enabled("kit") && reg.grantable("kit"));
        reg.disable("kit");
        // An update replaces the package; the learner's choice stands.
        reg.register(
            Arc::new(LuauPlugin::open(&pkg).unwrap()),
            source(),
            "h2".into(),
        );
        assert!(!reg.is_enabled("kit") && !reg.grantable("kit"));
        assert!(reg.available().is_empty());
        reg.enable("kit");
        assert!(reg.is_enabled("kit"));
        let _ = std::fs::remove_dir_all(pkg);
    }
}
