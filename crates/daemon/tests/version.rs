//! The file under test: what it says it is, and that a session starts the daemon `LIBREPODS_E2E_BINARY` names.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "these tests start one daemon and read its version, so the rig and the fakes the other suites share go unused"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Command, ExitStatus, Stdio};

    use super::support::{self, BINARY_VARIABLE, Session, run_to_end};

    /// The test a respawned copy of this test binary runs, by its full name.
    const UNDER_TEST: &str = "tests::the_daemon_a_session_starts_is_the_file_under_test";

    /// Runs `UNDER_TEST` alone in a new process of this test binary in `dir`, with `BINARY_VARIABLE` naming `file`.
    fn respawn_with(dir: &Path, file: &Path) -> (ExitStatus, String) {
        let mut command = Command::new(env::current_exe().unwrap());
        command
            .args(["--exact", UNDER_TEST])
            .current_dir(dir)
            .env(BINARY_VARIABLE, file)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = run_to_end(&mut command);
        // The harness prints a panic to stdout when it captures and to stderr when told not to.
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        (output.status, said)
    }

    #[test]
    fn version_flag_prints_name_and_version() {
        let out = Command::new(support::binary())
            .arg("--version")
            .output()
            .unwrap();
        assert!(out.status.success());
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            format!("librepods {}\n", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn the_daemon_a_session_starts_is_the_file_under_test() {
        let wanted = env::var_os(BINARY_VARIABLE).map_or_else(
            || PathBuf::from(env!("CARGO_BIN_EXE_librepods")),
            PathBuf::from,
        );
        let session = Session::new();
        let daemon = session.start();
        let running = fs::read_link(format!("/proc/{}/exe", daemon.child.id())).unwrap();
        assert_eq!(running, fs::canonicalize(wanted).unwrap());
    }

    #[test]
    fn a_session_starts_the_file_the_variable_names() {
        let dir = tempfile::tempdir().unwrap();
        let copy = dir.path().join("librepods-under-test");
        fs::copy(support::binary(), &copy).unwrap();
        let (status, said) = respawn_with(dir.path(), &copy);
        assert!(status.success(), "{said}");
        assert!(said.contains("test result: ok. 1 passed"), "{said}");
    }

    #[test]
    fn a_bare_name_in_the_variable_is_the_file_in_the_directory_the_suite_runs_in() {
        let dir = tempfile::tempdir().unwrap();
        let copy = dir.path().join("librepods-under-test");
        fs::copy(support::binary(), &copy).unwrap();
        let (status, said) = respawn_with(dir.path(), Path::new("librepods-under-test"));
        assert!(status.success(), "{said}");
        assert!(said.contains("test result: ok. 1 passed"), "{said}");
    }

    #[test]
    fn a_variable_that_names_no_file_fails_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        let nothing = dir.path().join("nothing");
        let (status, said) = respawn_with(dir.path(), &nothing);
        assert!(!status.success(), "{said}");
        let sentence = format!(
            "{BINARY_VARIABLE} names {}, which is no file: ",
            nothing.display()
        );
        assert!(said.contains(&sentence), "{said}");
    }

    #[test]
    fn a_variable_that_names_a_directory_fails_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        let (status, said) = respawn_with(dir.path(), dir.path());
        assert!(!status.success(), "{said}");
        let sentence = format!(
            "{BINARY_VARIABLE} names {}, which is no file\n",
            dir.path().display()
        );
        assert!(said.contains(&sentence), "{said}");
    }
}
