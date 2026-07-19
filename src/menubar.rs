use bevy::ecs::change_detection::DetectChanges;
use bevy::ecs::entity::Entity;
use bevy::ecs::lifecycle::RemovedComponents;
use bevy::ecs::message::MessageReader;
use bevy::ecs::query::{Added, Changed, Or, With};
use bevy::ecs::system::{Local, NonSendMut, Query, Res};
use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSColor, NSControlStateValueMixed, NSControlStateValueOff, NSControlStateValueOn,
    NSEventModifierFlags, NSMenu, NSMenuDelegate, NSMenuItem, NSStatusBar, NSStatusItem,
    NSVariableStatusItemLength,
};
use objc2_core_foundation::CGFloat;
use objc2_foundation::{NSObject, NSObjectProtocol, NSString};
use tracing::warn;

use crate::accessibility_prompt::{AccessibilitySetupAction, show_accessibility_setup};
use crate::commands::{
    Command, Operation, bind_window_command_target, set_last_focused_window_target,
};
use crate::config::Config;
use crate::ecs::layout::LayoutStrip;
use crate::ecs::{ActiveWorkspaceMarker, FocusedMarker, Unmanaged, WidthRatio};
use crate::events::{Event, EventSender};
use crate::manager::{Window, request_ax_privilege};
use crate::platform::Modifiers;
use crate::platform::login_item::{self, LoginItemStatus};
use crate::updater::SparkleUpdater;

#[derive(Debug, Clone)]
struct MenuActionTargetIvars {
    events: EventSender,
    launch_at_login_item: RefCell<Option<Retained<NSMenuItem>>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PaneruMenuActionTarget"]
    #[ivars = MenuActionTargetIvars]
    #[derive(Debug)]
    struct MenuActionTarget;

    unsafe impl NSObjectProtocol for MenuActionTarget {}

    impl MenuActionTarget {
        #[unsafe(method(setWidth:))]
        fn set_width(&self, item: &NSMenuItem) {
            let Ok(percentage) = i32::try_from(item.tag()) else {
                return;
            };
            let ratio = f64::from(percentage) / 100.0;
            self.send_command(Command::Window(Operation::SetWidth(ratio)));
        }

        #[unsafe(method(centerWindow:))]
        fn center_window(&self, _: &NSMenuItem) {
            self.send_command(Command::Window(Operation::Center));
        }

        #[unsafe(method(toggleManaged:))]
        fn toggle_managed(&self, _: &NSMenuItem) {
            self.send_command(Command::Window(Operation::Manage(None)));
        }

        #[unsafe(method(openAccessibilitySettings:))]
        fn open_accessibility_settings(&self, _: &NSMenuItem) {
            if let Err(error) = std::process::Command::new("/usr/bin/open")
                .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
                .spawn()
            {
                warn!(%error, "unable to open Accessibility settings");
            }
        }

        #[unsafe(method(showAccessibilityInstructions:))]
        fn show_accessibility_instructions(&self, _: &NSMenuItem) {
            let Some(main_thread_marker) = MainThreadMarker::new() else {
                warn!("unable to show Accessibility instructions outside the main thread");
                return;
            };

            if show_accessibility_setup(main_thread_marker)
                == AccessibilitySetupAction::Continue
            {
                request_ax_privilege();
            }
        }

        #[unsafe(method(toggleLaunchAtLogin:))]
        fn toggle_launch_at_login(&self, _: &NSMenuItem) {
            if let Err(error) = login_item::toggle() {
                warn!(%error, "unable to toggle Launch at Login");
                Self::show_login_item_error(&error);
            }
            self.refresh_launch_at_login_item();
        }

        #[unsafe(method(quitPaneru:))]
        fn quit_paneru(&self, _: &NSMenuItem) {
            self.send_command(Command::Quit);
        }
    }

    unsafe impl NSMenuDelegate for MenuActionTarget {
        #[unsafe(method(menuWillOpen:))]
        fn menu_will_open(&self, _: &NSMenu) {
            self.refresh_launch_at_login_item();
            if let Err(error) = self.ivars().events.send(Event::StatusMenuOpened) {
                warn!(%error, "unable to request menu refresh");
            }
        }
    }
);

impl MenuActionTarget {
    fn new(mtm: MainThreadMarker, events: EventSender) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(MenuActionTargetIvars {
            events,
            launch_at_login_item: RefCell::new(None),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn set_launch_at_login_item(&self, item: Retained<NSMenuItem>) {
        self.ivars().launch_at_login_item.replace(Some(item));
        self.refresh_launch_at_login_item();
    }

    fn refresh_launch_at_login_item(&self) {
        let item = self.ivars().launch_at_login_item.borrow();
        let Some(item) = item.as_ref() else {
            return;
        };

        let status = login_item::status();
        item.setEnabled(status != LoginItemStatus::Unavailable);
        item.setState(match status {
            LoginItemStatus::Enabled => NSControlStateValueOn,
            LoginItemStatus::RequiresApproval => NSControlStateValueMixed,
            LoginItemStatus::Unavailable
            | LoginItemStatus::NotRegistered
            | LoginItemStatus::NotFound => NSControlStateValueOff,
        });
        let title = match status {
            LoginItemStatus::RequiresApproval => "Launch at Login…",
            _ => "Launch at Login",
        };
        item.setTitle(&NSString::from_str(title));
    }

    fn show_login_item_error(error: &str) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str("Couldn’t Change Launch at Login"));
        alert.setInformativeText(&NSString::from_str(error));
        alert.addButtonWithTitle(&NSString::from_str("OK"));
        alert.runModal();
    }

    fn send_command(&self, command: Command) {
        let Some(command) = bind_window_command_target(command) else {
            warn!("ignoring window command because no focused window target is known");
            return;
        };
        if let Err(error) = self.ivars().events.send(Event::Command { command }) {
            warn!(%error, "unable to send menu bar command");
        }
    }
}

pub struct MenuBarManager {
    mtm: MainThreadMarker,
    status_bar: Retained<NSStatusBar>,
    status_item: Retained<NSStatusItem>,
    menu: Retained<NSMenu>,
    action_target: Retained<MenuActionTarget>,
    width_items: Vec<(i32, Retained<NSMenuItem>)>,
    managed_window_items: Vec<Retained<NSMenuItem>>,
    manage_item: Option<Retained<NSMenuItem>>,
    configured_widths: Vec<i32>,
    configured_shortcuts: MenuShortcuts,
    current_label: Option<String>,
    publication: MenuPublicationGate,
    updater: Option<SparkleUpdater>,
    check_for_updates_item: Option<Retained<NSMenuItem>>,
}

const STATUS_ITEM_BACKGROUND_ALPHA: CGFloat = 0.18;
const STATUS_ITEM_CORNER_RADIUS: CGFloat = 5.0;

#[derive(Debug, Eq, PartialEq)]
struct WindowMenuEnablement {
    managed_actions: bool,
    toggle_managed: bool,
}

#[derive(Default)]
struct MenuPublicationGate {
    published: bool,
}

impl MenuPublicationGate {
    fn publish_after_update(&mut self) -> bool {
        if self.published {
            false
        } else {
            self.published = true;
            true
        }
    }
}

#[derive(Default)]
pub(crate) struct MenuDirtyGate {
    initialized: bool,
}

impl MenuDirtyGate {
    fn should_update(&mut self, changed: bool) -> bool {
        let first = !self.initialized;
        self.initialized = true;
        first || changed
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MenuShortcut {
    key: String,
    modifiers: u8,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct MenuShortcuts {
    widths: Vec<(i32, Option<MenuShortcut>)>,
    center: Option<MenuShortcut>,
    manage: Option<MenuShortcut>,
    quit: Option<MenuShortcut>,
}

fn window_menu_enablement(
    has_focused_window: bool,
    focused_width_ratio: Option<f64>,
) -> WindowMenuEnablement {
    WindowMenuEnablement {
        managed_actions: focused_width_ratio.is_some(),
        toggle_managed: has_focused_window,
    }
}

impl MenuBarManager {
    pub fn new(mtm: MainThreadMarker, events: EventSender) -> Self {
        let status_bar = NSStatusBar::systemStatusBar();
        let status_item = status_bar.statusItemWithLength(NSVariableStatusItemLength);
        let menu = NSMenu::new(mtm);
        let action_target = MenuActionTarget::new(mtm, events.clone());

        menu.setAutoenablesItems(false);
        menu.setDelegate(Some(ProtocolObject::from_ref(&*action_target)));
        status_item.setMenu(Some(&menu));
        // Keep the status item unclickable until the first ECS snapshot has
        // built and enabled every menu item. This makes the first native open
        // synchronous with initialized content rather than an async refresh.
        status_item.setVisible(false);

        Self {
            mtm,
            status_bar,
            status_item,
            menu,
            action_target,
            width_items: Vec::new(),
            managed_window_items: Vec::new(),
            manage_item: None,
            configured_widths: Vec::new(),
            configured_shortcuts: MenuShortcuts::default(),
            current_label: None,
            publication: MenuPublicationGate::default(),
            updater: SparkleUpdater::load(mtm, events),
            check_for_updates_item: None,
        }
    }

    pub fn new_accessibility_required(mtm: MainThreadMarker, events: EventSender) -> Self {
        let mut manager = Self::new(mtm, events);
        manager.rebuild_accessibility_menu();
        manager.show_label("Paneru !".to_owned());
        manager
    }

    fn rebuild_accessibility_menu(&mut self) {
        self.menu.removeAllItems();

        let status = self.add_item("Paneru — Accessibility Required", None, None);
        status.setEnabled(false);

        let hint = self.add_item("Grant access; Paneru will start automatically", None, None);
        hint.setEnabled(false);

        self.menu.addItem(&NSMenuItem::separatorItem(self.mtm));
        self.add_item(
            "Show Setup Instructions…",
            Some(sel!(showAccessibilityInstructions:)),
            None,
        );
        self.add_item(
            "Open Accessibility Settings…",
            Some(sel!(openAccessibilitySettings:)),
            None,
        );

        self.menu.addItem(&NSMenuItem::separatorItem(self.mtm));
        self.add_launch_at_login_item();

        self.menu.addItem(&NSMenuItem::separatorItem(self.mtm));
        self.add_item("Quit Paneru", Some(sel!(quitPaneru:)), None);
    }

    fn update(
        &mut self,
        virtual_index: u32,
        show_virtual_workspace: bool,
        preset_widths: &[f64],
        has_focused_window: bool,
        focused_width_ratio: Option<f64>,
        shortcuts: &MenuShortcuts,
    ) {
        let widths = normalized_width_percentages(preset_widths);
        if self.configured_widths != widths || self.configured_shortcuts != *shortcuts {
            self.rebuild_menu(&widths, shortcuts);
        }

        let enablement = window_menu_enablement(has_focused_window, focused_width_ratio);
        for item in &self.managed_window_items {
            item.setEnabled(enablement.managed_actions);
        }
        if let Some(manage_item) = &self.manage_item {
            manage_item.setEnabled(enablement.toggle_managed);
        }
        if let Some(item) = &self.check_for_updates_item {
            let status = self.updater.as_ref().map(SparkleUpdater::status);
            let title = match status.as_ref() {
                Some(status) if status.is_checking => "Checking for Updates…".to_owned(),
                Some(status) if status.available_version.is_some() => {
                    format!(
                        "Update {}…",
                        status.available_version.as_deref().unwrap_or_default()
                    )
                }
                _ => "Check for Updates…".to_owned(),
            };
            item.setTitle(&NSString::from_str(&title));
            item.setEnabled(
                self.updater
                    .as_ref()
                    .is_some_and(SparkleUpdater::can_check_for_updates),
            );
        }
        for (percentage, item) in &self.width_items {
            let selected = focused_width_ratio
                .is_some_and(|ratio| (ratio.mul_add(100.0, -f64::from(*percentage))).abs() < 1.0);
            item.setState(if selected {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
        }

        let mut label = if show_virtual_workspace {
            format_virtual_workspace_label(virtual_index)
        } else {
            "Paneru".to_owned()
        };
        if self
            .updater
            .as_ref()
            .is_some_and(|updater| updater.status().available_version.is_some())
        {
            label.push_str(" •");
        }
        self.show_label(label);
        if self.publication.publish_after_update() {
            self.status_item.setVisible(true);
        }
    }

    fn rebuild_menu(&mut self, widths: &[i32], shortcuts: &MenuShortcuts) {
        self.menu.removeAllItems();
        self.width_items.clear();
        self.managed_window_items.clear();
        self.manage_item = None;
        self.check_for_updates_item = None;

        let status = self.add_item("Paneru — Running", None, None);
        status.setEnabled(false);
        self.menu.addItem(&NSMenuItem::separatorItem(self.mtm));

        let width_header = self.add_item("Window width", None, None);
        width_header.setEnabled(false);
        for &percentage in widths {
            let shortcut = shortcuts
                .widths
                .iter()
                .find_map(|(width, shortcut)| (*width == percentage).then_some(shortcut))
                .and_then(Option::as_ref);
            let item = self.add_item(&format!("{percentage}%"), Some(sel!(setWidth:)), shortcut);
            item.setTag(isize::try_from(percentage).expect("width percentage fits in isize"));
            self.managed_window_items.push(item.clone());
            self.width_items.push((percentage, item));
        }

        self.menu.addItem(&NSMenuItem::separatorItem(self.mtm));
        let center = self.add_item(
            "Center Window",
            Some(sel!(centerWindow:)),
            shortcuts.center.as_ref(),
        );
        let manage = self.add_item(
            "Toggle Managed",
            Some(sel!(toggleManaged:)),
            shortcuts.manage.as_ref(),
        );
        self.managed_window_items.push(center);
        self.manage_item = Some(manage);

        self.menu.addItem(&NSMenuItem::separatorItem(self.mtm));
        let check_for_updates =
            self.add_item("Check for Updates…", Some(sel!(checkForUpdates:)), None);
        if let Some(updater) = &self.updater {
            unsafe { check_for_updates.setTarget(Some(updater.controller_target())) };
            check_for_updates.setEnabled(updater.can_check_for_updates());
        } else {
            unsafe { check_for_updates.setTarget(None) };
            check_for_updates.setEnabled(false);
        }
        self.check_for_updates_item = Some(check_for_updates);

        self.add_launch_at_login_item();

        self.add_item(
            "Quit Paneru",
            Some(sel!(quitPaneru:)),
            shortcuts.quit.as_ref(),
        );
        self.configured_widths = widths.to_vec();
        self.configured_shortcuts = shortcuts.clone();
    }

    fn add_launch_at_login_item(&self) -> Retained<NSMenuItem> {
        let item = self.add_item("Launch at Login", Some(sel!(toggleLaunchAtLogin:)), None);
        self.action_target.set_launch_at_login_item(item.clone());
        item
    }

    fn add_item(
        &self,
        title: &str,
        action: Option<objc2::runtime::Sel>,
        shortcut: Option<&MenuShortcut>,
    ) -> Retained<NSMenuItem> {
        let item = unsafe {
            self.menu.addItemWithTitle_action_keyEquivalent(
                &NSString::from_str(title),
                action,
                &NSString::from_str(""),
            )
        };
        if action.is_some() {
            unsafe { item.setTarget(Some(&self.action_target)) };
        }
        if let Some(shortcut) = shortcut {
            item.setKeyEquivalent(&NSString::from_str(&shortcut.key));
            item.setKeyEquivalentModifierMask(native_modifier_flags(shortcut.modifiers));
        }
        item
    }

    fn show_label(&mut self, label: String) {
        if self.current_label.as_deref() == Some(label.as_str()) {
            return;
        }

        let title = NSString::from_str(&label);
        let tooltip = NSString::from_str("Paneru window manager");
        let Some(button) = self.status_item.button(self.mtm) else {
            warn!("unable to update menu bar: status item has no button");
            return;
        };

        button.setWantsLayer(true);
        if let Some(layer) = button.layer() {
            let background = NSColor::controlAccentColor()
                .colorWithAlphaComponent(STATUS_ITEM_BACKGROUND_ALPHA)
                .CGColor();
            layer.setBackgroundColor(Some(&background));
            layer.setCornerRadius(STATUS_ITEM_CORNER_RADIUS);
            layer.setMasksToBounds(true);
        }
        button.setTitle(&title);
        button.setToolTip(Some(&tooltip));
        self.current_label = Some(label);
    }
}

impl Drop for MenuBarManager {
    fn drop(&mut self) {
        self.status_bar.removeStatusItem(&self.status_item);
    }
}

#[allow(clippy::needless_pass_by_value, clippy::type_complexity)]
pub fn update_menu_bar(
    active_workspace: Query<(Entity, &LayoutStrip), With<ActiveWorkspaceMarker>>,
    focused: Query<(&Window, &WidthRatio, Option<&Unmanaged>), With<FocusedMarker>>,
    config: Res<Config>,
    menu_bar: Option<NonSendMut<MenuBarManager>>,
) {
    let Some(mut menu_bar) = menu_bar else {
        return;
    };
    let Some((_, strip)) = active_workspace.iter().next() else {
        return;
    };

    let focused_window = focused.iter().next();
    if let Some((window, _, _)) = focused_window {
        set_last_focused_window_target(window.id());
    }
    let can_toggle_focused = focused_window
        .is_some_and(|(_, _, unmanaged)| matches!(unmanaged, None | Some(Unmanaged::Passthrough)));
    let focused_width_ratio = focused_window
        .filter(|(_, _, unmanaged)| unmanaged.is_none())
        .map(|(_, ratio, _)| ratio.0);

    let preset_widths = config.preset_column_widths();
    let shortcuts = menu_shortcuts(&config, &preset_widths);
    menu_bar.update(
        strip.virtual_index,
        config.workspace_menu_status(),
        &preset_widths,
        can_toggle_focused,
        focused_width_ratio,
        &shortcuts,
    );
}

/// Gates `AppKit` mutations behind state that can change the visible menu.
/// The first update initializes the status item; subsequent animation frames
/// do no menu work unless focus, layout, ownership, updater status, or
/// configuration changed. Width selection uses the logical `WidthRatio`, so
/// animation-only `Bounds` changes never require an open-time refresh.
#[allow(clippy::needless_pass_by_value, clippy::type_complexity)]
pub fn menu_bar_dirty(
    mut gate: Local<MenuDirtyGate>,
    config: Res<Config>,
    active_workspace: Query<
        (),
        (
            With<ActiveWorkspaceMarker>,
            Or<(Added<ActiveWorkspaceMarker>, Changed<LayoutStrip>)>,
        ),
    >,
    focused: Query<
        (),
        (
            With<FocusedMarker>,
            Or<(Added<FocusedMarker>, Changed<Unmanaged>)>,
        ),
    >,
    mut focus_removed: RemovedComponents<FocusedMarker>,
    mut unmanaged_removed: RemovedComponents<Unmanaged>,
    mut events: MessageReader<Event>,
) -> bool {
    let event_dirty = events
        .read()
        .any(|event| matches!(event, Event::StatusMenuOpened | Event::UpdaterStatusChanged));
    let changed = config.is_changed()
        || !active_workspace.is_empty()
        || !focused.is_empty()
        || focus_removed.read().next().is_some()
        || unmanaged_removed.read().next().is_some()
        || event_dirty;
    gate.should_update(changed)
}

pub(crate) fn format_virtual_workspace_label(virtual_index: u32) -> String {
    format!("VW {}", virtual_index + 1)
}

fn normalized_width_percentages(widths: &[f64]) -> Vec<i32> {
    let mut percentages = widths
        .iter()
        .copied()
        .filter(|ratio| ratio.is_finite() && *ratio > 0.0)
        .map(|ratio| ratio.mul_add(100.0, 0.0).round() as i32)
        .filter(|percentage| *percentage > 0)
        .collect::<Vec<_>>();
    percentages.sort_unstable();
    percentages.dedup();
    percentages
}

fn menu_shortcuts(config: &Config, widths: &[f64]) -> MenuShortcuts {
    let widths = normalized_width_percentages(widths)
        .into_iter()
        .map(|percentage| {
            let command_name = format!("window_width_{percentage}");
            (
                percentage,
                config
                    .first_keybinding(&command_name)
                    .and_then(|binding| menu_shortcut(&binding.key, binding.modifiers)),
            )
        })
        .collect();

    let shortcut = |command_name| {
        config
            .first_keybinding(command_name)
            .and_then(|binding| menu_shortcut(&binding.key, binding.modifiers))
    };

    MenuShortcuts {
        widths,
        center: shortcut("window_center"),
        manage: shortcut("window_manage"),
        quit: shortcut("quit"),
    }
}

fn menu_shortcut(key: &str, modifiers: Modifiers) -> Option<MenuShortcut> {
    let key = match key {
        "equal" => "=",
        "minus" => "-",
        "rightbracket" => "]",
        "leftbracket" => "[",
        "quote" => "'",
        "semicolon" => ";",
        "backslash" => "\\",
        "comma" => ",",
        "slash" => "/",
        "period" => ".",
        "grave" => "`",
        "return" | "keypadenter" => "\r",
        "tab" => "\t",
        "space" => " ",
        "delete" => "\u{8}",
        "escape" => "\u{1b}",
        key if key.chars().count() == 1 => key,
        _ => return None,
    };

    Some(MenuShortcut {
        key: key.to_owned(),
        modifiers: modifiers.bits(),
    })
}

fn native_modifier_flags(modifier_bits: u8) -> NSEventModifierFlags {
    let modifiers = Modifiers::from_bits_retain(modifier_bits);
    let mut flags = NSEventModifierFlags::empty();
    if modifiers.intersects(Modifiers::SHIFT) {
        flags |= NSEventModifierFlags::Shift;
    }
    if modifiers.intersects(Modifiers::CTRL) {
        flags |= NSEventModifierFlags::Control;
    }
    if modifiers.intersects(Modifiers::ALT) {
        flags |= NSEventModifierFlags::Option;
    }
    if modifiers.intersects(Modifiers::CMD) {
        flags |= NSEventModifierFlags::Command;
    }
    flags
}

#[cfg(test)]
mod tests {
    use super::{
        MenuDirtyGate, MenuPublicationGate, WindowMenuEnablement, format_virtual_workspace_label,
        menu_bar_dirty, menu_shortcut, menu_shortcuts, normalized_width_percentages,
        window_menu_enablement,
    };
    use crate::config::Config;
    use crate::ecs::{Bounds, FocusedMarker};
    use crate::events::Event;
    use crate::platform::Modifiers;
    use bevy::app::{App, Update};
    use bevy::ecs::message::Messages;
    use bevy::ecs::resource::Resource;
    use bevy::ecs::schedule::IntoScheduleConfigs;
    use bevy::ecs::system::ResMut;
    use bevy::math::IVec2;

    #[test]
    fn label_is_one_based() {
        assert_eq!(format_virtual_workspace_label(0), "VW 1");
        assert_eq!(format_virtual_workspace_label(4), "VW 5");
    }

    #[test]
    fn status_item_is_published_only_after_first_content_update() {
        let mut publication = MenuPublicationGate::default();
        assert!(!publication.published);
        assert!(publication.publish_after_update());
        assert!(publication.published);
        assert!(!publication.publish_after_update());
    }

    #[test]
    fn menu_widths_are_sorted_deduplicated_and_valid() {
        assert_eq!(
            normalized_width_percentages(&[2.0, 0.5, 1.5, 0.5, 0.001, f64::NAN, -1.0]),
            vec![50, 150, 200]
        );
    }

    #[test]
    fn menu_shortcut_uses_native_key_and_preserves_modifiers() {
        let shortcut = menu_shortcut("1", Modifiers::LCTRL | Modifiers::RALT | Modifiers::LCMD)
            .expect("shortcut should be representable");
        assert_eq!(shortcut.key, "1");
        assert_eq!(
            shortcut.modifiers,
            (Modifiers::LCTRL | Modifiers::RALT | Modifiers::LCMD).bits()
        );
    }

    #[test]
    fn menu_shortcuts_come_from_command_bindings() {
        let config = Config::try_from(
            r#"
[options]

[bindings]
window_width_150 = "ctrl+alt+cmd-4"
window_center = "ctrl+alt+cmd-c"
"#,
        )
        .expect("bindings should parse");

        let shortcuts = menu_shortcuts(&config, &[1.0, 1.5]);
        assert_eq!(shortcuts.widths[0], (100, None));
        assert_eq!(
            shortcuts.widths[1]
                .1
                .as_ref()
                .map(|shortcut| shortcut.key.as_str()),
            Some("4")
        );
        assert_eq!(
            shortcuts
                .center
                .as_ref()
                .map(|shortcut| shortcut.key.as_str()),
            Some("c")
        );
    }

    #[test]
    fn unmanaged_focus_only_enables_toggle_managed() {
        assert_eq!(
            window_menu_enablement(true, None),
            WindowMenuEnablement {
                managed_actions: false,
                toggle_managed: true,
            }
        );
        assert_eq!(
            window_menu_enablement(false, None),
            WindowMenuEnablement {
                managed_actions: false,
                toggle_managed: false,
            }
        );
        assert_eq!(
            window_menu_enablement(true, Some(1.0)),
            WindowMenuEnablement {
                managed_actions: true,
                toggle_managed: true,
            }
        );
    }

    #[test]
    fn menu_dirty_gate_runs_once_then_only_for_changes() {
        let mut gate = MenuDirtyGate::default();
        assert!(gate.should_update(false));
        assert!(!gate.should_update(false));
        assert!(gate.should_update(true));
    }

    #[derive(Default, Resource)]
    struct MenuUpdateCount(usize);

    fn count_menu_update(mut count: ResMut<MenuUpdateCount>) {
        count.0 += 1;
    }

    #[test]
    fn animated_bounds_do_not_dirty_menu_but_updater_status_does() {
        let mut app = App::new();
        app.init_resource::<Messages<Event>>()
            .init_resource::<MenuUpdateCount>()
            .insert_resource(Config::default())
            .add_systems(Update, count_menu_update.run_if(menu_bar_dirty));
        let window = app
            .world_mut()
            .spawn((FocusedMarker, Bounds(IVec2::new(100, 100))))
            .id();
        app.update();
        assert_eq!(app.world().resource::<MenuUpdateCount>().0, 1);

        app.world_mut()
            .entity_mut(window)
            .get_mut::<Bounds>()
            .unwrap()
            .0
            .x += 10;
        app.update();
        assert_eq!(app.world().resource::<MenuUpdateCount>().0, 1);

        app.world_mut()
            .resource_mut::<Messages<Event>>()
            .write(Event::UpdaterStatusChanged);
        app.update();
        assert_eq!(app.world().resource::<MenuUpdateCount>().0, 2);
    }
}
