use std::time::{Duration, Instant};

use bevy::ecs::query::{Has, With};
use bevy::math::IRect;
use bevy::prelude::Entity;
use bevy::time::TimeUpdateStrategy;
use objc2_core_foundation::CGPoint;

use super::{
    GestureInput, InitialTouchpadLifecycle, Scrolling, SnapMode, apply_initial_touchpad_lifecycle,
    begin_touchpad_gesture, focus_target_after_scroll, resume_touchpad_gesture,
    scrolling_needs_frame, smooth_native_scroll, snap_mode, sticky_edge_snap_target,
};
use crate::commands::Command;
use crate::ecs::{
    ActiveWorkspaceMarker, EdgeOverscrollVisual, FocusedMarker, Position, VerifyWindowPosition,
};
use crate::events::Event;
use crate::manager::{Origin, Window, WindowManager};
use crate::platform::Modifiers;
use crate::tests::TestHarness;

#[test]
fn focus_target_prefers_the_most_visible_column_then_its_leading_edge() {
    let mut world = bevy::prelude::World::new();
    let first = world.spawn_empty().id();
    let second = world.spawn_empty().id();
    let third = world.spawn_empty().id();
    let viewport = IRect::new(0, 0, 1_000, 800);

    assert_eq!(
        focus_target_after_scroll(
            &viewport,
            -200,
            [(first, 0, 400), (second, 400, 400), (third, 800, 400)],
        ),
        Some(second),
        "equal fully-visible columns prefer the one closest to the leading edge"
    );
    assert_eq!(
        focus_target_after_scroll(
            &viewport,
            -1_000,
            [(first, 0, 1_000), (second, 1_000, 1_000)],
        ),
        Some(second),
        "a full-width page selects the newly visible window"
    );
    assert_eq!(
        focus_target_after_scroll(&viewport, -500, [(first, 0, 1_500), (second, 1_500, 500)],),
        Some(first),
        "panning inside an oversized window keeps that window focused"
    );
}

#[test]
fn settled_scroll_focuses_visible_window_without_warping_cursor() {
    let cursor = Origin::new(700, 300);
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::PrintState,
        },
    ];

    TestHarness::new()
        .with_windows(3)
        .on_iteration(0, move |world, _state| {
            world.resource::<WindowManager>().warp_mouse(cursor);
            let scroll_focus_origin = {
                let mut focused = world.query_filtered::<Entity, With<FocusedMarker>>();
                focused.single(world).expect("one focused window")
            };
            let strip_entity = {
                let mut strips = world.query_filtered::<(Entity, &mut Position), (
                    With<ActiveWorkspaceMarker>,
                    With<crate::ecs::layout::LayoutStrip>,
                )>();
                let (entity, mut position) = strips.single_mut(world).expect("one active strip");
                position.x = -176;
                entity
            };
            world.entity_mut(strip_entity).insert(Scrolling {
                position: -176.0,
                scroll_focus_origin: Some(scroll_focus_origin),
                last_event: Instant::now()
                    .checked_sub(Duration::from_millis(100))
                    .expect("100ms must fit before now"),
                ..Default::default()
            });
        })
        .on_iteration(1, move |world, state| {
            crate::assert_focused!(world, 1);
            assert_eq!(
                state.cursor_position(),
                cursor,
                "scroll-driven focus must not warp the cursor"
            );
            let mut scrolling = world.query_filtered::<&Scrolling, (
                With<ActiveWorkspaceMarker>,
                With<crate::ecs::layout::LayoutStrip>,
            )>();
            assert!(
                scrolling.single(world).is_err(),
                "focus changes only after scrolling reaches its terminal state"
            );
        })
        .run(commands);
}

#[test]
fn native_scroll_smoothing_converges_without_overshoot() {
    let mut position = 0.0;
    let mut settled = false;
    for _ in 0..120 {
        let previous = position;
        (position, settled) = smooth_native_scroll(position, 100.0, 1.0 / 60.0);
        assert!(position >= previous);
        assert!(position <= 100.0);
        if settled {
            break;
        }
    }
    assert!((position - 100.0).abs() < f64::EPSILON);
    assert!(settled);
}

#[test]
fn momentum_resume_preserves_target_but_new_touch_interrupts_it() {
    let mut scrolling = Scrolling {
        target_position: Some(-320.0),
        ..Default::default()
    };

    resume_touchpad_gesture(true, Some(&mut scrolling));
    assert_eq!(
        scrolling.target_position,
        Some(-320.0),
        "momentum must continue extending the in-flight native-scroll target"
    );

    begin_touchpad_gesture(true, true, true, None, Some(&mut scrolling));
    assert_eq!(
        scrolling.target_position, None,
        "a new physical touch must interrupt the previous target"
    );
}

#[test]
fn terminal_phase_closes_a_brand_new_overscroll_before_deferred_insertion() {
    let mut scrolling = Scrolling {
        position: 12.0,
        is_user_swiping: true,
        gesture_active: true,
        paging_gesture: Some(crate::ecs::PagingGesture {
            start_stop: 0.0,
            previous_stop: None,
            next_stop: Some(-600.0),
            release_velocity: 0.0,
        }),
        edge_overscroll: super::overscroll::EdgeOverscroll::armed(),
        ..Default::default()
    };
    super::paging::constrain_motion(&mut scrolling, 1.0, true);

    apply_initial_touchpad_lifecycle(
        &mut scrolling,
        InitialTouchpadLifecycle {
            physical_up: true,
            up: false,
            direction_modifier: 1.0,
        },
    );

    assert!(!scrolling.gesture_active);
    assert!(scrolling.is_user_swiping);
    assert!(!scrolling.edge_overscroll.accepts_input());
    assert!(scrolling.edge_overscroll.is_returning());
    assert!(scrolling_needs_frame(&scrolling));
}

#[test]
fn terminal_events_before_latest_down_cannot_close_the_new_contact() {
    let old_terminal_then_new_down = GestureInput {
        touchpad_physical_up: Some(0),
        touchpad_up: Some(1),
        touchpad_down: Some(2),
        ..Default::default()
    };
    assert!(
        !old_terminal_then_new_down
            .belongs_to_latest_contact(old_terminal_then_new_down.touchpad_physical_up)
    );
    assert!(
        !old_terminal_then_new_down
            .belongs_to_latest_contact(old_terminal_then_new_down.touchpad_up)
    );

    let new_down_then_terminal = GestureInput {
        touchpad_down: Some(0),
        touchpad_physical_up: Some(1),
        touchpad_up: Some(2),
        ..Default::default()
    };
    assert!(
        new_down_then_terminal
            .belongs_to_latest_contact(new_down_then_terminal.touchpad_physical_up)
    );
    assert!(new_down_then_terminal.belongs_to_latest_contact(new_down_then_terminal.touchpad_up));
}

#[test]
fn settled_overscroll_does_not_request_more_frames() {
    let mut scrolling = Scrolling {
        position: 80.0,
        paging_gesture: Some(crate::ecs::PagingGesture {
            start_stop: 0.0,
            previous_stop: None,
            next_stop: Some(-600.0),
            release_velocity: 0.0,
        }),
        edge_overscroll: super::overscroll::EdgeOverscroll::armed(),
        ..Default::default()
    };
    assert!(!scrolling_needs_frame(&scrolling));

    super::paging::constrain_motion(&mut scrolling, 1.0, true);
    assert!(
        !scrolling_needs_frame(&scrolling),
        "a static held pull is input-driven, not display-polled"
    );

    assert!(scrolling.edge_overscroll.release());
    assert!(scrolling_needs_frame(&scrolling));
    while scrolling.edge_overscroll.is_returning() {
        scrolling.edge_overscroll.integrate(1.0 / 120.0);
    }
    assert!(scrolling_needs_frame(&scrolling));
    scrolling.edge_overscroll.mark_restored();
    assert!(!scrolling_needs_frame(&scrolling));
}

#[test]
fn edge_overscroll_moves_only_visual_frames_then_restores_authored_positions() {
    let mut commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::TouchpadDown,
        Event::Scroll { delta: -100.0 },
        Event::TouchpadUp,
    ];
    commands.extend((0..10).map(|_| Event::Command {
        command: Command::PrintState,
    }));

    TestHarness::new()
        .with_windows(2)
        .on_iteration(0, |world, _state| {
            world.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
                16,
            )));
        })
        .on_iteration(2, |world, _state| {
            let mut strip =
                world.query_filtered::<(&Position, &Scrolling), With<ActiveWorkspaceMarker>>();
            let (position, scrolling) = strip.single(world).expect("active scrolling workspace");
            assert_eq!(position.x, 0, "persisted strip position stays at the edge");
            assert_eq!(scrolling.position, 0.0);
            assert!(
                scrolling.target_position.is_none_or(|target| target == 0.0),
                "any remaining logical target stays clamped to the real edge"
            );
            let visual = super::overscroll::visual_offset(scrolling.edge_overscroll.visual());
            assert!(visual > 0);
            let mut visual_owner =
                world.query_filtered::<&EdgeOverscrollVisual, With<ActiveWorkspaceMarker>>();
            assert_eq!(
                visual_owner
                    .single(world)
                    .expect("strip owns transient visual")
                    .offset,
                visual
            );
        })
        .on_iteration(3, |world, state| {
            let mut windows =
                world.query::<(Entity, &Window, &Position, Has<VerifyWindowPosition>)>();
            assert!(
                windows
                    .iter(world)
                    .all(|(_, window, position, _)| window.frame().min == position.0),
                "transient window positions use the ordinary verified commit path"
            );
            assert!(
                windows
                    .iter(world)
                    .any(|(_, _, position, _)| (1..=44).contains(&position.x)),
                "layout materializes the transient offset without changing strip Position"
            );
            assert!(
                windows.iter(world).any(|(_, _, _, verifying)| verifying),
                "transient commits participate in the shared position verifier"
            );
            let delayed_position = windows
                .iter(world)
                .find_map(|(_, window, position, _)| (window.id() == 0).then_some(position.0))
                .expect("first window has a transient position");
            state.os_move_window(0, delayed_position);
        })
        .on_iteration(13, |world, _state| {
            let mut strip =
                world.query_filtered::<(&Position, &Scrolling), With<ActiveWorkspaceMarker>>();
            let (position, scrolling) = strip.single(world).expect("active scrolling workspace");
            assert_eq!(position.x, 0);
            assert_eq!(scrolling.position, 0.0);
            assert!(!scrolling.edge_overscroll.is_active());

            let mut windows = world.query::<(&Window, &Position)>();
            assert!(
                windows
                    .iter(world)
                    .all(|(window, position)| window.frame().min == position.0),
                "settlement restores exact authored window positions"
            );
        })
        .run(commands);
}

#[test]
fn mouse_down_removal_gets_a_zero_offset_restore_frame() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::TouchpadDown,
        Event::Scroll { delta: -100.0 },
        Event::MouseDown {
            point: CGPoint::new(100.0, 100.0),
            modifiers: Modifiers::empty(),
        },
        Event::Command {
            command: Command::PrintState,
        },
    ];

    TestHarness::new()
        .with_windows(2)
        .on_iteration(0, |world, _state| {
            world.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
                16,
            )));
        })
        .on_iteration(2, |world, _state| {
            assert!(
                world
                    .query::<&EdgeOverscrollVisual>()
                    .iter(world)
                    .next()
                    .is_some(),
                "edge gesture claims visual ownership before mouse cancellation"
            );
        })
        .on_iteration(4, |world, _state| {
            assert!(
                world
                    .query::<&EdgeOverscrollVisual>()
                    .iter(world)
                    .next()
                    .is_none(),
                "mouse-down cancellation completes the two-phase restore"
            );
            assert!(world.query::<&Scrolling>().iter(world).next().is_none());
            let mut strip = world.query_filtered::<&Position, With<ActiveWorkspaceMarker>>();
            assert_eq!(
                strip.single(world).expect("active strip").x,
                0,
                "persisted strip position never contains the transient offset"
            );
            let mut windows = world.query::<(&Window, &Position)>();
            assert!(
                windows
                    .iter(world)
                    .all(|(window, position)| window.frame().min == position.0),
                "final AX and authored window positions are canonical"
            );
        })
        .run(commands);
}

#[test]
fn sticky_scroll_snaps_on_both_sides_of_an_edge() {
    let viewport = IRect::new(0, 0, 1000, 800);
    let columns = [(0, 600), (600, 600)];
    assert_eq!(
        sticky_edge_snap_target(-631, &viewport, columns, 32),
        Some(-600)
    );
    assert_eq!(
        sticky_edge_snap_target(-169, &viewport, columns, 32),
        Some(-200)
    );
    assert_eq!(
        sticky_edge_snap_target(-591, &viewport, columns, 32),
        Some(-600)
    );
    assert_eq!(
        sticky_edge_snap_target(-209, &viewport, columns, 32),
        Some(-200)
    );
    assert_eq!(sticky_edge_snap_target(-567, &viewport, columns, 32), None);
    assert_eq!(sticky_edge_snap_target(-167, &viewport, columns, 32), None);
}

#[test]
fn sticky_scroll_returns_after_crossing_a_window_stop() {
    let viewport = IRect::new(0, 0, 1000, 800);
    let columns = [(0, 1000), (1000, 1000)];

    assert_eq!(
        sticky_edge_snap_target(-12, &viewport, columns, 32),
        Some(0)
    );
    assert_eq!(sticky_edge_snap_target(12, &viewport, columns, 32), Some(0));
    assert_eq!(
        sticky_edge_snap_target(-988, &viewport, columns, 32),
        Some(-1000)
    );
    assert_eq!(
        sticky_edge_snap_target(-1012, &viewport, columns, 32),
        Some(-1000)
    );
}

#[test]
fn sticky_scroll_uses_configured_edge_hit_zone() {
    let viewport = IRect::new(0, 0, 1000, 800);
    let columns = [(0, 600), (600, 600)];
    assert_eq!(sticky_edge_snap_target(-650, &viewport, columns, 32), None);
    assert_eq!(
        sticky_edge_snap_target(-650, &viewport, columns, 64),
        Some(-600)
    );
}

#[test]
fn sticky_release_zone_overrides_paging_snap_selection() {
    assert_eq!(snap_mode(true, true, false), SnapMode::Sticky);
    assert_eq!(snap_mode(true, false, false), SnapMode::Paging);

    let viewport = IRect::new(0, 0, 1000, 800);
    let columns = [(0, 1000), (1000, 1000)];
    assert_eq!(
        sticky_edge_snap_target(-500, &viewport, columns, 32),
        None,
        "paging may constrain the gesture, but sticky release must not snap from mid-strip"
    );
    assert_eq!(
        sticky_edge_snap_target(-968, &viewport, columns, 32),
        Some(-1000),
        "the combined mode still snaps inside the 32-point edge zone"
    );
}

#[test]
fn sticky_scroll_exposes_both_edges_of_an_oversized_column() {
    let viewport = IRect::new(0, 0, 1000, 800);
    let column = [(0, 1500)];
    assert_eq!(sticky_edge_snap_target(-9, &viewport, column, 32), Some(0));
    assert_eq!(
        sticky_edge_snap_target(-491, &viewport, column, 32),
        Some(-500)
    );
    assert_eq!(sticky_edge_snap_target(-250, &viewport, column, 32), None);
}

#[test]
fn scrolling_component_is_removed_after_integer_effective_dead_zone() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::PrintState,
        },
    ];
    TestHarness::new()
        .with_windows(3)
        .on_iteration(0, |world, _state| {
            world.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
                16,
            )));
            let entity = {
                let mut query = world.query_filtered::<Entity, With<ActiveWorkspaceMarker>>();
                query.single(world).expect("one active workspace")
            };
            world.entity_mut(entity).insert(Scrolling {
                position: 0.0,
                target_position: Some(-1.0),
                last_event: Instant::now()
                    .checked_sub(Duration::from_millis(100))
                    .expect("100ms must fit before now"),
                ..Default::default()
            });
        })
        .on_iteration(1, |world, _state| {
            let mut scrolling = world.query_filtered::<&Scrolling, With<ActiveWorkspaceMarker>>();
            assert!(scrolling.single(world).is_err());
            let mut positions = world.query_filtered::<&Position, With<ActiveWorkspaceMarker>>();
            assert_eq!(positions.single(world).expect("one active workspace").x, -1);
        })
        .run(commands);
}

#[test]
fn explicit_touchpad_contact_is_not_ended_by_inactivity_fallback() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::PrintState,
        },
        Event::TouchpadUp,
    ];
    TestHarness::new()
        .with_windows(1)
        .on_iteration(0, |world, _state| {
            let entity = {
                let mut query = world.query_filtered::<Entity, With<ActiveWorkspaceMarker>>();
                query.single(world).expect("one active workspace")
            };
            world.entity_mut(entity).insert(Scrolling {
                is_user_swiping: true,
                gesture_active: true,
                last_event: Instant::now()
                    .checked_sub(Duration::from_millis(100))
                    .expect("100ms must fit before now"),
                ..Default::default()
            });
        })
        .on_iteration(1, |world, _state| {
            let mut query = world.query_filtered::<&Scrolling, With<ActiveWorkspaceMarker>>();
            let scrolling = query
                .single(world)
                .expect("active contact keeps scrolling alive");
            assert!(scrolling.is_user_swiping);
            assert!(scrolling.gesture_active);
        })
        .on_iteration(2, |world, _state| {
            let mut query = world.query_filtered::<&Scrolling, With<ActiveWorkspaceMarker>>();
            assert!(query.single(world).is_err());
        })
        .run(commands);
}

#[test]
fn explicit_touchpad_begin_creates_lifecycle_state_before_first_delta() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::TouchpadDown,
        Event::Command {
            command: Command::PrintState,
        },
        Event::TouchpadUp,
    ];
    TestHarness::new()
        .with_windows(1)
        .on_iteration(2, |world, _state| {
            let mut query = world.query_filtered::<&Scrolling, With<ActiveWorkspaceMarker>>();
            let scrolling = query
                .single(world)
                .expect("touch begin must create scrolling lifecycle state");
            assert!(scrolling.is_user_swiping);
            assert!(scrolling.gesture_active);
            assert!(scrolling.paging_gesture.is_some());
            let scroll_focus_origin = scrolling
                .scroll_focus_origin
                .expect("user gesture must retain its initial focused window");
            assert!(
                world.get::<FocusedMarker>(scroll_focus_origin).is_some(),
                "captured scroll focus must match the focused window"
            );
        })
        .run(commands);
}

#[test]
fn later_native_momentum_keeps_original_one_hop_paging_session() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::Window(crate::commands::Operation::SetWidth(2.0)),
        },
        Event::TouchpadDown,
        Event::Scroll { delta: -100.0 },
        Event::TouchpadPhysicalUp,
        Event::TouchpadMomentumStart,
        Event::Scroll { delta: -100.0 },
        Event::TouchpadUp,
        Event::Command {
            command: Command::PrintState,
        },
    ];
    TestHarness::new()
        .with_windows(1)
        .on_iteration(2, |world, _state| {
            // Keep the physical-scroll target in flight across the lifecycle
            // events below. The harness otherwise advances 500ms per event,
            // enough for the native-scroll integrator to settle completely.
            world.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(1)));
            assert_original_oversized_paging_session(world);
        })
        .on_iteration(4, |world, _state| {
            let (_, _, target_position, gesture_active, is_user_swiping) = paging_snapshot(world);
            assert_original_oversized_paging_session(world);
            assert_eq!(
                target_position,
                Some(0.0),
                "physical touch end must retain the unfinished native-scroll target"
            );
            assert!(!gesture_active);
            assert!(is_user_swiping);
        })
        .on_iteration(5, |world, _state| {
            let (_, _, target_position, gesture_active, _) = paging_snapshot(world);
            assert_original_oversized_paging_session(world);
            assert_eq!(
                target_position,
                Some(0.0),
                "momentum start must continue the unfinished physical-scroll target"
            );
            assert!(gesture_active);
        })
        .on_iteration(6, |world, _state| {
            let (_, position, target_position, _, _) = paging_snapshot(world);
            assert_original_oversized_paging_session(world);
            assert!((-1024.0..=0.0).contains(&position));
            assert!(target_position.is_none_or(|target| (-1024.0..=0.0).contains(&target)));
        })
        .on_iteration(8, |world, _state| {
            let mut query = world.query_filtered::<&Position, With<ActiveWorkspaceMarker>>();
            let position = query.single(world).expect("active workspace").x;
            assert!(
                (-1024..=0).contains(&position),
                "native momentum must settle within the original one-hop bounds"
            );
        })
        .run(commands);
}

#[test]
fn touchpad_down_during_pending_snap_starts_from_reached_stop() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::Window(crate::commands::Operation::SetWidth(2.0)),
        },
        Event::TouchpadDown,
        Event::Scroll { delta: 100.0 },
    ];
    TestHarness::new()
        .with_windows(2)
        .on_iteration(1, |world, _state| {
            insert_pending_snap_to_first_edge(world, false);
        })
        .on_iteration(2, |world, _state| {
            let (paging, _, target_position, gesture_active, is_user_swiping) =
                paging_snapshot(world);
            assert_eq!(paging.start_stop, -1024.0);
            assert_eq!(paging.previous_stop, Some(0.0));
            assert!(paging.next_stop.is_some_and(|stop| stop < -1024.0));
            assert_eq!(target_position, None);
            assert!(gesture_active);
            assert!(is_user_swiping);
        })
        .on_iteration(3, |world, _state| {
            let (paging, position, target_position, _, _) = paging_snapshot(world);
            assert_eq!(paging.start_stop, -1024.0);
            assert!(
                position < -1024.0 || target_position.is_some_and(|target| target < -1024.0),
                "the first swipe of the new gesture must advance past the consumed edge"
            );
        })
        .run(commands);
}

#[test]
fn scroll_delta_during_pending_snap_advances_without_waiting_for_animation() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::Window(crate::commands::Operation::SetWidth(2.0)),
        },
        // Model a phase-less wheel tick or a batch where physical Began was
        // not observed separately from motion.
        Event::Scroll { delta: 100.0 },
    ];
    TestHarness::new()
        .with_windows(2)
        .on_iteration(1, |world, _state| {
            insert_pending_snap_to_first_edge(world, true);
        })
        .on_iteration(2, |world, _state| {
            let (paging, position, target_position, _, is_user_swiping) = paging_snapshot(world);
            assert_eq!(paging.start_stop, -1024.0);
            assert!(paging.next_stop.is_some_and(|stop| stop < -1024.0));
            assert!(
                position < -1024.0 || target_position.is_some_and(|target| target < -1024.0),
                "scroll input during snap animation must advance immediately"
            );
            assert!(is_user_swiping);
        })
        .run(commands);
}

fn insert_pending_snap_to_first_edge(world: &mut bevy::prelude::World, snap_pending: bool) {
    let entity = {
        let mut query = world.query_filtered::<Entity, With<ActiveWorkspaceMarker>>();
        query.single(world).expect("one active workspace")
    };
    world
        .entity_mut(entity)
        .get_mut::<Position>()
        .expect("active workspace position")
        .0
        .x = -900;
    world.entity_mut(entity).insert(Scrolling {
        position: -900.0,
        target_position: Some(-1024.0),
        snap_pending,
        paging_gesture: Some(crate::ecs::PagingGesture {
            start_stop: 0.0,
            previous_stop: None,
            next_stop: Some(-1024.0),
            release_velocity: 0.0,
        }),
        ..Default::default()
    });
}

#[test]
fn physical_up_and_momentum_start_in_same_update_leave_momentum_active() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::Window(crate::commands::Operation::SetWidth(2.0)),
        },
        Event::TouchpadDown,
        Event::Scroll { delta: -100.0 },
        Event::Command {
            command: Command::PrintState,
        },
    ];
    TestHarness::new()
        .with_windows(1)
        .on_iteration(3, |world, _state| {
            assert_original_oversized_paging_session(world);
            world.write_message(Event::TouchpadPhysicalUp);
            world.write_message(Event::TouchpadMomentumStart);
        })
        .on_iteration(4, |world, _state| {
            assert_original_oversized_paging_session(world);
            let (_, _, _, gesture_active, is_user_swiping) = paging_snapshot(world);
            assert!(gesture_active, "momentum start must win over physical end");
            assert!(is_user_swiping);
        })
        .run(commands);
}

fn assert_original_oversized_paging_session(world: &mut bevy::prelude::World) {
    let (paging, position, target_position, _, _) = paging_snapshot(world);
    assert_eq!(paging.start_stop, -1024.0);
    assert_eq!(paging.previous_stop, Some(0.0));
    assert_eq!(
        paging.next_stop,
        Some(-1024.0),
        "position={position}, target_position={target_position:?}"
    );
}

fn paging_snapshot(
    world: &mut bevy::prelude::World,
) -> (crate::ecs::PagingGesture, f64, Option<f64>, bool, bool) {
    let mut query = world.query_filtered::<&Scrolling, With<ActiveWorkspaceMarker>>();
    let scrolling = query
        .single(world)
        .expect("active workspace should be scrolling");
    (
        scrolling
            .paging_gesture
            .expect("paging session should remain captured"),
        scrolling.position,
        scrolling.target_position,
        scrolling.gesture_active,
        scrolling.is_user_swiping,
    )
}
