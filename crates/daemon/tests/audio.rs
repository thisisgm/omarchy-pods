//! The audio server connection through the built daemon: the fake pods link, the fake audio server answers, and the status reads what the daemon found.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "each suite uses part of the shared support, so the rest goes unused here"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{Duration, Instant};

    use pods_fake::Profile;
    use pulseaudio::protocol::{MAX_VERSION, SubscriptionMask};
    use serde_json::Value;
    use zbus::OwnedVariant;

    use super::support::audio::wire::{FRAME_MAX, OVERSIZED_LENGTH, sink_named};
    use super::support::audio::{Asked, CALL_TIMEOUT, FakeAudio, LADDER_MS, PERCENT, Volumes};
    use super::support::bus::{BUS_NAME, INTERFACE, Listener, OBJECT_PATH, changes_rule};
    use super::support::players::PROPERTIES;
    use super::support::rig::{REAL_TIME_SLACK, Rig, assert_gap};
    use super::support::{HANG_LIMIT, pause, wait_for};

    /// Past the first two steps of the daemon's retry ladder, by the slack a real time check takes.
    const PAST_TWO_RETRIES: Duration =
        Duration::from_millis(LADDER_MS[0] + LADDER_MS[1]).saturating_add(REAL_TIME_SLACK);
    const BURST: u8 = 50;

    /// What the daemon sends on each connection, before any event.
    fn opening() -> Vec<Asked> {
        vec![
            Asked::Auth {
                version: MAX_VERSION,
            },
            Asked::ClientName,
            Asked::Subscribe {
                mask: SubscriptionMask::SINK
                    | SubscriptionMask::SERVER
                    | SubscriptionMask::SOURCE
                    | SubscriptionMask::SOURCE_OUTPUT,
            },
            Asked::ServerInfo,
            Asked::SinkList,
        ]
    }

    const SINK_REFRESH: [Asked; 1] = [Asked::SinkList];
    const SERVER_REFRESH: [Asked; 2] = [Asked::ServerInfo, Asked::SinkList];

    fn down_line(reason: &str) -> String {
        format!("audio: the audio server is down: {reason}")
    }

    fn property(rig: &Rig, name: &str) -> String {
        let bus = Listener::subscribe(rig.session_bus(), &changes_rule());
        let reply = bus
            .call(BUS_NAME, OBJECT_PATH, PROPERTIES, "Get", &(INTERFACE, name))
            .unwrap();
        let value: OwnedVariant = reply.body().deserialize().unwrap();
        String::try_from(value).unwrap()
    }

    #[test]
    fn audio_server_reads_idle_with_no_device_and_no_socket_is_open() {
        let mut rig = Rig::new();
        let audio = rig.start_audio();
        rig.add_pods(Profile::default());
        rig.start();
        // Long enough for a socket opened by mistake at startup, or by a retry, to show.
        pause(PAST_TWO_RETRIES);
        assert_eq!(rig.status()["audio_server"], "idle");
        assert_eq!(audio.clients(), 0);
        assert_eq!(audio.requests(), []);
        rig.finish();
    }

    #[test]
    fn it_reads_up_with_the_link_up() {
        let (rig, _device, audio) = Rig::with_audio(PERCENT);
        assert_eq!(rig.status()["audio_server"], "up");
        wait_for("the state file to read up", || {
            let line = fs::read_to_string(rig.session().state_file()).unwrap();
            let file: Value = serde_json::from_str(&line).unwrap();
            (file["audio_server"] == "up").then_some(())
        });
        assert_eq!(property(&rig, "AudioServer"), "up");
        let document: Value = serde_json::from_str(&property(&rig, "StatusJson")).unwrap();
        assert_eq!(document["audio_server"], "up");
        assert_eq!(audio.requests(), opening());
        assert_eq!(audio.clients(), 1);
        audio.assert_only_reads();
        rig.finish();
    }

    #[test]
    fn it_reads_down_when_nothing_listens() {
        let mut rig = Rig::new();
        let device = rig.add_pods(Profile::default());
        rig.start();
        rig.link_up(&device);
        rig.wait_filled();
        rig.wait_logged(&down_line("No such file or directory (os error 2)"));
        assert_eq!(rig.status()["audio_server"], "down");
        assert_eq!(property(&rig, "AudioServer"), "down");
        rig.finish();
    }

    #[test]
    fn the_socket_closes_when_the_link_is_lost() {
        let (rig, device, audio) = Rig::with_audio(PERCENT);
        rig.link_down(&device);
        rig.wait_audio_server("idle");
        audio.wait_clients(0, HANG_LIMIT);
        // Nothing is followed, so no retry may open it again.
        pause(PAST_TWO_RETRIES);
        assert_eq!(audio.clients(), 0);
        assert_eq!(audio.requests(), opening());
        rig.relink(&device);
        rig.wait_audio_server("up");
        assert_eq!(audio.requests(), [opening(), opening()].concat());
        audio.assert_only_reads();
        rig.finish();
    }

    #[test]
    fn a_server_that_never_answers_does_not_block_status() {
        let (mut rig, device, audio) = Rig::with_audio_holding(PERCENT);
        let linked = Instant::now();
        rig.link_up(&device);
        let asked = audio.wait_requests(1, HANG_LIMIT);
        assert_eq!(asked[..], opening()[..1]);
        let timed_out = down_line(&format!("no answer within {} ms", CALL_TIMEOUT.as_millis()));
        // The timeout not logged yet, so this answer came while the call hung.
        let status = rig.status();
        rig.assert_not_logged("status while the server holds its answer", &timed_out);
        assert_eq!(status["audio_server"], "down");
        audio.wait_clients(0, CALL_TIMEOUT.saturating_add(HANG_LIMIT));
        let closed = Instant::now();
        assert_gap(
            "the close after the link came up",
            linked,
            closed,
            &(CALL_TIMEOUT..=Duration::MAX),
        );
        rig.wait_logged(&timed_out);
        audio.hold_replies(false);
        rig.wait_audio_server("up");
        audio.assert_only_reads();
        rig.finish();
    }

    #[test]
    fn a_server_that_never_answers_does_not_delay_the_stop() {
        let (mut rig, device, audio) = Rig::with_audio_holding(PERCENT);
        rig.link_up(&device);
        audio.wait_requests(1, HANG_LIMIT);
        let status = rig.stop(HANG_LIMIT);
        assert!(status.success(), "{status:?}");
        audio.wait_clients(0, HANG_LIMIT);
        audio.assert_only_reads();
        rig.finish();
    }

    #[test]
    fn an_oversized_frame_ends_the_connection_and_is_retried() {
        let (mut rig, _device, audio) = Rig::with_audio(PERCENT);
        audio.send_oversized_frame();
        rig.wait_logged(&down_line(&format!(
            "a frame of {OVERSIZED_LENGTH} bytes, past {FRAME_MAX}"
        )));
        audio.wait_requests(opening().len().saturating_mul(2), HANG_LIMIT);
        rig.wait_audio_server("up");
        assert_eq!(audio.requests(), [opening(), opening()].concat());
        assert_eq!(audio.clients(), 1);
        audio.assert_only_reads();
        rig.finish();
    }

    #[test]
    fn an_event_burst_costs_two_refreshes_at_most() {
        let (rig, device, audio) = Rig::with_audio(PERCENT);
        let sink = sink_named(device.address);
        let before = audio.requests().len();
        // Held, so the whole burst reaches the daemon at once, behind no answer.
        audio.hold_replies(true);
        for step in 1..=BURST {
            audio.set_volume(&sink, PERCENT.saturating_add(step));
        }
        audio.hold_replies(false);
        audio.wait_requests(before.saturating_add(1), HANG_LIMIT);
        // A follow-up for any event still in flight would arrive within this.
        pause(REAL_TIME_SLACK);
        rig.link_down(&device);
        audio.wait_clients(0, HANG_LIMIT);
        let asked = audio.requests();
        assert!(
            matches!(
                &asked[before..],
                [Asked::SinkList] | [Asked::SinkList, Asked::SinkList]
            ),
            "{:?}",
            &asked[before..]
        );
        audio.assert_only_reads();
        rig.finish();
    }

    /// Makes `change` and waits for the requests its event should cause.
    fn refreshed_after(
        audio: &FakeAudio,
        wanted: &mut Vec<Asked>,
        reads: &[Asked],
        change: impl FnOnce(&FakeAudio),
    ) {
        change(audio);
        wanted.extend_from_slice(reads);
        audio.wait_requests(wanted.len(), HANG_LIMIT);
    }

    #[test]
    fn nothing_but_reads_is_ever_asked() {
        let (rig, device, audio) = Rig::with_audio(PERCENT);
        let sink = sink_named(device.address);
        let other = "alsa_output.speakers";
        let mut wanted = opening();
        refreshed_after(&audio, &mut wanted, &SINK_REFRESH, |audio| {
            audio.add_sink(other, PERCENT);
        });
        refreshed_after(&audio, &mut wanted, &SERVER_REFRESH, |audio| {
            audio.set_default_sink(other);
        });
        refreshed_after(&audio, &mut wanted, &SINK_REFRESH, |audio| {
            audio.set_volume(&sink, PERCENT.saturating_add(1));
        });
        refreshed_after(&audio, &mut wanted, &SERVER_REFRESH, |audio| {
            audio.set_default_sink(&sink);
        });
        refreshed_after(&audio, &mut wanted, &SINK_REFRESH, |audio| {
            audio.remove_sink(other);
        });
        audio.restart(Volumes::Kept);
        wanted.extend(opening());
        audio.wait_requests(wanted.len(), HANG_LIMIT);
        // Removing the default sends two events, which may reach the daemon together.
        audio.remove_sink(&sink);
        let removed = wait_for("the new default read after removal", || {
            let asked = audio.requests();
            let reads = &asked[wanted.len()..];
            reads.ends_with(&SERVER_REFRESH).then(|| reads.to_vec())
        });
        assert!(
            matches!(
                removed.as_slice(),
                [Asked::ServerInfo, Asked::SinkList]
                    | [Asked::SinkList, Asked::ServerInfo, Asked::SinkList]
            ),
            "{removed:?}"
        );
        wanted.extend(removed);
        refreshed_after(&audio, &mut wanted, &SINK_REFRESH, |audio| {
            audio.add_sink(&sink, PERCENT);
        });
        rig.link_down(&device);
        audio.wait_clients(0, HANG_LIMIT);
        assert_eq!(audio.requests(), wanted);
        audio.assert_only_reads();
        rig.finish();
    }
}
