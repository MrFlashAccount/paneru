use std::time::Instant;

use bevy::ecs::hierarchy::ChildOf;
use objc2_core_graphics::CGDirectDisplayID;

use super::RuntimeWork;
use crate::ecs::WindowDisposition;

pub(super) fn visit_relevant_frame_displays(
    work: &RuntimeWork<'_, '_>,
    now: Instant,
    mut visit: impl FnMut(CGDirectDisplayID),
) {
    let display_for_entity = |entity| -> Option<bevy::ecs::entity::Entity> {
        work.parents
            .get(entity)
            .ok()
            .map(ChildOf::parent)
            .filter(|parent| work.displays.get(*parent).is_ok())
            .or_else(|| {
                work.layout_strips
                    .iter()
                    .find(|(strip, _)| strip.contains(entity))
                    .map(|(_, parent)| parent.parent())
                    .filter(|parent| work.displays.get(*parent).is_ok())
            })
    };
    for (display_entity, display) in &work.displays {
        let has_repositioning =
            work.repositioning
                .iter()
                .any(|(entity, disposition, unmanaged)| {
                    disposition
                        .copied()
                        .unwrap_or(WindowDisposition::Managed)
                        .owns_geometry(unmanaged)
                        && display_for_entity(entity) == Some(display_entity)
                });
        let has_resizing = work
            .resizing
            .iter()
            .any(|(entity, disposition, unmanaged)| {
                disposition
                    .copied()
                    .unwrap_or(WindowDisposition::Managed)
                    .owns_geometry(unmanaged)
                    && display_for_entity(entity) == Some(display_entity)
            });
        let has_scrolling = work.scrolling.iter().any(|(entity, scrolling)| {
            super::super::scroll::scrolling_needs_frame(scrolling, now)
                && display_for_entity(entity) == Some(display_entity)
        });
        if has_repositioning || has_resizing || has_scrolling {
            visit(display.id());
        }
    }
}
