//! Restores the Managed strip's native z-order after focus returns from a
//! Passthrough window.

use bevy::ecs::entity::Entity;
use bevy::ecs::resource::Resource;
use bevy::ecs::system::Query;
use tracing::debug;

use crate::ecs::WindowDisposition;
use crate::ecs::params::Windows;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ConfirmedFocusTier {
    #[default]
    Unknown,
    Managed,
    Passthrough,
    Other,
}

/// Tracks confirmed focus tier transitions, independently from requested
/// focus. A promotion is consumed only by an exact passthrough-to-managed
/// confirmation; duplicate, floating, and managed-to-managed confirmations
/// are no-ops.
#[derive(Default, Resource)]
pub(crate) struct ManagedTierRaiseState {
    confirmed: ConfirmedFocusTier,
}

impl ManagedTierRaiseState {
    fn confirm(&mut self, tier: ConfirmedFocusTier) -> bool {
        let promote = self.confirmed == ConfirmedFocusTier::Passthrough
            && tier == ConfirmedFocusTier::Managed;
        self.confirmed = tier;
        promote
    }
}

/// Records an authoritative focus confirmation and, on a genuine return to the
/// managed strip, raises each still-eligible sibling without activating it.
///
/// The caller must invoke this only after macOS reports both the target
/// application frontmost and the exact target window focused.
pub(crate) fn reconcile_confirmed_focus(
    target: Entity,
    owner_windows: Option<&[Entity]>,
    windows: &Windows<'_, '_>,
    dispositions: &Query<'_, '_, &WindowDisposition>,
    state: &mut ManagedTierRaiseState,
) {
    let target_is_managed = owner_windows.is_some()
        && matches!(dispositions.get(target), Ok(WindowDisposition::Managed))
        && windows
            .get_managed(target)
            .is_some_and(|(_, _, unmanaged)| unmanaged.is_none());
    let tier = match dispositions.get(target) {
        Ok(WindowDisposition::Managed) if target_is_managed => ConfirmedFocusTier::Managed,
        Ok(WindowDisposition::Passthrough) => ConfirmedFocusTier::Passthrough,
        _ => ConfirmedFocusTier::Other,
    };
    if !state.confirm(tier) {
        return;
    }

    let Some(owner_windows) = owner_windows else {
        return;
    };
    for &entity in owner_windows {
        if entity == target || !matches!(dispositions.get(entity), Ok(WindowDisposition::Managed)) {
            continue;
        }
        let Some((window, _, unmanaged)) = windows.get_managed(entity) else {
            continue;
        };
        if unmanaged.is_some() {
            debug!(
                window_id = window.id(),
                ?unmanaged,
                "skipping ineligible managed-tier sibling"
            );
            continue;
        }
        let _ = window.raise_without_focus();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn promotion_is_one_shot_until_passthrough_focus_returns() {
        let mut state = ManagedTierRaiseState::default();

        assert!(!state.confirm(ConfirmedFocusTier::Managed));
        assert!(!state.confirm(ConfirmedFocusTier::Passthrough));
        assert!(state.confirm(ConfirmedFocusTier::Managed));
        assert!(!state.confirm(ConfirmedFocusTier::Managed));
        assert!(!state.confirm(ConfirmedFocusTier::Passthrough));
        assert!(state.confirm(ConfirmedFocusTier::Managed));
    }

    #[test]
    fn floating_focus_does_not_arm_managed_tier_raise() {
        let mut state = ManagedTierRaiseState::default();

        assert!(!state.confirm(ConfirmedFocusTier::Passthrough));
        assert!(!state.confirm(ConfirmedFocusTier::Other));
        assert!(!state.confirm(ConfirmedFocusTier::Managed));
    }
}
