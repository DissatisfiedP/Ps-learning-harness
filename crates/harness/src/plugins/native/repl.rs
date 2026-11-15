//! `repl`: real evaluation in local toolchains (docs/plugins.md §12).
//!
//! - `repl_eval { lang, input }` runs in the tutor's own session for that
//!   language in this chat session; definitions persist between calls.
//! - `repl_task { lang, prompt, check }` makes a REPL card (answer-mode view,
//!   docs/plugins.md §7.4). The learner's inputs go to a session of its own
//!   (one per card, the view's `input` call); grading
//!   ([`Plugin::grade_artifact`]) evaluates the committed `check` in that
//!   session's final state and returns the transcript with the result.
//!
//! Engines:
//! - **python** (`python3`) and **javascript** (`node`): one persistent
//!   process running a small driver (`repl/drivers/`) that evaluates each
//!   input in a kept namespace and echoes the last expression's value;
//! - **sql** (`sqlite3`, in memory): every input is kept and the whole
//!   history replayed in a fresh `sqlite3 -safe` per input, printing only
//!   the new input's output (deterministic statements give the same state);
//! - **nix** (`nix-instantiate --eval --strict`): `name = expr` inputs
//!   become `let` bindings for every later input and check.
//!
//! Each input has a 10 s limit (nix 30 s); a session past its limit is
//! stopped and starts empty on the next input, which the output says. At
//! most [`MAX_LIVE`] sessions are kept; the least recently used goes first.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use providers::ToolSpec;
use serde_json::{json, Value};

use super::exec::{self, Persistent, Spec};
use super::{blocking, card_svg, done, fail, ready, NativePackage};
use crate::plugins::{
    ArtifactPayload, Plugin, PluginContext, PluginManifest, ToolFuture, ToolOutcome,
};

const PYTHON_DRIVER: &str = include_str!("repl/drivers/python.py");
const NODE_DRIVER: &str = include_str!("repl/drivers/node.js");
const RES: &str = "\u{1e}LHRES\u{1e}";
const END: &str = "\u{1e}LHEND\u{1e}";
/// A function sqlite doesn't have: calling it puts an error naming it on
/// stderr, which marks where the replayed history's errors end.
const SQL_END: &str = "lh_end_of_replay";
/// Time one input may take.
pub const INPUT_TIMEOUT: Duration = Duration::from_secs(10);
const NIX_TIMEOUT: Duration = Duration::from_secs(30);
/// Bytes one input may print.
pub const INPUT_OUTPUT_CAP: usize = 32 * 1024;
/// Live sessions kept (tutor and card sessions together).
pub const MAX_LIVE: usize = 8;
/// Transcript entries kept per session.
const MAX_TRANSCRIPT: usize = 60;
/// Characters of an input or output quoted back.
const QUOTE: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    Python,
    JavaScript,
    Sql,
    Nix,
}

impl Lang {
    pub const ALL: [Lang; 4] = [Lang::Python, Lang::JavaScript, Lang::Sql, Lang::Nix];

    pub fn parse(name: &str) -> Result<Self, String> {
        Ok(match name.trim().to_ascii_lowercase().as_str() {
            "python" | "python3" | "py" => Lang::Python,
            "javascript" | "js" | "node" => Lang::JavaScript,
            "sql" | "sqlite" | "sqlite3" => Lang::Sql,
            "nix" => Lang::Nix,
            other => {
                return Err(format!(
                    "`lang` `{other}` isn't supported; use one of: python, javascript, sql, nix"
                ))
            }
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Lang::Python => "python",
            Lang::JavaScript => "javascript",
            Lang::Sql => "sql",
            Lang::Nix => "nix",
        }
    }

    fn programs(self) -> &'static [&'static str] {
        match self {
            Lang::Python => &["python3", "python"],
            Lang::JavaScript => &["node"],
            Lang::Sql => &["sqlite3"],
            Lang::Nix => &["nix-instantiate"],
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Lang::Python => "Install Python 3.",
            Lang::JavaScript => "Install Node.js.",
            Lang::Sql => "Install the sqlite3 command-line shell.",
            Lang::Nix => "Install Nix.",
        }
    }

    pub fn toolchain(self) -> Result<PathBuf, String> {
        exec::find_any(self.programs()).ok_or_else(|| {
            exec::missing(
                &format!("The {} REPL", self.name()),
                self.programs(),
                self.hint(),
            )
        })
    }

    pub fn installed() -> Vec<Lang> {
        Lang::ALL
            .into_iter()
            .filter(|l| l.toolchain().is_ok())
            .collect()
    }
}

/// What one input produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    /// What it printed, then its value (if any) or its error.
    pub output: String,
    /// False when it raised an error.
    pub ok: bool,
    /// Set when the session was stopped or restarted.
    pub note: Option<String>,
}

/// The result of evaluating a check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
    pub passed: bool,
    /// The check's value as the language prints it, or its error.
    pub value: String,
}

enum Engine {
    /// python/javascript driver process; `None` once stopped.
    Driver(Option<Persistent>),
    /// sqlite: every input so far.
    Sql(Vec<String>),
    /// nix: `let` bindings so far.
    Nix(Vec<(String, String)>),
}

/// One REPL session.
pub struct Session {
    lang: Lang,
    program: PathBuf,
    engine: Engine,
    transcript: Vec<(String, Reply)>,
    used: Instant,
}

const STOPPED: &str = "[the session was stopped (time or output limit); the next input starts a fresh one, so earlier definitions are gone]";

impl Session {
    pub fn new(lang: Lang) -> Result<Self, String> {
        let program = lang.toolchain()?;
        let engine = match lang {
            Lang::Python | Lang::JavaScript => Engine::Driver(None),
            Lang::Sql => Engine::Sql(Vec::new()),
            Lang::Nix => Engine::Nix(Vec::new()),
        };
        Ok(Self {
            lang,
            program,
            engine,
            transcript: Vec::new(),
            used: Instant::now(),
        })
    }

    fn driver(&self) -> Result<Persistent, String> {
        let (spec, file) = match self.lang {
            Lang::Python => (
                Spec::new(&self.program)
                    .args(["-I", "-u", "driver.py"])
                    .env("PYTHONDONTWRITEBYTECODE", "1")
                    .env("PYTHONIOENCODING", "utf-8"),
                ("driver.py", PYTHON_DRIVER),
            ),
            _ => (
                Spec::new(&self.program).args(["driver.js"]),
                ("driver.js", NODE_DRIVER),
            ),
        };
        // The CPU limit covers the session's whole life.
        Persistent::spawn(&spec.timeout(Duration::from_secs(600)), &[file])
    }

    /// One driver request: what the input printed, the driver's JSON reply
    /// (`None` when the session had to be stopped), and a note when the
    /// session was stopped or restarted.
    fn request(
        &mut self,
        op: &str,
        code: &str,
    ) -> Result<(String, Option<Value>, Option<String>), String> {
        let (dead, missing) = match &mut self.engine {
            Engine::Driver(Some(p)) => (!p.alive(), false),
            Engine::Driver(None) => (false, true),
            _ => unreachable!("driver request on a non-driver engine"),
        };
        let note = dead.then(|| STOPPED.to_owned());
        if dead || missing {
            let fresh = self.driver()?;
            self.engine = Engine::Driver(Some(fresh));
        }
        let Engine::Driver(Some(p)) = &mut self.engine else {
            unreachable!()
        };
        let line = format!("{}\n", json!({ "op": op, "code": code }));
        let ex = p.exchange(&line, END, INPUT_TIMEOUT, INPUT_OUTPUT_CAP)?;
        if ex.timed_out || ex.truncated {
            if let Engine::Driver(slot) = &mut self.engine {
                *slot = None;
            }
            let why = if ex.timed_out {
                format!(
                    "[stopped: the input ran past {} s]",
                    INPUT_TIMEOUT.as_secs()
                )
            } else {
                "[stopped: output limit reached]".to_owned()
            };
            return Ok((
                format!("{}\n{why}", ex.output.trim_end()),
                None,
                Some(STOPPED.to_owned()),
            ));
        }
        let (printed, result) = match ex.output.rfind(RES) {
            Some(at) => (
                ex.output[..at].to_owned(),
                serde_json::from_str::<Value>(ex.output[at + RES.len()..].trim()).ok(),
            ),
            None => (ex.output, None),
        };
        Ok((printed, result, note))
    }

    fn nix_let(bindings: &[(String, String)], body: &str) -> String {
        let mut s = String::from("let\n");
        for (n, v) in bindings {
            s.push_str(&format!("  {n} = {v};\n"));
        }
        s.push_str(&format!("in (\n{body}\n)"));
        s
    }

    fn nix_eval(&self, expr: &str) -> Result<exec::Output, String> {
        let mut spec = Spec::new(&self.program)
            .args(["--eval", "--strict", "--expr", expr])
            .timeout(NIX_TIMEOUT)
            .output_cap(INPUT_OUTPUT_CAP);
        if let Ok(path) = std::env::var("NIX_PATH") {
            spec = spec.env("NIX_PATH", &path);
        }
        exec::run(&spec, &[], "")
    }

    /// Replays `history`, then runs `tail`, in a fresh in-memory database.
    /// The replayed inputs print and fail again as they first did, so both
    /// streams of the returned output are cut down to what `tail` produced:
    /// stdout after a printed marker, stderr after the error of a call to
    /// [`SQL_END`] (the shell can't print to stderr in safe mode).
    fn sql_run(&self, history: &[String], tail: &str) -> Result<exec::Output, String> {
        let mut script = String::new();
        for h in history {
            script.push_str(h);
            script.push_str("\n;\n");
        }
        script.push_str(&format!(".print {END}\nSELECT {SQL_END}();\n{tail}\n;\n"));
        let spec = Spec::new(&self.program)
            .args(["-safe", "-batch", ":memory:"])
            .timeout(INPUT_TIMEOUT)
            .output_cap(INPUT_OUTPUT_CAP * 4);
        let mut out = exec::run(&spec, &[], &script)?;
        if let Some(at) = out.stdout.find(END) {
            out.stdout.replace_range(..at + END.len(), "");
        }
        out.stderr = after_sql_end(&out.stderr).to_owned();
        Ok(out)
    }

    /// Evaluate one input and record it in the transcript.
    pub fn eval(&mut self, input: &str) -> Result<Reply, String> {
        self.used = Instant::now();
        let reply = match self.lang {
            Lang::Python | Lang::JavaScript => {
                let (printed, result, note) = self.request("eval", input)?;
                let mut output = printed;
                let ok = match result {
                    Some(r) if r["ok"] == json!(true) => {
                        if let Some(v) = r["value"].as_str() {
                            push_line(&mut output, v);
                        }
                        true
                    }
                    Some(r) => {
                        push_line(&mut output, r["error"].as_str().unwrap_or("error"));
                        false
                    }
                    None => false,
                };
                Reply {
                    output: output.trim_end().to_owned(),
                    ok,
                    note,
                }
            }
            Lang::Sql => {
                let Engine::Sql(history) = &self.engine else {
                    unreachable!()
                };
                let out = self.sql_run(history, input)?;
                let after = out.combined().trim_matches('\n').to_owned();
                let ok = !sql_error(&after) && !out.timed_out;
                if !out.timed_out {
                    if let Engine::Sql(history) = &mut self.engine {
                        history.push(input.to_owned());
                    }
                }
                Reply {
                    output: after,
                    ok,
                    note: out.timed_out.then(|| {
                        "[stopped: time limit reached; the input was not kept]".to_owned()
                    }),
                }
            }
            Lang::Nix => {
                let Engine::Nix(bindings) = &self.engine else {
                    unreachable!()
                };
                let mut bindings = bindings.clone();
                let reply = match nix_binding(input) {
                    Some((name, value)) => {
                        bindings.retain(|(n, _)| n != &name);
                        bindings.push((name.clone(), value));
                        let out = self.nix_eval(&Self::nix_let(&bindings, &name))?;
                        if out.success() {
                            if let Engine::Nix(kept) = &mut self.engine {
                                *kept = bindings;
                            }
                            Reply {
                                output: format!("{name} = {}", out.stdout.trim()),
                                ok: true,
                                note: None,
                            }
                        } else {
                            Reply {
                                output: out.combined().trim().to_owned(),
                                ok: false,
                                note: Some(format!("[`{name}` was not bound]")),
                            }
                        }
                    }
                    None => {
                        let out = self.nix_eval(&Self::nix_let(&bindings, input))?;
                        Reply {
                            output: out.combined().trim().to_owned(),
                            ok: out.success(),
                            note: None,
                        }
                    }
                };
                reply
            }
        };
        if self.transcript.len() >= MAX_TRANSCRIPT {
            self.transcript.remove(0);
        }
        self.transcript.push((input.to_owned(), reply.clone()));
        Ok(reply)
    }

    /// Is `check` well-formed? (Evaluated nowhere.)
    pub fn syntax(&mut self, check: &str) -> Result<(), String> {
        match self.lang {
            Lang::Python | Lang::JavaScript => {
                let (_, result, _) = self.request("syntax", check)?;
                match result {
                    Some(r) if r["ok"] == json!(true) => Ok(()),
                    Some(r) => Err(r["error"].as_str().unwrap_or("syntax error").to_owned()),
                    None => Err("the REPL didn't answer".into()),
                }
            }
            Lang::Nix => {
                let spec = Spec::new(&self.program)
                    .args(["--parse", "--expr", check])
                    .timeout(NIX_TIMEOUT);
                let out = exec::run(&spec, &[], "")?;
                if out.success() {
                    Ok(())
                } else {
                    Err(out.combined().trim().to_owned())
                }
            }
            Lang::Sql => Ok(()),
        }
    }

    /// Evaluate `check` in the current state (not recorded).
    pub fn check(&mut self, check: &str) -> Result<CheckResult, String> {
        self.used = Instant::now();
        match self.lang {
            Lang::Python | Lang::JavaScript => {
                let (printed, result, note) = self.request("check", check)?;
                if let Some(note) = note {
                    return Ok(CheckResult {
                        passed: false,
                        value: format!("{printed}{note}"),
                    });
                }
                Ok(match result {
                    Some(r) if r["ok"] == json!(true) => CheckResult {
                        passed: r["truthy"] == json!(true),
                        value: r["value"].as_str().unwrap_or("").to_owned(),
                    },
                    Some(r) => CheckResult {
                        passed: false,
                        value: r["error"].as_str().unwrap_or("error").to_owned(),
                    },
                    None => CheckResult {
                        passed: false,
                        value: "the REPL didn't answer".into(),
                    },
                })
            }
            Lang::Sql => {
                let Engine::Sql(history) = &self.engine else {
                    unreachable!()
                };
                let out = self.sql_run(
                    history,
                    &format!(".mode list\n.headers off\nSELECT ({check});"),
                )?;
                let value = out.combined().trim().to_owned();
                let passed = !sql_error(&value)
                    && !out.timed_out
                    && !matches!(value.as_str(), "" | "0" | "0.0" | "false");
                Ok(CheckResult { passed, value })
            }
            Lang::Nix => {
                let Engine::Nix(bindings) = &self.engine else {
                    unreachable!()
                };
                let out = self.nix_eval(&Self::nix_let(bindings, check))?;
                let value = if out.success() {
                    out.stdout.trim().to_owned()
                } else {
                    out.combined().trim().to_owned()
                };
                Ok(CheckResult {
                    passed: out.success() && value == "true",
                    value,
                })
            }
        }
    }

    fn transcript_json(&self) -> Value {
        Value::Array(
            self.transcript
                .iter()
                .map(|(input, r)| {
                    json!({
                        "input": exec::clip(input, QUOTE),
                        "output": exec::clip(&r.output, QUOTE),
                        "ok": r.ok,
                    })
                })
                .collect(),
        )
    }
}

fn push_line(out: &mut String, line: &str) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(line);
}

fn sql_error(text: &str) -> bool {
    text.lines().any(|l| {
        let l = l.trim_start();
        l.starts_with("Parse error")
            || l.starts_with("Runtime error")
            || l.starts_with("Error:")
            || l.contains("cannot run") && l.contains("safe mode")
    })
}

/// What follows the [`SQL_END`] error in sqlite's stderr: past the last line
/// naming it (the error, and the statement newer shells quote under it) and
/// the caret line under that quote. All of `stderr` when the marker is absent.
fn after_sql_end(stderr: &str) -> &str {
    let Some(at) = stderr.rfind(SQL_END) else {
        return stderr;
    };
    let rest = stderr[at..].split_once('\n').map_or("", |(_, rest)| rest);
    let (line, more) = rest.split_once('\n').unwrap_or((rest, ""));
    if line.trim_start().starts_with("^---") {
        more
    } else {
        rest
    }
}

/// `name = expr` (an identifier, one `=` not part of `==`), else `None`.
pub fn nix_binding(input: &str) -> Option<(String, String)> {
    let input = input.trim();
    let at = input.find('=')?;
    let name = input[..at].trim();
    let rest = &input[at + 1..];
    if rest.starts_with('=') {
        return None;
    }
    let mut chars = name.chars();
    let ident = matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '\'' | '-'));
    if !ident {
        return None;
    }
    let value = rest.trim().trim_end_matches(';').trim();
    (!value.is_empty()).then(|| (name.to_owned(), value.to_owned()))
}

type Shared = Arc<Mutex<Session>>;

pub struct Repl {
    package: NativePackage,
    /// Tutor sessions by (chat session, language); card sessions by artifact id.
    sessions: Mutex<HashMap<String, Shared>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Repl {
    pub fn new() -> Self {
        let mut package = NativePackage::parse(
            include_str!("repl/plugin.toml"),
            &[
                (
                    "schemas/repl_eval.json",
                    include_str!("repl/schemas/repl_eval.json"),
                ),
                (
                    "schemas/repl_task.json",
                    include_str!("repl/schemas/repl_task.json"),
                ),
                ("prompts/tutor.md", include_str!("repl/prompts/tutor.md")),
                (
                    "prompts/practice.md",
                    include_str!("repl/prompts/practice.md"),
                ),
                ("views/repl.html", include_str!("repl/views/repl.html")),
            ],
        )
        .expect("repl package is valid");
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
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// The session under `key`, made for `lang` when missing (or when it is
    /// for another language). Evicts the least recently used past
    /// [`MAX_LIVE`].
    fn session(&self, key: &str, lang: Lang) -> Result<Shared, String> {
        let mut sessions = lock(&self.sessions);
        if let Some(s) = sessions.get(key) {
            if lock(s).lang == lang {
                return Ok(Arc::clone(s));
            }
        }
        let fresh = Arc::new(Mutex::new(Session::new(lang)?));
        sessions.insert(key.to_owned(), Arc::clone(&fresh));
        while sessions.len() > MAX_LIVE {
            let oldest = sessions
                .iter()
                .filter(|(k, _)| k.as_str() != key)
                .filter_map(|(k, s)| s.try_lock().ok().map(|g| (k.clone(), g.used)))
                .min_by_key(|(_, used)| *used)
                .map(|(k, _)| k);
            match oldest {
                Some(k) => {
                    sessions.remove(&k);
                }
                None => break,
            }
        }
        Ok(fresh)
    }

    fn eval_tool(&self, args: &Value, chat: &str) -> ToolOutcome {
        let lang = match Lang::parse(args.get("lang").and_then(Value::as_str).unwrap_or("")) {
            Ok(l) => l,
            Err(e) => return fail(e),
        };
        let input = args.get("input").and_then(Value::as_str).unwrap_or("");
        if input.trim().is_empty() {
            return fail("`input` is empty");
        }
        let session = match self.session(&format!("tutor:{chat}:{}", lang.name()), lang) {
            Ok(s) => s,
            Err(e) => return fail(e),
        };
        let reply = match lock(&session).eval(input) {
            Ok(r) => r,
            Err(e) => return fail(e),
        };
        let mut text = if reply.output.is_empty() {
            "(no output)".to_owned()
        } else {
            exec::clip(&reply.output, 12 * 1024)
        };
        if let Some(note) = reply.note {
            push_line(&mut text, &note);
        }
        if reply.ok {
            done(text)
        } else {
            // An evaluation error is a result to read, not a failed call.
            done(format!("error:\n{text}"))
        }
    }

    fn task_tool(&self, args: &Value) -> ToolOutcome {
        let lang = match Lang::parse(args.get("lang").and_then(Value::as_str).unwrap_or("")) {
            Ok(l) => l,
            Err(e) => return fail(e),
        };
        let prompt = args
            .get("prompt")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let check = args
            .get("check")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if prompt.is_empty() || check.is_empty() {
            return fail("`prompt` and `check` are required");
        }
        let mut probe = match Session::new(lang) {
            Ok(s) => s,
            Err(e) => return fail(e),
        };
        if let Err(e) = probe.syntax(check) {
            return fail(format!(
                "`check` doesn't parse as a {} expression:\n{e}",
                lang.name()
            ));
        }
        // A check that already holds in an empty session grades nothing.
        match probe.check(check) {
            Ok(r) if r.passed => {
                return fail(format!(
                    "`check` is already true in a fresh session ({}); make it depend on what the learner must do",
                    r.value
                ))
            }
            Ok(_) => {}
            Err(e) => return fail(e),
        }
        let svg = card_svg(
            &format!("REPL task · {}", lang.name()),
            &prompt.lines().map(str::to_owned).collect::<Vec<_>>(),
        );
        ToolOutcome {
            content: "REPL card ready. Place it, then ask with question_ask { view: { artifactId, task, target: {} } }.".into(),
            is_error: false,
            artifact: Some(ArtifactPayload {
                kind: "svg".into(),
                svg,
                data: json!({ "lang": lang.name(), "prompt": prompt }),
            }),
        }
    }

    fn input_call(&self, args: &Value) -> Result<Value, String> {
        let artifact_id = args
            .get("artifactId")
            .and_then(Value::as_str)
            .ok_or("artifactId is required")?;
        let lang = Lang::parse(args.get("lang").and_then(Value::as_str).unwrap_or(""))?;
        let input = args.get("input").and_then(Value::as_str).unwrap_or("");
        let session = self.session(&format!("card:{artifact_id}"), lang)?;
        let reply = lock(&session).eval(input)?;
        Ok(json!({ "output": reply.output, "ok": reply.ok, "note": reply.note }))
    }

    fn grade_task(&self, artifact_id: &str, input: &Value) -> Result<Value, String> {
        let lang = Lang::parse(input.get("lang").and_then(Value::as_str).unwrap_or(""))?;
        let check = input.get("check").and_then(Value::as_str).unwrap_or("");
        let key = format!("card:{artifact_id}");
        let existing = lock(&self.sessions).get(&key).cloned();
        let had_session = existing.is_some();
        let session = match existing {
            Some(s) => s,
            None => self.session(&key, lang)?,
        };
        let mut s = lock(&session);
        let result = s.check(check)?;
        let mut detail = if result.passed {
            format!("the check `{check}` holds")
        } else {
            format!(
                "the check `{check}` doesn't hold (it gave {})",
                exec::clip(&result.value, 400)
            )
        };
        if !had_session {
            detail.push_str("; no inputs were recorded for this card (none entered, or the app restarted since)");
        }
        Ok(json!({
            "correct": result.passed,
            "detail": detail,
            "lang": lang.name(),
            "check": { "expression": check, "value": exec::clip(&result.value, QUOTE), "passed": result.passed },
            "transcript": s.transcript_json(),
        }))
    }
}

impl Default for Repl {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for Repl {
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
        self.input_call(args)
    }

    fn grade_artifact(
        &self,
        _answer: &Value,
        _target: &Value,
        artifact_id: &str,
        input: &Value,
    ) -> Result<Value, String> {
        self.grade_task(artifact_id, input)
    }

    fn call(self: Arc<Self>, name: &str, args: &Value, ctx: PluginContext) -> ToolFuture {
        let args = args.clone();
        match self.package.entry(name) {
            Some("eval") => blocking(move || self.eval_tool(&args, &ctx.session_id)),
            Some("task") => blocking(move || self.task_tool(&args)),
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

    fn ctx() -> PluginContext {
        PluginContext {
            session_id: "S1".into(),
            platform: "desktop".into(),
        }
    }

    #[test]
    fn python_state_persists_and_values_echo() {
        if !have(Lang::Python) {
            return;
        }
        let mut s = Session::new(Lang::Python).unwrap();
        assert_eq!(s.eval("x = 40").unwrap().output, "");
        let r = s.eval("print('hi')\nx + 2").unwrap();
        assert_eq!((r.output.as_str(), r.ok), ("hi\n42", true));
        let r = s.eval("undefined_name").unwrap();
        assert!(!r.ok && r.output.contains("NameError"), "{r:?}");
        // input() can't read the protocol stream.
        let r = s.eval("input()").unwrap();
        assert!(!r.ok && r.output.contains("EOFError"), "{r:?}");
        assert!(s.check("x == 40").unwrap().passed);
        assert!(!s.check("x == 41").unwrap().passed);
    }

    #[test]
    fn a_runaway_input_stops_the_session_and_says_so() {
        if !have(Lang::Python) {
            return;
        }
        let mut s = Session::new(Lang::Python).unwrap();
        s.eval("y = 1").unwrap();
        let started = Instant::now();
        let r = s.eval("while True: pass").unwrap();
        assert!(started.elapsed() < INPUT_TIMEOUT + Duration::from_secs(5));
        assert!(r.output.contains("stopped"), "{r:?}");
        let r = s.eval("y").unwrap();
        assert!(!r.ok && r.output.contains("NameError"), "{r:?}");
    }

    #[test]
    fn sql_replays_history_and_checks_a_scalar() {
        if !have(Lang::Sql) {
            return;
        }
        let mut s = Session::new(Lang::Sql).unwrap();
        s.eval("CREATE TABLE t(n INTEGER)").unwrap();
        let r = s
            .eval("INSERT INTO t VALUES (1), (2); SELECT sum(n) FROM t")
            .unwrap();
        assert_eq!((r.output.as_str(), r.ok), ("3", true), "{r:?}");
        // Only the new input's output is shown.
        let r = s.eval("SELECT count(*) FROM t").unwrap();
        assert_eq!(r.output, "2");
        let r = s.eval("SELEC 1").unwrap();
        assert!(!r.ok, "{r:?}");
        assert!(s.check("(SELECT count(*) FROM t) = 2").unwrap().passed);
        assert!(!s.check("(SELECT count(*) FROM t) = 3").unwrap().passed);
        assert!(
            !s.check("(SELECT count(*) FROM missing) = 0")
                .unwrap()
                .passed
        );
    }

    #[test]
    fn nix_bindings_carry_into_later_inputs_and_checks() {
        if !have(Lang::Nix) {
            return;
        }
        let mut s = Session::new(Lang::Nix).unwrap();
        let r = s.eval("config = { port = 8080; }").unwrap();
        assert!(r.ok, "{r:?}");
        let r = s.eval("builtins.attrNames config").unwrap();
        assert_eq!(r.output, "[ \"port\" ]");
        assert!(s.check("config.port == 8080").unwrap().passed);
        let r = s.eval("broken = 1 +").unwrap();
        assert!(!r.ok && r.note.is_some());
        // The failed binding was not kept.
        assert!(s.check("config.port == 8080").unwrap().passed);
        assert!(s.syntax("x ==").is_err());
    }

    #[test]
    fn nix_binding_parsing() {
        assert_eq!(nix_binding("a = 1;"), Some(("a".into(), "1".into())));
        assert_eq!(nix_binding("a == 1"), None);
        assert_eq!(nix_binding("a <= 1"), None);
        assert_eq!(nix_binding("{ a = 1; }"), None);
        assert_eq!(nix_binding("let a = 1; in a"), None);
    }

    #[tokio::test]
    async fn repl_eval_keeps_the_tutors_session_per_chat() {
        if !have(Lang::Python) {
            return;
        }
        let repl = Arc::new(Repl::new());
        let out = Arc::clone(&repl)
            .call(
                "repl_eval",
                &json!({ "lang": "python", "input": "z = 6 * 7" }),
                ctx(),
            )
            .await;
        assert!(!out.is_error, "{}", out.content);
        let out = Arc::clone(&repl)
            .call(
                "repl_eval",
                &json!({ "lang": "python", "input": "z" }),
                ctx(),
            )
            .await;
        assert_eq!(out.content, "42");
        let other = PluginContext {
            session_id: "S2".into(),
            platform: "desktop".into(),
        };
        let out = Arc::clone(&repl)
            .call(
                "repl_eval",
                &json!({ "lang": "python", "input": "z" }),
                other,
            )
            .await;
        assert!(
            out.content.starts_with("error:") && out.content.contains("NameError"),
            "{}",
            out.content
        );
    }

    #[tokio::test]
    async fn a_repl_task_is_graded_on_the_cards_final_state() {
        if !have(Lang::Python) {
            return;
        }
        let repl = Arc::new(Repl::new());
        let args = json!({ "lang": "python", "prompt": "Make `result` the sum of 1..10.", "check": "result == 55" });
        let made = Arc::clone(&repl).call("repl_task", &args, ctx()).await;
        assert!(!made.is_error, "{}", made.content);
        assert!(!made.artifact.unwrap().data.to_string().contains("55"));
        repl.view_call(
            "input",
            &json!({ "artifactId": "C1", "lang": "python", "input": "result = sum(range(10))" }),
        )
        .unwrap();
        let g = repl
            .grade_artifact(&json!({}), &json!({}), "C1", &args)
            .unwrap();
        assert_eq!(g["correct"], json!(false), "{g}");
        assert_eq!(g["check"]["value"], json!("False"));
        repl.view_call(
            "input",
            &json!({ "artifactId": "C1", "lang": "python", "input": "result = sum(range(11))" }),
        )
        .unwrap();
        let g = repl
            .grade_artifact(&json!({}), &json!({}), "C1", &args)
            .unwrap();
        assert_eq!(g["correct"], json!(true), "{g}");
        assert_eq!(g["transcript"].as_array().unwrap().len(), 2);
        // A check that is already true, or doesn't parse, is refused.
        let trivial = json!({ "lang": "python", "prompt": "p", "check": "1 == 1" });
        assert!(
            Arc::clone(&repl)
                .call("repl_task", &trivial, ctx())
                .await
                .is_error
        );
        let broken = json!({ "lang": "python", "prompt": "p", "check": "result ==" });
        let out = Arc::clone(&repl).call("repl_task", &broken, ctx()).await;
        assert!(
            out.is_error && out.content.contains("doesn't parse"),
            "{}",
            out.content
        );
    }

    #[test]
    fn javascript_driver_keeps_state() {
        if !have(Lang::JavaScript) {
            return;
        }
        let mut s = Session::new(Lang::JavaScript).unwrap();
        s.eval("let xs = [3, 1, 2]").unwrap();
        let r = s.eval("console.log('n', xs.length); xs.sort()").unwrap();
        assert_eq!(r.output, "n 3\n[ 1, 2, 3 ]", "{r:?}");
        assert!(s.check("xs[0] === 1").unwrap().passed);
        let r = s.eval("nope()").unwrap();
        assert!(!r.ok && r.output.contains("ReferenceError"), "{r:?}");
    }
}
