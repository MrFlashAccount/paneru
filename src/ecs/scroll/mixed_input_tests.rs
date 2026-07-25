use bevy::ecs::query::With;

use super::Scrolling;
use crate::ecs::ActiveWorkspaceMarker;
use crate::events::Event;
use crate::tests::TestHarness;

#[test]
fn mixed_fresh_physical_and_orphan_momentum_keeps_only_physical_delta() {
    let mut harness = TestHarness::new().with_windows(2).with_focused_window(0);
    harness.run(vec![Event::MenuOpened { window_id: 0 }]);

    harness.world().write_message(Event::TouchpadDown);
    harness.world().write_message(Event::Scroll {
        delta: 100.0,
        is_momentum: false,
    });
    harness.world().write_message(Event::TouchpadMomentumStart);
    harness.world().write_message(Event::Scroll {
        delta: -10_000.0,
        is_momentum: true,
    });
    harness.app.update();

    let scrolling = harness
        .world()
        .query_filtered::<&Scrolling, With<ActiveWorkspaceMarker>>()
        .single(harness.world())
        .expect("fresh physical delta must create scrolling state");
    let visual = super::overscroll::visual_offset(scrolling.edge_overscroll.visual());
    assert!(
        visual < 0,
        "physical distance must survive while the opposite orphan momentum delta is excluded"
    );
}
