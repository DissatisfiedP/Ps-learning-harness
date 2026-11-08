//! The api-1 host standard library for Luau plugins (build prompt W5).
//! Pure functions only: JSON, SVG element building, real/complex arithmetic
//! and a dense linear solve, a seeded PRNG, and a debug log. No I/O.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use mlua::{Lua, Result as LuaResult, Table, Value};
use serde_json::Value as Json;

/// The host modules by name (`json`, `svg`, `num`, `rand`, `log`): what
/// `require("@harness/<name>")` returns in a plugin VM.
pub fn modules(lua: &Lua) -> LuaResult<Table> {
    let modules = lua.create_table()?;

    let json = lua.create_table()?;
    json.set(
        "encode",
        lua.create_function(|_, value: Value| Ok(json_encode(&lua_value_to_json(&value)?)))?,
    )?;
    json.set(
        "decode",
        lua.create_function(|lua, text: String| {
            let parsed: Json = serde_json::from_str(&text)
                .map_err(|e| mlua::Error::RuntimeError(format!("invalid JSON: {e}")))?;
            json_to_lua(lua, &parsed)
        })?,
    )?;
    modules.set("json", json)?;

    modules.set("svg", svg_lib(lua)?)?;
    modules.set("num", num_lib(lua)?)?;
    modules.set("rand", rand_lib(lua)?)?;
    modules.set("log", log_lib(lua)?)?;
    Ok(modules)
}

/// `@harness/test`, only in `plugin test` VMs. `golden(name, text)` compares
/// `text` with `tests/golden/<name>`; with `PLUGIN_UPDATE_GOLDEN=1` it writes
/// the file instead.
pub fn test_module(lua: &Lua, dir: &std::path::Path) -> LuaResult<Table> {
    let golden_dir = dir.join("tests/golden");
    let test = lua.create_table()?;
    test.set(
        "golden",
        lua.create_function(move |_, (name, text): (String, String)| {
            let bad = name.is_empty()
                || name.contains("..")
                || name.starts_with('/')
                || name.contains('\\');
            if bad {
                return Err(mlua::Error::RuntimeError(format!(
                    "golden: `{name}` must be a relative path inside tests/golden"
                )));
            }
            let path = golden_dir.join(&name);
            if std::env::var("PLUGIN_UPDATE_GOLDEN").as_deref() == Ok("1") {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(mlua::Error::external)?;
                }
                std::fs::write(&path, &text).map_err(mlua::Error::external)?;
                return Ok(());
            }
            let want = std::fs::read_to_string(&path).map_err(|_| {
                mlua::Error::RuntimeError(format!(
                    "golden file tests/golden/{name} missing (run with PLUGIN_UPDATE_GOLDEN=1 to write it)"
                ))
            })?;
            if want == text {
                return Ok(());
            }
            let (mut got_lines, mut want_lines) = (text.lines(), want.lines());
            let mut line = 1;
            loop {
                match (got_lines.next(), want_lines.next()) {
                    (Some(g), Some(w)) if g == w => line += 1,
                    (g, w) => {
                        return Err(mlua::Error::RuntimeError(format!(
                            "golden tests/golden/{name} differs at line {line}:\n  got:  {}\n  want: {}",
                            g.unwrap_or("<end of text>"),
                            w.unwrap_or("<end of file>")
                        )))
                    }
                }
            }
        })?,
    )?;
    Ok(test)
}

// ---- JSON -----------------------------------------------------------------

/// Deepest table nesting a plugin value may have when crossing into JSON.
pub const MAX_JSON_DEPTH: usize = 64;

/// Convert a Luau value to JSON. A table whose keys are exactly the integers
/// 1..n becomes an array; any other table becomes an object (integer keys as
/// strings). A self-referential table or nesting deeper than
/// [`MAX_JSON_DEPTH`] is a Lua error, never a stack overflow.
pub fn lua_value_to_json(value: &Value) -> LuaResult<Json> {
    let mut path = Vec::new();
    to_json(value, &mut path)
}

fn to_json(value: &Value, path: &mut Vec<*const std::ffi::c_void>) -> LuaResult<Json> {
    Ok(match value {
        Value::Nil => Json::Null,
        Value::Boolean(b) => Json::Bool(*b),
        Value::Integer(i) => Json::from(*i),
        Value::Number(n) => Json::from(*n),
        Value::String(s) => Json::String(s.to_str().map(|s| s.to_owned()).unwrap_or_default()),
        Value::Table(t) => {
            let ptr = t.to_pointer();
            if path.contains(&ptr) {
                return Err(mlua::Error::RuntimeError(
                    "the value contains a table that refers to itself; it can't become JSON".into(),
                ));
            }
            if path.len() >= MAX_JSON_DEPTH {
                return Err(mlua::Error::RuntimeError(format!(
                    "the value nests deeper than {MAX_JSON_DEPTH} tables; it can't become JSON"
                )));
            }
            path.push(ptr);
            let mut indexed: Vec<(i64, Value)> = Vec::new();
            let mut named: Vec<(String, Value)> = Vec::new();
            for pair in t.clone().pairs::<Value, Value>() {
                let (k, v) = pair?;
                match k {
                    Value::Integer(i) => indexed.push((int_to_i64(i), v)),
                    Value::Number(n) if n.fract() == 0.0 && n.abs() < 9.0e15 => {
                        indexed.push((n as i64, v))
                    }
                    Value::String(s) => {
                        if let Ok(s) = s.to_str() {
                            named.push((s.to_string(), v));
                        }
                    }
                    _ => {}
                }
            }
            indexed.sort_by_key(|(i, _)| *i);
            let contiguous = indexed
                .iter()
                .enumerate()
                .all(|(n, (i, _))| *i == n as i64 + 1);
            let out = if named.is_empty() && !indexed.is_empty() && contiguous {
                let mut array = Vec::with_capacity(indexed.len());
                for (_, v) in &indexed {
                    array.push(to_json(v, path)?);
                }
                Json::Array(array)
            } else {
                let mut map = serde_json::Map::new();
                for (i, v) in &indexed {
                    map.insert(i.to_string(), to_json(v, path)?);
                }
                for (k, v) in &named {
                    map.insert(k.clone(), to_json(v, path)?);
                }
                Json::Object(map)
            };
            path.pop();
            out
        }
        _ => Json::Null,
    })
}

// mlua's Luau `Integer` is `i32` on 32-bit targets (armv7, i686 Android) and
// `i64` elsewhere, so these conversions are identities on 64-bit hosts.
#[allow(clippy::useless_conversion)]
fn int_to_i64(i: mlua::Integer) -> i64 {
    i64::from(i)
}

/// `None` when the value doesn't fit the target's `Integer`; the caller then
/// passes it as a float, which is what a Luau number is anyway.
#[allow(clippy::useless_conversion)]
fn i64_to_int(i: i64) -> Option<mlua::Integer> {
    mlua::Integer::try_from(i).ok()
}

pub fn json_to_lua(lua: &Lua, value: &Json) -> LuaResult<Value> {
    Ok(match value {
        Json::Null => Value::Nil,
        Json::Bool(b) => Value::Boolean(*b),
        Json::Number(n) => {
            if let Some(i) = n.as_i64().and_then(i64_to_int) {
                Value::Integer(i)
            } else {
                Value::Number(n.as_f64().unwrap_or(0.0))
            }
        }
        Json::String(s) => Value::String(lua.create_string(s)?),
        Json::Array(items) => {
            let t = lua.create_table()?;
            for (i, item) in items.iter().enumerate() {
                t.raw_set(i + 1, json_to_lua(lua, item)?)?;
            }
            Value::Table(t)
        }
        Json::Object(map) => {
            let t = lua.create_table()?;
            for (k, v) in map {
                t.raw_set(k.as_str(), json_to_lua(lua, v)?)?;
            }
            Value::Table(t)
        }
    })
}

fn json_encode(value: &Json) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".into())
}

// ---- SVG ------------------------------------------------------------------

fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Attributes as a deterministic ` k="v"` string. Event handlers and
/// external references are refused here as well as in the core sanitizer.
fn attrs(table: Option<&Table>) -> Result<String, mlua::Error> {
    let Some(t) = table else {
        return Ok(String::new());
    };
    let mut pairs: Vec<(String, String)> = Vec::new();
    for (k, v) in t.clone().pairs::<String, Value>().flatten() {
        let value = match v {
            Value::String(s) => s.to_str().map(|s| s.to_owned()).unwrap_or_default(),
            Value::Integer(i) => i.to_string(),
            Value::Number(n) => n.to_string(),
            Value::Boolean(b) => b.to_string(),
            _ => continue,
        };
        let key = k.to_ascii_lowercase();
        if key.starts_with("on") {
            return Err(mlua::Error::RuntimeError(format!(
                "SVG attribute `{k}` is not allowed"
            )));
        }
        if (key == "href" || key == "xlink:href")
            && !(value.starts_with('#') || value.starts_with("data:image/"))
        {
            return Err(mlua::Error::RuntimeError(
                "external SVG references are not allowed".into(),
            ));
        }
        pairs.push((k, value));
    }
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(pairs
        .into_iter()
        .map(|(k, v)| format!(" {k}=\"{}\"", escape_attr(&v)))
        .collect())
}

fn element(tag: &str, attrs_str: &str, children: &str) -> String {
    if children.is_empty() {
        format!("<{tag}{attrs_str}/>")
    } else {
        format!("<{tag}{attrs_str}>{children}</{tag}>")
    }
}

fn svg_lib(lua: &Lua) -> LuaResult<Table> {
    let svg = lua.create_table()?;
    svg.set(
        "document",
        lua.create_function(|_, (width, height, content): (f64, f64, String)| {
            Ok(format!(
                "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\" fill=\"none\" stroke=\"currentColor\">{content}</svg>"
            ))
        })?,
    )?;
    svg.set(
        "group",
        lua.create_function(|_, (a, content): (Option<Table>, String)| {
            Ok(element("g", &attrs(a.as_ref())?, &content))
        })?,
    )?;
    svg.set(
        "path",
        lua.create_function(|_, (d, a): (String, Option<Table>)| {
            let mut attrs_str = attrs(a.as_ref())?;
            attrs_str.push_str(&format!(" d=\"{}\"", escape_attr(&d)));
            Ok(element("path", &attrs_str, ""))
        })?,
    )?;
    svg.set(
        "line",
        lua.create_function(
            |_, (x1, y1, x2, y2, a): (f64, f64, f64, f64, Option<Table>)| {
                let mut attrs_str = attrs(a.as_ref())?;
                attrs_str.push_str(&format!(" x1=\"{x1}\" y1=\"{y1}\" x2=\"{x2}\" y2=\"{y2}\""));
                Ok(element("line", &attrs_str, ""))
            },
        )?,
    )?;
    svg.set(
        "rect",
        lua.create_function(|_, (x, y, w, h, a): (f64, f64, f64, f64, Option<Table>)| {
            let mut attrs_str = attrs(a.as_ref())?;
            attrs_str.push_str(&format!(
                " x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\""
            ));
            Ok(element("rect", &attrs_str, ""))
        })?,
    )?;
    svg.set(
        "circle",
        lua.create_function(|_, (cx, cy, r, a): (f64, f64, f64, Option<Table>)| {
            let mut attrs_str = attrs(a.as_ref())?;
            attrs_str.push_str(&format!(" cx=\"{cx}\" cy=\"{cy}\" r=\"{r}\""));
            Ok(element("circle", &attrs_str, ""))
        })?,
    )?;
    svg.set(
        "text",
        lua.create_function(|_, (x, y, text, a): (f64, f64, String, Option<Table>)| {
            let mut attrs_str = attrs(a.as_ref())?;
            attrs_str.push_str(&format!(" x=\"{x}\" y=\"{y}\""));
            Ok(element("text", &attrs_str, &escape_text(&text)))
        })?,
    )?;
    svg.set(
        "escape",
        lua.create_function(|_, text: String| Ok(escape_text(&text)))?,
    )?;
    Ok(svg)
}

// ---- numbers --------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
struct C {
    re: f64,
    im: f64,
}

impl C {
    fn from_value(v: &Value) -> Option<C> {
        match v {
            Value::Integer(i) => Some(C {
                re: *i as f64,
                im: 0.0,
            }),
            Value::Number(n) => Some(C { re: *n, im: 0.0 }),
            // A complex number is a table with a numeric `re` (as `num.c`
            // makes); any other table (a vector, a matrix) is not a number.
            Value::Table(t) => {
                let re = t.get::<Option<f64>>("re").ok().flatten()?;
                let im = t.get::<Option<f64>>("im").ok().flatten().unwrap_or(0.0);
                Some(C { re, im })
            }
            _ => None,
        }
    }

    fn add(self, o: C) -> C {
        C {
            re: self.re + o.re,
            im: self.im + o.im,
        }
    }
    fn sub(self, o: C) -> C {
        C {
            re: self.re - o.re,
            im: self.im - o.im,
        }
    }
    fn mul(self, o: C) -> C {
        C {
            re: self.re * o.re - self.im * o.im,
            im: self.re * o.im + self.im * o.re,
        }
    }
    fn div(self, o: C) -> C {
        let d = o.re * o.re + o.im * o.im;
        C {
            re: (self.re * o.re + self.im * o.im) / d,
            im: (self.im * o.re - self.re * o.im) / d,
        }
    }
    fn abs(self) -> f64 {
        self.re.hypot(self.im)
    }
    fn phase_deg(self) -> f64 {
        self.im.atan2(self.re).to_degrees()
    }
    fn is_real(self) -> bool {
        self.im.abs() < 1e-12
    }
}

fn to_lua(lua: &Lua, c: C) -> LuaResult<Value> {
    if c.is_real() {
        return Ok(Value::Number(c.re));
    }
    let t = lua.create_table()?;
    t.set("re", c.re)?;
    t.set("im", c.im)?;
    Ok(Value::Table(t))
}

fn matrix(v: &Value) -> Option<Vec<Vec<C>>> {
    let Value::Table(rows) = v else {
        return None;
    };
    let mut out = Vec::new();
    for row in rows.clone().sequence_values::<Value>() {
        let row = row.ok()?;
        // A row is a sequence of numbers; a complex number is not a row.
        if C::from_value(&row).is_some() {
            return None;
        }
        let Value::Table(cells) = row else {
            return None;
        };
        let mut r = Vec::new();
        for cell in cells.sequence_values::<Value>() {
            r.push(C::from_value(&cell.ok()?)?);
        }
        out.push(r);
    }
    (!out.is_empty()).then_some(out)
}

fn vector(v: &Value) -> Option<Vec<C>> {
    let Value::Table(items) = v else {
        return None;
    };
    let mut out = Vec::new();
    for item in items.clone().sequence_values::<Value>() {
        out.push(C::from_value(&item.ok()?)?);
    }
    Some(out)
}

/// Largest matrix/vector side `@harness/num` accepts, so one host call can't run
/// far past the time limit (Rust host code is not interruptible).
pub const MAX_DIM: usize = 400;

/// Refuse a matrix or vector argument with a side over [`MAX_DIM`].
fn check_dims(op: &str, v: &Value) -> LuaResult<()> {
    let Value::Table(t) = v else { return Ok(()) };
    let too_big = || {
        Err(mlua::Error::RuntimeError(format!(
            "{op}: matrices and vectors are limited to {MAX_DIM} rows and columns"
        )))
    };
    if t.raw_len() > MAX_DIM {
        return too_big();
    }
    for row in t.clone().sequence_values::<Value>() {
        if let Value::Table(r) = row? {
            if r.raw_len() > MAX_DIM {
                return too_big();
            }
        }
    }
    Ok(())
}

fn solve(mut a: Vec<Vec<C>>, mut b: Vec<C>) -> Option<Vec<C>> {
    let n = a.len();
    if n == 0 || b.len() != n || a.iter().any(|r| r.len() != n) {
        return None;
    }
    for col in 0..n {
        let pivot = (col..n).max_by(|&i, &j| {
            a[i][col]
                .abs()
                .partial_cmp(&a[j][col].abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })?;
        if a[pivot][col].abs() < 1e-14 {
            return None;
        }
        a.swap(col, pivot);
        b.swap(col, pivot);
        let pivot_row = a[col].clone();
        let pivot_cell = pivot_row[col];
        let pivot_b = b[col];
        for row in col + 1..n {
            let f = a[row][col].div(pivot_cell);
            for (k, above) in pivot_row.iter().enumerate().skip(col) {
                a[row][k] = a[row][k].sub(f.mul(*above));
            }
            b[row] = b[row].sub(f.mul(pivot_b));
        }
    }
    let mut x = vec![C { re: 0.0, im: 0.0 }; n];
    for row in (0..n).rev() {
        let mut sum = b[row];
        for k in row + 1..n {
            sum = sum.sub(a[row][k].mul(x[k]));
        }
        x[row] = sum.div(a[row][row]);
    }
    Some(x)
}

fn num_lib(lua: &Lua) -> LuaResult<Table> {
    let num = lua.create_table()?;
    num.set(
        "c",
        lua.create_function(|lua, (re, im): (f64, Option<f64>)| {
            to_lua(
                lua,
                C {
                    re,
                    im: im.unwrap_or(0.0),
                },
            )
        })?,
    )?;
    num.set(
        "add",
        lua.create_function(|lua, (a, b): (Value, Value)| {
            match (C::from_value(&a), C::from_value(&b)) {
                (Some(x), Some(y)) => to_lua(lua, x.add(y)),
                _ => Err(mlua::Error::RuntimeError("add expects numbers".into())),
            }
        })?,
    )?;
    num.set(
        "sub",
        lua.create_function(|lua, (a, b): (Value, Value)| {
            match (C::from_value(&a), C::from_value(&b)) {
                (Some(x), Some(y)) => to_lua(lua, x.sub(y)),
                _ => Err(mlua::Error::RuntimeError("sub expects numbers".into())),
            }
        })?,
    )?;
    num.set(
        "mul",
        lua.create_function(|lua, (a, b): (Value, Value)| {
            let err = |m: &str| Err(mlua::Error::RuntimeError(format!("mul: {m}")));
            check_dims("mul", &a)?;
            check_dims("mul", &b)?;
            if let Some(m) = matrix(&a) {
                let k = m[0].len();
                if m.iter().any(|r| r.len() != k) {
                    return err("the matrix's rows have different lengths");
                }
                // matrix × vector → vector
                if let Some(v) = vector(&b) {
                    if v.len() != k {
                        return err(&format!(
                            "a matrix with {k} columns needs a vector of {k} numbers, got {}",
                            v.len()
                        ));
                    }
                    let t = lua.create_table()?;
                    for (i, row) in m.iter().enumerate() {
                        let dot = row
                            .iter()
                            .zip(&v)
                            .fold(C { re: 0.0, im: 0.0 }, |s, (x, y)| s.add(x.mul(*y)));
                        t.raw_set(i + 1, to_lua(lua, dot)?)?;
                    }
                    return Ok(Value::Table(t));
                }
                // matrix × matrix → matrix
                let Some(v) = matrix(&b) else {
                    return err("a matrix multiplies a vector or a matrix");
                };
                let cols = v[0].len();
                if v.len() != k || v.iter().any(|r| r.len() != cols) {
                    return err("matrix shapes don't match");
                }
                let mut out = vec![vec![C { re: 0.0, im: 0.0 }; cols]; m.len()];
                for (j, v_row) in v.iter().enumerate() {
                    for (c, cell) in v_row.iter().enumerate() {
                        for (out_row, m_row) in out.iter_mut().zip(m.iter()) {
                            out_row[c] = out_row[c].add(m_row[j].mul(*cell));
                        }
                    }
                }
                let t = lua.create_table()?;
                for (i, row) in out.iter().enumerate() {
                    let rt = lua.create_table()?;
                    for (j, c) in row.iter().enumerate() {
                        rt.raw_set(j + 1, to_lua(lua, *c)?)?;
                    }
                    t.raw_set(i + 1, rt)?;
                }
                return Ok(Value::Table(t));
            }
            match (C::from_value(&a), C::from_value(&b)) {
                (Some(x), Some(y)) => to_lua(lua, x.mul(y)),
                _ => err("expects two numbers, a matrix and a vector, or two matrices"),
            }
        })?,
    )?;
    num.set(
        "div",
        lua.create_function(|lua, (a, b): (Value, Value)| {
            match (C::from_value(&a), C::from_value(&b)) {
                (Some(x), Some(y)) => to_lua(lua, x.div(y)),
                _ => Err(mlua::Error::RuntimeError("div expects numbers".into())),
            }
        })?,
    )?;
    num.set(
        "abs",
        lua.create_function(|_, v: Value| {
            C::from_value(&v)
                .map(|c| c.abs())
                .ok_or_else(|| mlua::Error::RuntimeError("abs expects a number".into()))
        })?,
    )?;
    num.set(
        "phaseDeg",
        lua.create_function(|_, v: Value| {
            C::from_value(&v)
                .map(|c| c.phase_deg())
                .ok_or_else(|| mlua::Error::RuntimeError("phaseDeg expects a number".into()))
        })?,
    )?;
    num.set(
        "conj",
        lua.create_function(|lua, v: Value| match C::from_value(&v) {
            Some(c) => to_lua(
                lua,
                C {
                    re: c.re,
                    im: -c.im,
                },
            ),
            None => Err(mlua::Error::RuntimeError("conj expects a number".into())),
        })?,
    )?;
    num.set(
        "real",
        lua.create_function(|_, v: Value| {
            C::from_value(&v)
                .map(|c| c.re)
                .ok_or_else(|| mlua::Error::RuntimeError("real expects a number".into()))
        })?,
    )?;
    num.set(
        "imag",
        lua.create_function(|_, v: Value| {
            C::from_value(&v)
                .map(|c| c.im)
                .ok_or_else(|| mlua::Error::RuntimeError("imag expects a number".into()))
        })?,
    )?;
    num.set(
        "linsolve",
        lua.create_function(|lua, (a, b): (Value, Value)| {
            check_dims("linsolve", &a)?;
            check_dims("linsolve", &b)?;
            let Some(m) = matrix(&a) else {
                return Err(mlua::Error::RuntimeError(
                    "linsolve expects a matrix".into(),
                ));
            };
            let Some(v) = vector(&b) else {
                return Err(mlua::Error::RuntimeError(
                    "linsolve expects a vector".into(),
                ));
            };
            if v.len() != m.len() {
                return Err(mlua::Error::RuntimeError(
                    "linsolve shapes don't match".into(),
                ));
            }
            let Some(x) = solve(m, v) else {
                return Err(mlua::Error::RuntimeError(
                    "the matrix is singular (no unique solution)".into(),
                ));
            };
            let t = lua.create_table()?;
            for (i, c) in x.iter().enumerate() {
                t.raw_set(i + 1, to_lua(lua, *c)?)?;
            }
            Ok(Value::Table(t))
        })?,
    )?;
    Ok(num)
}

// ---- random / log ---------------------------------------------------------

fn rand_lib(lua: &Lua) -> LuaResult<Table> {
    let rand = lua.create_table()?;
    rand.set(
        "seeded",
        lua.create_function(|lua, seed: u64| {
            let state = Arc::new(AtomicU64::new(seed.max(1)));
            let table = lua.create_table()?;
            let next_state = Arc::clone(&state);
            table.set(
                "next",
                lua.create_function(move |_, ()| {
                    let mut x = next_state.load(Ordering::Relaxed);
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    next_state.store(x, Ordering::Relaxed);
                    Ok((x >> 11) as f64 / (1u64 << 53) as f64)
                })?,
            )?;
            let range_state = Arc::clone(&state);
            table.set(
                "range",
                lua.create_function(move |_, (lo, hi): (f64, f64)| {
                    let mut x = range_state.load(Ordering::Relaxed);
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    range_state.store(x, Ordering::Relaxed);
                    let r = (x >> 11) as f64 / (1u64 << 53) as f64;
                    Ok(lo + (hi - lo) * r)
                })?,
            )?;
            let int_state = Arc::clone(&state);
            table.set(
                "int",
                lua.create_function(move |_, (lo, hi): (i64, i64)| {
                    let mut x = int_state.load(Ordering::Relaxed);
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    int_state.store(x, Ordering::Relaxed);
                    let span = (hi - lo + 1).max(1) as u64;
                    Ok(lo + ((x >> 11) % span) as i64)
                })?,
            )?;
            Ok(table)
        })?,
    )?;
    Ok(rand)
}

fn log_lib(lua: &Lua) -> LuaResult<Table> {
    let log = lua.create_table()?;
    log.set(
        "debug",
        lua.create_function(|_, value: Value| {
            tracing::debug!(target: "plugin", "{}", lua_value_to_json(&value)?);
            Ok(())
        })?,
    )?;
    Ok(log)
}
