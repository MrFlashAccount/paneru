use std::time::{Duration, Instant};

use bevy::prelude::{Entity, World};

use crate::commands::Command;
use crate::config::Config;
use crate::ecs::focus::FocusIntentState;
use crate::ecs::{
    ActiveWorkspaceMarker, FocusedMarker, PagingGesture, Position, Scrolling, Unmanaged,
    WindowDisposition,
};
use crate::events::Event;
use crate::manager::{Origin, Window, WindowManager};
use crate::tests::{TEST_PROCESS_ID, TEST_WORKSPACE_ID, TestHarness};

const SECOND_PROCESS_ID: i32 = 2;

fn swipe_config() -> Config {
    Config::try_from(
        r"
[options]
mouse_follows_focus = true

[swipe]
sensitivity = 0.20
continuous = true
sticky = true
paging = true
snap_padding = 100

[bindings]
",
    )
    .expect("valid swipe config")
}

fn paging_config() -> Config {
    Config::try_from(
        r"
[options]
mouse_follows_focus = true

[swipe]
sensitivity = 0.20
continuous = true
sticky = false
paging = true
snap_padding = 100

[bindings]
",
    )
    .expect("valid paging config")
}

fn window_entity(world: &mut World, id: i32) -> Entity {
    world
        .query::<(Entity, &Window)>()
        .iter(world)
        .find_map(|(entity, window)| (window.id() == id).then_some(entity))
        .expect("window entity")
}

fn print_state_events(count: usize) -> Vec<Event> {
    (0..count)
        .map(|_| Event::Command {
            command: Command::PrintState,
        })
        .collect()
}

fn focus_request_before_snap_settlement() -> TestHarness {
    let mut harness = TestHarness::new()
        .with_config(paging_config())
        .with_window(0, |window| {
            window.frame = bevy::math::IRect::new(0, 0, 800, 800);
        })
        .with_window(1, |window| {
            window.frame = bevy::math::IRect::new(800, 0, 1_600, 800);
        })
        .with_focused_window(0);
    harness.run(print_state_events(1));
    harness.mock_state.set_auto_confirm_focus(false);
    harness.mock_state.clear_focus_attempts();

    let world = harness.world();
    let origin = window_entity(world, 0);
    let strip = world
        .query_filtered::<Entity, (
            bevy::prelude::With<ActiveWorkspaceMarker>,
            bevy::prelude::With<crate::ecs::layout::LayoutStrip>,
        )>()
        .single(world)
        .expect("active strip");
    world.entity_mut(strip).insert((
        Position(Origin::ZERO),
        Scrolling {
            position: -200.0,
            snap_pending: true,
            paging_gesture: Some(PagingGesture {
                start_stop: 0.0,
                previous_stop: None,
                next_stop: Some(-576.0),
                release_velocity: -1.0,
            }),
            scroll_focus_origin: Some(origin),
            ..Scrolling::default()
        },
    ));

    harness.app.update();
    harness
}

#[test]
fn semantic_snap_commit_requests_focus_before_animation_settles() {
    let mut harness = focus_request_before_snap_settlement();
    assert_eq!(harness.mock_state.focus_attempts(), vec![1]);
    crate::assert_focused!(harness.world(), 0);

    let scrolling = harness
        .world()
        .query_filtered::<&Scrolling, bevy::prelude::With<ActiveWorkspaceMarker>>()
        .single(harness.world())
        .expect("scroll animation remains active");
    let target = scrolling.target_position.expect("snap target committed");
    assert!(
        (target - scrolling.position).abs() > 10.0,
        "focus request must not wait for the old 90% animation-progress gate"
    );
}

#[test]
fn momentum_tail_prefocuses_without_pulling_the_window_into_viewport() {
    let cursor = Origin::new(700, 300);
    let mut harness = TestHarness::new()
        .with_config(paging_config())
        .with_window(0, |window| {
            window.frame = bevy::math::IRect::new(0, 0, 800, 800);
        })
        .with_window(1, |window| {
            window.frame = bevy::math::IRect::new(800, 0, 1_600, 800);
        })
        .with_focused_window(0);
    harness.run(print_state_events(1));
    harness.mock_state.set_auto_confirm_focus(false);
    harness.mock_state.clear_focus_attempts();
    harness
        .world()
        .resource::<WindowManager>()
        .warp_mouse(cursor);

    let world = harness.world();
    let origin = window_entity(world, 0);
    let strip = world
        .query_filtered::<Entity, (
            bevy::prelude::With<ActiveWorkspaceMarker>,
            bevy::prelude::With<crate::ecs::layout::LayoutStrip>,
        )>()
        .single(world)
        .expect("active strip");
    world.entity_mut(strip).insert((
        Position(Origin::new(-576, 0)),
        Scrolling {
            position: -576.0,
            target_position: Some(-576.0),
            snap_pending: true,
            is_user_swiping: true,
            gesture_active: true,
            physical_contact: crate::ecs::PhysicalContact::Inactive,
            paging_gesture: Some(PagingGesture {
                start_stop: 0.0,
                previous_stop: None,
                next_stop: Some(-576.0),
                release_velocity: -1.0,
            }),
            scroll_focus_origin: Some(origin),
            ..Scrolling::default()
        },
    ));

    harness.app.update();
    assert_eq!(harness.mock_state.focus_attempts(), vec![1]);
    crate::assert_focused!(harness.world(), 0);

    let geometry_before_confirmation = {
        let world = harness.world();
        let strip_position = world
            .query_filtered::<&Position, bevy::prelude::With<ActiveWorkspaceMarker>>()
            .single(world)
            .expect("one active strip")
            .0;
        let mut windows = world
            .query::<(&Window, &Position, &crate::ecs::LayoutPosition)>()
            .iter(world)
            .map(|(window, position, layout)| (window.id(), position.0, layout.0))
            .collect::<Vec<_>>();
        windows.sort_by_key(|(window_id, ..)| *window_id);
        (strip_position, windows)
    };

    harness.mock_state.confirm_window_focus(1);
    for event in harness.mock_state.drain_events() {
        harness.world().write_message(event);
    }
    for _ in 0..5 {
        harness.app.update();
    }

    crate::assert_focused!(harness.world(), 1);
    let geometry_after_confirmation = {
        let world = harness.world();
        let strip_position = world
            .query_filtered::<&Position, bevy::prelude::With<ActiveWorkspaceMarker>>()
            .single(world)
            .expect("one active strip")
            .0;
        let mut windows = world
            .query::<(&Window, &Position, &crate::ecs::LayoutPosition)>()
            .iter(world)
            .map(|(window, position, layout)| (window.id(), position.0, layout.0))
            .collect::<Vec<_>>();
        windows.sort_by_key(|(window_id, ..)| *window_id);
        (strip_position, windows)
    };

    assert_eq!(
        geometry_after_confirmation, geometry_before_confirmation,
        "pre-focus must not reshuffle, auto-center, or pull the target into the viewport"
    );
    assert_eq!(
        harness.mock_state.cursor_position(),
        cursor,
        "pre-focus must not warp the cursor"
    );
}

#[test]
fn exact_os_confirmation_commits_requested_focus() {
    let mut harness = focus_request_before_snap_settlement();
    harness.mock_state.confirm_window_focus(1);
    for event in harness.mock_state.drain_events() {
        harness.world().write_message(event);
    }
    harness.app.update();
    harness.app.update();

    crate::assert_focused!(harness.world(), 1);
}

#[test]
fn stale_confirmation_cannot_override_latest_intent() {
    let mut harness = focus_request_before_snap_settlement();
    let latest = window_entity(harness.world(), 0);
    harness
        .world()
        .resource_mut::<FocusIntentState>()
        .request(latest, 0, true, Instant::now());

    harness.mock_state.confirm_window_focus(1);
    for event in harness.mock_state.drain_events() {
        harness.world().write_message(event);
    }
    harness.app.update();
    harness.app.update();

    crate::assert_focused!(harness.world(), 0);
    assert_eq!(
        harness
            .world()
            .resource::<FocusIntentState>()
            .effective_entity(None),
        Some(latest)
    );

    harness.mock_state.confirm_window_focus(0);
    for event in harness.mock_state.drain_events() {
        harness.world().write_message(event);
    }
    harness.app.update();
    harness.app.update();
    crate::assert_focused!(harness.world(), 0);
    assert!(
        harness
            .world()
            .resource::<FocusIntentState>()
            .pending()
            .is_none()
    );
}

#[test]
fn application_front_switch_alone_does_not_claim_window_focus() {
    TestHarness::new()
        .with_windows(1)
        .with_focused_window(0)
        .with_app(SECOND_PROCESS_ID, "second", "Second", |app| {
            app.is_frontmost = false;
        })
        .with_app_window(SECOND_PROCESS_ID, 1, TEST_WORKSPACE_ID)
        .on_iteration(0, |_world, state| {
            state.confirm_frontmost(SECOND_PROCESS_ID);
        })
        .on_iteration(1, |world, state| {
            let target = window_entity(world, 1);
            assert_eq!(state.focused_window_id(SECOND_PROCESS_ID), None);
            assert!(!world.entity(target).contains::<FocusedMarker>());
            crate::assert_focused!(world, 0);
        })
        .run(print_state_events(2));
}

#[test]
fn quick_swipes_focus_exact_cross_app_targets_in_both_directions() {
    let cursor = Origin::new(700, 300);
    let events = print_state_events(8);
    let final_iteration = events.len() - 1;

    TestHarness::new()
        .with_config(swipe_config())
        .with_windows(1)
        .with_focused_window(0)
        .with_app(SECOND_PROCESS_ID, "second", "Second", |app| {
            app.is_frontmost = false;
        })
        .with_app_window(SECOND_PROCESS_ID, 1, TEST_WORKSPACE_ID)
        .with_workspace_window(2, TEST_WORKSPACE_ID, |_| {})
        .on_iteration(0, move |world, state| {
            world.resource::<WindowManager>().warp_mouse(cursor);
            state.clear_focus_attempts();
            world.write_message(Event::TouchpadDown);
            world.write_message(Event::Scroll {
                delta: 500.0,
                is_momentum: false,
            });
            world.write_message(Event::TouchpadUp);
        })
        .on_iteration(1, |world, _state| {
            let expired = Instant::now()
                .checked_sub(Duration::from_millis(100))
                .expect("100 ms is within the monotonic clock range");
            for mut scroll in world.query::<&mut Scrolling>().iter_mut(world) {
                scroll.last_event = expired;
            }
        })
        .on_iteration(2, |world, state| {
            crate::assert_focused!(world, 1);
            assert_eq!(state.focused_window_id(SECOND_PROCESS_ID), Some(1));
            assert_eq!(state.focus_attempts(), vec![1]);

            // Interrupt the last 10% of the first snap with a quick reverse
            // gesture. The old target must not be issued again.
            world.write_message(Event::TouchpadDown);
            world.write_message(Event::Scroll {
                delta: -500.0,
                is_momentum: false,
            });
            world.write_message(Event::TouchpadUp);
        })
        .on_iteration(3, |world, _state| {
            let expired = Instant::now()
                .checked_sub(Duration::from_millis(100))
                .expect("100 ms is within the monotonic clock range");
            for mut scroll in world.query::<&mut Scrolling>().iter_mut(world) {
                scroll.last_event = expired;
            }
        })
        .on_iteration(final_iteration, move |world, state| {
            crate::assert_focused!(world, 0);
            assert_eq!(state.focused_window_id(TEST_PROCESS_ID), Some(0));
            assert_eq!(
                state.focus_attempts(),
                vec![1, 0],
                "each direction must issue its exact target once; new input supersedes old intent"
            );
            assert_eq!(state.cursor_position(), cursor);
        })
        .run(events);
}

#[test]
fn quick_swipe_never_targets_adjacent_passthrough_window() {
    let events = print_state_events(8);

    TestHarness::new()
        .with_config(swipe_config())
        .with_windows(1)
        .with_focused_window(0)
        .with_app(SECOND_PROCESS_ID, "second", "Second", |app| {
            app.is_frontmost = false;
        })
        .with_default_window_disposition(WindowDisposition::Passthrough)
        .with_app_window(SECOND_PROCESS_ID, 1, TEST_WORKSPACE_ID)
        .on_iteration(0, |world, state| {
            state.clear_focus_attempts();
            world.write_message(Event::TouchpadDown);
            world.write_message(Event::Scroll {
                delta: 500.0,
                is_momentum: false,
            });
            world.write_message(Event::TouchpadUp);
        })
        .on_iteration(1, |world, _state| {
            let expired = Instant::now()
                .checked_sub(Duration::from_millis(100))
                .expect("100 ms is within the monotonic clock range");
            for mut scroll in world.query::<&mut Scrolling>().iter_mut(world) {
                scroll.last_event = expired;
            }
        })
        .on_iteration(7, |world, state| {
            let target = window_entity(world, 1);
            assert_eq!(
                world.entity(target).get::<Unmanaged>(),
                Some(&Unmanaged::Passthrough)
            );
            assert!(state.focus_attempts().is_empty());
            assert_eq!(state.focused_window_id(TEST_PROCESS_ID), Some(0));
            crate::assert_focused!(world, 0);
        })
        .run(events);
}
