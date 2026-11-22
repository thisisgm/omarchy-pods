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
    use pods_fake::Profile;
    use pods_samples::Placed::{InEar, OutOfEar};
    use pods_samples::advert::HEADER_LEN;

    use super::support::HANG_LIMIT;
    use super::support::advert::{HEADER, PAYLOAD, keyed, ours};
    use super::support::audio::PERCENT;
    use super::support::audio::wire::sink_named;
    use super::support::players::{Asked, Playback};
    use super::support::rig::{Device, Rig};

    /// `HEADER` with both buds in an ear: status `0x2a`, the left bud primary and out of the case.
    const BOTH_IN_HEADER: [u8; HEADER_LEN] = [
        0x07, 0x19, 0x01, 0x27, 0x20, 0x2a, 0x88, 0x47, 0x00, 0x00, 0x04,
    ];

    /// The sample keys stored, the fake audio server holding the pods' sink as its default, then the pods linked and the advert monitor dropped.
    fn linked_with_keys() -> (Rig, Device) {
        let mut rig = keyed();
        let audio = rig.start_audio();
        let device = rig.add_pods(Profile::default());
        let sink = sink_named(device.address);
        audio.add_sink(&sink, PERCENT);
        audio.set_default_sink(&sink);
        rig.start();
        rig.link_up(&device);
        rig.wait_filled();
        rig.wait_audio_server("up");
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
        rig.wait_audio_server("up");
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
}
