use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use bevy::ecs::entity::Entity;
use bevy::ecs::hierarchy::ChildOf;
use bevy::ecs::observer::On;
use bevy::ecs::query::Has;
use bevy::ecs::resource::Resource;
use bevy::ecs::system::{Commands, Query, Res, ResMut};
use objc2_core_graphics::CGDirectDisplayID;
use tracing::{Level, debug, info, instrument, warn};

use crate::config::{Config, MissingWindowBehavior};
use crate::ecs::layout::LayoutStrip;
use crate::ecs::params::Windows;
use crate::ecs::state::{
    PaneruState, SavedColumn, SavedStackItem, SavedStrip, SavedWindow, SavedWorkspace,
};
use crate::ecs::workspace::PreviousStripPosition;
use crate::ecs::{
    ActiveDisplayMarker, ActiveWorkspaceMarker, RefreshWindowSizes, RestoreWindowState,
    RestoredWindowMarker, SpawnCommandsExt, Unmanaged, WindowDisposition, WindowProperties,
};
use crate::manager::{Application, Display, Window};
use crate::platform::{Pid, WinID, WorkspaceId};

#[derive(Debug, Resource)]
pub(crate) struct SessionRestore {
    state: PaneruState,
    deadline: Instant,
    startup_grace_complete: bool,
    saved_hard_keys: HashSet<WindowHardMatchKey>,
}

impl SessionRestore {
    fn new(state: PaneruState, grace: Duration) -> Self {
        Self::new_at(state, grace, Instant::now())
    }

    fn new_at(state: PaneruState, grace: Duration, now: Instant) -> Self {
        let saved_hard_keys = saved_hard_match_keys(&state);
        Self {
            state,
            deadline: now + grace,
            startup_grace_complete: false,
            saved_hard_keys,
        }
    }

    pub(crate) fn from_live_snapshot(state: PaneruState) -> Self {
        let saved_hard_keys = saved_hard_match_keys(&state);
        Self {
            state,
            deadline: Instant::now(),
            startup_grace_complete: true,
            saved_hard_keys,
        }
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        (!self.startup_grace_complete).then_some(self.deadline)
    }

    fn expired(&self, now: Instant) -> bool {
        !self.startup_grace_complete && now >= self.deadline
    }

    fn complete_startup_grace(&mut self) {
        self.startup_grace_complete = true;
    }

    pub(crate) fn catalog(&self) -> &PaneruState {
        &self.state
    }

    pub(crate) fn reconcile_persisted_state(&mut self, live: PaneruState) -> PaneruState {
        let updates = persisted_window_updates(&self.state, &live);
        if updates.len() == self.state.windows().count() {
            self.state = live;
        } else {
            // Keep the absent slots/order and full-layout viewport. A compacted
            // live strip's pan is in a different coordinate system. Per-window
            // identities and widths must still follow the open windows.
            for saved in self.state.windows_mut() {
                if let Some(current) = updates.get(&saved.hard_key()) {
                    let full_layout_pan = saved.strip_position_x;
                    saved.clone_from(current);
                    saved.strip_position_x = full_layout_pan;
                }
            }
        }
        self.saved_hard_keys = saved_hard_match_keys(&self.state);
        self.state.clone()
    }

    pub(crate) fn forget_window(
        &mut self,
        window: &Window,
        app: &Application,
        mut live_windows: impl Iterator<Item = (WinID, Pid)>,
    ) -> bool {
        let Ok(pid) = window.pid() else {
            return false;
        };
        let bundle_id = app.bundle_id().unwrap_or_default().clone();
        let hard_key = WindowHardMatchKey::new(window.id(), pid, bundle_id.clone());
        let key = saved_windows_in_state(&self.state)
            .any(|saved| saved.hard_key() == hard_key)
            .then_some(hard_key)
            .or_else(|| {
                let key = unique_fallback_key(
                    &self.state,
                    &bundle_id,
                    &window.title().unwrap_or_default(),
                    &window.identifier().unwrap_or_default(),
                    &window.role().unwrap_or_default(),
                    &window.subrole().unwrap_or_default(),
                )?;
                // A relaunched window may be toggled off before the debounced
                // save refreshes its native identity. Fallback is valid only
                // when the old native owner is absent, never for a live sibling.
                (!live_windows.any(|(id, pid)| key.window_id == id && key.pid == pid))
                    .then_some(key)
            });
        let removed = key.is_some_and(|key| remove_saved_window(&mut self.state, &key));
        if removed {
            self.saved_hard_keys = saved_hard_match_keys(&self.state);
        }
        removed
    }

    #[cfg(test)]
    pub(crate) fn expire_now(&mut self) {
        self.deadline = Instant::now();
    }
}

#[allow(clippy::needless_pass_by_value)]
pub(super) fn tick_restore_grace(mut session: Option<ResMut<SessionRestore>>) {
    let Some(session) = session.as_mut() else {
        return;
    };

    if session.expired(Instant::now()) {
        info!("Session restore startup grace period ended; durable catalog remains active");
        session.complete_startup_grace();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CurrentWindowIdentity {
    pub entity: Entity,
    pub window_id: WinID,
    pub pid: Pid,
    pub bundle_id: String,
    pub title: String,
    pub identifier: String,
    pub role: String,
    pub subrole: String,
}

impl CurrentWindowIdentity {
    fn hard_key(&self) -> WindowHardMatchKey {
        WindowHardMatchKey::new(self.window_id, self.pid, self.bundle_id.clone())
    }

    #[cfg(test)]
    pub(crate) fn fallback_only(entity: Entity, bundle_id: &str, title: &str) -> Self {
        Self {
            entity,
            window_id: -1,
            pid: -1,
            bundle_id: bundle_id.to_string(),
            title: title.to_string(),
            identifier: "main".to_string(),
            role: "AXWindow".to_string(),
            subrole: "AXStandardWindow".to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct WindowHardMatchKey {
    window_id: WinID,
    pid: Pid,
    bundle_id: String,
}

impl WindowHardMatchKey {
    fn new(window_id: WinID, pid: Pid, bundle_id: String) -> Self {
        Self {
            window_id,
            pid,
            bundle_id,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PlannedColumn {
    Single(Entity),
    Stack(Vec<PlannedStackItem>),
    Tabs(Vec<Entity>),
    Fullscreen(Entity),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PlannedStackItem {
    Single(Entity),
    Tabs(Vec<Entity>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlannedStrip {
    pub workspace_id: WorkspaceId,
    pub display_id: Option<CGDirectDisplayID>,
    pub virtual_index: u32,
    pub columns: Vec<PlannedColumn>,
    pub position_x: Option<i32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct SavedGeometry {
    pub width_ratio: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RestorePlan {
    pub strips: Vec<PlannedStrip>,
    pub active_virtual_by_workspace: HashMap<WorkspaceId, u32>,
    pub consumed_entities: HashSet<Entity>,
    pub ignored_missing_windows: usize,
    pub skipped_ambiguous_matches: usize,
    pub geometry: HashMap<Entity, SavedGeometry>,
}

pub(crate) struct RestorePlanner<'a> {
    state: &'a PaneruState,
    saved_hard_keys: HashSet<WindowHardMatchKey>,
}

impl<'a> RestorePlanner<'a> {
    pub(crate) fn new(state: &'a PaneruState) -> Self {
        Self {
            state,
            saved_hard_keys: saved_hard_match_keys(state),
        }
    }

    pub(crate) fn plan(&self, current: &[CurrentWindowIdentity]) -> RestorePlan {
        let mut plan = RestorePlan::default();

        for workspace in &self.state.workspaces {
            let surviving_strips = self.plan_workspace(workspace, current, &mut plan);
            Self::record_active_virtual(workspace, &surviving_strips, &mut plan);
            plan.strips.extend(surviving_strips);
        }

        plan
    }

    fn plan_workspace(
        &self,
        workspace: &SavedWorkspace,
        current: &[CurrentWindowIdentity],
        plan: &mut RestorePlan,
    ) -> Vec<PlannedStrip> {
        workspace
            .strips
            .iter()
            .filter_map(|strip| self.plan_strip(workspace, strip, current, plan))
            .collect()
    }

    fn plan_strip(
        &self,
        workspace: &SavedWorkspace,
        strip: &SavedStrip,
        current: &[CurrentWindowIdentity],
        plan: &mut RestorePlan,
    ) -> Option<PlannedStrip> {
        let columns = strip
            .columns
            .iter()
            .filter_map(|column| self.plan_column(column, current, plan))
            .collect::<Vec<_>>();

        (!columns.is_empty()).then_some(PlannedStrip {
            workspace_id: workspace.workspace_id,
            display_id: workspace.display_id,
            virtual_index: strip.virtual_index,
            columns,
            position_x: strip
                .columns
                .iter()
                .flat_map(saved_windows_in_column)
                .find_map(|window| window.strip_position_x),
        })
    }

    fn plan_column(
        &self,
        column: &SavedColumn,
        current: &[CurrentWindowIdentity],
        plan: &mut RestorePlan,
    ) -> Option<PlannedColumn> {
        match column {
            SavedColumn::Single(saved) => self
                .match_window(saved, current, plan)
                .map(PlannedColumn::Single),
            SavedColumn::Fullscreen(saved) => self
                .match_window(saved, current, plan)
                .map(PlannedColumn::Fullscreen),
            SavedColumn::Tabs(tabs) => compact_entities(
                tabs.iter()
                    .filter_map(|saved| self.match_window(saved, current, plan))
                    .collect(),
            ),
            SavedColumn::Stack(items) => compact_stack_items(
                items
                    .iter()
                    .filter_map(|item| self.plan_stack_item(item, current, plan))
                    .collect(),
            ),
        }
    }

    fn plan_stack_item(
        &self,
        item: &SavedStackItem,
        current: &[CurrentWindowIdentity],
        plan: &mut RestorePlan,
    ) -> Option<PlannedStackItem> {
        match item {
            SavedStackItem::Single(saved) => self
                .match_window(saved, current, plan)
                .map(PlannedStackItem::Single),
            SavedStackItem::Tabs(tabs) => compact_stack_tabs(
                tabs.iter()
                    .filter_map(|saved| self.match_window(saved, current, plan))
                    .collect(),
            ),
        }
    }

    fn match_window(
        &self,
        saved: &SavedWindow,
        current: &[CurrentWindowIdentity],
        plan: &mut RestorePlan,
    ) -> Option<Entity> {
        if let Some(window) = current.iter().find(|window| {
            !plan.consumed_entities.contains(&window.entity)
                && saved.hard_match(window.window_id, window.pid, &window.bundle_id)
        }) {
            plan.consumed_entities.insert(window.entity);
            plan.geometry.insert(
                window.entity,
                SavedGeometry {
                    width_ratio: saved.width_ratio,
                },
            );
            return Some(window.entity);
        }

        let fallback_matches = current
            .iter()
            .filter(|window| {
                !plan.consumed_entities.contains(&window.entity)
                    && saved.fallback_match(window)
                    && !self.current_window_has_saved_hard_match(window)
            })
            .collect::<Vec<_>>();

        match fallback_matches.as_slice() {
            [window] => return Some(consume_window_match(saved, window, plan)),
            [] => {}
            _ => {
                plan.skipped_ambiguous_matches += 1;
                return None;
            }
        }

        let compatible_matches = current
            .iter()
            .filter(|window| {
                !plan.consumed_entities.contains(&window.entity)
                    && saved.compatible_fallback_match(window)
                    && !self.current_window_has_saved_hard_match(window)
                    && self.saved_compatible_match_count(window) == 1
            })
            .collect::<Vec<_>>();

        match compatible_matches.as_slice() {
            [window] => Some(consume_window_match(saved, window, plan)),
            [] => {
                plan.ignored_missing_windows += 1;
                None
            }
            _ => {
                plan.skipped_ambiguous_matches += 1;
                None
            }
        }
    }

    fn current_window_has_saved_hard_match(&self, current: &CurrentWindowIdentity) -> bool {
        self.saved_hard_keys.contains(&current.hard_key())
    }

    fn saved_compatible_match_count(&self, current: &CurrentWindowIdentity) -> usize {
        saved_windows_in_state(self.state)
            .filter(|saved| saved.compatible_fallback_match(current))
            .take(2)
            .count()
    }

    fn record_active_virtual(
        workspace: &SavedWorkspace,
        surviving_strips: &[PlannedStrip],
        plan: &mut RestorePlan,
    ) {
        let Some(saved_active) = workspace.active_virtual_index else {
            if let Some(first_survivor) = surviving_strips
                .iter()
                .map(|strip| strip.virtual_index)
                .min()
            {
                plan.active_virtual_by_workspace
                    .insert(workspace.workspace_id, first_survivor);
            }
            return;
        };
        let Some(nearest_active) = surviving_strips
            .iter()
            .map(|strip| strip.virtual_index)
            .min_by_key(|virtual_index| (virtual_index.abs_diff(saved_active), *virtual_index))
        else {
            return;
        };

        plan.active_virtual_by_workspace
            .insert(workspace.workspace_id, nearest_active);
    }
}

fn consume_window_match(
    saved: &SavedWindow,
    window: &CurrentWindowIdentity,
    plan: &mut RestorePlan,
) -> Entity {
    plan.consumed_entities.insert(window.entity);
    plan.geometry.insert(
        window.entity,
        SavedGeometry {
            width_ratio: saved.width_ratio,
        },
    );
    window.entity
}

fn saved_windows_in_state(state: &PaneruState) -> impl Iterator<Item = &SavedWindow> {
    state.windows()
}

fn persisted_window_updates<'a>(
    catalog: &PaneruState,
    live: &'a PaneruState,
) -> HashMap<WindowHardMatchKey, &'a SavedWindow> {
    let saved_windows = saved_windows_in_state(catalog).collect::<Vec<_>>();
    let live_windows = saved_windows_in_state(live).collect::<Vec<_>>();
    let saved_keys = saved_hard_match_keys(catalog);
    let mut updates = HashMap::new();
    for saved in &saved_windows {
        let exact_native = live_windows
            .iter()
            .copied()
            .find(|current| saved.hard_match(current.window_id, current.pid, &current.bundle_id));
        let current = exact_native.or_else(|| {
            // Reserve hard matches before using heuristics. Both sides must
            // be unique, so a returning sibling cannot overwrite a missing
            // window with the same app/identifier but a different title.
            let available = || {
                live_windows
                    .iter()
                    .copied()
                    .filter(|current| !saved_keys.contains(&current.hard_key()))
            };
            let exact_title = |left: &SavedWindow, right: &SavedWindow| {
                !left.title.is_empty()
                    && left.title == right.title
                    && left.compatible_persisted_identity(right)
            };
            if let Some(current) =
                only_saved_window(available().filter(|current| exact_title(saved, current)))
                && saved_windows
                    .iter()
                    .filter(|candidate| exact_title(candidate, current))
                    .count()
                    == 1
            {
                return Some(current);
            }
            let current = only_saved_window(
                available().filter(|current| saved.compatible_persisted_identity(current)),
            )?;
            (saved_windows
                .iter()
                .filter(|candidate| candidate.compatible_persisted_identity(current))
                .count()
                == 1)
                .then_some(current)
        });
        if let Some(current) = current {
            updates.insert(saved.hard_key(), current);
        }
    }
    updates
}

fn only_saved_window<'a>(
    mut windows: impl Iterator<Item = &'a SavedWindow>,
) -> Option<&'a SavedWindow> {
    let first = windows.next()?;
    windows.next().is_none().then_some(first)
}

fn unique_fallback_key(
    state: &PaneruState,
    bundle_id: &str,
    title: &str,
    identifier: &str,
    role: &str,
    subrole: &str,
) -> Option<WindowHardMatchKey> {
    let exact = saved_windows_in_state(state)
        .filter(|saved| {
            !title.is_empty()
                && saved.bundle_id == bundle_id
                && saved.title == title
                && saved.identifier == identifier
                && saved.role == role
                && saved.subrole == subrole
        })
        .map(SavedWindow::hard_key)
        .take(2)
        .collect::<Vec<_>>();
    if let [key] = exact.as_slice() {
        return Some(key.clone());
    }
    if !exact.is_empty() {
        return None;
    }

    let compatible = saved_windows_in_state(state)
        .filter(|saved| {
            saved.bundle_id == bundle_id
                && saved.identifier == identifier
                && saved.role == role
                && saved.subrole == subrole
        })
        .map(SavedWindow::hard_key)
        .take(2)
        .collect::<Vec<_>>();
    match compatible.as_slice() {
        [key] => Some(key.clone()),
        _ => None,
    }
}

fn remove_saved_window(state: &mut PaneruState, key: &WindowHardMatchKey) -> bool {
    let before = saved_windows_in_state(state).count();
    for workspace in &mut state.workspaces {
        for strip in &mut workspace.strips {
            strip
                .columns
                .retain_mut(|column| retain_column_windows(column, key));
        }
        workspace.strips.retain(|strip| !strip.columns.is_empty());
    }
    state
        .workspaces
        .retain(|workspace| !workspace.strips.is_empty());
    saved_windows_in_state(state).count() < before
}

fn retain_column_windows(column: &mut SavedColumn, key: &WindowHardMatchKey) -> bool {
    match column {
        SavedColumn::Single(window) | SavedColumn::Fullscreen(window) => window.hard_key() != *key,
        SavedColumn::Tabs(tabs) => {
            tabs.retain(|window| window.hard_key() != *key);
            !tabs.is_empty()
        }
        SavedColumn::Stack(items) => {
            items.retain_mut(|item| match item {
                SavedStackItem::Single(window) => window.hard_key() != *key,
                SavedStackItem::Tabs(tabs) => {
                    tabs.retain(|window| window.hard_key() != *key);
                    !tabs.is_empty()
                }
            });
            !items.is_empty()
        }
    }
}

fn saved_windows_in_column(column: &SavedColumn) -> Box<dyn Iterator<Item = &SavedWindow> + '_> {
    column.windows()
}

impl SavedWindow {
    fn hard_key(&self) -> WindowHardMatchKey {
        WindowHardMatchKey::new(self.window_id, self.pid, self.bundle_id.clone())
    }

    fn fallback_match(&self, current: &CurrentWindowIdentity) -> bool {
        !self.title.is_empty()
            && !self.role.is_empty()
            && self.bundle_id == current.bundle_id
            && self.title == current.title
            && self.identifier == current.identifier
            && self.role == current.role
            && self.subrole == current.subrole
    }

    fn compatible_fallback_match(&self, current: &CurrentWindowIdentity) -> bool {
        !self.role.is_empty()
            && self.bundle_id == current.bundle_id
            && self.identifier == current.identifier
            && self.role == current.role
            && self.subrole == current.subrole
    }

    fn compatible_persisted_identity(&self, current: &SavedWindow) -> bool {
        !self.role.is_empty()
            && self.bundle_id == current.bundle_id
            && self.identifier == current.identifier
            && self.role == current.role
            && self.subrole == current.subrole
    }
}

fn saved_hard_match_keys(state: &PaneruState) -> HashSet<WindowHardMatchKey> {
    saved_windows_in_state(state)
        .map(SavedWindow::hard_key)
        .collect()
}

fn has_saved_fallback_windows(state: &PaneruState) -> bool {
    saved_windows_in_state(state).any(|window| !window.role.is_empty())
}

pub(crate) fn matches_startup_restore_state(
    window: &Window,
    app: &Application,
    session: Option<&SessionRestore>,
    restoration: Option<&PaneruState>,
    config: &Config,
) -> bool {
    if !config.restore_enabled() {
        return false;
    }

    let Ok(pid) = window.pid() else {
        return false;
    };
    let bundle_id = app.bundle_id().unwrap_or_default().clone();
    let key = WindowHardMatchKey::new(window.id(), pid, bundle_id.clone());

    if let Some(session) = session {
        return session.saved_hard_keys.contains(&key);
    }

    let Some(state) = restoration else {
        return false;
    };
    saved_windows_in_state(state).any(|saved| saved.hard_match(window.id(), pid, &bundle_id))
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    clippy::too_many_lines
)]
#[instrument(level = Level::DEBUG, skip_all, fields(trigger))]
pub(super) fn restore_window_state(
    trigger: On<RestoreWindowState>,
    windows: Windows,
    mut workspaces: Query<(
        Entity,
        &mut LayoutStrip,
        Option<&ChildOf>,
        Has<ActiveWorkspaceMarker>,
    )>,
    displays: Query<(Entity, &Display, Has<ActiveDisplayMarker>)>,
    apps: Query<&Application>,
    config: Res<Config>,
    session: Option<Res<SessionRestore>>,
    restoration: Option<Res<PaneruState>>,
    mut commands: Commands,
) {
    let restoration = if let Some(session) = session.as_deref() {
        &session.state
    } else {
        let Some(restoration) = restoration.as_deref() else {
            return;
        };
        if !config.restore_enabled() {
            info!("Session restore disabled by configuration");
            commands.remove_resource::<PaneruState>();
            return;
        }
        match config.restore_missing_windows() {
            MissingWindowBehavior::Ignore => {}
        }
        commands.insert_resource(SessionRestore::new(
            restoration.clone(),
            config.restore_startup_grace(),
        ));
        commands.remove_resource::<PaneruState>();
        restoration
    };

    let current = current_window_identities(&windows, &apps, restoration, &config);
    let plan = RestorePlanner::new(restoration).plan(&current);

    if let Some(added_window_ids) = trigger.event().added_window_ids()
        && !current.iter().any(|window| {
            added_window_ids.contains(&window.window_id)
                && plan.consumed_entities.contains(&window.entity)
        })
    {
        debug!(
            ?added_window_ids,
            "new windows do not match the durable restore catalog"
        );
        return;
    }

    if plan.consumed_entities.is_empty() {
        info!(
            "Session restore matched 0 windows; missing={}, ambiguous={}",
            plan.ignored_missing_windows, plan.skipped_ambiguous_matches
        );
        return;
    }

    let mut existing_workspace_parents = HashMap::new();
    let mut active_workspace_ids = HashSet::new();
    let mut emptied_existing_strips = HashSet::new();
    for (entity, mut strip, child, active) in &mut workspaces {
        if active {
            active_workspace_ids.insert(strip.id());
        }
        if let Some(child) = child {
            existing_workspace_parents
                .entry(strip.id())
                .or_insert_with(|| child.parent());
        }

        let had_consumed_window = plan
            .consumed_entities
            .iter()
            .any(|entity| strip.contains(*entity));
        for entity in &plan.consumed_entities {
            strip.remove(*entity);
        }

        if had_consumed_window && strip.all_windows().is_empty() {
            emptied_existing_strips.insert(entity);
        }
    }

    for entity in &emptied_existing_strips {
        if let Ok(mut entity_commands) = commands.get_entity(*entity) {
            entity_commands.try_despawn();
        }
    }

    for entity in &plan.consumed_entities {
        if let Ok(mut entity_commands) = commands.get_entity(*entity) {
            // A window consumed into a restored strip must satisfy the same
            // ownership invariant as a freshly managed window. Without this,
            // a stale passthrough disposition can reappear on deminimize/show.
            entity_commands
                .try_insert((WindowDisposition::Managed, RestoredWindowMarker))
                .try_remove::<Unmanaged>();
        }
    }

    let mut restored_strips = 0;
    for planned in &plan.strips {
        let Some((display_entity, display)) = select_display(
            planned.workspace_id,
            planned.display_id,
            &existing_workspace_parents,
            &displays,
        ) else {
            warn!(
                "Skipping restore for workspace {} virtual {} because no display exists",
                planned.workspace_id, planned.virtual_index
            );
            continue;
        };

        let strip = layout_strip_from_plan(planned);
        if strip.all_windows().is_empty() {
            continue;
        }

        let is_active = plan
            .active_virtual_by_workspace
            .get(&planned.workspace_id)
            .is_some_and(|active| *active == planned.virtual_index);
        let is_global_active = is_active && active_workspace_ids.contains(&planned.workspace_id);
        if is_active {
            for (entity, strip, _, _) in &mut workspaces {
                if strip.id() == planned.workspace_id
                    && !emptied_existing_strips.contains(&entity)
                    && is_global_active
                    && let Ok(mut entity_commands) = commands.get_entity(entity)
                {
                    entity_commands.try_remove::<ActiveWorkspaceMarker>();
                }
            }
        }

        let origin = if is_global_active {
            let mut origin = display.bounds().min;
            if let Some(saved_x) = planned.position_x {
                origin.x = saved_x;
            }
            origin
        } else {
            display.bounds().max - 10
        };
        let previous = PreviousStripPosition {
            origin: display.bounds().min,
            focus: strip.all_windows().first().copied(),
        };

        let mut spawned =
            commands.spawn_layout_strip(strip, origin, display_entity, is_global_active);
        spawned.try_insert(RefreshWindowSizes::default());
        if !is_global_active {
            spawned.insert(previous);
        }
        restored_strips += 1;

        for entity in planned.columns.iter().flat_map(planned_column_entities) {
            if let Some(width_ratio) = plan
                .geometry
                .get(&entity)
                .and_then(|geometry| geometry.width_ratio)
                && width_ratio.is_finite()
                && width_ratio > 0.0
                && let Some(size) = windows.size(entity)
            {
                commands.resize_entity(
                    entity,
                    crate::manager::Size::new(
                        (f64::from(display.bounds().width()) * width_ratio).round() as i32,
                        size.y,
                    ),
                );
            }
        }
    }

    info!(
        "Session restore applied: matched={}, strips={}, missing={}, ambiguous={}",
        plan.consumed_entities.len(),
        restored_strips,
        plan.ignored_missing_windows,
        plan.skipped_ambiguous_matches
    );
}

fn planned_column_entities(column: &PlannedColumn) -> Box<dyn Iterator<Item = Entity> + '_> {
    match column {
        PlannedColumn::Single(entity) | PlannedColumn::Fullscreen(entity) => {
            Box::new(std::iter::once(*entity))
        }
        PlannedColumn::Tabs(tabs) => Box::new(tabs.iter().copied()),
        PlannedColumn::Stack(items) => Box::new(items.iter().flat_map(|item| match item {
            PlannedStackItem::Single(entity) => vec![*entity],
            PlannedStackItem::Tabs(tabs) => tabs.clone(),
        })),
    }
}

fn layout_strip_from_plan(planned: &PlannedStrip) -> LayoutStrip {
    if let [PlannedColumn::Fullscreen(entity)] = planned.columns.as_slice() {
        let mut strip = LayoutStrip::fullscreen(planned.workspace_id, *entity);
        strip.virtual_index = planned.virtual_index;
        return strip;
    }

    let mut strip = LayoutStrip::new(planned.workspace_id, planned.virtual_index);
    apply_planned_columns(&mut strip, &planned.columns);
    strip
}

fn current_window_identities(
    windows: &Windows,
    apps: &Query<&Application>,
    restoration: &PaneruState,
    config: &Config,
) -> Vec<CurrentWindowIdentity> {
    let mut current = windows
        .iter()
        .filter_map(|(window, entity)| {
            let (_, _, parent) = windows.find_parent(window.id())?;
            let app = apps.get(parent).ok()?;
            let explicit = WindowProperties::new(app, window, config).explicit_disposition();
            if matches!(
                explicit,
                Some(WindowDisposition::Passthrough | WindowDisposition::Floating)
            ) {
                return None;
            }
            Some(CurrentWindowIdentity {
                entity,
                window_id: window.id(),
                pid: window.pid().ok()?,
                bundle_id: app.bundle_id().unwrap_or_default().clone(),
                title: String::new(),
                identifier: String::new(),
                role: String::new(),
                subrole: String::new(),
            })
        })
        .collect::<Vec<_>>();

    if has_saved_fallback_windows(restoration) {
        let saved_hard_keys = saved_hard_match_keys(restoration);
        hydrate_fallback_identities(&mut current, windows, &saved_hard_keys);
    }

    current
}

fn hydrate_fallback_identities(
    current: &mut [CurrentWindowIdentity],
    windows: &Windows,
    saved_hard_keys: &HashSet<WindowHardMatchKey>,
) {
    for identity in current {
        if saved_hard_keys.contains(&identity.hard_key()) {
            continue;
        }
        let Some(window) = windows.get(identity.entity) else {
            continue;
        };
        identity.title = window.title().unwrap_or_default();
        identity.identifier = window.identifier().unwrap_or_default();
        identity.role = window.role().unwrap_or_default();
        identity.subrole = window.subrole().unwrap_or_default();
    }
}

fn select_display<'a>(
    workspace_id: WorkspaceId,
    planned_display_id: Option<CGDirectDisplayID>,
    existing_workspace_parents: &HashMap<WorkspaceId, Entity>,
    displays: &'a Query<(Entity, &Display, Has<ActiveDisplayMarker>)>,
) -> Option<(Entity, &'a Display)> {
    if let Some(display_entity) = existing_workspace_parents.get(&workspace_id)
        && let Ok((entity, display, _)) = displays.get(*display_entity)
    {
        let current_display_id = display.id();
        if planned_display_id.is_some_and(|display_id| display_id != current_display_id) {
            info!(
                "Session restore remapping workspace {} from saved display {:?} to current display {}",
                workspace_id, planned_display_id, current_display_id
            );
            return Some((entity, display));
        }
    }

    if let Some(display_id) = planned_display_id {
        if let Some((entity, display, _)) = displays
            .iter()
            .find(|(_, display, _)| display.id() == display_id)
        {
            return Some((entity, display));
        }
        info!(
            "Session restore remapping workspace {} from missing display {}",
            workspace_id, display_id
        );
    }

    if let Some(display_entity) = existing_workspace_parents.get(&workspace_id)
        && let Ok((entity, display, _)) = displays.get(*display_entity)
    {
        return Some((entity, display));
    }

    displays
        .iter()
        .find(|(_, _, active)| *active)
        .or_else(|| displays.iter().min_by_key(|(_, display, _)| display.id()))
        .map(|(entity, display, _)| (entity, display))
}

fn apply_planned_columns(strip: &mut LayoutStrip, columns: &[PlannedColumn]) {
    for column in columns {
        match column {
            PlannedColumn::Single(entity) | PlannedColumn::Fullscreen(entity) => {
                strip.append(*entity);
            }
            PlannedColumn::Tabs(entities) => {
                append_tabs(strip, entities);
            }
            PlannedColumn::Stack(items) => {
                append_stack(strip, items);
            }
        }
    }
}

fn append_tabs(strip: &mut LayoutStrip, entities: &[Entity]) -> Option<Entity> {
    let leader = *entities.first()?;
    strip.append(leader);
    for follower in &entities[1..] {
        _ = strip.convert_to_tabs(leader, *follower);
    }
    Some(leader)
}

fn append_stack(strip: &mut LayoutStrip, items: &[PlannedStackItem]) {
    let mut first = true;
    for item in items {
        let Some(leader) = append_stack_item(strip, item) else {
            continue;
        };
        if first {
            first = false;
        } else {
            _ = strip.stack(leader);
        }
    }
}

fn append_stack_item(strip: &mut LayoutStrip, item: &PlannedStackItem) -> Option<Entity> {
    match item {
        PlannedStackItem::Single(entity) => {
            strip.append(*entity);
            Some(*entity)
        }
        PlannedStackItem::Tabs(entities) => append_tabs(strip, entities),
    }
}

fn compact_entities(entities: Vec<Entity>) -> Option<PlannedColumn> {
    match entities.as_slice() {
        [] => None,
        [entity] => Some(PlannedColumn::Single(*entity)),
        _ => Some(PlannedColumn::Tabs(entities)),
    }
}

fn compact_stack_tabs(entities: Vec<Entity>) -> Option<PlannedStackItem> {
    match entities.as_slice() {
        [] => None,
        [entity] => Some(PlannedStackItem::Single(*entity)),
        _ => Some(PlannedStackItem::Tabs(entities)),
    }
}

fn compact_stack_items(items: Vec<PlannedStackItem>) -> Option<PlannedColumn> {
    match items.as_slice() {
        [] => None,
        [PlannedStackItem::Single(entity)] => Some(PlannedColumn::Single(*entity)),
        [PlannedStackItem::Tabs(entities)] => Some(PlannedColumn::Tabs(entities.clone())),
        _ => Some(PlannedColumn::Stack(items)),
    }
}

#[cfg(test)]
mod deadline_tests {
    use std::time::{Duration, Instant};

    use super::{
        SessionRestore, WindowHardMatchKey, remove_saved_window, saved_windows_in_state,
        unique_fallback_key,
    };
    use crate::ecs::state::{PaneruState, SavedColumn, SavedStrip, SavedWindow, SavedWorkspace};
    use crate::platform::ProcessSerialNumber;

    fn saved_window(window_id: i32) -> SavedWindow {
        SavedWindow {
            window_id,
            pid: 100 + window_id,
            psn: ProcessSerialNumber {
                high: 0,
                low: u32::try_from(window_id).unwrap(),
            },
            bundle_id: format!("com.example.app{window_id}"),
            title: format!("Window {window_id}"),
            identifier: "main".to_string(),
            role: "AXWindow".to_string(),
            subrole: "AXStandardWindow".to_string(),
            width_ratio: Some(1.0),
            strip_position_x: Some(-100),
        }
    }

    fn state(timestamp: u64, windows: Vec<SavedWindow>) -> PaneruState {
        PaneruState {
            version: 2,
            timestamp,
            active_display_id: None,
            displays: Vec::new(),
            workspaces: vec![SavedWorkspace {
                workspace_id: 1,
                display_id: None,
                active_virtual_index: Some(0),
                strips: vec![SavedStrip {
                    virtual_index: 0,
                    columns: windows.into_iter().map(SavedColumn::Single).collect(),
                }],
            }],
        }
    }

    #[test]
    fn restore_grace_fires_at_one_absolute_monotonic_deadline() {
        let now = Instant::now();
        let grace = Duration::from_millis(50);
        let mut restore = SessionRestore::new_at(
            PaneruState {
                version: 2,
                timestamp: 0,
                active_display_id: None,
                displays: Vec::new(),
                workspaces: Vec::new(),
            },
            grace,
            now,
        );
        assert_eq!(restore.next_deadline(), Some(now + grace));
        assert!(!restore.expired(now + grace.checked_sub(Duration::from_nanos(1)).unwrap()));
        assert!(restore.expired(now + grace));
        restore.complete_startup_grace();
        assert_eq!(restore.next_deadline(), None);
    }

    #[test]
    fn persistence_keeps_missing_slots_in_durable_catalog() {
        let mut restore = SessionRestore::new_at(
            state(1, vec![saved_window(1), saved_window(2)]),
            Duration::ZERO,
            Instant::now(),
        );

        let persisted = restore.reconcile_persisted_state(state(2, vec![saved_window(1)]));

        assert_eq!(saved_windows_in_state(&persisted).count(), 2);
        assert_eq!(persisted.timestamp, 1);
    }

    #[test]
    fn persistence_adopts_live_layout_once_every_slot_is_present() {
        let mut restore = SessionRestore::new_at(
            state(1, vec![saved_window(1), saved_window(2)]),
            Duration::ZERO,
            Instant::now(),
        );
        let mut first = saved_window(1);
        first.width_ratio = Some(1.5);
        let live = state(2, vec![saved_window(2), first]);

        let persisted = restore.reconcile_persisted_state(live.clone());

        assert_eq!(persisted, live);
    }

    #[test]
    fn persistence_updates_open_window_width_while_neighbor_is_closed() {
        let missing = saved_window(2);
        let mut restore =
            SessionRestore::from_live_snapshot(state(1, vec![saved_window(1), missing.clone()]));
        let mut resized = saved_window(1);
        resized.width_ratio = Some(1.75);
        let persisted = restore.reconcile_persisted_state(state(2, vec![resized.clone()]));
        let windows = persisted.windows().collect::<Vec<_>>();
        assert_eq!(windows, vec![&resized, &missing]);
    }

    #[test]
    fn partial_catalog_keeps_full_layout_pan_while_saving_new_width() {
        let mut restore =
            SessionRestore::from_live_snapshot(state(1, vec![saved_window(1), saved_window(2)]));
        let mut compacted = saved_window(2);
        compacted.width_ratio = Some(1.75);
        compacted.strip_position_x = Some(0);
        let persisted = restore.reconcile_persisted_state(state(2, vec![compacted]));
        let saved = persisted
            .windows()
            .find(|window| window.window_id == 2)
            .unwrap();
        assert_eq!(saved.width_ratio, Some(1.75));
        assert_eq!(saved.strip_position_x, Some(-100));
    }

    #[test]
    fn persistence_updates_relaunched_window_while_neighbor_is_closed() {
        let mut restore =
            SessionRestore::from_live_snapshot(state(1, vec![saved_window(1), saved_window(2)]));
        let mut relaunched = saved_window(1);
        relaunched.window_id = 101;
        relaunched.pid = 201;
        relaunched.title = "Changed title".to_string();
        relaunched.width_ratio = Some(1.75);
        let persisted = restore.reconcile_persisted_state(state(2, vec![relaunched.clone()]));
        assert_eq!(persisted.windows().next(), Some(&relaunched));
        assert!(restore.saved_hard_keys.contains(&relaunched.hard_key()));
        assert!(
            !restore
                .saved_hard_keys
                .contains(&saved_window(1).hard_key())
        );
    }

    #[test]
    fn persistence_matches_same_app_siblings_by_title_before_relaxed_identity() {
        let first = saved_window(1);
        let mut second = saved_window(2);
        second.bundle_id.clone_from(&first.bundle_id);
        let mut restore =
            SessionRestore::from_live_snapshot(state(1, vec![first.clone(), second.clone()]));
        second.window_id = 102;
        second.pid = 202;
        second.width_ratio = Some(1.75);
        let persisted = restore.reconcile_persisted_state(state(2, vec![second.clone()]));
        assert_eq!(
            persisted.windows().collect::<Vec<_>>(),
            vec![&first, &second]
        );
    }

    #[test]
    fn persistence_does_not_guess_between_ambiguous_saved_siblings() {
        let first = saved_window(1);
        let mut second = saved_window(2);
        second.bundle_id.clone_from(&first.bundle_id);
        let before = state(1, vec![first.clone(), second]);
        let mut restore = SessionRestore::from_live_snapshot(before.clone());
        let mut unknown = first;
        unknown.window_id = 100;
        unknown.pid = 200;
        unknown.title = "Changed title".to_string();
        unknown.width_ratio = Some(1.75);
        let persisted = restore.reconcile_persisted_state(state(2, vec![unknown]));
        assert_eq!(persisted, before);
    }

    #[test]
    fn explicit_unmanage_removes_only_the_target_slot() {
        let mut catalog = state(1, vec![saved_window(1), saved_window(2)]);
        let target = WindowHardMatchKey::new(1, 101, "com.example.app1".to_string());

        assert!(remove_saved_window(&mut catalog, &target));

        let remaining = saved_windows_in_state(&catalog).collect::<Vec<_>>();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].window_id, 2);
    }

    #[test]
    fn explicit_unmanage_can_forget_a_relaunched_window_by_unique_fallback() {
        let catalog = state(1, vec![saved_window(1), saved_window(2)]);

        let matched = unique_fallback_key(
            &catalog,
            "com.example.app2",
            "Changed title",
            "main",
            "AXWindow",
            "AXStandardWindow",
        );

        assert_eq!(
            matched,
            Some(WindowHardMatchKey::new(
                2,
                102,
                "com.example.app2".to_string()
            ))
        );
    }
}
