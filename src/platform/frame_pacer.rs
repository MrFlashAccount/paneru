use std::cell::Cell;
use std::collections::HashMap;

use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::NSScreen;
use objc2_core_graphics::CGDirectDisplayID;
use objc2_foundation::{NSDefaultRunLoopMode, NSObject, NSObjectProtocol, NSRunLoop};
use objc2_quartz_core::CADisplayLink;
use tracing::{debug, warn};

use crate::platform::macos_major_version;
use crate::util::{read_screen_property, screen_display_id};

#[derive(Debug)]
struct DisplayLinkTargetIvars {
    fired: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PaneruDisplayLinkTarget"]
    #[ivars = DisplayLinkTargetIvars]
    #[derive(Debug)]
    struct DisplayLinkTarget;

    unsafe impl NSObjectProtocol for DisplayLinkTarget {}

    impl DisplayLinkTarget {
        #[unsafe(method(displayLinkDidFire:))]
        fn display_link_did_fire(&self, _: &CADisplayLink) {
            self.ivars().fired.set(true);
        }
    }
);

impl DisplayLinkTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DisplayLinkTargetIvars {
            fired: Cell::new(false),
        });
        unsafe { msg_send![super(this), init] }
    }
}

pub(super) struct DisplayFramePacer {
    mtm: MainThreadMarker,
    target: Retained<DisplayLinkTarget>,
    display_id: Option<CGDirectDisplayID>,
    link: Option<Retained<CADisplayLink>>,
    refresh_rates: DisplayRefreshRateCache,
}

#[derive(Default)]
struct DisplayRefreshRateCache {
    rates: HashMap<CGDirectDisplayID, isize>,
    valid: bool,
}

impl DisplayRefreshRateCache {
    fn ensure_with(&mut self, enumerate: impl FnOnce(&mut dyn FnMut(CGDirectDisplayID, isize))) {
        if self.valid {
            return;
        }
        self.rates.clear();
        enumerate(&mut |display_id, refresh_rate| {
            self.rates.insert(display_id, refresh_rate);
        });
        self.valid = true;
    }

    fn invalidate(&mut self) {
        self.valid = false;
    }

    fn fastest(
        &self,
        visit_candidates: impl FnOnce(&mut dyn FnMut(CGDirectDisplayID)),
        mut on_lookup: impl FnMut(CGDirectDisplayID),
    ) -> Option<CGDirectDisplayID> {
        let mut best = None::<(CGDirectDisplayID, isize)>;
        visit_candidates(&mut |display_id| {
            on_lookup(display_id);
            let Some(refresh_rate) = self.rates.get(&display_id).copied() else {
                return;
            };
            if best.is_none_or(|(best_id, best_rate)| {
                refresh_rate > best_rate || (refresh_rate == best_rate && display_id < best_id)
            }) {
                best = Some((display_id, refresh_rate));
            }
        });
        best.map(|(display_id, _)| display_id)
    }
}

impl DisplayFramePacer {
    pub(super) fn new(mtm: MainThreadMarker) -> Self {
        Self {
            mtm,
            target: DisplayLinkTarget::new(mtm),
            display_id: None,
            link: None,
            refresh_rates: DisplayRefreshRateCache::default(),
        }
    }

    pub(super) fn arm(&mut self, display_id: CGDirectDisplayID) -> bool {
        // NSScreen display links are the non-deprecated macOS API starting in
        // macOS 14. Keep the existing timer pacing for our macOS 11-13 support.
        if macos_major_version() < 14 {
            return false;
        }
        if self.display_id != Some(display_id) {
            self.configure(display_id);
        }
        let Some(link) = self.link.as_ref() else {
            return false;
        };
        self.target.ivars().fired.set(false);
        link.setPaused(false);
        true
    }

    pub(super) fn fastest_display_id(
        &mut self,
        visit_candidates: impl FnOnce(&mut dyn FnMut(CGDirectDisplayID)),
    ) -> Option<CGDirectDisplayID> {
        self.refresh_rates.ensure_with(|add| {
            for screen in NSScreen::screens(self.mtm) {
                if let Some(display_id) = screen_display_id(&screen) {
                    add(display_id, screen.maximumFramesPerSecond());
                }
            }
        });
        self.refresh_rates.fastest(visit_candidates, |_| {})
    }

    pub(super) fn invalidate_refresh_rates(&mut self) {
        self.refresh_rates.invalidate();
    }

    pub(super) fn frame_fired(&self) -> bool {
        self.target.ivars().fired.get()
    }

    pub(super) fn pause(&self) {
        if let Some(link) = self.link.as_ref() {
            link.setPaused(true);
        }
    }

    fn configure(&mut self, display_id: CGDirectDisplayID) {
        if let Some(link) = self.link.take() {
            link.invalidate();
        }
        self.display_id = Some(display_id);

        let screens = NSScreen::screens(self.mtm);
        let Some((link, maximum_fps)) = read_screen_property(&screens, display_id, |screen| {
            let maximum_fps = screen.maximumFramesPerSecond();
            let link = unsafe {
                screen.displayLinkWithTarget_selector(&self.target, sel!(displayLinkDidFire:))
            };
            (link, maximum_fps)
        }) else {
            warn!(
                display_id,
                "unable to create display link for active screen"
            );
            return;
        };

        link.setPaused(true);
        let run_loop = NSRunLoop::mainRunLoop();
        unsafe {
            link.addToRunLoop_forMode(&run_loop, NSDefaultRunLoopMode);
        }
        debug!(
            display_id,
            maximum_fps, "using display-synchronized animation pacing"
        );
        self.link = Some(link);
    }
}

impl Drop for DisplayFramePacer {
    fn drop(&mut self) {
        if let Some(link) = self.link.take() {
            link.invalidate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DisplayRefreshRateCache;
    use std::cell::Cell;

    #[test]
    fn cache_enumerates_once_deduplicates_candidates_and_refreshes_after_invalidation() {
        let enumerations = Cell::new(0);
        let lookups = Cell::new(0);
        let mut cache = DisplayRefreshRateCache::default();
        let ensure = |cache: &mut DisplayRefreshRateCache, rates: &[(u32, isize)]| {
            cache.ensure_with(|add| {
                enumerations.set(enumerations.get() + 1);
                for (display_id, refresh_rate) in rates {
                    add(*display_id, *refresh_rate);
                }
            });
        };

        ensure(&mut cache, &[(41, 60), (17, 120)]);
        let selected = cache.fastest(
            |consider| {
                consider(41);
                consider(17);
            },
            |_| lookups.set(lookups.get() + 1),
        );
        assert_eq!(selected, Some(17));
        ensure(&mut cache, &[(41, 75), (17, 60)]);
        assert_eq!(enumerations.get(), 1);
        assert_eq!(lookups.get(), 2);

        cache.invalidate();
        ensure(&mut cache, &[(41, 75), (17, 60)]);
        assert_eq!(enumerations.get(), 2);
        assert_eq!(
            cache.fastest(
                |consider| {
                    consider(41);
                    consider(17);
                },
                |_| {},
            ),
            Some(41)
        );
        assert_eq!(cache.fastest(|_| {}, |_| {}), None);
    }
}
