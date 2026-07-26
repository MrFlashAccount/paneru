//! Opt-in counters for verifying display-paced scroll presentation.
//!
//! Production recording is disabled unless `PANERU_FRAME_METRICS` is set to a
//! truthy value. When enabled, storage remains bounded and a cumulative
//! snapshot is logged at most once per second.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use tracing::info;

use crate::platform::WinID;

const MAX_TRACKED_WINDOWS: usize = 256;
const REPORT_INTERVAL: Duration = Duration::from_secs(1);

static ENABLED: LazyLock<bool> = LazyLock::new(|| {
    std::env::var("PANERU_FRAME_METRICS").is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
});
#[cfg(test)]
static TEST_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static PRESENTATION_FRAME_TICKS: AtomicU64 = AtomicU64::new(0);
static DISPLAY_LINK_TICKS: AtomicU64 = AtomicU64::new(0);
static ACTIVE_SCROLL_ECS_UPDATES: AtomicU64 = AtomicU64::new(0);
static SCROLL_INTEGRATION_STEPS: AtomicU64 = AtomicU64::new(0);
static COMMIT_WINDOW_POSITION_EXECUTIONS: AtomicU64 = AtomicU64::new(0);
static DISPLAY_LINK_SAFETY_TIMEOUTS: AtomicU64 = AtomicU64::new(0);
static AX_POSITION_WRITES_OVERFLOW: AtomicU64 = AtomicU64::new(0);
static AX_POSITION_WRITES: LazyLock<Mutex<BTreeMap<WinID, u64>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));
static LAST_REPORT: LazyLock<Mutex<Instant>> = LazyLock::new(|| Mutex::new(Instant::now()));

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct FrameMetricsSnapshot {
    pub(crate) presentation_frame_ticks: u64,
    pub(crate) display_link_ticks: u64,
    pub(crate) active_scroll_ecs_updates: u64,
    pub(crate) scroll_integration_steps: u64,
    pub(crate) commit_window_position_executions: u64,
    pub(crate) ax_position_writes_by_window: BTreeMap<WinID, u64>,
    pub(crate) ax_position_writes_overflow: u64,
    pub(crate) display_link_safety_timeouts: u64,
}

fn enabled() -> bool {
    #[cfg(test)]
    if TEST_ENABLED.load(Ordering::Acquire) {
        return true;
    }
    *ENABLED
}

fn increment(counter: &AtomicU64) {
    if enabled() {
        counter.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn record_presentation_frame(active_scroll: bool) {
    if !enabled() {
        return;
    }
    PRESENTATION_FRAME_TICKS.fetch_add(1, Ordering::Relaxed);
    if active_scroll {
        ACTIVE_SCROLL_ECS_UPDATES.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn record_display_link_tick() {
    increment(&DISPLAY_LINK_TICKS);
}

pub(crate) fn record_scroll_integration_step() {
    increment(&SCROLL_INTEGRATION_STEPS);
}

pub(crate) fn record_commit_window_position_execution() {
    increment(&COMMIT_WINDOW_POSITION_EXECUTIONS);
}

pub(crate) fn record_display_link_safety_timeout() {
    increment(&DISPLAY_LINK_SAFETY_TIMEOUTS);
}

pub(crate) fn record_ax_position_write(window_id: WinID) {
    if !enabled() {
        return;
    }
    let mut writes = AX_POSITION_WRITES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(count) = writes.get_mut(&window_id) {
        *count = count.saturating_add(1);
    } else if writes.len() < MAX_TRACKED_WINDOWS {
        writes.insert(window_id, 1);
    } else {
        AX_POSITION_WRITES_OVERFLOW.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn snapshot() -> FrameMetricsSnapshot {
    let writes = AX_POSITION_WRITES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    FrameMetricsSnapshot {
        presentation_frame_ticks: PRESENTATION_FRAME_TICKS.load(Ordering::Relaxed),
        display_link_ticks: DISPLAY_LINK_TICKS.load(Ordering::Relaxed),
        active_scroll_ecs_updates: ACTIVE_SCROLL_ECS_UPDATES.load(Ordering::Relaxed),
        scroll_integration_steps: SCROLL_INTEGRATION_STEPS.load(Ordering::Relaxed),
        commit_window_position_executions: COMMIT_WINDOW_POSITION_EXECUTIONS
            .load(Ordering::Relaxed),
        ax_position_writes_by_window: writes,
        ax_position_writes_overflow: AX_POSITION_WRITES_OVERFLOW.load(Ordering::Relaxed),
        display_link_safety_timeouts: DISPLAY_LINK_SAFETY_TIMEOUTS.load(Ordering::Relaxed),
    }
}

pub(crate) fn report_if_due() {
    if !enabled() {
        return;
    }
    let now = Instant::now();
    let mut last_report = LAST_REPORT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if now.saturating_duration_since(*last_report) < REPORT_INTERVAL {
        return;
    }
    *last_report = now;
    let metrics = snapshot();
    info!(
        presentation_frame_ticks = metrics.presentation_frame_ticks,
        display_link_ticks = metrics.display_link_ticks,
        active_scroll_ecs_updates = metrics.active_scroll_ecs_updates,
        scroll_integration_steps = metrics.scroll_integration_steps,
        commit_window_position_executions = metrics.commit_window_position_executions,
        ax_position_writes_by_window = ?metrics.ax_position_writes_by_window,
        ax_position_writes_overflow = metrics.ax_position_writes_overflow,
        display_link_safety_timeouts = metrics.display_link_safety_timeouts,
        "display-frame scroll metrics"
    );
}

#[cfg(test)]
pub(crate) fn reset_for_tests() {
    TEST_ENABLED.store(true, Ordering::Release);
    PRESENTATION_FRAME_TICKS.store(0, Ordering::Relaxed);
    DISPLAY_LINK_TICKS.store(0, Ordering::Relaxed);
    ACTIVE_SCROLL_ECS_UPDATES.store(0, Ordering::Relaxed);
    SCROLL_INTEGRATION_STEPS.store(0, Ordering::Relaxed);
    COMMIT_WINDOW_POSITION_EXECUTIONS.store(0, Ordering::Relaxed);
    DISPLAY_LINK_SAFETY_TIMEOUTS.store(0, Ordering::Relaxed);
    AX_POSITION_WRITES_OVERFLOW.store(0, Ordering::Relaxed);
    AX_POSITION_WRITES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
}

#[cfg(test)]
mod tests {
    #[test]
    fn fallback_presentation_is_visible_without_faking_a_display_link_tick() {
        super::reset_for_tests();
        super::record_presentation_frame(true);

        let snapshot = super::snapshot();
        assert_eq!(snapshot.presentation_frame_ticks, 1);
        assert_eq!(snapshot.active_scroll_ecs_updates, 1);
        assert_eq!(snapshot.display_link_ticks, 0);
        assert_eq!(snapshot.display_link_safety_timeouts, 0);
    }

    #[test]
    fn snapshot_exposes_every_acceptance_counter() {
        super::reset_for_tests();
        super::record_presentation_frame(true);
        super::record_display_link_tick();
        super::record_scroll_integration_step();
        super::record_commit_window_position_execution();
        super::record_ax_position_write(17);
        super::record_display_link_safety_timeout();

        let snapshot = super::snapshot();
        assert_eq!(snapshot.presentation_frame_ticks, 1);
        assert_eq!(snapshot.display_link_ticks, 1);
        assert_eq!(snapshot.active_scroll_ecs_updates, 1);
        assert_eq!(snapshot.scroll_integration_steps, 1);
        assert_eq!(snapshot.commit_window_position_executions, 1);
        assert_eq!(snapshot.ax_position_writes_by_window.get(&17), Some(&1));
        assert_eq!(snapshot.display_link_safety_timeouts, 1);
    }
}
