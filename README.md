<p align="center">
  <img src="./assets/icons/AppIcon.svg" width="112" height="112" alt="Paneru app icon">
</p>

# Paneru

**A software ultrawide for laptop-first macOS work.**

Paneru lets an individual macOS window use 150% or 200% of your built-in display's width. Hold `Option` and scroll to pan across it. It is built for laptop-first work: a wide editor, terminal, research view, document, or canvas while you are traveling or away from a desk—without buying an ultrawide or carrying an external monitor.

Paneru does not add pixels or show the entire wide window at once. It treats the physical display as a viewport over a wider working surface. Only windows you opt in are managed; everything else continues to behave like an ordinary macOS window.

## How it works

- **Opt in only what you need.** Choose one window manually or through a rule; Paneru leaves everything else alone.
- **Make it wider than the screen.** Fresh-install presets include 150% and 200% of the usable display width.
- **Pan across it.** Hold `Option` and scroll to move between the oversized window's left and right edges. Opt in more windows to build a horizontal strip without resizing the existing windows.

Paneru has no Dock icon or main window. Daily controls live in the menu bar, with configured shortcuts shown using native macOS menu equivalents.

## Install

1. Download `Paneru-<version>.dmg` from the [latest release](https://github.com/MrFlashAccount/paneru/releases/latest).
2. Open the DMG and drag `Paneru.app` to `/Applications`.
3. Open Paneru. It will appear in the menu bar.

The downloadable app requires an Apple Silicon Mac and targets macOS 11 or later. **Launch at Login** requires macOS 13 or later.

Current releases are ad-hoc signed and not Apple-notarized, so macOS may warn on first launch. Control-click `Paneru.app` in Applications and choose **Open**. See [Troubleshooting](#troubleshooting) if it is still blocked.

## Grant Accessibility access

Paneru needs Accessibility permission to move and resize windows.

1. Choose **Continue** in Paneru's setup dialog.
2. In **System Settings → Privacy & Security → Accessibility**, enable Paneru.
3. Return to the menu bar; Paneru becomes ready as soon as access is available.

If Paneru is already listed but permission does not take effect, remove the old entry with the `−` button, add `/Applications/Paneru.app` again with `+`, and enable it.

## Your first oversized window

1. Focus the window you want to control.
2. Open Paneru in the menu bar and choose **Toggle Managed**, or press `⌃⌥⌘M`.
3. Choose **150%** or **200%** under **Window width**. On a fresh install, the shortcuts are `⌃⌥⌘4` and `⌃⌥⌘5`.
4. Hold `Option` (`⌥`) and scroll with two fingers. The strip slides across the display, revealing the rest of the oversized window.
5. Toggle more windows into managed mode to build a multi-window strip.

Use **Center Window** (`⌃⌥⌘C`) to bring the focused managed window back to the center. Toggle a window out of managed mode to return it to its captured pre-management frame when that frame is available.

## Daily use

The fresh-install configuration provides these menu actions and shortcuts:

| Action | Shortcut |
| --- | --- |
| Toggle the focused window in or out of the strip | `⌃⌥⌘M` |
| Set width to 50%, 75%, 100%, 150%, or 200% | `⌃⌥⌘1`–`⌃⌥⌘5` |
| Center the focused managed window | `⌃⌥⌘C` |
| Pan through the strip | `⌥` + scroll |
| Quit Paneru | `⌃⌥⌘Q` |

Menu commands apply to the window that was focused immediately before you opened the menu. Width and centering actions stay disabled until that window is managed.

The menu bar icon reflects the selected window's state: managed, unmanaged, or no manageable window.

Paneru saves managed layout state by default. At startup, it restores window order, widths—including oversized ratios—and strip position for matching windows that the current configuration marks as managed. Use `manage = true` window rules for durable ownership: a one-off **Toggle Managed** choice applies only to the current window instance and does not automatically opt a relaunched window back in. Missing saved windows are ignored rather than blocking startup.

Native macOS tab groups remain usable. Paneru infers a new tab when a same-app window shares the existing window's frame; set `disable_native_tabs = true` if that heuristic groups unrelated windows.

The default `Option` + scroll path does not take over macOS's three-finger Space-switching gesture.

## Configure Paneru

If no supported configuration exists, Paneru creates `$XDG_CONFIG_HOME/paneru/paneru.toml`, usually:

```text
~/.config/paneru/paneru.toml
```

It also recognizes `~/.paneru`, `~/.paneru.toml`, `$XDG_CONFIG_HOME/paneru/paneru.toml`, and an existing file selected through `$PANERU_CONFIG`. Changes are reloaded while Paneru is running.

Use the [Configuration Guide](./CONFIGURATION.md) to change width presets, scroll sensitivity and direction, paging and sticky snapping, shortcuts, window rules, session restore, displays, or optional virtual workspaces.

## Updates and startup

Paneru checks its signed Sparkle update feed automatically. Choose **Check for Updates…** in the menu bar to check immediately.

On macOS 13 or later, **Launch at Login** registers the installed app through macOS. If approval is required, Paneru opens **System Settings → General → Login Items**.

Because current releases do not use a stable Developer ID signature, macOS may require Accessibility approval again after an update. Removing the stale Accessibility entry and adding the installed app again repairs that state.

## Troubleshooting

### Menu actions are disabled

Focus a normal application window and reopen the Paneru menu. Choose **Toggle Managed** before using width or centering actions. Panels, popovers, and some non-standard windows may require an explicit `manage = true` [window rule](./CONFIGURATION.md#6-window-rules-windows).

### macOS still blocks the app

First control-click Paneru in Applications and choose **Open**. If you trust the downloaded release and macOS still refuses to launch it, remove the quarantine attribute once:

```shell
xattr -dr com.apple.quarantine /Applications/Paneru.app
```

### Accessibility stops working after an update

Open **System Settings → Privacy & Security → Accessibility**, remove the existing Paneru entry, add `/Applications/Paneru.app` again, and enable it.

### Windows jump between horizontally arranged displays

Paneru keeps off-screen windows just beyond the current display. macOS may relocate those windows onto a neighboring display when displays are arranged side by side. Arranging secondary displays above or below the main display avoids that conflict. The `horizontal_mouse_warp` option can preserve a horizontal-feeling cursor flow with that arrangement.

### A thin edge of an off-screen window remains visible

This is intentional. macOS can relocate windows that are completely off-screen, so Paneru leaves a small sliver at the display edge. Configure it with `sliver_width` and `sliver_height`.

## Build from source

You need Rust and the Xcode Command Line Tools. The build script downloads Sparkle, builds the host architecture, packages a real app bundle, and ad-hoc signs it:

```shell
git clone https://github.com/MrFlashAccount/paneru.git
cd paneru
./scripts/build-app.sh
open .build/release/Paneru.app
```

Useful validation commands:

```shell
cargo check --all-targets --locked
cargo clippy --bin paneru --locked -- -D warnings
cargo test --all-targets --locked -- --test-threads=1
```

For implementation details, see the [Architecture Guide](./ARCHITECTURE.md).

## Acknowledgements

This fork builds on the original [Paneru](https://github.com/karinushka/paneru) by Karinushka.

The sliding-strip interaction is inspired by [niri](https://github.com/YaLTeR/niri) and [PaperWM.spoon](https://github.com/mogenson/PaperWM.spoon). The macOS window-management architecture draws inspiration from [yabai](https://github.com/koekeishiya/yabai).

Paneru is available under the [MIT License](./LICENSE.txt).
