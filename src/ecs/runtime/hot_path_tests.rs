use std::time::Instant;

use super::{RuntimeActivity, RuntimeWork, pump_receiver_into, visit_relevant_frame_displays};
use crate::ecs::{ActiveDisplayMarker, LayoutStrip, Scrolling};
use crate::events::{Event, EventSender};
use crate::manager::Display;
use bevy::app::{App, Update};
use bevy::ecs::hierarchy::ChildOf;
use bevy::ecs::resource::Resource;
use bevy::ecs::system::ResMut;
use bevy::math::IRect;

#[test]
fn one_accumulator_preserves_pre_wait_and_handoff_order() {
    let (sender, receiver) = EventSender::new();
    sender.send(Event::WindowMoved { window_id: 7 }).unwrap();
    sender.send(Event::ApplicationActivated).unwrap();
    let activity = RuntimeActivity {
        frame_work: true,
        nearest_deadline: None,
    };

    let (mut accumulated, did_wait) =
        pump_receiver_into(&receiver, activity, Instant::now(), false, |_| {
            sender.send(Event::WindowMoved { window_id: 7 }).unwrap();
            sender.send(Event::ApplicationDeactivated).unwrap();
        });
    sender.send(Event::WindowMoved { window_id: 8 }).unwrap();
    sender.send(Event::ApplicationVisible { pid: 42 }).unwrap();
    accumulated.drain(&receiver);
    let (events, should_exit) = accumulated.finish();

    assert!(did_wait);
    assert!(!should_exit);
    assert_eq!(events.len(), 5);
    assert!(matches!(events[0], Event::ApplicationActivated));
    assert!(matches!(events[1], Event::WindowMoved { window_id: 7 }));
    assert!(matches!(events[2], Event::ApplicationDeactivated));
    assert!(matches!(events[3], Event::WindowMoved { window_id: 8 }));
    assert!(matches!(events[4], Event::ApplicationVisible { pid: 42 }));
}

#[derive(Resource, Default)]
struct CapturedFrameDisplays(Vec<u32>);

#[allow(clippy::needless_pass_by_value)]
fn capture_frame_displays(work: RuntimeWork, mut captured: ResMut<CapturedFrameDisplays>) {
    visit_relevant_frame_displays(&work, Instant::now(), |display_id| {
        captured.0.push(display_id);
    });
}

#[test]
fn scrolling_frame_is_owned_by_its_layout_display() {
    let mut app = App::new();
    app.init_resource::<CapturedFrameDisplays>()
        .add_systems(Update, capture_frame_displays);
    let active_display = app
        .world_mut()
        .spawn((
            Display::new(60, IRect::new(0, 0, 1_000, 800), 0),
            ActiveDisplayMarker,
        ))
        .id();
    let owner_display = app
        .world_mut()
        .spawn(Display::new(120, IRect::new(1_000, 0, 2_000, 800), 0))
        .id();
    let scrolling = app
        .world_mut()
        .spawn(Scrolling {
            velocity: 0.5,
            ..Default::default()
        })
        .id();
    let second_scrolling_window = app
        .world_mut()
        .spawn(Scrolling {
            target_position: Some(-100.0),
            ..Default::default()
        })
        .id();
    let mut strip = LayoutStrip::default();
    strip.append(scrolling);
    strip.append(second_scrolling_window);
    app.world_mut().spawn((strip, ChildOf(owner_display)));

    app.update();

    assert_eq!(app.world().resource::<CapturedFrameDisplays>().0, [120]);
    assert_ne!(owner_display, active_display);
}
