use bevy::ecs::message::Message;
use objc2::rc::Retained;
use objc2_core_foundation::{
    CFRetained, CFRunLoop, CFRunLoopSource, CFRunLoopSourceContext, CGPoint, kCFRunLoopDefaultMode,
};
use objc2_core_graphics::CGDirectDisplayID;
use std::ffi::c_void;
use std::ptr::{from_ref, null_mut};
use std::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvError, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};

use crate::commands::Command;
use crate::config::Config;
use crate::errors::Result;
use crate::platform::{Modifiers, ProcessSerialNumber, WinID, WorkspaceId, WorkspaceObserver};
use crate::util::AXUIWrapper;

mod turn_batch;

pub(crate) use turn_batch::TurnBatch;

/// `Event` represents various system-level and application-specific occurrences that the window manager reacts to.
/// These events drive the core logic of the window manager, from window creation to display changes.
#[allow(dead_code)]
#[derive(Clone, Debug, Message)]
pub enum Event {
    /// Signals the application to exit.
    Exit,
    /// Indicates that the initial set of processes has been loaded.
    ProcessesLoaded,

    /// Announces the initialy loaded configuration
    InitialConfig(Config),
    /// Signals that the configuration should be reloaded.
    ConfigRefresh(notify::Event),

    /// An application has been launched.
    ApplicationLaunched {
        psn: ProcessSerialNumber,
        observer: Retained<WorkspaceObserver>,
    },

    /// An application has terminated.
    ApplicationTerminated { psn: ProcessSerialNumber },
    /// The frontmost application has switched.
    ApplicationFrontSwitched { psn: ProcessSerialNumber },
    /// The application has been activated.
    ApplicationActivated,
    /// The application has been deactivated.
    ApplicationDeactivated,
    /// An application has become visible.
    ApplicationVisible { pid: i32 },
    /// An application has become hidden.
    ApplicationHidden { pid: i32 },

    /// A window has been created.
    WindowCreated { element: CFRetained<AXUIWrapper> },
    /// A window has been destroyed.
    WindowDestroyed { window_id: WinID },
    /// A window has gained focus.
    WindowFocused { window_id: WinID },
    /// A window has been moved.
    WindowMoved { window_id: WinID },
    /// A window has been resized.
    WindowResized { window_id: WinID },
    /// A window has been minimized.
    WindowMinimized { window_id: WinID },
    /// A window has been de-minimized (restored).
    WindowDeminimized { window_id: WinID },
    /// A window's title has changed.
    WindowTitleChanged { window_id: WinID },

    /// A mouse down event has occurred.
    MouseDown {
        point: CGPoint,
        modifiers: Modifiers,
    },
    /// A mouse up event has occurred.
    MouseUp {
        point: CGPoint,
        modifiers: Modifiers,
    },
    /// A mouse drag event has occurred.
    MouseDragged {
        point: CGPoint,
        modifiers: Modifiers,
    },
    /// A mouse move event has occurred.
    MouseMoved {
        point: CGPoint,
        modifiers: Modifiers,
    },

    /// A swipe gesture has been detected.
    Swipe { delta: f64, fingers: usize },

    /// A vertical trackpad gesture (accumulates delta to threshold before firing).
    VerticalSwipe { delta: f64, fingers: usize },

    /// A single scroll wheel tick for vertical workspace switching (fires immediately).
    VerticalScrollTick { delta: f64 },

    /// A mouse scroll has been detected.
    Scroll { delta: f64 },

    /// Fingers have been placed on the touchpad.
    TouchpadDown,
    /// Physical contact ended; a native momentum phase may still follow.
    TouchpadPhysicalUp,
    /// Native momentum began for the current physical gesture.
    TouchpadMomentumStart,
    /// The full touchpad gesture, including native momentum, has ended.
    TouchpadUp,

    /// A new space (virtual desktop) has been created.
    SpaceCreated { space_id: WorkspaceId },
    /// A space has been destroyed.
    SpaceDestroyed { space_id: WorkspaceId },
    /// The active space has changed.
    SpaceChanged,

    /// A new display has been added.
    DisplayAdded { display_id: CGDirectDisplayID },
    /// A display has been removed.
    DisplayRemoved { display_id: CGDirectDisplayID },
    /// A display has been moved.
    DisplayMoved { display_id: CGDirectDisplayID },
    /// A display has been resized.
    DisplayResized { display_id: CGDirectDisplayID },
    /// A display's configuration has changed.
    DisplayConfigured { display_id: CGDirectDisplayID },
    /// The overall display arrangement has changed.
    DisplayChanged,

    /// Mission Control: Show all windows.
    MissionControlShowAllWindows,
    /// Mission Control: Show frontmost application windows.
    MissionControlShowFrontWindows,
    /// Mission Control: Show desktop.
    MissionControlShowDesktop,
    /// Mission Control: Exit.
    MissionControlExit,

    /// Dock preferences have changed.
    DockDidChangePref { msg: String },
    /// The Dock has restarted.
    DockDidRestart { msg: String },

    /// A menu has been opened.
    MenuOpened { window_id: WinID },
    /// A menu has been closed.
    MenuClosed { window_id: WinID },
    /// The visibility of the menu bar has changed.
    MenuBarHiddenChanged { msg: String },
    /// Paneru's own status menu is about to open and needs a fresh snapshot.
    StatusMenuOpened,
    /// Sparkle updater state changed and the status menu label/item is stale.
    UpdaterStatusChanged,
    /// The system has woken from sleep.
    SystemWoke { msg: String },

    /// The system appearance (Light/Dark mode) has changed.
    ThemeChanged,

    /// A command has been issued to the window manager.
    Command { command: Command },
}

/// `EventSender` is a thin wrapper around a `std::sync::mpsc::Sender` for `Event`s.
/// It provides a convenient way to send events to the main event loop from various parts of the application.
#[derive(Clone, Debug)]
pub struct EventSender {
    tx: Sender<Event>,
    wake: Arc<EventWake>,
}

#[derive(Debug)]
struct EventWake {
    generation: AtomicU64,
    source: AtomicPtr<CFRunLoopSource>,
    active_signals: AtomicUsize,
    geometry_wakes: Mutex<GeometryWakeState>,
    #[cfg(test)]
    signal_count: AtomicUsize,
}

#[derive(Debug, Default)]
struct GeometryWakeState {
    deferrals: usize,
    skipped_wake: bool,
}

impl Default for EventWake {
    fn default() -> Self {
        Self {
            generation: AtomicU64::new(0),
            source: AtomicPtr::new(null_mut()),
            active_signals: AtomicUsize::new(0),
            geometry_wakes: Mutex::new(GeometryWakeState::default()),
            #[cfg(test)]
            signal_count: AtomicUsize::new(0),
        }
    }
}

impl EventWake {
    fn signal(&self) {
        #[cfg(test)]
        self.signal_count.fetch_add(1, Ordering::Relaxed);
        self.active_signals.fetch_add(1, Ordering::AcqRel);
        let source = self.source.load(Ordering::Acquire);
        if !source.is_null() {
            // Main-thread sends must signal too: AppKit callbacks can run while
            // PlatformCallbacks is draining queued NSEvents immediately before
            // entering CFRunLoop::run_in_mode. Without a latched source, that
            // subsequent run could sleep despite the newly queued ECS event.
            // The source callback itself is empty and channel draining coalesces
            // events, so it adds at most one wake turn, not persistent frame work.
            // SAFETY: `EventWakeSource` owns the source until it first clears
            // this pointer with Release ordering. Core Foundation permits
            // signalling a run-loop source from arbitrary threads.
            unsafe { &*source }.signal();
        }
        self.active_signals.fetch_sub(1, Ordering::Release);
        if let Some(main_loop) = CFRunLoop::main() {
            main_loop.wake_up();
        }
    }
}

/// Receiver paired with [`EventSender`]. The generation counter makes the
/// queue-before-sleep protocol testable; the registered run-loop source closes
/// the final check-to-sleep race because its signalled state is latched.
pub struct EventReceiver {
    rx: Receiver<Event>,
    wake: Arc<EventWake>,
}

impl EventReceiver {
    pub fn recv(&self) -> std::result::Result<Event, RecvError> {
        self.rx.recv()
    }

    pub fn try_recv(&self) -> std::result::Result<Event, TryRecvError> {
        self.rx.try_recv()
    }

    pub(crate) fn generation(&self) -> u64 {
        self.wake.generation.load(Ordering::Acquire)
    }

    /// Suppresses only geometry wake signals while a display-frame wake is
    /// already guaranteed. Events remain queued and generation-tracked.
    pub(crate) fn defer_geometry_wakes(&self) -> GeometryWakeDeferral {
        self.wake
            .geometry_wakes
            .lock()
            .expect("geometry wake state poisoned")
            .deferrals += 1;
        GeometryWakeDeferral {
            wake: Arc::clone(&self.wake),
            released: false,
        }
    }
}

pub(crate) struct GeometryWakeDeferral {
    wake: Arc<EventWake>,
    released: bool,
}

impl GeometryWakeDeferral {
    /// Drains once more and releases deferral while geometry senders are
    /// excluded. A sender therefore lands either in this drain or after wake
    /// signalling has been restored; it cannot enqueue into the handoff gap.
    pub(crate) fn release_after<R>(mut self, drain: impl FnOnce() -> R) -> R {
        let mut state = self
            .wake
            .geometry_wakes
            .lock()
            .expect("geometry wake state poisoned");
        let output = drain();
        state.deferrals -= 1;
        if state.deferrals == 0 {
            state.skipped_wake = false;
        }
        self.released = true;
        output
    }
}

impl Drop for GeometryWakeDeferral {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let should_signal = {
            let mut state = self
                .wake
                .geometry_wakes
                .lock()
                .expect("geometry wake state poisoned");
            state.deferrals -= 1;
            let should_signal = state.deferrals == 0 && state.skipped_wake;
            if state.deferrals == 0 {
                state.skipped_wake = false;
            }
            should_signal
        };
        if should_signal {
            self.wake.signal();
        }
    }
}

pub(crate) struct EventWakeSource {
    source: CFRetained<CFRunLoopSource>,
    wake: Arc<EventWake>,
}

impl Drop for EventWakeSource {
    fn drop(&mut self) {
        self.wake.source.swap(null_mut(), Ordering::AcqRel);
        while self.wake.active_signals.load(Ordering::Acquire) != 0 {
            std::thread::yield_now();
        }
        if let Some(main_loop) = CFRunLoop::main() {
            main_loop.remove_source(Some(&self.source), unsafe { kCFRunLoopDefaultMode });
        }
        self.source.invalidate();
    }
}

unsafe extern "C-unwind" fn consume_event_wake(_: *mut c_void) {}

impl EventSender {
    /// Creates a new `EventSender` and its corresponding `Receiver`.
    /// This function initializes an MPSC channel.
    ///
    /// # Returns
    ///
    /// A tuple containing the `EventSender` and `Receiver` for the created channel.
    pub fn new() -> (Self, EventReceiver) {
        let (tx, rx) = channel::<Event>();
        let wake = Arc::new(EventWake::default());
        (
            Self {
                tx,
                wake: Arc::clone(&wake),
            },
            EventReceiver { rx, wake },
        )
    }

    pub(crate) fn install_main_run_loop_source(&self) -> Option<EventWakeSource> {
        let mut context = CFRunLoopSourceContext {
            version: 0,
            info: null_mut(),
            retain: None,
            release: None,
            copyDescription: None,
            equal: None,
            hash: None,
            schedule: None,
            cancel: None,
            perform: Some(consume_event_wake),
        };
        let source = unsafe { CFRunLoopSource::new(None, 0, &raw mut context) }?;
        let main_loop = CFRunLoop::main()?;
        main_loop.add_source(Some(&source), unsafe { kCFRunLoopDefaultMode });
        self.wake.source.store(
            from_ref::<CFRunLoopSource>(&source).cast_mut(),
            Ordering::Release,
        );
        Some(EventWakeSource {
            source,
            wake: Arc::clone(&self.wake),
        })
    }

    /// Sends an `Event` through the internal channel.
    ///
    /// # Arguments
    ///
    /// * `event` - The `Event` to send.
    ///
    /// # Returns
    ///
    /// `Ok(())` if the event is sent successfully, otherwise `Err(Error)` if the receiver has disconnected.
    pub fn send(&self, event: Event) -> Result<()> {
        if matches!(
            event,
            Event::WindowMoved { .. } | Event::WindowResized { .. }
        ) {
            let mut state = self
                .wake
                .geometry_wakes
                .lock()
                .expect("geometry wake state poisoned");
            self.tx.send(event)?;
            self.wake.generation.fetch_add(1, Ordering::Release);
            if state.deferrals != 0 {
                state.skipped_wake = true;
                return Ok(());
            }
        } else {
            self.tx.send(event)?;
            self.wake.generation.fetch_add(1, Ordering::Release);
        }
        self.wake.signal();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Event, EventSender};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[test]
    fn generation_latches_queued_before_sleep_and_during_ecs() {
        let (sender, receiver) = EventSender::new();
        let before = receiver.generation();
        sender.send(Event::ApplicationActivated).unwrap();
        assert!(receiver.generation() > before);
        assert!(matches!(
            receiver.try_recv(),
            Ok(Event::ApplicationActivated)
        ));

        let barrier = Arc::new(Barrier::new(2));
        let worker_barrier = Arc::clone(&barrier);
        let worker = thread::spawn(move || {
            worker_barrier.wait();
            sender.send(Event::ApplicationDeactivated).unwrap();
        });
        let before_ecs = receiver.generation();
        barrier.wait();
        worker.join().unwrap();
        assert!(receiver.generation() > before_ecs);
        assert!(matches!(
            receiver.try_recv(),
            Ok(Event::ApplicationDeactivated)
        ));
    }

    #[test]
    fn geometry_wake_deferral_keeps_events_and_restores_idle_wakes() {
        let (sender, receiver) = EventSender::new();
        {
            let _deferral = receiver.defer_geometry_wakes();
            sender.send(Event::WindowMoved { window_id: 7 }).unwrap();
            assert_eq!(receiver.wake.signal_count.load(Ordering::Acquire), 0);
        }
        assert!(matches!(
            receiver.try_recv(),
            Ok(Event::WindowMoved { window_id: 7 })
        ));
        assert_eq!(receiver.wake.signal_count.load(Ordering::Acquire), 1);
        sender.send(Event::WindowMoved { window_id: 8 }).unwrap();
        assert!(matches!(
            receiver.try_recv(),
            Ok(Event::WindowMoved { window_id: 8 })
        ));
        assert_eq!(receiver.wake.signal_count.load(Ordering::Acquire), 2);
    }

    #[test]
    fn geometry_sender_cannot_fall_into_guard_release_handoff_gap() {
        let (sender, receiver) = EventSender::new();
        let deferral = receiver.defer_geometry_wakes();
        let start = Arc::new(Barrier::new(2));
        let sender_started = Arc::new(AtomicBool::new(false));
        let worker = thread::spawn({
            let start = Arc::clone(&start);
            let sender_started = Arc::clone(&sender_started);
            move || {
                start.wait();
                sender_started.store(true, Ordering::Release);
                sender.send(Event::WindowMoved { window_id: 9 }).unwrap();
            }
        });

        let drained = deferral.release_after(|| {
            start.wait();
            while !sender_started.load(Ordering::Acquire) {
                thread::yield_now();
            }
            receiver.try_recv()
        });
        assert!(matches!(drained, Err(std::sync::mpsc::TryRecvError::Empty)));

        worker.join().unwrap();
        assert!(matches!(
            receiver.try_recv(),
            Ok(Event::WindowMoved { window_id: 9 })
        ));
        assert_eq!(
            receiver.wake.signal_count.load(Ordering::Acquire),
            1,
            "post-drain geometry enqueue must restore the idle wake"
        );
    }
}
