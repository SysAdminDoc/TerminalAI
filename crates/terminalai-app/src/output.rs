//! Tauri output-channel routing for focused terminal panes.
//!
//! A channel is registered per session while the browser attaches to it. The
//! daemon can emit bytes during the replay window, so the route buffers those
//! bytes and removes only the part already covered by the replay before live
//! output is released.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tauri::ipc::{Channel, InvokeResponseBody};
use terminalai_core::SessionId;

pub(crate) type OutputChannels = Arc<Mutex<HashMap<SessionId, Arc<OutputRoute>>>>;

pub(crate) struct OutputRoute {
    channel: Channel<InvokeResponseBody>,
    state: Mutex<OutputRouteState>,
}

struct OutputRouteState {
    replaying: bool,
    pending: Vec<u8>,
}

impl OutputRoute {
    pub(crate) fn new(channel: Channel<InvokeResponseBody>, replaying: bool) -> Self {
        Self {
            channel,
            state: Mutex::new(OutputRouteState {
                replaying,
                pending: Vec::new(),
            }),
        }
    }

    pub(crate) fn queue(&self, data: Vec<u8>) -> Result<(), String> {
        self.state
            .lock()
            .map_err(|_| "output route is poisoned".to_string())?
            .pending
            .extend(data);
        Ok(())
    }

    pub(crate) fn flush(&self) -> Result<(), String> {
        let data = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "output route is poisoned".to_string())?;
            if state.replaying || state.pending.is_empty() {
                return Ok(());
            }
            std::mem::take(&mut state.pending)
        };
        self.channel
            .send(InvokeResponseBody::Raw(data))
            .map_err(|error| format!("send terminal bytes: {error}"))
    }

    pub(crate) fn complete_replay(&self, replay: Vec<u8>) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "output route is poisoned".to_string())?;
        let pending = std::mem::take(&mut state.pending);
        let pending_start = replay_overlap(&replay, &pending);
        self.channel
            .send(InvokeResponseBody::Raw(replay))
            .map_err(|error| format!("send terminal replay: {error}"))?;
        if pending_start < pending.len() {
            self.channel
                .send(InvokeResponseBody::Raw(pending[pending_start..].to_vec()))
                .map_err(|error| format!("send terminal output after replay: {error}"))?;
        }
        state.replaying = false;
        Ok(())
    }
}

/// Return the number of buffered bytes already covered by the replay.
///
/// Normally the buffered stream starts with a suffix of the ring. Searching
/// for the longest matching suffix also handles a very long RPC where the ring
/// has already dropped the oldest part of the buffered stream: those bytes
/// must still be discarded because the replay is the pane's reset point.
pub(crate) fn replay_overlap(replay: &[u8], pending: &[u8]) -> usize {
    let mut best_length = 0;
    let mut best_end = 0;
    for start in 0..pending.len() {
        let max_length = replay.len().min(pending.len() - start);
        for length in (1..=max_length).rev() {
            if replay[replay.len() - length..] == pending[start..start + length] {
                let end = start + length;
                if length > best_length || (length == best_length && end < best_end) {
                    best_length = length;
                    best_end = end;
                }
                break;
            }
        }
    }
    best_end
}

pub(crate) fn register_output_channel(
    id: SessionId,
    channel: Channel<InvokeResponseBody>,
    channels: &OutputChannels,
    replaying: bool,
) -> Result<Arc<OutputRoute>, String> {
    let mut channels = channels
        .lock()
        .map_err(|_| "output channel registry is poisoned".to_string())?;
    let route = Arc::new(OutputRoute::new(channel, replaying));
    channels.retain(|session_id, _| session_id == &id);
    channels.insert(id, route.clone());
    Ok(route)
}

pub(crate) fn remove_output_route(
    id: &SessionId,
    route: &Arc<OutputRoute>,
    channels: &OutputChannels,
) {
    if let Ok(mut channels) = channels.lock() {
        if channels
            .get(id)
            .is_some_and(|current| Arc::ptr_eq(current, route))
        {
            channels.remove(id);
        }
    }
}

pub(crate) fn send_raw(channel: &Channel<InvokeResponseBody>, data: Vec<u8>) -> Result<(), String> {
    channel
        .send(InvokeResponseBody::Raw(data))
        .map_err(|error| format!("send terminal bytes: {error}"))
}

#[cfg(test)]
mod tests {
    use super::replay_overlap;

    #[test]
    fn replay_overlap_drops_only_bytes_already_in_the_replay() {
        assert_eq!(
            replay_overlap(b"prompt\r\noutput> ", b"output> next\r\n"),
            8
        );
        assert_eq!(replay_overlap(b"abc", b"xyz"), 0);
    }
}
