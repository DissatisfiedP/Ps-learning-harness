//! `code-runner`: code tasks the learner works in, graded by the tutor's
//! tests (docs/plugins.md §12, `prompts/tutor/modes/procedure.md`).
//!
//! Flow (answer mode, docs/plugins.md §7.4):
//! 1. `code_runner_task` checks the task before the learner sees it — the
//!    `solution` passes every test, a `fix` starter fails one, a `read`
//!    starter prints every `expected`, an `error` starter fails — then
//!    returns an artifact: a card whose view the learner edits and runs in.
//!    The artifact `data` holds the prompt, starter and test inputs, never
//!    the expected outputs or the solution (those stay in the tool input).
//! 2. The tutor asks `question_ask { view: { artifactId, task, target: {} } }`.
//! 3. The view's `run` call runs the learner's code (each run is recorded
//!    per artifact; the prediction sent with the first run is the committed
//!    one). Submitting sends `{ code, prediction, explanation }`.
//! 4. [`Plugin::grade_artifact`] re-runs the submitted code against the
//!    tests from the committed tool input and returns
//!    `{ correct, detail, code, prediction, explanation, runs, tests }`.
//!
//! Languages: python (`python3`), javascript (`node`, else `deno`),
//! typescript (`deno`, else `node --experimental-strip-types`), sql
//! (`sqlite3`, an in-memory database: a test's `input` is setup SQL run
//! before the code) and bash. Programs run through [`exec`] (timeout,
//! output cap, scrubbed env, no network where the OS allows).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use providers::ToolSpec;
use serde::Deserialize;
use serde_json::{json, Value};

use super::exec::{self, Spec};
use super::{blocking, card_svg, fail, ready, NativePackage};
use crate::plugins::{
    ArtifactPayload, Plugin, PluginContext, PluginManifest, ToolFuture, ToolOutcome,
};

/// Wall-clock limit of one run.
pub const RUN_TIMEOUT: Duration = Duration::from_secs(10);
/// Bytes of output one run may print.
pub const RUN_OUTPUT_CAP: usize = 32 * 1024;
/// At most this many tests per task.
pub const MAX_TESTS: usize = 12;
/// Runs remembered per card for the grade.
const MAX_RUNS: usize = 20;
/// Characters of code or output quoted back per run.
const QUOTE: usize = 2000;

/// A language the runner knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Python,
    JavaScript,
    TypeScript,
    Sql,
    Bash,
}

impl Lang {
    pub const ALL: [Lang; 5] = [
        Lang::Python,
        Lang::JavaScript,
        Lang::TypeScript,
        Lang::Sql,
        Lang::Bash,
    ];

    pub fn parse(name: &str) -> Result<Self, String> {
        Ok(match name.trim().to_ascii_lowercase().as_str() {
            "python" | "python3" | "py" => Lang::Python,
            "javascript" | "js" | "node" => Lang::JavaScript,
            "typescript" | "ts" => Lang::TypeScript,
            "sql" | "sqlite" | "sqlite3" => Lang::Sql,
            "bash" | "sh" | "shell" => Lang::Bash,
            other => {
                return Err(format!(
                    "`language` `{other}` isn't supported; use one of: python, javascript, typescript, sql, bash"
                ))
            }
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Lang::Python => "python",
            Lang::JavaScript => "javascript",
            Lang::TypeScript => "typescript",
            Lang::Sql => "sql",
            Lang::Bash => "bash",
        }
    }

    fn programs(self) -> &'static [&'static str] {
        match self {
            Lang::Python => &["python3", "python"],
            Lang::JavaScript => &["node", "deno"],
            Lang::TypeScript => &["deno", "node"],
            Lang::Sql => &["sqlite3"],
            Lang::Bash => &["bash"],
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Lang::Python => "Install Python 3.",
            Lang::JavaScript => "Install Node.js or Deno.",
            Lang::TypeScript => "Install Deno (or Node.js 22.6 or later).",
            Lang::Sql => "Install the sqlite3 command-line shell.",
            Lang::Bash => "Install bash.",
        }
    }

    /// The installed toolchain, or the tool error naming what is missing.
    pub fn toolchain(self) -> Result<PathBuf, String> {
        exec::find_any(self.programs()).ok_or_else(|| {
            exec::missing(
                &format!("The {} toolchain", self.name()),
                self.programs(),
                self.hint(),
            )
        })
    }

    /// Languages whose toolchain is on this device.
    pub fn installed() -> Vec<Lang> {
        Lang::ALL
            .into_iter()
            .filter(|l| l.toolchain().is_ok())
            .collect()
    }
}

/// What one run produced (returned to the view and kept for the grade).
#[derive(Debug, Clone, PartialEq)]
pub struct RunResult {
    pub output: String,
    pub exit: Option<i32>,
    pub timed_out: bool,
    pub truncated: bool,
}

impl RunResult {
    fn ok(&self) -> bool {
        self.exit == Some(0) && !self.timed_out && !self.truncated
    }

    fn json(&self) -> Value {
        json!({
            "output": self.output,
            "exit": self.exit,
            "timedOut": self.timed_out,
            "truncated": self.truncated,
        })
    }
}

/// Run `code` in `lang` with `stdin` (for sql, setup statements run first).
/// `Err` only when the toolchain is missing or can't start.
pub fn run_code(lang: Lang, code: &str, stdin: &str) -> Result<RunResult, String> {
    let program = lang.toolchain()?;
    let tool = program
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let spec = Spec::new(&program)
        .timeout(RUN_TIMEOUT)
        .output_cap(RUN_OUTPUT_CAP);
    let (spec, file, input) = match lang {
        Lang::Python => (
            spec.args(["-I", "main.py"])
                .env("PYTHONDONTWRITEBYTECODE", "1")
                .env("PYTHONIOENCODING", "utf-8"),
            ("main.py", code),
            stdin.to_owned(),
        ),
        Lang::JavaScript if tool == "deno" => (
            spec.args(["run", "--quiet", "--no-prompt", "main.js"])
                .env("DENO_NO_UPDATE_CHECK", "1"),
            ("main.js", code),
            stdin.to_owned(),
        ),
        Lang::JavaScript => (spec.args(["main.js"]), ("main.js", code), stdin.to_owned()),
        Lang::TypeScript if tool == "deno" => (
            spec.args(["run", "--quiet", "--no-prompt", "main.ts"])
                .env("DENO_NO_UPDATE_CHECK", "1"),
            ("main.ts", code),
            stdin.to_owned(),
        ),
        Lang::TypeScript => (
            spec.args(["--experimental-strip-types", "--no-warnings", "main.ts"]),
            ("main.ts", code),
            stdin.to_owned(),
        ),
        Lang::Sql => (
            spec.args(["-safe", "-batch", ":memory:"]),
            ("main.sql", code),
            format!("{stdin}\n;\n{code}\n;\n"),
        ),
        Lang::Bash => (spec.args(["main.sh"]), ("main.sh", code), stdin.to_owned()),
    };
    let out = exec::run(&spec, &[file], &input)?;
    Ok(RunResult {
        output: out.combined(),
        exit: out.code,
        timed_out: out.timed_out,
        truncated: out.truncated,
    })
}

/// Output compared the way tests compare it: line endings unified,
/// trailing spaces on each line and trailing blank lines dropped.
pub fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n")
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end_matches('\n')
        .trim_start_matches('\n')
        .to_owned()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestCase {
    pub name: String,
    #[serde(default)]
    pub input: String,
    pub expected: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Write,
    Fix,
    Read,
    Error,
}

/// The committed tool input.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Task {
    pub language: String,
    pub kind: Kind,
    pub prompt: String,
    #[serde(default)]
    pub starter: Option<String>,
    #[serde(default)]
    pub solution: Option<String>,
    pub tests: Vec<TestCase>,
    #[serde(default)]
    pub predict_first: bool,
}

/// One test's outcome.
#[derive(Debug, Clone)]
pub struct TestResult {
    pub name: String,
    pub passed: bool,
    pub expected: String,
    pub actual: String,
    pub exit: Option<i32>,
}

impl TestResult {
    fn json(&self) -> Value {
        json!({
            "name": self.name,
            "passed": self.passed,
            "expected": self.expected,
            "actual": exec::clip(&self.actual, QUOTE),
            "exit": self.exit,
        })
    }
}

/// Run `code` against every test: a test passes when the program exits 0
/// within its limits and its normalized stdout+stderr equals `expected`.
pub fn run_tests(lang: Lang, code: &str, tests: &[TestCase]) -> Result<Vec<TestResult>, String> {
    tests
        .iter()
        .map(|t| {
            let r = run_code(lang, code, &t.input)?;
            Ok(TestResult {
                name: t.name.clone(),
                passed: r.ok() && normalize(&r.output) == normalize(&t.expected),
                expected: t.expected.clone(),
                actual: r.output,
                exit: r.exit,
            })
        })
        .collect()
}

fn failures(results: &[TestResult]) -> String {
    results
        .iter()
        .filter(|r| !r.passed)
        .map(|r| {
            format!(
                "- {}: expected {:?}, got {:?}{}",
                r.name,
                normalize(&r.expected),
                exec::clip(&normalize(&r.actual), 400),
                match r.exit {
                    Some(0) => String::new(),
                    Some(c) => format!(" (exit {c})"),
                    None => " (stopped)".into(),
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl Task {
    fn parse(args: &Value) -> Result<(Self, Lang), String> {
        let task: Task =
            serde_json::from_value(args.clone()).map_err(|e| format!("bad arguments: {e}"))?;
        let lang = Lang::parse(&task.language)?;
        if task.prompt.trim().is_empty() {
            return Err("`prompt` is empty".into());
        }
        if task.tests.len() > MAX_TESTS {
            return Err(format!("{} tests (at most {MAX_TESTS})", task.tests.len()));
        }
        let starter = task.starter.as_deref().unwrap_or("").trim();
        match task.kind {
            Kind::Write | Kind::Fix if task.tests.is_empty() => {
                return Err("a write or fix task needs at least one test".into())
            }
            Kind::Read if task.tests.is_empty() => {
                return Err(
                    "a read task needs a test whose `expected` is the starter's output".into(),
                )
            }
            Kind::Fix | Kind::Read | Kind::Error if starter.is_empty() => {
                return Err(format!("a {:?} task needs `starter` code", task.kind).to_lowercase())
            }
            _ => {}
        }
        Ok((task, lang))
    }

    /// The pre-flight check: everything the tutor committed must hold
    /// before the learner sees the task. Returns the error output for an
    /// `error` task.
    fn verify(&self, lang: Lang) -> Result<Option<String>, String> {
        let starter = self.starter.as_deref().unwrap_or("");
        if let Some(solution) = self.solution.as_deref().filter(|s| !s.trim().is_empty()) {
            let results = run_tests(lang, solution, &self.tests)?;
            if results.iter().any(|r| !r.passed) {
                return Err(format!(
                    "your solution fails your own tests, so the task wasn't shown; fix the tests or the solution:\n{}",
                    failures(&results)
                ));
            }
        }
        match self.kind {
            Kind::Write => Ok(None),
            Kind::Fix => {
                let results = run_tests(lang, starter, &self.tests)?;
                if results.iter().all(|r| r.passed) {
                    return Err("the starter already passes every test; a fix task needs a test the starter fails".into());
                }
                Ok(None)
            }
            Kind::Read => {
                let results = run_tests(lang, starter, &self.tests)?;
                if results.iter().any(|r| !r.passed) {
                    return Err(format!(
                        "the starter doesn't print what your tests expect, so the task wasn't shown; fix `expected`:\n{}",
                        failures(&results)
                    ));
                }
                Ok(None)
            }
            Kind::Error => {
                let stdin = self.tests.first().map_or("", |t| t.input.as_str());
                let r = run_code(lang, starter, stdin)?;
                if r.ok() {
                    return Err(format!(
                        "the starter runs without an error (it printed {:?}); an error task needs code that fails",
                        exec::clip(&r.output, 400)
                    ));
                }
                Ok(Some(exec::clip(&r.output, QUOTE)))
            }
        }
    }
}

/// One recorded run of a card.
#[derive(Debug, Clone)]
struct Run {
    code: String,
    stdin: String,
    result: RunResult,
}

#[derive(Default)]
struct Card {
    runs: Vec<Run>,
    /// The prediction sent with the first run (committed before any output).
    committed_prediction: Option<String>,
}

pub struct CodeRunner {
    package: NativePackage,
    cards: Mutex<HashMap<String, Card>>,
}

impl CodeRunner {
    pub fn new() -> Self {
        let mut package = NativePackage::parse(
            include_str!("code-runner/plugin.toml"),
            &[
                (
                    "schemas/code_runner_task.json",
                    include_str!("code-runner/schemas/code_runner_task.json"),
                ),
                (
                    "prompts/tutor.md",
                    include_str!("code-runner/prompts/tutor.md"),
                ),
                (
                    "prompts/practice.md",
                    include_str!("code-runner/prompts/practice.md"),
                ),
                (
                    "views/task.html",
                    include_str!("code-runner/views/task.html"),
                ),
            ],
        )
        .expect("code-runner package is valid");
        let installed: Vec<&str> = Lang::installed().into_iter().map(Lang::name).collect();
        package.manifest.use_when = format!(
            "{} Languages on this device: {}.",
            package.manifest.use_when,
            if installed.is_empty() {
                "none found".to_owned()
            } else {
                installed.join(", ")
            }
        );
        Self {
            package,
            cards: Mutex::new(HashMap::new()),
        }
    }

    fn task(args: &Value) -> ToolOutcome {
        let (task, lang) = match Task::parse(args) {
            Ok(t) => t,
            Err(e) => return fail(e),
        };
        let error_output = match task.verify(lang) {
            Ok(e) => e,
            Err(e) => return fail(e),
        };
        let data = json!({
            "language": lang.name(),
            "kind": format!("{:?}", task.kind).to_lowercase(),
            "prompt": task.prompt,
            "starter": task.starter.clone().unwrap_or_default(),
            "predictFirst": task.predict_first,
            "tests": task.tests.iter().map(|t| json!({ "name": t.name, "input": t.input })).collect::<Vec<_>>(),
            "errorOutput": error_output,
        });
        let kind = format!("{:?}", task.kind).to_lowercase();
        let mut lines = vec![task.prompt.clone()];
        if let Some(s) = task.starter.as_deref().filter(|s| !s.is_empty()) {
            lines.push(String::new());
            lines.extend(s.lines().take(12).map(str::to_owned));
        }
        let svg = card_svg(&format!("Code task · {} · {kind}", lang.name()), &lines);
        let tests = task.tests.len();
        ToolOutcome {
            content: format!(
                "Task checked ({tests} test{}{}). Place the card, then ask with question_ask {{ view: {{ artifactId, task, target: {{}} }} }}.",
                if tests == 1 { "" } else { "s" },
                if task.solution.is_some() { ", your solution passes" } else { "" }
            ),
            is_error: false,
            artifact: Some(ArtifactPayload {
                kind: "svg".into(),
                svg,
                data,
            }),
        }
    }

    fn run_call(&self, args: &Value) -> Result<Value, String> {
        let artifact_id = args
            .get("artifactId")
            .and_then(Value::as_str)
            .ok_or("artifactId is required")?
            .to_owned();
        let lang = Lang::parse(args.get("language").and_then(Value::as_str).unwrap_or(""))?;
        let code = args
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let stdin = args
            .get("stdin")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let prediction = args.get("prediction").and_then(Value::as_str).unwrap_or("");
        let result = run_code(lang, &code, &stdin)?;
        let mut cards = self.cards.lock().unwrap_or_else(|e| e.into_inner());
        let card = cards.entry(artifact_id).or_default();
        if card.runs.is_empty() && !prediction.trim().is_empty() {
            card.committed_prediction = Some(prediction.to_owned());
        }
        if card.runs.len() >= MAX_RUNS {
            card.runs.remove(0);
        }
        card.runs.push(Run {
            code,
            stdin,
            result: result.clone(),
        });
        Ok(result.json())
    }

    fn grade_task(
        &self,
        answer: &Value,
        artifact_id: &str,
        input: &Value,
    ) -> Result<Value, String> {
        let (task, lang) = Task::parse(input)?;
        let text = |k: &str| {
            answer
                .get(k)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        let starter = task.starter.clone().unwrap_or_default();
        let code = match task.kind {
            // Read and error tasks are not edited.
            Kind::Read | Kind::Error => starter.clone(),
            Kind::Write | Kind::Fix => text("code"),
        };
        let (runs, committed) = {
            let cards = self.cards.lock().unwrap_or_else(|e| e.into_inner());
            cards.get(artifact_id).map_or((Vec::new(), None), |c| {
                (c.runs.clone(), c.committed_prediction.clone())
            })
        };
        let prediction = committed.unwrap_or_else(|| text("prediction"));
        let explanation = text("explanation");
        let runs_json: Vec<Value> = runs
            .iter()
            .map(|r| {
                json!({
                    "code": if r.code == code { Value::Null } else { Value::String(exec::clip(&r.code, QUOTE)) },
                    "stdin": exec::clip(&r.stdin, 400),
                    "output": exec::clip(&r.result.output, QUOTE),
                    "exit": r.result.exit,
                    "timedOut": r.result.timed_out,
                })
            })
            .collect();
        let (correct, detail, tests) = match task.kind {
            Kind::Write | Kind::Fix => {
                if code.trim().is_empty() {
                    (json!(false), "no code submitted".to_owned(), Vec::new())
                } else {
                    let results = run_tests(lang, &code, &task.tests)?;
                    let passed = results.iter().filter(|r| r.passed).count();
                    let detail = if passed == results.len() {
                        format!("all {passed} tests pass")
                    } else {
                        format!(
                            "{passed} of {} tests pass:\n{}",
                            results.len(),
                            failures(&results)
                        )
                    };
                    (json!(passed == results.len()), detail, results)
                }
            }
            Kind::Read => {
                let expected = task
                    .tests
                    .first()
                    .map(|t| t.expected.as_str())
                    .unwrap_or("");
                let ok = normalize(&prediction) == normalize(expected);
                let detail = if ok {
                    "the prediction matches the output".to_owned()
                } else {
                    format!(
                        "predicted {:?}; the program prints {:?}",
                        normalize(&prediction),
                        normalize(expected)
                    )
                };
                (json!(ok), detail, Vec::new())
            }
            Kind::Error => (
                Value::Null,
                "judge the explanation yourself against the error".to_owned(),
                Vec::new(),
            ),
        };
        let mut out = json!({
            "correct": correct,
            "detail": detail,
            "language": lang.name(),
            "kind": format!("{:?}", task.kind).to_lowercase(),
            "code": exec::clip(&code, 8 * 1024),
            "runs": runs_json,
            "tests": tests.iter().map(TestResult::json).collect::<Vec<_>>(),
        });
        if task.predict_first || task.kind == Kind::Read {
            out["prediction"] = json!(prediction);
            out["predictionCommittedBeforeRunning"] =
                json!(!runs.is_empty() || !prediction.is_empty());
        }
        if task.kind == Kind::Error {
            out["explanation"] = json!(explanation);
        }
        Ok(out)
    }
}

impl Default for CodeRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for CodeRunner {
    fn manifest(&self) -> &PluginManifest {
        &self.package.manifest
    }

    fn tools(&self) -> Vec<ToolSpec> {
        self.package.tools.clone()
    }

    fn fragment(&self, agent: &str) -> String {
        self.package.fragment(agent)
    }

    fn view_html(&self) -> Result<String, String> {
        self.package.view_html()
    }

    fn view_call(&self, name: &str, args: &Value) -> Result<Value, String> {
        self.package.check_view_call(name)?;
        self.run_call(args)
    }

    fn grade_artifact(
        &self,
        answer: &Value,
        _target: &Value,
        artifact_id: &str,
        input: &Value,
    ) -> Result<Value, String> {
        self.grade_task(answer, artifact_id, input)
    }

    fn call(self: Arc<Self>, name: &str, args: &Value, _ctx: PluginContext) -> ToolFuture {
        match self.package.entry(name) {
            Some("task") => {
                let args = args.clone();
                blocking(move || Self::task(&args))
            }
            _ => ready(fail(format!("unknown plugin tool `{name}`"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn have(lang: Lang) -> bool {
        let ok = lang.toolchain().is_ok();
        if !ok {
            eprintln!("skipped: no {} toolchain on PATH", lang.name());
        }
        ok
    }

    fn task(args: Value) -> ToolOutcome {
        CodeRunner::task(&args)
    }

    #[test]
    fn python_runs_with_stdin_and_reports_errors() {
        if !have(Lang::Python) {
            return;
        }
        let r = run_code(Lang::Python, "print(int(input()) * 2)", "21\n").unwrap();
        assert_eq!((r.output.trim(), r.exit), ("42", Some(0)));
        let r = run_code(Lang::Python, "1/0", "").unwrap();
        assert_ne!(r.exit, Some(0));
        assert!(r.output.contains("ZeroDivisionError"), "{}", r.output);
        let r = run_code(Lang::Python, "while True: pass", "").unwrap();
        assert!(r.timed_out);
    }

    #[test]
    fn sql_runs_setup_then_the_query() {
        if !have(Lang::Sql) {
            return;
        }
        let r = run_code(
            Lang::Sql,
            "SELECT name FROM t ORDER BY n DESC LIMIT 1",
            "CREATE TABLE t(name, n); INSERT INTO t VALUES ('a', 1), ('b', 3);",
        )
        .unwrap();
        assert_eq!(r.output.trim(), "b");
        // -safe: the shell's escape hatches are off.
        let r = run_code(Lang::Sql, ".shell echo pwned", "").unwrap();
        assert!(
            !r.output.lines().any(|l| l.trim() == "pwned"),
            "{}",
            r.output
        );
    }

    #[test]
    fn a_task_whose_solution_fails_its_tests_is_refused() {
        if !have(Lang::Python) {
            return;
        }
        let base = json!({
            "language": "python", "kind": "write", "prompt": "Double the number on stdin.",
            "solution": "print(int(input()) * 2)",
            "tests": [{ "name": "two", "input": "2", "expected": "4" }, { "name": "neg", "input": "-3", "expected": "-6" }],
        });
        let ok = task(base.clone());
        assert!(!ok.is_error, "{}", ok.content);
        let art = ok.artifact.unwrap();
        // The learner's card never carries the expected outputs or solution.
        let data = art.data.to_string();
        assert!(!data.contains("-6") && !data.contains("* 2"), "{data}");
        let mut wrong = base.clone();
        wrong["tests"][1]["expected"] = json!("6");
        let bad = task(wrong);
        assert!(
            bad.is_error && bad.content.contains("neg"),
            "{}",
            bad.content
        );
    }

    #[test]
    fn fix_read_and_error_tasks_are_checked_before_the_learner_sees_them() {
        if !have(Lang::Python) {
            return;
        }
        let fix_passing = task(json!({
            "language": "python", "kind": "fix", "prompt": "p", "starter": "print(4)",
            "tests": [{ "name": "t", "expected": "4" }],
        }));
        assert!(fix_passing.is_error && fix_passing.content.contains("already passes"));
        let read_wrong = task(json!({
            "language": "python", "kind": "read", "prompt": "p", "starter": "print(sorted([3,1,2]))",
            "tests": [{ "name": "t", "expected": "[3, 2, 1]" }],
        }));
        assert!(
            read_wrong.is_error && read_wrong.content.contains("[1, 2, 3]"),
            "{}",
            read_wrong.content
        );
        let error_ok = task(json!({
            "language": "python", "kind": "error", "prompt": "Why?", "starter": "x = [1]\nprint(x[3])",
            "tests": [],
        }));
        assert!(!error_ok.is_error, "{}", error_ok.content);
        let data = error_ok.artifact.unwrap().data;
        assert!(data["errorOutput"].as_str().unwrap().contains("IndexError"));
        let error_runs = task(json!({
            "language": "python", "kind": "error", "prompt": "Why?", "starter": "print(1)", "tests": [],
        }));
        assert!(error_runs.is_error && error_runs.content.contains("without an error"));
    }

    #[test]
    fn grading_runs_the_submitted_code_against_the_committed_tests() {
        if !have(Lang::Python) {
            return;
        }
        let input = json!({
            "language": "python", "kind": "fix", "prompt": "Fix the sum.",
            "starter": "a, b = map(int, input().split())\nprint(a - b)",
            "tests": [{ "name": "small", "input": "2 3", "expected": "5" }, { "name": "zero", "input": "0 0", "expected": "0" }],
        });
        let runner = CodeRunner::new();
        // A run from the card is recorded and returned with the grade.
        let run = runner
            .view_call("run", &json!({ "artifactId": "A1", "language": "python", "code": "print(1)", "stdin": "" }))
            .unwrap();
        assert_eq!(run["exit"], json!(0));
        let wrong = runner
            .grade_artifact(
                &json!({ "code": "a, b = map(int, input().split())\nprint(a - b)" }),
                &json!({}),
                "A1",
                &input,
            )
            .unwrap();
        assert_eq!(wrong["correct"], json!(false));
        assert!(
            wrong["detail"].as_str().unwrap().contains("1 of 2"),
            "{wrong}"
        );
        assert_eq!(wrong["runs"].as_array().unwrap().len(), 1);
        let right = runner
            .grade_artifact(
                &json!({ "code": "a, b = map(int, input().split())\nprint(a + b)" }),
                &json!({}),
                "A1",
                &input,
            )
            .unwrap();
        assert_eq!(right["correct"], json!(true), "{right}");
    }

    #[test]
    fn a_read_prediction_is_the_one_committed_before_the_first_run() {
        if !have(Lang::Python) {
            return;
        }
        let input = json!({
            "language": "python", "kind": "read", "prompt": "What prints?", "starter": "print(len('abc') * 2)",
            "tests": [{ "name": "out", "expected": "6" }],
        });
        let runner = CodeRunner::new();
        runner
            .view_call("run", &json!({ "artifactId": "R", "language": "python", "code": "print(len('abc') * 2)", "prediction": "3" }))
            .unwrap();
        // Changing the prediction after seeing the output doesn't count.
        let g = runner
            .grade_artifact(&json!({ "prediction": "6" }), &json!({}), "R", &input)
            .unwrap();
        assert_eq!(g["correct"], json!(false), "{g}");
        assert_eq!(g["prediction"], json!("3"));
    }

    #[test]
    fn a_missing_toolchain_is_a_clear_tool_error() {
        let missing = Lang::ALL.into_iter().find(|l| l.toolchain().is_err());
        let Some(lang) = missing else {
            eprintln!("skipped: every toolchain is installed");
            return;
        };
        let out = task(json!({
            "language": lang.name(), "kind": "write", "prompt": "p",
            "solution": "x", "tests": [{ "name": "t", "expected": "1" }],
        }));
        assert!(out.is_error);
        assert!(
            out.content.contains("isn't available on this device"),
            "{}",
            out.content
        );
        assert!(out.content.contains(lang.programs()[0]), "{}", out.content);
    }

    #[test]
    fn unknown_language_and_missing_starter_are_refused() {
        let out = task(
            json!({ "language": "cobol", "kind": "write", "prompt": "p", "tests": [{ "name": "t", "expected": "1" }] }),
        );
        assert!(out.is_error && out.content.contains("cobol"));
        let out = task(
            json!({ "language": "python", "kind": "fix", "prompt": "p", "tests": [{ "name": "t", "expected": "1" }] }),
        );
        assert!(
            out.is_error && out.content.contains("starter"),
            "{}",
            out.content
        );
    }

    #[test]
    fn normalize_ignores_trailing_space_and_line_endings_only() {
        assert_eq!(normalize("a  \r\nb\n\n"), "a\nb");
        assert_ne!(normalize("a b"), normalize("ab"));
    }
}
