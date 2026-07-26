use std::time::{Duration, Instant};

use bevy::prelude::{Entity, World};

use crate::commands::Command;
use crate::config::Config;
use crate::ecs::focus::{FocusIntentState, FocusRequestPolicy};
use crate::ecs::{
    ActiveWorkspaceMarker, FocusedMarker, LayoutPosition, PagingGesture, Position, Scrolling,
    SpawnCommandsExt, Unmanaged, WindowDisposition,
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

fn geometry_snapshot(world: &mut World) -> (Origin, Vec<(i32, Origin, Origin)>) {
    let strip_position = world
        .query_filtered::<&Position, bevy::prelude::With<ActiveWorkspaceMarker>>()
        .single(world)
        .expect("one active strip")
        .0;
    let mut windows = world
        .query::<(&Window, &Position, &LayoutPosition)>()
        .iter(world)
        .map(|(window, position, layout)| (window.id(), position.0, layout.0))
        .collect::<Vec<_>>();
    windows.sort_by_key(|(window_id, ..)| *window_id);
    (strip_position, windows)
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
    assert!(
        harness.mock_state.focus_attempts().is_empty(),
        "same-app activation must not complete synchronously on the animation thread"
    );
    assert_eq!(
        harness.mock_state.focus_without_raise_attempts(),
        vec![1],
        "animation-critical pre-focus must use the no-raise path"
    );
    assert!(
        harness.mock_state.focus_with_raise_attempts().is_empty(),
        "animation-critical pre-focus must never perform AX raise"
    );
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

    std::thread::sleep(Duration::from_millis(25));
    harness.app.update();
    assert_eq!(
        harness.mock_state.focus_attempts(),
        vec![1],
        "same-app activation must complete after its non-blocking deadline"
    );
}

#[test]
fn quick_swipe_focuses_once_at_visual_snap_without_geometry_side_effects() {
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
        Position(Origin::new(-575, 0)),
        Scrolling {
            position: -575.0,
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
    assert!(
        harness.mock_state.focus_attempts().is_empty(),
        "focus must wait while visible motion remains before the exact snap edge"
    );
    crate::assert_focused!(harness.world(), 0);

    let world = harness.world();
    world
        .entity_mut(strip)
        .get_mut::<Position>()
        .expect("strip position")
        .0 = Origin::new(-576, 0);
    let mut strip_entity = world.entity_mut(strip);
    let mut scrolling = strip_entity
        .get_mut::<Scrolling>()
        .expect("scrolling state");
    scrolling.position = -576.0;
    scrolling.target_position = Some(-576.0);
    drop(scrolling);
    drop(strip_entity);
    for (layout, mut position) in world
        .query::<(&LayoutPosition, &mut Position)>()
        .iter_mut(world)
    {
        position.0.x = layout.0.x - 576;
    }

    let geometry_at_snap = geometry_snapshot(harness.world());
    harness.app.update();
    assert!(
        harness.mock_state.focus_attempts().is_empty(),
        "same-app activation must not block the visual snap frame"
    );
    assert_eq!(
        harness.mock_state.focus_without_raise_attempts(),
        vec![1],
        "visual snap completion must begin one no-raise focus request"
    );
    std::thread::sleep(Duration::from_millis(25));
    harness.app.update();
    assert_eq!(
        harness.mock_state.focus_attempts(),
        vec![1],
        "visual snap completion must activate the most-visible managed target exactly once"
    );
    crate::assert_focused!(harness.world(), 0);
    assert_eq!(geometry_snapshot(harness.world()), geometry_at_snap);
    assert_eq!(harness.mock_state.cursor_position(), cursor);

    harness.mock_state.confirm_window_focus(1);
    for event in harness.mock_state.drain_events() {
        harness.world().write_message(event);
    }
    for _ in 0..2 {
        harness.app.update();
        assert_eq!(harness.mock_state.focus_attempts(), vec![1]);
        assert_eq!(geometry_snapshot(harness.world()), geometry_at_snap);
        assert_eq!(harness.mock_state.cursor_position(), cursor);
    }
    crate::assert_focused!(harness.world(), 1);

    for delta in [0.25, 0.15, 0.05, 0.01] {
        harness.world().write_message(Event::Scroll {
            delta,
            is_momentum: true,
        });
        harness.app.update();
        assert_eq!(
            harness.mock_state.focus_attempts(),
            vec![1],
            "remaining native momentum must not retry the confirmed focus"
        );
        assert_eq!(
            geometry_snapshot(harness.world()),
            geometry_at_snap,
            "focus confirmation and momentum tail must not rebound or move strip/window geometry"
        );
        assert_eq!(
            harness.mock_state.cursor_position(),
            cursor,
            "scroll focus must not warp the cursor"
        );
    }
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
    std::thread::sleep(Duration::from_millis(45));
    harness.app.update();

    crate::assert_focused!(harness.world(), 1);
    assert!(
        harness.mock_state.focus_with_raise_attempts().is_empty(),
        "exact no-raise confirmation must complete without fallback"
    );
}

#[test]
fn unconfirmed_scroll_focus_raises_only_after_scrolling_settles() {
    let mut harness = focus_request_before_snap_settlement();

    std::thread::sleep(Duration::from_millis(45));
    harness.app.update();
    assert!(
        harness.mock_state.focus_with_raise_attempts().is_empty(),
        "fallback must remain deferred while the owner strip is scrolling"
    );

    let strip = harness
        .world()
        .query_filtered::<Entity, (
            bevy::prelude::With<ActiveWorkspaceMarker>,
            bevy::prelude::With<crate::ecs::layout::LayoutStrip>,
        )>()
        .single(harness.world())
        .expect("active strip");
    harness.world().entity_mut(strip).remove::<Scrolling>();
    harness.mock_state.set_auto_confirm_focus(true);
    std::thread::sleep(Duration::from_millis(45));
    harness.app.update();

    assert_eq!(
        harness.mock_state.focus_with_raise_attempts(),
        vec![1],
        "unconfirmed no-raise attempt must fall back exactly once after settlement"
    );
    for event in harness.mock_state.drain_events() {
        harness.world().write_message(event);
    }
    harness.app.update();
    harness.app.update();
    crate::assert_focused!(harness.world(), 1);
}

#[test]
fn superseding_focus_intent_cancels_old_deferred_raise() {
    let mut harness = focus_request_before_snap_settlement();
    let replacement = window_entity(harness.world(), 0);

    harness.world().commands().focus_entity(replacement, false);
    harness.app.update();
    std::thread::sleep(Duration::from_millis(45));
    let scrolling_entities = {
        let world = harness.world();
        world
            .query_filtered::<Entity, bevy::prelude::With<Scrolling>>()
            .iter(world)
            .collect::<Vec<_>>()
    };
    for entity in scrolling_entities {
        harness.world().entity_mut(entity).remove::<Scrolling>();
    }
    harness.app.update();

    assert!(
        harness.mock_state.focus_with_raise_attempts().is_empty(),
        "superseded fallback must not raise the old scroll target"
    );
    assert!(
        harness
            .mock_state
            .focus_without_raise_attempts()
            .contains(&0),
        "replacement intent must become the active native request"
    );
}

#[test]
fn stale_confirmation_cannot_override_latest_intent() {
    let mut harness = focus_request_before_snap_settlement();
    let latest = window_entity(harness.world(), 0);
    harness.world().resource_mut::<FocusIntentState>().request(
        latest,
        0,
        FocusRequestPolicy::RaiseNow,
        true,
        Instant::now(),
    );

    harness
        .world()
        .write_message(Event::WindowFocused { window_id: 1 });
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
fn authoritative_external_focus_supersedes_pending_intent() {
    let mut harness = TestHarness::new().with_windows(3).with_focused_window(0);
    harness.run(print_state_events(1));
    harness.mock_state.set_auto_confirm_focus(false);
    harness.mock_state.clear_focus_attempts();

    let pending_target = window_entity(harness.world(), 1);
    harness.world().resource_mut::<FocusIntentState>().request(
        pending_target,
        1,
        FocusRequestPolicy::RaiseNow,
        false,
        Instant::now() - Duration::from_millis(100),
    );
    harness.mock_state.confirm_window_focus(2);
    for event in harness.mock_state.drain_events() {
        harness.world().write_message(event);
    }

    harness.app.update();
    harness.app.update();

    crate::assert_focused!(harness.world(), 2);
    assert!(
        harness
            .world()
            .resource::<FocusIntentState>()
            .pending()
            .is_none(),
        "authoritative external focus must cancel the superseded intent"
    );
    assert!(
        harness.mock_state.focus_attempts().is_empty(),
        "the watchdog must not retry the old target after exact external focus"
    );
}

#[test]
fn delayed_no_raise_focus_retries_without_raise() {
    let mut harness = TestHarness::new().with_windows(2).with_focused_window(0);
    harness.run(print_state_events(1));
    harness.mock_state.set_auto_confirm_focus(false);
    harness.mock_state.clear_focus_attempts();

    let target = window_entity(harness.world(), 1);
    harness.world().commands().focus_entity(target, false);
    harness.app.update();
    std::thread::sleep(Duration::from_millis(45));
    harness.app.update();
    harness.app.update();
    std::thread::sleep(Duration::from_millis(25));
    harness.app.update();

    assert_eq!(harness.mock_state.focus_attempts(), vec![1, 1]);
    assert_eq!(
        harness.mock_state.focus_without_raise_attempts(),
        vec![1, 1],
        "both the original request and its retry must preserve no-raise policy"
    );
    assert!(
        harness.mock_state.focus_with_raise_attempts().is_empty(),
        "retry must not take the raise path"
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
