//! Sessions as append-only op logs: `sessions/<yyyy>/<ulid>.jsonl` in the data
//! repo (architecture §4.17). Each line is one op; readers order by `at`.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, Utc};
use providers::Role;
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::{now_iso, Error};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Learn,
    Practice,
}

/// Why an assistant message ended, when it was not a normal finish.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Ending {
    MaxTokens,
    Refusal,
    /// The learner pressed Stop.
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredMessage {
    pub op_id: String,
    pub at: String,
    pub role: Role,
    pub content: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub thinking: String,
    /// `provider/model` that wrote an assistant message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ending: Option<Ending>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Header {
    op_id: String,
    at: String,
    id: String,
    mode: Mode,
    title: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
enum Op {
    Session(Header),
    Message(StoredMessage),
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub id: String,
    pub mode: Mode,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    #[serde(flatten)]
    pub summary: SessionSummary,
    pub messages: Vec<StoredMessage>,
}

pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(repo: &Path) -> Self {
        Self {
            root: repo.join("sessions"),
        }
    }

    pub fn create(&self, mode: Mode, first_message: &str) -> Result<SessionSummary, Error> {
        let id = Ulid::new();
        let at = now_iso();
        let header = Header {
            op_id: Ulid::new().to_string(),
            at: at.clone(),
            id: id.to_string(),
            mode,
            title: title_from(first_message),
        };
        let path = self.path(&id);
        fs::create_dir_all(path.parent().expect("session path has a year dir"))?;
        append(&path, &Op::Session(header.clone()))?;
        Ok(SessionSummary {
            id: header.id,
            mode,
            title: header.title,
            created_at: at.clone(),
            updated_at: at,
        })
    }

    pub fn append(&self, id: &str, message: &StoredMessage) -> Result<(), Error> {
        let path = self.existing_path(id)?;
        append(&path, &Op::Message(message.clone()))
    }

    pub fn load(&self, id: &str) -> Result<Session, Error> {
        read(&self.existing_path(id)?)
    }

    pub fn delete(&self, id: &str) -> Result<(), Error> {
        fs::remove_file(self.existing_path(id)?)?;
        Ok(())
    }

    /// Newest activity first.
    pub fn list(&self) -> Result<Vec<SessionSummary>, Error> {
        let mut out = Vec::new();
        let years = match fs::read_dir(&self.root) {
            Ok(dir) => dir,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        for year in years {
            for entry in fs::read_dir(year?.path())? {
                let path = entry?.path();
                if path.extension().is_some_and(|e| e == "jsonl") {
                    out.push(read(&path)?.summary);
                }
            }
        }
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(out)
    }

    fn path(&self, id: &Ulid) -> PathBuf {
        let year = DateTime::<Utc>::from(id.datetime()).year();
        self.root.join(year.to_string()).join(format!("{id}.jsonl"))
    }

    /// Parsing as a ULID also rules out path traversal through the id.
    fn existing_path(&self, id: &str) -> Result<PathBuf, Error> {
        let ulid = Ulid::from_string(id).map_err(|_| Error::NotFound(format!("session {id}")))?;
        let path = self.path(&ulid);
        if path.exists() {
            Ok(path)
        } else {
            Err(Error::NotFound(format!("session {id}")))
        }
    }
}

pub(crate) fn new_message(role: Role, content: String) -> StoredMessage {
    StoredMessage {
        op_id: Ulid::new().to_string(),
        at: now_iso(),
        role,
        content,
        thinking: String::new(),
        model: None,
        ending: None,
    }
}

fn title_from(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("Untitled");
    match line.char_indices().nth(80) {
        Some((i, _)) => format!("{}…", line[..i].trim_end()),
        None => line.to_owned(),
    }
}

fn append(path: &Path, op: &Op) -> Result<(), Error> {
    let mut line = serde_json::to_vec(op)?;
    line.push(b'\n');
    // One write per op keeps lines whole under O_APPEND.
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(&line)?;
    Ok(())
}

fn read(path: &Path) -> Result<Session, Error> {
    let text = fs::read_to_string(path)?;
    let mut header: Option<Header> = None;
    let mut messages = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str::<Op>(line)? {
            Op::Session(h) => header = Some(h),
            Op::Message(m) => messages.push(m),
        }
    }
    let header = header
        .ok_or_else(|| Error::Corrupt(format!("{} has no session header", path.display())))?;
    // Lines from merged devices may interleave; `at` is the order of record.
    messages.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.op_id.cmp(&b.op_id)));
    let updated_at = messages
        .last()
        .map_or_else(|| header.at.clone(), |m| m.at.clone());
    Ok(Session {
        summary: SessionSummary {
            id: header.id,
            mode: header.mode,
            title: header.title,
            created_at: header.at,
            updated_at,
        },
        messages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lh-test-{name}-{}", Ulid::new()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn messages_survive_reload_in_time_order_and_list_is_newest_first() {
        let repo = temp_repo("store");
        let store = Store::new(&repo);
        let older = store.create(Mode::Learn, "first").unwrap();
        let newer = store.create(Mode::Practice, "second").unwrap();

        let mut late = new_message(Role::Assistant, "late".into());
        late.at = "2999-01-01T00:00:00.000Z".into();
        let early = new_message(Role::User, "early".into());
        // Appended out of order, as a union merge may leave them.
        store.append(&older.id, &late).unwrap();
        store.append(&older.id, &early).unwrap();

        let session = store.load(&older.id).unwrap();
        let contents: Vec<&str> = session
            .messages
            .iter()
            .map(|m| m.content.as_str())
            .collect();
        assert_eq!(contents, ["early", "late"]);
        assert_eq!(session.summary.updated_at, late.at);

        let ids: Vec<String> = store.list().unwrap().into_iter().map(|s| s.id).collect();
        assert_eq!(ids, [older.id.clone(), newer.id]);

        store.delete(&older.id).unwrap();
        assert!(matches!(store.load(&older.id), Err(Error::NotFound(_))));
        fs::remove_dir_all(repo).unwrap();
    }

    #[test]
    fn ids_that_are_not_ulids_never_touch_the_filesystem() {
        let store = Store::new(Path::new("/nonexistent"));
        assert!(matches!(
            store.load("../../etc/passwd"),
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    fn title_is_first_nonblank_line_capped() {
        assert_eq!(title_from("\n  Derivatives  \nmore"), "Derivatives");
        let long = "x".repeat(100);
        assert_eq!(title_from(&long).chars().count(), 81);
    }
}
