//! Contract tests for rebuilding the Managed native z-order after a confirmed
//! return from Passthrough focus.

use bevy::prelude::*;

use crate::assert_focused;
use crate::commands::Command;
use crate::ecs::{ActiveWorkspaceMarker, Unmanaged, WindowDisposition, layout::LayoutStrip};
use crate::events::Event;
use crate::manager::Window;

use super::*;

fn print_state_event() -> Event {
    Event::Command {
        command: Command::PrintState,
    }
}

fn make_window_passthrough(world: &mut World, window_id: i32) {
    let entity = find_window_entity(window_id, world);
    let mut strips = world.query::<&mut LayoutStrip>();
    for mut strip in strips.iter_mut(world) {
        strip.remove(entity);
    }
    world
        .entity_mut(entity)
        .insert((WindowDisposition::Passthrough, Unmanaged::Passthrough));
}

fn window_frames(world: &mut World) -> Vec<(i32, IRect)> {
    let mut frames = world
        .query::<&Window>()
        .iter(world)
        .map(|window| (window.id(), window.frame()))
        .collect::<Vec<_>>();
    frames.sort_unstable_by_key(|(window_id, _)| *window_id);
    frames
}

#[test]
fn raise_runs_after_target_confirmation_once_and_rearms_from_passthrough() {
    let mut harness = TestHarness::new().with_windows(4);
    harness.run(vec![print_state_event()]);
    make_window_passthrough(harness.world(), 3);

    let initial_frames = window_frames(harness.world());
    let initial_cursor = harness.mock_state.cursor_position();

    harness.mock_state.focus_window(3);
    harness.run(vec![print_state_event()]);
    harness.mock_state.clear_native_actions();

    harness.mock_state.focus_window(0);
    harness.run(vec![print_state_event()]);
    assert_eq!(
        harness.mock_state.native_actions(),
        vec![
            MockNativeAction::Focus(0),
            MockNativeAction::Raise(1),
            MockNativeAction::Raise(2),
        ],
        "the exact target focus must precede best-effort sibling raises"
    );
    assert_focused!(harness.world(), 0);
    assert_eq!(
        harness.mock_state.focused_window_id(TEST_PROCESS_ID),
        Some(0)
    );
    assert_eq!(window_frames(harness.world()), initial_frames);
    assert_eq!(harness.mock_state.cursor_position(), initial_cursor);

    harness.mock_state.clear_native_actions();
    harness.mock_state.focus_window(1);
    harness.run(vec![print_state_event()]);
    assert_eq!(
        harness.mock_state.native_actions(),
        vec![MockNativeAction::Focus(1)],
        "managed-to-managed focus must not repeat sibling raises"
    );

    harness.mock_state.clear_native_actions();
    harness.mock_state.focus_window(3);
    harness.run(vec![print_state_event()]);
    harness.mock_state.focus_window(2);
    harness.run(vec![print_state_event()]);
    assert_eq!(
        harness.mock_state.native_actions(),
        vec![
            MockNativeAction::Focus(3),
            MockNativeAction::Focus(2),
            MockNativeAction::Raise(0),
            MockNativeAction::Raise(1),
        ],
        "a later confirmed passthrough focus must rearm one later managed entry"
    );
}

#[test]
fn stale_or_unconfirmed_managed_focus_does_not_raise_siblings() {
    let mut harness = TestHarness::new().with_windows(4);
    harness.run(vec![print_state_event()]);
    make_window_passthrough(harness.world(), 3);

    harness.mock_state.focus_window(3);
    harness.run(vec![print_state_event()]);
    harness.mock_state.clear_native_actions();

    harness.run(vec![Event::WindowFocused { window_id: 0 }]);
    assert!(
        harness.mock_state.native_actions().is_empty(),
        "a stale event that fails the exact OS focus check must not raise"
    );

    harness.mock_state.set_auto_confirm_focus(false);
    harness.mock_state.focus_window(0);
    harness.run(vec![print_state_event()]);
    assert_eq!(
        harness.mock_state.native_actions(),
        vec![MockNativeAction::Focus(0)],
        "a native focus attempt without OS confirmation must not raise"
    );
    assert_focused!(harness.world(), 3);
}

#[test]
fn failed_and_nonexistent_siblings_are_ignored() {
    let mut harness = TestHarness::new().with_windows(4);
    harness.run(vec![print_state_event()]);
    make_window_passthrough(harness.world(), 3);

    let nonexistent = harness.world().spawn_empty().id();
    {
        let world = harness.world();
        let mut active_strip =
            world.query_filtered::<&mut LayoutStrip, With<ActiveWorkspaceMarker>>();
        active_strip
            .single_mut(world)
            .expect("one active strip")
            .append(nonexistent);
    }
    assert!(harness.world().despawn(nonexistent));
    harness.mock_state.fail_raise(1);

    harness.mock_state.focus_window(3);
    harness.run(vec![print_state_event()]);
    harness.mock_state.clear_native_actions();
    harness.mock_state.focus_window(0);
    harness.run(vec![print_state_event()]);

    assert_eq!(
        harness.mock_state.native_actions(),
        vec![
            MockNativeAction::Focus(0),
            MockNativeAction::Raise(1),
            MockNativeAction::Raise(2),
        ],
        "a failed sibling raise and stale strip entity must not stop later siblings"
    );
    assert_focused!(harness.world(), 0);
}
