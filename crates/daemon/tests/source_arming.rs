//! Program arming and read-only headset blocking through the daemon, the fake audio server and a private `PipeWire` graph.

#[cfg(test)]
#[expect(
    dead_code,
    reason = "each suite uses part of the shared support, so the rest goes unused here"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::rc::Rc;
    use std::time::Duration;

    use crate::support::audio::protocol::SinkState;
    use pods_core::policy::mic::{STOPPED_BY_THE_PODS_LINE, withdrawn_line};
    use pods_pwrig::Server;
    use pods_samples::{MODEL_NUMBER, Placed};

    use super::support::audio::wire::card_named;
    use super::support::audio::{Asked, FakeAudio, FakeProfile, Volumes};
    use super::support::pipewire::{links_of, record, start, wait_no_node, wait_node};
    use super::support::recording::{first_sound, frames};
    use super::support::rig::{Device, Rig};
    use super::support::uplink::{
        assert_no_sink_call, link, link_heard, put_in, read_up_to_now, rig_with_audio,
        set_off_unworn, starts, stops, take_out, wait_mic, wait_starts, wait_stop,
    };
    use super::support::{HANG_LIMIT, wait_for};

    const PLAYBACK: &str = "a2dp-sink";
    const HEADSET: &str = "headset-head-unit-msbc";
    const MAX_CLIENTS: usize = 256;
    const CAPTURE: Duration = Duration::from_millis(300);
    const PROFILES: [FakeProfile; 2] = [
        FakeProfile {
            name: PLAYBACK,
            available: true,
            sinks: 1,
            sources: 0,
        },
        FakeProfile {
            name: HEADSET,
            available: true,
            sinks: 1,
            sources: 1,
        },
    ];

    fn setup(state: SinkState, active: &str) -> (Rig, Device, Rc<FakeAudio>, Server, String) {
        let (rig, device, audio) = rig_with_audio("auto", &[MODEL_NUMBER], state);
        let card = card_named(device.address);
        audio.add_card(&card, &PROFILES, active);
        let pipewire = start(&rig);
        (rig, device, audio, pipewire, card)
    }

    fn ready() -> (Rig, Device, Rc<FakeAudio>, Server, String) {
        let (rig, device, audio, pipewire, card) = setup(SinkState::Suspended, PLAYBACK);
        link_heard(&rig, &device);
        wait_node(&pipewire);
        wait_mic(&rig, "ready");
        wait_for("the source list read after the node appeared", || {
            audio.requests().contains(&Asked::SourceList).then_some(())
        });
        (rig, device, audio, pipewire, card)
    }

    fn armed(rig: &Rig, device: &Device) {
        wait_starts(device, 1);
        wait_mic(rig, "live");
        assert!(device.pods.uplink().running());
    }

    fn add_unarmed(rig: &Rig, device: &Device, audio: &FakeAudio, hint: Option<&str>) -> u32 {
        let before = audio.requests().len();
        let index = audio.add_client(hint);
        wait_for("a client event to read the client list", || {
            audio.requests()[before..]
                .contains(&Asked::ClientList)
                .then_some(())
        });
        rig.wait_sink_told();
        read_up_to_now(rig, device);
        assert_eq!(starts(device), []);
        index
    }

    fn captured(rig: &Rig, device: &Device, pipewire: &Server) {
        let recorder = record(rig, pipewire, "capture.wav");
        wait_mic(rig, "recording");
        recorder.wait_frames(frames(CAPTURE), HANG_LIMIT);
        first_sound(&recorder.finish());
        read_up_to_now(rig, device);
        assert_eq!(starts(device).len(), 1);
        assert_eq!(stops(device), []);
    }

    #[test]
    fn a_program_that_asked_for_a_fast_start_holds_the_uplink_open() {
        let (rig, device, audio, pipewire, _) = ready();
        let before = audio.requests().len();
        audio.add_client(Some("true"));
        armed(&rig, &device);
        let node = wait_node(&pipewire);
        assert_eq!(
            links_of(&pipewire.dump(), &node),
            Vec::<serde_json::Value>::new()
        );
        assert_eq!(audio.clients(), 1);
        assert_eq!(audio.requests()[before..], [Asked::ClientList]);
        assert_eq!(starts(&device).len(), 1);
        assert_eq!(stops(&device), []);
        rig.finish();
        audio.assert_only_reads();
    }

    #[test]
    fn a_recorder_then_gets_audio_with_no_new_start() {
        let (rig, device, audio, pipewire, _) = ready();
        audio.add_client(Some("true"));
        armed(&rig, &device);
        captured(&rig, &device, &pipewire);
        rig.finish();
        audio.assert_only_reads();
    }

    #[test]
    fn a_program_without_the_property_arms_nothing() {
        let (rig, device, audio, _pipewire, _) = ready();
        add_unarmed(&rig, &device, &audio, None);
        wait_mic(&rig, "ready");
        audio.add_client(Some("true"));
        armed(&rig, &device);
        rig.finish();
        audio.assert_only_reads();
    }

    #[test]
    fn the_property_set_to_false_arms_nothing() {
        let (rig, device, audio, _pipewire, _) = ready();
        add_unarmed(&rig, &device, &audio, Some("false"));
        wait_mic(&rig, "ready");
        audio.add_client(Some("true"));
        armed(&rig, &device);
        rig.finish();
        audio.assert_only_reads();
    }

    #[test]
    fn the_program_and_no_bud_worn_arms_nothing() {
        let (rig, device, audio, _pipewire, _) = ready();
        take_out(&rig, &device);
        wait_mic(&rig, "not_worn");
        add_unarmed(&rig, &device, &audio, Some("true"));
        put_in(&rig, &device);
        armed(&rig, &device);
        rig.finish();
        audio.assert_only_reads();
    }

    #[test]
    fn a_headset_profile_withdraws_the_node_and_sends_nothing() {
        let (mut rig, device, audio, pipewire, card) = ready();
        let recorder = record(&rig, &pipewire, "linked.wav");
        wait_mic(&rig, "recording");
        let before = audio.requests().len();
        audio.set_profile_as_user(&card, HEADSET);
        let status = wait_mic(&rig, "blocked");
        assert_eq!(status["mic_source"], "");
        wait_no_node(&pipewire);
        rig.wait_logged(&withdrawn_line("blocked"));
        assert_eq!(audio.requests().get(before), Some(&Asked::CardList));
        read_up_to_now(&rig, &device);
        assert_eq!(starts(&device).len(), 1);
        assert_eq!(stops(&device), []);
        assert!(device.pods.uplink().running());
        drop(recorder.finish());
        rig.finish();
        audio.assert_only_reads();
    }

    #[test]
    fn a_link_with_an_armed_client_on_a_headset_profile_sends_no_start() {
        let (rig, device, audio, _pipewire, _) = setup(SinkState::Suspended, HEADSET);
        audio.add_client(Some("true"));
        // Let the daemon learn the buds are worn before it can read the card and client lists.
        audio.hold_replies(true);
        link(&rig, &device);
        audio.hold_replies(false);
        rig.wait_sink_told();
        wait_mic(&rig, "blocked");
        read_up_to_now(&rig, &device);
        assert!(
            starts(&device).is_empty(),
            "a START went out with the headset profile active"
        );
        rig.finish();
        audio.assert_only_reads();
    }

    #[test]
    fn always_starts_after_the_first_playback_profile_is_read() {
        let (rig, device, audio) = always_with_card_reply_held(PLAYBACK);
        audio.hold_replies(false);
        armed(&rig, &device);
        rig.finish();
        audio.assert_only_reads();
    }

    #[test]
    fn always_sends_no_start_when_the_first_profile_is_headset() {
        let (rig, device, audio) = always_with_card_reply_held(HEADSET);
        audio.hold_replies(false);
        wait_mic(&rig, "blocked");
        rig.wait_sink_told();
        read_up_to_now(&rig, &device);
        assert!(starts(&device).is_empty(), "START with a headset profile");
        rig.finish();
        audio.assert_only_reads();
    }

    fn always_with_card_reply_held(active: &str) -> (Rig, Device, Rc<FakeAudio>) {
        let (rig, device, audio) = rig_with_audio("always", &[MODEL_NUMBER], SinkState::Suspended);
        audio.add_card(&card_named(device.address), &PROFILES, active);
        audio.hold_at_card_list();
        device.pods.set_ear(Placed::InCase, Placed::InCase);
        rig.link_up(&device);
        wait_for("the first card read to reach the fake server", || {
            audio.requests().contains(&Asked::CardList).then_some(())
        });
        rig.wait_status(
            "the model reported while the card reply is held",
            |status| status["model_number"] == MODEL_NUMBER,
        );
        put_in(&rig, &device);
        read_up_to_now(&rig, &device);
        assert!(
            starts(&device).is_empty(),
            "START before the first card reply"
        );
        assert_eq!(rig.status()["audio_server"], "up");
        (rig, device, audio)
    }

    #[test]
    fn leaving_the_headset_profile_brings_the_node_back() {
        let (rig, device, audio, pipewire, card) = setup(SinkState::Suspended, HEADSET);
        link_heard(&rig, &device);
        wait_mic(&rig, "blocked");
        wait_no_node(&pipewire);
        add_unarmed(&rig, &device, &audio, Some("true"));
        audio.set_profile_as_user(&card, PLAYBACK);
        armed(&rig, &device);
        wait_node(&pipewire);
        rig.finish();
        audio.assert_only_reads();
    }

    #[test]
    fn a_stopped_marker_while_blocked_orders_no_restore() {
        let (mut rig, device, audio, _pipewire, card) = setup(SinkState::Running, PLAYBACK);
        link_heard(&rig, &device);
        audio.add_client(Some("true"));
        armed(&rig, &device);
        audio.set_profile_as_user(&card, HEADSET);
        wait_mic(&rig, "blocked");
        device.pods.uplink().stop_unasked();
        rig.wait_logged(STOPPED_BY_THE_PODS_LINE);
        read_up_to_now(&rig, &device);
        assert_no_sink_call(&rig, &audio);
        assert_eq!(stops(&device), []);
        rig.finish();
        audio.assert_only_reads();
    }

    #[test]
    fn a_restore_owed_across_server_loss_is_skipped_on_a_headset_profile() {
        let (mut rig, device, audio, _pipewire, card) = setup(SinkState::Running, PLAYBACK);
        link_heard(&rig, &device);
        audio.add_client(Some("true"));
        armed(&rig, &device);
        audio.stop();
        rig.wait_audio_server("down");
        set_off_unworn(&rig, &device);
        wait_stop(&device);
        rig.wait_logged("mic: restore owed, no audio server");
        wait_mic(&rig, "off");

        audio.set_profile_as_user(&card, HEADSET);
        audio.restart(Volumes::Kept);
        rig.wait_sink_told();
        rig.wait_logged("mic: restore skipped, headset profile active");
        let suspends: Vec<_> = audio
            .writes()
            .into_iter()
            .filter_map(|asked| {
                if let Asked::Suspend { suspend, .. } = asked {
                    Some(suspend)
                } else {
                    None
                }
            })
            .collect();
        assert!(
            suspends.is_empty(),
            "unexpected suspend flags after reconnect {suspends:?}"
        );
        rig.finish();
    }

    #[test]
    fn a_server_restart_brings_the_node_back_and_keeps_the_uplink() {
        let (mut rig, device, audio, pipewire, _) = ready();
        audio.add_client(Some("true"));
        armed(&rig, &device);
        audio.stop();
        rig.wait_audio_server("down");
        pipewire.restart();
        rig.wait_logged(&withdrawn_line("server"));
        audio.restart(Volumes::Kept);
        rig.wait_audio_server("up");
        wait_node(&pipewire);
        captured(&rig, &device, &pipewire);
        assert_eq!(audio.clients(), 1);
        rig.finish();
        audio.assert_only_reads();
    }

    #[test]
    fn an_oversized_client_list_is_refused_and_arms_nothing() {
        let (rig, device, audio, pipewire, _) = setup(SinkState::Suspended, PLAYBACK);
        for index in 0..MAX_CLIENTS {
            let hint = (index == MAX_CLIENTS / 2).then_some("true");
            audio.add_client(hint);
        }
        let excess = audio.add_client(None);
        link_heard(&rig, &device);
        wait_for("the oversized client list to be read", || {
            audio.requests().contains(&Asked::ClientList).then_some(())
        });
        wait_node(&pipewire);
        rig.wait_sink_told();
        read_up_to_now(&rig, &device);
        assert!(
            starts(&device).is_empty(),
            "the oversized client list armed the uplink"
        );
        wait_mic(&rig, "ready");
        audio.remove_client(excess);
        armed(&rig, &device);
        rig.finish();
        audio.assert_only_reads();
    }

    #[test]
    fn no_profile_default_or_port_is_ever_set() {
        let (rig, device, audio, pipewire, card) = setup(SinkState::Suspended, PLAYBACK);
        audio.add_card("another_card", &PROFILES, HEADSET);
        link_heard(&rig, &device);
        wait_node(&pipewire);
        wait_mic(&rig, "ready");
        audio.set_profile_as_user(&card, HEADSET);
        wait_mic(&rig, "blocked");
        add_unarmed(&rig, &device, &audio, Some("true"));
        assert_eq!(audio.active_profile(&card), HEADSET);
        audio.set_profile_as_user(&card, PLAYBACK);
        armed(&rig, &device);
        wait_node(&pipewire);
        assert_eq!(audio.active_profile(&card), PLAYBACK);
        rig.finish();
        audio.assert_only_reads();
    }
}
