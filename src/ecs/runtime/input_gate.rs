//! Native-event ingestion and the display-frame presentation gate.

use std::collections::BTreeMap;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use crate::events::{Event, EventReceiver};

use super::{ACTIVE_FRAME_INTERVAL, EdgeOverscrollPhase, RuntimeActivity, RuntimeWork};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PumpWait {
    pub(super) timeout: Option<Duration>,
    pub(super) frame_pacing: bool,
}

impl PumpWait {
    fn frame() -> Self {
        Self {
            timeout: Some(ACTIVE_FRAME_INTERVAL),
            frame_pacing: true,
        }
    }

    fn idle(activity: RuntimeActivity, now: Instant) -> Self {
        Self {
            timeout: activity.wait(now),
            frame_pacing: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct PumpOutcome {
    pub(super) did_frame_wait: bool,
    pub(super) did_idle_wait: bool,
    pub(super) presented_scroll_input: bool,
}

pub(super) fn active_scroll_work(work: &RuntimeWork<'_, '_>) -> bool {
    work.scrolling
        .iter()
        .any(super::super::scroll::scrolling_needs_frame)
        || work
            .edge_overscroll_visuals
            .iter()
            .any(|visual| visual.phase == EdgeOverscrollPhase::RestoreQueued)
}

pub(super) fn pump_receiver(
    receiver: &EventReceiver,
    activity: RuntimeActivity,
    now: Instant,
    synthetic_pending: bool,
    mut pump: impl FnMut(PumpWait),
) -> (Vec<Event>, bool, PumpOutcome) {
    let generation_before_drain = receiver.generation();
    let (mut received_events, mut should_exit) = drain_event_channel(receiver);
    let frame_pacing_required =
        activity.frame_work || contains_horizontal_scroll_input(&received_events);
    let idle_wait_required = !frame_pacing_required
        && !synthetic_pending
        && received_events.is_empty()
        && !should_exit
        && receiver.generation() == generation_before_drain;
    let mut outcome = PumpOutcome::default();

    if !should_exit && frame_pacing_required {
        pump(PumpWait::frame());
        outcome.did_frame_wait = true;
        append_after_wait(receiver, &mut received_events, &mut should_exit);
    } else if idle_wait_required {
        pump(PumpWait::idle(activity, now));
        outcome.did_idle_wait = true;
        append_after_wait(receiver, &mut received_events, &mut should_exit);

        // A fresh gesture can wake an otherwise idle run loop. Hold its whole
        // batch until the next display frame instead of publishing one
        // unpaced setup update and letting dense follow-up events race it.
        if !should_exit && contains_horizontal_scroll_input(&received_events) {
            pump(PumpWait::frame());
            outcome.did_frame_wait = true;
            append_after_wait(receiver, &mut received_events, &mut should_exit);
        }
    }

    outcome.presented_scroll_input = contains_horizontal_scroll_input(&received_events);
    (
        coalesce_window_geometry_events(received_events),
        should_exit,
        outcome,
    )
}

fn append_after_wait(
    receiver: &EventReceiver,
    received_events: &mut Vec<Event>,
    should_exit: &mut bool,
) {
    let (after_wait, exit_after_wait) = drain_event_channel(receiver);
    received_events.extend(after_wait);
    *should_exit |= exit_after_wait;
}

fn contains_horizontal_scroll_input(events: &[Event]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            Event::Swipe { .. }
                | Event::Scroll { .. }
                | Event::TouchpadDown
                | Event::TouchpadPhysicalUp
                | Event::TouchpadMomentumStart
                | Event::TouchpadUp
        )
    })
}

pub(super) fn drain_event_channel(receiver: &EventReceiver) -> (Vec<Event>, bool) {
    let mut received_events = Vec::new();
    let mut pending_mouse = None;
    let mut should_exit = false;
    loop {
        match receiver.try_recv() {
            Ok(Event::Exit) | Err(TryRecvError::Disconnected) => {
                should_exit = true;
                break;
            }
            Ok(event) if matches!(event, Event::MouseMoved { .. }) => {
                pending_mouse = Some(event);
            }
            Ok(event) => {
                received_events.extend(pending_mouse.take());
                received_events.push(event);
            }
            Err(TryRecvError::Empty) => break,
        }
    }
    received_events.extend(pending_mouse);
    (
        coalesce_window_geometry_events(received_events),
        should_exit,
    )
}

/// Keeps only the final geometry notification for each window and event kind.
///
/// Superseded events leave tombstones instead of moving their replacements
/// forward, so every surviving event retains its original FIFO position
/// relative to unrelated events and the other geometry kind.
fn coalesce_window_geometry_events(events: Vec<Event>) -> Vec<Event> {
    let mut latest_moved = BTreeMap::new();
    let mut latest_resized = BTreeMap::new();
    let mut coalesced = Vec::with_capacity(events.len());
    for event in events {
        let previous_position = match &event {
            Event::WindowMoved { window_id } => latest_moved.insert(*window_id, coalesced.len()),
            Event::WindowResized { window_id } => {
                latest_resized.insert(*window_id, coalesced.len())
            }
            _ => None,
        };
        if let Some(previous_position) = previous_position {
            coalesced[previous_position] = None;
        }
        coalesced.push(Some(event));
    }
    coalesced.into_iter().flatten().collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use bevy::app::{App, PostUpdate};

    use super::{PumpWait, pump_receiver};
    use crate::ecs::runtime::RuntimeActivity;
    use crate::ecs::{Position, systems};
    use crate::events::{Event, EventSender};
    use crate::manager::{MockWindowApi, Origin, Window};
    use crate::platform::AxMainThread;

    fn scroll_sum(events: &[Event]) -> f64 {
        events
            .iter()
            .filter_map(|event| match event {
                Event::Scroll { delta, .. } => Some(*delta),
                _ => None,
            })
            .sum()
    }

    #[test]
    fn dense_scroll_batches_wait_once_and_lose_no_deltas_at_supported_refresh_rates() {
        for refresh_hz in [60_u32, 120, 240] {
            let (sender, receiver) = EventSender::new();
            for _ in 0..refresh_hz {
                sender
                    .send(Event::Scroll {
                        delta: 1.0,
                        is_momentum: false,
                    })
                    .unwrap();
            }
            let mut waits = Vec::new();
            let (events, should_exit, outcome) = pump_receiver(
                &receiver,
                RuntimeActivity {
                    frame_work: false,
                    nearest_deadline: None,
                },
                Instant::now(),
                false,
                |wait| {
                    waits.push(wait);
                    for _ in 0..refresh_hz {
                        sender
                            .send(Event::Scroll {
                                delta: -0.25,
                                is_momentum: false,
                            })
                            .unwrap();
                    }
                },
            );

            assert!(!should_exit);
            assert_eq!(waits.len(), 1, "{refresh_hz} Hz");
            assert!(waits[0].frame_pacing, "{refresh_hz} Hz");
            assert!(outcome.did_frame_wait);
            assert!(outcome.presented_scroll_input);
            assert_eq!(
                scroll_sum(&events),
                f64::from(refresh_hz) * 0.75,
                "{refresh_hz} Hz"
            );
        }
    }

    #[test]
    fn active_animation_waits_even_with_queued_and_synthetic_work() {
        let (sender, receiver) = EventSender::new();
        sender.send(Event::UpdaterStatusChanged).unwrap();
        let mut waits = Vec::new();
        let (_, should_exit, outcome) = pump_receiver(
            &receiver,
            RuntimeActivity {
                frame_work: true,
                nearest_deadline: None,
            },
            Instant::now(),
            true,
            |wait| waits.push(wait),
        );

        assert!(!should_exit);
        assert_eq!(waits.len(), 1);
        assert!(waits[0].frame_pacing);
        assert!(outcome.did_frame_wait);
    }

    #[test]
    fn scroll_that_wakes_idle_wait_is_held_for_the_following_display_frame() {
        let (sender, receiver) = EventSender::new();
        let mut waits = Vec::new();
        let (events, should_exit, outcome) = pump_receiver(
            &receiver,
            RuntimeActivity {
                frame_work: false,
                nearest_deadline: Some(Instant::now() + Duration::from_secs(1)),
            },
            Instant::now(),
            false,
            |wait| {
                waits.push(wait);
                if wait.frame_pacing {
                    sender
                        .send(Event::Scroll {
                            delta: 3.0,
                            is_momentum: false,
                        })
                        .unwrap();
                } else {
                    sender.send(Event::TouchpadDown).unwrap();
                    sender
                        .send(Event::Scroll {
                            delta: 2.0,
                            is_momentum: false,
                        })
                        .unwrap();
                }
            },
        );

        assert!(!should_exit);
        assert_eq!(waits.len(), 2);
        assert_eq!(
            waits,
            [
                PumpWait {
                    timeout: waits[0].timeout,
                    frame_pacing: false,
                },
                PumpWait {
                    timeout: Some(super::ACTIVE_FRAME_INTERVAL),
                    frame_pacing: true,
                },
            ]
        );
        assert!(outcome.did_idle_wait);
        assert!(outcome.did_frame_wait);
        assert_eq!(scroll_sum(&events), 5.0);
        assert!(matches!(events.first(), Some(Event::TouchpadDown)));
    }

    #[test]
    fn each_simulated_tick_commits_each_changed_window_at_most_once() {
        for refresh_hz in [60_u32, 120, 240] {
            crate::frame_metrics::reset_for_tests();
            let attempts = Arc::new(AtomicUsize::new(0));
            let mut app = App::new();
            app.insert_non_send_resource(AxMainThread::for_tests())
                .add_systems(PostUpdate, systems::commit_window_position);
            let mut entities = Vec::new();

            for window_id in [41, 42] {
                let mut mock = MockWindowApi::new();
                mock.expect_reposition()
                    .times(refresh_hz as usize)
                    .returning({
                        let attempts = Arc::clone(&attempts);
                        move |_| {
                            attempts.fetch_add(1, Ordering::Relaxed);
                            crate::frame_metrics::record_ax_position_write(window_id);
                        }
                    });
                entities.push(
                    app.world_mut()
                        .spawn((Window::new(Box::new(mock)), Position(Origin::ZERO)))
                        .id(),
                );
            }
            app.world_mut().clear_trackers();

            for _ in 0..refresh_hz {
                for entity in &entities {
                    for _ in 0..8 {
                        app.world_mut()
                            .entity_mut(*entity)
                            .get_mut::<Position>()
                            .expect("window position")
                            .x += 1;
                    }
                }
                crate::frame_metrics::record_presentation_frame(true);
                crate::frame_metrics::record_display_link_tick();
                app.update();
            }

            let metrics = crate::frame_metrics::snapshot();
            assert_eq!(metrics.presentation_frame_ticks, u64::from(refresh_hz));
            assert_eq!(metrics.display_link_ticks, u64::from(refresh_hz));
            assert_eq!(metrics.active_scroll_ecs_updates, u64::from(refresh_hz));
            assert_eq!(
                metrics.commit_window_position_executions,
                u64::from(refresh_hz)
            );
            assert_eq!(
                attempts.load(Ordering::Relaxed),
                refresh_hz as usize * entities.len()
            );
            for window_id in [41, 42] {
                assert_eq!(
                    metrics.ax_position_writes_by_window.get(&window_id),
                    Some(&u64::from(refresh_hz)),
                    "{refresh_hz} Hz window {window_id}"
                );
            }
        }
    }
}
