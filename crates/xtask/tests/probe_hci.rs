//! The built binary's `probe hci` command, run on synthetic traces with `HOME` set to a temporary directory.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[path = "../src/probe/hci/fixture.rs"]
mod fixture;

#[cfg(test)]
mod hci {
    use std::fs::{self, Permissions};
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};

    use pods_proto::packets::{ATT_PSM, CONTROL_PSM};
    use pods_proto::sdulog::{Direction, parse_line};
    use tempfile::TempDir;

    use super::fixture::{
        ACCEPT, ATT_CIDS, AVDTP_PSM, AVDTP_START, CODEC_SBC, COMMAND, CONNECTION_REQUEST,
        CONTROL_CIDS, DEVICE_NAME, FIRST_SEID, GET_CAPABILITIES, HOST_ADDRESS, KEYBOARD_HANDLE,
        KEYSTROKES, LINK_KEY, MEDIA_CIDS, PODS_ADDRESS, PODS_HANDLE, SET_CONFIGURATION,
        SIGNALLING_CIDS, Trace, att_packets, avdtp, control_packets, media_codec, rtp, session,
    };

    /// A stamp of the shape `date -u +%Y%m%dT%H%M%SZ` prints.
    const STAMP: &str = "20261120T120000Z";
    /// The run every test creates.
    const RUN: &str = "trace-1";
    /// The modes the store gives every file and every directory it creates.
    const OWNER_FILE: u32 = 0o600;
    const OWNER_DIR: u32 = 0o700;
    /// The permission bits of a mode, with set-user-ID, set-group-ID and sticky.
    const PERMISSION_BITS: u32 = 0o7777;
    /// The longest run name the store takes.
    const RUN_NAME_MAX: usize = 32;
    /// A mode with a group bit, which the bounded read refuses.
    const GROUP_READABLE: u32 = 0o640;
    /// The store's bound on a capture read, 64 MiB, which a trace may pass.
    const CAPTURE_BOUND: usize = 64 * 1024 * 1024;
    /// The bound on a trace, 512 MiB, written out so the test pins the number.
    const TRACE_BOUND: u64 = 536_870_912;
    /// Monitor opcode of a system note, a record the reader skips (`BlueZ` src/shared/btsnoop.h, `BTSNOOP_OPCODE_SYSTEM_NOTE`).
    const OPCODE_SYSTEM_NOTE: u16 = 12;
    /// The most data bytes one record holds.
    const LONGEST_RECORD: usize = 65_535;
    /// Enough longest records to pass `CAPTURE_BOUND`: each takes 65 559 bytes with its header.
    const NOTES_PAST_CAPTURE_BOUND: usize = 1024;
    /// The L2CAP mode a channel must not be in to be exported: enhanced retransmission (Core 6.1 Vol 3 Part A 5.4).
    const ENHANCED_RETRANSMISSION: u8 = 0x03;
    /// An enhanced retransmission I-frame: two control bytes, then an ATT notification for handle 0x000d (Core 6.1 Vol 3 Part A 3.3.2).
    const I_FRAME: [u8; 5] = [0x00, 0x00, 0x1b, 0x0d, 0x00];
    const CONTROL_FIELD_LEN: usize = 2;
    /// The identification pattern, the version and the datalink type.
    const FILE_HEADER: usize = 16;
    /// A datalink type the reader refuses: HCI with no monitor header (`BlueZ` src/shared/btsnoop.h, `BTSNOOP_FORMAT_HCI`).
    const WRONG_DATALINK: u32 = 1001;
    /// Where the datalink type sits in the file header.
    const DATALINK_AT: std::ops::Range<usize> = 12..16;
    /// Two lengths, flags, drops and the timestamp.
    const RECORD_HEADER: usize = 24;
    /// Where the included length sits in a record header: after the original length.
    const INCLUDED_LEN: std::ops::Range<usize> = 4..8;
    /// Every file a run of the session trace holds.
    const SESSION_FILES: [&str; 6] = [
        "a2dp.tsv",
        "att.log",
        "avdtp.txt",
        "links.txt",
        "psm.tsv",
        "sdu.log",
    ];

    /// A made up connection handle for a second link.
    const SECOND_HANDLE: u16 = 0x000d;
    /// SBC codec bytes, made up, that the session trace never carries.
    const OTHER_SBC: [u8; 4] = [0x21, 0x15, 0x02, 0x20];
    /// A moment after the session's link ended that lies in the second of its last media packets.
    const SAME_SECOND_MS: u64 = 3_300;
    /// A moment in a second the session trace has no row for.
    const LATER_MS: u64 = 10_100;
    /// The music tables of the session trace.
    const SESSION_SECONDS: &str = "second\tmedia_packets\tmedia_bytes\trtp_gaps\tlargest_interval_ns\tavdtp_suspend\tavdtp_start\n\
        1\t1\t112\t0\t0\t0\t1\n\
        2\t5\t560\t1\t20000000\t1\t0\n\
        3\t2\t224\t0\t1100000000\t0\t1\n";
    const SESSION_CODECS: &str = "capability type=0x00 bytes=ffff0235\n\
        capability type=0x02 bytes=80018c84e200\n\
        capability type=0xff vendor=0x0000004c codec=0x8001 bytes=4c0000000180aabb\n\
        configuration type=0x02 bytes=80018c84e200\n";
    /// The music tables of one `stream` that starts at `LATER_MS`.
    const STREAM_SECONDS: &str = "second\tmedia_packets\tmedia_bytes\trtp_gaps\tlargest_interval_ns\tavdtp_suspend\tavdtp_start\n\
        10\t4\t448\t0\t1000000\t0\t1\n";
    const STREAM_CODECS: &str = "capability type=0x00 bytes=21150220\n\
        configuration type=0x00 bytes=21150220\n";
    /// The identifiers of three more channels, which no fixture trace opens.
    const SPARE_CIDS: [(u16, u16); 3] = [(0x0044, 0x0054), (0x0045, 0x0055), (0x0046, 0x0056)];
    /// The first byte of an AVDTP start packet, the first fragment of a longer signal (AVDTP 1.3 section 8.4.2, Table 8.4).
    const START_PACKET: u8 = 0x04;
    /// What `links.txt` says of a link's music tables when nothing on its AVDTP channels was left unread.
    const ALL_READ: &str = "avdtp_fragmented_signals=0 avdtp_malformed_signals=0 avdtp_other_channels=0 media_packets_not_rtp=0";

    /// The result of a connection response that refuses the PSM (Core 6.1 Vol 3 Part A 4.3).
    const PSM_NOT_SUPPORTED: u16 = 0x0002;
    /// The whole of `links.txt` for the session trace.
    const SESSION_LINKS: &str = "realtime_at_zero_ns 1795176000004000000\n\
        stamps_stepping_back 0\n\
        synchronous_connection_events 1\n\
        synchronous_connections_opened 1\n\
        acl_packets 45\n\
        malformed_acl_packets 0\n\
        continuations_without_start 0\n\
        unfinished_pdus 0\n\
        malformed_pdus 0\n\
        malformed_signals 0\n\
        responses_without_request 0\n\
        fixed_channel_pdus 0\n\
        requests_over_limit 0\n\
        unmapped_over_limit 0\n\
        not_basic_mode_pdus 0\n\
        avdtp_channels_on_other_links 0\n\
        log sdu.log handle=0x000b written=7 refused_empty=0 refused_too_long=0\n\
        log att.log handle=0x000b written=2 refused_empty=0 refused_too_long=0\n\
        music a2dp.tsv avdtp.txt handle=0x000b avdtp_fragmented_signals=0 avdtp_malformed_signals=0 avdtp_other_channels=0 media_packets_not_rtp=0\n";

    /// A temporary home with a trace file beside the store, owner only as the bounded read demands.
    fn home_with_trace(trace: &[u8]) -> (TempDir, PathBuf) {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("hci.btsnoop");
        fs::write(&path, trace).unwrap();
        fs::set_permissions(&path, Permissions::from_mode(OWNER_FILE)).unwrap();
        (home, path)
    }

    fn hci(home: &TempDir, trace: &Path, run: &str) -> Output {
        Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args(["probe", "hci"])
            .arg(trace)
            .args(["--stamp", STAMP, "--run", run])
            .env("HOME", home.path())
            .output()
            .unwrap()
    }

    fn run_dir(home: &TempDir, run: &str) -> PathBuf {
        home.path().join("omapods-captures").join(STAMP).join(run)
    }

    /// Every name in a directory, sorted.
    fn names_in(dir: &Path) -> Vec<String> {
        let entries = fs::read_dir(dir).unwrap();
        let mut names: Vec<String> = entries
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    /// The name and contents of every file in a run, sorted by name.
    fn files(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut files: Vec<(String, Vec<u8>)> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap())
            .map(|entry| {
                let name = entry.file_name().into_string().unwrap();
                (name, fs::read(entry.path()).unwrap())
            })
            .collect();
        files.sort();
        files
    }

    // Sample output for [0x02, 0xa1] with ":": 02:a1, lowercase.
    fn hex(bytes: &[u8], separator: &str) -> String {
        let pairs: Vec<String> = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        pairs.join(separator)
    }

    /// Every way one private byte string could show up: raw, and as hex in either case, joined, spaced or with colons.
    fn spellings(secret: &[u8]) -> Vec<Vec<u8>> {
        let mut spellings = vec![secret.to_vec()];
        for separator in ["", ":", " "] {
            let lower = hex(secret, separator);
            spellings.push(lower.to_uppercase().into_bytes());
            spellings.push(lower.into_bytes());
        }
        spellings
    }

    /// The private byte strings of the session trace, each in both byte orders.
    fn secrets() -> Vec<Vec<u8>> {
        let mut secrets = Vec::new();
        for secret in [
            &PODS_ADDRESS[..],
            &HOST_ADDRESS[..],
            &LINK_KEY[..],
            DEVICE_NAME.as_bytes(),
            KEYSTROKES,
        ] {
            let reversed: Vec<u8> = secret.iter().rev().copied().collect();
            secrets.extend(spellings(secret));
            secrets.extend(spellings(&reversed));
        }
        secrets
    }

    fn holds(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    fn assert_holds_no_secret(what: &str, bytes: &[u8]) {
        for secret in secrets() {
            assert!(!holds(bytes, &secret), "{what} holds a private byte string");
        }
    }

    /// The direction and bytes of every line of a log, read with the parser the analysis uses.
    fn log(dir: &Path, name: &str) -> Vec<(Direction, Vec<u8>)> {
        let text = fs::read_to_string(dir.join(name)).unwrap();
        let records: Vec<_> = text.lines().map(|line| parse_line(line).unwrap()).collect();
        assert!(records.is_sorted_by_key(|record| record.at_ns));
        records
            .into_iter()
            .map(|record| (record.direction, record.sdu))
            .collect()
    }

    #[test]
    fn output_holds_no_address() {
        let trace = session().bytes();
        for address in [PODS_ADDRESS, HOST_ADDRESS] {
            let on_the_wire: Vec<u8> = address.iter().rev().copied().collect();
            assert!(holds(&trace, &on_the_wire), "the trace holds the address");
        }
        assert!(holds(&trace, DEVICE_NAME.as_bytes()));
        assert!(holds(&trace, &LINK_KEY));
        assert!(holds(&trace, KEYSTROKES));
        let (home, path) = home_with_trace(&trace);

        let ran = hci(&home, &path, RUN);
        assert!(ran.status.success(), "{ran:?}");
        assert_holds_no_secret("stdout", &ran.stdout);
        assert_holds_no_secret("stderr", &ran.stderr);
        let written = files(&run_dir(&home, RUN));
        let names: Vec<&str> = written.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, SESSION_FILES);
        for (name, contents) in &written {
            assert_holds_no_secret(name, contents);
            assert_ne!(contents, &trace, "{name} is a copy of the trace");
        }

        let cut = &trace[..trace.len() - 1];
        let (home, path) = home_with_trace(cut);
        let ran = hci(&home, &path, RUN);
        assert!(ran.status.success(), "{ran:?}");
        assert!(holds(&ran.stderr, b"is cut short"), "{ran:?}");
        assert_holds_no_secret("stdout of a cut trace", &ran.stdout);
        assert_holds_no_secret("stderr of a cut trace", &ran.stderr);
        for (name, contents) in &files(&run_dir(&home, RUN)) {
            assert_holds_no_secret(name, contents);
        }

        let mut wrong_magic = trace.clone();
        wrong_magic[0] = b'B';
        let (home, path) = home_with_trace(&wrong_magic);
        let refused = hci(&home, &path, RUN);
        assert!(!refused.status.success(), "{refused:?}");
        assert_holds_no_secret("stdout of a refusal", &refused.stdout);
        assert_holds_no_secret("stderr of a refusal", &refused.stderr);
    }

    #[test]
    fn the_exported_logs_parse_and_round_trip() {
        let trace = session().bytes();
        let (home, path) = home_with_trace(&trace);
        let ran = hci(&home, &path, RUN);
        assert!(ran.status.success(), "{ran:?}");
        assert_eq!(String::from_utf8(ran.stderr).unwrap(), "");
        let dir = run_dir(&home, RUN);

        assert_eq!(log(&dir, "sdu.log"), control_packets());
        assert_eq!(log(&dir, "att.log"), att_packets());
        let links = fs::read_to_string(dir.join("links.txt")).unwrap();
        assert_eq!(String::from_utf8(ran.stdout).unwrap(), links);
        assert_eq!(links, SESSION_LINKS);
        for name in SESSION_FILES {
            let mode = fs::metadata(dir.join(name)).unwrap().permissions().mode();
            assert_eq!(mode & PERMISSION_BITS, OWNER_FILE, "{name}");
        }
        let root = home.path().join("omapods-captures");
        for private in [root.as_path(), &root.join(STAMP), &dir] {
            let mode = fs::metadata(private).unwrap().permissions().mode();
            assert_eq!(mode & PERMISSION_BITS, OWNER_DIR, "{}", private.display());
        }
        assert_eq!(fs::read(&path).unwrap(), trace);

        let before = files(&dir);
        let again = hci(&home, &path, RUN);
        assert!(!again.status.success(), "{again:?}");
        assert_eq!(again.stdout, b"");
        let error = String::from_utf8(again.stderr).unwrap();
        assert!(error.contains("a run is never reused"), "{error}");
        assert_eq!(files(&dir), before);
    }

    #[test]
    fn a_hostile_or_foreign_file_is_refused_and_no_run_is_made() {
        let trace = session().bytes();
        let mut wrong_magic = trace.clone();
        wrong_magic[0] = b'B';
        let mut wrong_datalink = trace.clone();
        wrong_datalink[DATALINK_AT].copy_from_slice(&WRONG_DATALINK.to_be_bytes());
        for (bytes, rule) in [
            (
                &trace[..FILE_HEADER - 1],
                "btsnoop trace is 15 bytes, shorter than its 16 byte file header",
            ),
            (
                &wrong_magic[..],
                "PacketLogger record 0 at byte offset 0 is cut short; a file that does not start with the btsnoop identification pattern is read as a PacketLogger trace, which is refused unless it parses to its last byte",
            ),
            (
                &wrong_datalink[..],
                "btsnoop trace has a datalink type other than the monitor datalink 2001, which btmon -w writes",
            ),
        ] {
            let (home, path) = home_with_trace(bytes);
            let refused = hci(&home, &path, RUN);
            assert!(!refused.status.success(), "{rule}: {refused:?}");
            assert_eq!(refused.stdout, b"", "{rule}");
            // The whole line is compared, so no value of the trace can ride along in any spelling.
            let said = format!("xtask probe: {}: {rule}\n", path.display());
            assert_eq!(String::from_utf8(refused.stderr).unwrap(), said);
            assert!(!home.path().join("omapods-captures").exists(), "{rule}");
        }
    }

    #[test]
    fn a_channel_not_in_basic_mode_gets_no_log_and_the_summary_says_why() {
        let mut trace = Trace::new();
        trace.open(Direction::ToPods, CONTROL_PSM, CONTROL_CIDS);
        trace.open(Direction::FromPods, ATT_PSM, ATT_CIDS);
        trace.offer_mode(Direction::FromPods, ATT_CIDS, ENHANCED_RETRANSMISSION);
        trace.send(Direction::FromPods, CONTROL_CIDS, &[0x01]);
        trace.send(Direction::FromPods, ATT_CIDS, &I_FRAME);
        let (home, path) = home_with_trace(&trace.bytes());
        let ran = hci(&home, &path, RUN);
        assert!(ran.status.success(), "{ran:?}");
        let written = files(&run_dir(&home, RUN));
        let names: Vec<&str> = written.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            ["a2dp.tsv", "avdtp.txt", "links.txt", "psm.tsv", "sdu.log"]
        );
        let control = [(Direction::FromPods, vec![0x01])];
        assert_eq!(log(&run_dir(&home, RUN), "sdu.log"), control);
        let said = "channel_not_read handle=0x000b psm=0x001f host_mode=- pods_mode=0x03 pdus=1 reason=l2cap_mode_is_not_basic\n";
        let stdout = String::from_utf8(ran.stdout).unwrap();
        assert!(stdout.contains(said), "{stdout}");
        assert!(stdout.contains("not_basic_mode_pdus 1\n"), "{stdout}");
        let packet = hex(&I_FRAME[CONTROL_FIELD_LEN..], "");
        for (name, contents) in &written {
            assert!(
                !holds(contents, packet.as_bytes()),
                "{name} holds the packet"
            );
        }
    }

    // Sample input: the 16 byte file header, then records: a 24 byte header whose bytes 4 to 8 are the included length, big endian, then that many data bytes.
    fn record_offsets(trace: &[u8]) -> Vec<usize> {
        let mut offsets = Vec::new();
        let mut rest = &trace[FILE_HEADER..];
        while !rest.is_empty() {
            offsets.push(trace.len().checked_sub(rest.len()).unwrap());
            let included = u32::from_be_bytes(rest[INCLUDED_LEN].try_into().unwrap());
            let record_len = RECORD_HEADER.checked_add(usize::try_from(included).unwrap());
            rest = &rest[record_len.unwrap()..];
        }
        offsets
    }

    #[test]
    fn a_cut_trace_is_read_up_to_its_last_whole_record_and_says_so() {
        let trace = session().bytes();
        let offsets = record_offsets(&trace);
        let last = offsets.len() - 1;
        let (home, path) = home_with_trace(&trace[..trace.len() - 3]);
        let ran = hci(&home, &path, RUN);
        assert!(ran.status.success(), "{ran:?}");
        let said = format!(
            "xtask probe: {}: btsnoop record {last} at byte offset {} is cut short, so the trace is read only up to the whole records before it\n",
            path.display(),
            offsets[last]
        );
        assert_eq!(String::from_utf8(ran.stderr).unwrap(), said);
        let dir = run_dir(&home, RUN);
        let links = fs::read_to_string(dir.join("links.txt")).unwrap();
        let first_line = format!("trace_cut_short record={last} offset={}\n", offsets[last]);
        assert!(links.starts_with(&first_line), "{links}");
        assert_eq!(String::from_utf8(ran.stdout).unwrap(), links);
        assert_eq!(log(&dir, "sdu.log"), control_packets());
        assert_eq!(log(&dir, "att.log"), att_packets());

        let (home, path) = home_with_trace(&trace[..offsets[0] + RECORD_HEADER]);
        let ran = hci(&home, &path, RUN);
        assert!(ran.status.success(), "{ran:?}");
        assert!(holds(
            &ran.stderr,
            b"btsnoop record 0 at byte offset 16 is cut short"
        ));
        let written = files(&run_dir(&home, RUN));
        let names: Vec<&str> = written.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["links.txt", "psm.tsv"]);
        let links = String::from_utf8(written[0].1.clone()).unwrap();
        assert!(
            links.starts_with("trace_cut_short record=0 offset=16\n"),
            "{links}"
        );

        let mut long_record = trace.clone();
        long_record[offsets[last]..][INCLUDED_LEN].copy_from_slice(&u32::MAX.to_be_bytes());
        let (home, path) = home_with_trace(&long_record);
        let ran = hci(&home, &path, RUN);
        assert!(ran.status.success(), "{ran:?}");
        // The whole line is compared, so the length the record claims cannot ride along in any spelling.
        let said = format!(
            "xtask probe: {}: btsnoop record {last} at byte offset {} claims more data than the 65535 bytes a record holds, so the trace is read only up to the whole records before it\n",
            path.display(),
            offsets[last]
        );
        assert_eq!(String::from_utf8(ran.stderr).unwrap(), said);
        let dir = run_dir(&home, RUN);
        let links = fs::read_to_string(dir.join("links.txt")).unwrap();
        let first_line = format!(
            "trace_record_too_long record={last} offset={}\n",
            offsets[last]
        );
        assert!(links.starts_with(&first_line), "{links}");
        assert_eq!(log(&dir, "sdu.log"), control_packets());

        let (home, path) = home_with_trace(&trace);
        let ran = hci(&home, &path, RUN);
        assert!(ran.status.success(), "{ran:?}");
        assert_eq!(ran.stderr, b"");
        assert!(!holds(&ran.stdout, b"trace_cut_short"));
    }

    #[test]
    fn a_trace_past_the_capture_bound_is_read_and_one_past_its_own_is_refused() {
        let mut long = session();
        let note = vec![0x5a; LONGEST_RECORD];
        for _ in 0..NOTES_PAST_CAPTURE_BOUND {
            long.record(OPCODE_SYSTEM_NOTE, &note);
        }
        let trace = long.bytes();
        assert!(trace.len() > CAPTURE_BOUND);
        let (home, path) = home_with_trace(&trace);
        let ran = hci(&home, &path, RUN);
        assert!(ran.status.success(), "{ran:?}");
        assert_eq!(log(&run_dir(&home, RUN), "sdu.log"), control_packets());

        let (home, path) = home_with_trace(&[]);
        let sparse = fs::File::options().write(true).open(&path).unwrap();
        sparse.set_len(TRACE_BOUND + 1).unwrap();
        drop(sparse);
        let refused = hci(&home, &path, RUN);
        assert!(!refused.status.success(), "{refused:?}");
        assert_eq!(refused.stdout, b"");
        let error = String::from_utf8(refused.stderr).unwrap();
        assert!(error.contains("is larger than 536870912 bytes"), "{error}");
        assert!(!home.path().join("omapods-captures").exists());
    }

    #[test]
    fn a_trace_others_can_read_or_a_missing_one_is_refused() {
        let trace = session().bytes();
        let (home, path) = home_with_trace(&trace);
        fs::set_permissions(&path, Permissions::from_mode(GROUP_READABLE)).unwrap();
        let refused = hci(&home, &path, RUN);
        assert!(!refused.status.success(), "{refused:?}");
        let error = String::from_utf8(refused.stderr).unwrap();
        assert!(error.contains("group and others have no access"), "{error}");
        let advice = "; create the file yourself with mode 600 before btmon -w writes into it, or make this one yours with chown and chmod 600\n";
        assert!(error.ends_with(advice), "{error}");

        let link = home.path().join("link.btsnoop");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        fs::set_permissions(&path, Permissions::from_mode(OWNER_FILE)).unwrap();
        let refused = hci(&home, &link, RUN);
        assert!(!refused.status.success(), "{refused:?}");
        let error = String::from_utf8(refused.stderr).unwrap();
        assert!(error.contains("is a symbolic link"), "{error}");

        let missing = hci(&home, &home.path().join("absent.btsnoop"), RUN);
        assert!(!missing.status.success(), "{missing:?}");
        assert!(holds(&missing.stderr, b"absent.btsnoop"), "{missing:?}");
        assert!(!holds(&missing.stderr, b"btmon"), "{missing:?}");
        assert!(!home.path().join("omapods-captures").exists());

        let bad_stamp = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args(["probe", "hci"])
            .arg(&path)
            .args(["--stamp", "today", "--run", RUN])
            .env("HOME", home.path())
            .output()
            .unwrap();
        assert!(!bad_stamp.status.success(), "{bad_stamp:?}");
        assert!(!home.path().join("omapods-captures").exists());
    }

    /// A music stream on `handle` from `at_ms` on: SBC offered and set, a start, then four media packets a millisecond apart.
    fn stream(trace: &mut Trace, handle: u16, at_ms: u64) {
        let (host, far) = (Direction::ToPods, Direction::FromPods);
        let offered = media_codec(CODEC_SBC, &OTHER_SBC);
        let mut chosen = vec![FIRST_SEID, FIRST_SEID];
        chosen.extend(&offered);
        trace.on(handle);
        trace.at_ms(at_ms);
        trace.open(host, AVDTP_PSM, SIGNALLING_CIDS);
        for (sender, packet) in [
            (far, avdtp(1, ACCEPT, GET_CAPABILITIES, &offered)),
            (host, avdtp(2, COMMAND, SET_CONFIGURATION, &chosen)),
            (far, avdtp(2, ACCEPT, SET_CONFIGURATION, &[])),
            (host, avdtp(3, COMMAND, AVDTP_START, &[FIRST_SEID])),
        ] {
            trace.send(sender, SIGNALLING_CIDS, &packet);
        }
        trace.open(host, AVDTP_PSM, MEDIA_CIDS);
        for sequence in 0..4 {
            trace.send(host, MEDIA_CIDS, &rtp(sequence));
        }
    }

    #[test]
    fn a_headset_that_is_not_the_pods_changes_no_music_table() {
        // The second handle is the one the pods' link had before it ended, so only the link tells the two devices apart.
        for handle in [SECOND_HANDLE, PODS_HANDLE] {
            let mut trace = session();
            stream(&mut trace, handle, SAME_SECOND_MS);
            let (home, path) = home_with_trace(&trace.bytes());
            let ran = hci(&home, &path, RUN);
            assert!(ran.status.success(), "{ran:?}");
            let dir = run_dir(&home, RUN);
            assert_eq!(names_in(&dir), SESSION_FILES);
            let seconds = fs::read_to_string(dir.join("a2dp.tsv")).unwrap();
            assert_eq!(seconds, SESSION_SECONDS, "handle {handle:#06x}");
            let codecs = fs::read_to_string(dir.join("avdtp.txt")).unwrap();
            assert_eq!(codecs, SESSION_CODECS, "handle {handle:#06x}");
            let links = String::from_utf8(ran.stdout).unwrap();
            let read = format!("music a2dp.tsv avdtp.txt handle=0x000b {ALL_READ}\n");
            assert!(links.contains(&read), "{links}");
            let left_out = "avdtp_channels_on_other_links 2\n";
            assert!(links.contains(left_out), "{links}");
            let requests = fs::read_to_string(dir.join("psm.tsv")).unwrap();
            let asked = format!("\thost\t0x0019\t0x0000\t{handle:#06x}\t");
            assert_eq!(
                requests.matches(&asked).count(),
                if handle == PODS_HANDLE { 4 } else { 2 }
            );
        }
    }

    #[test]
    fn a_trace_that_shows_no_pods_link_gets_no_music_table_and_says_so() {
        let mut trace = Trace::new();
        stream(&mut trace, PODS_HANDLE, LATER_MS);
        let (home, path) = home_with_trace(&trace.bytes());
        let ran = hci(&home, &path, RUN);
        assert!(ran.status.success(), "{ran:?}");
        assert_eq!(ran.stderr, b"");
        assert_eq!(names_in(&run_dir(&home, RUN)), ["links.txt", "psm.tsv"]);
        let links = String::from_utf8(ran.stdout).unwrap();
        let said = "avdtp_channels_on_other_links 2\n\
            music_tables_not_written reason=no_link_opened_psm_0x1001\n";
        assert!(links.ends_with(said), "{links}");
        assert!(!links.contains("music a2dp"), "{links}");
    }

    #[test]
    fn each_pods_link_gets_music_tables_of_its_own() {
        // The second link is a reconnection on the handle of the link that ended, then a link older than the pods' that opens its control channel later.
        for handle in [PODS_HANDLE, KEYBOARD_HANDLE] {
            let mut trace = session();
            trace.on(handle);
            trace.open(Direction::ToPods, CONTROL_PSM, SPARE_CIDS[0]);
            stream(&mut trace, handle, LATER_MS);
            let (home, path) = home_with_trace(&trace.bytes());
            let ran = hci(&home, &path, RUN);
            assert!(ran.status.success(), "{ran:?}");
            let dir = run_dir(&home, RUN);
            assert_eq!(
                names_in(&dir),
                [
                    "a2dp-2.tsv",
                    "a2dp.tsv",
                    "att.log",
                    "avdtp-2.txt",
                    "avdtp.txt",
                    "links.txt",
                    "psm.tsv",
                    "sdu-2.log",
                    "sdu.log"
                ]
            );
            for (name, expected) in [
                ("a2dp.tsv", SESSION_SECONDS),
                ("avdtp.txt", SESSION_CODECS),
                ("a2dp-2.tsv", STREAM_SECONDS),
                ("avdtp-2.txt", STREAM_CODECS),
            ] {
                let written = fs::read_to_string(dir.join(name)).unwrap();
                assert_eq!(written, expected, "{name} with handle {handle:#06x}");
            }
            let links = String::from_utf8(ran.stdout).unwrap();
            let said = format!(
                "music a2dp.tsv avdtp.txt handle=0x000b {ALL_READ}\n\
                 music a2dp-2.tsv avdtp-2.txt handle={handle:#06x} {ALL_READ}\n"
            );
            assert!(links.ends_with(&said), "{links}");
            assert!(
                links.contains("avdtp_channels_on_other_links 0\n"),
                "{links}"
            );
            assert!(!links.contains("music_tables_not_written"), "{links}");
        }
    }

    #[test]
    fn what_a_link_left_unread_is_counted_on_its_own_line() {
        let (host, pods) = (Direction::ToPods, Direction::FromPods);
        let mut trace = session();
        trace.on(SECOND_HANDLE);
        // The link opens two control channels and is still one link with one pair of tables.
        trace.open(host, CONTROL_PSM, CONTROL_CIDS);
        trace.open(host, CONTROL_PSM, ATT_CIDS);
        trace.open(host, AVDTP_PSM, SIGNALLING_CIDS);
        trace.open(host, AVDTP_PSM, MEDIA_CIDS);
        trace.send(pods, SIGNALLING_CIDS, &[START_PACKET]);
        for _ in 0..2 {
            trace.send(pods, SIGNALLING_CIDS, &[]);
        }
        for cids in SPARE_CIDS {
            trace.open(host, AVDTP_PSM, cids);
        }
        for _ in 0..4 {
            trace.send(host, MEDIA_CIDS, &[0x00]);
        }
        let (home, path) = home_with_trace(&trace.bytes());
        let ran = hci(&home, &path, RUN);
        assert!(ran.status.success(), "{ran:?}");
        let links = String::from_utf8(ran.stdout).unwrap();
        let said = format!(
            "music a2dp.tsv avdtp.txt handle=0x000b {ALL_READ}\n\
             music a2dp-2.tsv avdtp-2.txt handle=0x000d avdtp_fragmented_signals=1 avdtp_malformed_signals=2 avdtp_other_channels=3 media_packets_not_rtp=4\n"
        );
        assert!(links.ends_with(&said), "{links}");
    }

    #[test]
    fn a_control_request_refused_or_never_answered_marks_no_link_as_the_pods() {
        let host = Direction::ToPods;
        let mut trace = session();
        trace.on(SECOND_HANDLE);
        trace.connect(host, CONTROL_PSM, SPARE_CIDS[0], PSM_NOT_SUPPORTED);
        let mut unanswered = CONTROL_PSM.to_le_bytes().to_vec();
        unanswered.extend(SPARE_CIDS[1].0.to_le_bytes());
        trace.signal(host, CONNECTION_REQUEST, &unanswered);
        stream(&mut trace, SECOND_HANDLE, LATER_MS);
        let (home, path) = home_with_trace(&trace.bytes());
        let ran = hci(&home, &path, RUN);
        assert!(ran.status.success(), "{ran:?}");
        let dir = run_dir(&home, RUN);
        let requests = fs::read_to_string(dir.join("psm.tsv")).unwrap();
        for asked in [
            "\thost\t0x1001\t0x0002\t0x000d\t",
            "\thost\t0x1001\t-\t0x000d\t",
        ] {
            assert_eq!(requests.matches(asked).count(), 1, "{asked:?}");
        }
        assert_eq!(names_in(&dir), SESSION_FILES);
        let links = String::from_utf8(ran.stdout).unwrap();
        let said = format!("music a2dp.tsv avdtp.txt handle=0x000b {ALL_READ}\n");
        assert!(links.ends_with(&said), "{links}");
        let left_out = "avdtp_channels_on_other_links 2\n";
        assert!(links.contains(left_out), "{links}");
    }

    #[test]
    fn a_control_channel_in_another_mode_still_marks_its_link_as_the_pods() {
        let mut trace = Trace::new();
        trace.open(Direction::ToPods, CONTROL_PSM, CONTROL_CIDS);
        trace.offer_mode(Direction::FromPods, CONTROL_CIDS, ENHANCED_RETRANSMISSION);
        stream(&mut trace, PODS_HANDLE, LATER_MS);
        let (home, path) = home_with_trace(&trace.bytes());
        let ran = hci(&home, &path, RUN);
        assert!(ran.status.success(), "{ran:?}");
        let dir = run_dir(&home, RUN);
        assert_eq!(
            names_in(&dir),
            ["a2dp.tsv", "avdtp.txt", "links.txt", "psm.tsv"]
        );
        let seconds = fs::read_to_string(dir.join("a2dp.tsv")).unwrap();
        assert_eq!(seconds, STREAM_SECONDS);
        let codecs = fs::read_to_string(dir.join("avdtp.txt")).unwrap();
        assert_eq!(codecs, STREAM_CODECS);
        let links = String::from_utf8(ran.stdout).unwrap();
        let not_read = "channel_not_read handle=0x000b psm=0x1001 host_mode=- pods_mode=0x03 pdus=0 reason=l2cap_mode_is_not_basic\n";
        assert!(links.contains(not_read), "{links}");
        let said = format!("music a2dp.tsv avdtp.txt handle=0x000b {ALL_READ}\n");
        assert!(links.ends_with(&said), "{links}");
    }

    #[test]
    fn a_run_name_the_store_refuses_makes_no_directory() {
        let too_long = "a".repeat(RUN_NAME_MAX + 1);
        let whole = session().bytes();
        // The last trace is one the reader refuses, so its name line shows the name is judged before the trace.
        for (bad, trace) in [
            ("../escape", &whole[..]),
            ("", &whole[..]),
            ("Trace", &whole[..]),
            (too_long.as_str(), &whole[..]),
            ("Trace", &[][..]),
        ] {
            let (home, path) = home_with_trace(trace);
            let refused = hci(&home, &path, bad);
            assert!(!refused.status.success(), "{bad}: {refused:?}");
            assert_eq!(refused.stdout, b"", "{bad}");
            let said = format!(
                "xtask probe: run name {bad:?} is not 1 to 32 characters of a-z, 0-9 and -\n"
            );
            assert_eq!(String::from_utf8(refused.stderr).unwrap(), said);
            assert_eq!(names_in(home.path()), ["hci.btsnoop"], "{bad}");
        }
    }

    mod pklg;
}
