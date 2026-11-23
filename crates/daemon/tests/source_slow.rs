//! The source node and a bud taken out at real time, through the built daemon against the fake pods and a private `PipeWire`: seconds for the grace.

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
    use std::io::ErrorKind;
    use std::os::unix::net::UnixListener;
    use std::time::{Duration, Instant};

    use pods_core::policy::mic::{BUD_GRACE_MS, withdrawn_line};
    use pods_fake::Profile;
    use pods_pw::source::PW_OPEN_MS;
    use pods_pwrig::SOCKET;
    use pods_samples::{MODEL_NUMBER, Placed};

    use super::support::pipewire::{
        holds_no_node, node_name, ready, record, start, wait_no_node, wait_node,
    };
    use super::support::rig::assert_gap;
    use super::support::settings::set;
    use super::support::uplink::{link, rig_with, wait_mic};
    use super::support::wait_for;

    const GRACE: Duration = Duration::from_millis(BUD_GRACE_MS);
    /// Watched past the grace, so a withdrawal at its end would show.
    const PAST_THE_GRACE: Duration = GRACE.saturating_add(Duration::from_secs(1));
    /// Turns of the setting while an open is held, more than any queue of requests would keep.
    const TURNS: usize = 8;
    /// Twice the longest a held open lasts: the span watched for a node no request asked for.
    const PAST_THE_QUEUE: Duration = Duration::from_millis(PW_OPEN_MS * 2);

    #[test]
    #[ignore = "the grace at real time; the slow job runs it"]
    fn the_last_bud_out_withdraws_the_node_after_the_grace() {
        let (mut rig, device, pipewire) = ready();
        let left = Instant::now();
        device.pods.set_ear(Placed::InCase, Placed::InCase);
        wait_no_node(&pipewire);
        assert_gap(
            "the withdrawal after the last bud left",
            left,
            Instant::now(),
            &(GRACE..),
        );
        rig.wait_logged(&withdrawn_line("not_worn"));
        rig.finish();
    }

    #[test]
    #[ignore = "the grace at real time; the slow job runs it"]
    fn a_bud_out_while_recording_keeps_the_node() {
        let (mut rig, device, pipewire) = ready();
        let recorder = record(&rig, &pipewire, "capture.wav");
        wait_mic(&rig, "recording");
        device.pods.set_ear(Placed::InCase, Placed::InCase);
        rig.holds_for(PAST_THE_GRACE, "the node kept", |status| {
            status["mic_source"] == node_name()
        });
        rig.assert_not_logged("the node kept past the grace", &withdrawn_line("not_worn"));
        wait_node(&pipewire);
        drop(recorder.finish());
        wait_no_node(&pipewire);
        rig.wait_logged(&withdrawn_line("not_worn"));
        rig.finish();
    }

    #[test]
    #[ignore = "an open held for PW_OPEN_MS at real time; the slow job runs it"]
    fn turns_of_the_setting_while_an_open_hangs_end_on_the_last() {
        let (rig, device) = rig_with("auto", &[MODEL_NUMBER], Profile::default());
        let socket = rig.session().runtime_dir().join(SOCKET);
        assert!(
            socket.is_absolute() && socket.starts_with(rig.session().dir.path()),
            "{}",
            socket.display()
        );
        // Takes the connection and never answers, so the daemon's first open hangs until both drop.
        let mute = UnixListener::bind(&socket).unwrap();
        mute.set_nonblocking(true).unwrap();
        link(&rig, &device);
        let held = wait_for("the daemon's open on the held socket", || {
            match mute.accept() {
                Ok((stream, _)) => Some(stream),
                Err(error) if error.kind() == ErrorKind::WouldBlock => None,
                Err(error) => panic!("accept on {}: {error}", socket.display()),
            }
        });
        for _ in 0..TURNS {
            set(rig.session(), "microphone", "off");
            set(rig.session(), "microphone", "auto");
        }
        set(rig.session(), "microphone", "off");
        wait_mic(&rig, "off");
        drop(held);
        drop(mute);
        fs::remove_file(&socket).unwrap();
        let pipewire = start(&rig);
        holds_no_node(&pipewire, PAST_THE_QUEUE);
        set(rig.session(), "microphone", "auto");
        wait_node(&pipewire);
        // Ready only when the adapter's `Up` carries the reopen's id, never the first open's.
        wait_mic(&rig, "ready");
        rig.finish();
    }
}
