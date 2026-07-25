//! Scroll focus selection at a semantic strip offset.

use bevy::ecs::entity::Entity;
use bevy::ecs::system::Commands;
use bevy::math::IRect;
use tracing::debug;

use crate::ecs::Scrolling;
use crate::ecs::focus::{FocusIntentState, FocusWindow};
use crate::ecs::layout::LayoutStrip;
use crate::ecs::params::{GlobalState, Windows};

pub(super) fn target_after_scroll(
    viewport: &IRect,
    strip_offset: i32,
    columns: impl IntoIterator<Item = (Entity, i32, i32)>,
) -> Option<Entity> {
    use std::cmp::Reverse;

    columns
        .into_iter()
        .filter(|(_, _, width)| *width > 0)
        .filter_map(|(entity, layout_x, width)| {
            let left = strip_offset.saturating_add(layout_x);
            let right = left.saturating_add(width);
            let visible_width = right
                .min(viewport.max.x)
                .saturating_sub(left.max(viewport.min.x));
            (visible_width > 0).then_some((
                entity,
                visible_width,
                left.abs_diff(viewport.min.x),
                layout_x,
            ))
        })
        .min_by_key(|(_, visible_width, leading_distance, layout_x)| {
            (Reverse(*visible_width), *leading_distance, *layout_x)
        })
        .map(|(entity, _, _, _)| entity)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn request_for_offset(
    viewport: &IRect,
    strip: &LayoutStrip,
    strip_offset: i32,
    active: bool,
    scroll: &mut Scrolling,
    windows: &Windows<'_, '_>,
    intent: &FocusIntentState,
    global_state: &mut GlobalState<'_>,
    commands: &mut Commands,
) {
    let confirmed = windows.focused().map(|(_, entity)| entity);
    let semantic_focus = intent.effective_entity(confirmed);
    debug!(
        active,
        strip_offset,
        confirmed_focus = ?confirmed,
        pending_focus = ?intent.pending(),
        semantic_focus = ?semantic_focus,
        scroll_focus_origin = ?scroll.scroll_focus_origin,
        "evaluating semantic scroll focus"
    );
    if !active {
        debug!("skipping scroll focus because the strip is inactive");
        return;
    }
    if scroll.scroll_focus_origin != semantic_focus {
        debug!(
            semantic_focus = ?semantic_focus,
            scroll_focus_origin = ?scroll.scroll_focus_origin,
            "skipping scroll focus because gesture ownership is stale"
        );
        return;
    }
    let target = target_after_scroll(
        viewport,
        strip_offset,
        strip.columns().filter_map(|column| {
            let geometry_entity = column.top()?;
            let focus_entity = semantic_focus
                .filter(|entity| column.position_of(*entity).is_some())
                .unwrap_or(geometry_entity);
            let layout_x = windows.layout_position(geometry_entity)?.0.x;
            let width = column.width(&|entity| windows.moving_frame(entity))?;
            Some((focus_entity, layout_x, width))
        }),
    );
    let Some(target) = target else {
        debug!("skipping scroll focus because no visible target was resolved");
        return;
    };
    if Some(target) == semantic_focus {
        debug!(
            ?target,
            "skipping scroll focus because the semantic target is already focused"
        );
        return;
    }
    let Some(window) = windows.get(target) else {
        debug!(
            ?target,
            "skipping scroll focus because the target window disappeared"
        );
        return;
    };

    debug!(
        ?target,
        window_id = window.id(),
        "issuing semantic scroll focus request"
    );
    // Scroll focus never owns layout reshuffling or cursor movement.
    global_state.set_skip_reshuffle(true);
    global_state.set_ffm_flag(Some(window.id()));
    commands.trigger(FocusWindow {
        entity: target,
        raise: true,
        suppress_side_effects: true,
    });
    scroll.scroll_focus_origin = Some(target);
}
