//! Transient visual edge overscroll, kept separate from logical strip geometry.

use bevy::ecs::entity::Entity;
use bevy::ecs::query::With;
use bevy::ecs::system::{Commands, Populated};

use super::Scrolling;
use crate::ecs::layout::LayoutStrip;
use crate::ecs::{EdgeOverscrollPhase, EdgeOverscrollVisual, Position};

pub(super) const EDGE_OVERSCROLL_MAX: f64 = 36.0;
const RETURN_RESPONSE_SECONDS: f64 = 0.027;
const SETTLE_PX: f64 = 0.25;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct EdgeOverscroll {
    raw: f64,
    visual: f64,
    returning: bool,
    restore_pending: bool,
}

impl EdgeOverscroll {
    pub(super) fn apply_outward_input(&mut self, overflow: f64) {
        if self.returning {
            return;
        }
        self.restore_pending = false;
        self.raw += overflow;
        let magnitude = EDGE_OVERSCROLL_MAX * (1.0 - (-self.raw.abs() / EDGE_OVERSCROLL_MAX).exp());
        self.visual = magnitude.min(EDGE_OVERSCROLL_MAX).copysign(self.raw);
    }

    pub(super) fn release(&mut self) {
        self.raw = 0.0;
        self.returning = self.visual != 0.0;
    }

    pub(super) fn cancel(&mut self) {
        let restore_pending = self.visual != 0.0 || self.restore_pending;
        *self = Self {
            restore_pending,
            ..Self::default()
        };
    }

    pub(super) fn integrate(&mut self, dt: f64) {
        if !self.returning {
            return;
        }
        self.visual *= (-dt / RETURN_RESPONSE_SECONDS).exp();
        if self.visual.abs() <= SETTLE_PX {
            self.cancel();
        }
    }

    pub(crate) fn is_active(self) -> bool {
        self.visual != 0.0 || self.restore_pending
    }

    pub(super) fn visual(self) -> f64 {
        self.visual
    }

    pub(super) fn mark_restored(&mut self) {
        self.restore_pending = false;
    }
}

/// Publishes the visual-only displacement to the normal layout pipeline.
///
/// No AX call happens here. `position_layout_windows` turns the effective strip
/// position into window `Position` changes, which are then committed and
/// verified by the shared geometry path.
#[allow(clippy::needless_pass_by_value, clippy::type_complexity)]
pub(super) fn apply_edge_overscroll(
    mut strips: Populated<
        (
            Entity,
            &Position,
            Option<&mut Scrolling>,
            Option<&mut EdgeOverscrollVisual>,
        ),
        With<LayoutStrip>,
    >,
    mut commands: Commands,
) {
    for (entity, position, mut scrolling, visual) in &mut strips {
        let offset = scrolling
            .as_deref()
            .map_or(0, |scroll| scroll.edge_overscroll.visual().round() as i32);

        let Some(mut visual) = visual else {
            if offset == 0 {
                if let Some(scrolling) = scrolling.as_deref_mut() {
                    scrolling.edge_overscroll.mark_restored();
                }
            } else if let Ok(mut entity_commands) = commands.get_entity(entity) {
                entity_commands.try_insert(EdgeOverscrollVisual {
                    authored_position: position.0,
                    offset,
                    phase: EdgeOverscrollPhase::Applied,
                });
            }
            continue;
        };

        let authored_position_changed = visual.authored_position != position.0;
        let restore = authored_position_changed || scrolling.is_none() || offset == 0;
        if restore {
            if visual.phase == EdgeOverscrollPhase::Applied {
                visual.offset = 0;
                visual.phase = EdgeOverscrollPhase::RestoreQueued;
                if authored_position_changed && let Some(scrolling) = scrolling.as_deref_mut() {
                    scrolling.edge_overscroll.cancel();
                }
            } else {
                if let Some(scrolling) = scrolling.as_deref_mut() {
                    scrolling.edge_overscroll.mark_restored();
                }
                if let Ok(mut entity_commands) = commands.get_entity(entity) {
                    entity_commands.try_remove::<EdgeOverscrollVisual>();
                }
            }
        } else {
            if visual.phase == EdgeOverscrollPhase::RestoreQueued {
                visual.phase = EdgeOverscrollPhase::Applied;
            }
            if visual.offset != offset {
                visual.offset = offset;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use bevy::app::{App, Update};

    use super::apply_edge_overscroll;
    use crate::ecs::layout::LayoutStrip;
    use crate::ecs::{EdgeOverscrollPhase, EdgeOverscrollVisual, Position, Scrolling};
    use crate::manager::Origin;

    fn active_overscroll() -> Scrolling {
        let mut scrolling = Scrolling::default();
        scrolling.edge_overscroll.apply_outward_input(24.0);
        scrolling
    }

    #[test]
    fn removal_keeps_one_zero_offset_layout_frame_before_dropping_owner() {
        let mut app = App::new();
        app.add_systems(Update, apply_edge_overscroll);
        let strip = app
            .world_mut()
            .spawn((
                LayoutStrip::new(1, 0),
                Position(Origin::new(40, 20)),
                active_overscroll(),
            ))
            .id();

        app.update();
        let visual = *app
            .world()
            .get::<EdgeOverscrollVisual>(strip)
            .expect("first frame claims per-strip visual ownership");
        assert_ne!(
            visual.effective_position(Origin::new(40, 20)),
            Origin::new(40, 20)
        );

        app.world_mut().entity_mut(strip).remove::<Scrolling>();
        app.update();
        let visual = *app
            .world()
            .get::<EdgeOverscrollVisual>(strip)
            .expect("zero-offset owner survives the restore frame");
        assert_eq!(visual.phase, EdgeOverscrollPhase::RestoreQueued);
        assert_eq!(
            visual.effective_position(Origin::new(40, 20)),
            Origin::new(40, 20)
        );

        app.update();
        assert!(
            app.world().get::<EdgeOverscrollVisual>(strip).is_none(),
            "owner is removed only after a complete zero-offset schedule frame"
        );
    }

    #[test]
    fn external_strip_move_cancels_without_restoring_authored_position() {
        let mut app = App::new();
        app.add_systems(Update, apply_edge_overscroll);
        let strip = app
            .world_mut()
            .spawn((
                LayoutStrip::new(1, 0),
                Position(Origin::new(40, 20)),
                active_overscroll(),
            ))
            .id();
        let new_active_strip = app
            .world_mut()
            .spawn((LayoutStrip::new(2, 0), Position(Origin::new(-300, 20))))
            .id();

        app.update();
        app.world_mut()
            .entity_mut(strip)
            .insert(Position(Origin::new(2_000, 20)));
        app.world_mut().entity_mut(strip).remove::<Scrolling>();
        app.update();

        assert_eq!(
            app.world().get::<Position>(strip).unwrap().0,
            Origin::new(2_000, 20),
            "workspace/display ownership wins over stale overscroll cleanup"
        );
        let visual = *app
            .world()
            .get::<EdgeOverscrollVisual>(strip)
            .expect("owner remains for its zero-offset restore frame");
        assert_eq!(
            visual.effective_position(Origin::new(2_000, 20)),
            Origin::new(2_000, 20)
        );
        assert_eq!(
            app.world().get::<Position>(new_active_strip).unwrap().0,
            Origin::new(-300, 20),
            "cleanup ownership is isolated to the strip that authored the offset"
        );
        assert!(
            app.world()
                .get::<EdgeOverscrollVisual>(new_active_strip)
                .is_none()
        );

        app.update();
        assert_eq!(
            app.world().get::<Position>(strip).unwrap().0,
            Origin::new(2_000, 20)
        );
        assert!(app.world().get::<EdgeOverscrollVisual>(strip).is_none());
    }
}
