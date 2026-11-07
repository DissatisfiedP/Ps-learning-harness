//! Plugin package manifest (`plugin.toml`) and strict validation (W5).
//!
//! Unknown keys are errors: a typo in an AI-written manifest must fail loudly
//! rather than silently drop a permission or an agent scope.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Host API version this build implements.
pub const API_VERSION: u32 = 1;
/// Most tools one plugin may add.
pub const MAX_TOOLS_PER_PLUGIN: usize = 3;
/// Fragments are "about 200 words at most"; anything past this is refused.
pub const MAX_FRAGMENT_WORDS: usize = 250;

/// Agents that can receive plugin tools today. The rest are reserved names
/// documented for later (the loader rejects them until those agents exist).
pub const AGENTS: &[&str] = &["tutor", "practice"];
pub const RESERVED_AGENTS: &[&str] = &["coach", "figure-maker", "demo-builder", "problem-checker"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ToolDecl {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub agents: Vec<String>,
    /// JSON Schema path, relative to the package root.
    pub input: String,
    /// Function exported by `src/main.luau`.
    pub entry: String,
    #[serde(default)]
    pub artifact: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct RendererDecl {
    pub entry: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ViewDecl {
    pub name: String,
    pub html: String,
    #[serde(default)]
    pub calls: Vec<String>,
    #[serde(default)]
    pub answer: bool,
}

/// Parsed `plugin.toml`. Field names follow the TOML keys (`use_when` etc.).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    pub id: String,
    pub version: String,
    pub api: u32,
    pub title: String,
    pub description: String,
    pub license: String,
    pub platforms: Vec<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
    pub use_when: String,
    pub avoid_when: String,
    pub cost: String,
    #[serde(default, rename = "tool")]
    pub tools: Vec<ToolDecl>,
    #[serde(default, rename = "renderer")]
    pub renderer: Vec<RendererDecl>,
    #[serde(default, rename = "view")]
    pub view: Vec<ViewDecl>,
    #[serde(default)]
    pub prompts: BTreeMap<String, String>,
}

impl PluginManifest {
    /// One-line catalog entry the planner sees (architecture §4.20).
    pub fn catalog_line(&self) -> String {
        let tools: Vec<&str> = self.tools.iter().map(|t| t.name.as_str()).collect();
        format!(
            "- **{}** (`{}`): tools [{}]; platforms [{}]. Use when: {} Avoid when: {} Cost: {}",
            self.title,
            self.id,
            tools.join(", "),
            self.platforms.join(", "),
            self.use_when,
            self.avoid_when,
            self.cost,
        )
    }

    /// Tool names, for the Settings plugin list.
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.iter().map(|t| t.name.clone()).collect()
    }
}

/// Parse and validate a manifest from text. `dir` is the package root used to
/// check that every referenced file exists.
pub fn parse(text: &str, dir: &Path) -> Result<PluginManifest, String> {
    let manifest: PluginManifest = toml::from_str(text).map_err(|e| format!("plugin.toml: {e}"))?;
    validate(&manifest, dir)?;
    Ok(manifest)
}

/// A plugin id: a lowercase letter, then a-z, 0-9 and '-'. Also the path
/// segment under `config/plugins/`, so install/remove refuse anything else.
pub fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Every core tool name any agent can receive (tutor in every phase, with
/// and without a workspace; planner; researcher). Plugin ids and tool names
/// must not collide with these.
pub fn core_tool_names() -> std::collections::BTreeSet<String> {
    use crate::state::Phase;
    use crate::tools;
    let mut names = std::collections::BTreeSet::new();
    for phase in [
        Phase::Measure,
        Phase::Plan,
        Phase::AwaitApproval,
        Phase::Teach,
        Phase::Close,
    ] {
        for workspace in [false, true] {
            names.extend(
                tools::tutor_tools(phase, workspace)
                    .into_iter()
                    .map(|t| t.name),
            );
        }
    }
    names.extend(tools::planner_tools().into_iter().map(|t| t.name));
    names.extend(tools::researcher_tools().into_iter().map(|t| t.name));
    names
}

/// Everything the loader refuses, each with the problem named (W5 acceptance).
pub fn validate(m: &PluginManifest, dir: &Path) -> Result<(), String> {
    if !valid_id(&m.id) {
        return Err(format!(
            "id `{}` must start with a lowercase letter and use only a-z, 0-9 and '-'",
            m.id
        ));
    }
    semver::Version::parse(&m.version)
        .map_err(|_| format!("version `{}` is not semver (e.g. 0.1.0)", m.version))?;
    if m.api != API_VERSION {
        return Err(format!(
            "api {} is not supported (this build implements api {API_VERSION})",
            m.api
        ));
    }
    if m.title.trim().is_empty() || m.description.trim().is_empty() {
        return Err("title and description are required".into());
    }
    if m.license.trim().is_empty() {
        return Err("license is required".into());
    }
    if m.platforms.is_empty()
        || m.platforms
            .iter()
            .any(|p| !matches!(p.as_str(), "desktop" | "mobile"))
    {
        return Err("platforms must be a non-empty subset of [desktop, mobile]".into());
    }
    if !m.permissions.is_empty() {
        return Err(format!(
            "permissions must be empty for a Luau plugin (api {API_VERSION}); got [{}]",
            m.permissions.join(", ")
        ));
    }
    if m.tools.len() > MAX_TOOLS_PER_PLUGIN {
        return Err(format!(
            "{} tools (at most {MAX_TOOLS_PER_PLUGIN} per plugin)",
            m.tools.len()
        ));
    }
    let prefix = m.id.replace('-', "_");
    let core = core_tool_names();
    if let Some(clash) = core.iter().find(|n| n.starts_with(&format!("{prefix}_"))) {
        return Err(format!(
            "id `{}` collides with the core tool `{clash}`; pick another id",
            m.id
        ));
    }
    for t in &m.tools {
        if !t.name.starts_with(&format!("{prefix}_")) {
            return Err(format!(
                "tool `{}` must start with `{prefix}_` (the plugin id with '-' replaced by '_')",
                t.name
            ));
        }
        if t.description.trim().is_empty() {
            return Err(format!("tool `{}` needs a description", t.name));
        }
        if t.agents.is_empty() {
            return Err(format!("tool `{}` needs at least one agent", t.name));
        }
        for a in &t.agents {
            if RESERVED_AGENTS.contains(&a.as_str()) {
                return Err(format!(
                    "tool `{}` names reserved agent `{a}`; this build supports [{}]",
                    t.name,
                    AGENTS.join(", ")
                ));
            }
            if !AGENTS.contains(&a.as_str()) {
                return Err(format!(
                    "tool `{}` names unknown agent `{a}` (valid: [{}])",
                    t.name,
                    AGENTS.join(", ")
                ));
            }
        }
        if t.entry.trim().is_empty() {
            return Err(format!("tool `{}` needs an entry function", t.name));
        }
        check_file(dir, &t.input, &format!("tool `{}` input schema", t.name))?;
        if t.artifact && m.renderer.is_empty() {
            return Err(format!(
                "tool `{}` declares artifact = true but the plugin has no [[renderer]]",
                t.name
            ));
        }
    }
    for (agent, path) in &m.prompts {
        if !AGENTS.contains(&agent.as_str()) {
            return Err(format!(
                "prompts key `{agent}` is not an agent (valid: [{}])",
                AGENTS.join(", ")
            ));
        }
        check_file(dir, path, &format!("prompts.{agent}"))?;
        let words = std::fs::read_to_string(dir.join(path))
            .unwrap_or_default()
            .split_whitespace()
            .count();
        if words > MAX_FRAGMENT_WORDS {
            return Err(format!(
                "prompts.{agent} has {words} words (about 200 at most)"
            ));
        }
    }
    if m.view.len() > 1 {
        return Err("at most one [[view]] is allowed".into());
    }
    if m.renderer.len() > 1 {
        return Err("at most one [[renderer]] is allowed".into());
    }
    if let Some(v) = m.view.first() {
        if v.name.trim().is_empty() {
            return Err("view.name is required".into());
        }
        check_file(dir, &v.html, "view html")?;
    }
    check_file(dir, "src/main.luau", "entry module")?;
    Ok(())
}

fn check_file(dir: &Path, rel: &str, what: &str) -> Result<(), String> {
    let path = dir.join(rel);
    if !path.is_file() {
        return Err(format!("{what} `{rel}` does not exist"));
    }
    // Never allow a manifest path to escape the package.
    if path
        .canonicalize()
        .ok()
        .zip(dir.canonicalize().ok())
        .is_some_and(|(p, d)| !p.starts_with(&d))
    {
        return Err(format!("{what} `{rel}` is outside the package"));
    }
    Ok(())
}

/// The manifest file for a package directory.
pub fn read(dir: &Path) -> Result<PluginManifest, String> {
    let text = std::fs::read_to_string(dir.join("plugin.toml"))
        .map_err(|e| format!("can't read plugin.toml ({e})"))?;
    parse(&text, dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
id = "demo-kit"
version = "0.1.0"
api = 1
title = "Demo"
description = "A test package."
license = "MIT"
platforms = ["desktop"]
permissions = []
use_when = "Tests."
avoid_when = "Lessons."
cost = "instant"

[[tool]]
name = "demo_kit_draw"
description = "Draw."
agents = ["tutor"]
input = "schemas/in.json"
entry = "draw"

[prompts]
tutor = "prompts/tutor.md"
"#;

    fn package(fragment_words: usize) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lh-manifest-{}", ulid::Ulid::new()));
        for sub in ["schemas", "prompts", "src"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        std::fs::write(dir.join("schemas/in.json"), "{}").unwrap();
        std::fs::write(dir.join("src/main.luau"), "return {}").unwrap();
        std::fs::write(dir.join("prompts/tutor.md"), "word ".repeat(fragment_words)).unwrap();
        dir
    }

    fn tool(name: &str, agent: &str) -> String {
        format!(
            "\n[[tool]]\nname = \"{name}\"\ndescription = \"d\"\nagents = [\"{agent}\"]\ninput = \"schemas/in.json\"\nentry = \"e\"\n"
        )
    }

    #[test]
    fn every_refused_manifest_names_its_problem() {
        let dir = package(20);
        parse(VALID, &dir).expect("the base manifest is valid");
        let with_tools = |extra: &[String]| {
            let (head, prompts) = VALID.split_once("\n[prompts]").unwrap();
            format!("{head}{}\n[prompts]{prompts}", extra.concat())
        };
        let cases: Vec<(&str, String, &str)> = vec![
            (
                "unknown key",
                VALID.replace("cost = \"instant\"", "cost = \"instant\"\ncolour = \"red\""),
                "unknown field `colour`",
            ),
            (
                "tool without the id prefix",
                VALID.replace("demo_kit_draw", "draw"),
                "tool `draw` must start with `demo_kit_`",
            ),
            (
                "a fourth tool",
                with_tools(&[
                    tool("demo_kit_a", "tutor"),
                    tool("demo_kit_b", "tutor"),
                    tool("demo_kit_c", "tutor"),
                ]),
                "4 tools (at most 3 per plugin)",
            ),
            (
                "non-empty permissions",
                VALID.replace("permissions = []", "permissions = [\"net\"]"),
                "permissions must be empty for a Luau plugin",
            ),
            (
                "api 2",
                VALID.replace("api = 1", "api = 2"),
                "api 2 is not supported",
            ),
            (
                "a reserved agent",
                VALID.replace("agents = [\"tutor\"]", "agents = [\"coach\"]"),
                "names reserved agent `coach`",
            ),
            (
                "an unknown agent",
                VALID.replace("agents = [\"tutor\"]", "agents = [\"wizard\"]"),
                "names unknown agent `wizard`",
            ),
            (
                "an id whose tools would collide with core tools",
                VALID
                    .replace("id = \"demo-kit\"", "id = \"question\"")
                    .replace("demo_kit_draw", "question_ask"),
                "id `question` collides with the core tool `question_ask`",
            ),
            (
                "an id sharing a core tool prefix",
                VALID
                    .replace("id = \"demo-kit\"", "id = \"file\"")
                    .replace("demo_kit_draw", "file_draw"),
                "id `file` collides with the core tool `file_",
            ),
        ];
        for (what, text, needle) in cases {
            let err = parse(&text, &dir).expect_err(what);
            assert!(err.contains(needle), "{what}: {err}");
        }
        // The fragment cap is on words in the file the manifest points at.
        let long = package(MAX_FRAGMENT_WORDS + 1);
        let err = parse(VALID, &long).expect_err("over-long fragment");
        assert!(
            err.contains(&format!(
                "prompts.tutor has {} words",
                MAX_FRAGMENT_WORDS + 1
            )),
            "{err}"
        );
        let at_cap = package(MAX_FRAGMENT_WORDS);
        assert!(parse(VALID, &at_cap).is_ok(), "at the cap is fine");
        for d in [dir, long, at_cap] {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}
