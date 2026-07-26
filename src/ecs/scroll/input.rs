//! Per-frame horizontal gesture aggregation.

use crate::config::Config;
use crate::events::Event;

#[derive(Default)]
pub(super) struct GestureInput {
    pub(super) physical_scroll_delta: Option<f64>,
    pub(super) momentum_scroll_delta: Option<f64>,
    pub(super) gesture_delta: Option<f64>,
    pub(super) touchpad_down: Option<usize>,
    pub(super) touchpad_physical_up: Option<usize>,
    pub(super) touchpad_momentum_start: Option<usize>,
    pub(super) touchpad_up: Option<usize>,
}

impl GestureInput {
    pub(super) fn belongs_to_latest_contact(&self, phase: Option<usize>) -> bool {
        phase.is_some_and(|phase| self.touchpad_down.is_none_or(|down| phase > down))
    }

    pub(super) fn ingest(
        &mut self,
        order: usize,
        event: &Event,
        config: &Config,
        scroll_scale: f64,
    ) {
        match event {
            Event::TouchpadDown => {
                self.touchpad_down = Some(order);
                self.physical_scroll_delta = None;
                self.momentum_scroll_delta = None;
            }
            Event::TouchpadPhysicalUp => self.touchpad_physical_up = Some(order),
            Event::TouchpadMomentumStart => self.touchpad_momentum_start = Some(order),
            Event::TouchpadUp => self.touchpad_up = Some(order),
            Event::Scroll { delta, is_momentum } => {
                let scroll_delta = if *is_momentum {
                    &mut self.momentum_scroll_delta
                } else {
                    &mut self.physical_scroll_delta
                };
                *scroll_delta.get_or_insert(0.0) += *delta * scroll_scale;
            }
            Event::Swipe { delta, fingers }
                if config
                    .swipe_gesture_fingers()
                    .is_some_and(|configured| configured == *fingers) =>
            {
                *self.gesture_delta.get_or_insert(0.0) += *delta;
            }
            _ => {}
        }
    }
}
