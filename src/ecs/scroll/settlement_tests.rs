use super::Scrolling;
use crate::ecs::layout::LayoutStrip;
use crate::ecs::systems::PendingScrollVerification;
use crate::ecs::{ActiveWorkspaceMarker, Position, VerifyWindowPosition};
use crate::events::Event;
use crate::manager::Window;
use crate::tests::TestHarness;
use bevy::app::Last;
use bevy::ecs::query::{Added, With};
use bevy::prelude::*;
use bevy::time::TimeUpdateStrategy;
use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Resource, Default)]
struct VerificationStarts(HashMap<Entity, usize>);

fn count_verification_starts(
    started: Query<Entity, Added<VerifyWindowPosition>>,
    mut count: ResMut<VerificationStarts>,
) {
    for entity in started.iter() {
        *count.0.entry(entity).or_default() += 1;
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn final_scroll_verification_waits_for_real_schedule_physical_settlement() {
    let mut harness = TestHarness::new().with_windows(3);
    harness
        .app
        .init_resource::<VerificationStarts>()
        .add_systems(Last, count_verification_starts);
    harness.run(vec![Event::MenuOpened { window_id: 0 }]);
    harness
        .app
        .world_mut()
        .resource_mut::<VerificationStarts>()
        .0
        .clear();
    harness
        .app
        .world_mut()
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            100,
        )));

    let (strip, windows, initial_strip_x) = {
        let world = harness.app.world_mut();
        let mut strips = world
            .query_filtered::<(Entity, &LayoutStrip, &Position), With<ActiveWorkspaceMarker>>();
        let (strip, layout, position) = strips.single(world).expect("one active strip");
        let windows = layout.windows().collect::<Vec<_>>();
        assert!(!windows.is_empty(), "managed windows in strip");
        (strip, windows, position.x)
    };
    for window in &windows {
        harness
            .app
            .world_mut()
            .entity_mut(*window)
            .remove::<VerifyWindowPosition>();
    }
    let window = windows[0];
    harness.app.world_mut().entity_mut(strip).insert(Scrolling {
        position: f64::from(initial_strip_x),
        target_position: Some(f64::from(initial_strip_x - 10)),
        last_event: Instant::now()
            .checked_sub(Duration::from_millis(100))
            .expect("100ms must fit before now"),
        ..Default::default()
    });

    // First tail update advances the strip but keeps the target alive. The
    // next Update translates that delta before the tail settles and removes
    // Scrolling while applying one final strip delta.
    harness.app.update();
    assert!(harness.app.world().entity(strip).contains::<Scrolling>());
    harness.app.update();

    let final_strip_x = harness.app.world().get::<Position>(strip).unwrap().x;
    let pre_settlement_position = harness.app.world().get::<Position>(window).unwrap().0;
    let pre_settlement_frame = harness.app.world().get::<Window>(window).unwrap().frame();
    assert_eq!(final_strip_x, initial_strip_x - 10);
    assert_ne!(
        pre_settlement_position.x, final_strip_x,
        "the regression must exercise a final strip delta not yet translated to the window"
    );
    assert_eq!(pre_settlement_frame.min, pre_settlement_position);
    assert!(!harness.app.world().entity(strip).contains::<Scrolling>());
    assert!(
        harness
            .app
            .world()
            .entity(window)
            .contains::<PendingScrollVerification>()
    );
    assert!(
        !harness
            .app
            .world()
            .entity(window)
            .contains::<VerifyWindowPosition>()
    );
    assert!(
        harness
            .app
            .world()
            .resource::<VerificationStarts>()
            .0
            .is_empty()
    );

    // The following real Update runs layout translation, animation/commit,
    // then releases exactly one final verifier from PostUpdate.
    harness.app.update();

    let settled_position = harness.app.world().get::<Position>(window).unwrap().0;
    let settled_frame = harness.app.world().get::<Window>(window).unwrap().frame();
    assert_eq!(settled_frame.min, settled_position);
    assert_eq!(settled_position.x, final_strip_x);
    assert!(
        harness
            .app
            .world()
            .entity(window)
            .contains::<VerifyWindowPosition>()
    );
    for window in &windows {
        assert_eq!(
            harness
                .app
                .world()
                .resource::<VerificationStarts>()
                .0
                .get(window),
            Some(&1),
            "each physically moved window gets one final verifier"
        );
    }

    harness.app.update();
    for window in &windows {
        assert_eq!(
            harness
                .app
                .world()
                .resource::<VerificationStarts>()
                .0
                .get(window),
            Some(&1),
            "settlement must not restart final verification"
        );
    }
}
