//! One-turn native-event accumulation at the event-ingestion boundary.
//!
//! Runtime code owns when drains happen around the platform wait. This module
//! owns what one drain means: mouse batching, geometry coalescing, FIFO
//! preservation, and receiver-disconnection shutdown policy.

use std::collections::BTreeMap;
use std::sync::mpsc::TryRecvError;

use super::{Event, EventReceiver};

/// Opaque native-event batch spanning every drain in one ECS turn.
#[derive(Default)]
pub(crate) struct TurnBatch {
    events: Vec<Option<Event>>,
    latest_moved: BTreeMap<i32, usize>,
    latest_resized: BTreeMap<i32, usize>,
    pending_mouse: Option<Event>,
    live_events: usize,
    should_exit: bool,
}

impl TurnBatch {
    pub(crate) fn drain(&mut self, receiver: &EventReceiver) {
        loop {
            match receiver.try_recv() {
                Ok(Event::Exit) | Err(TryRecvError::Disconnected) => {
                    self.should_exit = true;
                    break;
                }
                Ok(event) if matches!(event, Event::MouseMoved { .. }) => {
                    self.pending_mouse = Some(event);
                }
                Ok(event) => {
                    self.flush_mouse();
                    self.push(event);
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        // Preserve the established per-drain mouse batching boundary while
        // retaining one geometry accumulator for the entire ECS turn.
        self.flush_mouse();
    }

    fn flush_mouse(&mut self) {
        if let Some(event) = self.pending_mouse.take() {
            self.push(event);
        }
    }

    /// Keeps only the final geometry notification for each window and kind.
    /// Tombstones retain every survivor's FIFO position relative to unrelated
    /// events and the other geometry kind until the single final compaction.
    fn push(&mut self, event: Event) {
        let previous_position = match &event {
            Event::WindowMoved { window_id } => {
                self.latest_moved.insert(*window_id, self.events.len())
            }
            Event::WindowResized { window_id } => {
                self.latest_resized.insert(*window_id, self.events.len())
            }
            _ => None,
        };
        if let Some(previous_position) = previous_position {
            self.events[previous_position] = None;
            self.live_events -= 1;
        }
        self.events.push(Some(event));
        self.live_events += 1;
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.live_events == 0
    }

    pub(crate) fn should_exit(&self) -> bool {
        self.should_exit
    }

    pub(crate) fn finish(mut self) -> (Vec<Event>, bool) {
        self.flush_mouse();
        (
            self.events.into_iter().flatten().collect(),
            self.should_exit,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::TurnBatch;
    use crate::events::{Event, EventSender};
    use crate::platform::Modifiers;
    use objc2_core_foundation::CGPoint;

    fn mouse_moved(x: f64) -> Event {
        Event::MouseMoved {
            point: CGPoint::new(x, 0.0),
            modifiers: Modifiers::empty(),
        }
    }

    fn drain(events: impl IntoIterator<Item = Event>) -> Vec<Event> {
        let (sender, receiver) = EventSender::new();
        for event in events {
            sender.send(event).unwrap();
        }
        let mut batch = TurnBatch::default();
        batch.drain(&receiver);
        batch.finish().0
    }

    #[test]
    fn duplicate_geometry_keeps_each_final_fifo_position() {
        let events = drain([
            Event::WindowMoved { window_id: 7 },
            Event::WindowResized { window_id: 7 },
            Event::WindowMoved { window_id: 8 },
            Event::UpdaterStatusChanged,
            Event::WindowMoved { window_id: 7 },
            Event::WindowResized { window_id: 7 },
        ]);
        assert_eq!(events.len(), 4);
        assert!(matches!(&events[0], Event::WindowMoved { window_id: 8 }));
        assert!(matches!(&events[1], Event::UpdaterStatusChanged));
        assert!(matches!(&events[2], Event::WindowMoved { window_id: 7 }));
        assert!(matches!(&events[3], Event::WindowResized { window_id: 7 }));
    }

    #[test]
    fn geometry_coalescing_preserves_mouse_drain_boundaries() {
        let events = drain([
            mouse_moved(1.0),
            Event::WindowMoved { window_id: 7 },
            mouse_moved(2.0),
            Event::WindowMoved { window_id: 7 },
            mouse_moved(3.0),
            mouse_moved(4.0),
            Event::WindowResized { window_id: 7 },
        ]);
        assert_eq!(events.len(), 5);
        assert!(
            matches!(&events[0], Event::MouseMoved { point, .. } if (point.x - 1.0).abs() < f64::EPSILON)
        );
        assert!(
            matches!(&events[1], Event::MouseMoved { point, .. } if (point.x - 2.0).abs() < f64::EPSILON)
        );
        assert!(matches!(&events[2], Event::WindowMoved { window_id: 7 }));
        assert!(
            matches!(&events[3], Event::MouseMoved { point, .. } if (point.x - 4.0).abs() < f64::EPSILON)
        );
        assert!(matches!(&events[4], Event::WindowResized { window_id: 7 }));
    }

    #[test]
    fn exit_and_disconnection_are_owned_by_ingestion_batch() {
        let (sender, receiver) = EventSender::new();
        sender.send(Event::ApplicationActivated).unwrap();
        sender.send(Event::Exit).unwrap();
        let mut batch = TurnBatch::default();
        batch.drain(&receiver);
        let (events, should_exit) = batch.finish();
        assert!(should_exit);
        assert!(matches!(events.as_slice(), [Event::ApplicationActivated]));

        let (sender, receiver) = EventSender::new();
        drop(sender);
        let mut batch = TurnBatch::default();
        batch.drain(&receiver);
        assert!(batch.finish().1);
    }
}
