//! Sessions on mobile (docs/sync-build.md, "Sessions on mobile: desktop-only
//! plugins and attached folders").
//!
//! Sync carries every session to every device. On a phone or tablet a Learn
//! session stays writable unless the tutor has actually *used* a plugin whose
//! `platforms` lacks `mobile`: one of its tool calls or `artifact` ops is in
//! the log. Planning a section with it, or granting it, is not use. Where a
//! plugin runs is read from the op first (plugin calls and artifacts record
//! `platforms` when written), then from the cached registry indexes; a plugin
//! found in neither does not lock. A `mobileUnlock` op releases the plugins it
//! names. Desktop ignores all of it.
//!
//! `list_sessions` applies the same rule on mobile from a device-local cache,
//! `<config>/session-locks.json`: each Learn session file's registry-
//! independent [`LockFacts`], keyed by the file's length and mtime, judged
//! against the installed plugins and cached indexes at list time.
//!
//! An attached folder never locks: its path is device-local, so a device
//! without the mapping just has no folder, and the tutor is told so.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::plugins::install::IndexPlugin;
use crate::sessions::{op_id, MobileUnlockOp, Op};
use crate::state::SessionState;
use crate::tools;
use crate::{lock, now_iso, Error, Harness, Mode, Session, SessionSummary};

/// `SessionSummary::read_only_reason` of a session locked on mobile.
pub const DESKTOP_PLUGIN: &str = "desktop-plugin";

/// What this device knows about plugins it may not have installed.
#[derive(Default)]
pub(crate) struct Known {
    /// Entries of the cached registry indexes, first registry first.
    index: Vec<IndexPlugin>,
    /// Installed tool name → plugin id.
    installed_tools: HashMap<String, String>,
    /// Installed plugin id → title.
    installed_titles: HashMap<String, String>,
}

impl Known {
    fn indexed(&self, id: &str) -> Option<&IndexPlugin> {
        self.index.iter().find(|p| p.id == id)
    }

    /// Where the cached index says `id` runs; `None` when it doesn't say.
    fn index_platforms(&self, id: &str) -> Option<&[String]> {
        self.indexed(id)
            .map(|p| p.platforms.as_slice())
            .filter(|p| !p.is_empty())
    }

    /// The plugin an op's tool name belongs to when the op didn't record it:
    /// an installed plugin declaring it, else the index entry whose
    /// `<id>_` prefix (dashes as underscores) it starts with, longest first.
    fn owner_of(&self, tool: &str) -> Option<String> {
        if tools::is_core_tutor_tool(tool) {
            return None;
        }
        if let Some(id) = self.installed_tools.get(tool) {
            return Some(id.clone());
        }
        self.index
            .iter()
            .filter(|p| {
                let prefix = p.id.replace('-', "_");
                tool.len() > prefix.len()
                    && tool.starts_with(&prefix)
                    && tool.as_bytes()[prefix.len()] == b'_'
            })
            .max_by_key(|p| p.id.len())
            .map(|p| p.id.clone())
    }

    /// Display name: the title if this device knows one, else the id.
    pub(crate) fn name(&self, id: &str) -> String {
        self.indexed(id)
            .map(|p| p.title.as_str())
            .or_else(|| self.installed_titles.get(id).map(String::as_str))
            .filter(|t| !t.trim().is_empty())
            .unwrap_or(id)
            .to_owned()
    }
}

/// A known platform list without `mobile`. An empty list is unknown.
pub(crate) fn desktop_only(platforms: &[String]) -> bool {
    !platforms.is_empty() && !platforms.iter().any(|p| p == "mobile")
}

/// One plugin use in a log: a tutor tool call or an `artifact` op, with what
/// the op itself recorded. Registry-independent, so it can be cached per
/// session file and judged against [`Known`] later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PluginUse {
    /// The plugin id the op recorded (artifacts, newer tool calls).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plugin: Option<String>,
    /// An older tool call's name, resolved by [`Known::owner_of`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool: Option<String>,
    /// The plugin's platforms as the op recorded them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    platforms: Option<Vec<String>>,
}

/// What a session log says for the mobile lock, in log order: the plugin
/// uses and the plugin ids `mobileUnlock` ops released.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LockFacts {
    #[serde(default)]
    uses: Vec<PluginUse>,
    #[serde(default)]
    unlocked: Vec<String>,
}

impl LockFacts {
    pub(crate) fn from_ops(ops: &[Op]) -> Self {
        let mut facts = Self::default();
        for op in ops {
            let used = match op {
                // The call op, not its result: the call carries the stamp,
                // and a result never names anything the call didn't.
                Op::Tool(t) if t.agent.is_none() && t.result.is_none() => match &t.plugin {
                    Some(id) => PluginUse {
                        plugin: Some(id.clone()),
                        tool: None,
                        platforms: t.platforms.clone(),
                    },
                    None if tools::is_core_tutor_tool(&t.name) => continue,
                    None => PluginUse {
                        plugin: None,
                        tool: Some(t.name.clone()),
                        platforms: t.platforms.clone(),
                    },
                },
                Op::Artifact(a) => PluginUse {
                    plugin: Some(a.plugin_id.clone()),
                    tool: None,
                    platforms: a.platforms.clone(),
                },
                Op::MobileUnlock(u) => {
                    for id in &u.plugins {
                        if !facts.unlocked.contains(id) {
                            facts.unlocked.push(id.clone());
                        }
                    }
                    continue;
                }
                _ => continue,
            };
            if !facts.uses.contains(&used) {
                facts.uses.push(used);
            }
        }
        facts
    }

    /// The plugins used that are known not to run on mobile, as ids in
    /// first-use order. Each use is judged by the platforms it recorded,
    /// else the index.
    pub(crate) fn desktop_only_used(&self, known: &Known) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for used in &self.uses {
            let id = match (&used.plugin, &used.tool) {
                (Some(id), _) => id.clone(),
                (None, Some(tool)) => match known.owner_of(tool) {
                    Some(id) => id,
                    None => continue,
                },
                (None, None) => continue,
            };
            if out.contains(&id) {
                continue;
            }
            let platforms = used
                .platforms
                .as_deref()
                .filter(|p| !p.is_empty())
                .or_else(|| known.index_platforms(&id));
            if platforms.is_some_and(desktop_only) {
                out.push(id);
            }
        }
        out
    }

    /// The plugins that keep this session read-only on mobile: used,
    /// desktop-only, and not released by a `mobileUnlock` op.
    pub(crate) fn locking(&self, known: &Known) -> Vec<String> {
        self.desktop_only_used(known)
            .into_iter()
            .filter(|id| !self.unlocked.contains(id))
            .collect()
    }
}

/// [`LockFacts::desktop_only_used`] of a log.
pub(crate) fn desktop_only_used(ops: &[Op], known: &Known) -> Vec<String> {
    LockFacts::from_ops(ops).desktop_only_used(known)
}

/// [`LockFacts::locking`] of a log.
pub(crate) fn locking(ops: &[Op], known: &Known) -> Vec<String> {
    LockFacts::from_ops(ops).locking(known)
}

/// Names joined for a sentence: "A", "A and B", "A, B and C".
fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// `<config>/session-locks.json`: never synced, derived, rebuilt when lost.
const LOCK_CACHE_FILE: &str = "session-locks.json";

/// The list-time cache (mobile only): each Learn session file's
/// [`LockFacts`], valid while the file's length and mtime are unchanged.
/// Registry knowledge is applied at list time, so a registry refresh changes
/// the result without a rebuild.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct LockCache {
    #[serde(default)]
    sessions: HashMap<String, LockEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LockEntry {
    len: u64,
    mtime: u64,
    facts: LockFacts,
}

impl LockCache {
    fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Best effort, like the title index: a failed write costs a rebuild.
    fn save(&self, path: &Path) {
        let write = || -> Result<(), Error> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec(self)?)?;
            std::fs::rename(&tmp, path)?;
            Ok(())
        };
        if let Err(e) = write() {
            tracing::debug!("session lock cache not written: {e}");
        }
    }
}

/// Marks a summary locked for `locked` (`(id, display name)`), if any.
fn lock_summary(summary: &mut SessionSummary, locked: Vec<(String, String)>) {
    if locked.is_empty() {
        return;
    }
    summary.read_only = true;
    summary.read_only_reason = Some(DESKTOP_PLUGIN.to_owned());
    summary.desktop_only_plugins = locked.into_iter().map(|(_, name)| name).collect();
}

/// `(id, display name)` of each plugin in `ids`.
fn named(ids: Vec<String>, known: &Known) -> Vec<(String, String)> {
    ids.into_iter()
        .map(|id| {
            let name = known.name(&id);
            (id, name)
        })
        .collect()
}

impl Harness {
    /// Installed plugins and cached registry indexes, read for the mobile
    /// rules only (never on desktop).
    pub(crate) fn known_plugins(&self) -> Known {
        let mut known = Known::default();
        for info in lock(&self.plugins).info() {
            for tool in &info.tools {
                known.installed_tools.insert(tool.clone(), info.id.clone());
            }
            known.installed_titles.insert(info.id, info.title);
        }
        known.index = self
            .registry_catalog()
            .into_iter()
            .flat_map(|c| c.plugins)
            .collect();
        known
    }

    /// `(id, display name)` of each plugin locking this session on this
    /// device. Always empty on desktop.
    fn mobile_lock(&self, ops: &[Op]) -> Vec<(String, String)> {
        if !self.is_mobile() {
            return Vec::new();
        }
        let known = self.known_plugins();
        named(locking(ops, &known), &known)
    }

    /// Marks a loaded Learn session read-only on mobile when the rule says
    /// so (the legacy rule, already applied by the store, wins).
    pub(crate) fn apply_mobile_lock(&self, session: &mut Session) {
        let summary = &session.summary;
        if summary.read_only || summary.mode != Mode::Learn {
            return;
        }
        let locked = self.mobile_lock(&session.ops);
        lock_summary(&mut session.summary, locked);
    }

    /// The same rule for `list_sessions`, on mobile only (desktop reads and
    /// writes nothing). A Learn session that isn't legacy read-only is read
    /// whole only when its file's length or mtime changed since the cached
    /// facts; entries of deleted sessions are dropped.
    pub(crate) fn apply_mobile_locks(&self, list: &mut [SessionSummary]) {
        if !self.is_mobile() {
            return;
        }
        let path = self.config_dir.join(LOCK_CACHE_FILE);
        let mut guard = lock(&self.session_locks);
        let cache = guard.get_or_insert_with(|| LockCache::load(&path));
        let before = cache.sessions.len();
        let live: HashSet<&str> = list.iter().map(|s| s.id.as_str()).collect();
        cache.sessions.retain(|id, _| live.contains(id.as_str()));
        let mut dirty = cache.sessions.len() != before;
        let mut known: Option<Known> = None;
        for summary in list.iter_mut() {
            if summary.mode != Mode::Learn || summary.read_only {
                continue;
            }
            // Stamp before reading: a write in between only costs a re-read.
            let Ok((len, mtime)) = self.store.file_stamp(&summary.id) else {
                continue;
            };
            let fresh = cache
                .sessions
                .get(&summary.id)
                .is_some_and(|e| e.len == len && e.mtime == mtime);
            if !fresh {
                let Ok(ops) = self.store.read_uncached(&summary.id) else {
                    continue;
                };
                let facts = LockFacts::from_ops(&ops);
                cache
                    .sessions
                    .insert(summary.id.clone(), LockEntry { len, mtime, facts });
                dirty = true;
            }
            let Some(entry) = cache.sessions.get(&summary.id) else {
                continue;
            };
            if entry.facts.uses.is_empty() {
                continue;
            }
            let known = known.get_or_insert_with(|| self.known_plugins());
            let locked = named(entry.facts.locking(known), known);
            lock_summary(summary, locked);
        }
        if dirty {
            cache.save(&path);
        }
    }

    /// A deleted session leaves the list-time cache (mobile only).
    pub(crate) fn forget_mobile_lock(&self, session_id: &str) {
        if !self.is_mobile() {
            return;
        }
        let path = self.config_dir.join(LOCK_CACHE_FILE);
        let mut guard = lock(&self.session_locks);
        let cache = guard.get_or_insert_with(|| LockCache::load(&path));
        if cache.sessions.remove(session_id).is_some() {
            cache.save(&path);
        }
    }

    /// The write path's refusal for a session locked on this device.
    pub(crate) fn check_mobile_lock(&self, ops: &[Op]) -> Result<(), Error> {
        let names: Vec<String> = self
            .mobile_lock(ops)
            .into_iter()
            .map(|(_, name)| name)
            .collect();
        if names.is_empty() {
            return Ok(());
        }
        let verb = if names.len() == 1 { "runs" } else { "run" };
        Err(Error::Invalid(format!(
            "this session used {}, which only {verb} on desktop, so it is read-only on this device; continue it on desktop, or choose “Continue here anyway”",
            join_names(&names)
        )))
    }

    /// "Continue here anyway" on a session locked on mobile: appends a
    /// `mobileUnlock` op naming the plugins that locked it and returns the
    /// reloaded session (now writable). Refused unless the session is
    /// currently locked for a desktop-only plugin on this device.
    pub fn mobile_unlock(&self, session_id: &str) -> Result<Session, Error> {
        let ops = self.runtime_learn_ops(session_id)?;
        let plugins: Vec<String> = self
            .mobile_lock(&ops)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        if plugins.is_empty() {
            return Err(Error::Invalid(
                "this session isn't locked for a desktop-only plugin on this device".into(),
            ));
        }
        self.append(
            session_id,
            Op::MobileUnlock(MobileUnlockOp {
                op_id: op_id(),
                at: now_iso(),
                plugins,
            }),
        )?;
        self.load_session(session_id)
    }

    /// The plugin a tutor call goes to and where it runs, recorded on the
    /// call op: the installed manifest, else an earlier op of this session
    /// that recorded it, else the cached registry index. `None` for the
    /// tutor's own tools and for names nothing here knows.
    pub(crate) fn plugin_stamp(
        &self,
        ops: &[Op],
        name: &str,
    ) -> Option<(String, Option<Vec<String>>)> {
        if tools::is_core_tutor_tool(name) {
            return None;
        }
        if let Some(m) = lock(&self.plugins).tool_owner(name) {
            return Some((m.id.clone(), Some(m.platforms.clone())));
        }
        let recorded = ops.iter().rev().find_map(|o| match o {
            Op::Tool(t) if t.name == name && t.plugin.is_some() && t.platforms.is_some() => {
                Some((t.plugin.clone()?, t.platforms.clone()))
            }
            Op::Artifact(a) if a.tool == name && a.platforms.is_some() => {
                Some((a.plugin_id.clone(), a.platforms.clone()))
            }
            _ => None,
        });
        if recorded.is_some() {
            return recorded;
        }
        let known = self.known_plugins();
        let id = known.owner_of(name)?;
        let platforms = known.index_platforms(&id).map(<[String]>::to_vec);
        Some((id, platforms))
    }

    /// What the tutor must know about this device: on mobile, the desktop-
    /// only plugins this session used (their tools are left out of the list:
    /// the returned ids), and on any device an attached folder it has no
    /// mapping for. `has_workspace`: this device maps the session's folder.
    pub(crate) fn device_notes(
        &self,
        ops: &[Op],
        state: &SessionState,
        has_workspace: bool,
    ) -> (Vec<String>, Vec<String>) {
        let mut notes = Vec::new();
        let mut unavailable = Vec::new();
        if self.is_mobile() {
            let known = self.known_plugins();
            unavailable = desktop_only_used(ops, &known);
            if !unavailable.is_empty() {
                let names: Vec<String> = unavailable
                    .iter()
                    .map(|id| format!("{} (`{id}`)", known.name(id)))
                    .collect();
                notes.push(format!(
                    "Not available on this device (they only run on desktop): {}. Their tools are left out here; teach without them, and if the learner asks, say they work on desktop.",
                    join_names(&names)
                ));
            }
        }
        if let Some(folder) = state.workspace.as_deref().filter(|_| !has_workspace) {
            notes.push(format!(
                "The folder `{folder}` is attached on another device; its files aren't available here."
            ));
        }
        (notes, unavailable)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::*;
    use crate::sessions::{ArtifactOp, PluginGrantOp, ToolOp};

    fn index(id: &str, title: &str, platforms: &[&str]) -> IndexPlugin {
        serde_json::from_value(json!({
            "id": id, "title": title,
            "platforms": platforms,
        }))
        .unwrap()
    }

    fn known(index: Vec<IndexPlugin>) -> Known {
        Known {
            index,
            ..Known::default()
        }
    }

    fn call(name: &str, plugin: Option<&str>, platforms: Option<&[&str]>) -> Op {
        Op::Tool(ToolOp {
            op_id: op_id(),
            at: now_iso(),
            call_id: format!("c-{name}"),
            name: name.into(),
            args: json!({}),
            result: None,
            agent: None,
            plugin: plugin.map(str::to_owned),
            platforms: platforms.map(|p| p.iter().map(|s| (*s).to_owned()).collect()),
        })
    }

    fn artifact(plugin: &str, platforms: Option<&[&str]>) -> Op {
        Op::Artifact(ArtifactOp {
            op_id: op_id(),
            at: now_iso(),
            artifact_id: "a1".into(),
            plugin_id: plugin.into(),
            plugin_version: "0.1.0".into(),
            package_hash: "h".into(),
            tool: format!("{plugin}_draw"),
            input: Value::Null,
            data: Value::Null,
            fallback_svg: "<svg/>".into(),
            platforms: platforms.map(|p| p.iter().map(|s| (*s).to_owned()).collect()),
        })
    }

    fn grant(plugin: &str) -> Op {
        Op::PluginGrant(PluginGrantOp {
            op_id: op_id(),
            at: now_iso(),
            section_id: "s1".into(),
            plugin: plugin.into(),
            by: "request".into(),
        })
    }

    fn unlock(plugins: &[&str]) -> Op {
        Op::MobileUnlock(MobileUnlockOp {
            op_id: op_id(),
            at: now_iso(),
            plugins: plugins.iter().map(|s| (*s).to_owned()).collect(),
        })
    }

    #[test]
    fn granting_or_requesting_a_desktop_only_plugin_is_not_use() {
        let k = known(vec![index("bench", "Bench", &["desktop"])]);
        let ops = [
            grant("bench"),
            call("plugin_request", None, None),
            call("question_ask", None, None),
        ];
        assert!(locking(&ops, &k).is_empty());
    }

    #[test]
    fn a_call_or_an_artifact_locks_by_its_recorded_platforms_first() {
        // Recorded on the op: no index needed.
        let ops = [call("bench_run", Some("bench"), Some(&["desktop"]))];
        assert_eq!(locking(&ops, &Known::default()), ["bench"]);
        let ops = [artifact("scope", Some(&["desktop"]))];
        assert_eq!(locking(&ops, &Known::default()), ["scope"]);
        // The op's own platforms win over the index, and the call's result
        // (which records nothing) doesn't fall back to the index.
        let k = known(vec![index("bench", "Bench", &["desktop"])]);
        let mut result = call("bench_run", None, None);
        if let Op::Tool(t) = &mut result {
            t.args = Value::Null;
            t.result = Some(crate::sessions::ToolResultValue {
                content: "ok".into(),
                is_error: false,
            });
        }
        let ops = [
            call("bench_run", Some("bench"), Some(&["desktop", "mobile"])),
            result,
        ];
        assert!(locking(&ops, &k).is_empty());
        // What the list-time cache stores survives a JSON round trip.
        let facts = LockFacts::from_ops(&ops);
        let back: LockFacts =
            serde_json::from_value(serde_json::to_value(&facts).unwrap()).unwrap();
        assert_eq!(back, facts);
    }

    #[test]
    fn older_ops_fall_back_to_the_index_and_unknown_never_locks() {
        let k = known(vec![
            index("bench", "Bench", &["desktop"]),
            index("bench-pro", "Bench Pro", &["desktop", "mobile"]),
        ]);
        // No `plugin` or `platforms` on the op: the tool name's prefix
        // finds the index entry, the longest id first.
        assert_eq!(locking(&[call("bench_run", None, None)], &k), ["bench"]);
        assert!(locking(&[call("bench_pro_run", None, None)], &k).is_empty());
        assert!(locking(&[artifact("bench-pro", None)], &k).is_empty());
        assert_eq!(locking(&[artifact("bench", None)], &k), ["bench"]);
        // In neither place, or listed without platforms: no lock.
        assert!(locking(&[call("ghost_run", None, None)], &k).is_empty());
        assert!(locking(&[artifact("ghost", None)], &k).is_empty());
        let empty = known(vec![index("bench", "Bench", &[])]);
        assert!(locking(&[call("bench_run", None, None)], &empty).is_empty());
        // A planner tool call is never the tutor's use.
        let mut planner = call("bench_run", Some("bench"), Some(&["desktop"]));
        if let Op::Tool(t) = &mut planner {
            t.agent = Some("planner".into());
        }
        assert!(locking(&[planner], &k).is_empty());
    }

    #[test]
    fn an_unlock_releases_only_the_plugins_it_names() {
        let ops = vec![
            call("bench_run", Some("bench"), Some(&["desktop"])),
            unlock(&["bench"]),
            // A later attempt at the same plugin doesn't lock again.
            call("bench_run", Some("bench"), Some(&["desktop"])),
        ];
        assert!(locking(&ops, &Known::default()).is_empty());
        // A different desktop-only plugin used afterwards does.
        let mut more = ops;
        more.push(artifact("scope", Some(&["desktop"])));
        assert_eq!(locking(&more, &Known::default()), ["scope"]);
    }

    #[test]
    fn names_are_titles_when_known_else_ids() {
        let k = known(vec![index("bench", "Bench", &["desktop"])]);
        assert_eq!(k.name("bench"), "Bench");
        assert_eq!(k.name("ghost"), "ghost");
        assert_eq!(
            join_names(&["A".into(), "B".into(), "C".into()]),
            "A, B and C"
        );
    }
}
