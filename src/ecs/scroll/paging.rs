//! Pure one-hop paging math for horizontal strip gestures.

use bevy::math::IRect;

use crate::ecs::{PagingGesture, Scrolling};

pub(super) const FLING_VELOCITY_THRESHOLD: f64 = 0.5;
const ADVANCE_RATIO: f64 = 0.25;
// Window positions are integrated as floats but ultimately applied in integer
// logical points. Treat sub-point drift as exact stop alignment so an already
// consumed stop is not reintroduced as a movement boundary.
const STOP_ALIGNMENT_EPSILON: f64 = 0.5;

pub(super) fn capture_gesture(
    current_position: f64,
    viewport: &IRect,
    columns: impl IntoIterator<Item = (i32, i32)>,
) -> Option<PagingGesture> {
    let stops = snap_stops(viewport, columns);
    let start_index = stops
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| {
            (current_position - **left)
                .abs()
                .total_cmp(&(current_position - **right).abs())
        })?
        .0;
    let start_stop = stops[start_index];
    let (previous_stop, next_stop) =
        if (current_position - start_stop).abs() <= STOP_ALIGNMENT_EPSILON {
            (
                start_index.checked_sub(1).map(|index| stops[index]),
                stops.get(start_index + 1).copied(),
            )
        } else {
            // The release target is still the nearest stop, but motion bounds
            // must be derived from the real gesture origin. If the strip starts
            // between two stops, deriving both neighbors from the nearest stop
            // marks that edge as already consumed and lets one direction skip it.
            (
                stops
                    .iter()
                    .copied()
                    .filter(|stop| *stop > current_position)
                    .min_by(f64::total_cmp),
                stops
                    .iter()
                    .copied()
                    .filter(|stop| *stop < current_position)
                    .max_by(f64::total_cmp),
            )
        };

    Some(PagingGesture {
        start_stop,
        previous_stop,
        next_stop,
        release_velocity: 0.0,
    })
}

pub(super) fn constrain_motion(
    scrolling: &mut Scrolling,
    direction_modifier: f64,
    user_input: bool,
) {
    let Some(paging) = scrolling.paging_gesture else {
        return;
    };
    let lower = paging.next_stop.unwrap_or(paging.start_stop);
    let upper = paging.previous_stop.unwrap_or(paging.start_stop);
    let previous_position = scrolling.position;
    let attempted_position = scrolling.target_position.unwrap_or(scrolling.position);
    let constrained_position = attempted_position.clamp(lower, upper);
    update_edge_overscroll(
        scrolling,
        attempted_position - constrained_position,
        paging,
        user_input,
    );
    scrolling.position = scrolling.position.clamp(lower, upper);
    if let Some(target) = scrolling.target_position.as_mut() {
        *target = target.clamp(lower, upper);
    }

    let coordinate_velocity = scrolling.velocity * direction_modifier;
    if (previous_position < lower && coordinate_velocity < 0.0)
        || (previous_position > upper && coordinate_velocity > 0.0)
    {
        scrolling.velocity = 0.0;
    }
}

fn update_edge_overscroll(
    scrolling: &mut Scrolling,
    overflow: f64,
    paging: PagingGesture,
    user_input: bool,
) {
    let at_application_edge = overflow > 0.0 && paging.previous_stop.is_none()
        || overflow < 0.0 && paging.next_stop.is_none();
    if overflow == 0.0 {
        if user_input && scrolling.edge_overscroll.is_active() {
            scrolling.edge_overscroll.cancel();
        }
    } else if user_input && at_application_edge {
        scrolling.edge_overscroll.apply_outward_input(overflow);
    }
}

/// Return reading-order paging stops. Numeric offsets decrease as the strip
/// advances to the right, so the result is sorted from greatest to smallest.
fn snap_stops(viewport: &IRect, columns: impl IntoIterator<Item = (i32, i32)>) -> Vec<f64> {
    let columns = columns
        .into_iter()
        .filter(|(_, width)| *width > 0)
        .collect::<Vec<_>>();
    let Some(content_min) = columns.iter().map(|(position, _)| *position).min() else {
        return Vec::new();
    };
    let content_max = columns
        .iter()
        .map(|(position, width)| position.saturating_add(*width))
        .max()
        .unwrap_or(content_min);
    let first_bound = viewport.min.x - content_min;
    let last_bound = viewport.max.x - content_max;
    let lower_bound = f64::from(first_bound.min(last_bound));
    let upper_bound = f64::from(first_bound.max(last_bound));

    let mut stops = columns
        .into_iter()
        .flat_map(|(position, width)| {
            let left_aligned = f64::from(viewport.min.x - position).clamp(lower_bound, upper_bound);
            let right_aligned =
                f64::from(viewport.max.x - position - width).clamp(lower_bound, upper_bound);
            if width > viewport.width() {
                [Some(left_aligned), Some(right_aligned)]
            } else {
                [Some(left_aligned), None]
            }
        })
        .flatten()
        .collect::<Vec<_>>();
    stops.sort_by(|left, right| right.total_cmp(left));
    stops.dedup();
    stops
}

pub(super) fn snap_target(
    current_position: f64,
    viewport_width: f64,
    paging: PagingGesture,
    snap_padding: i32,
) -> f64 {
    let edge_target = [
        Some(paging.start_stop),
        paging.previous_stop,
        paging.next_stop,
    ]
    .into_iter()
    .flatten()
    .filter(|stop| (current_position - *stop).abs() <= f64::from(snap_padding))
    .min_by(|left, right| {
        (current_position - *left)
            .abs()
            .total_cmp(&(current_position - *right).abs())
    });
    if let Some(target) = edge_target {
        return target;
    }

    let displacement = current_position - paging.start_stop;
    let displacement_neighbor = if displacement > 0.0 {
        paging.previous_stop
    } else if displacement < 0.0 {
        paging.next_stop
    } else {
        None
    };
    if let Some(neighbor) = displacement_neighbor {
        let threshold = ((paging.start_stop - neighbor).abs() * ADVANCE_RATIO)
            .min(viewport_width * ADVANCE_RATIO);
        if displacement.abs() >= threshold {
            return neighbor;
        }
    }

    let fling_neighbor = if paging.release_velocity > 0.0 {
        paging.previous_stop
    } else if paging.release_velocity < 0.0 {
        paging.next_stop
    } else {
        None
    };
    if paging.release_velocity.abs() >= FLING_VELOCITY_THRESHOLD
        && let Some(neighbor) = fling_neighbor
    {
        return neighbor;
    }

    paging.start_stop
}

pub(super) fn ready_to_snap(scrolling: &Scrolling) -> bool {
    !scrolling.gesture_active
        && !scrolling.is_user_swiping
        && scrolling.velocity.abs() <= FLING_VELOCITY_THRESHOLD
        && scrolling.target_position.is_none()
}

#[cfg(test)]
mod tests {
    use bevy::math::IRect;

    use super::{capture_gesture, constrain_motion, ready_to_snap, snap_stops, snap_target};
    use crate::ecs::scroll::overscroll::EDGE_OVERSCROLL_MAX;
    use crate::ecs::{PagingGesture, Scrolling};

    #[test]
    fn normal_has_one_stop_and_oversized_has_two() {
        let viewport = IRect::new(0, 0, 1000, 800);
        assert_eq!(
            snap_stops(&viewport, [(0, 600), (600, 1500), (2100, 600)]),
            vec![0.0, -600.0, -1100.0, -1700.0]
        );
    }

    #[test]
    fn arbitrarily_wide_column_still_has_exactly_two_stops() {
        let viewport = IRect::new(0, 0, 1000, 800);
        let stops = snap_stops(&viewport, [(0, 600), (600, 3500), (4100, 600)]);
        assert_eq!(stops, vec![0.0, -600.0, -3100.0, -3700.0]);
        assert_eq!(
            stops
                .iter()
                .filter(|stop| **stop <= -600.0 && **stop >= -3100.0)
                .count(),
            2
        );
    }

    #[test]
    fn neighborhood_is_ordered_and_reverse_symmetric() {
        let viewport = IRect::new(0, 0, 1000, 800);
        let columns = [(0, 600), (600, 1500), (2100, 600)];
        let left = capture_gesture(-600.0, &viewport, columns).unwrap();
        assert_eq!(
            (left.previous_stop, left.next_stop),
            (Some(0.0), Some(-1100.0))
        );
        let right = capture_gesture(-1100.0, &viewport, columns).unwrap();
        assert_eq!(
            (right.previous_stop, right.next_stop),
            (Some(-600.0), Some(-1700.0))
        );
        let sub_point_drift = capture_gesture(-599.75, &viewport, columns).unwrap();
        assert_eq!(
            (sub_point_drift.previous_stop, sub_point_drift.next_stop),
            (Some(0.0), Some(-1100.0))
        );
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn unsnapped_origin_cannot_skip_the_first_stop_in_either_direction() {
        let viewport = IRect::new(0, 0, 1000, 800);
        let columns = [(0, 600), (600, 1500), (2100, 600)];
        let paging = capture_gesture(-700.0, &viewport, columns).unwrap();
        assert_eq!(paging.start_stop, -600.0);
        assert_eq!(paging.previous_stop, Some(-600.0));
        assert_eq!(paging.next_stop, Some(-1100.0));

        let mut towards_previous = Scrolling {
            position: 500.0,
            target_position: Some(500.0),
            velocity: 2.0,
            paging_gesture: Some(paging),
            ..Default::default()
        };
        constrain_motion(&mut towards_previous, 1.0, false);
        assert_eq!(towards_previous.position, -600.0);
        assert_eq!(towards_previous.target_position, Some(-600.0));

        let mut towards_next = Scrolling {
            position: -5000.0,
            target_position: Some(-5000.0),
            velocity: -2.0,
            paging_gesture: Some(paging),
            ..Default::default()
        };
        constrain_motion(&mut towards_next, 1.0, false);
        assert_eq!(towards_next.position, -1100.0);
        assert_eq!(towards_next.target_position, Some(-1100.0));
    }

    #[test]
    fn motion_is_capped_at_adjacent_stops() {
        let mut scrolling = Scrolling {
            position: -5000.0,
            target_position: Some(-4000.0),
            velocity: -2.0,
            paging_gesture: Some(gesture()),
            ..Default::default()
        };
        constrain_motion(&mut scrolling, 1.0, false);
        assert_eq!(
            (
                scrolling.position,
                scrolling.target_position,
                scrolling.velocity
            ),
            (-1100.0, Some(-1100.0), 0.0)
        );

        scrolling.position = 5000.0;
        scrolling.target_position = Some(4000.0);
        scrolling.velocity = 2.0;
        constrain_motion(&mut scrolling, 1.0, false);
        assert_eq!(
            (
                scrolling.position,
                scrolling.target_position,
                scrolling.velocity
            ),
            (0.0, Some(0.0), 0.0)
        );
    }

    #[test]
    fn release_returns_or_advances_exactly_one_stop() {
        let paging = gesture();
        assert_eq!(snap_target(-700.0, 1000.0, paging, 32), -600.0);
        assert_eq!(snap_target(-730.0, 1000.0, paging, 32), -1100.0);
        assert_eq!(snap_target(-1080.0, 1000.0, paging, 32), -1100.0);
        assert_eq!(
            snap_target(
                -650.0,
                1000.0,
                PagingGesture {
                    release_velocity: -0.5,
                    ..paging
                },
                32,
            ),
            -1100.0
        );
        assert_eq!(
            snap_target(
                -970.0,
                1000.0,
                PagingGesture {
                    start_stop: -1100.0,
                    previous_stop: Some(-600.0),
                    next_stop: Some(-1700.0),
                    release_velocity: 0.0
                },
                32,
            ),
            -600.0
        );
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn larger_snap_padding_prefers_a_nearby_stop() {
        let paging = gesture();
        assert_eq!(snap_target(-730.0, 1000.0, paging, 32), -1100.0);
        assert_eq!(snap_target(-730.0, 1000.0, paging, 160), -600.0);
    }

    #[test]
    fn snap_waits_for_end_and_momentum_settlement() {
        let mut scrolling = Scrolling {
            gesture_active: true,
            is_user_swiping: true,
            ..Default::default()
        };
        assert!(!ready_to_snap(&scrolling));
        scrolling.gesture_active = false;
        assert!(!ready_to_snap(&scrolling));
        scrolling.is_user_swiping = false;
        scrolling.target_position = Some(-600.0);
        assert!(!ready_to_snap(&scrolling));
        scrolling.target_position = None;
        scrolling.velocity = 0.51;
        assert!(!ready_to_snap(&scrolling));
        scrolling.velocity = 0.5;
        assert!(ready_to_snap(&scrolling));
    }

    #[test]
    fn both_application_edges_resist_outward_motion_without_moving_logical_state() {
        for (paging, attempted, sign) in [
            (
                PagingGesture {
                    start_stop: 0.0,
                    previous_stop: None,
                    next_stop: Some(-600.0),
                    release_velocity: 0.0,
                },
                120.0,
                1.0,
            ),
            (
                PagingGesture {
                    start_stop: -600.0,
                    previous_stop: Some(0.0),
                    next_stop: None,
                    release_velocity: 0.0,
                },
                -720.0,
                -1.0,
            ),
        ] {
            let mut scrolling = Scrolling {
                position: attempted,
                target_position: Some(attempted),
                paging_gesture: Some(paging),
                ..Default::default()
            };
            constrain_motion(&mut scrolling, 1.0, true);
            assert_eq!(scrolling.position, paging.start_stop);
            assert_eq!(scrolling.target_position, Some(paging.start_stop));
            assert_eq!(scrolling.edge_overscroll.visual().signum(), sign);
            assert!(scrolling.edge_overscroll.visual().abs() < EDGE_OVERSCROLL_MAX);
        }
    }

    #[test]
    fn resistance_is_nonlinear_and_capped() {
        let paging = PagingGesture {
            start_stop: 0.0,
            previous_stop: None,
            next_stop: Some(-600.0),
            release_velocity: 0.0,
        };
        let mut scrolling = Scrolling {
            paging_gesture: Some(paging),
            ..Default::default()
        };
        let mut offsets = Vec::new();
        for delta in [12.0, 24.0, 96.0, 10_000.0] {
            scrolling.position = delta;
            constrain_motion(&mut scrolling, 1.0, true);
            offsets.push(scrolling.edge_overscroll.visual());
        }
        assert!(offsets.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(offsets[1] - offsets[0] > offsets[3] - offsets[2]);
        assert!(offsets[3] <= EDGE_OVERSCROLL_MAX);
    }

    #[test]
    fn release_returns_monotonically_and_settles_in_about_nine_sixty_hz_frames() {
        let paging = PagingGesture {
            start_stop: 0.0,
            previous_stop: None,
            next_stop: Some(-600.0),
            release_velocity: 0.0,
        };
        let mut scrolling = Scrolling {
            position: 10_000.0,
            paging_gesture: Some(paging),
            ..Default::default()
        };
        constrain_motion(&mut scrolling, 1.0, true);
        scrolling.edge_overscroll.release();
        let mut frames = 0;
        let mut previous = scrolling.edge_overscroll.visual();
        while scrolling.edge_overscroll.visual() != 0.0 {
            scrolling.edge_overscroll.integrate(1.0 / 60.0);
            assert!(scrolling.edge_overscroll.visual().abs() <= previous.abs());
            previous = scrolling.edge_overscroll.visual();
            frames += 1;
        }
        assert!((8..=9).contains(&frames), "settled after {frames} frames");
        assert_eq!(scrolling.edge_overscroll.visual(), 0.0);
        scrolling.edge_overscroll.mark_restored();
        assert!(!scrolling.edge_overscroll.is_active());
    }

    #[test]
    fn inward_input_cancels_overscroll_and_resumes_logical_motion() {
        let paging = PagingGesture {
            start_stop: 0.0,
            previous_stop: None,
            next_stop: Some(-600.0),
            release_velocity: 0.0,
        };
        let mut scrolling = Scrolling {
            position: 80.0,
            paging_gesture: Some(paging),
            ..Default::default()
        };
        constrain_motion(&mut scrolling, 1.0, true);
        assert!(scrolling.edge_overscroll.visual() > 0.0);

        scrolling.position = -40.0;
        constrain_motion(&mut scrolling, 1.0, true);
        assert_eq!(scrolling.position, -40.0);
        assert_eq!(scrolling.edge_overscroll.visual(), 0.0);
        scrolling.edge_overscroll.mark_restored();
        assert!(!scrolling.edge_overscroll.is_active());
    }

    #[test]
    fn cancel_clears_all_transient_motion() {
        let paging = PagingGesture {
            start_stop: 0.0,
            previous_stop: None,
            next_stop: Some(-600.0),
            release_velocity: 0.0,
        };
        let mut scrolling = Scrolling {
            position: 120.0,
            paging_gesture: Some(paging),
            ..Default::default()
        };
        constrain_motion(&mut scrolling, 1.0, true);
        scrolling.edge_overscroll.release();
        scrolling.edge_overscroll.cancel();
        assert_eq!(scrolling.edge_overscroll.visual(), 0.0);
        scrolling.edge_overscroll.mark_restored();
        assert!(!scrolling.edge_overscroll.is_active());
    }

    fn gesture() -> PagingGesture {
        PagingGesture {
            start_stop: -600.0,
            previous_stop: Some(0.0),
            next_stop: Some(-1100.0),
            release_velocity: 0.0,
        }
    }
}
