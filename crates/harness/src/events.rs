//! Events from the agent runtime to the UI.
//!
//! Turns and jobs run in the background (a job can wake the tutor long after
//! the command that started it returned), so the runtime emits through one
//! process-wide sink instead of a per-command callback. [`Coalescer`] batches
//! text deltas (build prompt M7.4): it flushes every ~16 ms or 2 KB, and
//! always before any other event so ordering is preserved.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::SessionSummary;

/// Which agent an event belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentName {
    Tutor,
    Planner,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AgentEvent {
    /// A session was created or retitled. Boxed: a summary with lineage is
    /// far larger than every other event.
    Session {
        session: Box<SessionSummary>,
    },
    TurnStart {
        agent: AgentName,
    },
    Text {
        agent: AgentName,
        delta: String,
    },
    Thinking {
        agent: AgentName,
        delta: String,
    },
    /// An op was persisted (serialized `Op`).
    Op {
        op: serde_json::Value,
    },
    TurnEnd {
        agent: AgentName,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Sync changed files in the data repo (another device's work arrived):
    /// session lists and open sessions should be re-read. Sent with an empty
    /// session id.
    RepoChanged,
}

/// Receives `(session_id, event)`. Must not block.
pub type EventSink = Arc<dyn Fn(&str, AgentEvent) + Send + Sync>;

/// Payload sent over IPC: the event plus its session id.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Envelope<'a> {
    pub session_id: &'a str,
    #[serde(flatten)]
    pub event: &'a AgentEvent,
}

const FLUSH_BYTES: usize = 2048;
const FLUSH_EVERY: Duration = Duration::from_millis(16);

struct Pending {
    session: String,
    agent: AgentName,
    thinking: bool,
    text: String,
    since: Instant,
}

/// Batches text/thinking deltas in front of an output sink.
pub struct Coalescer {
    out: EventSink,
    pending: Mutex<Option<Pending>>,
}

impl Coalescer {
    pub fn new(out: EventSink) -> Arc<Self> {
        Arc::new(Self {
            out,
            pending: Mutex::new(None),
        })
    }

    /// Use as the runtime's sink.
    pub fn sink(self: &Arc<Self>) -> EventSink {
        let me = Arc::clone(self);
        Arc::new(move |session, event| me.push(session, event))
    }

    pub fn push(&self, session: &str, event: AgentEvent) {
        let (agent, thinking, delta) = match event {
            AgentEvent::Text { agent, delta } => (agent, false, delta),
            AgentEvent::Thinking { agent, delta } => (agent, true, delta),
            other => {
                self.flush();
                (self.out)(session, other);
                return;
            }
        };
        let ready = {
            let mut guard = lock(&self.pending);
            let same = guard.as_ref().is_some_and(|p| {
                p.session == session && p.agent == agent && p.thinking == thinking
            });
            let previous = if same { None } else { guard.take() };
            let p = guard.get_or_insert_with(|| Pending {
                session: session.to_owned(),
                agent,
                thinking,
                text: String::new(),
                since: Instant::now(),
            });
            p.text.push_str(&delta);
            let due = p.text.len() >= FLUSH_BYTES || p.since.elapsed() >= FLUSH_EVERY;
            (previous, if due { guard.take() } else { None })
        };
        if let Some(p) = ready.0 {
            self.emit(p);
        }
        if let Some(p) = ready.1 {
            self.emit(p);
        }
    }

    /// Flush whatever is buffered (call on a ~16 ms timer and at turn end).
    pub fn flush(&self) {
        let taken = lock(&self.pending).take();
        if let Some(p) = taken {
            self.emit(p);
        }
    }

    /// Flush only if the buffer is older than the flush interval.
    pub fn flush_due(&self) {
        let due = lock(&self.pending)
            .as_ref()
            .is_some_and(|p| p.since.elapsed() >= FLUSH_EVERY);
        if due {
            self.flush();
        }
    }

    fn emit(&self, p: Pending) {
        let event = if p.thinking {
            AgentEvent::Thinking {
                agent: p.agent,
                delta: p.text,
            }
        } else {
            AgentEvent::Text {
                agent: p.agent,
                delta: p.text,
            }
        };
        (self.out)(&p.session, event);
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    type Seen = Arc<Mutex<Vec<(String, AgentEvent)>>>;

    fn collect() -> (EventSink, Seen) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = Arc::clone(&seen);
        let sink: EventSink = Arc::new(move |id, ev| lock(&s).push((id.to_owned(), ev)));
        (sink, seen)
    }

    fn text(delta: &str) -> AgentEvent {
        AgentEvent::Text {
            agent: AgentName::Tutor,
            delta: delta.into(),
        }
    }

    #[test]
    fn deltas_batch_and_other_events_flush_first_in_order() {
        let (sink, seen) = collect();
        let c = Coalescer::new(sink);
        for _ in 0..100 {
            c.push("s", text("ab"));
        }
        c.push(
            "s",
            AgentEvent::TurnEnd {
                agent: AgentName::Tutor,
                error: None,
            },
        );
        let seen = lock(&seen);
        // Far fewer events than deltas, all text before turnEnd, no text lost.
        assert!(seen.len() < 10, "{} events", seen.len());
        let joined: String = seen
            .iter()
            .filter_map(|(_, e)| match e {
                AgentEvent::Text { delta, .. } => Some(delta.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(joined, "ab".repeat(100));
        assert!(matches!(seen.last().unwrap().1, AgentEvent::TurnEnd { .. }));
    }

    #[test]
    fn two_kib_flushes_immediately_and_switching_stream_flushes() {
        let (sink, seen) = collect();
        let c = Coalescer::new(sink);
        c.push("s", text(&"x".repeat(2048)));
        assert_eq!(lock(&seen).len(), 1);
        c.push("s", text("a"));
        c.push(
            "s",
            AgentEvent::Thinking {
                agent: AgentName::Tutor,
                delta: "t".into(),
            },
        );
        assert_eq!(
            lock(&seen).len(),
            2,
            "the text batch flushed before thinking"
        );
        c.flush();
        assert_eq!(lock(&seen).len(), 3);
    }
}
