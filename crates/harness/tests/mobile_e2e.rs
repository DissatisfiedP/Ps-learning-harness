//! Sessions on mobile (docs/sync-build.md, "Sessions on mobile: desktop-only
//! plugins and attached folders"), end to end through the scripted provider.
//! A "phone" is a second `Harness` on the same data repo with its own config
//! dir (no plugins installed, no folder mappings, no index cache unless the
//! test adds one) and the platform set to `mobile`. A fresh phone is opened
//! after every desktop write, as a synced device would read the files anew.

mod common;

use std::path::Path;
use std::sync::Arc;

use common::*;
use harness::plugins::{Plugin, PluginContext, ToolFuture, ToolOutcome};
use harness::providers::ToolSpec;
use harness::{Harness, Op, PluginManifest};
use serde_json::{json, Value};
use tokio::sync::Semaphore;

/// `bench`: a plugin with one tutor tool, `bench_run`.
struct Bench {
    manifest: PluginManifest,
}

fn bench(platforms: &[&str]) -> Arc<dyn Plugin> {
    Arc::new(Bench {
        manifest: PluginManifest {
            id: "bench".into(),
            version: "0.1.0".into(),
            api: 1,
            title: "Bench".into(),
            description: "Lab bench instruments.".into(),
            license: "MIT".into(),
            tools: vec![harness::plugins::manifest::ToolDecl {
                name: "bench_run".into(),
                description: "Run the bench.".into(),
                agents: vec!["tutor".into()],
                input: "schemas/run.json".into(),
                entry: "run".into(),
                artifact: false,
            }],
            platforms: platforms.iter().map(|p| (*p).to_owned()).collect(),
            permissions: vec![],
            use_when: "Measuring.".into(),
            avoid_when: "Never.".into(),
            cost: "none".into(),
            renderer: vec![],
            view: vec![],
            prompts: Default::default(),
        },
    })
}

impl Plugin for Bench {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn tools(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "bench_run".into(),
            description: "Run the bench.".into(),
            input_schema: json!({ "type": "object", "properties": {} }),
        }]
    }

    fn call(self: Arc<Self>, _name: &str, _args: &Value, _ctx: PluginContext) -> ToolFuture {
        Box::pin(async {
            ToolOutcome {
                content: "measured 6 V".into(),
                is_error: false,
                artifact: None,
            }
        })
    }
}

/// Another device on `repo`: config dir `<root>/<name>`, platform mobile.
async fn phone(repo: &Path, name: &str, base: &str) -> Arc<Harness> {
    let config = repo.parent().unwrap().join(name);
    let h = harness(repo, &config, base).await;
    h.set_platform("mobile").unwrap();
    h
}

/// A cached registry index on `h` that lists `bench` as desktop-only.
async fn with_index(h: &Harness, repo: &Path) {
    let dir = repo.parent().unwrap().join("registry");
    std::fs::create_dir_all(&dir).unwrap();
    let index = json!({ "registry": 1, "name": "Test", "plugins": [{
        "id": "bench", "title": "Bench", "description": "Lab bench instruments.",
        "platforms": ["desktop"], "permissions": [], "versions": [] }] });
    let path = dir.join("index.json");
    std::fs::write(&path, index.to_string()).unwrap();
    // Offline: the built-in default registry would be fetched.
    h.remove_registry(harness::plugins::install::DEFAULT_REGISTRY_URL)
        .unwrap();
    h.add_registry("Test", &format!("file://{}", path.display()))
        .unwrap();
    let fetched = h.refresh_registries().await;
    assert!(fetched[0].ok, "{fetched:?}");
}

/// Desktop: a session in Teach whose section has `bench` from the plan,
/// granted but not called yet.
async fn teaching_with_bench(h: &Arc<Harness>, s: &Script, release: &Semaphore) -> String {
    let id = session_awaiting_approval(
        h,
        s,
        release,
        vec![Reply::Calls(
            "",
            vec![("w1", "plan_write", plan_args(&["bench"]))],
        )],
    )
    .await;
    s.push("tutor", Reply::Text("Section one."));
    h.approve(&id, vec![]).await.unwrap();
    assert_eq!(h.session_state(&id).unwrap().active_plugins, ["bench"]);
    id
}

fn last_tutor_request(s: &Script) -> Value {
    s.requests_of("tutor").pop().unwrap()
}

fn system(body: &Value) -> String {
    body["messages"][0]["content"].as_str().unwrap().to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_desktop_only_plugin_locks_the_phone_once_used_and_unlock_sticks() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("mobile-lock");
    let s = &*script;
    let desk = harness(&repo, &config, &base).await;
    desk.register_native_plugin(bench(&["desktop"])).unwrap();
    let id = teaching_with_bench(&desk, s, &release).await;

    // Planned and granted only: writable on a phone, even one whose index
    // knows `bench` is desktop-only.
    let p = phone(&repo, "phone-planned", &base).await;
    with_index(&p, &repo).await;
    let session = p.load_session(&id).unwrap();
    assert!(!session.summary.read_only);
    assert_eq!(session.summary.read_only_reason, None);
    assert!(session.summary.desktop_only_plugins.is_empty());

    // The tutor uses it on desktop: the call records the plugin and where
    // it runs.
    s.push(
        "tutor",
        Reply::Calls("", vec![("b1", "bench_run", json!({}))]),
    );
    s.push("tutor", Reply::Text("Six volts."));
    desk.send_learner_message(&id, "measure it").await.unwrap();
    let call = &tool_ops(&desk, &id, "bench_run")[0];
    assert_eq!(call["plugin"], "bench");
    assert_eq!(call["platforms"], json!(["desktop"]));
    assert!(!system(&last_tutor_request(s)).contains("Bench"));
    // Desktop is never affected.
    assert!(!desk.load_session(&id).unwrap().summary.read_only);

    // A phone that knows nothing else reads the platforms off the op and
    // names the plugin by id; the write path refuses the same way.
    let p = phone(&repo, "phone-bare", &base).await;
    let session = p.load_session(&id).unwrap();
    assert!(session.summary.read_only);
    assert_eq!(
        session.summary.read_only_reason.as_deref(),
        Some("desktop-plugin")
    );
    assert_eq!(session.summary.desktop_only_plugins, ["bench"]);
    let before = raw_op_logs(&repo);
    let err = p
        .send_learner_message(&id, "again")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("read-only"), "{err}");
    assert_eq!(raw_op_logs(&repo), before, "nothing appended");
    assert!(
        p.answer(&id, "x", answer("x")).await.is_err(),
        "every write is refused"
    );

    // With the index cached, the note names it by title.
    let p = phone(&repo, "phone-index", &base).await;
    with_index(&p, &repo).await;
    assert_eq!(
        p.load_session(&id).unwrap().summary.desktop_only_plugins,
        ["Bench"]
    );

    // Continue here anyway: writable, recorded in the log.
    let session = p.mobile_unlock(&id).unwrap();
    assert!(!session.summary.read_only);
    assert_eq!(session.summary.read_only_reason, None);
    assert!(session
        .ops
        .iter()
        .any(|o| matches!(o, Op::MobileUnlock(u) if u.plugins == ["bench"])));
    assert!(
        p.mobile_unlock(&id).is_err(),
        "only a locked session unlocks"
    );

    // The next turn on the phone: no bench tools, one line naming it, and a
    // call anyway is refused without locking again.
    s.push(
        "tutor",
        Reply::Calls("", vec![("b2", "bench_run", json!({}))]),
    );
    s.push("tutor", Reply::Text("By hand, then."));
    p.send_learner_message(&id, "measure again").await.unwrap();
    let request = s.requests_of("tutor").into_iter().rev().nth(1).unwrap();
    assert!(!tool_names(&request).contains(&"bench_run".to_owned()));
    assert!(system(&request).contains("Bench"), "{}", system(&request));
    let refused = results(&p, &id, "bench_run");
    let last = refused.last().unwrap();
    assert!(
        last.1 && last.0.contains("only runs on desktop"),
        "{refused:?}"
    );
    let session = p.load_session(&id).unwrap();
    assert!(
        !session.summary.read_only,
        "a later attempt doesn't re-lock"
    );
    assert!(session.messages.last().unwrap().content.contains("By hand"));

    // Desktop ignores the unlock: the op parses, and bench is offered there.
    let desk = harness(&repo, &repo.parent().unwrap().join("desk-2"), &base).await;
    desk.register_native_plugin(bench(&["desktop"])).unwrap();
    let session = desk.load_session(&id).unwrap();
    assert!(!session.summary.read_only);
    assert_eq!(session.skipped_ops, 0);
    s.push("tutor", Reply::Text("Back on the bench."));
    desk.send_learner_message(&id, "home again").await.unwrap();
    let request = last_tutor_request(s);
    assert!(tool_names(&request).contains(&"bench_run".to_owned()));
    assert!(!system(&request).contains("Bench"));
    assert!(s.unexpected.lock().unwrap().is_empty());
}

/// Session ids in a phone's `<config>/session-locks.json`.
fn cached_locks(repo: &Path, name: &str) -> Vec<String> {
    let path = repo.parent().unwrap().join(name).join("session-locks.json");
    let cache: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    cache["sessions"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_phone_lists_a_locked_session_as_locked_from_a_device_local_cache() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("mobile-list");
    let s = &*script;
    let desk = harness(&repo, &config, &base).await;
    desk.register_native_plugin(bench(&["desktop"])).unwrap();
    let id = teaching_with_bench(&desk, s, &release).await;
    s.push(
        "tutor",
        Reply::Calls("", vec![("b1", "bench_run", json!({}))]),
    );
    s.push("tutor", Reply::Text("Six volts."));
    desk.send_learner_message(&id, "measure it").await.unwrap();
    let listed = |h: &Harness| {
        h.list_sessions()
            .unwrap()
            .into_iter()
            .find(|x| x.id == id)
            .unwrap()
    };

    // Desktop lists it writable and keeps no lock cache.
    let row = listed(&desk);
    assert!(!row.read_only && row.read_only_reason.is_none());
    assert!(!config.join("session-locks.json").exists());

    // The phone lists it locked before opening it: from the op's recorded
    // platforms, named by id while it has no index.
    let p = phone(&repo, "phone", &base).await;
    let row = listed(&p);
    assert!(row.read_only);
    assert_eq!(row.read_only_reason.as_deref(), Some("desktop-plugin"));
    assert_eq!(row.desktop_only_plugins, ["bench"]);
    assert_eq!(cached_locks(&repo, "phone"), std::slice::from_ref(&id));
    // A registry refresh changes the names without touching the session
    // file: the cached facts are judged against the index at list time.
    with_index(&p, &repo).await;
    assert_eq!(listed(&p).desktop_only_plugins, ["Bench"]);

    // The unlock appends to the file, so the entry is rebuilt: writable.
    p.mobile_unlock(&id).unwrap();
    let row = listed(&p);
    assert!(!row.read_only && row.read_only_reason.is_none());
    assert!(row.desktop_only_plugins.is_empty());

    // A deleted session leaves the cache.
    p.delete_session(&id).unwrap();
    assert!(cached_locks(&repo, "phone").is_empty());
    assert!(p.list_sessions().unwrap().iter().all(|x| x.id != id));
    assert!(s.unexpected.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_made_on_the_phone_is_refused_and_locks_after_the_turn() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("mobile-call");
    let s = &*script;
    let desk = harness(&repo, &config, &base).await;
    desk.register_native_plugin(bench(&["desktop"])).unwrap();
    let id = teaching_with_bench(&desk, s, &release).await;

    let p = phone(&repo, "phone", &base).await;
    with_index(&p, &repo).await;
    assert!(!p.load_session(&id).unwrap().summary.read_only);
    s.push(
        "tutor",
        Reply::Calls("", vec![("b1", "bench_run", json!({}))]),
    );
    s.push("tutor", Reply::Text("We'll do it by hand."));
    // The turn finishes normally: refusal, then the tutor's reply.
    p.send_learner_message(&id, "measure it").await.unwrap();
    let refused = results(&p, &id, "bench_run");
    assert!(
        refused[0].1 && refused[0].0.contains("`bench` only runs on desktop"),
        "{refused:?}"
    );
    let session = p.load_session(&id).unwrap();
    assert!(session.messages.last().unwrap().content.contains("by hand"));
    // Locked once the turn ended.
    assert!(session.summary.read_only);
    assert_eq!(
        session.summary.read_only_reason.as_deref(),
        Some("desktop-plugin")
    );
    assert_eq!(session.summary.desktop_only_plugins, ["Bench"]);
    assert!(p.send_learner_message(&id, "and now?").await.is_err());
    assert!(s.unexpected.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_job_finishing_into_a_locked_session_runs_no_turn_until_unlocked() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("mobile-job");
    let s = &*script;
    let desk = harness(&repo, &config, &base).await;
    desk.register_native_plugin(bench(&["desktop"])).unwrap();
    let id = teaching_with_bench(&desk, s, &release).await;

    // On the phone, one turn starts a research job and calls bench: the
    // session locks when the turn ends, with the job still running.
    let p = phone(&repo, "phone", &base).await;
    with_index(&p, &repo).await;
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![
                (
                    "r1",
                    "research_request",
                    json!({ "kind": "verify", "question": "Is 6 V right?", "context": "divider" }),
                ),
                ("b1", "bench_run", json!({})),
            ],
        ),
    );
    s.push("tutor", Reply::Text("Checking that; by hand meanwhile."));
    p.send_learner_message(&id, "measure it").await.unwrap();
    assert!(p.load_session(&id).unwrap().summary.read_only);
    let turns = s.requests_of("tutor").len();

    // The job ends: its op is recorded, nothing is delivered, no turn runs.
    s.push(
        "researcher",
        Reply::Calls(
            "",
            vec![(
                "v1",
                "brief_submit",
                json!({ "kind": "verify", "question": "Is 6 V right?", "answer": "Yes." }),
            )],
        ),
    );
    release.add_permits(1);
    p.wait_background(&id).await;
    assert_eq!(s.requests_of("tutor").len(), turns, "no tutor turn ran");
    // This job's brief (the scope brief before approval has the same tag).
    let brief = |h: &Harness| {
        ops(h, &id).iter().any(|o| {
            matches!(o, Op::Message(m) if m.tag.as_deref() == Some("research-brief")
                && m.content.contains("Is 6 V right?"))
        })
    };
    let session = p.load_session(&id).unwrap();
    assert!(session.summary.read_only);
    assert!(session
        .ops
        .iter()
        .any(|o| matches!(o, Op::Job(j) if j.state == "done" && j.result.is_some())));
    assert!(!brief(&p), "the result stays queued");

    // After "Continue here anyway", the next turn delivers it.
    p.mobile_unlock(&id).unwrap();
    s.push("tutor", Reply::Text("The check came back: yes."));
    p.send_learner_message(&id, "and?").await.unwrap();
    assert!(brief(&p));
    assert!(s.unexpected.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plugin_with_unknown_platforms_never_locks_the_phone() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("mobile-unknown");
    let s = &*script;
    let desk = harness(&repo, &config, &base).await;
    desk.register_native_plugin(bench(&["desktop"])).unwrap();
    let id = teaching_with_bench(&desk, s, &release).await;

    // No index, not installed, never called before: the phone can't know
    // where bench runs. The call fails as it always did, and nothing locks.
    let p = phone(&repo, "phone", &base).await;
    s.push(
        "tutor",
        Reply::Calls("", vec![("b1", "bench_run", json!({}))]),
    );
    s.push("tutor", Reply::Text("Never mind."));
    p.send_learner_message(&id, "measure it").await.unwrap();
    let refused = results(&p, &id, "bench_run");
    assert!(
        refused[0].1 && !refused[0].0.contains("only runs on desktop"),
        "{refused:?}"
    );
    let call = &tool_ops(&p, &id, "bench_run")[0];
    assert!(call.get("platforms").is_none(), "{call}");
    assert!(!p.load_session(&id).unwrap().summary.read_only);
    assert!(s.unexpected.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_attached_folder_never_locks_and_the_tutor_is_told() {
    let script = Arc::new(Script::default());
    let base = serve(Arc::clone(&script), Arc::new(Semaphore::new(0))).await;
    let (repo, config) = temp_dirs("mobile-folder");
    let s = &*script;
    let desk = harness(&repo, &config, &base).await;
    s.push("tutor", Reply::Text("Hello."));
    s.push("title", Reply::Text("Thesis"));
    let id = desk.start_teaching("help with my thesis").await.unwrap().id;
    let folder = repo.parent().unwrap().join("thesis-notes");
    std::fs::create_dir_all(&folder).unwrap();
    let name = desk
        .attach_workspace(&id, folder.to_str().unwrap())
        .unwrap();
    assert_eq!(name, "thesis-notes");

    // The phone has no mapping for it: writable, the tutor is told, and
    // workspace_write says there is no workspace here.
    let p = phone(&repo, "phone", &base).await;
    assert!(!p.load_session(&id).unwrap().summary.read_only);
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "w1",
                "workspace_write",
                json!({ "path": "notes.md", "content": "x" }),
            )],
        ),
    );
    s.push(
        "tutor",
        Reply::Text("Your folder is on the other computer."),
    );
    p.send_learner_message(&id, "save my notes").await.unwrap();
    let request = s.requests_of("tutor").into_iter().rev().nth(1).unwrap();
    assert!(
        system(&request).contains("`thesis-notes`"),
        "{}",
        system(&request)
    );
    let refused = results(&p, &id, "workspace_write");
    assert!(
        refused[0].1
            && refused[0]
                .0
                .contains("no workspace is attached to this session"),
        "{refused:?}"
    );
    assert!(!p.load_session(&id).unwrap().summary.read_only);
    assert!(!folder.join("notes.md").exists());
    assert!(s.unexpected.lock().unwrap().is_empty());
}
