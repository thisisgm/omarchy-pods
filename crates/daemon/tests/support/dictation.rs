//! The built daemon on private audio servers, with both of the fake pods' channels.

use std::rc::Rc;
use std::time::Duration;

use pods_fake::doap::Table;
use pods_proto::doap::{STOP_REASON_HOST, doap_mode_on};
use pods_pwrig::{Recorder, Server};
use pods_samples::MODEL_NUMBER;
use pulseaudio::protocol::SinkState;

use super::audio::{FakeAudio, Recorder as Listed, RecorderId};
use super::pipewire::{node_name, record, start, wait_node};
use super::recording::frames_at;
use super::rig::{Device, Rig};
use super::uplink::{read_up_to_now, rig_with_audio, starts, stops, wait_mic};
use super::{HANG_LIMIT, wait_for};

pub(crate) const CAPTURE: Listed = Listed::Named("superwhisper-capture");
pub(crate) const HOST_STOP: u8 = STOP_REASON_HOST;
pub(crate) const LEFT: &str = "dictation: stop sent (left)";
pub(crate) const NO_RECORDER: &str = "mic: recorders (dictation 0, other 0)";

pub(crate) struct Fixture {
    pub(crate) rig: Rig,
    pub(crate) device: Device,
    pub(crate) audio: Rc<FakeAudio>,
    pub(crate) server: Server,
}

impl Fixture {
    pub(crate) fn new() -> Self {
        let (rig, device, audio) = rig_with_audio("auto", &[MODEL_NUMBER], SinkState::Running);
        device.pods.doap().table(Table::Observed);
        audio.add_source(&node_name());
        let server = start(&rig);
        rig.link_up(&device);
        rig.wait_filled();
        read_up_to_now(&rig, &device);
        rig.wait_sink_told();
        wait_node(&server);
        status(&rig, "armed");
        wait_mic(&rig, "ready");
        Self {
            rig,
            device,
            audio,
            server,
        }
    }

    pub(crate) fn list(&self, kind: Listed) -> RecorderId {
        self.audio.add_recorder(&node_name(), kind)
    }

    pub(crate) fn capture(&self) -> Recorder<'_> {
        record(&self.rig, &self.server, "dictation.wav")
    }

    pub(crate) fn call(&self) -> Recorder<'_> {
        let source = node_name();
        let file = self.rig.session().dir.path().join("call.wav");
        let call = Recorder::program(&self.server, &source, &file);
        self.server.link(&source, call.node());
        call
    }

    pub(crate) fn start(&self, count: u32) {
        let doap = self.device.pods.doap();
        wait_for("the requested DoAP START", || {
            assert!(
                starts(&self.device).is_empty(),
                "the daemon sent 0x58 START before DoAP"
            );
            (doap.starts() == count).then_some(())
        });
    }

    pub(crate) fn streaming(&self, count: u32) {
        self.start(count);
        status(&self.rig, "streaming");
        wait_mic(&self.rig, "recording");
    }

    pub(crate) fn stopped(&self, count: usize) {
        status(&self.rig, "armed");
        wait_for("the stem returned after STOP", || {
            let doap = self.device.pods.doap();
            (!doap.mode_seen() && doap.stops().len() == count).then_some(())
        });
        assert_eq!(self.device.pods.doap().stops(), vec![HOST_STOP; count]);
    }

    pub(crate) fn mode_writes(&self) -> usize {
        self.device
            .pods
            .heard()
            .iter()
            .filter(|heard| heard.sdu == doap_mode_on())
            .count()
    }

    pub(crate) fn no_uplink(&self) {
        assert!(starts(&self.device).is_empty(), "0x58 START heard");
        assert!(stops(&self.device).is_empty(), "0x58 STOP heard");
    }

    pub(crate) fn finish(self) {
        self.rig.finish();
    }
}

pub(crate) fn status(rig: &Rig, word: &str) {
    rig.wait_status(word, |status| status["mic_dictation"] == word);
}

pub(crate) fn uplink(fixture: &Fixture) {
    wait_for("0x58 START", || {
        (!starts(&fixture.device).is_empty()).then_some(())
    });
    wait_mic(&fixture.rig, "recording");
}

/// Waits for `span` of audio at the recorder's own rate, since a call records at 48 kHz.
pub(crate) fn ready_to_read(recorder: &Recorder<'_>, span: Duration) {
    recorder.wait_frames(frames_at(span, recorder.rate()), HANG_LIMIT);
}
