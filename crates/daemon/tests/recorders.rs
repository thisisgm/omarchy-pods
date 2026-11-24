//! The record streams on the microphone's source through the built daemon against the fake pods, the fake audio server and a private `PipeWire`: which count as dictation, which as any other recording, and when the list is read again.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "each suite uses part of the shared support, so the rest goes unused here"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use pods_pwrig::Server;
    use pods_samples::MODEL_NUMBER;
    use pulseaudio::protocol::SinkState;

    use super::support::audio::{Asked, FakeAudio, PERCENT, Recorder, Volumes};
    use super::support::pipewire::{node_name, start, wait_node};
    use super::support::rig::{Device, Rig};
    use super::support::uplink::{link_heard, rig_with_audio, wait_mic};
    use super::support::wait_for;

    /// The node names Superwhisper gives its two recorders.
    const CAPTURE: &str = "superwhisper-capture";
    const HISTORY: &str = "superwhisper-history";
    /// A source of the desktop's own.
    const BUILT_IN: &str = "alsa_input.built-in";
    /// A sink that comes and goes, so the daemon reads every list again.
    const PASSING_SINK: &str = "alsa_output.passing";
    /// The longest list of record streams the daemon reads.
    const MOST: usize = 256;
    const LINE_START: &str = "mic: recorders ";

    /// The line the daemon logs for these counts of record streams on its source.
    fn counted(dictation: u8, other: u8) -> String {
        format!("{LINE_START}(dictation {dictation}, other {other})")
    }

    /// Every line the daemon logged for the record streams, in order; sample `mic: recorders (dictation 1, other 0)`.
    fn lines(rig: &mut Rig) -> Vec<String> {
        let log = rig.daemon().log();
        let counts = log.lines().filter(|line| line.starts_with(LINE_START));
        counts.map(str::to_owned).collect()
    }

    fn wait_lines(rig: &mut Rig, wanted: &[String]) {
        wait_for(&format!("the lines {wanted:?}"), || {
            let logged = lines(rig);
            assert!(wanted.starts_with(&logged), "{logged:?}, not {wanted:?}");
            (logged == wanted).then_some(())
        });
    }

    /// The source up with no recorder, and the fake audio server listing it beside a source of the desktop's.
    fn ready() -> (Rig, Device, Rc<FakeAudio>, Server) {
        let (rig, device, audio) = rig_with_audio("auto", &[MODEL_NUMBER], SinkState::Suspended);
        let pipewire = start(&rig);
        link_heard(&rig, &device);
        wait_node(&pipewire);
        wait_mic(&rig, "ready");
        audio.add_source(BUILT_IN);
        audio.add_source(&node_name());
        (rig, device, audio, pipewire)
    }

    /// Returns once the daemon read the record streams again after a sink came and went.
    fn read_again(audio: &FakeAudio) {
        let before = audio.requests().len();
        audio.add_sink(PASSING_SINK, PERCENT);
        audio.remove_sink(PASSING_SINK);
        wait_for("the record streams read again", || {
            audio.requests()[before..]
                .contains(&Asked::RecorderList)
                .then_some(())
        });
    }

    #[test]
    fn superwhispers_recorder_counts_as_dictation() {
        let (mut rig, _device, audio, _pipewire) = ready();
        audio.add_recorder(&node_name(), Recorder::Named(CAPTURE));
        wait_lines(&mut rig, &[counted(1, 0)]);
        // An unchanged list read again is not told again.
        read_again(&audio);
        let history = audio.add_recorder(&node_name(), Recorder::Named(HISTORY));
        wait_lines(&mut rig, &[counted(1, 0), counted(2, 0)]);
        audio.remove_recorder(history);
        wait_lines(&mut rig, &[counted(1, 0), counted(2, 0), counted(1, 0)]);
        rig.finish();
    }

    #[test]
    fn any_other_recorder_counts_as_other() {
        let (mut rig, _device, audio, _pipewire) = ready();
        audio.add_recorder(&node_name(), Recorder::Program);
        wait_lines(&mut rig, &[counted(0, 1)]);
        audio.add_recorder(&node_name(), Recorder::Named("Chromium input"));
        wait_lines(&mut rig, &[counted(0, 1), counted(0, 2)]);
        audio.add_recorder(&node_name(), Recorder::Named(CAPTURE));
        wait_lines(&mut rig, &[counted(0, 1), counted(0, 2), counted(1, 2)]);
        rig.finish();
    }

    #[test]
    fn a_recorder_on_another_source_is_not_counted() {
        let (mut rig, _device, audio, _pipewire) = ready();
        // First a stream not Superwhisper's, so a read counting the streams elsewhere never logs the one line waited for.
        audio.add_recorder(BUILT_IN, Recorder::Program);
        audio.add_recorder(BUILT_IN, Recorder::Named(CAPTURE));
        audio.add_recorder("alsa_input.absent", Recorder::Named(HISTORY));
        // Last the one stream on the source, so the read that counts it lists the three elsewhere too.
        audio.add_recorder(&node_name(), Recorder::Named(HISTORY));
        wait_lines(&mut rig, &[counted(1, 0)]);
        rig.finish();
    }

    #[test]
    fn an_oversized_list_is_refused_and_counts_none() {
        let (mut rig, _device, audio, _pipewire) = ready();
        audio.add_recorder(&node_name(), Recorder::Named(CAPTURE));
        wait_lines(&mut rig, &[counted(1, 0)]);
        for _ in 0..MOST - 2 {
            audio.add_recorder(BUILT_IN, Recorder::Program);
        }
        // The longest list the daemon reads is still read.
        audio.add_recorder(&node_name(), Recorder::Named(HISTORY));
        wait_lines(&mut rig, &[counted(1, 0), counted(2, 0)]);
        let one_more = audio.add_recorder(BUILT_IN, Recorder::Program);
        wait_lines(&mut rig, &[counted(1, 0), counted(2, 0), counted(0, 0)]);
        audio.remove_recorder(one_more);
        let all = [counted(1, 0), counted(2, 0), counted(0, 0), counted(2, 0)];
        wait_lines(&mut rig, &all);
        assert_eq!(rig.status()["audio_server"], "up");
        rig.finish();
    }

    #[test]
    fn a_server_restart_reads_the_list_again() {
        let (mut rig, _device, audio, _pipewire) = ready();
        audio.add_recorder(&node_name(), Recorder::Named(CAPTURE));
        wait_lines(&mut rig, &[counted(1, 0)]);
        audio.restart(Volumes::Kept);
        wait_lines(&mut rig, &[counted(1, 0), counted(1, 0)]);
        rig.finish();
    }

    #[test]
    fn a_recorder_listed_before_the_source_is_up_is_counted_once_it_is() {
        let (mut rig, device, audio) =
            rig_with_audio("auto", &[MODEL_NUMBER], SinkState::Suspended);
        audio.add_source(&node_name());
        audio.add_recorder(&node_name(), Recorder::Named(CAPTURE));
        // Returns after a whole read of the server with no source named, and a later one begun.
        link_heard(&rig, &device);
        assert!(!audio.requests().contains(&Asked::SourceList));
        assert_eq!(lines(&mut rig), Vec::<String>::new());
        // No event of the audio server follows, so only the source's name coming up reads the list.
        let pipewire = start(&rig);
        wait_node(&pipewire);
        wait_lines(&mut rig, &[counted(1, 0)]);
        rig.finish();
    }
}
