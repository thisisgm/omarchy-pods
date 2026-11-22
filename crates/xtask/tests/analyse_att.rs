//! The built binary's `probe analyse` command on generated runs that hold a log of the attribute channel.

#[cfg(test)]
#[path = "support/att.rs"]
mod att_run;
#[cfg(test)]
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
    use pods_proto::dictation::DictationPacket;
    use pods_proto::sdulog::{Direction, Record};
    use tempfile::TempDir;

    use super::att_run::{Analysed, FRAMES_HEADER, START_NS, analyse, marks_of};
    use super::att_synth::{
        AUDIO_AFTER_MS, FLAGGED, FRAME_BYTES, NS_PER_MS, NS_PER_SECOND, Plan, REPLY_AFTER_MS,
        SAMPLES_PER_FRAME, STOP_AFTER_MS, STOP_REPLY_AFTER_MS, Session, synth,
    };
    use super::support::{log_text, names_in, row};

    /// The frames of the three sessions a Mac was seen to record, 2963 in all.
    const SEEN_FRAMES: [u32; 3] = [987, 988, 988];
    /// The idle time between two generated sessions.
    const BETWEEN_NS: u64 = 3 * NS_PER_SECOND;
    /// Two minutes of 20 ms frames.
    const TWO_MINUTES_FRAMES: u32 = 6000;
    /// The sample clock the rows set the measured one against.
    const REFERENCE_HZ: f64 = 16_000.0;
    /// Four frames a second.
    const FOUR_A_SECOND_HZ: u32 = 1280;
    const BITS_PER_BYTE: f64 = 8.0;
    /// How close the mean bitrate must come to the bits of the median second.
    const MEAN_NEAR_MEDIAN: f64 = 500.0;
    /// Parts in one.
    const MILLION: f64 = 1e6;
    /// How close the reported clock must come to the generated one, in parts per million.
    const WITHIN_PPM: f64 = 20.0;
    /// How close the clock of a session with a burst must come to the clock of the same session without one.
    const SAME_PPM: f64 = 2.0;
    /// The rows a session too thin for a rate refuses.
    const RATE_KEYS: [&str; 9] = [
        "frame_rate",
        "clock_hz",
        "clock_hz_low",
        "clock_hz_high",
        "clock_ppm_vs_16000",
        "bitrate_mean",
        "bitrate_second_min",
        "bitrate_second_median",
        "bitrate_second_max",
    ];
    /// The rows of the lead and the burst.
    const LEAD_KEYS: [&str; 4] = ["lead_ms", "burst_packets", "burst_frames", "burst_ms"];

    fn analyse_sessions(home: &TempDir, name: &str, sessions: &[Session]) -> Analysed {
        let records: Vec<Record> = sessions
            .iter()
            .flat_map(|session| session.records.clone())
            .collect();
        analyse(home, name, &log_text(&records), &marks_of(sessions))
    }

    fn analyse_plan(home: &TempDir, name: &str, plan: Plan) -> Analysed {
        analyse_sessions(home, name, &[synth(plan)])
    }

    /// Every frame of every audio notification the pods sent in `records`, as its flag, level, counter and bytes.
    fn frames_of(records: &[Record]) -> Vec<(u8, u8, u16, Vec<u8>)> {
        let mut frames = Vec::new();
        for record in records {
            if let Ok(Some(DictationPacket::Audio(audio))) = DictationPacket::parse(&record.sdu) {
                frames.extend(
                    audio
                        .frames()
                        .map(|frame| (frame.flag, frame.level, frame.counter, frame.data.to_vec())),
                );
            }
        }
        frames
    }

    /// The place in `records` of the audio notification that holds the frame with this counter.
    fn packet_with(records: &[Record], counter: u16) -> usize {
        let holds = |record: &Record| {
            frames_of(std::slice::from_ref(record))
                .iter()
                .any(|frame| frame.2 == counter)
        };
        records.iter().position(holds).unwrap()
    }

    /// Three sessions in a row, each continuing the packet counter of the one before.
    fn seen_sessions() -> Vec<Session> {
        let mut sessions: Vec<Session> = Vec::new();
        let mut start_ns = START_NS;
        let mut first_packet = 0;
        for frames in SEEN_FRAMES {
            let plan = Plan {
                first_packet,
                ..Plan::seen(start_ns, frames)
            };
            let session = synth(plan);
            start_ns = session
                .records
                .last()
                .unwrap()
                .at_ns
                .saturating_add(BETWEEN_NS);
            first_packet = session.next_packet;
            sessions.push(session);
        }
        sessions
    }

    #[test]
    fn layout_counts_match_the_generator() {
        let home = tempfile::tempdir().unwrap();
        let sessions = seen_sessions();
        let found = analyse_sessions(&home, "seen", &sessions);
        let records: Vec<Record> = sessions.iter().flat_map(|s| s.records.clone()).collect();
        let frames = frames_of(&records);
        let from_pods = records
            .iter()
            .filter(|r| r.direction == Direction::FromPods);
        let largest = from_pods.clone().map(|r| r.sdu.len()).max().unwrap();

        assert_eq!(found.value("audio_notifications"), "2813");
        assert_eq!(found.value("audio_frames"), "2963");
        assert_eq!(frames.len(), 2963);
        for key in [
            "att_log_cut_lines",
            "parse_failures",
            "length_mismatches",
            "trailing_bytes",
            "packet_counter_skips",
            "frame_counter_skips",
            "unexpected_codec",
            "audio_outside_a_session",
            "frames_other_flag",
            "requests_from_pods",
        ] {
            assert_eq!(found.value(key), "0", "{key}");
        }
        assert_eq!(found.value("largest_pdu"), largest.to_string());
        assert_eq!(
            found.value("att_packets_from_pods"),
            from_pods.count().to_string()
        );
        assert_eq!(found.value("att_packets_to_pods"), "6");
        assert_eq!(found.value("frames_per_packet"), "1:2774 4:6 5:33");
        let flagged = frames.iter().filter(|frame| frame.0 == FLAGGED).count();
        assert!(flagged > 1000 && flagged < 2000, "{flagged}");
        assert_eq!(found.value("frames_flagged"), flagged.to_string());
        assert_eq!(
            found.value("frames_unflagged"),
            (2963 - flagged).to_string()
        );
        let levels = || frames.iter().map(|frame| frame.1);
        assert_eq!(
            found.value("level_min"),
            levels().min().unwrap().to_string()
        );
        assert_eq!(
            found.value("level_max"),
            levels().max().unwrap().to_string()
        );
        let per_session: Vec<&str> = found.rows("session_frames").iter().map(|r| r.0).collect();
        assert_eq!(per_session, ["987", "988", "988"]);

        let mut lines = found.frames.lines();
        assert_eq!(lines.next(), Some(FRAMES_HEADER));
        let rows: Vec<Vec<&str>> = lines.map(|line| line.split('\t').collect()).collect();
        assert_eq!(rows.len(), 2963);
        for ((index, row), frame) in rows.iter().enumerate().zip(&frames) {
            let (flag, level, counter, data) = frame;
            let line: usize = row[1].parse().unwrap();
            let record = &records[line - 1];
            assert!(frames_of(std::slice::from_ref(record)).contains(frame));
            let expected = [
                index.to_string(),
                line.to_string(),
                record.at_ns.to_string(),
                row[3].to_owned(),
                counter.to_string(),
                format!("{flag:#04x}"),
                level.to_string(),
                data.len().to_string(),
                "0xb8".to_owned(),
            ];
            assert_eq!(row[..], expected[..]);
        }
        let in_session = |session: &str| rows.iter().filter(|row| row[3] == session).count();
        assert_eq!(
            [in_session("1"), in_session("2"), in_session("3")],
            [987, 988, 988]
        );
        assert_eq!(
            names_in(&found.run),
            [
                "analysis-att.tsv",
                "analysis.tsv",
                "att-handles.tsv",
                "att.log",
                "frames.tsv",
                "marks.tsv",
                "meta.txt",
                "sdu.log",
                "units.tsv"
            ]
        );
    }

    #[test]
    fn a_withheld_frame_is_one_frame_counter_skip_and_no_parse_failure() {
        let home = tempfile::tempdir().unwrap();
        let whole = analyse_plan(&home, "whole", Plan::seen(START_NS, 987));
        assert_eq!(whole.value("frame_counter_skips"), "0");
        assert_eq!(whole.rows("frame_counter_skip"), []);
        assert_eq!(whole.value("audio_frames"), "987");

        let plan = Plan {
            withheld: Some(500),
            ..Plan::seen(START_NS, 987)
        };
        let withheld = analyse_plan(&home, "withheld", plan);
        assert_eq!(withheld.value("frame_counter_skips"), "1");
        assert_eq!(
            withheld.rows("frame_counter_skip"),
            [("499 to 501", "session 1")]
        );
        assert_eq!(withheld.value("packet_counter_skips"), "0");
        assert_eq!(withheld.value("parse_failures"), "0");
        assert_eq!(withheld.value("audio_frames"), "986");
        let (_, note) = row(&withheld.table, "lead_ms");
        assert!(note.split("; ").any(|item| item == "skip"), "{note}");
        let (_, note) = row(&whole.table, "lead_ms");
        assert!(!note.contains("skip"), "{note}");
    }

    #[test]
    fn a_withheld_packet_is_one_packet_counter_skip() {
        let home = tempfile::tempdir().unwrap();
        let mut session = synth(Plan::seen(START_NS, 987));
        let lost = packet_with(&session.records, 500);
        let before = frames_of(&session.records[..lost]).len();
        assert_eq!(before, 500);
        session.records.remove(lost);
        let found = analyse_sessions(&home, "lost", &[session]);
        assert_eq!(found.value("packet_counter_skips"), "1");
        // The burst of 63 frames took 13 packets, so frame 500 rode in packet 450, whose counter is 450 less 256.
        assert_eq!(
            found.rows("packet_counter_skip"),
            [("193 to 195", "session 1")]
        );
        assert_eq!(found.value("frame_counter_skips"), "1");
        assert_eq!(
            found.rows("frame_counter_skip"),
            [("499 to 501", "session 1")]
        );
    }

    #[test]
    fn skips_past_the_twentieth_of_a_counter_are_counted_in_one_row() {
        let home = tempfile::tempdir().unwrap();
        for (lost, listed, unlisted) in [(20, 20, None), (25, 20, Some("5"))] {
            let mut session = synth(Plan::seen(START_NS, 987));
            // Every twentieth frame from frame 100 on is lost with the packet it rode in alone.
            for counter in (0..lost).map(|k| 100 + 20 * k) {
                let packet = packet_with(&session.records, counter);
                session.records.remove(packet);
            }
            let found = analyse_sessions(&home, &format!("lost-{lost}"), &[session]);
            for counter in ["packet_counter_skip", "frame_counter_skip"] {
                let total = format!("{counter}s");
                assert_eq!(found.value(&total), lost.to_string(), "{counter}");
                assert_eq!(found.rows(counter).len(), listed, "{counter}");
                let rest: Vec<&str> = found
                    .rows(&format!("{total}_unlisted"))
                    .iter()
                    .map(|row| row.0)
                    .collect();
                assert_eq!(rest, Vec::from_iter(unlisted), "{counter}");
            }
        }
    }

    #[test]
    fn the_packet_counter_wrap_at_256_is_not_a_skip() {
        let home = tempfile::tempdir().unwrap();
        let plan = Plan {
            first_packet: 250,
            ..Plan::seen(START_NS, 987)
        };
        let session = synth(plan);
        let counters: Vec<u8> = session
            .records
            .iter()
            .filter_map(|record| match DictationPacket::parse(&record.sdu) {
                Ok(Some(DictationPacket::Audio(audio))) => Some(audio.packet_counter),
                _ => None,
            })
            .collect();
        assert!(counters.windows(2).any(|pair| pair == [255, 0]));
        let found = analyse_sessions(&home, "wrap", &[session]);
        assert_eq!(found.value("packet_counter_skips"), "0");
        assert_eq!(found.value("frame_counter_skips"), "0");
        assert_eq!(
            found.value("audio_notifications"),
            counters.len().to_string()
        );
    }

    #[test]
    fn a_new_session_restarting_at_zero_is_not_a_skip() {
        let home = tempfile::tempdir().unwrap();
        let first = synth(Plan::seen(START_NS, 300));
        let second_start_ns = first.records.last().unwrap().at_ns + BETWEEN_NS;
        // The second session takes up a packet counter far from where the first one ended.
        let plan = Plan {
            first_packet: first.next_packet.wrapping_add(100),
            ..Plan::seen(second_start_ns, 300)
        };
        let found = analyse_sessions(&home, "two", &[first, synth(plan)]);
        assert_eq!(found.value("frame_counter_skips"), "0");
        assert_eq!(found.value("packet_counter_skips"), "0");
        assert_eq!(found.rows("session_frames").len(), 2);
    }

    #[test]
    fn frame_clock_is_within_20_ppm_on_a_clean_two_minute_log() {
        let home = tempfile::tempdir().unwrap();
        for (name, clock_hz) in [
            ("at-16000", 16_000_u32),
            ("at-16004", 16_004),
            ("at-15990", 15_990),
        ] {
            let plan = Plan {
                clock_hz,
                jitter_ms: 2,
                ..Plan::seen(START_NS, TWO_MINUTES_FRAMES)
            };
            let found = analyse_plan(&home, name, plan);
            let truth = f64::from(clock_hz);
            let apart_ppm = |hz: f64| ((hz / truth - 1.0) * MILLION).abs();
            let hz = found.number("clock_hz");
            assert!(apart_ppm(hz) < WITHIN_PPM, "{name}: {hz}");
            let from_frames = found.number("frame_rate") * f64::from(SAMPLES_PER_FRAME);
            assert!(apart_ppm(from_frames) < WITHIN_PPM, "{name}");
            let (low, high) = (found.number("clock_hz_low"), found.number("clock_hz_high"));
            assert!(low <= hz && hz <= high, "{name}: {low} {high}");
            assert!(
                apart_ppm(low) < WITHIN_PPM && apart_ppm(high) < WITHIN_PPM,
                "{name}: {low} {high}"
            );
            let expected_ppm = (truth / REFERENCE_HZ - 1.0) * MILLION;
            let ppm = found.number("clock_ppm_vs_16000");
            assert!((ppm - expected_ppm).abs() < WITHIN_PPM, "{name}: {ppm}");
            let (_, note) = row(&found.table, "clock_hz");
            let scaled = format!("session 1; slope x {SAMPLES_PER_FRAME} samples per frame; ");
            assert!(note.starts_with(&scaled), "{note}");
            let bitrate = found.number("bitrate_mean");
            let (least, most) = (
                found.number("bitrate_second_min"),
                found.number("bitrate_second_max"),
            );
            let median = found.number("bitrate_second_median");
            // A whole second holds the frames of one second of the clock, one more or less, each of a length the generator picks.
            let frames = truth / f64::from(SAMPLES_PER_FRAME);
            let bits = |frames: f64, bytes: u64| {
                frames * f64::from(u32::try_from(bytes).unwrap()) * BITS_PER_BYTE
            };
            let (floor, ceiling) = (
                bits(frames - 1.0, FRAME_BYTES.0),
                bits(frames + 1.0, FRAME_BYTES.1),
            );
            assert!(least >= floor && most <= ceiling, "{name}: {least} {most}");
            assert!(least <= median && median <= most, "{name}: {median}");
            assert!(
                (bitrate - median).abs() < MEAN_NEAR_MEDIAN,
                "{name}: {bitrate} {median}"
            );
        }
    }

    #[test]
    fn the_clock_ignores_the_opening_burst() {
        let home = tempfile::tempdir().unwrap();
        let plan = Plan::seen(START_NS, TWO_MINUTES_FRAMES);
        let with = analyse_plan(&home, "with-burst", plan);
        let without = analyse_plan(&home, "without-burst", plan.without_burst());
        assert_ne!(with.value("burst_packets"), "0");
        assert_eq!(without.value("burst_packets"), "0");
        let (with, without) = (with.number("frame_rate"), without.number("frame_rate"));
        let apart_ppm = ((with / without - 1.0) * MILLION).abs();
        assert!(apart_ppm < SAME_PPM, "{with} {without}");
    }

    #[test]
    fn short_session_is_refused() {
        /// One frame every 125 ms: slow enough that 500 packets span over a minute.
        const SLOW_CLOCK_HZ: u32 = 2560;
        let home = tempfile::tempdir().unwrap();
        let steady = Plan::seen(START_NS, 0).without_burst();
        let slow = Plan {
            clock_hz: SLOW_CLOCK_HZ,
            ..steady
        };
        let short = "3000 packets after the burst over 59.980 s; needs 500 over 60 s";
        for (name, plan, thin) in [
            // The burst delivers 63 frames; with its packets counted, a minute would be reached 63 frames early.
            ("burst-span-short", Plan::seen(START_NS, 3063), Some(short)),
            ("burst-span-enough", Plan::seen(START_NS, 3064), None),
            (
                "span-short",
                Plan {
                    frames: 3000,
                    ..steady
                },
                Some(short),
            ),
            (
                "span-enough",
                Plan {
                    frames: 3001,
                    ..steady
                },
                None,
            ),
            (
                "packets-short",
                Plan {
                    frames: 499,
                    ..slow
                },
                Some("499 packets after the burst over 62.250 s; needs 500 over 60 s"),
            ),
            (
                "packets-enough",
                Plan {
                    frames: 500,
                    ..slow
                },
                None,
            ),
        ] {
            let found = analyse_plan(&home, name, plan);
            for key in RATE_KEYS {
                let (value, note) = row(&found.table, key);
                match thin {
                    Some(thin) => {
                        assert_eq!(value, "refused", "{name} {key}");
                        assert_eq!(note, format!("session 1; {thin}"), "{name} {key}");
                    }
                    None => assert!(value.parse::<f64>().is_ok(), "{name} {key}: {value}"),
                }
            }
            // What needs no long span is there whatever the rates say.
            assert_eq!(
                found.value("audio_frames"),
                plan.frames.to_string(),
                "{name}"
            );
            assert_eq!(
                found.value("session_frames"),
                plan.frames.to_string(),
                "{name}"
            );
            let frames = usize::try_from(plan.frames).unwrap();
            assert_eq!(found.frames.lines().count(), frames + 1, "{name}");
            for key in LEAD_KEYS {
                assert!(found.number(key).is_finite(), "{name} {key}");
            }
            let reply = format!("{REPLY_AFTER_MS}.000");
            assert_eq!(found.value("start_to_reply_ms"), reply, "{name}");
        }
    }

    #[test]
    fn a_session_of_a_few_packets_refuses_its_lead_and_keeps_its_frames() {
        let home = tempfile::tempdir().unwrap();
        let found = analyse_plan(&home, "few", Plan::seen(START_NS, 99));
        let packets = found.value("session_audio_notifications");
        assert_eq!(packets, "49");
        let no_line = "session 1; fewer than 50 packets with a frame in the first 10 s";
        for key in LEAD_KEYS {
            assert_eq!(row(&found.table, key), ("refused", no_line), "{key}");
        }
        let unknown = format!("{no_line}, so the burst is not known");
        for key in RATE_KEYS {
            assert_eq!(
                row(&found.table, key),
                ("refused", unknown.as_str()),
                "{key}"
            );
        }
        assert_eq!(found.value("audio_frames"), "99");
        assert_eq!(found.frames.lines().count(), 100);

        let enough = analyse_plan(&home, "fifty", Plan::seen(START_NS, 100));
        assert_eq!(enough.value("session_audio_notifications"), "50");
        for key in LEAD_KEYS {
            assert!(enough.number(key).is_finite(), "{key}");
        }

        // Four frames a second: packets enough for a rate over 150 s, and too few in the first 10 s to find the burst.
        let slow = Plan {
            clock_hz: FOUR_A_SECOND_HZ,
            ..Plan::seen(START_NS, 600).without_burst()
        };
        let slow = analyse_plan(&home, "slow", slow);
        assert_eq!(slow.value("session_audio_notifications"), "600");
        for key in RATE_KEYS {
            assert_eq!(
                row(&slow.table, key),
                ("refused", unknown.as_str()),
                "{key}"
            );
        }
    }

    #[test]
    fn lead_and_burst_match_the_planted_values() {
        let home = tempfile::tempdir().unwrap();
        let found = analyse_plan(&home, "planted", Plan::seen(START_NS, 987));
        assert!((found.number("lead_ms") - 930.0).abs() < 1.0);
        assert_eq!(found.value("burst_packets"), "13");
        assert_eq!(found.value("burst_frames"), "63");
        let burst_ms = found.number("burst_ms");
        assert!((burst_ms - 250.0).abs() < 0.001, "{burst_ms}");
        let (_, note) = row(&found.table, "lead_ms");
        assert!(note.contains("20 ms a frame"), "{note}");
        let formula =
            "of the arrival of a packet minus 20 ms for every frame delivered by the end of it";
        assert!(note.contains(formula), "{note}");

        let steady = analyse_plan(&home, "no-burst", Plan::seen(START_NS, 987).without_burst());
        assert!((steady.number("lead_ms") - 0.0).abs() < 1.0);
        assert_eq!(steady.value("burst_frames"), "0");
    }

    #[test]
    fn the_burst_is_the_thirteen_packets_however_fast_the_seen_sessions_drained() {
        let home = tempfile::tempdir().unwrap();
        for (burst_ms, jitter_ms) in [(238, 0), (238, 19), (257, 0), (257, 19)] {
            let plan = Plan {
                burst_ms,
                jitter_ms,
                ..Plan::seen(START_NS, 987)
            };
            let name = format!("drained-{burst_ms}-{jitter_ms}");
            let found = analyse_plan(&home, &name, plan);
            assert_eq!(found.value("burst_packets"), "13", "{name}");
            assert_eq!(found.value("burst_frames"), "63", "{name}");
        }
    }

    #[test]
    fn a_frame_that_is_not_b8_is_counted_and_named() {
        let home = tempfile::tempdir().unwrap();
        let plan = Plan {
            odd_codec: Some((70, 0x78)),
            ..Plan::seen(START_NS, 200)
        };
        let found = analyse_plan(&home, "odd", plan);
        let (count, note) = row(&found.table, "unexpected_codec");
        assert_eq!(count, "1");
        assert!(note.ends_with("by first byte 0x78:1"), "{note}");
        let odd: Vec<&str> = found
            .frames
            .lines()
            .filter(|line| !line.ends_with("\t0xb8"))
            .collect();
        assert_eq!(odd.len(), 2, "{odd:?}");
        let fields: Vec<&str> = odd[1].split('\t').collect();
        let named = FRAMES_HEADER.split('\t').zip(fields);
        let read: Vec<(&str, &str)> = named
            .filter(|(column, _)| ["counter", "toc"].contains(column))
            .collect();
        assert_eq!(read, [("counter", "70"), ("toc", "0x78")]);
        assert_eq!(found.value("audio_frames"), "200");
    }

    #[test]
    fn reply_and_first_audio_delays_are_measured_from_the_start_mark() {
        let home = tempfile::tempdir().unwrap();
        let found = analyse_sessions(&home, "delays", &seen_sessions());
        let reply = format!("{REPLY_AFTER_MS}.000");
        let audio = format!("{AUDIO_AFTER_MS}.000");
        let stop_reply = format!("{STOP_REPLY_AFTER_MS}.000");
        for (key, expected) in [
            ("start_to_reply_ms", &reply),
            ("start_to_audio_ms", &audio),
            ("stop_to_reply_ms", &stop_reply),
        ] {
            let values: Vec<&str> = found.rows(key).iter().map(|row| row.0).collect();
            assert_eq!(values, [expected.as_str(); 3], "{key}");
        }
        let notes: Vec<&str> = found
            .rows("start_to_reply_ms")
            .iter()
            .map(|r| r.1)
            .collect();
        assert!(notes[1].starts_with("session 2; "), "{notes:?}");
    }

    #[test]
    fn a_session_with_no_reply_no_stop_or_no_audio_refuses_those_delays() {
        let home = tempfile::tempdir().unwrap();
        let mut session = synth(Plan::seen(START_NS, 200));
        session
            .records
            .retain(|record| record.sdu != pods_proto::dictation::STARTED);
        session
            .records
            .retain(|record| record.sdu != pods_proto::dictation::STOPPED);
        let silent = analyse_sessions(&home, "silent", std::slice::from_ref(&session));
        let no_start_reply = "session 1; no reply to the start write in the session";
        assert_eq!(
            row(&silent.table, "start_to_reply_ms"),
            ("refused", no_start_reply)
        );
        let no_stop_reply = "session 1; no reply to the stop write before the next session";
        assert_eq!(
            row(&silent.table, "stop_to_reply_ms"),
            ("refused", no_stop_reply)
        );
        assert_eq!(
            row(&silent.table, "audio_after_stop_reply"),
            ("refused", no_stop_reply)
        );
        let audio = format!("{AUDIO_AFTER_MS}.000");
        assert_eq!(silent.value("start_to_audio_ms"), audio);

        let start_ns = session.start_ns;
        let marks = format!("{start_ns}\tdictation-start\tdictation start\n");
        let open = analyse(&home, "open", &log_text(&session.records), &marks);
        let no_stop = "session 1; no dictation-stop mark ends this session";
        assert_eq!(row(&open.table, "stop_to_reply_ms"), ("refused", no_stop));
        assert_eq!(
            row(&open.table, "audio_after_stop_reply"),
            ("refused", no_stop)
        );
        assert_eq!(open.value("audio_outside_a_session"), "0");

        let unmarked = analyse(&home, "unmarked", &log_text(&session.records), "");
        assert_eq!(unmarked.value("audio_outside_a_session"), "150");
        assert_eq!(unmarked.rows("session_frames"), []);
        assert_eq!(unmarked.value("audio_frames"), "200");
        let no_session = unmarked
            .frames
            .lines()
            .skip(1)
            .all(|line| line.contains("\t\t"));
        assert!(no_session, "{}", unmarked.frames);

        let mut mute = synth(Plan::seen(START_NS, 200));
        mute.records.retain(|record| {
            let read = DictationPacket::parse(&record.sdu);
            !matches!(read, Ok(Some(DictationPacket::Audio(_))))
        });
        let mute = analyse_sessions(&home, "mute", &[mute]);
        let no_audio = "session 1; no audio notification in the session";
        assert_eq!(row(&mute.table, "start_to_audio_ms"), ("refused", no_audio));
        let none = ("refused", "no audio notification");
        assert_eq!(row(&mute.table, "frames_per_packet"), none);
        let reply = format!("{REPLY_AFTER_MS}.000");
        assert_eq!(mute.value("start_to_reply_ms"), reply);
    }

    #[test]
    fn audio_after_the_stop_reply_is_counted() {
        let home = tempfile::tempdir().unwrap();
        let plan = Plan {
            late: (2, 3),
            ..Plan::seen(START_NS, 200)
        };
        let found = analyse_plan(&home, "late", plan);
        assert_eq!(found.value("audio_after_stop_reply"), "3");
        // All five count on from the last frame within the tail, so the session takes them, the three after the reply too.
        assert_eq!(found.value("audio_outside_a_session"), "0");
        assert_eq!(found.value("session_frames"), "205");
        assert_eq!(found.value("session_frames_after_stop"), "5");
        assert_eq!(found.value("frames_after_stop_outside"), "0");
        assert_eq!(found.value("audio_frames"), "205");
        let frame_ns = u64::from(SAMPLES_PER_FRAME) * NS_PER_SECOND / u64::from(plan.clock_hz);
        // The second late packet ends two frames after the last of the session, which the stop write follows by `STOP_AFTER_MS`.
        let reply_ms = 2 * frame_ns / NS_PER_MS - STOP_AFTER_MS + STOP_REPLY_AFTER_MS;
        let reply = format!("{reply_ms}.000");
        assert_eq!(found.value("stop_to_reply_ms"), reply);
        assert_eq!(found.value("frame_counter_skips"), "0");

        // The session now runs on past the stop reply, which is no reply to its start write.
        let mut unanswered = synth(plan);
        let started = pods_proto::dictation::STARTED;
        unanswered.records.retain(|record| record.sdu != started);
        let unanswered = analyse_sessions(&home, "unanswered", &[unanswered]);
        assert_eq!(unanswered.value("session_frames_after_stop"), "5");
        assert_eq!(unanswered.value("start_to_reply_ms"), "refused");
    }

    #[test]
    fn marks_of_the_uplink_do_not_cut_a_dictation_session() {
        let home = tempfile::tempdir().unwrap();
        let session = synth(Plan::seen(START_NS, 200));
        let (start_ns, stop_ns) = (session.start_ns, session.stop_ns);
        let (before_ns, inside_ns) = (start_ns - NS_PER_SECOND, start_ns + NS_PER_SECOND);
        let again_ns = inside_ns + NS_PER_SECOND;
        // An uplink that starts before the session, stops inside it and starts again before it ends.
        let marks = [
            format!("{before_ns}\tstart\t\n{start_ns}\tdictation-start\t\n"),
            format!("{inside_ns}\tstop\t\n{again_ns}\tstart\t\n"),
            format!("{stop_ns}\tdictation-stop\t\n"),
        ]
        .concat();
        let found = analyse(&home, "both", &log_text(&session.records), &marks);
        assert_eq!(found.value("session_frames"), "200");
        assert_eq!(found.value("audio_outside_a_session"), "0");
        let stop_reply = format!("{STOP_REPLY_AFTER_MS}.000");
        assert_eq!(found.value("stop_to_reply_ms"), stop_reply);
        let uplink = std::fs::read_to_string(found.run.join("analysis.tsv")).unwrap();
        let starts = uplink
            .lines()
            .filter(|line| line.starts_with("segment_audio_sdus\t"));
        assert_eq!(starts.count(), 2, "{uplink}");
    }
}
