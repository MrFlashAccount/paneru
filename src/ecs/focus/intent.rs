//! Generation-checked state for native focus requests and deferred fallback.

use std::time::{Duration, Instant};

use bevy::ecs::entity::Entity;
use bevy::ecs::resource::Resource;

use crate::platform::WinID;

const FOCUS_RETRY_DELAY: Duration = Duration::from_millis(40);
const FOCUS_CONFIRM_TIMEOUT: Duration = Duration::from_millis(300);
const SAME_APP_ACTIVATION_DELAY: Duration = Duration::from_millis(20);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FocusRequestPolicy {
    RaiseNow,
    NoRaise,
    RaiseAfterScroll,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PendingFocusIntent {
    pub(super) generation: u64,
    pub(super) entity: Entity,
    pub(super) window_id: WinID,
    pub(super) retry_at: Instant,
    pub(super) expires_at: Instant,
    pub(super) activation_at: Option<Instant>,
    pub(super) retried: bool,
    pub(super) policy: FocusRequestPolicy,
    pub(super) suppress_side_effects: bool,
}

/// Separates the latest requested native focus from the last focus confirmed
/// by macOS. Replacing `pending` cancels callbacks and watchdog work belonging
/// to an older generation.
#[derive(Default, Resource)]
pub(crate) struct FocusIntentState {
    generation: u64,
    pending: Option<PendingFocusIntent>,
}

impl FocusIntentState {
    pub(crate) fn request(
        &mut self,
        entity: Entity,
        window_id: WinID,
        policy: FocusRequestPolicy,
        suppress_side_effects: bool,
        now: Instant,
    ) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.pending = Some(PendingFocusIntent {
            generation: self.generation,
            entity,
            window_id,
            retry_at: now + FOCUS_RETRY_DELAY,
            expires_at: now + FOCUS_CONFIRM_TIMEOUT,
            activation_at: None,
            retried: false,
            policy,
            suppress_side_effects,
        });
        self.generation
    }

    pub(crate) fn effective_entity(&self, confirmed: Option<Entity>) -> Option<Entity> {
        self.pending
            .map_or(confirmed, |pending| Some(pending.entity))
    }

    pub(crate) fn pending(&self) -> Option<PendingFocusIntent> {
        self.pending
    }

    pub(crate) fn pending_suppresses_side_effects(&self) -> bool {
        self.pending
            .is_some_and(|pending| pending.suppress_side_effects)
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.pending.map(|pending| {
            let focus_deadline = if pending.retried {
                pending.expires_at
            } else {
                pending.retry_at
            };
            pending
                .activation_at
                .map_or(focus_deadline, |activation_at| {
                    activation_at.min(focus_deadline)
                })
        })
    }

    pub(super) fn schedule_same_app_activation(&mut self, generation: u64, now: Instant) -> bool {
        let Some(pending) = self.pending.as_mut() else {
            return false;
        };
        if pending.generation != generation {
            return false;
        }
        pending.activation_at = Some(now + SAME_APP_ACTIVATION_DELAY);
        true
    }

    pub(super) fn take_due_same_app_activation(&mut self, generation: u64, now: Instant) -> bool {
        let Some(pending) = self.pending.as_mut() else {
            return false;
        };
        if pending.generation != generation
            || pending
                .activation_at
                .is_none_or(|activation_at| now < activation_at)
        {
            return false;
        }
        pending.activation_at = None;
        true
    }

    pub(crate) fn confirmation_policy(&mut self, window_id: WinID) -> Option<bool> {
        let Some(pending) = self.pending else {
            return Some(false);
        };
        if pending.window_id != window_id {
            return None;
        }
        self.pending = None;
        Some(pending.suppress_side_effects)
    }

    pub(super) fn mark_retried(&mut self, generation: u64, now: Instant) -> bool {
        let Some(pending) = self.pending.as_mut() else {
            return false;
        };
        if pending.generation != generation {
            return false;
        }
        pending.retried = true;
        pending.expires_at = now + FOCUS_CONFIRM_TIMEOUT;
        true
    }

    pub(super) fn defer_retry(&mut self, generation: u64, now: Instant) -> bool {
        let Some(pending) = self.pending.as_mut() else {
            return false;
        };
        if pending.generation != generation {
            return false;
        }
        pending.retry_at = now + FOCUS_RETRY_DELAY;
        true
    }

    pub(super) fn clear_generation(&mut self, generation: u64) -> bool {
        if self
            .pending
            .is_some_and(|pending| pending.generation == generation)
        {
            self.pending = None;
            true
        } else {
            false
        }
    }

    pub(super) fn clear_superseded(&mut self) {
        self.pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_app_activation_precedes_retry_without_blocking() {
        let now = Instant::now();
        let mut intent = FocusIntentState::default();
        let generation = intent.request(
            Entity::from_raw_u32(1).expect("entity"),
            7,
            FocusRequestPolicy::RaiseAfterScroll,
            true,
            now,
        );

        assert!(intent.schedule_same_app_activation(generation, now));
        assert_eq!(
            intent.next_deadline(),
            Some(now + SAME_APP_ACTIVATION_DELAY)
        );
        assert!(!intent.take_due_same_app_activation(generation, now + Duration::from_millis(19)));
        assert!(intent.take_due_same_app_activation(generation, now + SAME_APP_ACTIVATION_DELAY));
        assert_eq!(intent.next_deadline(), Some(now + FOCUS_RETRY_DELAY));
    }

    #[test]
    fn superseding_intent_cancels_delayed_same_app_activation() {
        let now = Instant::now();
        let mut intent = FocusIntentState::default();
        let old_generation = intent.request(
            Entity::from_raw_u32(1).expect("entity"),
            7,
            FocusRequestPolicy::RaiseAfterScroll,
            true,
            now,
        );
        assert!(intent.schedule_same_app_activation(old_generation, now));

        let new_generation = intent.request(
            Entity::from_raw_u32(2).expect("entity"),
            8,
            FocusRequestPolicy::RaiseAfterScroll,
            true,
            now + Duration::from_millis(1),
        );

        assert!(
            !intent.take_due_same_app_activation(old_generation, now + SAME_APP_ACTIVATION_DELAY)
        );
        assert_eq!(
            intent.pending().map(|pending| pending.generation),
            Some(new_generation)
        );
    }
}
