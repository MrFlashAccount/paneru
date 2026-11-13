use std::collections::HashMap;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use bevy::ecs::entity::Entity;
use bevy::ecs::hierarchy::ChildOf;
use bevy::ecs::query::Has;
use bevy::ecs::resource::Resource;
use bevy::ecs::system::Query;
use bevy::math::IRect;
use objc2_core_graphics::CGDirectDisplayID;
use serde::{Deserialize, Serialize};
use tracing::error;

use crate::ecs::layout::{Column, LayoutStrip, StackItem};
use crate::ecs::params::Windows;
use crate::ecs::{ActiveDisplayMarker, Position, Unmanaged};
use crate::manager::Application;
use crate::manager::Display;
use crate::platform::{Pid, ProcessSerialNumber, WinID, WorkspaceId};

pub const STATE_FILE_NAME: &str = "state.json";
const SUPPORTED_STATE_VERSION: u32 = 2;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Resource)]
pub struct PaneruState {
    pub version: u32,
    pub timestamp: u64,
    pub active_display_id: Option<CGDirectDisplayID>,
    #[serde(default)]
    pub displays: Vec<SavedDisplay>,
    pub workspaces: Vec<SavedWorkspace>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SavedDisplay {
    pub display_id: CGDirectDisplayID,
    pub bounds: SavedRect,
    pub active: bool,
    pub workspace_ids: Vec<WorkspaceId>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedRect {
    pub min_x: i32,
    pub min_y: i32,
    pub max_x: i32,
    pub max_y: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SavedWorkspace {
    pub workspace_id: WorkspaceId,
    pub display_id: Option<CGDirectDisplayID>,
    pub active_virtual_index: Option<u32>,
    pub strips: Vec<SavedStrip>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SavedStrip {
    pub virtual_index: u32,
    pub columns: Vec<SavedColumn>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum SavedColumn {
    Single(SavedWindow),
    Stack(Vec<SavedStackItem>),
    Tabs(Vec<SavedWindow>),
    Fullscreen(SavedWindow),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum SavedStackItem {
    Single(SavedWindow),
    Tabs(Vec<SavedWindow>),
}

impl SavedColumn {
    pub(crate) fn windows(&self) -> Box<dyn Iterator<Item = &SavedWindow> + '_> {
        match self {
            Self::Single(window) | Self::Fullscreen(window) => Box::new(std::iter::once(window)),
            Self::Tabs(windows) => Box::new(windows.iter()),
            Self::Stack(items) => Box::new(items.iter().flat_map(|item| match item {
                SavedStackItem::Single(window) => std::slice::from_ref(window).iter(),
                SavedStackItem::Tabs(windows) => windows.iter(),
            })),
        }
    }

    pub(crate) fn windows_mut(&mut self) -> Box<dyn Iterator<Item = &mut SavedWindow> + '_> {
        match self {
            Self::Single(window) | Self::Fullscreen(window) => Box::new(std::iter::once(window)),
            Self::Tabs(windows) => Box::new(windows.iter_mut()),
            Self::Stack(items) => Box::new(items.iter_mut().flat_map(|item| match item {
                SavedStackItem::Single(window) => std::slice::from_mut(window).iter_mut(),
                SavedStackItem::Tabs(windows) => windows.iter_mut(),
            })),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SavedWindow {
    // Primary matching (stable across WM restarts)
    pub window_id: WinID,
    pub pid: Pid,
    pub psn: ProcessSerialNumber,

    // Heuristic matching (if IDs change or apps restarted)
    pub bundle_id: String,
    pub title: String,
    pub identifier: String,
    pub role: String,
    pub subrole: String,

    /// Width relative to the display at save time. Optional for compatibility
    /// with v2 state files written before oversized-window persistence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width_ratio: Option<f64>,
    /// Owning strip's horizontal viewport position. Duplicated per window so
    /// old enum-shaped column data remains backward compatible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strip_position_x: Option<i32>,
}

impl From<IRect> for SavedRect {
    fn from(rect: IRect) -> Self {
        Self {
            min_x: rect.min.x,
            min_y: rect.min.y,
            max_x: rect.max.x,
            max_y: rect.max.y,
        }
    }
}

fn usable_window_role(role: &str) -> bool {
    // A locked session can return an application proxy in AXWindows. It is
    // not an identity update for the native window we are persisting. Keep
    // custom/forced window roles supported; reject only missing/app identity.
    !role.is_empty() && role != accessibility_sys::kAXApplicationRole
}

impl SavedWindow {
    pub fn from_entity(
        entity: Entity,
        windows: &Windows,
        apps: &Query<&Application>,
        strip_position_x: Option<i32>,
        previous: Option<&PaneruState>,
    ) -> Option<Self> {
        let (window, _, unmanaged) = windows.get_managed(entity)?;
        if matches!(unmanaged, Some(Unmanaged::Passthrough)) {
            return None;
        }
        let (_, _, app_entity) = windows.find_parent(window.id())?;
        let app = apps.get(app_entity).ok()?;

        let pid = window.pid().ok()?;
        let bundle_id = app.bundle_id().unwrap_or_default().clone();
        let previous = previous.and_then(|state| {
            state
                .windows()
                .find(|saved| saved.hard_match(window.id(), pid, &bundle_id))
        });
        // An app can stop answering AX queries before its destroy/terminate
        // event reaches us. A failed read is not an empty identity. Preserve
        // the last valid identity for this exact native window, while still
        // capturing its latest logical width and viewport position.
        let (title, identifier, role, subrole) = if let Ok(role) = window.role()
            && usable_window_role(&role)
        {
            (
                window.title().unwrap_or_else(|_| {
                    previous
                        .map(|saved| saved.title.clone())
                        .unwrap_or_default()
                }),
                window.identifier().unwrap_or_else(|_| {
                    previous
                        .map(|saved| saved.identifier.clone())
                        .unwrap_or_default()
                }),
                role,
                window.subrole().unwrap_or_else(|_| {
                    previous
                        .map(|saved| saved.subrole.clone())
                        .unwrap_or_default()
                }),
            )
        } else {
            let previous = previous.filter(|saved| usable_window_role(&saved.role))?;
            (
                previous.title.clone(),
                previous.identifier.clone(),
                previous.role.clone(),
                previous.subrole.clone(),
            )
        };

        Some(Self {
            window_id: window.id(),
            pid,
            psn: app.psn(),
            bundle_id,
            title,
            identifier,
            role,
            subrole,
            width_ratio: windows.width_ratio(entity),
            strip_position_x,
        })
    }

    pub fn hard_match(&self, other_id: WinID, other_proc_id: Pid, other_bundle: &str) -> bool {
        // 1. Exact match (including bundle to avoid cross-app PID collisions in edge cases)
        self.window_id == other_id && self.pid == other_proc_id && self.bundle_id == other_bundle
    }
}

impl PaneruState {
    pub(crate) fn windows(&self) -> impl Iterator<Item = &SavedWindow> {
        self.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.columns)
            .flat_map(SavedColumn::windows)
    }

    #[cfg(test)]
    pub(crate) fn windows_mut(&mut self) -> impl Iterator<Item = &mut SavedWindow> {
        self.workspaces
            .iter_mut()
            .flat_map(|workspace| &mut workspace.strips)
            .flat_map(|strip| &mut strip.columns)
            .flat_map(SavedColumn::windows_mut)
    }

    #[allow(clippy::type_complexity, clippy::too_many_lines)]
    pub fn extract<'a>(
        workspaces: impl IntoIterator<
            Item = (
                Option<&'a ChildOf>,
                &'a LayoutStrip,
                Option<&'a Position>,
                bool,
            ),
        >,
        displays: &Query<(&Display, Entity, Has<ActiveDisplayMarker>)>,
        windows: &Windows,
        apps: &Query<&Application>,
        previous: Option<&Self>,
    ) -> Self {
        let mut display_entity_ids = HashMap::new();
        let mut display_workspace_ids: HashMap<Entity, Vec<WorkspaceId>> = HashMap::new();
        let mut workspace_map: HashMap<WorkspaceId, SavedWorkspaceBuilder> = HashMap::new();
        let active_display_id = displays
            .iter()
            .find(|(_, _, active)| *active)
            .map(|(display, _, _)| display.id());

        for (display, entity, _) in displays {
            display_entity_ids.insert(entity, display.id());
            display_workspace_ids.insert(entity, Vec::new());
        }

        for (child, strip, strip_position, active_workspace) in workspaces {
            let strip_position_x = strip_position.map(|position| position.x);
            let display_entity = child.map(ChildOf::parent);
            let display_id =
                display_entity.and_then(|entity| display_entity_ids.get(&entity).copied());
            if let Some(entity) = display_entity
                && let Some(workspace_ids) = display_workspace_ids.get_mut(&entity)
                && !workspace_ids.contains(&strip.id())
            {
                workspace_ids.push(strip.id());
            }

            let mut saved_columns = Vec::new();
            for col in strip.columns() {
                let saved_col = match col {
                    Column::Single(entity) => {
                        SavedWindow::from_entity(*entity, windows, apps, strip_position_x, previous)
                            .map(SavedColumn::Single)
                    }
                    Column::Stack(items) => {
                        let saved_items = items
                            .iter()
                            .filter_map(|item| match item {
                                StackItem::Single(entity) => SavedWindow::from_entity(
                                    *entity,
                                    windows,
                                    apps,
                                    strip_position_x,
                                    previous,
                                )
                                .map(SavedStackItem::Single),
                                StackItem::Tabs(tabs) => {
                                    let saved_tabs: Vec<_> = tabs
                                        .iter()
                                        .filter_map(|&e| {
                                            SavedWindow::from_entity(
                                                e,
                                                windows,
                                                apps,
                                                strip_position_x,
                                                previous,
                                            )
                                        })
                                        .collect();
                                    if saved_tabs.is_empty() {
                                        None
                                    } else {
                                        Some(SavedStackItem::Tabs(saved_tabs))
                                    }
                                }
                            })
                            .collect::<Vec<_>>();
                        if saved_items.is_empty() {
                            None
                        } else {
                            Some(SavedColumn::Stack(saved_items))
                        }
                    }
                    Column::Tabs(tabs) => {
                        let saved_tabs: Vec<_> = tabs
                            .iter()
                            .filter_map(|&e| {
                                SavedWindow::from_entity(
                                    e,
                                    windows,
                                    apps,
                                    strip_position_x,
                                    previous,
                                )
                            })
                            .collect();
                        if saved_tabs.is_empty() {
                            None
                        } else {
                            Some(SavedColumn::Tabs(saved_tabs))
                        }
                    }
                    Column::Fullscren(entity) => {
                        SavedWindow::from_entity(*entity, windows, apps, strip_position_x, previous)
                            .map(SavedColumn::Fullscreen)
                    }
                };

                if let Some(sc) = saved_col {
                    saved_columns.push(sc);
                }
            }

            let workspace =
                workspace_map
                    .entry(strip.id())
                    .or_insert_with(|| SavedWorkspaceBuilder {
                        display_id,
                        active_virtual_index: None,
                        strips: Vec::new(),
                    });
            if workspace.display_id.is_none() {
                workspace.display_id = display_id;
            }
            if active_workspace {
                workspace.active_virtual_index = Some(strip.virtual_index);
            }
            workspace.strips.push(SavedStrip {
                virtual_index: strip.virtual_index,
                columns: saved_columns,
            });
        }

        let workspaces = workspace_map
            .into_iter()
            .map(|(workspace_id, mut workspace)| {
                workspace.strips.sort_by_key(|s| s.virtual_index);
                SavedWorkspace {
                    workspace_id,
                    display_id: workspace.display_id,
                    active_virtual_index: workspace.active_virtual_index,
                    strips: workspace.strips,
                }
            })
            .collect();
        let displays = displays
            .iter()
            .map(|(display, entity, active)| SavedDisplay {
                display_id: display.id(),
                bounds: display.bounds().into(),
                active,
                workspace_ids: display_workspace_ids.remove(&entity).unwrap_or_default(),
            })
            .collect();

        Self {
            version: SUPPORTED_STATE_VERSION,
            timestamp: now_timestamp(),
            active_display_id,
            displays,
            workspaces,
        }
    }

    pub fn save_to_file(&self, path: &Path) -> Result<(), std::io::Error> {
        let json = serde_json::to_string_pretty(self).map_err(|e| {
            error!("Failed to serialize state: {e}");
            std::io::Error::other(e)
        })?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp_path = path.with_extension("json.tmp");
        let mut tmp = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp_path)?;
        tmp.write_all(json.as_bytes())?;
        tmp.sync_all()?;
        fs::rename(tmp_path, path)?;
        if let Some(parent) = path.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    }

    pub fn load_from_file(path: &Path) -> Option<Self> {
        let data = fs::read_to_string(path).ok()?;
        let state: Self = serde_json::from_str(&data).ok()?;
        (state.version == SUPPORTED_STATE_VERSION).then_some(state)
    }

    pub fn default_state_file_path() -> PathBuf {
        xdg::BaseDirectories::with_prefix("paneru")
            .get_state_file(STATE_FILE_NAME)
            .expect("XDG state directory should be available")
    }

    #[cfg(test)]
    pub fn find_match(
        &self,
        window_id: WinID,
        pid: Pid,
        bundle_id: &str,
    ) -> Option<(WorkspaceId, u32, usize, SavedWindow)> {
        for workspace in &self.workspaces {
            for strip in &workspace.strips {
                for (col_idx, column) in strip.columns.iter().enumerate() {
                    let match_in_col = |sw: &SavedWindow| {
                        if sw.hard_match(window_id, pid, bundle_id) {
                            return Some(sw.clone());
                        }
                        None
                    };

                    match column {
                        SavedColumn::Single(sw) | SavedColumn::Fullscreen(sw) => {
                            if let Some(matched) = match_in_col(sw) {
                                return Some((
                                    workspace.workspace_id,
                                    strip.virtual_index,
                                    col_idx,
                                    matched,
                                ));
                            }
                        }
                        SavedColumn::Stack(items) => {
                            for item in items {
                                match item {
                                    SavedStackItem::Single(sw) => {
                                        if let Some(matched) = match_in_col(sw) {
                                            return Some((
                                                workspace.workspace_id,
                                                strip.virtual_index,
                                                col_idx,
                                                matched,
                                            ));
                                        }
                                    }
                                    SavedStackItem::Tabs(tabs) => {
                                        for sw in tabs {
                                            if let Some(matched) = match_in_col(sw) {
                                                return Some((
                                                    workspace.workspace_id,
                                                    strip.virtual_index,
                                                    col_idx,
                                                    matched,
                                                ));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        SavedColumn::Tabs(tabs) => {
                            for sw in tabs {
                                if let Some(matched) = match_in_col(sw) {
                                    return Some((
                                        workspace.workspace_id,
                                        strip.virtual_index,
                                        col_idx,
                                        matched,
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
        None
    }
}

#[derive(Default)]
struct SavedWorkspaceBuilder {
    display_id: Option<CGDirectDisplayID>,
    active_virtual_index: Option<u32>,
    strips: Vec<SavedStrip>,
}

fn now_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
