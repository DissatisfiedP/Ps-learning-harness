//! `sandbox`: disposable container labs the learner works in through a
//! terminal card (docs/plugins.md §12).
//!
//! - `sandbox_start { scenario, image? }` boots a lab and makes its terminal
//!   card (answer-mode view, docs/plugins.md §7.4). One lab per chat session:
//!   starting another removes the old container.
//! - `sandbox_observe { since? }` returns what the learner ran in the lab
//!   (their commands and the output) after entry `since`.
//! - `sandbox_check { checkId }` runs the scenario's check inside the lab.
//!   The check id is the scenario id.
//!
//! The learner's commands reach the lab through the view's `run` call; Submit
//! grades with [`Plugin::grade_artifact`], which runs the same check in the
//! lab the card is bound to (the lab of the card's first `run` call, else the
//! only lab there is) and returns the verdict with the transcript.
//!
//! Runtime: `podman`, else `docker`. The container has no network, 256 MB,
//! one CPU, 256 processes, no capabilities but a small set, `no-new-privileges`
//! and a 2 h lifetime (`sleep 7200`, `--rm`), and is removed when its lab is
//! replaced or dropped. A scenario is a pair of POSIX `sh` scripts
//! (`sandbox/scenarios/`) `exec`'d in the container: setup right after it
//! starts, check on demand. Both take an optional `$LAB_ROOT` path prefix so
//! the tests run them on the host. `LAB_FLAG` (random, per lab) is passed to
//! those two `exec`s only, so the learner's own commands can't read it.
//! Learner commands run as one argv (`exec <name> sh -c <command>`, no host
//! shell), each on its own: 20 s and 16 KiB limits, and the working directory
//! doesn't carry over.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use providers::ToolSpec;
use serde_json::{json, Value};

use super::exec::{self, Spec};
use super::{blocking, card_svg, done, fail, ready, NativePackage};
use crate::plugins::{
    ArtifactPayload, Plugin, PluginContext, PluginManifest, ToolFuture, ToolOutcome,
};

/// Image a lab runs in unless the tutor names another.
pub const DEFAULT_IMAGE: &str = "alpine:3.20";
/// Time one learner command may take.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);
/// Bytes one learner command may print.
pub const OUTPUT_CAP: usize = 16 * 1024;
/// Starting the container (the first start pulls the image).
const START_TIMEOUT: Duration = Duration::from_secs(300);
const SETUP_TIMEOUT: Duration = Duration::from_secs(60);
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const STOP_TIMEOUT: Duration = Duration::from_secs(30);
/// Transcript entries kept per lab (numbering goes on past it).
const MAX_ENTRIES: usize = 200;
/// Longest command accepted.
const MAX_COMMAND: usize = 4000;
/// Card-to-lab bindings kept.
const MAX_BOUND: usize = 64;
/// Characters of a command or output quoted back in a grade.
const QUOTE: usize = 1000;
/// Transcript entries quoted back in a grade.
const GRADE_ENTRIES: usize = 40;
/// What `docker`/`podman` exit with when the runtime itself failed (for an
/// `exec`: the container is gone).
const RUNTIME_ERROR: i32 = 125;

/// One lab scenario: its learner-facing goal and its two scripts.
pub struct Scenario {
    pub id: &'static str,
    /// The skill it trains, for the tool description.
    pub skill: &'static str,
    /// What the learner is asked to do (shown on the card).
    pub goal: &'static str,
    setup: &'static str,
    check: &'static str,
}

pub const SCENARIOS: [Scenario; 4] = [
    Scenario {
        id: "permissions",
        skill: "file modes and owners (ls -l, stat, chmod, chown)",
        goal: "/srv/app/secret.conf holds credentials but every user can read and change it. Make it readable and writable only by its owner, root (mode 600), and keep root as its owner.",
        setup: include_str!("sandbox/scenarios/permissions.setup.sh"),
        check: include_str!("sandbox/scenarios/permissions.check.sh"),
    },
    Scenario {
        id: "find-the-flag",
        skill: "searching logs (grep, find, less, redirection)",
        goal: "One of the six logs in /var/log/app has a line with a token shaped FLAG{...}. Find it and write exactly that token, braces included, to /tmp/flag.txt.",
        setup: include_str!("sandbox/scenarios/find-the-flag.setup.sh"),
        check: include_str!("sandbox/scenarios/find-the-flag.check.sh"),
    },
    Scenario {
        id: "harden-sshd",
        skill: "hardening a service's configuration file (sshd_config)",
        goal: "/etc/ssh/sshd_config is too permissive. Turn PermitRootLogin and PasswordAuthentication off, set every other setting that is `yes` to `no`, and keep the `Port 22` line.",
        setup: include_str!("sandbox/scenarios/harden-sshd.setup.sh"),
        check: include_str!("sandbox/scenarios/harden-sshd.check.sh"),
    },
    Scenario {
        id: "stop-the-service",
        skill: "processes and listening ports (ps, netstat, kill)",
        goal: "A web server (busybox httpd) is listening on port 8080 in this lab. Stop it so that nothing listens on port 8080.",
        setup: include_str!("sandbox/scenarios/stop-the-service.setup.sh"),
        check: include_str!("sandbox/scenarios/stop-the-service.check.sh"),
    },
];

fn scenario(id: &str) -> Option<&'static Scenario> {
    SCENARIOS.iter().find(|s| s.id == id)
}

fn scenario_ids() -> String {
    SCENARIOS
        .iter()
        .map(|s| s.id)
        .collect::<Vec<_>>()
        .join(", ")
}

/// `^[a-z0-9][a-z0-9./_-]*(:[A-Za-z0-9._-]+)?$`, at most 200 characters.
pub fn valid_image(image: &str) -> bool {
    if image.len() > 200 {
        return false;
    }
    let (name, tag) = match image.split_once(':') {
        Some((n, t)) => (n, Some(t)),
        None => (image, None),
    };
    let mut chars = name.chars();
    let first = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    first
        && chars.all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '/' | '_' | '-')
        })
        && tag.is_none_or(|t| {
            !t.is_empty()
                && t.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        })
}

/// A short image name is qualified so Podman doesn't need a registry search
/// list (it can't ask which registry to use without a terminal).
fn qualified(image: &str) -> String {
    if image.contains('/') {
        image.to_owned()
    } else {
        format!("docker.io/library/{image}")
    }
}

#[derive(Debug, Clone, Copy)]
struct Limits {
    command_timeout: Duration,
    output_cap: usize,
}

/// One command the learner ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub n: usize,
    pub command: String,
    pub output: String,
    /// Exit code; `None` when it was stopped.
    pub code: Option<i32>,
}

#[derive(Default)]
struct Transcript {
    /// Number of the last entry (entries are numbered from 1).
    last: usize,
    entries: VecDeque<Entry>,
}

/// The result of a scenario check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub passed: bool,
    /// The check's `ok:`/`fail:` lines.
    pub output: String,
}

/// One running lab: a container and what the learner has run in it.
struct Lab {
    id: String,
    name: String,
    scenario: &'static Scenario,
    runtime: PathBuf,
    flag: String,
    limits: Limits,
    log: Mutex<Transcript>,
    stopped: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 16 random lowercase characters (the random part of a ULID).
fn random_token() -> String {
    ulid::Ulid::new().to_string()[10..].to_ascii_lowercase()
}

impl Lab {
    /// Run one of the scenario's scripts (they need `LAB_FLAG`).
    fn script(&self, script: &str, timeout: Duration) -> Result<exec::Output, String> {
        let spec = Spec::new(&self.runtime)
            .args(["exec", "-e"])
            .args([format!("LAB_FLAG={}", self.flag)])
            .args([self.name.as_str(), "sh", "-c", script])
            .timeout(timeout)
            .output_cap(self.limits.output_cap)
            .no_isolation();
        let out = exec::run(&spec, &[], "")?;
        if out.code == Some(RUNTIME_ERROR) {
            return Err(format!(
                "the lab's container isn't answering ({}); start a new lab with sandbox_start",
                exec::clip(out.combined().trim(), 400)
            ));
        }
        Ok(out)
    }

    fn setup(&self) -> Result<(), String> {
        let out = self.script(self.scenario.setup, SETUP_TIMEOUT)?;
        if out.success() {
            Ok(())
        } else {
            Err(format!(
                "the scenario's setup failed:\n{}",
                exec::clip(out.combined().trim(), 2000)
            ))
        }
    }

    fn check(&self) -> Result<Verdict, String> {
        let out = self.script(self.scenario.check, CHECK_TIMEOUT)?;
        Ok(Verdict {
            passed: out.success(),
            output: exec::clip(out.combined().trim(), 2000),
        })
    }

    /// Run a learner command and record it.
    fn run(&self, command: &str) -> Result<Entry, String> {
        if self.stopped.load(Ordering::SeqCst) {
            return Err(
                "this lab has ended (a newer lab replaced it); ask the tutor for a new one".into(),
            );
        }
        if command.trim().is_empty() {
            return Err("the command is empty".into());
        }
        if command.len() > MAX_COMMAND {
            return Err(format!("the command is longer than {MAX_COMMAND} bytes"));
        }
        let spec = Spec::new(&self.runtime)
            .args(["exec", self.name.as_str(), "sh", "-c", command])
            .timeout(self.limits.command_timeout)
            .output_cap(self.limits.output_cap)
            .no_isolation();
        let out = exec::run(&spec, &[], "")?;
        let mut body = out.stdout.clone();
        if !out.stderr.is_empty() {
            if !body.is_empty() && !body.ends_with('\n') {
                body.push('\n');
            }
            body.push_str(&out.stderr);
        }
        // The cut goes before the notes, which must survive it.
        let mut output = exec::clip(body.trim_end(), self.limits.output_cap);
        if let Some(note) = out.note() {
            output.push('\n');
            output.push_str(&note);
        }
        if out.timed_out {
            output.push_str("\n[the command may still be running in the lab]");
        }
        if out.code == Some(RUNTIME_ERROR) {
            output.push_str("\n[the lab container may have stopped; ask the tutor for a new lab]");
        }
        let mut log = lock(&self.log);
        log.last += 1;
        let entry = Entry {
            n: log.last,
            command: command.to_owned(),
            output: output.trim_start_matches('\n').to_owned(),
            code: out.code,
        };
        if log.entries.len() >= MAX_ENTRIES {
            log.entries.pop_front();
        }
        log.entries.push_back(entry.clone());
        Ok(entry)
    }

    /// Entries after number `since`, and the latest number.
    fn since(&self, since: usize) -> (Vec<Entry>, usize) {
        let log = lock(&self.log);
        (
            log.entries
                .iter()
                .filter(|e| e.n > since)
                .cloned()
                .collect(),
            log.last,
        )
    }

    /// Remove the container (once).
    fn stop(&self) {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        let spec = Spec::new(&self.runtime)
            .args(["rm", "-f", self.name.as_str()])
            .timeout(STOP_TIMEOUT)
            .no_isolation();
        let _ = exec::run(&spec, &[], "");
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Default)]
struct State {
    /// The lab of each chat session.
    labs: HashMap<String, Arc<Lab>>,
    /// (artifact id, lab id), oldest first: the lab a card's first `run`
    /// call reached.
    bound: VecDeque<(String, String)>,
}

pub struct Sandbox {
    package: NativePackage,
    /// `Some` fixes the runtime (`None` inside: none); `None` looks for
    /// podman, then docker, at each start.
    runtime: Option<Option<PathBuf>>,
    limits: Limits,
    state: Mutex<State>,
}

fn runtime_missing() -> String {
    exec::missing(
        "The lab sandbox",
        &["podman", "docker"],
        "Install Podman or Docker, then try again.",
    )
}

impl Sandbox {
    pub fn new() -> Self {
        Self::build(None)
    }

    /// A sandbox that uses exactly this container client (`None`: none is
    /// installed).
    pub fn with_runtime(runtime: Option<PathBuf>) -> Self {
        Self::build(Some(runtime))
    }

    fn build(runtime: Option<Option<PathBuf>>) -> Self {
        let mut package = NativePackage::parse(
            include_str!("sandbox/plugin.toml"),
            &[
                (
                    "schemas/sandbox_start.json",
                    include_str!("sandbox/schemas/sandbox_start.json"),
                ),
                (
                    "schemas/sandbox_observe.json",
                    include_str!("sandbox/schemas/sandbox_observe.json"),
                ),
                (
                    "schemas/sandbox_check.json",
                    include_str!("sandbox/schemas/sandbox_check.json"),
                ),
                ("prompts/tutor.md", include_str!("sandbox/prompts/tutor.md")),
                (
                    "prompts/practice.md",
                    include_str!("sandbox/prompts/practice.md"),
                ),
                (
                    "views/terminal.html",
                    include_str!("sandbox/views/terminal.html"),
                ),
            ],
        )
        .expect("sandbox package is valid");
        let found = match &runtime {
            Some(r) => r.clone(),
            None => exec::find_any(&["podman", "docker"]),
        };
        package.manifest.use_when = format!(
            "{} Container runtime on this device: {}.",
            package.manifest.use_when,
            found
                .as_ref()
                .and_then(|p| p.file_stem())
                .map_or("none found".to_owned(), |n| n
                    .to_string_lossy()
                    .into_owned())
        );
        let catalog: String = SCENARIOS
            .iter()
            .map(|s| format!("\n- `{}`: {} Goal: {}", s.id, s.skill, s.goal))
            .collect();
        if let Some(start) = package.tools.iter_mut().find(|t| t.name == "sandbox_start") {
            start.description = format!(
                "{} The default image is {DEFAULT_IMAGE}. Scenarios:{catalog}\nThe result names the card; place it, then ask with question_ask on it.",
                start.description
            );
        }
        Self {
            package,
            runtime,
            limits: Limits {
                command_timeout: COMMAND_TIMEOUT,
                output_cap: OUTPUT_CAP,
            },
            state: Mutex::new(State::default()),
        }
    }

    #[cfg(test)]
    fn with_limits(mut self, command_timeout: Duration, output_cap: usize) -> Self {
        self.limits = Limits {
            command_timeout,
            output_cap,
        };
        self
    }

    fn runtime(&self) -> Result<PathBuf, String> {
        match &self.runtime {
            Some(r) => r.clone().ok_or_else(runtime_missing),
            None => exec::find_any(&["podman", "docker"]).ok_or_else(runtime_missing),
        }
    }

    fn lab_of_chat(&self, chat: &str) -> Result<Arc<Lab>, String> {
        lock(&self.state)
            .labs
            .get(chat)
            .cloned()
            .ok_or_else(|| "no lab is running in this chat; start one with sandbox_start".into())
    }

    fn start_tool(&self, args: &Value, chat: &str) -> ToolOutcome {
        let id = args.get("scenario").and_then(Value::as_str).unwrap_or("");
        let Some(scenario) = scenario(id) else {
            return fail(format!(
                "`scenario` `{id}` isn't in the catalog; use one of: {}",
                scenario_ids()
            ));
        };
        let image = match args.get("image").and_then(Value::as_str) {
            None | Some("") => DEFAULT_IMAGE,
            Some(image) if valid_image(image) => image,
            Some(image) => {
                return fail(format!(
                    "`image` `{image}` isn't a plain image name (lowercase letters, digits, `.`, `/`, `_`, `-`, then an optional `:tag`)"
                ))
            }
        };
        let runtime = match self.runtime() {
            Ok(r) => r,
            Err(e) => return fail(e),
        };
        // One lab per chat: the old container goes before the new one starts.
        let old = lock(&self.state).labs.remove(chat);
        if let Some(old) = old {
            old.stop();
        }
        let lab_id = random_token()[..10].to_owned();
        let name = format!("lh-lab-{lab_id}");
        let spec = Spec::new(&runtime)
            .args(["run", "-d", "--rm", "--name", name.as_str()])
            .args(["--network", "none", "--memory", "256m", "--cpus", "1"])
            .args(["--pids-limit", "256", "--stop-timeout", "1"])
            .args(["--cap-drop", "ALL"])
            .args(["--cap-add", "CHOWN,DAC_OVERRIDE,FOWNER,SETUID,SETGID,KILL"])
            .args(["--security-opt", "no-new-privileges"])
            .args([qualified(image).as_str(), "sleep", "7200"])
            .timeout(START_TIMEOUT)
            .no_isolation();
        match exec::run(&spec, &[], "") {
            Ok(out) if out.success() => {}
            Ok(out) => {
                return fail(format!(
                    "couldn't start the lab container:\n{}",
                    exec::clip(out.combined().trim(), 2000)
                ))
            }
            Err(e) => return fail(e),
        }
        let lab = Arc::new(Lab {
            id: lab_id.clone(),
            name,
            scenario,
            runtime,
            flag: random_token(),
            limits: self.limits,
            log: Mutex::new(Transcript::default()),
            stopped: AtomicBool::new(false),
        });
        // A failed setup drops `lab`, which removes the container.
        if let Err(e) = lab.setup() {
            return fail(e);
        }
        let replaced = lock(&self.state).labs.insert(chat.to_owned(), lab);
        if let Some(raced) = replaced {
            raced.stop();
        }
        let svg = card_svg(
            &format!("Lab · {}", scenario.id),
            &[
                scenario.goal.to_owned(),
                String::new(),
                format!("image {image}; the terminal needs the app"),
            ],
        );
        ToolOutcome {
            content: format!(
                "Lab `{lab_id}` is running (scenario `{}`, image {image}, no network).\nGoal on the card: {}\nPlace the card, then ask with question_ask {{ view: {{ artifactId, task, target: {{}} }} }}. Submit runs the check; you can run it with sandbox_check {{ checkId: \"{}\" }} and read the learner's commands with sandbox_observe. Their commands run one by one (`sh -c`, 20 s), so a `cd` doesn't carry over.",
                scenario.id, scenario.goal, scenario.id
            ),
            is_error: false,
            artifact: Some(ArtifactPayload {
                kind: "svg".into(),
                svg,
                data: json!({
                    "labId": lab_id,
                    "scenario": scenario.id,
                    "goal": scenario.goal,
                    "image": image,
                }),
            }),
        }
    }

    fn observe_tool(&self, args: &Value, chat: &str) -> ToolOutcome {
        let since = match args.get("since").and_then(Value::as_str).map(str::trim) {
            None | Some("") => 0,
            Some(s) => match s.parse::<usize>() {
                Ok(n) => n,
                Err(_) => return fail("`since` must be an entry number (0 for everything kept)"),
            },
        };
        let lab = match self.lab_of_chat(chat) {
            Ok(l) => l,
            Err(e) => return fail(e),
        };
        let (entries, last) = lab.since(since);
        if last == 0 {
            return done("The learner hasn't run anything in the lab yet (latest entry 0).");
        }
        if entries.is_empty() {
            return done(format!(
                "No commands after entry {since} (latest entry {last})."
            ));
        }
        let mut text = String::new();
        for e in &entries {
            text.push_str(&format!("[{}] $ {}\n", e.n, e.command));
            if !e.output.is_empty() {
                text.push_str(&e.output);
                text.push('\n');
            }
            match e.code {
                Some(c) => text.push_str(&format!("(exit {c})\n")),
                None => text.push_str("(stopped)\n"),
            }
        }
        text.push_str(&format!("Latest entry {last}."));
        done(text)
    }

    fn check_tool(&self, args: &Value, chat: &str) -> ToolOutcome {
        let id = args.get("checkId").and_then(Value::as_str).unwrap_or("");
        let lab = match self.lab_of_chat(chat) {
            Ok(l) => l,
            Err(e) => return fail(e),
        };
        if id != lab.scenario.id {
            return fail(format!(
                "`checkId` `{id}` isn't a check of the running lab; its scenario is `{0}`, so use `{0}`",
                lab.scenario.id
            ));
        }
        match lab.check() {
            Ok(v) => done(format!(
                "{} {}\n{}",
                if v.passed { "PASS" } else { "FAIL" },
                lab.scenario.id,
                v.output
            )),
            Err(e) => fail(e),
        }
    }

    /// The learner's `run { labId, artifactId, command }`.
    fn run_call(&self, args: &Value) -> Result<Value, String> {
        let lab_id = args
            .get("labId")
            .and_then(Value::as_str)
            .ok_or("labId is required")?;
        let command = args
            .get("command")
            .and_then(Value::as_str)
            .ok_or("command is required")?;
        let lab = lock(&self.state)
            .labs
            .values()
            .find(|l| l.id == lab_id)
            .cloned()
            .ok_or("this lab has ended (a newer lab replaced it, or the app restarted); ask the tutor for a new one")?;
        if let Some(artifact) = args
            .get("artifactId")
            .and_then(Value::as_str)
            .filter(|a| !a.is_empty())
        {
            let mut st = lock(&self.state);
            if !st.bound.iter().any(|(a, _)| a == artifact) {
                st.bound.push_back((artifact.to_owned(), lab.id.clone()));
                if st.bound.len() > MAX_BOUND {
                    st.bound.pop_front();
                }
            }
        }
        let e = lab.run(command)?;
        Ok(json!({ "n": e.n, "output": e.output, "exit": e.code, "ok": e.code == Some(0) }))
    }

    /// The lab a card's Submit grades.
    fn lab_for_card(&self, artifact_id: &str, input: &Value) -> Result<Arc<Lab>, String> {
        let st = lock(&self.state);
        if let Some((_, lab_id)) = st.bound.iter().find(|(a, _)| a == artifact_id) {
            return st
                .labs
                .values()
                .find(|l| &l.id == lab_id)
                .cloned()
                .ok_or_else(|| {
                    "the lab this card ran commands in has ended (a newer lab replaced it, or the app restarted), so its result can't be checked".into()
                });
        }
        let mut labs = st.labs.values();
        let lab = match (labs.next(), labs.next()) {
            (Some(l), None) => l.clone(),
            (None, _) => return Err("no lab is running, so there is nothing to check".into()),
            _ => {
                return Err(
                    "several labs are running and no command was entered through this card, so it can't be matched to one".into(),
                )
            }
        };
        match input.get("scenario").and_then(Value::as_str) {
            Some(s) if s != lab.scenario.id => Err(format!(
                "the only running lab is for `{}`, not `{s}`; the card's lab has ended",
                lab.scenario.id
            )),
            _ => Ok(lab),
        }
    }

    fn grade_card(&self, artifact_id: &str, input: &Value) -> Result<Value, String> {
        let lab = self.lab_for_card(artifact_id, input)?;
        let verdict = lab.check()?;
        let (entries, last) = lab.since(0);
        let skip = entries.len().saturating_sub(GRADE_ENTRIES);
        let transcript: Vec<Value> = entries
            .iter()
            .skip(skip)
            .map(|e| {
                json!({
                    "n": e.n,
                    "command": exec::clip(&e.command, QUOTE),
                    "output": exec::clip(&e.output, QUOTE),
                    "exit": e.code,
                })
            })
            .collect();
        let detail = if verdict.passed {
            format!("the {} check passed", lab.scenario.id)
        } else {
            format!("the {} check failed", lab.scenario.id)
        };
        Ok(json!({
            "correct": verdict.passed,
            "detail": if last == 0 {
                format!("{detail}; the learner ran no commands in the lab")
            } else {
                detail
            },
            "scenario": lab.scenario.id,
            "check": { "id": lab.scenario.id, "passed": verdict.passed, "output": verdict.output },
            "transcript": transcript,
        }))
    }
}

impl Default for Sandbox {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for Sandbox {
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
        _answer: &Value,
        _target: &Value,
        artifact_id: &str,
        input: &Value,
    ) -> Result<Value, String> {
        self.grade_card(artifact_id, input)
    }

    fn call(self: Arc<Self>, name: &str, args: &Value, ctx: PluginContext) -> ToolFuture {
        let args = args.clone();
        match self.package.entry(name) {
            Some("start") => blocking(move || self.start_tool(&args, &ctx.session_id)),
            Some("observe") => blocking(move || self.observe_tool(&args, &ctx.session_id)),
            Some("check") => blocking(move || self.check_tool(&args, &ctx.session_id)),
            _ => ready(fail(format!("unknown plugin tool `{name}`"))),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::time::Instant;

    const RUNTIME: &str = r##"#!@SH@
STATE='@STATE@'
cmd=$1
shift
case "$cmd" in
  run)
    printf 'run %s\n' "$*" >> "$STATE/log"
    name=
    while [ $# -gt 0 ]; do
      case "$1" in
        --name) name=$2; shift 2 ;;
        -d|--rm) shift ;;
        --network|--memory|--cpus|--pids-limit|--stop-timeout|--cap-drop|--cap-add|--security-opt) shift 2 ;;
        *) break ;;
      esac
    done
    if [ -f "$STATE/fail-run" ]; then
      echo "Error: unable to find image $1" >&2
      exit 125
    fi
    mkdir -p "$STATE/labs/$name/root"
    echo "$1" > "$STATE/labs/$name/image"
    echo 0123456789abcdef
    ;;
  exec)
    while [ "$1" = -e ]; do export "$2"; shift 2; done
    name=$1
    shift
    if [ ! -d "$STATE/labs/$name" ]; then
      echo "Error: no such container: $name" >&2
      exit 125
    fi
    export LAB_ROOT="$STATE/labs/$name/root" LAB_OWNER="$(id -un)"
    export PATH="$STATE/bin:$PATH"
    if [ -f "$STATE/port" ]; then LAB_PORT=$(cat "$STATE/port"); export LAB_PORT; fi
    exec "$@"
    ;;
  rm)
    printf 'rm %s\n' "$*" >> "$STATE/log"
    shift
    pid="$STATE/labs/$1/root/httpd.pid"
    if [ -f "$pid" ]; then kill "$(cat "$pid")" 2>/dev/null; fi
    rm -rf "$STATE/labs/$1"
    ;;
esac
exit 0
"##;

    /// `busybox httpd -p PORT`: the service a real busybox daemonizes. A
    /// listener has to outlive the setup script's process group, so this
    /// binary re-runs itself ([`listen_helper`]) in a group of its own.
    const BUSYBOX: &str = r##"#!@SH@
[ "$1" = httpd ] || exit 1
shift
port=
while [ $# -gt 0 ]; do
  case "$1" in
    -p) port=$2; shift 2 ;;
    *) shift ;;
  esac
done
LH_TEST_LISTEN="$port" LH_TEST_PIDFILE="$LAB_ROOT/httpd.pid" '@EXE@' --exact plugins::native::sandbox::tests::listen_helper --nocapture >/dev/null 2>&1 </dev/null
"##;

    /// Not a test: see [`BUSYBOX`].
    #[test]
    fn listen_helper() {
        let Ok(port) = std::env::var("LH_TEST_LISTEN") else {
            return;
        };
        let pidfile = std::env::var("LH_TEST_PIDFILE").unwrap();
        if std::env::var("LH_TEST_CHILD").is_err() {
            // The listener has to outlive this process, so it is never waited for.
            #[allow(clippy::zombie_processes)]
            let _child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "plugins::native::sandbox::tests::listen_helper",
                    "--nocapture",
                ])
                .env("LH_TEST_CHILD", "1")
                .env("LH_TEST_LISTEN", &port)
                .env("LH_TEST_PIDFILE", &pidfile)
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            return;
        }
        std::fs::write(&pidfile, std::process::id().to_string()).unwrap();
        let _listener = std::net::TcpListener::bind(("127.0.0.1", port.parse().unwrap())).unwrap();
        std::thread::sleep(Duration::from_secs(60));
    }

    /// A container client that is a script: a lab is a directory under
    /// `labs/` (its `root/` is the lab's `/`, passed to the scenario scripts
    /// as `LAB_ROOT`), `exec` runs the argv locally, and `log` records the
    /// `run` and `rm` calls.
    struct Fake {
        dir: exec::TempDir,
    }

    impl Fake {
        fn new() -> Self {
            let dir = exec::TempDir::new("sandbox-test").unwrap();
            let sh = exec::which("sh").unwrap();
            let exe = std::env::current_exe().unwrap();
            let fill = |t: &str| {
                t.replace("@SH@", &sh.to_string_lossy())
                    .replace("@STATE@", &dir.path().to_string_lossy())
                    .replace("@EXE@", &exe.to_string_lossy())
            };
            std::fs::create_dir_all(dir.path().join("bin")).unwrap();
            for (rel, text) in [("runtime", RUNTIME), ("bin/busybox", BUSYBOX)] {
                let path = dir.path().join(rel);
                std::fs::write(&path, fill(text)).unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            Self { dir }
        }

        fn state(&self) -> &Path {
            self.dir.path()
        }

        fn sandbox(&self) -> Arc<Sandbox> {
            Arc::new(Sandbox::with_runtime(Some(self.state().join("runtime"))))
        }

        fn log(&self) -> String {
            std::fs::read_to_string(self.state().join("log")).unwrap_or_default()
        }

        /// Names of the containers that exist.
        fn containers(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(self.state().join("labs"))
                .map(|d| {
                    d.flatten()
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            names.sort();
            names
        }

        fn root(&self, lab_id: &str) -> PathBuf {
            self.state()
                .join("labs")
                .join(format!("lh-lab-{lab_id}/root"))
        }
    }

    fn ctx(session: &str) -> PluginContext {
        PluginContext {
            session_id: session.into(),
            platform: "desktop".into(),
        }
    }

    async fn call(sb: &Arc<Sandbox>, tool: &str, args: Value, session: &str) -> ToolOutcome {
        Arc::clone(sb).call(tool, &args, ctx(session)).await
    }

    /// Start a lab; its id.
    async fn start(sb: &Arc<Sandbox>, scenario: &str, session: &str) -> String {
        let out = call(
            sb,
            "sandbox_start",
            json!({ "scenario": scenario }),
            session,
        )
        .await;
        assert!(!out.is_error, "{}", out.content);
        out.artifact.unwrap().data["labId"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// The learner's command through the view.
    fn type_in(sb: &Sandbox, lab: &str, artifact: &str, command: &str) -> Value {
        sb.view_call(
            "run",
            &json!({ "labId": lab, "artifactId": artifact, "command": command }),
        )
        .unwrap()
    }

    async fn check(sb: &Arc<Sandbox>, scenario: &str, session: &str) -> String {
        let out = call(sb, "sandbox_check", json!({ "checkId": scenario }), session).await;
        assert!(!out.is_error, "{}", out.content);
        out.content
    }

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn the_package_loads_as_a_terminal_lab_plugin() {
        let sb = Sandbox::with_runtime(None);
        let m = sb.manifest();
        assert_eq!(m.id, "sandbox");
        assert_eq!(m.permissions, vec!["exec".to_owned()]);
        assert_eq!(m.platforms, vec!["desktop".to_owned()]);
        assert!(m.use_when.contains("none found"), "{}", m.use_when);
        let tools = sb.tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["sandbox_start", "sandbox_observe", "sandbox_check"]);
        let artifacts: Vec<bool> = m.tools.iter().map(|t| t.artifact).collect();
        assert_eq!(artifacts, [true, false, false]);
        let view = &m.view[0];
        assert_eq!((view.name.as_str(), view.answer), ("Terminal", true));
        assert_eq!(view.calls, vec!["run".to_owned()]);
        assert!(sb.view_html().unwrap().contains("'run'"));
        for agent in ["tutor", "practice"] {
            let words = sb.fragment(agent).split_whitespace().count();
            assert!((50..=250).contains(&words), "{agent}: {words} words");
        }
        // The schemas offer exactly the catalog's scenarios, and the start
        // tool's description lists each.
        let ids: Vec<Value> = SCENARIOS.iter().map(|s| json!(s.id)).collect();
        let start = &tools[0];
        assert_eq!(
            start.input_schema["properties"]["scenario"]["enum"],
            json!(ids)
        );
        assert_eq!(
            tools[2].input_schema["properties"]["checkId"]["enum"],
            json!(ids)
        );
        for s in &SCENARIOS {
            assert!(start.description.contains(&format!("`{}`", s.id)));
        }
        assert!(sb.view_call("exec", &json!({})).is_err());
    }

    #[test]
    fn image_names_follow_the_allowed_shape() {
        for ok in [
            "alpine",
            "alpine:3.20",
            "debian:bookworm-slim",
            "ghcr.io/foo/bar_baz:1.2-rc.1",
            "docker.io/library/alpine:3.20",
            "9base",
        ] {
            assert!(valid_image(ok), "{ok}");
        }
        for bad in [
            "",
            "Alpine",
            "-v",
            "--privileged",
            ".hidden",
            "/etc",
            "alpine:",
            "alpine:3.20:x",
            "alpine 3",
            "alpine;rm",
            "alpine:$(id)",
            "alpine@sha256:abc",
            "a\nb",
            &"a".repeat(201),
        ] {
            assert!(!valid_image(bad), "{bad:?}");
        }
        assert_eq!(qualified("alpine:3.20"), "docker.io/library/alpine:3.20");
        assert_eq!(qualified("ghcr.io/foo/bar:1"), "ghcr.io/foo/bar:1");
    }

    #[tokio::test]
    async fn a_missing_runtime_unknown_scenario_and_bad_image_are_clear_errors() {
        let none = Arc::new(Sandbox::with_runtime(None));
        let out = call(
            &none,
            "sandbox_start",
            json!({ "scenario": "permissions" }),
            "S",
        )
        .await;
        assert!(out.is_error, "{}", out.content);
        assert!(
            out.content.contains("isn't available") && out.content.contains("podman, docker"),
            "{}",
            out.content
        );

        let fake = Fake::new();
        let sb = fake.sandbox();
        let out = call(&sb, "sandbox_start", json!({ "scenario": "nope" }), "S").await;
        assert!(out.is_error);
        for s in &SCENARIOS {
            assert!(out.content.contains(s.id), "{}", out.content);
        }
        for image in ["Alpine", "--privileged", "alpine;id", "alpine:"] {
            let out = call(
                &sb,
                "sandbox_start",
                json!({ "scenario": "permissions", "image": image }),
                "S",
            )
            .await;
            assert!(
                out.is_error && out.content.contains("isn't a plain image name"),
                "{image}: {}",
                out.content
            );
        }
        // Nothing was started for any of them.
        assert_eq!(fake.log(), "");
        let out = call(&sb, "sandbox_observe", json!({}), "S").await;
        assert!(out.is_error && out.content.contains("no lab is running"));
        let out = call(
            &sb,
            "sandbox_check",
            json!({ "checkId": "permissions" }),
            "S",
        )
        .await;
        assert!(out.is_error && out.content.contains("no lab is running"));
    }

    #[tokio::test]
    async fn the_container_is_started_locked_down_and_the_image_is_qualified() {
        let fake = Fake::new();
        let sb = fake.sandbox();
        let lab = start(&sb, "permissions", "S1").await;
        let log = fake.log();
        let run = log.lines().next().unwrap();
        for part in [
            "run -d --rm --name lh-lab-",
            "--network none",
            "--memory 256m",
            "--cpus 1",
            "--pids-limit 256",
            "--cap-drop ALL",
            "--cap-add CHOWN,DAC_OVERRIDE,FOWNER,SETUID,SETGID,KILL",
            "--security-opt no-new-privileges",
            " docker.io/library/alpine:3.20 sleep 7200",
        ] {
            assert!(run.contains(part), "{part} missing from: {run}");
        }
        assert!(run.contains(&format!("lh-lab-{lab} ")));
        // The flag is not on the container (the learner could read it with
        // `env`); only setup and check receive it.
        assert!(!run.contains("LAB_FLAG"), "{run}");

        let out = call(
            &sb,
            "sandbox_start",
            json!({ "scenario": "permissions", "image": "ghcr.io/acme/lab:2" }),
            "S2",
        )
        .await;
        assert!(!out.is_error, "{}", out.content);
        let data = out.artifact.unwrap().data;
        assert_eq!(data["image"], json!("ghcr.io/acme/lab:2"));
        let image_of = |id: &str| {
            std::fs::read_to_string(fake.state().join(format!("labs/lh-lab-{id}/image")))
                .unwrap()
                .trim()
                .to_owned()
        };
        assert_eq!(
            image_of(data["labId"].as_str().unwrap()),
            "ghcr.io/acme/lab:2"
        );
        assert_eq!(image_of(&lab), "docker.io/library/alpine:3.20");
    }

    #[tokio::test]
    async fn a_runtime_that_cannot_start_the_container_surfaces_its_error() {
        let fake = Fake::new();
        std::fs::write(fake.state().join("fail-run"), "").unwrap();
        let sb = fake.sandbox();
        let out = call(
            &sb,
            "sandbox_start",
            json!({ "scenario": "permissions" }),
            "S",
        )
        .await;
        assert!(out.is_error, "{}", out.content);
        assert!(
            out.content.contains("couldn't start the lab container")
                && out.content.contains("unable to find image"),
            "{}",
            out.content
        );
        assert!(call(&sb, "sandbox_observe", json!({}), "S").await.is_error);
    }

    #[tokio::test]
    async fn permissions_is_failed_until_the_mode_is_600() {
        let fake = Fake::new();
        let sb = fake.sandbox();
        let lab = start(&sb, "permissions", "S").await;
        let file = fake.root(&lab).join("srv/app/secret.conf");
        assert_eq!(mode(&file), 0o666);

        let before = check(&sb, "permissions", "S").await;
        assert!(
            before.starts_with("FAIL permissions") && before.contains("fail: the mode is 666"),
            "{before}"
        );
        assert!(before.contains("ok: the owner is"), "{before}");
        let g = sb
            .grade_artifact(
                &json!({}),
                &json!({}),
                "A1",
                &json!({ "scenario": "permissions" }),
            )
            .unwrap();
        assert_eq!(g["correct"], json!(false), "{g}");
        assert!(g["detail"].as_str().unwrap().contains("ran no commands"));

        // Emptying the file and fixing the mode still fails: the contents matter.
        type_in(
            &sb,
            &lab,
            "A1",
            r#": > "$LAB_ROOT/srv/app/secret.conf" && chmod 600 "$LAB_ROOT/srv/app/secret.conf""#,
        );
        let wiped = check(&sb, "permissions", "S").await;
        assert!(
            wiped.contains("fail: the file's contents were changed"),
            "{wiped}"
        );

        let lab2 = start(&sb, "permissions", "S").await;
        let r = type_in(
            &sb,
            &lab2,
            "A2",
            r#"chmod 600 "$LAB_ROOT/srv/app/secret.conf""#,
        );
        assert_eq!(r["exit"], json!(0));
        assert_eq!(mode(&fake.root(&lab2).join("srv/app/secret.conf")), 0o600);
        let after = check(&sb, "permissions", "S").await;
        assert!(after.starts_with("PASS permissions"), "{after}");
        let g = sb
            .grade_artifact(
                &json!({}),
                &json!({}),
                "A2",
                &json!({ "scenario": "permissions" }),
            )
            .unwrap();
        assert_eq!(g["correct"], json!(true), "{g}");
        assert_eq!(g["transcript"].as_array().unwrap().len(), 1);
        assert_eq!(g["transcript"][0]["exit"], json!(0));
    }

    #[tokio::test]
    async fn find_the_flag_hides_one_flag_per_lab_and_checks_the_file_exactly() {
        let fake = Fake::new();
        let sb = fake.sandbox();
        let out = call(
            &sb,
            "sandbox_start",
            json!({ "scenario": "find-the-flag" }),
            "S",
        )
        .await;
        let artifact = out.artifact.clone().unwrap();
        let lab = artifact.data["labId"].as_str().unwrap().to_owned();
        let logs = fake.root(&lab).join("var/log/app");
        let mut holders = Vec::new();
        let mut flag = String::new();
        let files: Vec<_> = std::fs::read_dir(&logs).unwrap().flatten().collect();
        assert_eq!(files.len(), 6);
        for f in files {
            let text = std::fs::read_to_string(f.path()).unwrap();
            if let Some(at) = text.find("FLAG{") {
                holders.push(f.file_name());
                flag = text[at..text[at..].find('}').unwrap() + at + 1].to_owned();
            }
        }
        assert_eq!(holders.len(), 1, "{holders:?}");
        // Neither the tutor's result nor the card gives it away.
        let secret = &flag[5..flag.len() - 1];
        assert!(!out.content.contains(secret) && !artifact.svg.contains(secret));
        assert!(!artifact.data.to_string().contains(secret));

        let before = check(&sb, "find-the-flag", "S").await;
        assert!(before.contains("/tmp/flag.txt doesn't exist"), "{before}");
        // The learner's commands don't get LAB_FLAG.
        let r = type_in(&sb, &lab, "A", r#"echo "[$LAB_FLAG]""#);
        assert_eq!(r["output"], json!("[]"));
        let tmp = r#""$LAB_ROOT/tmp/flag.txt""#;
        type_in(&sb, &lab, "A", &format!("echo FLAG{{wrong}} > {tmp}"));
        let wrong = check(&sb, "find-the-flag", "S").await;
        assert!(wrong.contains("doesn't hold the flag"), "{wrong}");
        type_in(&sb, &lab, "A", &format!("echo 'found: {flag}' > {tmp}"));
        let padded = check(&sb, "find-the-flag", "S").await;
        assert!(padded.contains("other text around it"), "{padded}");
        assert!(!padded.contains(secret), "the check output leaks the flag");
        type_in(
            &sb,
            &lab,
            "A",
            &format!(r#"grep -rho 'FLAG{{[^}}]*}}' "$LAB_ROOT/var/log/app" > {tmp}"#),
        );
        let right = check(&sb, "find-the-flag", "S").await;
        assert!(right.starts_with("PASS find-the-flag"), "{right}");
        let g = sb
            .grade_artifact(
                &json!({}),
                &json!({}),
                "A",
                &json!({ "scenario": "find-the-flag" }),
            )
            .unwrap();
        assert_eq!(g["correct"], json!(true), "{g}");
        assert_eq!(g["transcript"].as_array().unwrap().len(), 4);

        // Another lab has its own flag.
        let other = start(&sb, "find-the-flag", "S2").await;
        let text = std::fs::read_dir(fake.root(&other).join("var/log/app"))
            .unwrap()
            .flatten()
            .map(|f| std::fs::read_to_string(f.path()).unwrap())
            .collect::<String>();
        assert!(!text.contains(secret));
    }

    #[tokio::test]
    async fn harden_sshd_needs_every_requirement() {
        let fake = Fake::new();
        let sb = fake.sandbox();
        let lab = start(&sb, "harden-sshd", "S").await;
        let before = check(&sb, "harden-sshd", "S").await;
        for want in [
            "fail: PermitRootLogin is yes, not no",
            "fail: PasswordAuthentication is yes, not no",
            "ok: Port 22 is still set",
            "fail: still yes: PermitRootLogin PasswordAuthentication PermitEmptyPasswords X11Forwarding",
        ] {
            assert!(before.contains(want), "{want} missing:\n{before}");
        }
        let conf = r#""$LAB_ROOT/etc/ssh/sshd_config""#;
        type_in(
            &sb,
            &lab,
            "A",
            &format!(
                "sed 's/^PermitRootLogin yes/PermitRootLogin no/; s/^PasswordAuthentication yes/PasswordAuthentication no/' {conf} > {conf}.new && mv {conf}.new {conf}"
            ),
        );
        let partial = check(&sb, "harden-sshd", "S").await;
        assert!(partial.starts_with("FAIL"), "{partial}");
        assert!(partial.contains("ok: PermitRootLogin is no"), "{partial}");
        assert!(
            partial.contains("fail: still yes: PermitEmptyPasswords X11Forwarding"),
            "{partial}"
        );
        // Dropping the keywords or the Port line is not hardening.
        type_in(
            &sb,
            &lab,
            "A",
            &format!(
                "sed 's/ yes$/ no/; /^Port/d; /^PermitRootLogin/d' {conf} > {conf}.new && mv {conf}.new {conf}"
            ),
        );
        let gone = check(&sb, "harden-sshd", "S").await;
        assert!(
            gone.contains("fail: PermitRootLogin is not set, not no"),
            "{gone}"
        );
        assert!(
            gone.contains("fail: the Port 22 line is gone or changed"),
            "{gone}"
        );
        type_in(
            &sb,
            &lab,
            "A",
            &format!("printf 'Port 22\\nPermitRootLogin no\\n' >> {conf}"),
        );
        let fixed = check(&sb, "harden-sshd", "S").await;
        assert!(fixed.starts_with("PASS harden-sshd"), "{fixed}");
        // A `yes` in a comment doesn't count.
        assert!(
            std::fs::read_to_string(fake.root(&lab).join("etc/ssh/sshd_config"))
                .unwrap()
                .contains("#PermitRootLogin prohibit-password")
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn stop_the_service_passes_once_nothing_listens() {
        let fake = Fake::new();
        let port = std::net::TcpListener::bind(("127.0.0.1", 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        std::fs::write(fake.state().join("port"), port.to_string()).unwrap();
        let sb = fake.sandbox();
        let lab = start(&sb, "stop-the-service", "S").await;
        assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_ok());
        let before = check(&sb, "stop-the-service", "S").await;
        assert!(
            before.starts_with("FAIL stop-the-service")
                && before.contains(&format!("still listens on port {port}")),
            "{before}"
        );
        let r = type_in(&sb, &lab, "A", r#"kill "$(cat "$LAB_ROOT/httpd.pid")""#);
        assert_eq!(r["exit"], json!(0), "{r}");
        let after = check(&sb, "stop-the-service", "S").await;
        assert!(after.starts_with("PASS stop-the-service"), "{after}");
        assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err());
        let g = sb
            .grade_artifact(
                &json!({}),
                &json!({}),
                "A",
                &json!({ "scenario": "stop-the-service" }),
            )
            .unwrap();
        assert_eq!(g["correct"], json!(true), "{g}");
    }

    #[tokio::test]
    async fn a_failing_setup_reports_the_error_and_removes_the_container() {
        let fake = Fake::new();
        std::fs::remove_file(fake.state().join("bin/busybox")).unwrap();
        let sb = fake.sandbox();
        let out = call(
            &sb,
            "sandbox_start",
            json!({ "scenario": "stop-the-service" }),
            "S",
        )
        .await;
        assert!(out.is_error, "{}", out.content);
        assert!(out.content.contains("setup failed"), "{}", out.content);
        assert!(fake.containers().is_empty(), "{:?}", fake.containers());
        assert!(fake.log().contains("rm -f lh-lab-"));
        assert!(call(&sb, "sandbox_observe", json!({}), "S").await.is_error);
    }

    #[tokio::test]
    async fn observe_returns_commands_and_output_after_an_entry() {
        let fake = Fake::new();
        let sb = fake.sandbox();
        let lab = start(&sb, "permissions", "S1").await;
        let none = call(&sb, "sandbox_observe", json!({}), "S1").await;
        assert!(
            none.content.contains("hasn't run anything"),
            "{}",
            none.content
        );

        let r = type_in(&sb, &lab, "A", "echo one");
        assert_eq!(
            (r["n"].clone(), r["output"].clone(), r["ok"].clone()),
            (json!(1), json!("one"), json!(true))
        );
        let r = type_in(&sb, &lab, "A", "ls /definitely/not/here");
        assert_ne!(r["exit"], json!(0));
        assert_eq!(r["ok"], json!(false));
        assert!(!r["output"].as_str().unwrap().is_empty(), "stderr is kept");
        // The command is one argument to `sh -c`: quoting, `;` and pipes work.
        let r = type_in(&sb, &lab, "A", r#"echo "a  b"; echo c | tr c d"#);
        assert_eq!(r["output"], json!("a  b\nd"));

        let all = call(&sb, "sandbox_observe", json!({}), "S1").await.content;
        assert!(
            all.starts_with("[1] $ echo one\none\n(exit 0)\n[2] $ ls /definitely/not/here\n"),
            "{all}"
        );
        assert!(
            all.contains("[3] $ echo \"a  b\"; echo c | tr c d\na  b\nd\n(exit 0)\n"),
            "{all}"
        );
        assert!(all.ends_with("Latest entry 3."), "{all}");
        let tail = call(&sb, "sandbox_observe", json!({ "since": "2" }), "S1")
            .await
            .content;
        assert!(tail.starts_with("[3] $") && !tail.contains("[2]"), "{tail}");
        let caught_up = call(&sb, "sandbox_observe", json!({ "since": "3" }), "S1").await;
        assert_eq!(
            caught_up.content,
            "No commands after entry 3 (latest entry 3)."
        );
        let bad = call(&sb, "sandbox_observe", json!({ "since": "abc" }), "S1").await;
        assert!(bad.is_error);
        // A check run by the tutor isn't a learner command.
        check(&sb, "permissions", "S1").await;
        let caught_up = call(&sb, "sandbox_observe", json!({ "since": "3" }), "S1").await;
        assert!(caught_up.content.contains("latest entry 3"));
        // Another chat has no lab of its own.
        assert!(call(&sb, "sandbox_observe", json!({}), "S2").await.is_error);
        // A check id of another scenario is refused.
        let out = call(
            &sb,
            "sandbox_check",
            json!({ "checkId": "harden-sshd" }),
            "S1",
        )
        .await;
        assert!(
            out.is_error && out.content.contains("use `permissions`"),
            "{}",
            out.content
        );
    }

    #[tokio::test]
    async fn the_transcript_keeps_the_last_entries_and_numbers_on() {
        let fake = Fake::new();
        let sb = fake.sandbox();
        let lab = start(&sb, "permissions", "S").await;
        for _ in 0..MAX_ENTRIES + 5 {
            type_in(&sb, &lab, "A", "true");
        }
        let text = call(&sb, "sandbox_observe", json!({}), "S").await.content;
        assert_eq!(text.matches("] $ true").count(), MAX_ENTRIES);
        assert!(
            text.starts_with("[6] $ true") && text.ends_with("Latest entry 205."),
            "{}",
            &text[..40]
        );
    }

    #[tokio::test]
    async fn a_new_lab_replaces_the_chats_old_one_and_labs_are_removed_on_drop() {
        let fake = Fake::new();
        let sb = fake.sandbox();
        let first = start(&sb, "permissions", "S1").await;
        type_in(&sb, &first, "A1", "echo hi");
        let other_chat = start(&sb, "permissions", "S2").await;
        assert_eq!(fake.containers().len(), 2);

        let second = start(&sb, "harden-sshd", "S1").await;
        let names = fake.containers();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(!names.contains(&format!("lh-lab-{first}")));
        assert!(names.contains(&format!("lh-lab-{second}")));
        assert!(names.contains(&format!("lh-lab-{other_chat}")));
        assert!(fake.log().contains(&format!("rm -f lh-lab-{first}")));
        // The old card's terminal and grading say the lab is gone.
        let err = sb
            .view_call(
                "run",
                &json!({ "labId": first, "artifactId": "A1", "command": "id" }),
            )
            .unwrap_err();
        assert!(err.contains("lab has ended"), "{err}");
        let err = sb
            .grade_artifact(
                &json!({}),
                &json!({}),
                "A1",
                &json!({ "scenario": "permissions" }),
            )
            .unwrap_err();
        assert!(err.contains("has ended"), "{err}");
        // The tutor now observes the new lab.
        let out = call(&sb, "sandbox_observe", json!({}), "S1").await;
        assert!(out.content.contains("hasn't run anything"));

        drop(sb);
        assert!(fake.containers().is_empty(), "{:?}", fake.containers());
    }

    #[tokio::test]
    async fn a_card_grades_the_lab_it_ran_commands_in() {
        let fake = Fake::new();
        let sb = fake.sandbox();
        let lab1 = start(&sb, "permissions", "S1").await;
        let lab2 = start(&sb, "permissions", "S2").await;
        let input = json!({ "scenario": "permissions" });
        let fix = r#"chmod 600 "$LAB_ROOT/srv/app/secret.conf""#;

        // With two labs, a card that never ran anything can't be matched.
        let err = sb
            .grade_artifact(&json!({}), &json!({}), "A1", &input)
            .unwrap_err();
        assert!(err.contains("several labs"), "{err}");

        type_in(&sb, &lab1, "A1", fix);
        type_in(&sb, &lab2, "A2", "echo not fixed");
        let g = sb
            .grade_artifact(&json!({}), &json!({}), "A1", &input)
            .unwrap();
        assert_eq!(g["correct"], json!(true), "{g}");
        let g = sb
            .grade_artifact(&json!({}), &json!({}), "A2", &input)
            .unwrap();
        assert_eq!(g["correct"], json!(false), "{g}");
        assert_eq!(g["transcript"][0]["command"], json!("echo not fixed"));

        // The binding is made by a card's first call and doesn't move.
        type_in(&sb, &lab2, "A1", "echo from the other lab");
        let g = sb
            .grade_artifact(&json!({}), &json!({}), "A1", &input)
            .unwrap();
        assert_eq!(g["correct"], json!(true), "{g}");
        assert_eq!(g["transcript"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_unbound_card_uses_the_only_lab_when_its_scenario_matches() {
        let fake = Fake::new();
        let sb = fake.sandbox();
        let err = sb
            .grade_artifact(
                &json!({}),
                &json!({}),
                "A",
                &json!({ "scenario": "permissions" }),
            )
            .unwrap_err();
        assert!(err.contains("no lab is running"), "{err}");
        start(&sb, "permissions", "S").await;
        let err = sb
            .grade_artifact(
                &json!({}),
                &json!({}),
                "A",
                &json!({ "scenario": "harden-sshd" }),
            )
            .unwrap_err();
        assert!(
            err.contains("`permissions`") && err.contains("`harden-sshd`"),
            "{err}"
        );
        let g = sb
            .grade_artifact(
                &json!({}),
                &json!({}),
                "A",
                &json!({ "scenario": "permissions" }),
            )
            .unwrap();
        assert_eq!(g["correct"], json!(false));
    }

    #[tokio::test]
    async fn bad_run_calls_are_refused() {
        let fake = Fake::new();
        let sb = fake.sandbox();
        let lab = start(&sb, "permissions", "S").await;
        assert!(sb.view_call("run", &json!({ "command": "id" })).is_err());
        assert!(sb.view_call("run", &json!({ "labId": lab })).is_err());
        let err = sb
            .view_call("run", &json!({ "labId": "nope", "command": "id" }))
            .unwrap_err();
        assert!(err.contains("lab has ended"), "{err}");
        let err = sb
            .view_call("run", &json!({ "labId": lab, "command": "  " }))
            .unwrap_err();
        assert!(err.contains("empty"), "{err}");
        let long = "x".repeat(MAX_COMMAND + 1);
        assert!(sb
            .view_call("run", &json!({ "labId": lab, "command": long }))
            .is_err());
        // Refused commands are not transcript entries.
        let out = call(&sb, "sandbox_observe", json!({}), "S").await;
        assert!(out.content.contains("hasn't run anything"));
    }

    #[tokio::test]
    async fn a_command_past_its_time_or_output_limit_is_stopped_and_noted() {
        let fake = Fake::new();
        let sb = Arc::new(
            Sandbox::with_runtime(Some(fake.state().join("runtime")))
                .with_limits(Duration::from_millis(500), 1000),
        );
        let lab = start(&sb, "permissions", "S").await;
        let t = Instant::now();
        let r = type_in(&sb, &lab, "A", "echo before; sleep 20");
        assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
        let text = r["output"].as_str().unwrap();
        assert!(
            text.contains("time limit") && text.contains("may still be running"),
            "{text}"
        );
        assert_eq!(
            (r["exit"].clone(), r["ok"].clone()),
            (Value::Null, json!(false))
        );
        let r = type_in(&sb, &lab, "A", "yes abcdefgh");
        let text = r["output"].as_str().unwrap();
        assert!(text.contains("output limit"), "{text}");
        assert!(text.len() < 1100, "{}", text.len());
        // The lab goes on after a stopped command.
        let r = type_in(&sb, &lab, "A", "echo alive");
        assert_eq!(r["output"], json!("alive"));
        let seen = call(&sb, "sandbox_observe", json!({}), "S").await.content;
        assert!(seen.contains("(stopped)"), "{seen}");
    }

    #[tokio::test]
    async fn a_container_that_vanished_is_reported_not_graded() {
        let fake = Fake::new();
        let sb = fake.sandbox();
        let lab = start(&sb, "permissions", "S").await;
        std::fs::remove_dir_all(fake.state().join(format!("labs/lh-lab-{lab}"))).unwrap();
        let out = call(
            &sb,
            "sandbox_check",
            json!({ "checkId": "permissions" }),
            "S",
        )
        .await;
        assert!(
            out.is_error && out.content.contains("start a new lab"),
            "{}",
            out.content
        );
        let err = sb
            .grade_artifact(
                &json!({}),
                &json!({}),
                "A",
                &json!({ "scenario": "permissions" }),
            )
            .unwrap_err();
        assert!(err.contains("start a new lab"), "{err}");
        let r = type_in(&sb, &lab, "A", "id");
        assert!(r["output"].as_str().unwrap().contains("stopped"), "{r}");
    }
}
