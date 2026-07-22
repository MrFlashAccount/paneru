use super::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct ProbeLease(Arc<AtomicUsize>);

impl Drop for ProbeLease {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn stages_partially_applied_width_growth() {
    let previous = IRect::new(-400, 40, 400, 640);
    let actual = IRect::new(-400, 40, 2416, 640);

    assert_eq!(
        resize_staging_origin(previous, actual, 4112),
        Some(Origin::new(-1696, 40))
    );

    let nearly_complete = IRect::new(-2056, 40, 2016, 640);
    assert_eq!(
        resize_staging_origin(actual, nearly_complete, 4112),
        Some(Origin::new(-2096, 40))
    );
}

#[test]
fn does_not_stage_fixed_size_or_completed_resizes() {
    let fixed = IRect::new(0, 40, 230, 448);
    assert_eq!(resize_staging_origin(fixed, fixed, 4112), None);

    let previous = IRect::new(0, 40, 800, 640);
    let completed = IRect::new(0, 40, 4112, 640);
    assert_eq!(resize_staging_origin(previous, completed, 4112), None);
}

#[test]
fn batch_acquires_once_per_pid_reuses_it_and_restores_each_pid() {
    let acquisitions = AtomicUsize::new(0);
    let restorations = Arc::new(AtomicUsize::new(0));
    let mut batch = HashMap::new();
    for pid in [41, 41, 72] {
        retain_batch_lease(&mut batch, pid, || {
            acquisitions.fetch_add(1, Ordering::Relaxed);
            Some(ProbeLease(Arc::clone(&restorations)))
        });
    }

    assert_eq!(acquisitions.load(Ordering::Relaxed), 2);
    assert_eq!(batch.len(), 2);
    drop(batch);
    assert_eq!(restorations.load(Ordering::Relaxed), 2);
}

#[test]
fn failed_acquisition_is_cached_once_for_the_pid() {
    let acquisitions = AtomicUsize::new(0);
    let mut batch: HashMap<Pid, Option<ProbeLease>> = HashMap::new();
    for _ in 0..2 {
        retain_batch_lease(&mut batch, 41, || {
            acquisitions.fetch_add(1, Ordering::Relaxed);
            None
        });
    }

    assert_eq!(acquisitions.load(Ordering::Relaxed), 1);
    assert!(batch.contains_key(&41));
}

#[test]
fn lease_restores_after_failed_write_and_unwind() {
    let failed_write_restorations = Arc::new(AtomicUsize::new(0));
    let failed_write: Result<()> = {
        let mut batch = HashMap::new();
        retain_batch_lease(&mut batch, 41, || {
            Some(ProbeLease(Arc::clone(&failed_write_restorations)))
        });
        Err(Error::InvalidInput("simulated AX write failure".to_owned()))
    };
    assert!(failed_write.is_err());
    assert_eq!(failed_write_restorations.load(Ordering::Relaxed), 1);

    let unwind_restorations = Arc::new(AtomicUsize::new(0));
    let unwind = std::panic::catch_unwind({
        let unwind_restorations = Arc::clone(&unwind_restorations);
        move || {
            let mut batch = HashMap::new();
            retain_batch_lease(&mut batch, 72, || {
                Some(ProbeLease(Arc::clone(&unwind_restorations)))
            });
            panic!("cancel reposition batch");
        }
    });
    assert!(unwind.is_err());
    assert_eq!(unwind_restorations.load(Ordering::Relaxed), 1);
}

#[test]
fn reposition_batch_cleans_up_on_normal_and_unwind_exit() {
    assert!(!WindowRepositionBatch::active());
    {
        let _batch = WindowRepositionBatch::new();
        assert!(WindowRepositionBatch::active());
    }
    assert!(!WindowRepositionBatch::active());

    let unwind = std::panic::catch_unwind(|| {
        let _batch = WindowRepositionBatch::new();
        panic!("cancel reposition batch");
    });
    assert!(unwind.is_err());
    assert!(!WindowRepositionBatch::active());
}

#[test]
fn ax_position_failure_streak_updates_cache_only_after_success() {
    let origin = Origin::new(200, 300);
    let mut frame = IRect::new(0, 0, 400, 700);
    let mut failure_reported = false;

    complete_ax_position_write(
        9,
        origin,
        &mut frame,
        &mut failure_reported,
        Err(Error::InvalidInput("first failure".to_owned())),
    );
    assert_eq!(frame, IRect::new(0, 0, 400, 700));
    assert!(failure_reported);

    complete_ax_position_write(
        9,
        origin,
        &mut frame,
        &mut failure_reported,
        Err(Error::InvalidInput("repeated failure".to_owned())),
    );
    assert_eq!(frame, IRect::new(0, 0, 400, 700));
    assert!(failure_reported);

    complete_ax_position_write(9, origin, &mut frame, &mut failure_reported, Ok(()));
    assert_eq!(frame, IRect::new(200, 300, 600, 1000));
    assert!(!failure_reported);
}
