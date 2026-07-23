//! Transient visual edge overscroll, kept separate from logical strip geometry.

use bevy::ecs::entity::Entity;
use bevy::ecs::query::With;
use bevy::ecs::system::{Commands, Populated};

use super::Scrolling;
use crate::ecs::layout::LayoutStrip;
use crate::ecs::{EdgeOverscrollPhase, EdgeOverscrollVisual, Position};

pub(super) const EDGE_OVERSCROLL_MAX: f64 = 44.0;
const RETURN_SECONDS: f64 = 0.11;
const MAX_RAW_PULL: f64 = 1_000_000.0;

/// Physical-contact-owned visual displacement at an application edge.
///
/// The input latch is deliberately independent of paging's `gesture_active`:
/// native momentum reactivates that paging flag after physical lift, but must
/// never regain ownership of the rubber band.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct EdgeOverscroll {
    raw_pull: f64,
    visual: f64,
    direction: f64,
    accepts_physical_input: bool,
    returning: bool,
    return_start: f64,
    elapsed: f64,
    restore_pending: bool,
}

impl EdgeOverscroll {
    pub(super) fn armed() -> Self {
        Self {
            accepts_physical_input: true,
            ..Self::default()
        }
    }

    pub(super) fn apply_outward_input(&mut self, delta: f64) -> bool {
        if !self.accepts_physical_input || self.returning || delta == 0.0 || !delta.is_finite() {
            return false;
        }

        if self.raw_pull != 0.0 && self.direction.is_sign_positive() != delta.is_sign_positive() {
            self.cancel_pull();
            return false;
        }

        self.direction = delta.signum();
        self.raw_pull = (self.raw_pull + delta.abs()).min(MAX_RAW_PULL);
        self.visual = rubber_band(self.raw_pull).copysign(self.direction);
        // A new non-zero offset supersedes the old return frame. Keeping the
        // pending restore here would poll display frames throughout a static
        // held pull; a later cancel/release establishes its own restore.
        self.restore_pending = false;
        true
    }

    pub(super) fn rearm(&mut self) {
        self.cancel_pull();
        self.accepts_physical_input = true;
    }

    /// Cancels the current pull without closing input for this physical touch.
    pub(super) fn cancel_pull(&mut self) {
        self.restore_pending |= self.visual != 0.0;
        self.raw_pull = 0.0;
        self.visual = 0.0;
        self.direction = 0.0;
        self.returning = false;
        self.return_start = 0.0;
        self.elapsed = 0.0;
    }

    /// Atomically closes physical input and starts at most one finite return.
    pub(super) fn release(&mut self) -> bool {
        self.accepts_physical_input = false;
        self.raw_pull = 0.0;
        self.direction = 0.0;
        if self.returning || self.visual == 0.0 {
            return false;
        }

        self.returning = true;
        self.return_start = self.visual;
        self.elapsed = 0.0;
        true
    }

    pub(super) fn integrate(&mut self, dt: f64) {
        if !self.returning {
            return;
        }

        self.elapsed = (self.elapsed + dt.max(0.0)).min(RETURN_SECONDS);
        if self.elapsed >= RETURN_SECONDS {
            self.restore_pending |= self.visual != 0.0 || self.return_start != 0.0;
            self.visual = 0.0;
            self.returning = false;
            self.return_start = 0.0;
            self.elapsed = 0.0;
        } else {
            let remaining = 1.0 - self.elapsed / RETURN_SECONDS;
            self.visual = self.return_start * remaining.powi(3);
        }
    }

    pub(crate) fn is_active(self) -> bool {
        self.visual != 0.0 || self.returning || self.restore_pending
    }

    pub(super) fn needs_frame(self) -> bool {
        self.returning || self.restore_pending
    }

    pub(super) fn accepts_input(self) -> bool {
        self.accepts_physical_input
    }

    #[cfg(test)]
    pub(super) fn is_returning(self) -> bool {
        self.returning
    }

    pub(super) fn visual(self) -> f64 {
        self.visual
    }

    pub(super) fn mark_restored(&mut self) {
        self.restore_pending = false;
    }
}

fn rubber_band(raw_pull: f64) -> f64 {
    EDGE_OVERSCROLL_MAX * raw_pull / (EDGE_OVERSCROLL_MAX + raw_pull)
}

pub(super) fn visual_offset(visual: f64) -> i32 {
    if visual > 0.0 {
        visual.ceil() as i32
    } else {
        visual.floor() as i32
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
            .map_or(0, |scroll| visual_offset(scroll.edge_overscroll.visual()));

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
                    scrolling.edge_overscroll.cancel_pull();
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

    use super::{
        EDGE_OVERSCROLL_MAX, EdgeOverscroll, RETURN_SECONDS, apply_edge_overscroll, visual_offset,
    };
    use crate::ecs::layout::LayoutStrip;
    use crate::ecs::{EdgeOverscrollPhase, EdgeOverscrollVisual, Position, Scrolling};
    use crate::manager::Origin;

    fn active_overscroll() -> Scrolling {
        let mut scrolling = Scrolling {
            edge_overscroll: EdgeOverscroll::armed(),
            ..Default::default()
        };
        scrolling.edge_overscroll.apply_outward_input(24.0);
        scrolling
    }

    #[test]
    fn first_subpoint_pull_is_visible_on_both_edges() {
        assert_eq!(visual_offset(0.001), 1);
        assert_eq!(visual_offset(-0.001), -1);
        assert_eq!(visual_offset(0.0), 0);
    }

    #[test]
    fn resistance_is_near_linear_then_progressively_harder_and_capped_on_both_edges() {
        for direction in [-1.0, 1.0] {
            let mut band = EdgeOverscroll::armed();
            let mut ratios = Vec::new();
            let mut total = 0.0;
            for delta in [0.5, 3.5, 12.0, 48.0, 192.0, 4096.0] {
                assert!(band.apply_outward_input(delta * direction));
                total += delta;
                let visual = band.visual().abs();
                ratios.push(visual / total);
                assert_eq!(
                    band.visual().is_sign_positive(),
                    direction.is_sign_positive()
                );
                assert!(visual < EDGE_OVERSCROLL_MAX);
            }
            assert!(ratios[0] > 0.98, "near-zero pull must stay close to 1:1");
            assert!(ratios.windows(2).all(|pair| pair[0] > pair[1]));
            assert!(band.visual().abs() > 43.0);
        }
    }

    #[test]
    fn release_is_idempotent_and_blocks_later_momentum_input() {
        let mut band = EdgeOverscroll::armed();
        assert!(band.apply_outward_input(80.0));
        assert!(band.release());
        let return_start = band.visual();
        band.integrate(0.025);
        let after_first_step = band.visual();
        assert!(after_first_step < return_start);

        assert!(!band.release());
        assert!(!band.apply_outward_input(10_000.0));
        assert_eq!(band.visual(), after_first_step);
        assert!(!band.accepts_input());
    }

    #[test]
    fn finite_return_reaches_exact_zero_at_common_refresh_rates() {
        for hz in [60.0, 120.0, 240.0] {
            let dt = 1.0 / hz;
            let mut band = EdgeOverscroll::armed();
            assert!(band.apply_outward_input(-144.0));
            assert!(band.release());

            let mut previous = band.visual().abs();
            let mut elapsed = 0.0;
            while band.is_returning() {
                band.integrate(dt);
                elapsed += dt;
                assert!(band.visual().abs() <= previous);
                previous = band.visual().abs();
            }
            assert_eq!(band.visual(), 0.0);
            assert!(elapsed >= RETURN_SECONDS);
            assert!(elapsed <= RETURN_SECONDS + dt);
            assert!(band.needs_frame());
            band.mark_restored();
            assert!(!band.needs_frame());
            assert!(!band.is_active());
        }
    }

    #[test]
    fn rearm_interrupts_return_without_polling_a_static_held_pull() {
        let mut band = EdgeOverscroll::armed();
        assert!(band.apply_outward_input(36.0));
        assert!(band.release());
        band.integrate(0.025);

        band.rearm();
        assert!(band.accepts_input());
        assert!(!band.is_returning());
        assert_eq!(band.visual(), 0.0);
        assert!(band.apply_outward_input(-8.0));
        assert!(band.visual() < 0.0);
        assert!(!band.needs_frame());
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
