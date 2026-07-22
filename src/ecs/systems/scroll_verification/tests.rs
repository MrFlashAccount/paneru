use super::*;
use bevy::app::{App, PostUpdate, Update};
use bevy::ecs::message::Messages;
use bevy::prelude::*;
use objc2_core_foundation::CGPoint;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::ecs::Bounds;
use crate::ecs::runtime::{RuntimeActivity, pump_receiver};
use crate::events::Event as PaneruEvent;
use crate::events::EventSender;
use crate::manager::{MockWindowApi, Origin, Size};
use crate::platform::Modifiers;

fn verification_app() -> App {
    let mut app = App::new();
    app.insert_non_send_resource(AxMainThread::for_tests())
        .init_resource::<Messages<PaneruEvent>>()
        .add_systems(Update, window_moved_update_frame)
        .add_systems(
            PostUpdate,
            (
                commit_window_position,
                finalize_scroll_verifications,
                verify_window_position,
            )
                .chain(),
        );
    app
}

#[test]
fn duplicate_self_echo_reads_position_once_and_never_writes_back() {
    let position_reads = Arc::new(AtomicUsize::new(0));
    let reposition_writes = Arc::new(AtomicUsize::new(0));
    let mut mock = MockWindowApi::new();
    mock.expect_id().return_const(11);
    mock.expect_update_position().returning({
        let position_reads = Arc::clone(&position_reads);
        move || {
            position_reads.fetch_add(1, Ordering::Relaxed);
            Ok(Origin::new(-100, 0))
        }
    });
    mock.expect_update_frame().returning(|| {
        Ok(IRect::from_corners(
            Origin::new(-100, 0),
            Origin::new(300, 700),
        ))
    });
    mock.expect_reposition().returning({
        let reposition_writes = Arc::clone(&reposition_writes);
        move |_| {
            reposition_writes.fetch_add(1, Ordering::Relaxed);
        }
    });

    let mut app = verification_app();
    let entity = app
        .world_mut()
        .spawn((
            Window::new(Box::new(mock)),
            Position(Origin::new(-100, 0)),
            Bounds(Size::new(400, 700)),
            WindowDisposition::Managed,
        ))
        .id();

    app.update();
    assert_eq!(reposition_writes.load(Ordering::Relaxed), 1);
    let (sender, receiver) = EventSender::new();
    sender
        .send(PaneruEvent::WindowMoved { window_id: 11 })
        .unwrap();
    sender
        .send(PaneruEvent::WindowMoved { window_id: 11 })
        .unwrap();
    let (events, should_exit, did_wait) = pump_receiver(
        &receiver,
        RuntimeActivity {
            frame_work: false,
            nearest_deadline: None,
        },
        Instant::now(),
        true,
        |_| panic!("queued native echoes must not wait"),
    );
    assert!(!should_exit);
    assert!(!did_wait);
    for event in events {
        app.world_mut().write_message::<PaneruEvent>(event);
    }
    app.update();

    assert_eq!(position_reads.load(Ordering::Relaxed), 1);
    assert_eq!(reposition_writes.load(Ordering::Relaxed), 1);
    assert!(app.world().entity(entity).contains::<AxPositionWrite>());
    assert!(
        app.world()
            .get::<AxPositionWrite>(entity)
            .unwrap()
            .latest_acknowledged
    );
    assert!(!app.world().entity(entity).contains::<AxObservedPosition>());

    app.world_mut()
        .get_mut::<VerifyWindowPosition>(entity)
        .expect("latest acknowledgement must retain bounded verification")
        .next_attempt = Instant::now();
    app.update();
    assert!(!app.world().entity(entity).contains::<AxPositionWrite>());
    assert!(
        !app.world()
            .entity(entity)
            .contains::<VerifyWindowPosition>()
    );
}

#[test]
fn acknowledged_latest_survives_authored_history_overflow_as_applied_anchor() {
    let oldest = Origin::new(-100, 0);
    let acknowledged = Origin::new(-200, 0);
    let mut authored = AxPositionWrite::new(oldest);

    authored.record(acknowledged);
    assert!(authored.acknowledge_latest());

    for x in 1..=AUTHORED_POSITION_HISTORY_CAPACITY + 1 {
        authored.record(Origin::new(-300 - i32::try_from(x).unwrap() * 10, 0));
    }

    assert!(
        !authored.positions[..usize::from(authored.len)].contains(&acknowledged),
        "the ring must actually overflow the acknowledged coordinate"
    );
    assert_eq!(authored.applied_anchor, Some(acknowledged));
    assert!(authored.contains(acknowledged));
}

#[test]
fn ax_lag_during_active_scroll_does_not_cancel_the_gesture() {
    let reposition_writes = Arc::new(AtomicUsize::new(0));
    let target = Origin::new(-800, 0);
    let lagged = Origin::new(-430, 0);
    let mut mock = MockWindowApi::new();
    mock.expect_id().return_const(14);
    mock.expect_update_position().returning(move || Ok(lagged));
    mock.expect_reposition().returning({
        let reposition_writes = Arc::clone(&reposition_writes);
        move |_| {
            reposition_writes.fetch_add(1, Ordering::Relaxed);
        }
    });

    let mut app = verification_app();
    let window = app
        .world_mut()
        .spawn((
            Window::new(Box::new(mock)),
            Position(target),
            Bounds(Size::new(400, 700)),
            WindowDisposition::Managed,
            PendingScrollVerification,
            AxPositionWrite::new(target),
        ))
        .id();
    let mut layout = LayoutStrip::default();
    layout.append(window);
    let strip = app
        .world_mut()
        .spawn((
            layout,
            Scrolling {
                gesture_active: true,
                is_user_swiping: true,
                target_position: Some(f64::from(target.x)),
                ..Default::default()
            },
        ))
        .id();
    app.update();
    reposition_writes.store(0, Ordering::Relaxed);
    app.world_mut().clear_trackers();

    app.world_mut()
        .write_message::<PaneruEvent>(PaneruEvent::WindowMoved { window_id: 14 });
    app.update();

    assert_eq!(app.world().get::<Position>(window).unwrap().0, target);
    assert!(app.world().entity(window).contains::<AxPositionWrite>());
    assert!(
        app.world()
            .entity(window)
            .contains::<PendingScrollVerification>()
    );
    assert!(app.world().entity(strip).contains::<Scrolling>());
    assert_eq!(reposition_writes.load(Ordering::Relaxed), 0);
}

#[test]
#[allow(clippy::too_many_lines)]
fn overflowed_authored_history_retains_rejected_physical_anchor_until_verification() {
    let position_reads = Arc::new(AtomicUsize::new(0));
    let reposition_writes = Arc::new(AtomicUsize::new(0));
    let rejected = Origin::new(-100, 0);
    let latest = Origin::new(-800, 0);
    let mut mock = MockWindowApi::new();
    mock.expect_id().return_const(13);
    mock.expect_update_position().returning({
        let position_reads = Arc::clone(&position_reads);
        move || {
            let read = position_reads.fetch_add(1, Ordering::Relaxed);
            Ok(if read == 0 { latest } else { rejected })
        }
    });
    mock.expect_update_frame().returning(move || {
        Ok(IRect::from_corners(
            rejected,
            rejected + Size::new(400, 700),
        ))
    });
    mock.expect_reposition().returning({
        let reposition_writes = Arc::clone(&reposition_writes);
        move |_| {
            reposition_writes.fetch_add(1, Ordering::Relaxed);
        }
    });

    let mut authored = AxPositionWrite::new(rejected);
    for x in 1..=70 {
        authored.record(Origin::new(-100 - x * 10, 0));
    }
    authored.record(latest);

    let mut app = verification_app();
    let window = app
        .world_mut()
        .spawn((
            Window::new(Box::new(mock)),
            Position(latest),
            Bounds(Size::new(400, 700)),
            WindowDisposition::Managed,
            PendingScrollVerification,
            authored,
        ))
        .id();
    let mut layout = LayoutStrip::default();
    layout.append(window);
    let strip = app
        .world_mut()
        .spawn((
            layout,
            Scrolling {
                gesture_active: true,
                is_user_swiping: true,
                ..Default::default()
            },
        ))
        .id();
    app.update();
    reposition_writes.store(0, Ordering::Relaxed);
    app.world_mut().clear_trackers();

    app.world_mut()
        .write_message::<PaneruEvent>(PaneruEvent::WindowMoved { window_id: 13 });
    app.update();
    assert_eq!(app.world().get::<Position>(window).unwrap().0, latest);
    assert!(app.world().entity(window).contains::<AxPositionWrite>());
    assert!(
        app.world()
            .get::<AxPositionWrite>(window)
            .unwrap()
            .latest_acknowledged
    );
    assert!(app.world().entity(strip).contains::<Scrolling>());
    assert_eq!(reposition_writes.load(Ordering::Relaxed), 0);

    for _ in 0..2 {
        app.world_mut()
            .write_message::<PaneruEvent>(PaneruEvent::WindowMoved { window_id: 13 });
        app.update();
        assert_eq!(app.world().get::<Position>(window).unwrap().0, latest);
        assert!(app.world().entity(window).contains::<AxPositionWrite>());
        assert!(
            app.world()
                .entity(window)
                .contains::<PendingScrollVerification>()
        );
        assert!(app.world().entity(strip).contains::<Scrolling>());
        assert_eq!(reposition_writes.load(Ordering::Relaxed), 0);
    }

    app.world_mut().entity_mut(strip).remove::<Scrolling>();
    app.update();
    assert!(
        app.world()
            .entity(window)
            .contains::<VerifyWindowPosition>()
    );
    for _ in 0..3 {
        app.world_mut()
            .get_mut::<VerifyWindowPosition>(window)
            .expect("verification must remain until its final attempt")
            .next_attempt = Instant::now();
        app.update();
    }

    assert_eq!(position_reads.load(Ordering::Relaxed), 3);
    assert_eq!(reposition_writes.load(Ordering::Relaxed), 2);
    assert_eq!(app.world().get::<Position>(window).unwrap().0, rejected);
    assert!(!app.world().entity(window).contains::<AxPositionWrite>());
    assert!(
        !app.world()
            .entity(window)
            .contains::<VerifyWindowPosition>()
    );
}

#[test]
fn external_move_cancels_stale_scroll_verification_without_snap_back() {
    let position_reads = Arc::new(AtomicUsize::new(0));
    let reposition_writes = Arc::new(AtomicUsize::new(0));
    let external = Origin::new(-260, 0);
    let mut mock = MockWindowApi::new();
    mock.expect_id().return_const(12);
    mock.expect_update_position().returning({
        let position_reads = Arc::clone(&position_reads);
        move || {
            position_reads.fetch_add(1, Ordering::Relaxed);
            Ok(external)
        }
    });
    mock.expect_reposition().returning({
        let reposition_writes = Arc::clone(&reposition_writes);
        move |_| {
            reposition_writes.fetch_add(1, Ordering::Relaxed);
        }
    });

    let mut app = verification_app();
    let window = app
        .world_mut()
        .spawn((
            Window::new(Box::new(mock)),
            Position(Origin::new(-100, 0)),
            Bounds(Size::new(400, 700)),
            WindowDisposition::Managed,
            PendingScrollVerification,
            VerifyWindowPosition::default(),
            AxPositionWrite::new(Origin::new(-100, 0)),
            RepositionMarker(Origin::new(-100, 0)),
        ))
        .id();
    let mut layout = LayoutStrip::default();
    layout.append(window);
    let strip = app
        .world_mut()
        .spawn((
            layout,
            Scrolling {
                gesture_active: true,
                is_user_swiping: true,
                velocity: 1.0,
                last_event: Instant::now()
                    .checked_sub(Duration::from_secs(5))
                    .expect("five seconds must fit before now"),
                ..Default::default()
            },
            RepositionMarker(Origin::new(-200, 0)),
        ))
        .id();
    app.world_mut().clear_trackers();
    app.world_mut()
        .write_message::<PaneruEvent>(PaneruEvent::MouseDown {
            point: CGPoint::new(120.0, 80.0),
            modifiers: Modifiers::empty(),
        });
    app.world_mut()
        .write_message::<PaneruEvent>(PaneruEvent::WindowMoved { window_id: 12 });

    app.update();

    assert_eq!(position_reads.load(Ordering::Relaxed), 1);
    assert_eq!(reposition_writes.load(Ordering::Relaxed), 0);
    assert_eq!(app.world().get::<Position>(window).unwrap().0, external);
    assert!(!app.world().entity(window).contains::<AxPositionWrite>());
    assert!(
        !app.world()
            .entity(window)
            .contains::<PendingScrollVerification>()
    );
    assert!(
        !app.world()
            .entity(window)
            .contains::<VerifyWindowPosition>()
    );
    assert!(!app.world().entity(strip).contains::<Scrolling>());
    assert!(!app.world().entity(strip).contains::<RepositionMarker>());

    for _ in 0..4 {
        app.update();
    }
    assert_eq!(app.world().get::<Position>(window).unwrap().0, external);
    assert_eq!(reposition_writes.load(Ordering::Relaxed), 0);
}
