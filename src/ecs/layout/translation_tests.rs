use super::*;
use bevy::app::{App, PostUpdate, Update};
use bevy::prelude::*;
use bevy::time::TimeUpdateStrategy;
use std::time::Duration;

fn setup_translation_app(scrolling: bool) -> (App, Entity, Entity) {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            100,
        )))
        .insert_resource(Config::default())
        .add_systems(
            Update,
            (
                layout_sizes_changed,
                clear_strip_translated_frames,
                position_layout_strips,
            )
                .chain(),
        )
        .add_systems(PostUpdate, crate::ecs::systems::animate_entities);
    let display = app
        .world_mut()
        .spawn(Display::new(1, IRect::new(0, 0, 1000, 800), 0))
        .id();
    let mut mock = crate::manager::MockWindowApi::new();
    mock.expect_horizontal_padding().return_const(0);
    let window = app
        .world_mut()
        .spawn((
            Window::new(Box::new(mock)),
            LayoutPosition(Origin::ZERO),
            Position(Origin::ZERO),
            Bounds(Size::new(400, 700)),
        ))
        .id();
    let mut layout = LayoutStrip::new(1, 0);
    layout.append(window);
    let mut strip = app
        .world_mut()
        .spawn((layout, Position(Origin::ZERO), ChildOf(display)));
    if scrolling {
        strip.insert(Scrolling::default());
    }
    let strip = strip.id();
    app.world_mut().clear_trackers();
    (app, strip, window)
}

#[test]
fn strip_window_context_captures_stack_membership_without_a_map() {
    let mut world = World::new();
    let entities = world.spawn_batch(vec![(), (), ()]).collect::<Vec<_>>();
    let mut strip = LayoutStrip::default();
    for entity in &entities {
        strip.append(*entity);
    }
    strip.stack(entities[1]).unwrap();
    let strip_position = Origin::new(10, 20);

    let stacked_leader = strip_window_context(&strip, entities[0], strip_position, true);
    let stacked_follower = strip_window_context(&strip, entities[1], strip_position, true);
    let single_window = strip_window_context(&strip, entities[2], strip_position, true);

    assert_eq!(stacked_leader.strip_position, strip_position);
    assert!(stacked_leader.swiping);
    assert!(stacked_leader.stacked);
    assert!(stacked_follower.stacked);
    assert!(!single_window.stacked);
}

#[test]
fn pure_strip_pan_preserves_logical_layout_and_does_not_dirty_strip() {
    let (mut app, strip, window) = setup_translation_app(true);
    app.world_mut().get_mut::<Position>(strip).unwrap().0.x = -100;

    app.update();

    assert_eq!(
        app.world().get::<LayoutPosition>(window).unwrap().0,
        Origin::ZERO
    );
    assert_eq!(
        app.world().get::<Position>(window).unwrap().0,
        Origin::new(-100, 0)
    );
    assert!(
        app.world()
            .entity(window)
            .contains::<StripTranslatedFrame>()
    );

    app.update();

    assert_eq!(
        app.world().get::<LayoutPosition>(window).unwrap().0,
        Origin::ZERO
    );
    assert!(
        !app.world()
            .entity(strip)
            .get_ref::<LayoutStrip>()
            .unwrap()
            .is_changed(),
        "pure translation must not feed physical Position back into LayoutStrip"
    );
}

#[test]
fn programmatic_strip_animation_does_not_dirty_logical_layout() {
    let (mut app, strip, window) = setup_translation_app(false);
    app.world_mut().get_mut::<Position>(strip).unwrap().0.x = -100;

    for _ in 0..10 {
        app.update();
    }

    assert_eq!(
        app.world().get::<LayoutPosition>(window).unwrap().0,
        Origin::ZERO
    );
    assert_eq!(
        app.world().get::<Position>(window).unwrap().0,
        Origin::new(-100, 0)
    );
    assert!(!app.world().entity(window).contains::<RepositionMarker>());
    assert!(
        !app.world()
            .entity(strip)
            .get_ref::<LayoutStrip>()
            .unwrap()
            .is_changed(),
        "programmatic strip animation must not feed physical Position back into LayoutStrip"
    );
}
