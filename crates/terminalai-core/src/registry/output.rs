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
}
