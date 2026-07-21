use bevy::app::{App, Plugin, Update};
use bevy::ecs::entity::Entity;
use bevy::ecs::hierarchy::ChildOf;
use bevy::ecs::message::MessageReader;
use bevy::ecs::query::{With, Without};
use bevy::ecs::schedule::IntoScheduleConfigs as _;
use bevy::ecs::system::{Commands, Local, Populated, Query, Res, Single};
use bevy::math::IRect;
use bevy::time::Time;
use std::time::{Duration, Instant};
use tracing::{Level, instrument};

use crate::commands::{Command, Direction, Operation};
use crate::config::Config;
use crate::config::swipe::SwipeGestureDirection;
use crate::ecs::layout::{Column, LayoutStrip};
use crate::ecs::params::{ActiveDisplay, Windows};
use crate::ecs::{
    ActiveWorkspaceMarker, DockPosition, MissionControlActive, PagingGesture, Position, Scrolling,
    SendMessageTrigger,
};
use crate::errors::Result;
use crate::events::Event;
use crate::manager::{Display, Window, WindowManager};
use crate::platform::Modifiers;

mod paging;
use paging::{
    capture_gesture as capture_paging_gesture, constrain_motion as constrain_paging_motion,
    ready_to_snap as scrolling_ready_to_snap, snap_target as paging_snap_target,
};

pub struct ScrollEventsPlugin;

const NATIVE_SCROLL_RESPONSE_SECONDS: f64 = 0.04;
const NATIVE_SCROLL_SETTLE_PX: f64 = 0.25;
const SCROLL_VELOCITY_EPSILON: f64 = 0.0001;
const FINGER_LIFT_THRESHOLD: Duration = Duration::from_millis(50);
const STALE_GESTURE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SnapMode {
    Disabled,
    AutoCenter,
    Paging,
    Sticky,
}

fn snap_mode(paging: bool, sticky: bool, auto_center: bool) -> SnapMode {
    if sticky {
        SnapMode::Sticky
    } else if paging {
        SnapMode::Paging
    } else if auto_center {
        SnapMode::AutoCenter
    } else {
        SnapMode::Disabled
    }
}

#[derive(Default)]
struct GestureInput {
    scroll_delta: Option<f64>,
    gesture_delta: Option<f64>,
    lifecycle: u8,
}

const TOUCHPAD_DOWN: u8 = 1 << 0;
const TOUCHPAD_PHYSICAL_UP: u8 = 1 << 1;
const TOUCHPAD_MOMENTUM_START: u8 = 1 << 2;
const TOUCHPAD_UP: u8 = 1 << 3;

impl Plugin for ScrollEventsPlugin {
    fn build(&self, app: &mut App) {
        let mission_control_inactive = |mission_control: Option<Res<MissionControlActive>>| {
            mission_control.is_none_or(|active| !active.0)
        };

        app.add_systems(
            Update,
            (
                cleanup_detached_scrolling,
                vertical_swipe_gesture.run_if(mission_control_inactive),
                (
                    swipe_gesture.run_if(mission_control_inactive),
                    apply_inertia,
                    apply_snap_force,
                    scrolling_integrator,
                    apply_scrolling_constraints,
                    swiping_timeout,
                )
                    .chain()
                    .after(crate::ecs::workspace::show_active_workspace),
            ),
        );
    }
}

#[allow(clippy::needless_pass_by_value)]
fn cleanup_detached_scrolling(
    detached: Query<Entity, (With<Scrolling>, Without<ChildOf>)>,
    mut commands: Commands,
) {
    for entity in detached {
        if let Ok(mut entity_commands) = commands.get_entity(entity) {
            entity_commands.try_remove::<Scrolling>();
        }
    }
}

// This ECS system intentionally keeps event aggregation and component updates
// in one schedule boundary; pure paging math lives in `scroll::paging`.
#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
#[instrument(level = Level::TRACE, skip_all)]
fn swipe_gesture(
    mut messages: MessageReader<Event>,
    active_display: ActiveDisplay,
    mut active_workspace: Single<
        (Entity, &LayoutStrip, &Position, Option<&mut Scrolling>),
        With<ActiveWorkspaceMarker>,
    >,
    windows: Windows,
    time: Res<Time>,
    config: Res<Config>,
    mut commands: Commands,
) {
    let swipe_sensitivity = config.swipe_sensitivity();
    let snap_enabled = config.swipe_paging() || config.sticky_scroll() || config.auto_center();
    // Normalization: Touchpad deltas are typically small fractions.
    // Scroll wheel deltas can be larger. We scale it down slightly
    // to match the "feel" of a finger swipe.
    const SCROLL_SCALE_UPPER: f64 = 0.15;
    const SCROLL_SCALE_LOWER: f64 = 0.005;
    const SCROLL_FULL_RANGE: f64 = 2.0;
    let scroll_scale = SCROLL_SCALE_LOWER
        + ((SCROLL_SCALE_UPPER - SCROLL_SCALE_LOWER) / SCROLL_FULL_RANGE) * swipe_sensitivity;
    let input = read_gesture_input(&mut messages, &config, scroll_scale);
    let GestureInput {
        scroll_delta,
        gesture_delta,
        lifecycle,
    } = input;
    let touchpad_down = lifecycle & TOUCHPAD_DOWN != 0;
    let touchpad_physical_up = lifecycle & TOUCHPAD_PHYSICAL_UP != 0;
    let touchpad_momentum_start = lifecycle & TOUCHPAD_MOMENTUM_START != 0;
    let touchpad_up = lifecycle & TOUCHPAD_UP != 0;
    let has_gesture_event = gesture_delta.is_some();
    let has_scroll_event = scroll_delta.is_some() || has_gesture_event;
    let scroll_delta = scroll_delta.unwrap_or_default();
    let gesture_delta = gesture_delta.unwrap_or_default();

    if lifecycle == 0 && !has_scroll_event {
        return;
    }

    let (entity, layout_strip, position, scrolling) = &mut *active_workspace;
    let has_active_session = scrolling.as_ref().is_some_and(|scrolling| {
        scrolling.gesture_active
            || scrolling.is_user_swiping
            || scrolling.snap_pending
            || scrolling.paging_gesture.is_some()
    });
    let resumes_gesture = has_active_session && (touchpad_down || touchpad_momentum_start);
    let starts_new_gesture = (touchpad_down && !has_active_session)
        || (!has_active_session
            && has_scroll_event
            && !touchpad_momentum_start
            && scrolling
                .as_ref()
                .is_none_or(|scrolling| !scrolling.is_user_swiping));
    let viewport = active_display.actual_bounds(&config);
    let paging_gesture = (config.swipe_paging() && starts_new_gesture)
        .then(|| {
            current_paging_gesture(
                layout_strip,
                position,
                scrolling.as_deref(),
                &windows,
                &viewport,
            )
        })
        .flatten();

    begin_touchpad_gesture(
        starts_new_gesture,
        touchpad_down,
        snap_enabled,
        paging_gesture,
        scrolling.as_deref_mut(),
    );
    // AppKit can report physical Ended and momentum Began together. Apply the
    // physical end first so the momentum phase remains the final state.
    mark_physical_touch_end(touchpad_physical_up, scrolling.as_deref_mut());
    resume_touchpad_gesture(resumes_gesture, touchpad_down, scrolling.as_deref_mut());

    if touchpad_down && !has_scroll_event && scrolling.is_none() {
        insert_touchpad_begin_state(
            *entity,
            position.x,
            snap_enabled,
            paging_gesture,
            &mut commands,
        );
    }

    if has_scroll_event {
        // Preserve the established gesture-distance normalization. Paging
        // anchors themselves use the usable viewport below.
        let viewport_width = f64::from(active_display.bounds().width());
        let direction_modifier = horizontal_direction_modifier(&config);

        let dt = time.delta_secs_f64();
        let new_velocity = if has_gesture_event && dt > 0.0 {
            gesture_delta * swipe_sensitivity / dt
        } else {
            0.0
        };
        let gesture_distance =
            gesture_delta * viewport_width * direction_modifier * swipe_sensitivity;
        let scroll_distance =
            scroll_delta * viewport_width * direction_modifier * swipe_sensitivity;

        if let Some(scrolling) = scrolling.as_mut() {
            let was_user_swiping = scrolling.is_user_swiping;
            // Native modifier-scroll has momentum; synthesize inertia only for raw gestures.
            scrolling.velocity = if has_gesture_event {
                // Smoothen gesture velocity changes using EMA.
                0.3 * new_velocity + 0.7 * scrolling.velocity
            } else {
                0.0
            };
            scrolling.is_user_swiping = true;
            scrolling.snap_pending = snap_enabled;
            scrolling.last_event = Instant::now();

            if has_gesture_event {
                scrolling.target_position = None;
                scrolling.position += gesture_distance;
            }

            if scroll_delta != 0.0 {
                // A new physical gesture interrupts an in-flight sticky snap.
                // Native momentum events keep extending the same target.
                if !was_user_swiping {
                    scrolling.target_position = None;
                }
                let target = scrolling.target_position.unwrap_or(scrolling.position);
                scrolling.target_position = Some(target + scroll_distance);
            }
            constrain_paging_motion(scrolling, direction_modifier);
        } else if let Ok(mut entity_commands) = commands.get_entity(*entity) {
            let initial_position = f64::from(position.0.x) + gesture_distance;
            let mut scrolling = Scrolling {
                velocity: new_velocity,
                position: initial_position,
                target_position: (scroll_delta != 0.0)
                    .then_some(initial_position + scroll_distance),
                snap_pending: snap_enabled,
                is_user_swiping: !touchpad_up && (touchpad_down || has_scroll_event),
                gesture_active: touchpad_down && !touchpad_up,
                paging_gesture,
                last_event: Instant::now(),
            };
            constrain_paging_motion(&mut scrolling, direction_modifier);
            entity_commands.try_insert(scrolling);
        }
    }

    let direction_modifier = horizontal_direction_modifier(&config);
    finish_touchpad_gesture(touchpad_up, direction_modifier, scrolling.as_deref_mut());
}

fn read_gesture_input(
    messages: &mut MessageReader<Event>,
    config: &Config,
    scroll_scale: f64,
) -> GestureInput {
    let mut input = GestureInput::default();
    for event in messages.read() {
        match event {
            Event::TouchpadDown => input.lifecycle |= TOUCHPAD_DOWN,
            Event::TouchpadPhysicalUp => input.lifecycle |= TOUCHPAD_PHYSICAL_UP,
            Event::TouchpadMomentumStart => input.lifecycle |= TOUCHPAD_MOMENTUM_START,
            Event::TouchpadUp => input.lifecycle |= TOUCHPAD_UP,
            Event::Scroll { delta } => {
                *input.scroll_delta.get_or_insert(0.0) += *delta * scroll_scale;
            }
            Event::Swipe { delta, fingers }
                if config
                    .swipe_gesture_fingers()
                    .is_some_and(|configured| configured == *fingers) =>
            {
                *input.gesture_delta.get_or_insert(0.0) += *delta;
            }
            _ => {}
        }
    }
    input
}

fn insert_touchpad_begin_state(
    entity: Entity,
    position: i32,
    snap_enabled: bool,
    paging_gesture: Option<PagingGesture>,
    commands: &mut Commands,
) {
    if let Ok(mut entity_commands) = commands.get_entity(entity) {
        entity_commands.try_insert(Scrolling {
            position: f64::from(position),
            snap_pending: snap_enabled,
            is_user_swiping: true,
            gesture_active: true,
            paging_gesture,
            ..Default::default()
        });
    }
}

fn horizontal_direction_modifier(config: &Config) -> f64 {
    match config.swipe_gesture_direction() {
        SwipeGestureDirection::Natural => -1.0,
        SwipeGestureDirection::Reversed => 1.0,
    }
}

fn current_paging_gesture(
    layout_strip: &LayoutStrip,
    position: &Position,
    scrolling: Option<&Scrolling>,
    windows: &Windows<'_, '_>,
    viewport: &IRect,
) -> Option<PagingGesture> {
    let get_window_frame = |entity| windows.moving_frame(entity);
    let columns = layout_strip.columns().filter_map(|column| {
        let entity = column.top()?;
        Some((
            windows.layout_position(entity)?.0.x,
            column.width(&get_window_frame)?,
        ))
    });
    let current_position = scrolling.map_or(f64::from(position.x), |scrolling| scrolling.position);
    capture_paging_gesture(current_position, viewport, columns)
}

fn begin_touchpad_gesture(
    starts_new_gesture: bool,
    touchpad_down: bool,
    snap_enabled: bool,
    paging_gesture: Option<PagingGesture>,
    scrolling: Option<&mut Scrolling>,
) {
    if starts_new_gesture && let Some(scrolling) = scrolling {
        scrolling.velocity = 0.0;
        scrolling.target_position = None;
        scrolling.snap_pending = snap_enabled;
        scrolling.is_user_swiping = true;
        scrolling.gesture_active = touchpad_down;
        scrolling.paging_gesture = paging_gesture;
        scrolling.last_event = Instant::now();
    }
}

fn resume_touchpad_gesture(
    resumes_gesture: bool,
    interrupts_target: bool,
    scrolling: Option<&mut Scrolling>,
) {
    if resumes_gesture && let Some(scrolling) = scrolling {
        if interrupts_target {
            scrolling.target_position = None;
        }
        scrolling.snap_pending = true;
        scrolling.is_user_swiping = true;
        scrolling.gesture_active = true;
        scrolling.last_event = Instant::now();
    }
}

fn mark_physical_touch_end(physical_up: bool, scrolling: Option<&mut Scrolling>) {
    if physical_up && let Some(scrolling) = scrolling {
        scrolling.gesture_active = false;
        // Keep user-swiping true until either momentum starts or the inactivity
        // fallback proves there will be no native momentum phase.
        scrolling.is_user_swiping = true;
        scrolling.last_event = Instant::now();
    }
}

fn finish_touchpad_gesture(
    touchpad_up: bool,
    direction_modifier: f64,
    scrolling: Option<&mut Scrolling>,
) {
    // Momentum can keep moving afterwards, but sticky selection starts only
    // after both the gesture and any remaining target/velocity have settled.
    if touchpad_up && let Some(scrolling) = scrolling {
        if let Some(paging) = scrolling.paging_gesture.as_mut() {
            paging.release_velocity = scrolling.velocity * direction_modifier;
        }
        scrolling.gesture_active = false;
        scrolling.is_user_swiping = false;
    }
}

pub(super) fn scrolling_needs_frame(scroll: &Scrolling, now: Instant) -> bool {
    // A raw gesture with a lost terminal event may retain its last non-zero
    // velocity. Stop display-frame work after a short input lull while keeping
    // the session alive until the conservative lifecycle watchdog fires.
    if scroll.gesture_active
        && now.saturating_duration_since(scroll.last_event) >= FINGER_LIFT_THRESHOLD
    {
        return false;
    }
    scroll.target_position.is_some()
        || scroll.velocity.abs() > SCROLL_VELOCITY_EPSILON
        || (!scroll.gesture_active && (scroll.is_user_swiping || scroll.snap_pending))
}

pub(super) fn scrolling_deadline(scroll: &Scrolling) -> Option<Instant> {
    if scroll.gesture_active {
        Some(scroll.last_event + STALE_GESTURE_TIMEOUT)
    } else if scroll.is_user_swiping
        && scroll.target_position.is_none()
        && scroll.velocity.abs() <= SCROLL_VELOCITY_EPSILON
    {
        Some(scroll.last_event + FINGER_LIFT_THRESHOLD)
    } else {
        None
    }
}

fn expire_stale_gesture(scroll: &mut Scrolling, now: Instant) -> bool {
    if scroll.gesture_active
        && now.saturating_duration_since(scroll.last_event) >= STALE_GESTURE_TIMEOUT
    {
        scroll.gesture_active = false;
        scroll.is_user_swiping = false;
        scroll.velocity = 0.0;
        scroll.target_position = None;
        scroll.snap_pending = false;
        scroll.paging_gesture = None;
        true
    } else {
        false
    }
}

#[allow(clippy::needless_pass_by_value)]
#[instrument(level = Level::TRACE, skip_all)]
pub(super) fn swiping_timeout(
    strips: Populated<(Entity, &mut Scrolling, &ChildOf), With<LayoutStrip>>,
    displays: Query<(&Display, Option<&DockPosition>)>,
    time: Res<Time>,
    config: Res<Config>,
    window_manager: Res<WindowManager>,
    mut commands: Commands,
) {
    let dt = time.delta_secs_f64();
    let now = Instant::now();

    for (entity, mut scroll, parent) in strips {
        let Ok((display, dock)) = displays.get(parent.parent()) else {
            continue;
        };
        let viewport_width = f64::from(display.actual_display_bounds(dock, &config).width());
        expire_stale_gesture(&mut scroll, now);
        let timed_out = !scroll.gesture_active
            && now.saturating_duration_since(scroll.last_event) >= FINGER_LIFT_THRESHOLD;
        let outcome = update_swipe_timeout(&mut scroll, timed_out, dt, viewport_width);
        if outcome.remove
            && let Ok(mut entity_commands) = commands.get_entity(entity)
        {
            entity_commands.try_remove::<Scrolling>();
        }
        if outcome.should_emit_mouse_moved(config.focus_follows_mouse())
            && let Some(point) = window_manager.cursor_position()
        {
            commands.trigger(SendMessageTrigger(Event::MouseMoved {
                point,
                modifiers: Modifiers::empty(),
            }));
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SwipeTimeoutOutcome {
    emit_mouse_moved: bool,
    remove: bool,
}

impl SwipeTimeoutOutcome {
    fn should_emit_mouse_moved(self, focus_follows_mouse: bool) -> bool {
        self.emit_mouse_moved && focus_follows_mouse
    }
}

fn update_swipe_timeout(
    scroll: &mut Scrolling,
    timed_out: bool,
    dt: f64,
    viewport_width: f64,
) -> SwipeTimeoutOutcome {
    const MIN_VELOCITY_PX: f64 = 5.0;
    let emit_mouse_moved = timed_out && scroll.is_user_swiping;
    if emit_mouse_moved {
        scroll.is_user_swiping = false;
    }
    SwipeTimeoutOutcome {
        emit_mouse_moved,
        remove: timed_out
            && scroll.velocity.abs() * dt * viewport_width < MIN_VELOCITY_PX
            && scroll.target_position.is_none()
            && !scroll.snap_pending,
    }
}

#[allow(clippy::needless_pass_by_value)]
#[instrument(level = Level::TRACE, skip_all)]
fn apply_inertia(
    mut strips: Populated<(Entity, &mut Scrolling), With<LayoutStrip>>,
    time: Res<Time>,
    config: Res<Config>,
) {
    let dt = time.delta_secs_f64();
    for (_, mut scroll) in &mut strips {
        if scroll.is_user_swiping {
            continue;
        }

        if scroll.velocity.abs() > 0.001 {
            let decay_rate = config.swipe_deceleration();
            scroll.velocity *= (-decay_rate * dt).exp();
        } else {
            scroll.velocity = 0.0;
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
#[instrument(level = Level::TRACE, skip_all)]
fn apply_snap_force(
    mut strips: Populated<(&LayoutStrip, &Position, &mut Scrolling, &ChildOf)>,
    displays: Query<(&Display, Option<&DockPosition>)>,
    windows: Windows,
    config: Res<Config>,
) {
    const SNAP_DISPLAY_RATIO: f64 = 0.45;

    let paging = config.swipe_paging();
    let snap_padding = config.snap_padding();
    let mode = snap_mode(paging, config.sticky_scroll(), config.auto_center());
    for (layout_strip, position, mut scroll, parent) in &mut strips {
        if mode == SnapMode::Disabled {
            scroll.snap_pending = false;
            continue;
        }
        let Ok((display, dock)) = displays.get(parent.parent()) else {
            scroll.snap_pending = false;
            continue;
        };
        let viewport = display.actual_display_bounds(dock, &config);
        let snap_threshold = SNAP_DISPLAY_RATIO * f64::from(viewport.width());

        if !scrolling_ready_to_snap(&scroll) {
            continue;
        }

        let get_window_frame = |entity| windows.moving_frame(entity);
        let target_offset = match mode {
            SnapMode::Sticky => {
                let Some(target_offset) = sticky_edge_snap_target(
                    position.x,
                    &viewport,
                    layout_strip.columns().filter_map(|column| {
                        let entity = column.top()?;
                        let column_position = windows.layout_position(entity)?.0.x;
                        let column_width = column.width(&get_window_frame)?;
                        Some((column_position, column_width))
                    }),
                    snap_padding,
                ) else {
                    scroll.snap_pending = false;
                    continue;
                };
                target_offset
            }
            SnapMode::Paging => {
                let Some(paging_gesture) = scroll.paging_gesture else {
                    scroll.snap_pending = false;
                    continue;
                };
                paging_snap_target(
                    scroll.position,
                    f64::from(viewport.width()),
                    paging_gesture,
                    snap_padding,
                ) as i32
            }
            SnapMode::AutoCenter => {
                let viewport_center = viewport.center().x;
                layout_strip
                    .all_columns()
                    .into_iter()
                    .filter_map(|entity| {
                        windows
                            .layout_position(entity)
                            .map(|p| p.0.x)
                            .zip(Some(entity))
                    })
                    .map(|(column_position, entity)| {
                        let column_width = windows.moving_frame(entity).map_or(0, |f| f.width());
                        viewport_center - (column_position + column_width / 2)
                    })
                    .min_by_key(|target| (position.x - target).abs())
                    .unwrap_or(position.x)
            }
            SnapMode::Disabled => unreachable!("disabled snap mode exits before target selection"),
        };

        let dist_to_snap = f64::from(position.x - target_offset);
        scroll.snap_pending = false;
        if matches!(mode, SnapMode::Paging | SnapMode::Sticky)
            || dist_to_snap.abs() < snap_threshold
        {
            // Keep Scrolling alive until the shared target integrator reaches
            // the anchor for native modifier-scroll and raw gestures alike.
            scroll.velocity = 0.0;
            scroll.target_position = Some(f64::from(target_offset));
        }
    }
}

fn sticky_edge_snap_target(
    current_offset: i32,
    viewport: &IRect,
    columns: impl IntoIterator<Item = (i32, i32)>,
    snap_padding: i32,
) -> Option<i32> {
    let current_offset = i64::from(current_offset);
    let threshold = i64::from(snap_padding);

    columns
        .into_iter()
        .flat_map(|(column_position, column_width)| {
            // Keep the sticky zone symmetric around each stop. A gesture that
            // crosses an edge by a few points should return to that edge just
            // like a gesture released a few points before it.
            [
                (viewport.min.x - column_position, -threshold..=threshold),
                (
                    viewport.max.x - (column_position + column_width),
                    -threshold..=threshold,
                ),
            ]
        })
        .filter_map(|(target, hit_zone)| {
            hit_zone
                .contains(&(current_offset - i64::from(target)))
                .then_some(target)
        })
        .min_by_key(|target| (current_offset - i64::from(*target)).abs())
}

#[allow(clippy::needless_pass_by_value)]
#[instrument(level = Level::TRACE, skip_all)]
fn scrolling_integrator(
    mut strips: Populated<(&mut Scrolling, &ChildOf), With<LayoutStrip>>,
    time: Res<Time>,
    displays: Query<(&Display, Option<&DockPosition>)>,
    config: Res<Config>,
) {
    let dt = time.delta_secs_f64();

    // Direction modifier: Natural moves strip left (negative offset) for positive delta (finger left)
    let direction_modifier = horizontal_direction_modifier(&config);

    for (mut scroll, parent) in &mut strips {
        let viewport_width = displays
            .get(parent.parent())
            .map_or(0.0, |(display, dock)| {
                f64::from(display.actual_display_bounds(dock, &config).width())
            });
        integrate_scrolling(&mut scroll, dt, viewport_width, direction_modifier);
    }
}

fn integrate_scrolling(
    scroll: &mut Scrolling,
    dt: f64,
    viewport_width: f64,
    direction_modifier: f64,
) {
    if let Some(target) = scroll.target_position {
        let (position, settled) = smooth_native_scroll(scroll.position, target, dt);
        scroll.position = position;
        if settled {
            scroll.target_position = None;
            if !scroll.snap_pending {
                scroll.paging_gesture = None;
            }
        }
    } else if scroll.velocity.abs() > SCROLL_VELOCITY_EPSILON {
        scroll.position += scroll.velocity * dt * viewport_width * direction_modifier;
        constrain_paging_motion(scroll, direction_modifier);
    }
}

fn smooth_native_scroll(position: f64, target: f64, dt: f64) -> (f64, bool) {
    let blend = 1.0 - (-dt / NATIVE_SCROLL_RESPONSE_SECONDS).exp();
    let position = position + (target - position) * blend;

    if (target - position).abs() <= NATIVE_SCROLL_SETTLE_PX {
        (target, true)
    } else {
        (position, false)
    }
}

/// Preserve the integrator's subpixel remainder unless viewport constraints
/// actually changed the integer position that macOS can apply.
fn reconcile_integrated_position(
    integrated_position: f64,
    effective_position: i32,
    clamped_position: i32,
) -> f64 {
    if effective_position == clamped_position {
        integrated_position
    } else {
        f64::from(clamped_position)
    }
}

fn set_position_x_if_changed(
    position: &mut bevy::ecs::change_detection::Mut<'_, Position>,
    x: i32,
) {
    if position.x != x {
        position.x = x;
    }
}

#[allow(clippy::needless_pass_by_value, clippy::type_complexity)]
#[instrument(level = Level::TRACE, skip_all)]
fn apply_scrolling_constraints(
    mut strips: Populated<(&LayoutStrip, &mut Position, &mut Scrolling, &ChildOf), Without<Window>>,
    displays: Query<(&Display, Option<&DockPosition>)>,
    windows: Windows,
    config: Res<Config>,
) {
    for (strip, mut position, mut scroll, parent) in &mut strips {
        let Ok((display, dock)) = displays.get(parent.parent()) else {
            continue;
        };
        let viewport = display.actual_display_bounds(dock, &config);
        let get_window_frame = |entity| windows.moving_frame(entity);
        let effective_offset = scroll.position as i32;
        if let Some(clamped_offset) = clamp_viewport_offset(
            effective_offset,
            strip,
            &windows,
            &get_window_frame,
            &viewport,
            &config,
        ) {
            set_position_x_if_changed(&mut position, clamped_offset);
            scroll.position =
                reconcile_integrated_position(scroll.position, effective_offset, clamped_offset);
            if let Some(target) = scroll.target_position
                && let effective_target = target as i32
                && let Some(clamped_target) = clamp_viewport_offset(
                    effective_target,
                    strip,
                    &windows,
                    &get_window_frame,
                    &viewport,
                    &config,
                )
            {
                scroll.target_position = Some(reconcile_integrated_position(
                    target,
                    effective_target,
                    clamped_target,
                ));
            }
        } else {
            scroll.velocity = 0.0;
            scroll.target_position = None;
        }
    }
}

#[instrument(level = Level::TRACE, skip_all)]
fn clamp_viewport_offset<W>(
    current_offset: i32,
    layout_strip: &LayoutStrip,
    windows: &Windows,
    get_window_frame: &W,
    viewport: &IRect,
    config: &Config,
) -> Option<i32>
where
    W: Fn(Entity) -> Option<IRect>,
{
    let total_strip_width = layout_strip
        .last()
        .ok()
        .and_then(|column| column.top())
        .and_then(|entity| {
            windows
                .layout_position(entity)
                .zip(get_window_frame(entity))
        })
        .map(|(position, frame)| position.x + frame.width())?;

    if config.swipe_paging() {
        let content_min = layout_strip
            .columns()
            .filter_map(Column::top)
            .filter_map(|entity| windows.layout_position(entity))
            .map(|position| position.0.x)
            .min()?;
        let first_edge = viewport.min.x - content_min;
        let last_edge = viewport.max.x - total_strip_width;
        return Some(current_offset.clamp(first_edge.min(last_edge), first_edge.max(last_edge)));
    }

    let continuous_swipe = config.continuous_swipe();
    let has_oversized_column = layout_strip.columns().any(|column| {
        column
            .width(get_window_frame)
            .is_some_and(|width| width > viewport.width())
    });
    let strip_position = |column: Result<Column>| {
        column
            .ok()
            .and_then(|column| column.top())
            .and_then(|entity| windows.layout_position(entity))
            .map(|position| position.0.x)
    };

    let left_snap = strip_position(layout_strip.last());
    let right_snap = strip_position(layout_strip.get(1));

    let (first_edge, last_edge) = if continuous_swipe
        && !has_oversized_column
        && let Some((left_snap, right_snap)) = left_snap.zip(right_snap)
    {
        // Allow scrolling until the last or first window reaches the viewport
        // edge exactly. Sticky's 32pt value is only an activation threshold.
        (viewport.min.x - left_snap, viewport.max.x - right_snap)
    } else {
        // Pan between the leading and trailing strip edges. The min/max form
        // also handles strips narrower than the viewport without an inverted
        // clamp range.
        (viewport.min.x, viewport.max.x - total_strip_width)
    };

    Some(current_offset.clamp(first_edge.min(last_edge), first_edge.max(last_edge)))
}

#[derive(Default)]
struct VerticalGestureState {
    accumulated: f64,
    last_event: Option<Instant>,
    fired: bool,
}

#[allow(clippy::needless_pass_by_value)]
#[instrument(level = Level::TRACE, skip_all)]
fn vertical_swipe_gesture(
    mut messages: MessageReader<Event>,
    active_display: ActiveDisplay,
    config: Res<Config>,
    mut commands: Commands,
    mut state: Local<VerticalGestureState>,
) {
    const GESTURE_TIMEOUT: Duration = Duration::from_millis(150);

    if active_display.fullscreen().is_some() {
        return;
    }

    // Reset state when the gesture times out (fingers lifted).
    if let Some(last) = state.last_event
        && last.elapsed() > GESTURE_TIMEOUT
    {
        state.accumulated = 0.0;
        state.fired = false;
    }

    for event in messages.read() {
        match event {
            Event::VerticalScrollTick { delta } => {
                switch_virtual_workspace(*delta, &config, &mut commands);
            }
            Event::VerticalSwipe { delta, fingers }
                if config
                    .swipe_gesture_fingers()
                    .is_some_and(|fingers_configured| fingers_configured == *fingers) =>
            {
                state.last_event = Some(Instant::now());

                if !state.fired {
                    state.accumulated += delta;
                }
            }
            _ => {}
        }
    }

    // Threshold needs to be high enough that incidental vertical movement
    // during horizontal swipes doesn't trigger a workspace switch.
    let threshold = 0.15 / config.swipe_sensitivity();
    if state.accumulated.abs() >= threshold {
        switch_virtual_workspace(state.accumulated, &config, &mut commands);
        state.accumulated = 0.0;
        state.fired = true;
    }
}

fn switch_virtual_workspace(delta: f64, config: &Config, commands: &mut Commands) {
    let physical_finger_direction = if delta > 0.0 {
        Direction::South
    } else {
        Direction::North
    };
    let direction = match config.swipe_gesture_direction() {
        SwipeGestureDirection::Natural => physical_finger_direction.reverse(),
        SwipeGestureDirection::Reversed => physical_finger_direction,
    };
    commands.trigger(SendMessageTrigger(Event::Command {
        command: Command::Window(Operation::Virtual(direction)),
    }));
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod settlement_tests;

#[cfg(test)]
mod performance_tests {
    use std::time::Instant;

    use bevy::prelude::{App, ChildOf, Update};

    use super::{
        STALE_GESTURE_TIMEOUT, Scrolling, cleanup_detached_scrolling, expire_stale_gesture,
        integrate_scrolling, reconcile_integrated_position, scrolling_needs_frame,
        set_position_x_if_changed, update_swipe_timeout,
    };
    use crate::ecs::Position;
    use crate::manager::Origin;

    #[test]
    fn detached_strip_cancels_scrolling_on_next_ecs_update() {
        let mut app = App::new();
        app.add_systems(Update, cleanup_detached_scrolling);
        let parent = app.world_mut().spawn_empty().id();
        let strip = app
            .world_mut()
            .spawn((Scrolling::default(), ChildOf(parent)))
            .id();
        app.world_mut().entity_mut(strip).remove::<ChildOf>();

        app.update();

        assert!(!app.world().entity(strip).contains::<Scrolling>());
    }

    #[test]
    fn stationary_contact_is_passive_until_the_stale_watchdog_expires() {
        let last_event = Instant::now();
        let mut scrolling = Scrolling {
            is_user_swiping: true,
            gesture_active: true,
            snap_pending: true,
            last_event,
            ..Default::default()
        };

        assert!(!scrolling_needs_frame(&scrolling, last_event));
        scrolling.velocity = 2.0;
        assert!(scrolling_needs_frame(
            &scrolling,
            last_event + std::time::Duration::from_millis(49)
        ));
        assert!(!scrolling_needs_frame(
            &scrolling,
            last_event + std::time::Duration::from_millis(50)
        ));
        assert!(!expire_stale_gesture(
            &mut scrolling,
            last_event + STALE_GESTURE_TIMEOUT.saturating_sub(std::time::Duration::from_millis(1))
        ));
        assert!(scrolling.gesture_active);

        assert!(expire_stale_gesture(
            &mut scrolling,
            last_event + STALE_GESTURE_TIMEOUT
        ));
        assert!(!scrolling.gesture_active);
        assert!(!scrolling.is_user_swiping);
        assert!(scrolling.velocity.abs() < f64::EPSILON);
        assert!(scrolling.target_position.is_none());
        assert!(!scrolling.snap_pending);
        assert!(scrolling.paging_gesture.is_none());
        assert!(!scrolling_needs_frame(
            &scrolling,
            last_event + STALE_GESTURE_TIMEOUT
        ));
    }

    #[test]
    fn unchanged_integer_scroll_position_does_not_trigger_change_detection() {
        use bevy::ecs::change_detection::DetectChanges as _;

        let mut world = bevy::prelude::World::new();
        let strip = world.spawn(Position(Origin::new(42, 0))).id();
        world.clear_trackers();

        {
            let mut entity = world.entity_mut(strip);
            let mut position = entity.get_mut::<Position>().expect("strip position");
            set_position_x_if_changed(&mut position, 42);
        }

        let position = world
            .entity(strip)
            .get_ref::<Position>()
            .expect("strip position");
        assert!(
            !position.is_changed(),
            "an idle scrolling component must not keep persistence dirty"
        );
    }

    #[test]
    fn two_scrolling_strips_integrate_independently() {
        let now = Instant::now();
        let mut first = Scrolling {
            velocity: 1.0,
            position: 10.0,
            last_event: now,
            ..Scrolling::default()
        };
        let mut second = Scrolling {
            velocity: -2.0,
            position: -30.0,
            last_event: now,
            ..Scrolling::default()
        };
        integrate_scrolling(&mut first, 0.1, 1000.0, 1.0);
        integrate_scrolling(&mut second, 0.1, 500.0, 1.0);
        assert!((first.position - 110.0).abs() < f64::EPSILON);
        assert!((second.position + 130.0).abs() < f64::EPSILON);
    }

    #[test]
    fn timeout_uses_each_parent_display_width_and_emits_lift_once() {
        let now = Instant::now();
        let mut narrow = Scrolling {
            velocity: 1.0,
            is_user_swiping: true,
            last_event: now,
            ..Scrolling::default()
        };
        let mut wide = Scrolling {
            velocity: 1.0,
            is_user_swiping: true,
            last_event: now,
            ..Scrolling::default()
        };
        let narrow_result = update_swipe_timeout(&mut narrow, true, 0.016, 100.0);
        let wide_result = update_swipe_timeout(&mut wide, true, 0.016, 1000.0);
        assert!(narrow_result.remove);
        assert!(!wide_result.remove);
        assert!(narrow_result.emit_mouse_moved);
        assert!(wide_result.emit_mouse_moved);
        assert!(!narrow_result.should_emit_mouse_moved(false));
        assert!(narrow_result.should_emit_mouse_moved(true));
        assert!(
            !update_swipe_timeout(&mut narrow, true, 0.016, 100.0).emit_mouse_moved,
            "synthetic mouse move is emitted only on the swiping transition"
        );
    }

    #[test]
    fn clamp_reconciliation_preserves_remainder_only_inside_boundary() {
        assert!((reconcile_integrated_position(-0.75, 0, 0) + 0.75).abs() < f64::EPSILON);
        assert!((reconcile_integrated_position(2.25, 2, 1) - 1.0).abs() < f64::EPSILON);
    }
}
