#![allow(clippy::cast_possible_truncation)]

use std::sync::mpsc::TryRecvError;
use std::time::Duration;
use tracing::{error, warn};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

mod accessibility_prompt;
mod commands;
mod config;
mod ecs;
mod errors;
mod events;
mod manager;
mod menubar;
mod overlay;
mod platform;
mod updater;
mod util;

#[cfg(test)]
mod tests;

embed_plist::embed_info_plist!("../assets/Info.plist");

use events::EventSender;

use errors::Result;

use crate::accessibility_prompt::{AccessibilitySetupAction, show_accessibility_setup};
use crate::ecs::setup_bevy_app;
use crate::events::{Event, EventReceiver};
use crate::manager::{check_ax_privilege, request_ax_privilege};
use crate::menubar::MenuBarManager;
use crate::platform::PlatformCallbacks;

/// Starts the packaged Paneru application using its fixed runtime path.
///
/// # Returns
///
/// `Ok(())` if the application runs successfully, otherwise `Err(Error)`.
fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(
            fmt::layer()
                .with_level(true)
                .with_line_number(true)
                .with_file(true)
                .with_target(true)
                .with_thread_ids(false)
                .with_writer(std::io::stderr)
                .compact(),
        )
        .init();

    let (sender, receiver) = EventSender::new();
    if !check_ax_privilege() && !wait_for_accessibility(sender.clone(), &receiver) {
        return Ok(());
    }

    let mut app = setup_bevy_app(sender, receiver).inspect_err(|err| {
        error!("Error launching Paneru: {err}");
    })?;
    app.run();
    Ok(())
}

fn wait_for_accessibility(sender: EventSender, receiver: &EventReceiver) -> bool {
    let mut platform_callbacks = PlatformCallbacks::new(sender.clone());
    let _menu_bar =
        MenuBarManager::new_accessibility_required(platform_callbacks.main_thread_marker, sender);

    if show_accessibility_setup(platform_callbacks.main_thread_marker)
        == AccessibilitySetupAction::Continue
    {
        request_ax_privilege();
    }

    warn!(
        "Accessibility access is required. Paneru will remain in the menu bar and start automatically once access is granted."
    );

    loop {
        platform_callbacks.pump_cocoa_event_loop(Some(Duration::from_secs(1)), None);

        if check_ax_privilege() {
            return true;
        }

        match receiver.try_recv() {
            Ok(
                Event::Exit
                | Event::Command {
                    command: commands::Command::Quit,
                },
            )
            | Err(TryRecvError::Disconnected) => return false,
            Ok(event) => warn!(
                ?event,
                "ignoring event while waiting for Accessibility access"
            ),
            Err(TryRecvError::Empty) => {}
        }
    }
}
