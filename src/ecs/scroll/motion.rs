//! Pure interpolation and subpixel reconciliation for horizontal scrolling.

const NATIVE_SCROLL_RESPONSE_SECONDS: f64 = 0.04;
const NATIVE_SCROLL_SETTLE_PX: f64 = 0.25;

pub(super) fn smooth_native_scroll(position: f64, target: f64, dt: f64) -> (f64, bool) {
    let blend = 1.0 - (-dt / NATIVE_SCROLL_RESPONSE_SECONDS).exp();
    let position = position + (target - position) * blend;

    if (target - position).abs() <= NATIVE_SCROLL_SETTLE_PX {
        (target, true)
    } else {
        (position, false)
    }
}

/// Preserve the integrator's subpixel remainder unless viewport constraints
/// actually changed the integer position that macOS can apply.
pub(super) fn reconcile_integrated_position(
    integrated_position: f64,
    effective_position: i32,
    clamped_position: i32,
) -> f64 {
    if effective_position == clamped_position {
        integrated_position
    } else {
        f64::from(clamped_position)
    }
}
