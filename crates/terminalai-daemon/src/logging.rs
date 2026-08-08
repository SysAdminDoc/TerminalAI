//! Where the daemon says what it did, and why none of it is unbounded.
//!
//! This process runs for as long as the operator's machine is up and owns every
//! supervised agent, so both obvious logging designs are wrong here. Writing to
//! one file forever fills a disk on a long-lived process; keeping everything in
//! memory for the GUI grows without limit in the process the whole fleet
//! depends on staying alive.
//!
//! So there are three sinks and each is bounded by construction:
//!
//! - **On disk**, fourteen daily files under `%LOCALAPPDATA%\TerminalAI\logs\`.
//!   Rotation by day rather than by size, because the question being answered
//!   is almost always "what happened when that session died", and that is a
//!   time, not an offset.
//! - **In memory**, a 256-record tail for the diagnostics panel. A tail rather
//!   than a full buffer: the GUI shows recent history, and a viewer that can
//!   scroll back forever is a memory leak with a scrollbar.
//! - **To the WebView**, batched no faster than every 100 ms. A chatty session
//!   can emit records faster than a browser can lay them out, and an unbatched
//!   stream turns a busy fleet into an unresponsive window.
//!
//! Records carry a session span — id, agent and working directory — so a line
//! can be attributed to the session that produced it rather than read as
//! whole-daemon noise.
//!
//! The `WorkerGuard` deliberately lives in the process entry point rather than
//! here. It flushes on drop, and a panic record is exactly the one worth having;
//! holding the guard in a shorter-lived scope would drop the appender before the
//! records that explain why the process is ending have been written.

use std::collections::{BTreeMap, VecDeque};
use std::fs::create_dir_all;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use terminalai_core::{LogEntry, MAX_LOG_ENTRIES};
use tracing::{span, Event, Subscriber};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;

pub const MAX_LOG_FILES: usize = 14;

#[derive(Clone, Default)]
pub struct LogHub {
    inner: Arc<Mutex<LogState>>,
}

#[derive(Default)]
struct LogState {
    entries: VecDeque<LogEntry>,
    subscribers: Vec<SyncSender<LogEntry>>,
}

impl LogHub {
    pub(crate) fn subscribe(&self) -> Receiver<LogEntry> {
        let (sender, receiver) = mpsc::sync_channel(MAX_LOG_ENTRIES);
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for entry in &state.entries {
            let _ = sender.try_send(entry.clone());
        }
        state.subscribers.push(sender);
        receiver
    }

    fn push(&self, entry: LogEntry) {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.entries.push_back(entry.clone());
        while state.entries.len() > MAX_LOG_ENTRIES {
            let _ = state.entries.pop_front();
        }
        state
            .subscribers
            .retain(|subscriber| match subscriber.try_send(entry.clone()) {
                Ok(()) | Err(TrySendError::Full(_)) => true,
                Err(TrySendError::Disconnected(_)) => false,
            });
    }
}

/// The file guard must live until the process is done so the nonblocking
/// writer drains its final records, including the panic hook's tail.
pub struct LoggingGuard {
    hub: LogHub,
    _worker_guard: WorkerGuard,
}

impl LoggingGuard {
    pub fn hub(&self) -> LogHub {
        self.hub.clone()
    }
}

pub fn log_directory() -> Option<PathBuf> {
    dirs::data_local_dir().map(|root| root.join("TerminalAI").join("logs"))
}

pub fn init_logging() -> Option<LoggingGuard> {
    init_logging_with_prefix("terminalai")
}

pub fn init_logging_with_prefix(prefix: &str) -> Option<LoggingGuard> {
    let directory = log_directory()?;
    if create_dir_all(&directory).is_err() {
        return None;
    }
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(prefix)
        .filename_suffix("log")
        .max_log_files(MAX_LOG_FILES)
        .build(&directory)
        .ok()?;
    let (writer, worker_guard) = tracing_appender::non_blocking(appender);
    let hub = LogHub::default();
    let subscriber = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(writer),
        )
        .with(HubLayer { hub: hub.clone() });
    tracing::subscriber::set_global_default(subscriber).ok()?;
    Some(LoggingGuard {
        hub,
        _worker_guard: worker_guard,
    })
}

struct HubLayer {
    hub: LogHub,
}

impl<S> Layer<S> for HubLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &span::Attributes<'_>, id: &span::Id, context: Context<'_, S>) {
        let mut visitor = FieldVisitor::default();
        attrs.record(&mut visitor);
        if let Some(span) = context.span(id) {
            span.extensions_mut().insert(SpanFields(visitor.fields));
        }
    }

    fn on_event(&self, event: &Event<'_>, context: Context<'_, S>) {
        let mut visitor = FieldVisitor::default();
        let mut fields = BTreeMap::new();
        if let Some(scope) = context.event_scope(event) {
            for span in scope {
                if let Some(span_fields) = span.extensions().get::<SpanFields>() {
                    fields.extend(span_fields.0.clone());
                }
            }
        }
        event.record(&mut visitor);
        let message = visitor.message.take().unwrap_or_default();
        fields.extend(visitor.fields);
        self.hub.push(LogEntry {
            at: SystemTime::now(),
            level: event.metadata().level().to_string(),
            target: event.metadata().target().to_owned(),
            message,
            fields,
        });
    }
}

struct SpanFields(BTreeMap<String, String>);

#[derive(Default)]
struct FieldVisitor {
    message: Option<String>,
    fields: BTreeMap<String, String>,
}

impl FieldVisitor {
    fn record_value(&mut self, name: &str, value: String) {
        let value = value.trim_matches('"').to_owned();
        if name == "message" {
            self.message = Some(value);
        } else {
            self.fields.insert(name.to_owned(), value);
        }
    }
}

impl tracing::field::Visit for FieldVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.record_value(field.name(), format!("{value:?}"));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.record_value(field.name(), value.to_owned());
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.record_value(field.name(), value.to_string());
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.record_value(field.name(), value.to_string());
    }

    fn record_i128(&mut self, field: &tracing::field::Field, value: i128) {
        self.record_value(field.name(), value.to_string());
    }

    fn record_u128(&mut self, field: &tracing::field::Field, value: u128) {
        self.record_value(field.name(), value.to_string());
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.record_value(field.name(), value.to_string());
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.record_value(field.name(), value.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_hub_replays_only_a_bounded_tail() {
        let hub = LogHub::default();
        for index in 0..(MAX_LOG_ENTRIES + 2) {
            hub.push(LogEntry {
                at: SystemTime::UNIX_EPOCH,
                level: "INFO".into(),
                target: "test".into(),
                message: index.to_string(),
                fields: BTreeMap::new(),
            });
        }
        let receiver = hub.subscribe();
        let entries: Vec<_> = receiver.try_iter().collect();
        assert_eq!(entries.len(), MAX_LOG_ENTRIES);
        assert_eq!(
            entries.first().map(|entry| entry.message.as_str()),
            Some("2")
        );
        assert_eq!(
            entries.last().map(|entry| entry.message.as_str()),
            Some("257")
        );
    }

    /// A hub wired to a real subscriber, so the layer is exercised by `tracing`
    /// rather than by hand. Scoped with `with_default` — the daemon's own
    /// `init_logging` sets a global subscriber, and a test that did the same
    /// could only ever run first.
    fn record(emit: impl FnOnce()) -> Vec<LogEntry> {
        let hub = LogHub::default();
        let receiver = hub.subscribe();
        let subscriber = tracing_subscriber::registry().with(HubLayer { hub: hub.clone() });
        tracing::subscriber::with_default(subscriber, emit);
        receiver.try_iter().collect()
    }

    #[test]
    fn a_record_carries_the_session_span_that_produced_it() {
        // The whole reason the layer walks the span scope: without it every
        // line in a fourteen-session fleet reads as whole-daemon noise, and the
        // question being asked is almost always about one session.
        let entries = record(|| {
            let outer = tracing::info_span!("session", session_id = "s0007", agent = "claude");
            let _outer = outer.enter();
            let inner = tracing::info_span!("launch", attempt = 2);
            let _inner = inner.enter();
            tracing::warn!(exit_code = 1, "agent exited");
        });

        assert_eq!(entries.len(), 1, "one event, one record");
        let entry = &entries[0];
        assert_eq!(entry.message, "agent exited");
        assert_eq!(entry.level, "WARN");
        assert_eq!(entry.fields.get("session_id").map(String::as_str), Some("s0007"));
        assert_eq!(entry.fields.get("agent").map(String::as_str), Some("claude"));
        assert_eq!(entry.fields.get("attempt").map(String::as_str), Some("2"));
        assert_eq!(entry.fields.get("exit_code").map(String::as_str), Some("1"));
        // The message is the message, not a field as well. It is rendered on
        // its own line in the diagnostics panel, and a duplicate copy in the
        // field list is noise on every record the daemon writes.
        assert!(
            !entry.fields.contains_key("message"),
            "the message was duplicated into the fields: {:?}",
            entry.fields
        );
    }

    #[test]
    fn a_record_outside_any_span_carries_no_borrowed_fields() {
        // The counterpart to the test above: span fields must come from the
        // event's own scope, not from whatever span happened to be open last.
        let entries = record(|| {
            {
                let span = tracing::info_span!("session", session_id = "s0001");
                let _entered = span.enter();
                tracing::info!("inside");
            }
            tracing::info!("outside");
        });

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].fields.get("session_id").map(String::as_str), Some("s0001"));
        assert!(
            entries[1].fields.is_empty(),
            "a record outside the span borrowed its fields: {:?}",
            entries[1].fields
        );
    }

    #[test]
    fn every_field_type_reaches_the_panel_as_a_readable_string() {
        // The GUI renders strings. Each of these arrives through a different
        // visitor method, and one that is missing does not fail loudly — the
        // field simply never appears on the record.
        let entries = record(|| {
            tracing::info!(
                count = 3u64,
                delta = -4i64,
                ratio = 1.5f64,
                enabled = true,
                name = "claude",
                path = ?std::path::PathBuf::from("repo"),
                "typed"
            );
        });

        let fields = &entries[0].fields;
        assert_eq!(fields.get("count").map(String::as_str), Some("3"));
        assert_eq!(fields.get("delta").map(String::as_str), Some("-4"));
        assert_eq!(fields.get("ratio").map(String::as_str), Some("1.5"));
        assert_eq!(fields.get("enabled").map(String::as_str), Some("true"));
        assert_eq!(fields.get("name").map(String::as_str), Some("claude"));
        // Debug-formatted values arrive quoted; the panel shows the value, not
        // the quoting.
        assert_eq!(fields.get("path").map(String::as_str), Some("repo"));
    }

    #[test]
    fn a_closed_viewer_is_dropped_and_a_slow_one_is_kept() {
        // Two different failures. A GUI that closed and is never forgotten
        // leaks a channel per window for the life of the daemon; a GUI that is
        // merely behind and gets unsubscribed goes silent for good, which looks
        // exactly like a daemon that stopped logging.
        let hub = LogHub::default();
        let closed = hub.subscribe();
        let slow = hub.subscribe();
        assert_eq!(subscriber_count(&hub), 2);

        drop(closed);
        hub.push(entry("first"));
        assert_eq!(
            subscriber_count(&hub),
            1,
            "a closed viewer was kept and its channel with it"
        );

        // Fill the slow viewer's channel well past its capacity without reading.
        for index in 0..(MAX_LOG_ENTRIES * 2) {
            hub.push(entry(&index.to_string()));
        }
        assert_eq!(
            subscriber_count(&hub),
            1,
            "a viewer that fell behind was unsubscribed instead of throttled"
        );

        // And it still receives what is pushed once it catches up.
        let backlog: Vec<_> = slow.try_iter().collect();
        assert_eq!(backlog.len(), MAX_LOG_ENTRIES, "the queue is bounded");
        hub.push(entry("after"));
        assert_eq!(
            slow.try_iter().last().map(|entry| entry.message),
            Some("after".to_owned())
        );
    }

    fn subscriber_count(hub: &LogHub) -> usize {
        hub.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .subscribers
            .len()
    }

    fn entry(message: &str) -> LogEntry {
        LogEntry {
            at: SystemTime::UNIX_EPOCH,
            level: "INFO".into(),
            target: "test".into(),
            message: message.to_owned(),
            fields: BTreeMap::new(),
        }
    }
}
