use bevy::ecs::component::Component;
use bevy::ecs::entity::Entity;
use bevy::ecs::query::{Changed, With, Without};
use bevy::ecs::system::{Commands, NonSend, Populated, Query};
use tracing::{Level, instrument};

use crate::ecs::layout::LayoutStrip;
use crate::ecs::{
    AxObservedPosition, Position, RepositionMarker, Scrolling, Unmanaged, VerifyWindowPosition,
    WindowDisposition,
};
use crate::manager::Window;
use crate::platform::AxMainThread;

/// A window moved during strip scrolling and needs one bounded verification
/// after that scrolling session settles, not one verification per frame.
#[derive(Component)]
pub(crate) struct PendingScrollVerification;

#[allow(clippy::type_complexity)]
#[instrument(level = Level::TRACE, skip_all)]
pub(crate) fn commit_window_position(
    _main_thread: NonSend<AxMainThread>,
    mut moved_windows: Populated<
        (
            Entity,
            &mut Window,
            &Position,
            Option<&WindowDisposition>,
            Option<&Unmanaged>,
            Option<&RepositionMarker>,
            Option<&AxObservedPosition>,
            Option<&PendingScrollVerification>,
        ),
        Changed<Position>,
    >,
    scrolling_strips: Query<&LayoutStrip, With<Scrolling>>,
    directly_moved_strips: Query<&LayoutStrip, (Changed<Position>, Without<RepositionMarker>)>,
    mut commands: Commands,
) {
    let _reposition_batch = Window::reposition_batch();
    for (
        entity,
        mut window,
        position,
        disposition,
        unmanaged,
        repositioning,
        observed,
        pending_scroll_verification,
    ) in &mut moved_windows
    {
        if let Some(observed) = observed {
            if let Ok(mut entity_commands) = commands.get_entity(entity) {
                entity_commands
                    .try_remove::<AxObservedPosition>()
                    .try_remove::<VerifyWindowPosition>();
            }
            // The current position still matches the AX event, so this is external movement
            // (not a layout target that superseded it later in the same update). Preserve it
            // without echoing it back through AX and starting a verifier.
            if observed.0 == position.0 {
                continue;
            }
        }
        if !disposition
            .copied()
            .unwrap_or(WindowDisposition::Managed)
            .owns_geometry(unmanaged)
        {
            continue;
        }
        window.reposition(position.0);
        let scroll_settling = pending_scroll_verification.is_some()
            || scrolling_strips
                .iter()
                .any(|layout_strip| layout_strip.contains(entity))
            || directly_moved_strips
                .iter()
                .any(|layout_strip| layout_strip.contains(entity));
        if scroll_settling {
            if let Ok(mut entity_commands) = commands.get_entity(entity) {
                entity_commands
                    .try_insert(PendingScrollVerification)
                    .try_remove::<VerifyWindowPosition>();
            }
            continue;
        }
        // During an animation the marker remains until the final frame. Chained systems apply
        // its removal before this system runs, so only a completed/direct move starts the
        // acknowledgement window. Direct scrolling may refresh this component every frame; the
        // final position is verified once scrolling stops.
        if repositioning.is_none()
            && let Ok(mut entity_commands) = commands.get_entity(entity)
        {
            entity_commands.try_insert(VerifyWindowPosition::after_commit());
        }
    }
}

#[allow(clippy::needless_pass_by_value, clippy::type_complexity)]
pub(crate) fn finalize_scroll_verifications(
    pending: Populated<
        (Entity, Option<&WindowDisposition>, Option<&Unmanaged>),
        With<PendingScrollVerification>,
    >,
    scrolling_strips: Query<&LayoutStrip, With<Scrolling>>,
    moved_strips: Query<&LayoutStrip, Changed<Position>>,
    mut commands: Commands,
) {
    for (entity, disposition, unmanaged) in pending {
        if scrolling_strips
            .iter()
            .any(|layout_strip| layout_strip.contains(entity))
            || moved_strips
                .iter()
                .any(|layout_strip| layout_strip.contains(entity))
        {
            continue;
        }
        if let Ok(mut entity_commands) = commands.get_entity(entity) {
            entity_commands.try_remove::<PendingScrollVerification>();
            if disposition
                .copied()
                .unwrap_or(WindowDisposition::Managed)
                .owns_geometry(unmanaged)
            {
                entity_commands.try_insert(VerifyWindowPosition::after_commit());
            }
        }
    }
}
