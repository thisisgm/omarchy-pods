//! Pods that refuse every dial, taken up the held ladder until the daemon parks them.

use std::time::Duration;

use pods_fake::Profile;
use pods_trap::Answer;
use rustix::io::Errno;

use super::HANG_LIMIT;
use super::rig::{Device, HELD_LADDER, Rig, assert_gap};

/// Pods that refuse every dial, the daemon started and BlueZ not yet reporting them connected.
pub(crate) fn refused_pods() -> (Rig, Device) {
    let mut rig = Rig::new();
    let device = rig.add_pods(Profile::default());
    rig.start();
    rig.answer(
        &device,
        Answer::Refuse {
            after: Duration::ZERO,
            errno: Errno::CONNREFUSED,
        },
    );
    (rig, device)
}

/// Waits for the eleven dials of the held ladder, each gap not before its step, then for the failure that parks the pods.
pub(crate) fn climb_to_parked(rig: &Rig) {
    let mut dials = rig.wait_dials(1, HANG_LIMIT);
    for delay in HELD_LADDER {
        let before = dials.last().unwrap().at;
        dials = rig.wait_dials(
            dials.len().saturating_add(1),
            delay.saturating_add(HANG_LIMIT),
        );
        let what = format!("held dial {} after the one before", dials.len());
        assert_gap(&what, before, dials.last().unwrap().at, &(delay..));
    }
    rig.wait_status("one failure after the eleventh dial", |status| {
        status["reconnect_failures_total"] == 1
    });
}
