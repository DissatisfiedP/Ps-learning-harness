//! Focus mode: silence desktop notifications while a session is open
//! (ui-design §7.8). Best effort, Linux only.
//!
//! One backend is detected per call, in order: mako, swaync, dunst, GNOME.
//! A backend counts only when its binary is on `PATH` and a live state query
//! succeeds. The app never touches do-not-disturb the learner already had
//! on: it queries first and only undoes what it did. When the app turns DND
//! on it writes a marker file naming the backend; restoring removes it, and
//! startup restores from a leftover marker (crash safety).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A notification daemon the app knows how to pause.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Mako,
    Swaync,
    Dunst,
    Gnome,
}

/// A program and its arguments, built without running anything.
#[derive(Debug, PartialEq, Eq)]
pub struct Cmd {
    pub program: &'static str,
    pub args: Vec<&'static str>,
}

const MAKO_MODE: &str = "do-not-disturb";
const GNOME_SCHEMA: &str = "org.gnome.desktop.notifications";
const GNOME_KEY: &str = "show-banners";
const QUERY_TIMEOUT: Duration = Duration::from_secs(2);

impl Backend {
    /// Detection order.
    pub const ALL: [Backend; 4] = [
        Backend::Mako,
        Backend::Swaync,
        Backend::Dunst,
        Backend::Gnome,
    ];

    /// Short name shown in Settings and stored in the marker file.
    pub fn name(self) -> &'static str {
        match self {
            Backend::Mako => "mako",
            Backend::Swaync => "swaync",
            Backend::Dunst => "dunst",
            Backend::Gnome => "GNOME",
        }
    }

    pub fn from_name(name: &str) -> Option<Backend> {
        Backend::ALL.into_iter().find(|b| b.name() == name.trim())
    }

    pub fn program(self) -> &'static str {
        match self {
            Backend::Mako => "makoctl",
            Backend::Swaync => "swaync-client",
            Backend::Dunst => "dunstctl",
            Backend::Gnome => "gsettings",
        }
    }

    /// The command whose stdout [`Backend::parse_dnd`] reads.
    pub fn query(self) -> Cmd {
        let args = match self {
            Backend::Mako => vec!["mode"],
            Backend::Swaync => vec!["-D"],
            Backend::Dunst => vec!["is-paused"],
            Backend::Gnome => vec!["get", GNOME_SCHEMA, GNOME_KEY],
        };
        Cmd {
            program: self.program(),
            args,
        }
    }

    /// Whether do-not-disturb is on, from the query's stdout; `None` when the
    /// output is not recognised.
    pub fn parse_dnd(self, stdout: &str) -> Option<bool> {
        let out = stdout.trim();
        match self {
            Backend::Mako => Some(out.lines().any(|l| l.trim() == MAKO_MODE)),
            Backend::Swaync | Backend::Dunst => parse_bool(out),
            // Banners hidden = do-not-disturb.
            Backend::Gnome => parse_bool(out).map(|shown| !shown),
        }
    }

    /// The command that turns do-not-disturb on or off.
    pub fn set(self, on: bool) -> Cmd {
        let args = match (self, on) {
            (Backend::Mako, true) => vec!["mode", "-a", MAKO_MODE],
            (Backend::Mako, false) => vec!["mode", "-r", MAKO_MODE],
            (Backend::Swaync, true) => vec!["-dn"],
            (Backend::Swaync, false) => vec!["-df"],
            (Backend::Dunst, on) => vec!["set-paused", if on { "true" } else { "false" }],
            (Backend::Gnome, on) => vec![
                "set",
                GNOME_SCHEMA,
                GNOME_KEY,
                if on { "false" } else { "true" },
            ],
        };
        Cmd {
            program: self.program(),
            args,
        }
    }

    /// Whether this backend may be tried on the given desktop.
    fn applies(self, desktop: &str) -> bool {
        self != Backend::Gnome || desktop.split(':').any(|d| d.eq_ignore_ascii_case("GNOME"))
    }
}

fn parse_bool(s: &str) -> Option<bool> {
    match s {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// The first backend whose binary exists and whose live query parses.
/// `on_path` and `run` are injected so detection is testable without a system.
pub fn detect(
    desktop: &str,
    on_path: impl Fn(&str) -> bool,
    run: impl Fn(&Cmd) -> Option<String>,
) -> Option<(Backend, bool)> {
    Backend::ALL.into_iter().find_map(|b| {
        if !b.applies(desktop) || !on_path(b.program()) {
            return None;
        }
        let state = b.parse_dnd(&run(&b.query())?)?;
        Some((b, state))
    })
}

/// What `enable` decided, given the detected backend and its current state.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// No backend, or the learner already had DND on: leave it alone.
    Nothing,
    /// Turn DND on and record the marker.
    TurnOn(Backend),
}

pub fn plan_enable(detected: Option<(Backend, bool)>) -> Plan {
    match detected {
        Some((b, false)) => Plan::TurnOn(b),
        _ => Plan::Nothing,
    }
}

// ---- system side --------------------------------------------------------------

/// Managed state: the marker path and a lock that serialises toggles.
pub struct Focus {
    marker: PathBuf,
    lock: Mutex<()>,
}

impl Focus {
    pub fn new(config_dir: &Path) -> Focus {
        Focus {
            marker: config_dir.join("focus-dnd"),
            lock: Mutex::new(()),
        }
    }

    /// The detected backend's name, or `None` when unsupported.
    pub fn support(&self) -> Option<&'static str> {
        detect_system().map(|(b, _)| b.name())
    }

    /// Turn DND on unless the learner already had it on.
    pub fn enable(&self) -> Result<(), String> {
        let _guard = self.lock.lock().map_err(|e| e.to_string())?;
        if self.marker.exists() {
            return Ok(()); // already ours
        }
        let Plan::TurnOn(b) = plan_enable(detect_system()) else {
            return Ok(());
        };
        run_ok(&b.set(true))?;
        // Write the marker only after DND is on, so a failed write is the
        // only way to leave DND behind; report that.
        std::fs::write(&self.marker, b.name()).map_err(|e| {
            let _ = run_ok(&b.set(false));
            format!("focus marker: {e}")
        })
    }

    /// Undo what [`Focus::enable`] did; a no-op without a marker.
    pub fn restore(&self) -> Result<(), String> {
        let _guard = self.lock.lock().map_err(|e| e.to_string())?;
        let Ok(name) = std::fs::read_to_string(&self.marker) else {
            return Ok(());
        };
        // Clear the marker even if the command fails: a stale marker would
        // block the next `enable`, and daemon modes die with the daemon.
        let removed = std::fs::remove_file(&self.marker).map_err(|e| format!("focus marker: {e}"));
        if let Some(b) = Backend::from_name(&name) {
            run_ok(&b.set(false))?;
        }
        removed
    }
}

fn detect_system() -> Option<(Backend, bool)> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    detect(
        &desktop,
        |bin| dirs.iter().any(|d| d.join(bin).is_file()),
        run_stdout,
    )
}

/// Run with a timeout; stdout when the command exits 0.
fn run_stdout(cmd: &Cmd) -> Option<String> {
    let mut child = Command::new(cmd.program)
        .args(&cmd.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < QUERY_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(10))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    if !status.success() {
        return None;
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    Some(out)
}

fn run_ok(cmd: &Cmd) -> Result<(), String> {
    run_stdout(cmd)
        .map(|_| ())
        .ok_or_else(|| format!("{} failed", cmd.program))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_commands() {
        assert_eq!(
            Backend::Mako.set(true).args,
            ["mode", "-a", "do-not-disturb"]
        );
        assert_eq!(
            Backend::Mako.set(false).args,
            ["mode", "-r", "do-not-disturb"]
        );
        assert_eq!(Backend::Swaync.set(true).args, ["-dn"]);
        assert_eq!(Backend::Swaync.set(false).args, ["-df"]);
        assert_eq!(Backend::Dunst.set(true).args, ["set-paused", "true"]);
        assert_eq!(Backend::Dunst.set(false).args, ["set-paused", "false"]);
        // GNOME: DND on = banners off.
        assert_eq!(
            Backend::Gnome.set(true).args,
            [
                "set",
                "org.gnome.desktop.notifications",
                "show-banners",
                "false"
            ]
        );
        assert_eq!(Backend::Gnome.set(false).args.last(), Some(&"true"));
    }

    #[test]
    fn parse_state() {
        assert_eq!(
            Backend::Mako.parse_dnd("default\ndo-not-disturb\n"),
            Some(true)
        );
        assert_eq!(Backend::Mako.parse_dnd("default\n"), Some(false));
        assert_eq!(Backend::Mako.parse_dnd(""), Some(false));
        assert_eq!(Backend::Swaync.parse_dnd("true\n"), Some(true));
        assert_eq!(Backend::Dunst.parse_dnd("false\n"), Some(false));
        assert_eq!(Backend::Dunst.parse_dnd("garbage"), None);
        assert_eq!(Backend::Gnome.parse_dnd("false\n"), Some(true));
        assert_eq!(Backend::Gnome.parse_dnd("true"), Some(false));
    }

    #[test]
    fn detection_order_and_live_query() {
        let all = |_: &str| true;
        let ok = |c: &Cmd| {
            Some(if c.program == "makoctl" {
                "default".into()
            } else {
                "false".into()
            })
        };
        assert_eq!(detect("GNOME", all, ok), Some((Backend::Mako, false)));
        // A binary without a running daemon (query fails) is skipped.
        let no_mako = |c: &Cmd| (c.program != "makoctl").then(|| "true".to_string());
        assert_eq!(detect("sway", all, no_mako), Some((Backend::Swaync, true)));
        // Unparseable output is skipped too.
        let bad_swaync = |c: &Cmd| match c.program {
            "swaync-client" => Some("??".to_string()),
            _ => Some("false".to_string()),
        };
        assert_eq!(
            detect("sway", |b| b != "makoctl", bad_swaync),
            Some((Backend::Dunst, false))
        );
    }

    #[test]
    fn gnome_only_on_gnome_desktops() {
        let only_gsettings = |b: &str| b == "gsettings";
        let banners_on = |_: &Cmd| Some("true".to_string());
        assert_eq!(detect("niri", only_gsettings, banners_on), None);
        assert_eq!(
            detect("ubuntu:GNOME", only_gsettings, banners_on),
            Some((Backend::Gnome, false))
        );
        // Missing schema: gsettings exits non-zero.
        assert_eq!(detect("GNOME", only_gsettings, |_| None), None);
    }

    #[test]
    fn never_touches_learner_dnd() {
        assert_eq!(plan_enable(Some((Backend::Dunst, true))), Plan::Nothing);
        assert_eq!(plan_enable(None), Plan::Nothing);
        assert_eq!(
            plan_enable(Some((Backend::Dunst, false))),
            Plan::TurnOn(Backend::Dunst)
        );
    }

    #[test]
    fn marker_round_trip() {
        for b in Backend::ALL {
            assert_eq!(Backend::from_name(&format!("{}\n", b.name())), Some(b));
        }
        assert_eq!(Backend::from_name("other"), None);
    }

    #[test]
    fn restore_without_marker_is_noop() {
        let dir = std::env::temp_dir().join(format!("lh-focus-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let focus = Focus::new(&dir);
        assert_eq!(focus.restore(), Ok(()));
        // An unknown backend name in the marker is cleared without commands.
        std::fs::write(dir.join("focus-dnd"), "other").unwrap();
        assert_eq!(focus.restore(), Ok(()));
        assert!(!dir.join("focus-dnd").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
