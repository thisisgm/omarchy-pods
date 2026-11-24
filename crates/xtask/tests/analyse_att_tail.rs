//! The built binary's `probe analyse` command on sessions the pods still send frames of after the stop write: which of them the session takes, and which stay outside.

#[cfg(test)]
#[expect(
    dead_code,
    reason = "the readers of rows no test here asks for go unused"
)]
#[path = "support/att.rs"]
mod att_run;
#[cfg(test)]
#[expect(
    dead_code,
    reason = "the parts of the generator only the tests of whole sessions read go unused here"
)]
#[path = "../src/probe/analyse/att/synth.rs"]
mod att_synth;
#[cfg(test)]
#[expect(
    dead_code,
    unused_imports,
    reason = "the builders of uplink runs go unused where every run is a dictation run"
)]
mod support;
#[cfg(test)]
#[expect(
    dead_code,
    reason = "the uplink generator is here only because the shared builders name it"
)]
#[path = "../src/probe/analyse/synth.rs"]
mod synth;
#[cfg(test)]
#[path = "../src/probe/xorshift.rs"]
mod xorshift;

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod tests {
    use pods_proto::dictation::{
        DictationPacket, Frame, TOC_CELT_WIDEBAND_20MS_MONO, encode_audio,
    };
    use pods_proto::doap::Handles;
    use pods_proto::sdulog::{Direction, Record};

    use super::att_run::{Analysed, START_NS, analyse, marks_of};
    use super::att_synth::{NS_PER_MS, Plan, STOP_AFTER_MS, Session, synth};
    use super::support::log_text;

    /// The frames of a session before its stop write.
    const FRAMES: u32 = 200;
    /// How long after its stop mark a session still takes frames.
    const TAIL_NS: u64 = 1000 * NS_PER_MS;
    /// The field of a line of the frames table that holds its session, counted from 0.
    const SESSION_FIELD: usize = 3;

    /// A session of `FRAMES` frames that the pods send `late` more packets of after its stop write, each one frame on.
    fn with_late(late: u32) -> Session {
        synth(Plan {
            late: (late, 0),
            ..Plan::seen(START_NS, FRAMES)
        })
    }

    /// The places in `records` of the audio notifications stamped after the stop write of the session.
    fn late_audio(session: &Session) -> Vec<usize> {
        let is_late_audio = |record: &Record| {
            let parsed = DictationPacket::parse(&record.sdu, &Handles::OBSERVED);
            record.at_ns > session.stop_ns && matches!(parsed, Ok(Some(DictationPacket::Audio(_))))
        };
        let places = session.records.iter().enumerate();
        places
            .filter(|(_, record)| is_late_audio(record))
            .map(|(place, _)| place)
            .collect()
    }

    /// One audio notification of one frame with this frame counter.
    fn one_frame(counter: u16) -> Vec<u8> {
        let frame = Frame {
            flag: 0,
            level: 200,
            counter,
            data: &[TOC_CELT_WIDEBAND_20MS_MONO],
        };
        encode_audio(&Handles::OBSERVED, 0, &[frame]).unwrap()
    }

    fn analysed(name: &str, session: &Session) -> Analysed {
        let home = tempfile::tempdir().unwrap();
        let sessions = std::slice::from_ref(session);
        analyse(
            &home,
            name,
            &log_text(&session.records),
            &marks_of(sessions),
        )
    }

    // Sample input: `7\t3\t2080000000\t1\t7\t0x80\t213\t81\t0xb8`, a frame of session 1 by its fourth field, which is empty outside a session.
    fn listed_in(found: &Analysed, session: &str) -> usize {
        let fields = found.frames.lines().skip(1);
        let sessions = fields.map(|line| line.split('\t').nth(SESSION_FIELD));
        sessions.filter(|listed| *listed == Some(session)).count()
    }

    #[test]
    fn frames_in_flight_at_the_stop_write_are_frames_of_the_session() {
        let found = analysed("in-flight", &with_late(2));
        assert_eq!(found.value("session_frames"), "202");
        assert_eq!(found.value("session_frames_after_stop"), "2");
        let (_, rule) = found.rows("session_frames_after_stop")[0];
        for said in [
            "under 1000 ms after the mark",
            "up to the first that holds no whole frame",
            "does not count one on from the frame before",
            "none where the session holds no frame before the mark",
        ] {
            assert!(rule.contains(said), "{rule}");
        }
        for key in ["audio_outside_a_session", "frames_after_stop_outside"] {
            assert_eq!(found.value(key), "0", "{key}");
        }
        for key in ["frame_counter_skips", "packet_counter_skips"] {
            assert_eq!(found.value(key), "0", "{key}");
        }
        assert_eq!((listed_in(&found, "1"), listed_in(&found, "")), (202, 0));
        // The burst and the lead are read from the head of the session, which its tail does not move.
        let plain = analysed("plain", &with_late(0));
        assert_eq!(plain.value("session_frames"), "200");
        assert_eq!(plain.value("session_frames_after_stop"), "0");
        for key in ["lead_ms", "burst_packets", "burst_frames", "burst_ms"] {
            assert_eq!(found.value(key), plain.value(key), "{key}");
        }
    }

    #[test]
    fn a_late_frame_whose_counter_does_not_go_on_stays_outside_with_all_after_it() {
        let mut session = with_late(3);
        let late = late_audio(&session);
        // The second late packet repeats the counter of the first, and the third counts on from the second as sent.
        let repeated = u16::try_from(FRAMES).unwrap();
        session.records[late[1]].sdu = one_frame(repeated);
        let found = analysed("out-of-step", &session);
        assert_eq!(found.value("session_frames"), "201");
        assert_eq!(found.value("session_frames_after_stop"), "1");
        assert_eq!(found.value("audio_outside_a_session"), "2");
        assert_eq!(found.value("frames_after_stop_outside"), "2");
        assert_eq!(found.value("frame_counter_skips"), "0");
        assert_eq!((listed_in(&found, "1"), listed_in(&found, "")), (201, 2));

        // A notification with no frame counts nothing on, so the frame after it stays outside too.
        let mut hollow = with_late(2);
        let late = late_audio(&hollow);
        hollow.records[late[0]].sdu = encode_audio(&Handles::OBSERVED, 0, &[]).unwrap();
        hollow.records[late[1]].sdu = one_frame(repeated);
        let found = analysed("hollow", &hollow);
        assert_eq!(found.value("session_frames"), "200");
        assert_eq!(found.value("audio_outside_a_session"), "2");
        assert_eq!(found.value("frames_after_stop_outside"), "1");

        // A session that holds no frame before its stop write has no count to go on from.
        let mut empty = with_late(0);
        let stop_ns = empty.stop_ns;
        empty.records.retain(|record| {
            !matches!(
                DictationPacket::parse(&record.sdu, &Handles::OBSERVED),
                Ok(Some(DictationPacket::Audio(_)))
            )
        });
        let at_ns = stop_ns + NS_PER_MS;
        let (direction, sdu) = (Direction::FromPods, one_frame(0));
        empty.records.push(Record {
            at_ns,
            direction,
            sdu,
        });
        empty.records.sort_by_key(|record| record.at_ns);
        let found = analysed("empty", &empty);
        assert_eq!(found.value("session_frames"), "0");
        assert_eq!(found.value("frames_after_stop_outside"), "1");
    }

    #[test]
    fn the_tail_ends_one_second_after_the_stop_mark_to_the_nanosecond() {
        let mut session = with_late(2);
        let late = late_audio(&session);
        let end_ns = session.stop_ns + TAIL_NS;
        (
            session.records[late[0]].at_ns,
            session.records[late[1]].at_ns,
        ) = (end_ns - 1, end_ns);
        session.records.sort_by_key(|record| record.at_ns);
        let found = analysed("at-the-end", &session);
        assert_eq!(found.value("session_frames"), "201");
        assert_eq!(found.value("session_frames_after_stop"), "1");
        assert_eq!(found.value("frames_after_stop_outside"), "1");
        assert_eq!(found.value("audio_outside_a_session"), "1");
    }

    #[test]
    fn the_tail_ends_at_the_next_start_mark_though_the_count_goes_on_past_it() {
        let first = with_late(2);
        // The two late packets come one and three times `STOP_AFTER_MS` after the stop write, and the next start write falls between them.
        let second = synth(Plan {
            first_packet: first.next_packet,
            ..Plan::seen(first.stop_ns + 2 * STOP_AFTER_MS * NS_PER_MS, FRAMES)
        });
        let home = tempfile::tempdir().unwrap();
        let sessions = [first, second];
        let mut records: Vec<Record> = sessions.iter().flat_map(|s| s.records.clone()).collect();
        records.sort_by_key(|record| record.at_ns);
        let found = analyse(&home, "two", &log_text(&records), &marks_of(&sessions));
        // The second late packet counts on from the first within the tail, and still it is in the window of the second session.
        let frames: Vec<&str> = found.rows("session_frames").iter().map(|r| r.0).collect();
        assert_eq!(frames, ["201", "201"]);
        let tails = found.rows("session_frames_after_stop");
        assert_eq!([tails[0].0, tails[1].0], ["1", "0"]);
        for key in ["audio_outside_a_session", "frames_after_stop_outside"] {
            assert_eq!(found.value(key), "0", "{key}");
        }
        let listed = [listed_in(&found, "1"), listed_in(&found, "2")];
        assert_eq!(listed, [201, 201]);
    }
}
