use super::{GestureInput, Scrolling, begin_touchpad_gesture, integrate_scrolling};
use crate::config::Config;
use crate::events::Event;

#[test]
fn dense_input_keeps_every_delta_in_one_frame_batch() {
    let config = Config::default();
    let scale = 0.01;
    let mut input = GestureInput::default();
    let events = (0..240).map(|index| Event::Scroll {
        delta: f64::from(index % 7) - 3.0,
        is_momentum: false,
    });
    let expected = events.clone().fold(0.0, |sum, event| {
        let Event::Scroll { delta, .. } = event else {
            unreachable!()
        };
        sum + delta * scale
    });

    for (order, event) in events.enumerate() {
        input.ingest(order, &event, &config, scale);
    }

    assert!((input.physical_scroll_delta.expect("physical batch") - expected).abs() < f64::EPSILON);
}

#[test]
fn latest_reverse_contact_discards_old_direction_and_interrupts_snap() {
    let config = Config::default();
    let mut input = GestureInput::default();
    let events = [
        Event::Scroll {
            delta: 12.0,
            is_momentum: false,
        },
        Event::TouchpadDown,
        Event::Scroll {
            delta: -4.0,
            is_momentum: false,
        },
        Event::Scroll {
            delta: -7.0,
            is_momentum: false,
        },
    ];
    for (order, event) in events.iter().enumerate() {
        input.ingest(order, event, &config, 1.0);
    }

    assert_eq!(input.physical_scroll_delta, Some(-11.0));
    let mut scrolling = Scrolling {
        target_position: Some(-1_024.0),
        snap_pending: true,
        ..Default::default()
    };
    begin_touchpad_gesture(true, true, true, None, Some(&mut scrolling));
    assert_eq!(
        scrolling.target_position, None,
        "the new reverse contact must cancel the in-flight snap before integration"
    );
}

#[test]
fn integration_runs_once_per_tick_at_supported_refresh_rates() {
    for refresh_hz in [60_u32, 120, 240] {
        crate::frame_metrics::reset_for_tests();
        let mut scrolling = Scrolling {
            target_position: Some(-1_000.0),
            ..Default::default()
        };
        let dt = 1.0 / f64::from(refresh_hz);

        for _ in 0..refresh_hz {
            integrate_scrolling(&mut scrolling, dt, 1_000.0, 1.0);
        }

        let metrics = crate::frame_metrics::snapshot();
        assert_eq!(
            metrics.scroll_integration_steps,
            u64::from(refresh_hz),
            "{refresh_hz} Hz must produce one integration step per display tick"
        );
        assert!(
            (scrolling.position + 1_000.0).abs() < 1.0,
            "{refresh_hz} Hz failed to converge consistently: {}",
            scrolling.position
        );
    }
}
