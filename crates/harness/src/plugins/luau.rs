//! Sandboxed Luau plugin runtime (build prompt W5, D6).
//!
//! One VM per plugin, created lazily and reused. Limits: 64 MiB memory, 2 s
//! per tool call, 200 ms per view bridge call, checked by the Luau interrupt.
//! A call that hits a limit is answered as a tool error and the VM is thrown
//! away so the next call starts clean.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

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
    /// `plugin test` VMs also get `host.test` (golden-file compare).
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
        host::install(&lua).map_err(|e| format!("host library: {e}"))?;
        if self.test {
            host::install_test(&lua, &self.dir).map_err(|e| format!("host.test: {e}"))?;
        }
        lua.set_named_registry_value(
            "__plugin_modules",
            lua.create_table().map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let require = make_require(&lua, self.dir.clone()).map_err(|e| format!("require: {e}"))?;
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
                    .set_name("src/main.luau")
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
        self.with_vm(TEST_TIMEOUT_MS, |lua| {
            let value: Value = lua.load(&source).set_name(&name).eval()?;
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

    /// Run a package type check when `luau-analyze` is installed. Returns
    /// `None` when the binary is absent (documented in docs/plugins.md).
    pub fn analyze(&self) -> Option<Result<String, String>> {
        let bin = which("luau-analyze")?;
        let mut cmd = std::process::Command::new(bin);
        cmd.arg("--mode=nonstrict");
        // A package may ship its own definitions; otherwise use the api-1
        // file bundled with the harness (crates/harness/plugin-api/).
        let local = self.dir.join("host.d.luau");
        let shipped = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin-api/host.d.luau");
        if local.is_file() {
            cmd.arg(local);
        } else if shipped.is_file() {
            cmd.arg(shipped);
        }
        for entry in std::fs::read_dir(self.dir.join("src")).ok()?.flatten() {
            if entry.path().extension().is_some_and(|e| e == "luau") {
                cmd.arg(entry.path());
            }
        }
        Some(match cmd.output() {
            Ok(out) if out.status.success() => Ok(String::new()),
            Ok(out) => Err(String::from_utf8_lossy(&out.stderr).into_owned()),
            Err(e) => Err(format!("can't run luau-analyze ({e})")),
        })
    }
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

/// A `require` restricted to the package's `src/` and the host library.
fn make_require(lua: &Lua, dir: PathBuf) -> Result<mlua::Function, mlua::Error> {
    let base = dir.canonicalize().unwrap_or_else(|_| dir.clone());
    let src = base.join("src");
    let src_root = src.canonicalize().unwrap_or_else(|_| src.clone());
    lua.create_function(move |lua, name: String| {
        let name = name.trim();
        if let Some(host_name) = name.strip_prefix("host.") {
            let host: Table = lua.globals().get("host")?;
            return host.get::<Value>(host_name);
        }
        let cache: Table = lua.named_registry_value("__plugin_modules")?;
        if let Some(hit) = cache.get::<Option<Value>>(name)? {
            return Ok(hit);
        }
        if name.contains("..") || name.starts_with('/') || name.contains('\\') {
            return Err(mlua::Error::RuntimeError(format!(
                "require `{name}` is outside the plugin package"
            )));
        }
        let src = &src_root;
        let rel = name.strip_prefix("./").unwrap_or(name);
        let candidates = [
            src.join(format!("{rel}.luau")),
            src.join(rel).join("init.luau"),
            src.join(rel),
        ];
        let Some(path) = candidates.iter().find(|p| p.is_file()) else {
            return Err(mlua::Error::RuntimeError(format!(
                "module `{name}` is not in this plugin's src/"
            )));
        };
        let canonical = path
            .canonicalize()
            .map_err(|_| mlua::Error::RuntimeError(format!("module `{name}` is unreadable")))?;
        if !canonical.starts_with(src) {
            return Err(mlua::Error::RuntimeError(format!(
                "module `{name}` is outside the plugin's src/"
            )));
        }
        let mut source = String::new();
        std::fs::File::open(&canonical)
            .and_then(|mut f| f.read_to_string(&mut source))
            .map_err(|e| mlua::Error::RuntimeError(format!("can't read `{name}` ({e})")))?;
        let value: Value = lua
            .load(&source)
            .set_name(canonical.to_string_lossy().as_ref())
            .eval()?;
        let stored = if matches!(value, Value::Nil) {
            Value::Boolean(true)
        } else {
            value
        };
        cache.set(name, stored.clone())?;
        Ok(stored)
    })
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
            "return {\n up = function() return require('../outside') end,\n abs = function() return require('/etc/passwd') end,\n}\n",
        );
        let rt = LuauRuntime::new(&dir);
        for entry in ["up", "abs"] {
            let out = rt.call_tool(entry, &Json::Null, false);
            assert!(out.is_error, "{entry}: {}", out.content);
            assert!(
                out.content.contains("outside the plugin package"),
                "{entry}: {}",
                out.content
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_host_module_is_reachable_through_require() {
        let dir = package(
            "local num = require('host.num')\nreturn { solve = function()\n  local x = num.linsolve({ { 2 } }, { 6 })\n  return tostring(x[1])\nend }\n",
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
            r#"local num = host.num
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
            "return {\n loop = function() local t = {} t.t = t return t end,\n deep = function() local t = {} for i = 1, 100 do t = { t } end return t end,\n enc = function() local t = {} t[1] = t return host.json.encode(t) end,\n echo = function(a) return a.n end,\n}\n",
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
        let dir = package("return { peek = function() return require('leak') end }\n");
        std::fs::create_dir_all(dir.join("tests")).unwrap();
        std::fs::write(dir.join("tests/secret.luau"), "return 'secret'").unwrap();
        std::os::unix::fs::symlink(dir.join("tests/secret.luau"), dir.join("src/leak.luau"))
            .unwrap();
        let out = LuauRuntime::new(&dir).call_tool("peek", &Json::Null, false);
        assert!(
            out.is_error && out.content.contains("outside the plugin's src/"),
            "{}",
            out.content
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn num_refuses_matrices_over_the_size_cap() {
        let dir = package(
            "return {\n big = function() local r = {} for i = 1, 401 do r[i] = 1 end return host.num.linsolve({ r }, { 1 }) end,\n ok = function() local r = {} for i = 1, 400 do r[i] = 1 end return #host.num.mul({ r }, r) end,\n}\n",
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
    fn golden_compares_only_in_test_vms() {
        let dir = package("return {}\n");
        std::fs::create_dir_all(dir.join("tests/golden")).unwrap();
        std::fs::write(dir.join("tests/golden/a.svg"), "<svg>\n<g/>\n</svg>").unwrap();
        let run = |body: &str| {
            std::fs::write(dir.join("tests/t.luau"), body).unwrap();
            test_package(&dir)
        };
        assert!(
            run("return function() host.test.golden('a.svg', '<svg>\\n<g/>\\n</svg>') end").is_ok()
        );
        let err =
            run("return function() host.test.golden('a.svg', '<svg>\\n<rect/>\\n</svg>') end")
                .unwrap_err();
        assert!(
            err.contains("differs at line 2")
                && err.contains("got:  <rect/>")
                && err.contains("want: <g/>"),
            "{err}"
        );
        let err = run("return function() host.test.golden('b.svg', 'x') end").unwrap_err();
        assert!(
            err.contains("golden file tests/golden/b.svg missing"),
            "{err}"
        );
        let err = run("return function() host.test.golden('../t.luau', 'x') end").unwrap_err();
        assert!(err.contains("inside tests/golden"), "{err}");
        // Not part of the runtime host API.
        std::fs::write(
            dir.join("src/main.luau"),
            "return { has = function() return host.test == nil end }",
        )
        .unwrap();
        assert_eq!(
            LuauRuntime::new(&dir)
                .call_tool("has", &Json::Null, false)
                .content,
            "true"
        );
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
