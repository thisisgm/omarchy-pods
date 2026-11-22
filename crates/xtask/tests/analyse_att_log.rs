//! The built binary's `probe analyse` command on runs whose log of the attribute channel is no clean session: a walk of the table, requests, packets no parser reads, garbage, or no log at all.

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
    use std::fs;

    use pods_proto::att;
    use pods_proto::dictation::{Frame, encode_audio};
    use pods_proto::sdulog::{Direction, format_line};

    use super::att_run::{CONTROL_LOG, FRAMES_HEADER, analyse, marks_of};
    use super::att_synth::{NS_PER_SECOND, Plan, synth};
    use super::support::{log_text, make_run_of, names_in, probe, row};

    /// The status the command exits with when it refuses; a panic would exit with 101.
    const REFUSED: i32 = 1;
    /// Where the session of a run here starts.
    const START_NS: u64 = 2 * NS_PER_SECOND;

    /// One line of a log: `7 < 1b0d00`, a stamp, the direction and the packet.
    fn line(at_ns: u64, direction: Direction, pdu: &[u8]) -> String {
        format_line(at_ns, direction, pdu).unwrap() + "\n"
    }

    #[test]
    fn handles_tsv_lists_the_walk_and_the_properties() {
        /// Stands in for a vendor's 128 bit identifier; no real one is a run of counting bytes.
        const LONG: [u8; 16] = [
            0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xfb, 0xfc, 0xfd,
            0xfe, 0xff,
        ];
        let home = tempfile::tempdir().unwrap();
        let (to, from) = (Direction::ToPods, Direction::FromPods);
        let services = att::read_by_type_request(1, 0xffff, 0x2800);
        let declarations = att::read_by_type_request(1, 0xffff, att::CHARACTERISTIC_DECLARATION);
        let wide = [&[0x05, 0x02, 0x04, 0x00][..], &LONG].concat();
        let log = [
            line(1, to, &att::find_information_request(1, 0xffff)),
            line(
                2,
                from,
                &[
                    0x05, 0x01, 1, 0, 0x00, 0x28, 2, 0, 0x03, 0x28, 3, 0, 0x2b, 0x2a,
                ],
            ),
            line(3, to, &att::find_information_request(4, 0xffff)),
            line(4, from, &wide),
            line(5, to, &att::find_information_request(5, 0xffff)),
            line(
                6,
                from,
                &att::error_response(0x04, 5, att::ATTRIBUTE_NOT_FOUND),
            ),
            // The answer to a read of another type has the shape of a declaration of handle 6 and is none.
            line(7, to, &services),
            line(8, from, &[0x09, 0x07, 1, 0, 0x02, 6, 0, 0x99, 0x99]),
            line(9, to, &declarations),
            line(
                10,
                from,
                &[
                    0x09, 0x07, 2, 0, 0x10, 3, 0, 0x2b, 0x2a, 8, 0, 0x04, 9, 0, 0x2c, 0x2a,
                ],
            ),
        ]
        .concat();
        let found = analyse(&home, "walked", &log, "");
        let handles = fs::read_to_string(found.run.join("att-handles.tsv")).unwrap();
        // The wire holds the least significant octet of a type first, and both widths are written the other way round.
        let uuid = "fffefdfcfbfaf9f8f7f6f5f4f3f2f1f0";
        let expected = format!(
            "handle\tuuid\tproperties\n0x0001\t0x2800\t\n0x0002\t0x2803\t\n0x0003\t0x2a2b\t0x10\n0x0004\t{uuid}\t\n0x0009\t0x2a2c\t0x04\n"
        );
        assert_eq!(handles, expected);
        assert_eq!(found.value("handles_listed"), "5");
        assert_eq!(found.value("audio_notifications"), "0");
        assert_eq!(found.frames, format!("{FRAMES_HEADER}\n"));
    }

    #[test]
    fn a_request_from_the_pods_is_listed_by_opcode() {
        let home = tempfile::tempdir().unwrap();
        let from = Direction::FromPods;
        let log = [
            line(1, from, &[0x02, 0x00, 0x02]),
            line(2, from, &[0x0a, 0x03, 0x00]),
            line(3, from, &[0x02, 0x00, 0x02]),
            line(4, Direction::ToPods, &[0x02, 0x00, 0x02]),
        ]
        .concat();
        let found = analyse(&home, "asked", &log, "");
        assert_eq!(found.value("requests_from_pods"), "3");
        assert_eq!(found.value("request_opcode_0x02"), "2");
        assert_eq!(found.value("request_opcode_0x0a"), "1");
        assert_eq!(found.value("att_packets_to_pods"), "1");
    }

    #[test]
    fn a_notification_the_parser_cannot_read_is_counted_and_keeps_no_frame() {
        let home = tempfile::tempdir().unwrap();
        let from = Direction::FromPods;
        let frame = Frame {
            flag: 0x40,
            level: 200,
            counter: 0,
            data: &[],
        };
        let whole = encode_audio(7, &[frame]).unwrap();
        let mut short_length = whole.clone();
        short_length[6] -= 1;
        let stray = [&whole[..], &[0xee, 0xee, 0xee]].concat();
        // Stamps a hundred apart, so no stamp is the number of its line.
        let log = [
            line(100, from, &[0x1b, 0x0d]),
            line(200, from, &[0x1b, 0x0d, 0x00, 0x01, 0x00]),
            line(300, from, &[0x1b, 0x0d, 0x00, 0x02, 0x00, 0x2a, 0x00, 0x00]),
            line(400, from, &short_length),
            line(500, from, &stray),
            "600 < 1b".to_owned(),
        ]
        .concat();
        let found = analyse(&home, "odd", &log, "");
        assert_eq!(found.value("att_log_cut_lines"), "1");
        assert_eq!(found.value("att_packets_from_pods"), "5");
        assert_eq!(found.value("parse_failures"), "3");
        assert_eq!(found.value("audio_notifications"), "2");
        assert_eq!(found.value("length_mismatches"), "2");
        assert_eq!(
            row(&found.table, "trailing_bytes"),
            ("3", "after the last whole frame; in 1 notifications")
        );
        assert_eq!(found.value("audio_frames"), "2");
        assert_eq!(found.value("frames_other_flag"), "2");
        let (count, note) = row(&found.table, "unexpected_codec");
        assert_eq!(count, "2");
        assert!(note.ends_with("by first byte empty:2"), "{note}");
        assert_eq!(found.value("largest_pdu"), stray.len().to_string());
        let rows: Vec<&str> = found.frames.lines().collect();
        let fourth = "0\t4\t400\t\t0\t0x40\t200\t0\t";
        let fifth = "1\t5\t500\t\t0\t0x40\t200\t0\t";
        assert_eq!(rows, [FRAMES_HEADER, fourth, fifth]);
    }

    #[test]
    fn garbage_log_is_refused_with_the_line_number() {
        let home = tempfile::tempdir().unwrap();
        let inputs = ["att.log", "marks.tsv", "meta.txt", "sdu.log"];
        for (name, att_log, said) in [
            (
                "garbage",
                &b"1 < 00\n2 < 01\n3 < 02:00:00:00:00:01\n"[..],
                "att.log line 3: ",
            ),
            (
                "backwards",
                b"5 < 00\n4 < 01\n",
                "att.log line 2: its stamp is earlier",
            ),
            // 0xff is in no UTF-8 text.
            (
                "not-text",
                b"1 < 00\n2 < \xff\n",
                "att.log line 2: it is not UTF-8 text",
            ),
        ] {
            let files = [("sdu.log", CONTROL_LOG), ("att.log", ""), ("marks.tsv", "")];
            let run = make_run_of(&home, name, &files);
            fs::write(run.join("att.log"), att_log).unwrap();
            let refused = probe(&home, "analyse", &run).output().unwrap();
            assert_eq!(refused.status.code(), Some(REFUSED), "{refused:?}");
            assert_eq!(refused.stdout, b"");
            let error = String::from_utf8(refused.stderr).unwrap();
            assert!(error.contains(said), "{error}");
            assert!(!error.contains("02:00"), "{error}");
            assert_eq!(names_in(&run), inputs, "{name}");
        }
    }

    #[test]
    fn a_run_without_att_log_writes_no_att_files() {
        let home = tempfile::tempdir().unwrap();
        let marks = marks_of(&[synth(Plan::seen(START_NS, 100))]);
        let run = make_run_of(
            &home,
            "uplink",
            &[("sdu.log", CONTROL_LOG), ("marks.tsv", &marks)],
        );
        let done = probe(&home, "analyse", &run).output().unwrap();
        assert!(done.status.success(), "{done:?}");
        let table = fs::read_to_string(run.join("analysis.tsv")).unwrap();
        assert_eq!(String::from_utf8(done.stdout).unwrap(), table);
        assert_eq!(
            names_in(&run),
            [
                "analysis.tsv",
                "marks.tsv",
                "meta.txt",
                "sdu.log",
                "units.tsv"
            ]
        );
    }

    #[test]
    fn a_run_is_refused_as_empty_only_when_neither_log_holds_a_packet() {
        let home = tempfile::tempdir().unwrap();
        let session = synth(Plan::seen(START_NS, 100));
        let att_log = log_text(&session.records);
        let marks = marks_of(std::slice::from_ref(&session));
        let files = [
            ("sdu.log", ""),
            ("att.log", att_log.as_str()),
            ("marks.tsv", &marks),
        ];
        let run = make_run_of(&home, "att-only", &files);
        let done = probe(&home, "analyse", &run).output().unwrap();
        assert!(done.status.success(), "{done:?}");
        let table = fs::read_to_string(run.join("analysis-att.tsv")).unwrap();
        assert_eq!(row(&table, "audio_frames").0, "100");
        let uplink = fs::read_to_string(run.join("analysis.tsv")).unwrap();
        assert_eq!(row(&uplink, "packets_from_pods").0, "0");

        let quiet = analyse(&home, "control-only", "", "");
        assert_eq!(quiet.value("att_packets_from_pods"), "0");
        let nothing = ("refused", "no packet from the pods");
        assert_eq!(row(&quiet.table, "largest_pdu"), nothing);
        assert_eq!(row(&quiet.table, "level_min"), ("refused", "no frame"));

        let files = [("sdu.log", ""), ("att.log", ""), ("marks.tsv", "")];
        let run = make_run_of(&home, "neither", &files);
        let refused = probe(&home, "analyse", &run).output().unwrap();
        assert_eq!(refused.status.code(), Some(REFUSED), "{refused:?}");
        assert_eq!(refused.stdout, b"");
        let error = String::from_utf8(refused.stderr).unwrap();
        assert!(
            error.contains("sdu.log and att.log hold no packet"),
            "{error}"
        );
        assert_eq!(
            names_in(&run),
            ["att.log", "marks.tsv", "meta.txt", "sdu.log"]
        );
    }
}
