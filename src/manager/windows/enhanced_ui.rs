//! Cached, nest-safe handling of the per-application AX enhanced-UI workaround.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use objc2_core_foundation::{CFBoolean, CFString, kCFBooleanFalse, kCFBooleanTrue};

use super::{WindowApi, WindowOS};
use crate::manager::skylight::AXUIElementSetAttributeValue;
use crate::platform::Pid;
use crate::util::AXUIAttributes;

const ATTRIBUTE: &str = "AXEnhancedUserInterface";
const CACHE_TTL: Duration = Duration::from_millis(250);
const CACHE_RETENTION: Duration = Duration::from_secs(60);

static STATES: LazyLock<Mutex<HashMap<Pid, EnhancedUiState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Copy, Debug)]
struct Observation {
    enabled: bool,
    at: Instant,
}

#[derive(Debug, Default)]
struct EnhancedUiState {
    depth: usize,
    observation: Option<Observation>,
    restore_on_exit: bool,
}

impl EnhancedUiState {
    fn begin(&mut self, now: Instant, observe: impl FnOnce() -> bool) -> bool {
        if self.depth > 0 {
            self.depth += 1;
            return false;
        }
        let enabled = self
            .observation
            .filter(|observation| now.saturating_duration_since(observation.at) < CACHE_TTL)
            .map_or_else(observe, |observation| observation.enabled);
        self.observation = Some(Observation { enabled, at: now });
        self.restore_on_exit = enabled;
        self.depth = 1;
        enabled
    }

    fn end(&mut self, now: Instant) -> bool {
        if self.depth == 0 {
            return false;
        }
        self.depth -= 1;
        if self.depth > 0 || !self.restore_on_exit {
            return false;
        }
        self.restore_on_exit = false;
        self.observation = Some(Observation {
            enabled: true,
            at: now,
        });
        true
    }

    fn is_retained(&self, now: Instant) -> bool {
        self.depth > 0
            || self.observation.is_some_and(|observation| {
                now.saturating_duration_since(observation.at) < CACHE_RETENTION
            })
    }
}

/// Opens one nest-safe workaround scope. A recent observation is reused for
/// rapid animation frames, while stale state is re-read so external AX clients
/// can still enable or disable the attribute.
pub(super) fn disable(window: &WindowOS) {
    let Ok(pid) = window.pid() else { return };
    let Some(app_element) = window.app_reference() else {
        return;
    };
    let attr = CFString::from_static_str(ATTRIBUTE);
    let now = Instant::now();
    let should_disable = {
        let mut states = STATES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        states.retain(|_, state| state.is_retained(now));
        states.entry(pid).or_default().begin(now, || {
            app_element
                .get_attribute::<CFBoolean>(&attr)
                .is_ok_and(|value| CFBoolean::value(&value))
        })
    };
    if should_disable {
        unsafe {
            AXUIElementSetAttributeValue(
                app_element.as_ptr(),
                attr.as_ref(),
                kCFBooleanFalse.unwrap(),
            );
        }
    }
}

/// Closes one workaround scope and restores enhanced UI only when the matching
/// outermost disable observed it enabled.
pub(super) fn reenable(window: &WindowOS) {
    let Ok(pid) = window.pid() else { return };
    let now = Instant::now();
    let should_restore = STATES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_mut(&pid)
        .is_some_and(|state| state.end(now));
    if !should_restore {
        return;
    }
    if let Some(app_element) = window.app_reference() {
        let attr = CFString::from_static_str(ATTRIBUTE);
        unsafe {
            AXUIElementSetAttributeValue(
                app_element.as_ptr(),
                attr.as_ref(),
                kCFBooleanTrue.unwrap(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::{CACHE_TTL, EnhancedUiState};

    #[test]
    fn enabled_state_is_cached_across_animation_frames() {
        let now = std::time::Instant::now();
        let queries = Cell::new(0);
        let mut state = EnhancedUiState::default();
        let observe_enabled = || {
            queries.set(queries.get() + 1);
            true
        };

        assert!(state.begin(now, observe_enabled));
        assert!(state.end(now));
        assert!(state.begin(now + CACHE_TTL / 2, observe_enabled));
        assert!(state.end(now + CACHE_TTL / 2));
        assert_eq!(queries.get(), 1);
    }

    #[test]
    fn nested_scopes_disable_and_restore_only_once() {
        let now = std::time::Instant::now();
        let mut state = EnhancedUiState::default();

        assert!(state.begin(now, || true));
        assert!(!state.begin(now, || panic!("nested scope must not query")));
        assert!(!state.end(now));
        assert!(state.end(now));
        assert!(!state.end(now));
    }

    #[test]
    fn stale_cached_state_is_observed_again() {
        let now = std::time::Instant::now();
        let queries = Cell::new(0);
        let mut state = EnhancedUiState::default();
        let observe_disabled = || {
            queries.set(queries.get() + 1);
            false
        };

        assert!(!state.begin(now, observe_disabled));
        assert!(!state.end(now));
        assert!(!state.begin(now + CACHE_TTL, observe_disabled));
        assert_eq!(queries.get(), 2);
    }
}
