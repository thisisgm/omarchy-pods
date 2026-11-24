//! Support tests: the arm and the dictation parser against the fake pods over socket pairs; the fake is built from sample bytes and names nothing of this crate.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use pods_fake::doap::{SHIFT, Table};
    use pods_fake::{Pods, Profile, pair};
    use pods_proto::dictation::{DictationPacket, Event, Frame};
    use pods_proto::doap::{
        self, ArmFailure, ArmStep, Arming, Handles, STOP_REASON_HOST, STOP_REASON_UNSOLICITED,
    };
    use pods_sys::l2cap::{Channel, Received};

    /// Longer than any loaded host takes to hand over a packet already queued.
    const RECV_TIMEOUT: Duration = Duration::from_secs(5);
    /// Larger than any packet the fake sends.
    const BUFFER_LEN: usize = 65_536;
    /// The audio notifications read once a stream runs, and the most packets read for its end.
    const STREAMED: u8 = 3;
    const TAIL: usize = 10;

    /// The pods on a control channel and an attribute channel; the drop ends every serving thread and waits for it on every way out.
    struct Served {
        pods: Pods,
        control: Channel,
        att: Channel,
    }

    impl Served {
        fn new(table: Table) -> Self {
            let pods = Pods::new(Profile::default());
            pods.doap().table(table);
            let (control, end) = pair();
            pods.serve(end);
            let (att, end) = pair();
            att.set_recv_timeout(RECV_TIMEOUT).unwrap();
            pods.serve_att(end);
            Self { pods, control, att }
        }

        fn recv(&self) -> Vec<u8> {
            let mut buffer = vec![0; BUFFER_LEN];
            match self.att.recv(&mut buffer).unwrap() {
                Received::Sdu(len) => buffer[..len].to_vec(),
                other @ (Received::Truncated { .. } | Received::TimedOut | Received::Closed) => {
                    panic!("wanted one packet within {RECV_TIMEOUT:?}, got {other:?}")
                }
            }
        }

        /// Runs one arm to its end, a reply at a time; the arm bounds its own requests.
        fn arm(&self) -> ArmStep {
            let (mut arming, first) = Arming::new();
            self.att.send(&first).unwrap();
            loop {
                match arming.on_pdu(&self.recv()) {
                    ArmStep::Send(request) => self.att.send(&request).unwrap(),
                    ArmStep::Wait => {}
                    over @ (ArmStep::Done { .. } | ArmStep::Failed(_)) => return over,
                }
            }
        }
    }

    impl Drop for Served {
        fn drop(&mut self) {
            let finished = self.pods.clone().finish();
            if !thread::panicking() {
                finished.unwrap();
            }
        }
    }

    #[test]
    fn arming_against_the_fake_ends_done() {
        let served = Served::new(Table::Observed);
        let done = ArmStep::Done {
            select: doap::select_codec(&Handles::OBSERVED),
            handles: Handles::OBSERVED,
        };
        assert_eq!(served.arm(), done);
    }

    #[test]
    fn a_table_that_stands_32_handles_up_is_armed_where_it_stands() {
        let served = Served::new(Table::Shifted);
        let seen = Handles::OBSERVED;
        let handles = Handles {
            supported_codecs: seen.supported_codecs + SHIFT,
            select_codec: seen.select_codec + SHIFT,
            start: seen.start + SHIFT,
            stop: seen.stop + SHIFT,
            send_data: seen.send_data + SHIFT,
            event: seen.event + SHIFT,
            cccds: seen.cccds.map(|descriptor| descriptor + SHIFT),
        };
        let select = doap::select_codec(&handles);
        assert_eq!(served.arm(), ArmStep::Done { select, handles });
    }

    #[test]
    fn pods_without_the_service_fail_the_arm() {
        let served = Served::new(Table::Absent);
        assert_eq!(served.arm(), ArmStep::Failed(ArmFailure::NoService));
    }

    #[test]
    fn what_the_fake_sends_on_a_start_and_a_stop_is_read_as_dictation() {
        let served = Served::new(Table::Observed);
        let ArmStep::Done { select, handles } = served.arm() else {
            panic!("the arm did not end done");
        };
        served.att.send(&select).unwrap();
        let read = |pdu: &[u8]| {
            DictationPacket::parse(pdu, &handles)
                .unwrap()
                .map(|packet| {
                    // The frames are counted here, since a parsed packet borrows the bytes it was read from.
                    match packet {
                        DictationPacket::Audio(audio) => {
                            assert!(audio.length_matches());
                            let frames: Vec<_> = audio.frames().collect();
                            assert!(frames.iter().all(Frame::is_expected_codec));
                            Ok((audio.packet_counter, frames.len()))
                        }
                        DictationPacket::Event(event) => Err(Some(event)),
                        DictationPacket::Stopped { reason } => {
                            assert_eq!(reason, STOP_REASON_UNSOLICITED);
                            Err(None)
                        }
                        DictationPacket::Unknown { handle, .. } => {
                            panic!("unknown on {handle:#06x}")
                        }
                    }
                })
        };

        // With the mode unset a start is refused.
        served.att.send(&doap::start(&handles)).unwrap();
        assert_eq!(read(&served.recv()), Some(Err(None)));

        let mode = doap::doap_mode_on();
        served.control.send(&mode).unwrap();
        served.pods.wait_heard(&mode, 1, RECV_TIMEOUT).unwrap();
        served.att.send(&doap::start(&handles)).unwrap();
        assert_eq!(read(&served.recv()), Some(Err(Some(Event::STARTED))));
        // Whatever the burst holds, the notifications count from 0 and none is empty.
        for packet in 0..STREAMED {
            let Some(Ok((counter, frames))) = read(&served.recv()) else {
                panic!("notification {packet} is not audio");
            };
            assert_eq!(counter, packet);
            assert!(frames > 0);
        }

        served
            .att
            .send(&doap::stop(&handles, STOP_REASON_HOST))
            .unwrap();
        // Frames heard before the stop may still be on their way; the end follows them.
        let end = Some(Err(Some(Event::StreamingEnd)));
        let mut tail = (0..TAIL).map(|_| read(&served.recv()));
        let before: Vec<_> = tail.by_ref().take_while(|packet| *packet != end).collect();
        assert!(before.len() < TAIL, "no end within {TAIL} packets");
        assert!(before.iter().all(|packet| matches!(packet, Some(Ok(_)))));
        assert_eq!(served.pods.doap().stops(), [STOP_REASON_HOST]);
    }
}
