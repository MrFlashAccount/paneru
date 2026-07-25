//! Ordered native bridge from a Paneru focus intent to macOS window focus.

use accessibility_sys::kAXRaiseAction;
use objc2_core_foundation::CFString;

use super::{CPS_USER_GENERATED, WindowOS};
use crate::manager::skylight::{_SLPSSetFrontProcessWithOptions, AXUIElementPerformAction};
use crate::platform::ProcessSerialNumber;

fn ordered_focus_transition(
    mut activate_app: impl FnMut(),
    mut make_key: impl FnMut(),
    mut raise_window: impl FnMut(),
) {
    activate_app();
    make_key();
    raise_window();
}

pub(super) fn focus_with_raise(window: &WindowOS, psn: ProcessSerialNumber) {
    let window_id = window.id;
    let element = window.ax_element.as_ptr();
    ordered_focus_transition(
        || unsafe {
            _SLPSSetFrontProcessWithOptions(&psn, window_id, CPS_USER_GENERATED);
        },
        || window.make_key_window(&psn),
        || {
            let action = CFString::from_static_str(kAXRaiseAction);
            unsafe { AXUIElementPerformAction(element, &action) };
        },
    );
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::ordered_focus_transition;

    #[test]
    fn activates_process_then_makes_target_key_then_raises() {
        let steps = RefCell::new(Vec::new());
        ordered_focus_transition(
            || steps.borrow_mut().push("activate-app"),
            || steps.borrow_mut().push("make-key"),
            || steps.borrow_mut().push("raise-window"),
        );
        assert_eq!(
            steps.into_inner(),
            ["activate-app", "make-key", "raise-window"]
        );
    }
}
