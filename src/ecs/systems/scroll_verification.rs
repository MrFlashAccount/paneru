use bevy::ecs::change_detection::{DetectChangesMut, Mut};
use bevy::ecs::component::Component;
use bevy::ecs::entity::Entity;
use bevy::ecs::message::MessageReader;
use bevy::ecs::query::{Changed, With, Without};
use bevy::ecs::system::{Commands, EntityCommands, NonSend, Populated, Query};
use bevy::math::IRect;
use tracing::{Level, instrument, trace, warn};

use crate::ecs::layout::LayoutStrip;
use crate::ecs::{
    AxObservedPosition, Bounds, EnsureVisibleMarker, Position, RepositionMarker,
    ReshuffleAroundMarker, ResizeMarker, Scrolling, Unmanaged, VerifyWindowPosition,
    WindowDisposition,
};
use crate::events::Event;
use crate::manager::{Origin, Window};
use crate::platform::AxMainThread;

/// A window moved during strip scrolling and needs one bounded verification
/// after that scrolling session settles, not one verification per frame.
#[derive(Component)]
pub(crate) struct PendingScrollVerification;

// At 240 Hz, 64 positions cover about 267 ms: longer than the three 50 ms AX
// verification attempts plus the 50 ms display-link safety timeout. The oldest
// and last physically observed authored anchors are retained separately, so a
// rejected physical position stays recognizable even after arbitrary ring overflow.
const AUTHORED_POSITION_HISTORY_CAPACITY: usize = 64;

/// Bounded AX positions authored by Paneru and still eligible to produce an
/// out-of-order `WindowMoved` acknowledgement. `latest` remains the active ECS
/// target; older entries only prevent stale self echoes from being mistaken for
/// external movement.
#[derive(Clone, Component, Copy)]
pub(crate) struct AxPositionWrite {
    latest: Origin,
    latest_acknowledged: bool,
    oldest: Origin,
    applied_anchor: Option<Origin>,
    positions: [Origin; AUTHORED_POSITION_HISTORY_CAPACITY],
    len: u8,
    next: u8,
}

impl AxPositionWrite {
    pub(crate) fn new(expected: Origin) -> Self {
        let mut positions = [Origin::ZERO; AUTHORED_POSITION_HISTORY_CAPACITY];
        positions[0] = expected;
        Self {
            latest: expected,
            latest_acknowledged: false,
            oldest: expected,
            applied_anchor: None,
            positions,
            len: 1,
            next: 1,
        }
    }

    pub(crate) fn record(&mut self, expected: Origin) {
        if self.latest != expected {
            self.latest = expected;
            self.latest_acknowledged = false;
        }
        if self.contains(expected) {
            return;
        }
        if usize::from(self.len) < AUTHORED_POSITION_HISTORY_CAPACITY {
            self.positions[usize::from(self.next)] = expected;
            self.len += 1;
            self.next = (self.next + 1) % AUTHORED_POSITION_HISTORY_CAPACITY as u8;
        } else {
            self.positions[usize::from(self.next)] = expected;
            self.next = (self.next + 1) % self.len;
        }
    }

    fn contains(&self, observed: Origin) -> bool {
        observed == self.oldest
            || self.applied_anchor == Some(observed)
            || self.positions[..usize::from(self.len)].contains(&observed)
    }

    fn remember_applied(&mut self, observed: Origin) {
        self.applied_anchor = Some(observed);
    }

    fn acknowledge_latest(&mut self) -> bool {
        self.remember_applied(self.latest);
        !std::mem::replace(&mut self.latest_acknowledged, true)
    }
}

/// Cancels every deferred write owned by a window's current geometry lifecycle.
///
/// Callers use this before ownership loss, native fullscreen, or shutdown so
/// `PostUpdate` cannot replay an animation or verifier after the transition.
pub(crate) fn cancel_window_geometry_ownership(entity: &mut EntityCommands<'_>) {
    entity
        .try_remove::<AxPositionWrite>()
        .try_remove::<PendingScrollVerification>()
        .try_remove::<VerifyWindowPosition>()
        .try_remove::<AxObservedPosition>()
        .try_remove::<RepositionMarker>()
        .try_remove::<ResizeMarker>()
        .try_remove::<EnsureVisibleMarker>()
        .try_remove::<ReshuffleAroundMarker>();
}

/// Cancels strip-level motion that could recreate window geometry work.
pub(crate) fn cancel_strip_geometry_ownership(entity: &mut EntityCommands<'_>) {
    entity
        .try_remove::<Scrolling>()
        .try_remove::<RepositionMarker>();
}

fn reposition_and_ack(
    entity: Entity,
    window: &mut Window,
    expected: Origin,
    authored_position: Option<&mut AxPositionWrite>,
    commands: &mut Commands,
) {
    window.reposition(expected);
    if let Some(authored_position) = authored_position {
        authored_position.record(expected);
    } else if let Ok(mut entity_commands) = commands.get_entity(entity) {
        entity_commands.try_insert(AxPositionWrite::new(expected));
    }
}

fn strip_motion_active(
    entity: Entity,
    strips: &Query<(Entity, &LayoutStrip, Option<&Scrolling>)>,
    now: std::time::Instant,
) -> bool {
    strips.iter().any(|(_, strip, scrolling)| {
        strip.contains(entity)
            && scrolling
                .is_some_and(|scrolling| crate::ecs::scroll::scrolling_needs_frame(scrolling, now))
    })
}

#[allow(clippy::needless_pass_by_value, clippy::type_complexity)]
#[instrument(level = Level::TRACE, skip_all)]
pub(crate) fn window_moved_update_frame(
    _main_thread: NonSend<AxMainThread>,
    mut messages: MessageReader<Event>,
    mut windows: Query<
        (
            Entity,
            &mut Window,
            &mut Position,
            &Bounds,
            Option<&Unmanaged>,
            Option<&VerifyWindowPosition>,
            Option<&PendingScrollVerification>,
            Option<&mut AxPositionWrite>,
        ),
        Without<LayoutStrip>,
    >,
    strips: Query<(Entity, &LayoutStrip, Option<&Scrolling>)>,
    mut commands: Commands,
) {
    let now = std::time::Instant::now();
    for event in messages.read() {
        let Event::WindowMoved { window_id } = event else {
            continue;
        };

        let Some((
            entity,
            mut window,
            mut position,
            bounds,
            unmanaged,
            verification,
            pending_scroll_verification,
            authored_position,
        )) = windows
            .iter_mut()
            .find(|window| window.1.id() == *window_id)
        else {
            continue;
        };
        if matches!(unmanaged, Some(Unmanaged::Minimized | Unmanaged::Hidden)) {
            continue;
        }

        let scroll_motion_active = strip_motion_active(entity, &strips, now);
        let authored_move_pending = authored_position.is_some();
        let observed_position = if let Some(mut authored_position) = authored_position {
            let observed_position = match window.update_position() {
                Ok(position) => position,
                Err(err) => {
                    warn!(
                        window_id,
                        error = %err,
                        "unable to read AX position while correlating authored move"
                    );
                    continue;
                }
            };
            if observed_position == authored_position.latest {
                let first_acknowledgement = authored_position.acknowledge_latest();
                trace!(
                    window_id,
                    ?observed_position,
                    first_acknowledgement,
                    "acknowledged latest AX position; retaining authored history until settlement"
                );
                continue;
            }
            if authored_position.contains(observed_position) {
                authored_position.remember_applied(observed_position);
                trace!(
                    window_id,
                    ?observed_position,
                    latest = ?authored_position.latest,
                    "ignoring out-of-order AX acknowledgement for an older authored position"
                );
                continue;
            }
            if scroll_motion_active {
                trace!(
                    window_id,
                    ?observed_position,
                    latest = ?authored_position.latest,
                    "deferring unmatched AX position until active scroll motion settles"
                );
                continue;
            }
            observed_position
        } else {
            let Ok(new_frame) = window.update_frame() else {
                continue;
            };
            new_frame.min
        };

        let old_frame = IRect::from_corners(position.0, position.0 + bounds.0);
        if old_frame.min == observed_position {
            continue;
        }
        position.0 = observed_position;
        if let Ok(mut entity_commands) = commands.get_entity(entity) {
            entity_commands
                .try_insert(AxObservedPosition(observed_position))
                .try_remove::<AxPositionWrite>()
                .try_remove::<PendingScrollVerification>()
                .try_remove::<VerifyWindowPosition>()
                .try_remove::<RepositionMarker>();
        }
        if authored_move_pending || pending_scroll_verification.is_some() || verification.is_some()
        {
            for (strip_entity, strip, _) in &strips {
                if strip.contains(entity)
                    && let Ok(mut strip_commands) = commands.get_entity(strip_entity)
                {
                    strip_commands
                        .try_remove::<Scrolling>()
                        .try_remove::<RepositionMarker>();
                }
            }
        }
    }
}

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
            Option<&mut AxPositionWrite>,
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
        authored_position,
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
            if let Ok(mut entity_commands) = commands.get_entity(entity) {
                cancel_window_geometry_ownership(&mut entity_commands);
            }
            continue;
        }
        reposition_and_ack(
            entity,
            &mut window,
            position.0,
            authored_position.map(Mut::into_inner),
            &mut commands,
        );
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
#[instrument(level = Level::TRACE, skip_all)]
pub(crate) fn verify_window_position(
    _main_thread: NonSend<AxMainThread>,
    mut windows: Populated<(
        Entity,
        &mut Window,
        &mut Position,
        &mut VerifyWindowPosition,
        Option<&WindowDisposition>,
        Option<&Unmanaged>,
        Option<&mut AxPositionWrite>,
    )>,
    mut commands: Commands,
) {
    let now = std::time::Instant::now();
    for (
        entity,
        mut window,
        mut position,
        mut verification,
        disposition,
        unmanaged,
        authored_position,
    ) in &mut windows
    {
        if !disposition
            .copied()
            .unwrap_or(WindowDisposition::Managed)
            .owns_geometry(unmanaged)
        {
            if let Ok(mut entity_commands) = commands.get_entity(entity) {
                cancel_window_geometry_ownership(&mut entity_commands);
            }
            continue;
        }
        if !verification.due(now) {
            continue;
        }
        let expected = position.0;
        match window.update_frame() {
            Ok(frame) if frame.min == expected => {
                if let Ok(mut entity_commands) = commands.get_entity(entity) {
                    entity_commands
                        .try_remove::<VerifyWindowPosition>()
                        .try_remove::<AxPositionWrite>();
                }
            }
            Ok(frame) if verification.tick() => {
                tracing::warn!(
                    window_id = window.id(),
                    ?expected,
                    actual = ?frame.min,
                    "window rejected the requested position; accepting the AX frame"
                );
                position.bypass_change_detection().0 = frame.min;
                if let Ok(mut entity_commands) = commands.get_entity(entity) {
                    entity_commands
                        .try_remove::<VerifyWindowPosition>()
                        .try_remove::<AxPositionWrite>();
                }
            }
            Ok(_) => reposition_and_ack(
                entity,
                &mut window,
                expected,
                authored_position.map(Mut::into_inner),
                &mut commands,
            ),
            Err(err) => {
                tracing::warn!(
                    window_id = window.id(),
                    "unable to verify window position: {err}"
                );
                if verification.tick() {
                    if let Ok(mut entity_commands) = commands.get_entity(entity) {
                        entity_commands
                            .try_remove::<VerifyWindowPosition>()
                            .try_remove::<AxPositionWrite>();
                    }
                } else {
                    reposition_and_ack(
                        entity,
                        &mut window,
                        expected,
                        authored_position.map(Mut::into_inner),
                        &mut commands,
                    );
                }
            }
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
            } else {
                cancel_window_geometry_ownership(&mut entity_commands);
            }
        }
    }
}

#[cfg(test)]
mod tests;
