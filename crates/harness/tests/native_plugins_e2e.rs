//! Built-in native plugins through the agent loop (docs/plugins.md §12):
//! consent decides what the planner and tutor are offered, a granted native
//! tool dispatches, an answer-mode native grades the learner's work, and the
//! mobile rule refuses a desktop-only native tool. The model is scripted;
//! toolchains are real where they exist on this machine (python3 with SymPy
//! for `cas`, python3 for `repl`) and those parts are skipped, loudly, where
//! they don't.

mod common;

use std::path::Path;
use std::sync::Arc;

use common::*;
use harness::plugins::native::{cas, repl};
use harness::{Harness, Op};
use serde_json::{json, Value};
use tokio::sync::Semaphore;

fn info(h: &Harness, id: &str) -> harness::plugins::PluginInfo {
    h.plugins().into_iter().find(|p| p.id == id).unwrap()
}

/// The same repo and config directory opened again, as after a restart.
fn reopen(repo: &Path, config: &Path) -> Harness {
    Harness::open_with_secrets(
        repo,
        config,
        Arc::new(harness::secrets::SecretStore::memory()),
    )
    .unwrap()
}

fn settings(config: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(config.join("settings.json")).unwrap()).unwrap()
}

fn listed(list: &Value, id: &str) -> bool {
    list.as_array().is_some_and(|a| a.iter().any(|v| v == id))
}
/// Whether a catalog (the planner's plugin list) has a line for plugin `id`.
/// The planner prompt also names plugins in its examples, so match the line.
fn lists(catalog: &str, id: &str) -> bool {
    catalog.contains(&format!("(`{id}`):"))
}

/// The planner's first request, which carries the plugin catalog.
fn catalog(s: &Script) -> String {
    s.requests_of("planner")[0]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn system(body: &Value) -> String {
    body["messages"][0]["content"].as_str().unwrap().to_owned()
}

fn artifacts(h: &Harness, id: &str) -> Vec<Value> {
    ops(h, id)
        .iter()
        .filter_map(|o| match o {
            Op::Artifact(a) => serde_json::to_value(a).ok(),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn builtins_that_declare_permissions_start_off_and_enabling_is_the_recorded_consent() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("native-consent");
    let h = harness(&repo, &config, &base).await;

    let natives: Vec<_> = h.plugins().into_iter().filter(|p| p.builtin).collect();
    let ids: Vec<&str> = natives.iter().map(|p| p.id.as_str()).collect();
    for id in [
        "cas",
        "code-parse",
        "code-runner",
        "image-set",
        "live-search",
        "repl",
        "sandbox",
    ] {
        assert!(ids.contains(&id), "{id} is a built-in: {ids:?}");
    }
    for p in &natives {
        // Off until consent exactly when a permission is declared.
        assert_eq!(p.enabled, p.permissions.is_empty(), "{}: {p:?}", p.id);
        assert_eq!(p.source, "built-in", "{}", p.id);
        // Running programs needs the local toolchains a phone doesn't have.
        if p.permissions.iter().any(|x| x == "exec") {
            assert_eq!(p.platforms, ["desktop"], "{} claims mobile", p.id);
        }
        assert!(p.on_platform, "{} should run on desktop", p.id);
    }
    assert_eq!(info(&h, "code-parse").platforms, ["desktop", "mobile"]);
    assert!(settings(&config).get("enabledPlugins").is_none());

    // A built-in can be turned off, not removed.
    let err = h.remove_plugin("cas").unwrap_err().to_string();
    assert!(
        err.contains("built-in") || err.contains("built in"),
        "{err}"
    );

    // Enabling one with permissions records the consent in settings.json and
    // it survives a restart; disabling withdraws it.
    h.set_plugin_enabled("cas", true).unwrap();
    assert!(listed(&settings(&config)["enabledPlugins"], "cas"));
    assert!(info(&h, "cas").enabled);
    let h = reopen(&repo, &config);
    assert!(info(&h, "cas").enabled, "consent lost across a restart");
    assert!(
        !info(&h, "repl").enabled,
        "consent leaked to another plugin"
    );
    h.set_plugin_enabled("cas", false).unwrap();
    let after = settings(&config);
    assert!(after
        .get("enabledPlugins")
        .is_none_or(|l| !listed(l, "cas")));
    assert!(listed(&after["disabledPlugins"], "cas"));
    let h = reopen(&repo, &config);
    assert!(!info(&h, "cas").enabled);

    // One without permissions needs no consent: it is on from the start,
    // stays off when switched off, and records nothing as consent.
    h.set_plugin_enabled("code-parse", false).unwrap();
    assert!(!info(&reopen(&repo, &config), "code-parse").enabled);
    h.set_plugin_enabled("code-parse", true).unwrap();
    assert!(settings(&config)
        .get("enabledPlugins")
        .is_none_or(|l| !listed(l, "code-parse")));
    assert!(info(&reopen(&repo, &config), "code-parse").enabled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_native_is_offered_and_dispatched_once_consented_and_replays() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("native-grant");
    let h = harness(&repo, &config, &base).await;
    let s = &*script;

    // The planner's catalog lists the permission-free built-in and not the
    // ones still waiting for consent; a plan naming one is refused, and the
    // planner's retry with `code-parse` goes through.
    let id = session_awaiting_approval(
        &h,
        s,
        &release,
        vec![
            Reply::Calls("", vec![("w0", "plan_write", plan_args(&["cas"]))]),
            Reply::Calls("", vec![("w1", "plan_write", plan_args(&["code-parse"]))]),
        ],
    )
    .await;
    let catalogue = catalog(s);
    assert!(lists(&catalogue, "code-parse"), "{catalogue}");
    for gated in [
        "cas",
        "repl",
        "code-runner",
        "live-search",
        "image-set",
        "sandbox",
    ] {
        assert!(!lists(&catalogue, gated), "{gated} offered before consent");
    }
    let retry = s.requests_of("planner").into_iter().nth(1).unwrap();
    let refusal = tool_message(&retry, "w0").expect("the planner saw its first plan refused");
    assert!(refusal.contains("cas"), "{refusal}");

    // Granted by the plan: the tutor gets its tools and its prompt fragment,
    // and a call runs the real parser.
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![
                (
                    "e1",
                    "code_parse_errors",
                    json!({ "language": "python", "source": "def f(x)\n    return x\n" }),
                ),
                (
                    "o1",
                    "code_parse_outline",
                    json!({ "language": "python", "source": "class A:\n    def m(self):\n        pass\n" }),
                ),
            ],
        ),
    );
    s.push("tutor", Reply::Text("Look at line 1."));
    h.approve(&id, vec![]).await.unwrap();
    assert_eq!(h.session_state(&id).unwrap().active_plugins, ["code-parse"]);
    let request = s.requests_of("tutor").pop().unwrap();
    let offered = tool_names(&request);
    for tool in [
        "code_parse_errors",
        "code_parse_outline",
        "code_parse_locate",
    ] {
        assert!(offered.contains(&tool.to_owned()), "{tool} in {offered:?}");
    }
    for tool in ["cas_check", "repl_eval", "repl_task"] {
        assert!(!offered.contains(&tool.to_owned()), "{tool} in {offered:?}");
    }
    assert!(
        system(&request).contains("Code structure"),
        "the plugin's tutor fragment is in the system prompt"
    );
    let parsed = results(&h, &id, "code_parse_errors");
    assert!(
        !parsed[0].1 && parsed[0].0.contains("1 syntax error") && parsed[0].0.contains("line 1"),
        "{parsed:?}"
    );
    let outline = results(&h, &id, "code_parse_outline");
    assert!(
        !outline[0].1 && outline[0].0.contains("A") && outline[0].0.contains("m"),
        "{outline:?}"
    );
    let call = &tool_ops(&h, &id, "code_parse_errors")[0];
    assert_eq!(call["plugin"], "code-parse");
    assert_eq!(call["platforms"], json!(["desktop", "mobile"]));

    // `cas` isn't consented: the tutor can't take it mid-section either.
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "r0",
                "plugin_request",
                json!({ "plugin": "cas", "reason": "check it" }),
            )],
        ),
    );
    s.push("tutor", Reply::Text("By hand, then."));
    h.send_learner_message(&id, "Can you check the algebra?")
        .await
        .unwrap();
    let denied = results(&h, &id, "plugin_request");
    assert!(
        denied[0].1 && denied[0].0.contains("`cas`") && denied[0].0.contains("not available"),
        "{denied:?}"
    );
    assert_eq!(h.session_state(&id).unwrap().active_plugins, ["code-parse"]);

    // The learner consents in Settings: now it is grantable, then offered,
    // then a call dispatches to the real engine.
    h.set_plugin_enabled("cas", true).unwrap();
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "r1",
                "plugin_request",
                json!({ "plugin": "cas", "reason": "check it" }),
            )],
        ),
    );
    s.push("tutor", Reply::Text("I can check it now."));
    h.send_learner_message(&id, "Please check it.")
        .await
        .unwrap();
    let granted = results(&h, &id, "plugin_request");
    assert!(
        !granted[1].1 && granted[1].0.starts_with("Granted"),
        "{granted:?}"
    );
    assert_eq!(
        h.session_state(&id).unwrap().active_plugins,
        ["code-parse", "cas"]
    );
    let after = s.requests_of("tutor").pop().unwrap();
    assert!(tool_names(&after).contains(&"cas_check".to_owned()));
    assert!(system(&after).contains("CAS check"));

    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "c1",
                "cas_check",
                json!({ "code": "print(expand((x + 1)**2))" }),
            )],
        ),
    );
    s.push("tutor", Reply::Text("Confirmed."));
    h.send_learner_message(&id, "Expand it.").await.unwrap();
    let checked = results(&h, &id, "cas_check");
    let engine = cas::check("print(1)");
    if engine.as_ref().is_ok_and(|o| o.ok) {
        assert!(
            !checked[0].1 && checked[0].0.contains("x**2 + 2*x + 1"),
            "{checked:?}"
        );
    } else {
        eprintln!("skipped the live SymPy call: {engine:?}");
        assert!(
            checked[0].1,
            "an unavailable engine is reported as an error: {checked:?}"
        );
    }
    let call = &tool_ops(&h, &id, "cas_check")[0];
    assert_eq!(call["plugin"], "cas");
    assert_eq!(call["platforms"], json!(["desktop"]));
    assert!(script.unexpected.lock().unwrap().is_empty());

    // Restart: the grants replay from the log and consent is still on file.
    drop(h);
    let h = reopen(&repo, &config);
    assert!(info(&h, "cas").enabled);
    assert_eq!(
        h.session_state(&id).unwrap().active_plugins,
        ["code-parse", "cas"]
    );
}

fn repl_question(artifact: &str, prompt: &str) -> Value {
    json!({ "role": "check", "level": "use", "nodeId": NODE, "format": "free", "prompt": prompt,
        "answer": "result = sum(range(11))",
        "view": { "artifactId": artifact, "task": "Make `result` the sum of 1 to 10.", "target": {} } })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_repl_task_is_graded_by_the_native_on_the_cards_final_state() {
    if repl::Lang::Python.toolchain().is_err() {
        eprintln!("skipped: no python3 on PATH");
        return;
    }
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("native-repl");
    let h = harness(&repo, &config, &base).await;
    let s = &*script;

    // Not consented yet: the card and its view are unreachable.
    let err = h
        .plugin_view_call(
            "repl",
            "input",
            json!({ "artifactId": "X", "lang": "python", "input": "1" }),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("disabled"), "{err}");
    h.set_plugin_enabled("repl", true).unwrap();

    let id = session_awaiting_approval(
        &h,
        s,
        &release,
        vec![Reply::Calls(
            "",
            vec![("w1", "plan_write", plan_args(&["repl"]))],
        )],
    )
    .await;
    assert!(lists(&catalog(s), "repl"));

    // The tutor evaluates for itself (state persists across its calls) and
    // makes the learner's task card.
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![
                ("v1", "repl_eval", json!({ "lang": "python", "input": "n = 6" })),
                ("v2", "repl_eval", json!({ "lang": "python", "input": "n * 7" })),
                (
                    "t1",
                    "repl_task",
                    json!({ "lang": "python", "prompt": "Make `result` the sum of 1 to 10.", "check": "result == 55" }),
                ),
            ],
        ),
    );
    s.push("tutor", Reply::Text("Your turn."));
    h.approve(&id, vec![]).await.unwrap();
    assert_eq!(h.session_state(&id).unwrap().active_plugins, ["repl"]);
    let evals = results(&h, &id, "repl_eval");
    assert!(!evals[1].1 && evals[1].0.contains("42"), "{evals:?}");
    let made = artifacts(&h, &id);
    assert_eq!(made.len(), 1, "{made:?}");
    assert_eq!(made[0]["pluginId"], "repl");
    assert_eq!(made[0]["tool"], "repl_task");
    assert_eq!(made[0]["input"]["check"], "result == 55");
    let card = made[0]["artifactId"].as_str().unwrap().to_owned();
    // The committed check never reaches the learner's card or the artifact data.
    let shown = format!("{} {}", made[0]["data"], made[0]["fallbackSvg"]);
    assert!(!shown.contains("55"), "{shown}");

    // Ask with the card as the answer surface; the learner works in the card.
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![("q1", "question_ask", repl_question(&card, "Do it."))],
        ),
    );
    h.send_learner_message(&id, "ready").await.unwrap();
    assert_eq!(
        h.session_state(&id).unwrap().pending_call.unwrap().call_id,
        "q1"
    );
    let entered = h
        .plugin_view_call(
            "repl",
            "input",
            json!({ "artifactId": card, "lang": "python", "input": "result = sum(range(10))" }),
        )
        .unwrap();
    assert_eq!(entered["ok"], true, "{entered}");
    // The next question waits for the tutor's verdict on this one.
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![
                (
                    "r1",
                    "verdict_record",
                    json!({ "questionId": "q1", "verdict": "wrong", "note": "summed 0 to 9" }),
                ),
                ("q2", "question_ask", repl_question(&card, "Again.")),
            ],
        ),
    );
    h.answer(&id, "q1", json!({ "answer": { "submitted": true } }))
        .await
        .unwrap();
    let wrong: Value =
        serde_json::from_str(&tool_message(&s.requests_of("tutor").pop().unwrap(), "q1").unwrap())
            .unwrap();
    assert_eq!(wrong["view"]["correct"], false, "{wrong}");
    assert_eq!(wrong["questionId"], "q1");

    h.plugin_view_call(
        "repl",
        "input",
        json!({ "artifactId": card, "lang": "python", "input": "result = sum(range(11))" }),
    )
    .unwrap();
    s.push("tutor", Reply::Text("That holds."));
    h.answer(&id, "q2", json!({ "answer": { "submitted": true } }))
        .await
        .unwrap();
    let right: Value =
        serde_json::from_str(&tool_message(&s.requests_of("tutor").pop().unwrap(), "q2").unwrap())
            .unwrap();
    assert_eq!(right["view"]["correct"], true, "{right}");
    assert_eq!(
        right["view"]["transcript"].as_array().unwrap().len(),
        2,
        "{right}"
    );
    // Grading is evidence for the tutor: the one verdict is the tutor's own.
    assert_eq!(results(&h, &id, "verdict_record").len(), 1);

    // Withdrawing consent stops the card: no more inputs, no more grading.
    h.set_plugin_enabled("repl", false).unwrap();
    let err = h
        .plugin_view_call(
            "repl",
            "input",
            json!({ "artifactId": card, "lang": "python", "input": "1" }),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("disabled"), "{err}");
    assert!(script.unexpected.lock().unwrap().is_empty());
}

/// Another device on `repo`: config dir `<root>/<name>`, platform mobile.
async fn phone(repo: &Path, name: &str, base: &str) -> Arc<Harness> {
    let config = repo.parent().unwrap().join(name);
    let h = harness(repo, &config, base).await;
    h.set_platform("mobile").unwrap();
    h
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_mobile_rule_keeps_desktop_only_natives_off_the_phone() {
    let script = Arc::new(Script::default());
    let release = Arc::new(Semaphore::new(0));
    let base = serve(Arc::clone(&script), Arc::clone(&release)).await;
    let (repo, config) = temp_dirs("native-mobile");
    let s = &*script;

    // Desktop: consent, plan, a real call to a desktop-only native.
    let desk = harness(&repo, &config, &base).await;
    desk.set_plugin_enabled("cas", true).unwrap();
    let id = session_awaiting_approval(
        &desk,
        s,
        &release,
        vec![Reply::Calls(
            "",
            vec![("w1", "plan_write", plan_args(&["cas"]))],
        )],
    )
    .await;
    s.push("tutor", Reply::Text("Section one."));
    desk.approve(&id, vec![]).await.unwrap();
    assert_eq!(desk.session_state(&id).unwrap().active_plugins, ["cas"]);
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![("c1", "cas_check", json!({ "code": "print(1 + 1)" }))],
        ),
    );
    s.push("tutor", Reply::Text("Two."));
    desk.send_learner_message(&id, "what is 1 + 1?")
        .await
        .unwrap();
    assert_eq!(
        tool_ops(&desk, &id, "cas_check")[0]["platforms"],
        json!(["desktop"])
    );

    // The phone: the session is locked and names the plugin; unlocking it
    // never gives the phone the tool.
    let p = phone(&repo, "phone", &base).await;
    let cas_on_phone = info(&p, "cas");
    assert!(!cas_on_phone.on_platform);
    assert!(info(&p, "code-parse").on_platform);
    let session = p.load_session(&id).unwrap();
    assert!(session.summary.read_only);
    assert_eq!(
        session.summary.read_only_reason.as_deref(),
        Some("desktop-plugin")
    );
    assert_eq!(
        session.summary.desktop_only_plugins,
        std::slice::from_ref(&cas_on_phone.title)
    );
    p.mobile_unlock(&id).unwrap();
    s.push(
        "tutor",
        Reply::Calls("", vec![("c2", "cas_check", json!({ "code": "print(2)" }))]),
    );
    s.push("tutor", Reply::Text("By hand."));
    p.send_learner_message(&id, "and 2?").await.unwrap();
    let request = s.requests_of("tutor").into_iter().rev().nth(1).unwrap();
    assert!(!tool_names(&request).contains(&"cas_check".to_owned()));
    let refused = results(&p, &id, "cas_check");
    let last = refused.last().unwrap();
    assert!(
        last.1 && last.0.contains("`cas` only runs on desktop"),
        "{refused:?}"
    );

    // A session planned on the phone is offered only what runs there.
    let planned = session_awaiting_approval(
        &p,
        s,
        &release,
        vec![Reply::Calls(
            "",
            vec![("w1", "plan_write", plan_args(&["code-parse"]))],
        )],
    )
    .await;
    let planner = s.requests_of("planner").pop().unwrap()["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(lists(&planner, "code-parse"), "{planner}");
    for gone in ["cas", "repl", "code-runner"] {
        assert!(!lists(&planner, gone), "{gone} offered on a phone");
    }
    s.push(
        "tutor",
        Reply::Calls(
            "",
            vec![(
                "e1",
                "code_parse_errors",
                json!({ "language": "rust", "source": "fn main( {}" }),
            )],
        ),
    );
    s.push("tutor", Reply::Text("The parenthesis."));
    p.approve(&planned, vec![]).await.unwrap();
    let parsed = results(&p, &planned, "code_parse_errors");
    assert!(
        !parsed[0].1 && !parsed[0].0.contains("parses cleanly"),
        "{parsed:?}"
    );
    assert!(!p.load_session(&planned).unwrap().summary.read_only);
    assert!(script.unexpected.lock().unwrap().is_empty());
}
