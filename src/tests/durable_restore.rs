//! Regression coverage for durable restore reconciliation and configuration changes.

use crate::config::Config;
use crate::ecs::layout::LayoutStrip;
use crate::ecs::restore::SessionRestore;
use crate::ecs::state::{PaneruState, SavedWindow};
use crate::ecs::{SpawnWindowTrigger, WidthRatio, WindowDisposition};
use crate::tests::state::extract_state;
use crate::tests::{
    TEST_DISPLAY_HEIGHT, TEST_PROCESS_ID, TEST_WINDOW_WIDTH, TEST_WORKSPACE_ID, TestHarness,
    find_window_entity,
};
use bevy::math::IRect;

fn pump(harness: &mut TestHarness, frames: usize) {
    for _ in 0..frames {
        for event in harness.mock_state.drain_events() {
            harness.world().write_message(event);
        }
        harness.app.update();
    }
}

fn with_catalog(count: i32) -> TestHarness {
    let mut harness = TestHarness::new().with_windows(count);
    pump(&mut harness, 10);
    let saved = extract_state(&mut harness, None);
    assert_eq!(saved.windows().count(), usize::try_from(count).unwrap());
    harness
        .world()
        .insert_resource(SessionRestore::from_live_snapshot(saved));
    harness
}

fn close(harness: &mut TestHarness, window_id: i32) {
    harness.mock_state.os_close_window(window_id);
    pump(harness, 5);
    let world = harness.world();
    assert!(
        world
            .query::<&crate::manager::Window>()
            .iter(world)
            .all(|window| window.id() != window_id),
        "the closed native window must also leave ECS"
    );
}

fn add_window(harness: &mut TestHarness, window_id: i32, identity: Option<&SavedWindow>) {
    let window = harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        TEST_WORKSPACE_ID,
        window_id,
        IRect::new(0, 20, TEST_WINDOW_WIDTH, TEST_DISPLAY_HEIGHT),
    );
    if let Some(saved) = identity {
        harness.mock_state.update_window(window_id, |window| {
            window.title.clone_from(&saved.title);
            window.identifier.clone_from(&saved.identifier);
            window.role.clone_from(&saved.role);
            window.subrole.clone_from(&saved.subrole);
        });
    }
    harness.world().trigger(SpawnWindowTrigger(vec![window]));
    pump(harness, 15);
}

fn save(harness: &mut TestHarness) -> PaneruState {
    let previous = harness
        .world()
        .resource::<SessionRestore>()
        .catalog()
        .clone();
    let live = extract_state(harness, Some(&previous));
    harness
        .world()
        .resource_mut::<SessionRestore>()
        .reconcile_persisted_state(live)
}

fn rows(harness: &mut TestHarness) -> Vec<(u32, Vec<i32>)> {
    let world = harness.world();
    let mut query = world.query::<&LayoutStrip>();
    query
        .iter(world)
        .filter(|strip| strip.id() == TEST_WORKSPACE_ID && !strip.all_windows().is_empty())
        .map(|strip| {
            (
                strip.virtual_index,
                strip
                    .all_windows()
                    .iter()
                    .map(|entity| world.get::<crate::manager::Window>(*entity).unwrap().id())
                    .collect(),
            )
        })
        .collect()
}

#[test]
fn reopening_saved_window_keeps_new_live_neighbor_in_same_row() {
    assert_reopening_keeps_new_neighbor(true);
}

#[test]
fn reopening_before_debounced_save_keeps_new_live_neighbor_in_same_row() {
    assert_reopening_keeps_new_neighbor(false);
}

fn assert_reopening_keeps_new_neighbor(save_before_return: bool) {
    let mut harness = with_catalog(2);
    let returning = harness
        .world()
        .resource::<SessionRestore>()
        .catalog()
        .windows()
        .find(|window| window.window_id == 1)
        .unwrap()
        .clone();
    close(&mut harness, 1);
    add_window(&mut harness, 2, None);
    let before = rows(&mut harness);
    assert_eq!(before.len(), 1);
    assert!(before[0].1.contains(&0) && before[0].1.contains(&2));
    if save_before_return {
        save(&mut harness);
    }

    add_window(&mut harness, 101, Some(&returning));
    let after = rows(&mut harness);
    assert_eq!(
        after,
        vec![(before[0].0, vec![0, 101, 2])],
        "returning B must not split A and newly managed C into separate rows"
    );
}

#[test]
fn disabled_restore_cannot_reclaim_managed_ownership() {
    let mut harness = with_catalog(1);
    let enabled = harness.world().resource::<Config>().clone();
    let mut saved = harness
        .world()
        .resource::<SessionRestore>()
        .catalog()
        .clone();
    let window = saved.windows_mut().next().unwrap();
    window.width_ratio = Some(1.5);
    let returning = window.clone();
    harness
        .world()
        .insert_resource(SessionRestore::from_live_snapshot(saved));
    close(&mut harness, 0);

    // Model the Config resource after a successful supported configuration reload.
    let disabled = Config::try_from("[options]\n[restore]\nenabled = false\n[bindings]\n")
        .expect("disabled restore config");
    assert!(!disabled.restore_enabled());
    harness.world().insert_resource(disabled);
    harness
        .world()
        .insert_resource(crate::ecs::DefaultWindowDisposition(
            WindowDisposition::Passthrough,
        ));

    add_window(&mut harness, 100, Some(&returning));
    let world = harness.world();
    let entity = find_window_entity(100, world);
    let disposition = world.get::<WindowDisposition>(entity).copied();
    let width = world
        .get::<crate::manager::Window>(entity)
        .unwrap()
        .frame()
        .width();
    assert_eq!(
        disposition,
        Some(WindowDisposition::Passthrough),
        "a retained durable catalog must respect the current enabled=false setting"
    );
    assert_eq!(width, TEST_WINDOW_WIDTH);

    // Disabling is a gate, not an opt-out: the saved slot must survive and
    // work again when the user re-enables restoration and the window returns.
    close(&mut harness, 100);
    harness.world().insert_resource(enabled);
    add_window(&mut harness, 101, Some(&returning));
    let world = harness.world();
    let entity = find_window_entity(101, world);
    assert_eq!(
        world.get::<WindowDisposition>(entity),
        Some(&WindowDisposition::Managed)
    );
    assert_eq!(world.get::<WidthRatio>(entity).unwrap().0, 1.5);
}

#[test]
fn late_restore_keeps_unsaved_order_and_widths_of_live_windows() {
    let mut harness = with_catalog(3);
    let mut catalog = harness
        .world()
        .resource::<SessionRestore>()
        .catalog()
        .clone();
    let returning = catalog
        .windows_mut()
        .find(|window| window.window_id == 1)
        .unwrap();
    returning.width_ratio = Some(1.5);
    let returning = returning.clone();
    harness
        .world()
        .insert_resource(SessionRestore::from_live_snapshot(catalog));
    close(&mut harness, 1);

    let world = harness.world();
    let first = find_window_entity(0, world);
    let last = find_window_entity(2, world);
    world.get_mut::<WidthRatio>(first).unwrap().0 = 1.75;
    let mut query = world.query::<&mut LayoutStrip>();
    let mut strip = query
        .iter_mut(world)
        .find(|strip| strip.contains(last))
        .unwrap();
    strip.remove(last);
    strip.insert_at(0, last);

    // No persistence pass between these live edits and the returning window.
    add_window(&mut harness, 101, Some(&returning));
    assert_eq!(rows(&mut harness), vec![(0, vec![2, 0, 101])]);
    let world = harness.world();
    let returning = find_window_entity(101, world);
    assert_eq!(world.get::<WidthRatio>(first).unwrap().0, 1.75);
    assert_eq!(world.get::<WidthRatio>(returning).unwrap().0, 1.5);
}

#[test]
fn partial_catalog_saves_newly_managed_windows() {
    let mut harness = with_catalog(2);
    close(&mut harness, 1);
    add_window(&mut harness, 2, None);
    let live_rows = rows(&mut harness);
    assert!(live_rows.iter().any(|(_, members)| members.contains(&2)));
    let saved = save(&mut harness);
    let ids = saved
        .windows()
        .map(|window| window.window_id)
        .collect::<Vec<_>>();
    assert!(ids.contains(&1), "keep the absent window's slot");
    assert!(
        ids.contains(&2),
        "also persist the newly managed live window"
    );
}

#[test]
fn partial_catalog_saves_reordered_surviving_windows() {
    let mut harness = with_catalog(3);
    close(&mut harness, 1);
    let world = harness.world();
    let moved = find_window_entity(2, world);
    let mut query = world.query::<&mut LayoutStrip>();
    let mut strip = query
        .iter_mut(world)
        .find(|strip| strip.contains(moved))
        .unwrap();
    strip.remove(moved);
    strip.insert_at(0, moved);
    pump(&mut harness, 5);
    let live_rows = rows(&mut harness);
    assert_eq!(live_rows[0].1, vec![2, 0]);
    let saved = save(&mut harness);
    let surviving_order = saved
        .windows()
        .map(|window| window.window_id)
        .filter(|id| *id != 1)
        .collect::<Vec<_>>();
    assert_eq!(
        surviving_order,
        vec![2, 0],
        "an absent neighbor must not freeze live order"
    );
}

#[test]
fn unrelated_sibling_cannot_replace_a_missing_saved_slot() {
    let mut harness = with_catalog(2);
    let original = harness
        .world()
        .resource::<SessionRestore>()
        .catalog()
        .windows()
        .find(|window| window.window_id == 0)
        .unwrap()
        .clone();
    close(&mut harness, 0);
    add_window(&mut harness, 2, None);
    let saved = save(&mut harness);
    assert_eq!(
        saved.windows().find(|window| window.window_id == 0),
        Some(&original)
    );
    assert!(saved.windows().any(|window| window.window_id == 1));
}
