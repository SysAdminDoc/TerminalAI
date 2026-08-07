//! What arrives from a session's pty: the in-memory ring, the last line, and
//! handing the same bytes to the disk tier.
//!
//! Split out of `mod.rs` for size. The lock discipline is the part to preserve:
//! the spool takes its own lock and never the state lock, so the two can be held
//! in either order without a cycle.

use super::*;
pub(super) fn handle_output(inner: &Arc<Inner>, id: &SessionId, generation: u64, bytes: &[u8]) {
    let (send_output, session, notifications) = {
        let mut state = lock_state(inner);
        let focused = state.focused.as_ref() == Some(id);
        let clear_operator_edit = state.entries.get(id).is_some_and(|entry| {
            entry.generation == generation && entry.session.status != SessionStatus::Idle
        });
        if clear_operator_edit {
            state.operator_edited.remove(id);
        }
        let Some(entry) = state.entries.get_mut(id) else {
            return;
        };
        if entry.generation != generation {
            return;
        }
        entry.scrollback.push(bytes);
        entry.grid.advance(bytes);
        // Any byte at all is evidence the process is alive, whether or not it
        // moves the status. A session can hold `Working` for an hour while
        // printing a build log the whole way, and nothing else on this path
        // would tell the supervisor the difference between that and a wedge.
        entry.session.note_progress_at(SystemTime::now());
        // Queued, never written here: this runs on the pty reader thread with
        // the state lock held, so a blocking write would stall every other
        // session and back-pressure the agent that produced the bytes.
        spool_append(inner, id, bytes);
        if let Some(line) = entry.scrollback.last_line() {
            entry.session.set_last_line(&line);
        }
        let previous_status = entry.session.status;
        let previous_state_since = entry.session.state_since;
        if entry.session.status == SessionStatus::Starting {
            entry
                .session
                .set_status_from(SessionStatus::Idle, StatusSource::PtyOutput);
        }
        // PTY output also contains terminal-local echo. Only output observed
        // while the provider is in a non-idle state is strong enough evidence
        // to clear the operator-input guard.
        if clear_operator_edit {
            entry.queue.clear_focus_edit();
            entry.session.queued_prompts = entry.queue.len();
            entry.session.queue_paused = entry.queue.paused();
        }
        let session = entry.session.clone();
        let notifications = if previous_status != session.status {
            state.notifications.observe(
                &session,
                previous_status,
                previous_state_since,
                SystemTime::now(),
            )
        } else {
            Vec::new()
        };
        (focused || session.pinned, session, notifications)
    };
    if send_output {
        emit_inner(
            inner,
            RegistryEvent::Output {
                id: id.clone(),
                data: bytes.to_vec(),
            },
        );
    }
    emit_inner(inner, RegistryEvent::SessionUpdated {
                session: Box::new(session),
            });
    emit_notification_changes_inner(inner, notifications);
}

/// Hand bytes to the disk tier if one is attached.
///
/// Takes its own lock rather than the state lock, and never takes the state
/// lock, so the two can be held in either order without a cycle.
pub(super) fn spool_append(inner: &Arc<Inner>, id: &SessionId, bytes: &[u8]) {
    if let Some(spool) = inner.spool() {
        spool.append(id, bytes);
    }
}

pub(super) fn spool_forget(inner: &Arc<Inner>, id: &SessionId) {
    if let Some(spool) = inner.spool() {
        spool.forget(id);
    }
}

#[derive(Default)]
pub(super) struct RingBuffer {
    bytes: VecDeque<u8>,
}

impl RingBuffer {
    pub(super) fn push(&mut self, bytes: &[u8]) {
        if bytes.len() >= MAX_SCROLLBACK_BYTES {
            self.bytes.clear();
            self.bytes
                .extend(bytes[bytes.len() - MAX_SCROLLBACK_BYTES..].iter().copied());
            return;
        }
        self.bytes.extend(bytes.iter().copied());
        while self.bytes.len() > MAX_SCROLLBACK_BYTES {
            let _ = self.bytes.pop_front();
        }
    }

    pub(super) fn to_vec(&self) -> Vec<u8> {
        self.bytes.iter().copied().collect()
    }

    pub(super) fn last_line(&self) -> Option<String> {
        let mut candidate_reversed = Vec::with_capacity(MAX_LAST_LINE_BYTES.min(self.bytes.len()));
        for byte in self.bytes.iter().rev().take(MAX_LAST_LINE_BYTES) {
            if matches!(byte, b'\r' | b'\n') {
                if let Some(line) = decode_last_line_candidate(&mut candidate_reversed) {
                    return Some(line);
                }
            } else {
                candidate_reversed.push(*byte);
            }
        }
        decode_last_line_candidate(&mut candidate_reversed)
    }
}

fn decode_last_line_candidate(candidate_reversed: &mut Vec<u8>) -> Option<String> {
    if candidate_reversed.is_empty() {
        return None;
    }
    candidate_reversed.reverse();
    let text = String::from_utf8_lossy(candidate_reversed);
    let line = (!text.trim().is_empty()).then(|| text.into_owned());
    candidate_reversed.clear();
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_keeps_a_bounded_tail() {
        let mut ring = RingBuffer::default();
        ring.push(&vec![b'x'; MAX_SCROLLBACK_BYTES + 10]);
        assert_eq!(ring.to_vec().len(), MAX_SCROLLBACK_BYTES);
    }

    #[test]
    fn ring_buffer_finds_last_non_empty_line() {
        let mut ring = RingBuffer::default();
        ring.push(b"first\r\nlast\r\n");
        assert_eq!(ring.last_line().as_deref(), Some("last"));
    }

    #[test]
    fn ring_buffer_finds_last_line_across_chunks() {
        let mut ring = RingBuffer::default();
        ring.push(b"first\r");
        ring.push(b"\npart");
        ring.push(b"ial");
        assert_eq!(ring.last_line().as_deref(), Some("partial"));
    }

    #[test]
    fn ring_buffer_finds_partial_line_without_a_newline() {
        let mut ring = RingBuffer::default();
        ring.push(b"still typing");
        assert_eq!(ring.last_line().as_deref(), Some("still typing"));
    }

    #[test]
    fn ring_buffer_bounds_last_line_scan() {
        let mut ring = RingBuffer::default();
        ring.push(&vec![b'x'; MAX_LAST_LINE_BYTES + 1]);
        assert_eq!(
            ring.last_line().as_deref(),
            Some("x".repeat(MAX_LAST_LINE_BYTES).as_str())
        );
    }
    
    
    
    
    
    use crate::registry::testing::*;
    
    
    

    #[test]
    fn history_reaches_past_what_the_memory_ring_kept() {
        // The reason the disk tier exists. The ring is deliberately small, so
        // an agent that produced more than a megabyte of output has already
        // lost the beginning by the time anyone asks.
        let dir = spool_scratch("past-ring");
        let registry = SessionRegistry::new();
        let id = SessionId::new(1);
        insert_session(&registry, &id, SessionStatus::Working);
        registry.set_scrollback_spool(Arc::new(
            ScrollbackSpool::new(&dir.0).expect("spool"),
        ));

        feed(&registry, &id, &vec![b'x'; 500 * 1024]);
        let marker = b"THE FIRST THING IT SAID\n";
        feed(&registry, &id, marker);
        feed(&registry, &id, &vec![b'y'; 600 * 1024]);
        registry.inner.spool().expect("spool").flush();

        let ring = registry.scrollback(&id).expect("ring");
        assert!(
            !ring.windows(marker.len()).any(|window| window == marker),
            "the ring is supposed to have dropped the older marker"
        );
        let request_bytes = MAX_SCROLLBACK_BYTES as u64 + 128 * 1024;
        let history = registry
            .scrollback_history(&id, request_bytes)
            .expect("history");
        assert!(history.len() > MAX_SCROLLBACK_BYTES, "history did not reach past the ring");
        let text = String::from_utf8_lossy(&history);
        assert!(text.contains("THE FIRST THING IT SAID"), "history lost the older output");
    }
    #[test]
    fn without_a_spool_history_falls_back_to_the_ring() {
        // The in-process app server and every test construct a registry with no
        // disk tier. Asking for history there must answer with what exists
        // rather than nothing.
        let registry = SessionRegistry::new();
        let id = SessionId::new(1);
        insert_session(&registry, &id, SessionStatus::Working);
        feed(&registry, &id, b"only in memory
");
        let history = registry.scrollback_history(&id, 1024).expect("history");
        assert_eq!(history, b"only in memory
".to_vec());
    }

    #[test]
    fn history_for_an_unknown_session_is_an_error_not_an_empty_answer() {
        // An empty answer would read as "this session produced nothing".
        let registry = SessionRegistry::new();
        assert!(registry.scrollback_history(&SessionId::new(7), 1024).is_err());
    }

    #[test]
    fn the_store_stops_carrying_output_once_a_log_owns_it() {
        // Persistence rewrites the whole store on a debounce. Copying every
        // session's ring into it once a second duplicated bytes the spool had
        // already appended, and was the most expensive thing it did.
        let dir = spool_scratch("store-free");
        let registry = SessionRegistry::new();
        let id = SessionId::new(1);
        insert_session(&registry, &id, SessionStatus::Working);
        feed(&registry, &id, b"output
");
        assert_eq!(
            registry.store_snapshot().sessions[0].scrollback,
            b"output
".to_vec(),
            "with no log, the store is the only durable copy"
        );

        registry.set_scrollback_spool(Arc::new(
            ScrollbackSpool::new(&dir.0).expect("spool"),
        ));
        assert!(
            registry.store_snapshot().sessions[0].scrollback.is_empty(),
            "the store is still carrying bytes the log owns"
        );
    }

    #[test]
    fn a_restarted_registry_replays_its_ring_from_the_log() {
        // Because the store no longer carries output, this is the only thing
        // that puts a restored session's last screenful back in front of the
        // operator. Without it, restarting the daemon would blank every pane.
        let dir = spool_scratch("rehydrate");
        let first = SessionRegistry::new();
        let id = SessionId::new(1);
        insert_session(&first, &id, SessionStatus::Working);
        first.set_scrollback_spool(Arc::new(ScrollbackSpool::new(&dir.0).expect("spool")));
        feed(&first, &id, b"said before the restart
");
        first.inner.spool().expect("spool").flush();
        let stored = first.store_snapshot();
        drop(first);

        let restarted = SessionRegistry::from_store(stored);
        assert!(
            restarted.scrollback(&id).expect("ring").is_empty(),
            "nothing should have come from the store"
        );
        restarted.set_scrollback_spool(Arc::new(ScrollbackSpool::new(&dir.0).expect("spool")));
        let ring = restarted.scrollback(&id).expect("ring");
        assert_eq!(ring, b"said before the restart
".to_vec());
        // The grid is replayed too, or a pinned pane restores blank while the
        // focused one has content.
        let grid = restarted.grid_snapshot(&id).expect("grid");
        assert!(
            grid.lines.iter().any(|line| line.contains("before the restart")),
            "the grid was not replayed"
        );
    }

    #[test]
    fn removing_a_session_stops_it_paying_for_disk() {
        let dir = spool_scratch("forget");
        let registry = SessionRegistry::new();
        let id = SessionId::new(1);
        insert_session(&registry, &id, SessionStatus::Exited);
        registry.set_scrollback_spool(Arc::new(ScrollbackSpool::new(&dir.0).expect("spool")));
        feed(&registry, &id, b"some output
");
        let spool = registry.inner.spool().expect("spool");
        spool.flush();
        assert!(!spool.history(&id, 1024).is_empty());

        registry.archive(&id).expect("archive");
        spool.flush();
        assert!(
            spool.history(&id, 1024).is_empty(),
            "an archived row kept its history on disk"
        );
    }
}
