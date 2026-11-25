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
    use std::fs::{self, Permissions};
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::{Command, Output};

    use pods_core::policy::audio::sink_prefix;
    use pods_proto::addr::BdAddr;
    use tempfile::TempDir;

    use super::fixture::{OTHER_PODS_ADDRESS, two_devices, whole_connection};
    use super::hci_fixture::{EVENT_DISCONNECTION_COMPLETE, PODS_ADDRESS, PODS_HANDLE, Trace};

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

    fn device() -> String {
        PODS_ADDRESS.map(|byte| format!("{byte:02X}")).join(":")
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
        assert!(!ran.status.success());
        assert_eq!(text(&ran.stdout), "");
        assert!(text(&ran.stderr).contains(said), "{}", text(&ran.stderr));
        assert_no_address(ran);
    }

    /// Nothing the command wrote spells the address, with colons, with underscores or bare, in capitals or not.
    fn assert_no_address(ran: &Output) {
        let written = (text(&ran.stdout) + &text(&ran.stderr)).to_lowercase();
        for address in [PODS_ADDRESS, OTHER_PODS_ADDRESS] {
            let pairs = address.map(|byte| format!("{byte:02x}"));
            for spelling in [pairs.join(":"), pairs.join("_"), pairs.concat()] {
                assert!(!written.contains(&spelling), "{written}");
            }
        }
        assert!(!written.contains("bluez_output"), "{written}");
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
}
