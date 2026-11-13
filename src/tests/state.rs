use bevy::prelude::*;

use crate::ecs::layout::LayoutStrip;
use crate::ecs::params::Windows;
use crate::ecs::restore::CurrentWindowIdentity;
use crate::ecs::state::{
    PaneruState, SavedColumn, SavedDisplay, SavedRect, SavedStackItem, SavedStrip, SavedWindow,
    SavedWorkspace,
};
use crate::ecs::{ActiveDisplayMarker, ActiveWorkspaceMarker, Position};
use crate::manager::{Application, Display};
use crate::platform::{Pid, ProcessSerialNumber, WinID};
use crate::tests::{
    TEST_DISPLAY_HEIGHT, TEST_DISPLAY_ID, TEST_DISPLAY_WIDTH, TEST_MENUBAR_HEIGHT,
    TEST_WORKSPACE_ID,
};
use bevy::ecs::hierarchy::ChildOf;
use bevy::ecs::query::Has;
use bevy::ecs::system::SystemState;

type StateExtractionState<'w, 's> = SystemState<(
    Query<
        'w,
        's,
        (
            Option<&'static ChildOf>,
            &'static LayoutStrip,
            Option<&'static Position>,
            Has<ActiveWorkspaceMarker>,
        ),
    >,
    Query<'w, 's, (&'static Display, Entity, Has<ActiveDisplayMarker>)>,
    Windows<'w, 's>,
    Query<'w, 's, &'static Application>,
)>;

#[test]
fn test_state_serialization() {
    let window = SavedWindow {
        window_id: 1,
        pid: 123,
        psn: ProcessSerialNumber { high: 0, low: 1 },
        bundle_id: "com.apple.Finder".to_string(),
        title: "Finder".to_string(),
        identifier: "finder-main".to_string(),
        role: "AXWindow".to_string(),
        subrole: "AXStandardWindow".to_string(),
        width_ratio: None,
        strip_position_x: None,
    };

    let state = PaneruState {
        version: 2,
        timestamp: 123_456_789,
        active_display_id: Some(TEST_DISPLAY_ID),
        displays: vec![SavedDisplay {
            display_id: TEST_DISPLAY_ID,
            bounds: SavedRect {
                min_x: 0,
                min_y: TEST_MENUBAR_HEIGHT,
                max_x: TEST_DISPLAY_WIDTH,
                max_y: TEST_DISPLAY_HEIGHT,
            },
            active: true,
            workspace_ids: vec![TEST_WORKSPACE_ID],
        }],
        workspaces: vec![SavedWorkspace {
            workspace_id: TEST_WORKSPACE_ID,
            display_id: Some(TEST_DISPLAY_ID),
            active_virtual_index: Some(0),
            strips: vec![SavedStrip {
                virtual_index: 0,
                columns: vec![SavedColumn::Single(window)],
            }],
        }],
    };

    let json = serde_json::to_string(&state).expect("Failed to serialize");
    let deserialized: PaneruState = serde_json::from_str(&json).expect("Failed to deserialize");

    assert_eq!(state, deserialized);
}

#[test]
fn saved_window_geometry_is_backward_compatible() {
    let mut value = serde_json::to_value(saved_window(1, 123, "com.example.app", "Main"))
        .expect("saved window should serialize");
    let object = value.as_object_mut().expect("saved window is an object");
    object.remove("width_ratio");
    object.remove("strip_position_x");

    let restored: SavedWindow =
        serde_json::from_value(value).expect("legacy saved window should deserialize");
    assert_eq!(restored.width_ratio, None);
    assert_eq!(restored.strip_position_x, None);
}

#[test]
fn test_state_restoration() {
    let window = SavedWindow {
        window_id: 1,
        pid: 123,
        psn: ProcessSerialNumber { high: 0, low: 1 },
        bundle_id: "com.apple.Finder".to_string(),
        title: "Finder".to_string(),
        identifier: "finder-main".to_string(),
        role: "AXWindow".to_string(),
        subrole: "AXStandardWindow".to_string(),
        width_ratio: None,
        strip_position_x: None,
    };

    let state = PaneruState {
        version: 2,
        timestamp: 123_456_789,
        active_display_id: None,
        displays: Vec::new(),
        workspaces: vec![SavedWorkspace {
            workspace_id: TEST_WORKSPACE_ID,
            display_id: None,
            active_virtual_index: Some(1),
            strips: vec![SavedStrip {
                virtual_index: 1,
                columns: vec![SavedColumn::Single(window)],
            }],
        }],
    };

    let matched = state.find_match(1, 123, "com.apple.Finder");

    assert!(matched.is_some());
    let (ws_id, virt_idx, col_idx, _) = matched.unwrap();
    assert_eq!(ws_id, TEST_WORKSPACE_ID);
    assert_eq!(virt_idx, 1);
    assert_eq!(col_idx, 0);
}

#[test]
fn test_state_extraction() {
    use crate::ecs::state::SavedColumn;
    use crate::tests::harness::TestHarness;

    let mut harness = TestHarness::new().with_windows(1);

    // Initial world run to setup windows
    harness.app.update();

    let world = harness.world();
    let mut system_state: StateExtractionState<'_, '_> = SystemState::new(world);
    let (workspaces, displays, windows, apps) = system_state.get(world);

    let state = PaneruState::extract(&workspaces, &displays, &windows, &apps, None);

    assert_eq!(state.workspaces.len(), 1);
    assert_eq!(state.active_display_id, Some(TEST_DISPLAY_ID));
    assert_eq!(state.displays.len(), 1);
    assert_eq!(state.displays[0].display_id, TEST_DISPLAY_ID);
    assert_eq!(state.displays[0].workspace_ids, vec![TEST_WORKSPACE_ID]);
    assert!(state.displays[0].active);
    assert_eq!(state.workspaces[0].display_id, Some(TEST_DISPLAY_ID));
    assert_eq!(state.workspaces[0].active_virtual_index, Some(0));
    assert_eq!(state.workspaces[0].strips.len(), 1);
    assert_eq!(state.workspaces[0].strips[0].columns.len(), 1);

    if let SavedColumn::Single(ref win) = state.workspaces[0].strips[0].columns[0] {
        assert_eq!(win.window_id, 0); // MockWindow::new(i, ...)
        assert_eq!(win.bundle_id, "test");
    } else {
        panic!("Expected SavedColumn::Single");
    }
}

#[test]
fn test_state_serializes_display_and_active_virtual_workspace() {
    use crate::tests::harness::TestHarness;

    let mut harness = TestHarness::new().with_windows(1);

    harness.app.update();

    let world = harness.world();
    let mut active_display_query =
        world.query_filtered::<Entity, With<crate::ecs::ActiveDisplayMarker>>();
    let display_entity = active_display_query
        .single(world)
        .expect("active display should exist");
    world.spawn((
        LayoutStrip::new(TEST_WORKSPACE_ID, 2),
        ChildOf(display_entity),
    ));

    let mut system_state: StateExtractionState<'_, '_> = SystemState::new(world);
    let (workspaces, displays, windows, apps) = system_state.get(world);

    let state = PaneruState::extract(&workspaces, &displays, &windows, &apps, None);

    assert_eq!(state.version, 2);
    assert_eq!(state.active_display_id, Some(TEST_DISPLAY_ID));
    assert_eq!(
        state.displays,
        vec![SavedDisplay {
            display_id: TEST_DISPLAY_ID,
            bounds: SavedRect {
                min_x: 0,
                min_y: TEST_MENUBAR_HEIGHT,
                max_x: TEST_DISPLAY_WIDTH,
                max_y: TEST_DISPLAY_HEIGHT,
            },
            active: true,
            workspace_ids: vec![TEST_WORKSPACE_ID],
        }]
    );
    assert_eq!(state.workspaces.len(), 1);
    assert_eq!(state.workspaces[0].display_id, Some(TEST_DISPLAY_ID));
    assert_eq!(state.workspaces[0].active_virtual_index, Some(0));
    assert_eq!(state.workspaces[0].strips.len(), 2);

    let json = serde_json::to_value(&state).expect("state should serialize");
    assert_eq!(json["active_display_id"], TEST_DISPLAY_ID);
    assert_eq!(json["displays"][0]["display_id"], TEST_DISPLAY_ID);
    assert_eq!(
        json["displays"][0]["workspace_ids"],
        serde_json::json!([TEST_WORKSPACE_ID])
    );
    assert_eq!(json["workspaces"][0]["display_id"], TEST_DISPLAY_ID);
    assert_eq!(json["workspaces"][0]["active_virtual_index"], 0);
}

pub(super) fn extract_state(
    harness: &mut crate::tests::TestHarness,
    previous: Option<&PaneruState>,
) -> PaneruState {
    let world = harness.world();
    let mut system_state: StateExtractionState<'_, '_> = SystemState::new(world);
    let (workspaces, displays, windows, apps) = system_state.get(world);
    PaneruState::extract(&workspaces, &displays, &windows, &apps, previous)
}

#[test]
fn failed_ax_identity_reads_preserve_relaunch_matching_and_latest_width() {
    use crate::ecs::WidthRatio;
    use crate::ecs::restore::{RestorePlanner, SessionRestore};
    use crate::tests::{MockIdentityAttribute, TestHarness, find_window_entity};

    for attribute in [
        MockIdentityAttribute::Role,
        MockIdentityAttribute::Title,
        MockIdentityAttribute::Identifier,
        MockIdentityAttribute::Subrole,
    ] {
        let mut harness = TestHarness::new().with_windows(1);
        harness.app.update();
        let before = extract_state(&mut harness, None);
        let original = before.windows().next().unwrap().clone();
        harness
            .mock_state
            .update_window(0, |window| window.identity_failure = Some(attribute));
        let entity = find_window_entity(0, harness.world());
        harness.world().get_mut::<WidthRatio>(entity).unwrap().0 = 1.75;

        let live = extract_state(&mut harness, Some(&before));
        let mut catalog = SessionRestore::from_live_snapshot(before);
        let persisted = catalog.reconcile_persisted_state(live);
        let saved = persisted.windows().next().unwrap();
        assert_eq!(saved.title, original.title, "{attribute:?}");
        assert_eq!(saved.identifier, original.identifier, "{attribute:?}");
        assert_eq!(saved.role, original.role, "{attribute:?}");
        assert_eq!(saved.subrole, original.subrole, "{attribute:?}");
        assert_eq!(saved.width_ratio, Some(1.75));

        let current = CurrentWindowIdentity {
            entity,
            window_id: original.window_id + 100,
            pid: original.pid + 100,
            bundle_id: original.bundle_id,
            title: "Title after relaunch".to_owned(),
            identifier: original.identifier,
            role: original.role,
            subrole: original.subrole,
        };
        let plan = RestorePlanner::new(&persisted).plan(&[current]);
        assert!(
            plan.consumed_entities.contains(&entity),
            "{attribute:?} must not break relaunch restore"
        );
        assert_eq!(plan.geometry[&entity].width_ratio, Some(1.75));
    }
}

#[test]
fn successful_empty_identity_values_are_not_treated_as_ax_failures() {
    let mut harness = crate::tests::TestHarness::new().with_windows(1);
    harness.app.update();
    let before = extract_state(&mut harness, None);
    harness.mock_state.update_window(0, |window| {
        window.title.clear();
        window.identifier.clear();
        window.subrole.clear();
    });
    let after = extract_state(&mut harness, Some(&before));
    let saved = after.windows().next().unwrap();
    assert!(saved.title.is_empty());
    assert!(saved.identifier.is_empty());
    assert!(saved.subrole.is_empty());
    assert_eq!(saved.role, "AXWindow");
}

#[test]
fn locked_session_application_proxy_does_not_replace_window_identity() {
    let mut harness = crate::tests::TestHarness::new().with_windows(1);
    harness.app.update();
    let before = extract_state(&mut harness, None);
    harness.mock_state.update_window(0, |window| {
        window.title = "Application".to_string();
        window.role = "AXApplication".to_string();
        window.identifier.clear();
        window.subrole.clear();
    });
    let after = extract_state(&mut harness, Some(&before));
    let saved = after.windows().next().unwrap();
    let original = before.windows().next().unwrap();
    assert_eq!(saved.title, original.title);
    assert_eq!(saved.role, original.role);
    assert_eq!(saved.subrole, original.subrole);
    assert_eq!(saved.identifier, original.identifier);
    assert_eq!(extract_state(&mut harness, None).windows().count(), 0);
}

#[test]
fn snapshot_keeps_explicitly_managed_nonstandard_window_roles() {
    let mut harness = crate::tests::TestHarness::new().with_windows(1);
    harness.app.update();
    harness.mock_state.update_window(0, |window| {
        window.role = "AXSheet".to_string();
    });
    let state = extract_state(&mut harness, None);
    assert_eq!(state.windows().next().unwrap().role, "AXSheet");
}

#[test]
fn unreadable_identity_cannot_borrow_metadata_from_a_different_native_window() {
    let mut harness = crate::tests::TestHarness::new().with_windows(1);
    harness.app.update();
    let mut before = extract_state(&mut harness, None);
    before.windows_mut().next().unwrap().pid += 100;
    harness.mock_state.update_window(0, |window| {
        window.identity_failure = Some(crate::tests::MockIdentityAttribute::Role);
    });
    assert_eq!(
        extract_state(&mut harness, Some(&before)).windows().count(),
        0
    );
    assert_eq!(extract_state(&mut harness, None).windows().count(), 0);
}

#[test]
fn test_state_extraction_includes_parentless_layout_strips() {
    use crate::tests::harness::TestHarness;

    let mut harness = TestHarness::new().with_windows(0);

    harness.app.update();

    let world = harness.world();
    let parentless_workspace_id = TEST_WORKSPACE_ID + 1;
    world.spawn(LayoutStrip::new(parentless_workspace_id, 0));

    let mut system_state: StateExtractionState<'_, '_> = SystemState::new(world);
    let (workspaces, displays, windows, apps) = system_state.get(world);

    let state = PaneruState::extract(&workspaces, &displays, &windows, &apps, None);

    let parentless_workspace = state
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == parentless_workspace_id)
        .expect("parentless layout strip should be saved");
    assert_eq!(parentless_workspace.display_id, None);
    assert_eq!(parentless_workspace.active_virtual_index, None);
    assert_eq!(parentless_workspace.strips.len(), 1);
    assert_eq!(parentless_workspace.strips[0].virtual_index, 0);

    assert!(
        state
            .displays
            .iter()
            .all(|display| !display.workspace_ids.contains(&parentless_workspace_id)),
        "parentless layout strip should not be associated with any display"
    );
}

#[test]
fn test_state_load_rejects_unsupported_version() {
    let path = unique_state_path("unsupported-version");
    std::fs::write(
        &path,
        r#"{"version":1,"timestamp":123456789,"workspaces":[]}"#,
    )
    .expect("state fixture should write");

    assert!(PaneruState::load_from_file(&path).is_none());

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_state_save_is_loadable_from_path() {
    let state = PaneruState {
        version: 2,
        timestamp: 123_456_789,
        active_display_id: Some(TEST_DISPLAY_ID),
        displays: vec![SavedDisplay {
            display_id: TEST_DISPLAY_ID,
            bounds: SavedRect {
                min_x: 0,
                min_y: TEST_MENUBAR_HEIGHT,
                max_x: TEST_DISPLAY_WIDTH,
                max_y: TEST_DISPLAY_HEIGHT,
            },
            active: true,
            workspace_ids: vec![TEST_WORKSPACE_ID],
        }],
        workspaces: vec![SavedWorkspace {
            workspace_id: TEST_WORKSPACE_ID,
            display_id: Some(TEST_DISPLAY_ID),
            active_virtual_index: Some(0),
            strips: Vec::new(),
        }],
    };
    let path = unique_state_path("save-load");

    state
        .save_to_file(&path)
        .expect("state should save to requested path");

    let loaded = PaneruState::load_from_file(&path).expect("state should load from saved path");
    assert_eq!(loaded, state);

    let _ = std::fs::remove_file(path);
}

fn unique_state_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "paneru-{name}-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos()
    ))
}

#[test]
fn restore_plan_compacts_missing_windows_and_preserves_active_virtual_row() {
    use crate::ecs::restore::{PlannedColumn, RestorePlanner};

    let mut world = World::new();
    let present_tab = world.spawn_empty().id();
    let present_stack_tab = world.spawn_empty().id();

    let state = restore_state(vec![SavedWorkspace {
        workspace_id: TEST_WORKSPACE_ID,
        display_id: Some(TEST_DISPLAY_ID),
        active_virtual_index: Some(1),
        strips: vec![
            SavedStrip {
                virtual_index: 0,
                columns: vec![SavedColumn::Single(saved_window(
                    10,
                    110,
                    "com.example.missing",
                    "Missing",
                ))],
            },
            SavedStrip {
                virtual_index: 1,
                columns: vec![
                    SavedColumn::Tabs(vec![
                        saved_window(11, 111, "com.example.missing", "Missing Tab"),
                        saved_window(12, 112, "com.example.editor", "Editor"),
                    ]),
                    SavedColumn::Stack(vec![
                        SavedStackItem::Single(saved_window(
                            13,
                            113,
                            "com.example.missing",
                            "Missing Stack",
                        )),
                        SavedStackItem::Tabs(vec![
                            saved_window(14, 114, "com.example.terminal", "Terminal"),
                            saved_window(15, 115, "com.example.missing", "Missing Stack Tab"),
                        ]),
                    ]),
                ],
            },
        ],
    }]);
    let current = vec![
        current_window(present_tab, 12, 112, "com.example.editor", "Editor"),
        current_window(
            present_stack_tab,
            14,
            114,
            "com.example.terminal",
            "Terminal",
        ),
    ];

    let plan = RestorePlanner::new(&state).plan(&current);

    assert_eq!(plan.strips.len(), 1);
    assert_eq!(plan.strips[0].workspace_id, TEST_WORKSPACE_ID);
    assert_eq!(plan.strips[0].display_id, Some(TEST_DISPLAY_ID));
    assert_eq!(plan.strips[0].virtual_index, 1);
    assert_eq!(
        plan.active_virtual_by_workspace.get(&TEST_WORKSPACE_ID),
        Some(&1)
    );
    assert_eq!(
        plan.strips[0].columns,
        vec![
            PlannedColumn::Single(present_tab),
            PlannedColumn::Single(present_stack_tab),
        ]
    );
    assert_eq!(
        plan.consumed_entities,
        [present_tab, present_stack_tab].into_iter().collect()
    );
    assert_eq!(plan.ignored_missing_windows, 4);
    assert_eq!(plan.skipped_ambiguous_matches, 0);
}

#[test]
fn restore_plan_prefers_later_hard_match_over_earlier_fallback_match() {
    use crate::ecs::restore::{PlannedColumn, RestorePlanner};

    let mut world = World::new();
    let current_x = world.spawn_empty().id();
    let saved_a = saved_window(20, 120, "com.example.notes", "Daily Notes");
    let saved_b = saved_window(21, 121, "com.example.notes", "Daily Notes");
    let state = restore_state(vec![SavedWorkspace {
        workspace_id: TEST_WORKSPACE_ID,
        display_id: Some(TEST_DISPLAY_ID),
        active_virtual_index: Some(0),
        strips: vec![
            SavedStrip {
                virtual_index: 0,
                columns: vec![SavedColumn::Single(saved_a)],
            },
            SavedStrip {
                virtual_index: 1,
                columns: vec![SavedColumn::Single(saved_b)],
            },
        ],
    }]);
    let current = vec![current_window(
        current_x,
        21,
        121,
        "com.example.notes",
        "Daily Notes",
    )];

    let plan = RestorePlanner::new(&state).plan(&current);

    assert_eq!(plan.strips.len(), 1);
    assert_eq!(plan.strips[0].virtual_index, 1);
    assert_eq!(
        plan.strips[0].columns,
        vec![PlannedColumn::Single(current_x)]
    );
    assert_eq!(plan.consumed_entities, [current_x].into_iter().collect());
    assert_eq!(plan.ignored_missing_windows, 1);
    assert_eq!(plan.skipped_ambiguous_matches, 0);
}

#[test]
fn restore_plan_skips_ambiguous_fallback_match() {
    use crate::ecs::restore::{CurrentWindowIdentity, RestorePlanner};

    let mut world = World::new();
    let first = world.spawn_empty().id();
    let second = world.spawn_empty().id();
    let saved = saved_window(20, 120, "com.example.notes", "Daily Notes");
    let state = restore_state(vec![SavedWorkspace {
        workspace_id: TEST_WORKSPACE_ID,
        display_id: None,
        active_virtual_index: Some(0),
        strips: vec![SavedStrip {
            virtual_index: 0,
            columns: vec![SavedColumn::Single(saved)],
        }],
    }]);
    let current = vec![
        CurrentWindowIdentity::fallback_only(first, "com.example.notes", "Daily Notes"),
        CurrentWindowIdentity::fallback_only(second, "com.example.notes", "Daily Notes"),
    ];

    let plan = RestorePlanner::new(&state).plan(&current);

    assert!(plan.strips.is_empty());
    assert!(plan.active_virtual_by_workspace.is_empty());
    assert!(plan.consumed_entities.is_empty());
    assert_eq!(plan.ignored_missing_windows, 0);
    assert_eq!(plan.skipped_ambiguous_matches, 1);
}

#[test]
fn restore_plan_matches_unique_window_after_dynamic_title_changes() {
    use crate::ecs::restore::{CurrentWindowIdentity, PlannedColumn, RestorePlanner};

    let mut world = World::new();
    let current = world.spawn_empty().id();
    let saved = saved_window(20, 120, "com.example.notes", "Yesterday");
    let state = restore_state(vec![SavedWorkspace {
        workspace_id: TEST_WORKSPACE_ID,
        display_id: None,
        active_virtual_index: Some(0),
        strips: vec![SavedStrip {
            virtual_index: 0,
            columns: vec![SavedColumn::Single(saved)],
        }],
    }]);
    let current_windows = vec![CurrentWindowIdentity::fallback_only(
        current,
        "com.example.notes",
        "Today",
    )];

    let plan = RestorePlanner::new(&state).plan(&current_windows);

    assert_eq!(plan.strips.len(), 1);
    assert_eq!(plan.strips[0].columns, vec![PlannedColumn::Single(current)]);
    assert_eq!(plan.consumed_entities, [current].into_iter().collect());
    assert_eq!(plan.ignored_missing_windows, 0);
    assert_eq!(plan.skipped_ambiguous_matches, 0);
}

#[test]
fn restore_plan_does_not_guess_between_saved_windows_with_dynamic_titles() {
    use crate::ecs::restore::{CurrentWindowIdentity, RestorePlanner};

    let mut world = World::new();
    let current = world.spawn_empty().id();
    let state = restore_state(vec![SavedWorkspace {
        workspace_id: TEST_WORKSPACE_ID,
        display_id: None,
        active_virtual_index: Some(0),
        strips: vec![SavedStrip {
            virtual_index: 0,
            columns: vec![
                SavedColumn::Single(saved_window(20, 120, "com.example.notes", "Yesterday")),
                SavedColumn::Single(saved_window(21, 121, "com.example.notes", "Tomorrow")),
            ],
        }],
    }]);
    let current_windows = vec![CurrentWindowIdentity::fallback_only(
        current,
        "com.example.notes",
        "Today",
    )];

    let plan = RestorePlanner::new(&state).plan(&current_windows);

    assert!(plan.consumed_entities.is_empty());
    assert_eq!(plan.strips.len(), 0);
    assert_eq!(plan.ignored_missing_windows, 2);
}

fn restore_state(workspaces: Vec<SavedWorkspace>) -> PaneruState {
    PaneruState {
        version: 2,
        timestamp: 123_456_789,
        active_display_id: Some(TEST_DISPLAY_ID),
        displays: Vec::new(),
        workspaces,
    }
}

fn saved_window(window_id: WinID, pid: Pid, bundle_id: &str, title: &str) -> SavedWindow {
    SavedWindow {
        window_id,
        pid,
        psn: ProcessSerialNumber { high: 0, low: 1 },
        bundle_id: bundle_id.to_string(),
        title: title.to_string(),
        identifier: "main".to_string(),
        role: "AXWindow".to_string(),
        subrole: "AXStandardWindow".to_string(),
        width_ratio: None,
        strip_position_x: None,
    }
}

fn current_window(
    entity: Entity,
    window_id: WinID,
    pid: Pid,
    bundle_id: &str,
    title: &str,
) -> CurrentWindowIdentity {
    CurrentWindowIdentity {
        entity,
        window_id,
        pid,
        bundle_id: bundle_id.to_string(),
        title: title.to_string(),
        identifier: "main".to_string(),
        role: "AXWindow".to_string(),
        subrole: "AXStandardWindow".to_string(),
    }
}
