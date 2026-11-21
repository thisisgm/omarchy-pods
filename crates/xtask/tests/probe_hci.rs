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

    use pods_proto::sdulog::{Direction, parse_line};
    use tempfile::TempDir;

    use super::fixture::{
        DEVICE_NAME, HOST_ADDRESS, KEYSTROKES, LINK_KEY, PODS_ADDRESS, att_packets,
        control_packets, session,
    };

    /// A stamp of the shape `date -u +%Y%m%dT%H%M%SZ` prints.
    const STAMP: &str = "20261120T120000Z";
    /// The run every test creates.
    const RUN: &str = "trace-1";
    /// The mode the store gives every file it creates.
    const OWNER_FILE: u32 = 0o600;
    /// A mode with a group bit, which the bounded read refuses.
    const GROUP_READABLE: u32 = 0o640;
    /// The store's bound on a capture read, 64 MiB, which a trace may pass.
    const CAPTURE_BOUND: usize = 64 * 1024 * 1024;
    /// The bound on a trace, 512 MiB, written out so the test pins the number.
    const TRACE_BOUND: u64 = 536_870_912;
    /// Monitor opcode of a system note, a record the reader skips.
    const OPCODE_SYSTEM_NOTE: u16 = 12;
    /// The most data bytes one record holds.
    const LONGEST_RECORD: usize = 65_535;
    /// Enough longest records to pass `CAPTURE_BOUND`: each takes 65 559 bytes with its header.
    const NOTES_PAST_CAPTURE_BOUND: usize = 1024;
    /// Every file a run of the session trace holds.
    const SESSION_FILES: [&str; 6] = [
        "a2dp.tsv",
        "att.log",
        "avdtp.txt",
        "links.txt",
        "psm.tsv",
        "sdu.log",
    ];

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
        assert!(links.contains("log sdu.log written=7 refused_empty=0 refused_too_long=0\n"));
        for name in SESSION_FILES {
            let mode = fs::metadata(dir.join(name)).unwrap().permissions().mode();
            assert_eq!(mode & 0o7777, OWNER_FILE, "{name}");
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
        wrong_datalink[12..16].copy_from_slice(&1001_u32.to_be_bytes());
        let mut long_record = trace.clone();
        long_record[20..24].copy_from_slice(&u32::MAX.to_be_bytes());
        for (bytes, rule) in [
            (&[][..], "shorter than its 16 byte file header"),
            (&wrong_magic[..], "identification pattern"),
            (&wrong_datalink[..], "datalink type 1001"),
            (&trace[..trace.len() - 3], "is cut short"),
            (&trace[..40], "is cut short"),
            (&long_record[..], "a record holds at most 65535"),
        ] {
            let (home, path) = home_with_trace(bytes);
            let refused = hci(&home, &path, RUN);
            assert!(!refused.status.success(), "{rule}: {refused:?}");
            assert_eq!(refused.stdout, b"", "{rule}");
            let error = String::from_utf8(refused.stderr).unwrap();
            assert!(error.contains(rule), "{rule}: {error}");
            assert!(error.contains(&path.display().to_string()), "{error}");
            assert!(!home.path().join("omapods-captures").exists(), "{rule}");
        }
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

        let link = home.path().join("link.btsnoop");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        fs::set_permissions(&path, Permissions::from_mode(OWNER_FILE)).unwrap();
        let refused = hci(&home, &link, RUN);
        assert!(!refused.status.success(), "{refused:?}");
        let error = String::from_utf8(refused.stderr).unwrap();
        assert!(error.contains("is a symbolic link"), "{error}");

        let missing = hci(&home, &home.path().join("absent.btsnoop"), RUN);
        assert!(!missing.status.success(), "{missing:?}");
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
}
