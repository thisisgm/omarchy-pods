//! Ear detection through the built daemon as the link leaves up: a closed channel, where the wear state comes from the pods' adverts, and a loss before a relink.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "each suite uses part of the shared support, so the rest goes unused here"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pods_core::policy::BOTH_OUT_SETTLE_MS;
    use pods_fake::Profile;
    use pods_samples::Placed::{self, InEar, OutOfEar};
    use pods_samples::advert::HEADER_LEN;

    use super::support::HANG_LIMIT;
    use super::support::advert::{HEADER, PAYLOAD, keyed, ours};
    use super::support::audio::PERCENT;
    use super::support::audio::wire::sink_named;
    use super::support::players::{Asked, Playback};
    use super::support::rig::{Device, REAL_TIME_SLACK, Rig, pause_ended};
    use super::support::settings::set;

    const SETTLE: Duration = Duration::from_millis(BOTH_OUT_SETTLE_MS);

    /// `HEADER` with both buds in an ear: status `0x2a`, the left bud primary and out of the case.
    const BOTH_IN_HEADER: [u8; HEADER_LEN] = [
        0x07, 0x19, 0x01, 0x27, 0x20, 0x2a, 0x88, 0x47, 0x00, 0x00, 0x04,
    ];

    /// `HEADER` with neither bud in an ear: status `0x20`, the left bud primary and out of the case.
    const BOTH_OUT_HEADER: [u8; HEADER_LEN] = [
        0x07, 0x19, 0x01, 0x27, 0x20, 0x20, 0x88, 0x47, 0x00, 0x00, 0x04,
    ];

    /// The sample keys stored, the fake audio server holding the pods' sink as its default, then the pods linked and the advert monitor dropped.
    fn linked_with_keys() -> (Rig, Device) {
        let mut rig = keyed();
        let audio = rig.start_audio();
        let device = rig.add_pods(Profile::default());
        rig.session().store_sample_keys(Some(device.address));
        let sink = sink_named(device.address);
        audio.add_sink(&sink, PERCENT);
        audio.set_default_sink(&sink);
        rig.start();
        rig.link_up(&device);
        rig.wait_filled();
        rig.wait_sink_told();
        rig.bluez().wait_monitors(0, HANG_LIMIT);
        (rig, device)
    }

    fn wait_worn(rig: &Rig, worn: usize) {
        rig.wait_status(&format!("{worn} buds in an ear"), |status| {
            ["right", "left"]
                .iter()
                .filter(|&&bud| status[bud]["in_ear"] == true)
                .count()
                == worn
        });
    }

    /// Closes the channel and waits for the advert monitor, which is wanted again once the link has left up.
    fn close_and_listen(rig: &Rig, device: &Device) {
        rig.close_channel(device);
        rig.bluez().wait_monitors(1, HANG_LIMIT);
    }

    /// Hangs up and waits for the advert monitor alone, a step the close itself takes, so no status poll delays what follows.
    fn hang_up_and_listen(rig: &Rig, device: &Device) {
        rig.hang_up_channel(device);
        rig.bluez().wait_monitors(1, HANG_LIMIT);
    }

    #[test]
    fn a_bud_out_in_an_advert_while_the_channel_is_closed_pauses_and_both_back_resume() {
        let (rig, device) = linked_with_keys();
        let player = rig.player("one", Playback::Playing);
        device.pods.set_ear(InEar, InEar);
        wait_worn(&rig, 2);
        close_and_listen(&rig, &device);
        let one_out = ours(HEADER, PAYLOAD);
        let advertiser = rig.bluez().advertise(&one_out.address, &one_out.data);
        wait_worn(&rig, 1);
        player.wait_asked(Asked::Pause, HANG_LIMIT);
        assert_eq!(player.playback(), Playback::Paused);
        advertiser.update(&ours(BOTH_IN_HEADER, PAYLOAD).data);
        wait_worn(&rig, 2);
        player.wait_asked(Asked::Play, HANG_LIMIT);
        assert_eq!(player.asked(), [Asked::Pause, Asked::Play]);
        player.leave();
        rig.finish();
    }

    #[test]
    fn both_back_in_an_advert_while_the_channel_is_closed_resume_what_was_paused_before() {
        let (rig, device) = linked_with_keys();
        let player = rig.player("one", Playback::Playing);
        device.pods.set_ear(InEar, OutOfEar);
        player.wait_asked(Asked::Pause, HANG_LIMIT);
        device.pods.set_ear(OutOfEar, OutOfEar);
        // The adapter serves one request at a time, so this second read follows the first pause's report to the driver.
        player.wait_reads(2, HANG_LIMIT);
        close_and_listen(&rig, &device);
        let both_in = ours(BOTH_IN_HEADER, PAYLOAD);
        let _advertiser = rig.bluez().advertise(&both_in.address, &both_in.data);
        player.wait_asked(Asked::Play, HANG_LIMIT);
        assert_eq!(player.asked(), [Asked::Pause, Asked::Play]);
        player.leave();
        rig.finish();
    }

    #[test]
    fn after_a_loss_and_a_relink_a_bud_out_pauses_and_both_back_resume() {
        let (rig, device) = linked_with_keys();
        let player = rig.player("one", Playback::Playing);
        rig.disconnect();
        rig.relink(&device);
        rig.wait_sink_told();
        device.pods.set_ear(InEar, InEar);
        wait_worn(&rig, 2);
        device.pods.set_ear(InEar, OutOfEar);
        player.wait_asked(Asked::Pause, HANG_LIMIT);
        device.pods.set_ear(InEar, InEar);
        player.wait_asked(Asked::Play, HANG_LIMIT);
        assert_eq!(player.asked(), [Asked::Pause, Asked::Play]);
        player.leave();
        rig.finish();
    }

    /// After a loss the adverts show both buds in, then as `out` has them; the pods link again placed the same, and no pause comes once their sink is read.
    fn out_in_an_advert_after_a_loss_pauses_nothing(
        prefers: &str,
        out: [u8; HEADER_LEN],
        primary: Placed,
    ) {
        let (rig, device) = linked_with_keys();
        set(rig.session(), "ear_detection", prefers);
        let player = rig.player("one", Playback::Playing);
        rig.disconnect();
        rig.bluez().wait_monitors(1, HANG_LIMIT);
        let both_in = ours(BOTH_IN_HEADER, PAYLOAD);
        let advertiser = rig.bluez().advertise(&both_in.address, &both_in.data);
        wait_worn(&rig, 2);
        advertiser.update(&ours(out, PAYLOAD).data);
        wait_worn(&rig, usize::from(primary == InEar));
        device.pods.set_ear(primary, OutOfEar);
        rig.relink(&device);
        rig.wait_sink_told();
        // A pause left waiting for the link or for this sink read would come within one settle of the read.
        player.none_for(SETTLE.saturating_add(REAL_TIME_SLACK));
        assert_eq!(player.asked(), []);
        assert_eq!(player.playback(), Playback::Playing);
        player.leave();
        rig.finish();
    }

    #[test]
    fn a_bud_out_in_an_advert_after_a_loss_pauses_nothing_once_relinked() {
        out_in_an_advert_after_a_loss_pauses_nothing("one", HEADER, InEar);
    }

    #[test]
    fn both_buds_out_in_an_advert_after_a_loss_pause_nothing_once_relinked() {
        out_in_an_advert_after_a_loss_pauses_nothing("both", BOTH_OUT_HEADER, OutOfEar);
    }

    #[test]
    fn a_pause_answered_after_the_channel_closes_is_remembered_and_both_back_resume() {
        let (mut rig, device) = linked_with_keys();
        let player = rig.player("one", Playback::Playing);
        device.pods.set_ear(InEar, InEar);
        wait_worn(&rig, 2);
        player.hold_asked(true);
        device.pods.set_ear(InEar, OutOfEar);
        player.wait_asked(Asked::Pause, HANG_LIMIT);
        hang_up_and_listen(&rig, &device);
        // Still in the daemon's wait for the pause's answer, which would report the player either way once it ends.
        rig.assert_not_logged("the close while the pause is unanswered", &pause_ended(1));
        player.hold_asked(false);
        let both_in = ours(BOTH_IN_HEADER, PAYLOAD);
        let _advertiser = rig.bluez().advertise(&both_in.address, &both_in.data);
        player.wait_asked(Asked::Play, HANG_LIMIT);
        assert_eq!(player.asked(), [Asked::Pause, Asked::Play]);
        player.leave();
        rig.finish();
    }

    #[test]
    fn a_close_before_the_second_player_is_asked_keeps_the_first_and_spares_the_second() {
        let (mut rig, device) = linked_with_keys();
        let first = rig.player("first", Playback::Playing);
        let second = rig.player("second", Playback::Playing);
        device.pods.set_ear(InEar, InEar);
        wait_worn(&rig, 2);
        second.hold_replies(true);
        device.pods.set_ear(InEar, OutOfEar);
        first.wait_asked(Asked::Pause, HANG_LIMIT);
        second.wait_reads(1, HANG_LIMIT);
        hang_up_and_listen(&rig, &device);
        // Answered `Playing` inside the daemon's wait, so only the close keeps the pause from the second player.
        rig.assert_not_logged(
            "the close while the status read is pending",
            &pause_ended(1),
        );
        second.hold_replies(false);
        let both_in = ours(BOTH_IN_HEADER, PAYLOAD);
        let _advertiser = rig.bluez().advertise(&both_in.address, &both_in.data);
        // The adapter serves one request at a time, so this resume follows the end of the pause work.
        first.wait_asked(Asked::Play, HANG_LIMIT);
        assert_eq!(first.asked(), [Asked::Pause, Asked::Play]);
        assert_eq!(second.asked(), []);
        assert_eq!(second.playback(), Playback::Playing);
        first.leave();
        second.leave();
        rig.finish();
    }

    #[test]
    fn a_pause_queued_when_the_channel_closes_reads_no_player() {
        let (mut rig, device) = linked_with_keys();
        let player = rig.player("one", Playback::Playing);
        device.pods.set_ear(InEar, InEar);
        wait_worn(&rig, 2);
        player.hold_asked(true);
        device.pods.set_ear(InEar, OutOfEar);
        player.wait_asked(Asked::Pause, HANG_LIMIT);
        // Back and out again while the first pause is unanswered, so a second pause waits in the queue behind it; the channel reads them before its end.
        device.pods.set_ear(InEar, InEar);
        device.pods.set_ear(InEar, OutOfEar);
        device.pods.wait_sent(HANG_LIMIT).unwrap();
        hang_up_and_listen(&rig, &device);
        rig.assert_not_logged(
            "the close while the first pause is unanswered",
            &pause_ended(1),
        );
        player.hold_asked(false);
        rig.wait_logged(&pause_ended(0));
        let both_in = ours(BOTH_IN_HEADER, PAYLOAD);
        let _advertiser = rig.bluez().advertise(&both_in.address, &both_in.data);
        player.wait_asked(Asked::Play, HANG_LIMIT);
        assert_eq!(player.reads(), 1);
        assert_eq!(player.asked(), [Asked::Pause, Asked::Play]);
        player.leave();
        rig.finish();
    }
}
