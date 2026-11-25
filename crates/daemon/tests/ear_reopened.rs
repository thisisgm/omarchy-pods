//! Ear detection through the built daemon as a closed channel reopens: the pods report a bud out before the audio server answers for their sink.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "each suite uses part of the shared support, so the rest goes unused here"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use pods_core::policy::BOTH_OUT_SETTLE_MS;
    use pods_samples::Placed::{self, InEar, OutOfEar};

    use super::support::HANG_LIMIT;
    use super::support::audio::PERCENT;
    use super::support::players::{Asked, Playback};
    use super::support::rig::{REAL_TIME_SLACK, Rig, assert_gap, not_before};
    use super::support::settings::set;

    const SETTLE: Duration = Duration::from_millis(BOTH_OUT_SETTLE_MS);

    /// The channel closes with both buds worn and reopens with them as placed while the audio server holds every answer; the sink is read `unread` after the report, and the pause comes one settle after that read.
    fn pauses_once_the_sink_is_read(prefers: &str, right: Placed, left: Placed, unread: Duration) {
        let (rig, device, audio) = Rig::with_audio(PERCENT);
        rig.wait_sink_told();
        set(rig.session(), "ear_detection", prefers);
        let player = rig.player("one", Playback::Playing);
        rig.close_channel(&device);
        audio.hold_replies(true);
        device.pods.set_ear(right, left);
        rig.accept(&device);
        rig.wait_filled();
        rig.wait_status("the buds as placed", |status| {
            status["right"]["in_ear"] == (right == InEar)
                && status["left"]["in_ear"] == (left == InEar)
        });
        player.none_for(unread);
        assert_eq!(player.asked(), []);
        let read = Instant::now();
        audio.hold_replies(false);
        player.wait_asked(Asked::Pause, SETTLE.saturating_add(HANG_LIMIT));
        let paused = Instant::now();
        assert_gap(
            "the pause after the sink was read",
            read,
            paused,
            &not_before(SETTLE),
        );
        assert_eq!(player.asked(), [Asked::Pause]);
        assert_eq!(player.playback(), Playback::Paused);
        player.leave();
        rig.finish();
    }

    #[test]
    fn a_bud_out_before_the_sink_is_read_still_pauses() {
        pauses_once_the_sink_is_read("one", InEar, OutOfEar, REAL_TIME_SLACK);
    }

    #[test]
    fn both_buds_out_for_a_whole_settle_before_the_sink_is_read_still_pause() {
        // The settle starts before the status shows the buds out, so it ends inside this wait with the sink still unread.
        pauses_once_the_sink_is_read("both", OutOfEar, OutOfEar, SETTLE);
    }
}
