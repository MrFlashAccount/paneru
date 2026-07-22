use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use objc2_core_foundation::{CFBoolean, CFRetained, CFString};
use tracing::warn;

use super::{WindowApi, WindowOS};
use crate::platform::Pid;
use crate::util::{AXUIAttributes, AXUIWrapper, set_ax_boolean_attribute};

/// Per-PID ref-count for the `AXEnhancedUserInterface` workaround. Concurrent
/// operations restore the attribute only after the last lease completes.
static ENHANCED_UI_REFCOUNT: LazyLock<Mutex<HashMap<Pid, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) struct EnhancedUiLease {
    pub(super) pid: Pid,
    pub(super) app: CFRetained<AXUIWrapper>,
}

impl Drop for EnhancedUiLease {
    fn drop(&mut self) {
        let mut counts = ENHANCED_UI_REFCOUNT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(count) = counts.get_mut(&self.pid) else {
            return;
        };
        *count -= 1;
        if *count == 0 {
            counts.remove(&self.pid);
            let attr = CFString::from_static_str("AXEnhancedUserInterface");
            let restore_result = set_ax_boolean_attribute(&self.app, attr.as_ref(), true);
            if let Err(err) = restore_result {
                warn!(
                    pid = self.pid,
                    error = %err,
                    "unable to restore AX Enhanced UI after reposition batch"
                );
            }
        }
    }
}

impl WindowOS {
    /// Disables Enhanced UI once and returns an RAII lease that restores it.
    pub(super) fn acquire_enhanced_ui(&self) -> Option<EnhancedUiLease> {
        let Ok(pid) = self.pid() else { return None };
        let app = self.app_reference()?;
        let mut counts = ENHANCED_UI_REFCOUNT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(count) = counts.get_mut(&pid) {
            *count += 1;
        } else {
            let attr = CFString::from_static_str("AXEnhancedUserInterface");
            if !app
                .get_attribute::<CFBoolean>(&attr)
                .is_ok_and(|value| CFBoolean::value(&value))
            {
                return None;
            }
            let disable_result = set_ax_boolean_attribute(&app, attr.as_ref(), false);
            if let Err(err) = disable_result {
                warn!(
                    pid,
                    error = %err,
                    "unable to disable AX Enhanced UI for reposition batch"
                );
                return None;
            }
            counts.insert(pid, 1);
        }
        Some(EnhancedUiLease { pid, app })
    }
}

thread_local! {
    static REPOSITION_BATCHES: RefCell<Vec<HashMap<Pid, Option<EnhancedUiLease>>>> =
        const { RefCell::new(Vec::new()) };
}

pub(super) fn retain_batch_lease<T>(
    batch: &mut HashMap<Pid, Option<T>>,
    pid: Pid,
    acquire: impl FnOnce() -> Option<T>,
) {
    batch.entry(pid).or_insert_with(acquire);
}

/// Bounds the workaround to one ECS position-commit batch. Each application is
/// inspected once and every acquired lease is restored by `Drop`.
pub(super) struct WindowRepositionBatch;

impl WindowRepositionBatch {
    pub(super) fn new() -> Self {
        REPOSITION_BATCHES.with(|batches| batches.borrow_mut().push(HashMap::new()));
        Self
    }

    pub(super) fn handles(window: &WindowOS) -> bool {
        let Ok(pid) = window.pid() else {
            return false;
        };
        REPOSITION_BATCHES.with(|batches| {
            let mut batches = batches.borrow_mut();
            let Some(batch) = batches.last_mut() else {
                return false;
            };
            retain_batch_lease(batch, pid, || window.acquire_enhanced_ui());
            true
        })
    }

    #[cfg(test)]
    pub(super) fn active() -> bool {
        REPOSITION_BATCHES.with(|batches| !batches.borrow().is_empty())
    }
}

impl Drop for WindowRepositionBatch {
    fn drop(&mut self) {
        let leases = REPOSITION_BATCHES.with(|batches| batches.borrow_mut().pop());
        drop(leases);
    }
}
