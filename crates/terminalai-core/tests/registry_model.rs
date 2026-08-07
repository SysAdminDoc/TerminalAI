//! A reference model of the fleet, run alongside the real registry.
//!
//! The example-based suite is structurally blind to the failure this program is
//! most exposed to: an *ordering* of individually legal operations whose end
//! state diverges from what it should be. Archiving the focused row, pinning a
//! row that is then archived, persisting and reloading in between — each step is
//! covered on its own, and no test says what the combination must produce.
//!
//! The model here has no threads, no pty, no disk and no clock: it is a map of
//! ids to four booleans and a counter, plus the archive list. Every generated
//! operation is applied to both, and every field the operator can see is
//! compared after each one. A divergence shrinks to the shortest sequence that
//! still produces it.
//!
//! **Scope: the deterministic surface only.** Rows are seeded through
//! `from_store`, which starts no process, and nothing here calls `launch` or
//! `kill`. Spawning is asynchronous — `prepare_and_start` runs off the caller's
//! thread on purpose — so a model that included it would have to wait for
//! quiescence between operations, which is how a state-machine test turns into
//! a sleep-based one that fails under load. Process lifetime is covered by the
//! example tests that drive real processes; what is covered here is the state
//! the daemon persists and restores.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use proptest::prelude::*;
use proptest::test_runner::Config;
use proptest_state_machine::{prop_state_machine, ReferenceStateMachine, StateMachineTest};

use terminalai_core::agent::Agent;
use terminalai_core::land::Landing;
use terminalai_core::launch::{spec_for, ResolvedCommand};
use terminalai_core::registry::{RegistryError, SessionRegistry};
use terminalai_core::session::{Session, SessionId};
use terminalai_core::store::{SessionStoreSnapshot, StoredSession, MAX_ARCHIVES};

/// The fleet's pin limit. Not exported, so it is restated here — and the model
/// is what proves the two agree: a fourth pin has to be refused in both.
const MAX_PINNED: usize = 3;

/// How many rows the generated fleet starts with. Small on purpose: the bugs
/// this is looking for are about the *order* operations land in, and a shrunk
/// counterexample over three rows is one a person can read.
const FLEET: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    pinned: bool,
    unread: bool,
    queued: usize,
    landed: bool,
}

impl Row {
    fn new() -> Self {
        Self {
            pinned: false,
            unread: false,
            queued: 0,
            landed: false,
        }
    }
}

#[derive(Debug, Clone)]
struct Model {
    live: BTreeMap<SessionId, Row>,
    /// Archive order, newest last, deduplicated by id — the same contract the
    /// store's own list has.
    archived: Vec<SessionId>,
    focused: Option<SessionId>,
}

impl Model {
    fn ids(&self) -> Vec<SessionId> {
        self.live.keys().cloned().collect()
    }
}

#[derive(Debug, Clone)]
enum Op {
    /// Archive the nth live row.
    Archive(usize),
    /// Flip the nth live row's pin.
    TogglePin(usize),
    /// Focus the nth live row, or nothing.
    Focus(Option<usize>),
    /// Clear the nth live row's unread mark.
    MarkRead(usize),
    /// Record a landing against the nth live row.
    RecordLanding(usize),
    /// Add a prompt to the nth live row's queue.
    Enqueue(usize),
    /// Persist the whole fleet and load it back into a fresh registry. The
    /// operator does this every time the daemon restarts, and it is the step
    /// most likely to quietly drop a field nobody serialized.
    RoundTrip,
}

struct FleetModel;

impl ReferenceStateMachine for FleetModel {
    type State = Model;
    type Transition = Op;

    fn init_state() -> BoxedStrategy<Self::State> {
        Just(Model {
            live: (1..=FLEET as u64)
                .map(|id| (SessionId::new(id), Row::new()))
                .collect(),
            archived: Vec::new(),
            focused: None,
        })
        .boxed()
    }

    fn transitions(state: &Self::State) -> BoxedStrategy<Self::Transition> {
        let live = state.live.len().max(1);
        prop_oneof![
            2 => (0..live).prop_map(Op::Archive),
            3 => (0..live).prop_map(Op::TogglePin),
            3 => proptest::option::of(0..live).prop_map(Op::Focus),
            2 => (0..live).prop_map(Op::MarkRead),
            2 => (0..live).prop_map(Op::RecordLanding),
            3 => (0..live).prop_map(Op::Enqueue),
            3 => Just(Op::RoundTrip),
        ]
        .boxed()
    }

    /// An index past the end of the fleet is not a transition worth generating:
    /// it exercises the error path, which the example tests already pin, and it
    /// would make every shrunk counterexample noisier.
    fn preconditions(state: &Self::State, transition: &Self::Transition) -> bool {
        let within = |index: &usize| *index < state.live.len();
        match transition {
            Op::Archive(index)
            | Op::TogglePin(index)
            | Op::MarkRead(index)
            | Op::RecordLanding(index)
            | Op::Enqueue(index) => within(index),
            Op::Focus(Some(index)) => within(index),
            Op::Focus(None) | Op::RoundTrip => true,
        }
    }

    fn apply(mut state: Self::State, transition: &Self::Transition) -> Self::State {
        let ids = state.ids();
        match transition {
            Op::Archive(index) => {
                let id = ids[*index].clone();
                state.live.remove(&id);
                // Archiving the focused row leaves the fleet with nothing
                // focused rather than a dangling id.
                if state.focused.as_ref() == Some(&id) {
                    state.focused = None;
                }
                state.archived.retain(|archived| archived != &id);
                state.archived.push(id);
                if state.archived.len() > MAX_ARCHIVES {
                    let excess = state.archived.len() - MAX_ARCHIVES;
                    state.archived.drain(..excess);
                }
            }
            Op::TogglePin(index) => {
                // Unpinning always works. Pinning a fourth row is refused, and
                // the refusal must leave the row exactly as it was rather than
                // half-applying.
                let pinned = state.live.values().filter(|row| row.pinned).count();
                let row = state.live.get_mut(&ids[*index]).expect("live row");
                if row.pinned || pinned < MAX_PINNED {
                    row.pinned = !row.pinned;
                }
            }
            Op::Focus(target) => {
                state.focused = target.map(|index| ids[index].clone());
            }
            Op::MarkRead(index) => {
                state.live.get_mut(&ids[*index]).expect("live row").unread = false;
            }
            Op::RecordLanding(index) => {
                state.live.get_mut(&ids[*index]).expect("live row").landed = true;
            }
            Op::Enqueue(index) => {
                state.live.get_mut(&ids[*index]).expect("live row").queued += 1;
            }
            // A round trip is invisible to every row field. Saying so in the
            // model — rather than leaving it out — is what makes a field that
            // fails to survive persistence a failure rather than an omission.
            //
            // Focus is the one exception, and it is deliberate: the store is
            // what the daemon restores with no window attached, and focus is a
            // property of an attached view. The renderer priority and output
            // stream that `focus` sets up would be pointed at nobody. The UI
            // agrees — `state.focused` starts `null` on every load and is set
            // by the operator's next click. Asserted on its own in
            // `focus_is_not_part_of_what_the_daemon_persists`.
            Op::RoundTrip => state.focused = None,
        }
        state
    }
}

struct Fleet;

fn stored(id: &SessionId) -> StoredSession {
    let cwd = PathBuf::from(".");
    let spec = spec_for(Agent::Claude, &cwd);
    StoredSession {
        session: Session::new(id.clone(), &spec),
        spec,
        command: ResolvedCommand {
            program: PathBuf::from("claude.exe"),
            args: Vec::new(),
            cwd,
        },
        scrollback: Vec::new(),
        queue: Default::default(),
    }
}

fn a_landing() -> Landing {
    Landing {
        at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        target: PathBuf::from("C:/repos/project"),
        target_head: "abc1234".into(),
        files_changed: 3,
        verified: Some(true),
    }
}

impl StateMachineTest for Fleet {
    type SystemUnderTest = SessionRegistry;
    type Reference = FleetModel;

    fn init_test(reference: &Model) -> Self::SystemUnderTest {
        SessionRegistry::from_store(SessionStoreSnapshot {
            sessions: reference.live.keys().map(stored).collect(),
            ..SessionStoreSnapshot::default()
        })
    }

    fn apply(registry: Self::SystemUnderTest, reference: &Model, transition: Op) -> Self::SystemUnderTest {
        // The model applies the transition to the state *before* it, so the ids
        // an index refers to are the ones the registry still holds here.
        //
        // Sorted by id, which is how the model's `BTreeMap` orders them.
        // `snapshot()` returns fleet order — attention first, then
        // longest-waiting — so indexing it directly would have the two sides
        // pick different rows for the same operation. That is a bug in the
        // harness rather than in the fleet, and it is what the first run found.
        let mut ids: Vec<SessionId> = registry
            .snapshot()
            .into_iter()
            .map(|session| session.id)
            .collect();
        ids.sort();
        match transition {
            Op::Archive(index) => {
                registry.archive(&ids[index]).expect("archive a stopped row");
            }
            Op::TogglePin(index) => {
                // The pin limit is a rule, so the refusal is asserted rather
                // than tolerated: the model already knows which way this call
                // must go, and a registry that pinned a fourth row — or refused
                // a third — is the divergence worth catching.
                let expected = reference
                    .live
                    .get(&ids[index])
                    .expect("the model kept this row")
                    .pinned;
                match registry.toggle_pin(&ids[index]) {
                    Ok(pinned) => assert_eq!(
                        pinned, expected,
                        "toggle_pin reported a state the model did not expect"
                    ),
                    Err(RegistryError::PinLimit) => assert!(
                        !expected,
                        "the fleet refused a pin the model says is allowed"
                    ),
                    Err(error) => panic!("toggle pin: {error}"),
                }
            }
            Op::Focus(target) => {
                registry
                    .focus(target.map(|index| ids[index].clone()))
                    .expect("focus");
            }
            Op::MarkRead(index) => {
                registry.mark_read(&ids[index]).expect("mark read");
            }
            Op::RecordLanding(index) => {
                registry
                    .record_landing(&ids[index], a_landing())
                    .expect("record landing");
            }
            Op::Enqueue(index) => {
                registry
                    .enqueue_prompt(&ids[index], "do the thing")
                    .expect("enqueue");
            }
            Op::RoundTrip => {
                let restored = SessionRegistry::from_store(registry.store_snapshot());
                registry.shutdown();
                return restored;
            }
        }
        registry
    }

    fn check_invariants(registry: &Self::SystemUnderTest, reference: &Model) {
        let rows = registry.snapshot();
        let live: BTreeMap<SessionId, Row> = rows
            .iter()
            .map(|session| {
                (
                    session.id.clone(),
                    Row {
                        pinned: session.pinned,
                        unread: session.unread,
                        queued: session.queued_prompts,
                        landed: session.landed.is_some(),
                    },
                )
            })
            .collect();
        assert_eq!(live, reference.live, "live rows diverged");

        assert_eq!(registry.focused(), reference.focused, "focus diverged");

        // `archives()` reports newest first, for a list the operator reads top
        // down. The model keeps insertion order, which is what the store holds.
        let archived: Vec<SessionId> = registry
            .archives()
            .into_iter()
            .rev()
            .map(|archive| archive.id)
            .collect();
        assert_eq!(archived, reference.archived, "archive list diverged");

        // Two things the model does not carry but that must hold of any fleet:
        // an id is either live or archived and never both, and the focused row
        // is one that exists.
        for id in &archived {
            assert!(
                !live.contains_key(id),
                "{id} is both live and archived",
            );
        }
        if let Some(focused) = registry.focused() {
            assert!(live.contains_key(&focused), "focus points at a row that is gone");
        }
    }

    fn teardown(registry: Self::SystemUnderTest, _reference: Model) {
        registry.shutdown();
    }
}

prop_state_machine! {
    #![proptest_config(Config {
        // Enough sequences to reach interleavings a person would not write, and
        // few enough that the suite stays a suite. Every case runs a real
        // registry, so this is not free.
        cases: 96,
        .. Config::default()
    })]

    #[test]
    fn the_fleet_matches_its_model_over_generated_operation_sequences(
        sequential 1..24 => Fleet
    );
}

/// Stated on its own because the model above has to encode it, and a rule a
/// model encodes silently is a rule nobody can find.
///
/// The property test found this by asserting the opposite: `Focus` then
/// `RoundTrip` and the focus was gone. It is correct — the persisted store is
/// what a daemon reloads with no window attached, so a focused row would have
/// its renderer priority and output stream pointed at nobody, and the UI's own
/// `state.focused` starts null on every load regardless.
#[test]
fn focus_is_not_part_of_what_the_daemon_persists() {
    let id = SessionId::new(1);
    let registry = SessionRegistry::from_store(SessionStoreSnapshot {
        sessions: vec![stored(&id)],
        ..SessionStoreSnapshot::default()
    });
    registry.focus(Some(id.clone())).expect("focus");
    assert_eq!(registry.focused(), Some(id.clone()));

    let restored = SessionRegistry::from_store(registry.store_snapshot());
    assert_eq!(
        restored.focused(),
        None,
        "a reloaded daemon has no window attached, so nothing is focused"
    );
    assert_eq!(
        restored.snapshot().len(),
        1,
        "the row itself survives; only the view's focus does not"
    );
    registry.shutdown();
    restored.shutdown();
}
