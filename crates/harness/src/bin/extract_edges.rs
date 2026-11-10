//! Edge extractor: proposes prerequisite (`requires`) and confusion
//! (`contrasts`) edges between claims that already exist in the learner's
//! knowledge graph. It never adds, renames or rewords a claim.
//!
//! Usage:
//! `extract_edges <repo-dir> [--out graph/edges.proposed.jsonl] [--accept] [--limit N] [--dry-run] [--role planner]`
//!
//! Batches: one model call per topic (its claims, with titles and statements)
//! and then one cross-topic call over a compact list of every claim (id, title,
//! home) — an edge that crosses the tree is exactly what a per-topic view
//! cannot see. Input is capped by `MAX_CLAIMS_PER_CALL` (and the cross-topic
//! list by `MAX_COMPACT_CLAIMS_PER_CALL`), so a large topic is split into
//! consecutive calls instead of being truncated. Each statement is cut at
//! `MAX_STATEMENT_CHARS` characters for the same reason.
//!
//! Everything the model returns is untrusted and checked here, never there:
//! ids must exist, no self-edges, no duplicate of the same pair *and* relation
//! in the graph, `why` must be non-empty, `relation` must be known, and no
//! `requires` proposal may close a cycle (a reachability check per proposal;
//! the whole-graph DFS mirrors `tools.rs::find_cycle`). A `contrasts` proposal
//! claims no order — the two claims are easily confused, not sequenced — so it
//! is never cycle-checked and may even reverse an existing prerequisite.
//! Rejections are printed and never written; `graph/edges.proposed.jsonl` holds
//! only accepted proposals — it is the review surface. A cycle the graph
//! already had is reported as a note and never blames a proposal.
//!
//! `--accept` merges that review file into the graph by appending `edge` ops to
//! `graph/ops.jsonl` (validation and dedupe as above) and makes no model call,
//! so a human reviews first.
//!
//! Provider access is the app's own plumbing: the device's settings pick the
//! role's model (the Planner is the default; `--role` overrides), the saved or
//! environment credential authorises it, and `providers::stream_chat` makes the
//! call. With no credential the run fails with what to do about it, while
//! `--dry-run` still works.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use harness::knowledge::{self, KnowledgeEdge, KnowledgeMap, KnowledgeNode};
use harness::providers::{
    stream_chat, ChatRequest, Client, Credential, Endpoint, Message, ProviderError, ProviderId,
    Role as ChatRole, StreamEvent,
};
use harness::{Composer, ModelChoice, ModelRole, Settings};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Claims per per-topic call: 40 titles plus statements cut at 600 characters
/// is roughly 20k characters of prompt, small enough to be cheap and precise.
const MAX_CLAIMS_PER_CALL: usize = 40;
/// Compact rows per cross-topic call: `{id, title, home}` is ~50 characters,
/// so 120 rows stay inside one screenful of prompt.
const MAX_COMPACT_CLAIMS_PER_CALL: usize = 120;
/// Longest statement sent to the model (characters).
const MAX_STATEMENT_CHARS: usize = 600;
const CACHE_KEY: &str = "edge-extractor";
const PROMPT: &str = "jobs/edge-extractor.md";
const GRAPH: &str = "the graph";
const DEFAULT_OUT: &str = "graph/edges.proposed.jsonl";
const APP_ID: &str = "com.pslearningharness.app";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

type Res<T> = Result<T, Fail>;

/// A plain message: the CLI's whole error vocabulary.
#[derive(Debug)]
struct Fail(String);

impl fmt::Display for Fail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Fail {}

impl From<io::Error> for Fail {
    fn from(e: io::Error) -> Self {
        Fail(e.to_string())
    }
}

impl From<serde_json::Error> for Fail {
    fn from(e: serde_json::Error) -> Self {
        Fail(e.to_string())
    }
}

impl From<harness::Error> for Fail {
    fn from(e: harness::Error) -> Self {
        Fail(e.to_string())
    }
}

impl From<ProviderError> for Fail {
    fn from(e: ProviderError) -> Self {
        Fail(e.to_string())
    }
}

#[tokio::main]
async fn main() {
    match run(std::env::args().skip(1).collect()).await {
        Ok(()) => {}
        Err(e) => {
            eprintln!("extract_edges: {e}");
            std::process::exit(1);
        }
    }
}

async fn run(argv: Vec<String>) -> Res<()> {
    if argv.iter().any(|a| a == "-h" || a == "--help") {
        print!("{}", usage());
        return Ok(());
    }
    let args = Args::parse(argv)?;
    if !args.repo.is_dir() {
        return Err(Fail(format!("{} is not a directory", args.repo.display())));
    }
    let map = knowledge::load(&args.repo)?;
    if args.accept {
        return accept(&args, &map);
    }
    if args.dry_run {
        return dry_run(&args, &map);
    }
    extract(&args, &map).await
}

fn usage() -> &'static str {
    "extract_edges — propose prerequisite and contrast edges for the knowledge graph

usage: extract_edges <repo-dir> [options]

  <repo-dir>          the data repo; its graph/ directory is read
  --out <path>        review file, relative to <repo-dir> unless absolute
                      (default: graph/edges.proposed.jsonl)
  --accept            append the review file to graph/ops.jsonl (validated,
                      deduplicated) and stop; no model calls
  --limit <n>         stop after n model calls (batches)
  --dry-run           print the batches and the role/model; no calls, no writes
  --role <role>       tutor | planner | reader | librarian | builder
                      (default: planner)
  -h, --help          this text

One model call per topic (its claims, with titles and statements), then one
cross-topic call over every claim (id, title, home). Proposals the graph cannot
keep — an unknown id, a self-edge, a duplicate of the graph, a cycle, an
empty why, an unknown relation — are reported and never written.
"
}

struct Args {
    repo: PathBuf,
    out: PathBuf,
    accept: bool,
    limit: Option<usize>,
    dry_run: bool,
    role: ModelRole,
}

impl Args {
    fn parse(argv: Vec<String>) -> Res<Self> {
        let mut repo: Option<PathBuf> = None;
        let mut out: Option<PathBuf> = None;
        let mut limit: Option<usize> = None;
        let mut accept = false;
        let mut dry_run = false;
        let mut role = ModelRole::Planner;
        let mut rest = argv.into_iter();
        while let Some(arg) = rest.next() {
            match arg.as_str() {
                "--out" => out = Some(PathBuf::from(value(&mut rest, "--out")?)),
                "--limit" => {
                    let n = value(&mut rest, "--limit")?;
                    limit = Some(n.parse().map_err(|_| {
                        Fail(format!("--limit expects a number of calls, got `{n}`"))
                    })?);
                }
                "--accept" => accept = true,
                "--dry-run" => dry_run = true,
                "--role" => role = role_of(&value(&mut rest, "--role")?)?,
                other if other.starts_with('-') => {
                    return Err(Fail(format!("unknown option `{other}` (try --help)")));
                }
                other if repo.is_none() => repo = Some(PathBuf::from(other)),
                other => {
                    return Err(Fail(format!(
                        "unexpected argument `{other}`: one <repo-dir> only"
                    )));
                }
            }
        }
        let repo = repo.ok_or_else(|| Fail("missing <repo-dir> (try --help)".into()))?;
        if accept && dry_run {
            return Err(Fail(
                "--accept merges the review file and --dry-run only prints; pick one".into(),
            ));
        }
        if accept && limit.is_some() {
            return Err(Fail(
                "--limit caps model calls and --accept makes none; drop one".into(),
            ));
        }
        let out = match out {
            Some(p) if p.is_absolute() => p,
            Some(p) => repo.join(p),
            None => repo.join(DEFAULT_OUT),
        };
        Ok(Self {
            repo,
            out,
            accept,
            limit,
            dry_run,
            role,
        })
    }
}

fn value(rest: &mut impl Iterator<Item = String>, flag: &str) -> Res<String> {
    rest.next()
        .ok_or_else(|| Fail(format!("{flag} needs a value")))
}

fn role_of(name: &str) -> Res<ModelRole> {
    ModelRole::ALL
        .into_iter()
        .find(|r| r.as_str() == name)
        .ok_or_else(|| {
            Fail(format!(
                "unknown role `{name}`: pick tutor, planner, reader, librarian or builder"
            ))
        })
}

// ---- batches ---------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BatchKind {
    /// The claims homed in one topic.
    Topic,
    /// Every claim in the graph, compact.
    Cross,
}

#[derive(Debug)]
struct Batch {
    kind: BatchKind,
    label: String,
    claims: usize,
    payload: Value,
}

/// One call per home topic (claims with titles and statements), in topic-id
/// order, then the cross-topic calls. A topic with more claims than
/// `MAX_CLAIMS_PER_CALL` becomes consecutive parts; nothing is silently
/// dropped, and a claim whose home has no topic entry still goes out (under its
/// home id) rather than disappearing.
fn batches(map: &KnowledgeMap) -> Vec<Batch> {
    let mut by_home: BTreeMap<&str, Vec<&KnowledgeNode>> = BTreeMap::new();
    for node in &map.nodes {
        by_home.entry(node.home.as_str()).or_default().push(node);
    }
    let titles: BTreeMap<&str, &str> = map
        .topics
        .iter()
        .map(|t| (t.id.as_str(), t.title.as_str()))
        .collect();

    let mut out = Vec::new();
    for (home, nodes) in &by_home {
        let title = titles.get(home).copied().unwrap_or("(no topic entry)");
        let parts = nodes.len().div_ceil(MAX_CLAIMS_PER_CALL);
        for (i, chunk) in nodes.chunks(MAX_CLAIMS_PER_CALL).enumerate() {
            let label = if parts > 1 {
                format!("topic {home} (part {}/{parts})", i + 1)
            } else {
                format!("topic {home}")
            };
            let claims: Vec<Value> = chunk
                .iter()
                .map(|n| {
                    json!({
                        "id": n.id,
                        "title": n.title,
                        "statement": truncate(&n.statement, MAX_STATEMENT_CHARS),
                    })
                })
                .collect();
            out.push(Batch {
                kind: BatchKind::Topic,
                label,
                claims: claims.len(),
                payload: json!({
                    "batch": "one_topic",
                    "topic": { "id": home, "title": title },
                    "claims": claims,
                }),
            });
        }
    }

    let compact: Vec<Value> = map
        .nodes
        .iter()
        .map(|n| json!({ "id": n.id, "title": n.title, "home": n.home }))
        .collect();
    for (i, chunk) in compact.chunks(MAX_COMPACT_CLAIMS_PER_CALL).enumerate() {
        let parts = compact.len().div_ceil(MAX_COMPACT_CLAIMS_PER_CALL);
        let label = if parts > 1 {
            format!("cross-topic (part {}/{parts})", i + 1)
        } else {
            "cross-topic".to_owned()
        };
        out.push(Batch {
            kind: BatchKind::Cross,
            label,
            claims: chunk.len(),
            payload: json!({
                "batch": "cross_topic",
                "claims": chunk,
            }),
        });
    }
    out
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn plural(n: usize, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

// ---- proposals and validation ----------------------------------------------

/// One accepted proposal, as a line of `edges.proposed.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Proposal {
    from: String,
    to: String,
    relation: String,
    why: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    at: String,
}

#[derive(Debug, Default, Deserialize)]
struct RawReply {
    #[serde(default)]
    edges: Option<Vec<RawEdge>>,
}

#[derive(Debug, Default, Deserialize)]
struct RawEdge {
    #[serde(default)]
    from: String,
    #[serde(default)]
    to: String,
    #[serde(default)]
    relation: String,
    #[serde(default)]
    why: String,
}

#[derive(Debug, Default)]
struct Review {
    accepted: Vec<Proposal>,
    rejected: Vec<(Proposal, String)>,
    /// The same pair proposed twice, or already in the graph on an accept.
    duplicates: usize,
    /// A cycle the graph already had before this run: a data problem the
    /// tool reports but does not cause, so it never rejects a proposal.
    existing_cycle: Option<Vec<String>>,
}

/// The code-side gate. Every rule here is checked against the graph on disk,
/// never against what the model says about itself; a proposal that fails is
/// reported with the reason and left out of the review file.
fn validate(
    proposals: Vec<Proposal>,
    ids: &HashSet<String>,
    existing: &[KnowledgeEdge],
    report_duplicates: bool,
) -> Review {
    // A duplicate is the same pair *and* the same relation: the graph may hold
    // a prerequisite and a contrast over one pair at once.
    let in_file: HashSet<(String, String, String)> = existing
        .iter()
        .map(|e| (e.from.clone(), e.to.clone(), e.relation.clone()))
        .collect();
    // Dangling edges stay in the file but are not part of the graph `load` sees.
    // Only `requires` orders claims, so only it takes part in the cycle test.
    let mut graph: Vec<(String, String)> = existing
        .iter()
        .filter(|e| e.is_requires() && ids.contains(&e.from) && ids.contains(&e.to))
        .map(|e| (e.from.clone(), e.to.clone()))
        .collect();
    let mut accepted_pairs: HashSet<(String, String, String)> = HashSet::new();

    let mut accepted = Vec::new();
    let mut rejected = Vec::new();
    let mut duplicates = 0;

    for mut p in proposals {
        p.from = p.from.trim().to_owned();
        p.to = p.to.trim().to_owned();
        p.relation = p.relation.trim().to_ascii_lowercase();
        p.why = p.why.trim().to_owned();

        if p.from.is_empty() {
            rejected.push((p, "no `from` claim id".to_owned()));
            continue;
        }
        if p.to.is_empty() {
            rejected.push((p, "no `to` claim id".to_owned()));
            continue;
        }
        if p.relation != "requires" && p.relation != "contrasts" {
            rejected.push((
                p,
                "relation is not `requires` or `contrasts` (dropped)".to_owned(),
            ));
            continue;
        }
        if p.why.is_empty() {
            rejected.push((p, "`why` is empty".to_owned()));
            continue;
        }
        if !ids.contains(&p.from) {
            let reason = format!("unknown claim id `{}`", p.from);
            rejected.push((p, reason));
            continue;
        }
        if !ids.contains(&p.to) {
            let reason = format!("unknown claim id `{}`", p.to);
            rejected.push((p, reason));
            continue;
        }
        if p.from == p.to {
            let reason = format!("self-edge: `{}` cannot be both sides of an edge", p.from);
            rejected.push((p, reason));
            continue;
        }

        let pair = (p.from.clone(), p.to.clone(), p.relation.clone());
        if accepted_pairs.contains(&pair) {
            // The cross-topic pass repeating a per-topic edge is expected.
            duplicates += 1;
            continue;
        }
        if in_file.contains(&pair) {
            if report_duplicates {
                rejected.push((p, format!("already in {GRAPH}")));
            } else {
                duplicates += 1;
            }
            continue;
        }

        if p.relation == knowledge::REQUIRES {
            graph.push((p.from.clone(), p.to.clone()));
            // A candidate closes a cycle exactly when its `to` already reaches
            // its `from`; checking only that path keeps an unrelated cycle
            // already in the graph from rejecting everything (it is
            // reported instead).
            if let Some(back) = path_between(&graph, &p.to, &p.from) {
                graph.pop();
                let reason = format!("would close a cycle: {} → {}", p.from, back.join(" → "));
                rejected.push((p, reason));
                continue;
            }
        }
        // A `contrasts` edge is not pushed: it claims no order, so it can never
        // be part of a loop (and both directions may exist).
        accepted_pairs.insert(pair);
        accepted.push(p);
    }

    Review {
        accepted,
        rejected,
        duplicates,
        existing_cycle: find_cycle(ids, &graph),
    }
}

/// A path `from → … → to` through `edges`, if one exists (DFS with a visited
/// set). Used to decide whether one proposal would close a cycle.
fn path_between(edges: &[(String, String)], from: &str, to: &str) -> Option<Vec<String>> {
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for (a, b) in edges {
        adj.entry(a.as_str()).or_default().push(b.as_str());
    }
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(from.to_owned());
    let mut stack: Vec<Vec<String>> = vec![vec![from.to_owned()]];
    while let Some(current) = stack.pop() {
        let last = current.last().map(String::as_str).unwrap_or_default();
        for next in adj.get(last).map(Vec::as_slice).unwrap_or_default() {
            let mut path = current.clone();
            path.push((*next).to_owned());
            if *next == to {
                return Some(path);
            }
            if seen.insert((*next).to_owned()) {
                stack.push(path);
            }
        }
    }
    None
}

/// A cycle in the `from → to` prerequisite edges, if any (DFS with colours,
/// mirroring `crates/harness/src/tools.rs::find_cycle`). Returns the cycle's
/// path. Callers pass `requires` edges only: a `contrasts` edge orders nothing.
fn find_cycle(ids: &HashSet<String>, edges: &[(String, String)]) -> Option<Vec<String>> {
    fn visit<'a>(
        id: &'a str,
        adj: &HashMap<&'a str, Vec<&'a str>>,
        color: &mut HashMap<&'a str, u8>,
        stack: &mut Vec<&'a str>,
    ) -> Option<Vec<String>> {
        match color.get(id) {
            Some(1) => {
                let start = stack.iter().position(|s| *s == id).unwrap_or(0);
                let mut cycle: Vec<String> =
                    stack[start..].iter().map(|s| (*s).to_owned()).collect();
                cycle.push(id.to_owned());
                return Some(cycle);
            }
            Some(2) => return None,
            _ => {}
        }
        color.insert(id, 1);
        stack.push(id);
        for d in adj.get(id).map(Vec::as_slice).unwrap_or_default() {
            if let Some(c) = visit(d, adj, color, stack) {
                return Some(c);
            }
        }
        stack.pop();
        color.insert(id, 2);
        None
    }

    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for (from, to) in edges {
        if !ids.contains(from) || !ids.contains(to) {
            continue;
        }
        adj.entry(from.as_str()).or_default().push(to.as_str());
    }
    let mut roots: Vec<&str> = ids.iter().map(String::as_str).collect();
    roots.sort_unstable();
    let mut color: HashMap<&str, u8> = HashMap::new();
    let mut stack: Vec<&str> = Vec::new();
    roots
        .into_iter()
        .find_map(|id| visit(id, &adj, &mut color, &mut stack))
}

// ---- files -----------------------------------------------------------------

#[cfg(test)]
fn read_proposals(path: &Path) -> Res<Vec<Proposal>> {
    let text = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        out.push(
            serde_json::from_str(line)
                .map_err(|e| Fail(format!("{}:{}: {e}", path.display(), i + 1)))?,
        );
    }
    Ok(out)
}

fn write_proposals(path: &Path, proposals: &[Proposal]) -> Res<()> {
    let mut text = String::new();
    for p in proposals {
        text.push_str(&serde_json::to_string(p)?);
        text.push('\n');
    }
    write_atomic(path, &text)
}

/// Write-then-rename, like every other file this app owns.
fn write_atomic(path: &Path, text: &str) -> Res<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

// ---- model access ----------------------------------------------------------

/// `$XDG_CONFIG_HOME/com.pslearningharness.app`, else `~/.config/...` — where
/// the app keeps `settings.json` and `credentials.json`.
fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join(APP_ID);
        }
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".config").join(APP_ID)
}

/// The device's settings, through the same loader the app uses, so the legacy
/// single-model migration has already run.
fn settings(repo: &Path) -> Res<Settings> {
    Ok(harness::Harness::open(repo, &config_dir())?.settings())
}

fn saved_credentials(dir: &Path) -> Res<BTreeMap<ProviderId, Credential>> {
    let path = dir.join("credentials.json");
    match std::fs::read(&path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|e| Fail(format!("{}: {e}", path.display())))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(e.into()),
    }
}

fn model_for(role: ModelRole, settings: &Settings) -> Res<ModelChoice> {
    settings
        .roles
        .get(role)
        .cloned()
        .or_else(|| settings.roles.tutor.clone())
        .ok_or_else(|| {
            Fail(format!(
                "no model for the {} role: choose a {} model, or a Tutor model, in the app's Settings",
                role.label(),
                role.label()
            ))
        })
}

fn credential_state(provider: ProviderId, dir: &Path) -> String {
    let spec = provider.spec();
    let saved = saved_credentials(dir)
        .map(|creds| creds.contains_key(&provider))
        .unwrap_or(false);
    if saved {
        format!("{} credential saved", spec.name)
    } else if provider.credential_from_env().is_some() {
        format!("{} credential from ${}", spec.name, spec.env_var)
    } else {
        format!(
            "none for {} — connect it in the app's Settings or set ${}",
            spec.name, spec.env_var
        )
    }
}

/// A resolved role, model and credential: everything one call needs.
struct Access {
    provider: ProviderId,
    model: String,
    effort: Option<String>,
    credential: Credential,
    base: Option<String>,
    /// The model's own output limit from the app's model cache, if known.
    max_tokens: Option<u32>,
}

impl Access {
    /// Through the same harness the app uses, so the legacy single-model
    /// migration has run and the model cache is the app's.
    fn resolve(role: ModelRole, repo: &Path, dir: &Path) -> Res<Self> {
        let harness = harness::Harness::open(repo, dir)?;
        let settings = harness.settings();
        let choice = model_for(role, &settings)?;
        let credential = saved_credentials(dir)?
            .get(&choice.provider)
            .cloned()
            .or_else(|| choice.provider.credential_from_env())
            .ok_or_else(|| {
                Fail(format!(
                    "the {} model runs on {}, which is not connected: connect {} in the app's Settings, or set ${}",
                    role.label(),
                    choice.provider.spec().name,
                    choice.provider.spec().name,
                    choice.provider.spec().env_var
                ))
            })?;
        Ok(Self {
            base: settings.base_urls.get(&choice.provider).cloned(),
            max_tokens: harness.max_tokens(choice.provider, &choice.model),
            provider: choice.provider,
            model: choice.model,
            effort: choice.effort,
            credential,
        })
    }
}

async fn call(
    client: &Client,
    endpoint: &Endpoint,
    access: &Access,
    system: &str,
    payload: &str,
) -> Res<String> {
    let messages = [Message::text(ChatRole::User, payload)];
    let req = ChatRequest {
        model: &access.model,
        system,
        messages: &messages,
        max_tokens: access.max_tokens,
        effort: access.effort.as_deref(),
        tools: &[],
        cache_key: Some(CACHE_KEY),
    };
    let mut text = String::new();
    let mut on_event = |ev: StreamEvent<'_>| {
        if let StreamEvent::Text(delta) = ev {
            text.push_str(delta);
        }
    };
    stream_chat(client, endpoint, &req, &mut on_event)
        .await
        .map_err(|e| Fail(format!("{}: {e}", access.provider.spec().name)))?;
    Ok(text)
}

/// The batch reply: JSON only, tolerating a stray code fence or a sentence
/// before it. Anything else is a hard error — a silent empty result would look
/// like "the graph needs no edges".
fn parse_edges(text: &str) -> Res<Vec<RawEdge>> {
    let start = text
        .find('{')
        .ok_or_else(|| Fail(format!("the model returned no JSON: {}", preview(text))))?;
    let end = text.rfind('}').filter(|end| *end > start).ok_or_else(|| {
        Fail(format!(
            "the model's reply is not a JSON object: {}",
            preview(text)
        ))
    })?;
    let reply: RawReply = serde_json::from_str(&text[start..=end])
        .map_err(|e| Fail(format!("the model's JSON did not parse: {e}")))?;
    Ok(reply.edges.unwrap_or_default())
}

fn preview(text: &str) -> String {
    truncate(text.trim(), 200)
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn ids(map: &KnowledgeMap) -> HashSet<String> {
    map.nodes.iter().map(|n| n.id.clone()).collect()
}

// ---- commands --------------------------------------------------------------

fn dry_run(args: &Args, map: &KnowledgeMap) -> Res<()> {
    let all = batches(map);
    let shown: Vec<&Batch> = match args.limit {
        Some(n) => all.iter().take(n).collect(),
        None => all.iter().collect(),
    };
    let topics = shown.iter().filter(|b| b.kind == BatchKind::Topic).count();
    let cross = shown.len() - topics;
    let homed: HashSet<&str> = map.nodes.iter().map(|n| n.home.as_str()).collect();
    let existing: Vec<KnowledgeEdge> = map.edges.clone();
    let system = Composer::new(&args.repo).load(PROMPT)?;

    println!("extract_edges: dry run — no model calls, nothing written");
    println!("repo: {}", args.repo.display());
    println!(
        "claims: {} in {} home topics ({} topics in the tree)",
        map.nodes.len(),
        homed.len(),
        map.topics.len()
    );
    println!("existing edges: {}", existing.len());
    let known = ids(map);
    let pairs: Vec<(String, String)> = existing
        .iter()
        .filter(|e| e.is_requires() && known.contains(&e.from) && known.contains(&e.to))
        .map(|e| (e.from.clone(), e.to.clone()))
        .collect();
    if let Some(cycle) = find_cycle(&known, &pairs) {
        println!(
            "note: {GRAPH} already has a cycle: {} (not caused by this tool)",
            cycle.join(" → ")
        );
    }
    match args.limit {
        Some(n) => println!(
            "batches: {} of {} (--limit {n}) — {topics} topic, {cross} cross-topic",
            shown.len(),
            all.len()
        ),
        None => println!(
            "batches: {} — {topics} topic, {cross} cross-topic",
            shown.len()
        ),
    }
    for batch in &shown {
        println!("  {}: {}", batch.label, plural(batch.claims, "claim"));
    }
    println!("role: {} ({})", args.role.label(), args.role.as_str());
    let dir = config_dir();
    match settings(&args.repo) {
        Ok(s) => match model_for(args.role, &s) {
            Ok(choice) => {
                println!("model: {} ({})", choice.model, choice.provider.spec().name);
                println!("credential: {}", credential_state(choice.provider, &dir));
            }
            Err(e) => println!("model: unset — {e}"),
        },
        Err(e) => println!("model: unknown — {e}"),
    }
    println!("settings: {}", dir.display());
    println!("system prompt: {PROMPT} ({} chars)", system.chars().count());
    Ok(())
}

fn accept(args: &Args, _map: &KnowledgeMap) -> Res<()> {
    if !args.out.is_file() && args.out == harness::proposals::path(&args.repo) {
        return Err(Fail(format!(
            "nothing to accept: {} does not exist (run the extractor first)",
            args.out.display()
        )));
    }
    // One implementation for both callers: the CLI and the Map page's review
    // dialog go through `proposals::decide`, so the rules cannot drift.
    let keys: Vec<String> = harness::proposals::proposals(&args.repo)
        .iter()
        .map(harness::proposals::ProposedEdge::key)
        .collect();
    let out = harness::proposals::decide(&args.repo, &keys, &[])?;
    for r in &out.rejected {
        println!(
            "skipped {}: {}",
            harness::proposals::describe(&r.key),
            r.reason
        );
    }
    if out.accepted.is_empty() {
        println!("extract_edges: nothing new; graph/ops.jsonl unchanged");
    } else {
        println!(
            "extract_edges: appended {} to graph/ops.jsonl",
            harness::proposals::phrase(&out.accepted_by_relation),
        );
    }
    Ok(())
}

async fn extract(args: &Args, map: &KnowledgeMap) -> Res<()> {
    let system = Composer::new(&args.repo).load(PROMPT)?;
    let dir = config_dir();
    let access = Access::resolve(args.role, &args.repo, &dir)?;
    let client = Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(|e| Fail(format!("http client: {e}")))?;
    let endpoint = access
        .provider
        .endpoint(&access.credential, access.base.as_deref(), CACHE_KEY)
        .map_err(|e| Fail(format!("{}: {e}", access.provider.spec().name)))?;

    let all = batches(map);
    let run: Vec<&Batch> = match args.limit {
        Some(n) => all.iter().take(n).collect(),
        None => all.iter().collect(),
    };
    println!(
        "extract_edges: {}, {} batches, model {}/{}",
        plural(map.nodes.len(), "claim"),
        run.len(),
        access.provider.spec().name,
        access.model
    );

    let mut raw = Vec::new();
    for (i, batch) in run.iter().enumerate() {
        println!(
            "[{}/{}] {} — {}",
            i + 1,
            run.len(),
            batch.label,
            plural(batch.claims, "claim")
        );
        let text = call(
            &client,
            &endpoint,
            &access,
            &system,
            &batch.payload.to_string(),
        )
        .await?;
        let edges = parse_edges(&text)?;
        println!("  {} proposals", edges.len());
        for e in edges {
            raw.push(Proposal {
                from: e.from,
                to: e.to,
                relation: e.relation,
                why: e.why,
                model: access.model.clone(),
                at: now(),
            });
        }
    }

    let existing: Vec<KnowledgeEdge> = map.edges.clone();
    let review = validate(raw, &ids(map), &existing, true);
    if let Some(cycle) = &review.existing_cycle {
        println!(
            "  note: {GRAPH} already had a cycle before this run: {}",
            cycle.join(" → ")
        );
    }
    for (p, why) in &review.rejected {
        println!("  rejected {} → {}: {why}", p.from, p.to);
    }
    if review.duplicates > 0 {
        println!("  collapsed {} duplicate proposals", review.duplicates);
    }
    write_proposals(&args.out, &review.accepted)?;
    println!(
        "extract_edges: wrote {} proposals ({} rejected) to {}",
        review.accepted.len(),
        review.rejected.len(),
        args.out.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness::knowledge::KnowledgeTopic;
    use harness::state::Kind;
    use std::fs;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lh-edges-{name}-{}", ulid::Ulid::new()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn ids_of(list: &[&str]) -> HashSet<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    fn edge(from: &str, to: &str) -> KnowledgeEdge {
        KnowledgeEdge {
            from: from.to_owned(),
            to: to.to_owned(),
            relation: knowledge::REQUIRES.to_owned(),
            weight: "load-bearing".to_owned(),
            why: None,
            provenance: None,
        }
    }

    fn contrast(from: &str, to: &str) -> KnowledgeEdge {
        KnowledgeEdge {
            from: from.to_owned(),
            to: to.to_owned(),
            relation: knowledge::CONTRASTS.to_owned(),
            weight: "load-bearing".to_owned(),
            why: None,
            provenance: None,
        }
    }

    fn proposal(from: &str, to: &str) -> Proposal {
        Proposal {
            from: from.to_owned(),
            to: to.to_owned(),
            relation: "requires".to_owned(),
            why: "`to` uses the object `from` defines".to_owned(),
            model: "test-model".to_owned(),
            at: "2026-11-07T00:00:00.000Z".to_owned(),
        }
    }

    fn node(id: &str, home: &str) -> KnowledgeNode {
        KnowledgeNode {
            id: id.to_owned(),
            title: format!("title of {id}"),
            statement: format!("statement of {id}"),
            kind: Kind::Fact,
            home: home.to_owned(),
            correct: 0,
            missed: 0,
            provenance: None,
            attribution: None,
            scope: None,
            aliases: Vec::new(),
        }
    }

    fn topic(id: &str, title: &str) -> KnowledgeTopic {
        KnowledgeTopic {
            id: id.to_owned(),
            title: title.to_owned(),
            parent: None,
        }
    }

    #[test]
    fn a_cycle_is_rejected() {
        let ids = ids_of(&["a", "b", "c"]);
        // a → b → c already in the graph; c → a would close it.
        let existing = [edge("a", "b"), edge("b", "c")];
        let review = validate(vec![proposal("c", "a")], &ids, &existing, true);
        assert!(review.accepted.is_empty());
        assert_eq!(review.rejected.len(), 1);
        let why = &review.rejected[0].1;
        assert!(why.contains("would close a cycle"), "{why}");
        assert!(why.contains("c → a"), "{why}");
    }

    #[test]
    fn a_preexisting_cycle_is_reported_but_rejects_nothing() {
        // Real graphs can carry a cycle already; that must not block work.
        let ids = ids_of(&["a", "b", "x", "y"]);
        let existing = [edge("x", "y"), edge("y", "x")];
        let review = validate(vec![proposal("a", "b")], &ids, &existing, true);
        assert_eq!(review.accepted.len(), 1);
        assert!(review.rejected.is_empty());
        let cycle = review.existing_cycle.expect("the cycle is reported");
        assert_eq!(cycle.len(), 3, "{cycle:?}");
        assert_eq!(cycle.first(), cycle.last());
    }

    #[test]
    fn a_cycle_between_two_proposals_is_caught() {
        let ids = ids_of(&["a", "b"]);
        let review = validate(
            vec![proposal("a", "b"), proposal("b", "a")],
            &ids,
            &[],
            true,
        );
        assert_eq!(review.accepted.len(), 1);
        assert_eq!(review.rejected.len(), 1);
        assert!(review.rejected[0].1.contains("would close a cycle"));
    }

    #[test]
    fn an_unknown_id_is_rejected() {
        let ids = ids_of(&["a"]);
        let review = validate(vec![proposal("a", "ghost")], &ids, &[], true);
        assert!(review.accepted.is_empty());
        assert_eq!(review.rejected[0].1, "unknown claim id `ghost`");
    }

    #[test]
    fn a_duplicate_of_an_existing_edge_is_rejected() {
        let ids = ids_of(&["a", "b"]);
        let existing = [edge("a", "b")];
        let review = validate(vec![proposal("a", "b")], &ids, &existing, true);
        assert!(review.accepted.is_empty());
        assert_eq!(review.rejected[0].1, "already in the graph");
        // A re-proposed dangling edge is a duplicate too: the file says so.
        let dangling = [edge("a", "ghost")];
        let review = validate(vec![proposal("a", "ghost")], &ids, &dangling, true);
        assert!(review.accepted.is_empty());
    }

    #[test]
    fn a_self_edge_is_rejected() {
        let ids = ids_of(&["a"]);
        let review = validate(vec![proposal("a", "a")], &ids, &[], true);
        assert!(review.accepted.is_empty());
        assert!(review.rejected[0].1.contains("self-edge"));
    }

    #[test]
    fn an_empty_why_is_rejected() {
        let ids = ids_of(&["a", "b"]);
        let mut p = proposal("a", "b");
        p.why = "   ".to_owned();
        let review = validate(vec![p], &ids, &[], true);
        assert!(review.accepted.is_empty());
        assert_eq!(review.rejected[0].1, "`why` is empty");
    }

    #[test]
    fn an_unknown_relation_is_dropped() {
        let ids = ids_of(&["a", "b"]);
        let mut p = proposal("a", "b");
        p.relation = "supports".to_owned();
        let review = validate(vec![p], &ids, &[], true);
        assert!(review.accepted.is_empty());
        assert!(review.rejected[0].1.contains("`requires` or `contrasts`"));
    }

    #[test]
    fn a_contrast_is_not_a_duplicate_of_the_prerequisite_over_the_same_pair() {
        let ids = ids_of(&["a", "b"]);
        let existing = [edge("a", "b")];
        let mut p = proposal("a", "b");
        p.relation = "contrasts".to_owned();
        let review = validate(vec![p.clone()], &ids, &existing, true);
        assert_eq!(review.accepted.len(), 1, "{:?}", review.rejected);
        assert_eq!(review.accepted[0].relation, "contrasts");
        // The same relation on the same pair is a duplicate of that relation.
        let review = validate(vec![p], &ids, &[edge("a", "b"), contrast("a", "b")], true);
        assert!(review.accepted.is_empty());
        assert_eq!(review.rejected[0].1, "already in the graph");
    }

    #[test]
    fn a_contrast_is_never_cycle_checked() {
        let ids = ids_of(&["a", "b", "c"]);
        // a → b → c are prerequisites; the reverse claim as a contrast is fine.
        let existing = [edge("a", "b"), edge("b", "c")];
        let mut p = proposal("c", "a");
        p.relation = "contrasts".to_owned();
        let review = validate(vec![p], &ids, &existing, true);
        assert_eq!(review.accepted.len(), 1, "{:?}", review.rejected);
        assert!(review.rejected.is_empty());
        assert!(review.existing_cycle.is_none(), "a contrast is not a loop");
        // Both directions of a contrast may exist.
        let existing = [contrast("a", "b")];
        let mut p = proposal("b", "a");
        p.relation = "contrasts".to_owned();
        let review = validate(vec![p], &ids, &existing, true);
        assert_eq!(review.accepted.len(), 1, "{:?}", review.rejected);
    }

    #[test]
    fn a_requires_and_a_contrast_over_one_pair_are_both_kept() {
        let ids = ids_of(&["a", "b"]);
        let mut c = proposal("a", "b");
        c.relation = "contrasts".to_owned();
        let review = validate(vec![proposal("a", "b"), c], &ids, &[], true);
        assert_eq!(review.accepted.len(), 2, "{:?}", review.rejected);
        assert_eq!(review.duplicates, 0);
    }

    #[test]
    fn the_same_pair_proposed_twice_is_collapsed() {
        let ids = ids_of(&["a", "b"]);
        let review = validate(
            vec![proposal("a", "b"), proposal("a", "b")],
            &ids,
            &[],
            true,
        );
        assert_eq!(review.accepted.len(), 1);
        assert_eq!(review.duplicates, 1);
        assert!(review.rejected.is_empty());
    }

    #[test]
    fn a_valid_proposal_round_trips_into_the_review_file() {
        let ids = ids_of(&["a", "b"]);
        let review = validate(vec![proposal("a", "b")], &ids, &[], true);
        assert_eq!(review.accepted.len(), 1);

        let path = temp("roundtrip").join(DEFAULT_OUT);
        write_proposals(&path, &review.accepted).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert_eq!(read_proposals(&path).unwrap(), review.accepted);
        let value: Value = serde_json::from_str(text.trim()).unwrap();
        for key in ["from", "to", "relation", "why", "model", "at"] {
            assert!(
                value.get(key).is_some_and(Value::is_string),
                "missing {key}"
            );
        }
        assert_eq!(value["relation"], "requires");
    }

    #[test]
    fn batches_cover_each_topic_then_every_claim_once() {
        let map = KnowledgeMap {
            topics: vec![topic("t1", "One"), topic("t2", "Two")],
            nodes: vec![
                node("a", "t1"),
                node("b", "t1"),
                node("c", "t2"),
                node("d", "orphan"),
            ],
            ..Default::default()
        };
        let out = batches(&map);
        assert_eq!(out.len(), 4, "three home topics plus the cross-topic pass");
        assert_eq!(out[0].label, "topic orphan");
        assert_eq!(out[0].claims, 1);
        assert_eq!(out[1].label, "topic t1");
        assert_eq!(out[1].claims, 2);
        assert_eq!(out[2].claims, 1);
        assert_eq!(out[3].kind, BatchKind::Cross);
        assert_eq!(out[3].claims, 4);
        // The cross-topic pass is compact: ids, titles and homes, no statements.
        let compact = out[3].payload.to_string();
        assert!(!compact.contains("statement"), "{compact}");
        // Statements do reach the per-topic pass, with the topic title.
        let full = out[1].payload.to_string();
        assert!(full.contains("statement of a"));
        assert!(full.contains("\"title\":\"One\""));
    }

    #[test]
    fn a_large_topic_is_split_into_consecutive_batches() {
        let nodes: Vec<KnowledgeNode> = (0..MAX_CLAIMS_PER_CALL + 2)
            .map(|i| node(&format!("n{i}"), "big"))
            .collect();
        let map = KnowledgeMap {
            nodes,
            ..Default::default()
        };
        let out = batches(&map);
        let parts: Vec<&Batch> = out.iter().filter(|b| b.kind == BatchKind::Topic).collect();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].claims, MAX_CLAIMS_PER_CALL);
        assert_eq!(parts[1].claims, 2);
        assert!(parts[0].label.ends_with("(part 1/2)"), "{}", parts[0].label);
        let seen: usize = parts.iter().map(|b| b.claims).sum();
        assert_eq!(seen, MAX_CLAIMS_PER_CALL + 2);
    }

    #[test]
    fn a_reply_surrounded_by_text_still_parses() {
        let text = "Here you go:\n```json\n{\"edges\":[{\"from\":\"a\",\"to\":\"b\",\"relation\":\"contrasts\",\"why\":\"both call it limit\"}]}\n```\n";
        let edges = parse_edges(text).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].relation, "contrasts");
        assert_eq!(edges[0].from, "a");
    }

    #[test]
    fn a_reply_that_is_not_json_is_an_error_not_an_empty_result() {
        assert!(parse_edges("no edges here").is_err());
        assert!(parse_edges("{\"edges\": []}").unwrap().is_empty());
    }
}
