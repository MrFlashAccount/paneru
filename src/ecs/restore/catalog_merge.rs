//! Merge live layout edits with absent durable slots without moving live members.

use std::collections::HashMap;

use super::{WindowHardMatchKey, persisted_window_updates};
use crate::ecs::state::{
    PaneruState, SavedColumn, SavedStackItem, SavedStrip, SavedWindow, SavedWorkspace,
};

type Matches = HashMap<WindowHardMatchKey, WindowHardMatchKey>;

pub(super) fn merge(catalog: &PaneruState, mut live: PaneruState) -> PaneruState {
    let matches = persisted_window_updates(catalog, &live)
        .into_iter()
        .map(|(saved, current)| (saved, current.hard_key()))
        .collect::<Matches>();
    if matches.len() == catalog.windows().count() {
        return live;
    }

    for saved_workspace in &catalog.workspaces {
        for saved_strip in &saved_workspace.strips {
            if !saved_strip
                .columns
                .iter()
                .flat_map(SavedColumn::windows)
                .any(|window| !matches.contains_key(&window.hard_key()))
            {
                continue;
            }
            let workspace = workspace_for_missing_slots(&mut live, saved_workspace);
            let index = workspace
                .strips
                .iter()
                .position(|strip| strip.virtual_index == saved_strip.virtual_index)
                .unwrap_or_else(|| {
                    workspace.strips.push(SavedStrip {
                        virtual_index: saved_strip.virtual_index,
                        columns: Vec::new(),
                    });
                    workspace.strips.len() - 1
                });
            let strip = &mut workspace.strips[index];
            merge_sequence(&mut strip.columns, &saved_strip.columns, &matches);

            // This row still has absent slots. Its live pan uses compacted
            // coordinates; retain the full-layout pan, but not stale geometry,
            // order, grouping, or placement of the live windows.
            if let Some(pan) = saved_strip
                .columns
                .iter()
                .flat_map(SavedColumn::windows)
                .find_map(|window| window.strip_position_x)
            {
                for window in strip.columns.iter_mut().flat_map(SavedColumn::windows_mut) {
                    window.strip_position_x = Some(pan);
                }
            }
            workspace.strips.sort_by_key(|strip| strip.virtual_index);
        }
    }

    for workspace in &live.workspaces {
        if let Some(display_id) = workspace.display_id {
            if !live
                .displays
                .iter()
                .any(|display| display.display_id == display_id)
                && let Some(saved) = catalog
                    .displays
                    .iter()
                    .find(|display| display.display_id == display_id)
            {
                live.displays.push(saved.clone());
            }
            if let Some(display) = live
                .displays
                .iter_mut()
                .find(|display| display.display_id == display_id)
                && !display.workspace_ids.contains(&workspace.workspace_id)
            {
                display.workspace_ids.push(workspace.workspace_id);
            }
        }
    }
    live
}

fn workspace_for_missing_slots<'a>(
    live: &'a mut PaneruState,
    saved: &SavedWorkspace,
) -> &'a mut SavedWorkspace {
    let index = live
        .workspaces
        .iter()
        .position(|workspace| workspace.workspace_id == saved.workspace_id)
        .unwrap_or_else(|| {
            live.workspaces.push(SavedWorkspace {
                workspace_id: saved.workspace_id,
                display_id: saved.display_id,
                active_virtual_index: saved.active_virtual_index,
                strips: Vec::new(),
            });
            live.workspaces.len() - 1
        });
    &mut live.workspaces[index]
}

/// Every level keeps live order. Only missing nodes are inserted, after the
/// nearest surviving saved predecessor (or before a successor without one).
/// The same rule applies to columns, vertical stack items, and native tabs.
fn merge_sequence<T: CatalogNode>(live: &mut Vec<T>, saved: &[T], matches: &Matches) {
    for (index, saved_node) in saved.iter().enumerate() {
        let Some(missing) = saved_node.missing(matches) else {
            continue;
        };
        if let Some(current) = live
            .iter_mut()
            .find(|current| shares_member(*current, saved_node, matches))
            && current.merge_group(saved_node, matches)
        {
            continue;
        }
        let insertion = saved[..index]
            .iter()
            .rev()
            .find_map(|previous| {
                live.iter()
                    .position(|current| shares_member(current, previous, matches))
                    .map(|i| i + 1)
            })
            .or_else(|| {
                saved[index + 1..].iter().find_map(|next| {
                    live.iter()
                        .position(|current| shares_member(current, next, matches))
                })
            })
            .unwrap_or(index.min(live.len()));
        live.insert(insertion, missing);
    }
}

fn shares_member<T: CatalogNode>(live: &T, saved: &T, matches: &Matches) -> bool {
    saved.windows().any(|saved_window| {
        let saved_key = saved_window.hard_key();
        let key = matches.get(&saved_key).unwrap_or(&saved_key);
        live.windows().any(|current| key == &current.hard_key())
    })
}

/// Internal catalog tree operations; matching policy remains owned by restore.
trait CatalogNode: Sized {
    fn windows(&self) -> impl Iterator<Item = &SavedWindow>;
    fn missing(&self, matches: &Matches) -> Option<Self>;
    fn merge_group(&mut self, saved: &Self, matches: &Matches) -> bool;
}

impl CatalogNode for SavedWindow {
    fn windows(&self) -> impl Iterator<Item = &SavedWindow> {
        std::iter::once(self)
    }

    fn missing(&self, matches: &Matches) -> Option<Self> {
        (!matches.contains_key(&self.hard_key())).then(|| self.clone())
    }

    fn merge_group(&mut self, _: &Self, _: &Matches) -> bool {
        false
    }
}

impl CatalogNode for SavedStackItem {
    fn windows(&self) -> impl Iterator<Item = &SavedWindow> {
        match self {
            Self::Single(window) => std::slice::from_ref(window).iter(),
            Self::Tabs(windows) => windows.iter(),
        }
    }

    fn missing(&self, matches: &Matches) -> Option<Self> {
        match self {
            Self::Single(window) => window.missing(matches).map(Self::Single),
            Self::Tabs(windows) => {
                let missing = windows
                    .iter()
                    .filter_map(|window| window.missing(matches))
                    .collect::<Vec<_>>();
                (!missing.is_empty()).then_some(Self::Tabs(missing))
            }
        }
    }

    fn merge_group(&mut self, saved: &Self, matches: &Matches) -> bool {
        let Self::Tabs(saved) = saved else {
            return false;
        };
        if let Self::Single(window) = self {
            *self = Self::Tabs(vec![window.clone()]);
        }
        if let Self::Tabs(windows) = self {
            merge_sequence(windows, saved, matches);
        }
        true
    }
}

impl CatalogNode for SavedColumn {
    fn windows(&self) -> impl Iterator<Item = &SavedWindow> {
        self.windows()
    }

    fn missing(&self, matches: &Matches) -> Option<Self> {
        match self {
            Self::Single(window) => window.missing(matches).map(Self::Single),
            Self::Fullscreen(window) => window.missing(matches).map(Self::Fullscreen),
            Self::Tabs(windows) => {
                let missing = windows
                    .iter()
                    .filter_map(|window| window.missing(matches))
                    .collect::<Vec<_>>();
                (!missing.is_empty()).then_some(Self::Tabs(missing))
            }
            Self::Stack(items) => {
                let missing = items
                    .iter()
                    .filter_map(|item| item.missing(matches))
                    .collect::<Vec<_>>();
                (!missing.is_empty()).then_some(Self::Stack(missing))
            }
        }
    }

    fn merge_group(&mut self, saved: &Self, matches: &Matches) -> bool {
        match saved {
            Self::Tabs(saved) => {
                if let Self::Single(window) = self {
                    *self = Self::Tabs(vec![window.clone()]);
                }
                let Self::Tabs(windows) = self else {
                    return false;
                };
                merge_sequence(windows, saved, matches);
            }
            Self::Stack(saved) => {
                match self {
                    Self::Single(window) => {
                        *self = Self::Stack(vec![SavedStackItem::Single(window.clone())]);
                    }
                    Self::Tabs(windows) => {
                        *self = Self::Stack(vec![SavedStackItem::Tabs(std::mem::take(windows))]);
                    }
                    _ => {}
                }
                let Self::Stack(items) = self else {
                    return false;
                };
                merge_sequence(items, saved, matches);
            }
            _ => return false,
        }
        true
    }
}
