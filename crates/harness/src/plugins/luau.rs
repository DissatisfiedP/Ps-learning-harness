//! Sandboxed Luau plugin runtime (build prompt W5, D6).
//!
//! One VM per plugin, created lazily and reused. Limits: 64 MiB memory, 2 s
//! per tool call, 200 ms per view bridge call, checked by the Luau interrupt.
//! A call that hits a limit is answered as a tool error and the VM is thrown
//! away so the next call starts clean.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use mlua::luau::{NavigateError, Require};
use mlua::{Lua, LuaOptions, StdLib, Table, Value, VmState};
use serde_json::Value as Json;

use super::host;
use super::{ArtifactPayload, ToolOutcome};

/// Byte cap for one Luau VM.
pub const MEMORY_LIMIT: usize = 64 * 1024 * 1024;
/// Wall-clock cap for one tool call (milliseconds).
pub const TOOL_TIMEOUT_MS: u64 = 2_000;
/// Wall-clock cap for one view bridge call (milliseconds).
pub const VIEW_TIMEOUT_MS: u64 = 200;
/// Tool result text cap is enforced by the caller (W5).
pub const MAX_RESULT_BYTES: usize = 16 * 1024;
/// Cap on an artifact's serialized `data` (same as the fallback SVG cap, W6).
pub const MAX_ARTIFACT_DATA_BYTES: usize = 256 * 1024;
/// The one return shape of an `artifact = true` tool (W6), quoted in errors.
const ARTIFACT_SHAPE: &str =
    "an artifact tool must return { text = \"…\", artifact = { kind = \"svg\", svg = \"<svg…>\", data = {…} } }";
/// Test-file cap, so a runaway test cannot hold the CLI forever.
pub const TEST_TIMEOUT_MS: u64 = 10_000;

struct Vm {
    lua: Lua,
    /// Milliseconds after `epoch` at which the running call must stop.
    deadline_ms: Arc<AtomicU64>,
    /// Monotonic base for the deadline, so wall-clock jumps don't matter.
    epoch: Instant,
}

pub struct LuauRuntime {
    dir: PathBuf,
    vm: Arc<Mutex<Option<Vm>>>,
    /// `plugin test` VMs: `@harness/test` exists and tests/ files may require.
    test: bool,
}

impl LuauRuntime {
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_owned(),
            vm: Arc::new(Mutex::new(None)),
            test: false,
        }
    }

    fn with_vm<R>(
        &self,
        timeout_ms: u64,
        f: impl FnOnce(&Lua) -> mlua::Result<R>,
    ) -> Result<R, String> {
        let mut slot = self
            .vm
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some(self.build()?);
        }
        let vm = slot.as_mut().expect("vm just built");
        let now = vm.epoch.elapsed().as_millis() as u64;
        vm.deadline_ms.store(now + timeout_ms, Ordering::Relaxed);
        let out = f(&vm.lua);
        if out.is_err() {
            *slot = None;
        }
        out.map_err(|e| match e {
            mlua::Error::MemoryError(m) => format!("plugin memory limit: {m}"),
            mlua::Error::RuntimeError(m) if m.contains("time limit") => {
                format!("plugin time limit ({timeout_ms} ms) exceeded")
            }
            other => other.to_string(),
        })
    }

    fn build(&self) -> Result<Vm, String> {
        let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default())
            .map_err(|e| format!("can't create the Luau VM ({e})"))?;
        {
            let globals = lua.globals();
            for name in [
                "io",
                "os",
                "debug",
                "package",
                "dofile",
                "loadfile",
                "loadstring",
                "load",
                "collectgarbage",
            ] {
                let _ = globals.set(name, Value::Nil);
            }
        }
        let modules = host::modules(&lua).map_err(|e| format!("host library: {e}"))?;
        if self.test {
            let test =
                host::test_module(&lua, &self.dir).map_err(|e| format!("@harness/test: {e}"))?;
            modules
                .set("test", test)
                .map_err(|e| format!("@harness/test: {e}"))?;
        }
        lua.set_named_registry_value(HARNESS_MODULES, &modules)
            .map_err(|e| e.to_string())?;
        let require = lua
            .create_require_function(PackageRequire::new(&self.dir, self.test))
            .map_err(|e| format!("require: {e}"))?;
        lua.globals()
            .set("require", require)
            .map_err(|e| e.to_string())?;
        let deadline = Arc::new(AtomicU64::new(u64::MAX));
        let interrupt_deadline = Arc::clone(&deadline);
        let epoch = Instant::now();
        lua.set_interrupt(move |_| {
            if epoch.elapsed().as_millis() as u64 > interrupt_deadline.load(Ordering::Relaxed) {
                return Err(mlua::Error::RuntimeError("time limit exceeded".into()));
            }
            Ok(VmState::Continue)
        });
        lua.sandbox(true)
            .map_err(|e| format!("can't sandbox the Luau VM ({e})"))?;
        lua.set_memory_limit(MEMORY_LIMIT)
            .map_err(|e| format!("can't set the memory limit ({e})"))?;
        Ok(Vm {
            lua,
            deadline_ms: deadline,
            epoch,
        })
    }

    /// The entry module's exports, evaluated once per VM and cached in the
    /// registry (a fresh VM — after an error — re-reads src/main.luau).
    fn export(&self, lua: &Lua, name: &str) -> Result<mlua::Function, String> {
        const KEY: &str = "__plugin_exports";
        let table: Table = match lua.named_registry_value::<Option<Table>>(KEY) {
            Ok(Some(t)) => t,
            _ => {
                let source = std::fs::read_to_string(self.dir.join("src/main.luau"))
                    .map_err(|e| format!("can't read src/main.luau ({e})"))?;
                let t: Table = lua
                    .load(&source)
                    .set_name("@src/main.luau")
                    .eval()
                    .map_err(|e| format!("src/main.luau: {e}"))?;
                lua.set_named_registry_value(KEY, &t)
                    .map_err(|e| e.to_string())?;
                t
            }
        };
        table
            .get::<mlua::Function>(name)
            .map_err(|_| format!("the plugin exports no function `{name}`"))
    }

    /// One plugin tool call. `artifact = true` means the entry returns
    /// `{ text?, artifact = { kind = "svg", svg, data? } }`.
    pub fn call_tool(&self, entry: &str, args: &Json, artifact: bool) -> ToolOutcome {
        let entry = entry.to_owned();
        let args = args.clone();
        let result = self.with_vm(TOOL_TIMEOUT_MS, |lua| {
            let f = self
                .export(lua, &entry)
                .map_err(mlua::Error::RuntimeError)?;
            let lua_args = host::json_to_lua(lua, &args)?;
            let out: Value = f.call(lua_args)?;
            Ok(out)
        });
        match result {
            Err(error) => ToolOutcome {
                content: format!("plugin error: {error}"),
                is_error: true,
                artifact: None,
            },
            Ok(value) => match self.decode_outcome(&value, artifact) {
                Ok(out) => out,
                Err(error) => ToolOutcome {
                    content: format!("plugin error: {error}"),
                    is_error: true,
                    artifact: None,
                },
            },
        }
    }

    fn decode_outcome(&self, value: &Value, artifact: bool) -> Result<ToolOutcome, String> {
        if !artifact {
            let content = match value {
                Value::String(s) => s.to_str().map(|s| s.to_owned()).unwrap_or_default(),
                Value::Table(t) => {
                    if let Ok(Some(text)) = t.get::<Option<String>>("text") {
                        text
                    } else {
                        json_text(value)?
                    }
                }
                Value::Nil => String::new(),
                other => json_text(other)?,
            };
            return Ok(ToolOutcome {
                content: truncate(content),
                is_error: false,
                artifact: None,
            });
        }
        let Value::Table(t) = value else {
            return Err(ARTIFACT_SHAPE.into());
        };
        let text = t
            .get::<Option<String>>("text")
            .ok()
            .flatten()
            .unwrap_or_default();
        let artifact = match t.get::<Value>("artifact") {
            Ok(Value::Table(a)) => a,
            Ok(Value::Nil) => {
                return Err(format!("the result has no `artifact`; {ARTIFACT_SHAPE}"))
            }
            _ => return Err(format!("`artifact` must be a table; {ARTIFACT_SHAPE}")),
        };
        match artifact.get::<Option<String>>("kind").ok().flatten() {
            Some(kind) if kind == "svg" => {}
            Some(kind) => {
                return Err(format!(
                    "artifact.kind `{kind}` is not supported (only \"svg\")"
                ))
            }
            None => return Err("artifact.kind is required (\"svg\")".into()),
        }
        let svg = artifact
            .get::<Option<String>>("svg")
            .ok()
            .flatten()
            .ok_or_else(|| "artifact.svg is required (the static SVG figure)".to_owned())?;
        let data = match artifact.get::<Option<Value>>("data").ok().flatten() {
            Some(v) => host::lua_value_to_json(&v).map_err(|e| format!("artifact.data: {e}"))?,
            None => Json::Null,
        };
        let size = serde_json::to_string(&data).map(|s| s.len()).unwrap_or(0);
        if size > MAX_ARTIFACT_DATA_BYTES {
            return Err(format!(
                "artifact.data is {} KiB, over the {} KiB limit; ask for less data (a smaller figure)",
                size.div_ceil(1024),
                MAX_ARTIFACT_DATA_BYTES / 1024
            ));
        }
        Ok(ToolOutcome {
            content: truncate(text),
            is_error: false,
            artifact: Some(ArtifactPayload {
                kind: "svg".into(),
                svg,
                data,
            }),
        })
    }

    /// Render an artifact's data to its fallback SVG.
    pub fn render(&self, entry: &str, data: &Json) -> Result<String, String> {
        let entry = entry.to_owned();
        let data = data.clone();
        self.with_vm(TOOL_TIMEOUT_MS, |lua| {
            let f = self
                .export(lua, &entry)
                .map_err(mlua::Error::RuntimeError)?;
            let out: Value = f.call(host::json_to_lua(lua, &data)?)?;
            match out {
                Value::String(s) => Ok(s.to_str().map(|s| s.to_owned()).unwrap_or_default()),
                other => Err(mlua::Error::RuntimeError(format!(
                    "renderer returned {:?}, not an SVG string",
                    other
                ))),
            }
        })
    }

    /// A view bridge call: `calls` names an exported function.
    pub fn view_call(&self, function: &str, args: &Json) -> Result<Json, String> {
        let function = function.to_owned();
        let args = args.clone();
        self.with_vm(VIEW_TIMEOUT_MS, |lua| {
            let f = self
                .export(lua, &function)
                .map_err(mlua::Error::RuntimeError)?;
            let out: Value = f.call(host::json_to_lua(lua, &args)?)?;
            host::lua_value_to_json(&out)
        })
    }

    /// Answer-mode grading. `target` is never shown to the learner.
    pub fn grade(&self, answer: &Json, target: &Json) -> Result<Json, String> {
        let answer = answer.clone();
        let target = target.clone();
        self.with_vm(TOOL_TIMEOUT_MS, |lua| {
            let f = self
                .export(lua, "grade")
                .map_err(mlua::Error::RuntimeError)?;
            let out: Value = f.call((
                host::json_to_lua(lua, &answer)?,
                host::json_to_lua(lua, &target)?,
            ))?;
            host::lua_value_to_json(&out)
        })
    }

    /// Run a `tests/*.luau` file. The chunk may return a function or a table
    /// of named functions; either way every `assert` inside must pass.
    pub fn run_test(&self, path: &Path) -> Result<Vec<String>, String> {
        let source = std::fs::read_to_string(path)
            .map_err(|e| format!("can't read {} ({e})", path.display()))?;
        let name = path.display().to_string();
        // Named by its place in the package, so its requires resolve from it.
        let chunk = match path.strip_prefix(&self.dir) {
            Ok(rel) => format!("@{}", rel.display()),
            Err(_) => format!("={name}"),
        };
        self.with_vm(TEST_TIMEOUT_MS, |lua| {
            let value: Value = lua.load(&source).set_name(&chunk).eval()?;
            let mut ran = Vec::new();
            match value {
                Value::Function(f) => {
                    f.call::<()>(())?;
                    ran.push(name.clone());
                }
                Value::Table(t) => {
                    let mut entries: Vec<(String, mlua::Function)> = Vec::new();
                    for pair in t.clone().pairs::<String, mlua::Function>() {
                        let (k, f) = pair?;
                        entries.push((k, f));
                    }
                    if entries.is_empty() {
                        return Err(mlua::Error::RuntimeError(
                            "the test file returned an empty table".into(),
                        ));
                    }
                    entries.sort_by(|a, b| a.0.cmp(&b.0));
                    for (k, f) in entries {
                        f.call::<()>(())?;
                        ran.push(format!("{name}::{k}"));
                    }
                }
                Value::Nil => {}
                _ => {
                    return Err(mlua::Error::RuntimeError(
                        "a test file must return a function or a table of functions".into(),
                    ))
                }
            }
            Ok(ran)
        })
    }

    /// Type-check the package (`src/` and `tests/`, recursively) when
    /// `luau-analyze` is installed. Returns `None` when the binary is absent
    /// (documented in docs/plugins.md).
    ///
    /// luau-analyze resolves `@harness/*` through a `.luaurc` above the
    /// requiring file, and a package may sit anywhere on disk, so the check
    /// runs on a copy: the package's `.luau` files plus the bundled type
    /// modules (crates/harness/plugin-api/) under a generated `.luaurc`.
    /// No flags: luau-analyze's default nonstrict mode (a file opts into
    /// `--!strict`) and default (new) solver; 0.738 rejects `--mode=nonstrict`.
    /// The copy is real files: through symlinked folders luau-analyze can't
    /// resolve requires.
    pub fn analyze(&self) -> Option<Result<String, String>> {
        let bin = which("luau-analyze")?;
        let work = std::env::temp_dir().join(format!("lh-analyze-{}", ulid::Ulid::new()));
        let result = self.analyze_in(&bin, &work);
        let _ = std::fs::remove_dir_all(&work);
        Some(result)
    }

    fn analyze_in(&self, bin: &Path, work: &Path) -> Result<String, String> {
        let io = |e: std::io::Error| format!("can't prepare the type check ({e})");
        std::fs::create_dir_all(work.join("harness")).map_err(io)?;
        std::fs::write(
            work.join(".luaurc"),
            r#"{ "aliases": { "harness": "./harness" } }"#,
        )
        .map_err(io)?;
        for (name, source) in HARNESS_TYPES {
            std::fs::write(work.join("harness").join(name), source).map_err(io)?;
        }
        let mut files = Vec::new();
        for folder in ["src", "tests"] {
            copy_luau(
                &self.dir.join(folder),
                &work.join(folder),
                Path::new(folder),
                &mut files,
            )
            .map_err(io)?;
        }
        if !files.iter().any(|f| f.starts_with("src")) {
            return Err("src/ has no *.luau files".into());
        }
        files.sort();
        // Absolute, so a file is the same module whether passed or required.
        let out = std::process::Command::new(bin)
            .current_dir(work)
            .args(files.iter().map(|f| work.join(f)))
            .output()
            .map_err(|e| format!("can't run luau-analyze ({e})"))?;
        // Report paths in the package, not in the copy.
        let report = String::from_utf8_lossy(&out.stderr)
            .replace(&work.display().to_string(), &self.dir.display().to_string());
        if out.status.success() {
            Ok(report)
        } else {
            Err(report)
        }
    }
}

/// The api-1 type modules `@harness/*` resolves to under `luau-analyze`.
const HARNESS_TYPES: [(&str, &str); 6] = [
    ("json.luau", include_str!("../../plugin-api/json.luau")),
    ("svg.luau", include_str!("../../plugin-api/svg.luau")),
    ("num.luau", include_str!("../../plugin-api/num.luau")),
    ("rand.luau", include_str!("../../plugin-api/rand.luau")),
    ("log.luau", include_str!("../../plugin-api/log.luau")),
    ("test.luau", include_str!("../../plugin-api/test.luau")),
];

/// Copy the `.luau` files under `from` (followed into real subfolders, not
/// symlinked ones) to `to`, recording each copy's path relative to the work
/// folder. A missing `from` copies nothing.
fn copy_luau(from: &Path, to: &Path, rel: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let Ok(entries) = std::fs::read_dir(from) else {
        return Ok(());
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let rel = rel.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_luau(&path, &to.join(entry.file_name()), &rel, files)?;
        } else if path.extension().is_some_and(|e| e == "luau") && path.is_file() {
            std::fs::create_dir_all(to)?;
            std::fs::copy(&path, to.join(entry.file_name()))?;
            files.push(rel);
        }
    }
    Ok(())
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|p| p.join(name))
        .find(|p| p.is_file())
}

fn json_text(value: &Value) -> Result<String, String> {
    let json = host::lua_value_to_json(value).map_err(|e| e.to_string())?;
    serde_json::to_string(&json).map_err(|e| e.to_string())
}

fn truncate(mut text: String) -> String {
    if text.len() > MAX_RESULT_BYTES {
        text.truncate(text.floor_char_boundary(MAX_RESULT_BYTES));
        text.push_str("\n[result truncated at 16 KiB]");
    }
    text
}

/// Registry key of the table of host modules `@harness/<name>` returns.
const HARNESS_MODULES: &str = "__harness_modules";
/// The host modules of every VM; `test` is added in `plugin test` VMs.
const HARNESS_NAMES: [&str; 5] = ["json", "svg", "num", "rand", "log"];

/// Luau require-by-string for one plugin package. mlua runs Luau's own
/// navigator (the one luau-analyze uses); this supplies the package's view:
///
/// - `./x` and `../x` resolve from the requiring module's path (its file
///   without `.luau`; for `init.luau`, its folder), `@self/x` from the module
///   itself;
/// - module path `p` is the file `p.luau` or `p/init.luau` (both present is
///   ambiguous); a folder without `init.luau` is only a step on the way;
/// - `@harness/<name>` is a host module, `@harness/test` only under
///   `plugin test`; there are no other aliases;
/// - a path never steps above the package root, and a module's file (after
///   symlinks) must lie in `src/`, or also `tests/` under `plugin test`;
/// - results are cached per resolved file for the VM's life (mlua's cache).
struct PackageRequire {
    /// The package folder (canonical).
    root: PathBuf,
    /// Folders a module's file may resolve into (canonical).
    allowed: Vec<PathBuf>,
    test: bool,
    /// The requiring chunk (package-relative), for error messages.
    requirer: String,
    at: At,
    /// Set while Luau looks for a `.luaurc` declaring an alias: stepping
    /// above the root then ends that search instead of failing.
    alias_search: bool,
}

enum At {
    /// A module path relative to the package root (empty: the root), and
    /// the file it names, if any.
    Package {
        path: Vec<String>,
        file: Option<PathBuf>,
    },
    Harness,
    HarnessModule(String),
}

fn require_error(message: String) -> NavigateError {
    NavigateError::Other(mlua::Error::RuntimeError(message))
}

impl PackageRequire {
    fn new(dir: &Path, test: bool) -> Self {
        let canonical = |p: PathBuf| p.canonicalize().unwrap_or(p);
        let root = canonical(dir.to_owned());
        let mut allowed = vec![canonical(root.join("src"))];
        if test {
            allowed.push(canonical(root.join("tests")));
        }
        Self {
            root,
            allowed,
            test,
            requirer: String::new(),
            at: At::Harness,
            alias_search: false,
        }
    }

    /// The file module path `path` names (Luau CLI rules, `.luau` only).
    fn resolve(&self, path: &[String]) -> Result<Option<PathBuf>, NavigateError> {
        let Some(last) = path.last() else {
            return Ok(None);
        };
        let base = path.iter().fold(self.root.clone(), |p, c| p.join(c));
        let mut file = base.clone().into_os_string();
        file.push(".luau");
        let file = PathBuf::from(file);
        let found = last != "init" && file.is_file();
        if base.is_dir() {
            if found {
                return Err(NavigateError::Ambiguous);
            }
            let init = base.join("init.luau");
            return Ok(init.is_file().then_some(init));
        }
        if found {
            Ok(Some(file))
        } else {
            Err(NavigateError::NotFound)
        }
    }
}

impl Require for PackageRequire {
    fn is_require_allowed(&self, chunk_name: &str) -> bool {
        chunk_name.starts_with('@')
    }

    fn reset(&mut self, chunk_name: &str) -> Result<(), NavigateError> {
        self.alias_search = false;
        let rel = chunk_name
            .strip_prefix('@')
            .ok_or(NavigateError::NotFound)?;
        self.requirer = rel.to_owned();
        let mut path: Vec<String> = rel
            .split('/')
            .filter(|c| !c.is_empty() && *c != ".")
            .map(str::to_owned)
            .collect();
        if path.last().is_some_and(|c| c == "init.luau") {
            path.pop();
        } else if let Some(stem) = path.last().and_then(|c| c.strip_suffix(".luau")) {
            let stem = stem.to_owned();
            *path.last_mut().expect("non-empty") = stem;
        }
        let file = self.resolve(&path)?;
        self.at = At::Package { path, file };
        Ok(())
    }

    fn jump_to_alias(&mut self, _path: &str) -> Result<(), NavigateError> {
        // Only reached through a `.luaurc` alias, and none is read.
        Err(NavigateError::NotFound)
    }

    fn to_alias_override(&mut self, alias: &str) -> Result<(), NavigateError> {
        if alias == "harness" {
            self.at = At::Harness;
            return Ok(());
        }
        self.alias_search = true;
        Err(NavigateError::NotFound)
    }

    fn to_alias_fallback(&mut self, alias: &str) -> Result<(), NavigateError> {
        Err(require_error(format!(
            "@{alias} is not a require alias here: a plugin requires @harness/<module>, @self/…, ./… or ../…"
        )))
    }

    fn to_parent(&mut self) -> Result<(), NavigateError> {
        let At::Package { path, .. } = &self.at else {
            return Err(NavigateError::NotFound);
        };
        if path.is_empty() {
            if self.alias_search {
                return Err(NavigateError::NotFound);
            }
            return Err(require_error(format!(
                "{}: a require path can't leave the plugin package",
                self.requirer
            )));
        }
        let mut path = path.clone();
        path.pop();
        // As in Luau, stepping up is never ambiguous.
        let file = match self.resolve(&path) {
            Ok(file) => file,
            Err(NavigateError::Ambiguous) => None,
            Err(e) => return Err(e),
        };
        self.at = At::Package { path, file };
        Ok(())
    }

    fn to_child(&mut self, name: &str) -> Result<(), NavigateError> {
        match &self.at {
            At::Package { path, .. } => {
                let mut path = path.clone();
                path.push(name.to_owned());
                let file = self.resolve(&path)?;
                self.at = At::Package { path, file };
                Ok(())
            }
            At::Harness if HARNESS_NAMES.contains(&name) || (name == "test" && self.test) => {
                self.at = At::HarnessModule(name.to_owned());
                Ok(())
            }
            At::Harness if name == "test" => Err(require_error(
                "@harness/test is only available under `plugin test`".into(),
            )),
            At::Harness => Err(require_error(format!(
                "@harness/{name} is not a harness module (json, svg, num, rand, log)"
            ))),
            At::HarnessModule(module) => Err(require_error(format!(
                "@harness/{module}/{name} is not a harness module"
            ))),
        }
    }

    fn has_module(&self) -> bool {
        match &self.at {
            At::Package { file, .. } => file.is_some(),
            At::HarnessModule(_) => true,
            At::Harness => false,
        }
    }

    fn cache_key(&self) -> String {
        match &self.at {
            At::Package {
                file: Some(file), ..
            } => file
                .canonicalize()
                .unwrap_or_else(|_| file.clone())
                .display()
                .to_string(),
            At::HarnessModule(module) => format!("@harness/{module}"),
            _ => String::new(),
        }
    }

    fn has_config(&self) -> bool {
        false
    }

    fn config(&self) -> std::io::Result<Vec<u8>> {
        Err(std::io::ErrorKind::NotFound.into())
    }

    fn loader(&self, lua: &Lua) -> mlua::Result<mlua::Function> {
        let file = match &self.at {
            At::HarnessModule(module) => {
                let module = module.clone();
                return lua.create_function(move |lua, ()| {
                    lua.named_registry_value::<Table>(HARNESS_MODULES)?
                        .get::<Value>(module.as_str())
                });
            }
            At::Package {
                file: Some(file), ..
            } => file,
            _ => return Err(mlua::Error::RuntimeError("no module to load".into())),
        };
        let rel = file.strip_prefix(&self.root).unwrap_or(file).display();
        let canonical = file
            .canonicalize()
            .map_err(|e| mlua::Error::RuntimeError(format!("can't read `{rel}` ({e})")))?;
        if !self.allowed.iter().any(|root| canonical.starts_with(root)) {
            let roots = if self.test { "src/ and tests/" } else { "src/" };
            return Err(mlua::Error::RuntimeError(format!(
                "module `{rel}` resolves outside the plugin's {roots}"
            )));
        }
        let source = std::fs::read_to_string(&canonical)
            .map_err(|e| mlua::Error::RuntimeError(format!("can't read `{rel}` ({e})")))?;
        lua.load(source).set_name(format!("@{rel}")).into_function()
    }
}

/// Run every `tests/*.luau` file of a package (no manifest validation here).
pub fn test_package(dir: &Path) -> Result<Vec<String>, String> {
    let runtime = LuauRuntime {
        test: true,
        ..LuauRuntime::new(dir)
    };
    let mut ran = Vec::new();
    let tests = dir.join("tests");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&tests)
        .map_err(|_| "the package has no tests/ folder".to_owned())?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "luau"))
        .collect();
    files.sort();
    if files.is_empty() {
        return Err("tests/ has no *.luau files".into());
    }
    for file in files {
        ran.extend(runtime.run_test(&file)?);
    }
    Ok(ran)
}

/// Analyze one package directory (CLI helper).
pub fn analyze_package(dir: &Path) -> Option<Result<String, String>> {
    LuauRuntime::new(dir).analyze()
}
#[cfg(test)]
mod tests {
    use super::*;

    fn package(main: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lh-plugin-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.luau"), main).unwrap();
        dir
    }

    #[test]
    fn a_runaway_loop_hits_the_time_limit_and_the_next_call_runs() {
        let dir = package(
            "return {\n echo = function(a) return a.n end,\n spin = function() while true do end end,\n}\n",
        );
        let rt = LuauRuntime::new(&dir);
        let start = std::time::Instant::now();
        let out = rt.call_tool("spin", &Json::Null, false);
        assert!(out.is_error, "{}", out.content);
        assert!(out.content.contains("time limit"), "{}", out.content);
        assert!(
            start.elapsed() < std::time::Duration::from_millis(TOOL_TIMEOUT_MS + 1_000),
            "the interrupt must stop the loop, not the caller"
        );
        // The VM that hit the limit is thrown away; the next call is clean.
        let out = rt.call_tool("echo", &serde_json::json!({ "n": 7 }), false);
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(out.content, "7");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_memory_bomb_is_refused_with_a_memory_error() {
        let dir = package(
            "return {\n echo = function() return 'ok' end,\n grow = function()\n  local t = {}\n  for i = 1, 16 do t[i] = string.rep(string.char(64 + i), 16 * 1024 * 1024) end\n end,\n}\n",
        );
        let rt = LuauRuntime::new(&dir);
        let out = rt.call_tool("grow", &Json::Null, false);
        assert!(out.is_error, "{}", out.content);
        assert!(
            out.content.to_ascii_lowercase().contains("memory"),
            "{}",
            out.content
        );
        assert_eq!(rt.call_tool("echo", &Json::Null, false).content, "ok");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn require_cannot_escape_the_package() {
        let dir = package(
            "return {\n root = function() return require('../outside') end,\n up = function() return require('../../outside') end,\n abs = function() return require('/etc/passwd') end,\n bare = function() return require('util') end,\n}\n",
        );
        std::fs::write(dir.join("outside.luau"), "return 'outside'").unwrap();
        std::fs::write(dir.join("src/util.luau"), "return 'util'").unwrap();
        let rt = LuauRuntime::new(&dir);
        for (entry, needle) in [
            // The package root is a step on the way, never a module source.
            (
                "root",
                "module `outside.luau` resolves outside the plugin's src/",
            ),
            (
                "up",
                "src/main.luau: a require path can't leave the plugin package",
            ),
            ("abs", "must start with a valid prefix: ./, ../, or @"),
            // Luau require-by-string: no bare names.
            ("bare", "must start with a valid prefix: ./, ../, or @"),
        ] {
            let out = rt.call_tool(entry, &Json::Null, false);
            assert!(
                out.is_error && out.content.contains(needle),
                "{entry}: {}",
                out.content
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn requires_resolve_from_the_requiring_file() {
        let dir = package(
            "local child = require('./folder/child')\nlocal group = require('./group')\nreturn { run = function()\n  return { child.name, group.name, group.inner, tostring(child.sibling == require('./folder/../sibling')) }\nend }\n",
        );
        std::fs::create_dir_all(dir.join("src/folder")).unwrap();
        std::fs::create_dir_all(dir.join("src/group")).unwrap();
        std::fs::write(
            dir.join("src/folder/child.luau"),
            "local sibling = require('../sibling')\nreturn { name = 'child of ' .. sibling.name, sibling = sibling }",
        )
        .unwrap();
        std::fs::write(dir.join("src/sibling.luau"), "return { name = 'sibling' }").unwrap();
        // init.luau is its folder's module: `./` reaches the folder's
        // siblings, `@self/` its own children.
        std::fs::write(
            dir.join("src/group/init.luau"),
            "return { name = require('./sibling').name, inner = require('@self/inner') }",
        )
        .unwrap();
        std::fs::write(dir.join("src/group/inner.luau"), "return 'inner'").unwrap();
        let out = LuauRuntime::new(&dir).call_tool("run", &Json::Null, false);
        assert_eq!(
            (out.is_error, out.content.as_str()),
            (false, r#"["child of sibling","sibling","inner","true"]"#),
            "the same file is one cached module, whatever path reached it"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_folder_and_a_file_of_one_name_are_ambiguous() {
        let dir = package("return { get = function() return require('./util') end }\n");
        std::fs::write(dir.join("src/util.luau"), "return 1").unwrap();
        std::fs::create_dir_all(dir.join("src/util")).unwrap();
        std::fs::write(dir.join("src/util/init.luau"), "return 2").unwrap();
        let out = LuauRuntime::new(&dir).call_tool("get", &Json::Null, false);
        assert!(
            out.is_error
                && out
                    .content
                    .contains("could not resolve child component \"util\" (ambiguous)"),
            "{}",
            out.content
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn only_harness_and_self_aliases_resolve() {
        let dir = package(
            "return {\n test = function() return require('@harness/test') end,\n other = function() return require('@harness/fs') end,\n alias = function() return require('@lune/fs') end,\n}\n",
        );
        let rt = LuauRuntime::new(&dir);
        for (entry, needle) in [
            (
                "test",
                "@harness/test is only available under `plugin test`",
            ),
            (
                "other",
                "@harness/fs is not a harness module (json, svg, num, rand, log)",
            ),
            ("alias", "@lune is not a require alias here"),
        ] {
            let out = rt.call_tool(entry, &Json::Null, false);
            assert!(
                out.is_error && out.content.contains(needle),
                "{entry}: {}",
                out.content
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_host_module_is_reachable_through_require() {
        let dir = package(
            "local num = require('@harness/num')\nreturn { solve = function()\n  local x = num.linsolve({ { 2 } }, { 6 })\n  return tostring(x[1])\nend }\n",
        );
        let rt = LuauRuntime::new(&dir);
        let out = rt.call_tool("solve", &Json::Null, false);
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(out.content, "3");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn num_mul_multiplies_matrices_by_vectors_and_refuses_other_shapes() {
        let dir = package(
            r#"local num = require("@harness/num")
return {
  mv = function() return num.mul({ { 1, 2 }, { 3, 4 } }, { 5, 6 }) end,
  mm = function() return num.mul({ { 1, 2 }, { 3, 4 } }, { { 1, 0 }, { 0, 1 } }) end,
  cv = function() return num.mul({ { num.c(0, 1) } }, { num.c(0, 1) }) end,
  short = function() return num.mul({ { 1, 2 } }, { 1 }) end,
  scaled = function() return num.mul(2, { { 1 } }) end,
  ragged = function() return num.mul({ { 1, 2 }, { 3 } }, { 1, 1 }) end,
}
"#,
        );
        let rt = LuauRuntime::new(&dir);
        let call = |f: &str| rt.call_tool(f, &Json::Null, false);
        assert_eq!(call("mv").content, "[17,39]");
        assert_eq!(call("mm").content, "[[1,2],[3,4]]");
        assert_eq!(
            call("cv").content,
            "[-1]",
            "i·i = -1, returned as a plain number"
        );
        for (f, needle) in [
            (
                "short",
                "a matrix with 2 columns needs a vector of 2 numbers, got 1",
            ),
            (
                "scaled",
                "mul: expects two numbers, a matrix and a vector, or two matrices",
            ),
            ("ragged", "mul: the matrix's rows have different lengths"),
        ] {
            let out = call(f);
            assert!(
                out.is_error && out.content.contains(needle),
                "{f}: {}",
                out.content
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_self_referential_result_is_a_tool_error_and_the_next_call_works() {
        let dir = package(
            "local json = require('@harness/json')\nreturn {\n loop = function() local t = {} t.t = t return t end,\n deep = function() local t = {} for i = 1, 100 do t = { t } end return t end,\n enc = function() local t = {} t[1] = t return json.encode(t) end,\n echo = function(a) return a.n end,\n}\n",
        );
        let rt = LuauRuntime::new(&dir);
        for (f, needle) in [
            ("loop", "refers to itself"),
            ("deep", "nests deeper than 64"),
            ("enc", "refers to itself"),
        ] {
            let out = rt.call_tool(f, &Json::Null, false);
            assert!(
                out.is_error && out.content.contains(needle),
                "{f}: {}",
                out.content
            );
            let out = rt.call_tool("echo", &serde_json::json!({ "n": 7 }), false);
            assert_eq!(
                (out.is_error, out.content.as_str()),
                (false, "7"),
                "after {f}"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn only_contiguous_integer_keys_make_an_array() {
        let dir = package(
            "return {\n dense = function() return { 'a', 'b' } end,\n sparse = function() return { [1] = 'a', [3] = 'c' } end,\n mixed = function() return { 'a', x = 1 } end,\n}\n",
        );
        let rt = LuauRuntime::new(&dir);
        let call = |f: &str| rt.call_tool(f, &Json::Null, false).content;
        assert_eq!(call("dense"), r#"["a","b"]"#);
        assert_eq!(call("sparse"), r#"{"1":"a","3":"c"}"#);
        assert_eq!(call("mixed"), r#"{"1":"a","x":1}"#);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_long_multibyte_result_is_truncated_at_a_char_boundary() {
        // 'Ω' is 2 bytes; a leading 'x' puts byte 16384 inside one.
        let dir = package("return { big = function() return 'x' .. string.rep('Ω', 9000) end }\n");
        let out = LuauRuntime::new(&dir).call_tool("big", &Json::Null, false);
        assert!(!out.is_error, "{}", out.content);
        let body = out
            .content
            .strip_suffix("\n[result truncated at 16 KiB]")
            .expect("marked");
        assert_eq!(body.len(), MAX_RESULT_BYTES - 1);
        assert!(body.ends_with('Ω'));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn require_resolving_outside_src_through_a_symlink_is_refused() {
        let dir = package("return { peek = function() return require('./leak') end }\n");
        std::fs::create_dir_all(dir.join("tests/data")).unwrap();
        std::fs::write(dir.join("tests/data/secret.luau"), "return 'secret'").unwrap();
        std::os::unix::fs::symlink(
            dir.join("tests/data/secret.luau"),
            dir.join("src/leak.luau"),
        )
        .unwrap();
        let out = LuauRuntime::new(&dir).call_tool("peek", &Json::Null, false);
        assert!(
            out.is_error
                && out
                    .content
                    .contains("module `src/leak.luau` resolves outside the plugin's src/"),
            "{}",
            out.content
        );
        // Under `plugin test`, tests/ is in reach but the outside world isn't.
        let outside = package("return 'far away'\n");
        std::fs::remove_file(dir.join("src/leak.luau")).unwrap();
        std::os::unix::fs::symlink(outside.join("src/main.luau"), dir.join("src/leak.luau"))
            .unwrap();
        std::fs::write(
            dir.join("tests/t.luau"),
            "return function() assert(require('./data/secret') == 'secret') require('../src/leak') end",
        )
        .unwrap();
        let err = test_package(&dir).unwrap_err();
        assert!(
            err.contains("module `src/leak.luau` resolves outside the plugin's src/ and tests/"),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn num_refuses_matrices_over_the_size_cap() {
        let dir = package(
            "local num = require('@harness/num')\nreturn {\n big = function() local r = {} for i = 1, 401 do r[i] = 1 end return num.linsolve({ r }, { 1 }) end,\n ok = function() local r = {} for i = 1, 400 do r[i] = 1 end return #num.mul({ r }, r) end,\n}\n",
        );
        let rt = LuauRuntime::new(&dir);
        let out = rt.call_tool("big", &Json::Null, false);
        assert!(
            out.is_error && out.content.contains("limited to 400 rows and columns"),
            "{}",
            out.content
        );
        assert_eq!(rt.call_tool("ok", &Json::Null, false).content, "1");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn main_module_state_lives_as_long_as_the_vm() {
        let dir = package("local n = 0\nreturn { bump = function() n += 1 return n end }\n");
        let rt = LuauRuntime::new(&dir);
        assert_eq!(rt.call_tool("bump", &Json::Null, false).content, "1");
        // Evaluated once: a later edit on disk doesn't change the running VM.
        std::fs::write(
            dir.join("src/main.luau"),
            "return { bump = function() return 'new' end }",
        )
        .unwrap();
        assert_eq!(rt.call_tool("bump", &Json::Null, false).content, "2");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_test_file_requires_the_package_through_src() {
        let dir = package("return { double = function(n) return 2 * n end }\n");
        std::fs::create_dir_all(dir.join("tests")).unwrap();
        std::fs::write(
            dir.join("tests/t.luau"),
            "local api = require('../src/main')\nreturn { doubles = function() assert(api.double(3) == 6, 'double') end }",
        )
        .unwrap();
        let ran = test_package(&dir).unwrap();
        assert_eq!(ran.len(), 1);
        assert!(ran[0].ends_with("tests/t.luau::doubles"), "{ran:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn golden_compares_in_test_vms() {
        let dir = package("return {}\n");
        std::fs::create_dir_all(dir.join("tests/golden")).unwrap();
        std::fs::write(dir.join("tests/golden/a.svg"), "<svg>\n<g/>\n</svg>").unwrap();
        let run = |body: &str| {
            std::fs::write(
                dir.join("tests/t.luau"),
                format!(
                    "local golden = require('@harness/test').golden\nreturn function() {body} end"
                ),
            )
            .unwrap();
            test_package(&dir)
        };
        assert!(run("golden('a.svg', '<svg>\\n<g/>\\n</svg>')").is_ok());
        let err = run("golden('a.svg', '<svg>\\n<rect/>\\n</svg>')").unwrap_err();
        assert!(
            err.contains("differs at line 2")
                && err.contains("got:  <rect/>")
                && err.contains("want: <g/>"),
            "{err}"
        );
        let err = run("golden('b.svg', 'x')").unwrap_err();
        assert!(
            err.contains("golden file tests/golden/b.svg missing"),
            "{err}"
        );
        let err = run("golden('../t.luau', 'x')").unwrap_err();
        assert!(err.contains("inside tests/golden"), "{err}");
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod artifact_tests {
    use super::*;

    fn package(main: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lh-plugin-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.luau"), main).unwrap();
        dir
    }

    #[test]
    fn an_artifact_tool_returns_text_and_a_nested_svg_artifact() {
        let dir = package(
            "return { draw = function(a)\n return { text = 'two nodes', artifact = { kind = 'svg', svg = '<svg><rect/></svg>', data = { n = a.n } } }\nend }\n",
        );
        let rt = LuauRuntime::new(&dir);
        let out = rt.call_tool("draw", &serde_json::json!({ "n": 2 }), true);
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(out.content, "two nodes");
        let artifact = out.artifact.expect("artifact");
        assert_eq!(artifact.svg, "<svg><rect/></svg>");
        assert_eq!(artifact.data["n"], 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_other_artifact_shape_is_an_error_naming_the_problem() {
        let cases = [
            // The old flat shape is gone, not aliased.
            (
                "{ text = 'x', svg = '<svg/>', data = {} }",
                "has no `artifact`",
            ),
            (
                "{ text = 'x', artifact = '<svg/>' }",
                "`artifact` must be a table",
            ),
            (
                "{ artifact = { svg = '<svg/>' } }",
                "artifact.kind is required",
            ),
            (
                "{ artifact = { kind = 'png', svg = '<svg/>' } }",
                "artifact.kind `png` is not supported",
            ),
            (
                "{ artifact = { kind = 'svg', fallbackSvg = '<svg/>' } }",
                "artifact.svg is required",
            ),
            ("'<svg/>'", "an artifact tool must return {"),
        ];
        for (ret, needle) in cases {
            let dir = package(&format!(
                "return {{ draw = function() return {ret} end }}\n"
            ));
            let out = LuauRuntime::new(&dir).call_tool("draw", &Json::Null, true);
            assert!(out.is_error && out.artifact.is_none(), "{ret}");
            assert!(out.content.contains(needle), "{ret}: {}", out.content);
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn oversized_artifact_data_is_refused_asking_for_less() {
        let dir = package(
            "return { draw = function()\n return { text = 'x', artifact = { kind = 'svg', svg = '<svg/>', data = { blob = string.rep('a', 300 * 1024) } } }\nend }\n",
        );
        let out = LuauRuntime::new(&dir).call_tool("draw", &Json::Null, true);
        assert!(out.is_error && out.artifact.is_none(), "{}", out.content);
        assert!(
            out.content
                .contains("over the 256 KiB limit; ask for less data"),
            "{}",
            out.content
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
