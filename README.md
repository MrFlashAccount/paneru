<div align="center">
  <img src="./images/paneru.png" alt="Paneru" width="600"/>
</div>

##
A sliding, tiling window manager for MacOS.

## About

Paneru is a MacOS window manager that arranges windows on an infinite strip,
extending to the right. A core principle is that opening a new window will
**never** cause existing windows to resize, maintaining your layout stability.

Each monitor operates with its own independent window strip, ensuring that
windows remain confined to their respective displays and do not "overflow" onto
adjacent monitors.

https://github.com/user-attachments/assets/cbc2e820-635f-408b-923a-6cb47c44704c

(Video by @emreekici3 - https://github.com/emreekici3/dotfiles)

https://github.com/user-attachments/assets/793e7eaa-7909-4086-8380-1fb7861f8780


## Why Paneru?

- **Niri-like Behavior on MacOS:** Inspired by the user experience of [Niri],
  Paneru aims to bring a similar scrollable tiling workflow to MacOS.
- **Works with MacOS workspaces:** You can use existing workspaces and switch
  between them with keyboard or touchpad gestures - with a separate window strip
  on each. Drag and dropping windows between them works as well.
- **Virtual Workspaces (Experimental):** Group your windows into tasks by
  stacking multiple horizontal strips (rows) within a single space. Use native
  macOS workspaces for broad segregation (e.g., 'Work', 'Personal') and virtual
  workspaces to stay organized within each context.
- **Menu bar workspace indicator:** Shows the currently active virtual
  workspace in the macOS menu bar.
- **Startup session restore:** Restores managed window layouts, virtual
  workspaces, and display assignments from the last saved state when Paneru
  starts.
- **Focus follows mouse on MacOS:** Very useful for people who would like to
  avoid an extra click.
- **Sliding windows with touchpad:** Using a touchpad is quite natural for
  navigation of the window pane.
- **Native macOS tabs support:** Applications like Ghostty use these, so
  Paneru manages them on the layout strip like other windows.
- **Optimal for Large Displays:** Standard tiling window managers can be
  suboptimal for large displays, often resulting in either huge maximized
  windows or numerous tiny, unusable windows. Paneru addresses this by
  providing a more flexible and practical arrangement.
- **Improved Small Display Usability:** On smaller displays (like laptops),
  traditional tiling can make windows too small to be productive, forcing users
  to constantly maximize. Paneru's sliding strip approach aims to provide a
  better experience without this compromise.

## Inspiration

The fundamental architecture and window management techniques are heavily
inspired by [Yabai], another excellent MacOS window manager. Studying its
source code has provided invaluable insights into managing windows on MacOS,
particularly regarding undocumented functions.

The innovative concept of managing windows on a sliding strip is directly
inspired by [Niri] and [PaperWM.spoon].

## Installation

### Installing the macOS app

Download `Paneru-<version>.dmg` from the
[latest GitHub Release](https://github.com/MrFlashAccount/paneru/releases/latest),
open it, and drag `Paneru.app` to Applications. Paneru lives in the menu bar;
there is no Dock icon or main window. Until Accessibility access is granted it
stays alive as **Paneru !** in the menu bar. Use **Open Accessibility
Settings…**, press `+`, select `/Applications/Paneru.app`, and enable it. Paneru
starts automatically as soon as macOS grants access.

Prebuilt GitHub releases require an Apple Silicon Mac. Intel Macs are not
supported by the downloadable app.

The current releases are ad-hoc signed but not Apple-notarized. If macOS blocks
the first launch, control-click Paneru in Applications and choose **Open**. If
that is still blocked, remove the downloaded quarantine attribute once:

```shell
xattr -dr com.apple.quarantine /Applications/Paneru.app
```

Paneru checks the signed GitHub update feed automatically in the background.
Use **Check for Updates…** in the menu bar to check immediately. Update
archives and the appcast are protected with Sparkle's Ed25519 signatures.

On macOS 13 or later, use **Launch at Login** in the menu bar to let macOS
start the installed `Paneru.app` when you sign in. If macOS requires manual
approval, Paneru opens **System Settings → General → Login Items**. This native
login item is the supported way to start Paneru automatically.

Because the current builds are ad-hoc signed, macOS may require Accessibility
approval again after installing a different Paneru version. A stable Developer
ID signature is required to preserve that approval safely across updates.

### Getting started

Paneru has no main window. After launch, click **Paneru** in the menu bar to
control the window that was focused immediately before you opened the menu.
The currently configured shortcuts appear at the right side of their menu
items, using the standard macOS symbols.

#### 1. Add windows to the managed strip

1. Click the application window you want Paneru to control.
2. Open **Paneru** in the menu bar.
3. Choose **Toggle Managed** or press `Control-Option-Command-M`
   (`⌃⌥⌘M`).
4. Repeat for the other application windows you want in the same horizontal
   strip.

Managed windows are tiled next to one another on the current display and macOS
Space. Normal windows start in passthrough mode and behave like ordinary macOS
windows until you opt them in. **Toggle Managed** changes only the concrete
window that was focused when the shortcut was pressed or immediately before
the menu opened; it does not toggle the workspace or every window in an
application. Toggling the window off restores its pre-management frame when
Paneru captured one.

If the width actions are disabled, the focused window is not currently managed.
Choose **Toggle Managed** first. If **Toggle Managed** is also disabled, click a
normal application window and reopen the menu; panels, popovers, and some
non-standard windows cannot be managed automatically.

#### 2. Set the window width

Focus a managed window, then choose a percentage under **Window width** in the
menu bar or use its shortcut:

| Width | Shortcut |
| :--- | :--- |
| 50% | `⌃⌥⌘1` |
| 75% | `⌃⌥⌘2` |
| 100% | `⌃⌥⌘3` |
| 150% | `⌃⌥⌘4` |
| 200% | `⌃⌥⌘5` |

The percentage is relative to the usable width of the display. A 150% or 200%
window is intentionally wider than the screen; Paneru keeps it in the
scrollable strip instead of shrinking it to fit. Use **Center Window** or
`⌃⌥⌘C` to bring the focused window to the center of the viewport.

#### 3. Scroll through the strip

Hold `Option` (`⌥`) and scroll with two fingers on the trackpad. A horizontal
gesture moves along the strip; a vertical two-finger scroll is also accepted
when it has no horizontal component. The same modifier works with a mouse
scroll wheel.

The generated configuration uses a reversed direction, sensitivity `0.20`,
sticky scrolling, paging, and `snap_padding = 32`. Paging limits each gesture
to adjacent stops: a regular window has one stop, while a window wider than the
display has exactly two—its left and right edges. Starting a gesture between
stops cannot skip the first edge in either direction. Sticky release snapping
engages only within `snap_padding` logical points of a real window edge, even
when paging is enabled; outside that zone, the strip stays where you released
it. Paneru does not claim the native three-finger gesture by default, so the
usual macOS gesture for switching Spaces remains available.

#### Default controls

| Action | Menu item | Shortcut |
| :--- | :--- | :--- |
| Add or remove the focused window from the strip | **Toggle Managed** | `⌃⌥⌘M` |
| Set an exact width | **Window width → 50–200%** | `⌃⌥⌘1`–`⌃⌥⌘5` |
| Center the focused window | **Center Window** | `⌃⌥⌘C` |
| Check for a new version | **Check for Updates…** | — |
| Quit Paneru | **Quit Paneru** | `⌃⌥⌘Q` |

Paneru writes the first-run configuration to
`$XDG_CONFIG_HOME/paneru/paneru.toml` (usually
`~/.config/paneru/paneru.toml`). Changes are reloaded while Paneru is running.
See the **[Configuration Guide](./CONFIGURATION.md)** to change gestures,
shortcuts, width presets, window rules, or session restore behavior.

### Recommended System Options

- Like all non-native window managers for MacOS, Paneru requires accessibility
  access to move windows. Once it runs you may get a dialog window asking for
  permissions. Otherwise check the setting in System Settings under "Privacy &
  Security -> Accessibility".

- Check your System Settings for "Displays have separate spaces" option. It
  should be enabled - this allows Paneru to manage the workspaces independently.

- **Multiple displays**. Paneru is moving the windows off-screen, hiding them
  to the left or right. If you have multiple displays, for example your laptop
  open when docked to an external monitor you may experience weird behavior.
  The issue is that when MacOS notices a window being moved too far off-screen
  it will relocate it to a different display - which confuses Paneru! The
  solution is to change the spatial arrangement of your additional display -
  instead of having it to the left or right, move it above or below your main
  display.
  A [similar situation](https://nikitabobko.github.io/AeroSpace/guide#proper-monitor-arrangement)
  exists with Aerospace window manager.
  An option exists (`horizontal_mouse_warp`) which can make a vertical
  arrangement of displays "feel" horizontal.

- **Off-screen window slivers**. Because macOS will forcibly relocate windows
  that are moved fully off-screen, Paneru keeps a thin sliver of each
  off-screen window visible at the screen edge. The `sliver_width` and
  `sliver_height` options control the size of this sliver. This is a
  workaround for a macOS limitation, not a design choice.

### Building the macOS app from source

```shell
$ git clone https://github.com/karinushka/paneru.git
$ cd paneru
$ ./scripts/build-app.sh
$ open .build/release/Paneru.app
```

Local builds target the current Mac architecture. GitHub release builds target
Apple Silicon (`arm64`) only.

### Configuration

Paneru checks for configuration in following locations:

- `$HOME/.paneru`
- `$HOME/.paneru.toml`
- `$XDG_CONFIG_HOME/paneru/paneru.toml`

Additionally it allows overriding the location with `$PANERU_CONFIG` environment variable.
If none of these files exists, Paneru creates
`$XDG_CONFIG_HOME/paneru/paneru.toml` with the built-in defaults on first launch.

You can use the following basic configuration as a starting point. For a
complete guide to all available options, keybindings, and window rules, see the
**[Configuration Guide](./CONFIGURATION.md)**.

```toml
# basic .paneru.toml
[options]
focus_follows_mouse = true
mouse_follows_focus = true

[bindings]
window_focus_west = "cmd - h"
window_focus_east = "cmd - l"
window_resize = "alt - r"
window_center = "alt - c"
quit = "ctrl + alt - q"
```

### Live reloading

Changes made to the active configuration file are automatically reloaded while
Paneru is running. This is useful for tweaking keyboard bindings and other
settings without restarting the application.

### Startup session restore

Paneru saves managed window layout state to the user state directory
(`$XDG_STATE_HOME/paneru/state.json`, usually
`~/.local/state/paneru/state.json`) after a short quiet period following a
relevant layout change and flushes pending state on shutdown. During the
startup restore window, Paneru matches reopened windows to the saved session and
restores their layout placement, virtual workspace row, display assignment,
oversized width ratio, and horizontal strip pan where possible.

Restore is startup-only. After the configured startup grace period expires, new
or unmatched windows follow the normal configuration and window-rule behavior.
Saved windows that are not present are ignored by default and the restored
layout is compacted around the windows that were found. The behavior is
configured with `[restore]`; see the
**[Session Restore](./CONFIGURATION.md#session-restore)** section in the
configuration guide.
When restore is disabled, Paneru neither reads nor writes the state file.


## Future Enhancements

- More commands for manipulating windows: finegrained size adjustments, touchpad resizing, etc.
- Scriptability. For example using Lua for configuration or automation of window handling,
  like triggering and positioning specific windows or applications.

## Communication

There is a public Matrix room
[`#paneru:matrix.org`](https://matrix.to/#/%23paneru%3Amatrix.org). Join and
ask any questions.

## Architecture Overview

For a detailed high-level overview of Paneru's internal design, data flow, and
ECS patterns, please refer to the **[Architecture Guide](./ARCHITECTURE.md)**.

Paneru's architecture is built around the **Bevy ECS (Entity Component
System)**, which manages the window manager's state as a collection of entities
(displays, workspaces, applications, and windows) and components.

The system is decoupled into three primary layers:

1.  **Platform Layer (`src/platform/`)**: Directly interfaces with macOS via `objc2` and Core Graphics. It runs the native Cocoa event loop and pumps OS events into a channel consumed by Bevy.
2.  **Management Layer (`src/manager/`)**: Defines OS-agnostic traits (`WindowManagerApi`, `WindowApi`) that abstract window manipulation. The macOS-specific implementations (`WindowManagerOS`, `WindowOS`) bridge these traits to the Accessibility and SkyLight APIs.
3.  **ECS Layer (`src/ecs/`)**: The "brain" of the application. Bevy systems process incoming events, handle input triggers, and manage animations.

### Repository Structure

- **`main` branch**: Contains the stable, released code.
- **`testing` branch**: Used for experimental features and architectural refactors. This branch is volatile and may be force-pushed.

### Publishing a release

1. Update the package version in `Cargo.toml` and commit the change.
2. Add `SPARKLE_ED25519_PRIVATE_KEY` to the protected GitHub `release`
   environment. It must match `SUPublicEDKey` in `assets/Info.plist`.
3. Run the **Release** workflow and enter that same version, with or without a
   leading `v`.

The workflow cross-compiles `arm64` and `x86_64`, creates a universal
`Paneru.app`, ZIP and DMG, generates and verifies a signed `appcast.xml`, tags
the selected commit, and publishes all three files to GitHub Releases. The ZIP
is Sparkle's update enclosure; the DMG is the human-facing installer.

For a production distribution without Gatekeeper warnings—and to keep macOS
Accessibility approval stable across releases—the next step is Developer ID
signing and Apple notarization. Sparkle's Ed25519 signature secures updates,
but it does not replace Apple's code-signing identity.

## Tile Scrollably Elsewhere

Here are some other projects which implement a similar workflow:

- [Niri]: a scrollable tiling Wayland compositor.
- [PaperWM]: scrollable tiling on top of GNOME Shell.
- [karousel]: scrollable tiling on top of KDE.
- [papersway]: scrollable tiling on top of sway/i3.
- [hyprscroller] and [hyprslidr]: scrollable tiling on top of Hyprland.
- [PaperWM.spoon]: scrollable tiling for MacOS on top of HammerSpoon.
- [Nehir]: scrollable tiling for MacOS

[Yabai]: https://github.com/koekeishiya/yabai
[Niri]: https://github.com/YaLTeR/niri
[PaperWM]: https://github.com/paperwm/PaperWM
[karousel]: https://github.com/peterfajdiga/karousel
[papersway]: https://spwhitton.name/tech/code/papersway/
[hyprscroller]: https://github.com/dawsers/hyprscroller
[hyprslidr]: https://gitlab.com/magus/hyprslidr
[PaperWM.spoon]: https://github.com/mogenson/PaperWM.spoon
[Nehir]: https://github.com/Guria/Nehir
