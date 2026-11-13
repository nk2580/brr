use brr_remote_protocol::{Event, EventCursor, EventReplay};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex},
};
use tokio::sync::broadcast;

const MAX_RETAINED_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone)]
pub struct EventHub {
    state: Arc<Mutex<State>>,
    sender: broadcast::Sender<Event>,
}

struct State {
    node_id: String,
    epoch: String,
    sequence: u64,
    capacity: usize,
    history: VecDeque<(Event, usize)>,
    bytes: usize,
    remote_cursors: HashMap<String, EventCursor>,
    retired_remote_epochs: HashSet<(String, String)>,
}

impl EventHub {
    pub fn new(node_id: String, epoch: String, capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(256);
        Self {
            state: Arc::new(Mutex::new(State {
                node_id,
                epoch,
                sequence: 0,
                capacity: capacity.max(1),
                history: VecDeque::new(),
                bytes: 0,
                remote_cursors: HashMap::new(),
                retired_remote_epochs: HashSet::new(),
            })),
            sender,
        }
    }
    pub fn publish(&self, message: Value) {
        let mut state = self.state.lock().unwrap();
        state.sequence += 1;
        let event = Event {
            node_id: state.node_id.clone(),
            epoch: state.epoch.clone(),
            sequence: state.sequence,
            owner_epoch: Some(state.epoch.clone()),
            message,
        };
        self.publish_locked(&mut state, event);
    }
    /// Re-sequence a peer event in the primary's stream without changing its owner.
    /// The source cursor deduplicates reconnect replay; ownerEpoch fences live controls.
    pub fn publish_remote(&self, mut event: Event) -> bool {
        let mut state = self.state.lock().unwrap();
        if state
            .retired_remote_epochs
            .contains(&(event.node_id.clone(), event.epoch.clone()))
        {
            return false;
        }
        if state
            .remote_cursors
            .get(&event.node_id)
            .is_some_and(|saved| saved.epoch == event.epoch && saved.sequence >= event.sequence)
        {
            return false;
        }
        if let Some(previous) = state
            .remote_cursors
            .get(&event.node_id)
            .filter(|saved| saved.epoch != event.epoch)
            .cloned()
        {
            state
                .retired_remote_epochs
                .insert((event.node_id.clone(), previous.epoch));
        }
        state.remote_cursors.insert(
            event.node_id.clone(),
            EventCursor {
                epoch: event.epoch.clone(),
                sequence: event.sequence,
            },
        );
        if event.owner_epoch.is_none() {
            event.owner_epoch = Some(event.epoch.clone());
        }
        state.sequence += 1;
        event.epoch = state.epoch.clone();
        event.sequence = state.sequence;
        self.publish_locked(&mut state, event);
        true
    }
    fn publish_locked(&self, state: &mut State, event: Event) {
        let size = serde_json::to_vec(&event)
            .map(|value| value.len())
            .unwrap_or(MAX_RETAINED_BYTES + 1);
        state.bytes += size;
        state.history.push_back((event.clone(), size));
        while state.history.len() > state.capacity || state.bytes > MAX_RETAINED_BYTES {
            if let Some((_, bytes)) = state.history.pop_front() {
                state.bytes -= bytes;
            } else {
                break;
            }
        }
        // Publish in the same lock order as history so subscribers see sequence order.
        let _ = self.sender.send(event);
    }
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.sender.subscribe()
    }
    pub fn cursor(&self) -> EventCursor {
        let state = self.state.lock().unwrap();
        EventCursor {
            epoch: state.epoch.clone(),
            sequence: state.sequence,
        }
    }
    pub fn replay(&self, cursor: Option<&EventCursor>) -> EventReplay {
        let state = self.state.lock().unwrap();
        let after = cursor.map(|c| c.sequence).unwrap_or(0);
        let oldest = state
            .history
            .front()
            .map(|(event, _)| event.sequence)
            .unwrap_or(state.sequence + 1);
        let requires_snapshot = cursor.is_none_or(|cursor| {
            cursor.epoch != state.epoch
                || cursor.sequence > state.sequence
                || cursor.sequence < oldest.saturating_sub(1)
        });
        EventReplay {
            epoch: state.epoch.clone(),
            latest: state.sequence,
            requires_snapshot,
            events: if requires_snapshot {
                Vec::new()
            } else {
                state
                    .history
                    .iter()
                    .filter(|(event, _)| event.sequence > after)
                    .map(|(event, _)| event.clone())
                    .collect()
            },
        }
    }
    pub fn rotate_epoch(&self, epoch: String) {
        {
            let mut state = self.state.lock().unwrap();
            state.epoch = epoch;
            state.sequence = 0;
            state.history.clear();
            state.bytes = 0;
        }
        self.publish(serde_json::json!({"method":"brr/epoch","params":{}}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reconnecting_client_receives_only_events_after_its_cursor() {
        let events = EventHub::new("node-one".into(), "runtime-one".into(), 4);
        events.publish(json!({"method":"turn/started","params":{"turnId":"turn-one"}}));
        events.publish(json!({"method":"item/agentMessage/delta","params":{"delta":"hello"}}));
        let replay = events.replay(Some(&EventCursor {
            epoch: "runtime-one".into(),
            sequence: 1,
        }));
        assert!(!replay.requires_snapshot);
        assert_eq!(replay.events.len(), 1);
        assert_eq!(replay.events[0].sequence, 2);
        assert_eq!(replay.events[0].message["params"]["delta"], "hello");
    }

    #[test]
    fn missing_history_or_a_different_runtime_requires_a_snapshot() {
        let events = EventHub::new("node-one".into(), "runtime-two".into(), 1);
        events.publish(json!({"method":"one"}));
        events.publish(json!({"method":"two"}));
        for cursor in [
            None,
            Some(EventCursor {
                epoch: "runtime-one".into(),
                sequence: 2,
            }),
            Some(EventCursor {
                epoch: "runtime-two".into(),
                sequence: 0,
            }),
            Some(EventCursor {
                epoch: "runtime-two".into(),
                sequence: 5,
            }),
        ] {
            let replay = events.replay(cursor.as_ref());
            assert!(
                replay.requires_snapshot,
                "An unavailable cursor was treated as complete: {cursor:?}"
            );
            assert!(replay.events.is_empty());
        }
        let complete = events.replay(Some(&EventCursor {
            epoch: "runtime-two".into(),
            sequence: 1,
        }));
        assert!(!complete.requires_snapshot);
        assert_eq!(complete.events.len(), 1);
    }

    #[test]
    fn remote_runtime_restarts_keep_the_primary_cursor_and_original_owner_generation() {
        let events = EventHub::new("primary".into(), "primary-stream".into(), 8);
        let remote = Event {
            node_id: "linux".into(),
            epoch: "remote-one".into(),
            sequence: 7,
            owner_epoch: Some("runtime-one".into()),
            message: json!({"method":"turn/started"}),
        };
        assert!(events.publish_remote(remote.clone()));
        assert!(!events.publish_remote(remote));
        assert!(events.publish_remote(Event {
            node_id: "linux".into(),
            epoch: "remote-two".into(),
            sequence: 1,
            owner_epoch: Some("runtime-two".into()),
            message: json!({"method":"brr/epoch"})
        }));
        let replay = events.replay(Some(&EventCursor {
            epoch: "primary-stream".into(),
            sequence: 0,
        }));
        assert!(!replay.requires_snapshot);
        assert_eq!(replay.events.len(), 2);
        assert_eq!(replay.events[1].sequence, 2);
        assert_eq!(replay.events[1].epoch, "primary-stream");
        assert_eq!(replay.events[1].owner_epoch.as_deref(), Some("runtime-two"));
        assert_eq!(replay.events[1].node_id, "linux");
        assert!(!events.publish_remote(Event {
            node_id: "linux".into(),
            epoch: "remote-one".into(),
            sequence: 8,
            owner_epoch: Some("runtime-one".into()),
            message: json!({"method":"turn/completed"})
        }));
    }
}
