//! Running local programs for the `exec` built-ins (code-runner, repl, cas,
//! sandbox): toolchain discovery, one-shot runs and persistent processes.
//!
//! Every run gets:
//! - a fresh temporary working directory, removed when the run (or the
//!   persistent process) is dropped; `HOME` and `TMPDIR` point into it;
//! - a scrubbed environment: only `PATH` survives from the app's own
//!   environment (so installed toolchains are found), plus `LANG`/`LC_ALL`
//!   `C.UTF-8`, `TERM=dumb` and what the caller adds. API keys and tokens
//!   in the app's environment never reach the program;
//! - a wall-clock timeout and a cap on the bytes it may print; hitting
//!   either kills it and says so in the result;
//! - kill-on-drop: the child is the leader of its own process group, and
//!   the whole group is killed when the run ends or its handle is dropped.
//!
//! What isolation is real, by OS:
//! - **Linux**, when unprivileged user namespaces work (probed once with
//!   `unshare`, util-linux): the program runs under `unshare --user
//!   --map-root-user --net --pid --fork --kill-child`. It has **no network**
//!   (a fresh network namespace with only a down loopback) and its own PID
//!   namespace, so killing the run kills every descendant even if one left
//!   the process group. `prlimit` (when present) adds CPU-seconds and
//!   file-size limits. The file system is **not** isolated: the program runs
//!   as the learner's own user and can read and write what they can.
//! - **Linux without user namespaces, macOS, Windows**: no network or file
//!   system isolation; timeout, output cap, process-group kill (Unix), env
//!   scrubbing and the temp directory still apply.
//! - The `sandbox` plugin is different: its labs are containers (podman or
//!   docker) with their own file system and an internal or no network.
//!
//! None of this makes hostile code safe to run; it keeps a learner's and a
//! tutor's ordinary mistakes (an infinite loop, a print in a loop, a stray
//! request) from hurting the machine. Enabling an `exec` plugin is consent
//! to run programs (docs/plugins.md §12).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

/// Default limits for one run.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
pub const DEFAULT_OUTPUT_CAP: usize = 64 * 1024;
/// Largest file a run may write (`prlimit --fsize`, Linux).
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Find `name` on `PATH` (with the platform's executable suffixes).
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: &[&str] = if cfg!(windows) {
        &[".exe", ".cmd", ".bat", ""]
    } else {
        &[""]
    };
    std::env::split_paths(&path).find_map(|dir| {
        exts.iter()
            .map(|ext| dir.join(format!("{name}{ext}")))
            .find(|p| is_executable(p))
    })
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// The first of `names` found on `PATH`.
pub fn find_any(names: &[&str]) -> Option<PathBuf> {
    names.iter().find_map(|n| which(n))
}

/// The error a tool returns when a toolchain is missing.
pub fn missing(what: &str, names: &[&str], hint: &str) -> String {
    format!(
        "{what} isn't available on this device: none of [{}] is on PATH. {hint}",
        names.join(", ")
    )
}

/// What isolation [`run`] and [`Persistent`] apply on this device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Isolation {
    /// Linux user + network + PID namespaces (no network).
    Namespaces,
    /// Timeout, output cap, process group, env scrubbing only.
    ProcessOnly,
}

/// Probed once: do unprivileged user + network + PID namespaces work here?
static ISOLATION: LazyLock<Isolation> = LazyLock::new(|| {
    if !cfg!(target_os = "linux") {
        return Isolation::ProcessOnly;
    }
    let Some(unshare) = which("unshare") else {
        return Isolation::ProcessOnly;
    };
    let ok = Command::new(unshare)
        .args(UNSHARE_ARGS)
        .arg("true")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        Isolation::Namespaces
    } else {
        Isolation::ProcessOnly
    }
});

/// What isolation applies on this device (probed on first use).
pub fn isolation() -> Isolation {
    *ISOLATION
}

const UNSHARE_ARGS: &[&str] = &[
    "--user",
    "--map-root-user",
    "--net",
    "--pid",
    "--fork",
    "--kill-child",
];

/// A temporary directory removed on drop.
#[derive(Debug)]
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(prefix: &str) -> std::io::Result<Self> {
        let path = std::env::temp_dir().join(format!("lh-{prefix}-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// One program to run.
#[derive(Debug, Clone)]
pub struct Spec {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Extra environment on top of the scrubbed base.
    pub env: Vec<(String, String)>,
    pub timeout: Duration,
    pub output_cap: usize,
    /// Apply namespace isolation when available. Off only for container
    /// clients (podman/docker must reach their service or socket).
    pub isolate: bool,
}

impl Spec {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
            output_cap: DEFAULT_OUTPUT_CAP,
            isolate: true,
        }
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.to_owned(), value.to_owned()));
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn output_cap(mut self, cap: usize) -> Self {
        self.output_cap = cap;
        self
    }

    pub fn no_isolation(mut self) -> Self {
        self.isolate = false;
        self
    }

    /// The program must exist: behind the `unshare`/`prlimit` wrapper a
    /// missing program would come back as exit code 127, not a start error.
    fn check_program(&self) -> Result<(), String> {
        let found = if self.program.components().count() > 1 {
            self.program.exists()
        } else {
            which(&self.program.to_string_lossy()).is_some()
        };
        if found {
            Ok(())
        } else {
            Err(format!(
                "couldn't start {} (not found)",
                self.program.display()
            ))
        }
    }

    /// The command with the scrubbed environment and the isolation wrapper.
    fn command(&self, workdir: &Path) -> Command {
        let mut cmd = if self.isolate && isolation() == Isolation::Namespaces {
            let mut c = Command::new(which("unshare").unwrap_or_else(|| "unshare".into()));
            c.args(UNSHARE_ARGS);
            if let Some(prlimit) = which("prlimit") {
                c.arg(prlimit).args([
                    format!("--cpu={}", self.timeout.as_secs() + 1),
                    format!("--fsize={MAX_FILE_BYTES}"),
                ]);
            }
            c.arg(&self.program);
            c
        } else {
            Command::new(&self.program)
        };
        cmd.args(&self.args).current_dir(workdir).env_clear();
        if let Some(path) = std::env::var_os("PATH") {
            cmd.env("PATH", path);
        }
        #[cfg(windows)]
        for key in ["SYSTEMROOT", "WINDIR", "PATHEXT", "COMSPEC"] {
            if let Some(v) = std::env::var_os(key) {
                cmd.env(key, v);
            }
        }
        cmd.env("HOME", workdir)
            .env("TMPDIR", workdir)
            .env("TMP", workdir)
            .env("TEMP", workdir)
            .env("LANG", "C.UTF-8")
            .env("LC_ALL", "C.UTF-8")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1");
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        cmd
    }
}

/// What a finished run produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Output {
    pub stdout: String,
    pub stderr: String,
    /// Exit code; `None` when it was killed (timeout, cap) or by a signal.
    pub code: Option<i32>,
    pub timed_out: bool,
    /// The output cap was hit and the program was killed.
    pub truncated: bool,
}

impl Output {
    pub fn success(&self) -> bool {
        self.code == Some(0) && !self.timed_out && !self.truncated
    }

    /// stdout then stderr, with a note when the run was cut short.
    pub fn combined(&self) -> String {
        let mut s = self.stdout.clone();
        if !self.stderr.is_empty() {
            if !s.is_empty() && !s.ends_with('\n') {
                s.push('\n');
            }
            s.push_str(&self.stderr);
        }
        if let Some(note) = self.note() {
            if !s.is_empty() && !s.ends_with('\n') {
                s.push('\n');
            }
            s.push_str(&note);
        }
        s
    }

    /// Why the run was cut short, if it was.
    pub fn note(&self) -> Option<String> {
        if self.timed_out {
            Some("[stopped: time limit reached]".into())
        } else if self.truncated {
            Some("[stopped: output limit reached]".into())
        } else {
            None
        }
    }
}

/// Kills the child's process group (and the child) when dropped.
struct KillOnDrop {
    child: Child,
}

impl KillOnDrop {
    fn kill(&mut self) {
        kill_group(self.child.id());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            self.kill();
        }
    }
}

#[cfg(unix)]
fn kill_group(pid: u32) {
    // `kill -KILL -<pgid>`: the group leader is the child (`process_group(0)`).
    let _ = Command::new("kill")
        .args(["-KILL", &format!("-{pid}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(unix))]
fn kill_group(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Read a pipe into a buffer, counting against a shared cap; past the cap
/// it stops keeping bytes and raises `over`.
fn reader(
    mut pipe: impl Read + Send + 'static,
    total: Arc<AtomicUsize>,
    cap: usize,
    over: Arc<AtomicBool>,
) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let before = total.fetch_add(n, Ordering::SeqCst);
                    let room = cap.saturating_sub(before);
                    kept.extend_from_slice(&buf[..n.min(room)]);
                    if before + n > cap {
                        over.store(true, Ordering::SeqCst);
                    }
                }
            }
        }
        let _ = tx.send(kept);
    });
    rx
}

/// Run `spec` to completion in `workdir` with `stdin`, under its timeout
/// and output cap. Errors only when the program can't be started.
pub fn run_in(spec: &Spec, workdir: &Path, stdin: &str) -> Result<Output, String> {
    spec.check_program()?;
    let mut cmd = spec.command(workdir);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd
        .spawn()
        .map_err(|e| format!("couldn't start {} ({e})", spec.program.display()))?;
    let mut guard = KillOnDrop { child };
    let total = Arc::new(AtomicUsize::new(0));
    let over = Arc::new(AtomicBool::new(false));
    let out_rx = reader(
        guard.child.stdout.take().expect("piped"),
        Arc::clone(&total),
        spec.output_cap,
        Arc::clone(&over),
    );
    let err_rx = reader(
        guard.child.stderr.take().expect("piped"),
        Arc::clone(&total),
        spec.output_cap,
        Arc::clone(&over),
    );
    if let Some(mut pipe) = guard.child.stdin.take() {
        let input = stdin.as_bytes().to_vec();
        // A program that never reads its input must not block the run.
        std::thread::spawn(move || {
            let _ = pipe.write_all(&input);
        });
    }
    let started = Instant::now();
    let mut out = Output::default();
    loop {
        if let Ok(Some(status)) = guard.child.try_wait() {
            out.code = status.code();
            // A descendant may still hold the pipes; the group goes too.
            kill_group(guard.child.id());
            break;
        }
        if over.load(Ordering::SeqCst) {
            out.truncated = true;
            guard.kill();
            break;
        }
        if started.elapsed() >= spec.timeout {
            out.timed_out = true;
            guard.kill();
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let collect = |rx: mpsc::Receiver<Vec<u8>>| {
        rx.recv_timeout(Duration::from_secs(2))
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default()
    };
    out.stdout = collect(out_rx);
    out.stderr = collect(err_rx);
    if over.load(Ordering::SeqCst) && !out.truncated {
        // Finished, but printed more than the cap: the rest was dropped.
        out.truncated = true;
    }
    Ok(out)
}

/// [`run_in`] in a fresh temporary directory, after writing `files` into it.
pub fn run(spec: &Spec, files: &[(&str, &str)], stdin: &str) -> Result<Output, String> {
    let dir = TempDir::new("run").map_err(|e| format!("can't make a working directory ({e})"))?;
    for (name, text) in files {
        std::fs::write(dir.path().join(name), text)
            .map_err(|e| format!("can't write {name} ({e})"))?;
    }
    run_in(spec, dir.path(), stdin)
}

/// A long-lived program (a REPL) that takes input on stdin and answers on
/// stdout+stderr (one pipe, so their order is kept). Killed with its whole
/// process group, and its working directory removed, when dropped.
pub struct Persistent {
    guard: KillOnDrop,
    stdin: std::process::ChildStdin,
    rx: mpsc::Receiver<Vec<u8>>,
    /// Bytes read past the last marker, kept for the next exchange.
    pending: Vec<u8>,
    _dir: TempDir,
}

/// One exchange's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exchange {
    /// Everything printed before the marker.
    pub output: String,
    pub timed_out: bool,
    pub truncated: bool,
}

impl Persistent {
    pub fn spawn(spec: &Spec, files: &[(&str, &str)]) -> Result<Self, String> {
        spec.check_program()?;
        let dir =
            TempDir::new("repl").map_err(|e| format!("can't make a working directory ({e})"))?;
        for (name, text) in files {
            std::fs::write(dir.path().join(name), text)
                .map_err(|e| format!("can't write {name} ({e})"))?;
        }
        let (read, write) = std::io::pipe().map_err(|e| format!("can't make a pipe ({e})"))?;
        let write2 = write
            .try_clone()
            .map_err(|e| format!("can't make a pipe ({e})"))?;
        let mut cmd = spec.command(dir.path());
        cmd.stdin(Stdio::piped()).stdout(write).stderr(write2);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("couldn't start {} ({e})", spec.program.display()))?;
        // Our copies of the write end must close, or EOF never arrives.
        drop(cmd);
        let stdin = child.stdin.take().expect("piped");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut read = read;
            let mut buf = [0u8; 8192];
            loop {
                match read.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Ok(Self {
            guard: KillOnDrop { child },
            stdin,
            rx,
            pending: Vec::new(),
            _dir: dir,
        })
    }

    /// Whether the process is still running.
    pub fn alive(&mut self) -> bool {
        matches!(self.guard.child.try_wait(), Ok(None))
    }

    /// Write `input`, then read until the line `marker` appears (the input
    /// must make the program print it when done). On timeout or past `cap`
    /// bytes the process is killed: the caller should drop it.
    pub fn exchange(
        &mut self,
        input: &str,
        marker: &str,
        timeout: Duration,
        cap: usize,
    ) -> Result<Exchange, String> {
        self.stdin
            .write_all(input.as_bytes())
            .and_then(|()| self.stdin.flush())
            .map_err(|e| format!("the process has stopped ({e})"))?;
        let started = Instant::now();
        let needle = marker.as_bytes();
        let mut buf = std::mem::take(&mut self.pending);
        loop {
            if let Some(at) = find(&buf, needle) {
                let rest = buf.split_off(at + needle.len());
                buf.truncate(at);
                // Drop the marker's line ending.
                self.pending = rest
                    .strip_prefix(b"\r\n")
                    .or_else(|| rest.strip_prefix(b"\n"))
                    .unwrap_or(&rest)
                    .to_vec();
                return Ok(Exchange {
                    output: String::from_utf8_lossy(&buf).into_owned(),
                    timed_out: false,
                    truncated: false,
                });
            }
            if buf.len() > cap {
                self.guard.kill();
                buf.truncate(cap);
                return Ok(Exchange {
                    output: String::from_utf8_lossy(&buf).into_owned(),
                    timed_out: false,
                    truncated: true,
                });
            }
            let left = timeout.saturating_sub(started.elapsed());
            if left.is_zero() {
                self.guard.kill();
                return Ok(Exchange {
                    output: String::from_utf8_lossy(&buf).into_owned(),
                    timed_out: true,
                    truncated: false,
                });
            }
            match self.rx.recv_timeout(left.min(Duration::from_millis(50))) {
                Ok(chunk) => buf.extend_from_slice(&chunk),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = self.guard.child.wait();
                    return Err(format!(
                        "the process exited{}",
                        if buf.is_empty() {
                            String::new()
                        } else {
                            format!(": {}", String::from_utf8_lossy(&buf).trim())
                        }
                    ));
                }
            }
        }
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Cut `text` to at most `max` bytes at a char boundary, with a note.
pub fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut s = text[..text.floor_char_boundary(max)].to_owned();
    s.push_str("\n[cut]");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh() -> PathBuf {
        which("sh").expect("sh on PATH")
    }

    #[test]
    fn a_run_returns_output_and_exit_code() {
        let out = run(
            &Spec::new(sh()).args(["-c", "cat; echo err >&2; exit 3"]),
            &[],
            "hello",
        )
        .unwrap();
        assert_eq!(out.stdout, "hello");
        assert_eq!(out.stderr.trim(), "err");
        assert_eq!(out.code, Some(3));
        assert!(!out.success());
    }

    #[test]
    fn the_environment_is_scrubbed_and_home_is_the_temp_dir() {
        // Cargo sets CARGO_MANIFEST_DIR for the test process; it must not
        // reach the program (nor would an API key).
        assert!(std::env::var_os("CARGO_MANIFEST_DIR").is_some());
        let out = run(
            &Spec::new(sh()).args(["-c", "echo \"${CARGO_MANIFEST_DIR:-none} $HOME\"; pwd"]),
            &[],
            "",
        )
        .unwrap();
        let mut lines = out.stdout.lines();
        let first = lines.next().unwrap();
        let pwd = lines.next().unwrap();
        assert_eq!(first, format!("none {pwd}"), "{out:?}");
        assert!(pwd.contains("lh-run-"), "{pwd}");
    }

    #[test]
    fn a_run_past_its_timeout_is_killed_with_its_children() {
        let marker = std::env::temp_dir().join(format!("lh-orphan-{}", ulid::Ulid::new()));
        let script = format!("(sleep 2; touch {}) & sleep 30", marker.display());
        let started = Instant::now();
        let out = run(
            &Spec::new(sh())
                .args(["-c", &script])
                .timeout(Duration::from_millis(300)),
            &[],
            "",
        )
        .unwrap();
        assert!(out.timed_out && out.code.is_none(), "{out:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(out.combined().contains("time limit"));
        // The background child died with the group: it never touches the file.
        std::thread::sleep(Duration::from_millis(2500));
        assert!(!marker.exists(), "a child outlived the run");
    }

    #[test]
    fn output_past_the_cap_is_cut_and_the_program_killed() {
        let started = Instant::now();
        let out = run(
            &Spec::new(sh())
                .args(["-c", "while true; do echo yyyyyyyyyyyyyyyy; done"])
                .output_cap(1000),
            &[],
            "",
        )
        .unwrap();
        assert!(out.truncated, "{:?}", out.code);
        assert!(out.stdout.len() <= 1000);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn the_working_directory_is_removed_after_the_run() {
        let out = run(&Spec::new(sh()).args(["-c", "pwd"]), &[("a.txt", "x")], "").unwrap();
        let dir = PathBuf::from(out.stdout.trim());
        assert!(!dir.exists(), "{dir:?} left behind");
    }

    #[test]
    fn a_missing_program_is_an_error_not_a_panic() {
        let err = run(&Spec::new("/nonexistent/lh-nothing"), &[], "").unwrap_err();
        assert!(err.contains("couldn't start"), "{err}");
        assert!(which("lh-surely-not-a-program").is_none());
        let msg = missing("Python", &["python3"], "Install Python 3.");
        assert!(msg.contains("python3") && msg.contains("Install Python 3."));
    }

    #[test]
    fn isolated_runs_have_no_network() {
        if isolation() != Isolation::Namespaces {
            eprintln!("skipped: no user namespaces here");
            return;
        }
        // Only the loopback interface exists, and it is down.
        let out = run(&Spec::new(sh()).args(["-c", "cat /proc/net/dev"]), &[], "").unwrap();
        let ifaces: Vec<&str> = out
            .stdout
            .lines()
            .skip(2)
            .filter_map(|l| l.split(':').next())
            .map(str::trim)
            .collect();
        assert_eq!(ifaces, ["lo"], "{}", out.stdout);
    }

    #[test]
    fn a_persistent_process_keeps_state_and_dies_on_timeout() {
        let mut p = Persistent::spawn(&Spec::new(sh()).args(["-s"]), &[]).unwrap();
        let a = p
            .exchange("X=41\necho __M__\n", "__M__", Duration::from_secs(5), 4096)
            .unwrap();
        assert_eq!(a.output, "");
        let b = p
            .exchange(
                "echo $((X+1)); echo oops >&2; echo __M__\n",
                "__M__",
                Duration::from_secs(5),
                4096,
            )
            .unwrap();
        assert_eq!(b.output, "42\noops\n");
        let c = p
            .exchange(
                "sleep 30; echo __M__\n",
                "__M__",
                Duration::from_millis(300),
                4096,
            )
            .unwrap();
        assert!(c.timed_out);
        assert!(!p.alive());
    }
}
