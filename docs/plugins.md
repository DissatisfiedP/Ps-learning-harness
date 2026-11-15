# Plugins

Status: build document, last revised 2026-11-15. Describes the plugin system as built in `crates/harness/src/plugins/`, the reference package in `plugins/circuit/`, and the host API version 1. Checked against the code on 2026-11-06, and §3, §4, §6.1 and §9 again on 2026-11-15 by running the `plugin` checker; where this file and the code disagree, the code is the authority and this file is the bug.

A plugin is a package (a directory or a `.tar.gz`) that can add any of:

- **tools** the model calls;
- a **renderer** that redraws an artifact from its stored data;
- one **interactive view** (HTML + JS + CSS) the learner manipulates;
- **prompt fragments**, one per agent that may use the plugin.

Plugins are written in **Luau** and run in a sandboxed `mlua` VM with a small pure host library. Native (Rust) plugins use the same manifest shape for system access (programs, network, files); seven are built in (see "Native plugins" below). The design reasons are in `docs/architecture.md` §4.20.

## 1. Package layout

```
<id>/
  plugin.toml          manifest (§2); required
  prompts/<agent>.md   one fragment per agent that uses the plugin; ≤ 250 words
  src/main.luau        entry module; required; must return a table of functions
  src/**/*.luau        the plugin's own modules, loaded with require (§3)
  schemas/*.json       JSON Schema for each tool's input; required per tool
  views/*.html|js|css  optional interactive view (one per plugin)
  tests/*.luau         required by `plugin test`; at least one file
```

- Paths in the manifest are relative to the package root and must name an existing file inside it (`check_file` canonicalises both, so `..` and symlink escapes are refused).
- A `.tar.gz` archive may put `plugin.toml` at its root or inside one top-level directory. Entries with absolute paths or `..` are refused.
- Everything the plugin needs at runtime must be in the package: there is no network, filesystem, clock or process access.
- Plugins have no host globals: the host API is modules required as `@harness/<name>` (§4). Their types live in `crates/harness/plugin-api/` (one type-only `.luau` file per module); `cargo run -p harness --bin plugin -- test` type-checks against them when `luau-analyze` is installed (§9), and an editor (luau-lsp) finds them through a `.luaurc` alias: this repo's root `.luaurc` maps `harness` to `./crates/harness/plugin-api`, and a package kept elsewhere adds `{ "aliases": { "harness": "<relative path to crates/harness/plugin-api>" } }` in a `.luaurc` at or above it (the registry repo does).

## 2. Manifest (`plugin.toml`)

Parsed with the `toml` crate and validated strictly. **Unknown keys are errors**, at every level.

| Key | Type | Constraint |
|---|---|---|
| `id` | string | `[a-z][a-z0-9-]*` (first character a lowercase letter); unique among loaded plugins (a later registration with the same id replaces the earlier one). An id whose tool prefix `<id>_` begins any core tool name (tutor, planner or researcher tools, e.g. `question` → `question_ask`, `file` → `file_read`) is refused naming the collision. An installed copy lives at `<config>/plugins/<id>/<sha256>/`; a folder plugin's directory may have any name |
| `version` | string | semver, e.g. `0.1.0` |
| `api` | integer | must be `1`; the loader refuses any other host API version |
| `title` | string | non-empty; shown in Settings, the planner catalog and the consent screen |
| `description` | string | non-empty; one sentence |
| `license` | string | non-empty (SPDX identifier recommended) |
| `platforms` | array of string | non-empty subset of `desktop`, `mobile` |
| `permissions` | array of string | **must be empty** for a Luau plugin in api 1 (no privileged host modules exist). Defaults to `[]` |
| `use_when` | string | when a section should use the plugin; read by the planner |
| `avoid_when` | string | when it should not; read by the planner |
| `cost` | string | free-form; the planner sees it (the reference package uses `instant`) |
| `[[tool]]` | table array | at most **3** entries; see below |
| `[[renderer]]` | table array | at most one; required if any tool has `artifact = true` |
| `[[view]]` | table array | at most one |
| `[prompts]` | table | agent name → fragment path; defaults to empty. Keys must be `tutor` or `practice` |

`[[tool]]`:

| Key | Type | Constraint |
|---|---|---|
| `name` | string | must start with `<id>_` where `<id>` has `-` replaced by `_` (e.g. id `live-search` → `live_search`) |
| `description` | string | non-empty; this is what the model reads before choosing the tool |
| `agents` | array of string | non-empty; `tutor` or `practice` today (see §2.1). Reserved names are refused |
| `input` | string | path to a JSON Schema file inside the package; the loader also requires it to parse as a JSON **object** |
| `entry` | string | name of a function exported by `src/main.luau` |
| `artifact` | boolean | default `false`; when true the tool returns an artifact (§6) and the plugin must declare a `[[renderer]]` |

`[[renderer]]`:

| Key | Type | Constraint |
|---|---|---|
| `entry` | string | function taking the artifact's `data` and returning an SVG string |

`[[view]]`:

| Key | Type | Constraint |
|---|---|---|
| `name` | string | non-empty; a short view name |
| `html` | string | path to an HTML file that exists in the package |
| `calls` | array of string | Luau functions the view may call through the bridge; defaults to `[]` |
| `answer` | boolean | default `false`; `true` lets the view serve as a `question_ask` answer surface |

`[prompts]`: keys must be agent names (`tutor`, `practice`); each value is a path to an existing file inside the package of at most **250 words** (counted with whitespace splitting; the target is about 200). A missing file, an unknown agent or an over-long fragment fails validation. Fragments are read from disk each time a prompt is composed.

`src/main.luau` must exist. Validation stops at the first problem and names it, e.g. ``tool `x_draw` must start with `bars_` (the plugin id with '-' replaced by '_')`` or `api 2 is not supported (this build implements api 1)`.

### 2.1 Agents

Tools and fragments are scoped per agent:

- `tutor` — the live teaching agent. Plugin tools reach it through the normal tool list, filtered by the section's active plugins.
- `practice` — accepted by the manifest and live: the coach (`crates/harness/src/practice/coach.rs`) receives the `practice`-scoped tools and `prompts/practice.md` fragments of the plugins that were active when a problem's target was taught (the scheduler records them in the problem's plan: at most three, installed, enabled and on this platform).
- `coach`, `figure-maker`, `demo-builder`, `problem-checker` stay **reserved** names: the loader rejects them (`practice` is the manifest's name for the coach; the checker takes no plugin tools).

## 3. Entry module (`src/main.luau`)

`src/main.luau` is evaluated when a function is first needed. It must return a table. Each tool's `entry`, the optional renderer's `entry`, every name in a view's `calls`, and `grade` are looked up on that table by name.

```lua
local function show(args) ... end
local function render(data) ... end

return {
  show = show,
  render = render,
  solve = solve,   -- a view may call this when listed in [[view]].calls
  grade = grade,   -- required only for answer = true
}
```

`src/main.luau` is evaluated once per VM; its exports table is cached and reused by every later tool, renderer, view and `grade` call on that VM, so module-level state in `main.luau` lives as long as the VM. Modules loaded with `require` are cached per VM the same way. Any Lua error (a syntax error, a missing export, a runtime error, a limit hit) becomes a tool error and the VM is thrown away; the next call starts clean and re-reads `src/main.luau`. A folder plugin whose files change gets a new VM (§8.6).

`require` follows Luau's require-by-string rules (the runtime drives Luau's own resolver through mlua, the same one `luau-analyze` uses), so the editor, the type check and the runtime resolve a path identically:

- `require("@harness/json")`, `"@harness/svg"`, `"@harness/num"`, `"@harness/rand"`, `"@harness/log"` return the host module (§4); `"@harness/test"` exists only under `plugin test` (§9) and is otherwise an error (``@harness/test is only available under `plugin test` ``). Any other `@harness/<name>` is an error naming it (``@harness/<name> is not a harness module (json, svg, num, rand, log)``).
- `./x` and `../x` are relative to the **requiring file**: `src/a/b.luau` reaches `src/a/x.luau` with `./x` and `src/x.luau` with `../x`; a test file reaches the entry module with `require("../src/main")`. An `init.luau` file is its folder's module, so from `src/a/init.luau`, `./x` is `src/x` (the folder's sibling) and `@self/x` is `src/a/x`. From any other file, `@self/x` is the file's own child (`src/b.luau` → `src/b/x`).
- A module path `p` is the file `p.luau` or `p/init.luau`; when both `p.luau` and a folder `p/` exist the require is ambiguous and fails. A folder without `init.luau` is only a step on the way. Only `.luau` files load.
- No other forms: a bare name (`require("util")`) or an absolute path is refused (`require path must start with a valid prefix: ./, ../, or @`), and no alias other than `@harness` and `@self` exists at runtime (`@<alias> is not a require alias here: …`); a package's own `.luaurc` is not read.
- Sandbox: a path never steps above the package root (`<file>: a require path can't leave the plugin package`), and the file a module resolves to, after following symlinks, must lie inside the package's `src/` (``module `<path>` resolves outside the plugin's src/``); under `plugin test`, `tests/` is allowed too.
- The result is cached per resolved file for the VM's life, however the path reached it; a module returning nothing is cached as `true`.

Removed globals: `io`, `os`, `debug`, `package`, `dofile`, `loadfile`, `loadstring`, `load`, `collectgarbage`. The VM is built with `StdLib::ALL_SAFE` and `sandbox(true)`. Two globals that remain are not result channels: `print` writes to the host process's standard output (the terminal under `plugin test`, nowhere a learner or the model sees in the app), and `math.random` is seeded differently in every VM, so anything that must be reproducible uses `@harness/rand` (§4.4).

### 3.1 Tool calls

Arguments arrive as the JSON value converted to Lua: objects become tables, arrays become 1-based tables, `null` becomes `nil`, numbers stay numbers. Return values (and `json.encode`, `log.debug`, `artifact.data`, view call and `grade` results) convert back the other way: a table whose keys are exactly the integers `1..n` becomes an array; any other table becomes an object (integer keys written as strings, in sorted order before string keys; other key types are dropped). A table that contains itself, or nesting deeper than 64 tables, is a Lua error ("…refers to itself…", "…nests deeper than 64 tables…"). The return value:

- **Non-artifact tool:** a string is the result. A table with a `text` string uses that; any other table is JSON-encoded; `nil` gives an empty result.
- **Artifact tool (`artifact = true`):** a table `{ text?, artifact = { kind = "svg", svg, data? } }` (§6.1). Any other shape is a tool error that names the problem (each after `plugin error: `): ``the result has no `artifact`; …`` or ``` `artifact` must be a table; … ``` (both followed by the expected shape), `artifact.kind is required ("svg")`, ``artifact.kind `<kind>` is not supported (only "svg")``, `artifact.svg is required (the static SVG figure)`, and for a non-table return the shape alone: `an artifact tool must return { text = "…", artifact = { kind = "svg", svg = "<svg…>", data = {…} } }`.

A Lua error becomes a tool error whose text starts `plugin error:`. The model may retry; the guest cannot crash the app.

The `input` schema is what the model is shown. The core does **not** validate a call's arguments against it (the core's own tools check their arguments in code, and a plugin does the same): a missing field is `nil` and a wrong type is whatever the model sent, so the entry function checks what it reads and raises an error the model can act on. A view's `submit` value and a `question_ask` `target` are likewise passed to `grade` unchecked (§7.4).

An error raised by a host function (§4) is an error object, not a string: `pcall` returns it as userdata, `tostring(err)` is the message followed by a traceback, and `string.find(err, …)` fails. Match on `tostring(err)`.

## 4. Host API, api 1

The host API is five modules, each required by name (there are no host globals): `local json = require("@harness/json")`. They are pure: JSON, SVG building, real/complex arithmetic and a dense solve, a seeded PRNG and a debug log. No I/O, no clock, no randomness from the environment. Their types are `crates/harness/plugin-api/<name>.luau` (§1). The tables below name each module by the local it is usually bound to.

### 4.1 `@harness/json`

| Signature | Result |
|---|---|
| `json.encode(value) -> string` | Compact JSON, by the conversion of §3.1: a table whose keys are exactly the integers `1..n` encodes as an array; any other table encodes as an object, its integer keys written as strings (so `{ [1] = "a", [3] = "c" }` is `{"1":"a","3":"c"}`, and a table with both kinds of key keeps both). An **empty table is `{}`, never `[]`**: a consumer that expects a list (a view reading `data.items`) must accept `{}` for an empty one. Functions and other unsupported values become `null`; NaN and infinities become `null`. Object keys come out sorted |
| `json.decode(text) -> value` | Parses JSON; invalid JSON raises an error (`invalid JSON: …`) |

Examples:

```lua
local json = require("@harness/json")
local log = require("@harness/log")
local payload = json.decode(args.text)
log.debug(json.encode({ ok = true, n = 3 }))  -- {"n":3,"ok":true}
```

### 4.2 `@harness/svg`

Strings are escaped; attributes are emitted in sorted key order so output is deterministic.

| Signature | Notes |
|---|---|
| `svg.document(width, height, content) -> string` | Wraps `content` (not escaped: it is markup) in `<svg xmlns=… width height viewBox="0 0 w h" fill="none" stroke="currentColor">`, so strokes follow the UI theme |
| `svg.group(attrs?, content) -> string` | `<g>` |
| `svg.path(d, attrs?) -> string` | `<path d=…>` |
| `svg.line(x1, y1, x2, y2, attrs?) -> string` | |
| `svg.rect(x, y, w, h, attrs?) -> string` | |
| `svg.circle(cx, cy, r, attrs?) -> string` | |
| `svg.text(x, y, text, attrs?) -> string` | Text is escaped; useful attrs: `text-anchor`, `font-size`, `fill`, `stroke`, `class` |
| `svg.escape(text) -> string` | Escapes `&`, `<`, `>` |

Attribute values may be strings, numbers or booleans; a value of any other type is skipped. An attribute whose name starts with `on` in any letter case raises an error (a prefix match, so `only` is refused too); the builder accepts `href` and `xlink:href` only when they start with `#` or `data:image/`. The core sanitizer (§6.2) then applies its own rules to the finished figure, which are stricter: it keeps a link only for `#…` or `data:image/png|jpeg|gif|webp;base64,…`, so anything else the builder let through (e.g. `data:image/svg+xml`) is dropped before storage, and a plugin cannot smuggle a script.

Numbers print with the shortest digits that round-trip and no exponent (`10 / 3` is `3.3333333333333335`; NaN and infinities print as `NaN` and `inf`, which no renderer accepts), so round coordinates before drawing. An attribute in `attrs` that repeats one the function sets itself (`x` for `svg.rect`, `d` for `svg.path`) is emitted twice; the sanitizer keeps the first, which is the one from `attrs`. There is no builder for `polygon`, `polyline`, `ellipse`, `tspan`, `defs` and the like: write them as strings and pass their text through `svg.escape`.

```lua
local svg = require("@harness/svg")
local parts = {
  svg.rect(0, 0, 40, 12, { fill = "currentColor", ["fill-opacity"] = "0.2" }),
  svg.text(44, 10, "R₁ = 1 kΩ"),
}
local figure = svg.document(200, 40, table.concat(parts))
```

### 4.3 `@harness/num`

Every `Number` below is a plain Lua number (real part; imaginary part 0) or a **complex** `{ re = <number>, im = <number> }`. Functions return a plain number when the imaginary part is below `1e-12`, otherwise a complex table.

| Signature | Notes |
|---|---|
| `num.c(re, im?) -> Number` | `im` defaults to 0 |
| `num.add(a, b) -> Number` / `sub` / `div` | No error for division by zero: `num.div(x, 0)` is a complex table whose parts are both NaN, for a real `x` too |
| `num.mul(a, b) -> Number \| Vector \| Matrix` | number × number (real or complex), matrix × flat vector → vector, or matrix × matrix → matrix. A matrix is a table of rows; a flat vector is a table of numbers. A ragged matrix, a number times a matrix or mismatched shapes raise `mul: …` with the problem named |
| `num.abs(a) -> number` | magnitude |
| `num.phaseDeg(a) -> number` | argument in degrees |
| `num.conj(a) -> Number` | |
| `num.real(a) -> number` / `imag(a) -> number` | |
| `num.linsolve(a, b) -> Vector` | `a` is an `n×n` matrix (rows of `Number`), `b` an `n` vector; partial-pivoting LU. Shapes must match; a singular matrix raises "the matrix is singular (no unique solution)" |

Pass a table to these functions only as a complex number (`{re, im}`) or as a matrix/vector where one is expected. A table without a numeric `re` is not a number: it raises `add expects numbers` (and the like for the other functions); `im` alone defaults to 0. Because a result collapses to a plain number when its imaginary part is below `1e-12`, code that handles results must accept both forms (`num.real`, `num.imag` and `num.abs` take either).

```lua
local num = require("@harness/num")
local jw = num.mul(num.c(0, 1), num.c(2 * math.pi * 50))   -- jω
local zc = num.div(num.c(1), num.mul(jw, num.c(1e-6)))     -- 1/(jωC)
local z = num.add(num.c(1000), zc)                          -- R + 1/(jωC)
local magnitude = num.abs(z)
```

### 4.4 `@harness/rand`

| Signature | Notes |
|---|---|
| `rand.seeded(seed) -> Rng` | Deterministic xorshift64; the seed comes from the caller, never the clock. It must be a whole number from 0 to 2^64 − 1 (a negative, fractional or larger one is an argument error); `0` is treated as `1` |
| `rng.next() -> number` | Uniform in `[0, 1)` |
| `rng.range(lo, hi) -> number` | Uniform real in `[lo, hi)` |
| `rng.int(lo, hi) -> integer` | Integer in `[lo, hi]`, inclusive, for any two 64-bit integers (modulo reduction, so very slightly biased for huge spans); `lo` when `hi < lo` |

Call the generator's functions with `.` (`rng.int(1, 6)`), not `:`; a method call passes the table as the first argument and `range`/`int` then fail.

Use a seed derived from the task (for example the problem number) when a figure must be reproducible across replays and devices.

### 4.5 `@harness/log`

`log.debug(value) -> ()` converts `value` to JSON (§3.1) and emits it as one `debug` event on the core's `tracing` log with target `plugin`. It is never shown to the model or the learner; use it for development, not as a result channel. Where it goes depends on the binary (`harness::logging::init`, events to stderr): `plugin test` shows debug events, so a package's `log.debug` lines appear in the terminal as `<time> DEBUG plugin: <json>`; the app shows `info` and above, so they appear only when it is started with `PS_LEARN_LOG=debug`.

## 5. Limits

All enforced by the host, not the plugin.

| Limit | Value | Where |
|---|---|---|
| Tools per plugin | 3 | `MAX_TOOLS_PER_PLUGIN` |
| Fragment words per agent | 250 (target about 200) | `MAX_FRAGMENT_WORDS` |
| Plugins per plan section | 3 | `MAX_PLUGINS_PER_SECTION` |
| Luau memory per plugin instance | 64 MiB | `MEMORY_LIMIT` |
| Wall time per tool call | 2 000 ms | `TOOL_TIMEOUT_MS` |
| Wall time per view bridge call | 200 ms | `VIEW_TIMEOUT_MS` |
| Tool result text | 16 KiB; longer text is cut at a character boundary and marked `[result truncated at 16 KiB]` | `MAX_RESULT_BYTES` |
| Artifact `data`, serialized as JSON | 256 KiB; more is a tool error (`artifact.data is <n> KiB, over the 256 KiB limit; ask for less data …`) | `MAX_ARTIFACT_DATA_BYTES` |
| Matrix/vector side in `num.mul` / `linsolve` (`@harness/num`) | 400; larger is a Lua error | `host::MAX_DIM` |
| Fallback SVG per artifact | 256 KiB | `MAX_SVG_BYTES` |
| Wall time per test file | 10 000 ms | `TEST_TIMEOUT_MS` |

A call that exceeds memory or time, or raises any other Lua error, returns a tool error and the VM is discarded; the next call builds a fresh VM. A plugin gets one VM, created lazily and shared by its tool calls, renderer, view calls and `grade`. The time limit is measured on a monotonic clock from the start of the call and checked by the Luau interrupt; a limit hit reads `plugin time limit (2000 ms) exceeded` or `plugin memory limit: …`. Host functions are Rust and not interruptible, which is why `@harness/num` caps matrix sizes.

## 6. Artifacts

### 6.1 Contract

A tool with `artifact = true` returns:

```lua
return {
  text = "Solved: Vout = 3 V.",                 -- optional; the model reads this
  artifact = {
    kind = "svg",                               -- required; the only kind in api 1
    svg = figure,                               -- required; the static fallback
    data = { components = ..., result = ... },  -- optional; stored, and given to the view
  },
}
```

There is no other accepted shape: a flat `{ text, svg, data }` table is an error.

The core stores an `artifact` op:

```json
{
  "op": "artifact", "opId": "01J…", "at": "2026-11-06T10:00:00.000Z",
  "artifactId": "01J…", "pluginId": "circuit", "pluginVersion": "0.1.0",
  "packageHash": "<sha256 of the package>", "tool": "circuit_draw",
  "input": { "components": [] }, "data": { "result": { "probes": { "Vout": 3 } } },
  "fallbackSvg": "<svg …>"
}
```

- `data` is whatever the view needs to work: the circuit plugin stores the components, the analysis, what the figure shows, its annotations and the learner-safe part of the solved result. It is JSON by the conversion of §3.1, so an empty list in it is `{}`, and its numbers read back exactly as stored.
- The tool result the model sees is its `text`, a blank line (omitted when `text` is empty), then the placement line from `artifacts::placement_line`:
  `artifact: 01J… — place it once with ![a short description](artifact:01J…) next to the sentence that refers to it`
- `input` is the tool call's arguments, verbatim; `packageHash` is the content hash of the package directory (§8.3).
- The model places the artifact with Markdown image syntax `![alt](artifact:<id>)`. The alt text is the figure's accessible name, so it must describe the figure.
- An artifact created in a turn but never referenced is appended after that turn's message, in creation order.
- An unknown id renders as the alt text in brackets. A plugin that is uninstalled later still shows its artifact: the fallback SVG is in the op, and **replay never re-runs a plugin**.

The renderer (`[[renderer]].entry`) takes the artifact's stored `data` and returns an SVG string. The manifest requires it for every artifact tool, but the app itself never calls it (`LuauPlugin::render` has no caller): the stored fallback is always the `artifact.svg` the tool returned, so replay stays exact. It is called only when a view lists it in `[[view]].calls`, as the `circuit` view does to redraw its schematic after a slider move, and by the package's own tests.

### 6.2 Sanitizing

The fallback is sanitized in the core before it is stored (`crates/harness/src/artifacts.rs`). The rules:

- at most 256 KiB, else `the fallback SVG is <n> KiB (limit 256 KiB); ask for a simpler figure`; it must be well-formed XML (`the SVG is not well-formed XML (…)`) and contain `<svg` (`the fallback is not an SVG document`). Any of these makes the tool call an error;
- element names are matched case-insensitively on their local name (a `svg:` prefix does not hide a `script`);
- removed elements: `script`, `foreignObject`, `iframe`, `object`, `embed`, `audio`, `video`, `animate`, `animateTransform`, `animateMotion`, `set`, `discard`, `style` (with their contents);
- removed attributes: anything starting with `on`, and `style` (draw with presentation attributes);
- `href`/`xlink:href` are kept only for `#fragment` or `data:image/png|jpeg|gif|webp;base64,…` (so `data:image/svg+xml` is dropped, and an `image`/`use` with any other link loses it); any attribute whose (entity-decoded, case-insensitive) value contains `javascript:`, a backslash, `image-set(`, `image(`, `src(`, `cross-fade(`, `element(`, or a `url(` that does not point at `#…`, is dropped;
- comments, CDATA, the XML declaration, processing instructions and the doctype are dropped; the five XML entities and numeric references are decoded and re-escaped, every other entity is dropped. Attribute values are re-escaped on output.

The UI sanitizes again with DOMPurify's SVG profile plus the same element, attribute, link and `url(` rules (`ui/artifacts.js`) before inserting the figure.

## 7. Views

### 7.1 Package side

One view per plugin: an HTML document that follows the bridge protocol below. Put any JS and CSS in the package; the loader inlines every `<link … href=…>` and every `<script src=…></script>` whose file it can read into the page (`<style>` / `<script>`), so it works under the view's CSP. `href`/`src` are resolved **relative to the package root**, not to the HTML file's folder (a leading `./` is ignored); a reference that cannot be read is left as written and will not load. The HTML (and its inlined assets) is re-read each time the view is opened.

Views are optional. Every artifact still has its static SVG fallback, so a replay, a phone without views, a screen reader or a plugin the learner removed still shows something.

### 7.2 Host side

The UI half is specified in `docs/ui-design.md` §7.2.15 (figures and views) and §7.2.17 (the view document's CSP). The plugin-facing contract (message names, the `calls` allow-list, the 200 ms budget, the 500-character state cap) is enforced by the core.

**How the page is loaded.** The UI creates `<iframe sandbox="allow-scripts">` (**never** `allow-same-origin`, so the frame has an opaque origin) and points it at the shell's `pluginview` URI scheme: `pluginview://localhost/<id>` on Linux and macOS, `http://pluginview.localhost/<id>` on Windows and Android (Tauri's naming for custom schemes on those platforms). The scheme handler in `crates/app/src/lib.rs` serves the inlined view HTML with its own response header:

```
Content-Security-Policy: sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; font-src data:; connect-src 'none'; base-uri 'none'; form-action 'none'
```

The CSP `sandbox` directive keeps the document opaque-origin even if it were ever loaded outside the sandboxed iframe. The app CSP allows these frames with `frame-src pluginview: http://pluginview.localhost`.

**Why not `srcdoc`.** The work order asked for `srcdoc` with the view CSP. A `srcdoc` document inherits the embedding page's CSP, and the app CSP has `script-src 'self'`, so the view's inline script never runs. Measured in headless Chromium 155 on 2026-11-06: under the app CSP a sandboxed `srcdoc` frame whose own `<meta>` CSP allows `'unsafe-inline'` did not run its script; the same page served from a separate origin with the view CSP header did; with the app CSP removed, the `srcdoc` frame ran too (so the inherited policy is the cause). Loosening the app CSP to `'unsafe-inline'` would weaken the main window, so views get their own document instead.

**IPC isolation.** Tauri IPC must be unreachable from inside the frame; that is a tested requirement, not an assumption. The probe is committed: `plugins/fixtures/ipc-probe` is a package whose view tries every way out we know of, and `scripts/ui-check/run.sh ipc` runs it in headless Chromium (Bun and Chromium from the flake's pinned nixpkgs; no app build needed). The runner serves `ui/` with the app CSP from `tauri.conf.json` as `http://tauri.localhost`, the view with `VIEW_CSP` from `crates/app/src/lib.rs` as `http://pluginview.localhost/ipc-probe` (Tauri's Windows/Android origins; the `pluginview` scheme itself exists only inside Tauri), and `http://ipc.localhost`, all through one recording server. The probe view opens through the real view host (`ui/artifacts.js`, `ui/viewhost.js`) in a page with a fake bridge (`scripts/ui-check/fake-tauri.js`) that defines `__TAURI__`, `__TAURI_INTERNALS__`, `window.ipc` and `window.webkit.messageHandlers.ipc` to steal, and every IPC attempt carries the destructive `remove_plugin` command. Its 49 attempts: the bridge globals in the frame (`__TAURI__`, `__TAURI_INTERNALS__`, `__TAURI_IPC__`, `__TAURI_POST_MESSAGE__`, `isTauri`, `window.ipc.postMessage`, `window.webkit.messageHandlers.ipc.postMessage`, `window.chrome.webview.postMessage`); the same through `window.parent`, plus `parent.document`, `parent.localStorage`, `parent.location`, `top.location`, `frameElement` and a forged Tauri-shaped `postMessage`; bridge `call`s outside `calls`; the frame's own storage (`localStorage`, `sessionStorage`, `document.cookie`, IndexedDB, Cache Storage); `fetch` to `tauri://localhost`, `http://tauri.localhost`, `ipc://localhost`, `http://ipc.localhost`, its own origin and an `https` site, `XMLHttpRequest`, `WebSocket`, `EventSource`, an `<img>`, a `<script>`, dynamic `import()`, a `Worker`, `sendBeacon`, a CSS `url()`, `<link rel=prefetch>` and a nested iframe; `eval`, `new Function` and `setTimeout` with a string; `window.open`, top navigation, a `target=_top` link, a form post, and finally navigating the frame itself to the app origin. The run fails if any attempt reports an escape, if any request reaches the server's leak paths or the IPC origin, if the bridge sees a command the view host doesn't send, if the app page navigates, or if a window opens; a control request from the app page itself must reach the IPC origin, so the server would have seen a leak. Result on 2026-11-07, Chromium 156: 0 of 49 escaped, 0 requests leaked (`sendBeacon`, the CSS `url()`, prefetch, nested iframe, link and form report "sent" and nothing arrived). With `VIEW_CSP` weakened to `sandbox allow-scripts` the same run reports 9 escapes and 13 leaked requests, so the check can fail. In the Tauri 2.12.5 source the IPC initialisation scripts, which carry the invoke key added for GHSA-57fm-592m-34r7 / CVE-2024-35222, are registered with `for_main_frame_only: true` (`src/manager/webview.rs`), so a frame never receives them. **WebKitGTK (Linux) and the mobile webviews still need a manual run:** add `plugins/fixtures/ipc-probe` with **Settings → Plugins → Add folder plugin…**, start a Learn session whose section lists `ipc-probe` (or grant it with `plugin_request`), and ask the tutor for the probe. The view lists every attempt; its `state` line (`ipc-probe: 49 attempts, 0 escaped`) lands in the session log as a `viewState` op with the learner's next message. Rows marked "sent" need the Web Inspector's Network panel (`WEBKIT_INSPECTOR_SERVER` or the dev build's inspector) to confirm no request left. Not yet done (`docs/agent-system-plan.md`, Known gaps).

The view and host exchange JSON messages with `postMessage`. Every message carries `protocol: 1`. The host ignores messages whose `event.source` is not that iframe, that are not objects, or whose `protocol` is not `1`. A view should likewise ignore messages whose `event.source` is not `parent`.

| Direction | Message | Meaning |
|---|---|---|
| host → view | `{ protocol: 1, type: "init", artifact: { artifactId, data }, theme: { <token>: <value> }, answer: boolean }` | Sent on the iframe's `load` event. `data` is the stored artifact data (`{}` when absent). `theme` carries the UI's CSS custom properties by name (`paper`, `raised`, `ink`, `ink-2`, `ink-3`, `rule`, `rule-strong`, `accent`, `accent-hover`, `on-accent`, `danger`, `select`, `font-ui`) so the view can set `--<name>`. `answer` is true only for an answer surface (§7.4) |
| host → view | `{ protocol: 1, type: "theme", theme }` | The learner switched between light and dark while the view was open; same token map as `init` |
| view → host | `{ protocol: 1, type: "call", id, fn, args }` | Call a Luau export. `id` is a number or string, `fn` a string; anything else is ignored. `fn` must be in `[[view]].calls`: the UI answers other names with an error without calling the core, and the core refuses them again ("`<fn>` is not one of the view's callable functions"). Calls are capped at 200 ms. One call per frame is in flight at a time: a `call` sent while another is pending is answered at once with `{ ok: false, error: "busy" }`, so wait for each `result` before sending the next |
| host → view | `{ protocol: 1, type: "result", id, ok: true, value }` or `{ … ok: false, error }` | Answer to `call`; `value` is the Luau return value as JSON |
| view → host | `{ protocol: 1, type: "resize", height }` | Ask the host to set the iframe height in px; clamped to 160 px … 80 % of the window height |
| view → host | `{ protocol: 1, type: "state", summary }` | A short summary of what the learner changed. The UI cuts it to 500 characters, sends at most one every 250 ms, and records it with the `view_state` command. The core refuses an artifact id that isn't in the session and a session that can't be written, turns control characters and line breaks into spaces, cuts to 500 characters again, and holds only the latest summary per artifact. When the learner sends their next message, those summaries are stored as `viewState` ops just before it (§7.3); other interaction is not logged |
| view → host | `{ protocol: 1, type: "submit", answer }` | Answer mode only (ignored otherwise): the learner's submission (§7.4). After a submit the frame is inert: the host drops its further `call`, `state` and `submit` messages (only `resize` still works); a failed send makes it live again |
| view → host | `{ protocol: 1, type: "error", message }` | The view failed to start. Views don't send this themselves: the core puts a guard script first in every view document (`crates/harness/src/plugins/view_guard.js`), and it posts `error` for an uncaught exception thrown while the document loads or while its `init` listeners run. The host removes the frame and shows the artifact's fallback SVG again (in answer mode with a line saying the view failed). Exceptions after that, and rejected promises, are left to the view |

The host exposes the calls as the `plugin_view_call { pluginId, function, args }` command; `plugin_view { pluginId }` returns `{ name, calls, answer }` (metadata only), and `plugin_view_url { pluginId }` returns the URL the iframe loads. The inlined view document is served directly by the `pluginview` scheme response, not returned to the UI. The view must not animate on its own; follow `prefers-reduced-motion`, and apply the theme tokens so the figure reads in light and dark. Send `state` only after the learner changed something, never on load: opening a session would otherwise log a line for every figure on screen.

A minimal client loop:

```html
<script>
(() => {
  const pending = new Map();
  let nextId = 1;
  const send = (m) => { m.protocol = 1; parent.postMessage(m, '*'); };
  const call = (fn, args) => new Promise((resolve, reject) => {
    const id = nextId++;
    pending.set(id, { resolve, reject });
    send({ type: 'call', id, fn, args });
  });
  const theme = (t) => {
    for (const [k, v] of Object.entries(t || {})) document.documentElement.style.setProperty('--' + k, v);
  };
  window.addEventListener('message', (event) => {
    const m = event.data;
    if (event.source !== parent || !m || typeof m !== 'object' || m.protocol !== 1) return;
    if (m.type === 'init') {
      theme(m.theme);
      render(m.artifact.data);
    }
    if (m.type === 'theme') theme(m.theme);
    if (m.type === 'result') {
      const p = pending.get(m.id);
      if (!p) return;
      pending.delete(m.id);
      m.ok ? p.resolve(m.value) : p.reject(new Error(m.error));
    }
  });
  send({ type: 'resize', height: document.body.scrollHeight });
})();
</script>
```

### 7.3 `state`

`state` is how an interactive figure contributes to teaching without logging every slider move. Post it when the learner changes something meaningful, not on every frame. Example: `send({ type: 'state', summary: 'R2 = 2.2 kΩ, Vout = 3.4 V' })`.

The tutor sees the summaries with the learner's next message, as one line per artifact (sorted by artifact id) after a blank line: `[view state <artifactId>] <summary>`. The lines are added when the provider request is built from the `viewState` ops; the stored message and the learner's note in the timeline hold only what they typed. Summaries posted before the app quits and with no message after them are dropped.

**[learning]** Simulations improve on traditional instruction when the learner is guided in what to manipulate and the exploration is tied back to the lesson (Rutten, van Joolingen & van der Veen 2012, doi:10.1016/j.compedu.2011.07.017). The summary lets the tutor build on what the learner just tried instead of guessing; sending only the latest one per artifact keeps that context short **[inference]**.

### 7.4 Answer mode and `grade`

Set `answer = true` in the `[[view]]` table to make the view an answer surface. The tutor then asks:

```json
{
  "role": "check", "nodeId": "divider-rule", "format": "free",
  "prompt": "Set R2 so that Vout is 3 V.",
  "answer": "R2 = 1 kΩ (checked in the figure)",
  "view": {
    "artifactId": "01J…",
    "task": "Move R2 until Vout = 3 V, then submit.",
    "target": { "probe": "Vout", "equals": 3.0, "tolerance": 0.05 }
  }
}
```

- `target` is committed in the `question_ask` call op and is **never shown to the learner**.
- The call is refused unless `view` is an object with a string `artifactId`, a non-empty `task` and an object `target`; the artifact exists in this session; its plugin is installed **and enabled** and active in the current section; and that plugin's view has `answer = true`. Refusals: "question_ask refused: the plugin that made `<artifactId>` is disabled; ask without the view", "… isn't active in this section; ask without the view". If the plugin is disabled after the question was asked, submitting answers with "the plugin that made this view is disabled; enable it in Settings → Plugins to submit this answer" and the question stays pending.
- The learner works in the view and presses submit; the view posts `{ type: "submit", answer }`; the UI sends the `answer` command with payload `{ answer: <that value> }`.
- Grading calls the plugin's exported `grade(answer, target)` with `answer` = the view's `submit` value exactly as the view sent it, and `target` from the call op. The tool result the tutor gets is `{ questionId, artifactId, answer, view: <grade result> }` (none of the free-answer fields such as `confidence`). A grading error is returned to the UI as `the plugin couldn't grade the answer: …` and the question stays open.
- The plugin's result should be `{ correct = <boolean>, detail = "<one line for the tutor>" }`; the core passes whatever it returns through unchanged. Only the tutor sees it (the UI shows "Answer submitted."), because `detail` may name the target value; the learner hears the outcome from the tutor.
- **The plugin never records a verdict.** The tutor still calls `verdict_record` with the same `questionId`, exactly as for a free-recall answer.

**[learning]** Answering in the figure is still retrieval practice: the learner produces an answer and commits to it before any feedback, and retrieval practice improves retention over restudying (Roediger & Karpicke 2006, doi:10.1111/j.1467-9280.2006.01693.x). The target is never sent to the view, so the view cannot give it away. Keeping the verdict with the tutor gives typed and manipulated answers one grading policy **[inference]**.

## 8. Registries, install, lock

### 8.1 Registry index

A registry is an HTTPS URL (or a `file://` URL, or an absolute local path) serving one `index.json`:

```json
{
  "registry": 1,
  "name": "Example registry",
  "plugins": [
    {
      "id": "bars",
      "title": "Bar chart",
      "description": "Draws a labelled bar chart from a list of values.",
      "use_when": "The learner must compare sizes at a glance.",
      "avoid_when": "Two numbers would do.",
      "platforms": ["desktop", "mobile"],
      "permissions": [],
      "versions": [
        {
          "version": "0.1.0",
          "api": 1,
          "url": "https://example.org/plugins/bars-0.1.0.tar.gz",
          "sha256": "<hex sha256 of the .tar.gz>",
          "published": "2026-11-06"
        }
      ]
    }
  ]
}
```

- `registry` must be `1`; other values are refused (`registry format <n> is not supported (this build reads 1)`).
- Keys are snake_case where they have two words (`use_when`, `avoid_when`), matching `plugin.toml`. `id` and `title` are required per plugin; `description`, `use_when`, `avoid_when`, `platforms`, `permissions` and `versions` default to empty; `published` is optional. Each version needs `version`, `api`, `url` and `sha256`. The index's `api` is informational: install validates the unpacked manifest's `api` instead.
- `url` may be `https://`, `http://`, `file://` or a local path. `sha256` is the hex digest of the archive bytes (compared case-insensitively). An index whose plugin `id` is not a valid plugin id (§2), or whose `sha256` is not 64 hex digits, is refused: both become path segments under `<config>/plugins/`.
- Indexes are cached at `<config>/registry-cache/<sha256 of the URL>.json`. Install reads the cache and fetches only when there is no parseable cached copy; `plugin_search`, the Settings browser and the consent screen read only the cache. "Refresh registries" fetches every configured registry, rewrites each cache that parses, and reports `{ name, url, ok, error, fetchedAt, plugins }` per registry.
- **The default registry** is `https://dissatisfiedp.github.io/Ps-learning-harness-plugins/index.json` (`install::DEFAULT_REGISTRY_URL`), listed first on every device. It is built in, never written to `settings.json`: removing it sets `hideDefaultRegistry: true`, and adding its URL again clears that. The learner's own registries are device-local `settings.json` → `registries: [{ name, url }]`; adding refuses any URL that is not `https://`, `file://` or an absolute path. `registries` returns `{ name, url, builtin }`, and Settings tags the built-in row "default".
- **At startup** the app fetches every listed registry whose cached index is missing or more than a day old (`refresh_stale_registries`, in the background; a failure is logged and the old cache kept), so the browser and `plugin_search` see the default registry without the learner pressing Refresh.
- **The default registry's source** is a separate repository, [`DissatisfiedP/Ps-learning-harness-plugins`](https://github.com/DissatisfiedP/Ps-learning-harness-plugins) (checked out beside this one): plugin sources under `plugins/<id>/`, packed reproducibly by its GitHub Actions into Release assets, with `index.json` deployed to GitHub Pages. Its README covers publishing. `plugins/circuit/` here is the app's test fixture; the registry holds its own copy, which is the published one.

### 8.2 Install

1. The learner picks a plugin version from the Settings browser (or the consent screen opened from a plan chip); the UI calls `install_plugin { registry, id, version }`.
2. The archive is downloaded (at most 16 MiB) and its sha256 compared with the index; a mismatch is refused with both digests in the message (`…; refusing to install`). `install_plugin` and `remove_plugin` refuse an id that is not a valid plugin id.
3. The `.tar.gz` is unpacked to `<config>/plugins/<id>/<sha256>/` (the hash is lower-cased); one entry may unpack to at most 16 MiB and the whole package to 64 MiB. A previous copy at that path is replaced. Unpacking, validation and hashing run off the async runtime.
4. The unpacked package is validated by the manifest loader; the manifest's `id` and `version` must match what was requested, or the copy is removed and the install fails.
5. The plugin is registered (replacing a loaded plugin with the same id), and `plugins.lock` gets the entry. Enabled state is kept: a plugin the learner disabled stays disabled after an install or update.

Several versions of one id can sit side by side under `<config>/plugins/<id>/`. At startup the loader loads the copy whose directory name matches the sha256 in `plugins.lock` (falling back to the last directory by name only when that copy is missing). A failed validation or unpack leaves no copy behind. If a cached index doesn't list the requested id/version, install refetches it once before failing.

### 8.3 `plugins.lock`

In the data repo, so every device sees what the learner chose:

```json
[
  {
    "id": "circuit",
    "version": "0.1.0",
    "sha256": "<sha256 from the index, as written there>",
    "source": { "registry": "https://example.org/index.json" }
  },
  {
    "id": "bars",
    "version": "0.1.0",
    "sha256": "<package hash of the folder>",
    "source": { "folder": "bars" }
  }
]
```

- Sorted by `id`; written on install, folder addition and removal. The core exposes `missing_locked`: registry entries whose locked `<id>/<sha256>/` copy is not on this device (even if another version of the id loaded), and folder entries whose id is not loaded; the UI uses it to offer the consent screen again.
- A `plugins.lock` that exists but doesn't parse is an error for install, folder add, remove and `missing_locked` (never treated as empty and overwritten); startup loads plugins without it and logs a warning.
- For a folder plugin, `sha256` is the package hash: SHA-256 over every file's relative path, its length and its bytes, in sorted path order (the same hash stored as an artifact's `packageHash`).
- A folder plugin cannot be fetched from another device: those devices show the artifact fallbacks, and approval leaves the plugin out of sections (with a plan notice) until it is installed.

### 8.4 Consent and permissions

Installing runs third-party code, so it needs explicit consent. The consent screen is UI (`docs/ui-design.md`); the core supplies its data through `install_candidate { id }` (`{ registry, plugin, versions }` from the cached indexes). It shows:

- title, description, version, source (registry URL or folder name);
- the permissions in plain words. For a Luau api 1 plugin `permissions` is empty, so it reads **"No access to your files, network or programs."**

There are no automatic updates and no separate update command: an update is installing another version through `install_plugin`. It is refused while any unfinished session is in Teach with that plugin active in its current section ("`<id>` is in use in the current section of the session “<title>”; update it after that section ends"), checked before the download and again before the swap. A running session therefore keeps the version it was granted until its next section starts (build prompt W8). Otherwise the new package replaces the loaded one. Removing or disabling a plugin never removes its artifacts from sessions; those keep their fallback SVGs.

Native plugins may declare `exec`, `net`, `fs-read`, `fs-write`. The field is validated as empty for Luau plugins today because no privileged host module exists. A built-in native that declares permissions is consented to by enabling it in Settings, on this same screen (§12 *Consent and the Settings row*).

### 8.5 Enable, disable, remove

- **Disable** keeps the package installed but removes it from the planner catalog, from approval grants and `plugin_request`, and from section tools and fragments. The choice is device-local (`settings.json` → `disabledPlugins`). The core also refuses a disabled plugin's `plugin_view` and `plugin_view_call` ("plugin `<id>` is disabled; enable it in Settings → Plugins") and its answer-mode checks.
- **Enable** reverses it. Enabling a built-in that declares permissions is the consent to them: Settings shows the consent screen first and the core records it in `enabledPlugins` (§12). A built-in cannot be removed, only disabled.
- **Remove** deletes `<config>/plugins/<id>/` (every installed copy), drops the id from `pluginFolders`, `disabledPlugins` and `plugins.lock`, and unloads it. Artifacts already in sessions still display.

### 8.6 Folder plugins (development)

`Add folder plugin…` (`add_folder_plugin { path }`) points at a package directory. The path is canonicalised; the package is validated and loaded immediately, recorded in `settings.json` → `pluginFolders: [{ id, path }]` and in `plugins.lock` with `source: { folder: <id> }`. A folder plugin wins over an installed copy with the same id. There is no background file watcher: instead the registry fingerprints the package's files (modification time and size, excluding `tests/`) and reloads manifest, schemas, fragments and the Luau VM when that fingerprint changes, at the point the plugin is next handed out. A failed reload keeps the last good package and reports its error as `loadError` in `plugins()`; editing a `require`d module therefore also takes effect without a host restart.

### 8.7 Install offers

A session can want a plugin that this device can't run yet: a registry plugin that isn't installed, or a built-in that waits for consent (§12). The planner listed it on a section (§8.1's index, or the built-in's manifest, made it nameable), or the tutor asked for it with `plugin_request` after a `plugin_search` (the planner and the measuring tutor both have the search, which returns both kinds). The app then asks the learner, with the consent screen of §8.4, at the moment the plugin would be used:

- **When the tutor asks** (while measuring, or in a section): the call writes a `pluginOffer` op (`state: "offered"`, with the tutor's reason) and its result tells the tutor to say what the tool is for and end its turn.
- **When a section starts** whose plan lists the plugin and the learner didn't untick it at approval. No op is needed: the plan already says it.

`session_state` computes `installOffers` for the device: the plugins wanted where the session is now that were not declined and that it could come to run, each with an `action`: `install` (listed by a cached registry index for this platform, no copy installed) or `enable` (a built-in with permissions that is off and was never turned off by the learner). The UI opens the consent screen for each (the install screen, or the built-in's "Enable ⟨Title⟩?" screen with its permissions), and reports the decision with `resolve_install_offer { sessionId, plugin, installed }`:

- **installed** (or, for a built-in, allowed): the plugin is granted for the section (or for measuring, under the section id `(measure)`), and the tutor gets `[plan update] The learner installed …` (`allowed …`) and a turn. Its tools and prompt fragment are in that turn.
- **declined**: a `pluginOffer` op with `state: "declined"`. The offer is not made again for that section and plugin, a repeated `plugin_request` for it is refused, and the tutor gets `[plan update] The learner chose not to install …` (`not to allow …`).

With a question open the tutor is not interrupted: the grant or refusal is recorded and its next prompt shows it. A plugin that becomes usable some other way (Settings, another section's offer) is granted at the start of the next tutor turn in a section that wants it. A plugin the learner turned off in Settings is never offered: that choice stands until they turn it back on there. On the plan card a built-in waiting for consent shows as "⟨Title⟩ — allow" where a registry plugin shows "— install".

## 9. Testing a package

```
cargo run -p harness --bin plugin -- validate <dir>
cargo run -p harness --bin plugin -- test <dir>
```

`validate` parses the manifest (every check in §2, stopping at the first problem) and checks that every `input` schema parses as a JSON object; it prints `ok: <id> <version>`. Errors go to stderr with exit code 1; a wrong command line prints the usage and exits 2.

`test` runs `validate`, then:

- a Luau type check when a `luau-analyze` binary is on `PATH`: every `.luau` file under `src/` and `tests/` (subfolders included), in luau-analyze's default mode (nonstrict; a file opts into `--!strict` itself) and default (new) type solver. luau-analyze resolves `@harness/*` through a `.luaurc` above the requiring file, so the check runs on a copy of those files in a temporary folder next to the bundled type modules and a generated `.luaurc` (`harness` → the type modules); it works the same for a package anywhere on disk, and reports paths in the package. A non-zero exit fails the run with `luau-analyze failed:` and its report. If the binary is missing, the CLI prints `luau-analyze: not installed (install the Luau CLI to type-check; tests still run)` and runs the tests anyway; it is not in the dev shell today (`nix shell nixpkgs#luau` provides it).
- every `.luau` file directly in `tests/` (subfolders are not searched), sorted by name, in one sandboxed VM with the same host modules and `require` (§3), so a test reaches the package with `require("../src/main")`. Test files may also require helpers, but a helper placed directly in `tests/` is itself run as a test file: keep helpers in a subfolder, or have them return a function that passes. A missing `tests/` or one with no `.luau` file fails the run. A test file is evaluated; it must return a function or a table of named functions (run in sorted key order; an empty table is an error). A file with several statements and no `return` runs nothing more, but a file that is a single expression (`assert(1 == 1)`) returns that expression's value and fails with `a test file must return a function or a table of functions`. The first failing `assert` or error stops the run. Each file has 10 s.
- test VMs also have `require("@harness/test")` (an error at runtime). Its `golden(name, text)` compares `text` with `tests/golden/<name>` and fails naming the first differing line (`got:` / `want:`), or `golden file tests/golden/<name> missing`. Run with `PLUGIN_UPDATE_GOLDEN=1` to write the file instead. `name` must stay inside `tests/golden/`: no leading `/`, no backslash, and no `..` anywhere in it (so `a..txt` is refused too).
- a package's `log.debug` lines go to stderr as they happen (§4.5); the report below goes to stdout.

```console
$ cargo run -q -p harness --bin plugin -- test plugins/argument-map
manifest ok: argument-map 0.1.0 (1 tools)
luau-analyze: ok
13 test(s) passed:
  plugins/argument-map/tests/argument_map.luau::blanks_hide_text_from_the_learner_but_not_the_model
  plugins/argument-map/tests/argument_map.luau::duplicate_ids_are_refused
  plugins/argument-map/tests/argument_map.luau::empty_argument_is_refused
  plugins/argument-map/tests/argument_map.luau::golden_hume_blanks_card
  plugins/argument-map/tests/argument_map.luau::golden_hume_card
  plugins/argument-map/tests/argument_map.luau::long_text_wraps_inside_the_box
  plugins/argument-map/tests/argument_map.luau::objections_are_optional
  plugins/argument-map/tests/argument_map.luau::objections_sit_under_their_target
  plugins/argument-map/tests/argument_map.luau::render_redraws_the_stored_card
  plugins/argument-map/tests/argument_map.luau::summary_names_counts_and_attacks
  plugins/argument-map/tests/argument_map.luau::unknown_blank_is_refused
  plugins/argument-map/tests/argument_map.luau::unknown_target_is_refused
  plugins/argument-map/tests/argument_map.luau::wrap_counts_characters_not_bytes
```

`tests/` is required by the authoring contract even though the loader does not read it at runtime.

## 10. Worked example: `bars`

A minimal complete package that draws a bar chart. (The `circuit` plugin in `plugins/circuit/` is the reference for a real solver; it is not copied here.)

```
bars/
  plugin.toml
  prompts/tutor.md
  schemas/bars.json
  src/main.luau
  tests/bars.luau
```

`plugin.toml`:

```toml
id = "bars"
version = "0.1.0"
api = 1
title = "Bar chart"
description = "Draws a labelled bar chart from a list of values and returns their total."
license = "MIT"
platforms = ["desktop", "mobile"]
permissions = []
use_when = "The learner must compare sizes at a glance: energy use by source, spending by category, share of a whole."
avoid_when = "Two numbers would do, or the exact values matter more than their sizes."
cost = "instant"

[[tool]]
name = "bars_show"
description = "Draw a bar chart from labelled values. Returns the total. Place the figure with ![short description](artifact:<id>)."
agents = ["tutor"]
input = "schemas/bars.json"
entry = "show"
artifact = true

[[renderer]]
entry = "render"

[prompts]
tutor = "prompts/tutor.md"
```

`schemas/bars.json`:

```json
{
  "type": "object",
  "properties": {
    "title": { "type": "string" },
    "unit": { "type": "string" },
    "values": {
      "type": "array",
      "minItems": 1,
      "maxItems": 12,
      "items": {
        "type": "object",
        "properties": {
          "label": { "type": "string" },
          "value": { "type": "number" }
        },
        "required": ["label", "value"]
      }
    }
  },
  "required": ["values"]
}
```

`prompts/tutor.md`:

```markdown
## Bar charts

Draw a chart when the point is a comparison: which source is largest, how far apart two shares are.

- Label every bar; put the unit in the title, not on each bar.
- Place the chart once, next to the sentence that uses it, with `![…](artifact:<id>)`.
- State the total only when it is what the learner should notice; otherwise let the bars carry it.
```

`src/main.luau`:

```lua
local svg = require("@harness/svg")

local BAR = 24
local GAP = 8
local LEFT = 120

local function total(values)
  local sum = 0
  for _, item in ipairs(values or {}) do
    sum = sum + (tonumber(item.value) or 0)
  end
  return sum
end

local function chart(args)
  local values = args.values or {}
  local max = 1
  for _, item in ipairs(values) do
    max = math.max(max, math.abs(tonumber(item.value) or 0))
  end
  local width = 480
  local height = #values * (BAR + GAP) + GAP * 2
  local parts = {}
  for i, item in ipairs(values) do
    local y = GAP + (i - 1) * (BAR + GAP)
    local value = tonumber(item.value) or 0
    local w = (width - LEFT) * math.abs(value) / max
    parts[#parts + 1] = svg.text(LEFT - 8, y + BAR * 0.7, item.label or "", { ["text-anchor"] = "end" })
    parts[#parts + 1] = svg.rect(LEFT, y, w, BAR, { fill = "currentColor", ["fill-opacity"] = "0.25" })
    parts[#parts + 1] = svg.text(LEFT + w + 6, y + BAR * 0.7, string.format("%.4g", value) .. " " .. (args.unit or ""))
  end
  local title = args.title and svg.text(0, 14, args.title, { ["font-weight"] = "600" }) or ""
  return svg.document(width, height + 20, title .. table.concat(parts))
end

local function show(args)
  local sum = total(args.values)
  return {
    text = string.format("%d bars; the total is %.6g %s.", #(args.values or {}), sum, args.unit or ""),
    artifact = {
      kind = "svg",
      svg = chart(args),
      data = { title = args.title, unit = args.unit, values = args.values, total = sum },
    },
  }
end

local function render(data)
  return chart(data or {})
end

return {
  show = show,
  render = render,
  total = total,
}
```

`tests/bars.luau`:

```lua
local api = require("../src/main")

return {
  sums = function()
    assert(api.total({ { label = "a", value = 1 }, { label = "b", value = 2 } }) == 3)
  end,
  draws = function()
    local out = api.show({ title = "Spending", unit = "EUR", values = { { label = "Rent", value = 900 }, { label = "Food", value = 300 } } })
    assert(out.artifact.kind == "svg" and out.artifact.svg:find("<svg") == 1)
    assert(out.artifact.data.total == 1200)
  end,
}
```

Run it:

```console
$ cargo run -q -p harness --bin plugin -- test /tmp/bars
manifest ok: bars 0.1.0 (1 tools)
luau-analyze: not installed (install the Luau CLI to type-check; tests still run)
2 test(s) passed:
  /tmp/bars/tests/bars.luau::draws
  /tmp/bars/tests/bars.luau::sums
```

What happens in a session:

1. The planner sees the catalog line ``- **Bar chart** (`bars`): tools [bars_show]; platforms [desktop, mobile]. Use when: … Avoid when: … Cost: instant`` (`PluginManifest::catalog_line`).
2. If the section lists `bars` and it is granted, `bars_show` reaches the tutor's tool list in Teach.
3. The tutor calls `bars_show` with labelled values; the tool returns text plus the placement line.
4. The tutor writes `![Spending by category](artifact:01J…)` beside the sentence that uses it; the UI shows the stored SVG.
5. If the plugin declared a view with `answer = true`, the same artifact could serve a `question_ask` answer surface; `bars` deliberately has none, because moving a slider is not the point of a bar chart.

## 11. Catalogue

These ids are the plugins the architecture envisages; the Status column says which are built. Besides the built-ins (§12), the loader knows `circuit` and `argument-map` (Luau packages in `plugins/`, installed by folder or registry), the security fixture `plugins/fixtures/ipc-probe` (§7.2, added only by hand) and whatever the learner installs.

Tool names follow the `<id>_` rule (§2), so several were renamed from the old briefs (`code_task` → `code_runner_task`, `lab_start` → `sandbox_start`, `web_search` → `live_search_web`, …). Every built-in's manifest, schemas, prompt fragments and view live in `crates/harness/src/plugins/native/<id>/` and are compiled in (§12).

| Plugin id | Gives | Platforms | Status |
|---|---|---|---|
| `sandbox` | Local container lab the learner works in through a terminal card: `sandbox_start` (artifact), `sandbox_observe`, `sandbox_check` | desktop | **Built** in (native, `exec`; §12 `sandbox`). |
| `code-runner` | Write, fix, read and error-reading tasks graded by tests: `code_runner_task` | desktop | **Built** in (native, `exec`; §12 `code-runner`). Python, JavaScript, TypeScript, SQL (SQLite) and bash, each only where its toolchain is installed. Desktop only: it runs the device's own toolchains. |
| `repl` | `repl_eval` for the agent, `repl_task` for the learner (python, javascript, sql, nix) | desktop | **Built** in (native, `exec`; §12 `repl`). |
| `cas` | `cas_check` (SymPy) to verify the tutor's own working and answer keys | desktop | **Built** in (native, `exec`; §12 `cas`). Needs `python3` with SymPy. Also a Rust API (`cas::equivalent`) that no core path calls yet. |
| `live-search` | `live_search_web`, `live_search_fetch`, `live_search_images` inside the lesson | desktop, mobile | **Built** in (native, `net`; §12 `live-search`). |
| `image-set` | Labelled image drills: `image_set_list`, `image_set_drill` | desktop, mobile | **Built** in (native, `fs-read`; §12 `image-set`). Sets are the learner's labelled folders copied into the data repo. |
| `argument-map` | Pinned premise/objection card: `argument_map_show` (artifact) | desktop, mobile | **Built** (Luau api 1, `plugins/argument-map/`; ships like `circuit`: through the default registry, `../ps-learn-registry/plugins/argument-map`, which is a copy of the app repo's folder. Re-copy it from the app repo and raise `version` before publishing a change; the registry's CI refuses a changed folder under a published version). Objections nest under the premise they target, replies under their objection; `blanks` hides a part (premise or objection id, `O1.1` for a reply, `conclusion`) from the figure **and** the stored data while the tool text still gives the model the hidden text to grade against. No view. Tests with two goldens in `tests/`. |
| `demo` | Interactive simulation jobs: `demo_request` | desktop, mobile | Not a plugin in the current plan: a Builder-role job (`crates/harness/prompts/jobs/demo-builder.md`, which also holds the brief rules the tutor follows). |
| `figure` | Exact SVG figure jobs: `figure_request` | desktop, mobile | Not a plugin in the current plan: a Builder-role job (`crates/harness/prompts/jobs/figure-maker.md`, likewise). |

The catalog the planner sees contains only installed, enabled plugins available on the current platform; with none it says "none available". A built-in that declares permissions is not in it until the learner has enabled it (§12 *Consent and the Settings row*), so the planner can't propose, and the tutor can't request, a plugin that would be refused. On a phone only the manifests that list `mobile` appear.

## 12. Native plugins

A native plugin is an in-tree Rust type implementing `plugins::Plugin` (manifest, tools, fragment, view, grade, call). It uses the same `plugin.toml` shape (its `entry` names the Rust method; there is no `src/main.luau`) and may declare permissions (`exec`, `net`, `fs-read`, `fs-write`). The built-ins are listed by `plugins::native::builtin` (once per `Harness`, on the first `reload_plugins`; later registry rebuilds keep the same instances, so a running REPL survives an install elsewhere). They register after installed and folder packages, so a package can't shadow one, and otherwise behave like a Luau plugin to the planner, the tool loop, the plan limits, artifacts (§6) and answer-mode views (§7.4). `Harness::register_native_plugin` adds a further in-tree plugin (tests use it) under the same consent rule.

Seven are built in: `code-parse`, `live-search`, `image-set`, `code-runner`, `repl`, `cas` and `sandbox`. Each manifest is validated like a package's, with the native rules (`manifest::validate_native`); a unit test loads every built-in.

**Consent and the Settings row.** `PluginInfo.builtin` marks them. A built-in is never removable (`remove_plugin` refuses it; Settings shows no Remove button) and its row reads "built in". One with an empty `permissions` list (`code-parse`) is on from the start. One that declares permissions starts **disabled**: `set_plugin_enabled(id, true)` is the learner's consent to those permissions and is recorded device-locally in `settings.json` → `enabledPlugins` (turning it off withdraws it and lists it in `disabledPlugins`, as for any plugin). Settings → Plugins therefore opens the same consent screen as an install (§8.4, with the permissions in plain words: "Run programs on your device", "Use the network", "Read your files") when the learner presses **Enable**, and only calls `set_plugin_enabled` after they accept; **Disable** needs no consent. Until then the plugin is not in the planner's catalog, `plugin_request` refuses it, its tools are not offered, and its view and bridge calls are refused ("plugin `<id>` is disabled"). Consent is per device: another device has its own.

**Platforms.** A manifest's `platforms` is what the plugin does today. `exec` natives (`code-runner`, `repl`, `cas`, `sandbox`) list only `desktop`, because they run programs installed on the device and no such toolchain exists on a phone. `code-parse`, `live-search` and `image-set` list `mobile` too: they are Rust without external programs (the tree-sitter grammars compile from C; HTTP uses the app's client; images are read from the synced data repo). Checked 2026-11-14: `cargo check -p harness --target aarch64-linux-android` in the flake's `android` shell (NDK r26, the six grammars' C compiled by the NDK clang) passes with every native in the tree, so the harness, `exec.rs` included, builds for Android; only that one ABI was checked, and no native has run on a phone. On a phone a desktop-only plugin is `onPlatform: false`, never offered or granted, a tool call to one is refused, and a session that used one locks (`docs/sync-build.md` "Sessions on mobile"); `crates/harness/tests/native_plugins_e2e.rs` runs that rule against the real manifests (`cas` on a phone).

**Running programs (`exec`).** The `exec` built-ins share `native/exec.rs`. Every run gets a fresh temporary working directory (removed afterwards; `HOME` and `TMPDIR` point into it), a scrubbed environment (only `PATH` survives, so API keys in the app's environment never reach the program), a wall-clock limit and an output cap (hitting either kills it and says so), and its own process group, killed when the run or its handle ends. On Linux, when unprivileged user namespaces work (probed once with `unshare`), it also runs under `unshare --user --map-root-user --net --pid --fork --kill-child`: **no network**, a PID namespace that takes every descendant with it, and `prlimit` CPU and file-size limits when present. The file system is **not** isolated: the program runs as the learner's user and can read and write what they can. Elsewhere (macOS, Windows, Linux without user namespaces) there is no network or file isolation at all, and `exec::isolation()` says which applies. None of this makes hostile code safe; it keeps ordinary mistakes (an infinite loop, a stray request) from hurting the machine. Enabling an `exec` plugin is consent to run programs. Only `sandbox` (container labs, below) gets a separate file system. A missing toolchain is a plain tool error naming what to install (`code-runner` and `repl` run whichever of their languages are installed).

### `code-parse` (tree-sitter)

Built in, `crates/harness/src/plugins/native/code_parse.rs` (assets in `native/code-parse/`). Permissions `[]`: every tool takes the code as `source` text (≤ 256 KiB, the `file_read` cap; optional `firstLine` shifts reported lines for a range read), so it reads no files itself. Desktop and mobile.

| Tool | Returns |
|---|---|
| `code_parse_outline { language, source, firstLine? }` | Whether it parses cleanly, then every definition nested (function/method, class, struct, enum, union, trait, impl, interface, type, namespace/mod, macro) as `lines a–b (kind): signature` (the node's text up to its body, whitespace collapsed, ≤ 100 chars); at most 300 entries |
| `code_parse_errors { language, source, firstLine? }` | The outermost `ERROR` nodes and every `MISSING` node of tree-sitter's error recovery, in order, each as `line L, cols a–b: unexpected `…` in <construct>` or `missing `)` in arguments`, with the source line and a caret; at most 20. `parses cleanly` otherwise |
| `code_parse_locate { language, source, symbol, text?, firstLine? }` | Each definition whose path of enclosing definition names ends with `symbol` (`Stack.push`, `shapes::Circle::area`; a Rust `impl` is named by its type without generics), with kind, line and column range, signature and, with `text: true`, its text (≤ 200 lines); then every line where an identifier leaf is spelled like the name (strings and comments never match). Unknown symbols list the names that are defined |

Lines and columns are 1-based and columns count characters, not tree-sitter's bytes. Traversal uses explicit stacks, so deeply nested code cannot overflow the thread. `language` is one of `python`, `javascript` (JSX included), `typescript`, `tsx`, `rust`, `c`; extensions (`py`, `js`, `ts`, `rs`, `h`, …) are accepted too.

**Grammar set and size.** Each grammar is a generated C parse table compiled into the app (tree-sitter 0.25 runtime; grammar crates python 0.25, javascript 0.25, typescript 0.23, rust 0.24, c 0.24). Measured 2026-11-13 as the growth of a stripped x86_64 release binary: runtime about 0.46 MB, Python +0.47 MB, JavaScript +0.42 MB, TypeScript +1.43 MB, TSX +1.45 MB, Rust +1.13 MB, C +0.63 MB; all six together 6.0 MB. The set covers the languages the catalogue's code tasks use (`code-runner` JS/TS/Python) plus the two systems languages; TSX costs as much as TypeScript but `.tsx` files fail under the plain TypeScript grammar. Every further grammar is another 0.4–1.5 MB per Android ABI, so the set stays fixed rather than growing on demand.

### `live-search`

Built in, `crates/harness/src/plugins/native/live_search.rs` (assets in `native/live-search/`). Permissions `[net]`, so it starts disabled until the learner enables it in Settings. Desktop and mobile. Tutor only.

| Tool | Returns |
|---|---|
| `live_search_web { query, recency? }` | The researcher's search chain (`research_search::search_at`: Brave with `BRAVE_API_KEY`, else Wikipedia; `recency` is `day`/`week`/`month`/`year` and only Brave honours it) as a numbered list of up to 8 sources: title, URL, snippet (≤ 240 chars) |
| `live_search_fetch { url }` | The researcher's `web::page_fetch`: readable text of an HTML page (≤ 40k chars, `[truncated]`), a PDF as `[p.N]`-tagged text. Every URL and every redirect hop goes through the same `web::GuardedClient` as `page_fetch`: http(s) only, no loopback, private, link-local, CGNAT or metadata addresses, and names resolving to one are dropped by the resolver |
| `live_search_images { query, count? }` | An `svg` artifact: up to `count` images (default 4, at most 6), two per row, each numbered with its caption shortened below. The text lists each image's full caption, source page and credit (licence and author when the service gives one), plus any hit whose download failed |

**How images reach the learner.** The image search is Brave images (with the key), else Wikimedia Commons file search (keyless; raster files only, caption from the file's own description, else its file name, credit from its author and licence). Twice `count` hits are downloaded at once through the guard (≤ 4 MiB each; PNG, JPEG, GIF or WebP by content, anything else is skipped) and embedded in the artifact SVG as `data:` URIs: `data:image/png|jpeg|gif|webp` is the only image link the artifact sanitizer keeps (§6.2) and the only image source the view CSP allows (`img-src data:`). So nothing is fetched from the network when the figure is shown or replayed, and a page that disappears later doesn't break the lesson. To keep the fallback SVG under its 256 KiB cap, the figure has a 232 KiB image budget split evenly across its images. A PNG or JPEG over 360 px or over its share is re-encoded as a JPEG at the largest size from 360 px down to 80 px that fits. A GIF or WebP is kept only if it already fits. The artifact `data` holds the sources (`query`, `images[]: { n, caption, pageUrl, imageUrl, credit }`), not the pixels. There is no view.

The fragment tells the tutor that a snippet is not evidence (fetch first, cite) and to label an image only from its source's caption, never from how it looks. Single facts still go to `research_request` (architecture §4.20).

### `image-set`

Built in, `crates/harness/src/plugins/native/image_set.rs` (assets in `native/image-set/`, including the answer-mode view `views/drill.html`). Permissions `[fs-read]`, so it starts disabled until the learner enables it in Settings. Desktop and mobile. Tutor and practice (the practice fragment waits for the coach).

**Where sets live.** A set is `<repo>/image-sets/<setId>/<label>/<image>`: the subfolder name is the label. An optional `set.json` `{ "title" }` names the set. Sets are in the synced data repo, like message images (architecture §4.25), because a drill must work on every device. A learner's own labelled folder is copied in once, on a desktop, with Settings → Libraries → **Import image set…** (`image_set_import { path, name? }`; `image_sets`, `image_set_remove { id }` list and delete). The source folder is never touched, and an import into an existing id replaces it. Only PNG, JPEG, GIF and WebP files (by content; a missing extension is added) up to 5 MiB count. Hidden entries, symlinks and loose files are skipped, and a set needs at least two labels.

| Tool | Returns |
|---|---|
| `image_set_list {}` | JSON `[{ id, title, labels: [{ label, count }], size }]`, or how the learner adds a set when there is none |
| `image_set_drill { setId, labels?, n, format, interleave: true }` | An `svg` artifact (the drill figure), and text with the labels used and `target: {…}`, the answer key the tutor copies into `question_ask`'s `view.target` |

**Drill.** `classify` shows `n` images, and the learner picks each image's label from `choices`. `same-or-different` shows `n` pairs, about half of them same-label pairs, and the learner answers `same` or `different` per pair. `odd-one-out` shows `n` triples (two of one label, one of another), and the learner picks A, B or C. Labels are interleaved: an item never repeats the previous item's label when another is available, and images are drawn without repeats until a label runs out. At most 36 images per drill, so 12 triples. Images are embedded as `data:` URIs within the same 232 KiB budget as `live-search` (so `data` and the fallback SVG each stay under their 256 KiB caps). The view's `data` (`{ setId, format, items: [{ images }], choices }`) holds no labels.

**Grading** (§7.4). The view posts `{ answers: [...] }`, with labels, `same`/`different`, or the 0-based odd image. `grade(answer, target)` returns `{ correct, score, total, detail, items: [{ item, expected, given, correct }], confusions: [{ expected, given, count }] }`, with confusions sorted by count. A confusion is a true label taken for another one: the label given, the second label of a different pair called `same`, or the label of the image picked instead of the odd one. The target carries `check`, a hash of `format`, `key` and `labels`. A target the tutor didn't copy exactly is refused ("…not copied exactly…"), so the question stays open instead of being graded against a wrong key. As always, the tutor records the verdict.

### `code-runner`

Built in, `crates/harness/src/plugins/native/code_runner.rs` (assets in `native/code-runner/`, including the answer-mode view `views/task.html`). Permissions `[exec]`, so it starts disabled until the learner enables it in Settings. Desktop only. Tutor and practice (the practice fragment waits for the coach). Languages: python (`python3`), javascript (`node`, else `deno`), typescript (`deno`, else `node --experimental-strip-types`), sql (`sqlite3`, in memory: a test's `input` is setup SQL run before the code) and bash. Only languages whose program is on `PATH` run; a missing one is a tool error that says what to install.

| Tool | Returns |
|---|---|
| `code_runner_task { language, kind, prompt, starter?, solution?, tests, predictFirst? }` | Checks the task before the learner sees it (`write`/`fix`: the reference `solution` passes every test, and a `fix` starter fails at least one; `read`: the starter prints every `expected`; `error`: the starter fails), then an `svg` artifact, the task card. `tests` are `{ name, input?, expected }` (stdin in, trimmed stdout compared), at most 12. The artifact `data` holds the prompt, starter and test inputs, never the expected outputs or the solution; those stay in the committed tool input |

The tutor asks with `question_ask { view: { artifactId, task, target: {} } }`. **Grading** (§7.4): the view's `run` call runs the learner's code (10 s, 32 KiB of output per run; the card remembers its last 20 runs, and the prediction sent with the first run is the committed one). Submit posts `{ code, prediction, explanation }`; `grade_artifact` re-runs that code against the tests in the artifact's committed input and returns `{ correct, detail, code, prediction, explanation, runs, tests }`. As always, the tutor records the verdict. The card is replayable without the plugin from its fallback SVG; a card whose plugin is off or missing can't take an answer.

### `repl`

Built in, `crates/harness/src/plugins/native/repl.rs` (assets in `native/repl/`: the view `views/repl.html`, the python and node drivers in `drivers/`). Permissions `[exec]`, so it starts disabled until the learner enables it in Settings. Desktop only. Tutor and practice (the practice fragment waits for the coach).

| Tool | Returns |
|---|---|
| `repl_eval { lang, input }` | What the input prints and its value, in the tutor's **own** session for that language in this chat (state persists between calls; another chat has its own). Not an artifact: the learner sees it only if the tutor quotes it |
| `repl_task { lang, prompt, check }` | An `svg` artifact, a REPL card. `check` is an expression true in the learner's final session state (`result == 55`; SQL a scalar such as `(SELECT count(*) FROM users) = 3`; nix `builtins.hasAttr "port" config`). It is refused when it already holds in a fresh session or doesn't parse, and it never appears in the card's data |

Engines: **python** (`python3`) and **javascript** (`node`) run one persistent process with a small driver that evaluates each input in a kept namespace and echoes the last expression; **sql** (`sqlite3`) keeps every input and replays the history into a fresh `sqlite3 -safe` per input, printing only the new input's output; **nix** (`nix-instantiate --eval --strict`) turns `name = expr` inputs into `let` bindings for every later input. An input has 10 s (nix 30 s) and 32 KiB of output; one past its limit stops the session, which starts empty on the next input and says so. At most 8 sessions stay alive (least recently used first), and a transcript keeps its last 60 entries.

**Grading** (§7.4). The learner's inputs go to a session of their own per card (the view's `input` call; the card's id is the key). `grade_artifact` evaluates the committed `check` in that session's final state and returns `{ correct, detail, lang, check: { expression, value, passed }, transcript }`; a card with no recorded input is graded on an empty session and says so (after an app restart the session is gone). As always, the tutor records the verdict.

### `cas`

Built in, `crates/harness/src/plugins/native/cas.rs` (assets in `native/cas/`). Permissions `[exec]`, so it starts disabled until the learner enables it in Settings. Desktop only. Tutor and practice (the practice fragment waits for the coach). Needs `python3` with SymPy installed for that python; without them `cas_check` returns an error that names what's missing.

| Tool | Returns |
|---|---|
| `cas_check { code }` | What a short SymPy program prints (stdout then stderr, with a note when stopped). `from sympy import *` is done and `x, y, z, t, a, b, c, n, k` are symbols; ≤ 16 KiB of code, 20 s, 16 KiB of output. For checking the tutor's own derivations and numbers; not an artifact |

The same engine is a Rust API for core code that must check math without a model: `cas::check(code)` and `cas::equivalent(a, b)` (`simplify(a - b) == 0`, then SymPy's numeric `equals`; implicit multiplication and `^` as power; the expressions travel as JSON on stdin, never spliced into the program). Both block (run them off the async runtime) and fail with `CasError::Unavailable` when python3 or SymPy is missing. No core path calls them yet.

### `sandbox`

Built in, `crates/harness/src/plugins/native/sandbox.rs` (assets in `native/sandbox/`: scenario scripts in `scenarios/`, the answer-mode view `views/terminal.html`). Permissions `[exec]`, so it starts disabled until the learner enables it in Settings. Desktop only. Tutor and practice (the practice fragment waits for the coach). Needs Podman (preferred) or Docker on `PATH`; without one the tools return the usual "isn't available on this device" error and the plugin's `use_when` says "Container runtime on this device: none found".

| Tool | Returns |
|---|---|
| `sandbox_start { scenario, image? }` | Boots the lab and returns an `svg` artifact, the terminal card (`data`: `{ labId, scenario, goal, image }`), and text with the goal and the lab id. `scenario` is one of the four below (an enum in the schema, each listed with its goal in the tool description); `image` defaults to `alpine:3.20` and must match `^[a-z0-9][a-z0-9./_-]*(:[A-Za-z0-9._-]+)?$` (a name without `/` is qualified as `docker.io/library/<name>`, because Podman can't ask which registry to search without a terminal) |
| `sandbox_observe { since? }` | The learner's commands in this chat's lab after entry number `since` (default all kept: the last 200), each as `[n] $ command`, its output and `(exit c)`; the text ends with `Latest entry N.` |
| `sandbox_check { checkId }` | `PASS`/`FAIL <scenario>` and the check's `ok:`/`fail:` lines. `checkId` is the scenario id; another id is refused |

**Lab.** One lab per chat session: starting another removes the old container first, and a lab is removed when it is replaced, when the plugin is dropped and, if the app is killed first, when its `sleep 7200` ends (`--rm`). The container is `run -d --rm --name lh-lab-<id> --network none --memory 256m --cpus 1 --pids-limit 256 --stop-timeout 1 --cap-drop ALL --cap-add CHOWN,DAC_OVERRIDE,FOWNER,SETUID,SETGID,KILL --security-opt no-new-privileges <image> sleep 7200`; the scenario's setup script is then `exec`'d synchronously (a setup that fails removes the container and returns its output). The client runs through `exec::run` with `no_isolation()` (it has to reach the container service).

**Scenarios.** Each is a pair of POSIX `sh` scripts (`scenarios/<id>.setup.sh`, `.check.sh`) sent as one `sh -c` argument. Both honour an optional `$LAB_ROOT` path prefix (empty in a container) and `LAB_OWNER` (default `root`), which is how the tests run them on the host. `LAB_FLAG`, a random token per lab, goes to setup and check as an `exec -e` only, so the learner's own commands (and `env`) can't read it. A check prints `ok:`/`fail:` lines and exits 0 only when all hold.

| Scenario | Setup | Check |
|---|---|---|
| `permissions` | `/srv/app/secret.conf`, mode 666 | mode 600, owner root, contents intact |
| `find-the-flag` | six logs in `/var/log/app`; one (picked from the flag) holds a `FLAG{…}` line | `/tmp/flag.txt` holds exactly the flag |
| `harden-sshd` | `/etc/ssh/sshd_config` with `yes` on `PermitRootLogin`, `PasswordAuthentication`, `PermitEmptyPasswords`, `X11Forwarding` | the first active `PermitRootLogin` and `PasswordAuthentication` are `no`, the `Port 22` line is kept, no active setting is `yes` |
| `stop-the-service` | busybox `httpd` on port 8080, started and waited for | no LISTEN socket on the port in `/proc/net/tcp{,6}` (looked at for up to a second, so a just-signalled service can close) |

**Terminal.** The `Terminal` view (`calls = ["run"]`, answer mode) shows the goal and a command line. `run { labId, artifactId, command }` runs `exec <container> sh -c <command>` as one argv (no host shell), each command on its own (a `cd` doesn't carry over), with a 20 s timeout and a 16 KiB output cap; a command past either is stopped and the output says so (a timed-out command may keep running in the lab). The transcript (commands, output, exit codes) is kept in memory. The view shows `[exit n]` for a failure and stops accepting input when its lab has been replaced.

**Grading** (§7.4). Submit posts `{ commands: n }`; `grade_artifact` runs the scenario check in the lab the card is bound to and returns `{ correct, detail, scenario, check: { id, passed, output }, transcript }` (the last 40 commands). A card is bound to the lab of its first `run` call (so two chats' cards each grade their own lab); a card that never ran a command uses the only lab there is when its scenario matches, and otherwise gets an error. A replaced lab, an app restart or a container that has vanished gives a "start a new lab" error rather than a failed check. As always, the tutor records the verdict.

Tested with a fake container client (a shell script whose "container" is a directory, so every scenario's setup and check run on the host): each check fails before the fix and passes after, plus the transcript, binding, replacement, limits and error paths. No Podman or Docker is installed on the development machine, so a real container run is not covered; the `permissions`, `find-the-flag` and `harden-sshd` scripts were also run under BusyBox `ash` (with the host's coreutils and awk), and `stop-the-service` needs BusyBox's `httpd`, which only the image has.
