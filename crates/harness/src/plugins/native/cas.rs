//! `cas`: SymPy checks for the tutor's own working (docs/plugins.md §12).
//!
//! `cas_check { code }` runs a short SymPy program with `python3` and
//! returns what it prints. Everything from `sympy` is imported and
//! `x, y, z, t, a, b, c, n, k` are symbols.
//!
//! The same engine is a Rust API for core code that must check math without
//! a model (the problem checker, answer keys):
//! - [`check`] runs a program and returns its output;
//! - [`equivalent`] decides whether two expressions are equal
//!   (`simplify(a - b) == 0`, then SymPy's numeric `equals`), parsing them
//!   with implicit multiplication and `^` as power. The expressions travel
//!   as JSON on stdin, never spliced into the program text.
//!
//! Both are blocking (run them off the async runtime) and fail with
//! [`CasError::Unavailable`] when python3 or SymPy is missing.

use std::sync::Arc;
use std::time::Duration;

use providers::ToolSpec;
use serde_json::{json, Value};

use super::exec::{self, Spec};
use super::{blocking, done, fail, ready, NativePackage};
use crate::plugins::{Plugin, PluginContext, PluginManifest, ToolFuture};

/// Wall-clock limit of one check (importing SymPy alone takes about 1 s).
pub const TIMEOUT: Duration = Duration::from_secs(20);
/// Bytes a check may print.
pub const OUTPUT_CAP: usize = 16 * 1024;
/// Largest program accepted.
pub const MAX_CODE: usize = 16 * 1024;

const PRELUDE: &str =
    "from sympy import *\nx, y, z, t, a, b, c, n, k = symbols('x y z t a b c n k')\n";
const RUNNER: &str =
    "exec(compile(open('check.py', encoding='utf-8').read(), 'check.py', 'exec'), globals())\n";
const EQUIVALENT: &str = r#"import json, sys
from sympy import simplify, Symbol
from sympy.parsing.sympy_parser import parse_expr, standard_transformations, implicit_multiplication_application, convert_xor
T = standard_transformations + (implicit_multiplication_application, convert_xor)
req = json.load(sys.stdin)
try:
    A = parse_expr(req["a"], transformations=T)
    B = parse_expr(req["b"], transformations=T)
except Exception as e:
    print(json.dumps({"result": "unparsed", "detail": f"{type(e).__name__}: {e}"}))
    sys.exit(0)
d = simplify(A - B)
if d == 0:
    print(json.dumps({"result": "equal", "detail": "0"}))
else:
    eq = (A - B).equals(0)
    if eq is True:
        print(json.dumps({"result": "equal", "detail": "numerically 0"}))
    else:
        print(json.dumps({"result": "different", "detail": str(d)}))
"#;

/// Why the CAS couldn't run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CasError {
    /// python3 or SymPy is not installed (the message says which and how).
    Unavailable(String),
    /// It started but failed for another reason (the message says why).
    Failed(String),
}

impl std::fmt::Display for CasError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CasError::Unavailable(m) | CasError::Failed(m) => f.write_str(m),
        }
    }
}

/// What a [`check`] program produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckOutput {
    /// stdout then stderr (a traceback on error), with a note if stopped.
    pub output: String,
    /// It exited cleanly within its limits.
    pub ok: bool,
}

/// The verdict of [`equivalent`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Equivalence {
    Equal,
    /// Not equal; carries `simplify(a - b)`.
    Different(String),
    /// One side didn't parse; carries the parser's message.
    Unparsed(String),
}

fn python() -> Result<std::path::PathBuf, CasError> {
    exec::find_any(&["python3", "python"]).ok_or_else(|| {
        CasError::Unavailable(exec::missing(
            "The CAS",
            &["python3", "python"],
            "Install Python 3 and SymPy (e.g. `pip install sympy`).",
        ))
    })
}

fn sympy_missing(output: &str) -> bool {
    output.contains("No module named 'sympy'")
}

const NO_SYMPY: &str = "SymPy isn't installed for this device's python3. Install it (e.g. `pip install sympy`, or your system's python3-sympy package) and try again.";

fn run(files: &[(&str, &str)], stdin: &str) -> Result<exec::Output, CasError> {
    let spec = Spec::new(python()?)
        .args(["-I", files[0].0])
        .timeout(TIMEOUT)
        .output_cap(OUTPUT_CAP)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("PYTHONIOENCODING", "utf-8");
    let out = exec::run(&spec, files, stdin).map_err(CasError::Failed)?;
    if sympy_missing(&out.stderr) {
        return Err(CasError::Unavailable(NO_SYMPY.into()));
    }
    Ok(out)
}

/// Run a SymPy program (with the prelude) and return what it printed.
pub fn check(code: &str) -> Result<CheckOutput, CasError> {
    if code.len() > MAX_CODE {
        return Err(CasError::Failed(format!(
            "the program is {} bytes (at most {MAX_CODE})",
            code.len()
        )));
    }
    let main = format!("{PRELUDE}{RUNNER}");
    let out = run(&[("main.py", &main), ("check.py", code)], "")?;
    Ok(CheckOutput {
        output: out.combined().trim_end().to_owned(),
        ok: out.success(),
    })
}

/// Are expressions `a` and `b` equal (as SymPy parses them)?
pub fn equivalent(a: &str, b: &str) -> Result<Equivalence, CasError> {
    let input = json!({ "a": a, "b": b }).to_string();
    let out = run(&[("equivalent.py", EQUIVALENT)], &input)?;
    if !out.success() {
        return Err(CasError::Failed(out.combined().trim().to_owned()));
    }
    let v: Value = serde_json::from_str(out.stdout.trim())
        .map_err(|e| CasError::Failed(format!("unexpected CAS output ({e}): {}", out.stdout)))?;
    let detail = v["detail"].as_str().unwrap_or("").to_owned();
    Ok(match v["result"].as_str() {
        Some("equal") => Equivalence::Equal,
        Some("unparsed") => Equivalence::Unparsed(detail),
        _ => Equivalence::Different(detail),
    })
}

pub struct Cas {
    package: NativePackage,
}

impl Cas {
    pub fn new() -> Self {
        Self {
            package: NativePackage::parse(
                include_str!("cas/plugin.toml"),
                &[
                    (
                        "schemas/cas_check.json",
                        include_str!("cas/schemas/cas_check.json"),
                    ),
                    ("prompts/tutor.md", include_str!("cas/prompts/tutor.md")),
                    (
                        "prompts/practice.md",
                        include_str!("cas/prompts/practice.md"),
                    ),
                ],
            )
            .expect("cas package is valid"),
        }
    }
}

impl Default for Cas {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for Cas {
    fn manifest(&self) -> &PluginManifest {
        &self.package.manifest
    }

    fn tools(&self) -> Vec<ToolSpec> {
        self.package.tools.clone()
    }

    fn fragment(&self, agent: &str) -> String {
        self.package.fragment(agent)
    }

    fn call(self: Arc<Self>, name: &str, args: &Value, _ctx: PluginContext) -> ToolFuture {
        if self.package.entry(name) != Some("check") {
            return ready(fail(format!("unknown plugin tool `{name}`")));
        }
        let code = args
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if code.trim().is_empty() {
            return ready(fail("`code` is empty"));
        }
        blocking(move || match check(&code) {
            Ok(out) if out.ok && out.output.is_empty() => {
                done("(no output; print the result you want to see)")
            }
            Ok(out) if out.ok => done(out.output),
            // A Python error is the program's result, for the tutor to read.
            Ok(out) => done(format!("error:\n{}", out.output)),
            Err(e) => fail(e.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn have_sympy() -> bool {
        match check("print(1)") {
            Ok(_) => true,
            Err(e) => {
                eprintln!("skipped: {e}");
                false
            }
        }
    }

    #[test]
    fn a_check_runs_sympy_with_symbols_predefined() {
        if !have_sympy() {
            return;
        }
        let out =
            check("print(simplify(diff(x**3*sin(x), x) - (3*x**2*sin(x) + x**3*cos(x))))").unwrap();
        assert_eq!(
            out,
            CheckOutput {
                output: "0".into(),
                ok: true
            }
        );
        let out = check("print(solve(x**2 - 4, x))").unwrap();
        assert_eq!(out.output, "[-2, 2]");
        let out = check("print(undefined_thing)").unwrap();
        assert!(!out.ok && out.output.contains("NameError"), "{out:?}");
        assert!(out.output.contains("check.py"), "{out:?}");
    }

    #[test]
    fn equivalence_is_decided_by_sympy() {
        if !have_sympy() {
            return;
        }
        assert_eq!(
            equivalent("(x+1)^2", "x^2 + 2x + 1").unwrap(),
            Equivalence::Equal
        );
        assert_eq!(
            equivalent("sin(x)^2 + cos(x)^2", "1").unwrap(),
            Equivalence::Equal
        );
        match equivalent("(x+1)^2", "x^2 + 1").unwrap() {
            Equivalence::Different(d) => assert_eq!(d, "2*x"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            equivalent("x +* 2", "1").unwrap(),
            Equivalence::Unparsed(_)
        ));
        // The expressions are data, not code.
        assert!(matches!(
            equivalent("__import__('os').system('true')", "0").unwrap(),
            Equivalence::Unparsed(_) | Equivalence::Different(_)
        ));
    }

    #[test]
    fn missing_python_or_sympy_is_a_clear_unavailable_error() {
        let sympy_importable = python().is_ok_and(|py| {
            exec::run(&Spec::new(py).args(["-I", "-c", "import sympy"]), &[], "")
                .is_ok_and(|o| o.success())
        });
        if sympy_importable {
            eprintln!(
                "skipped: SymPy is installed (run under a python3 without it to exercise this)"
            );
            return;
        }
        let err = check("print(1)").unwrap_err();
        let CasError::Unavailable(msg) = &err else {
            panic!("{err:?}");
        };
        assert!(msg.contains("SymPy") || msg.contains("python3"), "{msg}");
        let err = equivalent("x", "x").unwrap_err();
        assert!(matches!(err, CasError::Unavailable(_)), "{err:?}");
    }

    #[tokio::test]
    async fn the_tool_returns_output_or_the_error_text() {
        if !have_sympy() {
            return;
        }
        let cas = Arc::new(Cas::new());
        let ctx = PluginContext {
            session_id: "s".into(),
            platform: "desktop".into(),
        };
        let out = Arc::clone(&cas)
            .call(
                "cas_check",
                &json!({ "code": "print(integrate(2*x, x))" }),
                ctx.clone(),
            )
            .await;
        assert_eq!((out.content.as_str(), out.is_error), ("x**2", false));
        let out = Arc::clone(&cas)
            .call("cas_check", &json!({ "code": "1/0" }), ctx)
            .await;
        assert!(
            !out.is_error && out.content.contains("ZeroDivisionError"),
            "{}",
            out.content
        );
    }
}
