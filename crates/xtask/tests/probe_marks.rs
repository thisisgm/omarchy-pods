//! The built binary's `probe marks` command, run on a synthetic trace of two devices and on made up logs.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "the traces of the other probe commands go unused here"
)]
#[path = "../src/probe/hci/fixture.rs"]
mod hci_fixture;

#[cfg(test)]
#[expect(
    dead_code,
    reason = "the marks unit tests also build legacy adverts and a failed page"
)]
#[path = "../src/probe/marks/fixture.rs"]
mod fixture;

#[cfg(test)]
mod marks {
    use std::collections::BTreeMap;
    use std::fs::{self, Permissions};
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::{Command, Output};

    use pods_core::policy::audio::sink_prefix;
    use pods_proto::addr::BdAddr;
    use pods_proto::sdulog::{Direction, format_line};
    use tempfile::TempDir;

    use super::fixture::{
        OTHER_PODS_ADDRESS, OTHER_PODS_HANDLE, TAKEOVER_TRANSACTION, accessory_sdu, playing_link,
        source_sdu, two_devices, whole_connection,
    };
    use super::hci_fixture::{
        ACCEPT, AVDTP_PSM, AVDTP_START, AVDTP_SUSPEND, COMMAND, EVENT_DISCONNECTION_COMPLETE,
        FIRST_SEID, HOST_ADDRESS, PODS_ADDRESS, PODS_HANDLE, SIGNALLING_CIDS, START_UNIX_MICROS,
        Trace, avdtp,
    };

    const RECONNECTED_PODS_HANDLE: u16 = 0x000e;
    /// The mode the bounded read demands of the trace and the logs, and exactly of the device file.
    const OWNER_FILE: u32 = 0o600;
    /// A mode with access for group and others, which the device file may not have.
    const WORLD_READABLE: u32 = 0o644;
    /// A mode that belongs to the owner alone but is still wrong for the device file.
    const OWNER_READ_ONLY: u32 = 0o400;
    /// The three trials of `two_devices`, ten seconds each from the moment the trace starts.
    const TRIALS: &str = "1795176000.000000000 trial 1\n\
        1795176010.000000000 trial 2\n\
        1795176020.000000000 trial 3\n";
    /// The daemon's lines: up and populated in the first trial, up alone in the third.
    const JOURNAL: &str = "{\"__REALTIME_TIMESTAMP\":\"1795176001320000\",\"__MONOTONIC_TIMESTAMP\":\"5000000\",\"MESSAGE\":\"link: features to up\"}\n\
        {\"__REALTIME_TIMESTAMP\":\"1795176001350000\",\"__MONOTONIC_TIMESTAMP\":\"5030000\",\"MESSAGE\":\"link: state populated\"}\n\
        {\"__REALTIME_TIMESTAMP\":\"1795176022320000\",\"__MONOTONIC_TIMESTAMP\":\"26000000\",\"MESSAGE\":\"link: features to up\"}\n";
    /// What the command prints for `two_devices` with these logs.
    fn table(third_ended: &str) -> String {
        format!(
            "trial\tbegan\tadvert_ms\tset_configurations\topen_ms\tstart_ms\tcontrol_dials\tmedia_ms\tended\tup_ms\tpopulated_ms\tsink_ms\n\
             1\tpods\t-280\t1\t220\t320\t20:0x0000\t420\t0x13\t40\t70\t620\n\
             2\t\t\t\t\t\t\t\t\t\t\t\n\
             3\thost\t\t2\t\t\t100:0x0004,900:0x0000\t\t{third_ended}\t920\t\t\n"
        )
    }

    fn address_file_text(address: [u8; 6]) -> String {
        address.map(|byte| format!("{byte:02X}")).join(":")
    }

    fn device() -> String {
        address_file_text(PODS_ADDRESS)
    }

    /// A speaker and the other device's sink before the named device's sink.
    fn sinks() -> String {
        let sink = sink_prefix(BdAddr::from(PODS_ADDRESS));
        let other = sink_prefix(BdAddr::from(OTHER_PODS_ADDRESS));
        format!(
            "1795176000.500000000 56\talsa_output.speakers\tPipeWire\ts16le 2ch 48000Hz\tRUNNING\n\
             1795176001.500000000 58\t{other}.1\tPipeWire\ts16le 2ch 48000Hz\tRUNNING\n\
             1795176001.900000000 57\t{sink}.1\tPipeWire\ts16le 2ch 48000Hz\tSUSPENDED\n"
        )
    }

    fn put(dir: &TempDir, name: &str, contents: &[u8], mode: u32) {
        let path = dir.path().join(name);
        fs::write(&path, contents).unwrap();
        fs::set_permissions(&path, Permissions::from_mode(mode)).unwrap();
    }

    /// A directory with the device file, the logs and `trace`, each the owner's alone.
    fn sitting(trace: &[u8]) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        put(
            &dir,
            "device",
            format!("{}\n", device()).as_bytes(),
            OWNER_FILE,
        );
        put(&dir, "hci.btsnoop", trace, OWNER_FILE);
        put(&dir, "trials.log", TRIALS.as_bytes(), OWNER_FILE);
        put(&dir, "journal.json", JOURNAL.as_bytes(), OWNER_FILE);
        put(&dir, "sinks.log", sinks().as_bytes(), OWNER_FILE);
        dir
    }

    fn marks(dir: &Path) -> Output {
        let mut marks = Command::new(env!("CARGO_BIN_EXE_xtask"));
        marks.args(["probe", "marks"]).arg(dir.join("hci.btsnoop"));
        for (flag, file) in [
            ("--device-file", "device"),
            ("--trials", "trials.log"),
            ("--journal", "journal.json"),
            ("--sinks", "sinks.log"),
        ] {
            marks.arg(flag).arg(dir.join(file));
        }
        marks.env("HOME", dir).output().unwrap()
    }

    fn text(bytes: &[u8]) -> String {
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    /// The command failed and printed no table, and what it said holds `said`.
    fn assert_refused(ran: &Output, said: &str) {
        assert_no_address(ran);
        assert!(!ran.status.success());
        assert_eq!(text(&ran.stdout), "");
        assert!(text(&ran.stderr).contains(said), "{}", text(&ran.stderr));
    }

    /// Nothing the command wrote spells the address, with colons, with underscores or bare, in capitals or not.
    fn assert_no_address(ran: &Output) {
        let written = (text(&ran.stdout) + &text(&ran.stderr)).to_lowercase();
        for address in [PODS_ADDRESS, OTHER_PODS_ADDRESS, HOST_ADDRESS] {
            let mut reversed = address;
            reversed.reverse();
            for octets in [address, reversed] {
                let pairs = octets.map(|byte| format!("{byte:02x}"));
                for spelling in [pairs.join(":"), pairs.join("_"), pairs.concat()] {
                    assert!(
                        !written.contains(&spelling),
                        "output includes a device identifier"
                    );
                }
            }
        }
        assert!(
            !written.contains("bluez_output"),
            "output includes a sink name"
        );
    }

    #[test]
    fn marks_prints_one_row_for_each_trial() {
        let dir = sitting(&two_devices().bytes());
        let ran = marks(dir.path());
        assert!(ran.status.success(), "{}", text(&ran.stderr));
        assert_eq!(text(&ran.stdout), table(""));
        assert_eq!(text(&ran.stderr), "");
        assert_no_address(&ran);
    }

    #[test]
    fn marks_leaves_out_journal_and_sink_marks_from_a_reconnect() {
        const FIRST_ACL_MS: u64 = 100;
        const DISCONNECT_MS: u64 = 1_000;
        const RECONNECTED_ACL_MS: u64 = 1_280;

        let mut trace = Trace::new();
        trace.create_connection(PODS_ADDRESS);
        trace.at_ms(FIRST_ACL_MS);
        trace.link_up(PODS_HANDLE, PODS_ADDRESS);
        trace.at_ms(DISCONNECT_MS);
        trace.end_link(PODS_HANDLE);
        trace.create_connection(PODS_ADDRESS);
        trace.at_ms(RECONNECTED_ACL_MS);
        trace.link_up(PODS_HANDLE, PODS_ADDRESS);
        let dir = sitting(&trace.bytes());
        let ran = marks(dir.path());

        assert!(ran.status.success(), "{}", text(&ran.stderr));
        assert_eq!(
            text(&ran.stdout).lines().nth(1),
            Some("1\thost\t\t0\t\t\t\t\t0x13\t\t\t")
        );
    }

    #[test]
    fn marks_ends_the_link_when_the_disconnect_reason_is_missing() {
        const FIRST_ACL_MS: u64 = 100;
        const DISCONNECT_MS: u64 = 1_000;
        const RECONNECTED_ACL_MS: u64 = 1_280;
        const STATUS_SUCCESS: u8 = 0x00;

        let mut trace = Trace::new();
        trace.create_connection(PODS_ADDRESS);
        trace.at_ms(FIRST_ACL_MS);
        trace.link_up(PODS_HANDLE, PODS_ADDRESS);
        trace.at_ms(DISCONNECT_MS);
        let [low, high] = PODS_HANDLE.to_le_bytes();
        trace.event(EVENT_DISCONNECTION_COMPLETE, &[STATUS_SUCCESS, low, high]);
        trace.create_connection(PODS_ADDRESS);
        trace.at_ms(RECONNECTED_ACL_MS);
        trace.link_up(PODS_HANDLE, PODS_ADDRESS);
        trace.on(PODS_HANDLE);
        trace.carry(&whole_connection());
        let dir = sitting(&trace.bytes());
        let ran = marks(dir.path());

        assert!(ran.status.success(), "{}", text(&ran.stderr));
        assert_eq!(
            text(&ran.stdout).lines().nth(1),
            Some("1\thost\t\t0\t\t\t\t\treasonless\t\t\t")
        );
        assert_no_address(&ran);
    }

    #[test]
    fn marks_reads_a_cut_trace_up_to_the_cut_and_says_so() {
        let mut cut = two_devices().bytes();
        cut.extend([0x00, 0x00]);
        let dir = sitting(&cut);
        let trials = format!("{TRIALS}1795176030.000000000 trial 4\n");
        put(&dir, "trials.log", trials.as_bytes(), OWNER_FILE);
        let ran = marks(dir.path());
        assert!(!ran.status.success());
        assert_eq!(
            text(&ran.stdout),
            format!("{}4\tincomplete\t\t\t\t\t\t\t\t\t\t\n", table("incomplete"))
        );
        assert!(text(&ran.stderr).contains("is cut short"));
        assert_no_address(&ran);
    }

    #[test]
    fn marks_labels_a_connected_trial_when_the_trace_ends_mid_window() {
        const ACL_UP_MS: u64 = 1_280;

        let mut trace = Trace::new();
        trace.create_connection(PODS_ADDRESS);
        trace.at_ms(ACL_UP_MS);
        trace.link_up(PODS_HANDLE, PODS_ADDRESS);
        let mut cut = trace.bytes();
        cut.extend([0x00, 0x00]);
        let dir = sitting(&cut);
        let ran = marks(dir.path());

        assert_eq!(
            text(&ran.stdout).lines().nth(1),
            Some("1\thost\t\t0\t\t\t\t\tincomplete\t40\t70\t620")
        );
    }

    #[test]
    fn marks_keeps_the_disconnect_reason_when_the_trace_ends_mid_window() {
        const ACL_UP_MS: u64 = 1_280;
        const DISCONNECT_MS: u64 = 3_000;
        const CUT_MS: u64 = 5_000;

        let mut trace = Trace::new();
        trace.create_connection(PODS_ADDRESS);
        trace.at_ms(ACL_UP_MS);
        trace.link_up(PODS_HANDLE, PODS_ADDRESS);
        trace.at_ms(DISCONNECT_MS);
        trace.end_link(PODS_HANDLE);
        trace.at_ms(CUT_MS);
        trace.create_connection(PODS_ADDRESS);
        let mut cut = trace.bytes();
        cut.pop().unwrap();
        let dir = sitting(&cut);
        let ran = marks(dir.path());

        assert!(!ran.status.success());
        assert!(text(&ran.stderr).contains("is cut short"));
        assert_eq!(
            text(&ran.stdout).lines().nth(1),
            Some("1\thost\t\t0\t\t\t\t\t0x13\t40\t70\t620")
        );
    }

    #[test]
    fn marks_names_no_address_in_any_refusal() {
        // A device file that others can read, then one that holds the address and one byte more.
        let dir = sitting(&two_devices().bytes());
        let device_path = dir.path().join("device");
        fs::set_permissions(&device_path, Permissions::from_mode(WORLD_READABLE)).unwrap();
        assert_refused(&marks(dir.path()), "has mode 0644");
        fs::set_permissions(&device_path, Permissions::from_mode(OWNER_READ_ONLY)).unwrap();
        assert_refused(&marks(dir.path()), "has mode 0400");
        fs::set_permissions(&device_path, Permissions::from_mode(OWNER_FILE)).unwrap();
        put(
            &dir,
            "device",
            format!("{}0\n", device()).as_bytes(),
            OWNER_FILE,
        );
        assert_refused(&marks(dir.path()), "does not hold one Bluetooth address");
        // A trials log with a line that is no trial.
        let dir = sitting(&two_devices().bytes());
        put(
            &dir,
            "trials.log",
            format!("{}\n", device()).as_bytes(),
            OWNER_FILE,
        );
        assert_refused(&marks(dir.path()), "trials log line 1");
        // A link that no request and no command began, and a file that is no trace.
        let mut unbegun = Trace::new();
        unbegun.link_up(PODS_HANDLE, PODS_ADDRESS);
        let dir = sitting(&unbegun.bytes());
        assert_refused(&marks(dir.path()), "the named device");
        let dir = sitting(device().as_bytes());
        assert_refused(&marks(dir.path()), "identification pattern");
    }

    const RETRIAL: &str = "1795176000.020000000 trial 1\n";
    const NANOS_PER_MS: u64 = 1_000_000;
    const NANOS_PER_MICRO: u64 = 1_000;
    const CONNECTED_DEVICES_OPCODE: u16 = 0x002e;
    const MEDIA: u8 = 0x02;
    const MEDIA_WITH_UPLINK: u8 = 0x22;

    fn at_us(ms: u64) -> u64 {
        START_UNIX_MICROS.saturating_add(ms.saturating_mul(1_000))
    }

    fn takeover(dir: &Path, files: &[(&str, &str)]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
        command.args(["probe", "marks", "takeover"]);
        for (flag, file) in files {
            command.arg(flag).arg(dir.join(file));
        }
        command.env("HOME", dir).output().unwrap()
    }

    const TRACE_FILES: [(&str, &str); 3] = [
        ("--trace", "hci.btsnoop"),
        ("--trials", "trials.log"),
        ("--device-file", "device"),
    ];
    const SDU_FILES: [(&str, &str); 2] = [("--sdus", "sdu.log"), ("--host-file", "host")];

    fn takeover_rows(output: &Output) -> Vec<BTreeMap<String, String>> {
        assert_no_address(output);
        assert!(output.status.success(), "{}", text(&output.stderr));
        assert_eq!(text(&output.stderr), "");
        let table = text(&output.stdout);
        let mut lines = table.lines();
        let header = lines.next().unwrap();
        lines
            .map(|line| {
                let fields = line.split('\t');
                assert_eq!(fields.clone().count(), header.split('\t').count());
                header
                    .split('\t')
                    .map(str::to_owned)
                    .zip(fields.map(str::to_owned))
                    .collect()
            })
            .collect()
    }

    fn put_sdu_log(dir: &TempDir, packets: &[(u64, Direction, Vec<u8>)]) {
        let lines = packets.iter().map(|(ms, direction, sdu)| {
            format_line(ms.saturating_mul(NANOS_PER_MS), *direction, sdu).unwrap()
        });
        let log = lines.collect::<Vec<_>>().join("\n");
        put(dir, "sdu.log", log.as_bytes(), OWNER_FILE);
        put(
            dir,
            "host",
            address_file_text(HOST_ADDRESS).as_bytes(),
            OWNER_FILE,
        );
        let zero_ns = START_UNIX_MICROS.saturating_mul(NANOS_PER_MICRO);
        let meta = format!("script=takeover\nrealtime_at_zero_ns={zero_ns}\n");
        put(dir, "meta.txt", meta.as_bytes(), OWNER_FILE);
    }

    #[test]
    fn takeover_reads_the_reconnected_link() {
        const DISCONNECT_MS: u64 = 15;
        const RECONNECT_MS: u64 = 25;
        const SUSPEND_MS: u64 = 40;

        let mut trace = playing_link();
        trace.at_ms(DISCONNECT_MS);
        trace.end_link(PODS_HANDLE);
        trace.at_ms(RECONNECT_MS);
        trace.link_up(RECONNECTED_PODS_HANDLE, PODS_ADDRESS);
        trace.on(RECONNECTED_PODS_HANDLE);
        trace.open(Direction::ToPods, AVDTP_PSM, SIGNALLING_CIDS);
        trace.at_ms(SUSPEND_MS);
        trace.send(
            Direction::FromPods,
            SIGNALLING_CIDS,
            &avdtp(TAKEOVER_TRANSACTION, COMMAND, AVDTP_SUSPEND, &[FIRST_SEID]),
        );
        let dir = sitting(&trace.bytes());
        put(&dir, "trials.log", RETRIAL.as_bytes(), OWNER_FILE);
        let rows = takeover_rows(&takeover(dir.path(), &TRACE_FILES));

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["link"], "present");
        assert_eq!(rows[0]["suspend_us"], at_us(SUSPEND_MS).to_string());
        assert_eq!(rows[0]["suspend_from"], "pods");
        assert_eq!(rows[0]["ended"], "");
    }

    #[test]
    fn takeover_marks_a_trial_with_no_link_as_missing() {
        const DISCONNECT_MS: u64 = 15;

        let mut trace = playing_link();
        trace.at_ms(DISCONNECT_MS);
        trace.end_link(PODS_HANDLE);
        let dir = sitting(&trace.bytes());
        let trials = "1795176000.000000000 trial 7\n1795176000.020000000 trial 8\n";
        put(&dir, "trials.log", trials.as_bytes(), OWNER_FILE);
        let rows = takeover_rows(&takeover(dir.path(), &TRACE_FILES));

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["link"], "present");
        assert_eq!(rows[0]["suspend_us"], "");
        assert_eq!(rows[0]["ended"], "0x13");
        assert_eq!(rows[1]["trial"], "8");
        assert_eq!(rows[1]["link"], "missing");
        assert_eq!(rows[1]["suspend_us"], "");
        assert_eq!(rows[1]["ended"], "");
    }

    #[test]
    fn takeover_reads_only_the_named_link_at_the_trial_boundary() {
        let mut trace = playing_link();
        trace.at_ms(20);
        trace.on(OTHER_PODS_HANDLE);
        trace.link_up(OTHER_PODS_HANDLE, OTHER_PODS_ADDRESS);
        trace.open(Direction::ToPods, AVDTP_PSM, SIGNALLING_CIDS);
        trace.send(
            Direction::ToPods,
            SIGNALLING_CIDS,
            &avdtp(TAKEOVER_TRANSACTION, COMMAND, AVDTP_SUSPEND, &[FIRST_SEID]),
        );
        trace.end_link(OTHER_PODS_HANDLE);
        trace.on(PODS_HANDLE);
        trace.at_ms(40);
        trace.send(
            Direction::FromPods,
            SIGNALLING_CIDS,
            &avdtp(TAKEOVER_TRANSACTION, COMMAND, AVDTP_SUSPEND, &[FIRST_SEID]),
        );
        trace.at_ms(60);
        trace.end_link(PODS_HANDLE);
        let dir = sitting(&trace.bytes());
        let trials = format!("{RETRIAL}1795176000.060000000 trial 2\n");
        put(&dir, "trials.log", trials.as_bytes(), OWNER_FILE);
        let rows = takeover_rows(&takeover(dir.path(), &TRACE_FILES));

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["suspend_us"], at_us(40).to_string());
        assert_eq!(rows[0]["suspend_from"], "pods");
        assert_eq!(rows[0]["ended"], "");
        assert_eq!(rows[1]["suspend_us"], "");
        assert_eq!(rows[1]["ended"], "0x13");
    }

    #[test]
    fn takeover_redacts_source_and_device_reports() {
        let dir = tempfile::tempdir().unwrap();
        let mut wire = HOST_ADDRESS;
        wire.reverse();
        put_sdu_log(
            &dir,
            &[
                (1, Direction::FromPods, source_sdu(wire, MEDIA_WITH_UPLINK)),
                (2, Direction::FromPods, source_sdu(HOST_ADDRESS, MEDIA)),
                (
                    3,
                    Direction::FromPods,
                    source_sdu(OTHER_PODS_ADDRESS, MEDIA),
                ),
                (4, Direction::FromPods, source_sdu([0; 6], 0)),
                (5, Direction::ToPods, source_sdu(wire, MEDIA)),
                (
                    6,
                    Direction::FromPods,
                    accessory_sdu(CONNECTED_DEVICES_OPCODE, &[]),
                ),
                (
                    7,
                    Direction::FromPods,
                    accessory_sdu(CONNECTED_DEVICES_OPCODE, &[0]),
                ),
            ],
        );
        let rows = takeover_rows(&takeover(dir.path(), &SDU_FILES));

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["trial"], "");
        assert_eq!(
            rows[0]["sources"],
            "1000:host:wire:0x22,2000:host:reversed:0x02,3000:other:neither:0x02,4000:zero:neither:0x00"
        );
        assert_eq!(rows[0]["connected_devices"], "6000:6:,7000:7:0");
    }

    #[test]
    fn takeover_joins_source_stream_journal_and_sink_times() {
        const CLOSE: u8 = 0x08;
        const REJECT: u8 = 0b11;
        const BAD_STATE: u8 = 0x31;

        let mut trace = playing_link();
        trace.at_ms(40);
        trace.send(
            Direction::FromPods,
            SIGNALLING_CIDS,
            &avdtp(TAKEOVER_TRANSACTION, COMMAND, AVDTP_SUSPEND, &[FIRST_SEID]),
        );
        trace.at_ms(50);
        trace.send(
            Direction::ToPods,
            SIGNALLING_CIDS,
            &avdtp(TAKEOVER_TRANSACTION, COMMAND, CLOSE, &[FIRST_SEID]),
        );
        trace.at_ms(60);
        trace.send(
            Direction::FromPods,
            SIGNALLING_CIDS,
            &avdtp(TAKEOVER_TRANSACTION, ACCEPT, AVDTP_START, &[]),
        );
        trace.at_ms(80);
        trace.send(
            Direction::ToPods,
            SIGNALLING_CIDS,
            &avdtp(TAKEOVER_TRANSACTION, ACCEPT, AVDTP_START, &[]),
        );
        trace.at_ms(85);
        trace.send(
            Direction::FromPods,
            SIGNALLING_CIDS,
            &avdtp(
                TAKEOVER_TRANSACTION,
                REJECT,
                AVDTP_START,
                &[FIRST_SEID, BAD_STATE],
            ),
        );
        trace.at_ms(90);
        trace.send(
            Direction::FromPods,
            SIGNALLING_CIDS,
            &avdtp(TAKEOVER_TRANSACTION, ACCEPT, AVDTP_START, &[]),
        );
        let dir = sitting(&trace.bytes());
        put(&dir, "trials.log", RETRIAL.as_bytes(), OWNER_FILE);
        put_sdu_log(
            &dir,
            &[
                (
                    30,
                    Direction::FromPods,
                    source_sdu(OTHER_PODS_ADDRESS, MEDIA),
                ),
                (70, Direction::FromPods, source_sdu([0; 6], 0)),
            ],
        );
        let files = [
            TRACE_FILES.as_slice(),
            SDU_FILES.as_slice(),
            &[("--journal", "journal.json"), ("--sinks", "sinks.log")],
        ]
        .concat();
        let rows = takeover_rows(&takeover(dir.path(), &files));

        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(
            row["sources"],
            format!(
                "{}:other:neither:0x02,{}:zero:neither:0x00",
                at_us(30),
                at_us(70)
            )
        );
        assert_eq!(row["suspend_us"], at_us(40).to_string());
        assert_eq!(row["suspend_from"], "pods");
        assert_eq!(row["close_us"], at_us(50).to_string());
        assert_eq!(row["close_from"], "host");
        assert_eq!(row["restart_us"], at_us(90).to_string());
        assert_eq!(row["suspend_after_source_us"], "10000");
        assert_eq!(row["close_after_source_us"], "20000");
        assert_eq!(row["restart_after_handback_us"], "20000");
        assert_eq!(row["up_us"], at_us(1_320).to_string());
        assert_eq!(row["populated_us"], at_us(1_350).to_string());
        assert_eq!(row["sink_states"], format!("{}:suspended", at_us(1_900)));
    }

    #[test]
    fn takeover_refuses_bad_log_inputs_without_echoing_payloads() {
        let dir = tempfile::tempdir().unwrap();
        put(
            &dir,
            "host",
            address_file_text(HOST_ADDRESS).as_bytes(),
            OWNER_FILE,
        );
        assert_refused(&takeover(dir.path(), &SDU_FILES), "sdu.log");
        put(
            &dir,
            "sdu.log",
            address_file_text(HOST_ADDRESS).as_bytes(),
            WORLD_READABLE,
        );
        assert_refused(&takeover(dir.path(), &SDU_FILES), "has mode 0644");
        put(
            &dir,
            "sdu.log",
            address_file_text(HOST_ADDRESS).as_bytes(),
            OWNER_FILE,
        );
        assert_refused(&takeover(dir.path(), &SDU_FILES), "sdu.log line 1");
    }
}
