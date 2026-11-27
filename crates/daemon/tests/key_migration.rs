//! The real daemon imports the Qt daemon's proximity keys on startup, before any device connects.

#[cfg(test)]
#[expect(
    dead_code,
    reason = "each suite uses part of the shared support, so the rest goes unused here"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::fs::{self, Permissions};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::PathBuf;

    use super::support::{FILE_MODE, Session};

    const VALID: &str = include_str!("fixtures/qt-keys/valid.conf");
    const IDENTITY: &str = "1001615c223b3d2c29000a0d097fff41";
    const PAYLOAD: &str = "0203340561064607080b0c0e0f201b5a";

    fn legacy_file(session: &Session) -> PathBuf {
        session
            .config_home()
            .join("AirPodsTrayApp")
            .join("AirPodsTrayApp.conf")
    }

    fn plant_legacy(session: &Session, text: &str) -> PathBuf {
        let path = legacy_file(session);
        fs::create_dir(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }

    fn keys_json(identity: &str, payload: &str) -> String {
        format!("{{\"identity\":\"{identity}\",\"payload\":\"{payload}\"}}\n")
    }

    fn assert_no_keys_in_log(log: &str, planted: &str) {
        for key in [IDENTITY, PAYLOAD] {
            assert!(!log.contains(key), "log disclosed a key");
        }
        for (_, encoded) in planted.lines().filter_map(|line| line.split_once('=')) {
            assert!(!log.contains(encoded), "log disclosed an encoded key");
        }
    }

    #[test]
    fn imports_qsettings_keys_without_a_device_connection() {
        let session = Session::new();
        let old = plant_legacy(&session, VALID);
        let mut daemon = session.start();

        let path = session.keys_file();
        let metadata = fs::metadata(&path).expect("the old keys were not imported");
        assert_eq!(metadata.permissions().mode() & 0o7777, FILE_MODE);
        let stored = fs::read_to_string(path).unwrap();
        assert!(
            stored == keys_json(IDENTITY, PAYLOAD),
            "imported keys differ"
        );
        assert!(
            fs::read_to_string(old).unwrap() == VALID,
            "old file changed"
        );
        assert!(daemon.stop().success());
        assert_no_keys_in_log(&daemon.log(), VALID);
    }

    fn refused(source: &str, unreadable: bool, reason: &str) {
        let session = Session::new();
        let old = plant_legacy(&session, source);
        if unreadable {
            fs::set_permissions(&old, Permissions::from_mode(0o0)).unwrap();
        }
        let mut daemon = session.start();

        assert!(!session.keys_file().exists(), "imported unusable keys");
        assert!(daemon.stop().success());
        let log = daemon.log();
        assert_no_keys_in_log(&log, source);
        let mut lines = log
            .lines()
            .filter(|line| line.contains("AirPodsTrayApp.conf"));
        let line = lines.next().expect("no legacy config diagnostic");
        assert!(
            line.contains(&*old.to_string_lossy()),
            "diagnostic omitted the path"
        );
        assert!(
            line.contains(reason),
            "diagnostic omitted the reason {reason}"
        );
        assert!(lines.next().is_none(), "repeated legacy config diagnostic");
    }

    #[test]
    fn diagnoses_wrong_length() {
        refused(
            include_str!("fixtures/qt-keys/short.conf"),
            false,
            "not 16 bytes",
        );
    }

    #[test]
    fn diagnoses_overlong_key() {
        let long = VALID.replacen(r"\x1bZ)", r"\x1bZZ)", 1);
        assert!(long != VALID, "fixture has no matching key end");
        refused(&long, false, "magicAccEncKey is not 16 bytes");
    }

    #[test]
    fn diagnoses_missing_field() {
        refused(
            include_str!("fixtures/qt-keys/missing.conf"),
            false,
            "missing magicAccEncKey",
        );
    }

    #[test]
    fn diagnoses_malformed_value() {
        refused(
            include_str!("fixtures/qt-keys/malformed.conf"),
            false,
            "malformed magicAccEncKey",
        );
    }

    #[test]
    fn diagnoses_malformed_escape() {
        let malformed = VALID.replacen(r"\x10", r"\q", 1);
        assert!(malformed != VALID, "fixture has no matching escape");
        refused(&malformed, false, "malformed magicAccIRK");
    }

    #[test]
    fn diagnoses_unreadable_file() {
        refused(VALID, true, "Permission denied");
    }

    #[test]
    fn existing_keys_skip_legacy_import_even_when_unusable() {
        let current = keys_json(PAYLOAD, IDENTITY);
        for stored in [current.as_str(), "{}\n"] {
            let session = Session::new();
            plant_legacy(&session, VALID);
            let keys = session.keys_file();
            fs::create_dir(keys.parent().unwrap()).unwrap();
            fs::write(session.settings_file(), "{}\n").unwrap();
            fs::write(&keys, stored).unwrap();
            fs::set_permissions(&keys, Permissions::from_mode(FILE_MODE)).unwrap();
            let original_inode = fs::metadata(&keys).unwrap().ino();
            let mut daemon = session.start();

            assert!(
                fs::read_to_string(&keys).unwrap() == stored,
                "existing keys changed"
            );
            assert_eq!(fs::metadata(&keys).unwrap().ino(), original_inode);
            assert!(daemon.stop().success());
            let log = daemon.log();
            assert_no_keys_in_log(&log, VALID);
            assert!(
                !log.contains("AirPodsTrayApp.conf") && !log.contains("keys: cannot write"),
                "attempted legacy key import"
            );
        }
    }
}
