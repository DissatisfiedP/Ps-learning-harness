//! Built-in native plugins (docs/plugins.md §12): in-tree Rust types that
//! implement [`Plugin`] with the same `plugin.toml` shape as a Luau package,
//! and may declare permissions (`exec`, `net`, `fs-read`, `fs-write`).
//!
//! Each plugin lives in its own file here; its manifest, prompt fragments,
//! tool schemas and view sit under `plugins/native/<id>/` in this source
//! tree and are compiled in with `include_str!` ([`NativePackage`]); no
//! package directory exists at runtime.
//!
//! [`builtin`] is called once per `Harness` (the first `reload_plugins`);
//! later rebuilds of the registry re-register the same instances, so a
//! running REPL or lab survives an install elsewhere. Built-ins register
//! after installed and folder packages, so a package can't shadow one.
//! A built-in that declares a permission starts disabled: enabling it in
//! Settings is the learner's consent (`settings.json` → `enabledPlugins`).

pub mod cas;
pub mod code_parse;
pub mod code_runner;
pub mod exec;
pub mod image_set;
pub mod live_search;
pub mod repl;
pub mod sandbox;

use std::path::PathBuf;
use std::sync::Arc;

use providers::ToolSpec;
use serde_json::Value;

use super::manifest::{self, PluginManifest};
use super::{Plugin, ToolFuture, ToolOutcome};

/// What a native plugin may need from the host at construction time.
#[derive(Clone)]
pub struct NativeEnv {
    /// The synced data repo root.
    pub repo: PathBuf,
    /// The device-local config directory.
    pub config_dir: PathBuf,
    /// The app's shared HTTP client.
    pub client: providers::Client,
}

/// Every built-in native plugin, freshly constructed.
pub fn builtin(env: &NativeEnv) -> Vec<Arc<dyn Plugin>> {
    vec![
        Arc::new(code_parse::CodeParse::new()),
        Arc::new(live_search::LiveSearch::new(env)),
        Arc::new(image_set::ImageSetPlugin::new(env)),
        Arc::new(code_runner::CodeRunner::new()),
        Arc::new(repl::Repl::new()),
        Arc::new(cas::Cas::new()),
        Arc::new(sandbox::Sandbox::new()),
    ]
}

/// A plain card SVG (title plus text lines) for a native artifact's static
/// fallback: what a replay, a phone or a removed plugin shows.
pub fn card_svg(title: &str, lines: &[String]) -> String {
    const WIDTH: usize = 640;
    const CHARS: usize = 84;
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    let mut rows: Vec<String> = Vec::new();
    for line in lines {
        let chars: Vec<char> = line.chars().filter(|c| !c.is_control()).collect();
        if chars.is_empty() {
            rows.push(String::new());
        }
        for chunk in chars.chunks(CHARS) {
            rows.push(chunk.iter().collect());
        }
        if rows.len() >= 16 {
            rows.truncate(16);
            rows.push("…".into());
            break;
        }
    }
    let height = 48 + rows.len() * 18;
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {WIDTH} {height}\" width=\"{WIDTH}\" height=\"{height}\" font-family=\"ui-monospace, monospace\" font-size=\"13\"><rect x=\"1\" y=\"1\" width=\"{}\" height=\"{}\" rx=\"8\" fill=\"none\" stroke=\"currentColor\" stroke-opacity=\"0.4\"/><text x=\"16\" y=\"26\" font-weight=\"bold\" fill=\"currentColor\">{}</text>",
        WIDTH - 2,
        height - 2,
        esc(title)
    );
    for (i, row) in rows.iter().enumerate() {
        svg.push_str(&format!(
            "<text x=\"16\" y=\"{}\" fill=\"currentColor\" xml:space=\"preserve\">{}</text>",
            52 + i * 18,
            esc(row)
        ));
    }
    svg.push_str("</svg>");
    svg
}

/// A native plugin's compiled-in package: its validated manifest, the tool
/// specs built from its schemas, and the other files (fragments, view).
pub struct NativePackage {
    pub manifest: PluginManifest,
    pub tools: Vec<ToolSpec>,
    files: Vec<(&'static str, &'static str)>,
}

impl NativePackage {
    /// Parse `plugin.toml` text and validate it against `files` (paths
    /// relative to the package root, as the manifest names them) with the
    /// native rules (`manifest::validate_native`). A failure is a build
    /// mistake, so callers `expect` it and a unit test loads every built-in.
    pub fn parse(
        manifest_toml: &str,
        files: &[(&'static str, &'static str)],
    ) -> Result<Self, String> {
        let manifest: PluginManifest =
            toml::from_str(manifest_toml).map_err(|e| format!("plugin.toml: {e}"))?;
        manifest::validate_native(&manifest, files)?;
        let mut tools = Vec::with_capacity(manifest.tools.len());
        for tool in &manifest.tools {
            let text = files
                .iter()
                .find(|(p, _)| *p == tool.input)
                .map(|(_, t)| *t)
                .unwrap_or_default();
            let input_schema: Value = serde_json::from_str(text)
                .map_err(|e| format!("tool `{}` schema is not JSON ({e})", tool.name))?;
            if !input_schema.is_object() {
                return Err(format!("tool `{}` schema must be an object", tool.name));
            }
            tools.push(ToolSpec {
                name: tool.name.clone(),
                description: tool.description.clone(),
                input_schema,
            });
        }
        Ok(Self {
            manifest,
            tools,
            files: files.to_vec(),
        })
    }

    /// A compiled-in file by its package-relative path.
    pub fn file(&self, rel: &str) -> Option<&'static str> {
        self.files.iter().find(|(p, _)| *p == rel).map(|(_, t)| *t)
    }

    /// The fragment for one agent, empty when the package has none.
    pub fn fragment(&self, agent: &str) -> String {
        self.manifest
            .prompts
            .get(agent)
            .and_then(|p| self.file(p))
            .unwrap_or_default()
            .to_owned()
    }

    /// The view document, served as written: a native view is one
    /// self-contained HTML file with its script and style inline (no asset
    /// inlining happens).
    pub fn view_html(&self) -> Result<String, String> {
        let view = self
            .manifest
            .view
            .first()
            .ok_or_else(|| "this plugin has no view".to_owned())?;
        self.file(&view.html)
            .map(str::to_owned)
            .ok_or_else(|| format!("the view `{}` is not in the package", view.html))
    }

    /// Refuse a view call the manifest doesn't allow-list.
    pub fn check_view_call(&self, name: &str) -> Result<(), String> {
        let view = self
            .manifest
            .view
            .first()
            .ok_or_else(|| "this plugin has no view".to_owned())?;
        if view.calls.iter().any(|c| c == name) {
            Ok(())
        } else {
            Err(format!(
                "`{name}` is not one of the view's callable functions"
            ))
        }
    }

    /// The manifest entry (`entry` key) of tool `name`.
    pub fn entry(&self, name: &str) -> Option<&str> {
        self.manifest
            .tools
            .iter()
            .find(|t| t.name == name)
            .map(|t| t.entry.as_str())
    }
}

/// A finished tool result.
pub fn done(content: impl Into<String>) -> ToolOutcome {
    ToolOutcome {
        content: content.into(),
        is_error: false,
        artifact: None,
    }
}

/// A tool error the model reads (and can recover from).
pub fn fail(content: impl Into<String>) -> ToolOutcome {
    ToolOutcome {
        content: content.into(),
        is_error: true,
        artifact: None,
    }
}

/// An already-resolved tool future.
pub fn ready(outcome: ToolOutcome) -> ToolFuture {
    Box::pin(async move { outcome })
}

/// Run blocking work (processes, pipes) off the async runtime.
pub fn blocking<F>(f: F) -> ToolFuture
where
    F: FnOnce() -> ToolOutcome + Send + 'static,
{
    Box::pin(async move {
        tokio::task::spawn_blocking(f)
            .await
            .unwrap_or_else(|e| fail(format!("plugin call failed ({e})")))
    })
}
