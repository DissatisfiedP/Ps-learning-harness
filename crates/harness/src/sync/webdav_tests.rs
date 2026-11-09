//! WebDAV target tests (docs/sync-build.md, "WebDAV target", Verification)
//! against a small WebDAV server on `127.0.0.1:0` over a temp dir: no network
//! and no external binary. Also the automatic triggers (periodic, if
//! changed, before exit), which need both targets.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;

use super::tests::{bare, header, op, op_ids, snapshot, Device, Tmp};
use super::{ConflictPath, Sync, WebdavReport};

const USER: &str = "alice";
const PASSWORD: &str = "app-pass-7Qx9";
const SESSION: &str = "sessions/2026/s.jsonl";

/// A WebDAV server good enough for the sync pass: `PROPFIND` (Depth 0/1,
/// `infinity` refused like many hosts), `GET`, `PUT` and `DELETE` honouring
/// `If-Match`/`If-None-Match`, `MKCOL`, Basic auth, and an ETag that changes
/// on every write. Hrefs alternate between absolute paths and full URLs, the
/// namespace prefix is `D:`, and ETags are entity-escaped, as real servers do.
struct Server {
    _root: Tmp,
    port: u16,
    state: Arc<State>,
}

struct State {
    files: PathBuf,
    host: String,
    inner: Mutex<Inner>,
    precondition_failed: AtomicU32,
}

#[derive(Default)]
struct Inner {
    etags: HashMap<String, u64>,
    next: u64,
    /// After each `GET` of this path, append the next line: another device
    /// writing between this device's `GET` and its `PUT`.
    inject: Option<(String, Vec<String>)>,
}

impl Inner {
    fn bump(&mut self, rel: &str) -> u64 {
        self.next += 1;
        self.etags.insert(rel.to_owned(), self.next);
        self.next
    }

    fn etag(&mut self, rel: &str) -> u64 {
        match self.etags.get(rel) {
            Some(etag) => *etag,
            None => self.bump(rel),
        }
    }
}

impl Server {
    fn start(tag: &str) -> Self {
        let root = Tmp::new(tag);
        let files = root.path().join("dav");
        fs::create_dir_all(&files).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(State {
            files,
            host: format!("http://127.0.0.1:{port}"),
            inner: Mutex::new(Inner::default()),
            precondition_failed: AtomicU32::new(0),
        });
        let shared = Arc::clone(&state);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let state = Arc::clone(&shared);
                std::thread::spawn(move || serve(stream, &state));
            }
        });
        Self {
            _root: root,
            port,
            state,
        }
    }

    /// The sync folder; it does not exist until the first pass creates it.
    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/dav/learning%20data/", self.port)
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.state.files.join("learning data").join(rel)
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.path(rel)).unwrap()
    }

    fn exists(&self, rel: &str) -> bool {
        self.path(rel).exists()
    }

    fn inject(&self, rel: &str, lines: Vec<String>) {
        let mut inner = crate::lock(&self.state.inner);
        inner.inject = Some((format!("learning data/{rel}"), lines));
    }

    fn precondition_failures(&self) -> u32 {
        self.state.precondition_failed.load(Ordering::SeqCst)
    }
}

fn serve(stream: TcpStream, state: &State) {
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_half);
    let mut writer = stream;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut parts = line.split_whitespace();
        let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
            return;
        };
        let (method, target) = (method.to_owned(), target.to_owned());
        let mut headers = HashMap::new();
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).unwrap_or(0) == 0 {
                return;
            }
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':') {
                headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
            }
        }
        let length: usize = headers
            .get("content-length")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let mut body = vec![0; length];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        let response = handle(state, &method, &target, &headers, &body);
        if writer.write_all(&response).is_err() {
            return;
        }
    }
}

fn respond(status: &str, headers: &[(&str, String)], body: &[u8]) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n", body.len());
    for (name, value) in headers {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    out.push_str("\r\n");
    let mut out = out.into_bytes();
    out.extend_from_slice(body);
    out
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(value) = u8::from_str_radix(&text[i + 1..i + 3], 16) {
                out.push(value);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap()
}

fn encode(rel: &str) -> String {
    rel.split('/')
        .map(|segment| {
            segment
                .bytes()
                .map(|byte| {
                    if byte.is_ascii_alphanumeric() || b"-._".contains(&byte) {
                        char::from(byte).to_string()
                    } else {
                        format!("%{byte:02X}")
                    }
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn handle(
    state: &State,
    method: &str,
    target: &str,
    headers: &HashMap<String, String>,
    body: &[u8],
) -> Vec<u8> {
    let expected = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{USER}:{PASSWORD}"))
    );
    if headers.get("authorization") != Some(&expected) {
        return respond(
            "401 Unauthorized",
            &[("WWW-Authenticate", "Basic realm=\"dav\"".into())],
            b"",
        );
    }
    let Some(rest) = target.strip_prefix("/dav/") else {
        return respond("404 Not Found", &[], b"");
    };
    let rel = decode(rest).trim_end_matches('/').to_owned();
    assert!(
        !rel.split('/').any(|s| s == ".."),
        "the client escaped: {rel}"
    );
    let path = if rel.is_empty() {
        state.files.clone()
    } else {
        state.files.join(&rel)
    };
    let mut inner = crate::lock(&state.inner);
    let if_match = headers.get("if-match").cloned();
    let stale = |inner: &mut Inner| match &if_match {
        Some(tag) => !path.is_file() || *tag != format!("\"{}\"", inner.etag(&rel)),
        None => false,
    };
    match method {
        "PROPFIND" => {
            let depth = headers.get("depth").map_or("infinity", String::as_str);
            if depth == "infinity" {
                return respond("403 Forbidden", &[], b"");
            }
            if !path.exists() {
                return respond("404 Not Found", &[], b"");
            }
            let mut listed = vec![(rel.clone(), path.clone(), true)];
            if path.is_dir() && depth == "1" {
                let mut children: Vec<_> = fs::read_dir(&path)
                    .unwrap()
                    .flatten()
                    .map(|entry| {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        let child = if rel.is_empty() {
                            name
                        } else {
                            format!("{rel}/{name}")
                        };
                        (child, entry.path(), false)
                    })
                    .collect();
                children.sort();
                listed.extend(children);
            }
            let mut xml = String::from(
                "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:multistatus xmlns:D=\"DAV:\">\n",
            );
            for (child, child_path, is_self) in listed {
                let dir = child_path.is_dir();
                let mut href = format!("/dav/{}", encode(&child));
                if dir {
                    href.push('/');
                }
                if !is_self {
                    href = format!("{}{href}", state.host);
                }
                let props = if dir {
                    "<D:resourcetype><D:collection/></D:resourcetype>".to_owned()
                } else {
                    let len = fs::metadata(&child_path).unwrap().len();
                    format!(
                        "<D:resourcetype/><D:getetag>&quot;{}&quot;</D:getetag><D:getcontentlength>{len}</D:getcontentlength>",
                        inner.etag(&child)
                    )
                };
                xml.push_str(&format!(
                    "<D:response><D:href>{href}</D:href><D:propstat><D:prop>{props}</D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat>"
                ));
                if dir {
                    xml.push_str("<D:propstat><D:prop><D:getetag/></D:prop><D:status>HTTP/1.1 404 Not Found</D:status></D:propstat>");
                }
                xml.push_str("</D:response>\n");
            }
            xml.push_str("</D:multistatus>\n");
            respond(
                "207 Multi-Status",
                &[("Content-Type", "application/xml; charset=utf-8".into())],
                xml.as_bytes(),
            )
        }
        "GET" => {
            if !path.is_file() {
                return respond("404 Not Found", &[], b"");
            }
            let bytes = fs::read(&path).unwrap();
            let etag = inner.etag(&rel);
            let response = respond("200 OK", &[("ETag", format!("\"{etag}\""))], &bytes);
            let injected = match &mut inner.inject {
                Some((target, lines)) if *target == rel && !lines.is_empty() => {
                    Some(lines.remove(0))
                }
                _ => None,
            };
            if let Some(line) = injected {
                let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
                file.write_all(format!("{line}\n").as_bytes()).unwrap();
                inner.bump(&rel);
            }
            response
        }
        "PUT" => {
            if !path.parent().is_some_and(|parent| parent.is_dir()) {
                return respond("409 Conflict", &[], b"");
            }
            let none_match = headers.get("if-none-match").is_some_and(|v| v == "*");
            if stale(&mut inner) || (none_match && path.exists()) {
                state.precondition_failed.fetch_add(1, Ordering::SeqCst);
                return respond("412 Precondition Failed", &[], b"");
            }
            let created = !path.exists();
            fs::write(&path, body).unwrap();
            let etag = inner.bump(&rel);
            let status = if created {
                "201 Created"
            } else {
                "204 No Content"
            };
            respond(status, &[("ETag", format!("\"{etag}\""))], b"")
        }
        "DELETE" => {
            if !path.exists() {
                return respond("404 Not Found", &[], b"");
            }
            if stale(&mut inner) {
                state.precondition_failed.fetch_add(1, Ordering::SeqCst);
                return respond("412 Precondition Failed", &[], b"");
            }
            if path.is_dir() {
                fs::remove_dir_all(&path).unwrap();
            } else {
                fs::remove_file(&path).unwrap();
            }
            inner.etags.remove(&rel);
            respond("204 No Content", &[], b"")
        }
        "MKCOL" => {
            if path.exists() {
                return respond("405 Method Not Allowed", &[], b"");
            }
            if !path.parent().is_some_and(|parent| parent.is_dir()) {
                return respond("409 Conflict", &[], b"");
            }
            fs::create_dir(&path).unwrap();
            respond("201 Created", &[], b"")
        }
        _ => respond("405 Method Not Allowed", &[], b""),
    }
}

// ---- helpers ----------------------------------------------------------------

fn dav_device(tag: &str, server: &Server) -> Device {
    let device = Device::new(tag);
    device
        .sync
        .set_webdav(Some(&server.url()), Some(USER), Some(PASSWORD))
        .unwrap();
    device
}

fn dav_run(device: &Device) -> WebdavReport {
    device.run().webdav.expect("the WebDAV pass ran")
}

fn base_session() -> String {
    format!(
        "{}\n{}\n{}\n",
        header("01H00000000000000000000000", "2026-01-01T00:00:00Z"),
        op("01H00000000000000000000001", "2026-01-01T00:00:01Z", "one"),
        op("01H00000000000000000000002", "2026-01-01T00:00:02Z", "two"),
    )
}

fn assert_unique(text: &str) {
    let ids = op_ids(text);
    let mut unique = ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(ids.len(), unique.len(), "no duplicate opId: {text}");
}

// ---- tests ------------------------------------------------------------------

#[test]
fn first_upload_creates_the_folder_and_another_device_pulls_it() {
    let server = Server::start("dav-first");
    let a = dav_device("dav-first-a", &server);
    a.write(SESSION, &base_session());
    a.write("prompts/my notes.md", "Explain with pictures.\n");
    assert!(a.sync.status().unwrap().webdav.unsynced, "never synced");

    let first = dav_run(&a);
    assert_eq!(first.uploaded, 2, "{first:?}");
    assert!(server.exists(""), "the missing sync folder was created");
    assert_eq!(server.read(SESSION), base_session());
    assert_eq!(
        server.read("prompts/my notes.md"),
        "Explain with pictures.\n"
    );
    let status = a.sync.status().unwrap();
    assert!(!status.webdav.unsynced);
    assert!(status.webdav.last_sync_at.is_some());
    assert!(!status.configured, "git stays off");
    assert!(
        a.config.join("webdav-state.json").exists() && !a.repo.join("webdav-state.json").exists(),
        "the manifest is device-local"
    );
    assert_eq!(dav_run(&a), WebdavReport::default(), "nothing left to do");

    // A second device pulls both files byte for byte.
    let b = dav_device("dav-first-b", &server);
    assert_eq!(dav_run(&b).downloaded, 2);
    assert_eq!(snapshot(&b.repo), snapshot(&a.repo));

    // A third device that already holds identical bytes just records them.
    let c = dav_device("dav-first-c", &server);
    c.write(SESSION, &base_session());
    let joined = c.run();
    assert!(joined.conflicts.is_empty(), "{:?}", joined.conflicts);
    assert_eq!(joined.webdav.unwrap().merged, 0);
    assert_eq!(snapshot(&c.repo), snapshot(&a.repo));

    // A write on one device reaches the other.
    let other = "sessions/2026/b.jsonl";
    b.write(
        other,
        &format!(
            "{}\n",
            op(
                "01H0000000000000000000000B",
                "2026-01-02T00:00:00Z",
                "from b"
            )
        ),
    );
    assert_eq!(dav_run(&b).uploaded, 1);
    assert_eq!(dav_run(&a).downloaded, 1);
    assert!(a.read(other).contains("from b"));
}

#[test]
fn both_devices_appending_to_one_session_merge_by_union() {
    let server = Server::start("dav-union");
    let a = dav_device("dav-union-a", &server);
    a.write(SESSION, &base_session());
    dav_run(&a);
    let b = dav_device("dav-union-b", &server);
    dav_run(&b);

    a.append(
        SESSION,
        &op(
            "01H00000000000000000000003",
            "2026-01-01T00:00:03Z",
            "from a",
        ),
    );
    b.append(
        SESSION,
        &op(
            "01H00000000000000000000004",
            "2026-01-01T00:00:04Z",
            "from b",
        ),
    );
    // A torn write on one side is dropped, not fatal.
    b.append(
        SESSION,
        "{\"op\":\"message\",\"opId\":\"01H0000000000000000000000",
    );
    assert_eq!(dav_run(&a).uploaded, 1);

    let report = b.run();
    assert_eq!(report.webdav.as_ref().unwrap().merged, 1);
    assert_eq!(
        report.conflicts,
        vec![ConflictPath {
            path: SESSION.into(),
            resolution: "union".into(),
        }]
    );
    assert!(report.needs_attention.is_empty(), "a union loses nothing");
    let expected = vec![
        "01H00000000000000000000000",
        "01H00000000000000000000001",
        "01H00000000000000000000002",
        "01H00000000000000000000003",
        "01H00000000000000000000004",
    ];
    assert_eq!(op_ids(&b.read(SESSION)), expected);
    assert_eq!(
        server.read(SESSION),
        b.read(SESSION),
        "the union was uploaded"
    );

    assert_eq!(dav_run(&a).downloaded, 1);
    assert_eq!(op_ids(&a.read(SESSION)), expected);
    assert_unique(&a.read(SESSION));
}

#[test]
fn a_concurrent_write_is_a_412_then_a_remerge() {
    let server = Server::start("dav-412");
    let a = dav_device("dav-412-a", &server);
    a.write(SESSION, &base_session());
    dav_run(&a);
    let b = dav_device("dav-412-b", &server);
    dav_run(&b);

    a.append(
        SESSION,
        &op(
            "01H00000000000000000000003",
            "2026-01-01T00:00:03Z",
            "from a",
        ),
    );
    b.append(
        SESSION,
        &op(
            "01H00000000000000000000004",
            "2026-01-01T00:00:04Z",
            "from b",
        ),
    );
    dav_run(&b);
    // A third device writes right after a's GET, before its PUT.
    server.inject(
        SESSION,
        vec![op(
            "01H00000000000000000000005",
            "2026-01-01T00:00:05Z",
            "from c",
        )],
    );

    let report = dav_run(&a);
    assert_eq!(
        server.precondition_failures(),
        1,
        "the stale PUT was refused"
    );
    assert_eq!((report.merged, report.skipped), (1, 0), "{report:?}");
    let ids = op_ids(&server.read(SESSION));
    assert_eq!(ids.len(), 6, "every device's op is kept: {ids:?}");
    assert_unique(&server.read(SESSION));
    assert_eq!(a.read(SESSION), server.read(SESSION));
}

#[test]
fn a_path_that_keeps_changing_is_left_for_the_next_run() {
    let server = Server::start("dav-busy");
    let a = dav_device("dav-busy-a", &server);
    a.write(SESSION, &base_session());
    dav_run(&a);
    let b = dav_device("dav-busy-b", &server);
    dav_run(&b);
    b.append(
        SESSION,
        &op(
            "01H00000000000000000000004",
            "2026-01-01T00:00:04Z",
            "from b",
        ),
    );
    dav_run(&b);
    a.append(
        SESSION,
        &op(
            "01H00000000000000000000003",
            "2026-01-01T00:00:03Z",
            "from a",
        ),
    );
    a.write("graph/topics.json", "{}");
    // Another device writes after every GET of a's merge, more often than
    // the retries allow.
    server.inject(
        SESSION,
        (0..10)
            .map(|i| {
                op(
                    &format!("01H0000000000000000000010{i}"),
                    "2026-01-01T00:01:00Z",
                    "busy",
                )
            })
            .collect(),
    );

    let error = a.sync.sync().unwrap_err().to_string();
    assert!(error.contains("left for the next run"), "{error}");
    assert_eq!(
        server.precondition_failures(),
        4,
        "one try plus three retries"
    );
    assert!(server.exists("graph/topics.json"), "other paths still sync");
    let status = a.sync.status().unwrap().webdav;
    assert!(status.last_error.unwrap().contains(SESSION));
    assert!(status.unsynced);

    server.inject(SESSION, Vec::new());
    dav_run(&a);
    assert!(a.read(SESSION).contains("from a") && a.read(SESSION).contains("from b"));
    assert_unique(&a.read(SESSION));
    assert_eq!(server.read(SESSION), a.read(SESSION));
    assert!(a.sync.status().unwrap().webdav.last_error.is_none());
}

#[test]
fn a_deleted_session_stays_deleted_and_a_change_beats_a_delete() {
    let server = Server::start("dav-delete");
    let a = dav_device("dav-delete-a", &server);
    let gone = "sessions/2026/gone.jsonl";
    let kept = "sessions/2026/kept.jsonl";
    a.write(gone, &base_session());
    a.write(kept, &base_session());
    dav_run(&a);
    let b = dav_device("dav-delete-b", &server);
    dav_run(&b);

    fs::remove_file(a.repo.join(gone)).unwrap();
    assert_eq!(dav_run(&a).deleted_remote, 1);
    assert!(!server.exists(gone));
    assert_eq!(dav_run(&b).deleted_local, 1);
    assert!(!b.repo.join(gone).exists());
    dav_run(&a);
    dav_run(&b);
    assert!(
        !server.exists(gone) && !a.repo.join(gone).exists() && !b.repo.join(gone).exists(),
        "the deleted session never comes back"
    );

    // b changes a session a deletes: the change wins on every device.
    b.append(
        kept,
        &op("01H00000000000000000000003", "2026-01-01T00:00:03Z", "kept"),
    );
    fs::remove_file(a.repo.join(kept)).unwrap();
    dav_run(&a);
    assert!(!server.exists(kept));
    assert_eq!(dav_run(&b).uploaded, 1, "b's change is uploaded again");
    assert_eq!(dav_run(&a).downloaded, 1);
    assert!(a.read(kept).contains("kept"));
}

#[test]
fn a_wrong_password_is_readable_and_leaves_local_files_untouched() {
    let server = Server::start("dav-auth");
    let a = dav_device("dav-auth-a", &server);
    a.write(SESSION, &base_session());
    dav_run(&a);

    let wrong = "wrong-secret-31";
    a.sync
        .set_webdav(Some(&server.url()), None, Some(wrong))
        .unwrap();
    a.append(
        SESSION,
        &op(
            "01H00000000000000000000003",
            "2026-01-01T00:00:03Z",
            "local",
        ),
    );
    let before = snapshot(&a.repo);
    let error = a.sync.sync().unwrap_err().to_string();
    assert!(
        error.contains("username or app password was rejected"),
        "{error}"
    );
    assert!(!error.contains(wrong));
    assert_eq!(snapshot(&a.repo), before, "local files are byte-identical");

    let status = a.sync.status().unwrap().webdav;
    assert!(status.last_error.unwrap().contains("rejected"));
    assert!(status.unsynced, "the local change is still unsynced");
    assert!(status.has_password);
    assert!(!format!("{:?}", a.sync.config()).contains(wrong));
    assert!(!serde_json::to_string(&a.sync.status().unwrap())
        .unwrap()
        .contains(wrong));
}

#[test]
fn a_non_jsonl_conflict_keeps_local_and_parks_the_remote_copy() {
    let server = Server::start("dav-graph");
    let a = dav_device("dav-graph-a", &server);
    let topics = "graph/topics.json";
    a.write(topics, r#"{"topics":[{"id":"t","title":"one"}]}"#);
    dav_run(&a);
    let b = dav_device("dav-graph-b", &server);
    dav_run(&b);

    let from_a = r#"{"topics":[{"id":"t","title":"from a"}]}"#;
    let from_b = r#"{"topics":[{"id":"t","title":"from b"}]}"#;
    a.write(topics, from_a);
    dav_run(&a);
    b.write(topics, from_b);

    let report = b.run();
    assert_eq!(
        report.conflicts,
        vec![ConflictPath {
            path: topics.into(),
            resolution: "local-wins".into(),
        }]
    );
    assert_eq!(report.needs_attention, vec![topics.to_owned()]);
    assert_eq!(b.read(topics), from_b, "local wins");
    assert_eq!(server.read(topics), from_b, "and is uploaded");
    let parked: Vec<PathBuf> = fs::read_dir(b.repo.join(".conflicts/graph"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(parked.len(), 1);
    let name = parked[0]
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(name.starts_with("topics.json."), "{name}");
    assert_eq!(fs::read_to_string(&parked[0]).unwrap(), from_a);
    assert_eq!(
        b.sync.status().unwrap().needs_attention,
        vec![topics.to_owned()]
    );

    // The copy syncs like any other file, so the other device has it too.
    dav_run(&b);
    assert!(server.exists(&format!(".conflicts/graph/{name}")));
}

#[test]
fn git_and_webdav_carry_changes_between_devices_through_a_third() {
    let (_remote_root, remote) = bare("both");
    let server = Server::start("both-dav");
    // c syncs both targets, a only git, b only WebDAV.
    let c = Device::connected("both-c", &remote);
    c.sync
        .set_webdav(Some(&server.url()), Some(USER), Some(PASSWORD))
        .unwrap();
    c.write(SESSION, &base_session());
    let first = c.run();
    assert!(first.committed);
    assert!(first.webdav.unwrap().uploaded >= 1);

    let a = Device::connected("both-a", &remote);
    a.run();
    let b = dav_device("both-b", &server);
    dav_run(&b);

    a.append(
        SESSION,
        &op(
            "01H0000000000000000000000A",
            "2026-01-01T00:00:03Z",
            "from a",
        ),
    );
    a.run();
    b.append(
        SESSION,
        &op(
            "01H0000000000000000000000B",
            "2026-01-01T00:00:04Z",
            "from b",
        ),
    );
    dav_run(&b);

    // c's first run pulls a's op through git and merges b's in through
    // WebDAV; its second commits that union and pushes it.
    c.run();
    c.run();
    a.run();
    dav_run(&b);

    let expected = vec![
        "01H00000000000000000000000",
        "01H00000000000000000000001",
        "01H00000000000000000000002",
        "01H0000000000000000000000A",
        "01H0000000000000000000000B",
    ];
    for device in [&a, &b, &c] {
        assert_eq!(op_ids(&device.read(SESSION)), expected);
    }
    assert_eq!(snapshot(&a.repo), snapshot(&c.repo), "git-only matches");
    assert_eq!(snapshot(&b.repo), snapshot(&c.repo), "WebDAV-only matches");
}

#[test]
fn set_webdav_checks_the_url_and_keeps_or_clears_the_password() {
    let device = Device::new("dav-settings");
    for bad in [
        "http://example.com/dav/",
        "ftp://localhost/dav/",
        "not a url",
        "https://bob:hunter2@dav.example.com/dav/",
    ] {
        let error = device
            .sync
            .set_webdav(Some(bad), Some(USER), Some("pw"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("WebDAV"), "{bad}: {error}");
        assert!(!error.contains("hunter2"), "{error}");
    }
    assert!(
        !device.sync.status().unwrap().webdav.configured,
        "a refused URL saves nothing"
    );
    for good in [
        "http://localhost:8080/dav/",
        "http://127.0.0.1/dav/",
        "http://[::1]:9000/dav/",
    ] {
        device.sync.set_webdav(Some(good), None, None).unwrap();
        assert!(device.sync.status().unwrap().webdav.configured, "{good}");
    }

    let url = "https://dav.example.com/remote.php/dav/files/alice/learning";
    let saved = format!("{url}/");
    device
        .sync
        .set_webdav(Some(url), Some(" alice "), Some("first-pass"))
        .unwrap();
    let status = device.sync.status().unwrap().webdav;
    assert_eq!(status.url.as_deref(), Some(saved.as_str()));
    assert_eq!(status.username.as_deref(), Some("alice"));
    assert!(status.has_password);

    // Same URL, no password given: the saved one stays, and so does the
    // manifest.
    let manifest = device.config.join("webdav-state.json");
    fs::write(&manifest, format!(r#"{{"url":"{saved}","files":{{}}}}"#)).unwrap();
    device.sync.set_webdav(Some(&saved), None, None).unwrap();
    assert_eq!(
        device.sync.config().webdav.password.as_deref(),
        Some("first-pass")
    );
    assert!(manifest.exists());
    // An empty password clears it.
    device
        .sync
        .set_webdav(Some(&saved), None, Some(""))
        .unwrap();
    assert!(!device.sync.status().unwrap().webdav.has_password);
    // A new URL starts the manifest over.
    device
        .sync
        .set_webdav(Some("https://dav.example.com/other/"), None, Some("second"))
        .unwrap();
    assert!(!manifest.exists());

    // A blank URL disconnects and forgets everything.
    fs::write(&manifest, "{}").unwrap();
    let off = device.sync.set_webdav(Some("  "), None, None);
    assert!(off.is_ok());
    let status = device.sync.status().unwrap().webdav;
    assert!(!status.configured);
    assert_eq!((status.url, status.username), (None, None));
    assert!(!status.has_password);
    assert!(!manifest.exists());
    let file = fs::read_to_string(device.config.join("sync.json")).unwrap();
    assert!(
        !file.contains("dav.example.com") && !file.contains("second"),
        "{file}"
    );
    assert!(device.sync.sync().is_err(), "nothing is configured");
}

// ---- automatic triggers -------------------------------------------------

/// Wait for the background run in flight to finish.
fn wait_idle(sync: &Sync) {
    let start = Instant::now();
    while sync.running.load(Ordering::SeqCst) {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "the run never ended"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn remote_has(remote: &Path, rel: &str) -> bool {
    let Ok(repo) = git2::Repository::open(remote) else {
        return false;
    };
    let tree = repo
        .find_reference("refs/heads/main")
        .and_then(|reference| reference.peel_to_tree());
    tree.is_ok_and(|tree| tree.get_path(Path::new(rel)).is_ok())
}

#[test]
fn trigger_if_changed_runs_only_for_local_progress() {
    // Nothing configured: nothing to do.
    let idle = Device::new("changed-none");
    idle.write(SESSION, &base_session());
    assert!(!idle.sync.trigger_if_changed());

    // Git: clean after a run, so the trigger skips; a new op runs it.
    let (_remote_root, remote) = bare("changed-git");
    let git = Device::connected("changed-git-a", &remote);
    git.write(SESSION, &base_session());
    git.run();
    assert!(
        !git.sync.trigger_if_changed(),
        "a clean tree is not progress"
    );
    let other = "sessions/2026/new.jsonl";
    git.write(other, &base_session());
    assert!(git.sync.trigger_if_changed());
    wait_idle(&git.sync);
    assert!(remote_has(&remote, other), "the change was pushed");
    assert!(
        !git.sync.trigger_if_changed(),
        "and the tree is clean again"
    );

    // Remote-only changes are not local progress.
    let peer = Device::connected("changed-git-b", &remote);
    peer.run();
    peer.write("sessions/2026/peer.jsonl", &base_session());
    peer.run();
    assert!(!git.sync.trigger_if_changed());

    // WebDAV: the same, through the stat-only check.
    let server = Server::start("changed-dav");
    let dav = dav_device("changed-dav-a", &server);
    dav.write(SESSION, &base_session());
    assert!(dav.sync.trigger_if_changed(), "never synced");
    wait_idle(&dav.sync);
    assert!(server.exists(SESSION));
    assert!(!dav.sync.trigger_if_changed(), "nothing changed since");
    dav.append(
        SESSION,
        &op(
            "01H00000000000000000000003",
            "2026-01-01T00:00:03Z",
            "later",
        ),
    );
    assert!(dav.sync.trigger_if_changed());
    wait_idle(&dav.sync);
    assert!(server.read(SESSION).contains("later"));
    assert!(dav.sync.status().unwrap().webdav.last_error.is_none());
}

#[test]
fn sync_before_exit_gives_up_at_its_deadline() {
    // A server that accepts and never answers.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming().flatten() {
            held.push(stream);
        }
    });
    let device = Device::new("exit-stall");
    device
        .sync
        .set_webdav(
            Some(&format!("http://127.0.0.1:{port}/dav/")),
            Some(USER),
            Some(PASSWORD),
        )
        .unwrap();
    device.write(SESSION, &base_session());
    assert!(device.sync.pending_before_exit());
    let before = snapshot(&device.repo);

    let deadline = Duration::from_millis(800);
    let start = Instant::now();
    assert!(!device.sync.sync_before_exit(deadline), "it did not finish");
    let took = start.elapsed();
    assert!(
        took >= deadline && took < deadline + Duration::from_secs(2),
        "{took:?}"
    );
    assert_eq!(snapshot(&device.repo), before, "the tree is unchanged");
    let error = device.sync.status().unwrap().webdav.last_error.unwrap();
    assert!(error.contains("did not finish in time"), "{error}");

    // With nothing to sync, closing never waits.
    let clean = Device::new("exit-clean");
    assert!(!clean.sync.pending_before_exit());
    let start = Instant::now();
    assert!(clean.sync.sync_before_exit(Duration::from_secs(5)));
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn the_periodic_timer_syncs_progress_and_stops_with_the_engine() {
    let (_remote_root, remote) = bare("timer");
    let device = Device::new("timer-a");
    let engine = Arc::new(Sync::new(&device.repo, &device.config));
    engine
        .set(Some(remote.to_str().unwrap()), None, None)
        .unwrap();
    let timer = engine
        .start_periodic(Duration::from_millis(30))
        .expect("the timer started");
    assert!(
        engine.start_periodic(Duration::from_millis(30)).is_none(),
        "one timer per engine"
    );

    device.write(SESSION, &base_session());
    let start = Instant::now();
    while !remote_has(&remote, SESSION) {
        assert!(start.elapsed() < Duration::from_secs(20), "no tick synced");
        std::thread::sleep(Duration::from_millis(20));
    }
    wait_idle(&engine);
    let synced_at = engine.status().unwrap().last_sync_at;
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(
        engine.status().unwrap().last_sync_at,
        synced_at,
        "clean ticks don't run"
    );

    // Dropping the engine (what dropping the Harness does) ends the thread.
    drop(engine);
    let (done, ended) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = timer.join();
        let _ = done.send(());
    });
    assert!(
        ended.recv_timeout(Duration::from_secs(5)).is_ok(),
        "the timer thread stopped"
    );
}
